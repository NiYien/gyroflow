// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::{hash_map::DefaultHasher, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

#[path = "optical_translation/smoothing.rs"]
mod smoothing;
#[cfg(all(test, feature = "use-opencv"))]
#[path = "optical_translation/acceptance.rs"]
mod acceptance;
pub const GEOMETRY_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OpticalTranslationSettings {
    pub reference: f64,
    pub smoothness_s: f64,
    pub along_axis: bool,
}

impl Default for OpticalTranslationSettings {
    fn default() -> Self {
        Self { reference: 1.0, smoothness_s: 1.0, along_axis: false }
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
}

impl TranslationSample {
    fn geometry(&self) -> Option<smoothing::Geometry> {
        let [w, x, y, z] = self.camera_to_world.map(f64::from);
        let q = nalgebra::Quaternion::new(w, x, y, z);
        let norm = q.norm_squared();
        let focal_ratio = self.focal_length_over_short_side as f64;
        if !norm.is_finite() || (norm - 1.0).abs() > 1e-3 || !focal_ratio.is_finite() || focal_ratio <= 0.0 { return None; }
        let rotation = nalgebra::UnitQuaternion::new_normalize(q);
        Some(smoothing::Geometry { world_to_camera: rotation.inverse().to_rotation_matrix().into_inner(), focal_ratio })
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
}

#[derive(Clone, Debug)]
struct TranslationCurvePoint {
    timestamp_us: i64,
    segment: u32,
    shift: nalgebra::Vector3<f64>,
}

const CONFIDENCE_SIGMA_US: i128 = 250_000;
const CONFIDENCE_RADIUS_US: i128 = 3 * CONFIDENCE_SIGMA_US;

const ZERO_CONFIDENCE_CACHE_BYTES: usize = 32 * 1024 * 1024;
const ZERO_CONFIDENCE_CACHE_GRIDS: usize = 2048;

struct ZeroConfidenceCache {
    suffixes: HashMap<(i128, i128), Vec<f64>>,
    payload_bytes: usize,
    payload_limit: usize,
    grid_limit: usize,
    #[cfg(test)]
    evaluated_weights: usize,
}

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

fn filtered_inverse_depth(segment: &[TranslationSample]) -> Vec<f64> {
    let n = segment.len();
    let times: Vec<_> = segment.iter().map(|s| time_difference_s(s.timestamp_us, segment[0].timestamp_us)).collect();
    let mut output = vec![0.0; n];
    for i in 0..n {
        // Invalid depth remains unavailable; nearby valid samples must not invent it.
        if !segment[i].ref_inv_depth.is_finite() || segment[i].ref_inv_depth <= 0.0 { continue; }
        let left = times.partition_point(|t| *t < times[i] - 0.75);
        let right = times.partition_point(|t| *t <= times[i] + 0.75);
        let (mut sum, mut weights) = (0.0, 0.0);
        for j in left..right {
            let s = &segment[j];
            if !s.ref_inv_depth.is_finite() || s.ref_inv_depth <= 0.0 || !s.confidence.is_finite() || s.confidence <= 0.0 { continue; }
            let duration = if n == 1 { 1.0 } else {
                (times[(j + 1).min(n - 1)] - times[j.saturating_sub(1)]) * 0.5
            };
            let weight = (-0.5 * ((times[j] - times[i]) / 0.25).powi(2)).exp() * s.confidence.clamp(0.0, 1.0) as f64 * duration;
            sum += weight * s.ref_inv_depth as f64;
            weights += weight;
        }
        if weights > 0.0 { output[i] = sum / weights; }
    }
    output
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
            return true;
        };
        let mut previous_position = [0.0; 3];
        for sample in &mut samples {
            if sample.position.iter().all(|value| value.is_finite()) && sample.ref_inv_depth.is_finite() && sample.confidence.is_finite() {
                previous_position = sample.position;
                sample.confidence = sample.confidence.clamp(0.0, 1.0);
            } else {
                sample.position = previous_position;
                sample.ref_inv_depth = 0.0;
                sample.confidence = 0.0;
            }
        }
        let requested_sigma = if self.settings.smoothness_s.is_finite() { self.settings.smoothness_s } else { OpticalTranslationSettings::default().smoothness_s };
        let reference = if self.settings.reference.is_finite() { self.settings.reference } else { 0.0 };
        let mut smoothness = Vec::new();
        let mut zero_confidence = ZeroConfidenceCache::default();
        let (mut solved_segments, mut fallback_segments, mut iterations) = (0, 0, 0);
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
            let sigma = if track_age_k.is_finite() && track_age_k > 0.0 { requested_sigma.min(track_age_k * median(&mut ages)) } else { requested_sigma }.max(0.001);
            smoothness.push(sigma);
            let times: Vec<_> = segment.iter().map(|sample| time_difference_s(sample.timestamp_us, segment[0].timestamp_us)).collect();
            let positions: Vec<_> = segment.iter().map(|sample| nalgebra::Vector3::new(sample.position[0] as f64, sample.position[1] as f64, sample.position[2] as f64)).collect();
            let depths = filtered_inverse_depth(segment);
            let mut fixed = vec![false; segment.len()];
            let curve_start = curve.len();
            let last = segment.len() - 1;
            let left_interval_us = segment[1].timestamp_us as i128 - segment[0].timestamp_us as i128;
            let right_interval_us = segment[last].timestamp_us as i128 - segment[last - 1].timestamp_us as i128;
            for i in 0..segment.len() {
                if cancelled() { return false; }
                if i == 0 || i == last {
                    fixed[i] = true;
                    curve.push(TranslationCurvePoint { timestamp_us: segment[i].timestamp_us, segment: segment[i].segment, shift: nalgebra::Vector3::zeros() });
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
                let confidence_sigma = CONFIDENCE_SIGMA_US as f64 / 1e6;
                let confidence_radius = 3.0 * confidence_sigma;
                let confidence_left = times.partition_point(|time| *time < times[i] - confidence_radius);
                let confidence_right = times.partition_point(|time| *time <= times[i] + confidence_radius);
                let mut confidence_sum = 0.0;
                let mut confidence_weight_sum = 0.0;
                for j in confidence_left..confidence_right {
                    let weight = (-0.5 * ((times[j] - times[i]) / confidence_sigma).powi(2)).exp();
                    confidence_sum += segment[j].confidence as f64 * weight;
                    confidence_weight_sum += weight;
                }
                // Continue the nearest endpoint interval with zero confidence outside the segment.
                let left_distance_us = segment[i].timestamp_us as i128 - segment[0].timestamp_us as i128 + left_interval_us;
                let right_distance_us = segment[last].timestamp_us as i128 - segment[i].timestamp_us as i128 + right_interval_us;
                confidence_weight_sum += zero_confidence.sum(left_distance_us, left_interval_us);
                confidence_weight_sum += zero_confidence.sum(right_distance_us, right_interval_us);
                let ramp = 1.0_f64.min(times[i] / confidence_sigma).min((times[last] - times[i]) / confidence_sigma).max(0.0);
                let confidence = confidence_sum / confidence_weight_sum * ramp;
                let gain = reference * confidence * depths[i];
                fixed[i] = gain == 0.0;
                let shift = (positions[i] - sum / weight_sum) * gain;
                curve.push(TranslationCurvePoint { timestamp_us: segment[i].timestamp_us, segment: segment[i].segment, shift });
            }
            let request: Vec<_> = curve[curve_start..].iter().map(|p| p.shift).collect();
            let config = TranslationConfig::resolved();
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
        self.effective_smoothness_s = median(&mut smoothness);
        log::debug!(target: "stab.translation", "translation rebuild samples={} corrected_segments={} fallback_segments={} iterations={} elapsed_ms={:.3}",
            samples.len(), solved_segments, fallback_segments, iterations, began.elapsed().as_secs_f64() * 1000.0);
        true
    }

    /// Returns world-frame displacement over depth, including reference and confidence.
    /// Returns zero outside the analysed range and between separate segments.
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
    pub(crate) fn camera_shift_at(&self, source: &nalgebra::UnitQuaternion<f64>, timestamp_ms: f64,
        short_side: f64, focal_px: f64, inverted: bool, config: &TranslationConfig) -> nalgebra::Vector3<f64> {
        let shift = self.shift_at(timestamp_ms);
        let t_quat = -(source.inverse() * shift);
        let mut t = nalgebra::Vector3::new(t_quat.x, if inverted { t_quat.y } else { -t_quat.y }, -t_quat.z);
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

    pub fn is_active(&self) -> bool {
        self.enabled && self.applies && self.geometry_valid && !self.samples.is_empty()
    }

    pub fn hash_into(&self, hasher: &mut impl Hasher) {
        self.enabled.hash(hasher);
        self.settings.reference.to_bits().hash(hasher);
        self.settings.smoothness_s.to_bits().hash(hasher);
        self.settings.along_axis.hash(hasher);
        self.quats_checksum.hash(hasher);
        self.context_checksum.hash(hasher);
        self.geometry_version.hash(hasher);
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
        TranslationSample { timestamp_us: i * 33_333, position: [i as f32 * 0.01, 0.0, 0.0], ref_inv_depth: 2.0, confidence: 1.0, track_age_s: 5.0, segment: 0, ..geometry_sample() }
    }

    #[test]
    fn settings_default_to_the_spec_values() {
        assert_eq!(OpticalTranslationSettings::default(), OpticalTranslationSettings { reference: 1.0, smoothness_s: 1.0, along_axis: false });
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
            TranslationSample { timestamp_us: (t * 1e6) as i64, position: [(0.02 * (std::f64::consts::TAU * 3.0 * t).sin() + 0.5 * t) as f32, 0.0, 0.0],
                ref_inv_depth: 2.0, confidence, track_age_s, segment: 0, ..geometry_sample() }
        }).collect()
    }
    fn built(samples: Vec<TranslationSample>, settings: OpticalTranslationSettings, k: f64) -> OpticalTranslation {
        let mut t = OpticalTranslation { enabled: true, applies: true, samples, settings, geometry_version: GEOMETRY_VERSION, ..Default::default() };
        t.rebuild_with(k);
        t
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
        samples[100].position = [f32::NAN, 0.0, 0.0];
        samples[150].position = [f32::INFINITY, 0.0, 0.0];
        samples[200].ref_inv_depth = f32::NAN;
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
    fn confidence_uses_zero_extension_at_segment_boundaries() {
        let samples = (0..3).map(|i| TranslationSample {
            timestamp_us: i * 100_000,
            position: [if i == 1 { 1.0 } else { 0.0 }, 0.0, 0.0],
            ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0, ..geometry_sample()
        }).collect();
        let t = built(samples, OpticalTranslationSettings { smoothness_s: 0.1, ..Default::default() }, 0.0);
        // Three real position samples plus their point reflections give this residual.
        let residual = 1.0 - (1.0 - 2.0 * (-2.0_f64).exp()) /
            (1.0 + 2.0 * (-0.5_f64).exp() + 2.0 * (-2.0_f64).exp() + 2.0 * (-4.5_f64).exp());
        // Confidence has three nonzero samples among fifteen samples within its time window.
        let denominator: f64 = (-7..=7).map(|i| (-0.5 * (i as f64 * 0.1 / 0.25).powi(2)).exp()).sum();
        let confidence = (1.0 + 2.0 * (-0.08_f64).exp()) / denominator;
        let got = t.shift_at(100.0).x;
        let want = residual * confidence * 0.4;
        assert!((got - want).abs() < 1e-12, "got {got} want {want}");
        assert!(got < residual * 0.4, "outside confidence must remain zero");
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
        let samples = vec![
            TranslationSample { timestamp_us: 0, position: [0.0; 3], ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0, ..geometry_sample() },
            TranslationSample { timestamp_us: 1, position: [1.0, 0.0, 0.0], ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0, ..geometry_sample() },
            TranslationSample { timestamp_us: 1_000_000, position: [0.0; 3], ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0, ..geometry_sample() },
        ];
        let t = built(samples, OpticalTranslationSettings { smoothness_s: 0.001, ..Default::default() }, 0.0);
        let g1 = (-0.5 * (1.0_f64 / 1000.0).powi(2)).exp();
        let g2 = (-0.5 * (2.0_f64 / 1000.0).powi(2)).exp();
        let residual = 1.0 - (1.0 - g2) / (1.0 + g1 + g2);
        let real_weight = 1.0 + (-0.5 * (1.0_f64 / 250_000.0).powi(2)).exp();
        let denominator = real_weight + naive_zero_confidence_weight_sum(2, 1);
        let want = residual * real_weight / denominator * (1.0 / 250_000.0);
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
        samples[20].ref_inv_depth = 0.0;
        samples[21].ref_inv_depth = f32::NAN;
        samples[22].ref_inv_depth = -1.0;
        samples[23].ref_inv_depth = 1000.0;
        samples[23].confidence = 0.0;
        let depths = filtered_inverse_depth(&samples);
        for (i, depth) in depths.iter().enumerate() {
            if (20..=22).contains(&i) { assert_eq!(*depth, 0.0); }
            else { assert!((*depth - 2.0).abs() < 1e-12, "sample {i}: {depth}"); }
        }
        for sample in &mut samples { sample.confidence = 0.0; }
        assert!(filtered_inverse_depth(&samples).iter().all(|rho| *rho == 0.0));
    }

    #[test]
    fn depth_filter_respects_time_density_and_arbitrary_depth_scale() {
        let make = |times: Vec<f64>| times.into_iter().map(|t| TranslationSample {
            timestamp_us: (t*1e6).round() as i64, ref_inv_depth: (2.0 + 0.2*t) as f32,
            ..sample(0)
        }).collect::<Vec<_>>();
        let regular = make((0..101).map(|i| i as f64 * 0.02).collect());
        let mut times: Vec<_> = (0..101).map(|i| i as f64 * 0.02).collect();
        times.extend((0..100).map(|i| i as f64 * 0.02 + 0.01).filter(|t| *t > 1.0));
        times.sort_by(f64::total_cmp);
        let irregular = make(times);
        let regular_depth = filtered_inverse_depth(&regular)[50];
        let index = irregular.iter().position(|s| s.timestamp_us == 1_000_000).unwrap();
        let irregular_depth = filtered_inverse_depth(&irregular)[index];
        assert!((regular_depth-irregular_depth).abs() < 0.0001);
        let scaled: Vec<_> = irregular.iter().map(|s| TranslationSample { ref_inv_depth: s.ref_inv_depth * 8.0, ..*s }).collect();
        assert!((filtered_inverse_depth(&scaled)[index] - 8.0*irregular_depth).abs() < 1e-12);
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
        t.samples[30].camera_to_world = [0.0;4];
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
    fn live_pose_difference_keeps_the_final_guard_even_with_large_forward_motion() {
        let t = OpticalTranslation::with_curve(vec![(0,[0.0,0.0,2.0]),(1_000_000,[0.0,0.0,2.0])]);
        let measured = nalgebra::UnitQuaternion::identity();
        let corrected = nalgebra::UnitQuaternion::from_euler_angles(0.0,0.02,0.0);
        let config = TranslationConfig::DEFAULT;
        assert_eq!(t.camera_shift_at(&measured,500.0,1080.0,2160.0,false,&config).norm(),0.0);
        let applied = t.camera_shift_at(&corrected,500.0,1080.0,2160.0,false,&config);
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
