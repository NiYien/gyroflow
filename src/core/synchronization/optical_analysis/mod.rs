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
pub mod residual_rotation;
mod image_space;
#[cfg(feature = "use-opencv")]
pub mod pipeline;
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
use translation::{ PairPoint, TranslationSolver, TranslationSolverConfig };
use crate::gyro_source::optical_translation::TranslationSample;
use super::optical_motion::tracks::Observation;
#[cfg(feature = "use-opencv")]
use super::optical_motion::{FrameTracker, tracker::KltTracker};
#[cfg(feature = "use-opencv")]
use super::GrayImage;

/// Tracks kept alive per frame
#[cfg(feature = "use-opencv")]
const MAX_POINTS: usize = 1500;
/// Width the frames are tracked at
#[cfg(feature = "use-opencv")]
const TRACK_WIDTH: u32 = 960;
/// Bands of rows (along the readout) each frame pair is measured in
const BANDS: usize = 6;
/// Frame pairs measured at once
const CHUNK: usize = 240;
const MIN_BAND_POINTS: usize = 25;
/// Floor of the uncertainty of one band's rotation, in pixels of the tracked frame
const SIGMA_FLOOR_PX: f64 = 0.01;

/// Which decoded frames are analyzed, their index and whether each continues the one before: what `feed_frame`
/// decides before tracking. A pipeline's decoder thread decides it with a copy, the same way
#[derive(Clone)]
pub(crate) struct FrameSequencer {
    /// The part of the clip analyzed: the trim ranges, in the file's own milliseconds
    ranges_ms: Vec<(f64, f64)>,
    fps_scale: Option<f64>,
    scaled_fps: f64,
    every_nth_frame: usize,
    /// Index and time of the frame accepted last
    last: Option<(usize, f64)>,
}

/// A frame `FrameSequencer::accept` let through
pub(crate) struct FrameSlot {
    pub index: usize,
    pub ts_ms: f64,
    pub continuous: bool,
}

impl FrameSequencer {
    fn source_fps(&self) -> f64 { self.scaled_fps / self.fps_scale.unwrap_or(1.0) }

    /// Check before grayscale conversion, and again in core for callers without early sampling.
    pub fn wants_frame(&self, timestamp_us: i64) -> bool {
        let file_ms = timestamp_us as f64 / 1000.0;
        self.ranges_ms.iter().any(|(a, b)| file_ms >= *a - 0.5 && file_ms <= *b + 0.5)
            && super::optical_sampling::keep_frame(timestamp_us, self.source_fps(), self.every_nth_frame)
    }

    /// Whether frame `index` at `timestamp_ms` follows the one at `last` without a gap: the next one sampled, within
    /// the same range
    fn continues(&self, last: Option<(usize, f64)>, index: usize, timestamp_ms: f64) -> bool {
        last.is_some_and(|(last_index, last_ms)| {
            let scale = self.fps_scale.unwrap_or(1.0);
            index == last_index + self.every_nth_frame && self.ranges_ms.iter().any(|(a, b)| {
                last_ms * scale >= *a - 0.5 && timestamp_ms * scale <= *b + 0.5
            })
        })
    }

    /// The decoded frame at this time if it's analyzed and not a repeat. It's then the last one
    pub fn accept(&mut self, timestamp_us: i64) -> Option<FrameSlot> {
        if !self.wants_frame(timestamp_us) { return None; }
        let file_ms = timestamp_us as f64 / 1000.0;
        let mut ts_ms = file_ms;
        if let Some(scale) = self.fps_scale { ts_ms /= scale; }
        let index = crate::frame_at_timestamp(ts_ms, self.scaled_fps).max(0) as usize;
        if self.last.map(|(last, _)| index <= last).unwrap_or(false) { return None; } // a repeated frame
        let continuous = self.continues(self.last, index, ts_ms);
        self.last = Some((index, ts_ms));
        Some(FrameSlot { index, ts_ms, continuous })
    }
}

/// Whether an analysis is cancelled, or overtaken by another file, a project or Clear. Latched, and shared by every
/// copy: the cancel flag is shared, and something else may lower it again before the analysis ends
#[derive(Clone)]
pub(crate) struct CancelCheck {
    flag: Arc<AtomicBool>,
    /// `StabilizationManager::optical_generation`, and its value when this started: another file, a project or Clear
    /// move it on, and cancel what's running for the one before
    generation: (Arc<AtomicU64>, u64),
    latched: Arc<AtomicBool>,
}

impl CancelCheck {
    pub fn is_cancelled(&self) -> bool {
        if !self.latched.load(Relaxed) && (self.flag.load(Relaxed) || self.generation.0.load(SeqCst) != self.generation.1) {
            self.latched.store(true, Relaxed);
        }
        self.latched.load(Relaxed)
    }
}

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
#[derive(Clone, Copy)]
struct Derived {
    id: u32,
    seq: usize,
    band: u8,
    /// Where the quaternions put the point in the second frame, and how far off that was (quaternion frame)
    p: Vector3<f64>,
    r: Vector3<f64>,
    ta_ms: f64,
    tb_ms: f64,
    /// The tracker's corner response in the second frame, NaN when unknown
    texture: f32,
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
    pub(crate) translation_replay: Option<TranslationReplay>,
    /// Pairs kept for the residual rotation: a file with motion data analyzed with the correction requested
    pub(crate) residual_replay: Option<ResidualReplay>,
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
    solve_stab_on_device(m,quats,config,-1,cancel)
}

pub(crate) fn solve_stab_on_device(m: &OpticalMeasurements, quats: &TimeQuat, config: &crate::gyro_source::optical_stab::StabReconConfig, processing_device:i32, cancel: impl Fn() -> bool) -> Result<crate::gyro_source::optical_stab::OpticalStabReconstruction, String> {
    use crate::gyro_source::optical_stab::{OpticalStabReconstruction, PriorError, PriorSampler, INPUT_CUTOFF_HZ};
    if cancel() { return Err("Cancelled".into()); }
    if optical_correction::checksum(quats) != m.quats_checksum {
        log::warn!("Sensor reconstruction rejected: motion checksum changed after analysis");
        return Err("Motion data changed after analysis; analyze again".into());
    }
    let prior_error = |error: PriorError| {
        log::warn!("Sensor reconstruction prior failed: {error:?}");
        if error == PriorError::Cancelled { "Cancelled".to_string() } else { format!("Sensor reconstruction prior failed: {error:?}") }
    };
    let cutoff_hz = config.cutoff_hz.unwrap_or(INPUT_CUTOFF_HZ);
    let frame_us = 1e6 / m.scaled_fps.max(1.0);
    let mut sampler = PriorSampler::new(quats, cutoff_hz).map_err(prior_error)?;
    let fit = sensor_solver::solve_response(&m.stab_pairs, &m.stab_bands, &mut sampler, frame_us, processing_device, &cancel).map_err(|error| {
        log::warn!("Sensor reconstruction failed: {error:?}");
        match error {
            sensor_solver::SensorSolveError::NoMeasurements => "Not enough of the image could be tracked".to_string(),
            sensor_solver::SensorSolveError::Cancelled => "Cancelled".to_string(),
            _ => format!("Sensor reconstruction failed: {error:?}"),
        }
    })?;
    log::info!("Sensor reconstruction response: bands={} pairs={} gain_5hz={:?} scale_px={} cutoff_hz={cutoff_hz}", fit.bands, fit.measured_pairs, fit.gain_5hz, fit.scale_px);
    if cancel() { return Err("Cancelled".into()); }
    let mut result = OpticalStabReconstruction::default();
    result.enabled = true;
    result.start_us = fit.start_us;
    result.end_us = fit.end_us;
    result.response = fit.taps.iter().map(|(lag_us, h)| crate::gyro_source::ResponseTap { lag_us: *lag_us, h: h.map(|v| v as f32) }).collect();
    result.cutoff_hz = cutoff_hz;
    result.quats_checksum = m.quats_checksum;
    result.context_checksum = m.context_checksum;
    result.frames = m.frames;
    result.measured_frames = fit.measured_pairs;
    result.rebuild_with_prior(&mut sampler, config, &cancel).map_err(prior_error)?;
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

/// The points of each pair from `from_seq` on, in the order `derived` has them: one pass instead of one per pair
fn points_by_pair(derived: &[Derived], from_seq: usize) -> HashMap<usize, Vec<PairPoint>> {
    let mut points: HashMap<usize, Vec<PairPoint>> = HashMap::new();
    for d in derived.iter().filter(|d| d.seq >= from_seq) {
        points.entry(d.seq).or_default().push(PairPoint { id: d.id, band: d.band, p: d.p, r: d.r, texture: d.texture });
    }
    points
}

/// A bearing from the camera's frame into the quaternions' frame: the axis flips `FrameTransform::at_timestamp` applies
/// (to upright frames, see `measurement_params`)
fn to_quat_frame(b: (f32, f32)) -> Vector3<f64> {
    Vector3::new(b.0 as f64, -b.1 as f64, -1.0).normalize()
}

struct TranslationState {
    solver: TranslationSolver,
    solve_time: std::time::Duration,
    layer_time: std::time::Duration,
    measured_pairs: usize,
    #[cfg(feature="use-opencv")]
    gpu_layer: Option<super::optical_motion::gpu_fitting::GpuLayers>,
    #[cfg(feature="use-opencv")]
    pending_layers: Vec<(usize,image_space::LayerInput)>,
    next_seq: usize,
    pairs: HashMap<usize, (Vector3<f64>, HashMap<u32, f64>)>,
    position: Vector3<f64>,
    segment: u32,
    depth_segment: u32,
    samples: Vec<TranslationSample>,
    median_depths: Vec<f64>,
    /// Numerator and denominator of each sample's dominant layer gain, see `finish`
    dominant: Vec<[f32; 2]>,
}

/// Tracks kept only until the sensor response is known; no second decode or tracking pass is needed.
pub(crate) struct TranslationReplay {
    params: ComputeParams,
    pairs: VecDeque<Pair>,
    track_size: (u32, u32),
    horizontal_readout: bool,
    focal_px: f64,
    processing_device: i32,
}

impl TranslationReplay {
    pub(crate) fn measure(mut self, reconstruction: &crate::gyro_source::OpticalStabReconstruction,
        cancel: &impl Fn() -> bool) -> Result<Vec<TranslationSample>, String> {
        let mut reconstruction = reconstruction.clone();
        reconstruction.enabled = true;
        reconstruction.applies = true;
        self.params.gyro.write().optical_stab = Some(reconstruction);
        self.params.apply_optical_stab = true;
        let mut state = TranslationState::with_device(self.processing_device);
        while !self.pairs.is_empty() {
            if cancel() { return Err("Cancelled".into()); }
            let pairs: VecDeque<_> = self.pairs.drain(..self.pairs.len().min(CHUNK)).collect();
            let mut derived = OpticalMotionAnalysis::derive_pairs(&self.params, &pairs, &None, self.track_size, self.horizontal_readout);
            let next_seq = pairs.back().unwrap().seq + 1;
            OpticalMotionAnalysis::measure_translation_pairs(&self.params, &mut state, &pairs, next_seq, self.focal_px, &mut derived, false, cancel);
        }
        if cancel() { return Err("Cancelled".into()); }
        Ok(state.finish())
    }
}

/// Points kept per frame pair for the residual rotation, half of them from each half of the tracks
const RESIDUAL_POINTS_PER_PAIR: usize = 256;
/// Memory of the kept points beyond which only every other kept pair stays (and only every other one is kept on)
const RESIDUAL_MEMORY_BYTES: usize = 128 << 20;

/// Per half of the tracks and equal-area cell (`residual_cell`), how many points were tracked in a pair
const RESIDUAL_CELL_COUNT: usize = residual_rotation::CELLS.0 * residual_rotation::CELLS.1;
type TrackedCounts = [u16; 2 * RESIDUAL_CELL_COUNT];

/// What the residual rotation (`residual_rotation`) needs from the tracking, kept while the analysis runs: every
/// pair's timing, and for every `step`-th pair a subsample of its points even over the frame and the two halves of the
/// tracks, with how many points each cell had before subsampling. `step` doubles whenever the points kept would take
/// more than `RESIDUAL_MEMORY_BYTES`
pub(crate) struct ResidualStore {
    kept: Vec<(Pair, TrackedCounts)>,
    timing: Vec<(usize, Frame, Frame)>,
    step: usize,
    bytes: usize,
    cap: usize,
}

impl ResidualStore {
    fn new() -> Self { Self::with_cap(RESIDUAL_MEMORY_BYTES) }
    fn with_cap(cap: usize) -> Self { Self { kept: Vec::new(), timing: Vec::new(), step: 1, bytes: 0, cap } }

    fn pair_bytes(p: &Pair) -> usize {
        p.obs.len() * std::mem::size_of::<Observation>() + std::mem::size_of::<Pair>() + std::mem::size_of::<TrackedCounts>()
    }

    fn push(&mut self, seq: usize, a: Frame, b: Frame, obs: &[Observation], size: (u32, u32)) {
        self.timing.push((seq, a, b));
        if seq % self.step != 0 { return; }
        let mut tracked = [0u16; 2 * RESIDUAL_CELL_COUNT];
        for o in obs {
            let k = (o.id % 2) as usize * RESIDUAL_CELL_COUNT + residual_cell(o.a, size) as usize;
            tracked[k] = tracked[k].saturating_add(1);
        }
        let pair = Pair { seq, a, b, obs: residual_subsample(obs, size) };
        self.bytes += Self::pair_bytes(&pair);
        self.kept.push((pair, tracked));
        while self.bytes > self.cap {
            self.step *= 2;
            let step = self.step;
            self.kept.retain(|(p, _)| p.seq % step == 0);
            // Thinned only a few times over a clip: give the room back
            self.kept.shrink_to_fit();
            self.bytes = self.kept.iter().map(|(p, _)| Self::pair_bytes(p)).sum();
        }
    }
}

/// The equal-area cell of a tracked position, row-major over `residual_rotation::CELLS`
fn residual_cell(pos: [f32; 2], size: (u32, u32)) -> u16 {
    let (cw, ch) = residual_rotation::CELLS;
    let cx = ((pos[0] / size.0.max(1) as f32 * cw as f32).floor() as i64).clamp(0, cw as i64 - 1);
    let cy = ((pos[1] / size.1.max(1) as f32 * ch as f32).floor() as i64).clamp(0, ch as i64 - 1);
    (cy * cw as i64 + cx) as u16
}

/// Up to `RESIDUAL_POINTS_PER_PAIR` points, half from each half of the tracks (id parity), taken one per cell in turn so
/// the whole frame is covered; in id order within a cell
fn residual_subsample(obs: &[Observation], size: (u32, u32)) -> Vec<Observation> {
    let (cw, ch) = residual_rotation::CELLS;
    let mut out = Vec::with_capacity(RESIDUAL_POINTS_PER_PAIR.min(obs.len()));
    for half in 0..2u32 {
        let mut cells: Vec<Vec<&Observation>> = vec![Vec::new(); cw * ch];
        let mut sorted: Vec<&Observation> = obs.iter().filter(|o| o.id % 2 == half).collect();
        sorted.sort_by_key(|o| o.id);
        for o in sorted { cells[residual_cell(o.a, size) as usize].push(o); }
        let quota = RESIDUAL_POINTS_PER_PAIR / 2;
        let (mut taken, mut round) = (0, 0);
        while taken < quota {
            let mut any = false;
            for cell in &cells {
                if let Some(o) = cell.get(round) {
                    any = true;
                    if taken < quota { out.push(**o); taken += 1; }
                }
            }
            if !any { break; }
            round += 1;
        }
    }
    out.sort_by_key(|o| o.id);
    out
}

/// The kept pairs, to measure the residual rotation once the reconstruction and the translation are known: no second
/// decode or tracking pass is needed
pub(crate) struct ResidualReplay {
    /// A copy of the analysis' own: the reconstruction is put into its motion data
    params: ComputeParams,
    /// The kept pairs, each with its points tracked per half and cell before subsampling
    kept: Vec<(Pair, TrackedCounts)>,
    timing: Vec<(usize, Frame, Frame)>,
    track_size: (u32, u32),
    horizontal_readout: bool,
    focal_px: f64,
    /// Analysis cadence, pairs per second
    fps: f64,
    /// Analyzed ranges, video milliseconds
    ranges_ms: Vec<(f64, f64)>,
}

impl ResidualReplay {
    /// Every kept point, measured against the uncorrected quaternions with the reconstruction that applies
    fn points(&mut self, reconstruction: Option<&crate::gyro_source::OpticalStabReconstruction>) -> Vec<residual_rotation::ResidualPoint> {
        if let Some(r) = reconstruction {
            let mut r = r.clone();
            r.enabled = true;
            r.applies = true;
            self.params.gyro.write().optical_stab = Some(r);
            self.params.apply_optical_stab = true;
        }
        let params = &self.params;
        let per_pair: Vec<Vec<residual_rotation::ResidualPoint>> = self.kept.par_iter().map(|(pair, _)| {
            let derived = OpticalMotionAnalysis::derive_pair(params, pair, &None, self.track_size, self.horizontal_readout);
            let cells: HashMap<u32, u16> = pair.obs.iter().map(|o| (o.id, residual_cell(o.a, self.track_size))).collect();
            let gyro = params.gyro.read();
            derived.iter().map(|d| residual_rotation::ResidualPoint {
                pair: pair.seq as u32, half: (d.id % 2) as u8, cell: cells[&d.id], ta_ms: d.ta_ms, tb_ms: d.tb_ms, p: d.p, r: d.r,
                rb: *gyro.org_quat_at_timestamp(d.tb_ms).to_rotation_matrix().matrix(),
            }).collect()
        }).collect();
        per_pair.into_iter().flatten().collect()
    }

    /// The motion one half of the tracks shows in a render with `render`'s settings: per pair the similarity fit of its
    /// points' projections (960 px wide output, 16 px margins, at least 20 points), integrated; pitch and yaw over the
    /// output focal length of each frame. Each kept point counts for the points its cell had before subsampling, so the
    /// fit sees the picture with the density of all the tracks: the subsample is even over the frame, and where layers
    /// of the picture move differently (parallax) an even one would weigh them by area instead of by what's visible
    fn held_out(&self, render: &ComputeParams, half: u32) -> residual_rotation::HeldOutMotion {
        let (ow, oh) = (render.output_width.max(1) as f64, render.output_height.max(1) as f64);
        let s = 960.0 / ow;
        let h960 = (oh * s).round();
        let centre = (480.0, h960 / 2.0);
        let to_source = |p: [f32; 2]| (p[0] * render.width as f32 / self.track_size.0.max(1) as f32, p[1] * render.height as f32 / self.track_size.1.max(1) as f32);
        let kept: HashMap<usize, &(Pair, TrackedCounts)> = self.kept.iter().map(|k| (k.0.seq, k)).collect();
        let rates: Vec<[f64; 3]> = self.timing.par_iter().map(|(seq, a, b)| {
            let Some((pair, tracked)) = kept.get(seq).copied() else { return [f64::NAN; 3] };
            let obs: Vec<&Observation> = pair.obs.iter().filter(|o| o.id % 2 == half).collect();
            let cells: Vec<usize> = obs.iter().map(|o| residual_cell(o.a, self.track_size) as usize).collect();
            let mut in_subsample = [0u32; RESIDUAL_CELL_COUNT];
            for c in &cells { in_subsample[*c] += 1; }
            let tracked = &tracked[half as usize * RESIDUAL_CELL_COUNT..];
            let density: Vec<f64> = cells.iter().map(|c| tracked[*c].max(1) as f64 / in_subsample[*c] as f64).collect();
            let pa: Vec<(f32, f32)> = obs.iter().map(|o| to_source(o.a)).collect();
            let pb: Vec<(f32, f32)> = obs.iter().map(|o| to_source(o.b)).collect();
            let oa = crate::stabilization::undistort_points_with_rolling_shutter(&pa, a.timestamp_ms, Some(a.index), render, 1.0, true, false);
            let ob = crate::stabilization::undistort_points_with_rolling_shutter(&pb, b.timestamp_ms, Some(b.index), render, 1.0, true, false);
            let inside = |p: (f64, f64)| p.0.is_finite() && p.1.is_finite() && p.0 > 16.0 && p.0 < 944.0 && p.1 > 16.0 && p.1 < h960 - 16.0;
            let (mut va, mut vb, mut prior) = (Vec::new(), Vec::new(), Vec::new());
            for ((x, y), w) in oa.iter().zip(&ob).zip(&density) {
                let (x, y) = ((x.0 as f64 * s, x.1 as f64 * s), (y.0 as f64 * s, y.1 as f64 * s));
                if inside(x) && inside(y) { va.push(x); vb.push(y); prior.push(*w); }
            }
            if va.len() < 20 { return [f64::NAN; 3]; }
            residual_rotation::fit_similarity(&va, &vb, centre, &prior)
        }).collect();
        let path = |c: usize| residual_rotation::cumulative(&rates.iter().map(|r| r[c]).collect::<Vec<_>>());
        let (roll, x, y) = (path(0), path(1), path(2));
        let f960 = self.focal_960();
        let mut times_ms = Vec::with_capacity(self.timing.len() + 1);
        let mut frames = Vec::with_capacity(self.timing.len() + 1);
        if let Some((_, a, _)) = self.timing.first() { times_ms.push(a.mid_ms); frames.push(a.index); }
        for (_, _, b) in &self.timing { times_ms.push(b.mid_ms); frames.push(b.index); }
        let out = (0..times_ms.len()).map(|i| {
            let fov = render.fovs.get(frames[i]).copied().filter(|f| *f > 0.0).unwrap_or(1.0);
            let fout = f960 / fov;
            [y[i] / fout, x[i] / fout, roll[i]]
        }).collect();
        residual_rotation::HeldOutMotion { times_ms, out, fps: self.fps }
    }

    /// The tracking's focal length in pixels of a 960 px wide picture
    fn focal_960(&self) -> f64 { self.focal_px * 960.0 / self.track_size.0.max(1) as f64 }

    /// The whole measurement: points with the reconstruction, the dominant layer of `translation` taken out, the
    /// strength from the two halves' motion in `render` (the current settings without this correction), the final δ
    /// on the motion data timeline
    pub(crate) fn measure(mut self, reconstruction: Option<&crate::gyro_source::OpticalStabReconstruction>,
        translation: Option<&crate::gyro_source::OpticalTranslation>, render: &ComputeParams, quats_checksum: u64,
        context_checksum: u64, cancel: &dyn Fn() -> bool) -> Result<crate::gyro_source::OpticalResidualRotation, String> {
        use crate::gyro_source::optical_residual;
        let started = std::time::Instant::now();
        let mut points = self.points(reconstruction);
        points.sort_by_key(|p| p.pair);
        let measured_frames = { let mut s: Vec<u32> = points.iter().map(|p| p.pair).collect(); s.dedup(); s.len() };
        if cancel() { return Err("Cancelled".into()); }
        let halves_ok = points.iter().any(|p| p.half == 0) && points.iter().any(|p| p.half == 1);
        let mut result = optical_residual::OpticalResidualRotation {
            version: optical_residual::VERSION, enabled: true, quats_checksum, context_checksum,
            reconstruction_checksum: reconstruction.map_or(0, |r| r.measurement_checksum()),
            translation_checksum: optical_residual::translation_fingerprint(translation),
            frames: self.timing.len(), measured_frames, spacing_us: residual_rotation::GRID_MS * 1000.0, tau_s: residual_rotation::TAUS_S[0],
            ..Default::default()
        };
        let Some(mut problem) = residual_rotation::ResidualProblem::new(points, self.focal_px).filter(|_| halves_ok && measured_frames >= 2) else {
            ::log::info!("Residual rotation: not enough tracked points ({measured_frames} pairs), strength zero");
            return Ok(result);
        };
        let gyro = self.params.gyro.read();
        let grid_t: Vec<f64> = (0..problem.nodes).map(|k| problem.grid_ms(k)).collect();
        if let Some(t) = translation {
            let samples: Vec<residual_rotation::DominantSample> = t.samples.iter().map(|s| {
                let q = UnitQuaternion::from_quaternion(nalgebra::Quaternion::new(s.camera_to_world[0] as f64, s.camera_to_world[1] as f64, s.camera_to_world[2] as f64, s.camera_to_world[3] as f64));
                residual_rotation::DominantSample { time_ms: s.timestamp_us as f64 / 1000.0, segment: s.segment,
                    motion: [s.layer_motion[0] as f64 * s.auto_beta as f64, s.layer_motion[1] as f64 * s.auto_beta as f64],
                    camera_to_world: *q.to_rotation_matrix().matrix() }
            }).collect();
            problem.subtract_dominant(&residual_rotation::dominant_chain(&samples, &grid_t));
        }
        let orientation: Vec<Matrix3<f64>> = grid_t.iter().map(|t| *gyro.org_quat_at_timestamp(*t).to_rotation_matrix().matrix()).collect();
        let held = [self.held_out(render, 0), self.held_out(render, 1)];
        if cancel() { return Err("Cancelled".into()); }
        let selected = residual_rotation::select_strength(&problem, [&held[0], &held[1]], &orientation, self.focal_960(), cancel);
        if cancel() { return Err("Cancelled".into()); }
        let Some(selection) = selected else {
            ::log::info!("Residual rotation: the held-out motion is too short to score or one half shows none, strength zero");
            return Ok(result);
        };
        result.sigma_deg = selection.sigma_deg;
        result.tau_s = selection.tau_s;
        result.scores = selection.scores;
        let to_gyro_ms = |t: f64| t - gyro.offset_at_video_timestamp(t);
        let t_first = grid_t.first().copied().unwrap_or_default();
        let t_last = grid_t.last().copied().unwrap_or_default();
        result.ranges_us = self.ranges_ms.iter().filter_map(|(a, b)| {
            let (a, b) = (a.max(t_first), b.min(t_last));
            (b > a).then(|| (to_gyro_ms(a) * 1000.0, to_gyro_ms(b) * 1000.0))
        }).collect();
        result.start_us = to_gyro_ms(t_first) * 1000.0;
        if selection.sigma_deg > 0.0 {
            let d = problem.estimate(selection.sigma_deg.to_radians(), selection.tau_s).ok_or_else(|| "The residual rotation could not be solved".to_string())?;
            // From the video grid onto a uniform one on the motion data timeline
            let d_w: Vec<[f32; 3]> = (0..problem.nodes).map(|k| {
                let tg = result.start_us / 1000.0 + k as f64 * residual_rotation::GRID_MS;
                let tv = tg + gyro.offset_at_gyro_timestamp(tg);
                let v = Vector3::from_fn(|c, _| residual_rotation::interp(tv, &grid_t, |i| d[i][c]));
                [v.x as f32, v.y as f32, v.z as f32]
            }).collect();
            result.d_w = optical_residual::quantize(&d_w);
            let inside: Vec<f64> = grid_t.iter().zip(&d).filter(|(t, _)| self.ranges_ms.iter().any(|(a, b)| **t >= *a && **t <= *b)).map(|(_, v)| v.norm_squared()).collect();
            result.rms_deg = (inside.iter().sum::<f64>() / inside.len().max(1) as f64).sqrt().to_degrees();
        }
        ::log::info!("Residual rotation: sigma={}deg tau={}s rms={:.4}deg pairs={}/{} nodes={} in {} ms",
            result.sigma_deg, result.tau_s, result.rms_deg, measured_frames, result.frames, problem.nodes, started.elapsed().as_millis());
        Ok(result)
    }
}

/// Time scale (Gaussian sigma) over which the automatic reference pools the dominant layer of neighbouring samples:
/// the window of the offline estimate it was validated with (target/translation-e0, 2026-10-09)
const AUTO_DOMINANT_SIGMA_S: f64 = 1.0;

/// Range of the automatic reference, in multiples of the median inverse depth
const AUTO_DOMINANT_RANGE: (f64, f64) = (0.02, 50.0);

impl TranslationState {
    fn new() -> Self {
        Self { solver: TranslationSolver::new(TranslationSolverConfig::resolved()),
            #[cfg(feature="use-opencv")] gpu_layer: None,
            #[cfg(feature="use-opencv")] pending_layers: Vec::new(),
            solve_time: std::time::Duration::ZERO, layer_time: std::time::Duration::ZERO, measured_pairs: 0,
            next_seq: 0, pairs: HashMap::new(), position: Vector3::zeros(), segment: 0, depth_segment: 0,
            samples: Vec::new(), median_depths: Vec::new(), dominant: Vec::new() }
    }
    fn with_device(device:i32)->Self {
        #[allow(unused_mut)] let mut state=Self::new();
        #[cfg(feature="use-opencv")] {state.gpu_layer=super::optical_motion::gpu_fitting::GpuLayers::for_device(device);}
        #[cfg(not(feature="use-opencv"))] let _=device;
        state
    }
    #[cfg(feature="use-opencv")]
    fn flush_layers(&mut self,cancel:&dyn Fn()->bool) {
        if self.pending_layers.is_empty() || cancel(){return;}
        let started=std::time::Instant::now();
        let pending=std::mem::take(&mut self.pending_layers);
        let results=if pending.len()>=8 {
            match self.gpu_layer.as_mut().map(|gpu|gpu.fit_batch(&pending.iter().map(|(_,input)|input).collect::<Vec<_>>())) {
                Some(Ok(results))=>Some(results),
                Some(Err(e))=>{log::warn!(target:"sync","Optical layer fit: CPU fallback, {e}");self.gpu_layer=None;None},
                None=>None,
            }
        }else{None};
        if cancel(){return;}
        for (slot,(index,input)) in pending.into_iter().enumerate() {
            let layer=results.as_ref().and_then(|r|r[slot]).map_or_else(||image_space::fit_prepared(&input),|v|image_space::finish(&input,Vector3::from(v)));
            let sample=&mut self.samples[index];
            sample.layer_motion=layer.motion;sample.layer_scale_rate=layer.scale_rate;sample.far_beta=layer.far_beta;sample.weight=layer.weight;
            self.dominant[index]=layer.dominant;self.median_depths[index]=layer.median_inv_depth;
        }
        self.layer_time+=started.elapsed();
    }
    /// Each sample's automatic layer: the dominant layer gain of the samples around it in its segment, numerators and
    /// denominators added up weighted by time and confidence, so that what a stretch of video shows decides.
    fn finish(self) -> Vec<TranslationSample> {
        #[cfg(feature="use-opencv")]
        if let Some(gpu)=&self.gpu_layer {if gpu.batches>0 {log::info!(target:"sync","Optical layer GPU: pairs={} batches={}",gpu.pairs,gpu.batches);}}
        log::info!(target: "sync", "Optical translation speed: pairs={} solve={:.1} ms image_layer={:.1} ms",
            self.measured_pairs, self.solve_time.as_secs_f64() * 1000.0, self.layer_time.as_secs_f64() * 1000.0);
        let TranslationState { mut samples, mut dominant, median_depths, .. } = self;
        // One constant unit per solve span. A changing visible depth distribution must not rescale each frame's motion.
        let mut start = 0;
        while start < samples.len() {
            let mut end = start + 1;
            while end < samples.len() && samples[end].segment == samples[start].segment
                && samples[end].depth_segment == samples[start].depth_segment { end += 1; }
            let (mut sum, mut weight) = (0.0, 0.0);
            for i in start..end {
                let depth = median_depths[i];
                if depth.is_finite() && depth > 0.0 && samples[i].weight > 0.0 {
                    let duration = if end - start == 1 { 1.0 } else {
                        (samples[(i + 1).min(end - 1)].timestamp_us - samples[i.saturating_sub(1).max(start)].timestamp_us) as f64
                    };
                    let w = duration * samples[i].weight as f64;
                    sum += w * depth.ln(); weight += w;
                }
            }
            if weight > 0.0 {
                let unit = (sum / weight).exp();
                for i in start..end {
                    for motion in &mut samples[i].layer_motion { *motion = (*motion as f64 * unit) as f32; }
                    samples[i].layer_scale_rate = (samples[i].layer_scale_rate as f64 * unit) as f32;
                    samples[i].far_beta = (samples[i].far_beta as f64 / unit) as f32;
                    dominant[i][0] = (dominant[i][0] as f64 * unit) as f32;
                    dominant[i][1] = (dominant[i][1] as f64 * unit * unit) as f32;
                }
            }
            start = end;
        }
        let seconds = |a: &TranslationSample, b: &TranslationSample| (a.timestamp_us - b.timestamp_us) as f64 / 1e6;
        let chosen: Vec<_> = (0..samples.len()).map(|i| {
            let (mut numerator, mut denominator) = (0.0, 0.0);
            let mut add = |j: usize| -> bool {
                let dt = seconds(&samples[j], &samples[i]);
                if samples[j].segment != samples[i].segment || samples[j].depth_segment != samples[i].depth_segment
                    || dt.abs() > 3.0 * AUTO_DOMINANT_SIGMA_S { return false; }
                let [n, d] = dominant[j];
                if n.is_finite() && d.is_finite() && d > 0.0 {
                    let k = (-0.5 * (dt / AUTO_DOMINANT_SIGMA_S).powi(2)).exp() * samples[j].weight.clamp(0.0, 1.0) as f64;
                    numerator += k * n as f64;
                    denominator += k * d as f64;
                }
                true
            };
            add(i);
            for j in (0..i).rev() { if !add(j) { break; } }
            for j in i + 1..samples.len() { if !add(j) { break; } }
            (denominator > 0.0).then(|| (numerator / denominator).clamp(AUTO_DOMINANT_RANGE.0, AUTO_DOMINANT_RANGE.1) as f32)
        }).collect();
        for (sample, chosen) in samples.iter_mut().zip(chosen) {
            sample.auto_beta = chosen.unwrap_or(0.0);
        }
        samples
    }
}

pub struct OpticalMotionAnalysis {
    /// Its `gyro` is a copy of the motion data without any optical correction: that's what the new one is measured against
    params: ComputeParams,
    processing_device: i32,
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
    /// Uncorrected bearings are fixed during reconstruction analysis; retain only the rolling window.
    derived_cache: HashMap<usize, Vec<Derived>>,
    #[cfg(test)]
    reuse_derived: bool,
    #[cfg(test)]
    derived_pair_evaluations: usize,
    next_seq: usize,
    measured_upto: usize,
    measurements: Vec<BandMeasurement>,
    measured_pairs: usize,
    frames: usize,
    total_frames: usize,
    /// A file without motion data: the rotation between each two frames, measured from their tracks and chained, at the
    /// frames' own times. What the rest of the analysis compares the image against instead of the quaternions
    vision: Option<TimeQuat>,
    translation: Option<TranslationState>,
    translation_replay: Option<VecDeque<Pair>>,
    residual: Option<ResidualStore>,
    stab_requested: bool,
    stab_next_seq: usize,
    stab_pairs: Vec<sensor::SensorPair>,
    stab_bands: Vec<sensor::SensorBand>,
    sensor_projections: HashMap<(usize, u64, (u32, u32)), Arc<crate::stabilization::SensorProjection>>,
    /// The robust rotation blended towards the odometry as its structure and leverage warrant.
    base: base::VisionBase,
    sg_cache: HashMap<usize, DMatrix<f64>>,
    /// Which frames `feed_frame` takes
    sequencer: FrameSequencer,
    cancel: CancelCheck,
    /// When it was made, with the sampling logged: a pipeline's speed log counts from here
    #[cfg_attr(not(feature = "use-opencv"), allow(dead_code))]
    started: std::time::Instant,
}

impl OpticalMotionAnalysis {
    pub fn from_manager(stab: &StabilizationManager, cancel_flag: Arc<AtomicBool>) -> Result<Self, String> {
        #[cfg(feature = "use-opencv")]
        let ui = *stab.optical_ui.read();
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
            gyro.strip_rotation_corrections();
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
            let processing_device=stab.params.read().current_device;
            let translation = (vision.is_none() && ui.translation_enabled).then(||TranslationState::with_device(processing_device));
            let translation_replay = (translation.is_some() && ui.stab_enabled).then(VecDeque::new);
            // With motion data the correction is the residual rotation, measured from a subsample of the tracks
            let residual = (vision.is_none() && ui.correction_enabled).then(ResidualStore::new);
            Ok(Self {
                params, fps_scale, scaled_fps, quats_checksum, context_checksum, horizontal_readout,
                every_nth_frame, hp_min, hp_max,
                track_size: (0, 0),
                focal_px: 0.0,
                tracker: KltTracker::with_device(MAX_POINTS, 1.0, TRACK_WIDTH,
                    super::optical_motion::tracker::KltDevice::for_processing_device(processing_device)),
                processing_device,
                last: None,
                pairs: VecDeque::new(),
                derived_cache: HashMap::new(),
                #[cfg(test)] reuse_derived: true,
                #[cfg(test)] derived_pair_evaluations: 0,
                next_seq: 0,
                measured_upto: 0,
                measurements: Vec::new(),
                measured_pairs: 0,
                frames: 0,
                total_frames,
                sequencer: FrameSequencer { ranges_ms, fps_scale, scaled_fps, every_nth_frame, last: None },
                stab_requested: vision.is_none() && ui.stab_enabled,
                stab_next_seq: 0, stab_pairs: Vec::new(), stab_bands: Vec::new(), sensor_projections: HashMap::new(),
                vision,
                translation,
                translation_replay,
                residual,
                base: base::VisionBase::new(OpticalBaseMode::resolved(), BlendConfig::resolved()),
                sg_cache: HashMap::new(),
                cancel: CancelCheck { flag: cancel_flag, generation: (stab.optical_generation.clone(), generation), latched: Arc::new(AtomicBool::new(false)) },
                started: std::time::Instant::now(),
            })
        }
    }

    /// Whether it's been cancelled, or overtaken by another file, a project or Clear: then the frames fed are ignored
    /// and `finish` says "Cancelled"
    pub fn is_cancelled(&self) -> bool { self.cancel.is_cancelled() }

    /// CPU placeholder after the configured tracker moves into the pipeline's tracking thread.
    #[cfg(feature = "use-opencv")]
    fn new_tracker() -> KltTracker { KltTracker::new(MAX_POINTS, 1.0, TRACK_WIDTH) }

    /// Frames fed so far and to be analyzed
    pub fn progress(&self) -> (usize, usize) { (self.frames, self.total_frames.max(self.frames)) }

    /// What to decode: the trim ranges, in the file's own milliseconds
    pub fn ranges_ms(&self) -> Vec<(f64, f64)> { self.sequencer.ranges_ms.clone() }

    pub fn frame_step(&self) -> usize { self.every_nth_frame }

    pub fn source_fps(&self) -> f64 { self.scaled_fps / self.fps_scale.unwrap_or(1.0) }

    /// Check before grayscale conversion, and again in core for callers without early sampling.
    pub fn wants_frame(&self, timestamp_us: i64) -> bool { self.sequencer.wants_frame(timestamp_us) }

    fn continuous(&self, index: usize, timestamp_ms: f64) -> bool {
        self.sequencer.continues(self.last.map(|last| (last.index, last.timestamp_ms)), index, timestamp_ms)
    }

    /// Feeds the next decoded frame, in decoding order: 8-bit luma, ideally about 1000 px wide. Frames outside of
    /// `ranges_ms` are ignored
    pub fn feed_frame(&mut self, timestamp_us: i64, width: u32, height: u32, stride: usize, pixels: &[u8]) -> Result<(), String> {
        if self.is_cancelled() { return Ok(()); }
        let Some(FrameSlot { index, ts_ms, continuous }) = self.sequencer.accept(timestamp_us) else { return Ok(()) };

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
                // Prepared like the pipeline does, corner response included: it gives each point its texture
                let frame = KltTracker::prepare_for_backend(&image, TRACK_WIDTH, self.tracker.uses_gpu())
                    .map(|mut frame| { frame.precompute_corners(); frame });
                let (obs, tracked_size) = self.tracker.track_prepared(frame)?;
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
            self.derived_cache.clear();
            self.track_size = size;
            let (k, ..) = FrameTransform::get_lens_data_at_timestamp(&self.params, ts_ms, false);
            self.focal_px = k[(0, 0)] * size.0 as f64 / self.params.width.max(1) as f64;
        }
        let frame = self.frame(index, ts_ms);
        let continuous = self.continuous(index, ts_ms);
        if continuous && !obs.is_empty() {
            if let Some(a) = self.last {
                if let Some(store) = &mut self.residual { store.push(self.next_seq, a, frame, &obs, size); }
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
        let residual_replay = self.residual.take().map(|store| {
            ::log::info!("Residual rotation kept {} of {} pairs (every {}), {} points, {:.1} MB",
                store.kept.len(), store.timing.len(), store.step, store.kept.iter().map(|(p, _)| p.obs.len()).sum::<usize>(), store.bytes as f64 / 1048576.0);
            let mut params = self.params.clone();
            params.gyro = Arc::new(parking_lot::RwLock::new(self.params.gyro.read().clone()));
            let scale = self.fps_scale.unwrap_or(1.0);
            ResidualReplay { params, kept: store.kept, timing: store.timing, track_size: self.track_size,
                horizontal_readout: self.horizontal_readout, focal_px: self.focal_px,
                fps: self.scaled_fps / self.every_nth_frame as f64,
                ranges_ms: self.sequencer.ranges_ms.iter().map(|(a, b)| (a / scale, b / scale)).collect() }
        });
        let translation_replay = self.translation_replay.take().map(|mut pairs| {
            pairs.append(&mut self.pairs);
            TranslationReplay { params: self.params, pairs, track_size: self.track_size,
                horizontal_readout: self.horizontal_readout, focal_px: self.focal_px, processing_device:self.processing_device }
        });
        Ok(OpticalMeasurements {
            bands: self.measurements,
            stab_requested: self.stab_requested,
            stab_pairs: self.stab_pairs,
            stab_bands: self.stab_bands,
            translation_requested: self.translation.is_some(),
            translation_samples: self.translation.map(TranslationState::finish).unwrap_or_default(),
            translation_replay,
            residual_replay,
            scaled_fps: self.scaled_fps / self.every_nth_frame as f64,
            quats_checksum,
            context_checksum: self.context_checksum,
            video_base,
            frames: self.frames,
            measured_frames: self.measured_pairs,
            generation: self.cancel.generation.1,
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

        let mut derived = self.derive((!last_call).then_some(end.saturating_sub(self.hp_max)));
        if self.translation_replay.is_none() { self.measure_translation(&mut derived); }
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
        let mut ms: Vec<(usize, u8, BandMeasurement)> = groups.par_iter().filter_map(|(&(seq, band), idx)| {
            let gyro = gyro.read();
            fit_band(&derived, &rhp, idx, floor, &gyro, vision).map(|mut m| { m.pair = seq; (seq, band, m) })
        }).collect();
        // The hash map hands the bands over in any order. With a global shutter all bands of a pair have the same
        // time, so the band decides: the same video always gives the same measurements, in the same order
        ms.sort_by(|a, b| a.0.cmp(&b.0).then(a.2.ta_us.total_cmp(&b.2.ta_us)).then(a.1.cmp(&b.1)));
        let mut seqs: Vec<usize> = ms.iter().map(|(s, ..)| *s).collect();
        seqs.sort_unstable();
        seqs.dedup();
        self.measured_pairs += seqs.len();
        self.measurements.extend(ms.into_iter().map(|(.., m)| m));

        self.measured_upto = end;
        let keep_from = end.saturating_sub(self.hp_max);
        while self.pairs.front().map(|p| p.seq < keep_from).unwrap_or(false) {
            if let Some(pair) = self.pairs.pop_front() {
                self.derived_cache.remove(&pair.seq);
                if let Some(state) = &mut self.translation { state.pairs.remove(&pair.seq); }
                if let Some(replay) = &mut self.translation_replay { replay.push_back(pair); }
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
        Self::measure_translation_pairs(&self.params, state, &self.pairs, self.next_seq, self.focal_px, derived, true, &||self.cancel.is_cancelled());
    }

    fn measure_translation_pairs(params: &ComputeParams, state: &mut TranslationState, pairs: &VecDeque<Pair>,
        next_seq: usize, focal_px: f64, derived: &mut [Derived], remove_parallax: bool, cancel:&dyn Fn()->bool) {
        // Lens queries take the gyro lock themselves. Complete them before reading poses.
        let mut focal_ratios = HashMap::new();
        let mut output_projections = HashMap::new();
        for pair in pairs.iter().filter(|pair| pair.seq >= state.next_seq) {
            for frame in [pair.a, pair.b] {
                focal_ratios.entry(frame.index).or_insert_with(|| {
                    let (mut k, ..) = FrameTransform::get_lens_data_at_timestamp(params, frame.timestamp_ms, false);
                    FrameTransform::dequantize_camera_matrix(params, frame.index, &mut k);
                    (k[(0, 0)] / params.width.min(params.height).max(1) as f64) as f32
                });
            }
            let centre = [(params.width as f32 / 2.0, params.height as f32 / 2.0)];
            let transform = FrameTransform::at_timestamp_for_points(params, &centre, pair.b.timestamp_ms, Some(pair.b.index), true);
            output_projections.insert(pair.b.index, transform.3[0]);
        }
        let gyro = params.gyro.read();
        let camera_to_world = |timestamp| {
            let q = gyro.org_quat_at_timestamp(timestamp);
            [q.w as f32, q.i as f32, q.j as f32, q.k as f32]
        };
        let mut points_of = points_by_pair(derived, state.next_seq);
        let from_seq=state.next_seq;
        for pair in pairs.iter().filter(|pair| pair.seq >= from_seq) {
            if cancel(){return;}
            let points = points_of.remove(&pair.seq).unwrap_or_default();
            let start_us = (pair.a.mid_ms * 1000.0).round() as i64;
            if state.samples.last().is_none_or(|s| s.timestamp_us != start_us) {
                if !state.samples.is_empty() { state.segment += 1; }
                state.solver.reset();
                state.position = Vector3::zeros();
                state.dominant.push([0.0; 2]);
                state.median_depths.push(0.0);
                state.samples.push(TranslationSample {
                    timestamp_us: start_us,
                    segment: state.segment,
                    depth_segment: state.depth_segment,
                    camera_to_world: camera_to_world(pair.a.mid_ms),
                    focal_length_over_short_side: focal_ratios[&pair.a.index],
                    ..Default::default()
                });
            }
            let started = std::time::Instant::now();
            let result = state.solver.step(&points, 1.0 / focal_px, pair.b.mid_ms / 1000.0);
            state.solve_time += started.elapsed();
            state.measured_pairs += 1;
            if result.new_segment && state.samples.len() > 1 { state.depth_segment += 1; }
            let started = std::time::Instant::now();
            #[cfg(feature="use-opencv")] let use_gpu=state.gpu_layer.is_some();
            #[cfg(not(feature="use-opencv"))] let use_gpu=false;
            #[cfg(feature="use-opencv")]
            let pending_layer=if use_gpu {image_space::prepare(&points,&result,&output_projections[&pair.b.index],(params.output_width,params.output_height))}else{None};
            let layer=if use_gpu {image_space::LayerMotion::default()}else{image_space::fit(&points,&result,&output_projections[&pair.b.index],(params.output_width,params.output_height))};
            state.layer_time += started.elapsed();
            if result.new_segment { state.position = Vector3::zeros(); }
            if result.confidence > 0.0 {
                state.position += gyro.org_quat_at_timestamp(pair.b.mid_ms) * result.c_segment;
                if remove_parallax { state.pairs.insert(pair.seq, (result.c, result.inv_depth)); }
            }
            state.samples.push(TranslationSample {
                timestamp_us: (pair.b.mid_ms * 1000.0).round() as i64,
                position: [state.position.x as f32, state.position.y as f32, state.position.z as f32],
                ref_inv_depth: result.ref_inv_depth as f32,
                confidence: result.confidence as f32,
                track_age_s: result.track_age_s as f32,
                segment: state.segment,
                depth_segment: state.depth_segment,
                camera_to_world: camera_to_world(pair.b.mid_ms),
                focal_length_over_short_side: focal_ratios[&pair.b.index],
                layer_motion: layer.motion,
                layer_scale_rate: layer.scale_rate,
                far_beta: layer.far_beta,
                weight: layer.weight,
                ..Default::default()
            });
            state.dominant.push(layer.dominant);
            state.median_depths.push(layer.median_inv_depth);
            #[cfg(feature="use-opencv")]
            if let Some(input)=pending_layer {
                state.pending_layers.push((state.samples.len()-1,input));
                if state.pending_layers.len()>=super::optical_motion::gpu_fitting::GpuLayers::BATCH {state.flush_layers(cancel);}
            }
        }
        #[cfg(feature="use-opencv")] state.flush_layers(cancel);
        state.next_seq = next_seq;
        if remove_parallax {
            for d in derived {
                if let Some((c, depth)) = state.pairs.get(&d.seq) {
                    if let Some(rho) = depth.get(&d.id) { d.r += *rho * (c - c.dot(&d.p) * d.p); }
                }
            }
        }
    }

    /// Residuals of every point of the pairs held, against the quaternions
    fn derive(&mut self, keep_from: Option<usize>) -> Vec<Derived> {
        let reuse = self.stab_requested;
        #[cfg(test)] let reuse = reuse && self.reuse_derived;
        if !reuse || (keep_from.is_none() && self.derived_cache.is_empty()) {
            #[cfg(test)] { self.derived_pair_evaluations += self.pairs.len(); }
            return Self::derive_pairs(&self.params, &self.pairs, &self.vision, self.track_size, self.horizontal_readout);
        }
        let missing: Vec<_> = self.pairs.iter().filter(|pair| !self.derived_cache.contains_key(&pair.seq)).collect();
        #[cfg(test)] { self.derived_pair_evaluations += missing.len(); }
        let measured: Vec<_> = missing.par_iter().map(|pair|
            Self::derive_pair(&self.params, pair, &self.vision, self.track_size, self.horizontal_readout)).collect();
        let mut measured = measured.into_iter();
        let mut out = Vec::with_capacity(self.pairs.iter().map(|pair| pair.obs.len()).sum());
        for pair in &self.pairs {
            if let Some(points) = self.derived_cache.get(&pair.seq) {
                out.extend_from_slice(points);
            } else {
                let points = measured.next().unwrap();
                out.extend_from_slice(&points);
                // Only the overlap with a later processing block has another consumer.
                if keep_from.is_some_and(|start| pair.seq >= start) { self.derived_cache.insert(pair.seq, points); }
            }
        }
        out
    }

    fn derive_pairs(params: &ComputeParams, pairs: &VecDeque<Pair>, vision: &Option<TimeQuat>, size: (u32, u32), horizontal: bool) -> Vec<Derived> {
        let per_pair: Vec<_> = pairs.par_iter().map(|pair| Self::derive_pair(params, pair, vision, size, horizontal)).collect();
        per_pair.into_iter().flatten().collect()
    }

    fn derive_pair(params: &ComputeParams, pair: &Pair, vision: &Option<TimeQuat>, size: (u32, u32), horizontal: bool) -> Vec<Derived> {
        let track_rows = if horizontal { size.0 } else { size.1 }.max(1) as f32;
        let pts_a: Vec<(f32, f32)> = pair.obs.iter().map(|o| (o.a[0], o.a[1])).collect();
        let pts_b: Vec<(f32, f32)> = pair.obs.iter().map(|o| (o.b[0], o.b[1])).collect();
        let ba = undistort_points_to_plane(&pts_a, pair.a.timestamp_ms, pair.a.index, params, size);
        let bb = undistort_points_to_plane(&pts_b, pair.b.timestamp_ms, pair.b.index, params, size);
        let gyro = params.gyro.read();
        let orient = |t: f64| orientation(vision, &gyro, t);
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
            out.push(Derived { id: o.id, seq: pair.seq, band, p, r: vb - p, ta_ms: ta, tb_ms: tb, texture: o.texture });
        }
        out
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

    #[test]
    fn translation_auto_layer_is_the_pooled_dominant_gain() {
        let mut state = TranslationState::new();
        state.samples.push(TranslationSample { timestamp_us: 0, ..Default::default() });
        state.dominant.push([0.0; 2]);
        // Single pairs alternate between two layers: the pooled gain is steady in between
        for i in 1..60 {
            let gain = if i % 2 == 0 { 2.0 } else { 0.5 };
            state.samples.push(TranslationSample { timestamp_us: i * 40_000, weight: 1.0, ..Default::default() });
            state.dominant.push([gain * 1e-6, 1e-6]);
        }
        // A sample of another segment neither joins nor takes from the pool, and a segment without any measurement
        // stays unmeasured
        state.samples.push(TranslationSample { timestamp_us: 60 * 40_000, segment: 1, weight: 1.0, ..Default::default() });
        state.dominant.push([9e-6, 1e-6]);
        state.samples.push(TranslationSample { timestamp_us: 61 * 40_000, segment: 2, weight: 1.0, ..Default::default() });
        state.dominant.push([0.0, 0.0]);
        state.median_depths = vec![1.0; state.samples.len()];
        let samples = state.finish();
        for s in &samples[5..55] { assert!((s.auto_beta - 1.25).abs() < 0.05, "{}", s.auto_beta); }
        for w in samples[5..55].windows(2) { assert!((w[1].auto_beta / w[0].auto_beta - 1.0).abs() < 0.02); }
        assert!(samples[0].auto_beta > 0.0, "the segment start takes its neighbours' layer");
        assert!((samples[60].auto_beta - 9.0).abs() < 1e-5, "{}", samples[60].auto_beta);
        assert_eq!(samples[61].auto_beta, 0.0);
    }

    #[test]
    fn translation_depth_units_are_normalized_once_per_solve_span() {
        let build = |scale: f64| {
            let mut state = TranslationState::new();
            for i in 0..120 {
                let depth_segment = u32::from(i >= 60);
                let unit = if depth_segment == 0 { 1.0 } else { scale };
                state.samples.push(TranslationSample { timestamp_us: i * 40_000, depth_segment,
                    weight: 1.0, layer_motion: [(0.002 * unit) as f32, 0.0],
                    far_beta: (0.5 / unit) as f32, ..Default::default() });
                // The tracked area alternates between near and far, without any change in camera velocity.
                state.median_depths.push(if i % 2 == 0 { 0.5 / unit } else { 2.0 / unit });
                state.dominant.push([(4e-6 * unit) as f32, (4e-6 * unit * unit) as f32]);
            }
            state.finish()
        };
        let reference = build(1.0);
        let changed = build(32.0);
        for (a, b) in reference.iter().zip(&changed) {
            assert!((a.layer_motion[0] * a.auto_beta - b.layer_motion[0] * b.auto_beta).abs() * 4616.63 < 0.01);
            assert!((b.layer_motion[0] * b.auto_beta - 0.002).abs() * 4616.63 < 0.01);
            assert!((b.layer_motion[0] * b.far_beta - 0.001).abs() * 4616.63 < 0.01);
        }
    }

    #[test]
    fn points_grouped_by_pair_match_a_scan_per_pair() {
        // Pairs laid out one after the other as `derive` does, and interleaved, which it doesn't
        let point = |seq: usize, id: u32| Derived {
            id, seq, band: (id % 6) as u8,
            p: Vector3::new(id as f64 * 0.1, seq as f64 * -0.2, 1.0 + id as f64 * 1e-3), r: Vector3::new(1e-4 * id as f64, -3e-5, 7e-6 * seq as f64),
            ta_ms: 0.0, tb_ms: 0.0, texture: id as f32,
        };
        let laid_out: Vec<_> = (3..9).flat_map(|seq| (0..40).map(move |id| point(seq, id * 7 + seq as u32))).collect();
        let interleaved: Vec<_> = (0..120u32).map(|i| point(3 + (i as usize * 5) % 6, i)).collect();
        let bits = |points: &[PairPoint]| points.iter().map(|p| (p.id, p.band, p.p.map(f64::to_bits), p.r.map(f64::to_bits))).collect::<Vec<_>>();
        for derived in [&laid_out, &interleaved] {
            for from_seq in [0, 3, 5, 9] {
                let mut grouped = points_by_pair(derived, from_seq);
                for seq in from_seq..10 {
                    let scanned: Vec<PairPoint> = derived.iter().filter(|d| d.seq == seq)
                        .map(|d| PairPoint { id: d.id, band: d.band, p: d.p, r: d.r, texture: d.texture }).collect();
                    assert_eq!(bits(&grouped.remove(&seq).unwrap_or_default()), bits(&scanned), "seq {seq} from {from_seq}");
                }
                assert!(grouped.is_empty(), "nothing before from_seq");
            }
        }
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn raw_sensor_short_tracks_finish_and_duplicate_flush() {
        let stab = analysis_fixture(8, |_| crate::Quat64::identity());
        stab.optical_ui.write().stab_enabled = true;
        let mut analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        for frame in 1..8 {
            let obs = (0..400).map(|id| {
                let a = [(id % 20) as f32 * 40.0 + 50.0, (id / 20) as f32 * 25.0 + 20.0];
                Observation { id, a, b: [a[0] - 1.0, a[1] + 0.5], texture: f32::NAN }
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
    fn raw_sensor_defers_translation_until_reconstruction() {
        let stab = analysis_fixture(8, |_| crate::Quat64::identity());
        { let mut ui = stab.optical_ui.write(); ui.stab_enabled = true; ui.translation_enabled = true; }
        let result = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        assert!(result.translation.is_some());
        assert!(result.translation_replay.is_some());
        assert_eq!(result.every_nth_frame, 1);
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn reconstruction_reuses_each_uncorrected_pair_and_keeps_identical_measurements() {
        let run = |reuse| {
            let manager = analysis_fixture(900, |_| crate::Quat64::identity());
            manager.set_stab_reconstruction_enabled(true);
            manager.set_translation_stabilization_enabled(true);
            let mut analysis = OpticalMotionAnalysis::from_manager(&manager, Arc::new(AtomicBool::new(false))).unwrap();
            analysis.reuse_derived = reuse;
            for frame in (1..900).filter(|f| !(95..105).contains(f)) {
                let a: HashMap<_,_> = parallax_observations(frame-1).into_iter().collect();
                let obs = parallax_observations(frame).into_iter().map(|(id,b)| Observation { id, a:a[&id], b, texture:f32::NAN }).collect();
                analysis.push_test_pair(frame-1,frame,obs);
                assert!(analysis.derived_cache.len() <= analysis.pairs.len());
                assert!(analysis.derived_cache.len() <= 2*analysis.hp_max+CHUNK);
            }
            analysis.flush();
            let evaluations = analysis.derived_pair_evaluations;
            if reuse { assert_eq!(evaluations, analysis.next_seq); }
            (analysis.finish().unwrap(), evaluations)
        };
        let (reference, repeated) = run(false);
        let (cached, reused) = run(true);
        assert_eq!(measurement_bits(&reference), measurement_bits(&cached));
        assert!(repeated > reused, "repeated={repeated} reused={reused}");

        let response = crate::gyro_source::OpticalStabReconstruction::from_samples(
            vec![(-20_000,[0.0;3]),(30_000_000,[0.003,-0.002,0.01])]);
        let mut replay = reference.translation_replay.unwrap();
        replay.params.apply_optical_stab = true;
        replay.params.gyro.write().optical_stab = Some(response.clone());
        let mut state = TranslationState::new();
        while !replay.pairs.is_empty() {
            let pairs: VecDeque<_> = replay.pairs.drain(..replay.pairs.len().min(CHUNK)).collect();
            let mut derived = OpticalMotionAnalysis::derive_pairs(&replay.params, &pairs, &None, replay.track_size, replay.horizontal_readout);
            OpticalMotionAnalysis::measure_translation_pairs(&replay.params, &mut state, &pairs,
                pairs.back().unwrap().seq+1, replay.focal_px, &mut derived, true, &||false);
            state.pairs.clear();
        }
        let samples = cached.translation_replay.unwrap().measure(&response,&||false).unwrap();
        assert_eq!(state.finish(), samples, "unused parallax removal must not change translation samples");
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn reconstruction_bearing_cache_follows_tracking_geometry() {
        let manager = analysis_fixture(8, |_| crate::Quat64::identity());
        manager.set_stab_reconstruction_enabled(true);
        let mut analysis = OpticalMotionAnalysis::from_manager(&manager, Arc::new(AtomicBool::new(false))).unwrap();
        let a: HashMap<_,_> = parallax_observations(0).into_iter().collect();
        let obs = parallax_observations(1).into_iter().map(|(id,b)| Observation { id, a:a[&id], b, texture:f32::NAN }).collect();
        analysis.push_test_pair(0,1,obs);
        let old = analysis.derive(Some(0));
        assert!(!analysis.derived_cache.is_empty());
        analysis.push_tracked_frame(2, 2000.0/30.0, (480,270), vec![]);
        assert!(analysis.derived_cache.is_empty());
        let fresh = OpticalMotionAnalysis::derive_pairs(&analysis.params,&analysis.pairs,&None,analysis.track_size,analysis.horizontal_readout);
        let actual = analysis.derive(None);
        assert!(old.iter().zip(&actual).any(|(a,b)| a.p != b.p));
        assert_eq!(fresh.len(), actual.len());
        for (a,b) in fresh.iter().zip(&actual) { assert_eq!((a.p,a.r),(b.p,b.r)); }
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn combined_translation_removes_sensor_motion_before_estimating_parallax() {
        use crate::gyro_source::{OpticalStabReconstruction, CameraStabData, splines::CatmullRom};
        use nalgebra::{Vector2, Rotation2};
        for horizontal in [false, true] {
            let frames = 90;
            let make = |recorded: bool| {
                let stab = analysis_fixture(frames, |_| crate::Quat64::identity());
                { let mut p = stab.params.write(); p.frame_readout_time = 10.0;
                    if horizontal { p.frame_readout_direction = crate::stabilization_params::ReadoutDirection::LeftToRight; } }
                { let mut ui = stab.optical_ui.write(); ui.stab_enabled = true; ui.translation_enabled = true; }
                if recorded {
                    let mut spline = CatmullRom::new();
                    for row in [0.0, 1080.0] { spline.add_point(row, Vector3::new(3.0, -2.0, 0.02_f64.to_degrees() * 1000.0)); }
                    stab.gyro.write().file_metadata.write().camera_stab_data = (0..frames).map(|_| CameraStabData {
                        sensor_size: (960, 540), crop_area: (0.0, 0.0, 960.0, 540.0), pixel_pitch: (1, 1),
                        ibis_spline: spline.clone(), ..Default::default() }).collect();
                }
                stab
            };
            let truth = OpticalStabReconstruction::from_samples((-20..=3020).map(|ms| {
                let t = ms as f64 / 1000.0;
                (ms * 1000, [0.006 * (19.0*t).sin(), 0.004*(23.0*t).cos(), 0.008*(11.0*t).sin()])
            }).collect());
            let zero = OpticalStabReconstruction::from_samples(vec![(-20000, [0.0;3]), (3020000, [0.0;3])]);
            let base = make(false);
            let shifted = make(true);
            let baseline = analyze_pairs(&base, frames, parallax_observations);
            let moved = analyze_pairs(&shifted, frames, |frame| parallax_observations(frame).into_iter().map(|(id, xy)| {
                let body = Vector2::new(xy[0] as f64, xy[1] as f64);
                let mut raw = body;
                for _ in 0..12 {
                    let row = if horizontal { raw.x / 960.0 } else { raw.y / 540.0 };
                    let t = frame as f64 * 1e6 / 30.0 + 10000.0 * (row - 0.5);
                    let s = truth.at(t);
                    raw = Rotation2::new(-0.02-s.z) * (body - Vector2::new(483.0,268.0) - s.xy()*500.0) + Vector2::new(480.0,270.0);
                }
                (id, [raw.x as f32, raw.y as f32])
            }).collect());
            assert!(moved.translation_samples.is_empty());
            let a = baseline.translation_replay.unwrap().measure(&zero, &||false).unwrap();
            let replay = moved.translation_replay.unwrap();
            let mut params = replay.params.clone();
            params.apply_optical_stab = true;
            params.gyro.write().optical_stab = Some(truth.clone());
            let corrected = OpticalMotionAnalysis::derive_pairs(&params, &replay.pairs, &None, (960,540), horizontal);
            let mut max_error = 0.0_f64;
            for d in &corrected {
                let xy = parallax_observations(d.seq + 1).into_iter().find(|(id,_)| *id == d.id).unwrap().1;
                max_error = max_error.max(((d.p+d.r)-to_quat_frame(((xy[0]-480.0)/500.0,(xy[1]-270.0)/500.0))).norm()*500.0);
            }
            assert!(max_error < 0.01, "horizontal={horizontal} rays={max_error}");
            let b = replay.measure(&truth, &||false).unwrap();
            assert_eq!(a.len(), b.len());
            let mut max_motion = 0.0_f64;
            for (a,b) in a.iter().zip(&b).skip(10) {
                for k in 0..2 { max_motion = max_motion.max((a.layer_motion[k]*a.auto_beta*a.weight-b.layer_motion[k]*b.auto_beta*b.weight).abs() as f64*500.0); }
            }
            eprintln!("combined horizontal={horizontal} ray_px={max_error} motion_px={max_motion}");
            assert!(max_motion < 0.02);
        }
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
                Observation { id, a, b: [a[0] - 1.0, a[1] + 0.5], texture: f32::NAN }
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
            let obs = current.iter().filter_map(|(id, b)| by_id.get(id).map(|a| Observation { id: *id, a: *a, b: *b, texture: f32::NAN })).collect();
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
        analysis.sequencer.ranges_ms = vec![(0.0, 4_800.0), (4_900.0, 15_000.0)];
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
            let obs = current.iter().map(|(id, b)| Observation { id: *id, a: previous[id], b: *b, texture: f32::NAN }).collect();
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
            let obs = (0..400u32).map(|id| Observation { id, a: previous[&id], b: current[&id], texture: f32::NAN }).collect();
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

    /// Frames of a smooth random texture that moves, turns and zooms a little, 8-bit luma
    #[cfg(feature = "use-opencv")]
    fn moving_texture(frames: usize, (w, h): (i32, i32)) -> Vec<Vec<u8>> {
        use opencv::{ core::{ Mat, Point2f, Size }, imgproc, prelude::* };
        let mut seed = 99u32;
        let noise: Vec<u8> = (0..1400 * 900).map(|_| { seed ^= seed << 13; seed ^= seed >> 17; seed ^= seed << 5; seed as u8 }).collect();
        let noise = Mat::new_rows_cols_with_data::<u8>(900, 1400, &noise).unwrap().try_clone().unwrap();
        let mut canvas = Mat::default();
        imgproc::gaussian_blur_def(&noise, &mut canvas, Size::new(0, 0), 1.5).unwrap();
        (0..frames).map(|k| {
            let t = k as f64;
            let mut m = imgproc::get_rotation_matrix_2d(Point2f::new(700.0, 450.0), (t * 0.21).sin() * 2.0, 1.0 + 0.01 * (t * 0.13).sin()).unwrap();
            *m.at_2d_mut::<f64>(0, 2).unwrap() += -300.0 + 30.0 * (t * 0.17).sin() + 0.5 * t;
            *m.at_2d_mut::<f64>(1, 2).unwrap() += -250.0 + 6.0 * (t * 0.23).cos();
            let mut frame = Mat::default();
            imgproc::warp_affine_def(&canvas, &mut frame, &m, Size::new(w, h)).unwrap();
            frame.data_bytes().unwrap().to_vec()
        }).collect()
    }

    /// A clip without motion data: the analysis measures the motion itself
    #[cfg(feature = "use-opencv")]
    fn vision_fixture(frames: usize) -> StabilizationManager {
        let stab = analysis_fixture(frames, |_| crate::Quat64::identity());
        stab.gyro.write().file_metadata.write().quaternions.clear();
        stab.gyro.write().quaternions.clear();
        assert!(!stab.gyro.read().file_metadata.read().has_motion());
        stab
    }

    /// Everything the measurements hold, bit for bit and in order
    #[cfg(feature = "use-opencv")]
    fn measurement_bits(m: &OpticalMeasurements) -> String {
        let f = |x: f64| x.to_bits();
        let mut s = format!("{:?}|{:?}|{}|{}|{}|{}|{}|{}|{}|{}\n", m.bands, m.translation_samples, m.stab_requested, m.translation_requested,
            f(m.scaled_fps), m.quats_checksum, m.context_checksum, m.frames, m.measured_frames, m.generation);
        for (t, q) in &m.video_base { s += &format!("{t}:{:?}", q.map(f32::to_bits)); }
        for p in &m.stab_pairs {
            s += &format!("\npair {} {} {}", p.seq, f(p.duration_us), p.points.len());
            for x in &p.points {
                s += &format!(" {:?}{:?}{:?}{:?}{}{}{:?}{}", x.a.sensor_full.map(f32::to_bits), x.a.known_translation_full.map(f32::to_bits),
                    x.b.sensor_full.map(f32::to_bits), x.b.known_translation_full.map(f32::to_bits), f(x.ta_us), f(x.tb_us),
                    x.gyro_ab.coords.map(f), x.band);
            }
        }
        for b in &m.stab_bands {
            s += &format!("\nband {} {} {} {} {:?} {:?} {}", b.pair, b.band, f(b.ta_us), f(b.tb_us), b.correction.map(f), b.info.map(f), f(b.cauchy_scale_px));
        }
        s
    }

    /// The frames fed one by one, and through the pipeline; with the error of each, if any
    #[cfg(feature = "use-opencv")]
    fn analyze_both_ways(stab: &StabilizationManager, frames: &[(i64, u32, u32, usize, Vec<u8>)]) -> (Result<String, String>, Result<String, String>) {
        let mut one_by_one = OpticalMotionAnalysis::from_manager(stab, Arc::new(AtomicBool::new(false))).unwrap();
        let sequential = frames.iter().try_for_each(|(ts, w, h, stride, pixels)| one_by_one.feed_frame(*ts, *w, *h, *stride, pixels))
            .and_then(|()| one_by_one.finish()).map(|m| measurement_bits(&m));
        let analysis = OpticalMotionAnalysis::from_manager(stab, Arc::new(AtomicBool::new(false))).unwrap();
        let (running, mut sink) = pipeline::AnalysisPipeline::start(analysis).unwrap();
        for (ts, w, h, stride, pixels) in frames {
            if sink.push(*ts, *w, *h, *stride, pixels).is_err() { break; }
        }
        drop(sink);
        let outcome = running.join().unwrap();
        let piped = match outcome.error.clone() {
            Some(e) => Err(e),
            None => outcome.finish().map(|m| measurement_bits(&m)),
        };
        (sequential, piped)
    }

    #[cfg(feature = "use-opencv")]
    fn frames_at_30fps(pixels: Vec<Vec<u8>>, (w, h): (u32, u32)) -> Vec<(i64, u32, u32, usize, Vec<u8>)> {
        pixels.into_iter().enumerate().map(|(k, p)| ((k as f64 * 1e6 / 30.0).round() as i64, w, h, w as usize, p)).collect()
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn gpu_layer_failure_recomputes_the_same_batch_on_cpu() {
        for cancelled in [false,true] {
            let mut state=TranslationState::new();
            state.gpu_layer=Some(super::super::optical_motion::gpu_fitting::GpuLayers::failure_fixture());
            let expected=image_space::fit_prepared(&image_space::fixture_input());
            for i in 0..8 {state.samples.push(TranslationSample::default());state.dominant.push([0.0;2]);state.median_depths.push(0.0);state.pending_layers.push((i,image_space::fixture_input()));}
            state.flush_layers(&||cancelled);
            if cancelled {
                assert_eq!(state.pending_layers.len(),8);assert!(state.samples.iter().all(|s|s.weight==0.0));
            }else{
                assert!(state.gpu_layer.is_none());assert!(state.pending_layers.is_empty());
                for (i,sample) in state.samples.iter().enumerate(){assert_eq!(sample.layer_motion,expected.motion);assert_eq!(sample.layer_scale_rate,expected.scale_rate);assert_eq!(sample.weight,expected.weight);assert_eq!(state.dominant[i],expected.dominant);}
            }
        }
    }

    #[cfg(feature="use-opencv")]
    #[test]
    fn gpu_layer_cancel_discards_a_completed_batch() {
        let Some(gpu)=super::super::optical_motion::gpu_fitting::GpuLayers::for_test()else{return;};
        let mut state=TranslationState::new();state.gpu_layer=Some(gpu);
        for i in 0..8 {state.samples.push(TranslationSample::default());state.dominant.push([0.0;2]);state.median_depths.push(0.0);state.pending_layers.push((i,image_space::fixture_input()));}
        let checks=std::cell::Cell::new(0);
        state.flush_layers(&||{checks.set(checks.get()+1);checks.get()>1});
        assert_eq!(state.gpu_layer.as_ref().unwrap().pairs,8);
        assert!(state.samples.iter().all(|s|s.weight==0.0 && s.layer_motion==[0.0;2]));
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn pipeline_measures_like_feeding_the_frames_one_by_one() {
        // Long enough for a measurement while frames are still coming (`CHUNK` pairs past the high-pass window)
        let rotating = |t: f64| crate::Quat64::from_euler_angles(0.02 * (t * 3.0).sin(), 0.01 * t, 0.015 * (t * 2.0).cos());
        let stab = analysis_fixture(320, rotating);
        stab.optical_ui.write().translation_enabled = true;
        let frames = frames_at_30fps(moving_texture(320, (640, 360)), (640, 360));
        let (sequential, piped) = analyze_both_ways(&stab, &frames);
        let sequential = sequential.unwrap();
        assert!(sequential.len() > 10_000, "the analysis measured too little to compare");
        assert_eq!(sequential, piped.unwrap());
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn pipeline_matches_in_every_mode_and_on_gaps() {
        let rotating = |t: f64| crate::Quat64::from_euler_angles(0.02 * (t * 3.0).sin(), 0.01 * t, 0.0);
        let texture = moving_texture(48, (640, 360));
        // Correction and translation; a frame with too few pixels; a frame decoded twice
        let stab = analysis_fixture(48, rotating);
        stab.optical_ui.write().translation_enabled = true;
        let mut frames = frames_at_30fps(texture.clone(), (640, 360));
        frames[17].4.truncate(640 * 100);
        let repeated = frames[30].clone();
        frames.insert(31, repeated);
        let (a, b) = analyze_both_ways(&stab, &frames);
        assert_eq!(a.unwrap(), b.unwrap(), "correction and translation");
        // A gap in the trim ranges: tracks start again after it
        stab.params.write().trim_ranges = vec![(0.0, 0.4), (0.55, 1.0)];
        let (a, b) = analyze_both_ways(&stab, &frames_at_30fps(texture.clone(), (640, 360)));
        assert_eq!(a.unwrap(), b.unwrap(), "trim gap");
        stab.params.write().trim_ranges.clear();
        // The sensor reconstruction (every frame, raw observations)
        stab.optical_ui.write().translation_enabled = false;
        stab.optical_ui.write().stab_enabled = true;
        let (a, b) = analyze_both_ways(&stab, &frames_at_30fps(texture.clone(), (640, 360)));
        let a = a.unwrap();
        assert!(a.contains("\npair "), "no raw sensor observations to compare");
        assert_eq!(a, b.unwrap(), "sensor reconstruction");
        // Without motion data: the motion measured from the video
        let stab = vision_fixture(48);
        let (a, b) = analyze_both_ways(&stab, &frames_at_30fps(texture, (640, 360)));
        let a = a.unwrap();
        assert!(a.contains(':'), "no motion from the video to compare");
        assert_eq!(a, b.unwrap(), "motion from the video");
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn global_shutter_analysis_repeats_exactly() {
        // The fixture reads out instantly: all bands of a pair are measured at the same time
        let rotating = |t: f64| crate::Quat64::from_euler_angles(0.02 * (t * 3.0).sin(), 0.01 * t, 0.0);
        let stab = analysis_fixture(48, rotating);
        stab.optical_ui.write().translation_enabled = true;
        let frames = frames_at_30fps(moving_texture(48, (640, 360)), (640, 360));
        let analyze = || {
            let mut analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
            for (ts, w, h, stride, pixels) in &frames { analysis.feed_frame(*ts, *w, *h, *stride, pixels).unwrap(); }
            analysis.finish().unwrap()
        };
        let first = analyze();
        let same_time = first.bands.windows(2).filter(|b| b[0].pair == b[1].pair && b[0].ta_us == b[1].ta_us).count();
        assert!(same_time > 0, "no two bands of a pair share their time: not a global shutter");
        for _ in 0..3 { assert_eq!(measurement_bits(&first), measurement_bits(&analyze())); }
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn pipeline_reports_a_tracking_error_like_feed_frame() {
        let stab = analysis_fixture(30, |_| crate::Quat64::identity());
        let mut frames = frames_at_30fps(moving_texture(30, (640, 360)), (640, 360));
        // Wider than the tracking width: tracked at another size than it came in
        frames[12] = (frames[12].0, 1920, 1080, 1920, vec![128; 1920 * 1080]);
        let (a, b) = analyze_both_ways(&stab, &frames);
        assert_eq!(a.as_ref().err().map(String::as_str), Some("Tracking size (960, 540) differs from input (1920, 1080)"));
        assert_eq!(a, b);
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn pipeline_cancels_within_a_second() {
        let stab = analysis_fixture(300, |_| crate::Quat64::identity());
        let cancel = Arc::new(AtomicBool::new(false));
        let analysis = OpticalMotionAnalysis::from_manager(&stab, cancel.clone()).unwrap();
        let (running, mut sink) = pipeline::AnalysisPipeline::start(analysis).unwrap();
        let frames = frames_at_30fps(moving_texture(300, (640, 360)), (640, 360));
        for (ts, w, h, stride, pixels) in &frames[..40] { sink.push(*ts, *w, *h, *stride, pixels).unwrap(); }
        cancel.store(true, SeqCst);
        // Lowered again by something else: the analysis stays cancelled
        let cancelled_at = std::time::Instant::now();
        assert!(sink.stopped());
        cancel.store(false, SeqCst);
        assert!(sink.push(frames[40].0, 640, 360, 640, &frames[40].4).is_err());
        let outcome = running.join().unwrap();
        assert!(cancelled_at.elapsed() < std::time::Duration::from_secs(1), "took {:?}", cancelled_at.elapsed());
        drop(sink);
        assert!(outcome.is_cancelled());
        assert_eq!(outcome.finish().err().as_deref(), Some("Cancelled"));
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn pipeline_queues_are_bounded() {
        let stab = analysis_fixture(200, |_| crate::Quat64::identity());
        let analysis = OpticalMotionAnalysis::from_manager(&stab, Arc::new(AtomicBool::new(false))).unwrap();
        let (running, mut sink) = pipeline::AnalysisPipeline::start(analysis).unwrap();
        running.hold_measuring(true);
        let frames = frames_at_30fps(moving_texture(200, (640, 360)), (640, 360));
        let pushed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let feeder = {
            let pushed = pushed.clone();
            std::thread::spawn(move || {
                for (ts, w, h, stride, pixels) in &frames {
                    sink.push(*ts, *w, *h, *stride, pixels).unwrap();
                    pushed.fetch_add(1, SeqCst);
                }
            })
        };
        // Until the queues are full: nothing more handed over for a second
        let (mut last, mut still, started) = (0, std::time::Instant::now(), std::time::Instant::now());
        while still.elapsed().as_millis() < 1000 && started.elapsed().as_secs() < 60 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let now = pushed.load(SeqCst);
            if now != last { (last, still) = (now, std::time::Instant::now()); }
        }
        // The measuring queue, one frame in the hands of the measuring and the tracking stage, and per worker its two
        // queues and the frame it holds
        let bound = pipeline::MEASURE_QUEUE + 2 + pipeline::PREPARE_WORKERS * (2 * pipeline::PREPARE_QUEUE + 1);
        assert_eq!(pushed.load(SeqCst), bound, "frames handed over while measuring was held");
        running.hold_measuring(false);
        feeder.join().unwrap();
        let outcome = running.join().unwrap();
        assert!(outcome.error.is_none());
        assert_eq!(outcome.finish().unwrap().frames, 200);
    }

    #[test]
    fn residual_subsample_balances_the_frame_and_the_halves() {
        use std::collections::HashSet;
        let size = (960, 540);
        let obs: Vec<Observation> = (0..2000u32).map(|id| {
            let (x, y) = (((id * 7919) % 960) as f32, ((id * 104_729) % 540) as f32);
            Observation { id, a: [x, y], b: [x + 1.0, y], texture: f32::NAN }
        }).collect();
        let kept = residual_subsample(&obs, size);
        assert_eq!(kept.len(), RESIDUAL_POINTS_PER_PAIR);
        for half in 0..2 {
            let mine: Vec<_> = kept.iter().filter(|o| o.id % 2 == half).collect();
            assert_eq!(mine.len(), RESIDUAL_POINTS_PER_PAIR / 2);
            let cells: HashSet<u16> = mine.iter().map(|o| residual_cell(o.a, size)).collect();
            let occupied: HashSet<u16> = obs.iter().filter(|o| o.id % 2 == half).map(|o| residual_cell(o.a, size)).collect();
            assert_eq!(cells.len(), occupied.len().min(RESIDUAL_POINTS_PER_PAIR / 2), "one per cell before a second");
        }
        assert_eq!(residual_subsample(&obs[..100], size).len(), 100, "fewer points than the quota: all of them");
    }

    #[test]
    fn residual_store_keeps_evenly_spaced_pairs_within_its_memory() {
        let frame = |i: usize| Frame { index: i, timestamp_ms: i as f64 * 40.0, start_ms: i as f64 * 40.0, per_px_ms: 0.0, mid_ms: i as f64 * 40.0 };
        let obs: Vec<Observation> = (0..300u32).map(|id| Observation { id, a: [(id % 30) as f32 * 30.0, (id / 30) as f32 * 50.0], b: [0.0; 2], texture: f32::NAN }).collect();
        let per_pair = RESIDUAL_POINTS_PER_PAIR * std::mem::size_of::<Observation>() + std::mem::size_of::<Pair>() + std::mem::size_of::<TrackedCounts>();
        let mut store = ResidualStore::with_cap(per_pair * 10);
        for seq in 0..100 { store.push(seq, frame(seq), frame(seq + 1), &obs, (960, 540)); }
        assert_eq!(store.timing.len(), 100, "every pair's timing is kept");
        assert!(store.bytes <= per_pair * 10);
        assert!(store.kept.iter().all(|(p, _)| p.seq % store.step == 0));
        assert!(store.kept.windows(2).all(|w| w[1].0.seq - w[0].0.seq == store.step), "evenly spaced");
        assert!(store.kept.len() >= 5);
    }

    /// Ten minutes at 60 fps with a thousand tracks a frame: the points kept stay within `RESIDUAL_MEMORY_BYTES`,
    /// thinned to every other pair, every pair's timing kept
    #[test]
    fn residual_store_of_a_ten_minute_clip_stays_within_its_memory() {
        let frame = |i: usize| Frame { index: i, timestamp_ms: i as f64 * 1000.0 / 60.0, start_ms: i as f64 * 1000.0 / 60.0, per_px_ms: 0.0, mid_ms: i as f64 * 1000.0 / 60.0 };
        let obs: Vec<Observation> = (0..1000u32).map(|id| Observation { id, a: [((id * 7919) % 960) as f32, ((id * 104_729) % 540) as f32], b: [0.0; 2], texture: f32::NAN }).collect();
        let mut store = ResidualStore::new();
        let pairs = 10 * 60 * 60;
        for seq in 0..pairs { store.push(seq, frame(seq), frame(seq + 1), &obs, (960, 540)); }
        let allocated: usize = store.kept.iter().map(|(p, _)| p.obs.capacity() * std::mem::size_of::<Observation>()).sum::<usize>()
            + store.kept.capacity() * std::mem::size_of::<(Pair, TrackedCounts)>();
        eprintln!("residual store: {} of {} pairs (every {}), {:.1} MB", store.kept.len(), pairs, store.step, allocated as f64 / 1048576.0);
        assert!(allocated <= RESIDUAL_MEMORY_BYTES);
        assert_eq!(store.timing.len(), pairs);
        assert_eq!(store.step, 2);
        assert_eq!(store.kept.len(), pairs / 2);
    }

    /// The picture of `analysis_fixture` rotates by the motion data and an `error_deg`, 7 Hz wobble on top that the
    /// motion data misses; every id seen in every frame
    #[cfg(feature = "use-opencv")]
    fn residual_body(t: f64) -> crate::Quat64 {
        crate::Quat64::from_euler_angles(0.3f64.to_radians() * (std::f64::consts::TAU * 1.1 * t).sin(),
            0.3f64.to_radians() * (std::f64::consts::TAU * 0.7 * t).sin(), 0.0)
    }
    #[cfg(feature = "use-opencv")]
    fn residual_error(error_deg: f64, t: f64) -> crate::Quat64 {
        crate::Quat64::from_euler_angles(error_deg.to_radians() * (std::f64::consts::TAU * 7.0 * t).sin(),
            0.5 * error_deg.to_radians() * (std::f64::consts::TAU * 5.0 * t).cos(), 0.0)
    }
    /// Tracking noise of 0.1 px per coordinate (Box-Muller over a hash of the frame and id): without it every
    /// strength scores within rounding of the others and the choice among them is arbitrary
    #[cfg(feature = "use-opencv")]
    fn residual_noise(frame: usize, id: u32) -> [f64; 2] {
        let mut x = (frame as u64) << 32 | id as u64;
        let mut next = || {
            x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = x;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        };
        let (u1, u2) = (next().max(1e-300), next());
        let r = 0.1 * (-2.0 * u1.ln()).sqrt();
        [r * (std::f64::consts::TAU * u2).cos(), r * (std::f64::consts::TAU * u2).sin()]
    }
    #[cfg(feature = "use-opencv")]
    fn residual_observations(error_deg: f64, ids: impl Fn(u32) -> bool + Copy) -> impl Fn(usize) -> Vec<(u32, [f32; 2])> {
        move |frame| {
            let t = frame as f64 / 30.0;
            let observed = residual_body(t) * residual_error(error_deg, t);
            (0..400).filter(|id| ids(*id)).map(|id| {
                let u = (id % 20) as f64 * 48.0 + 24.0;
                let v = (id / 20) as f64 * 27.0 + 13.5;
                let world = Vector3::new((u - 480.0) * 5.0 / 500.0, (270.0 - v) * 5.0 / 500.0, -5.0);
                let (p, n) = (project(observed.inverse() * world), residual_noise(frame, id));
                (id, [p[0] + n[0] as f32, p[1] + n[1] as f32])
            }).collect()
        }
    }
    /// `analysis_fixture` with the integration a file carrying quaternions loads with, so taking a correction off
    /// and integrating again gives back the motion data
    #[cfg(feature = "use-opencv")]
    fn residual_fixture() -> StabilizationManager {
        let stab = analysis_fixture(120, residual_body);
        stab.gyro.write().integration_method = 0;
        stab
    }
    #[cfg(feature = "use-opencv")]
    fn residual_scene(error_deg: f64, translation: bool) -> (StabilizationManager, OpticalMeasurements) {
        let stab = residual_fixture();
        { let mut ui = stab.optical_ui.write(); ui.correction_enabled = true; ui.translation_enabled = translation; }
        let m = analyze_pairs(&stab, 120, residual_observations(error_deg, |_| true));
        (stab, m)
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn residual_rotation_corrects_what_the_motion_data_missed() {
        let (stab, m) = residual_scene(0.05, false);
        assert!(m.residual_replay.is_some());
        let before = stab.gyro.read().quaternions.clone();
        stab.set_optical_measurements(m).unwrap();
        stab.recompute_blocking();
        let gyro = stab.gyro.read();
        let r = gyro.optical_residual.as_ref().unwrap();
        assert!(r.sigma_deg > 0.0 && r.applies, "selected {} deg", r.sigma_deg);
        assert!(gyro.optical_correction.is_none(), "with motion data the residual rotation replaces the band-fitted one");
        // Against the picture's own orientation in the middle of the clip
        let error = |quats: &TimeQuat| -> f64 {
            let mut s = Vec::new();
            for ms in (1000..3000).step_by(5) {
                let truth = residual_body(ms as f64 / 1000.0) * residual_error(0.05, ms as f64 / 1000.0);
                s.push(GyroSource::clamped_quat_at_gyro_timestamp(quats, ms as f64).angle_to(&truth).powi(2));
            }
            (s.iter().sum::<f64>() / s.len() as f64).sqrt()
        };
        let (was, is) = (error(&before), error(&gyro.quaternions));
        eprintln!("residual rotation: sigma {} deg tau {} s, picture error {:.5} -> {:.5} deg", r.sigma_deg, r.tau_s, was.to_degrees(), is.to_degrees());
        assert!(is < 0.5 * was);
    }

    /// With only tracking noise to fit, every strength above the weakest scores clearly worse than none. The weakest
    /// one can win by chance: in this scene the held-out 3-10 Hz motion is nothing but the other half's noise
    /// (~0.01 px), where any change of a few thousandths of a pixel moves the log ratio either way. Whatever it picks
    /// stays far below anything visible
    #[cfg(feature = "use-opencv")]
    #[test]
    fn residual_rotation_without_an_error_corrects_nothing_visible() {
        let (stab, m) = residual_scene(0.0, false);
        let before = stab.gyro.read().quaternions.clone();
        stab.set_optical_measurements(m).unwrap();
        stab.recompute_blocking();
        let gyro = stab.gyro.read();
        let r = gyro.optical_residual.as_ref().unwrap();
        assert!(r.sigma_deg <= residual_rotation::SIGMAS_DEG[1], "selected {} deg", r.sigma_deg);
        assert!(r.scores.iter().filter(|s| s.0 >= 0.05).all(|s| s.2 > 0.0), "strong corrections fit the noise");
        let largest = before.iter().map(|(t, q)| q.angle_to(&GyroSource::clamped_quat_at_gyro_timestamp(&gyro.quaternions, *t as f64 / 1000.0))).fold(0.0, f64::max);
        assert!(largest.to_degrees() < 0.002, "changed by up to {} deg", largest.to_degrees());
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn analysis_after_the_residual_rotation_measures_the_same() {
        let (stab, first) = residual_scene(0.05, true);
        let expected = (first.quats_checksum, first.translation_samples.clone(), bands_hash(&first.bands));
        stab.set_optical_measurements(first).unwrap();
        stab.recompute_blocking();
        assert!(stab.gyro.read().optical_residual.as_ref().unwrap().applies);
        let again = analyze_pairs(&stab, 120, residual_observations(0.05, |_| true));
        assert_eq!(again.quats_checksum, expected.0);
        assert_eq!(again.translation_samples, expected.1);
        assert_eq!(bands_hash(&again.bands), expected.2);
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn residual_rotation_stops_when_cancelled_during_the_selection() {
        let (stab, mut m) = residual_scene(0.05, false);
        let replay = m.residual_replay.take().unwrap();
        let render = ComputeParams::from_manager(&stab);
        let calls = std::cell::Cell::new(0);
        let cancel = || { calls.set(calls.get() + 1); calls.get() > 3 };
        assert_eq!(replay.measure(None, None, &render, m.quats_checksum, m.context_checksum, &cancel).unwrap_err(), "Cancelled");
        assert!(stab.gyro.read().optical_residual.is_none());
    }

    #[cfg(feature = "use-opencv")]
    #[test]
    fn residual_rotation_with_one_half_of_the_tracks_selects_none() {
        let stab = residual_fixture();
        stab.optical_ui.write().correction_enabled = true;
        let m = analyze_pairs(&stab, 120, residual_observations(0.05, |id| id % 2 == 0));
        stab.set_optical_measurements(m).unwrap();
        let gyro = stab.gyro.read();
        assert_eq!(gyro.optical_residual.as_ref().unwrap().sigma_deg, 0.0, "not enough to cross-validate: zero, no error");
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
            translation_requested: false, translation_samples: vec![], translation_replay: None, residual_replay: None, scaled_fps: fps,
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
    fn solve_stab_reports_the_input_cutoff_and_measured_pairs() {
        let (q, m) = prior_scene(1.5, 0.5, |_| true);
        assert_eq!(m.stab_bands.len(), 45 * 6);
        let result = solve_stab(&m, &q, &StabReconConfig::DEFAULT).unwrap();
        assert_eq!(result.cutoff_hz, 0.5);
        assert_eq!(result.measured_frames, 45);
        let forced = solve_stab(&m, &q, &StabReconConfig { cutoff_hz: Some(0.7), ..StabReconConfig::DEFAULT }).unwrap();
        assert_eq!(forced.cutoff_hz, 0.7);
    }

    fn shaking_body(t: f64) -> UnitQuaternion<f64> {
        let w = |amplitude: f64, hz: f64, phase: f64| amplitude * (std::f64::consts::TAU * hz * t + phase).sin();
        UnitQuaternion::from_scaled_axis(Vector3::new(w(0.003, 2.3, 0.2) + w(0.001, 6.1, 1.0), w(0.005, 3.7, 0.7) + w(0.0015, 1.4, 2.0), w(0.002, 5.1, 0.3)))
    }

    /// Frame pairs at 30 fps whose sensor follows `truth` (normalized shift and roll at the given gyro times).
    fn response_scene(seconds: f64, truth: impl Fn(&TimeQuat, &[f64]) -> Vec<Vector3<f64>>) -> (TimeQuat, OpticalMeasurements) {
        let quats: TimeQuat = (-1000..=((seconds + 1.0) * 1000.0) as i64).map(|i| (i * 1000, shaking_body(i as f64 / 1000.0))).collect();
        let times: Vec<_> = (0..=(seconds * 30.0).round() as usize).map(|i| i as f64 * 1e6 / 30.0).collect();
        let s = truth(&quats, &times);
        let pairs = times.windows(2).enumerate().map(|(i, t)| {
            sensor::tests::physical_pair(i * 2 + 7, t[0], t[1], shaking_body(t[0] / 1e6), shaking_body(t[1] / 1e6), s[i], s[i + 1])
        }).collect();
        let m = measurements(&quats, pairs, 30.0);
        (quats, m)
    }

    fn interior(seconds: f64) -> Vec<f64> { (1500..((seconds - 1.5) * 1000.0) as i64).map(|ms| ms as f64 * 1000.0).collect() }

    #[test]
    fn solve_stab_recovers_a_partial_lagged_stabilizer() {
        // The stabilizer takes out 60% of the shake, 8 ms late (not a multiple of the frame).
        let truth = |q: &TimeQuat, t: &[f64]| optical_stab::prior(q, 0.5, &t.iter().map(|t| t - 8000.0).collect::<Vec<_>>()).into_iter().map(|v| v * 0.6).collect();
        let (q, m) = response_scene(8.0, truth);
        let result = solve_stab(&m, &q, &StabReconConfig::DEFAULT).unwrap();
        let times = interior(8.0);
        let expected = truth(&q, &times);
        let amplitude = expected.iter().map(|s| s.norm()).fold(0.0, f64::max);
        let error = times.iter().zip(&expected).map(|(&t, s)| (result.at(t) - s).norm()).fold(0.0, f64::max);
        println!("partial lagged stabilizer amplitude={amplitude} max_error={error}");
        assert!(amplitude > 0.003 && error <= 0.05 * amplitude, "amplitude={amplitude} error={error}");
    }

    #[test]
    fn solve_stab_does_not_absorb_slow_motion_the_rotation_does_not_drive() {
        // Full stabilization plus a slow 2 degree sensor-plane drift no rotation explains (an arc's parallax looks like this).
        let drift = |t: f64| Vector3::new(2.0_f64.to_radians() * (std::f64::consts::TAU * 0.1 * t / 1e6).sin(), 0.0, 0.0);
        let truth = |q: &TimeQuat, t: &[f64]| optical_stab::prior(q, 0.5, t).into_iter().zip(t).map(|(v, &t)| v + drift(t)).collect();
        let (q, m) = response_scene(20.0, truth);
        let result = solve_stab(&m, &q, &StabReconConfig::DEFAULT).unwrap();
        let times = interior(20.0);
        let stabilizer = optical_stab::prior(&q, 0.5, &times);
        let amplitude = stabilizer.iter().map(|s| s.norm()).fold(0.0, f64::max);
        let error = times.iter().zip(&stabilizer).map(|(&t, s)| (result.at(t) - s).norm()).fold(0.0, f64::max);
        println!("slow drift not absorbed amplitude={amplitude} max_error={error}");
        assert!(error <= 0.05 * amplitude, "amplitude={amplitude} error={error}");
    }

    #[test]
    fn solve_stab_without_a_stabilizer_reconstructs_nothing() {
        let (q, m) = response_scene(8.0, |_, t| vec![Vector3::zeros(); t.len()]);
        let result = solve_stab(&m, &q, &StabReconConfig::DEFAULT).unwrap();
        let largest = interior(8.0).into_iter().map(|t| result.at(t).norm()).fold(0.0, f64::max);
        assert!(result.is_active() || result.has_table());
        assert!(largest <= 1e-5, "largest={largest}");
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
    fn solve_stab_bands_without_raw_points_cannot_fall_back() {
        let (q,mut m)=prior_scene(0.1,0.3,|_|true); m.stab_pairs.clear();
        assert_eq!(solve_stab(&m,&q,&StabReconConfig::DEFAULT).unwrap_err(),"Not enough of the image could be tracked");
    }

    #[test]
    fn solve_stab_rejects_changed_quaternions_and_cancellation() {
        let (mut q,m)=prior_scene(0.1,0.3,|_|true);
        let count=std::cell::Cell::new(0);
        assert_eq!(solve_stab_with_cancel(&m,&q,&StabReconConfig::DEFAULT,|| { count.set(count.get()+1);count.get()>2 }).unwrap_err(),"Cancelled");
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
