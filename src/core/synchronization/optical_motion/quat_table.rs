// SPDX-License-Identifier: GPL-3.0-or-later

//! Gyro orientation pre-sampled on a uniform grid, so that a cost evaluation looks a quaternion up with an index and
//! one slerp instead of a `BTreeMap` search.

use crate::gyro_source::{ GyroSource, Quat64, TimeQuat };

/// `clamped_quat_at_gyro_timestamp` sampled every `STEP_MS` of gyro time from `t0_ms`
pub struct QuatTable {
    t0_ms: f64,
    quats: Vec<Quat64>,
    /// First and last key of the source quaternions, ms; (+inf, -inf) when there are none
    gyro_range_ms: (f64, f64),
}

impl QuatTable {
    pub const STEP_MS: f64 = 0.5;

    /// Samples `clamped_quat_at_gyro_timestamp` every STEP_MS over [from_ms, to_ms]
    pub fn build(quats: &TimeQuat, from_ms: f64, to_ms: f64) -> Self {
        let gyro_range_ms = match (quats.keys().next(), quats.keys().next_back()) {
            (Some(&first), Some(&last)) => (first as f64 / 1000.0, last as f64 / 1000.0),
            _ => (f64::INFINITY, f64::NEG_INFINITY),
        };
        // At least one sample; the last one is at or past `to_ms`
        let steps = ((to_ms - from_ms) / Self::STEP_MS).ceil();
        let n = if steps.is_finite() && steps > 0.0 { steps as usize + 1 } else { 1 };
        let table = (0..n).map(|i| GyroSource::clamped_quat_at_gyro_timestamp(quats, from_ms + i as f64 * Self::STEP_MS)).collect();
        Self { t0_ms: from_ms, quats: table, gyro_range_ms }
    }

    /// Slerp between neighbours, clamped to the table ends
    pub fn at(&self, gyro_ms: f64) -> Quat64 {
        let last = self.quats.len() - 1;
        let x = (gyro_ms - self.t0_ms) / Self::STEP_MS;
        if x.is_nan() || x <= 0.0 { return self.quats[0]; }
        if x >= last as f64 { return self.quats[last]; }
        let i = x.floor() as usize;
        self.quats[i].slerp(&self.quats[i + 1], x - i as f64)
    }

    /// Whether both `from_ms` and `to_ms` lie inside the first..last key of the source quaternions. This is about the
    /// gyro data, not the table's own range: building the table wide enough is the caller's job.
    pub fn covers(&self, from_ms: f64, to_ms: f64) -> bool {
        let inside = |t: f64| t >= self.gyro_range_ms.0 && t <= self.gyro_range_ms.1;
        inside(from_ms) && inside(to_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synchronization::optical_motion::testutil::{ synth_window, SynthSpec };

    #[test] fn table_matches_direct_lookup() {
        let (_, quats) = synth_window(&SynthSpec::default());
        let t = QuatTable::build(&quats, 1000.0, 9000.0);
        for i in 0..2000 {
            let ms = 900.0 + i as f64 * 4.137;       // also reaches outside the table: clamped
            let d = GyroSource::clamped_quat_at_gyro_timestamp(&quats, ms.clamp(1000.0, 9000.0));
            assert!(t.at(ms).angle_to(&d) < 1e-5, "at {ms}");
        }
        assert!(t.covers(-7000.0, 19000.0) && !t.covers(-9000.0, 0.0) && !t.covers(0.0, 21000.0));
    }
}
