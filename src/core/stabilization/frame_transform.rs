// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use super::{ComputeParams, KernelParams};
use crate::gyro_source::FileMetadata;
use crate::keyframes::KeyframeType;
use crate::util::{MapClosest, map_coord};
use nalgebra::Matrix3;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

#[derive(Default, Clone)]
pub struct FrameTransform {
    pub matrices: Vec<[f32; 14]>,
    pub kernel_params: super::KernelParams,
    pub fov: f64,
    pub minimal_fov: f64,
    pub focal_length: Option<f64>,
    pub mesh_data: Vec<f32>,
}

impl FrameTransform {
    /// `detected_source` is the camera type optionally followed by ` <model>`, so a
    /// Sony clip reads either "Sony" or "Sony <model>". Both forms are matched
    /// explicitly so an unrelated brand whose name merely starts with the same
    /// letters cannot slip through.
    fn detected_source_is_sony(detected_source: Option<&str>) -> bool {
        detected_source.is_some_and(|s| s == "Sony" || s.starts_with("Sony "))
    }

    /// Scales a whole-sensor readout time down to the rows this clip actually
    /// captured, i.e. `captured rows / sensor rows`.
    ///
    /// Sony reports `Imager::FrameReadoutTime` for the full sensor height, but a
    /// cropped readout mode only scans part of it, so rolling shutter correction
    /// must be scaled or it over-corrects by the inverse of the crop ratio. Two
    /// independent proofs that the tag is whole-sensor: the per-row time stays
    /// constant across modes on one body (5.0067 us on a ZV-E10M2 in both the
    /// full-height and the 2/3-height mode, even though the tag value differs), and
    /// `gyro_source::sony::stab_calc_splines` already divides the very same tag by
    /// the full sensor height to build its IBIS/OIS spline domain.
    ///
    /// NIYIEN DEVIATION FROM UPSTREAM - keep the `is_sony` guard across upstream
    /// merges. Upstream scales unconditionally because upstream can only ever obtain
    /// a readout time from in-camera telemetry. This fork additionally injects
    /// readout times from camera_db, whose values are measured per shooting mode and
    /// therefore already describe the captured rows; scaling those would subtract
    /// the crop a second time. Nikon ZR is the concrete case: telemetry-parser hands
    /// it both size fields, yet its readout time comes from camera_db. Dropping this
    /// guard raises no compile error and produces no merge conflict, so re-check
    /// this function whenever upstream is merged.
    fn readout_crop_scale(
        is_sony: bool,
        capture_area_height: Option<f32>,
        sensor_height_px: Option<u32>,
    ) -> f64 {
        if !is_sony {
            return 1.0;
        }
        match (capture_area_height, sensor_height_px) {
            // Rejecting zero and NaN is defensive only: valid telemetry never hits
            // it, but a bad value would otherwise turn the readout time into
            // inf/NaN and destroy every row transform of the frame.
            (Some(capture), Some(sensor)) if sensor > 0 && capture > 0.0 => {
                capture as f64 / sensor as f64
            }
            _ => 1.0,
        }
    }

    fn get_frame_readout_time(
        params: &ComputeParams,
        can_invert: bool,
        timestamp_ms: f64,
        file_metadata: &FileMetadata,
    ) -> f64 {
        let mut frame_readout_time = params.frame_readout_time.abs();

        // The Sony check gates the lookup itself, not just the arithmetic: a
        // non-Sony source never reads lens_params, which makes "unchanged for every
        // other source" a property of the control flow rather than of the numbers
        // happening to come out as 1.0.
        let is_sony = Self::detected_source_is_sony(file_metadata.detected_source.as_deref());
        let closest = if is_sony {
            file_metadata
                .lens_params
                .get_closest(&((timestamp_ms * 1000.0).round() as i64), 100000) // closest within 100ms
        } else {
            None
        };
        let scale = Self::readout_crop_scale(
            is_sony,
            closest.and_then(|v| v.capture_area_size).map(|x| x.1),
            closest.and_then(|v| v.sensor_size_px).map(|x| x.1),
        );

        if can_invert
            && params.framebuffer_inverted
            && !params.frame_readout_direction.is_horizontal()
        {
            frame_readout_time *= -1.0;
        }
        if params.frame_readout_direction.is_inverted() {
            frame_readout_time *= -1.0;
        }
        frame_readout_time * scale
    }
    fn get_new_k(params: &ComputeParams, camera_matrix: &Matrix3<f64>, fov: f64) -> Matrix3<f64> {
        let horizontal_ratio = if params.lens.input_horizontal_stretch > 0.01 {
            params.lens.input_horizontal_stretch
        } else {
            1.0
        };

        let img_dim_ratio = 1.0 / horizontal_ratio;

        let out_dim = (params.output_width as f64, params.output_height as f64);
        //let focal_center = (params.video_width as f64 / 2.0, params.video_height as f64 / 2.0);

        let mut new_k = *camera_matrix;
        new_k[(0, 0)] = new_k[(0, 0)] * img_dim_ratio / fov;
        new_k[(1, 1)] = new_k[(1, 1)] * img_dim_ratio / fov;
        new_k[(0, 2)] = /*(params.video_width  as f64 / 2.0 - new_k[(0, 2)]) * img_dim_ratio / fov + */out_dim.0 / 2.0;
        new_k[(1, 2)] = /*(params.video_height as f64 / 2.0 - new_k[(1, 2)]) * img_dim_ratio / fov + */out_dim.1 / 2.0;
        new_k
    }
    fn get_fov(
        params: &ComputeParams,
        frame: usize,
        use_fovs: bool,
        timestamp_ms: f64,
        for_ui: bool,
    ) -> f64 {
        let mut fov_scale = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::Fov, timestamp_ms)
            .unwrap_or(params.fov_scale);
        fov_scale += if params.fov_overview && use_fovs && !for_ui {
            1.0
        } else {
            0.0
        };
        let mut fov = if use_fovs {
            params.fovs.get(frame).unwrap_or(if params.fovs.len() > 1 {
                params.fovs.last().unwrap()
            } else {
                &1.0
            }) * fov_scale
        } else {
            1.0
        }
        .max(0.001);
        fov *= params.width as f64 / params.output_width.max(1) as f64;
        fov
    }

    pub fn get_lens_data_at_timestamp(
        params: &ComputeParams,
        timestamp_ms: f64,
        invert_asym_lens: bool,
    ) -> (Matrix3<f64>, [f64; 12], f64, f64, f64, Option<f64>) {
        let mut interpolated_lens = None;
        let gyro = params.gyro.read();
        let file_metadata = gyro.file_metadata.read();
        if !file_metadata.lens_positions.is_empty() {
            if let Some(val) = file_metadata
                .lens_positions
                .get_closest(&((timestamp_ms * 1000.0).round() as i64), 100000)
            {
                // closest within 100ms
                interpolated_lens = Some(params.lens.get_interpolated_lens_at(*val));
            }
        }
        let lens = interpolated_lens.as_ref().unwrap_or(&params.lens);

        let mut focal_length = lens.focal_length;

        let mut camera_matrix =
            lens.get_camera_matrix((params.width, params.height), invert_asym_lens);
        let mut distortion_coeffs = lens.get_distortion_coeffs();

        let mut radial_distortion_limit = lens
            .fisheye_params
            .radial_distortion_limit
            .unwrap_or_default();

        let mut stretch_lens = true;
        let digital_zoom = file_metadata.digital_zoom.unwrap_or_default();

        if !file_metadata.lens_params.is_empty() && lens.fisheye_params.distortion_coeffs.len() < 4
        {
            if let Some(val) = file_metadata
                .lens_params
                .get_closest(&((timestamp_ms * 1000.0).round() as i64), 100000)
            {
                // closest within 100ms
                let pixel_focal_length = val.pixel_focal_length.map(|x| x as f64).or_else(|| {
                    focal_length = Some(val.focal_length? as f64);
                    Some(
                        (val.focal_length? as f64
                            / ((val.pixel_pitch?.1 as f64 / 1000000.0)
                                * val.capture_area_size?.1 as f64))
                            * params.height as f64,
                    )
                });
                if let Some(pfl) = pixel_focal_length {
                    if !lens.lens_group_override {
                        // println!("pfl: {pfl:.3}px, lens: {:?}", val);
                        camera_matrix[(0, 0)] = pfl;
                        camera_matrix[(1, 1)] = pfl;
                        camera_matrix[(0, 2)] = params.width as f64 / 2.0;
                        camera_matrix[(1, 2)] = params.height as f64 / 2.0;
                        stretch_lens = false;

                        if let Some(fl) = val.focal_length {
                            focal_length = Some(fl as f64);
                        }
                    }
                }
                if !val.distortion_coefficients.is_empty()
                    && val.distortion_coefficients.len() <= 12
                {
                    for (i, x) in val.distortion_coefficients.iter().enumerate() {
                        distortion_coeffs[i] = *x;
                    }

                    radial_distortion_limit = params
                        .distortion_model
                        .radial_distortion_limit(&distortion_coeffs)
                        .unwrap_or_default();
                }
            }
        }
        drop(file_metadata);
        drop(gyro);

        let (calib_width, calib_height) =
            if lens.calib_dimension.w > 0 && lens.calib_dimension.h > 0 {
                (lens.calib_dimension.w as f64, lens.calib_dimension.h as f64)
            } else {
                (params.width.max(1) as f64, params.height.max(1) as f64)
            };

        let input_horizontal_stretch = if lens.input_horizontal_stretch > 0.01 {
            lens.input_horizontal_stretch
        } else {
            1.0
        };
        let input_vertical_stretch = if lens.input_vertical_stretch > 0.01 {
            lens.input_vertical_stretch
        } else {
            1.0
        };

        if stretch_lens {
            let lens_ratiox = (params.width as f64 / calib_width) * input_horizontal_stretch;
            let lens_ratioy = (params.height as f64 / calib_height) * input_vertical_stretch;
            camera_matrix[(0, 0)] *= lens_ratiox;
            camera_matrix[(1, 1)] *= lens_ratioy;
            camera_matrix[(0, 2)] *= lens_ratiox;
            camera_matrix[(1, 2)] *= lens_ratioy;
        }
        if digital_zoom > 0.0 {
            camera_matrix[(0, 0)] *= digital_zoom;
            camera_matrix[(1, 1)] *= digital_zoom;
        }

        (
            camera_matrix,
            distortion_coeffs,
            radial_distortion_limit,
            input_horizontal_stretch,
            input_vertical_stretch,
            focal_length,
        )
    }

    pub fn at_timestamp(params: &ComputeParams, timestamp_ms: f64, frame: usize) -> Self {
        // ----------- Keyframes -----------
        let video_rotation = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::VideoRotation, timestamp_ms)
            .unwrap_or(params.video_rotation);
        let background_margin = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::BackgroundMargin, timestamp_ms)
            .unwrap_or(params.background_margin);
        let background_feather = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::BackgroundFeather, timestamp_ms)
            .unwrap_or(params.background_margin_feather);
        let lens_correction_amount = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::LensCorrectionStrength, timestamp_ms)
            .unwrap_or(params.lens_correction_amount);
        let adaptive_zoom_center_x = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::ZoomingCenterX, timestamp_ms)
            .unwrap_or(params.adaptive_zoom_center_offset.0);
        let mut adaptive_zoom_center_y = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::ZoomingCenterY, timestamp_ms)
            .unwrap_or(params.adaptive_zoom_center_offset.1);

        let light_refraction_coefficient = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::LightRefractionCoeff, timestamp_ms)
            .unwrap_or(params.light_refraction_coefficient);

        // let additional_translation_x = params.keyframes.value_at_video_timestamp(&KeyframeType::AdditionalTranslationX, timestamp_ms).unwrap_or(params.additional_translation.0) as f32;
        // let additional_translation_y = params.keyframes.value_at_video_timestamp(&KeyframeType::AdditionalTranslationY, timestamp_ms).unwrap_or(params.additional_translation.1) as f32;
        // let additional_translation_z = params.keyframes.value_at_video_timestamp(&KeyframeType::AdditionalTranslationZ, timestamp_ms).unwrap_or(params.additional_translation.2) as f32;
        // ----------- Keyframes -----------

        // ----------- Lens -----------
        let (
            camera_matrix,
            distortion_coeffs,
            radial_distortion_limit,
            input_horizontal_stretch,
            input_vertical_stretch,
            focal_length,
        ) = Self::get_lens_data_at_timestamp(params, timestamp_ms, false);
        // ----------- Lens -----------

        let lens_correction_amount = params.apply_anamorphic_decay(lens_correction_amount);

        let mut fov = Self::get_fov(params, frame, true, timestamp_ms, false);
        let mut ui_fov = Self::get_fov(params, frame, true, timestamp_ms, true);
        if let Some(adj) = params.lens.optimal_fov {
            if params.fovs.is_empty() {
                fov *= adj;
            } else {
                ui_fov /= adj;
            }
        }

        let scaled_k = camera_matrix;
        let new_k = Self::get_new_k(&params, &camera_matrix, fov);

        let gyro = params.gyro.read();
        let file_metadata = gyro.file_metadata.read();

        let mut mesh_data = Vec::new();
        if let Some(mc) = file_metadata.mesh_correction.get(frame) {
            mesh_data = mc.1.clone(); // undistorting mesh
        }

        // ----------- Rolling shutter correction -----------
        let frame_readout_time =
            Self::get_frame_readout_time(params, true, timestamp_ms, &file_metadata);

        let row_readout_time = frame_readout_time
            / if params.frame_readout_direction.is_horizontal() {
                params.width
            } else {
                params.height
            } as f64;
        let timestamp_ms = timestamp_ms
            + file_metadata
                .per_frame_time_offsets
                .get(frame)
                .unwrap_or(&0.0);
        let start_ts = timestamp_ms - (frame_readout_time / 2.0);
        // ----------- Rolling shutter correction -----------

        // let frame_period = 1000.0 / params.scaled_fps as f64;
        // dbg!(frame_period);

        let is_scale = if let Some(is) = file_metadata.camera_stab_data.get(frame) {
            (
                params.width as f64 / is.crop_area.2 as f64 / is.pixel_pitch.0 as f64,
                params.height as f64 / is.crop_area.3 as f64 / is.pixel_pitch.1 as f64
                    * (if params.framebuffer_inverted {
                        -1.0
                    } else {
                        1.0
                    }),
            )
        } else {
            (1.0, 1.0)
        };
        // let height_scale = params.video_height as f64 / params.height.max(1) as f64;

        let image_rotation = Matrix3::new_rotation(video_rotation * (std::f64::consts::PI / 180.0));

        let quat1 = gyro.org_quat_at_timestamp(timestamp_ms).inverse();
        let smoothed_quat1 = gyro.smoothed_quat_at_timestamp(timestamp_ms);

        // Only compute 1 matrix if not using rolling shutter correction
        let rows = if frame_readout_time.abs() > 0.0 {
            if params.frame_readout_direction.is_horizontal() {
                params.width
            } else {
                params.height
            }
        } else {
            1
        };

        let matrices = (0..rows)
            .into_par_iter()
            .map(|y| {
                let quat_time = if frame_readout_time.abs() > 0.0 {
                    start_ts + row_readout_time * y as f64
                } else {
                    start_ts
                };
                let quat = smoothed_quat1 * quat1 * gyro.org_quat_at_timestamp(quat_time);

                let mut r = image_rotation * *quat.to_rotation_matrix().matrix();
                if params.framebuffer_inverted {
                    r[(0, 2)] *= -1.0;
                    r[(1, 2)] *= -1.0;
                    r[(2, 0)] *= -1.0;
                    r[(2, 1)] *= -1.0;
                } else {
                    r[(0, 1)] *= -1.0;
                    r[(0, 2)] *= -1.0;
                    r[(1, 0)] *= -1.0;
                    r[(2, 0)] *= -1.0;
                }

                let (mut sx, mut sy, mut ra, mut ox, mut oy) =
                    if let Some(is) = file_metadata.camera_stab_data.get(frame) {
                        // let ts = ((row_readout_time * y as f64 + frame_period * frame as f64) * 1000.0).round() as i64;
                        let y_sensor = map_coord(
                            y as f64,
                            0.0,
                            params.height as f64,
                            is.crop_area.1 as f64,
                            is.crop_area.1 as f64 + is.crop_area.3 as f64,
                        );
                        let y_sensor = if params.framebuffer_inverted {
                            is.sensor_size.1 as f64 - y_sensor
                        } else {
                            y_sensor
                        };

                        let s = is
                            .ibis_spline
                            .interpolate(y_sensor + is.offset)
                            .unwrap_or_default();
                        let sx = s.x * is_scale.0;
                        let sy = s.y * is_scale.1;
                        let ra = s.z / 1000.0
                            * (if params.framebuffer_inverted {
                                -1.0
                            } else {
                                1.0
                            });

                        let o = is
                            .ois_spline
                            .interpolate(y_sensor + is.offset)
                            .unwrap_or_default();
                        let ox = o.x * is_scale.0;
                        let oy = o.y * is_scale.1;

                        // if y == 0 { log::debug!("IBIS data at frame: {frame}, ts: {ts}, sx: {sx:.3}, sy: {sy:.3}, ra: {ra:.3}, ox: {ox:.3}, oy: {oy:.3}"); }
                        (
                            sx as f32,
                            sy as f32,
                            ra.to_radians() as f32,
                            ox as f32,
                            oy as f32,
                        )
                    } else {
                        (0.0, 0.0, 0.0, 0.0, 0.0)
                    };

                if params.suppress_rotation {
                    r = Matrix3::identity();
                    if params.frame_readout_time == 0.0 {
                        sx = 0.0;
                        sy = 0.0;
                        ra = 0.0;
                        ox = 0.0;
                        oy = 0.0;
                    }
                }

                let i_r = (new_k * r).pseudo_inverse(0.000001);
                if let Err(err) = i_r {
                    log::error!(
                        "Failed to multiply matrices: {:?} * {:?}: {}",
                        new_k,
                        r,
                        err
                    );
                }
                let i_r: Matrix3<f32> = nalgebra::convert(i_r.unwrap_or_default());
                [
                    i_r[(0, 0)],
                    i_r[(0, 1)],
                    i_r[(0, 2)],
                    i_r[(1, 0)],
                    i_r[(1, 1)],
                    i_r[(1, 2)],
                    i_r[(2, 0)],
                    i_r[(2, 1)],
                    i_r[(2, 2)],
                    sx,
                    sy,
                    ra,
                    ox,
                    oy,
                ]
            })
            .collect::<Vec<[f32; 14]>>();
        drop(file_metadata);
        drop(gyro);

        let mut digital_lens_params = [0f32; 4];
        if let Some(p) = &params.digital_lens_params {
            for (i, v) in p.iter().enumerate() {
                digital_lens_params[i] = *v as f32;
            }
        }
        if params.framebuffer_inverted {
            adaptive_zoom_center_y *= -1.0;
        }

        let kernel_params = KernelParams {
            matrix_count: matrices.len() as i32,
            f: [scaled_k[(0, 0)] as f32, scaled_k[(1, 1)] as f32],
            c: [scaled_k[(0, 2)] as f32, scaled_k[(1, 2)] as f32],
            k: distortion_coeffs
                .iter()
                .map(|x| *x as f32)
                .collect::<Vec<f32>>()
                .try_into()
                .unwrap(),
            fov: fov as f32,
            r_limit: radial_distortion_limit as f32,
            lens_correction_amount: lens_correction_amount as f32,
            input_vertical_stretch: input_vertical_stretch as f32,
            input_horizontal_stretch: input_horizontal_stretch as f32,
            background_mode: params.background_mode as i32,
            background_margin: background_margin as f32,
            background_margin_feather: background_feather as f32,
            translation2d: [
                (adaptive_zoom_center_x * params.width as f64 / fov) as f32,
                (adaptive_zoom_center_y * params.height as f64 / fov) as f32,
            ],
            translation3d: [0.0, 0.0, 0.0, 0.0], // currently unused
            digital_lens_params,
            light_refraction_coefficient: light_refraction_coefficient as f32,
            ..Default::default()
        };

        Self {
            matrices,
            kernel_params,
            fov: ui_fov,
            minimal_fov: *params.minimal_fovs.get(frame).unwrap_or(&1.0),
            focal_length,
            mesh_data,
        }
    }

    pub fn at_timestamp_for_points(
        params: &ComputeParams,
        points: &[(f32, f32)],
        timestamp_ms: f64,
        frame: Option<usize>,
        use_fovs: bool,
    ) -> (
        Matrix3<f64>,
        [f64; 12],
        Matrix3<f64>,
        Vec<Matrix3<f64>>,
        Option<Vec<(f32, f32, f32, f32, f32)>>,
        Option<Vec<f64>>,
    ) {
        // camera_matrix, dist_coeffs, p, rotations_per_point
        // ----------- Keyframes -----------
        let video_rotation = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::VideoRotation, timestamp_ms)
            .unwrap_or(params.video_rotation);
        // ----------- Keyframes -----------

        let frame = frame
            .unwrap_or_else(|| crate::frame_at_timestamp(timestamp_ms, params.scaled_fps) as usize);

        let (camera_matrix, distortion_coeffs, _, _, _, _) =
            Self::get_lens_data_at_timestamp(params, timestamp_ms, params.framebuffer_inverted);

        let fov = Self::get_fov(params, 0, use_fovs, timestamp_ms, false);

        let scaled_k = camera_matrix;
        let new_k = Self::get_new_k(params, &camera_matrix, fov);

        let gyro = params.gyro.read();
        let file_metadata = gyro.file_metadata.read();

        let mut mesh_correction = None;
        if let Some(mc) = file_metadata.mesh_correction.get(frame) {
            mesh_correction = Some(mc.0.clone()); // distorting mesh
        }

        // ----------- Rolling shutter correction -----------
        let frame_readout_time =
            Self::get_frame_readout_time(params, false, timestamp_ms, &file_metadata);

        let row_readout_time = frame_readout_time
            / if params.frame_readout_direction.is_horizontal() {
                params.width
            } else {
                params.height
            } as f64;
        let timestamp_ms = timestamp_ms
            + gyro
                .file_metadata
                .read()
                .per_frame_time_offsets
                .get(frame)
                .unwrap_or(&0.0);
        let start_ts = timestamp_ms - (frame_readout_time / 2.0);
        // ----------- Rolling shutter correction -----------

        let image_rotation = Matrix3::new_rotation(video_rotation * (std::f64::consts::PI / 180.0));

        let quat1 = gyro.org_quat_at_timestamp(timestamp_ms).inverse();
        let smoothed_quat1 = gyro.smoothed_quat_at_timestamp(timestamp_ms);

        // Only compute 1 matrix if not using rolling shutter correction
        let points_iter = if frame_readout_time.abs() > 0.0 {
            points
        } else {
            &[(0.0, 0.0)]
        };

        let rotations: Vec<Matrix3<f64>> = points_iter
            .iter()
            .map(|&(x, y)| {
                let quat_time = if frame_readout_time.abs() > 0.0 {
                    start_ts
                        + row_readout_time
                            * if params.frame_readout_direction.is_horizontal() {
                                x
                            } else {
                                y
                            } as f64
                } else {
                    start_ts
                };
                let quat = smoothed_quat1 * quat1 * gyro.org_quat_at_timestamp(quat_time);

                let mut r = image_rotation * *quat.to_rotation_matrix().matrix();
                r[(0, 1)] *= -1.0;
                r[(0, 2)] *= -1.0;
                r[(1, 0)] *= -1.0;
                r[(2, 0)] *= -1.0;

                if params.suppress_rotation {
                    r = Matrix3::identity();
                }

                new_k * r
            })
            .collect();

        let mut shifts: Option<Vec<(f32, f32, f32, f32, f32)>> =
            if let Some(is) = file_metadata.camera_stab_data.get(frame) {
                let is_scale = (
                    params.width as f64 / is.crop_area.2 as f64 / is.pixel_pitch.0 as f64,
                    params.height as f64 / is.crop_area.3 as f64 / is.pixel_pitch.1 as f64,
                );
                Some(
                    points_iter
                        .iter()
                        .map(|&(_x, y)| {
                            let y = map_coord(
                                y as f64,
                                0.0,
                                params.height as f64,
                                is.crop_area.1 as f64,
                                is.crop_area.1 as f64 + is.crop_area.3 as f64,
                            );
                            let s = is
                                .ibis_spline
                                .interpolate(y + is.offset)
                                .unwrap_or_default();
                            let sx = s.x * is_scale.0;
                            let sy = s.y * is_scale.1;
                            let ra = s.z / 1000.0;

                            let o = is.ois_spline.interpolate(y + is.offset).unwrap_or_default();
                            let ox = o.x * is_scale.0;
                            let oy = o.y * is_scale.1;

                            (
                                sx as f32,
                                sy as f32,
                                ra.to_radians() as f32,
                                ox as f32,
                                oy as f32,
                            )
                        })
                        .collect(),
                )
            } else {
                None
            };
        if params.suppress_rotation && params.frame_readout_time == 0.0 {
            shifts = None;
        }

        (
            scaled_k,
            distortion_coeffs,
            new_k,
            rotations,
            shifts,
            mesh_correction,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::FrameTransform;

    // Measured on a Sony ZV-E10M2: 4K60 reads 2104.1875 of the sensor's 3156 rows.
    const CROPPED_CAPTURE_H: f32 = 2104.1875;
    const SENSOR_H: u32 = 3156;
    // Same body, full-height mode.
    const FULL_CAPTURE_H: f32 = 3155.8125;

    #[test]
    fn readout_crop_scale_uses_captured_rows_for_sony() {
        let scale = FrameTransform::readout_crop_scale(true, Some(CROPPED_CAPTURE_H), Some(SENSOR_H));
        assert!(
            (scale - 0.666_727).abs() < 1e-6,
            "expected the 2/3-height crop ratio, got {scale}"
        );
        // 15.801 ms whole-sensor readout becomes 10.535 ms over the captured rows.
        assert!((15.801 * scale - 10.535).abs() < 0.001);
    }

    #[test]
    fn readout_crop_scale_is_near_identity_for_full_height_sony() {
        let scale = FrameTransform::readout_crop_scale(true, Some(FULL_CAPTURE_H), Some(SENSOR_H));
        // Deliberately not exactly 1.0: the capture area is a hair short of the full
        // sensor, so full-height Sony clips shift by ~0.006%. Documented as accepted.
        assert!((scale - 1.0).abs() < 2e-4, "unexpected drift: {scale}");
    }

    #[test]
    fn readout_crop_scale_is_identity_for_non_sony() {
        // Nikon ZR shape: telemetry-parser supplies both size fields, but its readout
        // time comes from camera_db and is already per shooting mode, so it must not
        // be scaled again.
        assert_eq!(
            FrameTransform::readout_crop_scale(false, Some(2232.0), Some(3348)),
            1.0
        );
    }

    #[test]
    fn readout_crop_scale_falls_back_when_a_size_field_is_missing() {
        assert_eq!(
            FrameTransform::readout_crop_scale(true, Some(CROPPED_CAPTURE_H), None),
            1.0
        );
        assert_eq!(
            FrameTransform::readout_crop_scale(true, None, Some(SENSOR_H)),
            1.0
        );
    }

    #[test]
    fn readout_crop_scale_falls_back_when_no_lens_params_entry_is_in_range() {
        // `get_closest` returning None outside the 100 ms window reaches the scale
        // helper as a pair of None, which must degrade to no scaling.
        assert_eq!(FrameTransform::readout_crop_scale(true, None, None), 1.0);
    }

    #[test]
    fn readout_crop_scale_rejects_degenerate_values() {
        assert_eq!(
            FrameTransform::readout_crop_scale(true, Some(CROPPED_CAPTURE_H), Some(0)),
            1.0
        );
        assert_eq!(
            FrameTransform::readout_crop_scale(true, Some(0.0), Some(SENSOR_H)),
            1.0
        );
        assert_eq!(
            FrameTransform::readout_crop_scale(true, Some(f32::NAN), Some(SENSOR_H)),
            1.0
        );
    }

    #[test]
    fn detected_source_is_sony_matches_bare_and_model_forms() {
        assert!(FrameTransform::detected_source_is_sony(Some("Sony")));
        assert!(FrameTransform::detected_source_is_sony(Some(
            "Sony ZV-E10M2"
        )));
        assert!(FrameTransform::detected_source_is_sony(Some("Sony ILCE-6400")));

        assert!(!FrameTransform::detected_source_is_sony(None));
        assert!(!FrameTransform::detected_source_is_sony(Some("Nikon ZR")));
        assert!(!FrameTransform::detected_source_is_sony(Some(
            "Blackmagic Design Pocket Cinema Camera 6K"
        )));
        assert!(!FrameTransform::detected_source_is_sony(Some(
            "Canon EOS R5 Mark II"
        )));
        // A brand that merely starts with the same letters must not slip through.
        assert!(!FrameTransform::detected_source_is_sony(Some("Sonyx Cam")));
    }

    #[test]
    fn sony_guard_wraps_the_lens_params_lookup() {
        // Structural guard for the "no change for other sources" promise: the
        // lens_params lookup must sit inside the `is_sony` branch. Flattening it
        // would still yield 1.0 for other brands today, but the guarantee would
        // degrade from a control-flow property into an arithmetic coincidence that
        // any future change to readout_crop_scale could silently break.
        let src = include_str!("frame_transform.rs");
        let compact: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(
            compact.contains("letclosest=ifis_sony{file_metadata.lens_params.get_closest("),
            "the lens_params lookup must stay gated behind `if is_sony`"
        );
        assert!(
            compact.contains("if!is_sony{return1.0;}"),
            "readout_crop_scale must short-circuit for non-Sony sources"
        );
    }
}
