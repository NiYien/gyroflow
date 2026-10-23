// SPDX-License-Identifier: GPL-3.0-or-later

//! The rotation a file with motion data shows in its picture beyond the motion data, the in-camera stabilization
//! reconstruction and the dominant layer of the translation analysis (`synchronization::optical_analysis::
//! residual_rotation`): a world-frame rotation δ(t) on the motion data timeline, composed as `q' = exp(δ(t))·q` after
//! the reconstruction table is built. Its strength was chosen by cross-validation of the tracks and may be zero, in
//! which case it changes nothing.
//!
//! It only applies to what it was measured on: the uncorrected quaternions and the context (`quats_checksum`,
//! `context_checksum`, as the older `OpticalCorrection`), the reconstruction that applied then
//! (`reconstruction_checksum`, zero for none) and the translation measurement whose dominant layer it left out
//! (`translation_checksum`, zero for none)

use nalgebra::{ UnitQuaternion, Vector3 };
use super::{ OpticalTranslation, TimeQuat };

/// Where δ fades in and out at the ends of each analyzed range, microseconds
const FADE_US: f64 = 250_000.0;

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OpticalResidualRotation {
    pub version: u32,
    /// Applied at all ("Optical correction" ticked). Switched off rather than dropped
    pub enabled: bool,
    /// Whether the last `integrate` composed it
    #[serde(skip)]
    pub applies: bool,
    /// Motion data timestamp of the first node and the node spacing, microseconds
    pub start_us: f64,
    pub spacing_us: f64,
    /// δ at the nodes, world-frame rotation vectors, radians: 16-bit integers of a power-of-two scale, already in
    /// memory (`quantize`), so a project saved and loaded again applies exactly the same
    #[serde(with = "quantized")]
    pub d_w: Vec<[f32; 3]>,
    /// The analyzed ranges on the motion data timeline, microseconds: δ fades over `FADE_US` at their ends, zero outside
    pub ranges_us: Vec<(f64, f64)>,
    /// The strength the cross-validation chose (σ = 0: none) and its table, (σ degrees, τ s, mean log ratio)
    pub sigma_deg: f64,
    pub tau_s: f64,
    pub scores: Vec<(f64, f64, f64)>,
    pub quats_checksum: u64,
    pub context_checksum: u64,
    pub reconstruction_checksum: u64,
    pub translation_checksum: u64,
    /// Frame pairs analyzed, and how many of them contributed points
    pub frames: usize,
    pub measured_frames: usize,
    /// Rms of δ where it was measured, degrees
    pub rms_deg: f64,
}

pub const VERSION: u32 = 1;

/// The nodes as they are stored: multiples of a power of two that keeps the largest within an i16
pub fn quantize(d_w: &[[f32; 3]]) -> Vec<[f32; 3]> {
    let scale = quantized::scale(d_w);
    d_w.iter().map(|c| c.map(|v| (v / scale).round().clamp(-32767.0, 32767.0) * scale)).collect()
}

/// Written as `optical_correction::quantized` reads it, with a power-of-two scale
mod quantized {
    use serde::{ Serialize, Serializer };
    #[derive(Serialize)]
    struct Q { scale: f32, v: Vec<i16> }

    /// The power of two that maps the largest magnitude into (16384, 32767]: values already on it map back exactly
    pub fn scale(c: &[[f32; 3]]) -> f32 {
        let max = c.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()));
        if !(max > 0.0) || !max.is_finite() { return 1.0; }
        2f32.powi((max / 32767.0).log2().ceil() as i32)
    }
    pub fn serialize<S: Serializer>(c: &Vec<[f32; 3]>, s: S) -> Result<S::Ok, S::Error> {
        let scale = scale(c);
        Q { scale, v: c.iter().flatten().map(|v| (v / scale).round().clamp(-32767.0, 32767.0) as i16).collect() }.serialize(s)
    }
    pub(super) use crate::gyro_source::optical_correction::quantized::deserialize;
}

impl OpticalResidualRotation {
    /// δ at a motion data timestamp, faded at the ends of the analyzed ranges; zero outside
    pub fn at(&self, timestamp_us: f64) -> Vector3<f64> {
        if self.d_w.is_empty() || self.spacing_us <= 0.0 { return Vector3::zeros(); }
        let fade = self.ranges_us.iter().map(|(a, b)| {
            ((timestamp_us - a) / FADE_US).min((b - timestamp_us) / FADE_US).clamp(0.0, 1.0)
        }).fold(0.0, f64::max);
        if fade <= 0.0 { return Vector3::zeros(); }
        let u = (timestamp_us - self.start_us) / self.spacing_us;
        if u < 0.0 || u > (self.d_w.len() - 1) as f64 { return Vector3::zeros(); }
        let i = (u.floor() as usize).min(self.d_w.len().saturating_sub(2));
        let f = u - i as f64;
        let v = |k: usize| Vector3::new(self.d_w[k][0] as f64, self.d_w[k][1] as f64, self.d_w[k][2] as f64);
        let d = if self.d_w.len() == 1 { v(0) } else { v(i) * (1.0 - f) + v(i + 1) * f };
        d * fade
    }

    /// Whether it was measured on these uncorrected quaternions, context, reconstruction and translation measurement
    pub fn measured_on(&self, quats_checksum: u64, context_checksum: u64, reconstruction_checksum: u64, translation_checksum: u64) -> bool {
        self.version == VERSION && self.quats_checksum == quats_checksum && self.context_checksum == context_checksum
            && self.reconstruction_checksum == reconstruction_checksum && self.translation_checksum == translation_checksum
    }

    pub fn apply(&self, quats: &mut TimeQuat) {
        if self.sigma_deg <= 0.0 { return; }
        for (ts, q) in quats.iter_mut() {
            let d = self.at(*ts as f64);
            if d != Vector3::zeros() {
                *q = UnitQuaternion::from_scaled_axis(d) * *q;
            }
        }
    }

    pub fn hash_into(&self, hasher: &mut impl std::hash::Hasher) {
        hasher.write_u32(self.version);
        hasher.write_u8(self.enabled as u8);
        hasher.write_u64(self.start_us.to_bits());
        hasher.write_u64(self.spacing_us.to_bits());
        hasher.write_u64(self.sigma_deg.to_bits());
        hasher.write_u64(self.tau_s.to_bits());
        hasher.write_u64(self.quats_checksum);
        hasher.write_u64(self.context_checksum);
        hasher.write_u64(self.reconstruction_checksum);
        hasher.write_u64(self.translation_checksum);
        for (a, b) in &self.ranges_us { hasher.write_u64(a.to_bits()); hasher.write_u64(b.to_bits()); }
        hasher.write_usize(self.d_w.len());
        for c in self.d_w.iter().step_by((self.d_w.len() / 512).max(1)) {
            hasher.write_u32(c[0].to_bits()); hasher.write_u32(c[1].to_bits()); hasher.write_u32(c[2].to_bits());
        }
    }
}

/// `translation_checksum` of a translation measurement, see `OpticalTranslation::dominant_layer_fingerprint`: zero
/// without one
pub fn translation_fingerprint(t: Option<&OpticalTranslation>) -> u64 {
    t.map_or(0, OpticalTranslation::dominant_layer_fingerprint)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(sigma_deg: f64) -> OpticalResidualRotation {
        OpticalResidualRotation {
            version: VERSION, enabled: true, start_us: 0.0, spacing_us: 5000.0, sigma_deg, tau_s: 0.1,
            d_w: quantize(&(0..401).map(|k| [1e-3 * (k as f32 * 0.1).sin(), 0.0, 2e-4]).collect::<Vec<_>>()),
            ranges_us: vec![(0.0, 2_000_000.0)], ..Default::default()
        }
    }

    #[test]
    fn fades_at_the_range_ends_and_is_zero_outside() {
        let r = result(0.02);
        assert_eq!(r.at(0.0), Vector3::zeros());
        assert_eq!(r.at(2_000_000.0), Vector3::zeros());
        assert_eq!(r.at(-1000.0), Vector3::zeros());
        assert_eq!(r.at(2_500_000.0), Vector3::zeros());
        let mid = r.at(1_000_000.0);
        assert!((mid.z - 2e-4).abs() < 1e-7, "{mid}");
        let half = r.at(125_000.0);
        assert!((half.z - 1e-4).abs() < 1e-7, "{half}");
        assert_eq!(quantize(&r.d_w), r.d_w, "quantization is idempotent");
    }

    #[test]
    fn zero_strength_leaves_the_quaternions_bit_for_bit() {
        let mut quats = TimeQuat::new();
        for t in 0..2000 { quats.insert(t * 1000, UnitQuaternion::from_euler_angles(0.001 * t as f64, 0.2, -0.1)); }
        let before = quats.clone();
        result(0.0).apply(&mut quats);
        assert_eq!(quats, before);
        result(0.02).apply(&mut quats);
        assert_ne!(quats, before);
    }

    #[test]
    fn composes_in_the_world_frame() {
        let mut quats = TimeQuat::new();
        let q = UnitQuaternion::from_euler_angles(0.3, -0.2, 0.5);
        quats.insert(1_000_000, q);
        let r = result(0.02);
        r.apply(&mut quats);
        let expected = UnitQuaternion::from_scaled_axis(r.at(1_000_000.0)) * q;
        assert!(quats[&1_000_000].angle_to(&expected) < 1e-12);
    }

    #[test]
    fn round_trips_through_the_project_format() {
        let r = result(0.02);
        let s = crate::util::compress_to_base91_cbor(&r).unwrap();
        let back: OpticalResidualRotation = crate::util::decompress_from_base91_cbor(&s).unwrap();
        let s2 = crate::util::compress_to_base91_cbor(&back).unwrap();
        assert_eq!(s, s2);
        assert_eq!(back.d_w, r.d_w);
        assert_eq!(back.ranges_us, r.ranges_us);
        assert_eq!(back.scores, r.scores);
        for t in [0.0, 250_000.0, 777_777.0, 1_999_000.0] { assert_eq!(back.at(t), r.at(t)); }
    }
}
