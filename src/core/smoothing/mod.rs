// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

pub mod default_algo;
pub mod fixed;
pub mod horizon;
pub mod none;
pub mod plain;

use super::gyro_source::{Quat64, TimeQuat};
use dyn_clone::{DynClone, clone_trait_object};
pub use nalgebra::*;
use std::borrow::Cow;
pub use std::collections::HashMap;

use crate::ComputeParams;
use std::collections::hash_map::DefaultHasher;
use std::hash::Hasher;

pub trait SmoothingAlgorithm: DynClone {
    fn get_name(&self) -> String;

    fn get_parameters_json(&self) -> serde_json::Value;
    fn get_status_json(&self) -> serde_json::Value;
    fn set_parameter(&mut self, name: &str, val: f64);
    fn get_parameter(&self, name: &str) -> f64;

    fn get_checksum(&self) -> u64;

    fn smooth(&self, quats: &TimeQuat, duration: f64, _compute_params: &ComputeParams) -> TimeQuat;
}
clone_trait_object!(SmoothingAlgorithm);

struct Algs(Vec<Box<dyn SmoothingAlgorithm>>);
impl Default for Algs {
    fn default() -> Self {
        Self(vec![
            Box::new(self::none::None::default()),
            Box::new(self::default_algo::DefaultAlgo::default()),
            Box::new(self::plain::Plain::default()),
            Box::new(self::fixed::Fixed::default()),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn trimmed_quats_interpolate_range_gaps_on_video_timeline_after_offset() {
        let quats = BTreeMap::from([
            (0, Quat64::from_euler_angles(0.0, 0.0, 0.0)),
            (1_000_000, Quat64::from_euler_angles(1.0, 0.0, 0.0)),
            (2_000_000, Quat64::from_euler_angles(2.0, 0.0, 0.0)),
            (3_000_000, Quat64::from_euler_angles(3.0, 0.0, 0.0)),
            (4_000_000, Quat64::from_euler_angles(4.0, 0.0, 0.0)),
        ]);
        let compute_params = ComputeParams {
            scaled_duration_ms: 5_000.0,
            gyro_offsets: BTreeMap::from([
                (0, 0.0),
                (1_000_000, 500.0),
                (2_000_000, 1_000.0),
                (4_000_000, 1_000.0),
            ]),
            ..Default::default()
        };

        let trimmed = Smoothing::get_trimmed_quats(
            &quats,
            5_000.0,
            true,
            &[(0.0, 0.1), (0.8, 1.0)],
            &compute_params,
        );
        let trimmed = trimmed.as_ref();
        let expected = quats[&1_000_000].slerp(&quats[&3_000_000], 0.6);

        assert!((trimmed[&2_000_000].inverse() * expected).angle() < 1e-12);
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct Smoothing {
    #[serde(skip)]
    algs: Algs,
    current_id: usize,

    pub horizon_lock: horizon::HorizonLock,
}
unsafe impl Send for Smoothing {}
unsafe impl Sync for Smoothing {}

impl Default for Smoothing {
    fn default() -> Self {
        Self {
            algs: Algs::default(),

            current_id: 1,

            horizon_lock: horizon::HorizonLock::default(),
        }
    }
}

impl Clone for Smoothing {
    fn clone(&self) -> Self {
        let mut ret = Self::default();
        ret.current_id = self.current_id;
        ret.horizon_lock = self.horizon_lock.clone();

        let parameters = self.current().get_parameters_json();
        if let serde_json::Value::Array(ref arr) = parameters {
            for v in arr {
                if let serde_json::Value::Object(obj) = v {
                    (|| -> Option<()> {
                        let name = obj.get("name").and_then(|x| x.as_str())?;
                        let value = obj.get("value").and_then(|x| x.as_f64())?;
                        ret.current_mut().set_parameter(name, value);
                        Some(())
                    })();
                }
            }
        }

        ret
    }
}

impl Smoothing {
    pub fn set_current(&mut self, id: usize) {
        self.current_id = id.min(self.algs.0.len() - 1);
    }

    pub fn current_id(&self) -> usize {
        self.current_id
    }

    pub fn current(&self) -> &Box<dyn SmoothingAlgorithm> {
        &self.algs.0[self.current_id]
    }
    pub fn current_mut(&mut self) -> &mut Box<dyn SmoothingAlgorithm> {
        &mut self.algs.0[self.current_id]
    }

    pub fn get_state_checksum(&self, gyro_checksum: u64) -> u64 {
        let mut hasher = DefaultHasher::new();
        hasher.write_u64(gyro_checksum);
        hasher.write_usize(self.current_id);
        hasher.write_u64(self.algs.0[self.current_id].get_checksum());
        hasher.write_u64(self.horizon_lock.get_checksum());
        hasher.finish()
    }

    pub fn get_names(&self) -> Vec<String> {
        self.algs.0.iter().map(|x| x.get_name()).collect()
    }

    pub fn get_trimmed_quats<'a>(
        quats: &'a TimeQuat,
        duration_ms: f64,
        trim_range_only: bool,
        trim_ranges: &[(f64, f64)],
        compute_params: &ComputeParams,
    ) -> Cow<'a, TimeQuat> {
        if trim_range_only && !trim_ranges.is_empty() {
            let mut quats_copy = quats.clone();
            let ranges = trim_ranges.to_vec();
            let mut prev_q = quats
                .iter()
                .find(|(ts, _)| {
                    compute_params.video_timestamp_for_gyro_timestamp(**ts as f64 / 1000.0)
                        >= ranges.first().unwrap().0 * duration_ms
                })
                .map(|(&a, &b)| (a, b));
            let mut next_q = prev_q;
            let mut range = *ranges.first().unwrap();
            let mut current_range = 0;
            for (ts, q) in quats_copy.iter_mut() {
                let timestamp_ms =
                    compute_params.video_timestamp_for_gyro_timestamp(*ts as f64 / 1000.0);
                while timestamp_ms > range.1 * duration_ms {
                    if let Some(next_range) = ranges.get(current_range + 1) {
                        current_range += 1;
                        range = *next_range;
                    } else {
                        prev_q = quats
                            .iter()
                            .rev()
                            .find(|(ts, _)| {
                                compute_params
                                    .video_timestamp_for_gyro_timestamp(**ts as f64 / 1000.0)
                                    < ranges.last().unwrap().1 * duration_ms
                            })
                            .map(|(&a, &b)| (a, b));
                        next_q = prev_q;
                        range = (f64::INFINITY, f64::INFINITY);
                        break;
                    }
                    prev_q = Some((*ts, q.clone()));
                    next_q = quats
                        .iter()
                        .find(|(ts, _)| {
                            compute_params
                                .video_timestamp_for_gyro_timestamp(**ts as f64 / 1000.0)
                                >= range.0 * duration_ms
                        })
                        .map(|(&a, &b)| (a, b));
                }
                if !(timestamp_ms >= range.0 * duration_ms
                    && timestamp_ms <= range.1 * duration_ms)
                {
                    if let Some(prev_q) = prev_q {
                        if let Some(next_q) = next_q {
                            let prev_timestamp_ms = compute_params
                                .video_timestamp_for_gyro_timestamp(prev_q.0 as f64 / 1000.0);
                            let next_timestamp_ms = compute_params
                                .video_timestamp_for_gyro_timestamp(next_q.0 as f64 / 1000.0);
                            let dist_to_next = if next_timestamp_ms == prev_timestamp_ms {
                                0.0
                            } else {
                                (timestamp_ms - prev_timestamp_ms)
                                    / (next_timestamp_ms - prev_timestamp_ms)
                            };
                            if dist_to_next.abs() == 0.0 {
                                *q = prev_q.1;
                            } else {
                                *q = prev_q.1.slerp(&next_q.1, dist_to_next);
                            }
                        }
                    }
                }
            }
            Cow::Owned(quats_copy)
        } else {
            Cow::Borrowed(quats)
        }
    }

    pub fn get_max_angles(
        quats: &TimeQuat,
        smoothed_quats: &TimeQuat,
        params: &ComputeParams,
    ) -> (f64, f64, f64) {
        // -> (pitch, yaw, roll) in deg
        let ranges = params
            .trim_ranges
            .iter()
            .map(|x| (x.0 * params.scaled_duration_ms, x.1 * params.scaled_duration_ms))
            .collect::<Vec<_>>();
        let identity_quat = Quat64::identity();

        let mut max_pitch = 0.0;
        let mut max_yaw = 0.0;
        let mut max_roll = 0.0;

        for (timestamp, quat) in smoothed_quats.iter() {
            let video_timestamp_ms =
                params.video_timestamp_for_gyro_timestamp(*timestamp as f64 / 1000.0);
            let within_range = ranges.is_empty()
                || ranges
                    .iter()
                    .any(|x| video_timestamp_ms >= x.0 && video_timestamp_ms <= x.1);
            if within_range {
                let dist = quat.inverse() * quats.get(timestamp).unwrap_or(&identity_quat);
                let euler_dist = dist.euler_angles();
                if euler_dist.2.abs() > max_roll {
                    max_roll = euler_dist.2.abs();
                }
                if euler_dist.0.abs() > max_pitch {
                    max_pitch = euler_dist.0.abs();
                }
                if euler_dist.1.abs() > max_yaw {
                    max_yaw = euler_dist.1.abs();
                }
            }
        }

        const RAD2DEG: f64 = 180.0 / std::f64::consts::PI;
        (max_pitch * RAD2DEG, max_yaw * RAD2DEG, max_roll * RAD2DEG)
    }
}
