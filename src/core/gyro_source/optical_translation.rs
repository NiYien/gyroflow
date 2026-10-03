// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::OnceLock;

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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TranslationConfig {
    pub track_age_k: f64,
    pub max_shift: f64,
    pub per_row: bool,
}

impl TranslationConfig {
    pub const DEFAULT: Self = Self { track_age_k: 2.0, max_shift: 0.04, per_row: true };

    pub fn resolved() -> Self {
        static RESOLVED: OnceLock<TranslationConfig> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let config = Self {
                track_age_k: resolve_number("GYROFLOW_TRANSLATION_TRACK_AGE_K", Self::DEFAULT.track_age_k, |v| v >= 0.0),
                max_shift: resolve_number("GYROFLOW_TRANSLATION_MAX_SHIFT", Self::DEFAULT.max_shift, |v| v > 0.0 && v <= 0.5),
                per_row: resolve_per_row(),
            };
            log::info!(target: "lifecycle", "translation_config resolved track_age_k={} max_shift={} per_row={}", config.track_age_k, config.max_shift, config.per_row);
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
    #[serde(skip)]
    pub applies: bool,
    #[serde(skip)]
    curve: Vec<TranslationCurvePoint>,
    #[serde(skip)]
    effective_smoothness_s: f64,
}

#[derive(Clone, Debug)]
struct TranslationCurvePoint {
    timestamp_us: i64,
    segment: u32,
    shift: nalgebra::Vector3<f64>,
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

impl OpticalTranslation {
    /// Builds the curve without deciding whether its analysis context is current.
    pub fn new(samples: Vec<TranslationSample>, settings: OpticalTranslationSettings) -> Self {
        let mut result = Self { enabled: true, samples, settings, ..Default::default() };
        result.rebuild();
        result
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
        self.curve.clear();
        self.effective_smoothness_s = 0.0;
        let mut samples = self.samples.clone();
        samples.sort_by_key(|sample| sample.timestamp_us);
        samples.dedup_by_key(|sample| sample.timestamp_us);
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
        let mut start = 0;
        while start < samples.len() {
            let mut end = start + 1;
            while end < samples.len() && samples[end].segment == samples[start].segment { end += 1; }
            let segment = &samples[start..end];
            if segment.len() == 1 {
                self.curve.push(TranslationCurvePoint { timestamp_us: segment[0].timestamp_us, segment: segment[0].segment, shift: nalgebra::Vector3::zeros() });
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
            let last = segment.len() - 1;
            for i in 0..segment.len() {
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
                let confidence_sigma = 0.25;
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
                let left_interval = time_difference_s(segment[1].timestamp_us, segment[0].timestamp_us);
                let right_interval = time_difference_s(segment[last].timestamp_us, segment[last - 1].timestamp_us);
                let mut distance = times[i] + left_interval;
                while distance <= confidence_radius {
                    confidence_weight_sum += (-0.5 * (distance / confidence_sigma).powi(2)).exp();
                    distance += left_interval;
                }
                let mut distance = times[last] - times[i] + right_interval;
                while distance <= confidence_radius {
                    confidence_weight_sum += (-0.5 * (distance / confidence_sigma).powi(2)).exp();
                    distance += right_interval;
                }
                let ramp = 1.0_f64.min(times[i] / confidence_sigma).min((times[last] - times[i]) / confidence_sigma).max(0.0);
                let confidence = confidence_sum / confidence_weight_sum * ramp;
                let shift = (positions[i] - sum / weight_sum) * (reference * confidence * segment[i].ref_inv_depth as f64);
                self.curve.push(TranslationCurvePoint { timestamp_us: segment[i].timestamp_us, segment: segment[i].segment, shift });
            }
            start = end;
        }
        self.effective_smoothness_s = median(&mut smoothness);
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

    #[cfg(test)]
    pub(crate) fn with_curve(mut points: Vec<(i64, [f64; 3])>) -> Self {
        points.sort_by_key(|point| point.0);
        points.dedup_by_key(|point| point.0);
        let samples = points.iter().map(|point| TranslationSample { timestamp_us: point.0, ..Default::default() }).collect();
        let curve = points.into_iter().map(|(timestamp_us, shift)| TranslationCurvePoint { timestamp_us, segment: 0, shift: nalgebra::Vector3::from(shift) }).collect();
        Self { enabled: true, applies: true, samples, curve, ..Default::default() }
    }

    pub fn is_active(&self) -> bool {
        self.enabled && self.applies && !self.samples.is_empty()
    }

    pub fn hash_into(&self, hasher: &mut impl Hasher) {
        self.enabled.hash(hasher);
        self.settings.reference.to_bits().hash(hasher);
        self.settings.smoothness_s.to_bits().hash(hasher);
        self.settings.along_axis.hash(hasher);
        self.quats_checksum.hash(hasher);
        self.context_checksum.hash(hasher);
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
        }
    }

    pub fn checksum(&self) -> u64 {
        if !self.is_active() { return 0; }
        let mut hasher = DefaultHasher::new();
        self.hash_into(&mut hasher);
        hasher.finish().max(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(i: i64) -> TranslationSample {
        TranslationSample { timestamp_us: i * 33_333, position: [i as f32 * 0.01, 0.0, 0.0], ref_inv_depth: 2.0, confidence: 1.0, track_age_s: 5.0, segment: 0 }
    }

    #[test]
    fn settings_default_to_the_spec_values() {
        assert_eq!(OpticalTranslationSettings::default(), OpticalTranslationSettings { reference: 1.0, smoothness_s: 1.0, along_axis: false });
        assert_eq!(TranslationConfig::DEFAULT, TranslationConfig { track_age_k: 2.0, max_shift: 0.04, per_row: true });
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
        let mut t = OpticalTranslation { enabled: true, applies: true, samples: (0..90).map(sample).collect(), ..Default::default() };
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
                ref_inv_depth: 2.0, confidence, track_age_s, segment: 0 }
        }).collect()
    }
    fn built(samples: Vec<TranslationSample>, settings: OpticalTranslationSettings, k: f64) -> OpticalTranslation {
        let mut t = OpticalTranslation { enabled: true, applies: true, samples, settings, ..Default::default() };
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
        samples.extend((120..210).map(|i| TranslationSample { timestamp_us: (i as f64 / 30.0 * 1e6) as i64, position: [i as f32 * 3.0, 50.0, 0.0], ref_inv_depth: 0.01, confidence: 1.0, track_age_s: 100.0, segment: 1 }));
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
            ref_inv_depth: 1.0, confidence: 1.0, track_age_s: 100.0, segment: 0,
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
}
