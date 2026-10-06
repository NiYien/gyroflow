// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

//! Persistent KLT feature tracks from one frame to the next, ported from upstream gyroflow 322cb312
//! (`synchronization/optical_motion/tracker.rs`). Every pair of consecutive frames yields the points that survived a
//! forward-backward check, with the id of the track they belong to: the cost needs whole tracks, to take the slow
//! parallax of each point out of its motion.
//!
//! Differences from upstream: frames wider than the tracking width are downscaled first; new corners are only added
//! while the tracks are below `replenish_ratio · max_points`; the ids of the corners one replenish adds are handed out
//! round-robin over the grid cells; ids are never reused, not even across `reset`.

use opencv::{ core::{ self, Mat, Point, Point2f, Rect, Scalar, Size, TermCriteria, Vector, CV_8UC1 }, prelude::*, imgproc, video };

use crate::synchronization::GrayImage;
use super::FrameTracker;
use super::tracks::{ round_robin_ids, tracking_size, Observation };

/// Grid the new corners are spread over, see `KltTracker::replenish`
const GRID_ROWS: i32 = 6;
const GRID_COLS: i32 = 4;
/// What counts as a corner: its smaller eigenvalue against the strongest one in the frame
const QUALITY: f64 = 0.002;
/// ... or, in a cell with nothing that strong (cloudy sky), against the noise of flat areas, see `replenish`
const NOISE_MULTIPLE: f64 = 8.0;
/// No new corners within this many radii of a compact saturated blob (the sun)
const GLARE_RADII: f64 = 2.5;
const GLARE_MIN_AREA: f64 = 30.0;
/// Pyramidal Lucas-Kanade: levels, window, iterations and epsilon of the search (upstream values)
const LK_LEVELS: i32 = 3;
const LK_WIN: i32 = 21;
const LK_MAX_ITERS: i32 = 50;
const LK_EPS: f64 = 0.001;
/// Largest distance between a point and its forward-backward tracked self, px at tracking resolution
const FB_MAX_PX: f32 = 0.1;

/// Resize borrowed decoder pixels once before sharing a frame between tracking windows.
/// Keep the original INTER_AREA operation so moving it does not change the tracked pixels.
pub fn prepare_frame(width: u32, height: u32, stride: usize, pixels: &[u8], track_width: u32) -> Result<GrayImage, opencv::Error> {
    let len = stride.checked_mul(height as usize).unwrap_or(usize::MAX);
    if width == 0 || height == 0 || stride < width as usize || pixels.len() < len {
        return Err(opencv::Error::new(core::StsBadArg, "Invalid grayscale frame size"));
    }
    let (tw, th) = tracking_size((width, height), track_width);
    let packed = if (tw, th) == (width, height) {
        pixels[..len].chunks_exact(stride).flat_map(|row| row[..width as usize].iter().copied()).collect()
    } else {
        let src = Mat::new_rows_cols_with_data::<u8>(height as i32, stride as i32, &pixels[..len])?;
        let cropped = Mat::roi(&src, Rect::new(0, 0, width as i32, height as i32))?;
        let mut dst = Mat::default();
        imgproc::resize(&cropped, &mut dst, Size::new(tw as i32, th as i32), 0.0, 0.0, imgproc::INTER_AREA)?;
        dst.data_bytes()?.to_vec()
    };
    GrayImage::from_raw(tw, th, packed).ok_or_else(|| opencv::Error::new(core::StsBadArg, "Invalid tracking frame size"))
}

struct TrackingPyramid {
    layers: Vector<Mat>,
    levels: i32,
    /// What a replenish needs of the first level, computed on first use or ahead by `PreparedFrame::precompute_corners`
    corners: Option<Result<CornerCache, opencv::Error>>,
}

impl TrackingPyramid {
    fn new(img: &impl core::ToInputArray) -> Result<Self, opencv::Error> {
        let mut layers = Vector::<Mat>::new();
        // Own the pixels: the caller's grayscale buffer is released after this frame.
        // Derivatives and padding match the pyramids that LK builds internally.
        let levels = video::build_optical_flow_pyramid(img, &mut layers, Size::new(LK_WIN, LK_WIN), LK_LEVELS,
            true, core::BORDER_REFLECT_101, core::BORDER_CONSTANT, false)?;
        Ok(Self { layers, levels, corners: None })
    }

    /// The corner cache of the first level, `size` px
    fn corners(&mut self, size: (i32, i32)) -> Result<&CornerCache, opencv::Error> {
        if self.corners.is_none() {
            self.corners = Some(self.layers.get(0).and_then(|img| CornerCache::new(&img, size)));
        }
        match self.corners.as_ref() {
            Some(Ok(cache)) => Ok(cache),
            Some(Err(e)) => Err(opencv::Error::new(e.code, e.message.clone())),
            None => unreachable!(),
        }
    }
}

/// What a replenish needs of a frame that doesn't depend on the tracks: the corner response, the noise floor, the
/// glare and the candidates of the global top-up. Computing it once instead of in each OpenCV call gives the same
/// corners, see `select_global`
struct CornerCache {
    /// `cornerMinEigenVal` of the frame, as `goodFeaturesToTrack` computes it
    eig: Mat,
    /// The 10% quantile of the positive responses on an 8 px grid, None without any
    noise: Option<f64>,
    /// Circles to keep new corners out of, see `GLARE_RADII`
    glare: Vec<(Point, i32)>,
    /// Local 3x3 maxima of the positive responses inside the 1 px border, as (response, x, y): strongest first, equal
    /// ones by position, last first. The order `goodFeaturesToTrack` sorts its candidates in
    candidates: Vec<(f32, i32, i32)>,
}

impl CornerCache {
    fn new(img: &Mat, (w, h): (i32, i32)) -> Result<Self, opencv::Error> {
        let glare = glare_circles(img, (w, h))?;
        let mut eig = Mat::default();
        imgproc::corner_min_eigen_val(img, &mut eig, 7, 3, core::BORDER_DEFAULT)?;
        let noise = {
            let mut v: Vec<f32> = Vec::with_capacity(((w / 8 + 1) * (h / 8 + 1)) as usize);
            for y in (0..h).step_by(8) { for x in (0..w).step_by(8) { let e = *eig.at_2d::<f32>(y, x)?; if e > 0.0 { v.push(e); } } }
            if v.is_empty() { None } else {
                let i = v.len() / 10;
                Some(*v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1 as f64)
            }
        };
        let mut dilated = Mat::default();
        imgproc::dilate(&eig, &mut dilated, &Mat::default(), Point::new(-1, -1), 1, core::BORDER_CONSTANT, imgproc::morphology_default_border_value()?)?;
        let mut candidates = Vec::new();
        for y in 1..eig.rows() - 1 {
            let (row, max) = (eig.at_row::<f32>(y)?, dilated.at_row::<f32>(y)?);
            for x in 1..eig.cols() - 1 {
                let response = row[x as usize];
                if response > 0.0 && response == max[x as usize] { candidates.push((response, x, y)); }
            }
        }
        candidates.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then((b.2, b.1).cmp(&(a.2, a.1))));
        Ok(Self { eig, noise, glare, candidates })
    }

    /// The corners `goodFeaturesToTrack(img, max_corners, quality, min_distance, mask, 7)` adds, without computing
    /// the response again. It keeps the local maxima of the response thresholded at `quality` of its masked maximum,
    /// strongest first, each at least `min_distance` from those kept before. Above a positive threshold the maxima of
    /// the thresholded response are those of the response itself; for a non-positive one OpenCV is asked
    fn select_global(&self, img: &Mat, mask: &Mat, max_corners: i32, quality: f64, min_distance: f64) -> Result<Vec<Point2f>, opencv::Error> {
        let mut max_val = 0.0;
        core::min_max_loc(&self.eig, None, Some(&mut max_val), None, None, mask)?;
        if max_val <= 0.0 {
            let mut corners = Vector::<Point2f>::new();
            imgproc::good_features_to_track(img, &mut corners, max_corners, quality, min_distance, mask, 7, false, 0.04)?;
            return Ok(corners.to_vec());
        }
        // `threshold` compares 32-bit responses with the threshold rounded to 32 bits
        let threshold = (max_val * quality) as f32;
        let (w, h) = (self.eig.cols(), self.eig.rows());
        let cell = min_distance.round_ties_even() as i32;
        let (grid_w, grid_h) = ((w + cell - 1) / cell, (h + cell - 1) / cell);
        let mut grid: Vec<Vec<(f32, f32)>> = vec![Vec::new(); (grid_w * grid_h) as usize];
        let min_distance_sq = min_distance * min_distance;
        let mut corners = Vec::new();
        for &(response, x, y) in &self.candidates {
            if response <= threshold { break; }
            if *mask.at_2d::<u8>(y, x)? == 0 { continue; }
            let (cx, cy) = (x / cell, y / cell);
            let near = ((cy - 1).max(0)..=(cy + 1).min(grid_h - 1)).any(|gy| ((cx - 1).max(0)..=(cx + 1).min(grid_w - 1)).any(|gx| {
                grid[(gy * grid_w + gx) as usize].iter().any(|&(px, py)| {
                    let (dx, dy) = (x as f32 - px, y as f32 - py);
                    ((dx * dx + dy * dy) as f64) < min_distance_sq
                })
            }));
            if near { continue; }
            grid[(cy * grid_w + cx) as usize].push((x as f32, y as f32));
            corners.push(Point2f::new(x as f32, y as f32));
            if max_corners > 0 && corners.len() as i32 == max_corners { break; }
        }
        Ok(corners)
    }
}

/// The circles that keep new corners away from the sun and the like, see `GLARE_RADII`
fn glare_circles(img: &Mat, (w, h): (i32, i32)) -> Result<Vec<(Point, i32)>, opencv::Error> {
    let w = w as f64;
    let mut bright = Mat::default();
    imgproc::threshold(img, &mut bright, 249.0, 255.0, imgproc::THRESH_BINARY)?;
    let (mut labels, mut stats, mut centroids) = (Mat::default(), Mat::default(), Mat::default());
    let n = imgproc::connected_components_with_stats(&bright, &mut labels, &mut stats, &mut centroids, 8, core::CV_32S)?;
    let mut circles = Vec::new();
    for i in 1..n {
        let area = *stats.at_2d::<i32>(i, imgproc::CC_STAT_AREA)? as f64;
        let at = |c: i32| -> Result<f64, opencv::Error> { Ok(*stats.at_2d::<i32>(i, c)? as f64) };
        let (bx, by, bw, bh) = (at(imgproc::CC_STAT_LEFT)?, at(imgproc::CC_STAT_TOP)?, at(imgproc::CC_STAT_WIDTH)?, at(imgproc::CC_STAT_HEIGHT)?);
        let cut = bx <= 0.0 || by <= 0.0 || bx + bw >= w || by + bh >= h as f64;
        let compact = area >= 0.5 * bw * bh && bw.max(bh) <= if cut { 3.0 } else { 2.0 } * bw.min(bh);
        let radius = bw.max(bh) / 2.0;
        if area < GLARE_MIN_AREA * (w / 960.0).powi(2) || !compact || radius > w / 12.0 { continue; }
        let (cx, cy) = (bx + bw / 2.0, by + bh / 2.0);
        circles.push((Point::new(cx as i32, cy as i32), (GLARE_RADII * radius) as i32));
    }
    Ok(circles)
}

/// A frame made ready for `KltTracker::track_prepared`: resized to the tracking width and its pyramid built. None of
/// it depends on the tracks, so frames can be prepared ahead, side by side
pub struct PreparedFrame {
    pyramid: TrackingPyramid,
    size: (u32, u32),
}

impl PreparedFrame {
    /// Also computes what a replenish will need of this frame, now instead of while tracking the next one. An error
    /// is kept for that replenish, where `track` would have met it
    pub fn precompute_corners(&mut self) {
        let size = (self.size.0 as i32, self.size.1 as i32);
        let _ = self.pyramid.corners(size);
    }
}

pub struct KltTracker {
    prev: Option<TrackingPyramid>,
    size: (i32, i32),
    points: Vec<Point2f>,
    ids: Vec<u32>,
    next_id: u32,
    max_points: usize,
    replenish_ratio: f64,
    track_width: u32,
    /// Replenishes as before `CornerCache`, to compare with
    #[cfg(test)]
    reference: bool,
}

impl KltTracker {
    pub fn new(max_points: usize, replenish_ratio: f64, track_width: u32) -> Self {
        Self { prev: None, size: (0, 0), points: Vec::new(), ids: Vec::new(), next_id: 0, max_points, replenish_ratio, track_width,
            #[cfg(test)] reference: false }
    }

    /// Prepares a frame for `track_prepared`, independently of any tracker with this `track_width`. The errors are
    /// those `track` would return
    pub fn prepare(img: &GrayImage, track_width: u32) -> Result<PreparedFrame, String> {
        if img.width() == 0 || img.height() == 0 {
            return Err(format!("Empty frame {}x{}", img.width(), img.height()));
        }
        Self::prepare_frame(img, track_width).map_err(|e| format!("OpenCV error: {e:?}"))
    }

    fn prepare_frame(img: &GrayImage, track_width: u32) -> Result<PreparedFrame, opencv::Error> {
        let (iw, ih) = img.dimensions();
        let (tw, th) = tracking_size((iw, ih), track_width);
        // Borrows the pixels, no copy
        let src = Mat::new_rows_cols_with_data::<u8>(ih as i32, iw as i32, &img.as_raw()[..iw as usize * ih as usize])?;
        // Build each frame's pyramid once for both directions and the following frame pair.
        let pyramid = if (tw, th) == (iw, ih) {
            TrackingPyramid::new(&src)?
        } else {
            let mut dst = Mat::default();
            imgproc::resize(&src, &mut dst, Size::new(tw as i32, th as i32), 0.0, 0.0, imgproc::INTER_AREA)?;
            TrackingPyramid::new(&dst)?
        };
        Ok(PreparedFrame { pyramid, size: (tw, th) })
    }

    /// `track` of a frame prepared by `prepare`, or of the error preparing it: the tracker is then reset, as by an
    /// error of `track`
    pub fn track_prepared(&mut self, frame: Result<PreparedFrame, String>) -> Result<(Vec<Observation>, (u32, u32)), String> {
        let tracked = frame.and_then(|frame| self.track_frame(frame).map_err(|e| format!("OpenCV error: {e:?}")));
        if tracked.is_err() { self.reset(); }
        tracked
    }

    fn track_frame(&mut self, frame: PreparedFrame) -> Result<(Vec<Observation>, (u32, u32)), opencv::Error> {
        let PreparedFrame { pyramid: cur, size: (tw, th) } = frame;
        let (w, h) = (tw as i32, th as i32);
        if self.size != (w, h) { self.reset(); }
        self.size = (w, h);

        let mut out = Vec::new();
        #[allow(unused_mut)]
        if let Some(mut prev) = self.prev.take() {
            #[cfg(test)]
            if self.reference { self.replenish_reference(&prev.layers.get(0)?)?; } else { self.replenish(&mut prev)?; }
            #[cfg(not(test))]
            self.replenish(&mut prev)?;
            if !self.points.is_empty() {
                let pa: Vector<Point2f> = Vector::from_slice(&self.points);
                let mut pb = Vector::<Point2f>::new();
                let mut pa2 = Vector::<Point2f>::new();
                let mut st = Vector::<u8>::new();
                let mut st2 = Vector::<u8>::new();
                let mut err = Vector::<f32>::new();
                let criteria = TermCriteria::new(3 /* COUNT | EPS */, LK_MAX_ITERS, LK_EPS)?;
                let win = Size::new(LK_WIN, LK_WIN);
                let levels = prev.levels.min(cur.levels);
                video::calc_optical_flow_pyr_lk(&prev.layers, &cur.layers, &pa, &mut pb, &mut st, &mut err, win, levels, criteria, 0, 1e-4)?;
                video::calc_optical_flow_pyr_lk(&cur.layers, &prev.layers, &pb, &mut pa2, &mut st2, &mut err, win, levels, criteria, 0, 1e-4)?;

                let mut points = Vec::with_capacity(self.points.len());
                let mut ids = Vec::with_capacity(self.points.len());
                for i in 0..pa.len() {
                    let (a, b, a2) = (pa.get(i)?, pb.get(i)?, pa2.get(i)?);
                    let fb = ((a2.x - a.x).powi(2) + (a2.y - a.y).powi(2)).sqrt();
                    let inside = b.x > 2.0 && b.y > 2.0 && b.x < (w - 3) as f32 && b.y < (h - 3) as f32;
                    if st.get(i)? == 1 && st2.get(i)? == 1 && fb < FB_MAX_PX && inside {
                        out.push(Observation { id: self.ids[i], a: [a.x, a.y], b: [b.x, b.y] });
                        points.push(b);
                        ids.push(self.ids[i]);
                    }
                }
                self.points = points;
                self.ids = ids;
            }
        }
        self.prev = Some(cur);
        Ok((out, (tw, th)))
    }

    /// Tops the tracks up with new corners, away from the ones already tracked: first each cell of a grid up to its
    /// share, then with whatever the cells without enough corners left, the strongest anywhere. What doesn't depend
    /// on the tracks comes from the frame's `CornerCache`
    fn replenish(&mut self, prev: &mut TrackingPyramid) -> Result<(), opencv::Error> {
        if self.points.len() as f64 >= self.replenish_ratio * self.max_points as f64 { return Ok(()); }
        let (w, h) = self.size;
        let min_distance = (w.max(h) as f64 / 120.0).max(4.0);
        let mut mask = Mat::new_rows_cols_with_default(h, w, CV_8UC1, Scalar::all(255.0))?;
        for p in &self.points {
            imgproc::circle(&mut mask, Point::new(p.x as i32, p.y as i32), min_distance as i32, Scalar::all(0.0), -1, imgproc::LINE_8, 0)?;
        }
        let img = prev.layers.get(0)?;
        let cache = prev.corners(self.size)?;
        for &(centre, radius) in &cache.glare {
            imgproc::circle(&mut mask, centre, radius, Scalar::all(0.0), -1, imgproc::LINE_8, 0)?;
        }
        let eig = &cache.eig;
        let mut strongest = 0.0;
        core::min_max_loc(eig, None, Some(&mut strongest), None, None, &mask)?;
        if strongest <= 0.0 { return Ok(()); } // A flat frame: black, a fade
        let threshold = QUALITY * strongest;
        let Some(noise) = cache.noise else { return Ok(()) };
        let floor = NOISE_MULTIPLE * noise;
        let share = self.max_points / (GRID_ROWS * GRID_COLS) as usize;
        // The cells add their corners one after the other, with consecutive ids; relabelled round-robin below
        let (first_new, first_id) = (self.points.len(), self.next_id);
        let mut per_cell = vec![0usize; (GRID_ROWS * GRID_COLS) as usize];
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                let (x0, y0) = (w * c / GRID_COLS, h * r / GRID_ROWS);
                let cell = Rect::new(x0, y0, w * (c + 1) / GRID_COLS - x0, h * (r + 1) / GRID_ROWS - y0);
                let has = self.points.iter().filter(|p| cell.contains(Point::new(p.x as i32, p.y as i32))).count();
                if has >= share { continue; }
                let mut corners = Vector::<Point2f>::new();
                {
                    let (eig_cell, mask_cell, img_cell) = (Mat::roi(eig, cell)?, Mat::roi(&mask, cell)?, Mat::roi(&img, cell)?);
                    let mut cell_strongest = 0.0;
                    core::min_max_loc(&eig_cell, None, Some(&mut cell_strongest), None, None, &mask_cell)?;
                    let cell_threshold = threshold.min(floor.max(QUALITY * cell_strongest));
                    if cell_threshold <= 0.0 || cell_strongest < cell_threshold { continue; }
                    imgproc::good_features_to_track(&img_cell, &mut corners, (share - has) as i32, cell_threshold / cell_strongest, min_distance, &mask_cell, 7, false, 0.04)?;
                }
                per_cell[(r * GRID_COLS + c) as usize] = corners.len();
                for p in corners {
                    let p = Point2f::new(p.x + x0 as f32, p.y + y0 as f32);
                    imgproc::circle(&mut mask, Point::new(p.x as i32, p.y as i32), min_distance as i32, Scalar::all(0.0), -1, imgproc::LINE_8, 0)?;
                    self.add(p);
                }
            }
        }
        // Same ids, round-robin over the cells: the coarse scan breaks ties between equally long tracks by id
        self.ids[first_new..].copy_from_slice(&round_robin_ids(&per_cell, first_id));
        if self.points.len() >= self.max_points { return Ok(()); }
        for p in cache.select_global(&img, &mask, (self.max_points - self.points.len()) as i32, QUALITY, min_distance)? { self.add(p); }
        Ok(())
    }

    /// The replenish as it was before `CornerCache`, kept to prove the cached one adds the same corners
    #[cfg(test)]
    fn replenish_reference(&mut self, img: &Mat) -> Result<(), opencv::Error> {
        if self.points.len() as f64 >= self.replenish_ratio * self.max_points as f64 { return Ok(()); }
        let (w, h) = self.size;
        let min_distance = (w.max(h) as f64 / 120.0).max(4.0);
        let mut mask = Mat::new_rows_cols_with_default(h, w, CV_8UC1, Scalar::all(255.0))?;
        for p in &self.points {
            imgproc::circle(&mut mask, Point::new(p.x as i32, p.y as i32), min_distance as i32, Scalar::all(0.0), -1, imgproc::LINE_8, 0)?;
        }
        self.mask_glare_reference(img, &mut mask)?;
        let mut eig = Mat::default();
        imgproc::corner_min_eigen_val(img, &mut eig, 7, 3, core::BORDER_DEFAULT)?;
        let mut strongest = 0.0;
        core::min_max_loc(&eig, None, Some(&mut strongest), None, None, &mask)?;
        if strongest <= 0.0 { return Ok(()); } // A flat frame: black, a fade
        let threshold = QUALITY * strongest;
        let noise = {
            let mut v: Vec<f32> = Vec::with_capacity(((w / 8 + 1) * (h / 8 + 1)) as usize);
            for y in (0..h).step_by(8) { for x in (0..w).step_by(8) { let e = *eig.at_2d::<f32>(y, x)?; if e > 0.0 { v.push(e); } } }
            if v.is_empty() { return Ok(()); }
            let i = v.len() / 10;
            *v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1 as f64
        };
        let floor = NOISE_MULTIPLE * noise;
        let share = self.max_points / (GRID_ROWS * GRID_COLS) as usize;
        // The cells add their corners one after the other, with consecutive ids; relabelled round-robin below
        let (first_new, first_id) = (self.points.len(), self.next_id);
        let mut per_cell = vec![0usize; (GRID_ROWS * GRID_COLS) as usize];
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                let (x0, y0) = (w * c / GRID_COLS, h * r / GRID_ROWS);
                let cell = Rect::new(x0, y0, w * (c + 1) / GRID_COLS - x0, h * (r + 1) / GRID_ROWS - y0);
                let has = self.points.iter().filter(|p| cell.contains(Point::new(p.x as i32, p.y as i32))).count();
                if has >= share { continue; }
                let mut corners = Vector::<Point2f>::new();
                {
                    let (eig_cell, mask_cell, img_cell) = (Mat::roi(&eig, cell)?, Mat::roi(&mask, cell)?, Mat::roi(img, cell)?);
                    let mut cell_strongest = 0.0;
                    core::min_max_loc(&eig_cell, None, Some(&mut cell_strongest), None, None, &mask_cell)?;
                    let cell_threshold = threshold.min(floor.max(QUALITY * cell_strongest));
                    if cell_threshold <= 0.0 || cell_strongest < cell_threshold { continue; }
                    imgproc::good_features_to_track(&img_cell, &mut corners, (share - has) as i32, cell_threshold / cell_strongest, min_distance, &mask_cell, 7, false, 0.04)?;
                }
                per_cell[(r * GRID_COLS + c) as usize] = corners.len();
                for p in corners {
                    let p = Point2f::new(p.x + x0 as f32, p.y + y0 as f32);
                    imgproc::circle(&mut mask, Point::new(p.x as i32, p.y as i32), min_distance as i32, Scalar::all(0.0), -1, imgproc::LINE_8, 0)?;
                    self.add(p);
                }
            }
        }
        // Same ids, round-robin over the cells: the coarse scan breaks ties between equally long tracks by id
        self.ids[first_new..].copy_from_slice(&round_robin_ids(&per_cell, first_id));
        if self.points.len() >= self.max_points { return Ok(()); }
        let mut corners = Vector::<Point2f>::new();
        imgproc::good_features_to_track(img, &mut corners, (self.max_points - self.points.len()) as i32, QUALITY, min_distance, &mask, 7, false, 0.04)?;
        for p in corners { self.add(p); }
        Ok(())
    }

    /// `mask_glare` as it was before `CornerCache`
    #[cfg(test)]
    fn mask_glare_reference(&self, img: &Mat, mask: &mut Mat) -> Result<(), opencv::Error> {
        let w = self.size.0 as f64;
        let mut bright = Mat::default();
        imgproc::threshold(img, &mut bright, 249.0, 255.0, imgproc::THRESH_BINARY)?;
        let (mut labels, mut stats, mut centroids) = (Mat::default(), Mat::default(), Mat::default());
        let n = imgproc::connected_components_with_stats(&bright, &mut labels, &mut stats, &mut centroids, 8, core::CV_32S)?;
        for i in 1..n {
            let area = *stats.at_2d::<i32>(i, imgproc::CC_STAT_AREA)? as f64;
            let at = |c: i32| -> Result<f64, opencv::Error> { Ok(*stats.at_2d::<i32>(i, c)? as f64) };
            let (bx, by, bw, bh) = (at(imgproc::CC_STAT_LEFT)?, at(imgproc::CC_STAT_TOP)?, at(imgproc::CC_STAT_WIDTH)?, at(imgproc::CC_STAT_HEIGHT)?);
            let cut = bx <= 0.0 || by <= 0.0 || bx + bw >= w || by + bh >= self.size.1 as f64;
            let compact = area >= 0.5 * bw * bh && bw.max(bh) <= if cut { 3.0 } else { 2.0 } * bw.min(bh);
            let radius = bw.max(bh) / 2.0;
            if area < GLARE_MIN_AREA * (w / 960.0).powi(2) || !compact || radius > w / 12.0 { continue; }
            let (cx, cy) = (bx + bw / 2.0, by + bh / 2.0);
            imgproc::circle(mask, Point::new(cx as i32, cy as i32), (GLARE_RADII * radius) as i32, Scalar::all(0.0), -1, imgproc::LINE_8, 0)?;
        }
        Ok(())
    }

    fn add(&mut self, p: Point2f) {
        self.points.push(p);
        self.ids.push(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
    }
}

impl FrameTracker for KltTracker {
    /// Forgets the tracks: the next frame starts new ones (a gap in the decoded frames, a cut). Ids keep counting
    fn reset(&mut self) {
        self.prev = None;
        self.points.clear();
        self.ids.clear();
    }

    /// Tracks the points of the previous frame into this one; nothing for the first frame. An error leaves the tracker
    /// reset, so the next frame starts new tracks
    fn track(&mut self, img: &GrayImage) -> Result<(Vec<Observation>, (u32, u32)), String> {
        let frame = Self::prepare(img, self.track_width);
        self.track_prepared(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::testutil::XorShift64;

    /// 640x360 frame of 400 random 5x5 blobs (seeded), all moved by `shift` px
    fn blobs(w: u32, h: u32, n: usize, shift: (i32, i32)) -> GrayImage {
        let mut rng = XorShift64::new(42);
        let mut img = GrayImage::new(w, h);
        for _ in 0..n {
            let (x, y) = (rng.range(0.0, w as f64) as i32 + shift.0, rng.range(0.0, h as f64) as i32 + shift.1);
            // Below the glare threshold, so no blob gets masked as the sun
            let v = rng.range(60.0, 240.0) as u8;
            for dy in 0..5 {
                for dx in 0..5 {
                    let (px, py) = (x + dx, y + dy);
                    if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h { img.put_pixel(px as u32, py as u32, image::Luma([v])); }
                }
            }
        }
        img
    }

    fn median(mut v: Vec<f32>) -> f32 {
        v.sort_unstable_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    }

    #[test] fn preparing_padded_frames_preserves_the_original_resize_pixels() {
        for (w, h, stride) in [(1920, 1080, 1936), (1440, 1080, 1456), (960, 540, 968), (853, 480, 864)] {
            let image = blobs(w, h, 400, (0, 0));
            let mut padded = vec![255; stride * h as usize];
            for (src, dst) in image.as_raw().chunks_exact(w as usize).zip(padded.chunks_exact_mut(stride)) {
                dst[..w as usize].copy_from_slice(src);
            }
            let prepared = prepare_frame(w, h, stride, &padded, 960).unwrap();
            let (tw, th) = tracking_size((w, h), 960);
            assert_eq!(prepared.dimensions(), (tw, th));
            if (w, h) == (tw, th) {
                assert_eq!(prepared, image);
            } else {
                // Reference: crop to a packed GrayImage, then resize inside KltTracker.
                let src = Mat::new_rows_cols_with_data::<u8>(h as i32, w as i32, image.as_raw()).unwrap();
                let mut expected = Mat::default();
                imgproc::resize(&src, &mut expected, Size::new(tw as i32, th as i32), 0.0, 0.0, imgproc::INTER_AREA).unwrap();
                assert_eq!(prepared.as_raw().as_slice(), expected.data_bytes().unwrap());
            }
        }
        assert!(prepare_frame(32, 24, 31, &[0; 32 * 24], 960).is_err());
        assert!(prepare_frame(32, 24, 32, &[0; 16], 960).is_err());
    }

    #[test] fn cached_pyramids_match_image_tracking_in_both_directions() {
        fn flow(a: &impl core::ToInputArray, b: &impl core::ToInputArray, points: &Vector<Point2f>, levels: i32) -> (Vector<Point2f>, Vector<u8>) {
            let (mut out, mut status, mut err) = (Vector::<Point2f>::new(), Vector::<u8>::new(), Vector::<f32>::new());
            let criteria = TermCriteria::new(3, LK_MAX_ITERS, LK_EPS).unwrap();
            video::calc_optical_flow_pyr_lk(a, b, points, &mut out, &mut status, &mut err,
                Size::new(LK_WIN, LK_WIN), levels, criteria, 0, 1e-4).unwrap();
            (out, status)
        }
        fn same(a: &(Vector<Point2f>, Vector<u8>), b: &(Vector<Point2f>, Vector<u8>)) {
            assert_eq!(a.1.to_vec(), b.1.to_vec());
            for (x, y) in a.0.iter().zip(b.0.iter()) {
                assert_eq!((x.x.to_bits(), x.y.to_bits()), (y.x.to_bits(), y.y.to_bits()));
            }
        }
        // Include small images where OpenCV cannot build every requested level.
        for (w, h) in [(640, 360), (64, 48), (32, 24)] {
            let a = blobs(w, h, 400, (0, 0));
            let b = blobs(w, h, 400, (3, 2));
            let ma = Mat::new_rows_cols_with_data::<u8>(h as i32, w as i32, a.as_raw()).unwrap();
            let mb = Mat::new_rows_cols_with_data::<u8>(h as i32, w as i32, b.as_raw()).unwrap();
            let (pa, pb) = (TrackingPyramid::new(&ma).unwrap(), TrackingPyramid::new(&mb).unwrap());
            let points: Vector<Point2f> = (4..h - 4).step_by(7)
                .flat_map(|y| (4..w - 4).step_by(7).map(move |x| Point2f::new(x as f32, y as f32))).collect();
            let expected = flow(&ma, &mb, &points, LK_LEVELS);
            let actual = flow(&pa.layers, &pb.layers, &points, pa.levels.min(pb.levels));
            same(&expected, &actual);
            same(&flow(&mb, &ma, &expected.0, LK_LEVELS),
                 &flow(&pb.layers, &pa.layers, &actual.0, pa.levels.min(pb.levels)));
        }
    }

    #[test] fn cached_pyramids_are_discarded_on_size_change_and_error() {
        let mut tracker = KltTracker::new(1500, 1.0, 960);
        tracker.track(&blobs(640, 360, 400, (0, 0))).unwrap();
        assert!(tracker.track(&blobs(320, 180, 400, (0, 0))).unwrap().0.is_empty());
        assert!(!tracker.track(&blobs(320, 180, 400, (3, 2))).unwrap().0.is_empty());
        assert!(tracker.track(&GrayImage::new(0, 0)).is_err());
        assert!(tracker.prev.is_none());
        assert!(tracker.track(&blobs(320, 180, 400, (6, 4))).unwrap().0.is_empty());
    }

    #[test] fn tracks_a_translated_texture() {
        let mut t = KltTracker::new(1500, 1.0, 960);
        let (obs, size) = t.track(&blobs(640, 360, 400, (0, 0))).unwrap();
        assert_eq!((obs.len(), size), (0, (640, 360)));
        let (obs, size) = t.track(&blobs(640, 360, 400, (3, 2))).unwrap();
        assert_eq!(size, (640, 360));
        assert!(obs.len() >= 100, "{} observations", obs.len());
        let dx = median(obs.iter().map(|o| o.b[0] - o.a[0]).collect());
        let dy = median(obs.iter().map(|o| o.b[1] - o.a[1]).collect());
        assert!((dx - 3.0).abs() < 0.2 && (dy - 2.0).abs() < 0.2, "median motion ({dx}, {dy})");
    }

    #[test] fn ids_persist_and_are_never_reused_after_reset() {
        let mut t = KltTracker::new(1500, 1.0, 960);
        t.track(&blobs(640, 360, 400, (0, 0))).unwrap();
        let (pair1, _) = t.track(&blobs(640, 360, 400, (3, 2))).unwrap();
        let (pair2, _) = t.track(&blobs(640, 360, 400, (6, 4))).unwrap();
        assert!(!pair1.is_empty() && !pair2.is_empty());
        assert!(pair2.iter().any(|o| pair1.iter().any(|p| p.id == o.id)), "no track continues from pair 1 to pair 2");
        let max_old = pair1.iter().chain(&pair2).map(|o| o.id).max().unwrap();

        t.reset();
        let (first, _) = t.track(&blobs(640, 360, 400, (9, 6))).unwrap();
        assert!(first.is_empty());
        let (after, _) = t.track(&blobs(640, 360, 400, (12, 8))).unwrap();
        assert!(!after.is_empty());
        let min_new = after.iter().map(|o| o.id).min().unwrap();
        assert!(min_new > max_old, "new id {min_new} <= old id {max_old}");
    }

    #[test] fn wide_frames_are_downscaled() {
        let mut t = KltTracker::new(1500, 1.0, 960);
        let (_, size) = t.track(&blobs(1920, 1080, 1600, (0, 0))).unwrap();
        assert_eq!(size, (960, 540));
        let (obs, size) = t.track(&blobs(1920, 1080, 1600, (6, 4))).unwrap();
        assert_eq!(size, (960, 540));
        assert!(!obs.is_empty());
        assert!(obs.iter().all(|o| o.b[0] < 960.0 && o.b[1] < 540.0));
        let dx = median(obs.iter().map(|o| o.b[0] - o.a[0]).collect());
        let dy = median(obs.iter().map(|o| o.b[1] - o.a[1]).collect());
        assert!((dx - 3.0).abs() < 0.2 && (dy - 2.0).abs() < 0.2, "median motion ({dx}, {dy}) at half resolution");
    }

    #[test] fn first_ids_of_a_replenish_span_the_grid() {
        let mut t = KltTracker::new(1500, 1.0, 960);
        t.track(&blobs(640, 360, 400, (0, 0))).unwrap();
        t.track(&blobs(640, 360, 400, (3, 2))).unwrap();
        // The corners the replenish added, with their ids: the lowest ids, cell by cell
        let first = t.ids.iter().copied().min().unwrap();
        let mut cells: Vec<i32> = t.points.iter().zip(&t.ids)
            .filter(|&(_, &id)| id < first + 24)
            .map(|(p, _)| (p.y as i32 * GRID_ROWS / 360) * GRID_COLS + p.x as i32 * GRID_COLS / 640)
            .collect();
        cells.sort_unstable();
        cells.dedup();
        assert!(cells.len() >= 12, "the 24 lowest ids are in {} cells", cells.len());
    }

    /// Smoothed random noise (corners of every strength), a few saturated discs (glare) and a flat dark patch
    fn texture(w: i32, h: i32, seed: u64) -> Mat {
        let mut rng = XorShift64::new(seed);
        let noise: Vec<u8> = (0..w * h).map(|_| rng.range(0.0, 256.0) as u8).collect();
        let noise = Mat::new_rows_cols_with_data::<u8>(h, w, &noise).unwrap().try_clone().unwrap();
        let mut img = Mat::default();
        imgproc::gaussian_blur_def(&noise, &mut img, Size::new(0, 0), 1.5).unwrap();
        for _ in 0..3 {
            let centre = Point::new(rng.range(0.0, w as f64) as i32, rng.range(0.0, h as f64) as i32);
            imgproc::circle(&mut img, centre, (w / 40).max(2), Scalar::all(255.0), -1, imgproc::LINE_8, 0).unwrap();
        }
        imgproc::rectangle(&mut img, Rect::new(0, 0, w / 5, h / 5), Scalar::all(20.0), -1, imgproc::LINE_8, 0).unwrap();
        img
    }

    /// Equal responses everywhere along the edges: the order of equal candidates matters
    fn checkerboard(w: i32, h: i32, square: i32) -> Mat {
        let data: Vec<u8> = (0..h).flat_map(|y| (0..w).map(move |x| if (x / square + y / square) % 2 == 0 { 40 } else { 200 })).collect();
        Mat::new_rows_cols_with_data::<u8>(h, w, &data).unwrap().try_clone().unwrap()
    }

    fn point_bits(points: &[Point2f]) -> Vec<(u32, u32)> {
        points.iter().map(|p| (p.x.to_bits(), p.y.to_bits())).collect()
    }

    fn obs_bits(obs: &[Observation]) -> Vec<(u32, [u32; 4])> {
        obs.iter().map(|o| (o.id, [o.a[0].to_bits(), o.a[1].to_bits(), o.b[0].to_bits(), o.b[1].to_bits()])).collect()
    }

    #[test] fn global_selection_matches_good_features_to_track() {
        let mut rng = XorShift64::new(7);
        let mut images = vec![texture(960, 540, 1), texture(640, 360, 2), texture(64, 48, 3), checkerboard(320, 240, 8)];
        // A pyramid's first level is a view into a padded buffer, like the frames a replenish gets
        let padded = TrackingPyramid::new(&texture(960, 540, 4)).unwrap().layers.get(0).unwrap();
        images.push(padded);
        for img in &images {
            let (w, h) = (img.cols(), img.rows());
            let cache = CornerCache::new(img, (w, h)).unwrap();
            let full = Mat::new_rows_cols_with_default(h, w, CV_8UC1, Scalar::all(255.0)).unwrap();
            let mut masks = vec![full.clone(), Mat::new_rows_cols_with_default(h, w, CV_8UC1, Scalar::all(0.0)).unwrap()];
            for tracks in [50, 400, 2000] {
                let mut mask = full.clone();
                for _ in 0..tracks {
                    let centre = Point::new(rng.range(0.0, w as f64) as i32, rng.range(0.0, h as f64) as i32);
                    imgproc::circle(&mut mask, centre, rng.range(3.0, 12.0) as i32, Scalar::all(0.0), -1, imgproc::LINE_8, 0).unwrap();
                }
                masks.push(mask);
            }
            for (m, mask) in masks.iter().enumerate() {
                for max_corners in [1, 37, 300, 100_000] {
                    for quality in [QUALITY, 0.01, 0.2] {
                        for min_distance in [4.0, 5.333333333333333, 6.5, 8.0] {
                            let mut expected = Vector::<Point2f>::new();
                            imgproc::good_features_to_track(img, &mut expected, max_corners, quality, min_distance, mask, 7, false, 0.04).unwrap();
                            let actual = cache.select_global(img, mask, max_corners, quality, min_distance).unwrap();
                            assert_eq!(point_bits(&expected.to_vec()), point_bits(&actual),
                                "{w}x{h} mask {m} max_corners {max_corners} quality {quality} min_distance {min_distance}");
                        }
                    }
                }
            }
        }
    }

    /// Frames of a moving, turning and zooming texture, with a black frame, wider frames and a smaller size at the end
    fn moving_frames() -> Vec<GrayImage> {
        let canvas = texture(1800, 1100, 11);
        (0..50).map(|k| {
            let (w, h) = match k { 20 => return GrayImage::new(960, 540), 0..=29 => (960, 540), 30..=41 => (1920, 1080), _ => (640, 360) };
            let mut m = imgproc::get_rotation_matrix_2d(Point2f::new(900.0, 550.0), k as f64 * 0.15, 1.0 + 0.002 * k as f64).unwrap();
            *m.at_2d_mut::<f64>(0, 2).unwrap() += -300.0 + 2.3 * k as f64;
            *m.at_2d_mut::<f64>(1, 2).unwrap() += -200.0 + 1.1 * k as f64;
            let mut frame = Mat::default();
            imgproc::warp_affine_def(&canvas, &mut frame, &m, Size::new(w, h)).unwrap();
            GrayImage::from_raw(w as u32, h as u32, frame.data_bytes().unwrap().to_vec()).unwrap()
        }).collect()
    }

    #[test] fn cached_replenish_tracks_like_the_reference() {
        let frames = moving_frames();
        // The optical analysis' tracker, and the synchronization's (replenished below 85%, narrower)
        for (ratio, width) in [(1.0, 960), (0.85, 640)] {
            let mut cached = KltTracker::new(1500, ratio, width);
            let mut reference = KltTracker::new(1500, ratio, width);
            reference.reference = true;
            let mut tracked = 0;
            for (k, frame) in frames.iter().enumerate() {
                let (a, size_a) = cached.track(frame).unwrap();
                let (b, size_b) = reference.track(frame).unwrap();
                assert_eq!(size_a, size_b, "frame {k}");
                assert_eq!(obs_bits(&a), obs_bits(&b), "ratio {ratio} width {width} frame {k}");
                assert_eq!((point_bits(&cached.points), &cached.ids), (point_bits(&reference.points), &reference.ids), "frame {k}");
                tracked += a.len();
            }
            assert!(tracked > 10_000, "only {tracked} observations: the frames don't exercise the tracker");
        }
    }

    #[test] fn frames_prepared_ahead_track_like_the_reference() {
        let frames = moving_frames();
        let mut ahead = KltTracker::new(1500, 1.0, 960);
        let mut reference = KltTracker::new(1500, 1.0, 960);
        reference.reference = true;
        // All prepared first, as the pipeline's workers do, and the corners computed then
        let prepared: Vec<_> = frames.iter().map(|frame| KltTracker::prepare(frame, 960).map(|mut p| { p.precompute_corners(); p })).collect();
        for (k, (frame, prepared)) in frames.iter().zip(prepared).enumerate() {
            let (a, _) = ahead.track_prepared(prepared).unwrap();
            let (b, _) = reference.track(frame).unwrap();
            assert_eq!(obs_bits(&a), obs_bits(&b), "frame {k}");
        }
        assert!(KltTracker::prepare(&GrayImage::new(0, 0), 960).is_err_and(|e| e == "Empty frame 0x0"));
        assert!(ahead.track_prepared(Err("Empty frame 0x0".into())).is_err());
        assert!(ahead.prev.is_none() && ahead.points.is_empty(), "an error resets the tracker");
    }
}
