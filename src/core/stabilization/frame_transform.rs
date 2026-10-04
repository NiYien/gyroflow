// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use super::{ComputeParams, KernelParams};
use crate::gyro_source::{FileMetadata, GyroSource, Quat64, TranslationConfig};
use crate::keyframes::KeyframeType;
use crate::util::{MapClosest, map_coord};
use nalgebra::{Matrix3, Vector3};
use rayon::iter::{IntoParallelIterator, ParallelIterator};

/// The translation one matrix row (or one point) gets, in the frame its matrix is in. `source` is the camera's
/// orientation at `quat_time_ms`, `frame_time_ms` the frame's own time, `focal_px` the source focal length in pixels.
fn optical_translation_for(
    gyro: &GyroSource, params: &ComputeParams, source: &Quat64, quat_time_ms: f64, frame_time_ms: f64,
    focal_px: f64, inverted: bool, config: &TranslationConfig,
) -> Option<Vector3<f64>> {
    if !params.apply_optical_translation || params.suppress_rotation {
        return None;
    }
    let translation = gyro.optical_translation.as_ref()?;
    if !translation.is_active() {
        return None;
    }
    let shift = translation.shift_at(if config.per_row { quat_time_ms } else { frame_time_ms });
    let t_quat = -(source.inverse() * shift);
    let mut t = Vector3::new(t_quat.x, if inverted { t_quat.y } else { -t_quat.y }, -t_quat.z);
    let soft = |x: f64, limit: f64| {
        let half = limit / 2.0;
        if x <= half { x } else { half + half * ((x - half) / half).tanh() }
    };
    let limit = config.max_shift * params.width.min(params.height) as f64 / focal_px;
    let n = t.xy().norm();
    if n > 0.0 {
        let scale = soft(n, limit) / n;
        t.x *= scale;
        t.y *= scale;
    }
    t.z = t.z.signum() * soft(t.z.abs(), config.max_shift);
    if !translation.settings.along_axis {
        t.z = 0.0;
    }
    if t.norm() == 0.0 { None } else { Some(t) }
}

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

    pub(crate) fn get_frame_readout_time(
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
        let horizontal_ratio = params.lens.horizontal_stretch_normalized();

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

    /// The metadata focal length is often quantized (whole millimetres on many Sony lenses) while the optics
    /// zoom smoothly. Projecting with the stepped value makes the gyro correction jump at every step, because
    /// the correction shift scales with the focal length, so the camera matrix is rescaled to the dequantized
    /// per-frame focal length from `smoothing::focal_length` whenever the curve exists. Aspect and center are
    /// kept, and the distortion coefficients stay normalized as the camera delivered them. The curve stays within
    /// the dequantization band of the metadata (a fraction of a quantization step) everywhere except across a
    /// confirmed metadata glitch, where it bridges the levels around it (`focal_length::remove_outliers`), so this
    /// never moves the projection by more than a step on the strength of a heuristic; the clamp only guards
    /// against a curve that doesn't belong to this lens data at all. Returns the scale
    pub fn dequantize_camera_matrix(
        params: &ComputeParams,
        frame: usize,
        camera_matrix: &mut Matrix3<f64>,
    ) -> f64 {
        let Some(Some(dequantized)) = params
            .focal_lengths
            .get(frame)
            .or(params.focal_lengths.last())
            .copied()
        else {
            return 1.0;
        };
        let raw = (camera_matrix[(0, 0)] * camera_matrix[(1, 1)]).sqrt();
        if !(raw > 0.0) || !(dequantized > 0.0) {
            return 1.0;
        }
        let scale = (dequantized / raw).clamp(0.1, 10.0);
        camera_matrix[(0, 0)] *= scale;
        camera_matrix[(1, 1)] *= scale;
        scale
    }

    /// Sensor row the picture row `y_source` sits at within the capture area `crop_y .. crop_y + crop_h`, for the
    /// per-row data (sensor and lens shift, lens breathing). `y_source` indexes the rows of the framebuffer the
    /// matrices are looked up by: inverted, row `y` holds picture row `height - y`, which sits at the mirrored
    /// position within the capture area, not within the sensor (the two differ as soon as the crop is off-centre,
    /// as it is with a moving EIS crop)
    fn sensor_row(params: &ComputeParams, y_source: f64, crop_y: f64, crop_h: f64) -> f64 {
        let y_sensor = map_coord(y_source, 0.0, params.height as f64, crop_y, crop_y + crop_h);
        if params.framebuffer_inverted {
            2.0 * crop_y + crop_h - y_sensor
        } else {
            y_sensor
        }
    }

    /// The lens breathing compensation of one matrix row as a zoom of the output frame around its centre, by the
    /// row's magnification `k` (see `gyro_source::sony::breathing`). `None` when the row has no usable zoom: only a
    /// positive, finite one is a zoom at all, anything else makes the matrix singular and maps the whole output to
    /// the centre. `at_timestamp` post-multiplies its inverse transform by it and `at_timestamp_for_points`
    /// pre-multiplies its forward projection by the `inverse` of it, so the two directions stay exact inverses of
    /// each other - the STMap export writes one map from each and they only compose back to the identity if they do.
    /// The centre is the output frame's, in the coordinates of the caller's own output size: the two paths describe
    /// the same frame at different scales, and a zoom about a point survives that scaling unchanged
    fn breathing_matrix(params: &ComputeParams, k: f64, inverse: bool) -> Option<Matrix3<f64>> {
        if !(k.is_finite() && k > 0.0) {
            return None;
        }
        let k = if inverse { 1.0 / k } else { k };
        let (cx, cy) = (
            params.output_width as f64 / 2.0,
            params.output_height as f64 / 2.0,
        );
        Some(Matrix3::new(
            k,
            0.0,
            cx * (1.0 - k),
            0.0,
            k,
            cy * (1.0 - k),
            0.0,
            0.0,
            1.0,
        ))
    }

    /// Camera matrix, distortion coefficients, radial distortion limit, input stretches, focal length in millimetres,
    /// and whether the camera matrix's focal length came from per-frame lens metadata (see `get_lens_data_at_timestamp_with_metadata`)
    pub fn get_lens_data_at_timestamp(
        params: &ComputeParams,
        timestamp_ms: f64,
        invert_asym_lens: bool,
    ) -> (Matrix3<f64>, [f64; 24], f64, f64, f64, Option<f64>, bool) {
        let gyro = params.gyro.read();
        let file_metadata = gyro.file_metadata.read();
        Self::get_lens_data_at_timestamp_with_metadata(
            params,
            &file_metadata,
            timestamp_ms,
            invert_asym_lens,
        )
    }

    /// `get_lens_data_at_timestamp` on file metadata the caller already holds. Anything that holds the `gyro` or
    /// the `file_metadata` read guard has to come through here: both are parking_lot locks, and a second `read()`
    /// of a lock this thread already reads is not a re-entry but a deadlock as soon as a writer has queued up
    /// behind the first guard (the lock is writer-fair: the new reader waits for the writer, the writer waits for
    /// the first guard, and the first guard waits for the new reader).
    ///
    /// The last element tells whether the focal length of the camera matrix came from the lens metadata of this frame
    /// (an interpolated lens profile, the camera's pixel focal length, or a millimetre focal length scaled into the
    /// profile) rather than from the static profile alone. The per-frame focal length curves (`smoothing::focal_length`)
    /// follow exactly this, so they can never disagree with the projection about which frames have a focal length of
    /// their own, whichever way the camera reports it
    pub fn get_lens_data_at_timestamp_with_metadata(
        params: &ComputeParams,
        file_metadata: &FileMetadata,
        timestamp_ms: f64,
        invert_asym_lens: bool,
    ) -> (Matrix3<f64>, [f64; 24], f64, f64, f64, Option<f64>, bool) {
        // The lens metadata may lag the picture by a few frames (per lens, see synchronization::lens_delay): every lookup uses the corrected time
        Self::get_lens_data_at_lens_timestamp(
            params,
            file_metadata,
            params.lens_timestamp_us(timestamp_ms),
            invert_asym_lens,
        )
    }

    fn interpolated_lens_at(
        params: &ComputeParams,
        metadata: &FileMetadata,
        lens_timestamp_us: i64,
    ) -> Option<crate::lens_profile::LensProfile> {
        if !params.lens.has_interpolations() {
            return None;
        }
        metadata.lens_positions.get_closest(&lens_timestamp_us, 100000)
            .map(|position| params.lens.get_interpolated_lens_at(*position))
    }

    pub fn input_stretch_at_timestamp(params: &ComputeParams, timestamp_ms: f64) -> (f64, f64) {
        // Ordinary point batches need no metadata lock or camera reconstruction.
        let selected = if params.lens.has_interpolations() {
            let gyro = params.gyro.read();
            let metadata = gyro.file_metadata.read();
            Self::interpolated_lens_at(params, &metadata, params.lens_timestamp_us(timestamp_ms))
        } else {
            None
        };
        let lens = selected.as_ref().unwrap_or(&params.lens);
        (lens.horizontal_stretch_normalized(), lens.vertical_stretch_normalized())
    }

    /// `get_lens_data_at_timestamp_with_metadata` at a lens metadata time already shifted by the delay
    /// (`ComputeParams::lens_timestamp_us`), for callers that apply a delay of their own choosing (the focal length
    /// curves are extracted without one and shifted by frames afterwards)
    pub fn get_lens_data_at_lens_timestamp(params: &ComputeParams, file_metadata: &FileMetadata, lens_timestamp_us: i64, invert_asym_lens: bool) -> (Matrix3<f64>, [f64; 24], f64, f64, f64, Option<f64>, bool) {
        let interpolated_lens = Self::interpolated_lens_at(params, file_metadata, lens_timestamp_us);
        let mut per_frame = interpolated_lens.is_some();
        let lens = interpolated_lens.as_ref().unwrap_or(&params.lens);

        // Telemetry focal lengths and principal points describe the original
        // sensor pixels, even when a host has rescaled the incoming image.
        let size_scale = lens.input_size_scale();
        let sensor_width = params.width as f64 / size_scale[0];
        let sensor_height = params.height as f64 / size_scale[1];

        let mut focal_length = lens.focal_length;

        let mut camera_matrix = lens.get_camera_matrix((sensor_width.round() as usize, sensor_height.round() as usize), invert_asym_lens);
        let mut distortion_coeffs = lens.get_distortion_coeffs();

        let mut radial_distortion_limit = lens.fisheye_params.radial_distortion_limit.unwrap_or_default();

        let mut stretch_lens = true;
        let mut zoom_scale = 1.0;
        let digital_zoom = file_metadata.digital_zoom.unwrap_or_default();

        if lens.fisheye_params.distortion_coeffs.len() < 4 {
            if let Some(val) = file_metadata.lens_params_closest(lens_timestamp_us, 100000, |v| v.has_projection_data()) { // closest within 100ms
                let pixel_focal_length = val.pixel_focal_length.map(|f| (f.0 as f64, f.1 as f64)).or_else(|| {
                    let fl_mm = val.focal_length? as f64;
                    focal_length = Some(fl_mm);
                    let pp = val.pixel_pitch?;
                    let crop = val.capture_area_size?;
                    if pp.0 == 0 || pp.1 == 0 || crop.0 <= 0.0 || crop.1 <= 0.0 { return None; }
                    let fx = (fl_mm / ((pp.0 as f64 / 1_000_000.0) * crop.0 as f64)) * sensor_width;
                    let fy = (fl_mm / ((pp.1 as f64 / 1_000_000.0) * crop.1 as f64)) * sensor_height;
                    Some((fx, fy))
                });
                if let Some((fx, fy)) = pixel_focal_length.filter(|_| !lens.lens_group_override) {
                    camera_matrix[(0, 0)] = fx;
                    camera_matrix[(1, 1)] = fy;
                    if let Some((cx, cy)) = val.principal_point {
                        camera_matrix[(0, 2)] = cx as f64;
                        camera_matrix[(1, 2)] = if invert_asym_lens { sensor_height - cy as f64 } else { cy as f64 };
                    }
                    stretch_lens = false;
                    per_frame = true;

                    if let Some(fl) = val.focal_length {
                        focal_length = Some(fl as f64);
                    }
                }
                if !val.distortion_coefficients.is_empty() && val.distortion_coefficients.len() <= 24 {
                    for (i, x) in val.distortion_coefficients.iter().enumerate() {
                        distortion_coeffs[i] = *x;
                    }

                    radial_distortion_limit = params.distortion_model.radial_distortion_limit(&distortion_coeffs).unwrap_or_default();
                }
            }
        } else if !lens.lens_group_override && !params.lens.has_interpolations() && file_metadata.lens_focal_length_varies() {
            // A single calibration for a lens whose metadata records a changing focal length in millimetres (a zoom
            // lens on a Blackmagic, RED, Nikon or Z CAM body): the projection follows the zoom by scaling the
            // calibrated focal length with the metadata, relative to the focal length the profile declares or,
            // failing that, the one its camera matrix implies on this sensor. The distortion coefficients stay those
            // of the calibration. Cameras that also report the focal length in pixels (Canon) are left to that value.
            //
            // The profile asked here is `params.lens`, not the `lens` this frame projects with: a profile with
            // calibrations at several lens positions already follows the zoom through them, and scaling one of those
            // again would apply the zoom twice. `get_interpolated_lens_at` hands out the calibration of the position
            // itself - a profile of its own, with no interpolations left - whenever the lookup lands on a knot or
            // outside their range, and a blend that keeps them everywhere in between, so asking `lens` would turn
            // the branch on and off along the lens travel and jump the projection at every knot
            if let Some(val) = file_metadata.lens_params_closest(lens_timestamp_us, 100000, |v| v.focal_length.is_some() && v.pixel_focal_length.is_none()) {
                let mm = val.focal_length.unwrap_or_default() as f64;
                let calib_w = if lens.calib_dimension.w > 0 { lens.calib_dimension.w as f64 } else { params.width.max(1) as f64 };
                let reference = lens.focal_length.filter(|f| *f > 0.0).or_else(|| {
                    let (pp, crop) = (val.pixel_pitch?, val.capture_area_size?);
                    if pp.0 == 0 || crop.0 <= 0.0 { return None; }
                    Some(camera_matrix[(0, 0)] * (pp.0 as f64 / 1_000_000.0) * crop.0 as f64 / calib_w)
                });
                if let Some(reference) = reference {
                    if mm > 0.0 && reference > 0.0 {
                        zoom_scale = mm / reference;
                        focal_length = Some(mm);
                        per_frame = true;
                    }
                }
            }
        }

        let (calib_width, calib_height) = if lens.calib_dimension.w > 0 && lens.calib_dimension.h > 0 {
            (lens.calib_dimension.w as f64, lens.calib_dimension.h as f64)
        } else {
            (sensor_width.max(1.0), sensor_height.max(1.0))
        };

        let input_horizontal_stretch = lens.horizontal_stretch_normalized();
        let input_vertical_stretch = lens.vertical_stretch_normalized();

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
        if zoom_scale != 1.0 {
            camera_matrix[(0, 0)] *= zoom_scale;
            camera_matrix[(1, 1)] *= zoom_scale;
        }

        (camera_matrix, distortion_coeffs, radial_distortion_limit, input_horizontal_stretch, input_vertical_stretch, focal_length, per_frame)
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
            mut camera_matrix,
            distortion_coeffs,
            radial_distortion_limit,
            input_horizontal_stretch,
            input_vertical_stretch,
            focal_length,
            _,
        ) = Self::get_lens_data_at_timestamp(params, timestamp_ms, false);
        let focal_scale = Self::dequantize_camera_matrix(params, frame, &mut camera_matrix);
        let focal_length = focal_length.map(|f| f * focal_scale);
        // ----------- Lens -----------

        let lens_correction_amount = params.apply_anamorphic_decay(lens_correction_amount);

        // Focal length stabilization: a uniform digital zoom (never above 1, so never past the frame) on
        // top of the adaptive zoom, see smoothing::focal_length. It's part of the applied zoom, so the UI
        // readout includes it too: the overlay then shows the true total zoom and the apparent focal length
        let fl_compensation = crate::smoothing::focal_length::compensation_at(params, frame);
        let mut fov = Self::get_fov(params, frame, true, timestamp_ms, false) * fl_compensation;
        let mut ui_fov = Self::get_fov(params, frame, true, timestamp_ms, true) * fl_compensation;
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

        // Undistorting mesh of the frame, empty when it has none (the kernel flags say so, the buffer is then not uploaded)
        let mesh_data = file_metadata.mesh_correction.kernel_buffer(frame);

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

        let breathing = if params.lens_breathing_enabled {
            file_metadata
                .lens_breathing
                .get(frame)
                .filter(|b| !b.scale.is_empty())
        } else {
            None
        };

        // Sensor row a matrix row is looked up at, for the per-row data (sensor and lens shift, lens breathing).
        // Without rolling shutter correction the single matrix stands for the whole frame and is evaluated at its
        // centre row
        let sensor_row = |y: usize, crop_y: f64, crop_h: f64| -> f64 {
            Self::sensor_row(
                params,
                if rows > 1 {
                    y as f64
                } else {
                    params.height as f64 / 2.0
                },
                crop_y,
                crop_h,
            )
        };

        let translation_config = TranslationConfig::resolved();

        let matrices = (0..rows)
            .into_par_iter()
            .map(|y| {
                let quat_time = if frame_readout_time.abs() > 0.0 {
                    start_ts + row_readout_time * y as f64
                } else {
                    start_ts
                };
                let source = gyro.org_quat_at_timestamp(quat_time);
                let quat = smoothed_quat1 * quat1 * source;

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
                        let y_sensor = sensor_row(y, is.crop_area.1 as f64, is.crop_area.3 as f64);

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
                            .interpolate(y_sensor + is.ois_offset.unwrap_or(is.offset))
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
                let mut i_r = i_r.unwrap_or_default();
                if let Some(b) = breathing {
                    // Lens breathing: a zoom of the output around its centre, by the row's magnification
                    if let Some(m) = Self::breathing_matrix(
                        params,
                        b.scale_at_row(sensor_row(y, b.crop_y as f64, b.crop_h as f64)),
                        false,
                    ) {
                        i_r *= m;
                    }
                }
                if let Some(t) = optical_translation_for(
                    &gyro, params, &source, quat_time, timestamp_ms, scaled_k[(0, 0)],
                    params.framebuffer_inverted, &translation_config,
                ) {
                    i_r[(0, 2)] += t.x;
                    i_r[(1, 2)] += t.y;
                    i_r[(2, 2)] += t.z;
                }
                let i_r: Matrix3<f32> = nalgebra::convert(i_r);
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
        [f64; 24],
        Matrix3<f64>,
        Vec<Matrix3<f64>>,
        Option<Vec<(f32, f32, f32, f32, f32)>>,
        Option<Vec<f64>>,
        f64,
        f64,
    ) {
        // camera_matrix, dist_coeffs, p, rotations_per_point, shifts, mesh, fov, radial_distortion_limit
        // ----------- Keyframes -----------
        let video_rotation = params
            .keyframes
            .value_at_video_timestamp(&KeyframeType::VideoRotation, timestamp_ms)
            .unwrap_or(params.video_rotation);
        // ----------- Keyframes -----------

        let frame = frame
            .unwrap_or_else(|| crate::frame_at_timestamp(timestamp_ms, params.scaled_fps) as usize);

        let (mut camera_matrix, distortion_coeffs, radial_distortion_limit, _, _, _, _) =
            Self::get_lens_data_at_timestamp(params, timestamp_ms, params.framebuffer_inverted);
        Self::dequantize_camera_matrix(params, frame, &mut camera_matrix);

        // The focal length compensation is part of the applied zoom, not of the base projection:
        // measurements at fov = 1 (zoom polygon, sync, features) must not include it, or the zoom would
        // fit the frame around the crop and undo it, see zooming::calculate_fovs
        let fl_compensation = if use_fovs {
            crate::smoothing::focal_length::compensation_at(params, frame)
        } else {
            1.0
        };
        let fov = Self::get_fov(params, frame, use_fovs, timestamp_ms, false) * fl_compensation;

        let scaled_k = camera_matrix;
        let new_k = Self::get_new_k(params, &camera_matrix, fov);

        let gyro = params.gyro.read();
        let file_metadata = gyro.file_metadata.read();

        let mesh_correction = file_metadata.mesh_correction.forward_mesh(frame); // distorting mesh, none when the frame has none

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
            + file_metadata
                .per_frame_time_offsets
                .get(frame)
                .unwrap_or(&0.0);
        let start_ts = timestamp_ms - (frame_readout_time / 2.0);
        // ----------- Rolling shutter correction -----------

        let image_rotation = Matrix3::new_rotation(video_rotation * (std::f64::consts::PI / 180.0));

        let quat1 = gyro.org_quat_at_timestamp(timestamp_ms).inverse();
        let smoothed_quat1 = gyro.smoothed_quat_at_timestamp(timestamp_ms);

        // Only compute 1 matrix if not using rolling shutter correction; it stands for the whole frame, so the
        // per-row data (sensor and lens shift, lens breathing) is looked up at the centre row, like `at_timestamp` does
        let centre = [(params.width as f32 / 2.0, params.height as f32 / 2.0)];
        let points_iter: &[(f32, f32)] = if frame_readout_time.abs() > 0.0 {
            points
        } else {
            &centre
        };

        // Lens breathing, the zoom `at_timestamp` folds into its matrices, so this direction can undo it and the two
        // stay invertible (the STMap export writes a map from each). Like the focal length compensation above it's
        // part of the applied zoom and not of the base projection, so it follows `use_fovs` too: the measurements at
        // fov = 1 (zoom polygon, sync, features) describe the picture the zoom is fitted around, and a zoom folded
        // into them would only let the fit relax and undo it
        let breathing = if use_fovs && params.lens_breathing_enabled {
            file_metadata
                .lens_breathing
                .get(frame)
                .filter(|b| !b.scale.is_empty())
        } else {
            None
        };

        let translation_config = TranslationConfig::resolved();

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
                let source = gyro.org_quat_at_timestamp(quat_time);
                let quat = smoothed_quat1 * quat1 * source;

                let mut r = image_rotation * *quat.to_rotation_matrix().matrix();
                r[(0, 1)] *= -1.0;
                r[(0, 2)] *= -1.0;
                r[(1, 0)] *= -1.0;
                r[(2, 0)] *= -1.0;

                if params.suppress_rotation {
                    r = Matrix3::identity();
                }

                let mut p = new_k * r;
                if let Some(b) = breathing {
                    // Looked up by the same index `at_timestamp` looks its matrices up by: the point's readout position
                    let readout_pos = if frame_readout_time.abs() > 0.0 {
                        (if params.frame_readout_direction.is_horizontal() {
                            x
                        } else {
                            y
                        }) as f64
                    } else {
                        params.height as f64 / 2.0
                    };
                    if let Some(m) = Self::breathing_matrix(
                        params,
                        b.scale_at_row(Self::sensor_row(
                            params,
                            readout_pos,
                            b.crop_y as f64,
                            b.crop_h as f64,
                        )),
                        true,
                    ) {
                        p = m * p;
                    }
                }
                if let Some(t) = optical_translation_for(
                    &gyro, params, &source, quat_time, timestamp_ms, scaled_k[(0, 0)],
                    false, &translation_config,
                ) {
                    let s = p * t;
                    let d = 1.0 + s.z;
                    if d > 0.5 {
                        p -= s * p.row(2) / d;
                    }
                }
                p
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

                            let o = is
                                .ois_spline
                                .interpolate(y + is.ois_offset.unwrap_or(is.offset))
                                .unwrap_or_default();
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
            fov,
            radial_distortion_limit,
        )
    }
}

#[cfg(test)]
mod niyien_tests {
    use super::FrameTransform;

    // Measured on a Sony ZV-E10M2: 4K60 reads 2104.1875 of the sensor's 3156 rows.
    const CROPPED_CAPTURE_H: f32 = 2104.1875;
    const SENSOR_H: u32 = 3156;
    // Same body, full-height mode.
    const FULL_CAPTURE_H: f32 = 3155.8125;

    #[test]
    fn readout_crop_scale_uses_captured_rows_for_sony() {
        let scale =
            FrameTransform::readout_crop_scale(true, Some(CROPPED_CAPTURE_H), Some(SENSOR_H));
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
        assert!(FrameTransform::detected_source_is_sony(Some(
            "Sony ILCE-6400"
        )));

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
#[cfg(test)]
mod tests {
    use super::*;
    use crate::gyro_source::{BreathingFrame, FileMetadata, LensParams};
    use crate::lens_profile::{Dimensions, LensProfile};
    use crate::stabilization::{Stabilization, undistort_points};

    const W: usize = 1920;
    const H: usize = 1080;
    const POINTS: [(f32, f32); 5] = [
        (0.0, 0.0),
        (1919.0, 0.0),
        (960.0, 540.0),
        (300.0, 900.0),
        (1600.0, 1079.0),
    ];

    use crate::gyro_source::{OpticalTranslation, Quat64, TranslationConfig};

    /// `stabilized_params` without the smoothing correction, at a fixed orientation, with a constant world shift
    fn translated(readout_time: f64, orientation: Quat64, shift: [f64; 3], along_axis: bool) -> ComputeParams {
        let mut p = params(vec![1.0], readout_time);
        p.suppress_rotation = false;
        p.apply_optical_translation = true;
        {
            let mut gyro = p.gyro.write();
            gyro.duration_ms = 1000.0;
            gyro.quaternions.insert(0, orientation);
            gyro.quaternions.insert(1_000_000, orientation);
            let mut t = OpticalTranslation::with_curve(vec![(0, shift), (1_000_000, shift)]);
            t.settings.along_axis = along_axis;
            gyro.optical_translation = Some(t);
        }
        p
    }
    /// The same parameters with a gyro source of their own, changed by `change`
    fn with_gyro(p: &ComputeParams, change: impl FnOnce(&mut crate::gyro_source::GyroSource)) -> ComputeParams {
        let mut gyro = p.gyro.read().clone();
        change(&mut gyro);
        let mut q = p.clone();
        q.gyro = std::sync::Arc::new(parking_lot::RwLock::new(gyro));
        q
    }
    fn untranslated(p: &ComputeParams) -> ComputeParams {
        with_gyro(p, |gyro| gyro.optical_translation = None)
    }
    fn output_focal(p: &ComputeParams) -> (f64, f64, f64) {
        let k = FrameTransform::at_timestamp_for_points(p, &POINTS, 0.0, Some(0), true).2;
        (k[(0, 0)], k[(0, 2)], k[(1, 2)])
    }
    fn assert_shift(p: &ComputeParams, want: (f64, f64)) {
        let base = untranslated(p);
        for &pt in &POINTS {
            let (a, b) = (to_output(&base, pt), to_output(p, pt));
            assert!(((b.0 - a.0) as f64 - want.0).abs() < 0.02 && ((b.1 - a.1) as f64 - want.1).abs() < 0.02,
                "{pt:?}: moved by {:?}, expected {want:?}", (b.0 - a.0, b.1 - a.1));
        }
    }

    #[test]
    fn camera_moved_right_shifts_the_picture_right() {
        // The camera is 0.01 (in units of the reference depth) to the right of its smooth path: the output camera sits
        // to its left and sees every point of the reference layer further right
        let p = translated(0.0, Quat64::identity(), [0.01, 0.0, 0.0], false);
        assert_shift(&p, (output_focal(&p).0 * 0.01, 0.0));
    }

    #[test]
    fn camera_moved_up_shifts_the_picture_up() {
        // The quaternions' y axis points up, the picture's down
        let p = translated(0.0, Quat64::identity(), [0.0, 0.01, 0.0], false);
        assert_shift(&p, (0.0, -output_focal(&p).0 * 0.01));
    }

    #[test]
    fn world_shift_is_taken_into_the_camera_frame() {
        // Camera rolled by 90° about its axis: the world x axis is the camera's -y
        let roll = Quat64::from_axis_angle(&nalgebra::Vector3::z_axis(), std::f64::consts::FRAC_PI_2);
        let p = translated(0.0, roll, [0.01, 0.0, 0.0], false);
        assert_shift(&p, (0.0, output_focal(&p).0 * 0.01));
    }

    #[test]
    fn movement_along_the_axis_scales_about_the_principal_point_only_when_asked() {
        // The camera looks down -z: 0.01 forward of its smooth path, everything is 1% larger and is scaled back
        let p = translated(0.0, Quat64::identity(), [0.0, 0.0, -0.01], true);
        let (base, (_, cx, cy)) = (untranslated(&p), output_focal(&p));
        for &pt in &POINTS {
            let (a, b) = (to_output(&base, pt), to_output(&p, pt));
            assert!(((b.0 as f64 - cx) - 0.99 * (a.0 as f64 - cx)).abs() < 0.02 && ((b.1 as f64 - cy) - 0.99 * (a.1 as f64 - cy)).abs() < 0.02, "{pt:?}: {a:?} -> {b:?}");
        }
        assert_shift(&translated(0.0, Quat64::identity(), [0.0, 0.0, -0.01], false), (0.0, 0.0));
    }

    #[test]
    fn translation_round_trips() {
        let tilted = Quat64::from_euler_angles(0.05, -0.08, 0.3);
        assert_round_trip(&translated(0.0, tilted, [0.01, -0.006, 0.004], true));
        // Rolling shutter, a rotating camera and a shift that changes during the readout
        let mut p = stabilized_params(12.0);
        p.apply_optical_translation = true;
        p.gyro.write().optical_translation = Some(OpticalTranslation::with_curve(vec![(0, [0.0, 0.0, 0.0]), (1_000_000, [0.03, -0.02, 0.0])]));
        for &pt in &POINTS {
            let (k, coeffs, _p, rotations, is, mesh, fov, r_limit) = FrameTransform::at_timestamp_for_points(&p, &[pt], 500.0, Some(0), true);
            let out = undistort_points(&[pt], k, &coeffs, rotations[0], None, Some(rotations), &p, 1.0, fov, 500.0, is, mesh, r_limit)[0];
            let t = FrameTransform::at_timestamp(&p, 500.0, 0);
            let mut kp = t.kernel_params;
            (kp.width, kp.height, kp.output_width, kp.output_height) = (W as i32, H as i32, W as i32, H as i32);
            let back = Stabilization::rotate_and_distort(out, (pt.1 as usize).min(t.matrices.len() - 1), &kp, &t.matrices, &p.distortion_model, None, kp.r_limit * kp.r_limit, &[]).unwrap();
            assert!((back.0 - pt.0).abs() < 0.05 && (back.1 - pt.1).abs() < 0.05, "{pt:?} -> {out:?} -> {back:?}");
        }
    }

    #[test]
    fn translation_round_trips_with_video_rotation_and_horizontal_readout() {
        let tilted = Quat64::from_euler_angles(0.05, -0.08, 0.3);
        let mut rotated = translated(0.0, tilted, [0.01, -0.006, 0.0], false);
        rotated.video_rotation = 90.0;
        assert_round_trip(&rotated);
        let mut sideways = translated(12.0, tilted, [0.01, -0.006, 0.0], false);
        sideways.frame_readout_direction = crate::stabilization_params::ReadoutDirection::LeftToRight;
        for &pt in &POINTS {
            let out = to_output(&sideways, pt);
            let back = to_source(&sideways, out, pt.0 as usize).unwrap();
            assert!((back.0 - pt.0).abs() < 0.05 && (back.1 - pt.1).abs() < 0.05, "{pt:?} -> {out:?} -> {back:?}");
        }
    }

    #[test]
    fn inverted_framebuffer_flips_the_vertical_shift() {
        let centre = (W as f32 / 2.0, H as f32 / 2.0);
        let moved = |inverted: bool| {
            let mut p = translated(0.0, Quat64::identity(), [0.01, 0.006, 0.0], false);
            p.framebuffer_inverted = inverted;
            let (a, b) = (to_source(&untranslated(&p), centre, 0).unwrap(), to_source(&p, centre, 0).unwrap());
            (b.0 - a.0, b.1 - a.1)
        };
        let (upright, inverted) = (moved(false), moved(true));
        assert!(upright.0.abs() > 1.0 && upright.1.abs() > 1.0);
        assert!((inverted.0 - upright.0).abs() < 0.05 && (inverted.1 + upright.1).abs() < 0.05, "{upright:?} vs {inverted:?}");
    }

    #[test]
    fn translation_is_limited_to_a_part_of_the_frame() {
        let p = translated(0.0, Quat64::identity(), [10.0, 0.0, 0.0], false);
        let limit_px = output_focal(&p).0 * 0.04 * H as f64 / 1400.0;
        let (a, b) = (to_output(&untranslated(&p), POINTS[2]), to_output(&p, POINTS[2]));
        let moved = (b.0 - a.0) as f64;
        assert!(moved > 0.9 * limit_px && moved <= limit_px + 0.01, "moved {moved}, limit {limit_px}");
    }

    #[test]
    fn rows_take_the_shift_of_their_own_time() {
        let mut q = stabilized_params(12.0);
        q.apply_optical_translation = true;
        let mut gyro = q.gyro.read().clone();
        gyro.optical_translation = Some(OpticalTranslation::with_curve(vec![(0, [0.0, 0.0, 0.0]), (1_000_000, [0.03, 0.0, 0.0])]));
        let source = Quat64::identity();
        let per_row = TranslationConfig::DEFAULT;
        let per_frame = TranslationConfig { per_row: false, ..TranslationConfig::DEFAULT };
        let at = |time: f64, config: &TranslationConfig| optical_translation_for(&gyro, &q, &source, time, 500.0, 1400.0, false, config).unwrap().x;
        assert!((at(494.0, &per_row) - at(506.0, &per_row)).abs() > 1e-4);
        assert_eq!(at(494.0, &per_frame), at(506.0, &per_frame));
    }

    #[test]
    fn translation_is_left_out_when_not_applied() {
        let on = translated(0.0, Quat64::from_euler_angles(0.05, -0.08, 0.3), [0.01, -0.006, 0.004], true);
        let base = untranslated(&on);
        let backward = |p: &ComputeParams| bits_hash(FrameTransform::at_timestamp(p, 0.0, 0).matrices.iter().flatten().map(|v| *v as f64));
        let forward = |p: &ComputeParams| bits_hash(FrameTransform::at_timestamp_for_points(p, &POINTS, 0.0, Some(0), false).3.iter().flat_map(|m| m.iter().copied()));
        assert_ne!(backward(&on), backward(&base));
        assert_ne!(forward(&on), forward(&base));

        let mut not_applied = on.clone();
        not_applied.apply_optical_translation = false;
        assert_eq!((backward(&not_applied), forward(&not_applied)), (backward(&base), forward(&base)));

        let disabled = with_gyro(&on, |gyro| gyro.optical_translation.as_mut().unwrap().enabled = false);
        let stale = with_gyro(&on, |gyro| gyro.optical_translation.as_mut().unwrap().applies = false);
        for off in [&disabled, &stale] {
            assert_eq!((backward(off), forward(off)), (backward(&base), forward(&base)));
        }

        let (mut suppressed, mut suppressed_base) = (on.clone(), base.clone());
        suppressed.suppress_rotation = true;
        suppressed_base.suppress_rotation = true;
        assert_eq!((backward(&suppressed), forward(&suppressed)), (backward(&suppressed_base), forward(&suppressed_base)));
    }

    #[test]
    fn the_zoom_polygon_moves_with_the_translation() {
        // The adaptive zoom measures at fov = 1, without the applied zoom: `use_fovs` false
        let p = translated(0.0, Quat64::identity(), [0.01, 0.0, 0.0], false);
        let f = FrameTransform::at_timestamp_for_points(&p, &POINTS, 0.0, Some(0), false).2[(0, 0)];
        let at = |p: &ComputeParams| crate::stabilization::undistort_points_with_rolling_shutter(&[POINTS[3]], 0.0, Some(0), p, 1.0, false, false)[0];
        let (a, b) = (at(&untranslated(&p)), at(&p));
        assert!(((b.0 - a.0) as f64 - f * 0.01).abs() < 0.02 && (b.1 - a.1).abs() < 0.02, "{a:?} -> {b:?}");
    }

    /// A plain fisheye calibration on a still camera with one lens breathing table: everything the two transform
    /// paths need to describe the same frame, and nothing that could move between them
    fn params(scale: Vec<f32>, readout_time: f64) -> ComputeParams {
        let mut p = ComputeParams::default();
        p.width = W;
        p.height = H;
        p.output_width = W;
        p.output_height = H;
        p.frame_count = 1;
        p.scaled_fps = 30.0;
        p.fov_scale = 1.0;
        p.frame_readout_time = readout_time;
        p.suppress_rotation = true;
        p.lens_breathing_enabled = true;

        p.lens = LensProfile::default();
        p.lens.calib_dimension = Dimensions { w: W, h: H };
        p.lens.fisheye_params.camera_matrix = vec![
            [1400.0, 0.0, W as f64 / 2.0],
            [0.0, 1400.0, H as f64 / 2.0],
            [0.0, 0.0, 1.0],
        ];
        p.lens.fisheye_params.distortion_coeffs = vec![0.05, -0.012, 0.003, -0.0004];

        let mut md = FileMetadata::default();
        md.lens_breathing = vec![BreathingFrame {
            scale,
            crop_y: 0.0,
            crop_h: H as f32,
        }];
        p.gyro.write().file_metadata = md.into();
        p
    }

    /// A rotating camera with a stabilizing correction and nothing optical: what the translation work must leave
    /// bit-identical
    fn stabilized_params(readout_time: f64) -> ComputeParams {
        let mut p = params(vec![0.82], readout_time);
        p.suppress_rotation = false;
        {
            let mut gyro = p.gyro.write();
            gyro.duration_ms = 1000.0;
            for i in 0..=10i64 {
                let a = i as f64 * 0.01;
                gyro.quaternions.insert(i * 100_000, crate::gyro_source::Quat64::from_euler_angles(a, -0.5 * a, 0.3 * a));
                gyro.smoothed_quaternions.insert(i * 100_000, crate::gyro_source::Quat64::from_euler_angles(-0.2 * a, 0.1 * a, 0.0));
            }
        }
        p
    }

    fn bits_hash(values: impl Iterator<Item = f64>) -> u64 {
        use std::hash::Hasher;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for v in values { h.write_u64(v.to_bits()); }
        h.finish()
    }

    // Taken on the code before the translation work, by running this test with zeros here and copying the values it
    // prints. Depends on the toolchain's float library and DefaultHasher: take them again the same way after changing either
    const GOLDEN: (u64, u64, u64) = (18069766615594339656, 2781745888030282519, 11631596161409366322);

    #[test]
    fn stabilized_transforms_golden() {
        let p = stabilized_params(12.0);
        let backward = bits_hash(FrameTransform::at_timestamp(&p, 500.0, 0).matrices.iter().flatten().map(|v| *v as f64));
        let forward = bits_hash(FrameTransform::at_timestamp_for_points(&p, &POINTS, 500.0, Some(0), false).3.iter().flat_map(|m| m.iter().copied()));
        let mut q = p.clone();
        q.calculate_camera_fovs();
        let zoom = crate::zooming::get_checksum(&q, 0);
        assert_eq!((backward, forward, zoom), GOLDEN, "got {:?}", (backward, forward, zoom));
    }

    /// Output position of a source pixel: the direction the zoom, the sync and the STMap redistort map go
    fn to_output(p: &ComputeParams, pt: (f32, f32)) -> (f32, f32) {
        let (k, coeffs, _p, rotations, is, mesh, fov, r_limit) =
            FrameTransform::at_timestamp_for_points(p, &[pt], 0.0, Some(0), true);
        undistort_points(
            &[pt],
            k,
            &coeffs,
            rotations[0],
            None,
            Some(rotations),
            p,
            1.0,
            fov,
            0.0,
            is,
            mesh,
            r_limit,
        )[0]
    }

    /// Source pixel an output position samples: the direction the render and the STMap undistort map go. `row` is
    /// the matrix the render resolves for the pixel, the source row it lands on
    fn to_source(p: &ComputeParams, pt: (f32, f32), row: usize) -> Option<(f32, f32)> {
        let t = FrameTransform::at_timestamp(p, 0.0, 0);
        let mut kp = t.kernel_params;
        kp.width = W as i32;
        kp.height = H as i32;
        kp.output_width = W as i32;
        kp.output_height = H as i32;
        Stabilization::rotate_and_distort(
            pt,
            row.min(t.matrices.len() - 1),
            &kp,
            &t.matrices,
            &p.distortion_model,
            None,
            kp.r_limit * kp.r_limit,
            &[],
        )
    }

    fn assert_round_trip(p: &ComputeParams) {
        for &pt in &POINTS {
            let out = to_output(p, pt);
            let back = to_source(p, out, pt.1 as usize)
                .unwrap_or_else(|| panic!("{pt:?} -> {out:?} has no source pixel"));
            assert!(
                (back.0 - pt.0).abs() < 0.05 && (back.1 - pt.1).abs() < 0.05,
                "{pt:?} -> {out:?} -> {back:?}"
            );
        }
    }

    #[test]
    fn breathing_zoom_inverts_itself() {
        // One magnification for the whole frame. The STMap export writes one map from each direction, and they
        // only compose back to the identity if both carry the zoom
        assert_round_trip(&params(vec![0.82], 0.0));
    }

    #[test]
    fn per_row_breathing_zoom_inverts_itself() {
        // The focus moving during the readout: every matrix row has a magnification of its own, and the forward
        // direction has to look its own up at the same row
        assert_round_trip(&params(
            (0..9).map(|i| 0.75 + i as f32 * 0.02).collect(),
            12.0,
        ));
    }

    /// A profile with calibrations at several lens positions, on a body that also records the focal length in
    /// millimetres: the projection follows the zoom through the calibrations themselves, and the metadata must
    /// not scale them on top of that. `get_interpolated_lens_at` hands out a profile with no interpolations left
    /// wherever the lookup lands on a knot, so a check on the profile of the frame instead of the one in
    /// `ComputeParams` would turn the scaling on at the knots only and jump the projection there
    #[test]
    fn interpolated_calibrations_are_not_scaled_by_the_metadata_focal_length() {
        let mut p = params(vec![1.0], 0.0);
        p.lens.focal_length = Some(24.0); // the calibration is a wide one; the metadata reaches 70mm
        p.lens.interpolations = Some(serde_json::json!({
            "0.0": { "camera_matrix": [[1000.0, 0.0, 960.0], [0.0, 1000.0, 540.0], [0.0, 0.0, 1.0]] },
            "1.0": { "camera_matrix": [[2000.0, 0.0, 960.0], [0.0, 2000.0, 540.0], [0.0, 0.0, 1.0]] },
        }));
        p.lens
            .resolve_interpolations(&crate::lens_profile_database::LensProfileDatabase::default());
        assert!(p.lens.has_interpolations());

        let mut md = FileMetadata::default();
        for (i, (position, mm)) in [(0.0, 24.0f32), (0.5, 47.0), (1.0, 70.0)]
            .into_iter()
            .enumerate()
        {
            let ts = i as i64 * 33333;
            md.lens_positions.insert(ts, position);
            md.lens_params.insert(
                ts,
                LensParams {
                    focal_length: Some(mm),
                    ..Default::default()
                },
            );
        }
        assert!(md.lens_focal_length_varies());
        p.gyro.write().file_metadata = md.into();

        let fx = |ts: i64| {
            let gyro = p.gyro.read();
            let md = gyro.file_metadata.read();
            FrameTransform::get_lens_data_at_lens_timestamp(&p, &md, ts, false).0[(0, 0)]
        };
        // 1000 to 2000 across the lens travel: the calibrations at the ends and the blend in between, nothing else
        for (ts, expected) in [(0, 1000.0), (33333, 1500.0), (66666, 2000.0)] {
            assert!(
                (fx(ts) - expected).abs() < 1e-6,
                "at {ts}: {} instead of {expected}",
                fx(ts)
            );
        }
    }

    #[test]
    fn breathing_stays_out_of_the_fov_measurement() {
        // The measurements at fov = 1 (zoom polygon, sync, features) describe the picture the zoom is fitted
        // around; a zoom folded into them would only let the fit relax and undo it, like the focal length
        // compensation next to it
        let on = params(vec![0.82], 0.0);
        let mut off = params(vec![0.82], 0.0);
        off.lens_breathing_enabled = false;
        let at = |p: &ComputeParams, use_fovs: bool| {
            FrameTransform::at_timestamp_for_points(p, &POINTS, 0.0, Some(0), use_fovs).3
        };
        assert_eq!(at(&on, false), at(&off, false));
        assert_ne!(at(&on, true), at(&off, true));
    }

    #[test]
    fn host_input_stretch_preserves_every_sampled_projection() {
        use crate::lens_profile::{Dimensions, with_parsed_interpolations_for_test};
        for host in [[1.8, 1.0], [1.0, 1.5], [1.8, 1.5]] {
            for sample_count in [0, 1, 404] {
                for interpolated in [false, true] {
                    let manager = crate::StabilizationManager::default();
                    {
                        let mut p = manager.params.write();
                        p.size = (1000, 800);
                        p.output_size = ((1000.0 * host[0]) as usize, (800.0 * host[1]) as usize);
                        p.frame_count = 3;
                        p.fps = 50.0;
                        p.duration_ms = 60.0;
                    }
                    let mut lens = crate::lens_profile::LensProfile::default();
                    lens.calib_dimension = Dimensions { w: 1800, h: 1200 };
                    lens.orig_dimension = lens.calib_dimension.clone();
                    lens.fisheye_params.camera_matrix = vec![
                        [1200.0, 0.0, 900.0], [0.0, 1300.0, 600.0], [0.0, 0.0, 1.0],
                    ];
                    lens.fisheye_params.distortion_coeffs = vec![0.0; 4];
                    lens.set_input_stretch(host[0], host[1]);
                    if interpolated {
                        let mut second = lens.clone();
                        second.set_input_stretch(host[0] * 1.2, host[1] * 1.1);
                        second.fisheye_params.camera_matrix[0][0] = 1600.0;
                        lens = with_parsed_interpolations_for_test(lens.clone(), [(0.0, lens), (1.0, second)]);
                    }
                    *manager.lens.write() = lens;
                    {
                        let gyro = manager.gyro.read();
                        let mut md = gyro.file_metadata.write();
                        for i in 0..sample_count {
                            md.lens_positions.insert(i * 20000, (i % 3) as f64 / 2.0);
                            md.lens_params.insert(i * 20000, LensParams { focal_length: Some(85.0), ..Default::default() });
                        }
                    }
                    let before = ComputeParams::from_manager(&manager);
                    let metadata_before = manager.gyro.read().file_metadata.read().clone();
                    manager.disable_lens_stretch(true);
                    let after = ComputeParams::from_manager(&manager);
                    // Repeating the same input declaration must never compound the scale.
                    manager.disable_lens_stretch(true);
                    assert_eq!(manager.params.read().size, (after.width, after.height));
                    assert_eq!(manager.lens.read().input_horizontal_stretch, host[0]);
                    assert_eq!(manager.gyro.read().file_metadata.read().lens_positions, metadata_before.lens_positions);
                    for t in [0.0, 20.0, 40.0] {
                        let a = FrameTransform::get_lens_data_at_timestamp(&before, t, false);
                        let b = FrameTransform::get_lens_data_at_timestamp(&after, t, false);
                        assert!((a.0 - b.0).norm() < 1e-9, "host={host:?} samples={sample_count} interp={interpolated} t={t}");
                        assert!((a.3 - b.3 * host[0]).abs() < 1e-12);
                        assert!((a.4 - b.4 * host[1]).abs() < 1e-12);
                        let ka = FrameTransform::get_new_k(&before, &a.0, before.width as f64 / before.output_width as f64);
                        let kb = FrameTransform::get_new_k(&after, &b.0, after.width as f64 / after.output_width as f64);
                        assert!((ka - kb).norm() < 1e-9);
                    }
                    let mut a = before;
                    let mut b = after;
                    if interpolated && sample_count > 1 {
                        // Test the actual render/point maps at a non-base lens knot.
                        // Matrix equality alone misses a point path using base stretch.
                        for delay in [0, 1] {
                            let mut render_params = b.clone();
                            render_params.lens_metadata_delay_frames = delay;
                            for timestamp in [0.0, 20.0, 40.0, 9000.0] {
                                let out = (b.output_width as f32 * 0.42, b.output_height as f32 * 0.43);
                                let transform = FrameTransform::at_timestamp(&render_params, timestamp, 0);
                                let mut kp = transform.kernel_params;
                                kp.width = b.width as i32;
                                kp.height = b.height as i32;
                                kp.output_width = b.output_width as i32;
                                kp.output_height = b.output_height as i32;
                                let source = Stabilization::rotate_and_distort(out, 0, &kp,
                                    &transform.matrices, &b.distortion_model, None, 0.0, &[]).unwrap();
                                let back = crate::stabilization::undistort_points_with_rolling_shutter(
                                    &[source], timestamp, Some(0), &render_params, 1.0, true, false)[0];
                                assert!((back.0 - out.0).abs() < 0.05 && (back.1 - out.1).abs() < 0.05,
                                    "host={host:?} delay={delay} t={timestamp} output={out:?} source={source:?} recovered={back:?}");
                            }
                        }
                    }
                    a.calculate_camera_fovs();
                    b.calculate_camera_fovs();
                    for (x, y) in a.camera_diagonal_fovs.iter().zip(&b.camera_diagonal_fovs) {
                        assert!((x - y).abs() < 1e-9, "FOV changed: {x} vs {y}");
                    }
                }
            }
        }
    }

    #[test]
    fn host_input_stretch_keeps_telemetry_in_sensor_coordinates() {
        for adjust_size in [false, true] {
            for pixel_focal_length in [None, Some((14000.0, 14000.0))] {
                let manager = crate::StabilizationManager::default();
                {
                    let mut p = manager.params.write();
                    p.size = (1000, 800);
                    p.output_size = (1800, 1200);
                }
                manager.lens.write().set_input_stretch(1.8, 1.5);
                manager.gyro.write().file_metadata.write().lens_params.insert(0, LensParams {
                    pixel_focal_length,
                    focal_length: Some(85.0),
                    principal_point: Some((500.0, 400.0)),
                    pixel_pitch: Some((6000, 6000)),
                    capture_area_size: Some((1000.0, 800.0)),
                    ..Default::default()
                });
                let before = ComputeParams::from_manager(&manager);
                manager.disable_lens_stretch(adjust_size);
                let after = ComputeParams::from_manager(&manager);
                for inverted in [false, true] {
                    let a = FrameTransform::get_lens_data_at_timestamp(&before, 0.0, inverted);
                    let b = FrameTransform::get_lens_data_at_timestamp(&after, 0.0, inverted);
                    assert!((a.0 - b.0).norm() < 1e-9, "adjust={adjust_size} inverted={inverted}");
                }
            }
        }
    }
}
