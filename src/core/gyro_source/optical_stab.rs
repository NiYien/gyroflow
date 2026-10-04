// SPDX-License-Identifier: GPL-3.0-or-later

use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;
use std::sync::{Arc, OnceLock};
use nalgebra::Vector3;
use super::{GyroSource, Quat64, TimeQuat};
use super::optical_correction::bspline_weights;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StabReconConfig {
    pub cutoff_hz: Option<f64>,
    pub max_deg: f64,
}

impl StabReconConfig {
    pub const DEFAULT: Self = Self { cutoff_hz: None, max_deg: 3.0 };

    pub fn resolved() -> Self {
        static RESOLVED: OnceLock<StabReconConfig> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let config = Self {
                cutoff_hz: resolve_number("GYROFLOW_STAB_RECON_CUTOFF_HZ", None, 50.0),
                max_deg: resolve_number("GYROFLOW_STAB_RECON_MAX_DEG", Some(Self::DEFAULT.max_deg), 30.0).unwrap(),
            };
            log::info!(target: "lifecycle", "stab_recon_config resolved cutoff_hz={:?} max_deg={}", config.cutoff_hz, config.max_deg);
            config
        })
    }
}

fn resolve_number(name: &str, default: Option<f64>, upper: f64) -> Option<f64> {
    match std::env::var(name) {
        Ok(raw) if !raw.is_empty() => {
            if let Ok(value) = raw.trim().parse::<f64>() {
                if value.is_finite() && value > 0.0 && value <= upper { return Some(value); }
            }
            log::warn!(target: "lifecycle", "{}={} invalid, falling back to {:?}", name, raw, default);
            default
        }
        _ => default,
    }
}

/// Logarithmic candidates: 0.05 * 40^(i/15), i = 0..16.
pub const CUTOFF_GRID_HZ: [f64; 16] = [
    0.05, 0.06394020196998705, 0.08176698855925471, 0.10456395525912732,
    0.13371680836098582, 0.1709975946676697, 0.21867241478865562, 0.2796391673370285,
    0.35760369676497206, 0.4573050519273263, 0.5848035476425731, 0.7478491389806213,
    0.9563524997900372, 1.2229874398215395, 1.563961278178932, 2.0,
];
pub const DEFAULT_CUTOFF_HZ: f64 = 0.3;

fn finite_vector(value: Vector3<f64>) -> Vector3<f64> {
    value.map(|component| if component.is_finite() { component } else { 0.0 })
}

/// High-frequency body rotation, expressed as normalized horizontal shift, vertical shift and roll.
pub fn prior(quats: &TimeQuat, cutoff_hz: f64, times_us: &[f64]) -> Vec<Vector3<f64>> {
    let zeros = || vec![Vector3::zeros(); times_us.len()];
    if quats.is_empty() || !cutoff_hz.is_finite() || cutoff_hz <= 0.0 { return zeros(); }
    let first = times_us.iter().copied().filter(|t| t.is_finite()).fold(f64::INFINITY, f64::min);
    let last = times_us.iter().copied().filter(|t| t.is_finite()).fold(f64::NEG_INFINITY, f64::max);
    let sigma_us = 0.1325 / cutoff_hz * 1e6;
    let step_us = sigma_us / 8.0;
    let start = first - 3.0 * sigma_us;
    let end = last + 3.0 * sigma_us;
    let count = ((end - start) / step_us).ceil() + 1.0;
    if !start.is_finite() || !end.is_finite() || !step_us.is_finite() || step_us <= 0.0 ||
        start + step_us == start || !count.is_finite() || count < 1.0 || count >= usize::MAX as f64 {
        return zeros();
    }
    let count = count as usize;
    let mut grid = Vec::new();
    if grid.try_reserve_exact(count).is_err() { return zeros(); }
    for i in 0..count {
        grid.push(GyroSource::clamped_quat_at_gyro_timestamp(quats, (start + i as f64 * step_us) / 1000.0));
    }
    let weights: Vec<_> = (-24..=24).map(|j| (-0.5 * (j as f64 / 8.0).powi(2)).exp()).collect();
    let mut lowpass = Vec::new();
    if lowpass.try_reserve_exact(count).is_err() { return zeros(); }
    for (i, q) in grid.iter().enumerate() {
        let mut sum = Vector3::zeros();
        let mut weight_sum = 0.0;
        for (j, weight) in weights.iter().enumerate() {
            let index = i as i128 + j as i128 - 24;
            if index >= 0 && index < count as i128 {
                sum += finite_vector((q.inverse() * grid[index as usize]).scaled_axis()) * *weight;
                weight_sum += *weight;
            }
        }
        lowpass.push(*q * Quat64::from_scaled_axis(sum / weight_sum));
    }
    times_us.iter().map(|time| {
        if !time.is_finite() { return Vector3::zeros(); }
        let coordinate = ((*time - start) / step_us).clamp(0.0, (count - 1) as f64);
        let left = coordinate.floor() as usize;
        let right = (left + 1).min(count - 1);
        let lp = lowpass[left].slerp(&lowpass[right], coordinate - left as f64);
        let q = GyroSource::clamped_quat_at_gyro_timestamp(quats, *time / 1000.0);
        let h = finite_vector((lp.inverse() * q).scaled_axis());
        Vector3::new(h.y, h.x, h.z)
    }).collect()
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct OpticalStabReconstruction {
    pub enabled: bool,
    pub start_us: f64,
    pub spacing_us: f64,
    /// Control points of u = s - s0, with normalized shifts and roll in radians.
    #[serde(with = "super::optical_correction::quantized")]
    pub coeffs: Vec<[f32; 3]>,
    pub cutoff_hz: f64,
    pub quats_checksum: u64,
    pub context_checksum: u64,
    pub frames: usize,
    pub measured_frames: usize,
    pub max_deg: f64,
    #[serde(skip)]
    pub applies: bool,
    #[serde(skip)]
    table: Arc<Vec<(i64, [f64; 3])>>,
}

impl OpticalStabReconstruction {
    pub fn is_active(&self) -> bool {
        self.enabled && self.applies && !self.table.is_empty()
    }

    /// Rebuilds s = u + s0 over the spline support without deciding whether its analysis is current.
    pub fn rebuild(&mut self, quats: &TimeQuat, config: &StabReconConfig) {
        self.table = Arc::default();
        self.max_deg = 0.0;
        if self.coeffs.len() < 3 || !self.start_us.is_finite() || !self.spacing_us.is_finite() || self.spacing_us <= 0.0 {
            return;
        }
        let start = (self.start_us + self.spacing_us).ceil();
        let end = (self.start_us + (self.coeffs.len() - 2) as f64 * self.spacing_us).floor();
        if !start.is_finite() || !end.is_finite() || start < i64::MIN as f64 || end >= i64::MAX as f64 || end < start {
            return;
        }
        let start = start as i64;
        let end = end as i64;
        let count = (end as i128 - start as i128) / 1000 + 1;
        let Ok(count) = usize::try_from(count) else { return };
        let mut times = Vec::new();
        let Some(capacity) = count.checked_add(1) else { return };
        if times.try_reserve_exact(capacity).is_err() { return; }
        for i in 0..count { times.push((start as i128 + i as i128 * 1000) as f64); }
        if times.last().copied() != Some(end as f64) { times.push(end as f64); }
        let s0 = prior(quats, self.cutoff_hz, &times);
        let limit = if config.max_deg.is_finite() && config.max_deg > 0.0 {
            config.max_deg.to_radians()
        } else { StabReconConfig::DEFAULT.max_deg.to_radians() };
        let soft = |value: f64| {
            let half = limit / 2.0;
            if value <= half { value } else { half + half * ((value - half) / half).tanh() }
        };
        let mut table = Vec::new();
        if table.try_reserve_exact(times.len()).is_err() { return; }
        for (time, prior) in times.into_iter().zip(s0) {
            let mut s = finite_vector(self.u_at(time) + prior);
            let norm = s.xy().norm();
            if norm > 0.0 {
                let scale = soft(norm) / norm;
                s.x *= scale;
                s.y *= scale;
            }
            s.z = s.z.signum() * soft(s.z.abs());
            self.max_deg = self.max_deg.max(s.xy().norm().max(s.z.abs()).to_degrees());
            table.push((time as i64, s.into()));
        }
        self.table = Arc::new(table);
    }

    /// Evaluates the stored spline u independently of the prior and activation state.
    pub fn u_at(&self, t_us: f64) -> Vector3<f64> {
        if !t_us.is_finite() || !self.start_us.is_finite() || !self.spacing_us.is_finite() || self.spacing_us <= 0.0 || self.coeffs.is_empty() {
            return Vector3::zeros();
        }
        let coordinate = (t_us - self.start_us) / self.spacing_us;
        if !coordinate.is_finite() || coordinate < -2.0 || coordinate >= self.coeffs.len() as f64 + 1.0 {
            return Vector3::zeros();
        }
        let (segment, weights) = bspline_weights(coordinate);
        let mut value = Vector3::zeros();
        for (i, weight) in weights.iter().enumerate() {
            let index = segment + i as i64 - 1;
            if index >= 0 && (index as usize) < self.coeffs.len() {
                let coefficient = self.coeffs[index as usize];
                value += finite_vector(Vector3::new(coefficient[0] as f64, coefficient[1] as f64, coefficient[2] as f64)) * *weight;
            }
        }
        finite_vector(value)
    }

    /// Interpolates normalized s in gyro microseconds and returns zero outside the cached range.
    pub fn at(&self, t_us: f64) -> Vector3<f64> {
        if !t_us.is_finite() { return Vector3::zeros(); }
        let right = self.table.partition_point(|point| point.0 as f64 <= t_us);
        if right == 0 { return Vector3::zeros(); }
        let left = &self.table[right - 1];
        if t_us == left.0 as f64 { return Vector3::from(left.1); }
        if right == self.table.len() { return Vector3::zeros(); }
        let next = &self.table[right];
        let fraction = (t_us - left.0 as f64) / (next.0 as i128 - left.0 as i128) as f64;
        Vector3::from(left.1) * (1.0 - fraction) + Vector3::from(next.1) * fraction
    }

    pub fn measured_on(&self, quats_checksum: u64, context_checksum: u64) -> bool {
        !self.coeffs.is_empty() && self.quats_checksum == quats_checksum && self.context_checksum == context_checksum
    }

    pub fn hash_into(&self, hasher: &mut impl Hasher) {
        hasher.write_u8(self.enabled as u8);
        hasher.write_u64(self.cutoff_hz.to_bits());
        hasher.write_u64(self.quats_checksum);
        hasher.write_u64(self.context_checksum);
        hasher.write_u64(self.start_us.to_bits());
        hasher.write_u64(self.spacing_us.to_bits());
        hasher.write_usize(self.coeffs.len());
        for c in &self.coeffs {
            for v in c { hasher.write_u32(v.to_bits()); }
        }
    }

    pub fn checksum(&self) -> u64 {
        if !self.is_active() { return 0; }
        let mut hasher = DefaultHasher::new();
        self.hash_into(&mut hasher);
        hasher.finish().max(1)
    }

    /// Installs the given table directly; rebuilding still uses only the stored spline and body rotations.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_samples(samples: Vec<(i64, [f64; 3])>) -> Self {
        let mut hasher = DefaultHasher::new();
        hasher.write_usize(samples.len());
        for (time, values) in &samples {
            hasher.write_i64(*time);
            for value in values { hasher.write_u64(value.to_bits()); }
        }
        Self { enabled: true, applies: true, context_checksum: hasher.finish(), table: Arc::new(samples), ..Default::default() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gyro_source::Quat64;
    use nalgebra::Vector3;

    fn rotations(axis: usize, hz: f64) -> TimeQuat {
        (0..=20_000).map(|i| {
            let mut v = Vector3::zeros();
            v[axis] = 0.5_f64.to_radians() * (std::f64::consts::TAU * hz * i as f64 / 1000.0).sin();
            (i * 1000, Quat64::from_scaled_axis(v))
        }).collect()
    }

    fn middle_times() -> Vec<f64> { (5000..15_000).map(|i| i as f64 * 1000.0).collect() }

    fn reconstruction(degrees: f64) -> OpticalStabReconstruction {
        OpticalStabReconstruction {
            enabled: true, applies: true, start_us: 0.0, spacing_us: 1000.0,
            coeffs: vec![[degrees.to_radians() as f32, 0.0, degrees.to_radians() as f32]; 12],
            cutoff_hz: 0.5, ..Default::default()
        }
    }

    #[test]
    fn prior_cancels_fast_rotation_and_keeps_slow() {
        let amplitude = 0.5_f64.to_radians();
        let times = middle_times();
        let fast = prior(&rotations(1, 4.0), 0.5, &times).iter().map(|v| v.x.abs()).fold(0.0, f64::max);
        let slow = prior(&rotations(1, 0.1), 0.5, &times).iter().map(|v| v.x.abs()).fold(0.0, f64::max);
        assert!(fast >= 0.9 * amplitude, "fast={fast} amplitude={amplitude}");
        assert!(slow <= 0.1 * amplitude, "slow={slow} amplitude={amplitude}");
    }

    #[test]
    fn prior_follows_the_axes() {
        for (axis, output) in [(0, 1), (1, 0), (2, 2)] {
            let values = prior(&rotations(axis, 4.0), 0.5, &middle_times());
            let peak = values.iter().map(|v| v[output].abs()).fold(0.0, f64::max);
            assert!(peak >= 0.9 * 0.5_f64.to_radians());
            for other in (0..3).filter(|i| *i != output) {
                assert!(values.iter().all(|v| v[other].abs() <= 0.01 * peak));
            }
        }
    }

    #[test]
    fn round_trip_keeps_what_is_stored() {
        let mut value = reconstruction(1.0);
        value.start_us = 123.0;
        value.spacing_us = 2800.0;
        value.quats_checksum = 42;
        value.context_checksum = 7;
        value.frames = 90;
        value.measured_frames = 88;
        value.max_deg = 1.2;
        value.coeffs[4] = [0.0123, -0.0052, 0.0081];
        let encoded = crate::util::compress_to_base91_cbor(&value).unwrap();
        let mut decoded: OpticalStabReconstruction = crate::util::decompress_from_base91_cbor(&encoded).unwrap();
        assert_eq!((decoded.enabled, decoded.start_us, decoded.spacing_us, decoded.cutoff_hz, decoded.quats_checksum,
                    decoded.context_checksum, decoded.frames, decoded.measured_frames, decoded.max_deg),
                   (value.enabled, value.start_us, value.spacing_us, value.cutoff_hz, value.quats_checksum,
                    value.context_checksum, value.frames, value.measured_frames, value.max_deg));
        assert_eq!(decoded.coeffs.len(), value.coeffs.len());
        let step = value.coeffs.iter().flatten().fold(0.0_f32, |m, v| m.max(v.abs())) / i16::MAX as f32;
        for (actual, expected) in decoded.coeffs.iter().flatten().zip(value.coeffs.iter().flatten()) {
            assert!((actual - expected).abs() <= step);
        }
        assert!(!decoded.applies);
        assert_eq!(decoded.at(10_000.0), Vector3::zeros());
        decoded.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert!(decoded.at(10_000.0).norm() > 0.0);
    }

    #[test]
    fn checksum_is_zero_unless_active_and_follows_the_content() {
        let mut value = reconstruction(1.0);
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        let checksum = value.checksum();
        assert_ne!(checksum, 0);
        value.cutoff_hz = 0.3;
        assert_ne!(value.checksum(), checksum);
        value.cutoff_hz = 0.5;
        value.coeffs[4][1] = 0.3;
        assert_ne!(value.checksum(), checksum);
        for (enabled, applies) in [(false, true), (true, false)] {
            value.enabled = enabled; value.applies = applies;
            assert_eq!(value.checksum(), 0);
        }
        assert_eq!(OpticalStabReconstruction { enabled: true, applies: true, ..Default::default() }.checksum(), 0);
        let a = OpticalStabReconstruction::from_samples(vec![(1000, [0.01, 0.0, 0.0]), (2000, [0.02, 0.0, 0.0])]);
        let b = OpticalStabReconstruction::from_samples(vec![(1000, [0.01, 0.0, 0.0]), (2000, [0.03, 0.0, 0.0])]);
        assert_ne!(a.checksum(), b.checksum());
        assert_eq!(a.at(1500.0), Vector3::new(0.015, 0.0, 0.0));
    }

    #[test]
    fn limit_is_soft_and_bounded() {
        let mut large = reconstruction(10.0);
        large.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        let mut small = reconstruction(1.0);
        small.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        for time in 1000..=10_000 {
            let s = large.at(time as f64);
            assert!(s.xy().norm() <= 3.0_f64.to_radians() + 1e-9);
            assert!(s.z.abs() <= 3.0_f64.to_radians() + 1e-9);
            assert!((small.at(time as f64).x - 1.0_f64.to_radians()).abs() <= 1e-9);
            assert!((small.at(time as f64).z - 1.0_f64.to_radians()).abs() <= 1e-9);
        }
        assert!(large.max_deg <= 3.0 + 1e-9 && large.max_deg > small.max_deg);
    }

    #[test]
    fn nan_never_reaches_the_table() {
        let mut value = reconstruction(1.0);
        value.coeffs[4] = [f32::NAN, f32::INFINITY, f32::NEG_INFINITY];
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        for time in 0..=11_000 {
            assert!(value.at(time as f64).iter().all(|v| v.is_finite()));
        }
        assert!(value.max_deg.is_finite());
    }

    #[test]
    fn outside_the_range_is_zero() {
        let mut value = reconstruction(1.0);
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert_eq!(value.at(999.0), Vector3::zeros());
        assert!(value.at(1000.0).norm() > 0.0);
        assert!(value.at(10_000.0).norm() > 0.0);
        assert_eq!(value.at(10_001.0), Vector3::zeros());
        value.enabled = false;
        value.applies = false;
        assert!(value.at(5000.0).norm() > 0.0);
    }

    #[test]
    fn rebuild_combines_spline_and_prior_and_shares_the_cache() {
        let quats = rotations(1, 4.0);
        let times: Vec<_> = (0..=20_000).map(|i| i as f64 * 1000.0).collect();
        let expected_prior = prior(&quats, 0.5, &times);
        let mut value = OpticalStabReconstruction {
            enabled: true, applies: true, start_us: -1000.0, spacing_us: 1000.0,
            coeffs: vec![[0.005, -0.003, 0.002]; 20_003], cutoff_hz: 0.5,
            quats_checksum: 7, context_checksum: 9, ..Default::default()
        };
        value.rebuild(&quats, &StabReconConfig::DEFAULT);
        assert!(value.measured_on(7, 9));
        assert!(!value.measured_on(8, 9) && !value.measured_on(7, 8));
        for i in 5000..15_000 {
            let expected = value.u_at(times[i]) + expected_prior[i];
            assert!((value.at(times[i]) - expected).norm() < 1e-12);
            let expected_half = (value.at(times[i]) + value.at(times[i + 1])) * 0.5;
            assert!((value.at(times[i] + 500.0) - expected_half).norm() < 1e-12);
        }
        let snapshot = value.clone();
        assert!(Arc::ptr_eq(&value.table, &snapshot.table));
        let snapshot_checksum = snapshot.checksum();
        let snapshot_sample = snapshot.at(5_000_000.0);
        value.coeffs[5001][0] += 0.01;
        value.rebuild(&quats, &StabReconConfig::DEFAULT);
        assert!(!Arc::ptr_eq(&value.table, &snapshot.table));
        assert_eq!(snapshot.checksum(), snapshot_checksum);
        assert_eq!(snapshot.at(5_000_000.0), snapshot_sample);
        assert_ne!(value.at(5_000_000.0), snapshot_sample);
    }

    #[test]
    fn cutoff_grid_and_defaults_follow_the_spec() {
        assert_eq!(StabReconConfig::DEFAULT, StabReconConfig { cutoff_hz: None, max_deg: 3.0 });
        assert_eq!(DEFAULT_CUTOFF_HZ, 0.3);
        for (i, cutoff) in CUTOFF_GRID_HZ.iter().enumerate() {
            let expected = 0.05 * 40.0_f64.powf(i as f64 / 15.0);
            assert!((*cutoff - expected).abs() <= expected * 1e-15);
        }
    }
    #[test]
    fn invalid_times_and_spacing_are_safe() {
        let mut point = reconstruction(1.0);
        point.coeffs.truncate(3);
        point.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert!(point.is_active());
        assert!((point.at(1000.0).x - 1.0_f64.to_radians()).abs() <= 1e-9);
        assert_eq!(point.at(999.0), Vector3::zeros());
        assert_eq!(point.at(1001.0), Vector3::zeros());
        for spacing in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let mut value = reconstruction(1.0);
            value.spacing_us = spacing;
            value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
            assert_eq!(value.at(5000.0), Vector3::zeros());
            assert_eq!(value.u_at(5000.0), Vector3::zeros());
            assert!(!value.is_active());
        }
        let mut value = reconstruction(1.0);
        value.start_us = f64::NAN;
        value.rebuild(&TimeQuat::new(), &StabReconConfig::DEFAULT);
        assert!(!value.is_active());
        assert_eq!(value.at(f64::NAN), Vector3::zeros());
        assert_eq!(value.u_at(f64::INFINITY), Vector3::zeros());
        for cutoff in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(prior(&rotations(0, 4.0), cutoff, &[5000.0]), vec![Vector3::zeros()]);
        }
        assert_eq!(prior(&TimeQuat::new(), 0.5, &[f64::NAN, 1000.0]), vec![Vector3::zeros(); 2]);
    }
}
