// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{hash_map::DefaultHasher, BTreeMap};
#[cfg(test)]
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

#[path = "optical_translation/smoothing.rs"]
mod smoothing;
#[cfg(all(test, feature = "use-opencv"))]
#[path = "optical_translation/acceptance.rs"]
mod acceptance;
pub const GEOMETRY_VERSION: u32 = 2;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OpticalTranslationSettings {
    pub reference: f64,
    pub smoothness_s: f64,
    pub along_axis: bool,
    /// Choose the reference layer and the smoothness from the analysis; `reference` and `smoothness_s` are kept but unused
    pub auto: bool,
}

impl Default for OpticalTranslationSettings {
    fn default() -> Self {
        Self { reference: 1.0, smoothness_s: 1.0, along_axis: true, auto: false }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TranslationSample {
    pub timestamp_us: i64,
    pub position: [f32; 3],
    pub ref_inv_depth: f32,
    pub confidence: f32,
    pub track_age_s: f32,
    pub segment: u32,
    /// Camera-to-world quaternion at this sample's video time, in w, x, y, z order.
    #[serde(default)]
    pub camera_to_world: [f32; 4],
    #[serde(default)]
    pub focal_length_over_short_side: f32,
    /// Image-plane increment of the layer with relative inverse depth one.
    #[serde(default)]
    pub layer_motion: [f32; 2],
    #[serde(default)]
    pub layer_scale_rate: f32,
    #[serde(default)]
    pub far_beta: f32,
    /// Layer the automatic parameters hold steady, relative like `far_beta`; zero in analyses made before it existed.
    #[serde(default)]
    pub auto_beta: f32,
    /// Confidence in the translation fit; retained rotation is weighted separately.
    #[serde(default)]
    pub weight: f32,
}

impl TranslationSample {
    fn geometry(&self) -> Option<smoothing::Geometry> {
        let focal_ratio = self.focal_length_over_short_side as f64;
        if !focal_ratio.is_finite() || focal_ratio <= 0.0 { return None; }
        Some(smoothing::Geometry { world_to_camera: nalgebra::Matrix3::identity(), focal_ratio })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TranslationConfig {
    pub track_age_k: f64,
    pub max_shift: f64,
    pub per_row: bool,
}

impl TranslationConfig {
    pub const DEFAULT: Self = Self { track_age_k: 2.0, max_shift: 0.08, per_row: true };

    // More room for lateral motion must not increase forward/backward zoom compensation.
    pub(crate) fn max_axial_shift(self) -> f64 { self.max_shift.min(0.04) }

    pub fn resolved() -> Self {
        static RESOLVED: OnceLock<TranslationConfig> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let config = Self {
                track_age_k: resolve_number("GYROFLOW_TRANSLATION_TRACK_AGE_K", Self::DEFAULT.track_age_k, |v| v >= 0.0),
                max_shift: resolve_number("GYROFLOW_TRANSLATION_MAX_SHIFT", Self::DEFAULT.max_shift, |v| v > 0.0 && v <= 0.5),
                per_row: resolve_per_row(),
            };
            log::info!(target: "lifecycle", "translation_config resolved track_age_k={} max_shift={} max_axial_shift={} per_row={}", config.track_age_k, config.max_shift, config.max_axial_shift(), config.per_row);
            config
        })
    }
}

fn resolve_number(name: &str, default: f64, valid: impl Fn(f64) -> bool) -> f64 {
    match std::env::var(name) {
        Ok(raw) if !raw.is_empty() => {
            if let Ok(value) = raw.trim().parse::<f64>() {
                if value.is_finite() && valid(value) {
                    return value;
                }
            }
            log::warn!(target: "lifecycle", "{}={} invalid, falling back to {}", name, raw, default);
            default
        }
        _ => default,
    }
}

fn resolve_per_row() -> bool {
    match std::env::var("GYROFLOW_TRANSLATION_PER_ROW") {
        Ok(raw) if !raw.is_empty() => match raw.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "off" | "no" => false,
            "1" | "true" | "on" | "yes" => true,
            _ => {
                log::warn!(target: "lifecycle", "GYROFLOW_TRANSLATION_PER_ROW={} invalid, falling back to {}", raw, TranslationConfig::DEFAULT.per_row);
                TranslationConfig::DEFAULT.per_row
            }
        },
        _ => TranslationConfig::DEFAULT.per_row,
    }
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OpticalTranslation {
    pub enabled: bool,
    pub settings: OpticalTranslationSettings,
    pub samples: Vec<TranslationSample>,
    pub quats_checksum: u64,
    pub context_checksum: u64,
    pub frames: usize,
    pub measured_frames: usize,
    pub geometry_version: u32,
    #[serde(skip)]
    pub applies: bool,
    #[serde(skip)]
    curve: Vec<TranslationCurvePoint>,
    #[serde(skip)]
    effective_smoothness_s: f64,
    #[serde(skip)]
    geometry_valid: bool,
    #[serde(skip)]
    pub(crate) output_path_checksum: u64,
    #[cfg(test)]
    #[serde(skip)]
    pub(crate) rebuild_count: usize,
}

#[derive(Clone, Debug)]
struct TranslationCurvePoint {
    timestamp_us: i64,
    segment: u32,
    shift: nalgebra::Vector3<f64>,
}

#[cfg(test)]
const CONFIDENCE_SIGMA_US: i128 = 250_000;
#[cfg(test)]
const CONFIDENCE_RADIUS_US: i128 = 3 * CONFIDENCE_SIGMA_US;

#[cfg(test)]
const ZERO_CONFIDENCE_CACHE_BYTES: usize = 32 * 1024 * 1024;
#[cfg(test)]
const ZERO_CONFIDENCE_CACHE_GRIDS: usize = 2048;

#[cfg(test)]
struct ZeroConfidenceCache {
    suffixes: HashMap<(i128, i128), Vec<f64>>,
    payload_bytes: usize,
    payload_limit: usize,
    grid_limit: usize,
    #[cfg(test)]
    evaluated_weights: usize,
}

#[cfg(test)]
impl Default for ZeroConfidenceCache {
    fn default() -> Self {
        Self {
            suffixes: HashMap::new(), payload_bytes: 0,
            payload_limit: ZERO_CONFIDENCE_CACHE_BYTES, grid_limit: ZERO_CONFIDENCE_CACHE_GRIDS,
            #[cfg(test)]
            evaluated_weights: 0,
        }
    }
}

#[cfg(test)]
impl ZeroConfidenceCache {
    fn sum(&mut self, distance_us: i128, interval_us: i128) -> f64 {
        if distance_us > CONFIDENCE_RADIUS_US { return 0.0; }
        let remainder = distance_us % interval_us;
        let first = if remainder == 0 { interval_us } else { remainder };
        let key = (interval_us, remainder);
        let index = ((distance_us - first) / interval_us) as usize;
        if let Some(suffix) = self.suffixes.get(&key) { return suffix[index]; }
        let count = ((CONFIDENCE_RADIUS_US - first) / interval_us + 1) as usize;
        let payload = (count + 1) * std::mem::size_of::<f64>();
        if payload > self.payload_limit.saturating_sub(self.payload_bytes) || self.suffixes.len() >= self.grid_limit {
            // Stream every term when another cached grid would exceed either resource budget.
            let mut distance = distance_us;
            let mut sum = 0.0;
            while distance <= CONFIDENCE_RADIUS_US {
                sum += (-0.5 * (distance as f64 / CONFIDENCE_SIGMA_US as f64).powi(2)).exp();
                #[cfg(test)]
                { self.evaluated_weights += 1; }
                distance += interval_us;
            }
            return sum;
        }
        // Integer timestamps put all queries with this remainder on the same finite grid.
        let mut suffix = vec![0.0; count + 1];
        for index in (0..count).rev() {
            let distance = first + index as i128 * interval_us;
            let weight = (-0.5 * (distance as f64 / CONFIDENCE_SIGMA_US as f64).powi(2)).exp();
            suffix[index] = weight + suffix[index + 1];
        }
        #[cfg(test)]
        { self.evaluated_weights += count; }
        let sum = suffix[index];
        self.payload_bytes += payload;
        self.suffixes.insert(key, suffix);
        sum
    }
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() { return 0.0; }
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 { values[middle - 1] * 0.5 + values[middle] * 0.5 } else { values[middle] }
}

fn time_difference_s(a: i64, b: i64) -> f64 {
    (a as i128 - b as i128) as f64 / 1e6
}

pub(crate) fn output_orientation(gyro: &super::GyroSource, timestamp_ms: f64) -> super::Quat64 {
    gyro.org_quat_at_timestamp(timestamp_ms) * gyro.smoothed_quat_at_timestamp(timestamp_ms).inverse()
}

/// Motion of the previous output's centre ray in the current output's normalized image plane.
pub(crate) fn retained_rotation(previous: &super::Quat64, current: &super::Quat64) -> nalgebra::Vector2<f64> {
    let ray = current.inverse() * previous * nalgebra::Vector3::new(0.0, 0.0, -1.0);
    if !ray.iter().all(|v| v.is_finite()) || ray.z >= -1e-9 { return nalgebra::Vector2::zeros(); }
    nalgebra::Vector2::new(-ray.x / ray.z, ray.y / ray.z)
}

fn filtered_far_beta(segment: &[TranslationSample]) -> Vec<f64> {
    filtered_beta(segment, |s| s.far_beta, false)
}

/// The automatic reference averages log depth, so that a switch between layers fades evenly in depth.
fn filtered_auto_beta(segment: &[TranslationSample]) -> Vec<f64> {
    filtered_beta(segment, |s| s.auto_beta, true)
}

fn filtered_beta(segment: &[TranslationSample], beta: impl Fn(&TranslationSample) -> f32, log: bool) -> Vec<f64> {
    let n = segment.len();
    let times: Vec<_> = segment.iter().map(|s| time_difference_s(s.timestamp_us, segment[0].timestamp_us)).collect();
    let mut output = vec![0.0; n];
    for i in 0..n {
        let left = times.partition_point(|t| *t < times[i] - 0.75);
        let right = times.partition_point(|t| *t <= times[i] + 0.75);
        let (mut sum, mut weights) = (0.0, 0.0);
        for j in left..right {
            let s = &segment[j];
            let value = beta(s);
            if !value.is_finite() || value <= 0.0 || !s.weight.is_finite() || s.weight <= 0.0 { continue; }
            let duration = if n == 1 { 1.0 } else {
                (times[(j + 1).min(n - 1)] - times[j.saturating_sub(1)]) * 0.5
            };
            let weight = (-0.5 * ((times[j] - times[i]) / 0.25).powi(2)).exp() * s.weight.clamp(0.0, 1.0) as f64 * duration;
            sum += weight * if log { (value as f64).ln() } else { value as f64 };
            weights += weight;
        }
        if weights > 0.0 { output[i] = if log { (sum / weights).exp() } else { sum / weights }; }
    }
    output
}

/// Position minus its Gaussian average, with time and position reflected at both ends so that a constant velocity
/// stays unchanged. Fades in and out over 0.25 s and is zero at both ends. `None` when cancelled.
fn high_pass(times: &[f64], positions: &[nalgebra::Vector3<f64>], sigma: f64, cancelled: &dyn Fn() -> bool) -> Option<Vec<nalgebra::Vector3<f64>>> {
    let last = times.len() - 1;
    let mut request = Vec::with_capacity(times.len());
    for i in 0..times.len() {
        if cancelled() { return None; }
        if i == 0 || i == last {
            request.push(nalgebra::Vector3::zeros());
            continue;
        }
        let mut sum = nalgebra::Vector3::zeros();
        let mut weight_sum = 0.0;
        let radius = 3.0 * sigma;
        let gaussian = |dt: f64| (-0.5 * (dt / sigma).powi(2)).exp();
        let left = times.partition_point(|time| *time < times[i] - radius);
        let right = times.partition_point(|time| *time <= times[i] + radius);
        for j in left..right {
            let weight = gaussian(times[j] - times[i]);
            sum += positions[j] * weight;
            weight_sum += weight;
        }
        // Reflect both time and position so a constant velocity stays unchanged.
        let reflected_left_end = times.partition_point(|time| *time <= radius - times[i]);
        for j in 1..reflected_left_end {
            let weight = gaussian(-times[j] - times[i]);
            sum += (positions[0] * 2.0 - positions[j]) * weight;
            weight_sum += weight;
        }
        let reflected_right_start = times.partition_point(|time| *time < 2.0 * times[last] - times[i] - radius);
        for j in reflected_right_start..last {
            let weight = gaussian(2.0 * times[last] - times[j] - times[i]);
            sum += (positions[last] * 2.0 - positions[j]) * weight;
            weight_sum += weight;
        }
        let ramp = 1.0_f64.min(times[i] / 0.25).min((times[last] - times[i]) / 0.25).max(0.0);
        request.push((positions[i] - sum / weight_sum) * ramp);
    }
    Some(request)
}

// Automatic smoothness: the strongest smoothing whose request leaves the planning budget for at most
// AUTO_OVER_BUDGET_TIME of the segment. The threshold first came from an offline sweep whose prototype planner had
// the barrier gradient sign wrong. Redone with the production planner and checked on rendered output of ten R50 V
// clips (2026-10-07, target/translation-smoothing): this rule picks 0.8-2 s, fixed 1 s and 1.58 s leave the same shake
// (within 1.5%), 0.5 s leaves more. Within that range the choice hardly matters, so the rule is kept as it is.
const AUTO_SIGMA_MIN_S: f64 = 0.1;
const AUTO_SIGMA_MAX_S: f64 = 10.0;
const AUTO_SIGMA_STEPS_PER_DECADE: f64 = 10.0;
const AUTO_OVER_BUDGET_TIME: f64 = 0.3;

/// Share of the segment's time in which the request leaves the planning budget, measured like `smoothing::constrain`.
fn over_budget_time(times: &[f64], request: &[nalgebra::Vector3<f64>], geometry: &[smoothing::Geometry],
    budget: f64, axial_budget: f64, along_axis: bool) -> f64 {
    let n = times.len();
    if n < 2 { return 0.0; }
    let (mut over, mut total) = (0.0, 0.0);
    for i in 0..n {
        let duration = (times[(i + 1).min(n - 1)] - times[i.saturating_sub(1)]) * 0.5;
        let lateral = request[i].xy().norm() * geometry[i].focal_ratio;
        if lateral > budget || (along_axis && request[i].z.abs() > axial_budget) { over += duration; }
        total += duration;
    }
    if total > 0.0 { over / total } else { 0.0 }
}

/// Smoothness in seconds and its share of time over the budget. Smoothing is tried from weak to strong and the search
/// stops at the first one that overruns the budget. `None` when cancelled.
fn auto_sigma(times: &[f64], positions: &[nalgebra::Vector3<f64>], geometry: &[smoothing::Geometry], cap: Option<f64>,
    config: &TranslationConfig, along_axis: bool, cancelled: &dyn Fn() -> bool) -> Option<(f64, f64)> {
    let upper = cap.map_or(AUTO_SIGMA_MAX_S, |cap| cap.min(AUTO_SIGMA_MAX_S));
    let (budget, axial_budget) = (config.max_shift * 0.5, config.max_axial_shift() * 0.5);
    let mut chosen = None;
    for step in 0.. {
        let sigma = (AUTO_SIGMA_MIN_S * 10.0_f64.powf(step as f64 / AUTO_SIGMA_STEPS_PER_DECADE)).min(upper).max(0.001);
        let request = high_pass(times, positions, sigma, cancelled)?;
        let over = over_budget_time(times, &request, geometry, budget, axial_budget, along_axis);
        if over > AUTO_OVER_BUDGET_TIME && chosen.is_some() { break; }
        chosen = Some((sigma, over));
        if over > AUTO_OVER_BUDGET_TIME || sigma >= upper { break; }
    }
    chosen
}

impl OpticalTranslation {
    /// Builds the curve without deciding whether its analysis context is current.
    pub fn new(samples: Vec<TranslationSample>, settings: OpticalTranslationSettings) -> Self {
        Self::new_with_cancel(samples, settings, &|| false).expect("a non-cancelled translation rebuild completes")
    }

    pub(crate) fn new_with_cancel(samples: Vec<TranslationSample>, settings: OpticalTranslationSettings, cancelled: &dyn Fn() -> bool) -> Option<Self> {
        let mut result = Self { enabled: true, samples, settings, geometry_version: GEOMETRY_VERSION, ..Default::default() };
        result.rebuild_with_cancel(TranslationConfig::resolved().track_age_k, cancelled).then_some(result)
    }

    /// Rebuilds the cached curve after loading or changing the stored data.
    pub fn rebuild(&mut self) {
        self.rebuild_with(TranslationConfig::resolved().track_age_k);
    }

    pub fn set_settings(&mut self, settings: OpticalTranslationSettings) {
        self.settings = settings;
        self.rebuild();
    }

    pub fn rebuild_with(&mut self, track_age_k: f64) {
        self.rebuild_with_cancel(track_age_k, &|| false);
    }

    /// Build privately and publish only a complete curve. Cancellation leaves the previous one intact.
    pub fn rebuild_with_cancel(&mut self, track_age_k: f64, cancelled: &dyn Fn() -> bool) -> bool {
        self.rebuild_with_output_path(track_age_k, &BTreeMap::new(), 0, cancelled)
    }

    /// Sample the already smoothed output while holding the gyro read lock, then plan without that lock.
    pub(crate) fn output_path(&self, gyro: &super::GyroSource) -> (BTreeMap<i64, super::Quat64>, u64) {
        let path: BTreeMap<_, _> = self.samples.iter().map(|s|
            (s.timestamp_us, output_orientation(gyro, s.timestamp_us as f64 / 1000.0))).collect();
        let mut hasher = DefaultHasher::new();
        gyro.get_checksum().hash(&mut hasher);
        for (ts, q) in &path {
            ts.hash(&mut hasher);
            for value in q.as_vector().iter() { value.to_bits().hash(&mut hasher); }
        }
        (path, hasher.finish().max(1))
    }

    pub(crate) fn rebuild_with_output_path(&mut self, track_age_k: f64, output_path: &BTreeMap<i64, super::Quat64>,
        path_checksum: u64, cancelled: &dyn Fn() -> bool) -> bool {
        let began = std::time::Instant::now();
        if cancelled() { return false; }
        let mut curve = Vec::new();
        let mut samples = self.samples.clone();
        samples.sort_by_key(|sample| sample.timestamp_us);
        samples.dedup_by_key(|sample| sample.timestamp_us);
        let geometry: Option<Vec<_>> = samples.iter().map(TranslationSample::geometry).collect();
        if cancelled() { return false; }
        let Some(geometry) = geometry.filter(|_| self.geometry_version == GEOMETRY_VERSION) else {
            self.curve.clear();
            self.effective_smoothness_s = 0.0;
            self.geometry_valid = false;
            self.output_path_checksum = path_checksum;
            return true;
        };
        for sample in &mut samples {
            if sample.layer_motion.iter().all(|value| value.is_finite()) && sample.layer_scale_rate.is_finite()
                && sample.far_beta.is_finite() && sample.far_beta > 0.0 && sample.weight.is_finite() {
                sample.weight = sample.weight.clamp(0.0, 1.0);
            } else {
                sample.layer_motion = [0.0; 2];
                sample.layer_scale_rate = 0.0;
                sample.far_beta = 0.0;
                sample.weight = 0.0;
            }
        }
        let requested_sigma = if self.settings.smoothness_s.is_finite() { self.settings.smoothness_s } else { OpticalTranslationSettings::default().smoothness_s };
        let auto = self.settings.auto;
        let reference = if auto { 1.0 } else if self.settings.reference.is_finite() { self.settings.reference } else { 0.0 };
        let config = TranslationConfig::resolved();
        let mut smoothness = Vec::new();
        let (mut solved_segments, mut fallback_segments, mut iterations, mut far_fallback_segments) = (0, 0, 0, 0);
        let mut start = 0;
        while start < samples.len() {
            if cancelled() { return false; }
            let mut end = start + 1;
            while end < samples.len() && samples[end].segment == samples[start].segment { end += 1; }
            let segment = &samples[start..end];
            if segment.len() == 1 {
                curve.push(TranslationCurvePoint { timestamp_us: segment[0].timestamp_us, segment: segment[0].segment, shift: nalgebra::Vector3::zeros() });
                start = end;
                continue;
            }
            let mut ages: Vec<_> = segment.iter().filter_map(|sample| {
                let age = sample.track_age_s as f64;
                (age.is_finite() && age >= 0.0).then_some(age)
            }).collect();
            let age_cap = (track_age_k.is_finite() && track_age_k > 0.0).then(|| track_age_k * median(&mut ages));
            let times: Vec<_> = segment.iter().map(|sample| time_difference_s(sample.timestamp_us, segment[0].timestamp_us)).collect();
            let far_fallback = auto && !segment.iter().any(|s| s.auto_beta.is_finite() && s.auto_beta > 0.0 && s.weight > 0.0);
            // Analysed before the automatic reference existed: hold the far layer until analysed again
            if far_fallback { far_fallback_segments += 1; }
            let depths = if auto && !far_fallback { filtered_auto_beta(segment) } else { filtered_far_beta(segment) };
            let mut position = nalgebra::Vector3::zeros();
            let positions: Vec<_> = segment.iter().enumerate().map(|(i, sample)| {
                if i > 0 {
                    let previous = output_path.get(&segment[i - 1].timestamp_us).copied().unwrap_or_default();
                    let current = output_path.get(&sample.timestamp_us).copied().unwrap_or_default();
                    let rotation = retained_rotation(&previous, &current);
                    let gain = reference * depths[i] * sample.weight as f64;
                    position += nalgebra::Vector3::new(rotation.x + gain * sample.layer_motion[0] as f64,
                        rotation.y + gain * sample.layer_motion[1] as f64,
                        if self.settings.along_axis { gain * sample.layer_scale_rate as f64 } else { 0.0 });
                }
                position
            }).collect();
            let sigma = if auto {
                let Some((sigma, over)) = auto_sigma(&times, &positions, &geometry[start..end], age_cap, &config, self.settings.along_axis, cancelled) else { return false; };
                log::debug!(target: "stab.translation", "translation auto segment={} samples={} sigma_s={} over_budget_time={} reference={}",
                    segment[0].segment, segment.len(), sigma, over, if far_fallback { "far" } else { "auto" });
                sigma
            } else {
                age_cap.map_or(requested_sigma, |cap| requested_sigma.min(cap))
            }.max(0.001);
            smoothness.push(sigma);
            let last = segment.len() - 1;
            let fixed: Vec<_> = (0..segment.len()).map(|i| i == 0 || i == last).collect();
            let curve_start = curve.len();
            let Some(request) = high_pass(&times, &positions, sigma, cancelled) else { return false; };
            curve.extend(segment.iter().zip(&request).map(|(sample, shift)|
                TranslationCurvePoint { timestamp_us: sample.timestamp_us, segment: sample.segment, shift: *shift }));
            match smoothing::constrain(&times, &request, &geometry[start..end], &fixed, sigma,
                config.max_shift * 0.5, config.max_axial_shift() * 0.5, self.settings.along_axis, cancelled) {
                Ok((shifts, stats)) => {
                    log::debug!(target: "stab.translation", "translation budget segment={} samples={} sigma_s={} request_ratio={} applied_ratio={} correction_ratio={} iterations={} stages={} line_searches={}",
                        segment[0].segment, segment.len(), sigma, stats.requested_fraction, stats.maximum_fraction,
                        stats.correction_fraction, stats.iterations, stats.stages, stats.line_searches);
                    solved_segments += usize::from(stats.iterations > 0);
                    iterations += stats.iterations;
                    for (point, shift) in curve[curve_start..].iter_mut().zip(shifts) { point.shift = shift; }
                }
                Err(error) if error.reason == smoothing::SolveError::Cancelled => return false,
                Err(error) => {
                    fallback_segments += 1;
                    log::warn!(target: "stab.translation", "translation budget fallback segment={} samples={} reason={:?} iterations={} stages={} line_searches={}",
                        segment[0].segment, segment.len(), error.reason, error.stats.iterations, error.stats.stages, error.stats.line_searches);
                }
            }
            start = end;
        }
        if cancelled() { return false; }
        self.curve = curve;
        self.geometry_valid = !samples.is_empty();
        self.output_path_checksum = path_checksum;
        #[cfg(test)]
        { self.rebuild_count += 1; }
        self.effective_smoothness_s = median(&mut smoothness);
        log::debug!(target: "stab.translation", "translation rebuild samples={} corrected_segments={} fallback_segments={} iterations={} elapsed_ms={:.3}",
            samples.len(), solved_segments, fallback_segments, iterations, began.elapsed().as_secs_f64() * 1000.0);
        if far_fallback_segments > 0 {
            log::info!(target: "stab.translation", "translation auto reference missing in {} segments, holding the far layer; analyze again to use it", far_fallback_segments);
        }
        true
    }

    /// Returns normalized image-plane motion and expansion to remove from the output.
    /// Returns zero outside the analysed ranges and in their gaps.
    pub fn shift_at(&self, timestamp_ms: f64) -> nalgebra::Vector3<f64> {
        let timestamp_us = timestamp_ms * 1000.0;
        if !timestamp_us.is_finite() { return nalgebra::Vector3::zeros(); }
        let right = self.curve.partition_point(|point| point.timestamp_us as f64 <= timestamp_us);
        if right == 0 { return nalgebra::Vector3::zeros(); }
        let left = &self.curve[right - 1];
        if timestamp_us == left.timestamp_us as f64 { return left.shift; }
        if right == self.curve.len() { return nalgebra::Vector3::zeros(); }
        let next = &self.curve[right];
        if left.segment != next.segment { return nalgebra::Vector3::zeros(); }
        let fraction = (timestamp_us - left.timestamp_us as f64) / (next.timestamp_us as i128 - left.timestamp_us as i128) as f64;
        left.shift * (1.0 - fraction) + next.shift * fraction
    }

    pub fn effective_smoothness_s(&self) -> f64 {
        self.effective_smoothness_s
    }

    /// Source-camera compensation, shared by rendering and the frame-centre status.
    pub(crate) fn camera_shift_at(&self, _source: &nalgebra::UnitQuaternion<f64>, timestamp_ms: f64,
        short_side: f64, focal_px: f64, inverted: bool, config: &TranslationConfig) -> nalgebra::Vector3<f64> {
        let shift = self.shift_at(timestamp_ms);
        // The inverse sampling matrix removes positive expansion with a negative z shift.
        let mut t = nalgebra::Vector3::new(shift.x, if inverted { -shift.y } else { shift.y }, -shift.z);
        let soft = |x: f64, limit: f64| {
            let half = limit / 2.0;
            if x <= half { x } else { half + half * ((x - half) / half).tanh() }
        };
        let limit = config.max_shift * short_side / focal_px;
        let n = t.xy().norm();
        if n > 0.0 {
            let scale = soft(n, limit) / n;
            t.x *= scale;
            t.y *= scale;
        }
        t.z = t.z.signum() * soft(t.z.abs(), config.max_axial_shift());
        if !self.settings.along_axis { t.z = 0.0; }
        t
    }

    #[cfg(test)]
    pub(crate) fn with_curve(mut points: Vec<(i64, [f64; 3])>) -> Self {
        points.sort_by_key(|point| point.0);
        points.dedup_by_key(|point| point.0);
        let samples = points.iter().map(|point| TranslationSample { timestamp_us: point.0, ..Default::default() }).collect();
        let curve = points.into_iter().map(|(timestamp_us, shift)| TranslationCurvePoint { timestamp_us, segment: 0, shift: nalgebra::Vector3::from(shift) }).collect();
        Self { enabled: true, applies: true, samples, curve, geometry_valid: true, geometry_version: GEOMETRY_VERSION, ..Default::default() }
    }

    pub fn has_valid_geometry(&self) -> bool { self.geometry_valid }

    pub(crate) fn validate_geometry(&mut self) {
        self.geometry_valid = self.geometry_version == GEOMETRY_VERSION && !self.samples.is_empty()
            && self.samples.iter().all(|sample| sample.geometry().is_some());
    }

    pub fn is_active(&self) -> bool {
        self.enabled && self.applies && self.geometry_valid && !self.samples.is_empty()
    }

    pub fn hash_into(&self, hasher: &mut impl Hasher) {
        self.enabled.hash(hasher);
        self.settings.reference.to_bits().hash(hasher);
        self.settings.smoothness_s.to_bits().hash(hasher);
        self.settings.along_axis.hash(hasher);
        // Hashed only when set, so results without it keep their checksum.
        if self.settings.auto { true.hash(hasher); }
        self.quats_checksum.hash(hasher);
        self.context_checksum.hash(hasher);
        self.geometry_version.hash(hasher);
        self.output_path_checksum.hash(hasher);
        let config = TranslationConfig::resolved();
        config.max_shift.to_bits().hash(hasher);
        config.max_axial_shift().to_bits().hash(hasher);
        self.samples.len().hash(hasher);
        for sample in &self.samples {
            sample.timestamp_us.hash(hasher);
            for value in sample.position {
                value.to_bits().hash(hasher);
            }
            sample.ref_inv_depth.to_bits().hash(hasher);
            sample.confidence.to_bits().hash(hasher);
            sample.track_age_s.to_bits().hash(hasher);
            sample.segment.hash(hasher);
            for value in sample.camera_to_world { value.to_bits().hash(hasher); }
            sample.focal_length_over_short_side.to_bits().hash(hasher);
            for value in sample.layer_motion { value.to_bits().hash(hasher); }
            sample.layer_scale_rate.to_bits().hash(hasher);
            sample.far_beta.to_bits().hash(hasher);
            if sample.auto_beta != 0.0 { sample.auto_beta.to_bits().hash(hasher); }
            sample.weight.to_bits().hash(hasher);
        }
    }

    pub fn checksum(&self) -> u64 {
        if !self.is_active() { return 0; }
        self.content_checksum()
    }

    pub(crate) fn content_checksum(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash_into(&mut hasher);
        hasher.finish().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn geometry_sample() -> TranslationSample {
        TranslationSample { camera_to_world: [1.0, 0.0, 0.0, 0.0], focal_length_over_short_side: 0.1, ..Default::default() }
    }

    fn sample(i: i64) -> TranslationSample {
        TranslationSample { timestamp_us: i * 33_333, position: [i as f32 * 0.01, 0.0, 0.0], ref_inv_depth: 2.0, confidence: 1.0, track_age_s: 5.0, segment: 0,
            layer_motion: [0.01, 0.0], far_beta: 2.0, weight: 1.0, ..geometry_sample() }
    }

    #[test]
    fn settings_default_to_the_spec_values() {
        assert_eq!(OpticalTranslationSettings::default(), OpticalTranslationSettings { reference: 1.0, smoothness_s: 1.0, along_axis: true, auto: false });
        assert_eq!(TranslationConfig::DEFAULT, TranslationConfig { track_age_k: 2.0, max_shift: 0.08, per_row: true });
    }

    #[test]
    fn project_round_trip_keeps_what_is_stored_and_nothing_else() {
        let t = OpticalTranslation { enabled: true, applies: true, samples: (0..90).map(sample).collect(), quats_checksum: 7, context_checksum: 9, frames: 90, measured_frames: 88, ..Default::default() };
        let text = crate::util::compress_to_base91_cbor(&t).unwrap();
        let back: OpticalTranslation = crate::util::decompress_from_base91_cbor(&text).unwrap();
        assert_eq!((back.enabled, back.settings, &back.samples, back.quats_checksum, back.context_checksum, back.frames, back.measured_frames),
                   (t.enabled, t.settings, &t.samples, t.quats_checksum, t.context_checksum, t.frames, t.measured_frames));
        assert!(!back.applies, "whether it applies is decided again after loading");
    }

    #[test]
    fn translation_v2_payload_round_trip_and_hash_cover_the_layer_fit() {
        let mut samples: Vec<_> = (0..90).map(sample).collect();
        samples[40].layer_motion = [0.001, -0.002];
        samples[40].layer_scale_rate = 0.003;
        samples[40].far_beta = 0.4;
        samples[40].weight = 0.7;
        let t = OpticalTranslation::new(samples, Default::default());
        let encoded = crate::util::compress_to_base91_cbor(&t).unwrap();
        let back: OpticalTranslation = crate::util::decompress_from_base91_cbor(&encoded).unwrap();
        assert_eq!(back.samples, t.samples);
        assert_eq!(back.geometry_version, 2);
        for field in 0..5 {
            let mut changed = back.clone();
            let s = &mut changed.samples[40];
            match field {
                0 => s.layer_motion[0] += 0.01,
                1 => s.layer_motion[1] += 0.01,
                2 => s.layer_scale_rate += 0.01,
                3 => s.far_beta += 0.01,
                _ => s.weight += 0.01,
            }
            assert_ne!(changed.content_checksum(), back.content_checksum());
        }
        let mut legacy = serde_json::to_value(&t).unwrap();
        legacy["geometry_version"] = 1.into();
        for sample in legacy["samples"].as_array_mut().unwrap() {
            for field in ["layer_motion", "layer_scale_rate", "far_beta", "weight"] { sample.as_object_mut().unwrap().remove(field); }
        }
        let mut legacy: OpticalTranslation = serde_json::from_value(legacy).unwrap();
        legacy.applies = true;
        legacy.rebuild();
        assert_eq!(legacy.samples.len(), t.samples.len());
        assert!(legacy.samples.iter().all(|s| s.weight == 0.0 && s.layer_motion == [0.0; 2]));
        assert!(!legacy.has_valid_geometry() && !legacy.is_active());
        assert_eq!(legacy.checksum(), 0);
    }

    #[test]
    fn checksum_is_zero_unless_active_and_follows_the_content() {
        let mut t = OpticalTranslation::new((0..90).map(sample).collect(), Default::default());
        t.applies = true;
        let c = t.checksum();
        assert_ne!(c, 0);
        t.settings.reference = 0.5;
        assert_ne!(t.checksum(), c);
        t.settings.reference = 1.0;
        t.samples[40].position[1] = 0.3;
        assert_ne!(t.checksum(), c);
        for (enabled, applies) in [(false, true), (true, false)] {
            t.enabled = enabled; t.applies = applies;
            assert_eq!(t.checksum(), 0);
        }
        assert_eq!(OpticalTranslation { enabled: true, applies: true, ..Default::default() }.checksum(), 0, "nothing measured");
    }

    /// 10 s at 30 fps: a fast sinusoid in x on top of a constant velocity
    fn walk(confidence: f32, track_age_s: f32) -> Vec<TranslationSample> {
        (0..300).map(|i| {
            let t = i as f64 / 30.0;
            let position = |t: f64| 0.02 * (std::f64::consts::TAU * 3.0 * t).sin() + 0.5 * t;
            TranslationSample { timestamp_us: (t * 1e6) as i64, position: [(0.02 * (std::f64::consts::TAU * 3.0 * t).sin() + 0.5 * t) as f32, 0.0, 0.0],
                ref_inv_depth: 2.0, confidence, track_age_s, segment: 0,
                layer_motion: [(position(t) - position(t - 1.0 / 30.0)) as f32, 0.0], far_beta: 2.0, weight: confidence, ..geometry_sample() }
        }).collect()
    }
    fn built(samples: Vec<TranslationSample>, settings: OpticalTranslationSettings, k: f64) -> OpticalTranslation {
        let mut t = OpticalTranslation { enabled: true, applies: true, samples, settings, geometry_version: GEOMETRY_VERSION, ..Default::default() };
        t.rebuild_with(k);
        t
    }

    #[test]
    fn translation_zero_reference_and_zero_weight_keep_the_rotation_high_pass() {
        let samples = walk(1.0, 100.0);
        let path: BTreeMap<_, _> = samples.iter().map(|s| {
            let t = s.timestamp_us as f64 / 1e6;
            (s.timestamp_us, super::super::Quat64::from_euler_angles(0.001 * (std::f64::consts::TAU * 1.5 * t).sin(), 0.0, 0.0))
        }).collect();
        let mut infinite = built(samples.clone(), OpticalTranslationSettings { reference: 0.0, ..Default::default() }, 0.0);
        assert!(infinite.rebuild_with_output_path(0.0, &path, 7, &|| false));
        let mut rejected = built(samples.iter().map(|s| TranslationSample { weight: 0.0, ..*s }).collect(), Default::default(), 0.0);
        assert!(rejected.rebuild_with_output_path(0.0, &path, 7, &|| false));
        for i in 100..200 {
            let time = samples[i].timestamp_us as f64 / 1000.0;
            let shift = infinite.shift_at(time);
            assert!((shift - rejected.shift_at(time)).norm() < 1e-12);
            let expected = 0.001 * (std::f64::consts::TAU * 1.5 * time / 1000.0).sin();
            assert!((shift.y - expected).abs() < 1e-5, "{time}: {} vs {expected}", shift.y);
        }
    }

    #[test]
    fn translation_vo_restart_does_not_split_the_analysis_range() {
        let mut samples = walk(1.0, 100.0);
        let mut before = built(samples.clone(), Default::default(), 0.0);
        // A VO reset changes its diagnostic position, but has no motion for that pair.
        samples[150].weight = 0.0;
        samples[150].confidence = 0.0;
        for sample in &mut samples[150..] { sample.position = [0.0; 3]; }
        before.samples[150].weight = 0.0;
        before.rebuild_with(0.0);
        let after = built(samples, Default::default(), 0.0);
        for time in [4960.0, 4990.0, 5000.0, 5010.0, 5040.0] {
            assert_eq!(before.shift_at(time), after.shift_at(time));
        }
        assert!(after.shift_at(5000.0).norm() > 1e-3);
        assert!((after.shift_at(5000.01) - after.shift_at(4999.99)).norm() < 1e-4);
    }

    #[test]
    fn fast_movement_is_recovered_and_steady_movement_is_left_alone() {
        let t = built(walk(1.0, 100.0), Default::default(), 2.0);
        for i in 90..210 {
            let s = i as f64 / 30.0;
            let got = t.shift_at(s * 1000.0);
            let want = 2.0 * 0.02 * (std::f64::consts::TAU * 3.0 * s).sin();
            assert!((got.x - want).abs() < 0.05 * 0.04, "t={s} got {} want {want}", got.x);
            assert!(got.y.abs() < 1e-9 && got.z.abs() < 1e-9);
        }
    }

    #[test]
    fn smoothness_is_capped_by_how_long_tracks_live() {
        // Tracks that live 0.2 s allow 0.4 s at k = 2 (the age is stored in single precision)
        assert!((built(walk(1.0, 0.2), Default::default(), 2.0).effective_smoothness_s() - 0.4).abs() < 1e-6);
        assert_eq!(built(walk(1.0, 0.2), Default::default(), 0.0).effective_smoothness_s(), 1.0);
        assert_eq!(built(walk(1.0, 100.0), Default::default(), 2.0).effective_smoothness_s(), 1.0);
    }

    #[test]
    fn shift_fades_to_zero_at_segment_ends() {
        let t = built(walk(1.0, 100.0), Default::default(), 2.0);
        assert_eq!(t.shift_at(0.0).norm(), 0.0);
        assert_eq!(t.shift_at(299.0 / 30.0 * 1000.0).norm(), 0.0);
        assert_eq!(t.shift_at(-50.0).norm(), 0.0);
        assert_eq!(t.shift_at(11_000.0).norm(), 0.0);
        let peak = (90..210).map(|i| t.shift_at(i as f64 / 30.0 * 1000.0).norm()).fold(0.0, f64::max);
        for i in (0..5).chain(295..300) {
            assert!(t.shift_at(i as f64 / 30.0 * 1000.0).norm() <= 1.5 * peak);
        }
    }

    #[test]
    fn segments_do_not_leak_into_each_other() {
        let mut samples = walk(1.0, 100.0);
        samples.truncate(90);
        let alone = built(samples.clone(), Default::default(), 2.0);
        samples.extend((120..210).map(|i| TranslationSample { timestamp_us: (i as f64 / 30.0 * 1e6) as i64, position: [i as f32 * 3.0, 50.0, 0.0], ref_inv_depth: 0.01, confidence: 1.0, track_age_s: 100.0, segment: 1, ..geometry_sample() }));
        let both = built(samples, Default::default(), 2.0);
        for i in 0..90 {
            let ts = i as f64 / 30.0 * 1000.0;
            assert_eq!(both.shift_at(ts), alone.shift_at(ts));
        }
        assert_eq!(both.shift_at(3500.0).norm(), 0.0, "between the segments");
    }

    #[test]
    fn confidence_and_reference_scale_the_shift() {
        let full = built(walk(1.0, 100.0), Default::default(), 2.0).shift_at(5010.0);
        let half = built(walk(0.5, 100.0), Default::default(), 2.0).shift_at(5010.0);
        let none = built(walk(0.0, 100.0), Default::default(), 2.0).shift_at(5010.0);
        let double = built(walk(1.0, 100.0), OpticalTranslationSettings { reference: 2.0, ..Default::default() }, 2.0).shift_at(5010.0);
        assert!(full.x.abs() > 1e-3);
        assert!((half.x - 0.5 * full.x).abs() < 1e-9 && (double.x - 2.0 * full.x).abs() < 1e-9);
        assert_eq!(none.norm(), 0.0);
    }

    #[test]
    fn bad_samples_never_reach_the_curve() {
        let mut samples = walk(1.0, 100.0);
        samples[100].layer_motion = [f32::NAN, 0.0];
        samples[150].layer_scale_rate = f32::INFINITY;
        samples[200].far_beta = f32::NAN;
        let t = built(samples, Default::default(), 2.0);
        for i in 0..3000 {
            let s = t.shift_at(i as f64 * 3.4);
            assert!(s.iter().all(|v| v.is_finite()), "at {} ms: {s:?}", i as f64 * 3.4);
        }
    }

    #[test]
    fn degenerate_inputs_do_not_panic() {
        assert_eq!(built(Vec::new(), Default::default(), 2.0).shift_at(100.0).norm(), 0.0);
        assert_eq!(built(vec![walk(1.0, 100.0)[5]], Default::default(), 2.0).shift_at(166.0).norm(), 0.0);
        let mut shuffled = walk(1.0, 100.0);
        shuffled.reverse();
        shuffled.push(shuffled[10]);
        let a = built(shuffled, Default::default(), 2.0);
        let b = built(walk(1.0, 100.0), Default::default(), 2.0);
        assert_eq!(a.shift_at(5010.0), b.shift_at(5010.0), "order and duplicates don't matter");
        let zero_smooth = built(walk(1.0, 100.0), OpticalTranslationSettings { smoothness_s: 0.0, ..Default::default() }, 2.0);
        assert!(zero_smooth.shift_at(5010.0).iter().all(|v| v.is_finite()));
    }

    #[test]
    fn settings_and_reloading_rebuild_the_curve() {
        let mut t = OpticalTranslation::new(walk(1.0, 100.0), Default::default());
        let before = t.shift_at(5010.0);
        t.set_settings(OpticalTranslationSettings { reference: 0.5, ..Default::default() });
        assert!((t.shift_at(5010.0).x - 0.5 * before.x).abs() < 1e-9);
        let text = crate::util::compress_to_base91_cbor(&t).unwrap();
        let mut back: OpticalTranslation = crate::util::decompress_from_base91_cbor(&text).unwrap();
        assert_eq!(back.shift_at(5010.0).norm(), 0.0, "no curve until rebuilt");
        back.rebuild();
        assert_eq!(back.shift_at(5010.0), t.shift_at(5010.0));
    }

    #[test]
    fn translation_range_boundary_uses_the_ramp_without_scaling_retained_rotation() {
        let samples = (0..3).map(|i| TranslationSample {
            timestamp_us: i * 100_000,
            position: [if i == 1 { 1.0 } else { 0.0 }, 0.0, 0.0],
            ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0,
            layer_motion: [match i { 1 => 1.0, 2 => -1.0, _ => 0.0 }, 0.0], far_beta: 1.0, weight: 1.0, ..geometry_sample()
        }).collect();
        let t = built(samples, OpticalTranslationSettings { smoothness_s: 0.1, ..Default::default() }, 0.0);
        // Three real position samples plus their point reflections give this residual.
        let residual = 1.0 - (1.0 - 2.0 * (-2.0_f64).exp()) /
            (1.0 + 2.0 * (-0.5_f64).exp() + 2.0 * (-2.0_f64).exp() + 2.0 * (-4.5_f64).exp());
        let got = t.shift_at(100.0).x;
        let want = residual * 0.4;
        assert!((got - want).abs() < 1e-12, "got {got} want {want}");
    }

    fn naive_zero_confidence_weight_sum(mut distance_us: i128, interval_us: i128) -> f64 {
        let mut sum = 0.0;
        while distance_us <= 750_000 {
            sum += (-0.5 * (distance_us as f64 / 250_000.0).powi(2)).exp();
            distance_us += interval_us;
        }
        sum
    }

    #[test]
    fn cached_zero_confidence_weights_match_discrete_sums() {
        let mut cache = ZeroConfidenceCache::default();
        for (distance, interval) in [(100_000, 100_000), (100_010, 100_003), (27_182, 12_345), (750_000, 250_000), (1_000_000, 1_000_000)] {
            let got = cache.sum(distance, interval);
            let want = naive_zero_confidence_weight_sum(distance, interval);
            assert!((got - want).abs() < 1e-12 * want.max(1.0), "distance={distance} interval={interval} got {got} want {want}");
        }
    }

    #[test]
    fn dense_zero_confidence_grid_is_built_once_for_different_queries() {
        let mut cache = ZeroConfidenceCache::default();
        let first = cache.sum(1, 1);
        assert_eq!(cache.evaluated_weights, 750_000);
        let second = cache.sum(2, 1);
        assert!((second - (first - (-0.5 * (1.0_f64 / 250_000.0).powi(2)).exp())).abs() < 1e-9);
        for distance in [100_000, 200_000, 500_000, 750_000] {
            let got = cache.sum(distance, 1);
            let want = naive_zero_confidence_weight_sum(distance, 1);
            assert!((got - want).abs() < 1e-12 * want.max(1.0), "distance={distance} got {got} want {want}");
        }
        assert_eq!(cache.evaluated_weights, 750_000, "queries on the same grid must reuse the suffix weights");
        assert_eq!(cache.suffixes.len(), 1);
    }

    #[test]
    fn dense_endpoint_compensation_matches_the_unoptimized_discrete_formula() {
        let mut samples = vec![
            TranslationSample { timestamp_us: 0, position: [0.0; 3], ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0, ..geometry_sample() },
            TranslationSample { timestamp_us: 1, position: [1.0, 0.0, 0.0], ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0, ..geometry_sample() },
            TranslationSample { timestamp_us: 1_000_000, position: [0.0; 3], ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0, ..geometry_sample() },
        ];
        for (s, motion) in samples.iter_mut().zip([0.0, 1.0, -1.0]) {
            s.layer_motion = [motion, 0.0]; s.far_beta = 1.0; s.weight = 1.0;
        }
        let t = built(samples, OpticalTranslationSettings { smoothness_s: 0.001, ..Default::default() }, 0.0);
        let g1 = (-0.5 * (1.0_f64 / 1000.0).powi(2)).exp();
        let g2 = (-0.5 * (2.0_f64 / 1000.0).powi(2)).exp();
        let residual = 1.0 - (1.0 - g2) / (1.0 + g1 + g2);
        let want = residual * (1.0 / 250_000.0);
        let got = t.shift_at(0.001).x;
        assert!((got - want).abs() < 1e-12 * want.abs(), "got {got} want {want}");
        assert_eq!(t.shift_at(0.0).norm(), 0.0);
        assert_eq!(t.shift_at(1000.0).norm(), 0.0);
    }

    #[test]
    fn zero_confidence_cache_respects_its_payload_budget() {
        let mut cache = ZeroConfidenceCache { payload_limit: 64, ..Default::default() };
        let cached = cache.sum(100_000, 100_000);
        assert_eq!(cache.payload_bytes, 64);
        assert_eq!(cache.suffixes.len(), 1);
        let evaluated = cache.evaluated_weights;
        for distance in [10_000, 110_000, 210_000] {
            let got = cache.sum(distance, 100_000);
            assert_eq!(got, naive_zero_confidence_weight_sum(distance, 100_000), "budget fallback must keep every discrete term");
            assert_eq!(cache.payload_bytes, 64);
            assert_eq!(cache.suffixes.len(), 1);
        }
        let after_fallback = cache.evaluated_weights;
        assert!(after_fallback > evaluated);
        assert_eq!(cache.sum(100_000, 100_000), cached, "already cached grids remain usable at the budget limit");
        assert_eq!(cache.evaluated_weights, after_fallback);
        let mut none = ZeroConfidenceCache { payload_limit: 0, ..Default::default() };
        assert_eq!(none.sum(100_000, 100_000), naive_zero_confidence_weight_sum(100_000, 100_000));
        assert_eq!(none.payload_bytes, 0);
        assert!(none.suffixes.is_empty());
    }

    #[test]
    fn zero_confidence_cache_respects_its_grid_budget() {
        let mut cache = ZeroConfidenceCache { grid_limit: 2, ..Default::default() };
        cache.sum(100_000, 100_000);
        cache.sum(200_000, 200_000);
        assert_eq!(cache.suffixes.len(), 2);
        let payload = cache.payload_bytes;
        let got = cache.sum(150_000, 150_000);
        assert_eq!(got, naive_zero_confidence_weight_sum(150_000, 150_000));
        assert_eq!(cache.suffixes.len(), 2);
        assert_eq!(cache.payload_bytes, payload);
        let evaluated = cache.evaluated_weights;
        cache.sum(300_000, 100_000);
        assert_eq!(cache.evaluated_weights, evaluated, "a different query on a cached grid must still reuse its weights");
        let mut none = ZeroConfidenceCache { grid_limit: 0, ..Default::default() };
        assert_eq!(none.sum(100_000, 100_000), naive_zero_confidence_weight_sum(100_000, 100_000));
        assert!(none.suffixes.is_empty());
        assert_eq!(none.payload_bytes, 0);
    }

    #[test]
    fn depth_filter_preserves_constant_gain_and_rejects_invalid_support() {
        let mut samples: Vec<_> = (0..61).map(sample).collect();
        samples[20].far_beta = 0.0;
        samples[21].far_beta = f32::NAN;
        samples[22].far_beta = -1.0;
        samples[23].far_beta = 1000.0;
        samples[23].weight = 0.0;
        let depths = filtered_far_beta(&samples);
        for depth in depths { assert!((depth - 2.0).abs() < 1e-12); }
        for sample in &mut samples { sample.weight = 0.0; }
        assert!(filtered_far_beta(&samples).iter().all(|rho| *rho == 0.0));
    }

    #[test]
    fn depth_filter_respects_time_density_and_arbitrary_depth_scale() {
        let make = |times: Vec<f64>| times.into_iter().map(|t| TranslationSample {
            timestamp_us: (t*1e6).round() as i64, far_beta: (2.0 + 0.2*t) as f32,
            ..sample(0)
        }).collect::<Vec<_>>();
        let regular = make((0..101).map(|i| i as f64 * 0.02).collect());
        let mut times: Vec<_> = (0..101).map(|i| i as f64 * 0.02).collect();
        times.extend((0..100).map(|i| i as f64 * 0.02 + 0.01).filter(|t| *t > 1.0));
        times.sort_by(f64::total_cmp);
        let irregular = make(times);
        let regular_depth = filtered_far_beta(&regular)[50];
        let index = irregular.iter().position(|s| s.timestamp_us == 1_000_000).unwrap();
        let irregular_depth = filtered_far_beta(&irregular)[index];
        assert!((regular_depth-irregular_depth).abs() < 0.0001);
        let scaled: Vec<_> = irregular.iter().map(|s| TranslationSample { far_beta: s.far_beta * 8.0, ..*s }).collect();
        assert!((filtered_far_beta(&scaled)[index] - 8.0*irregular_depth).abs() < 1e-12);
    }

    #[test]
    fn missing_or_unknown_geometry_stays_unavailable_without_dropping_samples() {
        let mut t = OpticalTranslation::new(walk(1.0, 100.0), Default::default());
        t.applies = true;
        assert!(t.is_active());
        let samples = t.samples.clone();
        t.geometry_version = 0;
        t.rebuild();
        assert!(!t.is_active());
        assert_eq!(t.samples, samples);
        t.geometry_version = GEOMETRY_VERSION + 1;
        t.rebuild();
        assert!(!t.has_valid_geometry());
        t.geometry_version = GEOMETRY_VERSION;
        t.samples[30].focal_length_over_short_side = 0.0;
        t.rebuild();
        assert!(!t.has_valid_geometry());
        assert_eq!(t.shift_at(1000.0), nalgebra::Vector3::zeros());
    }

    #[test]
    fn cancelled_curve_build_keeps_the_previous_complete_curve() {
        let mut t = built(walk(1.0, 100.0), Default::default(), 2.0);
        let before = t.shift_at(5010.0);
        let calls = std::cell::Cell::new(0);
        assert!(!t.rebuild_with_cancel(2.0, &|| { calls.set(calls.get()+1); calls.get() > 20 }));
        assert_eq!(before, t.shift_at(5010.0));
    }

    #[test]
    fn translation_camera_curve_is_independent_of_pose_and_keeps_the_final_guard() {
        let mut t = OpticalTranslation::with_curve(vec![(0,[0.5,0.0,2.0]),(1_000_000,[0.5,0.0,2.0])]);
        t.settings.along_axis = false;
        let measured = nalgebra::UnitQuaternion::identity();
        let corrected = nalgebra::UnitQuaternion::from_euler_angles(0.0,0.02,0.0);
        let config = TranslationConfig::DEFAULT;
        let applied = t.camera_shift_at(&corrected,500.0,1080.0,2160.0,false,&config);
        assert_eq!(t.camera_shift_at(&measured,500.0,1080.0,2160.0,false,&config), applied);
        let actual_fraction = applied.xy().norm()*2.0;
        assert!(actual_fraction>0.039 && actual_fraction<=config.max_shift);
        assert_eq!(applied.z,0.0);
    }

    #[test]
    fn larger_lateral_range_keeps_the_original_axial_guard() {
        let mut t = OpticalTranslation::with_curve(vec![(0,[0.5,0.0,0.5]),(1_000_000,[0.5,0.0,0.5])]);
        t.settings.along_axis = true;
        let source = nalgebra::UnitQuaternion::identity();
        let old = TranslationConfig { max_shift: 0.04, ..TranslationConfig::DEFAULT };
        let new = TranslationConfig::DEFAULT;
        let before = t.camera_shift_at(&source,500.0,2160.0,2160.0,false,&old);
        let after = t.camera_shift_at(&source,500.0,2160.0,2160.0,false,&new);
        assert!(after.xy().norm() > before.xy().norm() * 1.9);
        assert!(after.xy().norm() <= new.max_shift);
        assert_eq!(after.z, before.z);
        assert!(after.z.abs() <= 0.04);
        t.settings.along_axis = false;
        assert_eq!(t.camera_shift_at(&source,500.0,2160.0,2160.0,false,&new).z,0.0);
    }

    #[test]
    fn translation_auto_holds_the_auto_layer_and_ignores_the_manual_values() {
        let samples: Vec<_> = walk(1.0, 100.0).into_iter().map(|s| TranslationSample { auto_beta: 0.5, ..s }).collect();
        let auto = built(samples.clone(), OpticalTranslationSettings { reference: 2.0, smoothness_s: 0.2, auto: true, ..Default::default() }, 0.0);
        let sigma = auto.effective_smoothness_s();
        assert!(sigma > 0.2, "the manual smoothness is not used: {sigma}");
        // The same curve as a manual build whose far layer is the automatic one, at the chosen smoothness
        let manual = built(samples.iter().map(|s| TranslationSample { far_beta: 0.5, ..*s }).collect(),
            OpticalTranslationSettings { reference: 1.0, smoothness_s: sigma, ..Default::default() }, 0.0);
        for i in 30..270 {
            let time = i as f64 / 30.0 * 1000.0;
            assert!((auto.shift_at(time) - manual.shift_at(time)).norm() < 1e-12, "{time}");
        }
        assert!(auto.shift_at(5010.0).norm() > 1e-4);
    }

    #[test]
    fn translation_auto_without_auto_depths_holds_the_far_layer() {
        let auto = built(walk(1.0, 100.0), OpticalTranslationSettings { auto: true, ..Default::default() }, 0.0);
        let manual = built(walk(1.0, 100.0), OpticalTranslationSettings { smoothness_s: auto.effective_smoothness_s(), ..Default::default() }, 0.0);
        for time in [1000.0, 3333.0, 5010.0, 8000.0] {
            assert!((auto.shift_at(time) - manual.shift_at(time)).norm() < 1e-12, "{time}");
        }
    }

    #[test]
    fn translation_auto_off_keeps_the_checksum_and_on_changes_it() {
        let mut t = OpticalTranslation::new(walk(1.0, 100.0), Default::default());
        t.applies = true;
        let before = t.checksum();
        t.samples[10].auto_beta = 0.0;
        assert_eq!(t.checksum(), before, "unset automatic depths hash like before");
        t.settings.auto = true;
        assert_ne!(t.checksum(), before);
    }

    /// One minute at 25 fps: a 0.2 Hz sway whose request outgrows the budget as the smoothing gets stronger
    fn slow_sway() -> (Vec<f64>, Vec<nalgebra::Vector3<f64>>, Vec<smoothing::Geometry>) {
        let times: Vec<_> = (0..1500).map(|i| i as f64 / 25.0).collect();
        let positions = times.iter().map(|t| nalgebra::Vector3::new(0.2 * (std::f64::consts::TAU * 0.2 * t).sin() + 0.1 * t, 0.0, 0.0)).collect();
        let geometry = vec![smoothing::Geometry { world_to_camera: nalgebra::Matrix3::identity(), focal_ratio: 1.0 }; times.len()];
        (times, positions, geometry)
    }

    #[test]
    fn translation_auto_smoothness_is_the_strongest_that_fits_the_budget() {
        let (times, positions, geometry) = slow_sway();
        let config = TranslationConfig::DEFAULT;
        let over = |sigma: f64| over_budget_time(&times, &high_pass(&times, &positions, sigma, &|| false).unwrap(), &geometry,
            config.max_shift * 0.5, config.max_axial_shift() * 0.5, true);
        let (sigma, chosen_over) = auto_sigma(&times, &positions, &geometry, None, &config, true, &|| false).unwrap();
        assert!(sigma > 0.3 && sigma < 0.8, "{sigma}");
        assert_eq!(chosen_over, over(sigma));
        assert!(chosen_over <= AUTO_OVER_BUDGET_TIME);
        assert!(over(sigma * 10.0_f64.powf(1.0 / AUTO_SIGMA_STEPS_PER_DECADE)) > AUTO_OVER_BUDGET_TIME, "the next step overruns");
        // Small motion fits at the strongest smoothing, which the track age still caps
        let small: Vec<_> = positions.iter().map(|p| p * 0.01).collect();
        assert_eq!(auto_sigma(&times, &small, &geometry, None, &config, true, &|| false).unwrap().0, AUTO_SIGMA_MAX_S);
        assert_eq!(auto_sigma(&times, &small, &geometry, Some(2.5), &config, true, &|| false).unwrap().0, 2.5);
        assert_eq!(auto_sigma(&times, &small, &geometry, Some(0.05), &config, true, &|| false).unwrap().0, 0.05);
        // Motion too large for any smoothing takes the weakest one
        let large: Vec<_> = positions.iter().map(|p| p * 1000.0).collect();
        assert_eq!(auto_sigma(&times, &large, &geometry, None, &config, true, &|| false).unwrap().0, AUTO_SIGMA_MIN_S);
        assert!(auto_sigma(&times, &positions, &geometry, None, &config, true, &|| true).is_none(), "cancellation");
    }

    #[test]
    fn translation_high_pass_helper_matches_the_manual_curve_before_the_budget() {
        // The request the rebuild constrains is exactly the helper's output for small motion that fits the budget
        let t = built(walk(1.0, 100.0), Default::default(), 0.0);
        let samples = walk(1.0, 100.0);
        let times: Vec<_> = samples.iter().map(|s| time_difference_s(s.timestamp_us, samples[0].timestamp_us)).collect();
        let mut position = nalgebra::Vector3::zeros();
        let positions: Vec<_> = samples.iter().enumerate().map(|(i, s)| {
            if i > 0 { position += nalgebra::Vector3::new(2.0 * s.layer_motion[0] as f64, 2.0 * s.layer_motion[1] as f64, 0.0); }
            position
        }).collect();
        let request = high_pass(&times, &positions, 1.0, &|| false).unwrap();
        for (sample, want) in samples.iter().zip(&request) {
            assert!((t.shift_at(sample.timestamp_us as f64 / 1000.0) - want).norm() < 1e-12);
        }
    }

    #[test]
    fn changing_the_arbitrary_position_scale_does_not_change_compensation() {
        let original = walk(1.0,100.0);
        let scaled:Vec<_> = original.iter().map(|s|TranslationSample {
            position:s.position.map(|v|v*8.0),ref_inv_depth:s.ref_inv_depth/8.0,..*s
        }).collect();
        let a=built(original,Default::default(),2.0);
        let b=built(scaled,Default::default(),2.0);
        for time in [0.0,50.0,300.0,1200.0,5010.0,9200.0] {
            assert_eq!(a.shift_at(time),b.shift_at(time));
        }
    }
}
