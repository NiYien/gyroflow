// SPDX-License-Identifier: GPL-3.0-or-later
//! Fits prior-relative sensor splines using both physical endpoints of every observation.

use super::{
    sensor::{self, SensorBand, SensorPair},
    solver::{self, BandSym, Solution, SolverParams},
    SIGMA_FLOOR_PX,
};
use crate::gyro_source::{
    optical_correction::bspline_weights,
    optical_stab::{PriorError, PriorSampler, CUTOFF_GRID_HZ, DEFAULT_CUTOFF_HZ},
    TimeQuat,
};
use nalgebra::{Matrix2x3, Matrix3, Vector2, Vector3};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, PartialEq)]
pub(super) enum SensorSolveError {
    NoMeasurements,
    InvalidProjection,
    SingularSystem,
    NoDescent,
    NotConverged,
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

pub(super) struct SensorSolveResult<'a> {
    pub prior_sampler: PriorSampler<'a>,
    pub solution: Solution,
    pub cutoff_hz: f64,
    pub measured_pairs: usize,
    pub iterations: usize,
    pub initial_cost: f64,
    pub final_cost: f64,
    pub max_step_px: f64,
    pub band_bytes: usize,
}

struct Observation {
    pair: usize,
    point: usize,
    a: usize,
    b: usize,
    group: usize,
    cutoff_weight: f64,
}
struct Support {
    observations: Vec<Observation>,
    times: Vec<f64>,
    bands: Vec<usize>,
    measured_pairs: usize,
    duration_us: f64,
}

fn cancelled(cancel: &impl Fn() -> bool) -> Result<()> {
    if cancel() {
        Err(SensorSolveError::Cancelled)
    } else {
        Ok(())
    }
}

impl Support {
    fn new(pairs: &[SensorPair], bands: &[SensorBand], cancel: &impl Fn() -> bool) -> Result<Self> {
        let lookup: BTreeMap<_, _> = pairs.iter().enumerate().map(|(i, p)| (p.seq, i)).collect();
        if lookup.len() != pairs.len() {
            return Err(SensorSolveError::NoMeasurements);
        }
        let mut selected = BTreeMap::new();
        for (i, b) in bands.iter().enumerate() {
            if !lookup.contains_key(&b.pair)
                || !b.cauchy_scale_px.is_finite()
                || b.cauchy_scale_px <= 0.0
            {
                return Err(SensorSolveError::NoMeasurements);
            }
            if selected.insert((b.pair, b.band), i).is_some() {
                return Err(SensorSolveError::NoMeasurements);
            }
        }
        let mut out = Self {
            observations: vec![],
            times: vec![],
            bands: vec![],
            measured_pairs: 0,
            duration_us: 0.0,
        };
        let mut times = BTreeMap::new();
        let mut used = BTreeSet::new();
        let mut duration_roundoff = 0.0;
        for ((seq, band), index) in selected {
            cancelled(cancel)?;
            let pair_index = lookup[&seq];
            let pair = &pairs[pair_index];
            let b = &bands[index];
            let group = out.bands.len();
            let mut count = 0;
            let mut weight_sum = 0.0;
            for (point_index, point) in pair
                .points
                .iter()
                .enumerate()
                .filter(|(_, p)| p.band == band)
            {
                if !point.ta_us.is_finite() || !point.tb_us.is_finite() {
                    return Err(SensorSolveError::NoMeasurements);
                }
                let predicted = sensor::predicted_sensor(
                    pair,
                    point,
                    pair.frame_a.add_correction(&point.a, Vector3::zeros()),
                )
                .ok_or(SensorSolveError::InvalidProjection)?;
                let e = (pair.frame_b.add_correction(&point.b, b.correction) - predicted)
                    .component_mul(&pair.frame_b.full_to_track);
                let weight = 1.0 / (1.0 + e.norm_squared() / b.cauchy_scale_px.powi(2));
                if !weight.is_finite() {
                    return Err(SensorSolveError::InvalidProjection);
                }
                let mut endpoint = |t: f64| {
                    *times.entry(t.to_bits()).or_insert_with(|| {
                        let i = out.times.len();
                        out.times.push(t);
                        i
                    })
                };
                let a = endpoint(point.ta_us);
                let b = endpoint(point.tb_us);
                out.observations.push(Observation {
                    pair: pair_index,
                    point: point_index,
                    a,
                    b,
                    group,
                    cutoff_weight: weight,
                });
                count += 1;
                weight_sum += weight;
            }
            if count < super::MIN_BAND_POINTS || weight_sum < super::MIN_BAND_POINTS as f64 * 0.5 {
                return Err(SensorSolveError::NoMeasurements);
            }
            out.bands.push(index);
            if used.insert(seq) {
                if !pair.duration_us.is_finite() || pair.duration_us <= 0.0 {
                    return Err(SensorSolveError::NoMeasurements);
                }
                // Compensated summation preserves the strict two-second boundary without adding a tolerance.
                let increment = pair.duration_us - duration_roundoff;
                let total = out.duration_us + increment;
                duration_roundoff = (total - out.duration_us) - increment;
                out.duration_us = total;
            }
        }
        out.measured_pairs = used.len();
        if out.observations.is_empty() {
            return Err(SensorSolveError::NoMeasurements);
        }
        Ok(out)
    }

    fn prior_sampler<'a>(
        &self,
        pairs: &[SensorPair],
        quats: &'a TimeQuat,
        forced: Option<f64>,
        cancel: &impl Fn() -> bool,
    ) -> Result<PriorSampler<'a>> {
        if let Some(hz) = forced.filter(|hz| hz.is_finite() && *hz > 0.0 && *hz <= 50.0) {
            return Ok(PriorSampler::new(quats, hz)?);
        }
        if self.duration_us < 2e6 {
            return Ok(PriorSampler::new(quats, DEFAULT_CUTOFF_HZ)?);
        }
        let mut best: Option<(f64, PriorSampler<'a>)> = None;
        for hz in CUTOFF_GRID_HZ {
            cancelled(cancel)?;
            let mut sampler = PriorSampler::new(quats, hz)?;
            let values = sampler.sample(&self.times, cancel)?;
            let mut cost = 0.0;
            let mut previous_pair = usize::MAX;
            for obs in &self.observations {
                if previous_pair != obs.pair {
                    cancelled(cancel)?;
                    previous_pair = obs.pair;
                }
                let pair = &pairs[obs.pair];
                let Some(e) =
                    sensor::residual(pair, &pair.points[obs.point], values[obs.a], values[obs.b])
                else {
                    cost = f64::INFINITY;
                    break;
                };
                cost += obs.cutoff_weight * e.norm_squared();
            }
            log::debug!("Sensor cutoff candidate hz={hz} cost={cost} prior_nodes={} panels={} logs={} contributions={}",sampler.cached_nodes(),sampler.panels,sampler.log_evaluations,sampler.contributions);
            #[cfg(test)]
            if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {
                println!("sensor cutoff hz={hz} cost={cost} prior_nodes={} panels={} logs={} contributions={}",sampler.cached_nodes(),sampler.panels,sampler.log_evaluations,sampler.contributions);
            }
            if cost.is_finite() && best.as_ref().is_none_or(|v| cost < v.0) {
                best = Some((cost, sampler));
            }
        }
        best.map(|v| v.1).ok_or(SensorSolveError::InvalidProjection)
    }

    #[cfg(test)]
    fn cutoff(
        &self,
        pairs: &[SensorPair],
        quats: &TimeQuat,
        forced: Option<f64>,
        cancel: &impl Fn() -> bool,
    ) -> Result<f64> {
        self.prior_sampler(pairs, quats, forced, cancel)
            .map(|p| p.cutoff_hz())
    }
}

#[derive(Clone)]
struct Endpoint {
    prior: Vector3<f64>,
    knots: [usize; 4],
    weights: [f64; 4],
}
impl Endpoint {
    fn at(&self, coeffs: &[Vector3<f64>]) -> Vector3<f64> {
        self.knots
            .iter()
            .zip(self.weights)
            .fold(self.prior, |v, (&k, w)| v + coeffs[k] * w)
    }
}

fn layout(
    initial: &mut Solution,
    times: &[f64],
    values: Vec<Vector3<f64>>,
    spacing: f64,
) -> Vec<Endpoint> {
    let first = times
        .iter()
        .map(|t| bspline_weights((*t - initial.start_us) / spacing).0 - 1)
        .min()
        .unwrap();
    let pad = (-first).max(0) as usize;
    if pad > 0 {
        initial.start_us -= pad as f64 * spacing;
        initial
            .coeffs
            .splice(0..0, std::iter::repeat_n(Vector3::zeros(), pad));
    }
    let last = times
        .iter()
        .map(|t| bspline_weights((*t - initial.start_us) / spacing).0 + 2)
        .max()
        .unwrap() as usize;
    initial
        .coeffs
        .resize(initial.coeffs.len().max(last + 1), Vector3::zeros());
    times
        .iter()
        .zip(values)
        .map(|(t, prior)| {
            let (k, weights) = bspline_weights((*t - initial.start_us) / spacing);
            Endpoint {
                prior,
                knots: std::array::from_fn(|i| (k + i as i64 - 1) as usize),
                weights,
            }
        })
        .collect()
}

fn blocks(
    a: &Endpoint,
    b: &Endpoint,
    ja: Matrix2x3<f64>,
    jb: Matrix2x3<f64>,
) -> Vec<(usize, Matrix2x3<f64>)> {
    let mut out: Vec<(usize, Matrix2x3<f64>)> = Vec::with_capacity(8);
    for (endpoint, j) in [(a, ja), (b, jb)] {
        for (&k, w) in endpoint.knots.iter().zip(endpoint.weights) {
            if let Some(v) = out.iter_mut().find(|v| v.0 == k) {
                v.1 += j * w;
            } else {
                out.push((k, j * w));
            }
        }
    }
    out
}

fn accumulate(
    h: &mut BandSym,
    g: &mut [f64],
    blocks: &[(usize, Matrix2x3<f64>)],
    error: Vector2<f64>,
    weight: f64,
) {
    for (ka, ja) in blocks {
        let jt = ja.transpose() * weight;
        let grad = jt * error;
        for r in 0..3 {
            g[ka * 3 + r] += grad[r];
        }
        for (kb, jb) in blocks {
            if kb > ka {
                continue;
            }
            let block = jt * jb;
            for r in 0..3 {
                for c in 0..3 {
                    let (i, j) = (ka * 3 + r, kb * 3 + c);
                    if j <= i {
                        h.add(i, j, block[(r, c)]);
                    }
                }
            }
        }
    }
}

struct Regularization {
    local: Vec<Vector3<f64>>,
    ridge: f64,
    curvature: f64,
}
impl Regularization {
    fn new(h: &BandSym, knots: usize, reach: usize, params: &SolverParams) -> Result<Self> {
        let mut local = vec![Vector3::zeros(); knots];
        for axis in 0..3 {
            let own: Vec<_> = (0..knots)
                .map(|k| h.get(k * 3 + axis, k * 3 + axis))
                .collect();
            if own.iter().any(|v| !v.is_finite()) {
                return Err(SensorSolveError::SingularSystem);
            }
            let mut positive: Vec<_> = own.iter().copied().filter(|v| *v > 0.0).collect();
            if positive.is_empty() {
                log::warn!("Sensor axis {axis} has no data information");
                return Err(SensorSolveError::SingularSystem);
            }
            positive.sort_by(f64::total_cmp);
            let floor = positive[positive.len() / 2] * 1e-3;
            for k in 0..knots {
                local[k][axis] = own[k.saturating_sub(reach)..(k + reach + 1).min(knots)]
                    .iter()
                    .copied()
                    .fold(floor, f64::max);
            }
        }
        Ok(Self {
            local,
            ridge: params.ridge,
            curvature: params.curvature,
        })
    }

    fn terms(&self, mut visit: impl FnMut(usize, &[(usize, f64)], f64)) {
        for (k, local) in self.local.iter().enumerate() {
            for r in 0..3 {
                visit(r, &[(k, 1.0)], self.ridge * local[r]);
                if k > 0 && k + 1 < self.local.len() {
                    visit(
                        r,
                        &[(k - 1, 1.0), (k, -2.0), (k + 1, 1.0)],
                        self.curvature * local[r],
                    );
                }
            }
        }
    }

    fn cost(&self, coeffs: &[Vector3<f64>]) -> f64 {
        let mut cost = 0.0;
        self.terms(|r, taps, w| {
            let v: f64 = taps.iter().map(|(k, a)| coeffs[*k][r] * a).sum();
            cost += 0.5 * w * v * v;
        });
        cost
    }

    fn add(&self, h: &mut BandSym, g: &mut [f64], coeffs: &[Vector3<f64>]) {
        self.terms(|r, taps, w| {
            let v: f64 = taps.iter().map(|(k, a)| coeffs[*k][r] * a).sum();
            for &(ka, a) in taps {
                g[ka * 3 + r] += w * a * v;
                for &(kb, b) in taps {
                    if kb <= ka {
                        h.add(ka * 3 + r, kb * 3 + r, w * a * b);
                    }
                }
            }
        });
    }
}

fn residuals(
    pairs: &[SensorPair],
    support: &Support,
    endpoints: &[Endpoint],
    coeffs: &[Vector3<f64>],
    cancel: &impl Fn() -> bool,
) -> Result<Vec<Vector2<f64>>> {
    let mut errors = Vec::with_capacity(support.observations.len());
    let mut previous_pair = usize::MAX;
    for obs in &support.observations {
        if previous_pair != obs.pair {
            cancelled(cancel)?;
            previous_pair = obs.pair;
        }
        let pair = &pairs[obs.pair];
        errors.push(
            sensor::residual(
                pair,
                &pair.points[obs.point],
                endpoints[obs.a].at(coeffs),
                endpoints[obs.b].at(coeffs),
            )
            .ok_or(SensorSolveError::InvalidProjection)?,
        );
    }
    Ok(errors)
}

fn scales(support: &Support, errors: &[Vector2<f64>]) -> Vec<f64> {
    let mut groups = vec![vec![]; support.bands.len()];
    for (obs, e) in support.observations.iter().zip(errors) {
        groups[obs.group].push(e.norm());
    }
    groups
        .into_iter()
        .map(|mut values| {
            values.sort_by(f64::total_cmp);
            2.5 * (1.4826 * values[values.len() / 2]).max(SIGMA_FLOOR_PX)
        })
        .collect()
}

fn cost(
    support: &Support,
    errors: &[Vector2<f64>],
    scales: &[f64],
    reg: &Regularization,
    coeffs: &[Vector3<f64>],
) -> f64 {
    support
        .observations
        .iter()
        .zip(errors)
        .map(|(obs, e)| {
            let s2 = scales[obs.group].powi(2);
            0.5 * s2 * (e.norm_squared() / s2).ln_1p()
        })
        .sum::<f64>()
        + reg.cost(coeffs)
}

fn max_movement(
    pairs: &[SensorPair],
    support: &Support,
    endpoints: &[Endpoint],
    before: &[Vector3<f64>],
    after: &[Vector3<f64>],
    cancel: &impl Fn() -> bool,
) -> Result<f64> {
    let mut maximum: f64 = 0.0;
    let mut previous_pair = usize::MAX;
    for obs in &support.observations {
        if previous_pair != obs.pair {
            cancelled(cancel)?;
            previous_pair = obs.pair;
        }
        let pair = &pairs[obs.pair];
        let point = &pair.points[obs.point];
        for (frame, p, ep) in [
            (&pair.frame_a, &point.a, &endpoints[obs.a]),
            (&pair.frame_b, &point.b, &endpoints[obs.b]),
        ] {
            let d = (frame.add_correction(p, ep.at(after))
                - frame.add_correction(p, ep.at(before)))
            .component_mul(&frame.full_to_track)
            .norm();
            if !d.is_finite() {
                return Ok(f64::INFINITY);
            }
            maximum = maximum.max(d);
        }
    }
    Ok(maximum)
}

fn converged(predicted_px: f64, accepted_px: f64, relative_cost: f64) -> bool {
    predicted_px <= 1e-4 && accepted_px <= 1e-4 && relative_cost <= 1e-8
}

fn accepts_trial(old_cost: f64, trial_cost: f64, slope: f64, alpha: f64) -> bool {
    trial_cost.is_finite() && trial_cost <= old_cost + 1e-4 * alpha * slope
}

pub(super) fn solve_sensor<'a>(
    pairs: &[SensorPair],
    bands: &[SensorBand],
    quats: &'a TimeQuat,
    forced: Option<f64>,
    params: &SolverParams,
    cancel: &impl Fn() -> bool,
) -> Result<SensorSolveResult<'a>> {
    cancelled(cancel)?;
    let support = Support::new(pairs, bands, cancel)?;
    let mut sampler = support.prior_sampler(pairs, quats, forced, cancel)?;
    let cutoff_hz = sampler.cutoff_hz();
    cancelled(cancel)?;
    let band_times: Vec<_> = support
        .bands
        .iter()
        .flat_map(|i| [bands[*i].ta_us, bands[*i].tb_us])
        .collect();
    let band_prior = sampler.sample(&band_times, cancel)?;
    let initial_bands: Vec<_> = support
        .bands
        .iter()
        .enumerate()
        .map(|(j, &i)| {
            let b = &bands[i];
            solver::BandMeasurement {
                pair: b.pair,
                ta_us: b.ta_us,
                tb_us: b.tb_us,
                rho: -b.correction + band_prior[j * 2 + 1] - band_prior[j * 2],
                info: b.info,
                m: Matrix3::identity(),
            }
        })
        .collect();
    let mut solution =
        solver::solve(&initial_bands, params).ok_or(SensorSolveError::SingularSystem)?;
    cancelled(cancel)?;
    let endpoints = layout(
        &mut solution,
        &support.times,
        sampler.sample(&support.times, cancel)?,
        params.spacing_us,
    );
    let knots = solution.coeffs.len();
    #[cfg(test)]
    if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {
        println!("sensor memory observation_bytes={} endpoint_bytes={} raw_point_bytes={} observations={} endpoints={} knots={}",std::mem::size_of::<Observation>(),std::mem::size_of::<Endpoint>(),std::mem::size_of::<sensor::SensorPoint>(),support.observations.len(),endpoints.len(),knots);
    }
    let kb = support
        .observations
        .iter()
        .map(|o| {
            let a = &endpoints[o.a].knots;
            let b = &endpoints[o.b].knots;
            a[3].max(b[3]) - a[0].min(b[0])
        })
        .max()
        .unwrap()
        .max(2);
    let bw = 3 * kb + 2;
    let mut periods: Vec<_> = initial_bands
        .iter()
        .map(|b| (b.tb_us - b.ta_us).abs())
        .collect();
    periods.sort_by(f64::total_cmp);
    let reach = (periods[periods.len() / 2] / params.spacing_us)
        .ceil()
        .max(1.0) as usize;
    let mut initial_cost = 0.0;
    let mut final_cost = f64::INFINITY;
    let mut max_step_px = f64::INFINITY;
    let mut previous_scales: Option<Vec<f64>> = None;
    let mut previous_weights: Option<Vec<f64>> = None;
    for iteration in 0..20 {
        cancelled(cancel)?;
        let errors = residuals(pairs, &support, &endpoints, &solution.coeffs, cancel)?;
        let scales = scales(&support, &errors);
        let weights: Vec<_> = support
            .observations
            .iter()
            .zip(&errors)
            .map(|(obs, e)| 1.0 / (1.0 + e.norm_squared() / scales[obs.group].powi(2)))
            .collect();
        let scale_change = previous_scales.as_ref().map_or(0.0, |old| {
            scales
                .iter()
                .zip(old)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max)
        });
        let weight_change = previous_weights.as_ref().map_or(0.0, |old| {
            weights
                .iter()
                .zip(old)
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max)
        });
        let mut h = BandSym::new(knots * 3, bw);
        let mut g = vec![0.0; knots * 3];
        let mut previous_pair = usize::MAX;
        for (obs, weight) in support.observations.iter().zip(&weights) {
            if previous_pair != obs.pair {
                cancelled(cancel)?;
                previous_pair = obs.pair;
            }
            let pair = &pairs[obs.pair];
            let a = &endpoints[obs.a];
            let b = &endpoints[obs.b];
            let (error, ja, jb) = sensor::residual_and_jacobians(
                pair,
                &pair.points[obs.point],
                a.at(&solution.coeffs),
                b.at(&solution.coeffs),
            )
            .ok_or(SensorSolveError::InvalidProjection)?;
            accumulate(&mut h, &mut g, &blocks(a, b, ja, jb), error, *weight);
        }
        let reg = Regularization::new(&h, knots, reach, params)?;
        let old_cost = cost(&support, &errors, &scales, &reg, &solution.coeffs);
        if iteration == 0 {
            initial_cost = old_cost;
        }
        reg.add(&mut h, &mut g, &solution.coeffs);
        if !old_cost.is_finite() || g.iter().any(|v| !v.is_finite()) {
            return Err(SensorSolveError::SingularSystem);
        }
        let delta = h
            .solve(g.iter().map(|v| -v).collect())
            .ok_or(SensorSolveError::SingularSystem)?;
        if delta.iter().any(|v| !v.is_finite()) {
            return Err(SensorSolveError::SingularSystem);
        }
        let slope: f64 = g.iter().zip(&delta).map(|(g, d)| g * d).sum();
        let trial = |alpha: f64| {
            solution
                .coeffs
                .iter()
                .zip(delta.chunks(3))
                .map(|(u, d)| u + Vector3::new(d[0], d[1], d[2]) * alpha)
                .collect::<Vec<_>>()
        };
        let full = trial(1.0);
        let predicted = max_movement(pairs, &support, &endpoints, &solution.coeffs, &full, cancel)?;
        // Check the full predicted direction before line search can shrink it or projection rounding masks it.
        if predicted <= 1e-4 && slope.abs() / old_cost.max(1.0) <= 1e-8 {
            log::debug!("Sensor GN converged before trial iteration={} cost={old_cost} predicted_px={predicted} slope={slope} scale_change={scale_change} weight_change={weight_change}", iteration + 1);
            #[cfg(test)]
            if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {
                println!("sensor GN iteration={} cost={old_cost} predicted={predicted} slope={slope} step=0 alpha=0 scale_change={scale_change} weight_change={weight_change} done=true before_trial=true band_bytes={}", iteration+1, knots*3*(bw+1)*8);
            }
            return Ok(SensorSolveResult {
                prior_sampler: sampler,
                solution,
                cutoff_hz,
                measured_pairs: support.measured_pairs,
                iterations: iteration + 1,
                initial_cost,
                final_cost: old_cost,
                max_step_px: 0.0,
                band_bytes: knots * 3 * (bw + 1) * 8,
            });
        }
        #[cfg(test)]
        if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {
            println!(
                "sensor GN before iteration={} cost={old_cost} predicted={predicted} slope={slope}",
                iteration + 1
            );
        }
        let mut accepted = None;
        for step in 0..11 {
            cancelled(cancel)?;
            let alpha = 0.5f64.powi(step);
            let candidate = if step == 0 {
                full.clone()
            } else {
                trial(alpha)
            };
            let candidate_errors = match residuals(pairs, &support, &endpoints, &candidate, cancel)
            {
                Ok(e) => e,
                Err(SensorSolveError::InvalidProjection) => continue,
                Err(e) => return Err(e),
            };
            let trial_cost = cost(&support, &candidate_errors, &scales, &reg, &candidate);
            let relative = (trial_cost - old_cost).abs() / old_cost.max(1.0);
            let movement = max_movement(
                pairs,
                &support,
                &endpoints,
                &solution.coeffs,
                &candidate,
                cancel,
            )?;
            let done = converged(predicted, movement, relative);
            #[cfg(test)]
            if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {
                println!("sensor GN trial alpha={alpha} cost={trial_cost} relative={relative} step={movement} armijo_bound={}",old_cost+1e-4*alpha*slope);
            }
            if accepts_trial(old_cost, trial_cost, slope, alpha) {
                accepted = Some((candidate, trial_cost, movement, alpha, done));
                break;
            }
        }
        let Some((candidate, next_cost, movement, alpha, done)) = accepted else {
            log::warn!("Sensor GN NoDescent iteration={} cost={old_cost} predicted_px={predicted} slope={slope}", iteration + 1);
            return Err(SensorSolveError::NoDescent);
        };
        solution.coeffs = candidate;
        final_cost = next_cost;
        max_step_px = movement;
        log::debug!("Sensor GN iteration={} cost={old_cost} next={next_cost} predicted_px={predicted} accepted_px={movement} alpha={alpha} scale_change={scale_change} weight_change={weight_change} converged={done}", iteration + 1);
        #[cfg(test)]
        if std::env::var_os("GYROFLOW_SENSOR_TEST_DIAGNOSTICS").is_some() {
            println!("sensor GN iteration={} cost={old_cost} next={next_cost} predicted={predicted} step={movement} alpha={alpha} scale_change={scale_change} weight_change={weight_change} done={done}", iteration + 1);
        }
        if done {
            return Ok(SensorSolveResult {
                prior_sampler: sampler,
                solution,
                cutoff_hz,
                measured_pairs: support.measured_pairs,
                iterations: iteration + 1,
                initial_cost,
                final_cost,
                max_step_px,
                band_bytes: knots * 3 * (bw + 1) * 8,
            });
        }
        previous_scales = Some(scales);
        previous_weights = Some(weights);
    }
    log::warn!("Sensor GN NotConverged iterations=20 initial_cost={initial_cost} final_cost={final_cost} max_step_px={max_step_px}");
    Err(SensorSolveError::NotConverged)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nalgebra::{DMatrix, DVector, UnitQuaternion};

    #[test]
    fn sensor_microstep_cannot_bypass_armijo() {
        let old_cost: f64 = 1.0;
        let slope: f64 = -2e-8;
        let predicted = 8e-5;
        let movement = 8e-5;
        let trial_cost = old_cost + 5e-9;
        let done = converged(predicted, movement, (trial_cost - old_cost).abs() / old_cost.max(1.0));
        assert!(done);
        assert!(slope.abs() / old_cost.max(1.0) > 1e-8, "the pre-trial convergence check must not exit");
        assert!(!accepts_trial(old_cost, trial_cost, slope, 1.0), "a converged microstep must still satisfy Armijo");
        assert!(!accepts_trial(old_cost, old_cost - 1e-13, slope, 1.0), "a decrease smaller than the Armijo requirement is insufficient");
        assert!(accepts_trial(old_cost, old_cost + 1e-4 * slope, slope, 1.0));
        assert!(!accepts_trial(old_cost, f64::NAN, slope, 1.0));
    }

    #[test]
    fn sensor_band_assembly_matches_dense_with_overlapping_knots_and_regularization() {
        let a = Endpoint {
            prior: Vector3::zeros(),
            knots: [0, 1, 2, 3],
            weights: [0.1, 0.2, 0.3, 0.4],
        };
        let b = Endpoint {
            prior: Vector3::zeros(),
            knots: [1, 2, 3, 4],
            weights: [0.3, 0.2, 0.4, 0.1],
        };
        let ja = Matrix2x3::new(500.0, 2.0, 70.0, 1.0, 500.0, -120.0);
        let jb = Matrix2x3::new(-499.0, 3.0, -65.0, 2.0, -501.0, 130.0);
        let e = Vector2::new(0.3, -0.7);
        let mut j = DMatrix::zeros(2, 15);
        for (endpoint, derivative) in [(&a, ja), (&b, jb)] {
            for (&k, w) in endpoint.knots.iter().zip(endpoint.weights) {
                for r in 0..2 {
                    for c in 0..3 {
                        j[(r, 3 * k + c)] += w * derivative[(r, c)];
                    }
                }
            }
        }
        let mut dense = j.transpose() * &j * 0.7;
        let mut dg = j.transpose() * DVector::from_column_slice(e.as_slice()) * 0.7;
        let mut h = BandSym::new(15, 14);
        let mut g = vec![0.0; 15];
        let blocks = blocks(&a, &b, ja, jb);
        assert_eq!(blocks.len(), 5);
        accumulate(&mut h, &mut g, &blocks, e, 0.7);
        let params = SolverParams {
            ridge: 1e-5,
            ..Default::default()
        };
        let reg = Regularization::new(&h, 5, 2, &params).unwrap();
        let u: Vec<_> = (0..5)
            .map(|i| Vector3::new(i as f64 * 0.01, 0.03, -0.02))
            .collect();
        reg.add(&mut h, &mut g, &u);
        let mut independent_cost = 0.0;
        for k in 0..5 {
            for axis in 0..3 {
                let mut row = DVector::zeros(15);
                row[k * 3 + axis] = 1.0;
                let w = reg.local[k][axis] * params.ridge;
                let val = u[k][axis];
                independent_cost += 0.5 * w * val * val;
                dense += &row * row.transpose() * w;
                dg += row * w * val;
                if k > 0 && k < 4 {
                    let mut row = DVector::zeros(15);
                    row[(k - 1) * 3 + axis] = 1.0;
                    row[k * 3 + axis] = -2.0;
                    row[(k + 1) * 3 + axis] = 1.0;
                    let w = reg.local[k][axis] * params.curvature;
                    let val = u[k - 1][axis] - 2.0 * u[k][axis] + u[k + 1][axis];
                    independent_cost += 0.5 * w * val * val;
                    dense += &row * row.transpose() * w;
                    dg += row * w * val;
                }
            }
        }
        assert!((reg.cost(&u) - independent_cost).abs() < 1e-12);
        for i in 0..15 {
            assert!((g[i] - dg[i]).abs() < 1e-9);
            for k in 0..=i {
                assert!((h.get(i, k) - dense[(i, k)]).abs() < 1e-9);
            }
        }
        let band = h.solve(g.iter().map(|v| -v).collect()).unwrap();
        let expected = dense.cholesky().unwrap().solve(&(-dg));
        assert!((DVector::from_vec(band) - expected).norm() < 1e-8);
    }

    #[test]
    fn sensor_layout_extends_initial_function_for_raw_endpoint_support() {
        let mut initial = Solution {
            start_us: 0.0,
            coeffs: vec![Vector3::new(0.01, -0.02, 0.03); 8],
        };
        let before = initial.at(3000.0, 1000.0);
        let times = [-2100.0, 12100.0];
        let endpoints = layout(&mut initial, &times, vec![Vector3::zeros(); 2], 1000.0);
        assert!((initial.at(3000.0, 1000.0) - before).norm() < 1e-15);
        for e in endpoints {
            assert!(e.knots.iter().all(|k| *k < initial.coeffs.len()));
            assert!((e.weights.iter().sum::<f64>() - 1.0).abs() < 1e-14);
        }
    }

    #[test]
    fn sensor_small_alpha_does_not_turn_a_large_prediction_into_convergence() {
        assert!(!converged(0.1, 0.1 / 1024.0, 1e-10));
        assert!(converged(1e-5, 1e-5, 1e-10));
        assert!(!converged(1e-5, 1e-5, 1e-7));
    }

    #[test]
    fn sensor_regularization_rejects_an_axis_without_information() {
        let mut h = BandSym::new(12, 11);
        for k in 0..4 {
            h.add(3 * k, 3 * k, 500.0);
            h.add(3 * k + 1, 3 * k + 1, 500.0);
        }
        assert!(matches!(
            Regularization::new(&h, 4, 2, &SolverParams::default()),
            Err(SensorSolveError::SingularSystem)
        ));
    }

    #[test]
    fn sensor_invalid_trial_keeps_the_fixed_support() {
        let pair = sensor::tests::physical_pair(
            7,
            0.0,
            33333.0,
            UnitQuaternion::identity(),
            UnitQuaternion::from_euler_angles(0.0, 0.3, 0.0),
            Vector3::zeros(),
            Vector3::zeros(),
        );
        let band = sensor::fit_band_shift(&pair, &(0..40).collect::<Vec<_>>()).unwrap();
        let support = Support::new(&[pair], &[band], &|| false).unwrap();
        assert_eq!(support.observations.len(), 40);
        let pair = sensor::tests::physical_pair(
            7,
            0.0,
            33333.0,
            UnitQuaternion::identity(),
            UnitQuaternion::from_euler_angles(0.0, 0.3, 0.0),
            Vector3::zeros(),
            Vector3::zeros(),
        );
        let endpoints = vec![
            Endpoint {
                prior: Vector3::zeros(),
                knots: [0, 1, 2, 3],
                weights: [1.0, 0.0, 0.0, 0.0],
            },
            Endpoint {
                prior: Vector3::zeros(),
                knots: [0, 1, 2, 3],
                weights: [0.0, 1.0, 0.0, 0.0],
            },
        ];
        let mut invalid = false;
        for shift in [-20.0, 20.0] {
            let coeffs = vec![
                Vector3::new(shift, 0.0, 0.0),
                Vector3::zeros(),
                Vector3::zeros(),
                Vector3::zeros(),
            ];
            invalid |= matches!(
                residuals(
                    std::slice::from_ref(&pair),
                    &support,
                    &endpoints,
                    &coeffs,
                    &|| false
                ),
                Err(SensorSolveError::InvalidProjection)
            );
        }
        assert!(invalid);
        assert_eq!(support.observations.len(), 40);
    }
    #[test]
    fn sensor_cutoff_counts_unique_pairs_without_filling_the_gap() {
        let mut pairs = Vec::new();
        let mut bands = Vec::new();
        for i in 0..45 {
            let a = if i < 23 {
                i as f64 * 1e6 / 30.0
            } else {
                4e6 + (i - 23) as f64 * 1e6 / 30.0
            };
            let pair = sensor::tests::physical_pair(
                i * 3 + 7,
                a,
                a + 1e6 / 30.0,
                UnitQuaternion::identity(),
                UnitQuaternion::identity(),
                Vector3::zeros(),
                Vector3::zeros(),
            );
            for band in 0..6 {
                let indices: Vec<_> = pair
                    .points
                    .iter()
                    .enumerate()
                    .filter_map(|(j, p)| (p.band == band).then_some(j))
                    .collect();
                bands.push(sensor::fit_band_shift(&pair, &indices).unwrap());
            }
            pairs.push(pair);
        }
        let support = Support::new(&pairs, &bands, &|| false).unwrap();
        assert_eq!(support.measured_pairs, 45);
        assert!((support.duration_us - 1.5e6).abs() < 1e-8);
        assert_eq!(
            support
                .cutoff(&pairs, &TimeQuat::new(), None, &|| false)
                .unwrap(),
            DEFAULT_CUTOFF_HZ
        );
        assert_eq!(
            support
                .cutoff(&pairs, &TimeQuat::new(), Some(0.7), &|| false)
                .unwrap(),
            0.7
        );
        assert_eq!(
            support
                .cutoff(&pairs, &TimeQuat::new(), Some(f64::NAN), &|| false)
                .unwrap(),
            DEFAULT_CUTOFF_HZ
        );
    }
    #[test]
    fn sensor_exactly_two_seconds_uses_the_cutoff_grid() {
        let mut pairs = Vec::new();
        let mut bands = Vec::new();
        for i in 0..60 {
            let a = i as f64 * 1e6 / 30.0;
            let mut pair = sensor::tests::physical_pair(
                i,
                a,
                a + 1e6 / 30.0,
                UnitQuaternion::identity(),
                UnitQuaternion::identity(),
                Vector3::zeros(),
                Vector3::zeros(),
            );
            pair.duration_us = 1e6 / 30.0;
            bands.push(sensor::fit_band_shift(&pair, &(0..40).collect::<Vec<_>>()).unwrap());
            pairs.push(pair);
        }
        let support = Support::new(&pairs, &bands, &|| false).unwrap();
        assert!(
            support.duration_us >= 2e6,
            "duration={}",
            support.duration_us
        );
        assert_eq!(
            support
                .cutoff(&pairs, &TimeQuat::new(), None, &|| false)
                .unwrap(),
            CUTOFF_GRID_HZ[0]
        );
    }
}
