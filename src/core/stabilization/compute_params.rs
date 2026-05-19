// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use super::StabilizationManager;
use super::distortion_models::DistortionModel;
use crate::GyroSource;
use crate::keyframes::KeyframeManager;
use crate::lens_profile::LensProfile;
use crate::stabilization_params::ReadoutDirection;
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::Arc;

// Anamorphic-aware decay factor for lens_correction_amount.
// Applying the calibrated isotropic radial polynomial at full strength on an
// anamorphic squeeze over-corrects (straight lines bow outward). Attenuate the
// runtime strength so the UI 100% slider matches the visually correct render.
// f(λ) = 2 / (1 + λ²),  λ = max(stretch_h, stretch_v)
// Heuristic motivated by ⟨r_desqueezed² / r_sensor²⟩ averaging across axes.
pub(crate) fn anamorphic_lens_correction_decay(stretch_h: f64, stretch_v: f64) -> f64 {
    let lambda = stretch_h.max(stretch_v).max(1.0);
    if lambda <= 1.01 {
        return 1.0;
    }
    2.0 / (1.0 + lambda * lambda)
}

#[derive(Default, Clone)]
pub struct ComputeParams {
    pub gyro: Arc<RwLock<GyroSource>>,
    pub fovs: Vec<f64>,
    pub minimal_fovs: Vec<f64>,
    pub keyframes: KeyframeManager,
    pub gyro_offsets: BTreeMap<i64, f64>,
    pub lens: LensProfile,
    pub camera_diagonal_fovs: Vec<f64>,

    pub frame_count: usize,
    pub fov_scale: f64,
    pub fov_overview: bool,
    pub show_safe_area: bool,
    pub width: usize,
    pub height: usize,
    pub output_width: usize,
    pub output_height: usize,
    pub video_rotation: f64,
    pub lens_correction_amount: f64,
    pub light_refraction_coefficient: f64,
    pub video_speed: f64,
    pub video_speed_affects_smoothing: bool,
    pub video_speed_affects_zooming: bool,
    pub video_speed_affects_zooming_limit: bool,
    pub background: nalgebra::Vector4<f32>,
    pub background_mode: crate::stabilization_params::BackgroundMode,
    pub background_margin: f64,
    pub background_margin_feather: f64,
    pub frame_readout_time: f64,
    pub frame_readout_direction: ReadoutDirection,
    pub trim_ranges: Vec<(f64, f64)>,
    pub scaled_fps: f64,
    pub scaled_duration_ms: f64,
    pub adaptive_zoom_window: f64,
    pub adaptive_zoom_center_offset: (f64, f64),
    pub adaptive_zoom_method: i32,
    pub additional_rotation: (f64, f64, f64),
    pub additional_translation: (f64, f64, f64),
    pub framebuffer_inverted: bool,
    pub suppress_rotation: bool,
    pub fov_algorithm_margin: f32,
    pub smoothing_fov_limit_per_frame: Vec<f64>,
    pub max_zoom: Option<f64>,
    pub max_zoom_iterations: usize,

    pub zooming_debug_points: bool,

    pub distortion_model: DistortionModel,
    pub digital_lens: Option<DistortionModel>,
    pub digital_lens_params: Option<Vec<f64>>,
}
impl ComputeParams {
    pub fn from_manager(mgr: &StabilizationManager) -> Self {
        let params = mgr.params.read();

        let lens = mgr.lens.read().clone();

        let distortion_model = DistortionModel::from_name(
            lens.distortion_model.as_deref().unwrap_or("opencv_fisheye"),
        );
        let digital_lens = lens
            .digital_lens
            .as_ref()
            .map(|x| DistortionModel::from_name(&x));

        let digital_lens_params = lens.digital_lens_params.clone();
        let keyframes = mgr.keyframes.read().clone();
        let gyro_offsets = keyframes.gyro_offsets().clone();

        Self {
            gyro: mgr.gyro.clone(),
            lens,
            camera_diagonal_fovs: Vec::new(),

            smoothing_fov_limit_per_frame: Vec::new(),
            max_zoom: params.max_zoom.clone(),
            max_zoom_iterations: params.max_zoom_iterations,

            frame_count: params.frame_count,
            fov_scale: params.fov,
            fov_overview: params.fov_overview,
            show_safe_area: params.show_safe_area,
            fovs: params.fovs.clone(),
            minimal_fovs: params.minimal_fovs.clone(),
            width: params.size.0.max(1),
            height: params.size.1.max(1),
            output_width: params.output_size.0.max(1),
            output_height: params.output_size.1.max(1),
            video_rotation: params.video_rotation,
            background: params.background,
            background_mode: params.background_mode,
            background_margin: params.background_margin,
            background_margin_feather: params.background_margin_feather,
            lens_correction_amount: params.lens_correction_amount,
            light_refraction_coefficient: params.light_refraction_coefficient,
            framebuffer_inverted: params.framebuffer_inverted,
            frame_readout_time: params.frame_readout_time,
            frame_readout_direction: params.frame_readout_direction,
            trim_ranges: params.trim_ranges.clone(),
            scaled_fps: params.get_scaled_fps(),
            scaled_duration_ms: params.get_scaled_duration_ms(),
            adaptive_zoom_window: params.adaptive_zoom_window,
            adaptive_zoom_center_offset: params.adaptive_zoom_center_offset,
            additional_rotation: params.additional_rotation,
            additional_translation: params.additional_translation,
            adaptive_zoom_method: params.adaptive_zoom_method,
            video_speed: params.video_speed,
            video_speed_affects_smoothing: params.video_speed_affects_smoothing,
            video_speed_affects_zooming: params.video_speed_affects_zooming,
            video_speed_affects_zooming_limit: params.video_speed_affects_zooming_limit,

            distortion_model,
            digital_lens,
            digital_lens_params,
            suppress_rotation: false,
            fov_algorithm_margin: 2.0,

            keyframes,
            gyro_offsets,

            zooming_debug_points: false,
        }
    }

    pub fn anamorphic_lens_correction_decay(&self) -> f64 {
        // Read the raw stretch mirror when present so the decay factor
        // `α = 2/(1+λ²)` stays invariant under `disable_lens_stretch(adjust_size=true)`
        // (which resets the mutating fields to 1.0 but leaves the raw mirror
        // untouched). Fall back to the mutating field for legacy lens profiles
        // / call sites that pre-date the raw mirror; this preserves the pre-§10
        // numerical result whenever the raw fields are unset.
        let h_raw = self.lens.input_horizontal_stretch_raw.unwrap_or(self.lens.input_horizontal_stretch);
        let v_raw = self.lens.input_vertical_stretch_raw.unwrap_or(self.lens.input_vertical_stretch);
        let h = if h_raw > 0.01 { h_raw } else { 1.0 };
        let v = if v_raw > 0.01 { v_raw } else { 1.0 };
        anamorphic_lens_correction_decay(h, v)
    }

    pub fn apply_anamorphic_decay(&self, raw_lens_correction_amount: f64) -> f64 {
        (raw_lens_correction_amount * self.anamorphic_lens_correction_decay()).clamp(0.0, 1.0)
    }

    pub fn video_timestamp_for_gyro_timestamp(&self, timestamp_ms: f64) -> f64 {
        timestamp_ms + GyroSource::offset_at_timestamp(&self.gyro_offsets, timestamp_ms)
    }

    pub fn frame_at_gyro_timestamp(&self, timestamp_ms: f64) -> usize {
        crate::frame_at_timestamp(
            self.video_timestamp_for_gyro_timestamp(timestamp_ms),
            self.scaled_fps,
        )
        .max(0) as usize
    }

    pub fn calculate_camera_fovs(&mut self) {
        let frame_count = if self.gyro.read().file_metadata.read().lens_params.len() > 1 {
            self.frame_count
        } else {
            1 // FOV is constant (ie. lens is fixed focal length)
        };
        self.camera_diagonal_fovs = Vec::with_capacity(frame_count);
        for f in 0..frame_count as i32 {
            let timestamp = crate::timestamp_at_frame(f, self.scaled_fps);
            let (camera_matrix, _, _, _, _, _) =
                crate::stabilization::FrameTransform::get_lens_data_at_timestamp(
                    &self, timestamp, false,
                );
            let diag_length = ((self.width.pow(2) + self.height.pow(2)) as f64).sqrt();
            // let diag_pixel_focal_length = (camera_matrix[(0, 0)].powi(2) + camera_matrix[(1, 1)].powi(2)).sqrt();
            let d_fov = 2.0 * ((diag_length / (2.0 * camera_matrix[(1, 1)])).atan()) * 180.0
                / std::f64::consts::PI;
            self.camera_diagonal_fovs.push(d_fov);
        }
    }
}

impl std::fmt::Debug for ComputeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let gyro = self.gyro.read();
        f.debug_struct("ComputeParams")
            .field("gyro.imu_orientation", &gyro.imu_transforms.imu_orientation)
            .field(
                "gyro.imu_rotation",
                &gyro.imu_transforms.imu_rotation_angles,
            )
            .field(
                "gyro.acc_rotation",
                &gyro.imu_transforms.acc_rotation_angles,
            )
            .field("gyro.duration_ms", &gyro.duration_ms)
            .field("gyro.imu_lpf", &gyro.imu_transforms.imu_lpf)
            .field("gyro.imu_mf", &gyro.imu_transforms.imu_mf)
            .field("gyro.gyro_bias", &gyro.imu_transforms.gyro_bias)
            .field("gyro.integration_method", &gyro.integration_method)
            .field("fovs.len", &self.fovs.len())
            .field("keyframed", &self.keyframes.get_all_keys())
            .field("frame_count", &self.frame_count)
            .field("fov_scale", &self.fov_scale)
            .field("fov_overview", &self.fov_overview)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("output_width", &self.output_width)
            .field("output_height", &self.output_height)
            .field("video_rotation", &self.video_rotation)
            .field("lens_correction_amount", &self.lens_correction_amount)
            .field(
                "light_refraction_coefficient",
                &self.light_refraction_coefficient,
            )
            .field("background_mode", &self.background_mode)
            .field("background_margin", &self.background_margin)
            .field("background_margin_feather", &self.background_margin_feather)
            .field("frame_readout_time", &self.frame_readout_time)
            .field("frame_readout_direction", &self.frame_readout_direction)
            .field("trim_ranges", &self.trim_ranges)
            .field("scaled_fps", &self.scaled_fps)
            .field("adaptive_zoom_window", &self.adaptive_zoom_window)
            .field(
                "adaptive_zoom_center_offset",
                &self.adaptive_zoom_center_offset,
            )
            .field("additional_rotation", &self.additional_rotation)
            .field("additional_translation", &self.additional_translation)
            .field("adaptive_zoom_method", &self.adaptive_zoom_method)
            .field("framebuffer_inverted", &self.framebuffer_inverted)
            .field("zooming_debug_points", &self.zooming_debug_points)
            .field("distortion_model", &self.distortion_model.id())
            .field(
                "digital_lens",
                &self.digital_lens.as_ref().map(|x| x.id()).unwrap_or("None"),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::anamorphic_lens_correction_decay;

    #[test]
    fn anamorphic_decay_identity_when_no_stretch() {
        assert_eq!(anamorphic_lens_correction_decay(1.0, 1.0), 1.0);
    }

    #[test]
    fn anamorphic_decay_15x_matches_formula() {
        let d = anamorphic_lens_correction_decay(1.0, 1.5);
        assert!((d - 0.6153846).abs() < 1e-5, "got {d}");
    }

    #[test]
    fn anamorphic_decay_uses_max_axis() {
        // Horizontal-only stretch and vertical-only stretch produce identical decay.
        let h = anamorphic_lens_correction_decay(1.5, 1.0);
        let v = anamorphic_lens_correction_decay(1.0, 1.5);
        assert!((h - v).abs() < 1e-9);
    }

    #[test]
    fn anamorphic_decay_monotonic_in_lambda() {
        let d133 = anamorphic_lens_correction_decay(1.33, 1.0);
        let d150 = anamorphic_lens_correction_decay(1.5, 1.0);
        let d200 = anamorphic_lens_correction_decay(2.0, 1.0);
        assert!(d133 > d150 && d150 > d200);
    }

    #[test]
    fn anamorphic_decay_clamp_below_epsilon() {
        // Stretch values within tolerance of 1.0 should not trigger decay.
        assert_eq!(anamorphic_lens_correction_decay(1.005, 1.005), 1.0);
        assert_eq!(anamorphic_lens_correction_decay(0.5, 0.5), 1.0);
    }

    // §10.4/§10.5: decay must stay invariant when the mutating stretch fields
    // are reset to 1.0 by `disable_lens_stretch(adjust_size=true)`. We
    // construct a ComputeParams by hand (without a full StabilizationManager)
    // since the decay reads `self.lens` only.
    use super::ComputeParams;
    use crate::lens_profile::LensProfile;

    fn build_compute_params_with_stretch(h: f64, v: f64, raw: bool) -> ComputeParams {
        let mut lens = LensProfile::default();
        if raw {
            lens.set_input_stretch(h, v);
        } else {
            lens.input_horizontal_stretch = h;
            lens.input_vertical_stretch = v;
        }
        let mut cp = ComputeParams::default();
        cp.lens = lens;
        cp
    }

    fn simulate_disable_lens_stretch(cp: &mut ComputeParams) {
        // Mirror what `StabilizationManager::disable_lens_stretch(adjust_size=true)`
        // does to the LensProfile: clear the mutating stretch fields but
        // leave the raw mirror untouched. Width/height changes on size are
        // not relevant to `apply_anamorphic_decay`.
        cp.lens.input_horizontal_stretch = 1.0;
        cp.lens.input_vertical_stretch = 1.0;
    }

    #[test]
    fn apply_anamorphic_decay_invariant_under_disable_lens_stretch_lambda_2_0() {
        let mut cp = build_compute_params_with_stretch(2.0, 1.0, true);
        let pre = cp.apply_anamorphic_decay(0.5);
        // Pre: α = 2/(1+4) = 0.4, decay = 0.5 * 0.4 = 0.2 (clamped). All good.
        assert!((pre - 0.2).abs() < f64::EPSILON, "got pre={pre}");
        simulate_disable_lens_stretch(&mut cp);
        let post = cp.apply_anamorphic_decay(0.5);
        assert!((pre - post).abs() < f64::EPSILON, "pre={pre} post={post}");
    }

    #[test]
    fn apply_anamorphic_decay_invariant_under_disable_lens_stretch_lambda_1_5() {
        let mut cp = build_compute_params_with_stretch(1.5, 1.0, true);
        let pre = cp.apply_anamorphic_decay(1.0);
        // Pre: α = 2/(1+2.25) = 0.6153846..., decay = 0.6153846 (clamped).
        assert!((pre - 0.61538461538461542).abs() < 1e-12, "got pre={pre}");
        simulate_disable_lens_stretch(&mut cp);
        let post = cp.apply_anamorphic_decay(1.0);
        assert!((pre - post).abs() < f64::EPSILON, "pre={pre} post={post}");
    }

    #[test]
    fn apply_anamorphic_decay_invariant_under_disable_lens_stretch_vertical_1_5() {
        let mut cp = build_compute_params_with_stretch(1.0, 1.5, true);
        let pre = cp.apply_anamorphic_decay(1.0);
        assert!((pre - 0.61538461538461542).abs() < 1e-12, "got pre={pre}");
        simulate_disable_lens_stretch(&mut cp);
        let post = cp.apply_anamorphic_decay(1.0);
        assert!((pre - post).abs() < f64::EPSILON, "pre={pre} post={post}");
    }

    #[test]
    fn apply_anamorphic_decay_raw_unset_falls_back_to_mutating() {
        // Defensive fallback for code paths that construct a LensProfile
        // without going through `set_input_stretch*` helpers AND without
        // calling `LensProfile::init()`. In production all entry points
        // (serde load, helper writes, calibrator) backfill or set raw, so
        // this branch is a safety net rather than a supported workflow.
        // See `lens_profile_init_backfills_raw_*` in lens_profile.rs tests
        // for the production-path coverage.
        let cp = build_compute_params_with_stretch(2.0, 1.0, false);
        let d = cp.apply_anamorphic_decay(0.5);
        assert!((d - 0.2).abs() < f64::EPSILON, "got {d}");
    }

    // D.1 end-to-end coverage: production lens-load entry points (.gyroflow /
    // lens.json serde) MUST backfill raw via `LensProfile::init()` so that
    // any subsequent `disable_lens_stretch(adjust_size=true)` keeps decay
    // invariant. Pre-§10/§A.0 this test would fail because `init()` did not
    // populate raw and the decay would silently drop to α=1.0 after disable.
    #[test]
    fn lens_json_load_then_disable_lens_stretch_preserves_decay() {
        // Synthetic anamorphic 1.5× lens profile JSON. raw_* fields are
        // NOT serialized (#[serde(skip)]), so the loaded profile starts
        // with raw=None and the init() backfill is the only path that can
        // populate it before decay reads it.
        let json = r#"{
            "name": "test-1.5x",
            "calib_dimension": { "w": 1920, "h": 1080 },
            "orig_dimension": { "w": 1920, "h": 1080 },
            "input_horizontal_stretch": 1.5,
            "input_vertical_stretch": 1.0,
            "fisheye_params": {
                "RMS_error": 0.0,
                "camera_matrix": [[1000.0, 0.0, 960.0], [0.0, 1000.0, 540.0], [0.0, 0.0, 1.0]],
                "distortion_coeffs": [0.0, 0.0, 0.0, 0.0]
            }
        }"#;
        let lens = LensProfile::from_json(json).expect("json parse");
        // init() (called by from_json) must have backfilled raw from mutating.
        assert_eq!(lens.input_horizontal_stretch_raw(), Some(1.5));
        assert_eq!(lens.input_vertical_stretch_raw(), Some(1.0));

        let mut cp = ComputeParams::default();
        cp.lens = lens;
        let pre = cp.apply_anamorphic_decay(1.0);
        // α = 2/(1+λ²) for λ=max(1.5, 1.0)=1.5 → 2/(1+2.25) = 0.6153846...
        assert!(
            (pre - 0.61538461538461542).abs() < 1e-12,
            "pre decay should reflect raw stretch λ=1.5; got {pre}",
        );

        // Mimic disable_lens_stretch(adjust_size=true): mutating fields to
        // 1.0, raw left alone. decay must remain 0.6154.
        simulate_disable_lens_stretch(&mut cp);
        let post = cp.apply_anamorphic_decay(1.0);
        assert!(
            (pre - post).abs() < f64::EPSILON,
            "post-disable decay drifted: pre={pre} post={post}",
        );
    }
}
