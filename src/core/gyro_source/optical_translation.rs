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
}

impl OpticalTranslation {
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
}
