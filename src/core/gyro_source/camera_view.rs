// SPDX-License-Identifier: GPL-3.0-or-later

use std::{collections::BTreeMap, sync::Arc};

use nalgebra::{Matrix3, Vector3};
use parking_lot::RwLock;

use super::{GyroSource, Quat64, TimeQuat};
use crate::stabilization::{ComputeParams, FrameTransform, is_valid_point, undistort_points};

/// Keep smoothing on the recorded view. The renderer still needs the original
/// body orientation to undo IBIS/OIS/EIS and to correct rolling shutter.
pub(super) fn smoothing_quaternions(
    gyro: &GyroSource,
    body_quaternions: &TimeQuat,
    params: &ComputeParams,
) -> TimeQuat {
    let metadata = gyro.file_metadata.read();
    if !(metadata.has_camera_view_compensation() || params.optical_stab_checksum != 0)
        || body_quaternions.is_empty()
        || params.frame_count == 0
        || !(params.scaled_fps > 0.0)
    {
        return body_quaternions.clone();
    }
    let frame_offsets = metadata.per_frame_time_offsets.clone();
    drop(metadata);

    // A separate gyro lock avoids re-entering the caller's read guard when a
    // writer is queued. Empty quaternion tables leave only the image geometry.
    let mut geometry_gyro = GyroSource::new();
    geometry_gyro.file_metadata = gyro.file_metadata.clone();
    geometry_gyro.optical_stab = gyro.optical_stab.clone();
    geometry_gyro.set_offsets(gyro.get_offsets().clone());
    let mut geometry_params = params.clone();
    geometry_params.gyro = Arc::new(RwLock::new(geometry_gyro));
    geometry_params.framebuffer_inverted = false;
    geometry_params.suppress_rotation = false;

    let centre = (params.width as f32 / 2.0, params.height as f32 / 2.0);
    let points = [centre, (centre.0 + params.width as f32 / 40.0, centre.1)];
    let flip = Matrix3::from_diagonal(&Vector3::new(-1.0, 1.0, 1.0));
    let mut view_offsets = BTreeMap::new();
    for frame in 0..params.frame_count {
        let timestamp_ms = crate::timestamp_at_frame(frame as i32, params.scaled_fps);
        let (camera, coefficients, _, _, mut shifts, mesh, _, _) =
            FrameTransform::at_timestamp_for_points(
                &geometry_params,
                &points,
                timestamp_ms,
                Some(frame),
                false,
            );
        // With rolling shutter disabled the centre sample represents every
        // point in the frame, including the second ray used to measure roll.
        if let Some(shifts) = shifts.as_mut().filter(|shifts| shifts.len() == 1) {
            shifts.resize(points.len(), shifts[0]);
        }
        // Normalized rays before video rotation and output crop. Using the
        // forward projection keeps the same lens, mesh and shift conventions
        // as the renderer instead of maintaining a second IS formula.
        let rays = undistort_points(
            &points,
            camera,
            &coefficients,
            Matrix3::identity(),
            None,
            None,
            &geometry_params,
            1.0,
            1.0,
            timestamp_ms,
            shifts,
            mesh,
            0.0,
        );
        if rays
            .iter()
            .any(|p| !is_valid_point(*p) || p.0.abs() > 1e10 || p.1.abs() > 1e10)
        {
            return body_quaternions.clone();
        }
        let forward = Vector3::new(rays[0].0 as f64, rays[0].1 as f64, 1.0).normalize();
        let right = Vector3::new(rays[1].0 as f64, rays[1].1 as f64, 1.0).normalize();
        let Some(right) = (right - forward * right.dot(&forward)).try_normalize(1e-12) else {
            return body_quaternions.clone();
        };
        let down = forward.cross(&right);
        let offset =
            Quat64::from_matrix(&(flip * Matrix3::from_columns(&[right, down, forward]) * flip));
        let video_time_ms = timestamp_ms + frame_offsets.get(frame).copied().unwrap_or_default();
        let gyro_time_ms = video_time_ms - gyro.offset_at_video_timestamp(video_time_ms);
        view_offsets.insert((gyro_time_ms * 1000.0).round() as i64, offset);
    }

    // Interpolate just the frame-level view offset, retaining the original
    // gyro sampling rate and all of its motion between video frames.
    body_quaternions
        .iter()
        .map(|(&timestamp, body)| {
            let offset = GyroSource::clamped_quat_at_gyro_timestamp(
                &view_offsets,
                timestamp as f64 / 1000.0,
            );
            (timestamp, body * offset)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        StabilizationManager,
        gyro_source::{CameraStabData, splines::CatmullRom},
    };

    fn compensated_fixture() -> StabilizationManager {
        let manager = StabilizationManager::default();
        manager.init_from_video_data(600.0, 10.0, 6, (1920, 1080));
        manager.set_size(1920, 1080);
        manager.set_output_size(1920, 1080);
        manager
            .lens
            .write()
            .load_from_json_value(&serde_json::json!({
                "calib_dimension": {"w":1920,"h":1080},
                "distortion_model":"opencv_standard",
                "fisheye_params": {
                    "camera_matrix":[[1000.0,0.0,960.0],[0.0,1000.0,540.0],[0.0,0.0,1.0]],
                    "distortion_coeffs":[0.0,0.0,0.0,0.0]
                }
            }));
        let mut gyro = manager.gyro.write();
        gyro.duration_ms = 600.0;
        let mut metadata = gyro.file_metadata.write();
        metadata.detected_source = Some("Sony test".into());
        for frame in 0..6 {
            let yaw = (frame as f64 - 2.0) * 0.01;
            let mut ibis = CatmullRom::new();
            for row in [0.0, 1080.0] {
                ibis.add_point(row, Vector3::new(1000.0 * yaw.tan(), 0.0, 0.0));
            }
            metadata.camera_stab_data.push(CameraStabData {
                sensor_size: (1920, 1080),
                crop_area: (0.0, 0.0, 1920.0, 1080.0),
                pixel_pitch: (1, 1),
                ibis_spline: ibis,
                ..Default::default()
            });
        }
        drop(metadata);
        gyro.quaternions = (0..6)
            .map(|frame| {
                (
                    frame * 100_000,
                    Quat64::from_euler_angles(0.0, (frame as f64 - 2.0) * 0.01, 0.0),
                )
            })
            .collect();
        drop(gyro);
        manager
    }

    #[test]
    fn smoothing_follows_a_reconstructed_view() {
        let recorded = compensated_fixture();
        let params = ComputeParams::from_manager(&recorded);
        let gyro = recorded.gyro.read();
        let expected = smoothing_quaternions(&gyro, &gyro.quaternions, &params);
        drop(gyro);
        let reconstructed = compensated_fixture();
        {
            let mut gyro = reconstructed.gyro.write();
            gyro.file_metadata.write().camera_stab_data.clear();
            gyro.file_metadata.write().detected_source = Some("Nikon test".into());
            gyro.optical_stab = Some(crate::gyro_source::OpticalStabReconstruction::from_samples(
                (0..6).map(|frame| (frame * 100_000, [((frame as f64 - 2.0) * 0.01).tan(), 0.0, 0.0])).collect()
            ));
        }
        let mut params = ComputeParams::from_manager(&reconstructed);
        params.calculate_camera_fovs();
        assert_ne!(params.optical_stab_checksum, 0);
        assert!(params.smoothing_uses_camera_view);
        let gyro = reconstructed.gyro.read();
        let actual = smoothing_quaternions(&gyro, &gyro.quaternions, &params);
        for (time, quat) in expected { assert!((quat.inverse() * actual[&time]).angle() <= 1e-6); }
        let smoothing = reconstructed.smoothing.read();
        let checksum = smoothing.get_state_checksum(0, &params);
        params.optical_stab_checksum += 1;
        assert_ne!(smoothing.get_state_checksum(0, &params), checksum);
    }

    #[test]
    fn reconstructed_view_uses_frame_and_user_offsets() {
        let manager = compensated_fixture();
        {
            let mut gyro = manager.gyro.write();
            {
                let mut metadata = gyro.file_metadata.write();
                metadata.detected_source = Some("Nikon test".into());
                metadata.camera_stab_data.clear();
                metadata.per_frame_time_offsets = vec![25.0; 6];
            }
            gyro.set_offset(0, 10.0);
            gyro.quaternions = gyro.quaternions.iter().map(|(&time, &quat)| (time + 15_000, quat)).collect();
            gyro.optical_stab = Some(crate::gyro_source::OpticalStabReconstruction::from_samples(
                (0..6).map(|frame| (frame * 100_000 + 15_000, [((frame as f64 - 2.0) * 0.01).tan(), 0.0, 0.0])).collect()
            ));
        }
        let mut params = ComputeParams::from_manager(&manager);
        params.calculate_camera_fovs();
        let gyro = manager.gyro.read();
        for quat in smoothing_quaternions(&gyro, &gyro.quaternions, &params).values() {
            assert!(quat.angle() < 1e-6, "reconstructed view sampled at the wrong gyro time");
        }
    }

    #[test]
    fn sony_smoothing_follows_view_when_ibis_cancels_body_motion() {
        let manager = compensated_fixture();
        let params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        let view = smoothing_quaternions(&gyro, &gyro.quaternions, &params);
        assert_eq!(
            view.keys().collect::<Vec<_>>(),
            gyro.quaternions.keys().collect::<Vec<_>>()
        );
        for quat in view.values() {
            assert!(
                quat.angle() < 1e-6,
                "a stationary view must stay stationary: {}",
                quat.angle()
            );
        }
    }

    #[test]
    fn sony_smoothing_preserves_the_compensated_frame_centre() {
        let manager = compensated_fixture();
        let params = ComputeParams::from_manager(&manager);
        let before = crate::stabilization::undistort_points_with_rolling_shutter(
            &[(960.0, 540.0)],
            0.0,
            Some(0),
            &params,
            1.0,
            false,
            false,
        )[0];
        assert!(
            (before.0 - 960.0).abs() > 10.0,
            "the body-only target must reproduce the unwanted IS reversal"
        );
        let original = manager.gyro.read().quaternions.clone();
        for algorithm in [0, 1] {
            manager.smoothing.write().set_current(algorithm);
            manager.recompute_smoothness();
            assert_eq!(manager.gyro.read().quaternions, original);
            let params = ComputeParams::from_manager(&manager);
            for frame in 0..6 {
                let point = crate::stabilization::undistort_points_with_rolling_shutter(
                    &[(960.0, 540.0)],
                    frame as f64 * 100.0,
                    Some(frame),
                    &params,
                    1.0,
                    false,
                    false,
                )[0];
                assert!(
                    (point.0 - 960.0).abs() < 0.001 && (point.1 - 540.0).abs() < 0.001,
                    "smoothing must not restore the body shake: {point:?}"
                );
            }
        }
    }

    #[test]
    fn sony_view_uses_ois_and_sensor_roll_with_the_render_conventions() {
        for sensor_roll in [false, true] {
            let manager = compensated_fixture();
            {
                let mut gyro = manager.gyro.write();
                let mut metadata = gyro.file_metadata.write();
                for (frame, stab) in metadata.camera_stab_data.iter_mut().enumerate() {
                    let angle = (frame as f64 - 2.0) * 0.01;
                    stab.ibis_spline = CatmullRom::new();
                    for row in [0.0, 1080.0] {
                        if sensor_roll {
                            stab.ibis_spline.add_point(
                                row,
                                Vector3::new(0.0, 0.0, angle.to_degrees() * 1000.0),
                            );
                        } else {
                            stab.ois_spline
                                .add_point(row, Vector3::new(-1000.0 * angle.tan(), 0.0, 0.0));
                        }
                    }
                }
                drop(metadata);
                if sensor_roll {
                    for (&timestamp, quat) in gyro.quaternions.iter_mut() {
                        *quat = Quat64::from_euler_angles(
                            0.0,
                            0.0,
                            (timestamp as f64 / 100_000.0 - 2.0) * 0.01,
                        );
                    }
                }
            }
            let params = ComputeParams::from_manager(&manager);
            let gyro = manager.gyro.read();
            for quat in smoothing_quaternions(&gyro, &gyro.quaternions, &params).values() {
                assert!(
                    quat.angle() < 1e-6,
                    "IS direction disagrees with the renderer"
                );
            }
        }
    }

    #[test]
    fn view_offsets_use_frame_and_user_offsets_on_the_gyro_timeline() {
        let manager = compensated_fixture();
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata.write().per_frame_time_offsets = vec![25.0; 6];
            gyro.set_offset(0, 10.0);
            gyro.quaternions = gyro
                .quaternions
                .iter()
                .map(|(&timestamp, &quat)| (timestamp + 15_000, quat))
                .collect();
        }
        let params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        for quat in smoothing_quaternions(&gyro, &gyro.quaternions, &params).values() {
            assert!(quat.angle() < 1e-6, "view offset sampled at the wrong time");
        }
    }

    #[test]
    fn non_sony_and_missing_compensation_keep_body_quaternions() {
        let manager = compensated_fixture();
        let params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        gyro.file_metadata.write().detected_source = Some("Canon".into());
        assert_eq!(
            smoothing_quaternions(&gyro, &gyro.quaternions, &params),
            gyro.quaternions
        );
        let mut metadata = gyro.file_metadata.write();
        metadata.detected_source = Some("Sony".into());
        metadata.camera_stab_data.clear();
        drop(metadata);
        assert_eq!(
            smoothing_quaternions(&gyro, &gyro.quaternions, &params),
            gyro.quaternions
        );
    }

    fn max_rotation_difference(first: &TimeQuat, second: &TimeQuat) -> f64 {
        assert_eq!(first.len(), second.len());
        first
            .iter()
            .map(|(ts, q)| q.angle_to(&second[ts]))
            .fold(0.0, f64::max)
    }

    fn assert_camera_view_cache_matches_fresh(manager: &StabilizationManager, before: &TimeQuat) {
        manager.recompute_blocking();
        let cached = manager.gyro.read().smoothed_quaternions.clone();
        manager.invalidate_smoothing();
        manager.recompute_blocking();
        let fresh = manager.gyro.read().smoothed_quaternions.clone();
        assert!(
            max_rotation_difference(before, &fresh) > 1e-3,
            "the changed projection must affect the recorded view"
        );
        assert!(
            max_rotation_difference(&cached, &fresh) < 1e-12,
            "projection change reused a stale smoothing target: {} rad",
            max_rotation_difference(&cached, &fresh)
        );
    }

    #[test]
    fn camera_view_state_tracks_refraction_changes() {
        let manager = compensated_fixture();
        manager.set_smoothing_method(0);
        manager.set_max_zoom(0.0, 1);
        manager.recompute_blocking();
        manager.recompute_blocking();
        let before = manager.gyro.read().smoothed_quaternions.clone();
        manager.set_light_refraction_coefficient(1.33);
        assert_camera_view_cache_matches_fresh(&manager, &before);
    }

    #[test]
    fn camera_view_state_tracks_refraction_keyframes() {
        let manager = compensated_fixture();
        manager.set_smoothing_method(0);
        manager.set_max_zoom(0.0, 1);
        manager.recompute_blocking();
        manager.recompute_blocking();
        let before = manager.gyro.read().smoothed_quaternions.clone();
        manager.set_keyframe(&crate::KeyframeType::LightRefractionCoeff, 0, 1.33);
        assert_camera_view_cache_matches_fresh(&manager, &before);
        manager.clear_keyframes_type(&crate::KeyframeType::LightRefractionCoeff);
        manager.recompute_blocking();
        assert!(
            max_rotation_difference(&before, &manager.gyro.read().smoothed_quaternions) < 1e-12
        );
    }

    #[test]
    fn camera_view_state_tracks_lens_delay_even_when_focal_length_is_constant() {
        let manager = compensated_fixture();
        manager.set_smoothing_method(0);
        manager.set_max_zoom(0.0, 1);
        manager
            .lens
            .write()
            .fisheye_params
            .distortion_coeffs
            .clear();
        manager.gyro.read().file_metadata.write().lens_params = (0..6)
            .map(|frame| {
                (
                    frame * 100_000,
                    super::super::LensParams {
                        pixel_focal_length: Some((1000.0, 1000.0)),
                        principal_point: Some((960.0 + frame as f32 * 20.0, 540.0)),
                        ..Default::default()
                    },
                )
            })
            .collect();
        manager.recompute_blocking();
        manager.recompute_blocking();
        let before = manager.gyro.read().smoothed_quaternions.clone();
        manager.params.write().lens_metadata_delay_frames = 1;
        assert_camera_view_cache_matches_fresh(&manager, &before);
    }

    #[test]
    fn camera_view_does_not_apply_video_rotation_to_the_smoothing_input() {
        let manager = compensated_fixture();
        let mut params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        let reference = smoothing_quaternions(&gyro, &gyro.quaternions, &params);
        for rotation in [90.0, 180.0, -90.0] {
            params.video_rotation = rotation;
            params
                .keyframes
                .set(&crate::KeyframeType::VideoRotation, 0, rotation);
            let rotated = smoothing_quaternions(&gyro, &gyro.quaternions, &params);
            assert!(max_rotation_difference(&reference, &rotated) < 1e-12);
        }
    }

    #[test]
    fn camera_view_applies_requested_additional_rotation_once() {
        let manager = compensated_fixture();
        manager.set_smoothing_method(0);
        manager.set_additional_rotation_x(15.0);
        manager.set_additional_rotation_y(-10.0);
        manager.set_additional_rotation_z(5.0);
        manager.recompute_smoothness();
        let target = Quat64::from_euler_angles(
            (-10.0f64).to_radians(),
            15.0f64.to_radians(),
            5.0f64.to_radians(),
        );
        let gyro = manager.gyro.read();
        for (ts, correction) in &gyro.smoothed_quaternions {
            let actual = gyro.quaternions[ts] * correction.inverse();
            assert!(actual.angle_to(&target) < 1e-6);
        }
    }

    #[test]
    fn camera_view_preserves_gravity_horizon_when_ibis_cancels_roll() {
        let manager = compensated_fixture();
        {
            let mut gyro = manager.gyro.write();
            gyro.use_gravity_vectors = true;
            gyro.quaternions = (0..6)
                .map(|frame| {
                    (
                        frame * 100_000,
                        Quat64::from_euler_angles(0.0, 0.0, (frame as f64 - 2.0) * 0.01),
                    )
                })
                .collect();
            let mut metadata = gyro.file_metadata.write();
            metadata.gravity_vectors = Some(
                (0..6)
                    .map(|frame| {
                        let angle = (frame as f64 - 2.0) * 0.01;
                        (
                            frame * 100_000,
                            Vector3::new(-angle.sin(), angle.cos(), 0.0),
                        )
                    })
                    .collect(),
            );
            for (frame, stab) in metadata.camera_stab_data.iter_mut().enumerate() {
                stab.ibis_spline = CatmullRom::new();
                for row in [0.0, 1080.0] {
                    stab.ibis_spline.add_point(
                        row,
                        Vector3::new(
                            0.0,
                            0.0,
                            ((frame as f64 - 2.0) * 0.01).to_degrees() * 1000.0,
                        ),
                    );
                }
            }
        }
        manager.set_horizon_lock(
            100.0,
            0.0,
            false,
            0.0,
            false,
            5.0,
            500.0,
            1.0,
            f64::INFINITY,
        );
        for algorithm in [0, 1] {
            manager.set_smoothing_method(algorithm);
            manager.recompute_smoothness();
            let gyro = manager.gyro.read();
            for (ts, correction) in &gyro.smoothed_quaternions {
                let target = gyro.quaternions[ts] * correction.inverse();
                assert!(
                    target.angle() < 1e-6,
                    "horizon lock restored the cancelled sensor roll"
                );
            }
        }
    }

    #[test]
    fn zero_camera_view_offset_keeps_horizon_results_unchanged() {
        let manager = compensated_fixture();
        for stab in &mut manager.gyro.read().file_metadata.write().camera_stab_data {
            stab.ibis_spline = CatmullRom::new();
        }
        manager.set_horizon_lock(
            100.0,
            0.0,
            false,
            0.0,
            false,
            5.0,
            500.0,
            1.0,
            f64::INFINITY,
        );
        manager.recompute_smoothness();
        let sony = manager.gyro.read().smoothed_quaternions.clone();
        manager.gyro.read().file_metadata.write().detected_source = Some("Canon".into());
        manager.recompute_smoothness();
        assert!(max_rotation_difference(&sony, &manager.gyro.read().smoothed_quaternions) < 1e-12);
    }

    #[test]
    fn dynamic_focal_length_preserves_the_compensated_view_and_gyro_rate() {
        let manager = compensated_fixture();
        manager
            .lens
            .write()
            .fisheye_params
            .distortion_coeffs
            .clear();
        let mut gyro = manager.gyro.write();
        gyro.quaternions = (0..=50)
            .map(|sample| {
                (
                    sample * 10_000,
                    Quat64::from_euler_angles(0.0, (sample as f64 / 10.0 - 2.0) * 0.01, 0.0),
                )
            })
            .collect();
        let mut metadata = gyro.file_metadata.write();
        for frame in 0..6 {
            let focal = 500.0 + frame as f64 * 200.0;
            metadata.lens_params.insert(
                frame as i64 * 100_000,
                super::super::LensParams {
                    pixel_focal_length: Some((focal as f32, focal as f32)),
                    principal_point: Some((960.0, 540.0)),
                    ..Default::default()
                },
            );
            metadata.camera_stab_data[frame].ibis_spline = CatmullRom::new();
            for row in [0.0, 1080.0] {
                metadata.camera_stab_data[frame].ibis_spline.add_point(
                    row,
                    Vector3::new(focal * ((frame as f64 - 2.0) * 0.01).tan(), 0.0, 0.0),
                );
            }
        }
        drop(metadata);
        drop(gyro);
        let params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        let view = smoothing_quaternions(&gyro, &gyro.quaternions, &params);
        assert_eq!(view.len(), 51);
        for q in view.values() {
            assert!(q.angle() < 1e-6);
        }
    }

    #[test]
    fn anamorphic_poly5_view_stays_continuous_when_sensor_shift_crosses_zero() {
        let manager = compensated_fixture();
        manager.lens.write().load_from_json_value(&serde_json::json!({
            "calib_dimension": {"w":1920,"h":1620},
            "input_vertical_stretch":1.5,
            "distortion_model":"poly5",
            "fisheye_params": {
                "camera_matrix":[[1000.0,0.0,960.0],[0.0,1000.0,810.0],[0.0,0.0,1.0]],
                "distortion_coeffs":[-0.2,0.0,0.0,0.0]
            }
        }));
        {
            let mut gyro = manager.gyro.write();
            for quat in gyro.quaternions.values_mut() { *quat = Quat64::identity(); }
            for (frame, stab) in gyro.file_metadata.write().camera_stab_data.iter_mut().enumerate() {
                let shift = [0.0, 0.1, 0.0, -0.1, 0.0, 0.1][frame];
                stab.ibis_spline = CatmullRom::new();
                for row in [0.0, 1080.0] {
                    stab.ibis_spline.add_point(row, Vector3::new(shift, 0.0, 0.0));
                }
            }
        }
        let params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        let view = smoothing_quaternions(&gyro, &gyro.quaternions, &params);
        for (&time, quat) in &view {
            assert!(quat.angle() < 0.001, "subpixel shift produced a large view rotation at {time}");
        }
        for frame in [0, 2, 4] {
            assert!(view[&(frame * 100_000)].angle() < 1e-12);
        }
        assert!(view[&100_000].angle() > 1e-6, "valid sensor compensation was discarded");
    }

    #[test]
    fn invalid_point_sentinel_falls_back_to_body_quaternions() {
        let manager = compensated_fixture();
        {
            let mut lens = manager.lens.write();
            lens.distortion_model = Some("poly5".into());
            lens.fisheye_params.distortion_coeffs = vec![-0.2, 0.0, 0.0, 0.0];
            lens.fisheye_params.camera_matrix[0][0] = 1.0;
            lens.fisheye_params.camera_matrix[1][1] = 1.0;
            lens.init();
        }
        for stab in &mut manager.gyro.read().file_metadata.write().camera_stab_data {
            stab.ibis_spline = CatmullRom::new();
            for row in [0.0, 1080.0] {
                stab.ibis_spline.add_point(row, Vector3::new(0.5, 0.0, 0.0));
            }
        }
        let params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        assert_eq!(
            smoothing_quaternions(&gyro, &gyro.quaternions, &params),
            gyro.quaternions
        );
    }

    #[test]
    fn invalid_camera_projection_falls_back_to_body_quaternions() {
        let manager = compensated_fixture();
        manager.lens.write().fisheye_params.camera_matrix[0][0] = 0.0;
        let params = ComputeParams::from_manager(&manager);
        let gyro = manager.gyro.read();
        assert_eq!(
            smoothing_quaternions(&gyro, &gyro.quaternions, &params),
            gyro.quaternions
        );
    }

    #[test]
    fn non_sony_refraction_change_keeps_existing_smoothing_cache_behavior() {
        let manager = compensated_fixture();
        manager.gyro.read().file_metadata.write().detected_source = Some("Canon".into());
        manager.set_max_zoom(0.0, 1);
        manager.recompute_blocking();
        let sentinel: TimeQuat = manager
            .gyro
            .read()
            .smoothed_quaternions
            .keys()
            .map(|&ts| (ts, Quat64::from_euler_angles(0.005, 0.01, 0.003)))
            .collect();
        manager.gyro.write().smoothed_quaternions = sentinel.clone();
        manager.set_light_refraction_coefficient(1.33);
        manager.recompute_blocking();
        assert_eq!(manager.gyro.read().smoothed_quaternions, sentinel);
    }
}
