// SPDX-License-Identifier: GPL-3.0-or-later
//! Fits the in-camera stabilization as a linear response to the measured body rotation.
//!
//! Between two frames every readout band measures how far the camera's stabilizer moved the picture
//! (`fit_band_shift`). The stabilizer is driven by the body rotation, so its correction is modelled as a short linear
//! filter of the high-passed rotation p (the prior), fitted on the shake band only: a gain, a sub-frame delay and a
//! curvature (C0 p + C1 T p' + C2 T^2 p'' / 2), plus taps whole frames away for slower shaping. What the rotation
//! doesn't drive - the parallax of an arc or a walk, a zoom, a moving subject - has no component in it however large it
//! is, so it can't be absorbed, and the fit is one linear solve: nothing to converge.

use super::sensor::{self, SensorBand, SensorPair};
use crate::gyro_source::optical_stab::{PriorError, PriorSampler};
use nalgebra::{DMatrix, DVector, Matrix2, Matrix3, Vector2, Vector3};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq)]
pub(super) enum SensorSolveError {
    NoMeasurements,
    SingularSystem,
    Cancelled,
}
type Result<T> = std::result::Result<T, SensorSolveError>;

impl From<PriorError> for SensorSolveError {
    fn from(error: PriorError) -> Self {
        if error == PriorError::Cancelled {
            Self::Cancelled
        } else {
            log::warn!("Sensor prior integration failed: {error:?}");
            Self::SingularSystem
        }
    }
}

/// Time scale of the delay term, and the step of the central differences that give p' and p''.
const DELAY_US: f64 = 20_000.0;
const STEP_US: f64 = 5_000.0;
/// Whole frames of lag on either side for the slower shaping of the response.
const LAGS: usize = 6;
/// Basis blocks (gain, delay, curvature, then the frame lags), each with a 3x3 matrix of unknowns.
const BLOCKS: usize = 3 + 2 * LAGS;
const COLUMNS: usize = 9 * BLOCKS;
/// The fit only sees the shake band: both sides are high-passed with the prior's 1 Hz Gaussian (sigma 0.1325 s) within
/// each readout band, so neither the slow parallax of an arc or a walk nor a zoom can steer it.
const FIT_SIGMA_US: f64 = 0.1325e6;
/// Priors, in units of the measurement noise: a stabilizer's gain is about 1 or less and its delay about `DELAY_US`, so
/// the gain, delay and curvature terms get sigma 1. The frame taps share a mean squared gain of about 1 (by Parseval
/// sigma 1 / sqrt(2 LAGS + 1) each): without that bound a camera that hardly turns lets them explain anything with gyro
/// noise, and since spreading over frames costs more than the gain term, narrowband motion doesn't smear the gain.
const TAYLOR_SIGMA: f64 = 1.0;
const FRAME_SIGMA: f64 = 0.2773500981126146; // 1 / sqrt(13)
const IRLS_ITERATIONS: usize = 10;

pub(super) struct ResponseFit {
    /// (lag in us, row-major [output][input] matrix) pairs: s(t) = sum H p(t - lag).
    pub taps: Vec<(f64, [f64; 9])>,
    pub start_us: f64,
    pub end_us: f64,
    pub measured_pairs: usize,
    pub bands: usize,
    /// |H| on the diagonal at 5 Hz: how much of the body shake the stabilizer took out, per axis.
    pub gain_5hz: [f64; 3],
    pub scale_px: f64,
}

struct Band {
    pair: usize,
    phase: u8,
    middle: f64,
    measured: Vector3<f64>,
    /// The band's measurement as a linear function of the unknowns, one row of `COLUMNS` per component.
    rows: Vec<f64>,
    /// Tracking pixels per normalized unit, and the mean squared normalized radius of the band's points (roll lever).
    focal_px: f64,
    lever2: f64,
}

fn cancelled(cancel: &impl Fn() -> bool) -> Result<()> {
    if cancel() { Err(SensorSolveError::Cancelled) } else { Ok(()) }
}

/// The prior on a 1 ms grid, linearly interpolated.
struct Grid {
    start: f64,
    values: Vec<Vector3<f64>>,
}
impl Grid {
    fn at(&self, t: f64) -> Vector3<f64> {
        let x = ((t - self.start) / 1000.0).clamp(0.0, (self.values.len() - 1) as f64);
        let i = (x.floor() as usize).min(self.values.len() - 2);
        let f = x - i as f64;
        self.values[i] * (1.0 - f) + self.values[i + 1] * f
    }
}

/// Lags of the basis evaluations: p at t + STEP, t, t - STEP, then at t - l frame for l != 0.
fn shifts(frame_us: f64) -> Vec<f64> {
    [-STEP_US, 0.0, STEP_US].into_iter().chain((1..=LAGS).flat_map(|l| [-(l as f64), l as f64]).map(|l| l * frame_us)).collect()
}

/// Basis values at `t`: p, T p', T^2 p'' / 2, then the frame-lagged p.
fn basis(grid: &Grid, shifts: &[f64], t: f64) -> [Vector3<f64>; BLOCKS] {
    let at = |k: usize| grid.at(t - shifts[k]);
    let (ahead, now, behind) = (at(0), at(1), at(2));
    std::array::from_fn(|j| match j {
        0 => now,
        1 => (ahead - behind) * (DELAY_US / (2.0 * STEP_US)),
        2 => (ahead - now * 2.0 + behind) * (DELAY_US * DELAY_US / (2.0 * STEP_US * STEP_US)),
        _ => at(j),
    })
}

/// What each band measures as a function of the response: `fit_band_shift` finds the correction c at the later endpoint
/// that zeroes its Cauchy-weighted residuals, so to first order c = N^-1 sum w Jb^T (Jb s(tb) + Ja s(ta)) over its points,
/// N = sum w Jb^T Jb, every point at its own row times - the correction changes during the band's readout, and the gyro's
/// mapping of the earlier frame isn't the identity at the precision a long lens needs.
fn collect(pairs: &[SensorPair], bands: &[SensorBand], grid: &Grid, shifts: &[f64], cancel: &impl Fn() -> bool) -> Result<Vec<Band>> {
    use rayon::prelude::*;
    let lookup: BTreeMap<_, _> = pairs.iter().map(|p| (p.seq, p)).collect();
    let mut out = Vec::with_capacity(bands.len());
    let collect_band = |b: &SensorBand| -> Option<Band> {
        let pair = lookup.get(&b.pair)?;
        if !b.ta_us.is_finite() || !b.tb_us.is_finite() || !b.correction.iter().all(|v| v.is_finite()) || !(b.cauchy_scale_px > 0.0) { return None; }
        let frame = &pair.frame_b;
        let focal = frame.focal_full.x;
        let scale = Matrix2::from_diagonal(&frame.full_to_track);
        let mut normal = Matrix3::zeros();
        let mut sum = vec![0.0; 3 * COLUMNS];
        let (mut lever, mut mass) = (0.0, 0.0);
        for p in pair.points.iter().filter(|p| p.band == b.band) {
            let Some(predicted) = sensor::predicted_sensor(pair, p, pair.frame_a.add_correction(&p.a, Vector3::zeros())) else { continue };
            let Some((_, ja, jb)) = sensor::residual_and_jacobians(pair, p, Vector3::zeros(), Vector3::zeros()) else { continue };
            // The weight `fit_band_shift` ended with for this point.
            let e = (scale * (frame.add_correction(&p.b, b.correction) - predicted)).norm();
            let w = 1.0 / (1.0 + (e / b.cauchy_scale_px).powi(2));
            let (bb, ba) = (jb.transpose() * jb * w, jb.transpose() * ja * w);
            normal += bb;
            let (later, earlier) = (basis(grid, shifts, p.tb_us), basis(grid, shifts, p.ta_us));
            // Unknown (block j, output o, input r) sits at column 9 j + 3 o + r.
            for c in 0..3 {
                let row = &mut sum[c * COLUMNS..(c + 1) * COLUMNS];
                for j in 0..BLOCKS {
                    for o in 0..3 {
                        let (gb, ga) = (bb[(c, o)], ba[(c, o)]);
                        for r in 0..3 { row[9 * j + 3 * o + r] += gb * later[j][r] + ga * earlier[j][r]; }
                    }
                }
            }
            let pivot = frame.principal_full + Vector2::new(p.b.known_translation_full[0] as f64, p.b.known_translation_full[1] as f64);
            lever += w * ((Vector2::new(p.b.sensor_full[0] as f64, p.b.sensor_full[1] as f64) - pivot) / focal).norm_squared();
            mass += w;
        }
        let focal_px = focal * frame.full_to_track.x;
        let inverse = normal.try_inverse()?;
        if mass <= 0.0 || !focal_px.is_finite() || focal_px <= 0.0 || !lever.is_finite() { return None; }
        let mut rows = vec![0.0; 3 * COLUMNS];
        for c in 0..3 {
            for q in 0..COLUMNS {
                rows[c * COLUMNS + q] = (0..3).map(|m| inverse[(c, m)] * sum[m * COLUMNS + q]).sum();
            }
        }
        if !rows.iter().all(|v| v.is_finite()) { return None; }
        Some(Band { pair: b.pair, phase: b.band, middle: 0.5 * (b.ta_us + b.tb_us), measured: b.correction, rows, focal_px, lever2: lever / mass })
    };
    // Bands are independent; keep point accumulation and band ordering unchanged, with bounded cancellation latency.
    for batch in bands.chunks(128) {
        cancelled(cancel)?;
        let values: Vec<_> = batch.par_iter().map(collect_band).collect();
        out.extend(values.into_iter().flatten());
    }
    cancelled(cancel)?;
    Ok(out)
}

/// Subtracts the Gaussian-weighted local mean over time (`width` values per band), separately for each readout band;
/// gaps only lower the weight of the missing side.
fn highpass(bands: &[Band], rows: &mut [f64], width: usize, cancel: &impl Fn() -> bool) -> Result<()> {
    let mut by_phase: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
    for (i, b) in bands.iter().enumerate() { by_phase.entry(b.phase).or_default().push(i); }
    let original = rows.to_vec();
    let mut mean = vec![0.0; width];
    for indices in by_phase.values_mut() {
        cancelled(cancel)?;
        indices.sort_by(|a, b| bands[*a].middle.total_cmp(&bands[*b].middle));
        let mut lo = 0;
        for k in 0..indices.len() {
            let t = bands[indices[k]].middle;
            while bands[indices[lo]].middle < t - 3.0 * FIT_SIGMA_US { lo += 1; }
            let mut mass = 0.0;
            mean.fill(0.0);
            for &j in indices[lo..].iter().take_while(|&&j| bands[j].middle <= t + 3.0 * FIT_SIGMA_US) {
                let w = (-0.5 * ((bands[j].middle - t) / FIT_SIGMA_US).powi(2)).exp();
                mass += w;
                for (m, v) in mean.iter_mut().zip(&original[j * width..(j + 1) * width]) { *m += w * v; }
            }
            let i = indices[k];
            for (r, m) in rows[i * width..(i + 1) * width].iter_mut().zip(&mean) { *r -= m / mass; }
        }
    }
    Ok(())
}

fn robust_spread(values: impl Iterator<Item = f64>) -> f64 {
    let mut values: Vec<f64> = values.map(f64::abs).collect();
    values.sort_by(f64::total_cmp);
    1.4826 * values.get(values.len() / 2).copied().unwrap_or(0.0)
}

pub(super) fn solve_response(pairs: &[SensorPair], bands: &[SensorBand], sampler: &mut PriorSampler<'_>, frame_us: f64, processing_device:i32, cancel: &impl Fn() -> bool) -> Result<ResponseFit> {
    let started = std::time::Instant::now();
    cancelled(cancel)?;
    if !frame_us.is_finite() || frame_us <= 0.0 { return Err(SensorSolveError::NoMeasurements); }
    let start_us = pairs.iter().flat_map(|p| p.points.iter().map(|p| p.ta_us)).fold(f64::INFINITY, f64::min).floor();
    let end_us = pairs.iter().flat_map(|p| p.points.iter().map(|p| p.tb_us)).fold(f64::NEG_INFINITY, f64::max).ceil();
    if !start_us.is_finite() || !end_us.is_finite() || end_us < start_us { return Err(SensorSolveError::NoMeasurements); }
    let shifts = shifts(frame_us);
    let reach = shifts.iter().fold(0.0_f64, |m, s| m.max(s.abs())) + 2000.0;
    let grid_start = start_us - reach;
    let count = ((end_us + reach - grid_start) / 1000.0).ceil() as usize + 1;
    let times: Vec<f64> = (0..count).map(|i| grid_start + i as f64 * 1000.0).collect();
    let grid = Grid { start: grid_start, values: sampler.sample(&times, cancel)? };
    let prior_done = started.elapsed();
    let bands = collect(pairs, bands, &grid, &shifts, cancel)?;
    let collect_done = started.elapsed();
    if bands.len() < 2 { return Err(SensorSolveError::NoMeasurements); }
    let n = bands.len();
    let width = 3 * COLUMNS;
    let mut x: Vec<f64> = bands.iter().flat_map(|b| b.rows.iter().copied()).collect();
    let mut y: Vec<f64> = bands.iter().flat_map(|b| [b.measured.x, b.measured.y, b.measured.z]).collect();
    highpass(&bands, &mut x, width, cancel)?;
    highpass(&bands, &mut y, 3, cancel)?;
    let highpass_done = started.elapsed();
    let (mut assemble_time, mut factor_time) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    let mut validation_time=std::time::Duration::ZERO;
    let sigma: Vec<f64> = (0..COLUMNS).map(|c| if c < 27 { TAYLOR_SIGMA } else { FRAME_SIGMA }).collect();
    let setup_started=std::time::Instant::now();
    #[cfg(feature="use-opencv")]
    let mut gpu=super::super::optical_motion::gpu_fitting::GpuNormal::for_device(processing_device,&x,&y,COLUMNS);
    #[cfg(not(feature="use-opencv"))] let _=processing_device;
    let setup_time=setup_started.elapsed();
    let mut gpu_iterations=0;
    // First pass: the spread of the measurements themselves stands in for the residual noise.
    let mut noise: [f64; 3] = std::array::from_fn(|c| robust_spread((0..n).map(|i| y[i * 3 + c])));
    let mut weights = vec![1.0; n];
    let mut solution = DVector::zeros(COLUMNS);
    let mut scale_px = 0.0;
    for _ in 0..IRLS_ITERATIONS {
        cancelled(cancel)?;
        let assembling = std::time::Instant::now();
        #[allow(unused_mut)] let mut accelerated=None;
        #[cfg(feature="use-opencv")]
        if let Some(device)=&mut gpu {
            let row_weights:Vec<_>=weights.iter().flat_map(|w|noise.iter().map(move |s|w/(s*s).max(f64::MIN_POSITIVE))).collect();
            match device.assemble(&row_weights) {
                Ok(value)=>accelerated=Some(value),
                Err(e)=>{log::warn!(target:"sync","Optical normal matrix: CPU fallback, {e}");gpu=None;},
            }
        }
        cancelled(cancel)?;
        let used_gpu=accelerated.is_some();
        let (mut normal,rhs)=accelerated.unwrap_or_else(||cpu_normal(&x,&y,&weights,&noise));
        if !normal.iter().chain(rhs.iter()).all(|v| v.is_finite()) { return Err(SensorSolveError::SingularSystem); }
        for (i, sigma) in sigma.iter().enumerate() { normal[(i, i)] += 1.0 / (sigma * sigma); }
        let matrix_norm=if used_gpu {normal.norm()}else{0.0};
        assemble_time += assembling.elapsed();
        let factoring = std::time::Instant::now();
        let candidate=normal.cholesky().map(|chol|chol.solve(&rhs));
        factor_time+=factoring.elapsed();
        let checking=std::time::Instant::now();
        let accurate=if let Some(candidate)=&candidate {
            !used_gpu || normal_residual(&x,&y,&weights,&noise,&sigma,candidate)/(rhs.norm()+matrix_norm*candidate.norm()).max(1e-300)<1e-10
        }else{false};
        validation_time+=checking.elapsed();
        if used_gpu && !accurate {
            log::warn!(target:"sync","Optical normal matrix: CPU fallback after numerical validation");
            #[cfg(feature="use-opencv")] {gpu=None;}
            let assembling=std::time::Instant::now();
            let (mut normal,rhs)=cpu_normal(&x,&y,&weights,&noise);
            for (i,sigma) in sigma.iter().enumerate(){normal[(i,i)]+=1.0/(sigma*sigma);}
            assemble_time+=assembling.elapsed();
            let factoring=std::time::Instant::now();
            solution=normal.cholesky().ok_or(SensorSolveError::SingularSystem)?.solve(&rhs);
            factor_time+=factoring.elapsed();
        }else{
            solution=candidate.ok_or(SensorSolveError::SingularSystem)?;
            if used_gpu {gpu_iterations+=1;}
        }
        let residual: Vec<f64> = (0..n * 3).map(|q| {
            let (i, c) = (q / 3, q % 3);
            y[q] - x[i * width + c * COLUMNS..i * width + (c + 1) * COLUMNS].iter().zip(solution.iter()).map(|(a, b)| a * b).sum::<f64>()
        }).collect();
        noise = std::array::from_fn(|c| robust_spread((0..n).map(|i| residual[i * 3 + c])));
        let errors: Vec<f64> = bands.iter().enumerate().map(|(i, b)| {
            let e = &residual[i * 3..i * 3 + 3];
            b.focal_px * (e[0] * e[0] + e[1] * e[1] + b.lever2 * e[2] * e[2]).sqrt()
        }).collect();
        scale_px = 2.5 * robust_spread(errors.iter().copied()).max(1e-6);
        for (w, e) in weights.iter_mut().zip(&errors) { *w = 1.0 / (1.0 + (e / scale_px).powi(2)); }
    }
    if !solution.iter().all(|v| v.is_finite()) { return Err(SensorSolveError::SingularSystem); }
    // Row-major [output][input] matrix of basis block j.
    let block = |j: usize| -> [f64; 9] { std::array::from_fn(|q| solution[9 * j + q]) };
    let (gain, delay, curvature) = (block(0), block(1), block(2));
    let d = DELAY_US / (2.0 * STEP_US);
    let c = DELAY_US * DELAY_US / (2.0 * STEP_US * STEP_US);
    let mut taps = vec![
        (-STEP_US, std::array::from_fn(|k| delay[k] * d + curvature[k] * c)),
        (0.0, std::array::from_fn(|k| gain[k] - 2.0 * curvature[k] * c)),
        (STEP_US, std::array::from_fn(|k| -delay[k] * d + curvature[k] * c)),
    ];
    for (j, shift) in shifts.iter().enumerate().skip(3) { taps.push((*shift, block(j))); }
    let gain_5hz = std::array::from_fn(|axis| {
        let (mut re, mut im) = (0.0, 0.0);
        for (lag, tap) in &taps {
            let phase = -std::f64::consts::TAU * 5.0 * lag / 1e6;
            re += tap[axis * 3 + axis] * phase.cos();
            im += tap[axis * 3 + axis] * phase.sin();
        }
        (re * re + im * im).sqrt()
    });
    let measured_pairs = bands.iter().map(|b| b.pair).collect::<BTreeSet<_>>().len();
    let ms = |d: std::time::Duration| d.as_secs_f64() * 1000.0;
    log::info!(target: "sync", "Optical reconstruction speed: pairs={measured_pairs} bands={n} total={:.1} ms prior={:.1} collect={:.1} highpass={:.1} assemble={:.1} factor={:.1} validate={:.1} setup={:.1} gpu_iterations={gpu_iterations}",
        ms(started.elapsed()), ms(prior_done), ms(collect_done - prior_done), ms(highpass_done - collect_done), ms(assemble_time), ms(factor_time), ms(validation_time), ms(setup_time));
    Ok(ResponseFit { taps, start_us, end_us, measured_pairs, bands: n, gain_5hz, scale_px })
}

fn cpu_normal(x:&[f64],y:&[f64],weights:&[f64],noise:&[f64;3])->(DMatrix<f64>,DVector<f64>) {
    let mut normal=DMatrix::zeros(COLUMNS,COLUMNS);let mut rhs=DVector::zeros(COLUMNS);
    for (i,w) in weights.iter().enumerate(){for c in 0..3 {
        let weight=w/(noise[c]*noise[c]).max(f64::MIN_POSITIVE);
        let row=DVector::from_column_slice(&x[(i*3+c)*COLUMNS..(i*3+c+1)*COLUMNS]);
        normal.ger(weight,&row,&row,1.0);rhs.axpy(weight*y[i*3+c],&row,1.0);
    }}
    (normal,rhs)
}

/// Checks the GPU result against the original f64 rows without rebuilding the Gram matrix.
fn normal_residual(x:&[f64],y:&[f64],weights:&[f64],noise:&[f64;3],sigma:&[f64],solution:&DVector<f64>)->f64 {
    let mut gradient:Vec<f64>=solution.iter().zip(sigma).map(|(v,s)|v/(s*s)).collect();
    for (r,row) in x.chunks_exact(COLUMNS).enumerate() {
        let residual=row.iter().zip(solution.iter()).map(|(a,b)|a*b).sum::<f64>()-y[r];
        let weight=weights[r/3]/(noise[r%3]*noise[r%3]).max(f64::MIN_POSITIVE);
        for (g,a) in gradient.iter_mut().zip(row){*g+=a*(weight*residual);}
    }
    gradient.iter().map(|v|v*v).sum::<f64>().sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band(phase: u8, middle: f64) -> Band {
        Band { pair: 0, phase, middle, measured: Vector3::zeros(), rows: vec![], focal_px: 1.0, lever2: 0.0 }
    }

    #[test]
    fn highpass_removes_each_bands_own_level_and_keeps_the_shake() {
        // Two readout bands at 30 fps with their own constant levels, a slow 0.1 Hz drift and a 3 Hz shake.
        let mut bands = Vec::new();
        let mut rows = Vec::new();
        for k in 0..180 {
            for phase in 0..2u8 {
                let t = k as f64 * 1e6 / 30.0 + phase as f64 * 2000.0;
                bands.push(band(phase, t));
                let seconds = t / 1e6;
                rows.push([5.0, -3.0][phase as usize] + 2.0 * (std::f64::consts::TAU * 0.1 * seconds).sin() + (std::f64::consts::TAU * 3.0 * seconds).sin());
            }
        }
        highpass(&bands, &mut rows, 1, &|| false).unwrap();
        // A 3 Hz shake keeps exp(-0.5 (2 pi 3 0.1325)^2) = 4.4% less, the 0.1 Hz drift is down to 0.35% of 2.
        for (b, value) in bands.iter().zip(&rows) {
            let seconds = b.middle / 1e6;
            if !(1.0..5.0).contains(&seconds) { continue; }
            let shake = (std::f64::consts::TAU * 3.0 * seconds).sin();
            assert!((value - 0.956 * shake).abs() <= 0.02, "t={seconds} value={value} shake={shake}");
        }
    }

    #[test]
    fn highpass_handles_a_gap_without_mixing_the_bands() {
        let bands: Vec<_> = (0..40).filter(|k| !(15..25).contains(k)).map(|k| band(0, k as f64 * 33_333.0)).chain([band(1, 600_000.0)]).collect();
        let mut rows: Vec<f64> = bands.iter().map(|b| if b.phase == 0 { 7.0 } else { -50.0 }).collect();
        highpass(&bands, &mut rows, 1, &|| false).unwrap();
        assert!(rows.iter().all(|v| v.abs() <= 1e-12), "{rows:?}");
    }
}
