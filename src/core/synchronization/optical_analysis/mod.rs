// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>
// Ported from upstream gyroflow 322cb312 + eabdc789

//! "Analyze image optically": measures, from the video itself, the rotation the motion data got wrong, and fits the
//! correction the quaternions get (`gyro_source::OpticalCorrection`).
//!
//! Source frames are tracked at an integer interval near 25 fps (KLT, persistent tracks). For each tracked point the quaternions predict where
//! it should be in the next frame - rolling shutter included, each point at its own row's time - and the residual is
//! what they got wrong, plus the parallax of the camera's translation. Parallax changes slowly along a track while
//! the errors this is about don't, so a temporal high-pass of each track's residuals leaves the rotation error alone,
//! with no depth or translation to estimate. The points of each band of rows of each frame pair then fit one small
//! rotation, and `solver` turns those into a spline δ(t), rolling shutter resolution and all.
//!
//! The quaternions stay in charge of the slow motion (the image's own estimate of it would drift, and mixes rotation
//! up with translation); the image corrects them where it disagrees, which on a camera with good motion data is
//! nowhere

pub mod solver;
pub mod odometry;
mod base;
pub use base::{OpticalBaseMode, BlendConfig};
#[cfg(test)] pub(crate) mod synthetic;
pub mod translation;
#[cfg(test)] mod translation_stress;
pub(crate) mod sensor;
mod sensor_solver;

use std::collections::{ HashMap, VecDeque };
use std::sync::{ Arc, atomic::{ AtomicBool, AtomicU64, Ordering::{ Relaxed, SeqCst } } };
use nalgebra::{ DMatrix, Matrix3, Rotation3, UnitQuaternion, Vector3 };
use rayon::prelude::*;

use crate::StabilizationManager;
use crate::gyro_source::{ GyroSource, OpticalCorrection, OpticalCorrectionSettings, TimeQuat, optical_correction::{ self, Fnv } };
use crate::stabilization::{ ComputeParams, FrameTransform, undistort_points_to_plane };
use solver::{ BandMeasurement, SolverParams };
use translation::{ PairPoint, TranslationSolver };
#[cfg(feature = "use-opencv")]
use translation::TranslationSolverConfig;
use crate::gyro_source::optical_translation::TranslationSample;
use super::optical_motion::tracks::Observation;
#[cfg(feature = "use-opencv")]
use super::optical_motion::{FrameTracker, tracker::KltTracker};
#[cfg(feature = "use-opencv")]
use super::GrayImage;

/// Tracks kept alive per frame
#[cfg(feature = "use-opencv")]
const MAX_POINTS: usize = 1500;
/// Bands of rows (along the readout) each frame pair is measured in
const BANDS: usize = 6;
/// Frame pairs measured at once
const CHUNK: usize = 240;
const MIN_BAND_POINTS: usize = 25;
/// Floor of the uncertainty of one band's rotation, in pixels of the tracked frame
const SIGMA_FLOOR_PX: f64 = 0.01;

#[derive(Clone, Copy, Debug)]
struct Frame {
    index: usize,
    timestamp_ms: f64,
    /// Time of the first tracked row (or column), and per tracked pixel along the readout: `FrameTransform::at_timestamp`
    start_ms: f64,
    per_px_ms: f64,
    /// Middle of the readout: the frame's own time
    #[cfg_attr(not(feature = "use-opencv"), allow(dead_code))]
    mid_ms: f64,
}

struct Pair {
    seq: usize,
    a: Frame,
    b: Frame,
    obs: Vec<Observation>,
}

/// One tracked point in one frame pair, against the uncorrected quaternions
struct Derived {
    id: u32,
    seq: usize,
    band: u8,
    /// Where the quaternions put the point in the second frame, and how far off that was (quaternion frame)
    p: Vector3<f64>,
    r: Vector3<f64>,
    ta_ms: f64,
    tb_ms: f64,
}

/// What the image measured: kept (in memory) so a change of the settings refits the correction in a fraction of a
/// second instead of another pass over the video
pub struct OpticalMeasurements {
    pub bands: Vec<BandMeasurement>,
    pub stab_requested: bool,
    pub(crate) stab_pairs: Vec<sensor::SensorPair>,
    pub(crate) stab_bands: Vec<sensor::SensorBand>,
    pub translation_requested: bool,
    pub translation_samples: Vec<TranslationSample>,
    /// Actual analysis cadence in motion-data time, after integer frame sampling.
    pub scaled_fps: f64,
    /// Of the quaternions they were measured against, see `OpticalCorrection::quats_checksum`
    pub quats_checksum: u64,
    /// And of the rest they were measured with, see `context_checksum`
    pub context_checksum: u64,
    /// For a file without motion data, the orientation measured against, see `OpticalCorrection::video_base`
    pub video_base: Vec<(i64, [f32; 4])>,
    pub frames: usize,
    pub measured_frames: usize,
    /// `StabilizationManager::optical_generation` when the analysis started: they're only for what was loaded then
    pub generation: u64,
}

/// The solver's parameters for the user's strength: it moves the hand-over to the motion data (the ridge, relative
/// to what the image measured) over three decades, from only the fast errors of a vibrating gyro up to the image
/// overriding the motion data down to about a tenth of a Hz. The correction always works per row: both kinds of damage
/// seen (vibration, a gyro whose gain collapses during a roll) change within a frame, and one correction per frame did
/// worse on every clip tried
pub fn solver_params(settings: &OpticalCorrectionSettings, scaled_fps: f64) -> SolverParams {
    SolverParams {
        spacing_us: 1_000_000.0 / scaled_fps.max(1.0) / 6.0,
        ridge: 10f64.powf(-2.0 - 3.0 * settings.strength.clamp(0.0, 1.0)),
        ..Default::default()
    }
}

/// Fits the correction to the measurements
pub fn solve(m: &OpticalMeasurements, settings: &OpticalCorrectionSettings) -> Result<OpticalCorrection, String> {
    solve_with(m, settings, &solver_params(settings, m.scaled_fps))
}

pub fn solve_with(m: &OpticalMeasurements, settings: &OpticalCorrectionSettings, params: &SolverParams) -> Result<OpticalCorrection, String> {
    let bands = &m.bands;
    let sol = solver::solve(bands, params).ok_or_else(|| "Not enough of the image could be tracked".to_string())?;
    let rms_deg = (bands.iter().map(|b| sol.at(b.ta_us, params.spacing_us).norm_squared()).sum::<f64>() / bands.len().max(1) as f64).sqrt().to_degrees();
    Ok(OpticalCorrection {
        enabled: true,
        settings: *settings,
        start_us: sol.start_us,
        spacing_us: params.spacing_us,
        coeffs: sol.coeffs.iter().map(|c| [c.x as f32, c.y as f32, c.z as f32]).collect(),
        quats_checksum: m.quats_checksum,
        context_checksum: m.context_checksum,
        video_base: m.video_base.clone(),
        frames: m.frames,
        measured_frames: m.measured_frames,
        rms_deg,
    })
}

/// Reconstructs the sensor correction against the same uncorrected motion used during analysis.
pub fn solve_stab(m: &OpticalMeasurements, quats: &TimeQuat, config: &crate::gyro_source::optical_stab::StabReconConfig) -> Result<crate::gyro_source::optical_stab::OpticalStabReconstruction, String> {
    solve_stab_with_cancel(m, quats, config, || false)
}

pub(crate) fn solve_stab_with_cancel(m: &OpticalMeasurements, quats: &TimeQuat, config: &crate::gyro_source::optical_stab::StabReconConfig, cancel: impl Fn() -> bool) -> Result<crate::gyro_source::optical_stab::OpticalStabReconstruction, String> {
    use crate::gyro_source::optical_stab::OpticalStabReconstruction;
    if cancel() { return Err("Cancelled".into()); }
    if optical_correction::checksum(quats) != m.quats_checksum {
        log::warn!("Sensor reconstruction rejected: motion checksum changed after analysis");
        return Err("Motion data changed after analysis; analyze again".into());
    }
    let params = SolverParams { spacing_us: 1e6 / m.scaled_fps.max(1.0) / 6.0, ridge: 1e-5, ..Default::default() };
    let mut fit = sensor_solver::solve_sensor(&m.stab_pairs, &m.stab_bands, quats, config.cutoff_hz, &params, &cancel).map_err(|error| {
        log::warn!("Sensor reconstruction failed: {error:?}");
        match error {
            sensor_solver::SensorSolveError::NoMeasurements => "Not enough of the image could be tracked".to_string(),
            sensor_solver::SensorSolveError::Cancelled => "Cancelled".to_string(),
            _ => format!("Sensor reconstruction failed: {error:?}"),
        }
    })?;
    log::info!("Sensor reconstruction converged: iterations={} initial_cost={} final_cost={} max_step_px={} knots={} band_bytes={}", fit.iterations, fit.initial_cost, fit.final_cost, fit.max_step_px, fit.solution.coeffs.len(), fit.band_bytes);
    #[cfg(test)]
    if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {
        println!("sensor solved test={} iterations={} initial_cost={} final_cost={} max_step_px={} knots={} band_bytes={}",std::thread::current().name().unwrap_or("unnamed"),fit.iterations,fit.initial_cost,fit.final_cost,fit.max_step_px,fit.solution.coeffs.len(),fit.band_bytes);
    }
    if cancel() { return Err("Cancelled".into()); }
    let mut result = OpticalStabReconstruction::default();
    result.enabled = true;
    result.start_us = fit.solution.start_us;
    result.spacing_us = params.spacing_us;
    result.coeffs = fit.solution.coeffs.iter().map(|v| [v.x as f32, v.y as f32, v.z as f32]).collect();
    result.cutoff_hz = fit.cutoff_hz;
    result.quats_checksum = m.quats_checksum;
    result.context_checksum = m.context_checksum;
    result.frames = m.frames;
    result.measured_frames = fit.measured_pairs;
    result.rebuild_with_prior(&mut fit.prior_sampler,config,&cancel).map_err(|error| {
        log::warn!("Sensor reconstruction rebuild failed: {error:?}");
        if error==crate::gyro_source::optical_stab::PriorError::Cancelled {"Cancelled".to_string()} else {format!("Sensor reconstruction prior failed: {error:?}")}
    })?;
    log::debug!("Sensor prior cache nodes={} panels={} logs={} contributions={}",fit.prior_sampler.cached_nodes(),fit.prior_sampler.panels,fit.prior_sampler.log_evaluations,fit.prior_sampler.contributions);
    #[cfg(test)]
    if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {println!("sensor prior cache nodes={} panels={} logs={} contributions={}",fit.prior_sampler.cached_nodes(),fit.prior_sampler.panels,fit.prior_sampler.log_evaluations,fit.prior_sampler.contributions);}
    if cancel() { return Err("Cancelled".into()); }
    Ok(result)
}

/// The parameters the analysis measures with: the render's, minus what's about the output picture (keyframes, a lens
/// correction below 100%), and with the frames the right way up. The analysis gets them from the decoder, whatever way
/// up the preview's framebuffer is (OpenGL's is upside down, `framebuffer_inverted`), and the point undistortion never
/// reads that flag - left on, it flipped the rolling shutter and the axes against pictures that weren't flipped
pub fn measurement_params(stab: &StabilizationManager) -> ComputeParams {
    let mut params = ComputeParams::from_manager(stab);
    params.keyframes.clear();
    params.lens_correction_amount = 1.0;
    params.framebuffer_inverted = false;
    params.apply_optical_translation = false;
    params.apply_optical_stab = false;
    params
}

/// Fingerprint of what a measurement depends on besides the quaternions (`OpticalCorrection::context_checksum`): when
/// each tracked pixel was read out - the frame timing, the rolling shutter, the sync - and which ray it came from - the
/// lens, with all of its per-frame data (`undistort_points_to_plane`). Taken from what these give at a few
/// frames rather than from the settings behind them, so every setting that moves a pixel's time or ray is in it,
/// including ones this doesn't know of, and none that doesn't is. Rounded far below anything that matters (1 µs,
/// 1 µrad), so the last bit of a computation elsewhere doesn't make a correction stale. The sync points go in as they
/// are: one anywhere in the clip moves the frames around it. Takes the `gyro` lock: not for a caller that holds it
pub fn context_checksum(params: &ComputeParams) -> u64 {
    let mut h = Fnv::default();
    h.eat(params.width as u64);
    h.eat(params.height as u64);
    h.eat_rounded(params.scaled_fps, 1e6);
    for (ts, offset) in params.gyro.read().get_offsets() {
        h.eat(*ts as u64);
        h.eat_rounded(*offset, 1e3);
    }
    let size = (params.width as u32, params.height as u32);
    let rows = if params.frame_readout_direction.is_horizontal() { size.0 } else { size.1 };
    let at = [0.1f32, 0.5, 0.9];
    let grid: Vec<(f32, f32)> = at.iter().flat_map(|y| at.map(|x| (x * size.0 as f32, y * size.1 as f32))).collect();
    let fps = params.scaled_fps.max(1e-9);
    for t in at {
        let index = crate::frame_at_timestamp(t as f64 * params.scaled_duration_ms, fps).max(0) as usize;
        let ts = crate::timestamp_at_frame(index as i32, fps);
        let frame = frame_timing(params, index, ts, rows);
        h.eat(index as u64);
        h.eat_rounded(frame.start_ms, 1e3);
        h.eat_rounded(frame.per_px_ms * rows as f64, 1e3);
        for b in undistort_points_to_plane(&grid, ts, index, params, size) {
            match b {
                Some(b) => {
                    let b = to_quat_frame(b);
                    for v in [b.x, b.y, b.z] { h.eat_rounded(v, 1e6); }
                },
                None => h.eat(u64::MAX),
            }
        }
    }
    h.0
}

/// When the frame `index` is read out, over `rows` rows (or columns) of the tracked picture: the rolling shutter timing
/// of `FrameTransform::at_timestamp`
fn frame_timing(params: &ComputeParams, index: usize, timestamp_ms: f64, rows: u32) -> Frame {
    let gyro = params.gyro.read();
    let md = gyro.file_metadata.read();
    let readout = FrameTransform::get_frame_readout_time(params, false, timestamp_ms, &md);
    let ts = timestamp_ms + md.per_frame_time_offsets.get(index).unwrap_or(&0.0);
    Frame { index, timestamp_ms, start_ms: ts - readout / 2.0, per_px_ms: readout / rows.max(1) as f64, mid_ms: ts }
}

/// A bearing from the camera's frame into the quaternions' frame: the axis flips `FrameTransform::at_timestamp` applies
/// (to upright frames, see `measurement_params`)
fn to_quat_frame(b: (f32, f32)) -> Vector3<f64> {
    Vector3::new(b.0 as f64, -b.1 as f64, -1.0).normalize()
}

struct TranslationState {
    solver: TranslationSolver,
    next_seq: usize,
    pairs: HashMap<usize, (Vector3<f64>, HashMap<u32, f64>)>,
    position: Vector3<f64>,
    segment: u32,
    samples: Vec<TranslationSample>,
}

pub struct OpticalMotionAnalysis {
    /// Its `gyro` is a copy of the motion data without any optical correction: that's what the new one is measured against
    params: ComputeParams,
    fps_scale: Option<f64>,
    scaled_fps: f64,
    every_nth_frame: usize,
    hp_min: usize,
    hp_max: usize,
    quats_checksum: u64,
    context_checksum: u64,
    horizontal_readout: bool,
    track_size: (u32, u32),
    focal_px: f64,
    #[cfg(feature = "use-opencv")]
    tracker: KltTracker,
    last: Option<Frame>,
    pairs: VecDeque<Pair>,
    next_seq: usize,
    measured_upto: usize,
    measurements: Vec<BandMeasurement>,
    measured_pairs: usize,
    frames: usize,
    total_frames: usize,
    /// The part of the clip analyzed: the trim ranges, in the file's own milliseconds
    ranges_ms: Vec<(f64, f64)>,
    /// A file without motion data: the rotation between each two frames, measured from their tracks and chained, at the
    /// frames' own times. What the rest of the analysis compares the image against instead of the quaternions
    vision: Option<TimeQuat>,
    translation: Option<TranslationState>,
    stab_requested: bool,
    stab_next_seq: usize,
    stab_pairs: Vec<sensor::SensorPair>,
    stab_bands: Vec<sensor::SensorBand>,
    sensor_projections: HashMap<(usize, u64, (u32, u32)), Arc<crate::stabilization::SensorProjection>>,
    /// The robust rotation blended towards the odometry as its structure and leverage warrant.
    base: base::VisionBase,
    sg_cache: HashMap<usize, DMatrix<f64>>,
    cancel_flag: Arc<AtomicBool>,
    /// `StabilizationManager::optical_generation`, and its value when this started: another file, a project or Clear
    /// move it on, and cancel what's running for the one before
    generation: (Arc<AtomicU64>, u64),
    /// Latched: the cancel flag is shared, and something else may lower it again before the analysis ends
    cancelled: AtomicBool,
}

impl OpticalMotionAnalysis {
    pub fn from_manager(stab: &StabilizationManager, cancel_flag: Arc<AtomicBool>) -> Result<Self, String> {
        let ui = *stab.optical_ui.read();
        ui.validate_analysis_request()?;
        #[cfg(not(feature = "use-opencv"))]
        { let _ = (stab, cancel_flag); return Err("Optical analysis is not available in this build".into()); }

        #[cfg(feature = "use-opencv")]
        {
            // Before anything is read: whatever changes it from here on is a reason to stop
            let generation = stab.optical_generation.load(SeqCst);
            let mut params = measurement_params(stab);

            let mut gyro = stab.gyro.read().clone();
            // Without motion data in the file (also after an analysis of such a file: its orientation is only in the
            // correction) the analysis measures the motion itself
            let vision = (!gyro.file_metadata.read().has_motion()).then(TimeQuat::new);
            if gyro.optical_correction.take().is_some() {
                gyro.integrate();
            }
            if vision.is_some() {
                gyro.quaternions.clear();
            } else if gyro.quaternions.len() < 2 {
                return Err("No motion data to correct".into());
            }
            let quats_checksum = optical_correction::checksum(&gyro.quaternions);
            params.gyro = Arc::new(parking_lot::RwLock::new(gyro));
            let context_checksum = context_checksum(&params);

            let (fps_scale, scaled_fps, horizontal_readout, ranges_ms, total_frames) = {
                let p = stab.params.read();
                // Only what's going to be exported: the trim ranges, or the whole clip without any
                let ranges: Vec<(f64, f64)> = if p.trim_ranges.is_empty() { vec![(0.0, 1.0)] } else { p.trim_ranges.clone() };
                let ranges_ms = ranges.iter().map(|(a, b)| (a * p.duration_ms, b * p.duration_ms)).collect::<Vec<_>>();
                let total_frames: usize = ranges.iter().map(|(a, b)| ((b - a) * p.frame_count as f64).round() as usize).sum();
                (p.fps_scale, p.get_scaled_fps(), p.frame_readout_direction.is_horizontal(), ranges_ms, total_frames)
            };
            // Sensor reconstruction needs its original per-frame observations.
            let every_nth_frame = if vision.is_none() && ui.stab_enabled { 1 } else { super::optical_sampling::frame_step(scaled_fps) };
            let (hp_min, hp_max) = super::optical_sampling::high_pass_lengths(every_nth_frame);
            let total_frames = total_frames.div_ceil(every_nth_frame);
            log::info!("Optical analysis sampling: source_fps={scaled_fps:.6} every_nth={every_nth_frame} analysis_fps={:.6}",
                scaled_fps / every_nth_frame as f64);
            let translation = (vision.is_none() && ui.translation_enabled).then(|| TranslationState {
                solver: TranslationSolver::new(TranslationSolverConfig::resolved()),
                next_seq: 0, pairs: HashMap::new(), position: Vector3::zeros(), segment: 0, samples: Vec::new(),
            });
            Ok(Self {
                params, fps_scale, scaled_fps, quats_checksum, context_checksum, horizontal_readout,
                every_nth_frame, hp_min, hp_max,
                track_size: (0, 0),
                focal_px: 0.0,
                tracker: KltTracker::new(MAX_POINTS, 1.0, 960),
                last: None,
                pairs: VecDeque::new(),
                next_seq: 0,
                measured_upto: 0,
                measurements: Vec::new(),
                measured_pairs: 0,
                frames: 0,
                total_frames,
                ranges_ms,
                stab_requested: vision.is_none() && ui.stab_enabled,
                stab_next_seq: 0, stab_pairs: Vec::new(), stab_bands: Vec::new(), sensor_projections: HashMap::new(),
                vision,
                translation,
                base: base::VisionBase::new(OpticalBaseMode::resolved(), BlendConfig::resolved()),
                sg_cache: HashMap::new(),
                cancel_flag,
                generation: (stab.optical_generation.clone(), generation),
                cancelled: AtomicBool::new(false),
            })
        }
    }

    /// Whether it's been cancelled, or overtaken by another file, a project or Clear: then the frames fed are ignored
    /// and `finish` says "Cancelled"
    pub fn is_cancelled(&self) -> bool {
        if !self.cancelled.load(Relaxed) && (self.cancel_flag.load(Relaxed) || self.generation.0.load(SeqCst) != self.generation.1) {
            self.cancelled.store(true, Relaxed);
        }
        self.cancelled.load(Relaxed)
    }

    /// Frames fed so far and to be analyzed
    pub fn progress(&self) -> (usize, usize) { (self.frames, self.total_frames.max(self.frames)) }

    /// What to decode: the trim ranges, in the file's own milliseconds
    pub fn ranges_ms(&self) -> Vec<(f64, f64)> { self.ranges_ms.clone() }

    pub fn frame_step(&self) -> usize { self.every_nth_frame }

    pub fn source_fps(&self) -> f64 { self.scaled_fps / self.fps_scale.unwrap_or(1.0) }

    /// Check before grayscale conversion, and again in core for callers without early sampling.
    pub fn wants_frame(&self, timestamp_us: i64) -> bool {
        let file_ms = timestamp_us as f64 / 1000.0;
        self.ranges_ms.iter().any(|(a, b)| file_ms >= *a - 0.5 && file_ms <= *b + 0.5)
            && super::optical_sampling::keep_frame(timestamp_us, self.source_fps(), self.every_nth_frame)
    }

    fn continuous(&self, index: usize, timestamp_ms: f64) -> bool {
        self.last.is_some_and(|last| {
            let scale = self.fps_scale.unwrap_or(1.0);
            index == last.index + self.every_nth_frame && self.ranges_ms.iter().any(|(a, b)| {
                last.timestamp_ms * scale >= *a - 0.5 && timestamp_ms * scale <= *b + 0.5
            })
        })
    }

    /// Feeds the next decoded frame, in decoding order: 8-bit luma, ideally about 1000 px wide. Frames outside of
    /// `ranges_ms` are ignored
    pub fn feed_frame(&mut self, timestamp_us: i64, width: u32, height: u32, stride: usize, pixels: &[u8]) -> Result<(), String> {
        if self.is_cancelled() || !self.wants_frame(timestamp_us) { return Ok(()); }
        let file_ms = timestamp_us as f64 / 1000.0;
        let mut ts_ms = file_ms;
        if let Some(scale) = self.fps_scale { ts_ms /= scale; }
        let index = crate::frame_at_timestamp(ts_ms, self.scaled_fps).max(0) as usize;
        if self.last.map(|l| index <= l.index).unwrap_or(false) { return Ok(()); } // a repeated frame

        let continuous = self.continuous(index, ts_ms);

        #[cfg(feature = "use-opencv")]
        let obs = {
            if !continuous { self.tracker.reset(); }
            let obs = if pixels.len() < stride * height as usize {
                Vec::new()
            } else {
                let mut packed = Vec::with_capacity(width as usize * height as usize);
                for row in 0..height as usize {
                    let start = row * stride;
                    packed.extend_from_slice(&pixels[start..start + width as usize]);
                }
                let image = GrayImage::from_raw(width, height, packed).ok_or_else(|| "Invalid frame size".to_string())?;
                let (obs, tracked_size) = self.tracker.track(&image)?;
                if tracked_size != (width, height) {
                    return Err(format!("Tracking size {:?} differs from input {:?}", tracked_size, (width, height)));
                }
                obs
            };
            obs
        };
        #[cfg(not(feature = "use-opencv"))]
        let obs = { let _ = (continuous, stride, pixels); Vec::new() };
        self.push_tracked_frame(index, ts_ms, (width, height), obs);
        Ok(())
    }

    /// Common path for decoded tracks and synthetic observations.
    fn push_tracked_frame(&mut self, index: usize, ts_ms: f64, size: (u32, u32), obs: Vec<Observation>) {
        if self.track_size != size {
            self.track_size = size;
            let (k, ..) = FrameTransform::get_lens_data_at_timestamp(&self.params, ts_ms, false);
            self.focal_px = k[(0, 0)] * size.0 as f64 / self.params.width.max(1) as f64;
        }
        let frame = self.frame(index, ts_ms);
        let continuous = self.continuous(index, ts_ms);
        if continuous && !obs.is_empty() {
            if let Some(a) = self.last {
                self.pairs.push_back(Pair { seq: self.next_seq, a, b: frame, obs });
                self.next_seq += 1;
                if self.vision.is_some() { self.chain_vision(); }
            }
        }
        self.last = Some(frame);
        self.frames += 1;
        self.process(false);
    }

    #[cfg(all(test, feature = "use-opencv"))]
    fn push_test_pair(&mut self, a_index: usize, b_index: usize, obs: Vec<Observation>) {
        if self.is_cancelled() { return; }
        if self.last.map(|l| a_index > l.index).unwrap_or(true) {
            self.push_tracked_frame(a_index, crate::timestamp_at_frame(a_index as i32, self.scaled_fps), (960, 540), Vec::new());
        }
        if self.last.map(|l| b_index <= l.index).unwrap_or(false) { return; }
        self.push_tracked_frame(b_index, crate::timestamp_at_frame(b_index as i32, self.scaled_fps), (960, 540), obs);
    }

    /// Measures everything tracked so far, however recent
    pub fn flush(&mut self) { self.process(true); }

    /// The band measurements taken so far: all of them after `flush`
    pub fn measurements(&self) -> &[BandMeasurement] { &self.measurements }

    /// Measures what's left. The correction is then `solve`d from the measurements, as many times as the settings change
    pub fn finish(mut self) -> Result<OpticalMeasurements, String> {
        self.process(true);
        if self.is_cancelled() { return Err("Cancelled".into()); }
        if self.measurements.is_empty() && !(self.stab_requested && !self.stab_pairs.is_empty() && !self.stab_bands.is_empty()) { return Err("Not enough of the image could be tracked".into()); }
        ::log::info!("Optical analysis: {} frames, {} measured pairs, {} band measurements{}", self.frames, self.measured_pairs, self.measurements.len(), if self.vision.is_some() { ", motion from the video" } else { "" });
        if self.vision.is_some() { ::log::info!("Optical analysis base: {}", self.base.summary()); }
        let (quats_checksum, video_base) = match &self.vision {
            // Measured against what `integrate` makes of it
            Some(keys) => {
                let base: Vec<(i64, [f32; 4])> = keys.iter().map(|(t, q)| (*t, [q.w as f32, q.i as f32, q.j as f32, q.k as f32])).collect();
                (optical_correction::checksum(&optical_correction::base_quats(&base)), base)
            },
            None => (self.quats_checksum, Vec::new()),
        };
        Ok(OpticalMeasurements {
            bands: self.measurements,
            stab_requested: self.stab_requested,
            stab_pairs: self.stab_pairs,
            stab_bands: self.stab_bands,
            translation_requested: self.translation.is_some(),
            translation_samples: self.translation.map(|state| state.samples).unwrap_or_default(),
            scaled_fps: self.scaled_fps / self.every_nth_frame as f64,
            quats_checksum,
            context_checksum: self.context_checksum,
            video_base,
            frames: self.frames,
            measured_frames: self.measured_pairs,
            generation: self.generation.1,
        })
    }

    fn frame(&self, index: usize, timestamp_ms: f64) -> Frame {
        frame_timing(&self.params, index, timestamp_ms, if self.horizontal_readout { self.track_size.0 } else { self.track_size.1 })
    }

    /// Unit bearings of tracked points of a frame, in the quaternions' frame
    #[cfg_attr(not(feature = "use-opencv"), allow(dead_code))]
    fn bearings(&self, pts: &[(f32, f32)], frame: &Frame) -> Vec<Option<Vector3<f64>>> {
        undistort_points_to_plane(pts, frame.timestamp_ms, frame.index, &self.params, self.track_size)
            .into_iter()
            .map(|b| b.map(to_quat_frame))
            .collect()
    }

    /// Extends a file without motion data by moving from robust rotation `m0` towards the odometry's rotation
    /// by a continuous weight (the default `auto` mode). The odometry estimates motion at infinity by extrapolating
    /// from visible depths; the visible layer's motion is directly constrained by the image and equals `m0`.
    /// Historical depth structure that predicts a new pair, and a small extrapolation leverage, each supply a
    /// factor of the weight. `GYROFLOW_OPTICAL_BASE=odometry` restores the upstream output.
    #[cfg_attr(not(feature = "use-opencv"), allow(dead_code))]
    fn chain_vision(&mut self) {
        let Some(pair) = self.pairs.back() else { return };
        let (a, b) = (pair.a.mid_ms, pair.b.mid_ms);
        let pts_a: Vec<(f32, f32)> = pair.obs.iter().map(|o| (o.a[0], o.a[1])).collect();
        let pts_b: Vec<(f32, f32)> = pair.obs.iter().map(|o| (o.b[0], o.b[1])).collect();
        let (ba, bb) = (self.bearings(&pts_a, &pair.a), self.bearings(&pts_b, &pair.b));
        let mut va = Vec::with_capacity(pair.obs.len());
        let mut vb = Vec::with_capacity(pair.obs.len());
        let mut ids = Vec::with_capacity(pair.obs.len());
        for ((o, x), y) in pair.obs.iter().zip(ba).zip(bb) {
            if let (Some(x), Some(y)) = (x, y) { va.push(x); vb.push(y); ids.push(o.id); }
        }
        let m = if va.len() >= MIN_BAND_POINTS {
            let m0 = robust_rotation(&va, &vb);
            self.base.rotation(pair.b.index, &va, &vb, &ids, m0, 1.0 / self.focal_px.max(1.0))
        } else {
            self.base.reset();
            Matrix3::identity()
        };

        let Some(keys) = self.vision.as_mut() else { return };
        let (ka, kb) = ((a * 1000.0).round() as i64, (b * 1000.0).round() as i64);
        // A new run of tracks (the start, after a gap) goes on from where the orientation was
        let qa = match keys.get(&ka) {
            Some(q) => *q,
            None => {
                let q = keys.values().next_back().copied().unwrap_or_else(UnitQuaternion::identity);
                keys.insert(ka, q);
                q
            }
        };
        // m = R(b)ᵀ·R(a), so R(b) = R(a)·mᵀ
        let qm = UnitQuaternion::from_rotation_matrix(&Rotation3::from_matrix_unchecked(m));
        keys.insert(kb, qa * qm.inverse());
    }

    /// Measures the frame pairs whose tracks are complete enough for the high-pass
    fn process(&mut self, last_call: bool) {
        self.collect_stab_pairs();
        let end = if last_call { self.next_seq } else { self.next_seq.saturating_sub(self.hp_max) };
        if end <= self.measured_upto || (!last_call && end < self.measured_upto + CHUNK) { return; }
        if self.is_cancelled() { return; }

        let mut derived = self.derive();
        self.measure_translation(&mut derived);
        let rhp = self.high_pass(&derived);

        // (seq, band) -> the points
        let mut groups: HashMap<(usize, u8), Vec<usize>> = HashMap::new();
        for (i, d) in derived.iter().enumerate() {
            if d.seq >= self.measured_upto && d.seq < end && rhp[i].is_some() {
                groups.entry((d.seq, d.band)).or_default().push(i);
            }
        }
        let floor = SIGMA_FLOOR_PX / self.focal_px.max(1.0);
        let gyro = self.params.gyro.clone();
        let vision = &self.vision;
        let mut ms: Vec<(usize, BandMeasurement)> = groups.par_iter().filter_map(|(&(seq, _), idx)| {
            let gyro = gyro.read();
            fit_band(&derived, &rhp, idx, floor, &gyro, vision).map(|mut m| { m.pair = seq; (seq, m) })
        }).collect();
        ms.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.ta_us.total_cmp(&b.1.ta_us)));
        let mut seqs: Vec<usize> = ms.iter().map(|(s, _)| *s).collect();
        seqs.sort_unstable();
        seqs.dedup();
        self.measured_pairs += seqs.len();
        self.measurements.extend(ms.into_iter().map(|(_, m)| m));

        self.measured_upto = end;
        let keep_from = end.saturating_sub(self.hp_max);
        while self.pairs.front().map(|p| p.seq < keep_from).unwrap_or(false) {
            if let Some(pair) = self.pairs.pop_front() {
                if let Some(state) = &mut self.translation { state.pairs.remove(&pair.seq); }
            }
        }
    }

    /// Archive raw geometry before the legacy high-pass can return or release its rolling buffer.
    fn collect_stab_pairs(&mut self) {
        if !self.stab_requested || self.is_cancelled() { return; }
        let rows = if self.horizontal_readout { self.track_size.0 } else { self.track_size.1 }.max(1) as f32;
        for pair in self.pairs.iter().filter(|p| p.seq >= self.stab_next_seq) {
            if self.is_cancelled() { return; }
            let mut prepare = |frame: Frame, pts: Vec<(f32, f32)>| {
                let key = (frame.index, frame.timestamp_ms.to_bits(), self.track_size);
                let prepared = crate::stabilization::prepare_sensor_frame(&self.params, frame.timestamp_ms, frame.index, self.track_size, &pts, self.sensor_projections.get(&key));
                self.sensor_projections.entry(key).or_insert_with(|| prepared.projection.clone());
                prepared
            };
            let a = prepare(pair.a, pair.obs.iter().map(|o| (o.a[0], o.a[1])).collect());
            let b = prepare(pair.b, pair.obs.iter().map(|o| (o.b[0], o.b[1])).collect());
            let mut raw = sensor::SensorPair { seq: pair.seq, frame_a: a.projection, frame_b: b.projection, duration_us: (pair.b.timestamp_ms - pair.a.timestamp_ms) * 1000.0, points: Vec::new() };
            let gyro = self.params.gyro.read();
            let to_gyro_us = |t: f64| (t - gyro.offset_at_video_timestamp(t)) * 1000.0;
            let mut video_times = Vec::new();
            for (i, o) in pair.obs.iter().enumerate() {
                let (Some(a), Some(b)) = (a.points[i], b.points[i]) else { continue; };
                let pa = if self.horizontal_readout { o.a[0] } else { o.a[1] };
                let pb = if self.horizontal_readout { o.b[0] } else { o.b[1] };
                let ta = pair.a.start_ms + pair.a.per_px_ms * pa as f64;
                let tb = pair.b.start_ms + pair.b.per_px_ms * pb as f64;
                let point = sensor::SensorPoint { a, b, ta_us: to_gyro_us(ta), tb_us: to_gyro_us(tb), gyro_ab: gyro.org_quat_at_timestamp(tb).inverse() * gyro.org_quat_at_timestamp(ta), band: ((pa / rows) * BANDS as f32).floor().clamp(0.0, (BANDS - 1) as f32) as u8 };
                if sensor::residual(&raw, &point, Vector3::zeros(), Vector3::zeros()).is_none() { continue; }
                raw.points.push(point);
                video_times.push((ta, tb));
            }
            let bands_start = self.stab_bands.len();
            for band in 0..BANDS as u8 {
                let indices: Vec<_> = raw.points.iter().enumerate().filter_map(|(i, p)| (p.band == band).then_some(i)).collect();
                if let Some((mut measured, weights)) = sensor::fit_band_shift_with_weights(&raw, &indices) {
                    let sw: f64 = weights.iter().sum();
                    let ta = indices.iter().zip(&weights).map(|(&i, w)| video_times[i].0 * w).sum::<f64>() / sw;
                    let tb = indices.iter().zip(&weights).map(|(&i, w)| video_times[i].1 * w).sum::<f64>() / sw;
                    measured.ta_us = to_gyro_us(ta); measured.tb_us = to_gyro_us(tb);
                    self.stab_bands.push(measured);
                }
            }
            self.stab_bands[bands_start..].sort_by(|a, b| a.ta_us.total_cmp(&b.ta_us).then(a.band.cmp(&b.band)));
            if !raw.points.is_empty() { self.stab_pairs.push(raw); }
        }
        self.stab_next_seq = self.next_seq;
    }

    /// Solve each pair once, then reuse its parallax while the high-pass still needs it.
    fn measure_translation(&mut self, derived: &mut [Derived]) {
        let Some(state) = &mut self.translation else { return };
        // Lens queries take the gyro lock themselves. Complete them before reading poses.
        let mut focal_ratios = HashMap::new();
        for pair in self.pairs.iter().filter(|pair| pair.seq >= state.next_seq) {
            for frame in [pair.a, pair.b] {
                focal_ratios.entry(frame.index).or_insert_with(|| {
                    let (mut k, ..) = FrameTransform::get_lens_data_at_timestamp(&self.params, frame.timestamp_ms, false);
                    FrameTransform::dequantize_camera_matrix(&self.params, frame.index, &mut k);
                    (k[(0, 0)] / self.params.width.min(self.params.height).max(1) as f64) as f32
                });
            }
        }
        let gyro = self.params.gyro.read();
        let camera_to_world = |timestamp| {
            let q = gyro.org_quat_at_timestamp(timestamp);
            [q.w as f32, q.i as f32, q.j as f32, q.k as f32]
        };
        for pair in self.pairs.iter().filter(|pair| pair.seq >= state.next_seq) {
            let points: Vec<PairPoint> = derived.iter().filter(|d| d.seq == pair.seq)
                .map(|d| PairPoint { id: d.id, band: d.band, p: d.p, r: d.r }).collect();
            let result = state.solver.step(&points, 1.0 / self.focal_px, pair.b.mid_ms / 1000.0);
            if state.samples.is_empty() || result.new_segment {
                if !state.samples.is_empty() { state.segment += 1; }
                state.position = Vector3::zeros();
                state.samples.push(TranslationSample {
                    timestamp_us: (pair.a.mid_ms * 1000.0).round() as i64,
                    segment: state.segment,
                    camera_to_world: camera_to_world(pair.a.mid_ms),
                    focal_length_over_short_side: focal_ratios[&pair.a.index],
                    ..Default::default()
                });
            }
            if result.confidence > 0.0 {
                state.position += gyro.org_quat_at_timestamp(pair.b.mid_ms) * result.c_segment;
                state.pairs.insert(pair.seq, (result.c, result.inv_depth));
            }
            state.samples.push(TranslationSample {
                timestamp_us: (pair.b.mid_ms * 1000.0).round() as i64,
                position: [state.position.x as f32, state.position.y as f32, state.position.z as f32],
                ref_inv_depth: result.ref_inv_depth as f32,
                confidence: result.confidence as f32,
                track_age_s: result.track_age_s as f32,
                segment: state.segment,
                camera_to_world: camera_to_world(pair.b.mid_ms),
                focal_length_over_short_side: focal_ratios[&pair.b.index],
            });
        }
        state.next_seq = self.next_seq;
        for d in derived {
            if let Some((c, depth)) = state.pairs.get(&d.seq) {
                if let Some(rho) = depth.get(&d.id) { d.r += *rho * (c - c.dot(&d.p) * d.p); }
            }
        }
    }

    /// Residuals of every point of the pairs held, against the quaternions
    fn derive(&self) -> Vec<Derived> {
        let params = &self.params;
        let size = self.track_size;
        let horizontal = self.horizontal_readout;
        let track_rows = if horizontal { size.0 } else { size.1 }.max(1) as f32;
        let per_pair: Vec<Vec<Derived>> = self.pairs.par_iter().map(|pair| {
            let pts_a: Vec<(f32, f32)> = pair.obs.iter().map(|o| (o.a[0], o.a[1])).collect();
            let pts_b: Vec<(f32, f32)> = pair.obs.iter().map(|o| (o.b[0], o.b[1])).collect();
            let ba = undistort_points_to_plane(&pts_a, pair.a.timestamp_ms, pair.a.index, params, size);
            let bb = undistort_points_to_plane(&pts_b, pair.b.timestamp_ms, pair.b.index, params, size);
            let gyro = params.gyro.read();
            let orient = |t: f64| orientation(&self.vision, &gyro, t);
            let mut out = Vec::with_capacity(pair.obs.len());
            for (i, o) in pair.obs.iter().enumerate() {
                let (Some(a), Some(b)) = (ba.get(i).copied().flatten(), bb.get(i).copied().flatten()) else { continue };
                let pos_a = if horizontal { o.a[0] } else { o.a[1] };
                let pos_b = if horizontal { o.b[0] } else { o.b[1] };
                let ta = pair.a.start_ms + pair.a.per_px_ms * pos_a as f64;
                let tb = pair.b.start_ms + pair.b.per_px_ms * pos_b as f64;
                let m = (orient(tb).inverse() * orient(ta)).to_rotation_matrix().into_inner();
                let (va, vb) = (to_quat_frame(a), to_quat_frame(b));
                let p = m * va;
                let band = ((pos_a / track_rows) * BANDS as f32).floor().clamp(0.0, (BANDS - 1) as f32) as u8;
                out.push(Derived { id: o.id, seq: pair.seq, band, p, r: vb - p, ta_ms: ta, tb_ms: tb });
            }
            out
        }).collect();
        per_pair.into_iter().flatten().collect()
    }

    /// Takes the slow part out of each track's residuals: a local quadratic fit (Savitzky-Golay, the fit of the
    /// window's edge at the track's ends), which is where the parallax lives
    fn high_pass(&mut self, derived: &[Derived]) -> Vec<Option<Vector3<f64>>> {
        let mut order: Vec<usize> = (0..derived.len()).collect();
        order.sort_unstable_by_key(|&i| (derived[i].id, derived[i].seq));
        let mut out = vec![None; derived.len()];
        let mut s = 0;
        while s < order.len() {
            let mut e = s + 1;
            while e < order.len() && derived[order[e]].id == derived[order[s]].id && derived[order[e]].seq == derived[order[e - 1]].seq + 1 { e += 1; }
            let len = e - s;
            if len >= self.hp_min {
                let l = { let l = len.min(self.hp_max); if l % 2 == 0 { l - 1 } else { l } };
                let proj = self.sg_cache.entry(l).or_insert_with(|| sg_projection(l));
                for k in 0..len {
                    let w0 = (k as isize - (l / 2) as isize).clamp(0, (len - l) as isize) as usize;
                    let pos = k - w0;
                    let mut smooth = Vector3::zeros();
                    for j in 0..l { smooth += derived[order[s + w0 + j]].r * proj[(pos, j)]; }
                    out[order[s + k]] = Some(derived[order[s + k]].r - smooth);
                }
            }
            s = e;
        }
        out
    }
}

/// The orientation the image is compared against at a moment of the video: the motion data's, or for a file without
/// any the one chained from the frames (along the geodesic between them, as `optical_correction::densify` has it)
fn orientation(vision: &Option<TimeQuat>, gyro: &GyroSource, t_ms: f64) -> UnitQuaternion<f64> {
    let Some(keys) = vision else { return gyro.org_quat_at_timestamp(t_ms) };
    let t = (t_ms * 1000.0).round() as i64;
    match (keys.range(..=t).next_back(), keys.range(t..).next()) {
        (Some((&a, qa)), Some((&b, qb))) => if b == a { *qa } else { qa.slerp(qb, (t - a) as f64 / (b - a) as f64) },
        (Some((_, q)), None) | (None, Some((_, q))) => *q,
        (None, None) => UnitQuaternion::identity(),
    }
}

#[cfg_attr(not(feature = "use-opencv"), allow(dead_code))]
/// The rotation `R` with `b ≈ R·a` for most of the points: Kabsch, reweighted against the ones that disagree
fn robust_rotation(a: &[Vector3<f64>], b: &[Vector3<f64>]) -> Matrix3<f64> {
    let mut w = vec![1.0f64; a.len()];
    let mut r = Matrix3::identity();
    for _ in 0..6 {
        let h: Matrix3<f64> = a.iter().zip(b).zip(&w).map(|((a, b), w)| a * b.transpose() * *w).sum();
        let svd = h.svd(true, true);
        let (Some(u), Some(vt)) = (svd.u, svd.v_t) else { break };
        let d = (vt.transpose() * u.transpose()).determinant().signum();
        r = vt.transpose() * Matrix3::from_diagonal(&Vector3::new(1.0, 1.0, d)) * u.transpose();
        let res: Vec<f64> = a.iter().zip(b).map(|(a, b)| (b - r * a).norm()).collect();
        let mut sorted = res.clone();
        sorted.sort_by(|x, y| x.total_cmp(y));
        let scale = (1.4826 * sorted[sorted.len() / 2]).max(1e-9);
        for (w, e) in w.iter_mut().zip(&res) { *w = 1.0 / (1.0 + (e / (2.5 * scale)).powi(2)); }
    }
    r
}

/// Maps samples of a window of `l` to their least-squares quadratic
fn sg_projection(l: usize) -> DMatrix<f64> {
    let c = (l as f64 - 1.0) / 2.0;
    let x = DMatrix::from_fn(l, 3, |i, j| ((i as f64 - c) / c.max(1.0)).powi(j as i32));
    let xtx = x.transpose() * &x;
    let inv = xtx.try_inverse().unwrap_or_else(|| DMatrix::zeros(3, 3));
    &x * inv * x.transpose()
}

/// One band of one frame pair: the rotation its points moved by beyond the quaternions, `r ≈ ρ × p`, robustly
fn fit_band(derived: &[Derived], rhp: &[Option<Vector3<f64>>], idx: &[usize], sigma_floor: f64, gyro: &GyroSource, vision: &Option<TimeQuat>) -> Option<BandMeasurement> {
    if idx.len() < MIN_BAND_POINTS { return None; }
    let mut w = vec![1.0f64; idx.len()];
    let mut rho = Vector3::zeros();
    let mut h = Matrix3::zeros();
    let mut res = vec![0.0f64; idx.len()];
    for _ in 0..5 {
        h = Matrix3::zeros();
        let mut g = Vector3::zeros();
        for (k, &i) in idx.iter().enumerate() {
            let (p, r) = (derived[i].p, rhp[i]?);
            h += (Matrix3::identity() - p * p.transpose()) * w[k];
            g += p.cross(&r) * w[k];
        }
        rho = h.try_inverse()? * g;
        for (k, &i) in idx.iter().enumerate() {
            res[k] = (rhp[i]? - rho.cross(&derived[i].p)).norm();
        }
        let mut sorted = res.clone();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let scale = (1.4826 * sorted[sorted.len() / 2]).max(1e-9);
        for k in 0..idx.len() { w[k] = 1.0 / (1.0 + (res[k] / (2.5 * scale)).powi(2)); }
    }
    let sw: f64 = w.iter().sum();
    if sw < MIN_BAND_POINTS as f64 * 0.5 { return None; }
    let var = res.iter().zip(&w).map(|(e, w)| w * e * e).sum::<f64>() / sw / 2.0;
    let cov = h.try_inverse()? * var + Matrix3::identity() * sigma_floor * sigma_floor;
    let info = cov.try_inverse()?;

    let ta = idx.iter().zip(&w).map(|(&i, w)| derived[i].ta_ms * w).sum::<f64>() / sw;
    let tb = idx.iter().zip(&w).map(|(&i, w)| derived[i].tb_ms * w).sum::<f64>() / sw;
    let m = (orientation(vision, gyro, tb).inverse() * orientation(vision, gyro, ta)).to_rotation_matrix().into_inner();
    let to_gyro_us = |t: f64| (t - gyro.offset_at_video_timestamp(t)) * 1000.0;
    Some(BandMeasurement { pair: 0, ta_us: to_gyro_us(ta), tb_us: to_gyro_us(tb), rho, info, m })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "use-opencv")]
    #[test]
    fn raw_sensor_short_tracks_finish_and_duplicate_flush() {
        let stab = analysis_fixture(8, |_| crate::Quat64::identity());
        stab.optical_ui.write().stab_enabled = true;
        let mut analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        for frame in 1..8 {
            let obs = (0..400).map(|id| {
                let a = [(id % 20) as f32 * 40.0 + 50.0, (id / 20) as f32 * 25.0 + 20.0];
                Observation { id, a, b: [a[0] - 1.0, a[1] + 0.5] }
            }).collect();
            analysis.push_test_pair(frame - 1, frame, obs);
        }
        analysis.flush();
        analysis.flush();
        assert!(analysis.measurements().is_empty());
        let m = analysis.finish().unwrap();
        assert!(m.stab_requested);
        assert_eq!(m.stab_pairs.len(), 7);
        assert_eq!(m.stab_bands.len(), 42);
        assert_eq!(m.measured_frames, 0);
        eprintln!("raw archive: pairs={} points={} bands={} point_bytes={} pair_bytes={} band_bytes={} projection_bytes={}",
            m.stab_pairs.len(), m.stab_pairs.iter().map(|p| p.points.len()).sum::<usize>(), m.stab_bands.len(),
            std::mem::size_of::<sensor::SensorPoint>(), std::mem::size_of::<sensor::SensorPair>(), std::mem::size_of::<sensor::SensorBand>(), std::mem::size_of::<crate::stabilization::SensorProjection>());
        for adjacent in m.stab_pairs.windows(2) { assert!(Arc::ptr_eq(&adjacent[0].frame_b, &adjacent[1].frame_a)); }
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn raw_sensor_rejects_translation_and_stab_request() {
        let stab = analysis_fixture(8, |_| crate::Quat64::identity());
        { let mut ui = stab.optical_ui.write(); ui.stab_enabled = true; ui.translation_enabled = true; }
        let result = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false)));
        assert_eq!(result.err().as_deref(), Some("Translation stabilization and in-camera stabilization reconstruction cannot be analyzed together"));
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn raw_sensor_endpoint_and_band_times_follow_video_offsets() {
        for direction in [crate::stabilization_params::ReadoutDirection::TopToBottom, crate::stabilization_params::ReadoutDirection::LeftToRight] {
            let stab = analysis_fixture(8, |_| crate::Quat64::identity());
            { let mut p = stab.params.write(); p.frame_readout_time = 12.0; p.frame_readout_direction = direction; }
            stab.optical_ui.write().stab_enabled = true;
            { let mut g = stab.gyro.write(); g.set_offset(0, 2.0); g.set_offset(20_000, 4.0); g.set_offset(40_000, 1.0); }
            let obs: Vec<_> = (0..400).map(|id| {
                let a = [(id % 20) as f32 * 40.0 + 50.0, (id / 20) as f32 * 25.0 + 20.0];
                Observation { id, a, b: [a[0] - 1.0, a[1] + 0.5] }
            }).collect();
            let mut analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
            analysis.push_test_pair(0, 1, obs.clone());
            let a = analysis.frame(0, 0.0); let b = analysis.frame(1, 1000.0 / 30.0);
            let gyro = analysis.params.gyro.read();
            let time = |o: &Observation| {
                let axis = if direction.is_horizontal() { 0 } else { 1 };
                (a.start_ms + a.per_px_ms * o.a[axis] as f64, b.start_ms + b.per_px_ms * o.b[axis] as f64)
            };
            let convert = |t: f64| (t - gyro.offset_at_video_timestamp(t)) * 1000.0;
            let raw = &analysis.stab_pairs[0];
            assert!((raw.duration_us - 1e6 / 30.0).abs() < 1e-6);
            for (point, obs) in raw.points.iter().zip(&obs) {
                let (ta, tb) = time(obs);
                assert!((point.ta_us - convert(ta)).abs() < 1e-6);
                assert!((point.tb_us - convert(tb)).abs() < 1e-6);
            }
            for band in &analysis.stab_bands {
                let indices: Vec<_> = raw.points.iter().enumerate().filter_map(|(i,p)| (p.band == band.band).then_some(i)).collect();
                let (_, weights) = sensor::fit_band_shift_with_weights(raw, &indices).unwrap();
                let sw: f64 = weights.iter().sum();
                let ta = indices.iter().zip(&weights).map(|(&i,w)| time(&obs[i]).0 * w).sum::<f64>() / sw;
                let tb = indices.iter().zip(&weights).map(|(&i,w)| time(&obs[i]).1 * w).sum::<f64>() / sw;
                assert!((band.ta_us - convert(ta)).abs() < 1e-6);
                assert!((band.tb_us - convert(tb)).abs() < 1e-6);
                assert!(band.info.iter().all(|v| v.is_finite()));
                assert!(band.cauchy_scale_px >= 2.5 * SIGMA_FLOOR_PX);
            }
        }
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn raw_sensor_is_not_requested_for_vision_only() {
        let stab = analysis_fixture(90, |_| crate::Quat64::identity());
        { let mut gyro = stab.gyro.write(); gyro.quaternions.clear(); gyro.file_metadata.write().quaternions.clear(); }
        stab.optical_ui.write().stab_enabled = true;
        let m = analyze_pairs(&stab, 90, parallax_observations);
        assert!(!m.stab_requested);
        assert!(m.stab_pairs.is_empty());
        assert!(m.stab_bands.is_empty());
    }

    #[test]
    fn optical_analysis_measures_without_the_reconstruction() {
        assert!(!measurement_params(&manager()).apply_optical_stab);
    }

    #[test]
    fn optical_analysis_measures_without_the_translation() {
        assert!(!measurement_params(&manager()).apply_optical_translation);
    }
    use crate::StabilizationManager;
    use crate::gyro_source::OpticalCorrectionSettings;
    #[cfg(feature = "use-opencv")]
    use std::sync::{Arc, atomic::{AtomicBool, Ordering::SeqCst}};

    fn manager() -> StabilizationManager {
        let stab = StabilizationManager::default();
        {
            let mut p = stab.params.write();
            p.size = (1920, 1080);
            p.fps = 30.0;
            p.frame_count = 300;
            p.duration_ms = 10_000.0;
        }
        stab
    }

    /// 1920x1080 at 30 fps, pinhole f = 1000 px, orientation sampled at 1 kHz, no sync offsets.
    #[cfg(feature = "use-opencv")]
    fn analysis_fixture(frames: usize, orientation: impl Fn(f64) -> crate::Quat64) -> StabilizationManager {
        let stab = manager();
        let duration_ms = frames as f64 * 1000.0 / 30.0;
        stab.init_from_video_data(duration_ms, 30.0, frames, (1920, 1080));
        stab.set_size(1920, 1080);
        stab.set_output_size(1920, 1080);
        stab.lens.write().load_from_json_value(&serde_json::json!({
            "calib_dimension": {"w":1920,"h":1080},
            "distortion_model":"opencv_standard",
            "fisheye_params": {
                "camera_matrix":[[1000.0,0.0,960.0],[0.0,1000.0,540.0],[0.0,0.0,1.0]],
                "distortion_coeffs":[0.0,0.0,0.0,0.0]
            }
        }));
        let mut gyro = stab.gyro.write();
        gyro.duration_ms = duration_ms;
        gyro.quaternions = (0..=duration_ms.ceil() as i64)
            .map(|ms| (ms * 1000, orientation(ms as f64 / 1000.0))).collect();
        gyro.file_metadata.write().quaternions = gyro.quaternions.clone();
        assert!(gyro.has_motion());
        drop(gyro);
        stab
    }

    /// Project quaternion coordinates (x right, y up, looking down -z) to the tracked image.
    #[cfg(feature = "use-opencv")]
    fn project(p: Vector3<f64>) -> [f32; 2] {
        [(480.0 + 500.0 * p.x / -p.z) as f32, (270.0 - 500.0 * p.y / -p.z) as f32]
    }

    #[cfg(feature = "use-opencv")]
    fn analyze_pairs(stab: &StabilizationManager, frames: usize, observe: impl Fn(usize) -> Vec<(u32, [f32; 2])>) -> OpticalMeasurements {
        let mut analysis = OpticalMotionAnalysis::from_manager(stab, Arc::new(AtomicBool::new(false))).unwrap();
        let mut previous = observe(0);
        for frame in 1..frames {
            let current = observe(frame);
            let by_id: HashMap<_, _> = previous.into_iter().collect();
            let obs = current.iter().filter_map(|(id, b)| by_id.get(id).map(|a| Observation { id: *id, a: *a, b: *b })).collect();
            analysis.push_test_pair(frame - 1, frame, obs);
            previous = current;
        }
        analysis.finish().unwrap()
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn reconstruction_reproduces_the_recorded_compensation() {
        use crate::gyro_source::{CameraStabData, splines::CatmullRom};
        let yaw = |t: f64| 0.005 * (std::f64::consts::TAU * 3.0 * t).sin();
        let body = |t: f64| crate::Quat64::from_euler_angles(0.0, yaw(t), 0.0);
        let recorded = analysis_fixture(60, body);
        {
            let gyro = recorded.gyro.write();
            let mut md = gyro.file_metadata.write();
            md.detected_source = Some("Sony test".into());
            for frame in 0..60 {
                let mut ibis = CatmullRom::new();
                for row in [0.0, 1080.0] { ibis.add_point(row, Vector3::new(1000.0 * yaw(frame as f64 / 30.0).tan(), 0.0, 0.0)); }
                md.camera_stab_data.push(CameraStabData { sensor_size: (1920, 1080), crop_area: (0.0, 0.0, 1920.0, 1080.0),
                    pixel_pitch: (1, 1), ibis_spline: ibis, ..Default::default() });
            }
        }
        recorded.recompute_blocking();
        let reconstructed = recorded.get_cloned();
        reconstructed.gyro.write().file_metadata = crate::gyro_source::ReadOnlyFileMetadata::from({
            let gyro = recorded.gyro.read(); let mut md = gyro.file_metadata.read().clone(); md.camera_stab_data.clear(); md
        });
        reconstructed.set_stab_reconstruction_enabled(true);
        // World rays are projected through the true body, then through the inverse known sensor SE(2).
        let m = analyze_pairs(&reconstructed, 60, |frame| {
            let t = frame as f64 / 30.0;
            (0..400).map(|id| {
                let u = (id % 20) as f64 * 48.0 + 24.0; let v = (id / 20) as f64 * 27.0 + 13.5;
                let world = Vector3::new((u - 480.0) / 100.0, (270.0 - v) / 100.0, -5.0);
                let point = body(t).inverse() * world;
                (id, [(480.0 + 500.0 * point.x / -point.z - 500.0 * yaw(t).tan()) as f32,
                    (270.0 - 500.0 * point.y / -point.z) as f32])
            }).collect()
        });
        assert!(m.stab_requested && !m.stab_bands.is_empty());
        reconstructed.set_optical_measurements(m).unwrap(); reconstructed.recompute_blocking();
        assert!(reconstructed.gyro.read().optical_stab.as_ref().unwrap().is_active());
        let reference = ComputeParams::from_manager(&recorded); let actual = ComputeParams::from_manager(&reconstructed);
        let grid: Vec<_> = [360.0, 960.0, 1560.0].into_iter().flat_map(|x| [180.0, 540.0, 900.0].into_iter().map(move |y| (x, y))).collect();
        let render = |p: &ComputeParams, frame: usize| {
            let t = frame as f64 * 1000.0 / 30.0;
            let (k, coeffs, _, rotations, shifts, mesh, fov, limit) = FrameTransform::at_timestamp_for_points(p, &grid, t, Some(frame), true);
            let shifts = shifts.map(|s| if s.len() == 1 { vec![s[0]; grid.len()] } else { s });
            crate::stabilization::undistort_points(&grid, k, &coeffs, rotations[0], None, Some(rotations), p, 1.0, fov, t, shifts, mesh, limit)
        };
        let mut maximum = 0.0f64;
        for frame in 15..45 {
            for (a, b) in render(&reference, frame).iter().zip(render(&actual, frame)) {
                maximum = maximum.max(((a.0 - b.0) as f64).hypot((a.1 - b.1) as f64));
            }
        }
        println!("reconstruction recorded renderer maximum={maximum} px");
        assert!(maximum <= 0.1, "recorded renderer mismatch: {maximum} px");
    }

    #[cfg(feature = "use-opencv")]
    fn golden_scene(translation: bool) -> OpticalMeasurements {
        let body = |t: f64| crate::Quat64::from_euler_angles(
            0.3f64.to_radians() * (std::f64::consts::TAU * 3.3 * t).sin(),
            0.3f64.to_radians() * (std::f64::consts::TAU * 2.0 * t).sin(), 0.0);
        let stab = analysis_fixture(90, body);
        stab.optical_ui.write().translation_enabled = translation;
        analyze_pairs(&stab, 90, |frame| {
            let t = frame as f64 / 30.0;
            let observed = body(t) * crate::Quat64::from_euler_angles(
                0.05f64.to_radians() * (std::f64::consts::TAU * 7.0 * t).sin(), 0.0, 0.0);
            (0..400).map(|id| {
                let u = (id % 20) as f64 * 48.0 + 24.0;
                let v = (id / 20) as f64 * 27.0 + 13.5;
                let world = Vector3::new((u - 480.0) * 5.0 / 500.0, (270.0 - v) * 5.0 / 500.0, -5.0);
                (id, project(observed.inverse() * world))
            }).collect()
        })
    }

    #[cfg(feature = "use-opencv")]
    fn bits_hash(values: impl Iterator<Item = f64>) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for v in values { h.write_u64(v.to_bits()); }
        h.finish()
    }

    // Captured before translation work with zero here, after three processes confirmed the sorted hash.
    // Depends on the toolchain's float library and DefaultHasher; recapture after changing either.
    #[cfg(feature = "use-opencv")]
    const BANDS_GOLDEN: u64 = 291084217601240974;

    /// Ignore tied band times: their HashMap collection order changes between processes.
    #[cfg(feature = "use-opencv")]
    fn bands_hash(bands: &[BandMeasurement]) -> u64 {
        let mut rows: Vec<Vec<u64>> = bands.iter().map(|b| {
            [b.pair as f64, b.ta_us, b.tb_us].into_iter()
                .chain(b.rho.iter().copied()).chain(b.info.iter().copied()).map(f64::to_bits).collect()
        }).collect();
        rows.sort();
        bits_hash(rows.into_iter().flatten().map(f64::from_bits))
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn bands_golden_without_translation() {
        let m = golden_scene(false);
        let h = bands_hash(&m.bands);
        assert!(!m.bands.is_empty());
        assert_eq!(h, BANDS_GOLDEN, "got {h}");
    }

    #[cfg(feature = "use-opencv")]
    fn camera_x(frame: usize) -> f64 {
        let amplitude = 1.5 * 4.0 / 500.0;
        amplitude * (std::f64::consts::TAU * frame as f64 / 10.0).sin()
    }

    #[cfg(feature = "use-opencv")]
    fn parallax_observations(frame: usize) -> Vec<(u32, [f32; 2])> {
        (0..1200).map(|id| {
            let depth = [2.0, 4.0, 8.0][id as usize / 400];
            let grid = id % 400;
            let u = (grid % 20) as f64 * 48.0 + 24.0;
            let v = (grid / 20) as f64 * 27.0 + 13.5;
            let world = Vector3::new((u - 480.0) * depth / 500.0, (270.0 - v) * depth / 500.0, -depth);
            (id, project(world - Vector3::new(camera_x(frame), 0.0, 0.0)))
        }).collect()
    }

    #[cfg(feature = "use-opencv")]
    fn parallax_scene(translation: bool) -> OpticalMeasurements {
        let stab = analysis_fixture(90, |_| crate::Quat64::identity());
        stab.optical_ui.write().translation_enabled = translation;
        analyze_pairs(&stab, 90, parallax_observations)
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn parallax_is_taken_out_before_the_rotation_fit() {
        let settings = OpticalCorrectionSettings { strength: 0.5 };
        let without = solve(&parallax_scene(false), &settings).unwrap().rms_deg;
        let with = solve(&parallax_scene(true), &settings).unwrap().rms_deg;
        eprintln!("parallax rotation rms_deg: without={without} with={with} ratio={}", with / without);
        assert!(with <= 0.3 * without, "without={without}, with={with}");
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn translation_samples_follow_the_camera() {
        let m = parallax_scene(true);
        assert!(m.translation_requested);
        assert_eq!(m.translation_samples.len(), 90);
        assert!(m.translation_samples.iter().all(|s| s.segment == 0));
        let points: Vec<(f64, f64)> = (30..60).map(|frame| {
            let sample = &m.translation_samples[frame];
            assert_eq!(sample.timestamp_us, (frame as f64 * 1_000_000.0 / 30.0).round() as i64);
            (camera_x(frame), sample.position[0] as f64)
        }).collect();
        let mx = points.iter().map(|p| p.0).sum::<f64>() / points.len() as f64;
        let my = points.iter().map(|p| p.1).sum::<f64>() / points.len() as f64;
        let cov = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum::<f64>();
        let sx = points.iter().map(|p| (p.0 - mx).powi(2)).sum::<f64>();
        let sy = points.iter().map(|p| (p.1 - my).powi(2)).sum::<f64>();
        let correlation = cov / (sx * sy).sqrt();
        eprintln!("translation position Pearson correlation={correlation}");
        assert!(correlation >= 0.95, "correlation={correlation}");
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn sampled_analysis_tracks_integer_steps_and_preserves_original_frame_times() {
        let stab = analysis_fixture(90, |_| crate::Quat64::identity());
        {
            let mut p = stab.params.write();
            p.fps = 24.0;
            p.fps_scale = Some(5.0);
            p.frame_count = 360;
            p.duration_ms = 15_000.0;
        }
        let mut analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(analysis.frame_step(), 5);
        assert_eq!(analysis.source_fps(), 24.0);
        let mut seed = 7u32;
        let pixels: Vec<u8> = (0..640 * 360).map(|_| {
            seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed as u8
        }).collect();
        for frame in 0..120 {
            let ts = (frame as f64 * 1e6 / 24.0).round() as i64;
            analysis.feed_frame(ts, 640, 360, 640, &pixels).unwrap();
        }
        assert_eq!(analysis.frames, 24);
        assert_eq!(analysis.next_seq, 23);
        for pair in &analysis.pairs {
            assert_eq!(pair.b.index - pair.a.index, 5);
            assert!((pair.b.timestamp_ms - pair.a.timestamp_ms - 1000.0 / 24.0).abs() < 0.001);
        }
        assert!(!analysis.continuous(125, 125_000.0 / 120.0));
        // A trim gap can be shorter than one sampling step and must still break the pair.
        analysis.ranges_ms = vec![(0.0, 4_800.0), (4_900.0, 15_000.0)];
        assert!(!analysis.continuous(120, 1000.0));
        let m = analysis.finish().unwrap();
        assert_eq!(m.scaled_fps, 24.0);
        assert_eq!(m.frames, 24);
        assert!(m.measured_frames > 0);
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn sampled_translation_and_rotation_interpolate_between_source_frames() {
        use crate::gyro_source::optical_translation::{OpticalTranslation, OpticalTranslationSettings};
        let stab = analysis_fixture(90, |_| crate::Quat64::identity());
        { let mut p = stab.params.write(); p.fps = 120.0; p.frame_count = 360; }
        stab.optical_ui.write().translation_enabled = true;
        let mut analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        let mut previous: HashMap<_, _> = parallax_observations(0).into_iter().collect();
        for sample in 1..72 {
            let current = parallax_observations(sample);
            let obs = current.iter().map(|(id, b)| Observation { id: *id, a: previous[id], b: *b }).collect();
            analysis.push_test_pair((sample - 1) * 5, sample * 5, obs);
            previous = current.into_iter().collect();
        }
        let m = analysis.finish().unwrap();
        assert_eq!(m.scaled_fps, 24.0);
        assert_eq!(m.translation_samples.len(), 72);
        assert!(m.translation_samples.iter().filter(|s| s.confidence > 0.0).count() > 30);
        let correction = solve(&m, &OpticalCorrectionSettings::default()).unwrap();
        assert!((correction.spacing_us - 1e6 / 24.0 / 6.0).abs() < 1e-9);
        let translation = OpticalTranslation::new(m.translation_samples.clone(), OpticalTranslationSettings::default());
        let mut maximum = 0.0f64;
        for samples in m.translation_samples.windows(2).skip(10).take(40) {
            let (a, b) = (samples[0].timestamp_us as f64 / 1000.0, samples[1].timestamp_us as f64 / 1000.0);
            let (left, right) = (translation.shift_at(a), translation.shift_at(b));
            maximum = maximum.max(left.norm());
            for k in 1..5 {
                let fraction = k as f64 / 5.0;
                let time = a + (b - a) * fraction;
                assert!((translation.shift_at(time) - (left * (1.0 - fraction) + right * fraction)).norm() < 1e-10);
                assert!(correction.at(time * 1000.0).iter().all(|v| v.is_finite()));
            }
        }
        assert!(maximum > 1e-6);
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn sampled_pure_optical_base_interpolates_rotations() {
        let stab = analysis_fixture(90, |_| crate::Quat64::identity());
        { let mut p = stab.params.write(); p.fps = 240.0; p.frame_count = 720; }
        assert!(stab.set_ignore_file_motion(true));
        let mut analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(analysis.frame_step(), 10);
        let observe = |sample: usize| {
            let rotation = Rotation3::from_axis_angle(&Vector3::y_axis(), (sample as f64 * 0.1).to_radians());
            (0..400u32).map(|id| {
                let world = Vector3::new(((id % 20) as f64 - 9.5) / 10.0, ((id / 20) as f64 - 9.5) / 10.0, -5.0);
                (id, project(rotation * world))
            }).collect::<HashMap<_, _>>()
        };
        let mut previous = observe(0);
        for sample in 1..40 {
            let current = observe(sample);
            let obs = (0..400u32).map(|id| Observation { id, a: previous[&id], b: current[&id] }).collect();
            analysis.push_test_pair((sample - 1) * 10, sample * 10, obs);
            previous = current;
        }
        let m = analysis.finish().unwrap();
        assert_eq!(m.video_base.len(), 40);
        let quats = optical_correction::base_quats(&m.video_base);
        let first = GyroSource::clamped_quat_at_gyro_timestamp(&quats, 10.0 * 1000.0 / 24.0);
        let midpoint = GyroSource::clamped_quat_at_gyro_timestamp(&quats, 10.5 * 1000.0 / 24.0);
        assert!((first.angle_to(&midpoint).to_degrees() - 0.05).abs() < 0.003);
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn translation_measurements_store_the_pose_and_each_frames_focal_ratio() {
        let pose = crate::Quat64::from_euler_angles(0.1, -0.2, 0.3);
        let stab = analysis_fixture(90, |_| pose);
        stab.lens.write().fisheye_params.distortion_coeffs.clear();
        {
            let gyro = stab.gyro.read();
            let mut metadata = gyro.file_metadata.write();
            for frame in 0..90 {
                let focal = 1000.0 + frame as f32 * 2.0;
                metadata.lens_params.insert((frame as f64 * 1e6 / 30.0).round() as i64, crate::gyro_source::LensParams {
                    pixel_focal_length: Some((focal, focal)), sensor_size_px: Some((1920, 1080)),
                    capture_area_size: Some((1920.0, 1080.0)), ..Default::default()
                });
            }
        }
        stab.optical_ui.write().translation_enabled = true;
        let measurements = analyze_pairs(&stab, 90, |frame| {
            let scale = (1000.0 + frame as f32 * 2.0) / 1000.0;
            parallax_observations(frame).into_iter().map(|(id, p)| (id, [480.0+(p[0]-480.0)*scale,270.0+(p[1]-270.0)*scale])).collect()
        });
        assert!(measurements.translation_samples.len() >= 90);
        for sample in &measurements.translation_samples {
            let frame = (sample.timestamp_us as f64 * 30.0 / 1e6).round();
            assert!((sample.focal_length_over_short_side as f64 - (1000.0+2.0*frame)/1080.0).abs() < 2e-6);
            for (actual, expected) in sample.camera_to_world.into_iter().zip([pose.w,pose.i,pose.j,pose.k]) {
                assert!((actual as f64-expected).abs() < 1e-7);
            }
        }
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn a_file_without_motion_gets_its_base_through_vision_base() {
        let stab = analysis_fixture(60, |_| crate::Quat64::identity());
        assert!(stab.set_ignore_file_motion(true));
        let measurements = analyze_pairs(&stab, 60, |frame| {
            let rotation = Rotation3::from_axis_angle(&Vector3::y_axis(), (frame as f64 * 0.1).to_radians());
            (0..400).map(|id| {
                let depth = [2.0, 4.0, 8.0][id as usize % 3];
                let u = 192.0 + ((id % 20) as f64 + 0.5) * 576.0 / 20.0;
                let v = 108.0 + ((id / 20) as f64 + 0.5) * 324.0 / 20.0;
                let world = Vector3::new((u - 480.0) * depth / 500.0, (270.0 - v) * depth / 500.0, -depth);
                (id, project(rotation * world))
            }).collect()
        });
        assert_eq!(measurements.video_base.len(), 60);
        let quats: Vec<_> = measurements.video_base.iter().map(|(_, q)| {
            UnitQuaternion::new_normalize(nalgebra::Quaternion::new(q[0] as f64,q[1] as f64,q[2] as f64,q[3] as f64))
        }).collect();
        for pair in quats.windows(2) {
            let degrees = (pair[0].inverse() * pair[1]).angle().to_degrees();
            assert!((degrees - 0.1).abs() <= 0.005, "rotation={degrees} degrees");
        }
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn files_without_motion_never_request_translation() {
        let stab = analysis_fixture(90, |_| crate::Quat64::identity());
        {
            let mut gyro = stab.gyro.write();
            gyro.quaternions.clear();
            gyro.file_metadata.write().quaternions.clear();
        }
        stab.optical_ui.write().translation_enabled = true;
        let m = analyze_pairs(&stab, 90, parallax_observations);
        assert!(!m.translation_requested);
        assert!(m.translation_samples.is_empty());
        assert!(!m.video_base.is_empty());
    }

    #[test]
    fn cloned_manager_keeps_the_optical_ui() {
        let stab = manager();
        stab.optical_ui.write().translation_enabled = true;
        assert!(stab.get_cloned().optical_ui.read().translation_enabled);
    }

    #[test]
    fn context_checksum_tracks_the_sync() {
        let stab = manager();
        let c0 = context_checksum(&measurement_params(&stab));
        assert_eq!(c0, context_checksum(&measurement_params(&stab)));
        stab.gyro.write().set_offset(5_000_000, 12.0);
        assert_ne!(c0, context_checksum(&measurement_params(&stab)));
        stab.gyro.write().remove_offset(5_000_000);
        assert_eq!(c0, context_checksum(&measurement_params(&stab)));
    }

    #[test]
    fn ridge_falls_with_strength() {
        let lo = solver_params(&OpticalCorrectionSettings { strength: 0.2 }, 30.0);
        let hi = solver_params(&OpticalCorrectionSettings { strength: 0.8 }, 30.0);
        assert!(hi.ridge < lo.ridge);
        assert!((lo.spacing_us - 1e6 / 30.0 / 6.0).abs() < 1e-6);
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn analysis_without_frames_reports_nothing_tracked() {
        let stab = manager();
        let analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(analysis.finish().err().as_deref(), Some("Not enough of the image could be tracked"));
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn analysis_stops_when_the_generation_moves_on() {
        let stab = manager();
        let analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        stab.optical_generation.fetch_add(1, SeqCst);
        assert!(analysis.is_cancelled());
        assert_eq!(analysis.finish().err().as_deref(), Some("Cancelled"));
    }
}


#[cfg(test)]
mod sensor_solve_tests {
    use super::*;
    use crate::gyro_source::optical_stab::{self, StabReconConfig};

    fn measurements(quats: &TimeQuat, pairs: Vec<sensor::SensorPair>, fps: f64) -> OpticalMeasurements {
        let mut bands = Vec::new();
        for pair in &pairs {
            for band in 0..6 {
                let indices: Vec<_> = pair.points.iter().enumerate().filter_map(|(i, p)| (p.band == band).then_some(i)).collect();
                if let Some(fit) = sensor::fit_band_shift(pair, &indices) { bands.push(fit); }
            }
        }
        OpticalMeasurements { bands: vec![], stab_requested: true, stab_pairs: pairs, stab_bands: bands,
            translation_requested: false, translation_samples: vec![], scaled_fps: fps,
            quats_checksum: optical_correction::checksum(quats), context_checksum: 9, video_base: vec![],
            frames: 300, measured_frames: 0, generation: 0 }
    }

    fn prior_scene(seconds: f64, cutoff: f64, include: impl Fn(f64) -> bool) -> (TimeQuat, OpticalMeasurements) {
        let q = |t: f64| UnitQuaternion::from_scaled_axis(Vector3::new(0.0, 0.005 * (std::f64::consts::TAU * 3.0 * t + 0.7).sin(), 0.0));
        let quats: TimeQuat = (0..=(seconds * 1000.0) as i64).map(|i| (i * 1000, q(i as f64 / 1000.0))).collect();
        let times: Vec<_> = (0..=(seconds * 30.0).round() as usize).map(|i| i as f64 * 1e6 / 30.0).collect();
        let truth = optical_stab::prior(&quats, cutoff, &times);
        let pairs = times.windows(2).enumerate().filter(|(_, t)| include(t[0] / 1e6) && include(t[1] / 1e6)).map(|(i, t)| {
            sensor::tests::physical_pair(i * 2 + 7, t[0], t[1], q(t[0] / 1e6), q(t[1] / 1e6), truth[i], truth[i + 1])
        }).collect();
        let m = measurements(&quats, pairs, 30.0);
        (quats, m)
    }

    #[test]
    fn solve_stab_short_measurements_use_the_default_cutoff() {
        let (q, m) = prior_scene(1.5, optical_stab::DEFAULT_CUTOFF_HZ, |_| true);
        assert_eq!(m.stab_bands.len(), 45 * 6);
        let result = solve_stab(&m, &q, &StabReconConfig::DEFAULT).unwrap();
        assert_eq!(result.cutoff_hz, optical_stab::DEFAULT_CUTOFF_HZ);
        assert_eq!(result.measured_frames, 45);
    }

    #[test]
    fn solve_stab_no_stab_bands_is_an_error() {
        let q = TimeQuat::new();
        let m = measurements(&q, vec![], 30.0);
        assert_eq!(solve_stab(&m, &q, &StabReconConfig::DEFAULT).unwrap_err(), "Not enough of the image could be tracked");
    }
    #[test]
    fn solve_stab_unmeasured_stretches_follow_the_prior() {
        let (q,m)=prior_scene(6.0,0.5,|t| t<=2.0 || t>=4.0);
        let result=solve_stab(&m,&q,&StabReconConfig {cutoff_hz:Some(0.5),..StabReconConfig::DEFAULT}).unwrap();
        let times:Vec<_>=(2500..=3500).map(|i|i as f64*1000.0).collect();
        let truth=optical_stab::prior(&q,0.5,&times);
        let amplitude=truth.iter().map(|s|s.norm()).fold(0.0,f64::max);
        for (&t,s) in times.iter().zip(truth) { assert!((result.at(t)-s).norm()<=0.05*amplitude); }
        let first=optical_stab::prior(&q,0.5,&[0.0])[0];
        assert!(first.norm()>0.001);
        assert!((result.at(0.0)-first).norm()*500.0<0.1);
    }

    #[test]
    fn solve_stab_nonzero_observed_correction_decays_inside_the_gap() {
        let (q, mut m)=prior_scene(6.0,0.5,|t|t<=2.0 || t>=4.0);
        let mut pairs=Vec::new();
        for pair in &m.stab_pairs {
            let times=[pair.points[0].ta_us,pair.points[0].tb_us];
            let truth=optical_stab::prior(&q,0.5,&times);
            let u=|t:f64|Vector3::new(0.001*(std::f64::consts::TAU*2.0*t/1e6).sin(),0.0007*(std::f64::consts::TAU*3.0*t/1e6).sin(),0.0);
            pairs.push(sensor::tests::physical_pair(pair.seq,times[0],times[1],
                GyroSource::clamped_quat_at_gyro_timestamp(&q,times[0]/1000.0),GyroSource::clamped_quat_at_gyro_timestamp(&q,times[1]/1000.0),truth[0]+u(times[0]),truth[1]+u(times[1])));
        }
        m=measurements(&q,pairs,30.0);
        let result=solve_stab(&m,&q,&StabReconConfig {cutoff_hz:Some(0.5),..StabReconConfig::DEFAULT}).unwrap();
        let times:Vec<_>=(2500..=3500).map(|i|i as f64*1000.0).collect();
        let truth=optical_stab::prior(&q,0.5,&times);
        let amplitude=truth.iter().map(|s|s.norm()).fold(0.0,f64::max);
        let maximum=times.iter().zip(&truth).map(|(&t,s)|(result.at(t)-s).norm()).fold(0.0,f64::max);
        println!("gap nonzero u maximum={maximum} limit={}",0.05*amplitude);
        assert!(maximum<=0.05*amplitude);
        assert!(result.u_at(250_000.0).norm()>0.0002);
    }

    #[test]
    fn solve_stab_cutoff_fit_lands_next_to_the_truth() {
        let body=|t:f64|UnitQuaternion::from_scaled_axis(Vector3::new(0.0,[0.07,0.2,0.5,1.3,3.0].iter().map(|f|0.2f64.to_radians()*(std::f64::consts::TAU*f*t).sin()).sum(),0.0));
        let q:TimeQuat=(0..=12000).map(|i|(i*1000,body(i as f64/1000.0))).collect();
        let times:Vec<_>=(30..=330).map(|i|i as f64*1e6/30.0).collect();
        let truth=optical_stab::prior(&q,optical_stab::CUTOFF_GRID_HZ[8],&times);
        let pairs=times.windows(2).enumerate().map(|(i,t)|sensor::tests::physical_pair(i*2+7,t[0],t[1],body(t[0]/1e6),body(t[1]/1e6),truth[i],truth[i+1])).collect();
        let m=measurements(&q,pairs,30.0);
        let result=solve_stab(&m,&q,&StabReconConfig::DEFAULT).unwrap();
        let index=optical_stab::CUTOFF_GRID_HZ.iter().position(|f|*f==result.cutoff_hz).unwrap();
        println!("cutoff truth_index=8 fitted_index={index}");
        assert!(index.abs_diff(8)<=1);
    }

    #[test]
    fn solve_stab_bands_without_raw_points_cannot_fall_back() {
        let (q,mut m)=prior_scene(0.1,0.3,|_|true); m.stab_pairs.clear();
        assert_eq!(solve_stab(&m,&q,&StabReconConfig::DEFAULT).unwrap_err(),"Not enough of the image could be tracked");
    }

    #[test]
    fn solve_stab_rejects_changed_quaternions_and_cancellation() {
        let (mut q,m)=prior_scene(0.1,0.3,|_|true);
        let count=std::cell::Cell::new(0);
        assert_eq!(solve_stab_with_cancel(&m,&q,&StabReconConfig::DEFAULT,|| { count.set(count.get()+1);count.get()>22 }).unwrap_err(),"Cancelled");
        q.insert(0,UnitQuaternion::from_euler_angles(0.1,0.0,0.0));
        assert_eq!(solve_stab(&m,&q,&StabReconConfig::DEFAULT).unwrap_err(),"Motion data changed after analysis; analyze again");
    }

    #[derive(Clone, Copy)]
    struct PhysicalEndpoint {
        sensor: crate::stabilization::SensorEndpoint,
        raw: (f32, f32),
        time: f64,
        body: nalgebra::Vector2<f64>,
        known: Vector3<f64>,
    }

    // The truth is declared before fitting: either exactly the prior, or zero sensor motion for OIS.
    fn physical_scene(focal: f64, axis: Option<usize>, seed: u64, ois: bool) -> (TimeQuat, OpticalMeasurements, Vec<Vec<PhysicalEndpoint>>) {
        use nalgebra::{Vector2, Rotation2};
        let fps=60.0;
        let frames=if ois {721} else {61};
        let (columns,rows)=if ois {(40,24)} else {(20,12)};
        let duration=(frames-1) as f64/fps;
        let body=|t:f64| if ois { UnitQuaternion::from_scaled_axis(Vector3::new(0.0,0.0,0.005*(std::f64::consts::TAU*3.0*t).sin())) }
            else { UnitQuaternion::from_scaled_axis(Vector3::new(0.003*(std::f64::consts::TAU*2.3*t+0.2).sin(),0.005*(std::f64::consts::TAU*3.0*t+0.7).sin(),0.002*(std::f64::consts::TAU*2.0*t+0.3).sin())) };
        let q:TimeQuat=(-1000..=((duration+1.0)*1000.0) as i64).map(|i|(i*1000,body(i as f64/1000.0))).collect();
        let body_at=|time:f64|GyroSource::clamped_quat_at_gyro_timestamp(&q,time/1000.0);
        let mut prior_sampler=optical_stab::PriorSampler::new(&q,0.5).unwrap();
        let mut state=seed;
        let mut uniform=||{state=state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);((state>>11) as f64+0.5)/((1u64<<53) as f64)};
        let worlds:Vec<_>=(0..columns*rows).map(|i| {
            let jitter=if ois {Vector2::zeros()} else {Vector2::new(uniform()-0.5,uniform()-0.5)*8.0};
            let px=Vector2::new((i%columns) as f64*960.0/columns as f64+480.0/columns as f64,(i/columns) as f64*540.0/rows as f64+270.0/rows as f64)+jitter;
            Vector3::new((px.x-480.0)*5.0/focal,(270.0-px.y)*5.0/focal,-5.0)
        }).collect();
        let mut all=Vec::new();
        let mut max_row_error:f64=0.0;
        let mut max_clock_error:f64=0.0;
        for frame in 0..frames {
            let middle=frame as f64*1e6/fps;
            let mut times=vec![middle;worlds.len()];
            let mut endpoints:Vec<PhysicalEndpoint>=Vec::new();
            // Solve the exposure time from the final recorded pixel, not from the uncorrected row.
            for _ in 0..12 {
                let truth=if ois {vec![Vector3::zeros();times.len()]} else {prior_sampler.sample(&times,&||false).unwrap()};
                endpoints=worlds.iter().zip(&times).zip(&truth).enumerate().map(|(index,((world,&time),s))|{
                    // Preserve all twelve passes, reusing only an identical deterministic OIS input.
                    if ois && endpoints.get(index).is_some_and(|p|p.time==time) {return endpoints[index];}
                    let ray=body_at(time).inverse()*world;
                    let body_pixel=Vector2::new(480.0+focal*ray.x/-ray.z,270.0-focal*ray.y/-ray.z);
                    let known=if ois {Vector3::zeros()} else {Vector3::new(2.0*(time/1e6).sin(),-1.0,0.007*(time/1e6).cos())};
                    let center=Vector2::new(480.0,270.0); let pivot=center+known.xy();
                    let z=Rotation2::new(-s.z)*(body_pixel-pivot-focal*s.xy())+pivot;
                    let raw=Rotation2::new(-known.z)*(z-pivot)+center;
                    PhysicalEndpoint {sensor:crate::stabilization::SensorEndpoint {sensor_full:[z.x as f32,z.y as f32],known_translation_full:[known.x as f32,known.y as f32]},raw:(raw.x as f32,raw.y as f32),time,body:body_pixel,known}
                }).collect();
                if axis.is_none() {break;}
                if let Some(axis)=axis {
                    let dimension=if axis==0 {960.0} else {540.0};
                    times=endpoints.iter().map(|p|middle+10000.0*((if axis==0 {p.raw.0} else {p.raw.1}) as f64/dimension-0.5)).collect();
                }
            }
            if let Some(axis)=axis {
                let dimension=if axis==0 {960.0} else {540.0};
                for p in &mut endpoints {
                    let coordinate=if axis==0 {p.raw.0} else {p.raw.1};
                    let magnitude=coordinate.abs();
                    let ulp=(f32::from_bits(magnitude.to_bits()+1)-magnitude) as f64;
                    let row_time=middle+10000.0*(coordinate as f64/dimension-0.5);
                    let error=(row_time-p.time).abs();
                    let clock_error=(row_time.round()-p.time.round()).abs();
                    assert!(error<=1.0+10000.0/dimension*ulp,"row time exceeds input precision: {error}");
                    assert!(clock_error<=1.0,"row time crosses more than one gyro clock cell: {clock_error}");
                    max_row_error=max_row_error.max(error);max_clock_error=max_clock_error.max(clock_error);
                    // All consumers use the time reconstructed from the final recorded f32 pixel.
                    p.time=row_time;
                }
            }
            all.push(endpoints);
        }
        let projection=std::sync::Arc::new(crate::stabilization::SensorProjection::test_pinhole(focal,focal,Vector2::repeat(1.0)));
        let pairs=all.windows(2).enumerate().map(|(i,frame)|{
            let points=frame[0].iter().zip(&frame[1]).enumerate().map(|(j,(a,b))|sensor::SensorPoint {
                a:a.sensor,b:b.sensor,ta_us:a.time,tb_us:b.time,gyro_ab:body_at(b.time).inverse()*body_at(a.time),
                band:if axis==Some(0) {((j%columns)*6/columns) as u8} else {((j/columns)*6/rows) as u8},
            }).collect();
            sensor::SensorPair {seq:3*i+11,frame_a:projection.clone(),frame_b:projection.clone(),duration_us:1e6/fps,points}
        }).collect();
        let m=measurements(&q,pairs,fps);
        println!("physical exposure f={focal} axis={axis:?} ois={ois} seed={seed:#018x} max_row_time_error_us={max_row_error} max_rounded_clock_error_us={max_clock_error}");
        drop(prior_sampler);
        (q,m,all)
    }

    fn rendered_error(result:&crate::gyro_source::optical_stab::OpticalStabReconstruction, focal:f64, frames:&[Vec<PhysicalEndpoint>], interior:bool) -> (f64,f64) {
        use nalgebra::Vector2;
        let manager=StabilizationManager::default();
        manager.init_from_video_data(12000.0,60.0,721,(960,540));
        manager.lens.write().load_from_json_value(&serde_json::json!({"calib_dimension":{"w":960,"h":540},"distortion_model":"opencv_standard","fisheye_params":{"camera_matrix":[[focal,0.0,480.0],[0.0,focal,270.0],[0.0,0.0,1.0]],"distortion_coeffs":[0.0,0.0,0.0,0.0]}}));
        let params=measurement_params(&manager);
        let k=Matrix3::new(focal,0.0,480.0,0.0,focal,270.0,0.0,0.0,1.0);
        let mut sum=0.0; let mut maximum:f64=0.0; let mut count=0;
        for frame in frames {
            if interior && !(2e6..=10e6).contains(&frame[0].time) {continue;}
            let raw:Vec<_>=frame.iter().map(|p|p.raw).collect();
            let shifts=frame.iter().map(|p|{let s=result.at(p.time);((p.known.x+focal*s.x) as f32,(p.known.y+focal*s.y) as f32,(p.known.z+s.z) as f32,0.0,0.0)}).collect();
            let output=crate::stabilization::undistort_points(&raw,k,&[0.0;24],Matrix3::identity(),None,None,&params,1.0,1.0,0.0,Some(shifts),None,0.0);
            for (p,out) in frame.iter().zip(output) {
                let actual=Vector2::new(480.0+focal*out.0 as f64,270.0+focal*out.1 as f64);
                let error=(actual-p.body).norm(); sum+=error*error; maximum=maximum.max(error);count+=1;
            }
        }
        ((sum/count as f64).sqrt(),maximum)
    }

    #[test]
    fn solve_stab_physical_six_fixed_seeds_focal_readout_and_recorded() {
        for i in 0u64..6 {
            let seed=0x7452_414e_534c_4154u64.wrapping_add(i.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            for focal in [500.0,15000.0] {for axis in [0,1] {
                let (q,m,truth)=physical_scene(focal,Some(axis),seed,false);
                let start=std::time::Instant::now();
                let result=solve_stab(&m,&q,&StabReconConfig {cutoff_hz:Some(0.5),..StabReconConfig::DEFAULT}).unwrap();
                let (rms,max)=rendered_error(&result,focal,&truth,false);
                println!("physical seed={seed:#018x} f={focal} readout_axis={axis} rms_px={rms} max_px={max} elapsed={:?} points={}",start.elapsed(),m.stab_pairs.iter().map(|p|p.points.len()).sum::<usize>());
                assert!(max<0.1,"seed={seed:#018x} f={focal} axis={axis} rms={rms} max={max}");
            }}
        }
    }

    fn check_ois_without_roll(focal:f64,axis:Option<usize>) {
            let (q,m,truth)=physical_scene(focal,axis,0x7452_414e_534c_4154,true);
            let result=solve_stab(&m,&q,&StabReconConfig {cutoff_hz:Some(0.5),..StabReconConfig::DEFAULT}).unwrap();
            let (rms,max)=rendered_error(&result,focal,&truth,true);
            let mut dense_sum=0.0; let mut dense_max:f64=0.0;
            for ms in 2000..=10000 {
                let s=result.at(ms as f64*1000.0);
                let shift=s.xy()*focal;
                let roll=nalgebra::Rotation2::new(s.z);
                dense_sum+=shift.norm_squared()+4.0*(s.z/2.0).sin().powi(2)*317.820409f64.powi(2);
                let body=GyroSource::clamped_quat_at_gyro_timestamp(&q,ms as f64);
                for x in [-468.0,468.0] {for y in [-258.75,258.75] {
                    let ray=body.inverse()*Vector3::new(x*5.0/focal,-y*5.0/focal,-5.0);
                    let pixel=nalgebra::Vector2::new(focal*ray.x/-ray.z,-focal*ray.y/-ray.z);
                    dense_max=dense_max.max((roll*pixel+shift-pixel).norm());
                }}
            }
            println!("OIS f={focal} axis={axis:?} observed_rms_px={rms} observed_max_px={max} unobserved_ms_rms_px={} unobserved_ms_max_px={dense_max}",(dense_sum/8001.0).sqrt());
            assert!(max<0.1,"OIS f={focal} axis={axis:?} rms={rms} max={max}");
    }

    #[test]
    fn solve_stab_ois_500_gs() {check_ois_without_roll(500.0,None);}

    #[test]
    fn solve_stab_ois_500_horizontal() {check_ois_without_roll(500.0,Some(0));}

    #[test]
    fn solve_stab_ois_500_vertical() {check_ois_without_roll(500.0,Some(1));}

    #[test]
    fn solve_stab_ois_15000_gs() {check_ois_without_roll(15000.0,None);}

    #[test]
    fn solve_stab_ois_15000_horizontal() {check_ois_without_roll(15000.0,Some(0));}

    #[test]
    fn solve_stab_ois_15000_vertical() {check_ois_without_roll(15000.0,Some(1));}


}
