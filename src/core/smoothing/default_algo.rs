// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Aphobius

// 1. Calculate velocity for each quaternion
// 2. Smooth the velocities
// 3. Multiply max velocity (500 deg/s) with slider value
// 4. Perform plain 3D smoothing with varying alpha, where each alpha is interpolated between 1s smoothness at 0 velocity, 0.1s smoothness at max velocity and extrapolated above that
// 5. This way, low velocities are smoothed using 1s smoothness, but high velocities are smoothed using 0.1s smoothness at max velocity (500 deg/s multiplied by slider) and gradually lower smoothness above that
// 6. Calculate distance from smoothed quaternions to raw quaternions
// 7. Normalize distance and set everything bellow 0.5 to 0.0
// 8. Smooth distance
// 9. Normalize distance again and change range to 0.5 - 1.0
// 10. Perform plain 3D smoothing, on the last smoothed quaternions, with varying alpha, interpolated between 1s and 0.1s smoothness based on previously calculated velocity multiplied by the distance
// Without per-axis smoothing, the roll about the optical axis (twist) is a separate channel in all of the above:
// its alpha follows the roll velocity alone and its max velocity is not scaled by the lens FOV, because a roll moves
// the frame corners by angle * half diagonal and needs cos + aspect * sin of zoom at any focal length.
// The remaining rotation (swing) keeps the total velocity and the FOV scaling.

use std::collections::BTreeMap;

use super::*;
use crate::keyframes::*;
use nalgebra::*;

const MAX_VELOCITY: f64 = 500.0;
// Use 120 diagonal FOV as reference. Anything below (long focal length) scales the smoothness down. Anything above (short focal length) scales the smoothness up.
// This is needed, because the same rotation at long focal length will be much larger actual image rotation than at short focal length.
// It applies to pan and tilt only; the roll channel is focal length independent (see the notes above).
const FOV_REFERENCE: f64 = 120.0;
const RAD_TO_DEG: f64 = 180.0 / std::f64::consts::PI;

#[derive(Clone)]
pub struct DefaultAlgo {
    pub smoothness: f64,
    pub smoothness_pitch: f64,
    pub smoothness_yaw: f64,
    pub smoothness_roll: f64,
    pub per_axis: bool,
    pub second_pass: bool,
    pub trim_range_only: bool,
    pub max_smoothness: f64,
    pub alpha_0_1s: f64,
}

impl Default for DefaultAlgo {
    fn default() -> Self {
        Self {
            smoothness: 0.15,
            smoothness_pitch: 0.15,
            smoothness_yaw: 0.15,
            smoothness_roll: 0.15,
            per_axis: false,
            second_pass: true,
            trim_range_only: true,
            max_smoothness: 1.0,
            alpha_0_1s: 0.10,
        }
    }
}

impl SmoothingAlgorithm for DefaultAlgo {
    fn get_name(&self) -> String {
        "Default".to_owned()
    }

    fn set_parameter(&mut self, name: &str, val: f64) {
        match name {
            "smoothness" => self.smoothness = val,
            "smoothness_pitch" => self.smoothness_pitch = val,
            "smoothness_yaw" => self.smoothness_yaw = val,
            "smoothness_roll" => self.smoothness_roll = val,
            "per_axis" => self.per_axis = val > 0.1,
            // "second_pass"      => self.second_pass = val > 0.1,
            "trim_range_only" => self.trim_range_only = val > 0.1,
            "max_smoothness" => self.max_smoothness = val,
            "alpha_0_1s" => self.alpha_0_1s = val,
            _ => log::error!("Invalid parameter name: {}", name),
        }
    }
    fn get_parameter(&self, name: &str) -> f64 {
        match name {
            "smoothness" => self.smoothness,
            "smoothness_pitch" => self.smoothness_pitch,
            "smoothness_yaw" => self.smoothness_yaw,
            "smoothness_roll" => self.smoothness_roll,
            "per_axis" => {
                if self.per_axis {
                    1.0
                } else {
                    0.0
                }
            }
            // "second_pass"      => if self.second_pass { 1.0 } else { 0.0 },
            "trim_range_only" => {
                if self.trim_range_only {
                    1.0
                } else {
                    0.0
                }
            }
            "max_smoothness" => self.max_smoothness,
            "alpha_0_1s" => self.alpha_0_1s,
            _ => 0.0,
        }
    }

    fn get_parameters_json(&self) -> serde_json::Value {
        serde_json::json!([
            {
                "name": "smoothness",
                "description": "Smoothness",
                "type": "SliderWithField",
                "from": 0.001,
                "to": 1.0,
                "value": self.smoothness,
                "default": 0.5,
                "unit": "",
                "precision": 3,
                "keyframe": "SmoothingParamSmoothness"
            },
            {
                "name": "smoothness_pitch",
                "description": "Pitch smoothness",
                "type": "SliderWithField",
                "from": 0.001,
                "to": 1.0,
                "value": self.smoothness_pitch,
                "default": 0.5,
                "unit": "",
                "precision": 3,
                "keyframe": "SmoothingParamPitch"
            },
            {
                "name": "smoothness_yaw",
                "description": "Yaw smoothness",
                "type": "SliderWithField",
                "from": 0.001,
                "to": 1.0,
                "value": self.smoothness_yaw,
                "default": 0.5,
                "unit": "",
                "precision": 3,
                "keyframe": "SmoothingParamYaw"
            },
            {
                "name": "smoothness_roll",
                "description": "Roll smoothness",
                "type": "SliderWithField",
                "from": 0.001,
                "to": 1.0,
                "value": self.smoothness_roll,
                "default": 0.5,
                "unit": "",
                "precision": 3,
                "keyframe": "SmoothingParamRoll"
            },
            {
                "name": "per_axis",
                "description": "Per axis",
                "advanced": true,
                "type": "CheckBox",
                "default": self.per_axis,
                "value": if self.per_axis { 1.0 } else { 0.0 },
                "custom_qml": "Connections { function onCheckedChanged() {
                    const checked = root.getParamElement('per_axis').checked;
                    root.getParamElement('smoothness-label').visible = !checked;
                    root.getParamElement('smoothness_pitch-label').visible = checked;
                    root.getParamElement('smoothness_yaw-label').visible = checked;
                    root.getParamElement('smoothness_roll-label').visible = checked;
                }}"
            },
            /*{
                "name": "second_pass",
                "description": "Second smoothing pass",
                "advanced": true,
                "type": "CheckBox",
                "default": self.second_pass,
                "value": if self.second_pass { 1.0 } else { 0.0 },
            },*/
            {
                "name": "trim_range_only",
                "description": "Only within trim range",
                "advanced": true,
                "type": "CheckBox",
                "default": self.trim_range_only,
                "value": if self.trim_range_only { 1.0 } else { 0.0 },
            },
            {
                "name": "max_smoothness",
                "description": "Max smoothness",
                "advanced": true,
                "type": "SliderWithField",
                "from": 0.1,
                "to": 5.0,
                "value": self.max_smoothness,
                "default": 1.0,
                "precision": 3,
                "unit": "s",
                "keyframe": "SmoothingParamTimeConstant"
            },
            {
                "name": "alpha_0_1s",
                "description": "Max smoothness at high velocity",
                "advanced": true,
                "type": "SliderWithField",
                "from": 0.01,
                "to": 1.0,
                "value": self.alpha_0_1s,
                "default": 0.1,
                "precision": 3,
                "unit": "s",
                "keyframe": "SmoothingParamTimeConstant2"
            }
        ])
    }

    fn get_status_json(&self) -> serde_json::Value {
        serde_json::json!([])
    }

    fn get_checksum(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        hasher.write_u64(self.smoothness.to_bits());
        hasher.write_u64(self.smoothness_pitch.to_bits());
        hasher.write_u64(self.smoothness_yaw.to_bits());
        hasher.write_u64(self.smoothness_roll.to_bits());
        hasher.write_u64(self.max_smoothness.to_bits());
        hasher.write_u64(self.alpha_0_1s.to_bits());
        hasher.write_u8(if self.per_axis { 1 } else { 0 });
        hasher.write_u8(if self.second_pass { 1 } else { 0 });
        hasher.finish()
    }

    fn smooth(
        &self,
        quats: &TimeQuat,
        duration_ms: f64,
        compute_params: &ComputeParams,
    ) -> TimeQuat {
        // TODO Result<>?
        if quats.is_empty() || duration_ms <= 0.0 {
            return quats.clone();
        }

        let sample_rate: f64 = quats.len() as f64 / (duration_ms / 1000.0);
        let rad_to_deg_per_sec: f64 = sample_rate * RAD_TO_DEG;

        let get_alpha = |time_constant: f64| 1.0 - (-(1.0 / sample_rate) / time_constant).exp();
        let noop = |v| v;

        let keyframes = &compute_params.keyframes;

        let quats = Smoothing::get_trimmed_quats(
            quats,
            compute_params.scaled_duration_ms,
            self.trim_range_only,
            &compute_params.trim_ranges,
            compute_params,
        );
        let quats_inner = quats.as_ref();

        // Mirror-pad both ends so the 4-pass forward/backward EMA can do its
        // burn-in inside pad zones instead of leaking transient into the visible
        // first/last frames. Pad size = 3x the max time constant in samples,
        // capped to keep high-rate IMU buffers reasonable.
        let pad_sample_rate =
            (quats_inner.len() as f64) / (duration_ms / 1000.0).max(1e-9);
        let max_tc = self.max_smoothness.max(self.alpha_0_1s);
        let pad_n = ((max_tc * pad_sample_rate * 3.0).ceil() as usize)
            .min(quats_inner.len().saturating_sub(1))
            .min(2000);

        let (orig_first_ts, orig_last_ts) = (
            *quats_inner.iter().next().map(|(k, _)| k).unwrap_or(&0),
            *quats_inner.iter().next_back().map(|(k, _)| k).unwrap_or(&0),
        );

        let padded_quats = mirror_pad_quats(quats_inner, pad_n);
        let quats: &TimeQuat = &padded_quats;

        let get_keyframed_param = |typ: &KeyframeType,
                                   def: f64,
                                   cb: &dyn Fn(f64) -> f64|
         -> BTreeMap<i64, f64> {
            let mut ret = BTreeMap::<i64, f64>::new();
            if keyframes.is_keyframed(typ)
                || (compute_params.video_speed_affects_smoothing
                    && (compute_params.video_speed != 1.0
                        || keyframes.is_keyframed(&KeyframeType::VideoSpeed)))
            {
                ret = quats
                    .iter()
                    .map(|(ts, _)| {
                        let timestamp_ms = *ts as f64 / 1000.0;
                        let mut val = keyframes
                            .value_at_gyro_timestamp(typ, timestamp_ms)
                            .unwrap_or(def);
                        if compute_params.video_speed_affects_smoothing {
                            let vid_speed = keyframes
                                .value_at_gyro_timestamp(&KeyframeType::VideoSpeed, timestamp_ms)
                                .unwrap_or(compute_params.video_speed)
                                .abs();
                            if typ == &KeyframeType::SmoothingParamTimeConstant
                                || typ == &KeyframeType::SmoothingParamTimeConstant2
                            {
                                val *= 1.0 + ((vid_speed - 1.0) / 2.0);
                            } else {
                                val *= vid_speed;
                            }
                        }
                        (*ts, cb(val))
                    })
                    .collect();
            }
            ret
        };

        let alpha_smoothness_per_timestamp = get_keyframed_param(
            &KeyframeType::SmoothingParamTimeConstant,
            self.max_smoothness,
            &get_alpha,
        );
        let alpha_0_1s_per_timestamp = get_keyframed_param(
            &KeyframeType::SmoothingParamTimeConstant2,
            self.alpha_0_1s,
            &get_alpha,
        );
        let smoothness_per_timestamp = get_keyframed_param(
            &KeyframeType::SmoothingParamSmoothness,
            self.smoothness,
            &noop,
        );
        let smoothness_pitch_per_timestamp = get_keyframed_param(
            &KeyframeType::SmoothingParamPitch,
            self.smoothness_pitch,
            &noop,
        );
        let smoothness_yaw_per_timestamp =
            get_keyframed_param(&KeyframeType::SmoothingParamYaw, self.smoothness_yaw, &noop);
        let smoothness_roll_per_timestamp = get_keyframed_param(
            &KeyframeType::SmoothingParamRoll,
            self.smoothness_roll,
            &noop,
        );

        let alpha_smoothness = get_alpha(self.max_smoothness);
        let alpha_0_1s = get_alpha(self.alpha_0_1s);

        // Calculate velocity
        let mut velocity = BTreeMap::<i64, Vector3<f64>>::new();

        let first_quat = quats.iter().next().unwrap(); // First quat
        velocity.insert(*first_quat.0, Vector3::from_element(0.0));

        let mut prev_quat = *quats.iter().next().unwrap().1; // First quat
        for (timestamp, quat) in quats.iter().skip(1) {
            let dist = prev_quat.inverse() * quat;
            if self.per_axis {
                let euler = dist.euler_angles();
                velocity.insert(
                    *timestamp,
                    Vector3::new(
                        euler.0.abs() * rad_to_deg_per_sec,
                        euler.1.abs() * rad_to_deg_per_sec,
                        euler.2.abs() * rad_to_deg_per_sec,
                    ),
                );
            } else {
                velocity.insert(
                    *timestamp,
                    Vector3::new(
                        dist.angle() * rad_to_deg_per_sec,
                        twist_z(&dist).angle() * rad_to_deg_per_sec,
                        0.0,
                    ),
                );
            }
            prev_quat = *quat;
        }

        // Smooth velocity
        let mut prev_velocity = *velocity.iter().next().unwrap().1; // First velocity
        for (_timestamp, vel) in velocity.iter_mut().skip(1) {
            *vel = prev_velocity * (1.0 - alpha_0_1s) + *vel * alpha_0_1s;
            prev_velocity = *vel;
        }
        for (_timestamp, vel) in velocity.iter_mut().rev().skip(1) {
            *vel = prev_velocity * (1.0 - alpha_0_1s) + *vel * alpha_0_1s;
            prev_velocity = *vel;
        }

        // Normalize velocity
        for (ts, vel) in velocity.iter_mut() {
            let smoothness_pitch = smoothness_pitch_per_timestamp
                .get(ts)
                .unwrap_or(&self.smoothness_pitch);
            let smoothness_yaw = smoothness_yaw_per_timestamp
                .get(ts)
                .unwrap_or(&self.smoothness_yaw);
            let smoothness_roll = smoothness_roll_per_timestamp
                .get(ts)
                .unwrap_or(&self.smoothness_roll);
            let smoothness = smoothness_per_timestamp.get(ts).unwrap_or(&self.smoothness);

            let frame = compute_params.frame_at_gyro_timestamp(*ts as f64 / 1000.0);
            let mut fov_ratio = if compute_params.camera_diagonal_fovs.len() == 1 {
                compute_params.camera_diagonal_fovs[0] / FOV_REFERENCE
            } else {
                compute_params
                    .camera_diagonal_fovs
                    .get(frame)
                    .map(|x| *x / FOV_REFERENCE)
                    .unwrap_or(1.0)
            };

            let mut fov_limit = 1.0;
            // Samples past the last frame (end pad, gyro beyond the video) take the last frame's limit, like the
            // ones before the first frame take the first frame's
            let limits = &compute_params.smoothing_fov_limit_per_frame;
            if let Some(fov_limit_ratio) = limits.get(frame.min(limits.len().saturating_sub(1))) {
                fov_ratio *= *fov_limit_ratio;
                fov_limit = *fov_limit_ratio;
            }

            // Calculate max velocity
            let mut max_velocity = [MAX_VELOCITY, MAX_VELOCITY, MAX_VELOCITY];
            if self.per_axis {
                max_velocity[0] *= smoothness_pitch * fov_ratio;
                max_velocity[1] *= smoothness_yaw * fov_ratio;
                max_velocity[2] *= smoothness_roll * fov_ratio;
            } else {
                max_velocity[0] *= smoothness * fov_ratio;
                // Roll channel: no lens FOV factor, the Max Zoom limit still relaxes it
                max_velocity[1] *= smoothness * fov_limit;
            }

            // Doing this to get similar max zoom as without second pass
            if self.second_pass {
                max_velocity[0] *= 0.5;
                max_velocity[1] *= 0.5;
                if self.per_axis {
                    max_velocity[2] *= 0.5;
                }
            }

            // A Max Zoom limit of 0 means no smoothing: saturate the ratio, the factor then clamps to 1
            let normalize = |v: f64, max: f64| if max > 0.0 { v / max } else { f64::MAX };
            vel[0] = normalize(vel[0], max_velocity[0]);
            vel[1] = normalize(vel[1], max_velocity[1]);
            if self.per_axis {
                vel[2] = normalize(vel[2], max_velocity[2]);
            }
        }

        // Plain 3D smoothing with varying alpha
        // Forward pass
        let mut q = *quats.iter().next().unwrap().1;
        let smoothed1: TimeQuat = quats
            .iter()
            .map(|(ts, x)| {
                let ratio = velocity[ts];
                let alpha_smoothness = alpha_smoothness_per_timestamp
                    .get(ts)
                    .unwrap_or(&alpha_smoothness);
                let alpha_0_1s = alpha_0_1s_per_timestamp.get(ts).unwrap_or(&alpha_0_1s);
                if self.per_axis {
                    let pitch_factor = alpha_smoothness * (1.0 - ratio[0]) + alpha_0_1s * ratio[0];
                    let yaw_factor = alpha_smoothness * (1.0 - ratio[1]) + alpha_0_1s * ratio[1];
                    let roll_factor = alpha_smoothness * (1.0 - ratio[2]) + alpha_0_1s * ratio[2];

                    let euler_rot = (q.inverse() * x).euler_angles();

                    let quat_rot = Quat64::from_euler_angles(
                        euler_rot.0 * pitch_factor.min(1.0),
                        euler_rot.1 * yaw_factor.min(1.0),
                        euler_rot.2 * roll_factor.min(1.0),
                    );
                    q *= quat_rot;
                } else {
                    let swing = alpha_smoothness * (1.0 - ratio[0]) + alpha_0_1s * ratio[0];
                    let roll = alpha_smoothness * (1.0 - ratio[1]) + alpha_0_1s * ratio[1];
                    q = split_step(&q, x, swing.min(1.0), roll.min(1.0));
                }
                (*ts, q)
            })
            .collect();

        // Reverse pass
        // EXPERIMENT: init from raw quats.last() instead of smoothed1.last() to
        // check whether tail-end raw_fov drop (adaptive-zoom blow-up at the last
        // ~8 frames) is caused by forward-EMA lag being inherited as reverse
        // start. Revert if no improvement.
        let mut q = *quats.iter().next_back().unwrap().1;
        let smoothed2: TimeQuat = smoothed1
            .into_iter()
            .rev()
            .map(|(ts, x)| {
                let alpha_smoothness = alpha_smoothness_per_timestamp
                    .get(&ts)
                    .unwrap_or(&alpha_smoothness);
                let alpha_0_1s = alpha_0_1s_per_timestamp.get(&ts).unwrap_or(&alpha_0_1s);
                let ratio = velocity[&ts];
                if self.per_axis {
                    let pitch_factor = alpha_smoothness * (1.0 - ratio[0]) + alpha_0_1s * ratio[0];
                    let yaw_factor = alpha_smoothness * (1.0 - ratio[1]) + alpha_0_1s * ratio[1];
                    let roll_factor = alpha_smoothness * (1.0 - ratio[2]) + alpha_0_1s * ratio[2];

                    let euler_rot = (q.inverse() * x).euler_angles();

                    let quat_rot = Quat64::from_euler_angles(
                        euler_rot.0 * pitch_factor.min(1.0),
                        euler_rot.1 * yaw_factor.min(1.0),
                        euler_rot.2 * roll_factor.min(1.0),
                    );
                    q *= quat_rot;
                } else {
                    let swing = alpha_smoothness * (1.0 - ratio[0]) + alpha_0_1s * ratio[0];
                    let roll = alpha_smoothness * (1.0 - ratio[1]) + alpha_0_1s * ratio[1];
                    q = split_step(&q, &x, swing.min(1.0), roll.min(1.0));
                }
                (ts, q)
            })
            .collect();

        if !self.second_pass {
            return trim_pad(smoothed2, orig_first_ts, orig_last_ts);
        }

        // Calculate distance
        let mut distance = BTreeMap::<i64, Vector3<f64>>::new();
        let mut max_distance = Vector3::from_element(0.0);
        for (ts, quat) in smoothed2.iter() {
            let dist = quats[ts].inverse() * quat;
            if self.per_axis {
                let euler = dist.euler_angles();
                distance.insert(
                    *ts,
                    Vector3::new(euler.0.abs(), euler.1.abs(), euler.2.abs()),
                );
                if euler.0.abs() > max_distance[0] {
                    max_distance[0] = euler.0.abs();
                }
                if euler.1.abs() > max_distance[1] {
                    max_distance[1] = euler.1.abs();
                }
                if euler.2.abs() > max_distance[2] {
                    max_distance[2] = euler.2.abs();
                }
            } else {
                let roll = twist_z(&dist).angle();
                distance.insert(*ts, Vector3::new(dist.angle(), roll, 0.0));
                if dist.angle() > max_distance[0] {
                    max_distance[0] = dist.angle();
                }
                if roll > max_distance[1] {
                    max_distance[1] = roll;
                }
            }
        }
        if !self.per_axis {
            // Footage without any roll keeps a finite roll channel
            max_distance[1] = max_distance[1].max(1e-15);
        }

        // Normalize distance and discard under 0.5
        for (_ts, dist) in distance.iter_mut() {
            dist[0] /= max_distance[0];
            if dist[0] < 0.5 {
                dist[0] = 0.0;
            }
            if !self.per_axis {
                dist[1] /= max_distance[1];
                if dist[1] < 0.5 {
                    dist[1] = 0.0;
                }
            }
            if self.per_axis {
                dist[1] /= max_distance[1];
                if dist[1] < 0.5 {
                    dist[1] = 0.0;
                }
                dist[2] /= max_distance[2];
                if dist[2] < 0.5 {
                    dist[2] = 0.0;
                }
            }
        }

        // Smooth distance
        let mut prev_dist = *distance.iter().next().unwrap().1;
        for (_timestamp, dist) in distance.iter_mut().skip(1) {
            *dist = prev_dist * (1.0 - alpha_0_1s) + *dist * alpha_0_1s;
            prev_dist = *dist;
        }
        for (_timestamp, dist) in distance.iter_mut().rev().skip(1) {
            *dist = prev_dist * (1.0 - alpha_0_1s) + *dist * alpha_0_1s;
            prev_dist = *dist;
        }

        // Get max distance
        max_distance = Vector3::from_element(0.0);
        for (_ts, dist) in distance.iter_mut() {
            if dist[0] > max_distance[0] {
                max_distance[0] = dist[0];
            }
            if !self.per_axis && dist[1] > max_distance[1] {
                max_distance[1] = dist[1];
            }
            if self.per_axis {
                if dist[1] > max_distance[1] {
                    max_distance[1] = dist[1];
                }
                if dist[2] > max_distance[2] {
                    max_distance[2] = dist[2];
                }
            }
        }

        if !self.per_axis {
            max_distance[1] = max_distance[1].max(1e-15);
        }

        // Normalize distance and change range to 0.5 - 1.0
        for (_ts, dist) in distance.iter_mut() {
            dist[0] /= max_distance[0];
            dist[0] = (dist[0] + 1.0) / 2.0;
            if !self.per_axis {
                dist[1] /= max_distance[1];
                dist[1] = (dist[1] + 1.0) / 2.0;
            }
            if self.per_axis {
                dist[1] /= max_distance[1];
                dist[1] = (dist[1] + 1.0) / 2.0;
                dist[2] /= max_distance[2];
                dist[2] = (dist[2] + 1.0) / 2.0;
            }
        }

        // Plain 3D smoothing with varying alpha
        // Forward pass
        let mut q = *smoothed2.iter().next().unwrap().1;
        let smoothed1: TimeQuat = smoothed2
            .into_iter()
            .map(|(ts, x)| {
                let alpha_smoothness = alpha_smoothness_per_timestamp
                    .get(&ts)
                    .unwrap_or(&alpha_smoothness);
                let alpha_0_1s = alpha_0_1s_per_timestamp.get(&ts).unwrap_or(&alpha_0_1s);
                let vel_ratio = velocity[&ts];
                let dist_ratio = distance[&ts];
                if self.per_axis {
                    let pitch_factor = alpha_smoothness * (1.0 - vel_ratio[0] * dist_ratio[0])
                        + alpha_0_1s * vel_ratio[0] * dist_ratio[0];
                    let yaw_factor = alpha_smoothness * (1.0 - vel_ratio[1] * dist_ratio[1])
                        + alpha_0_1s * vel_ratio[1] * dist_ratio[1];
                    let roll_factor = alpha_smoothness * (1.0 - vel_ratio[2] * dist_ratio[2])
                        + alpha_0_1s * vel_ratio[2] * dist_ratio[2];

                    let euler_rot = (q.inverse() * x).euler_angles();

                    let quat_rot = Quat64::from_euler_angles(
                        euler_rot.0 * pitch_factor.min(1.0),
                        euler_rot.1 * yaw_factor.min(1.0),
                        euler_rot.2 * roll_factor.min(1.0),
                    );
                    q *= quat_rot;
                } else {
                    let swing = alpha_smoothness * (1.0 - vel_ratio[0] * dist_ratio[0])
                        + alpha_0_1s * vel_ratio[0] * dist_ratio[0];
                    let roll = alpha_smoothness * (1.0 - vel_ratio[1] * dist_ratio[1])
                        + alpha_0_1s * vel_ratio[1] * dist_ratio[1];
                    q = split_step(&q, &x, swing.min(1.0), roll.min(1.0));
                }
                (ts, q)
            })
            .collect();

        // Reverse pass
        // Init from raw quats.last() instead of forward-EMA terminal
        // smoothed1.last() to avoid inheriting forward transient at the start
        // of reverse. Mirror padding handles the rest of the boundary cleanup.
        let mut q = *quats.iter().next_back().unwrap().1;
        let final_smoothed: TimeQuat = smoothed1
            .into_iter()
            .rev()
            .map(|(ts, x)| {
                let alpha_smoothness = alpha_smoothness_per_timestamp
                    .get(&ts)
                    .unwrap_or(&alpha_smoothness);
                let alpha_0_1s = alpha_0_1s_per_timestamp.get(&ts).unwrap_or(&alpha_0_1s);
                let vel_ratio = velocity[&ts];
                let dist_ratio = distance[&ts];
                if self.per_axis {
                    let pitch_factor = alpha_smoothness * (1.0 - vel_ratio[0] * dist_ratio[0])
                        + alpha_0_1s * vel_ratio[0] * dist_ratio[0];
                    let yaw_factor = alpha_smoothness * (1.0 - vel_ratio[1] * dist_ratio[1])
                        + alpha_0_1s * vel_ratio[1] * dist_ratio[1];
                    let roll_factor = alpha_smoothness * (1.0 - vel_ratio[2] * dist_ratio[2])
                        + alpha_0_1s * vel_ratio[2] * dist_ratio[2];

                    let euler_rot = (q.inverse() * x).euler_angles();

                    let quat_rot = Quat64::from_euler_angles(
                        euler_rot.0 * pitch_factor.min(1.0),
                        euler_rot.1 * yaw_factor.min(1.0),
                        euler_rot.2 * roll_factor.min(1.0),
                    );
                    q *= quat_rot;
                } else {
                    let swing = alpha_smoothness * (1.0 - vel_ratio[0] * dist_ratio[0])
                        + alpha_0_1s * vel_ratio[0] * dist_ratio[0];
                    let roll = alpha_smoothness * (1.0 - vel_ratio[1] * dist_ratio[1])
                        + alpha_0_1s * vel_ratio[1] * dist_ratio[1];
                    q = split_step(&q, &x, swing.min(1.0), roll.min(1.0));
                }
                (ts, q)
            })
            .collect();
        trim_pad(final_smoothed, orig_first_ts, orig_last_ts)
    }
}

// Mirror-pad a TimeQuat using SO(3) spherical reflection around the boundary
// quaternion, so the 4-pass forward/backward EMA can do its burn-in inside the
// pad zones and enter real data with zero boundary transient. Callers slice
// the pad off with `trim_pad` after smoothing. Timestamps in the pad region
// are linearly extrapolated from the boundary dt.
//
// SO(3) reflection formula:  q_pad[k] = q_boundary * q_src[k].inverse() * q_boundary
//   - q_src[k] is the kth real frame measured from the boundary (towards interior)
//   - geodesic midpoint of (q_src[k], q_pad[k]) on the rotation manifold = q_boundary
//   - When q_src[k] ≈ q_boundary (camera static near the edge), q_pad[k] ≈ q_boundary
//     (degenerates to replicate). When q_src[k] differs by rotation Δ, q_pad[k]
//     differs by the inverse rotation Δ.inverse() — physical meaning: future
//     motion is the time-reversed mirror of past motion.
fn mirror_pad_quats(quats: &TimeQuat, pad_n: usize) -> TimeQuat {
    if pad_n == 0 || quats.len() < 2 {
        return quats.clone();
    }
    let entries: Vec<(i64, Quat64)> = quats.iter().map(|(k, v)| (*k, *v)).collect();
    let n = entries.len();
    let pad_n = pad_n.min(n - 1);

    let head_ts0 = entries[0].0;
    let tail_ts_last = entries[n - 1].0;
    let dt_head = (entries[1].0 - entries[0].0).max(1);
    let dt_tail = (entries[n - 1].0 - entries[n - 2].0).max(1);

    let head_quat = entries[0].1;
    let tail_quat = entries[n - 1].1;

    let mut out: TimeQuat = BTreeMap::new();
    // Head pad: reflect entries[1..=pad_n] around entries[0] (q_0)
    // pad position i ∈ [0, pad_n) maps to src index (pad_n - i) ∈ [1, pad_n]
    for i in 0..pad_n {
        let src_idx = pad_n - i;
        let pad_ts = head_ts0 - dt_head * (pad_n - i) as i64;
        let src_quat = entries[src_idx].1;
        let reflected = head_quat * src_quat.inverse() * head_quat;
        out.insert(pad_ts, reflected);
    }
    // Real data
    for (k, v) in &entries {
        out.insert(*k, *v);
    }
    // Tail pad: reflect entries[n-2..n-1-pad_n] around entries[n-1] (q_last)
    for i in 0..pad_n {
        let src_idx = n - 2 - i;
        let pad_ts = tail_ts_last + dt_tail * (i + 1) as i64;
        let src_quat = entries[src_idx].1;
        let reflected = tail_quat * src_quat.inverse() * tail_quat;
        out.insert(pad_ts, reflected);
    }
    out
}

fn trim_pad(padded: TimeQuat, first_ts: i64, last_ts: i64) -> TimeQuat {
    padded
        .into_iter()
        .filter(|(ts, _)| *ts >= first_ts && *ts <= last_ts)
        .collect()
}

// Rotation about the local z (optical) axis contained in `q`, so that q = swing * twist.
fn twist_z(q: &Quat64) -> Quat64 {
    let (w, k) = (q.quaternion().w, q.quaternion().k);
    let n = (w * w + k * k).sqrt();
    if n < 1e-15 {
        Quat64::identity()
    } else {
        Quat64::new_unchecked(Quaternion::new(w / n, 0.0, 0.0, k / n))
    }
}

// Move `q` towards `x` by fraction `f_swing` of the swing and `f_roll` of the twist.
fn split_step(q: &Quat64, x: &Quat64, f_swing: f64, f_roll: f64) -> Quat64 {
    let delta = q.inverse() * x;
    let twist = twist_z(&delta);
    let swing = delta * twist.inverse();
    q * swing.powf(f_swing) * twist.powf(f_roll)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE_HZ: f64 = 200.0;
    const DURATION_S: f64 = 6.0;

    fn motion(f: impl Fn(f64) -> Quat64) -> TimeQuat {
        (0..(RATE_HZ * DURATION_S) as i64)
            .map(|i| ((i as f64 * 1e6 / RATE_HZ) as i64, f(i as f64 / RATE_HZ)))
            .collect()
    }

    fn smooth_at_fov(quats: &TimeQuat, diagonal_fov: f64) -> TimeQuat {
        let algo = DefaultAlgo {
            trim_range_only: false,
            ..Default::default()
        };
        let params = ComputeParams {
            scaled_fps: 50.0,
            scaled_duration_ms: DURATION_S * 1000.0,
            camera_diagonal_fovs: vec![diagonal_fov],
            ..Default::default()
        };
        algo.smooth(quats, DURATION_S * 1000.0, &params)
    }

    fn max_difference_deg(a: &TimeQuat, b: &TimeQuat) -> f64 {
        a.values()
            .zip(b.values())
            .map(|(x, y)| (x.inverse() * y).angle().to_degrees())
            .fold(0.0, f64::max)
    }

    // Accumulated rotation about the optical axis, away from the padded ends.
    fn roll_rms_deg(quats: &TimeQuat) -> f64 {
        let values: Vec<_> = quats.values().collect();
        let mut roll = 0.0;
        let mut path = Vec::new();
        for pair in values.windows(2) {
            let twist = twist_z(&(pair[0].inverse() * pair[1]));
            roll += 2.0 * twist.quaternion().k.atan2(twist.quaternion().w);
            path.push(roll.to_degrees());
        }
        let inner = &path[path.len() / 6..path.len() * 5 / 6];
        let mean = inner.iter().sum::<f64>() / inner.len() as f64;
        (inner.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / inner.len() as f64).sqrt()
    }

    fn roll(deg: f64) -> Quat64 {
        Quat64::from_axis_angle(&Vector3::z_axis(), deg.to_radians())
    }

    fn pan(deg: f64) -> Quat64 {
        Quat64::from_axis_angle(&Vector3::y_axis(), deg.to_radians())
    }

    #[test]
    fn roll_smoothing_does_not_depend_on_focal_length() {
        let quats = motion(|t| {
            roll(20.0 * (std::f64::consts::TAU * 0.4 * t).sin() + 0.5 * (std::f64::consts::TAU * 2.0 * t).sin())
        });
        let long_lens = smooth_at_fov(&quats, 40.0);
        let wide_lens = smooth_at_fov(&quats, 120.0);
        assert!(max_difference_deg(&long_lens, &wide_lens) < 1e-9);
    }

    #[test]
    fn panning_does_not_relax_roll() {
        let jitter = |t: f64| 0.5 * (std::f64::consts::TAU * 2.0 * t).sin();
        let still = smooth_at_fov(&motion(|t| roll(jitter(t))), 120.0);
        let panning = smooth_at_fov(&motion(|t| pan(60.0 * t) * roll(jitter(t))), 120.0);
        let (still_rms, panning_rms) = (roll_rms_deg(&still), roll_rms_deg(&panning));
        assert!(panning_rms < still_rms * 1.25 + 1e-3, "still {still_rms} panning {panning_rms}");
    }

    #[test]
    fn pan_smoothing_still_scales_with_focal_length() {
        let quats = motion(|t| pan(20.0 * (std::f64::consts::TAU * 0.4 * t).sin()));
        let long_lens = smooth_at_fov(&quats, 40.0);
        let wide_lens = smooth_at_fov(&quats, 120.0);
        assert!(max_difference_deg(&long_lens, &wide_lens) > 0.1);
    }

    #[test]
    fn zero_max_zoom_limit_turns_smoothing_off() {
        let quats = motion(|t| pan(20.0 * (std::f64::consts::TAU * 0.4 * t).sin()) * roll(t * 30.0));
        for per_axis in [false, true] {
            let algo = DefaultAlgo {
                trim_range_only: false,
                per_axis,
                ..Default::default()
            };
            let params = ComputeParams {
                scaled_fps: 50.0,
                scaled_duration_ms: DURATION_S * 1000.0,
                camera_diagonal_fovs: vec![90.0],
                smoothing_fov_limit_per_frame: vec![0.0; (50.0 * DURATION_S) as usize],
                ..Default::default()
            };
            let smoothed = algo.smooth(&quats, DURATION_S * 1000.0, &params);
            assert!(max_difference_deg(&smoothed, &quats) < 1e-9, "per_axis={per_axis}");
        }
    }
}

