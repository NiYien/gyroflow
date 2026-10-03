// SPDX-License-Identifier: GPL-3.0-or-later

//! Synthetic feature-track window with known gyro motion and a known offset, for the cost and search tests.

use nalgebra::Vector3;

use crate::gyro_source::{ GyroSource, Quat64, TimeQuat };
use super::tracks::{ row_time_ms, FrameTiming, PairData, WindowTracks };

/// Video time of the window's first frame, ms
pub const WINDOW_START_MS: f64 = 5000.0;
/// Gyro samples every 1 ms over this range of gyro time, ms (both ends included)
const GYRO_RANGE_MS: (i64, i64) = (-8000, 20000);
/// Sines per axis of the angular velocity
const SINES: usize = 20;
/// Half the field of view the world directions are drawn from, degrees
const HALF_FOV_DEG: f64 = 25.0;

#[derive(Clone, Copy, Debug)]
pub struct SynthSpec {
    pub fps: f64, pub duration_ms: f64, pub tracks: usize, pub true_offset_ms: f64, pub readout_ms: f64, pub focal_px: f64,
    pub freq_hz: (f64, f64), pub amp_dps: f64, pub noise_px: f64, pub parallax: f64, pub seed: u64,
}

impl Default for SynthSpec {
    fn default() -> Self {
        Self {
            fps: 60.0, duration_ms: 3000.0, tracks: 400, true_offset_ms: -700.0, readout_ms: 10.0, focal_px: 1500.0,
            freq_hz: (1.0, 8.0), amp_dps: 20.0, noise_px: 0.2, parallax: 2e-3, seed: 1,
        }
    }
}

/// Deterministic xorshift64 generator
pub struct XorShift64(u64);

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        let mut rng = Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed });
        // A small seed needs a few rounds before its bits are mixed
        for _ in 0..8 { rng.next_u64(); }
        rng
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    /// Uniform in [0, 1)
    pub fn uniform(&mut self) -> f64 { (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64 }
    /// Uniform in [lo, hi)
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 { lo + (hi - lo) * self.uniform() }
    /// Standard normal (Box-Muller)
    pub fn gauss(&mut self) -> f64 {
        let u1 = 1.0 - self.uniform();   // (0, 1], keeps ln finite
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// A window of `duration_ms` at `fps` starting at video time `WINDOW_START_MS`, every track seen in every frame, and
/// the gyro quaternions it was made from (gyro time = video time - `true_offset_ms`, 1 kHz over -8000..20000 ms).
/// Without the observation noise every motion costs exactly 0 at the truth, so the noise is part of the fixture.
pub fn synth_window(spec: &SynthSpec) -> (WindowTracks, TimeQuat) {
    let (mut windows, quats) = synth_windows(spec, &[WINDOW_START_MS]);
    (windows.remove(0), quats)
}

/// Windows sharing one gyro motion; each start consumes its tracks and observation noise in order.
pub fn synth_windows(spec: &SynthSpec, starts_ms: &[f64]) -> (Vec<WindowTracks>, TimeQuat) {
    synth_windows_with_gyro_range(spec, starts_ms, GYRO_RANGE_MS)
}

/// Windows with an explicit gyro range, for benchmarks that need a wider search domain.
pub fn synth_windows_with_gyro_range(spec: &SynthSpec, starts_ms: &[f64], gyro_range_ms: (i64, i64)) -> (Vec<WindowTracks>, TimeQuat) {
    let mut rng = XorShift64::new(spec.seed);

    // Angular velocity in the camera frame: per axis a sum of sines (frequency Hz, phase rad), `amp_dps` each
    let sines: [Vec<(f64, f64)>; 3] = std::array::from_fn(|_| {
        (0..SINES).map(|_| (rng.range(spec.freq_hz.0, spec.freq_hz.1), rng.range(0.0, std::f64::consts::TAU))).collect()
    });
    let amp = spec.amp_dps.to_radians();
    let omega = |t_ms: f64| -> Vector3<f64> {
        Vector3::from_fn(|axis, _| sines[axis].iter().map(|(f, ph)| amp * (std::f64::consts::TAU * f * t_ms / 1000.0 + ph).sin()).sum())
    };
    let mut quats = TimeQuat::new();
    let mut q = Quat64::identity();
    for ms in gyro_range_ms.0..=gyro_range_ms.1 {
        quats.insert(ms * 1000, q);
        // 1 ms step with the rate at its middle
        q = Quat64::new_normalize((q * Quat64::from_scaled_axis(omega(ms as f64 + 0.5) * 1e-3)).into_inner());
    }

    let dt = 1000.0 / spec.fps;
    let n_frames = (spec.duration_ms / dt).round() as usize;
    let windows = starts_ms.iter().map(|&start_ms| {
        let frames: Vec<FrameTiming> = (0..n_frames).map(|k| {
            let ts_ms = start_ms + k as f64 * dt;
            FrameTiming { index: (ts_ms / dt).round() as usize, ts_ms, mid_ms: ts_ms, readout_ms: spec.readout_ms }
        }).collect();

        // Tracks: a world direction within the field of view at the window's middle, a parallax drift direction and a
        // fixed position along the readout in [0, 1)
        let q_mid = GyroSource::clamped_quat_at_gyro_timestamp(&quats, start_ms + spec.duration_ms / 2.0 - spec.true_offset_ms);
        let half_fov = HALF_FOV_DEG.to_radians();
        let tracks: Vec<(Vector3<f64>, Vector3<f64>, f32)> = (0..spec.tracks).map(|_| {
            let (ax, ay) = (rng.range(-half_fov, half_fov), rng.range(-half_fov, half_fov));
            let w = q_mid * Vector3::new(ax.tan(), ay.tan(), -1.0).normalize();
            let d = Vector3::new(rng.gauss(), rng.gauss(), rng.gauss()).normalize();
            let f = (rng.next_u64() >> 40) as f32 / (1u32 << 24) as f32;
            (w, d, f)
        }).collect();

        // One observation per (frame, track), shared by the two pairs the frame belongs to, each with its own noise
        let sigma = spec.noise_px / spec.focal_px;
        let bearings: Vec<Vec<Vector3<f64>>> = frames.iter().map(|fr| {
            tracks.iter().map(|(w, d, f)| {
                let world = (w + d * (spec.parallax * (fr.ts_ms - start_ms) / 1000.0)).normalize();
                let q = GyroSource::clamped_quat_at_gyro_timestamp(&quats, row_time_ms(fr, *f) - spec.true_offset_ms);
                let n = Vector3::new(rng.gauss(), rng.gauss(), rng.gauss()) * sigma;
                (q.inverse() * world + n).normalize()
            }).collect()
        }).collect();

        let ids: Vec<u32> = (0..spec.tracks as u32).collect();
        let pos: Vec<f32> = tracks.iter().map(|t| t.2).collect();
        let pairs = (1..n_frames).map(|k| PairData {
            seq: k - 1, a: frames[k - 1], b: frames[k], ids: ids.clone(),
            va: bearings[k - 1].clone(), vb: bearings[k].clone(), fa: pos.clone(), fb: pos.clone(),
        }).collect();
        WindowTracks { pairs, focal_px: spec.focal_px }
    }).collect();
    (windows, quats)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn synth_window_golden() {
        let spec = SynthSpec { fps: 30.0, duration_ms: 1000.0, tracks: 50, ..Default::default() };
        let (w, quats) = synth_window(&spec);
        let sum: f64 = w.pairs.iter().flat_map(|p| p.va.iter().chain(&p.vb)).flat_map(|v| v.iter()).sum();
        let middle_w = quats.values().nth(quats.len() / 2).unwrap().w;
        // Captured from the failing pre-refactor assertion with both expected values set to zero.
        assert_eq!((sum, middle_w), (-2824.6429587792795, 0.9948696147650294));
    }
}
