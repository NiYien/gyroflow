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

pub struct KltTracker {
    prev: Option<Mat>,
    size: (i32, i32),
    points: Vec<Point2f>,
    ids: Vec<u32>,
    next_id: u32,
    max_points: usize,
    replenish_ratio: f64,
    track_width: u32,
}

impl KltTracker {
    pub fn new(max_points: usize, replenish_ratio: f64, track_width: u32) -> Self {
        Self { prev: None, size: (0, 0), points: Vec::new(), ids: Vec::new(), next_id: 0, max_points, replenish_ratio, track_width }
    }

    fn track_frame(&mut self, img: &GrayImage) -> Result<(Vec<Observation>, (u32, u32)), opencv::Error> {
        let (iw, ih) = img.dimensions();
        let (tw, th) = tracking_size((iw, ih), self.track_width);
        // Borrows the pixels, no copy
        let src = Mat::new_rows_cols_with_data::<u8>(ih as i32, iw as i32, &img.as_raw()[..iw as usize * ih as usize])?;
        // The frame is kept as the next one's previous frame, which takes one copy: the downscaled frame, or a clone
        let cur = if (tw, th) == (iw, ih) {
            src.try_clone()?
        } else {
            let mut dst = Mat::default();
            imgproc::resize(&src, &mut dst, Size::new(tw as i32, th as i32), 0.0, 0.0, imgproc::INTER_AREA)?;
            dst
        };
        let (w, h) = (tw as i32, th as i32);
        if self.size != (w, h) { self.reset(); }
        self.size = (w, h);

        let mut out = Vec::new();
        if let Some(prev) = self.prev.take() {
            self.replenish(&prev)?;
            if !self.points.is_empty() {
                let pa: Vector<Point2f> = Vector::from_slice(&self.points);
                let mut pb = Vector::<Point2f>::new();
                let mut pa2 = Vector::<Point2f>::new();
                let mut st = Vector::<u8>::new();
                let mut st2 = Vector::<u8>::new();
                let mut err = Vector::<f32>::new();
                let criteria = TermCriteria::new(3 /* COUNT | EPS */, LK_MAX_ITERS, LK_EPS)?;
                let win = Size::new(LK_WIN, LK_WIN);
                video::calc_optical_flow_pyr_lk(&prev, &cur, &pa, &mut pb, &mut st, &mut err, win, LK_LEVELS, criteria, 0, 1e-4)?;
                video::calc_optical_flow_pyr_lk(&cur, &prev, &pb, &mut pa2, &mut st2, &mut err, win, LK_LEVELS, criteria, 0, 1e-4)?;

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
    /// share, then with whatever the cells without enough corners left, the strongest anywhere
    fn replenish(&mut self, img: &Mat) -> Result<(), opencv::Error> {
        if self.points.len() as f64 >= self.replenish_ratio * self.max_points as f64 { return Ok(()); }
        let (w, h) = self.size;
        let min_distance = (w.max(h) as f64 / 120.0).max(4.0);
        let mut mask = Mat::new_rows_cols_with_default(h, w, CV_8UC1, Scalar::all(255.0))?;
        for p in &self.points {
            imgproc::circle(&mut mask, Point::new(p.x as i32, p.y as i32), min_distance as i32, Scalar::all(0.0), -1, imgproc::LINE_8, 0)?;
        }
        self.mask_glare(img, &mut mask)?;
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

    /// Keeps new corners away from the sun and the like, see `GLARE_RADII`
    fn mask_glare(&self, img: &Mat, mask: &mut Mat) -> Result<(), opencv::Error> {
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
        if img.width() == 0 || img.height() == 0 {
            self.reset();
            return Err(format!("Empty frame {}x{}", img.width(), img.height()));
        }
        self.track_frame(img).map_err(|e| {
            self.reset();
            format!("OpenCV error: {e:?}")
        })
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
}
