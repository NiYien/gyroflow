// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

//! Persistent KLT tracks used by per-frame pure optical stabilization (Gyroflow 1.7.0).

use std::{ sync::mpsc, time::{ Duration, Instant } };
#[cfg(feature = "use-opencv")]
use opencv::{ core::{ self, Mat, Point2f, Size, TermCriteria, Vector, CV_8UC1 }, prelude::*, imgproc, video };
use super::{Observation, gpu_klt::GpuKlt};
use crate::synchronization::optical_motion::tracker::LocalCorners;

/// Grid the new corners are spread over, see `Tracker::replenish`
const GRID_ROWS: i32 = 6;
const GRID_COLS: i32 = 4;
/// What counts as a corner: its smaller eigenvalue against the strongest one in the frame
const QUALITY: f64 = 0.002;
/// ... or, in a cell with nothing that strong (cloudy sky), against the noise of flat areas, see `replenish`
const NOISE_MULTIPLE: f64 = 8.0;
/// No new corners within this many radii of a compact saturated blob (the sun): its glare is a smooth ring the tracker
/// slides along and its flare is fixed to the lens, neither moves like the scene. Blobs from this many pixels up, at
/// 960 px wide - glints on water are smaller, and the robust fit takes care of them among everything else
const GLARE_RADII: f64 = 2.5;
const GLARE_MIN_AREA: f64 = 30.0;
/// Saturated: above this
const GLARE_LEVEL: u8 = 249;
/// KLT's window, pixels: a point sees this much around it
pub(super) const WINDOW: i32 = 21;
/// ... the levels of its pyramid above the frame, when it stops: after this many iterations or a step this short (px),
/// and the smallest eigenvalue of a window it tracks (OpenCV's minEigThreshold, of the window's mean)
pub(super) const KLT_LEVELS: i32 = 3;
pub(super) const KLT_ITERATIONS: i32 = 50;
pub(super) const KLT_EPS: f64 = 0.001;
pub(super) const KLT_MIN_EIG: f64 = 1e-4;
/// How far a point may end up from where it started, tracked forward and back again, pixels
const FB_MAX: f32 = 0.1;
/// Without OpenCV there's no KLT but the GPU's
const NO_GPU: &str = "Optical analysis needs a GPU in this build";
/// Finding the GPU and setting it up taking longer than this means its driver hung: as if there were none
const SET_UP_TIMEOUT: Duration = Duration::from_secs(30);
/// Looks for the selected GPU adapter and makes its device and pipelines on a thread: the
/// first time in a process looking for an adapter can take seconds (the wgpu instance is made then, see `BACKENDS`),
/// and making the device and the pipelines a tenth of one. `found_gpu` waits for it
pub(super) fn find_gpu(selected: Option<usize>) -> mpsc::Receiver<Result<wgpu::Adapter, String>> {
    let (found, finding) = mpsc::channel();
    std::thread::spawn(move || {
        let adapter = crate::gpu::wgpu::WgpuWrapper::compute_adapter(selected).ok_or_else(|| "no GPU to use".to_string());
        let _ = found.send(adapter.and_then(|a| GpuKlt::prepare(&a).map(|_| a)));
    });
    finding
}

/// What `find_gpu` found, or why there's nothing
pub(super) fn found_gpu(finding: mpsc::Receiver<Result<wgpu::Adapter, String>>) -> Result<wgpu::Adapter, String> {
    finding.recv_timeout(SET_UP_TIMEOUT).unwrap_or_else(|e| Err(match e {
        mpsc::RecvTimeoutError::Timeout => format!("it took over {} s to set up", SET_UP_TIMEOUT.as_secs()),
        mpsc::RecvTimeoutError::Disconnected => "looking for it failed".into(),
    }))
}

pub struct Tracker {
    /// The frame before, when there is one: its pixels (without the padding of rows) when the CPU tracks, none when the
    /// GPU does - it keeps its own
    prev: Option<Vec<u8>>,
    size: (i32, i32),
    points: Vec<[f32; 2]>,
    ids: Vec<u32>,
    next_id: u32,
    max_points: usize,
    capture_texture: bool,
    texture: Vec<f32>,
    local_selection: bool,
    local_corners: Option<LocalCorners>,
    local_prepare_s: f64,
    local_select_s: f64,
    local_prepared_frames: usize,
    local_gpu_corner_frames: usize,
    /// KLT: the candidate corners of the frame before (see `local_maxima`, the strongest first), the corner strengths of
    /// every 8th pixel of it in both directions (its noise is a percentile of those) and its glare: what the next
    /// `replenish` picks from
    candidates: Vec<u64>,
    noise: Vec<f32>,
    glare: Vec<(i32, i32, i32)>,
    /// Whether `candidates` are sorted yet: from the GPU they're sorted only once needed, while it makes the next frame's
    /// levels
    sorted: bool,
    /// KLT on a GPU: whether it's wanted (not after it failed), its adapter or why there's none (looked for and its device
    /// made on a thread from the start, see `new`) and what's set up on it for the frame size
    gpu_wanted: bool,
    finding: Option<mpsc::Receiver<Result<wgpu::Adapter, String>>>,
    adapter: Result<wgpu::Adapter, String>,
    gpu: Option<GpuKlt>,
    /// Where the tracking ran, how long setting it up took and the tracking itself over how many frames, for the log
    device: String,
    setup: f64,
    busy: f64,
    frames: usize,
}

impl Tracker {
    pub fn new(max_points: usize, processing_device: i32) -> Self {
        use crate::synchronization::optical_motion::tracker::KltDevice;
        let finding = match KltDevice::for_processing_device(processing_device) {
            KltDevice::Cpu => None,
            KltDevice::Gpu(adapter) => Some(find_gpu(adapter)),
        };
        Self {
            prev: None, size: (0, 0), points: Vec::new(), ids: Vec::new(), next_id: 0, max_points,
            capture_texture: false, texture: Vec::new(),
            local_selection: false, local_corners: None, local_prepare_s: 0.0, local_select_s: 0.0,
            local_prepared_frames: 0, local_gpu_corner_frames: 0,
            candidates: Vec::new(), noise: Vec::new(), glare: Vec::new(), sorted: true,
            gpu_wanted: finding.is_some(), finding, adapter: Err("no GPU to use".into()), gpu: None, device: "the CPU".into(), setup: 0.0, busy: 0.0, frames: 0,
        }
    }

    pub fn with_texture(mut self, enabled: bool) -> Self {
        self.capture_texture = enabled;
        self
    }

    pub fn with_local_selection(mut self, enabled: bool) -> Self {
        self.local_selection = enabled;
        self
    }

    #[cfg(test)]
    pub(crate) fn uses_local_selection(&self) -> bool { self.local_selection }

    /// Where the tracking ran and how long it took a frame, for the log
    pub fn describe(&self) -> String {
        let setup = if self.setup > 0.0 { format!(" (set up in {:.0} ms)", self.setup * 1000.0) } else { String::new() };
        let selection = if self.local_selection {
            format!("; selection=local cpu_prepare_ms={:.1} cpu_select_ms={:.1} prepared_frames={} gpu_corner_frames={}",
                self.local_prepare_s * 1000.0, self.local_select_s * 1000.0, self.local_prepared_frames, self.local_gpu_corner_frames)
        } else { String::new() };
        format!("tracked on {} in {:.1} ms a frame{setup}{selection}", self.device, self.busy * 1000.0 / self.frames.max(1) as f64)
    }
    /// Forget everything: the next frame starts new tracks (a gap in the decoded frames, a cut)
    pub fn reset(&mut self) {
        self.prev = None;
        self.points.clear();
        self.ids.clear();
        self.local_corners = None;
    }

    /// Tracks the points of the previous frame into this one. Empty for the first frame
    pub fn track(&mut self, width: u32, height: u32, stride: usize, pixels: &[u8]) -> Result<Vec<Observation>, String> {
        let (w, h) = (width as i32, height as i32);
        if pixels.len() < stride * height as usize { return Ok(Vec::new()); }
        if self.size != (w, h) { self.reset(); }
        self.size = (w, h);
        if self.gpu_wanted && self.gpu.as_ref().is_none_or(|g| g.size != (width, height) || g.stride != stride as u32) {
            let started = Instant::now();
            let set_up = self.set_up_gpu(width, height, stride);
            self.setup += started.elapsed().as_secs_f64();
            set_up?;
        }

        let started = Instant::now();
        let mut out = Vec::new();
        let prev = self.prev.take();
        // On the GPU this frame goes up and its levels are made while the CPU picks the points to track into it:
        // the candidates of the frame before sorted and topped up from. All at once after that, the GPU sat idle
        // meanwhile, clocked down and took 1.5 times as long (a phone, Adreno 650)
        let levels = self.gpu.as_mut().map(|gpu| gpu.submit_levels(pixels));
        if prev.is_some() {
            if self.local_selection {
                let started = Instant::now();
                let mut points = self.points.iter().map(|p| Point2f::new(p[0], p[1])).collect();
                self.local_corners.as_ref().unwrap().replenish(&mut points, &mut self.ids, &mut self.next_id, self.max_points).map_err(cv)?;
                self.points = points.into_iter().map(|p| [p.x, p.y]).collect();
                self.local_select_s += started.elapsed().as_secs_f64();
            } else {
                self.sort_candidates();
                self.replenish();
            }
        }
        let tracking = prev.is_some() && !self.points.is_empty();
        let moved = self.klt_frame(prev.as_deref(), pixels, stride, tracking, levels)?;
        if let Some(moved) = moved {
            let mut points = Vec::with_capacity(self.points.len());
            let mut ids = Vec::with_capacity(self.points.len());
            for (i, b) in moved.into_iter().enumerate() {
                let Some(b) = b else { continue };
                let a = self.points[i];
                let inside = b[0] > 2.0 && b[1] > 2.0 && b[0] < (w - 3) as f32 && b[1] < (h - 3) as f32;
                if inside {
                    let fixed = |p: [f32; 2]| p.map(|v| (v * 512.0).round() / 512.0);
                    let texture = if self.capture_texture {
                        self.texture[(b[1].round() as i32 * w + b[0].round() as i32) as usize]
                    } else { f32::NAN };
                    out.push(Observation { id: self.ids[i], a: fixed(a), b: fixed(b), texture });
                    points.push(b);
                    ids.push(self.ids[i]);
                }
            }
            self.points = points;
            self.ids = ids;
        }
        self.prev = Some(if self.gpu.is_some() { Vec::new() } else { packed(pixels, stride, width as usize, height as usize) });
        self.busy += started.elapsed().as_secs_f64();
        self.frames += 1;
        Ok(out)
    }

    /// KLT on the GPU, for this frame size, once its adapter is found. On the CPU from here on when there's none or it
    /// can't be set up, and without OpenCV an error then
    fn set_up_gpu(&mut self, width: u32, height: u32, stride: usize) -> Result<(), String> {
        self.gpu = None;
        if let Some(finding) = self.finding.take() {
            self.adapter = found_gpu(finding);
        }
        // Room for every point: `replenish` tops each cell up to its share, so up to twice `max_points`, with fewer than
        // that before it
        let result = match &self.adapter {
            Ok(adapter) => GpuKlt::new(adapter, width, height, stride as u32, 2 * self.max_points),
            Err(e) => Err(e.clone()),
        };
        // The frame before, if any, was on the GPU: neither one set up again nor the CPU has it
        self.reset();
        match result {
            Ok(gpu) => {
                log::info!("Optical analysis: tracking on {}", gpu.name());
                self.device = gpu.name().to_string();
                self.gpu = Some(gpu);
                Ok(())
            },
            Err(e) => {
                self.gpu_wanted = false;
                self.device = "the CPU (the GPU couldn't be set up)".into();
                if !cfg!(feature = "use-opencv") { return Err(format!("{NO_GPU} ({e})")); }
                log::warn!("Optical analysis: tracking on the CPU, the GPU couldn't be set up for it: {e}");
                Ok(())
            }
        }
    }

    /// The candidates in the order `replenish` takes them: the strongest first
    fn sort_candidates(&mut self) {
        if self.sorted { return; }
        self.candidates.sort_unstable_by(|a, b| b.cmp(a));
        self.sorted = true;
    }

    /// KLT: the points of `prev` tracked into this frame (with `tracking`), and this frame's candidate corners, noise and
    /// glare found for the next `replenish`. On the GPU (`levels`: this frame's gone up, see `track`), the glare found
    /// while it works; with OpenCV when there's no GPU, and from the frame after the one it failed on
    fn klt_frame(&mut self, prev: Option<&[u8]>, pixels: &[u8], stride: usize, tracking: bool, levels: Option<Result<(), String>>) -> Result<Option<Vec<Option<[f32; 2]>>>, String> {
        let (w, h) = self.size;
        if let Some(gpu) = &mut self.gpu {
            let points = if tracking { &self.points[..] } else { &[] };
            let submitted = levels.unwrap_or_else(|| gpu.submit_levels(pixels)).and_then(|_| gpu.submit_lk(points.iter().copied()));
            self.glare = glare(pixels, stride, w, h);
            let finished = submitted.and_then(|_| gpu.finish());
            match finished {
                Ok(()) => {
                    // Taking them over gives `gpu` the old ones to fill again
                    std::mem::swap(&mut self.candidates, &mut gpu.candidates);
                    std::mem::swap(&mut self.noise, &mut gpu.noise);
                    self.sorted = false;
                    if self.local_selection {
                        let started = Instant::now();
                        self.local_corners = Some(LocalCorners::new(&frame_mat(pixels, stride, w, h).map_err(cv)?).map_err(cv)?);
                        self.local_prepare_s += started.elapsed().as_secs_f64();
                        self.local_prepared_frames += 1;
                        self.local_gpu_corner_frames += 1;
                    }
                    #[cfg(feature = "use-opencv")]
                    if self.capture_texture {
                        self.texture = corner_strengths(&frame_mat(pixels, stride, w, h).map_err(cv)?).map_err(cv)?
                            .data_typed::<f32>().map_err(cv)?.to_vec();
                    }
                    return Ok(tracking.then(|| checked(&self.points, &gpu.fwd, &gpu.bwd, FB_MAX)));
                },
                Err(e) if cfg!(feature = "use-opencv") => {
                    log::warn!("Optical analysis: tracking on the CPU from the next frame, the GPU failed: {e}");
                    self.gpu = None;
                    self.gpu_wanted = false;
                    self.device = "the CPU (the GPU failed)".into();
                    // The frame before is on the GPU only: its points are lost, new ones start on this frame
                    self.points.clear();
                    self.ids.clear();
                },
                Err(e) => return Err(format!("{NO_GPU}, and it failed: {e}")),
            }
        }
        let tracking = tracking && !self.points.is_empty();
        #[cfg(feature = "use-opencv")]
        return self.klt_frame_cpu(prev, pixels, stride, tracking).map_err(cv);
        #[cfg(not(feature = "use-opencv"))]
        { let _ = (prev, tracking); Err(NO_GPU.into()) }
    }

    /// `klt_frame` with OpenCV
    #[cfg(feature = "use-opencv")]
    fn klt_frame_cpu(&mut self, prev: Option<&[u8]>, pixels: &[u8], stride: usize, tracking: bool) -> opencv::Result<Option<Vec<Option<[f32; 2]>>>> {
        let (w, h) = self.size;
        let cur = frame_mat(pixels, stride, w, h)?;
        let moved = match prev {
            Some(prev) if tracking => Some(Self::klt(&frame_mat(prev, w as usize, w, h)?, &cur, &self.points, FB_MAX)?),
            _ => None,
        };
        if self.local_selection {
            let started = Instant::now();
            self.local_corners = Some(LocalCorners::new(&cur)?);
            self.local_prepare_s += started.elapsed().as_secs_f64();
            self.local_prepared_frames += 1;
            return Ok(moved);
        }
        let eig = corner_strengths(&cur)?;
        let eig = eig.data_typed::<f32>()?;
        self.candidates = local_maxima(eig, w, h);
        self.sorted = true;
        self.noise = noise_samples(eig, w, h);
        self.glare = glare(pixels, stride, w, h);
        if self.capture_texture { self.texture = eig.to_vec(); }
        Ok(moved)
    }

    /// Where each point went, by pyramidal Lucas-Kanade: None where it lost it, or didn't come back within `fb_max` px of
    /// where it started
    #[cfg(feature = "use-opencv")]
    pub(super) fn klt(prev: &Mat, cur: &Mat, points: &[[f32; 2]], fb_max: f32) -> opencv::Result<Vec<Option<[f32; 2]>>> {
        let pa: Vector<Point2f> = points.iter().map(|p| Point2f::new(p[0], p[1])).collect();
        let mut pb = Vector::<Point2f>::new();
        let mut pa2 = Vector::<Point2f>::new();
        let mut st = Vector::<u8>::new();
        let mut st2 = Vector::<u8>::new();
        let mut err = Vector::<f32>::new();
        let criteria = TermCriteria::new(3 /* COUNT | EPS */, KLT_ITERATIONS, KLT_EPS)?;
        let window = Size::new(WINDOW, WINDOW);
        video::calc_optical_flow_pyr_lk(prev, cur, &pa, &mut pb, &mut st, &mut err, window, KLT_LEVELS, criteria, 0, KLT_MIN_EIG)?;
        video::calc_optical_flow_pyr_lk(cur, prev, &pb, &mut pa2, &mut st2, &mut err, window, KLT_LEVELS, criteria, 0, KLT_MIN_EIG)?;
        (0..pa.len()).map(|i| {
            let (a, b, a2) = (pa.get(i)?, pb.get(i)?, pa2.get(i)?);
            let fb = ((a2.x - a.x).powi(2) + (a2.y - a.y).powi(2)).sqrt();
            Ok((st.get(i)? == 1 && st2.get(i)? == 1 && fb < fb_max).then_some([b.x, b.y]))
        }).collect()
    }

    /// Spread tracks across the readout so strongly textured regions do not monopolize
    /// the points. Weak cells use their noise floor to distinguish a corner from flat sky.
    /// The corners are picked as good_features_to_track picks them (once per cell, then once over the frame), from the
    /// candidates `klt_frame` found when the frame came: local maxima above the threshold, outside the cell's outermost
    /// pixels, the strongest first, each at least the minimum distance from the others
    fn replenish(&mut self) {
        if self.points.len() >= self.max_points { return; }
        let (w, h) = self.size;
        let min_distance = (w.max(h) as f64 / 120.0).max(4.0);
        let mut free = vec![true; (w * h) as usize];
        for p in &self.points { clear_disc(&mut free, w, h, p[0] as f64, p[1] as f64, min_distance); }
        for &(x, y, r) in &self.glare { clear_disc(&mut free, w, h, x as f64, y as f64, r as f64); }
        let candidates = std::mem::take(&mut self.candidates);
        let xy = |k: u64| ((k & 0xffff) as i32, (k >> 16 & 0xffff) as i32);
        let strength = |k: u64| f32::from_bits((k >> 32) as u32) as f64;
        let open = |free: &[bool], k: u64| { let (x, y) = xy(k); free[(y * w + x) as usize] };
        let strongest = |free: &[bool], list: &[u64]| list.iter().find(|&&k| open(free, k)).map(|&k| strength(k));

        // A flat frame: black, a fade
        let Some(strongest_all) = strongest(&free, &candidates) else { return };
        let threshold = QUALITY * strongest_all;
        // The floor of what a corner is where nothing stronger is around: a few times the noise of flat areas
        let Some(noise) = noise_level(&self.noise) else { return };
        let floor = NOISE_MULTIPLE * noise;
        let share = self.max_points / (GRID_ROWS * GRID_COLS) as usize;
        let col_of: Vec<i32> = (0..w).map(|x| (0..GRID_COLS).rev().find(|c| w * c / GRID_COLS <= x).unwrap_or(0)).collect();
        let row_of: Vec<i32> = (0..h).map(|y| (0..GRID_ROWS).rev().find(|r| h * r / GRID_ROWS <= y).unwrap_or(0)).collect();
        let mut cells = vec![Vec::new(); (GRID_ROWS * GRID_COLS) as usize];
        for &k in &candidates {
            let (x, y) = xy(k);
            cells[(row_of[y as usize] * GRID_COLS + col_of[x as usize]) as usize].push(k);
        }
        for r in 0..GRID_ROWS {
            for c in 0..GRID_COLS {
                let (x0, y0, x1, y1) = (w * c / GRID_COLS, h * r / GRID_ROWS, w * (c + 1) / GRID_COLS, h * (r + 1) / GRID_ROWS);
                let has = self.points.iter().filter(|p| { let (x, y) = (p[0] as i32, p[1] as i32); x >= x0 && y >= y0 && x < x1 && y < y1 }).count();
                if has >= share { continue; }
                let list = &cells[(r * GRID_COLS + c) as usize];
                let Some(cell_strongest) = strongest(&free, list) else { continue };
                let cell_threshold = threshold.min(floor.max(QUALITY * cell_strongest));
                if cell_threshold <= 0.0 || cell_strongest < cell_threshold { continue; }
                let mut wanted = share - has;
                for &k in list {
                    if strength(k) <= cell_threshold { break; }
                    let (x, y) = xy(k);
                    if x == x0 || y == y0 || x == x1 - 1 || y == y1 - 1 || !open(&free, k) { continue; }
                    clear_disc(&mut free, w, h, x as f64, y as f64, min_distance);
                    self.add([x as f32, y as f32]);
                    wanted -= 1;
                    if wanted == 0 { break; }
                }
            }
        }
        if self.points.len() >= self.max_points { return; }
        let Some(now_strongest) = strongest(&free, &candidates) else { return };
        let threshold = QUALITY * now_strongest;
        for &k in &candidates {
            if strength(k) <= threshold || self.points.len() >= self.max_points { break; }
            if !open(&free, k) { continue; }
            let (x, y) = xy(k);
            clear_disc(&mut free, w, h, x as f64, y as f64, min_distance);
            self.add([x as f32, y as f32]);
        }
    }

    fn add(&mut self, p: [f32; 2]) {
        self.points.push(p);
        self.ids.push(self.next_id);
        self.next_id = self.next_id.wrapping_add(1);
    }
}

#[cfg(feature = "use-opencv")]
pub(super) fn cv(e: opencv::Error) -> String { format!("OpenCV error: {e}") }

/// A frame's pixels as a Mat, borrowed: kept no longer than `pixels`
#[cfg(feature = "use-opencv")]
pub(super) fn frame_mat(pixels: &[u8], stride: usize, w: i32, h: i32) -> opencv::Result<Mat> {
    unsafe { Mat::new_size_with_data_unsafe(Size::new(w, h), CV_8UC1, pixels.as_ptr() as *mut std::ffi::c_void, stride) }
}

/// A frame's corner strengths, as `gpu_klt.wgsl` makes them: cornerMinEigenVal, Sobel 3, block 7
#[cfg(feature = "use-opencv")]
pub(super) fn corner_strengths(frame: &Mat) -> opencv::Result<Mat> {
    let mut eig = Mat::default();
    imgproc::corner_min_eigen_val(frame, &mut eig, 7, 3, core::BORDER_DEFAULT)?;
    Ok(eig)
}

/// A frame's pixels without the padding of its rows
fn packed(pixels: &[u8], stride: usize, w: usize, h: usize) -> Vec<u8> {
    if stride == w { return pixels[..w * h].to_vec(); }
    (0..h).flat_map(|y| &pixels[y * stride..y * stride + w]).copied().collect()
}

/// Where the GPU's KLT took each point, with `klt`'s forward-backward check
pub(super) fn checked(points: &[[f32; 2]], fwd: &[[f32; 4]], bwd: &[[f32; 4]], fb_max: f32) -> Vec<Option<[f32; 2]>> {
    points.iter().zip(fwd.iter().zip(bwd)).map(|(a, (f, b))| {
        let fb = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
        (f[2] > 0.5 && b[2] > 0.5 && fb < fb_max).then_some([f[0], f[1]])
    }).collect()
}

/// Where new corners stay away from the sun and the like: centre and radius, see `GLARE_RADII`. Only compact blobs,
/// not too large: the edges of an overexposed white wall are as good as any. The blobs are 8-connected components of
/// the saturated pixels (as OpenCV's connectedComponentsWithStats finds them), joined from the runs of them in each
/// row. Most frames have too few of those for even one blob, and cost a pass that counts them a row at a time
/// (vectorized: about 0.1 ms at 960x540); only the rows with any are looked at again. On one thread: it runs while the
/// GPU tracks, see `klt_frame`, and rayon was what made the tracking slow with every core busy (`local_maxima`)
fn glare(pixels: &[u8], stride: usize, w: i32, h: i32) -> Vec<(i32, i32, i32)> {
    let (wu, hu) = (w as usize, h as usize);
    let (wf, hf) = (w as f64, h as f64);
    let min_area = GLARE_MIN_AREA * (wf / 960.0).powi(2);
    let row = |y: usize| &pixels[y * stride..y * stride + wu];
    let counts: Vec<u32> = (0..hu).map(|y| row(y).iter().map(|&p| (p > GLARE_LEVEL) as u32).sum()).collect();
    if (counts.iter().sum::<u32>() as f64) < min_area { return Vec::new(); }

    // The runs (y, from x, to x past the run), each with the blob it's in: the run it's joined to, first in the frame
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    let mut parent: Vec<u32> = Vec::new();
    fn root(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            parent[i as usize] = parent[parent[i as usize] as usize];
            i = parent[i as usize];
        }
        i
    }
    let mut above = 0..0;
    for y in 0..hu {
        let start = runs.len();
        if counts[y] > 0 {
            let r = row(y);
            let mut x = 0;
            while let Some(skip) = r[x..].iter().position(|&p| p > GLARE_LEVEL) {
                let x0 = x + skip;
                x = x0 + r[x0..].iter().position(|&p| p <= GLARE_LEVEL).unwrap_or(wu - x0);
                let i = runs.len() as u32;
                runs.push((y as u32, x0 as u32, x as u32));
                parent.push(i);
                // 8-connected to the runs above that come within a pixel of it
                for j in above.clone() {
                    let (_, a0, a1) = runs[j];
                    if a0 <= x as u32 && x0 as u32 <= a1 {
                        let (ri, rj) = (root(&mut parent, i), root(&mut parent, j as u32));
                        if ri != rj { parent[ri.max(rj) as usize] = ri.min(rj); }
                    }
                }
                if x >= wu { break; }
            }
        }
        above = start..runs.len();
    }
    // Each blob's area and bounding box (the right and the bottom edge past it)
    let mut blobs = vec![(0u64, u32::MAX, u32::MAX, 0u32, 0u32); runs.len()];
    for i in 0..runs.len() {
        let (y, x0, x1) = runs[i];
        let b = &mut blobs[root(&mut parent, i as u32) as usize];
        *b = (b.0 + (x1 - x0) as u64, b.1.min(x0), b.2.min(y), b.3.max(x1), b.4.max(y + 1));
    }
    blobs.into_iter().filter(|b| b.0 > 0).filter_map(|(area, bx, by, x_end, y_end)| {
        let (area, bx, by, bw, bh) = (area as f64, bx as f64, by as f64, (x_end - bx) as f64, (y_end - by) as f64);
        // The frame's edge may cut it (a half disc is twice as wide as tall): its size is the larger side then
        let cut = bx <= 0.0 || by <= 0.0 || bx + bw >= wf || by + bh >= hf;
        let compact = area >= 0.5 * bw * bh && bw.max(bh) <= if cut { 3.0 } else { 2.0 } * bw.min(bh);
        let radius = bw.max(bh) / 2.0;
        (area >= min_area && compact && radius <= wf / 12.0).then(|| ((bx + bw / 2.0) as i32, (by + bh / 2.0) as i32, (GLARE_RADII * radius) as i32))
    }).collect()
}

/// The candidate corners in a frame's corner strengths (cornerMinEigenVal's, `w` x `h`): the pixels inside the outermost
/// ones whose strength is above zero and none of its 8 neighbours', the strongest first, as `bits of the strength << 32
/// | y << 16 | x` - the strengths are positive, so their bits sort as they do, and a tie goes to the later pixel, as in
/// good_features_to_track. `gpu_klt.wgsl`'s `corners` finds the same.
/// On one thread, as the sort of the GPU's: with every core busy (decoding, the analysis' own fits) rayon's joins wait on
/// threads descheduled elsewhere, which took the GPU's tracking from 13 to 27 ms a frame
pub(super) fn local_maxima(eig: &[f32], w: i32, h: i32) -> Vec<u64> {
    let (w, h) = (w as usize, h as usize);
    let mut out = Vec::new();
    for y in 1..h.saturating_sub(1) {
        let (up, row, down) = (&eig[(y - 1) * w..y * w], &eig[y * w..(y + 1) * w], &eig[(y + 1) * w..(y + 2) * w]);
        for x in 1..w.saturating_sub(1) {
            let v = row[x];
            if v > 0.0 && row[x - 1] <= v && row[x + 1] <= v && up[x - 1..=x + 1].iter().all(|&n| n <= v) && down[x - 1..=x + 1].iter().all(|&n| n <= v) {
                out.push((v.to_bits() as u64) << 32 | (y as u64) << 16 | x as u64);
            }
        }
    }
    out.sort_unstable_by(|a, b| b.cmp(a));
    out
}

/// The corner strengths of every 8th pixel in both directions, the noise of flat areas is found in
#[cfg(feature = "use-opencv")]
fn noise_samples(eig: &[f32], w: i32, h: i32) -> Vec<f32> {
    (0..h as usize).step_by(8).flat_map(|y| (0..w as usize).step_by(8).map(move |x| eig[y * w as usize + x])).collect()
}

/// The noise of flat areas in a frame's corner strengths (`noise_samples`): their 10th percentile - sky, water - and
/// higher still in a frame without any. Areas with none at all (clipped highlights, black bars) aren't noise: a
/// blown-out sky took it to 0, and with it every floor made of it. None for a frame with no detail at all
fn noise_level(samples: &[f32]) -> Option<f64> {
    let mut v: Vec<f32> = samples.iter().copied().filter(|&e| e > 0.0).collect();
    if v.is_empty() { return None; }
    let i = v.len() / 10;
    Some(*v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1 as f64)
}

/// Takes a disc out of `free` (`w` x `h`): new corners stay out of it. The pixels closer than `r` to the centre, as
/// good_features_to_track keeps corners apart: one exactly the minimum distance away is fine
pub(super) fn clear_disc(free: &mut [bool], w: i32, h: i32, cx: f64, cy: f64, r: f64) {
    for y in ((cy - r).floor() as i32 + 1).max(0)..=((cy + r).ceil() as i32 - 1).min(h - 1) {
        let half = (r * r - (y as f64 - cy).powi(2)).sqrt();
        let (x0, x1) = (((cx - half).floor() as i32 + 1).max(0), ((cx + half).ceil() as i32 - 1).min(w - 1));
        if x0 <= x1 { free[(y * w + x0) as usize..=(y * w + x1) as usize].fill(false); }
    }
}

#[cfg(all(test, feature = "use-opencv"))]
mod tests {
    use super::*;

    fn texture() -> Vec<u8> {
        let mut seed = 91u32;
        (0..320 * 180).map(|_| {
            seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5;
            (seed % 240) as u8
        }).collect()
    }

    #[test]
    fn translation_texture_is_the_current_corner_response_without_changing_tracks() {
        let image = texture();
        let mut plain = Tracker::new(1500, -1);
        let mut measured = Tracker::new(1500, -1).with_texture(true);
        assert!(plain.track(320, 180, 320, &image).unwrap().is_empty());
        assert!(measured.track(320, 180, 320, &image).unwrap().is_empty());
        let a = plain.track(320, 180, 320, &image).unwrap();
        let b = measured.track(320, 180, 320, &image).unwrap();
        assert!(a.len() > 100);
        let eig = corner_strengths(&frame_mat(&image, 320, 320, 180).unwrap()).unwrap();
        for (a, b) in a.iter().zip(&b) {
            assert_eq!((a.id, a.a, a.b), (b.id, b.a, b.b));
            assert!(a.texture.is_nan());
            assert_eq!(b.texture, *eig.at_2d::<f32>(b.b[1].round() as i32, b.b[0].round() as i32).unwrap());
        }
        measured.reset();
        assert!(measured.track(320, 180, 320, &image).unwrap().is_empty());
        let after = measured.track(320, 180, 320, &image).unwrap();
        assert!(after.iter().all(|o| o.id > b.iter().map(|o| o.id).max().unwrap()));
        measured.adapter = Err("test initialization failure".into());
        measured.set_up_gpu(320, 180, 320).unwrap();
        assert!(!measured.gpu_wanted);
        assert!(measured.track(320, 180, 320, &image).unwrap().is_empty());
        let recovered = measured.track(320, 180, 320, &image).unwrap();
        assert!(recovered.iter().all(|o| o.id > after.iter().map(|o| o.id).max().unwrap()));
    }

    #[test]
    fn local_selection_keeps_ids_unique_across_reset_and_size_change() {
        let image = texture();
        let mut tracker = Tracker::new(1500, -1).with_local_selection(true);
        assert!(tracker.track(320, 180, 320, &image).unwrap().is_empty());
        let first = tracker.track(320, 180, 320, &image).unwrap();
        assert!(first.len() > 100 && first.iter().all(|o| o.texture.is_nan()));
        let second = tracker.track(320, 180, 320, &image).unwrap();
        assert!(second.iter().any(|b| first.iter().any(|a| a.id == b.id)));
        let maximum = first.iter().chain(&second).map(|o| o.id).max().unwrap();
        tracker.reset();
        assert!(tracker.track(320, 180, 320, &image).unwrap().is_empty());
        let after = tracker.track(320, 180, 320, &image).unwrap();
        assert!(after.iter().all(|o| o.id > maximum));
        let maximum = after.iter().map(|o| o.id).max().unwrap();
        assert!(tracker.track(160, 90, 160, &image[..160 * 90]).unwrap().is_empty());
        let smaller = tracker.track(160, 90, 160, &image[..160 * 90]).unwrap();
        assert!(!smaller.is_empty() && smaller.iter().all(|o| o.id > maximum));
    }

}

