// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use itertools::{Either, Itertools};
use nalgebra::Vector4;
use parking_lot::Mutex;
use qmetaobject::*;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::mpsc::{Receiver, Sender, TryRecvError};

use qml_video_rs::video_item::MDKVideoItem;

use crate::core;
use crate::core::StabilizationManager;
use crate::log_context;
#[cfg(feature = "opencv")]
use crate::core::calibration::LensCalibrator;
use crate::core::filesystem;
use crate::core::keyframes::*;
use crate::core::stabilization::KernelParamsFlags;
use crate::core::synchronization;
use crate::core::synchronization::AutosyncProcess;
use crate::niyien_device::{DeviceCommand, DeviceConnectionStatus, DeviceEvent, DeviceManager};
use crate::qt_gpu::qrhi_undistort;
use crate::rendering;
use crate::rendering::VideoProcessor;
use crate::ui::components::FrequencyGraph::FrequencyGraph;
use crate::ui::components::TimelineGyroChart::TimelineGyroChart;
use crate::ui::components::TimelineKeyframesView::TimelineKeyframesView;
use crate::util;
use crate::wrap_simple_method;

#[derive(Default, SimpleListItem)]
struct OffsetItem {
    pub timestamp_us: i64,
    pub offset_ms: f64,
    pub linear_offset_ms: f64,
}

#[derive(Default, SimpleListItem)]
struct CalibrationItem {
    pub timestamp_us: i64,
    pub sharpness: f64,
    pub is_forced: bool,
}

#[allow(non_snake_case)]
#[derive(Default, QObject)]
pub struct Controller {
    base: qt_base_class!(trait QObject),

    init_player: qt_method!(fn(&self, player: QJSValue)),
    reset_player: qt_method!(fn(&self, player: QJSValue)),
    load_video: qt_method!(fn(&self, url: QUrl, player: QJSValue)),
    log_video_file_dialog: qt_method!(
        fn(&self, selected_count: i32, first_url: QUrl, selected_file: QUrl, current_folder: QUrl, selected_files_raw: QString)
    ),
    log_video_metadata_state: qt_method!(
        fn(&self, width: i32, height: i32, duration_ms: f64, fps: f64, frame_count: i32)
    ),
    video_file_loaded: qt_method!(fn(&self, player: QJSValue)),
    load_telemetry: qt_method!(
        fn(
            &self,
            url: QUrl,
            is_video: bool,
            player: QJSValue,
            sample_index: i32,
            project_version: u32,
        )
    ),
    get_image_sequence_fps: qt_method!(fn(&self, url: QUrl) -> f64),
    peek_container_rotation: qt_method!(fn(&self, url: QUrl) -> i32),
    load_lens_profile: qt_method!(fn(&mut self, url_or_id: QString)),
    get_preset_contents: qt_method!(fn(&mut self, url_or_id: QString) -> QString),
    lens_group_config: qt_property!(QString; READ get_lens_group_config NOTIFY lens_group_config_changed),
    lens_group_config_changed: qt_signal!(),
    set_lens_group_config: qt_method!(fn(&self, json: String)),
    apply_lens_group_to_main: qt_method!(fn(&self, lens_index: usize) -> QString),
    preview_lens_group_config: qt_method!(fn(&self, json: String, lens_index: usize) -> bool),
    lens_group_status: qt_property!(QString; READ get_lens_group_status NOTIFY lens_group_status_changed),
    lens_group_status_changed: qt_signal!(),
    get_lens_group_status: qt_method!(fn(&self) -> QString),
    refresh_lens_group_status: qt_method!(fn(&self)),
    #[allow(dead_code)]
    lens_group_manual_edit: qt_property!(bool; READ get_lens_group_manual_edit WRITE set_lens_group_manual_edit NOTIFY lens_group_manual_edit_changed),
    lens_group_manual_edit_changed: qt_signal!(),
    get_lens_presets: qt_method!(fn(&self) -> QString),
    has_neuflow_support: qt_method!(fn(&self) -> bool),
    export_lens_profile: qt_method!(fn(&mut self, url: QUrl, info: QJsonObject, upload: bool)),
    export_lens_profile_filename: qt_method!(fn(&mut self, info: QJsonObject) -> QString),

    set_of_method: qt_method!(fn(&self, v: u32)),
    start_autosync:
        qt_method!(fn(&mut self, timestamps_fract: String, sync_params: String, mode: String)),
    update_chart: qt_method!(fn(&self, chart: QJSValue, series: String) -> bool),
    update_frequency_graph:
        qt_method!(fn(&self, graph: QJSValue, idx: usize, ts: f64, sr: f64, fft_size: usize)),
    update_keyframes_view: qt_method!(fn(&self, kfview: QJSValue)),
    rolling_shutter_estimated: qt_signal!(rolling_shutter: f64),
    estimate_bias: qt_method!(fn(&self, timestamp_fract: QString)),
    bias_estimated: qt_signal!(bx: f64, by: f64, bz: f64),
    orientation_guessed: qt_signal!(orientation: QString),
    get_optimal_sync_points:
        qt_method!(fn(&mut self, target_sync_points: usize, initial_offset: f64) -> QString),

    start_autocalibrate: qt_method!(
        fn(
            &self,
            max_points: usize,
            every_nth_frame: usize,
            iterations: usize,
            max_sharpness: f64,
            custom_timestamp_ms: f64,
            no_marker: bool,
        )
    ),

    telemetry_loaded: qt_signal!(is_main_video: bool, filename: QString, camera: QString, additional_data: QJsonObject),
    lens_profile_loaded: qt_signal!(lens_json: QString, filepath: QString, checksum: QString),

    set_smoothing_method: qt_method!(fn(&self, index: usize) -> QJsonArray),
    get_smoothing_max_angles: qt_method!(fn(&self) -> QJsonArray),
    get_smoothing_status: qt_method!(fn(&self) -> QJsonArray),
    set_smoothing_param: qt_method!(fn(&self, name: QString, val: f64)),
    set_horizon_lock: qt_method!(
        fn(
            &self,
            lock_percent: f64,
            roll: f64,
            lock_pitch: bool,
            pitch: f64,
            automatic_lock: bool,
            turn_threshold: f64,
            turn_smoothing_ms: f64,
            turn_multiplier: f64,
            tilt_accel_limit: f64,
        )
    ),
    get_turn_speed: qt_method!(fn(&self, timestamp_ms: f64) -> f64),
    get_x_angle: qt_method!(fn(&self, timestamp_ms: f64) -> f64),
    set_use_gravity_vectors: qt_method!(fn(&self, v: bool)),
    set_horizon_lock_integration_method: qt_method!(fn(&self, v: i32)),
    set_preview_resolution: qt_method!(fn(&mut self, target_height: i32, player: QJSValue)),
    set_processing_resolution: qt_method!(fn(&mut self, target_height: i32)),
    set_background_color: qt_method!(fn(&self, color: QString, player: QJSValue)),
    set_integration_method: qt_method!(fn(&self, index: usize)),

    set_offset: qt_method!(fn(&self, timestamp_us: i64, offset_ms: f64)),
    remove_offset: qt_method!(fn(&self, timestamp_us: i64)),
    clear_offsets: qt_method!(fn(&self)),
    offset_at_video_timestamp: qt_method!(fn(&self, timestamp_us: i64) -> f64),
    offsets_model: qt_property!(RefCell<SimpleListModel<OffsetItem>>; NOTIFY offsets_updated),
    offsets_updated: qt_signal!(),

    load_profiles: qt_method!(fn(&self, reload_from_disk: bool)),
    all_profiles_loaded: qt_signal!(),
    search_lens_profile_finished: qt_signal!(profiles: QVariantList),
    search_lens_profile: qt_method!(
        fn(
            &self,
            text: QString,
            favorites: QVariantList,
            aspect_ratio: i32,
            aspect_ratio_swapped: i32,
        )
    ),
    fetch_profiles_from_github: qt_method!(fn(&self)),
    lens_profiles_updated: qt_signal!(reload_from_disk: bool),

    set_sync_lpf: qt_method!(fn(&self, lpf: f64)),
    set_imu_lpf: qt_method!(fn(&self, lpf: f64)),
    set_imu_median_filter: qt_method!(fn(&self, size: i32)),
    set_imu_rotation: qt_method!(fn(&self, pitch_deg: f64, roll_deg: f64, yaw_deg: f64)),
    set_acc_rotation: qt_method!(fn(&self, pitch_deg: f64, roll_deg: f64, yaw_deg: f64)),
    set_imu_orientation: qt_method!(fn(&self, orientation: String)),
    set_imu_bias: qt_method!(fn(&self, bx: f64, by: f64, bz: f64)),
    recompute_gyro: qt_method!(fn(&self)),

    override_video_fps: qt_method!(fn(&self, fps: f64, recompute: bool)),
    get_org_duration_ms: qt_method!(fn(&self) -> f64),
    get_scaled_duration_ms: qt_method!(fn(&self) -> f64),
    get_scaled_fps: qt_method!(fn(&self) -> f64),
    set_video_created_at: qt_method!(fn(&self, timestamp_ms: f64)),

    recompute_threaded: qt_method!(fn(&mut self)),
    request_recompute: qt_signal!(),

    stab_enabled: qt_property!(bool; WRITE set_stab_enabled),
    show_detected_features: qt_property!(bool; WRITE set_show_detected_features),
    show_optical_flow: qt_property!(bool; WRITE set_show_optical_flow),
    fov: qt_property!(f64; WRITE set_fov),
    fov_overview: qt_property!(bool; WRITE set_fov_overview),
    show_safe_area: qt_property!(bool; WRITE set_show_safe_area),
    frame_readout_time: qt_property!(f64; WRITE set_frame_readout_time),
    frame_readout_direction: qt_property!(i32; WRITE set_frame_readout_direction),

    adaptive_zoom: qt_property!(f64; WRITE set_adaptive_zoom),
    zooming_center_x: qt_property!(f64; WRITE set_zooming_center_x),
    zooming_center_y: qt_property!(f64; WRITE set_zooming_center_y),
    zooming_method: qt_property!(i32; WRITE set_zooming_method),

    additional_rotation_x: qt_property!(f64; WRITE set_additional_rotation_x),
    additional_rotation_y: qt_property!(f64; WRITE set_additional_rotation_y),
    additional_rotation_z: qt_property!(f64; WRITE set_additional_rotation_z),
    additional_translation_x: qt_property!(f64; WRITE set_additional_translation_x),
    additional_translation_y: qt_property!(f64; WRITE set_additional_translation_y),
    additional_translation_z: qt_property!(f64; WRITE set_additional_translation_z),

    lens_correction_amount: qt_property!(f64; WRITE set_lens_correction_amount),
    frame_offset: qt_property!(i32; WRITE set_frame_offset),
    light_refraction_coefficient: qt_property!(f64; WRITE set_light_refraction_coefficient),
    set_video_speed: qt_method!(fn(&self, v: f64, s: bool, z: bool, zl: bool)),
    set_max_zoom: qt_method!(fn(&self, v: f64, iters: usize)),

    input_horizontal_stretch: qt_property!(f64; WRITE set_input_horizontal_stretch),
    input_vertical_stretch: qt_property!(f64; WRITE set_input_vertical_stretch),
    lens_is_asymmetrical: qt_property!(bool; WRITE set_lens_is_asymmetrical),

    background_mode: qt_property!(i32; WRITE set_background_mode),
    background_margin: qt_property!(f64; WRITE set_background_margin),
    background_margin_feather: qt_property!(f64; WRITE set_background_margin_feather),

    lens_loaded: qt_property!(bool; NOTIFY lens_changed),
    set_lens_param: qt_method!(fn(&self, param: QString, value: f64)),
    set_user_focal_length: qt_method!(fn(&self, focal_length_mm: f64)),
    lens_changed: qt_signal!(),

    gyro_loaded: qt_property!(bool; NOTIFY gyro_changed),
    gyro_changed: qt_signal!(),

    gyro_has_raw_imu: qt_property!(bool; READ gyro_has_raw_imu NOTIFY gyro_changed),
    gyro_has_quaternions: qt_property!(bool; READ gyro_has_quaternions NOTIFY gyro_changed),
    gyro_has_accurate_timestamps: qt_property!(bool; READ gyro_has_accurate_timestamps NOTIFY gyro_changed),
    has_gravity_vectors: qt_property!(bool; READ has_gravity_vectors NOTIFY gyro_changed),

    compute_progress: qt_signal!(id: u64, progress: f64),
    sync_progress: qt_signal!(progress: f64, ready: usize, total: usize),

    set_video_rotation: qt_method!(fn(&self, angle: f64)),

    set_trim_ranges: qt_method!(fn(&self, trim_ranges: QString)),

    set_output_size: qt_method!(fn(&self, width: usize, height: usize)),

    load_default_preset: qt_method!(fn(&mut self)),

    chart_data_changed: qt_signal!(),
    zooming_data_changed: qt_signal!(),
    keyframes_changed: qt_signal!(),

    cancel_current_operation: qt_method!(fn(&mut self)),

    neuflow_available: qt_property!(bool; READ get_neuflow_available NOTIFY neuflow_available_changed),
    neuflow_available_changed: qt_signal!(),

    sync_in_progress: qt_property!(bool; NOTIFY sync_in_progress_changed),
    sync_in_progress_changed: qt_signal!(),

    calib_in_progress: qt_property!(bool; NOTIFY calib_in_progress_changed),
    calib_in_progress_changed: qt_signal!(),
    calib_progress: qt_signal!(progress: f64, rms: f64, ready: usize, total: usize, good: usize, sharpness: f64),

    loading_gyro_in_progress: qt_property!(bool; NOTIFY loading_gyro_in_progress_changed),
    loading_gyro_in_progress_changed: qt_signal!(),
    loading_gyro_progress: qt_signal!(progress: f64),

    calib_model: qt_property!(RefCell<SimpleListModel<CalibrationItem>>; NOTIFY calib_model_updated),
    calib_model_updated: qt_signal!(),

    add_calibration_point: qt_method!(fn(&mut self, timestamp_us: i64, no_marker: bool)),
    remove_calibration_point: qt_method!(fn(&mut self, timestamp_us: i64)),

    quats_at_timestamp: qt_method!(fn(&self, timestamp_us: i64) -> QVariantList),
    mesh_at_frame: qt_method!(fn(&self, frame: usize) -> QVariantList),
    get_scaling_ratio: qt_method!(fn(&self) -> f64),
    get_min_fov: qt_method!(fn(&self) -> f64),

    init_calibrator: qt_method!(fn(&mut self)),

    get_urls_from_gyroflow_file: qt_method!(fn(&mut self, url: QUrl) -> QStringList),
    get_version_from_gyroflow_file: qt_method!(fn(&mut self, url: QUrl) -> u32),
    import_gyroflow_file: qt_method!(fn(&mut self, url: QUrl)),
    import_gyroflow_data: qt_method!(fn(&mut self, data: QString)),
    gyroflow_file_loaded: qt_signal!(obj: QJsonObject),
    export_gyroflow_file:
        qt_method!(fn(&self, url: QUrl, typ: QString, additional_data: QJsonObject)),
    export_gyroflow_data:
        qt_method!(fn(&self, typ: QString, additional_data: QJsonObject) -> QString),

    input_file_url: qt_property!(QString; READ get_input_file_url NOTIFY input_file_url_changed),
    input_file_url_changed: qt_signal!(),

    project_file_url: qt_property!(QString; READ get_project_file_url NOTIFY project_file_url_changed),
    project_file_url_changed: qt_signal!(),

    check_updates: qt_method!(fn(&self)),
    fetch_available_versions: qt_method!(fn(&self) -> QString),
    updates_available: qt_signal!(version: QString, changelog: QString, download_url: QString),
    start_app_update: qt_method!(fn(&self)),
    start_app_update_version: qt_method!(fn(&self, version: QString)),
    open_downloaded_update_and_quit: qt_method!(fn(&self)),
    app_update_progress: qt_signal!(downloaded: f64, total: f64, message: QString),
    app_update_ready: qt_signal!(path: QString, platform: QString, message: QString),
    app_update_error: qt_signal!(message: QString),
    app_update_handoff_started: qt_signal!(),
    app_update_state: Arc<Mutex<Option<crate::distribution::PreparedAppUpdate>>>,
    sync_device_time: qt_method!(fn(&mut self, tz_offset_minutes: i32)),
    check_firmware_update: qt_method!(fn(&mut self)),
    start_firmware_update: qt_method!(fn(&mut self)),
    poll_device_events: qt_method!(fn(&mut self)),
    device_state_changed: qt_signal!(),
    device_time_sync_finished: qt_signal!(success: bool, message: QString),
    device_connected: qt_property!(bool; NOTIFY device_state_changed),
    device_connection_status: qt_property!(QString; NOTIFY device_state_changed),
    device_connection_message: qt_property!(QString; NOTIFY device_state_changed),
    device_name: qt_property!(QString; NOTIFY device_state_changed),
    device_soft_version: qt_property!(QString; NOTIFY device_state_changed),
    device_hard_version: qt_property!(QString; NOTIFY device_state_changed),
    device_time: qt_property!(QString; NOTIFY device_state_changed),
    device_time_sync_in_progress: qt_property!(bool; NOTIFY device_state_changed),
    ota_progress: qt_property!(f64; NOTIFY device_state_changed),
    ota_state: qt_property!(QString; NOTIFY device_state_changed),
    ota_error: qt_property!(QString; NOTIFY device_state_changed),
    firmware_update_available: qt_property!(bool; NOTIFY device_state_changed),
    firmware_latest_version: qt_property!(QString; NOTIFY device_state_changed),
    firmware_changelog: qt_property!(QString; NOTIFY device_state_changed),
    rate_profile:
        qt_method!(fn(&self, name: QString, json: QString, checksum: QString, is_good: bool)),
    request_profile_ratings: qt_method!(fn(&self)),

    set_preview_pipeline: qt_method!(fn(&self, index: i32)),
    set_gpu_decoding: qt_method!(fn(&self, enabled: bool)),

    list_gpu_devices: qt_method!(fn(&self)),
    set_device: qt_method!(fn(&self, i: i32)),
    set_rendering_gpu_type_from_name: qt_method!(fn(&self, name: String)),
    gpu_list_loaded: qt_signal!(list: QJsonArray),

    set_digital_lens_name: qt_method!(fn(&self, name: String)),
    set_digital_lens_param: qt_method!(fn(&self, index: usize, value: f64)),

    get_username: qt_method!(fn(&self) -> QString),
    copy_to_clipboard: qt_method!(fn(&self, text: QString)),

    image_to_b64: qt_method!(fn(&self, img: QImage) -> QString),
    export_preset: qt_method!(
        fn(
            &self,
            url: QUrl,
            data: QJsonObject,
            save_type: QString,
            preset_name: QString,
        ) -> QString
    ),
    export_full_metadata: qt_method!(fn(&self, url: QUrl, gyro_url: QUrl)),
    export_parsed_metadata: qt_method!(fn(&self, url: QUrl)),
    export_gyro_data: qt_method!(fn(&self, url: QUrl, data: QJsonObject)),

    message: qt_signal!(text: QString, arg: QString, callback: QString, id: QString),
    error: qt_signal!(text: QString, arg: QString, callback: QString),

    request_location: qt_signal!(url: QString, typ: QString),

    set_keyframe: qt_method!(fn(&self, typ: String, timestamp_us: i64, value: f64)),
    set_keyframe_easing: qt_method!(fn(&self, typ: String, timestamp_us: i64, easing: String)),
    keyframe_easing: qt_method!(fn(&self, typ: String, timestamp_us: i64) -> String),
    set_keyframe_timestamp: qt_method!(fn(&self, typ: String, id: u32, timestamp_us: i64)),
    keyframe_id: qt_method!(fn(&self, typ: String, timestamp_us: i64) -> u32),
    remove_keyframe: qt_method!(fn(&self, typ: String, timestamp_us: i64)),
    clear_keyframes_type: qt_method!(fn(&self, typ: String)),
    keyframe_value_at_video_timestamp:
        qt_method!(fn(&self, typ: String, timestamp_ms: f64) -> QJSValue),
    is_keyframed: qt_method!(fn(&self, typ: String) -> bool),
    set_prevent_recompute: qt_method!(fn(&self, v: bool)),

    keyframe_value_updated: qt_signal!(keyframe: String, value: f64),
    update_keyframe_values: qt_method!(fn(&self, timestamp_ms: f64)),

    check_external_sdk: qt_method!(fn(&self, filename: QString) -> bool),
    install_external_sdk: qt_method!(fn(&self, url: QString)),
    external_sdk_progress: qt_signal!(percent: f64, sdk_name: QString, error_string: QString, url: QString),

    mp4_merge: qt_method!(
        fn(&self, file_list: QStringList, output_folder: QUrl, output_filename: QString)
    ),
    mp4_merge_progress: qt_signal!(percent: f64, error_string: QString, url: QString),

    is_nle_installed: qt_method!(fn(&self) -> bool),
    nle_plugins: qt_method!(fn(&self, command: QString, typ: QString) -> QString),
    nle_plugins_result: qt_signal!(command: QString, result: QString),

    has_per_frame_lens_data: qt_method!(fn(&self) -> bool),
    export_stmap: qt_method!(fn(&self, folder_url: QUrl, per_frame: bool)),
    stmap_progress: qt_signal!(progress: f64, ready: usize, total: usize),

    // ---------- REDline conversion ----------
    find_redline: qt_method!(fn(&self) -> QString),
    // ---------- REDline conversion ----------
    play_sound: qt_method!(fn(&mut self, typ: String)),
    data_folder: qt_method!(fn(&self) -> QUrl),

    image_sequence_start: qt_property!(i32),
    image_sequence_fps: qt_property!(f64),

    preview_resolution: i32,
    processing_resolution: i32,

    current_fov: qt_property!(f64; NOTIFY processing_info_changed),
    current_minimal_fov: qt_property!(f64; NOTIFY processing_info_changed),
    current_focal_length: qt_property!(f64; NOTIFY processing_info_changed),
    processing_info: qt_property!(QString; NOTIFY processing_info_changed),
    processing_info_changed: qt_signal!(),

    // Feedback system (Phase 4)
    #[allow(non_snake_case)]
    estimateFeedbackSize: qt_method!(fn(&self, options_json: QString) -> i64),
    #[allow(non_snake_case)]
    submitFeedback: qt_method!(fn(&mut self, description: QString, email: QString, options_json: QString)),
    #[allow(non_snake_case)]
    scanCrashCheckpoints: qt_method!(fn(&mut self)),
    #[allow(non_snake_case)]
    feedbackProgress: qt_signal!(stage: QString, pct: i32),
    #[allow(non_snake_case)]
    feedbackCompleted: qt_signal!(success: bool, id: QString, error: QString),
    #[allow(non_snake_case)]
    crashCheckpointFound: qt_signal!(count: i32),

    cancel_flag: Arc<AtomicBool>,
    preview_pipeline: Arc<AtomicUsize>,

    ongoing_computations: BTreeSet<u64>,

    device_manager: Option<DeviceManager>,
    device_command_tx: Option<Sender<DeviceCommand>>,
    device_event_rx: Option<Arc<Mutex<Receiver<DeviceEvent>>>>,

    pub stabilizer: Arc<StabilizationManager>,
}

fn video_log_scheme(url: &str) -> &'static str {
    if url.is_empty() {
        "empty"
    } else if url.starts_with("content://") {
        "content"
    } else if url.starts_with("file://") {
        "file"
    } else if url.contains("://") {
        "url"
    } else {
        "path"
    }
}

fn video_log_decoder_label(custom_decoder: &str) -> &'static str {
    if custom_decoder.is_empty() {
        "default"
    } else if custom_decoder.starts_with("FFmpeg:") {
        "FFmpeg"
    } else if custom_decoder.starts_with("BRAW:") {
        "BRAW"
    } else if custom_decoder.starts_with("R3D:") {
        "R3D"
    } else {
        "custom"
    }
}

fn enter_video_load_log_context(filename: &str) -> log_context::CtxScope {
    log_context::LogContext::enter(
        log_context::LogContextUpdate::default()
            .op("video.load")
            .video_path(util::normalize_path_for_log(filename)),
    )
}

impl Controller {
    pub fn new() -> Self {
        let mut this = Self {
            preview_resolution: -1,
            processing_resolution: 720,
            ota_state: QString::from("none"),
            device_connection_status: QString::from(DeviceConnectionStatus::Idle.as_str()),
            ..Default::default()
        };
        let device_manager = DeviceManager::new();
        this.device_command_tx = Some(device_manager.command_sender());
        this.device_event_rx = Some(device_manager.event_receiver());
        this.device_manager = Some(device_manager);
        this
    }

    fn get_image_sequence_fps(&self, url: QUrl) -> f64 {
        let url = util::qurl_to_encoded(url);
        if let Ok(mut file) = filesystem::open_file(&url, false, false) {
            let filesize = file.size;
            let options = gyroflow_core::gyro_source::FileLoadOptions::default();
            let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            if let Ok(md) = gyroflow_core::gyro_source::GyroSource::parse_telemetry_file(
                file.get_file(),
                filesize,
                &url,
                &options,
                (0, 0),
                0.0,
                |_| {},
                cancel_flag,
            ) {
                if let Some(fps) = md.frame_rate {
                    return fps;
                }
            }
        }
        0.0
    }

    fn peek_container_rotation(&self, url: QUrl) -> i32 {
        let url = util::qurl_to_encoded(url);
        util::peek_container_rotation_from_url(&url)
    }

    fn load_video(&mut self, url: QUrl, player: QJSValue) {
        self.stabilizer.clear();
        *self.stabilizer.lens.write() = Default::default();
        self.lens_loaded = false;
        self.gyro_loaded = false;
        self.gyro_changed();
        let url = util::qurl_to_encoded(url.clone());
        let filename = filesystem::get_filename(&url);
        let url_scheme = video_log_scheme(&url);

        // Push log context for this load. RAII guard restores on scope exit.
        let _log_ctx = enter_video_load_log_context(&filename);
        ::log::info!(
            target: "video.load",
            "load_video request: filename={} scheme={} android_content={} preview_resolution={}",
            filename,
            url_scheme,
            cfg!(target_os = "android") && url_scheme == "content",
            self.preview_resolution,
        );
        #[cfg(target_os = "android")]
        if !matches!(url_scheme, "content" | "file") {
            ::log::warn!(
                target: "video.load",
                "load_video received an unusual Android URL scheme: scheme={} filename_empty={}",
                url_scheme,
                filename.is_empty(),
            );
        }
        if filename.is_empty() {
            ::log::warn!(
                target: "video.load",
                "load_video received URL without a resolved filename: scheme={}",
                url_scheme,
            );
        }

        // Load current (clean) state to the UI
        if self.stabilizer.lens_calibrator.read().is_none() {
            if let Ok(current_state) =
                self.stabilizer
                    .export_gyroflow_data(core::GyroflowProjectType::Simple, "{}", None)
            {
                if let Ok(current_state) = serde_json::from_str(current_state.as_str())
                    as serde_json::Result<serde_json::Value>
                {
                    self.gyroflow_file_loaded(util::serde_json_to_qt_object(&current_state));
                }
            }
        }

        self.chart_data_changed();
        self.keyframes_changed();
        self.update_offset_model();

        *self.stabilizer.input_file.write() = gyroflow_core::InputFile {
            url: url.clone(),
            project_file_url: None,
            image_sequence_start: self.image_sequence_start,
            image_sequence_fps: self.image_sequence_fps,
            preset_name: None,
            preset_output_size: None,
        };
        self.input_file_url_changed();
        self.project_file_url_changed();

        let mut custom_decoder = String::new(); // eg. BRAW:format=rgba64le
        if self.image_sequence_start > 0 {
            custom_decoder = format!(
                "FFmpeg:avformat_options=start_number={}",
                self.image_sequence_start
            );
        }

        let options = {
            let target_height = self.preview_resolution;
            if target_height > 0 {
                format!(":scale={}x{}", (target_height * 16) / 9, target_height)
            } else {
                "".to_owned()
            }
        };

        if filename.to_ascii_lowercase().ends_with("braw") {
            let gpu = if self.stabilizer.gpu_decoding.load(SeqCst) {
                "auto"
            } else {
                "no"
            }; // Disable GPU decoding for BRAW
            custom_decoder = format!("BRAW:gpu={}{}", gpu, options);
        }
        if filename.to_ascii_lowercase().ends_with("r3d")
            || filename.to_ascii_lowercase().ends_with("nev")
        {
            custom_decoder = format!("R3D:gpu=auto{}", options);
        }
        if !custom_decoder.is_empty() {
            ::log::debug!(target: "video.load", "Custom decoder: {custom_decoder}");
        }

        if let Some(vid) = player.to_qobject::<MDKVideoItem>() {
            let vid = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()
            filesystem::stop_accessing_url(&util::qurl_to_encoded(vid.url.clone()), false);
            filesystem::start_accessing_url(&url, false);
            ::log::info!(
                target: "video.load",
                "MDK setUrl: filename={} scheme={} decoder={}",
                filename,
                url_scheme,
                video_log_decoder_label(&custom_decoder),
            );
            vid.setUrl(
                QUrl::from(QString::from(url)),
                QString::from(custom_decoder),
            );
        }
    }

    fn log_video_file_dialog(
        &self,
        selected_count: i32,
        first_url: QUrl,
        selected_file: QUrl,
        current_folder: QUrl,
        selected_files_raw: QString,
    ) {
        let first_url = util::qurl_to_encoded(first_url);
        let selected_file = util::qurl_to_encoded(selected_file);
        let current_folder = util::qurl_to_encoded(current_folder);
        let selected_files_raw = selected_files_raw.to_string();
        let first_filename = filesystem::get_filename(&first_url);
        let selected_filename = filesystem::get_filename(&selected_file);
        let context_filename = if !first_filename.is_empty() {
            first_filename.as_str()
        } else {
            selected_filename.as_str()
        };
        let _log_ctx = enter_video_load_log_context(context_filename);
        ::log::info!(
            target: "video.load",
            "FileDialog accepted: selected_count={} first_scheme={} first_filename={} selected_file_scheme={} selected_file_filename={} current_folder_scheme={} selected_files_raw={}",
            selected_count,
            video_log_scheme(&first_url),
            first_filename,
            video_log_scheme(&selected_file),
            selected_filename,
            video_log_scheme(&current_folder),
            selected_files_raw,
        );
        if selected_count < 1 && selected_file.is_empty() {
            ::log::warn!(
                target: "video.load",
                "FileDialog accepted without selected files",
            );
        }
    }

    fn log_video_metadata_state(
        &self,
        width: i32,
        height: i32,
        duration_ms: f64,
        fps: f64,
        frame_count: i32,
    ) {
        let input_url = self.stabilizer.input_file.read().url.clone();
        let filename = filesystem::get_filename(&input_url);
        let _log_ctx = enter_video_load_log_context(&filename);
        ::log::info!(
            target: "video.load",
            "MDK metadata state: duration_ms={:.3} fps={:.6} frame_count={} width={} height={}",
            duration_ms,
            fps,
            frame_count,
            width,
            height,
        );
        if width <= 0 || height <= 0 || duration_ms <= 0.0 || fps <= 0.0 {
            ::log::warn!(
                target: "video.load",
                "MDK metadata state is invalid: duration_ms={:.3} fps={:.6} frame_count={} width={} height={}",
                duration_ms,
                fps,
                frame_count,
                width,
                height,
            );
        }
    }

    fn get_input_file_url(&self) -> QString {
        QString::from(self.stabilizer.input_file.read().url.clone())
    }
    fn get_project_file_url(&self) -> QString {
        QString::from(
            self.stabilizer
                .input_file
                .read()
                .project_file_url
                .as_ref()
                .cloned()
                .unwrap_or_default(),
        )
    }

    fn start_autosync(&mut self, timestamps_fract: String, sync_params: String, mode: String) {
        rendering::clear_log();
        // Reset the GPU-decode codec blocklist at every Auto-sync entry: a new
        // press is the user's signal to "try again", giving GPU a fresh chance
        // even after prior failures in this session.
        rendering::gpu_codec_blocklist::clear();

        let sync_params =
            serde_json::from_str(&sync_params) as serde_json::Result<synchronization::SyncParams>;
        if let Err(e) = sync_params {
            self.sync_in_progress = false;
            self.sync_in_progress_changed();
            return self.error(
                QString::from("An error occured: %1"),
                QString::from(format!("JSON parse error: {}", e)),
                QString::default(),
            );
        }
        let mut sync_params = sync_params.unwrap();

        sync_params.initial_offset *= 1000.0; // s to ms
        sync_params.time_per_syncpoint *= 1000.0; // s to ms
        sync_params.search_size *= 1000.0; // s to ms
        sync_params.every_nth_frame = sync_params.every_nth_frame.max(1);

        let for_rs = mode == "estimate_rolling_shutter";

        let every_nth_frame = sync_params.every_nth_frame;

        self.sync_in_progress = true;
        self.sync_in_progress_changed();

        let timestamps_fract: Vec<f64> = timestamps_fract
            .split(';')
            .filter_map(|x| x.parse::<f64>().ok())
            .collect();

        let progress = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, (percent, ready, total): (f64, usize, usize)| {
                this.sync_in_progress = ready < total || percent < 1.0;
                this.sync_in_progress_changed();
                this.chart_data_changed();
                this.sync_progress(percent, ready, total);
            },
        );
        let set_offsets = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, offsets: Vec<(f64, f64, f64, f64)>| {
                if for_rs {
                    if let Some(offs) = offsets.first() {
                        this.rolling_shutter_estimated(offs.1);
                    }
                } else {
                    let mut gyro = this.stabilizer.gyro.write();
                    gyro.prevent_recompute = true;
                    for x in offsets {
                        ::log::info!(
                            "Setting offset at {:.4}: {:.4} (cost {:.4}, conf {:.3})",
                            x.0,
                            x.1,
                            x.2,
                            x.3
                        );
                        let new_ts = ((x.0 - x.1) * 1000.0) as i64;
                        let confidence = x.3;
                        if confidence < 0.4 {
                            // Drop low-confidence sync points unconditionally
                            // (see render_queue.rs comment).
                            ::log::info!(
                                "Dropping sync point at {:.4}: conf {:.3} < 0.4",
                                x.0,
                                confidence
                            );
                            continue;
                        }
                        // Remove existing offsets within 100ms range
                        gyro.remove_offsets_near(new_ts, 100.0);
                        gyro.set_offset(new_ts, x.1);
                    }
                    gyro.prevent_recompute = false;
                    gyro.adjust_offsets();
                    this.stabilizer.keyframes.write().update_gyro(&gyro);
                    this.stabilizer.invalidate_zooming();
                }
                this.update_offset_model();
                this.request_recompute();
            },
        );
        let set_orientation = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, orientation: String| {
                ::log::info!("Setting orientation {}", &orientation);
                this.orientation_guessed(QString::from(orientation));
            },
        );
        let err = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, (msg, mut arg): (String, String)| {
                arg.push_str("\n\n");
                arg.push_str(&rendering::get_log());

                this.error(QString::from(msg), QString::from(arg), QString::default());

                this.sync_in_progress = false;
                this.sync_in_progress_changed();
                this.update_offset_model();
                this.request_recompute();
            },
        );
        self.sync_progress(0.0, 0, 0);

        self.cancel_flag.store(false, SeqCst);

        let sync_failure_detail = synchronization::describe_autosync_init_failure(
            &self.stabilizer,
            &timestamps_fract,
            &sync_params,
        );

        if let Ok(mut sync) = AutosyncProcess::from_manager(
            &self.stabilizer,
            &timestamps_fract,
            sync_params,
            mode.clone(),
            self.cancel_flag.clone(),
        ) {
            sync.on_progress(move |percent, ready, total| {
                progress((percent, ready, total));
            });
            sync.on_finished(move |arg| {
                match arg {
                    Either::Left(offsets) => set_offsets(offsets),
                    Either::Right(Some(orientation)) => set_orientation(orientation.0),
                    _ => (),
                };
            });

            let ranges = sync.get_ranges();
            let cancel_flag = self.cancel_flag.clone();

            let input_file = self.stabilizer.input_file.read().clone();
            let proc_height = self.processing_resolution;
            let gpu_decoding = self.stabilizer.gpu_decoding.load(SeqCst);
            core::run_threaded(move || {
                // Probe codec signature so we can consult the GPU blocklist
                // before attempting decode and record on failure. Probe only
                // when GPU is even a candidate; if the user has GPU disabled,
                // we go straight to software without paying probe cost.
                let codec_sig = if gpu_decoding {
                    match VideoProcessor::get_video_info(&input_file.url) {
                        Ok(info) => Some(rendering::gpu_codec_blocklist::CodecSignature::from(&info)),
                        Err(e) => {
                            ::log::debug!(
                                "[autosync] codec signature probe failed: {e:?} (proceeding without blocklist consultation)"
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                // Wrap sync in Rc before try_run so the closure captures the Rc
                // (cheap to clone per attempt) instead of the underlying value.
                let sync = std::rc::Rc::new(sync);

                let try_run = |use_gpu: bool, ranges: Vec<(f64, f64)>| -> Result<(), rendering::FFmpegError> {
                    let mut frame_no = 0;
                    let mut abs_frame_no = 0;

                    let mut decoder_options = ffmpeg_next::Dictionary::new();
                    if input_file.image_sequence_fps > 0.0 {
                        let fps = rendering::fps_to_rational(input_file.image_sequence_fps);
                        decoder_options.set(
                            "framerate",
                            &format!("{}/{}", fps.numerator(), fps.denominator()),
                        );
                    }
                    if input_file.image_sequence_start > 0 {
                        decoder_options.set(
                            "start_number",
                            &format!("{}", input_file.image_sequence_start),
                        );
                    }
                    if proc_height > 0 {
                        decoder_options.set(
                            "scale",
                            &format!("{}x{}", (proc_height * 16) / 9, proc_height),
                        );
                    }
                    ::log::debug!("Decoder options: {:?}", decoder_options);

                    let mut proc = VideoProcessor::from_file(
                        &input_file.url,
                        use_gpu,
                        0,
                        Some(decoder_options),
                    )?;

                    let err2 = err.clone();
                    let sync2 = sync.clone();
                    proc.on_frame(
                        move |timestamp_us,
                              input_frame,
                              _output_frame,
                              converter,
                              _rate_control| {
                            assert!(_output_frame.is_none());

                            if abs_frame_no % every_nth_frame == 0 {
                                let h = if proc_height > 0 {
                                    proc_height as u32
                                } else {
                                    input_frame.height()
                                };
                                let ratio = input_frame.height() as f64 / h as f64;
                                let sw = (input_frame.width() as f64 / ratio).round() as u32;
                                let sh = (input_frame.height() as f64
                                    / (input_frame.width() as f64 / sw as f64))
                                    .round()
                                    as u32;
                                // NeuFlow (of_method=3 or 4) needs NV12 for color data;
                                // other methods use GRAY8.
                                let pix_fmt = if sync2.sync_params.of_method == 3 || sync2.sync_params.of_method == 4 {
                                    ffmpeg_next::format::Pixel::NV12
                                } else {
                                    ffmpeg_next::format::Pixel::GRAY8
                                };
                                match converter.scale(
                                    input_frame,
                                    pix_fmt,
                                    sw,
                                    sh,
                                ) {
                                    Ok(small_frame) => {
                                        let (width, height, stride, pixels) = if sync2.sync_params.of_method == 3 || sync2.sync_params.of_method == 4 {
                                            // NV12: pass all planes (Y + UV)
                                            let total_len = small_frame.stride(0) * small_frame.plane_height(0) as usize
                                                          + small_frame.stride(1) * small_frame.plane_height(1) as usize;
                                            let all_data = {
                                                let _g = gyroflow_core::synchronization::sync_perf::StageGuard::new(
                                                    gyroflow_core::synchronization::sync_perf::Stage::DecodeNv12Concat,
                                                );
                                                let mut buf = Vec::with_capacity(total_len);
                                                buf.extend_from_slice(&small_frame.data(0)[..small_frame.stride(0) * small_frame.plane_height(0) as usize]);
                                                buf.extend_from_slice(&small_frame.data(1)[..small_frame.stride(1) * small_frame.plane_height(1) as usize]);
                                                buf
                                            };
                                            (
                                                small_frame.plane_width(0),
                                                small_frame.plane_height(0),
                                                small_frame.stride(0),
                                                all_data,
                                            )
                                        } else {
                                            (
                                                small_frame.plane_width(0),
                                                small_frame.plane_height(0),
                                                small_frame.stride(0),
                                                small_frame.data(0).to_vec(),
                                            )
                                        };

                                        sync2.feed_frame(
                                            timestamp_us,
                                            frame_no,
                                            width,
                                            height,
                                            stride,
                                            &pixels,
                                        );
                                    }
                                    Err(e) => err2((
                                        "An error occured: %1".to_string(),
                                        e.to_string(),
                                    )),
                                }
                                frame_no += 1;
                            }
                            abs_frame_no += 1;
                            Ok(())
                        },
                    );
                    proc.start_decoder_only(ranges, cancel_flag.clone())
                };

                // Decide whether to attempt GPU. Blocklist is advisory only when
                // the user setting allows GPU; if GPU is off we skip the check.
                let try_gpu = match (gpu_decoding, codec_sig.as_ref()) {
                    (true, Some(sig)) => {
                        if rendering::gpu_codec_blocklist::is_blocklisted(sig) {
                            ::log::info!(
                                "[autosync] skipping GPU for blocklisted signature {:?}",
                                sig
                            );
                            false
                        } else {
                            true
                        }
                    }
                    (true, None) => true,
                    (false, _) => false,
                };

                let result = if try_gpu {
                    match try_run(true, ranges.clone()) {
                        Err(rendering::FFmpegError::GPUDecodingFailed) => {
                            if let Some(sig) = codec_sig.clone() {
                                ::log::info!(
                                    "[autosync] GPU decode failed for signature {:?}, retrying with software",
                                    sig
                                );
                                rendering::gpu_codec_blocklist::record_failure(sig);
                            } else {
                                ::log::info!(
                                    "[autosync] GPU decode failed (no signature available), retrying with software"
                                );
                            }
                            try_run(false, ranges)
                        }
                        other => other,
                    }
                } else {
                    try_run(false, ranges)
                };

                if let Err(e) = result {
                    err(("An error occured: %1".to_string(), e.to_string()));
                }
                sync.finished_feeding_frames();
            });
        } else {
            let detail = format!("Invalid autosync parameters ({mode}): {sync_failure_detail}");
            ::log::warn!("[autosync] start_autosync rejected: {detail}");
            err(("An error occured: %1".to_string(), detail));
        }
    }

    fn estimate_bias(&mut self, timestamps_fract: QString) {
        let timestamps_fract: Vec<f64> = timestamps_fract
            .to_string()
            .split(';')
            .filter_map(|x| x.parse::<f64>().ok())
            .collect();

        let org_duration_ms = self.stabilizer.params.read().duration_ms;

        // sample 400 ms
        let ranges_ms: Vec<(f64, f64)> = timestamps_fract
            .iter()
            .map(|x| {
                let range = (
                    ((x * org_duration_ms) - (200.0)).max(0.0),
                    ((x * org_duration_ms) + (200.0)).min(org_duration_ms),
                );
                (range.0, range.1)
            })
            .collect();

        if !ranges_ms.is_empty() {
            let bias = self
                .stabilizer
                .gyro
                .read()
                .find_bias(ranges_ms[0].0, ranges_ms[0].1);
            self.bias_estimated(bias.0, bias.1, bias.2);
        }
    }

    fn get_optimal_sync_points(
        &mut self,
        target_sync_points: usize,
        initial_offset: f64,
    ) -> QString {
        QString::from(
            self.stabilizer
                .get_optimal_sync_points(target_sync_points, initial_offset * 1000.0)
                .into_iter()
                .map(|x| x.to_string())
                .join(";"),
        )
    }

    fn update_chart(&mut self, chart: QJSValue, series: String) -> bool {
        // Only update the chart if we're finished recomputing
        if !self.ongoing_computations.is_empty() {
            return false;
        }
        if let Some(chart) = chart.to_qobject::<TimelineGyroChart>() {
            let chart = unsafe { &mut *chart.as_ptr() }; // _self.borrow_mut();

            if self.stabilizer.pose_estimator.estimated_gyro.is_locked()
                || self.stabilizer.pose_estimator.estimated_quats.is_locked()
                || self.stabilizer.gyro.is_locked()
                || self.stabilizer.params.is_locked()
            {
                ::log::debug!("Chart mutex locked, retrying");
                return false;
            }

            if series.is_empty() {
                if let Some(est_gyro) = self.stabilizer.pose_estimator.estimated_gyro.try_read() {
                    chart.setSyncResults(&est_gyro);
                    if let Some(est_quats) =
                        self.stabilizer.pose_estimator.estimated_quats.try_read()
                    {
                        chart.setSyncResultsQuats(&est_quats);
                    }
                }
            }

            if let Some(gyro) = self.stabilizer.gyro.try_read() {
                if let Some(params) = self.stabilizer.params.try_read() {
                    if let Some(keyframes) = self.stabilizer.keyframes.try_read() {
                        chart.setFromGyroSource(&gyro, &params, &keyframes, &series);
                        return true;
                    }
                }
            }
        }
        false
    }

    fn update_frequency_graph(
        &mut self,
        graph: QJSValue,
        idx: usize,
        ts: f64,
        sr: f64,
        fft_size: usize,
    ) {
        if let Some(graph) = graph.to_qobject::<FrequencyGraph>() {
            let graph = unsafe { &mut *graph.as_ptr() }; // _self.borrow_mut();

            let gyro = &self.stabilizer.gyro.read();
            let file_metadata = gyro.file_metadata.read();
            let raw_imu = gyro.raw_imu(&file_metadata);

            if !raw_imu.is_empty() {
                let dt_ms = 1000.0 / sr;
                let center_ts = ts - gyro.offset_at_video_timestamp(ts);
                let last_ts = center_ts + dt_ms * (fft_size as f64) / 2.0;
                let mut sample_ts =
                    last_ts.min(raw_imu.last().unwrap().timestamp_ms) - (fft_size as f64) * dt_ms;
                sample_ts = sample_ts.max(0.0);

                let mut prev_ts = 0.0;
                let mut prev_val = 0.0;

                let mut samples: Vec<f64> = Vec::with_capacity(fft_size);
                for x in raw_imu {
                    let mut val = 0.0;
                    if idx < 3 {
                        if let Some(g) = x.gyro.as_ref() {
                            val = g[idx % 3];
                        }
                    } else {
                        if let Some(g) = x.accl.as_ref() {
                            val = g[idx % 3];
                        }
                    }

                    while x.timestamp_ms > sample_ts && samples.len() < fft_size {
                        let frac = (sample_ts - prev_ts) / (x.timestamp_ms - prev_ts);
                        let interpolated = prev_val + (val - prev_val) * frac.clamp(0.0, 1.0);
                        samples.push(interpolated /*+ samples.last().unwrap_or(&0.0)*/);
                        sample_ts += dt_ms;
                    }

                    if samples.len() >= fft_size {
                        break;
                    }

                    prev_ts = x.timestamp_ms;
                    prev_val = val;
                }

                if samples.len() == fft_size {
                    graph.setData(&samples, sr);
                } else {
                    graph.setData(&[], 0.0);
                }
            }
        }
    }

    fn update_keyframes_view(&mut self, view: QJSValue) {
        if let Some(view) = view.to_qobject::<TimelineKeyframesView>() {
            let view = unsafe { &mut *view.as_ptr() }; // _self.borrow_mut();

            view.setKeyframes(&self.stabilizer.keyframes.read());
        }
    }

    fn update_offset_model(&mut self) {
        self.offsets_model = RefCell::new(
            self.stabilizer
                .gyro
                .read()
                .get_offsets_plus_linear()
                .iter()
                .map(|(k, v)| OffsetItem {
                    timestamp_us: *k,
                    offset_ms: v.0,
                    linear_offset_ms: v.1,
                })
                .collect(),
        );

        util::qt_queued_callback(QPointer::from(self as &Self), |this, _| {
            this.offsets_updated();
            this.chart_data_changed();
        })(());
    }

    fn video_file_loaded(&mut self, player: QJSValue) {
        let stab = self.stabilizer.clone();
        let input_url = self.stabilizer.input_file.read().url.clone();
        let filename = filesystem::get_filename(&input_url);
        let _log_ctx = enter_video_load_log_context(&filename);

        if let Some(vid) = player.to_qobject::<MDKVideoItem>() {
            let vid = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()
            let mut duration_ms = vid.duration;
            let mut fps = vid.frameRate;
            let frame_count = vid.frameCount as usize;
            let video_size = (vid.videoWidth as usize, vid.videoHeight as usize);

            // For image sequences, MDK may report wrong fps/duration (defaults to 25fps).
            // Use the fps from telemetry or user input instead.
            if self.image_sequence_fps > 0.0 && frame_count > 0 {
                fps = self.image_sequence_fps;
                duration_ms = frame_count as f64 * 1000.0 / fps;
            }

            ::log::info!(
                target: "video.load",
                "video_file_loaded: duration_ms={:.3} fps={:.6} frame_count={} width={} height={}",
                duration_ms,
                fps,
                frame_count,
                video_size.0,
                video_size.1,
            );
            if video_size.0 == 0 || video_size.1 == 0 || duration_ms <= 0.0 || fps <= 0.0 {
                ::log::warn!(
                    target: "video.load",
                    "video_file_loaded has invalid media properties: duration_ms={:.3} fps={:.6} frame_count={} width={} height={}",
                    duration_ms,
                    fps,
                    frame_count,
                    video_size.0,
                    video_size.1,
                );
            }

            self.set_preview_resolution(self.preview_resolution, player);

            if duration_ms > 0.0 && fps > 0.0 {
                stab.init_from_video_data(duration_ms, fps, frame_count, video_size);
                stab.set_output_size(video_size.0, video_size.1);
            }
        }
    }

    fn load_telemetry(
        &mut self,
        url: QUrl,
        is_main_video: bool,
        player: QJSValue,
        sample_index: i32,
        project_version: u32,
    ) {
        let url = util::qurl_to_encoded(url);
        let stab = self.stabilizer.clone();
        let filename = filesystem::get_filename(&url);
        let url_scheme = video_log_scheme(&url);
        let _log_ctx = enter_video_load_log_context(&filename);
        let mut load_options = gyroflow_core::gyro_source::FileLoadOptions::default();
        load_options.project_version = project_version as _;

        if let Some(vid) = player.to_qobject::<MDKVideoItem>() {
            let vid = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()
            let mut duration_ms = vid.duration;
            let mut fps = vid.frameRate;
            let frame_count = vid.frameCount as usize;
            let video_size = (vid.videoWidth as usize, vid.videoHeight as usize);
            self.cancel_flag.store(false, SeqCst);
            let cancel_flag = self.cancel_flag.clone();

            // For image sequences, MDK may report wrong fps/duration (defaults to 25fps).
            if self.image_sequence_fps > 0.0 && frame_count > 0 {
                fps = self.image_sequence_fps;
                duration_ms = frame_count as f64 * 1000.0 / fps;
            }

            ::log::info!(
                target: "video.load",
                "load_telemetry start: filename={} scheme={} main_video={} sample_index={} project_version={} duration_ms={:.3} fps={:.6} frame_count={} width={} height={}",
                filename,
                url_scheme,
                is_main_video,
                sample_index,
                project_version,
                duration_ms,
                fps,
                frame_count,
                video_size.0,
                video_size.1,
            );
            if duration_ms <= 0.0 || fps <= 0.0 {
                ::log::warn!(
                    target: "video.load",
                    "load_telemetry skipped because media timing is invalid: filename={} scheme={} duration_ms={:.3} fps={:.6}",
                    filename,
                    url_scheme,
                    duration_ms,
                    fps,
                );
            }

            if is_main_video {
                self.set_preview_resolution(self.preview_resolution, player);
            }

            let err = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                |this, (msg, arg): (String, String)| {
                    this.error(QString::from(msg), QString::from(arg), QString::default());
                },
            );

            let progress = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, progress: f64| {
                    this.loading_gyro_in_progress = progress < 1.0;
                    this.loading_gyro_progress(progress);
                    this.loading_gyro_in_progress_changed();
                },
            );
            let stab2 = stab.clone();
            let finished = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, params: (bool, QString, QString, bool, serde_json::Value)| {
                    this.gyro_loaded = params.3; // Contains motion
                    this.gyro_changed();

                    this.loading_gyro_in_progress = false;
                    this.loading_gyro_progress(1.0);
                    this.loading_gyro_in_progress_changed();

                    this.update_offset_model();
                    this.chart_data_changed();

                    this.telemetry_loaded(
                        params.0,
                        params.1,
                        params.2,
                        util::serde_json_to_qt_object(&params.4),
                    );

                    stab2.invalidate_ongoing_computations();
                    stab2.invalidate_smoothing();
                    this.request_recompute();
                },
            );
            let load_lens = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, path: String| {
                    this.load_lens_profile(path.into());
                },
            );
            let reload_lens =
                util::qt_queued_callback_mut(QPointer::from(self as &Self), move |this, _| {
                    let lens = this.stabilizer.lens.read();
                    if this.lens_loaded
                        || !lens.path_to_file.is_empty()
                        || !lens.fisheye_params.camera_matrix.is_empty()
                        || !lens.camera_brand.is_empty()
                    {
                        this.lens_loaded = true;
                        this.lens_changed();
                        let json = lens.get_json().unwrap_or_default();
                        this.lens_profile_loaded(
                            QString::from(json),
                            QString::from(lens.path_to_file.as_str()),
                            QString::from(lens.checksum.clone().unwrap_or_default()),
                        );
                    }
                });

            if duration_ms > 0.0 && fps > 0.0 {
                if is_main_video {
                    stab.init_from_video_data(duration_ms, fps, frame_count, video_size);
                    stab.set_output_size(video_size.0, video_size.1);
                }

                self.loading_gyro_in_progress = true;
                self.loading_gyro_in_progress_changed();
                core::run_threaded(move || {
                    let mut additional_data = serde_json::Value::Object(serde_json::Map::new());
                    let additional_obj = additional_data.as_object_mut().unwrap();

                    {
                        if let Ok(mut file) = filesystem::open_file(&url, false, false) {
                            let filesize = file.size;
                            if is_main_video {
                                // Ignore the error here, video file may not contain the telemetry and it's ok
                                let _ = stab.load_gyro_data(
                                    file.get_file(),
                                    filesize,
                                    &url,
                                    is_main_video,
                                    &load_options,
                                    progress,
                                    cancel_flag,
                                );
                                stab.recompute_undistortion();

                                // Display anchor: only set for Canon Cinema RAW Proxy
                                // companion files (filename `*_Proxy.MP4`/`.mov` AND
                                // detected_source starts with "Canon"). Container-layer
                                // metadata cannot distinguish R52-style 1-frame Proxy
                                // offsets from regular Canon H.264/HEVC encodings (both
                                // carry elst.media_time = 1 frame for B-frame reorder),
                                // so we hard-code the rule on filename + brand. 1 frame
                                // duration is derived from fps so 29.97/59.94/etc all
                                // resolve correctly.
                                let url_lower = url.to_ascii_lowercase();
                                let is_canon_proxy_name = url_lower.ends_with("_proxy.mp4")
                                    || url_lower.ends_with("_proxy.mov");
                                let is_canon = stab
                                    .gyro
                                    .read()
                                    .file_metadata
                                    .read()
                                    .detected_source
                                    .as_deref()
                                    .is_some_and(|s| s.starts_with("Canon"));
                                let anchor_us = if is_canon_proxy_name && is_canon && fps > 0.0 {
                                    Some((1_000_000.0 / fps).round() as i64)
                                } else {
                                    None
                                };
                                stab.params.write().video_display_anchor_us = anchor_us;
                                ::log::info!(
                                    target: "video.load",
                                    "video_display_anchor_us = {:?} (is_canon_proxy_name={} is_canon={} fps={:.6})",
                                    anchor_us,
                                    is_canon_proxy_name,
                                    is_canon,
                                    fps
                                );
                            } else {
                                if sample_index > -1 {
                                    load_options.sample_index = Some(sample_index as usize);
                                }

                                if let Err(e) = stab.load_gyro_data(
                                    file.get_file(),
                                    filesize,
                                    &url,
                                    is_main_video,
                                    &load_options,
                                    progress,
                                    cancel_flag,
                                ) {
                                    err(("An error occured: %1".to_string(), e.to_string()));
                                }
                            }
                        }
                    }

                    stab.recompute_smoothness();

                    let gyro = stab.gyro.read();
                    let file_metadata = gyro.file_metadata.read();
                    let detected = file_metadata
                        .detected_source
                        .as_ref()
                        .map(String::clone)
                        .unwrap_or_default();
                    let has_raw_gyro = !file_metadata.raw_imu.is_empty();
                    let has_quats = !file_metadata.quaternions.is_empty();
                    let has_motion = has_raw_gyro || has_quats;
                    additional_obj.insert(
                        "imu_orientation".to_owned(),
                        serde_json::Value::String(
                            gyro.imu_transforms
                                .imu_orientation
                                .clone()
                                .unwrap_or_else(|| "XYZ".into()),
                        ),
                    );
                    additional_obj.insert(
                        "contains_raw_gyro".to_owned(),
                        serde_json::Value::Bool(has_raw_gyro),
                    );
                    additional_obj.insert(
                        "contains_quats".to_owned(),
                        serde_json::Value::Bool(has_quats),
                    );
                    additional_obj.insert(
                        "contains_motion".to_owned(),
                        serde_json::Value::Bool(has_motion),
                    );
                    additional_obj.insert(
                        "has_accurate_timestamps".to_owned(),
                        serde_json::Value::Bool(file_metadata.has_accurate_timestamps),
                    );
                    additional_obj.insert(
                        "sample_rate".to_owned(),
                        serde_json::to_value(
                            gyroflow_core::gyro_source::GyroSource::get_sample_rate(
                                &*file_metadata,
                            ),
                        )
                        .unwrap(),
                    );
                    let has_builtin_profile = file_metadata
                        .lens_profile
                        .as_ref()
                        .map(|y| y.is_object())
                        .unwrap_or_default();
                    let has_lens_params = !file_metadata.lens_params.is_empty();
                    let has_focal_length = file_metadata
                        .lens_params
                        .values()
                        .next()
                        .and_then(|p| p.focal_length)
                        .is_some();
                    let unit_px_fl = file_metadata.unit_pixel_focal_length;
                    additional_obj.insert(
                        "has_lens_params".to_owned(),
                        serde_json::Value::Bool(has_lens_params),
                    );
                    additional_obj.insert(
                        "has_builtin_profile".to_owned(),
                        serde_json::Value::Bool(has_builtin_profile),
                    );
                    additional_obj.insert(
                        "has_focal_length".to_owned(),
                        serde_json::Value::Bool(has_focal_length),
                    );
                    if let Some(upfl) = unit_px_fl {
                        additional_obj.insert(
                            "unit_pixel_focal_length".to_owned(),
                            serde_json::Number::from_f64(upfl).unwrap().into(),
                        );
                    }
                    // Pass telemetry creation time to UI and set video_created_at
                    if let Some(ref utc_str) = file_metadata.creation_date_utc {
                        additional_obj.insert(
                            "creation_date_utc".to_owned(),
                            serde_json::Value::String(utc_str.clone()),
                        );
                        if is_main_video {
                            if let Some(ms) = parse_creation_date_to_millis(utc_str) {
                                stab.params.write().video_created_at = Some(ms);
                            }
                        }
                    }
                    if let Some(ref tz) = file_metadata.timezone_offset {
                        additional_obj.insert(
                            "timezone_offset".to_owned(),
                            serde_json::Value::String(tz.clone()),
                        );
                        if is_main_video {
                            stab.params.write().video_timezone = Some(tz.clone());
                        }
                    }
                    if let Some(ref local) = file_metadata.creation_date {
                        additional_obj.insert(
                            "creation_date".to_owned(),
                            serde_json::Value::String(local.clone()),
                        );
                    }

                    let md_data = file_metadata.additional_data.clone();
                    if let Some(md_fps) = file_metadata.frame_rate {
                        additional_obj.insert(
                            "telemetry_fps".to_owned(),
                            serde_json::Number::from_f64(md_fps).unwrap().into(),
                        );
                    }
                    if let Some(rec_fps) = file_metadata.record_frame_rate {
                        let fps = stab.params.read().fps;
                        let ratio = rec_fps / fps;
                        if ratio > 1.2 || ratio < 1.0 / 1.2 {
                            additional_obj.insert(
                                "realtime_fps".to_owned(),
                                serde_json::Number::from_f64(rec_fps).unwrap().into(),
                            );
                        }
                    }
                    drop(file_metadata);
                    drop(gyro);

                    let camera_id = stab.camera_id.read();

                    let id_str = camera_id
                        .as_ref()
                        .map(|v| v.get_identifier_for_autoload())
                        .unwrap_or_default();
                    if is_main_video && !id_str.is_empty() && !has_builtin_profile {
                        let needs_load = {
                            let mut db = stab.lens_profile_db.write();
                            db.on_loaded(move |db| {
                                if db.contains_id(&id_str) {
                                    load_lens(id_str);
                                }
                            });
                            !db.loaded
                        };
                        if needs_load {
                            let db = stab.lens_profile_db.clone();
                            core::run_threaded(move || {
                                let mut new_db =
                                    core::lens_profile_database::LensProfileDatabase::default();
                                new_db.load_all();
                                db.write().set_from_db(new_db);
                            });
                        }
                    }
                    if is_main_video {
                        reload_lens(());
                    }

                    if let Some(cam_id) = camera_id.as_ref() {
                        additional_obj.insert(
                            "camera_identifier".to_owned(),
                            serde_json::to_value(cam_id).unwrap(),
                        );
                    }
                    drop(camera_id);

                    if md_data.is_object() {
                        gyroflow_core::util::merge_json(&mut additional_data, &md_data);
                    }

                    additional_data.as_object_mut().unwrap().insert(
                        "frame_readout_time".to_owned(),
                        serde_json::to_value(stab.params.read().frame_readout_time.abs()).unwrap(),
                    );
                    additional_data.as_object_mut().unwrap().insert(
                        "frame_readout_direction".to_owned(),
                        serde_json::to_value(stab.params.read().frame_readout_direction).unwrap(),
                    );

                    finished((
                        is_main_video,
                        filename.into(),
                        QString::from(detected.trim()),
                        has_motion,
                        additional_data,
                    ));
                });
            }
        }
    }
    fn load_lens_profile(&mut self, url_or_id: QString) {
        let (json, filepath, checksum) = {
            if let Err(e) = self.stabilizer.load_lens_profile(&url_or_id.to_string()) {
                self.error(
                    QString::from("An error occured: %1"),
                    QString::from(e.to_string()),
                    QString::default(),
                );
            }
            let lens = self.stabilizer.lens.read();
            (
                lens.get_json().unwrap_or_default(),
                lens.path_to_file.clone(),
                lens.checksum.clone().unwrap_or_default(),
            )
        };
        self.lens_loaded = true;
        self.lens_changed();
        self.lens_profile_loaded(
            QString::from(json),
            QString::from(filepath),
            QString::from(checksum),
        );
        self.request_recompute();
    }
    fn load_default_preset(&mut self) {
        // Assumes regular filesystem
        let local_path = gyroflow_core::lens_profile_database::LensProfileDatabase::get_path()
            .join("default.gyroflow");

        let settings_path = gyroflow_core::settings::data_dir()
            .join("lens_profiles")
            .join("default.gyroflow");
        if settings_path.exists() {
            self.import_gyroflow_file(QUrl::from(QString::from(filesystem::path_to_url(
                &settings_path.to_string_lossy(),
            ))));
        } else if local_path.exists() {
            self.import_gyroflow_file(QUrl::from(QString::from(filesystem::path_to_url(
                &local_path.to_string_lossy(),
            ))));
        }
    }
    fn get_preset_contents(&mut self, url_or_id: QString) -> QString {
        let db = self.stabilizer.lens_profile_db.read();
        QString::from(
            db.get_preset_by_id(&url_or_id.to_string())
                .unwrap_or_default(),
        )
    }
    fn get_lens_group_config(&self) -> QString {
        QString::from(self.stabilizer.get_lens_group_config_json())
    }
    fn set_lens_group_config(&self, json: String) {
        self.stabilizer.set_lens_group_config_json(&json);
        self.lens_group_config_changed();
        // Keep timeline + main-canvas preview in sync when the user edits focal length /
        // anamorphic squeeze / preset in the Lens groups panel. Mirrors set_smoothing_param.
        self.chart_data_changed();
        self.request_recompute();
    }
    fn apply_lens_group_to_main(&self, lens_index: usize) -> QString {
        // Push the focal length / anamorphic squeeze from the selected lens group into
        // the main stabilizer's camera_matrix so fx/fy update in the live preview.
        // Returns a JSON object with the new output dimension when anamorphic pushes
        // one, so the QML caller can sync Export settings' output width/height fields.
        let out_dim = self.stabilizer.apply_lens_group_to_main(lens_index);
        self.lens_group_config_changed();
        self.chart_data_changed();
        // Emit lens_profile_loaded so Full-mode LensProfile.qml re-reads k1-k4 / camera
        // matrix from the rebuilt lens profile.
        let lens_json = self.stabilizer.lens.read().get_json().unwrap_or_default();
        self.lens_profile_loaded(
            QString::from(lens_json),
            QString::default(),
            QString::default(),
        );
        self.lens_changed();
        self.request_recompute();
        match out_dim {
            Some((w, h)) => QString::from(format!("{{\"w\":{},\"h\":{}}}", w, h)),
            None => QString::default(),
        }
    }
    fn preview_lens_group_config(&self, json: String, lens_index: usize) -> bool {
        let Some(_) = self
            .stabilizer
            .apply_lens_group_config_json_to_main(&json, lens_index)
        else {
            return false;
        };

        let lens_json = self.stabilizer.lens.read().get_json().unwrap_or_default();
        self.lens_profile_loaded(
            QString::from(lens_json),
            QString::default(),
            QString::default(),
        );
        self.lens_changed();
        self.chart_data_changed();
        self.request_recompute();
        true
    }
    fn get_lens_group_status(&self) -> QString {
        QString::from(self.stabilizer.get_lens_group_status_json())
    }
    fn refresh_lens_group_status(&self) {
        self.lens_group_status_changed();
    }
    fn get_lens_group_manual_edit(&self) -> bool {
        if self.stabilizer.has_project_lens_group_config() {
            return true;
        }
        self.stabilizer.get_lens_group_manual_edit()
    }
    fn set_lens_group_manual_edit(&self, enabled: bool) {
        if !self.stabilizer.has_project_lens_group_config()
            && self.stabilizer.get_lens_group_manual_edit() == enabled
        {
            return;
        }
        self.stabilizer.set_lens_group_manual_edit(enabled);
        self.lens_group_config_changed();
        self.lens_group_manual_edit_changed();
        // Reapply the selected lens-group decision to the main stabilizer and recompute
        // so the live preview reflects the new manual/auto state immediately.
        let lens_index = {
            let gyro = self.stabilizer.gyro.read();
            let md = gyro.file_metadata.read();
            core::niyien_lens_presets::extract_lens_index(&md.additional_data)
        };
        if let Some(lens_index) = lens_index {
            self.apply_lens_group_to_main(lens_index);
            return;
        }
        self.chart_data_changed();
        self.request_recompute();
    }
    fn get_lens_presets(&self) -> QString {
        QString::from(self.stabilizer.get_lens_presets_json())
    }
    fn has_neuflow_support(&self) -> bool {
        cfg!(any(feature = "neuflow-ort", feature = "neuflow-burn"))
    }

    fn set_preview_resolution(&mut self, target_height: i32, player: QJSValue) {
        self.preview_resolution = target_height;
        if let Some(vid) = player.to_qobject::<MDKVideoItem>() {
            let vid = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()

            // fn aligned_to_8(mut x: u32) -> u32 { if x % 8 != 0 { x += 8 - x % 8; } x }

            if !self.stabilizer.input_file.read().url.is_empty() {
                let h = if target_height > 0 {
                    target_height as u32
                } else {
                    vid.videoHeight
                };
                let ratio = vid.videoHeight as f64 / h as f64;
                let new_w = (vid.videoWidth as f64 / ratio).floor() as u32;
                let new_h = (vid.videoHeight as f64 / (vid.videoWidth as f64 / new_w as f64))
                    .floor() as u32;
                ::log::info!("surface size: {}x{}", new_w, new_h);

                self.chart_data_changed();

                vid.setSurfaceSize(new_w, new_h);
                vid.setRotation(vid.getRotation());
                // vid.setCurrentFrame(vid.currentFrame);
            }
        }
    }

    fn set_processing_resolution(&mut self, target_height: i32) {
        self.processing_resolution = target_height;
        self.stabilizer.pose_estimator.clear();
        self.chart_data_changed();
    }

    fn set_integration_method(&mut self, index: usize) {
        let finished = util::qt_queued_callback(QPointer::from(self as &Self), |this, _| {
            this.chart_data_changed();
            this.request_recompute();
        });

        let stab = self.stabilizer.clone();

        if stab.gyro.read().integration_method == index {
            return;
        }

        core::run_threaded(move || {
            {
                stab.invalidate_ongoing_computations();

                let mut gyro = stab.gyro.write();
                gyro.integration_method = index;
                gyro.integrate();
            }
            stab.invalidate_smoothing();
            finished(());
        });
    }

    fn set_preview_pipeline(&self, index: i32) {
        self.preview_pipeline.store(index as usize, SeqCst);
    }

    fn set_prevent_recompute(&self, v: bool) {
        self.stabilizer.prevent_recompute.store(v, SeqCst);
    }

    fn set_gpu_decoding(&self, enabled: bool) {
        self.stabilizer.set_gpu_decoding(enabled);
    }

    fn reset_player(&self, player: QJSValue) {
        if let Some(vid) = player.to_qobject::<MDKVideoItem>() {
            let vid = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()
            vid.onResize(Box::new(|_, _| {}));
            vid.onProcessTexture(Box::new(|_, _, _, _, _, _, _, _, _, _| -> bool { false }));
            vid.onProcessPixels(Box::new(|_, _, _, _, _, _| -> (u32, u32, u32, *mut u8) {
                (0, 0, 0, std::ptr::null_mut())
            }));
            vid.readyForProcessing(Box::new(|| -> bool { false }));
        }
    }
    fn init_player(&self, player: QJSValue) {
        use gyroflow_core::stabilization::RGBA8;

        if let Some(vid) = player.to_qobject::<MDKVideoItem>() {
            let vid1 = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()
            let vid = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()

            let bg_color = vid.getBackgroundColor().get_rgba_f();
            self.stabilizer.params.write().background = Vector4::new(
                bg_color.0 as f32,
                bg_color.1 as f32,
                bg_color.2 as f32,
                bg_color.3 as f32,
            );
            {
                let mut stab = self.stabilizer.stabilization.write();
                stab.kernel_flags
                    .set(KernelParamsFlags::DRAWING_ENABLED, true);
                stab.cache_frame_transform = true;
            }
            let request_recompute =
                util::qt_queued_callback_mut(QPointer::from(self as &Self), move |this, _: ()| {
                    this.request_recompute();
                });
            let stab = self.stabilizer.clone();
            vid.onResize(Box::new(move |width, height| {
                let current_size = stab.params.read().size;
                if current_size.0 != width as usize || current_size.1 != height as usize {
                    stab.init_size();
                    request_recompute(());
                }
            }));

            use gyroflow_core::gpu::{BufferDescription, BufferSource, Buffers};

            let stab = self.stabilizer.clone();
            vid.readyForProcessing(Box::new(move || -> bool {
                !stab.params.is_locked_exclusive() && !stab.stabilization.is_locked_exclusive()
            }));
            let stab = self.stabilizer.clone();
            let preview_pipeline = self.preview_pipeline.clone();
            let out_pixels = RefCell::new(Vec::new());
            let update_info =
                util::qt_queued_callback_mut(
                    QPointer::from(self as &Self),
                    move |this,
                          (fov, minimal_fov, focal_length, info): (
                        f64,
                        f64,
                        Option<f64>,
                        QString,
                    )| {
                        this.current_fov = fov;
                        this.current_minimal_fov = minimal_fov;
                        this.current_focal_length = focal_length.unwrap_or_default();
                        this.processing_info = info;
                        this.processing_info_changed();
                    },
                );
            let update_info2 = update_info.clone();

            #[allow(unused_variables)]
            vid.onProcessTexture(Box::new(
                move |frame,
                      timestamp_ms,
                      width,
                      height,
                      backend_id,
                      ptr1,
                      ptr2,
                      ptr3,
                      ptr4,
                      ptr5|
                      -> bool {
                    if width < 4 || height < 4 || backend_id == 0 {
                        return false;
                    }

                    if !stab.params.read().stab_enabled {
                        return true;
                    }

                    let _time = std::time::Instant::now();

                    if preview_pipeline.load(SeqCst) == 0 {
                        let mut buffers = Buffers {
                            input: BufferDescription {
                                size: (width as usize, height as usize, width as usize * 4),
                                ..Default::default()
                            },
                            output: BufferDescription {
                                size: (width as usize, height as usize, width as usize * 4),
                                ..Default::default()
                            },
                        };

                        let (offset, fps) = {
                            let params = stab.params.read();
                            (params.frame_offset, params.fps)
                        };
                        let frame = (frame as i32 + offset).max(0) as u32;
                        let timestamp_ms = timestamp_ms + (offset as f64 / fps * 1000.0).round();

                        if let Some(ret) = qrhi_undistort::render(
                            vid1.get_mdkplayer(),
                            timestamp_ms,
                            frame as usize,
                            width,
                            height,
                            stab.clone(),
                            &mut buffers,
                        ) {
                            update_info2((
                                ret.fov,
                                ret.minimal_fov,
                                ret.focal_length,
                                QString::from(format!(
                                    "Processing {}x{} using {} took {:.2}ms",
                                    width,
                                    height,
                                    ret.backend,
                                    _time.elapsed().as_micros() as f64 / 1000.0
                                )),
                            ));
                        } else {
                            update_info2((1.0, 1.0, None, QString::from("---")));
                        }
                        return true;
                    }

                    if preview_pipeline.load(SeqCst) > 1 {
                        return false;
                    }

                    let size = (width as usize, height as usize, width as usize * 4);

                    let mut buffers = match backend_id {
                        1 => {
                            // OpenGL, ptr1: texture, ptr2: opengl context
                            Some((
                                Buffers {
                                    input: BufferDescription {
                                        size,
                                        data: BufferSource::OpenGL {
                                            texture: ptr1 as u32,
                                            context: ptr2 as *mut std::ffi::c_void,
                                        },
                                        ..Default::default()
                                    },
                                    output: BufferDescription {
                                        size,
                                        data: BufferSource::OpenGL {
                                            texture: ptr1 as u32,
                                            context: ptr2 as *mut std::ffi::c_void,
                                        },
                                        ..Default::default()
                                    },
                                },
                                "OpenGL",
                            ))
                        }
                        #[cfg(any(target_os = "macos", target_os = "ios"))]
                        2 => {
                            // Metal, ptr1: texture, ptr2: device, ptr3: command queue
                            Some((
                                Buffers {
                                    input: BufferDescription {
                                        size,
                                        data: BufferSource::Metal {
                                            texture: ptr1 as *mut std::ffi::c_void,
                                            command_queue: ptr3 as *mut std::ffi::c_void,
                                        },
                                        ..Default::default()
                                    },
                                    output: BufferDescription {
                                        size,
                                        texture_copy: true,
                                        data: BufferSource::Metal {
                                            texture: ptr1 as *mut std::ffi::c_void,
                                            command_queue: ptr3 as *mut std::ffi::c_void,
                                        },
                                        ..Default::default()
                                    },
                                },
                                "Metal",
                            ))
                        }
                        #[cfg(target_os = "windows")]
                        3 => {
                            // D3D11, ptr1: texture, ptr2: device, ptr3: device context
                            Some((
                                Buffers {
                                    input: BufferDescription {
                                        size,
                                        texture_copy: true,
                                        data: BufferSource::DirectX11 {
                                            texture: ptr1 as *mut std::ffi::c_void,
                                            device: ptr2 as *mut std::ffi::c_void,
                                            device_context: ptr3 as *mut std::ffi::c_void,
                                        },
                                        ..Default::default()
                                    },
                                    output: BufferDescription {
                                        size,
                                        texture_copy: true,
                                        data: BufferSource::DirectX11 {
                                            texture: ptr1 as *mut std::ffi::c_void,
                                            device: ptr2 as *mut std::ffi::c_void,
                                            device_context: ptr3 as *mut std::ffi::c_void,
                                        },
                                        ..Default::default()
                                    },
                                },
                                "DirectX11",
                            ))
                        }
                        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
                        4 => {
                            // Vulkan, ptr1: VkImage, ptr2: VkDevice, ptr3: VkCommandBuffer, ptr4: VkPhysicalDevice, ptr5: VkInstance
                            Some((
                                Buffers {
                                    input: BufferDescription {
                                        size,
                                        texture_copy: false,
                                        data: BufferSource::Vulkan {
                                            texture: ptr1,
                                            device: ptr2,
                                            physical_device: ptr4,
                                            instance: ptr5,
                                        },
                                        ..Default::default()
                                    },
                                    output: BufferDescription {
                                        size,
                                        texture_copy: true,
                                        data: BufferSource::Vulkan {
                                            texture: ptr1,
                                            device: ptr2,
                                            physical_device: ptr4,
                                            instance: ptr5,
                                        },
                                        ..Default::default()
                                    },
                                },
                                "Vulkan",
                            ))
                        }
                        _ => None,
                    };

                    if let Some((ref mut buffers, backend)) = buffers {
                        match stab.process_pixels::<RGBA8>(
                            (timestamp_ms * 1000.0).round() as i64,
                            Some(frame as usize),
                            buffers,
                        ) {
                            Ok(ret) => {
                                update_info2((
                                    ret.fov,
                                    ret.minimal_fov,
                                    ret.focal_length,
                                    QString::from(format!(
                                        "Processing {}x{} using {backend}->{} took {:.2}ms",
                                        width,
                                        height,
                                        ret.backend,
                                        _time.elapsed().as_micros() as f64 / 1000.0
                                    )),
                                ));
                                return true;
                            }
                            Err(e) => {
                                ::log::error!("Failed to process pixels: {e:?}");
                            }
                        }
                    }

                    update_info2((1.0, 1.0, None, QString::from("---")));
                    false
                },
            ));

            let stab = self.stabilizer.clone();
            let update_info2 = update_info.clone();
            vid.onProcessPixels(Box::new(
                move |frame,
                      timestamp_ms,
                      width,
                      height,
                      stride,
                      pixels: &mut [u8]|
                      -> (u32, u32, u32, *mut u8) {
                    let _time = std::time::Instant::now();

                    // TODO: cache in atomics instead of locking the mutex every time
                    let params = stab.params.read();
                    if !params.stab_enabled {
                        return (0, 0, 0, std::ptr::null_mut());
                    }
                    let (ow, oh) = params.output_size;
                    let os = ow * 4; // Assume RGBA8 - 4 bytes per pixel
                    drop(params);

                    let mut out_pixels = out_pixels.borrow_mut();
                    out_pixels.resize_with(os * oh, u8::default);

                    let ret = stab.process_pixels::<RGBA8>(
                        (timestamp_ms * 1000.0).round() as i64,
                        Some(frame as usize),
                        &mut Buffers {
                            input: BufferDescription {
                                size: (width as usize, height as usize, stride as usize),
                                data: BufferSource::Cpu { buffer: pixels },
                                ..Default::default()
                            },
                            output: BufferDescription {
                                size: (ow, oh, os),
                                data: BufferSource::Cpu {
                                    buffer: &mut out_pixels,
                                },
                                ..Default::default()
                            },
                        },
                    );
                    match ret {
                        Ok(bk) => {
                            update_info2((
                                bk.fov,
                                bk.minimal_fov,
                                bk.focal_length,
                                QString::from(format!(
                                    "Processing {}x{} using {} took {:.2}ms",
                                    width,
                                    height,
                                    bk.backend,
                                    _time.elapsed().as_micros() as f64 / 1000.0
                                )),
                            ));
                            (ow as u32, oh as u32, os as u32, out_pixels.as_mut_ptr())
                        }
                        Err(_) => {
                            update_info2((1.0, 1.0, None, QString::from("---")));
                            (0, 0, 0, std::ptr::null_mut())
                        }
                    }
                },
            ));
        }
    }

    fn set_background_color(&mut self, color: QString, player: QJSValue) {
        if let Some(vid) = player.to_qobject::<MDKVideoItem>() {
            let vid = unsafe { &mut *vid.as_ptr() }; // vid.borrow_mut()

            let color = QColor::from_name(&color.to_string());
            vid.setBackgroundColor(color);

            let bg = color.get_rgba_f();
            self.stabilizer.set_background_color(Vector4::new(
                bg.0 as f32,
                bg.1 as f32,
                bg.2 as f32,
                bg.3 as f32,
            ));
            self.request_recompute();
        }
    }

    fn set_smoothing_method(&mut self, index: usize) -> QJsonArray {
        let params = util::serde_json_to_qt_array(&self.stabilizer.set_smoothing_method(index));
        self.request_recompute();
        self.chart_data_changed();
        params
    }
    fn set_smoothing_param(&mut self, name: QString, val: f64) {
        self.stabilizer.set_smoothing_param(&name.to_string(), val);
        self.chart_data_changed();
        self.request_recompute();
    }
    wrap_simple_method!(set_horizon_lock, lock_percent: f64, roll: f64, lock_pitch: bool, pitch: f64, automatic_lock: bool, turn_threshold: f64, turn_smoothing_ms: f64, turn_multiplier: f64, tilt_accel_limit: f64; recompute; chart_data_changed);
    wrap_simple_method!(set_use_gravity_vectors, v: bool; recompute; chart_data_changed);
    wrap_simple_method!(set_horizon_lock_integration_method, v: i32; recompute; chart_data_changed);
    pub fn get_smoothing_algs(&self) -> QVariantList {
        self.stabilizer
            .get_smoothing_algs()
            .into_iter()
            .map(QString::from)
            .collect()
    }
    fn get_smoothing_status(&self) -> QJsonArray {
        util::serde_json_to_qt_array(&self.stabilizer.get_smoothing_status())
    }
    fn get_smoothing_max_angles(&self) -> QJsonArray {
        let max_angles = self.stabilizer.get_smoothing_max_angles();
        util::serde_json_to_qt_array(&serde_json::json!([
            max_angles.0,
            max_angles.1,
            max_angles.2
        ]))
    }

    fn recompute_threaded(&mut self) {
        if self.stabilizer.params.read().duration_ms <= 0.0 {
            return;
        }
        let id = self
            .stabilizer
            .recompute_threaded(util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                |this, (id, _discarded): (u64, bool)| {
                    if !this.ongoing_computations.contains(&id) {
                        ::log::error!("Unknown compute_id: {}", id);
                    }
                    this.ongoing_computations.remove(&id);
                    let finished = this.ongoing_computations.is_empty();
                    this.compute_progress(id, if finished { 1.0 } else { 0.0 });
                },
            ));
        self.ongoing_computations.insert(id);

        self.compute_progress(id, 0.0);
    }

    fn cancel_current_operation(&mut self) {
        self.cancel_flag.store(true, SeqCst);
    }

    fn export_gyroflow_file(&self, url: QUrl, typ: QString, additional_data: QJsonObject) {
        let url = util::qurl_to_encoded(url);
        let typ_str = typ.clone();
        let typ = core::GyroflowProjectType::from_str(&typ.to_string()).unwrap();

        #[cfg(not(any(target_os = "ios", target_os = "android")))]
        {
            gyroflow_core::settings::set("lastProject", filesystem::url_to_path(&url).into());
        }
        let finished = util::qt_queued_callback(
            QPointer::from(self as &Self),
            move |this, (res, arg): (&str, String)| {
                match res {
                    "ok" => this.message(
                        QString::from("Gyroflow file exported to %1."),
                        QString::from(format!("<b>{}</b>", filesystem::display_url(&arg))),
                        QString::default(),
                        QString::from("gyroflow-exported"),
                    ),
                    "location" => this.request_location(QString::from(arg), typ_str.clone()),
                    "err" => this.error(
                        QString::from("An error occured: %1"),
                        QString::from(arg),
                        QString::default(),
                    ),
                    _ => {}
                }
                this.request_recompute();
            },
        );

        let lens_checksum = self.stabilizer.lens.read().checksum.clone();

        let stab = self.stabilizer.clone();
        core::run_threaded(move || {
            util::report_lens_profile_usage(lens_checksum);

            match stab.export_gyroflow_file(&url, typ, &additional_data.to_json().to_string()) {
                Ok(_) => finished(("ok", url.to_string())),
                Err(core::GyroflowCoreError::IOError(ref e))
                    if e.kind() == std::io::ErrorKind::PermissionDenied =>
                {
                    finished(("location", url.to_string()))
                }
                Err(e) => finished(("err", e.to_string())),
            }
        });
    }

    fn export_gyroflow_data(&self, typ: QString, additional_data: QJsonObject) -> QString {
        let typ = core::GyroflowProjectType::from_str(&typ.to_string()).unwrap();

        util::report_lens_profile_usage(self.stabilizer.lens.read().checksum.clone());

        QString::from(
            self.stabilizer
                .export_gyroflow_data(typ, &additional_data.to_json().to_string(), None)
                .unwrap_or_default(),
        )
    }

    fn get_version_from_gyroflow_file(&mut self, url: QUrl) -> u32 {
        let url = util::qurl_to_encoded(url);
        let mut version = 0;
        if let Ok(data) = filesystem::read(&url) {
            if let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice(&data) {
                if let Some(v) = obj.get("version").and_then(|x| x.as_u64()) {
                    version = v as u32;
                }
            } else {
                ::log::error!("Failed to parse json: {}", unsafe {
                    std::str::from_utf8_unchecked(&data)
                });
            }
        }
        version
    }
    fn get_urls_from_gyroflow_file(&mut self, url: QUrl) -> QStringList {
        let url = util::qurl_to_encoded(url);
        let mut ret = vec![QString::default(); 2];
        if let Ok(data) = filesystem::read(&url) {
            if let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice(&data) {
                let mut org_video_url = obj
                    .get("videofile")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                if !org_video_url.is_empty() && !org_video_url.contains("://") {
                    org_video_url = filesystem::path_to_url(&org_video_url);
                }
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                if let Some(v) = obj
                    .get("videofile_bookmark")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty())
                {
                    let (resolved, _is_stale) = filesystem::apple::resolve_bookmark(v, Some(&url));
                    if !resolved.is_empty() {
                        org_video_url = resolved;
                    }
                }

                if let Some(seq_start) = obj.get("image_sequence_start").and_then(|x| x.as_i64()) {
                    self.image_sequence_start = seq_start as i32;
                }
                if let Some(seq_fps) = obj.get("image_sequence_fps").and_then(|x| x.as_f64()) {
                    self.image_sequence_fps = seq_fps;
                }
                if !org_video_url.is_empty() {
                    let video_path = StabilizationManager::get_new_videofile_url(
                        &org_video_url,
                        Some(&url),
                        self.image_sequence_start as u32,
                    );
                    ret[0] = QString::from(video_path);
                }

                if let Some(serde_json::Value::Object(gyro)) = obj.get("gyro_source") {
                    let mut gyro_url = gyro
                        .get("filepath")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string();
                    if !gyro_url.is_empty() && !gyro_url.contains("://") {
                        gyro_url = filesystem::path_to_url(&gyro_url);
                    }
                    #[cfg(any(target_os = "macos", target_os = "ios"))]
                    if let Some(v) = obj
                        .get("filepath_bookmark")
                        .and_then(|x| x.as_str())
                        .filter(|x| !x.is_empty())
                    {
                        let (resolved, _is_stale) =
                            filesystem::apple::resolve_bookmark(v, Some(&url));
                        if !resolved.is_empty() {
                            gyro_url = resolved;
                        }
                    }

                    if !gyro_url.is_empty() {
                        let gyro_url = StabilizationManager::get_new_videofile_url(
                            &gyro_url,
                            Some(&url),
                            self.image_sequence_start as u32,
                        );
                        ret[1] = QString::from(gyro_url);
                    }
                }
            } else {
                ::log::error!("Failed to parse json: {}", unsafe {
                    std::str::from_utf8_unchecked(&data)
                });
            }
        }
        QStringList::from_iter(ret.into_iter())
    }

    fn import_gyroflow_file(&mut self, url: QUrl) {
        let url = util::qurl_to_encoded(url);
        let progress = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, progress: f64| {
                this.loading_gyro_in_progress = progress < 1.0;
                this.loading_gyro_progress(progress);
                this.loading_gyro_in_progress_changed();
            },
        );
        let finished = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, obj: Result<serde_json::Value, gyroflow_core::GyroflowCoreError>| {
                this.loading_gyro_in_progress = false;
                this.loading_gyro_progress(1.0);
                this.loading_gyro_in_progress_changed();

                let obj = this.import_gyroflow_internal(obj);
                this.gyroflow_file_loaded(obj);
                this.project_file_url_changed();
            },
        );

        let stab = self.stabilizer.clone();
        let cancel_flag = self.cancel_flag.clone();
        cancel_flag.store(true, SeqCst);
        core::run_threaded(move || {
            if Arc::strong_count(&cancel_flag) > 2 {
                // Wait for other tasks to finish
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            cancel_flag.store(false, SeqCst);
            finished(stab.import_gyroflow_file(&url, false, progress, cancel_flag, false));
        });
    }
    fn import_gyroflow_data(&mut self, data: QString) {
        let progress = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, progress: f64| {
                this.loading_gyro_in_progress = progress < 1.0;
                this.loading_gyro_progress(progress);
                this.loading_gyro_in_progress_changed();
            },
        );
        let finished = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, obj: Result<serde_json::Value, gyroflow_core::GyroflowCoreError>| {
                this.loading_gyro_in_progress = false;
                this.loading_gyro_progress(1.0);
                this.loading_gyro_in_progress_changed();

                let obj = this.import_gyroflow_internal(obj);
                this.gyroflow_file_loaded(obj);
            },
        );

        let stab = self.stabilizer.clone();
        let cancel_flag = self.cancel_flag.clone();
        cancel_flag.store(true, SeqCst);
        core::run_threaded(move || {
            if Arc::strong_count(&cancel_flag) > 2 {
                // Wait for other tasks to finish
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            cancel_flag.store(false, SeqCst);
            let mut is_preset = false;
            finished(stab.import_gyroflow_data(
                data.to_string().as_bytes(),
                false,
                None,
                progress,
                cancel_flag,
                &mut is_preset,
                false,
            ));
        });
    }
    fn import_gyroflow_internal(
        &mut self,
        result: Result<serde_json::Value, gyroflow_core::GyroflowCoreError>,
    ) -> QJsonObject {
        match result {
            Ok(thin_obj) => {
                if thin_obj
                    .as_object()
                    .unwrap()
                    .contains_key("calibration_data")
                {
                    self.lens_loaded = true;
                    self.lens_changed();
                    let lens_json = self.stabilizer.lens.read().get_json().unwrap_or_default();
                    self.lens_profile_loaded(
                        QString::from(lens_json),
                        QString::default(),
                        QString::default(),
                    );
                    self.lens_group_config_changed();
                    self.lens_group_manual_edit_changed();
                }
                self.gyro_loaded = self.gyro_has_raw_imu() || self.gyro_has_quaternions();
                self.gyro_changed();
                self.update_offset_model();
                self.request_recompute();
                self.chart_data_changed();
                self.keyframes_changed();
                util::serde_json_to_qt_object(&thin_obj)
            }
            Err(e) => {
                self.error(
                    QString::from("An error occured: %1"),
                    QString::from(e.to_string()),
                    QString::default(),
                );
                QJsonObject::default()
            }
        }
    }

    fn set_output_size(&self, w: usize, h: usize) {
        if self.stabilizer.set_output_size(w, h) {
            self.stabilizer.recompute_undistortion();
            self.request_recompute();
        }
    }

    wrap_simple_method!(override_video_fps,         v: f64, r: bool; recompute; update_offset_model);
    wrap_simple_method!(set_video_rotation,         v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_stab_enabled,           v: bool);
    wrap_simple_method!(set_show_detected_features, v: bool);
    wrap_simple_method!(set_show_optical_flow,      v: bool);
    wrap_simple_method!(set_digital_lens_name,      v: String; recompute);
    wrap_simple_method!(set_digital_lens_param,     i: usize, v: f64; recompute);
    wrap_simple_method!(set_fov_overview,       v: bool; recompute);
    wrap_simple_method!(set_show_safe_area,     v: bool; recompute);
    wrap_simple_method!(set_fov,                v: f64; recompute; chart_data_changed);
    wrap_simple_method!(set_frame_readout_time, v: f64; recompute);
    wrap_simple_method!(set_frame_readout_direction, v: i32; recompute);
    wrap_simple_method!(set_adaptive_zoom,      v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_max_zoom,           v: f64, i: usize; recompute; zooming_data_changed);
    wrap_simple_method!(set_zooming_center_x,   v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_zooming_center_y,   v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_additional_rotation_x,v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_additional_rotation_y,v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_additional_rotation_z,v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_additional_translation_x,v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_additional_translation_y,v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_additional_translation_z,v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_zooming_method,     v: i32; recompute; zooming_data_changed);
    wrap_simple_method!(set_of_method,          v: u32; recompute; chart_data_changed);

    wrap_simple_method!(set_lens_correction_amount,    v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_frame_offset,              v: i32; recompute);
    wrap_simple_method!(set_light_refraction_coefficient, v: f64; recompute; zooming_data_changed);
    wrap_simple_method!(set_input_horizontal_stretch,  v: f64; recompute);
    wrap_simple_method!(set_lens_is_asymmetrical,      v: bool; recompute);
    wrap_simple_method!(set_input_vertical_stretch,    v: f64; recompute);
    wrap_simple_method!(set_background_mode,           v: i32; recompute);
    wrap_simple_method!(set_background_margin,         v: f64; recompute);
    wrap_simple_method!(set_background_margin_feather, v: f64; recompute);
    wrap_simple_method!(set_video_speed,               v: f64, s: bool, z: bool, zl: bool; recompute; zooming_data_changed);

    wrap_simple_method!(set_offset, timestamp_us: i64, offset_ms: f64; recompute; update_offset_model);
    wrap_simple_method!(clear_offsets,; recompute; update_offset_model);
    wrap_simple_method!(remove_offset, timestamp_us: i64; recompute; update_offset_model);

    wrap_simple_method!(set_imu_lpf, v: f64; recompute; chart_data_changed);
    wrap_simple_method!(set_imu_median_filter, size: i32; recompute; chart_data_changed);
    wrap_simple_method!(set_imu_rotation, pitch_deg: f64, roll_deg: f64, yaw_deg: f64; recompute; chart_data_changed);
    wrap_simple_method!(set_acc_rotation, pitch_deg: f64, roll_deg: f64, yaw_deg: f64; recompute; chart_data_changed);
    wrap_simple_method!(set_imu_orientation, v: String; recompute; chart_data_changed);
    wrap_simple_method!(set_sync_lpf, v: f64; recompute; chart_data_changed);
    wrap_simple_method!(set_imu_bias, bx: f64, by: f64, bz: f64; recompute; chart_data_changed);
    wrap_simple_method!(recompute_gyro,; recompute; chart_data_changed);
    wrap_simple_method!(set_device, v: i32);

    fn get_org_duration_ms(&self) -> f64 {
        self.stabilizer.params.read().duration_ms
    }
    fn get_scaled_duration_ms(&self) -> f64 {
        self.stabilizer.params.read().get_scaled_duration_ms()
    }
    fn get_scaled_fps(&self) -> f64 {
        self.stabilizer.params.read().get_scaled_fps()
    }
    fn get_scaling_ratio(&self) -> f64 {
        self.stabilizer.get_scaling_ratio()
    }
    fn get_min_fov(&self) -> f64 {
        self.stabilizer.get_min_fov()
    }
    fn set_video_created_at(&self, timestamp_ms: f64) {
        self.stabilizer.params.write().video_created_at = if timestamp_ms > 0.0 {
            Some(timestamp_ms as i64)
        } else {
            None
        };
    }

    fn set_trim_ranges(&self, ranges: QString) {
        let ranges = ranges
            .to_string()
            .split(';')
            .filter_map(|x| {
                let mut x = x.split(':');
                Some((
                    x.next()?.parse::<f64>().ok()?,
                    x.next()?.parse::<f64>().ok()?,
                ))
            })
            .collect::<Vec<(f64, f64)>>();
        self.stabilizer.set_trim_ranges(ranges);
        self.request_recompute();
        self.chart_data_changed();
    }

    fn offset_at_video_timestamp(&self, timestamp_us: i64) -> f64 {
        self.stabilizer.offset_at_video_timestamp(timestamp_us)
    }
    fn quats_at_timestamp(&self, timestamp_us: i64) -> QVariantList {
        let gyro = self.stabilizer.gyro.read();
        let ts = timestamp_us as f64 / 1000.0
            - gyro.offset_at_video_timestamp(timestamp_us as f64 / 1000.0);
        let sq = gyro.smoothed_quat_at_timestamp(ts);
        let q = gyro.org_quat_at_timestamp(ts);
        QVariantList::from_iter(&[q.w, q.i, q.j, q.k, sq.w, sq.i, sq.j, sq.k]) // scalar first
    }
    fn mesh_at_frame(&self, frame: usize) -> QVariantList {
        let gyro = self.stabilizer.gyro.read();
        let file_metadata = gyro.file_metadata.read();
        if let Some(mc) = file_metadata.mesh_correction.get(frame) {
            QVariantList::from_iter(mc.1.iter())
        } else {
            QVariantList::default()
        }
    }
    fn get_turn_speed(&self, timestamp_ms: f64) -> f64 {
        let params = self.stabilizer.params.read();
        let fps = params.fps;
        let frame_duration_ms = 1000.0 / fps;
        let lookback_ms = 60.0 * frame_duration_ms;
        if timestamp_ms < lookback_ms {
            return f64::NAN;
        }
        let current_timestamp_ms = timestamp_ms;
        let past_timestamp_ms = timestamp_ms - lookback_ms;
        let gyro = self.stabilizer.gyro.read();
        let quat_org_current = gyro.org_quat_at_timestamp(current_timestamp_ms);
        let quat_smooth_current = gyro.smoothed_quat_at_timestamp(current_timestamp_ms);
        let quat_org_past = gyro.org_quat_at_timestamp(past_timestamp_ms);
        let quat_smooth_past = gyro.smoothed_quat_at_timestamp(past_timestamp_ms);
        let quat_stab_current = (quat_smooth_current / quat_org_current).inverse();
        let quat_stab_past = (quat_smooth_past / quat_org_past).inverse();
        let euler_current = quat_stab_current.euler_angles();
        let euler_past = quat_stab_past.euler_angles();
        let roll_current = euler_current.2;
        let roll_past = euler_past.2;
        let mut angle_change_deg = (roll_current - roll_past).to_degrees();
        while angle_change_deg > 180.0 {
            angle_change_deg -= 360.0;
        }
        while angle_change_deg < -180.0 {
            angle_change_deg += 360.0;
        }
        let time_diff_s = lookback_ms / 1000.0;
        angle_change_deg / time_diff_s
    }
    fn get_x_angle(&self, timestamp_ms: f64) -> f64 {
        let gyro = self.stabilizer.gyro.read();
        let quat_org = gyro.org_quat_at_timestamp(timestamp_ms);
        let quat_smooth = gyro.smoothed_quat_at_timestamp(timestamp_ms);
        let quat_stab = (quat_smooth / quat_org).inverse();
        let euler = quat_stab.euler_angles();
        let roll = euler.2;
        roll.to_degrees()
    }
    fn set_lens_param(&self, param: QString, value: f64) {
        self.stabilizer
            .set_lens_param(param.to_string().as_str(), value);
        self.request_recompute();
    }
    fn set_user_focal_length(&mut self, focal_length_mm: f64) {
        self.stabilizer.set_user_focal_length(focal_length_mm);
        // Update UI with new lens data
        let lens = self.stabilizer.lens.read();
        let json = lens.get_json().unwrap_or_default();
        let filepath = lens.path_to_file.clone();
        let checksum = lens.checksum.clone().unwrap_or_default();
        drop(lens);
        self.lens_loaded = true;
        self.lens_changed();
        self.lens_profile_loaded(
            QString::from(json),
            QString::from(filepath),
            QString::from(checksum),
        );
        self.request_recompute();
    }

    fn check_updates(&self) {
        let update = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, (version, changelog, download_url): (String, String, String)| {
                this.updates_available(
                    QString::from(version),
                    QString::from(changelog),
                    QString::from(download_url),
                )
            },
        );
        core::run_threaded(move || match crate::distribution::fetch_manifest(false) {
            Ok(manifest) => {
                match crate::distribution::sync_data_packages(&manifest) {
                    Ok(results) => {
                        for result in results {
                            if result.updated {
                                ::log::info!("Updated distribution package {}", result.package);
                            }
                        }
                    }
                    Err(err) => {
                        ::log::warn!("Distribution data sync failed: {}", err);
                    }
                }

                if crate::distribution::has_app_update(&manifest) {
                    ::log::info!(
                        "Latest NiYien version: {}, current version: {}",
                        manifest.app.version,
                        util::get_version()
                    );
                    update((
                        manifest.app.version,
                        manifest.app.changelog,
                        manifest.app.url,
                    ));
                }
            }
            Err(err) => {
                ::log::warn!("Manifest check failed: {}", err);
                crate::distribution::report_download_event(
                    "manifest_fetch",
                    "manifest",
                    "",
                    gyroflow_core::distribution::manifest_api(),
                    "fail",
                    0,
                    0,
                    &err,
                );
            }
        });
    }

    fn fetch_available_versions(&self) -> QString {
        let payload = match crate::distribution::fetch_app_update_candidates(true) {
            Ok(updates) => serde_json::json!({
                "updates": updates
            }),
            Err(err) => serde_json::json!({
                "updates": [],
                "error": err
            }),
        };
        QString::from(
            serde_json::to_string(&payload).unwrap_or_else(|_| "{\"updates\":[]}".to_owned()),
        )
    }

    fn start_app_update(&self) {
        self.start_app_update_for_version(None);
    }

    fn start_app_update_version(&self, version: QString) {
        let version = version.to_string();
        let version = version.trim().to_owned();
        self.start_app_update_for_version((!version.is_empty()).then_some(version));
    }

    fn start_app_update_for_version(&self, requested_version: Option<String>) {
        let progress = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, (downloaded, total, message): (u64, u64, String)| {
                this.app_update_progress(downloaded as f64, total as f64, QString::from(message));
            },
        );
        let ready = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, (path, platform, message): (String, String, String)| {
                this.app_update_ready(
                    QString::from(path),
                    QString::from(platform),
                    QString::from(message),
                );
            },
        );
        let error =
            util::qt_queued_callback_mut(QPointer::from(self as &Self), |this, message: String| {
                this.app_update_error(QString::from(message));
            });
        let state = self.app_update_state.clone();
        core::run_threaded(move || {
            let result = (|| -> Result<crate::distribution::PreparedAppUpdate, String> {
                let manifest = crate::distribution::fetch_manifest(true)?;
                let selection = crate::distribution::app_update_package_for_requested_version(
                    &manifest,
                    requested_version.as_deref(),
                    crate::distribution::platform_name(),
                )
                .ok_or_else(|| match requested_version.as_deref() {
                    Some(version) => format!(
                        "No app update package is available for version {version} on this platform"
                    ),
                    None => "No app update package is available for this platform".to_owned(),
                })?;
                crate::distribution::download_app_update(&selection, |downloaded, total, status| {
                    progress((downloaded, total, status.to_owned()));
                })
            })();
            match result {
                Ok(prepared) => {
                    let path = prepared.path.display().to_string();
                    let platform = prepared.selection.platform.clone();
                    let message = if platform == "macos" {
                        "After the DMG opens, drag Gyroflow(NiYien).app to the Applications folder."
                            .to_owned()
                    } else {
                        "Update package is ready".to_owned()
                    };
                    *state.lock() = Some(prepared);
                    ready((path, platform, message));
                }
                Err(err) => error(err),
            }
        });
    }

    fn open_downloaded_update_and_quit(&self) {
        let prepared = self.app_update_state.lock().clone();
        match prepared {
            Some(prepared) => match crate::distribution::open_downloaded_update(&prepared) {
                Ok(()) => self.app_update_handoff_started(),
                Err(err) => self.app_update_error(QString::from(err)),
            },
            None => self.app_update_error(QString::from("No downloaded update is ready")),
        }
    }

    fn sync_device_time(&mut self, tz_offset_minutes: i32) {
        ::log::info!("NiYien: sync_device_time requested");
        if !self.device_connected {
            self.device_time_sync_finished(false, self.device_unavailable_message());
            return;
        }
        self.device_time_sync_in_progress = true;
        self.device_state_changed();
        if let Some(tx) = self.device_command_tx.as_ref() {
            let offset = tz_offset_minutes.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            if tx.send(DeviceCommand::SyncTime(offset)).is_ok() {
                return;
            }
        }

        self.device_time_sync_in_progress = false;
        self.device_state_changed();
        self.device_time_sync_finished(false, QString::from("Device command channel is unavailable"));
    }

    fn check_firmware_update(&mut self) {
        ::log::info!("NiYien: check_firmware_update requested");
        self.ota_error = QString::default();
        self.ota_state = QString::from("checking");
        self.firmware_update_available = false;
        self.firmware_latest_version = QString::default();
        self.firmware_changelog = QString::default();
        self.device_state_changed();

        if !self.device_connected {
            self.ota_state = QString::from("failed");
            self.ota_error = self.device_unavailable_message();
            self.device_state_changed();
            return;
        }

        if let Some(tx) = self.device_command_tx.as_ref() {
            let current_version = if self.device_soft_version.is_empty() {
                "0.0.0".to_owned()
            } else {
                self.device_soft_version.to_string()
            };
            if tx.send(DeviceCommand::CheckUpdate(current_version)).is_ok() {
                return;
            }
        }

        self.ota_state = QString::from("failed");
        self.ota_error = QString::from("Device command channel is unavailable");
        self.device_state_changed();
    }

    fn start_firmware_update(&mut self) {
        ::log::info!("NiYien: start_firmware_update requested");
        if !self.firmware_update_available {
            self.ota_state = QString::from("failed");
            self.ota_error = QString::from("No firmware update is available");
            self.device_state_changed();
            return;
        }
        if !self.device_connected {
            self.ota_state = QString::from("failed");
            self.ota_error = self.device_unavailable_message();
            self.device_state_changed();
            return;
        }

        self.ota_progress = 0.0;
        self.ota_error = QString::default();
        self.ota_state = QString::from("updating");
        self.device_state_changed();

        if let Some(tx) = self.device_command_tx.as_ref() {
            if tx.send(DeviceCommand::StartOta).is_ok() {
                return;
            }
        }

        self.ota_state = QString::from("failed");
        self.ota_error = QString::from("Device command channel is unavailable");
        self.device_state_changed();
    }

    fn poll_device_events(&mut self) {
        let Some(rx) = self.device_event_rx.as_ref().map(Arc::clone) else {
            return;
        };

        loop {
            let event = {
                let rx = rx.lock();
                rx.try_recv()
            };
            match event {
                Ok(event) => self.handle_device_event(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.device_connected = false;
                    self.device_connection_status =
                        QString::from(DeviceConnectionStatus::Error.as_str());
                    self.device_connection_message =
                        QString::from("Device event channel is unavailable");
                    self.device_state_changed();
                    break;
                }
            }
        }
    }

    fn handle_device_event(&mut self, event: DeviceEvent) {
        match event {
            DeviceEvent::ConnectionStatus(status, message) => {
                self.device_connection_status = QString::from(status.as_str());
                self.device_connection_message = QString::from(message);
                if status != DeviceConnectionStatus::Connected {
                    self.device_connected = false;
                    self.device_time_sync_in_progress = false;
                }
                if matches!(
                    status,
                    DeviceConnectionStatus::PermissionDenied
                        | DeviceConnectionStatus::Unsupported
                        | DeviceConnectionStatus::Error
                ) && self.ota_state == QString::from("updating")
                {
                    self.ota_state = QString::from("failed");
                    self.ota_error = self.device_unavailable_message();
                }
                self.device_state_changed();
            }
            DeviceEvent::Connected(info) => {
                let soft_version = info.soft_version.clone();
                let hard_version = info.hard_version.clone();
                let serial = Self::format_device_serial(&info.serial_number);
                self.device_connected = true;
                self.device_connection_status =
                    QString::from(DeviceConnectionStatus::Connected.as_str());
                self.device_connection_message = QString::default();
                self.device_name = QString::from(format!("A1:{serial}"));
                self.device_soft_version = QString::from(info.soft_version);
                self.device_hard_version = QString::from(info.hard_version);
                self.ota_error = QString::default();
                ::log::info!(
                    "NiYien: device connected name=A1:{} soft={} hard={}",
                    serial,
                    soft_version,
                    hard_version
                );
                self.device_state_changed();
                if std::env::var("GYROFLOW_NIYIEN_AUTO_SYNC_TIME").unwrap_or_default() == "1" {
                    let system_offset_minutes =
                        chrono::Local::now().offset().local_minus_utc() / 60;
                    self.sync_device_time(system_offset_minutes);
                }
                if self.ota_state != QString::from("updating") {
                    self.check_firmware_update();
                }
            }
            DeviceEvent::Disconnected => {
                self.device_connected = false;
                self.device_connection_status =
                    QString::from(DeviceConnectionStatus::Idle.as_str());
                self.device_connection_message = QString::default();
                self.device_time_sync_in_progress = false;
                self.device_name = QString::default();
                self.device_soft_version = QString::default();
                self.device_hard_version = QString::default();
                ::log::info!("NiYien: device disconnected");
                if self.ota_state == QString::from("updating") {
                    self.ota_state = QString::from("failed");
                    self.ota_error = QString::from("The device was disconnected");
                } else if self.ota_state != QString::from("failed") {
                    self.ota_state = QString::from("none");
                }
                self.device_state_changed();
            }
            DeviceEvent::TimeReceived(time) => {
                let formatted = Self::format_device_time(&time);
                self.device_time = QString::from(formatted.clone());
                // ::log::info!("NiYien: device time received {}", formatted);
                self.device_state_changed();
            }
            DeviceEvent::TimeSyncResult(success) => {
                ::log::info!("NiYien: time sync result success={}", success);
                self.device_time_sync_in_progress = false;
                self.device_state_changed();
                self.device_time_sync_finished(
                    success,
                    QString::from(if success {
                        "Device time synchronized successfully"
                    } else {
                        "Failed to synchronize device time"
                    }),
                );
            }
            DeviceEvent::UpdateAvailable(info) => {
                self.ota_state = QString::from("up_to_date");
                self.ota_error = QString::default();
                if let Some(info) = info {
                    ::log::info!(
                        "NiYien: firmware update available version={} file={}",
                        info.version,
                        info.filename
                    );
                    self.firmware_update_available = true;
                    self.ota_state = QString::from("update_available");
                    self.firmware_latest_version = QString::from(info.version);
                    self.firmware_changelog = QString::from(if info.changelog_en.is_empty() {
                        info.changelog_zh
                    } else {
                        info.changelog_en
                    });
                    if std::env::var("GYROFLOW_NIYIEN_AUTO_START_OTA").unwrap_or_default() == "1" {
                        self.start_firmware_update();
                    }
                } else {
                    ::log::info!("NiYien: no firmware update available");
                    self.firmware_update_available = false;
                    self.firmware_latest_version = QString::default();
                    self.firmware_changelog = QString::default();
                }
                self.device_state_changed();
            }
            DeviceEvent::UpdateCheckFailed(message) => {
                ::log::warn!("NiYien: update check failed: {}", message);
                self.firmware_update_available = false;
                self.firmware_latest_version = QString::default();
                self.firmware_changelog = QString::default();
                self.ota_state = QString::from("failed");
                self.ota_error = QString::from(message);
                self.device_state_changed();
            }
            DeviceEvent::OtaProgress(progress) => {
                ::log::info!("NiYien: OTA progress {:.0}%", progress * 100.0);
                self.ota_progress = progress;
                self.ota_state = QString::from("updating");
                self.device_state_changed();
            }
            DeviceEvent::OtaComplete => {
                ::log::info!("NiYien: OTA complete");
                self.ota_progress = 1.0;
                self.ota_state = QString::from("success");
                self.ota_error = QString::default();
                self.device_state_changed();
            }
            DeviceEvent::OtaFailed(message) => {
                ::log::warn!("NiYien: OTA failed: {}", message);
                self.ota_state = QString::from("failed");
                self.ota_error = QString::from(message);
                self.device_state_changed();
            }
        }
    }

    fn device_unavailable_message(&self) -> QString {
        if !self.device_connection_message.is_empty() {
            return self.device_connection_message.clone();
        }
        QString::from("Device is not connected")
    }

    fn format_device_serial(serial_number: &[u8; 12]) -> String {
        let ascii_bytes: Vec<u8> = serial_number
            .iter()
            .copied()
            .take_while(|byte| *byte != 0)
            .collect();
        if !ascii_bytes.is_empty() && ascii_bytes.iter().all(|byte| byte.is_ascii_graphic()) {
            return String::from_utf8_lossy(&ascii_bytes).into_owned();
        }

        let mut hex = String::with_capacity(serial_number.len() * 2);
        for byte in serial_number {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02X}");
        }
        hex
    }

    fn format_device_time(time: &crate::niyien_device::commands::DeviceTime) -> String {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            time.year, time.month, time.day, time.hour, time.minute, time.second
        )
    }

    pub fn init_calibrator(&self) {
        #[cfg(feature = "opencv")]
        {
            self.stabilizer.params.write().is_calibrator = true;
            *self.stabilizer.lens_calibrator.write() = Some(LensCalibrator::new());
            self.stabilizer.set_smoothing_method(2); // Plain 3D
            self.stabilizer.set_smoothing_param("time_constant", 2.0);
            self.stabilizer.set_max_zoom(0.0, 0);
            self.stabilizer.set_adaptive_zoom(0.0);
        }
    }

    fn start_autocalibrate(
        &mut self,
        max_points: usize,
        every_nth_frame: usize,
        iterations: usize,
        max_sharpness: f64,
        custom_timestamp_ms: f64,
        no_marker: bool,
    ) {
        #[cfg(feature = "opencv")]
        {
            rendering::clear_log();

            self.calib_in_progress = true;
            self.calib_in_progress_changed();
            self.calib_progress(0.0, 0.0, 0, 0, 0, 0.0);

            let stab = self.stabilizer.clone();

            let (
                fps,
                frame_count,
                trim_ranges_ms,
                trim_ratio,
                org_size,
                input_horizontal_stretch,
                input_vertical_stretch,
            ) = {
                let params = stab.params.read();
                let lens = stab.lens.read();
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
                (
                    params.fps,
                    params.frame_count,
                    params
                        .trim_ranges
                        .iter()
                        .map(|x| (x.0 * params.duration_ms, x.1 * params.duration_ms))
                        .collect(),
                    params.get_trim_ratio(),
                    params.size,
                    input_horizontal_stretch,
                    input_vertical_stretch,
                )
            };

            let is_forced = custom_timestamp_ms > -0.5;
            let ranges = if is_forced {
                vec![(custom_timestamp_ms - 1.0, custom_timestamp_ms + 1.0)]
            } else {
                trim_ranges_ms
            };

            let cal = stab.lens_calibrator.clone();
            if max_points > 0 {
                let mut lock = cal.write();
                let cal = lock.as_mut().unwrap();
                let saved: BTreeMap<i32, core::calibration::Detected> = {
                    let lock = cal.image_points.read();
                    cal.forced_frames
                        .iter()
                        .filter_map(|f| Some((*f, lock.get(f)?.clone())))
                        .collect()
                };
                *cal.image_points.write() = saved;
                cal.max_images = max_points;
                cal.iterations = iterations;
                cal.max_sharpness = max_sharpness;
            }

            let progress = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                |this, (ready, total, good, rms, sharpness): (usize, usize, usize, f64, f64)| {
                    this.calib_in_progress = ready < total;
                    this.calib_in_progress_changed();
                    this.calib_progress(
                        ready as f64 / total as f64,
                        rms,
                        ready,
                        total,
                        good,
                        sharpness,
                    );
                    if rms > 0.0 {
                        this.update_calib_model();
                    }
                },
            );
            let err = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                |this, (msg, mut arg): (String, String)| {
                    arg.push_str("\n\n");
                    arg.push_str(&rendering::get_log());

                    this.error(QString::from(msg), QString::from(arg), QString::default());

                    this.calib_in_progress = false;
                    this.calib_in_progress_changed();
                },
            );

            self.cancel_flag.store(false, SeqCst);
            let cancel_flag = self.cancel_flag.clone();

            let total = ((frame_count as f64 * trim_ratio) / every_nth_frame as f64) as usize;
            let total_read = Arc::new(AtomicUsize::new(0));
            let processed = Arc::new(AtomicUsize::new(0));

            let processing_resolution = self.processing_resolution;

            let input_file = stab.input_file.read().clone();
            core::run_threaded(move || {
                let mut decoder_options = ffmpeg_next::Dictionary::new();
                if input_file.image_sequence_fps > 0.0 {
                    let fps = rendering::fps_to_rational(input_file.image_sequence_fps);
                    decoder_options.set(
                        "framerate",
                        &format!("{}/{}", fps.numerator(), fps.denominator()),
                    );
                }
                if input_file.image_sequence_start > 0 {
                    decoder_options.set(
                        "start_number",
                        &format!("{}", input_file.image_sequence_start),
                    );
                }
                if processing_resolution > 0 {
                    decoder_options.set(
                        "scale",
                        &format!(
                            "{}x{}",
                            (processing_resolution * 16) / 9,
                            processing_resolution
                        ),
                    );
                }

                ::log::debug!("Decoder options: {:?}", decoder_options);
                let gpu_decoding = stab.gpu_decoding.load(SeqCst);
                match VideoProcessor::from_file(
                    &input_file.url,
                    gpu_decoding,
                    0,
                    Some(decoder_options),
                ) {
                    Ok(mut proc) => {
                        let progress = progress.clone();
                        let err2 = err.clone();
                        let cal = cal.clone();
                        let total_read = total_read.clone();
                        let processed = processed.clone();
                        let cancel_flag2 = cancel_flag.clone();

                        proc.on_frame(
                            move |timestamp_us,
                                  input_frame,
                                  _output_frame,
                                  converter,
                                  _rate_control| {
                                let frame =
                                    core::frame_at_timestamp(timestamp_us as f64 / 1000.0, fps);

                                if is_forced && total_read.load(SeqCst) > 0 {
                                    return Ok(());
                                }

                                if (frame % every_nth_frame as i32) == 0 {
                                    let mut width =
                                        (input_frame.width() as f64 * input_horizontal_stretch)
                                            .round() as u32;
                                    let mut height =
                                        (input_frame.height() as f64 * input_vertical_stretch)
                                            .round() as u32;
                                    let org_size = (
                                        (org_size.0 as f64 * input_horizontal_stretch).round()
                                            as u32,
                                        (org_size.1 as f64 * input_vertical_stretch).round() as u32,
                                    );
                                    let mut pt_scale = 1.0;
                                    if processing_resolution > 0
                                        && height > processing_resolution as u32
                                    {
                                        pt_scale = height as f32 / processing_resolution as f32;
                                        width = (width as f32 / pt_scale).round() as u32;
                                        height = (height as f32 / pt_scale).round() as u32;
                                    }
                                    match converter.scale(
                                        input_frame,
                                        ffmpeg_next::format::Pixel::GRAY8,
                                        width,
                                        height,
                                    ) {
                                        Ok(mut small_frame) => {
                                            let (width, height, stride, pixels) = (
                                                small_frame.plane_width(0),
                                                small_frame.plane_height(0),
                                                small_frame.stride(0),
                                                small_frame.data_mut(0),
                                            );

                                            total_read.fetch_add(1, SeqCst);
                                            let mut lock = cal.write();
                                            let cal = lock.as_mut().unwrap();
                                            if is_forced {
                                                cal.forced_frames.insert(frame);
                                            }
                                            cal.no_marker = no_marker;

                                            let (w, h) = org_size;
                                            if w > 0 && h > 0 {
                                                pt_scale = h as f32 / height as f32;
                                            }
                                            cal.feed_frame(
                                                timestamp_us,
                                                frame,
                                                (width, height),
                                                org_size,
                                                stride,
                                                pt_scale,
                                                pixels,
                                                cancel_flag2.clone(),
                                                total,
                                                processed.clone(),
                                                progress.clone(),
                                            );
                                        }
                                        Err(e) => err2((
                                            "An error occured: %1".to_string(),
                                            e.to_string(),
                                        )),
                                    }
                                }
                                Ok(())
                            },
                        );
                        if let Err(e) = proc.start_decoder_only(ranges, cancel_flag.clone()) {
                            err(("An error occured: %1".to_string(), e.to_string()));
                        }
                    }
                    Err(error) => {
                        err(("An error occured: %1".to_string(), error.to_string()));
                    }
                }
                // Don't lock the UI trying to draw chessboards while we calibrate
                stab.params.write().is_calibrator = false;

                while processed.load(SeqCst) < total_read.load(SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }

                let mut lock = cal.write();
                let cal = lock.as_mut().unwrap();
                if let Err(e) = cal.calibrate(is_forced) {
                    err(("An error occured: %1".to_string(), format!("{:?}", e)));
                } else {
                    if cal.rms < 100.0 {
                        stab.lens.write().set_from_calibrator(cal);
                    }
                    ::log::debug!(
                        "rms: {}, used_frames: {:?}, camera_matrix: {}, coefficients: {}",
                        cal.rms,
                        cal.used_points.keys(),
                        cal.k,
                        cal.d
                    );
                }

                let good = cal.image_points.read().len();
                progress((
                    total,
                    total,
                    good,
                    cal.rms,
                    *cal.sum_sharpness.read() / good.max(1) as f64,
                ));

                stab.params.write().is_calibrator = true;
            });
        }
    }

    fn update_calib_model(&mut self) {
        #[cfg(feature = "opencv")]
        {
            let cal = self.stabilizer.lens_calibrator.clone();

            let used_points = cal
                .read()
                .as_ref()
                .map(|x| x.used_points.clone())
                .unwrap_or_default();

            self.calib_model = RefCell::new(
                used_points
                    .values()
                    .map(|v| CalibrationItem {
                        timestamp_us: v.timestamp_us,
                        sharpness: v.avg_sharpness,
                        is_forced: v.is_forced,
                    })
                    .collect(),
            );

            util::qt_queued_callback(QPointer::from(self as &Self), |this, _| {
                this.calib_model_updated();
            })(());
        }
    }

    fn add_calibration_point(&mut self, timestamp_us: i64, no_marker: bool) {
        self.start_autocalibrate(0, 1, 1, 1000.0, timestamp_us as f64 / 1000.0, no_marker);
    }
    fn remove_calibration_point(&mut self, timestamp_us: i64) {
        #[cfg(feature = "opencv")]
        {
            let cal = self.stabilizer.lens_calibrator.clone();
            let mut rms = 0.0;
            {
                let mut lock = cal.write();
                let cal = lock.as_mut().unwrap();
                let mut frame_to_remove = None;
                for x in &cal.used_points {
                    if x.1.timestamp_us == timestamp_us {
                        frame_to_remove = Some(*x.0);
                        break;
                    }
                }
                if let Some(f) = frame_to_remove {
                    cal.forced_frames.remove(&f);
                    cal.used_points.remove(&f);
                }
                if cal.calibrate(true).is_ok() {
                    rms = cal.rms;
                    self.stabilizer.lens.write().set_from_calibrator(cal);
                    ::log::debug!(
                        "rms: {}, used_frames: {:?}, camera_matrix: {}, coefficients: {}",
                        cal.rms,
                        cal.used_points.keys(),
                        cal.k,
                        cal.d
                    );
                }
            }
            self.update_calib_model();
            if rms > 0.0 {
                self.calib_progress(1.0, rms, 1, 1, 1, 0.0);
            }
        }
    }

    fn export_lens_profile_filename(&self, info: QJsonObject) -> QString {
        let info_json = info.to_json().to_string();

        if let Ok(mut profile) = core::lens_profile::LensProfile::from_json(&info_json) {
            #[cfg(feature = "opencv")]
            if let Some(ref cal) = *self.stabilizer.lens_calibrator.read() {
                profile.set_from_calibrator(cal);
            }
            let name = profile
                .get_name()
                .replace([':', '|', '*', ':'], "_")
                .replace(['<', '"', '>', '/', '\\'], "");
            return QString::from(format!("{}.json", name));
        }
        QString::default()
    }

    fn export_lens_profile(&mut self, url: QUrl, info: QJsonObject, upload: bool) {
        let url = util::qurl_to_encoded(url);
        let info_json = info.to_json().to_string();

        match core::lens_profile::LensProfile::from_json(&info_json) {
            Ok(mut profile) => {
                #[cfg(feature = "opencv")]
                if let Some(ref cal) = *self.stabilizer.lens_calibrator.read() {
                    profile.set_from_calibrator(cal);
                }

                match profile.save_to_file(&url) {
                    Ok(json) => {
                        ::log::debug!("Lens profile json: {}", json);
                        if upload {
                            core::run_threaded(move || {
                                if let Ok(Ok(body)) =
                                    crate::network::post("https://api.gyroflow.xyz/upload_profile")
                                        .header("Content-Type", "application/json; charset=utf-8")
                                        .send(&json)
                                        .map(|x| x.into_body().read_to_string())
                                {
                                    ::log::debug!("Lens profile uploaded: {}", body.as_str());
                                }
                            });
                        }
                    }
                    Err(e) => {
                        self.error(
                            QString::from("An error occured: %1"),
                            QString::from(format!("{:?}", e)),
                            QString::default(),
                        );
                    }
                }
            }
            Err(e) => {
                self.error(
                    QString::from("An error occured: %1"),
                    QString::from(format!("{:?}", e)),
                    QString::default(),
                );
            }
        }
    }

    fn load_profiles(&self, reload_from_disk: bool) {
        let loaded = util::qt_queued_callback_mut(QPointer::from(self as &Self), |this, _: ()| {
            this.all_profiles_loaded();
        });
        let db = self.stabilizer.lens_profile_db.clone();
        core::run_threaded(move || {
            if reload_from_disk {
                let mut new_db = core::lens_profile_database::LensProfileDatabase::default();
                new_db.load_all();
                // Important! Disable `fetch_profiles_from_github` before running these functions
                // new_db.list_all_metadata();
                // new_db.process_adjusted_metadata();

                db.write().set_from_db(new_db);
            }

            db.write().prepare_list_for_ui();

            loaded(());
        });
    }

    fn search_lens_profile(
        &self,
        text: QString,
        favorites: QVariantList,
        aspect_ratio: i32,
        aspect_ratio_swapped: i32,
    ) {
        let finished = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, profiles: QVariantList| {
                this.search_lens_profile_finished(profiles);
            },
        );
        let db = self.stabilizer.lens_profile_db.clone();
        let text = text.to_string();
        let favorites = HashSet::<String>::from_iter(
            favorites.into_iter().map(|x| x.to_qbytearray().to_string()),
        );
        core::run_threaded(move || {
            let profiles = db
                .read()
                .search(&text, &favorites, aspect_ratio, aspect_ratio_swapped)
                .into_iter()
                .map(
                    |(name, file, crc, official, rating, aspect_ratio, _author)| {
                        let mut list = QVariantList::from_iter(
                            [QString::from(name), QString::from(file), QString::from(crc)]
                                .into_iter(),
                        );
                        list.push(official.into());
                        list.push(rating.into());
                        list.push(aspect_ratio.into());
                        list
                    },
                )
                .collect();

            finished(profiles);
        });
    }

    #[allow(unreachable_code)]
    fn fetch_profiles_from_github(&self) {
        use crate::core::lens_profile_database::LensProfileDatabase;

        if LensProfileDatabase::get_path().join("noupdate").exists() {
            ::log::info!("Skipping lens profile updates.");
            return;
        }

        let update = util::qt_queued_callback_mut(QPointer::from(self as &Self), |this, _| {
            this.lens_profiles_updated(true);
        });

        let current_version = self.stabilizer.lens_profile_db.read().version;

        let db_path = LensProfileDatabase::get_path().join("profiles.cbor.gz");
        if db_path.exists()
            || gyroflow_core::settings::data_dir()
                .join("lens_profiles")
                .exists()
        {
            core::run_threaded(move || {
                if let Ok(Ok(body)) =
                    crate::network::get(
                        "https://api.github.com/repos/gyroflow/lens_profiles/releases",
                    )
                    .call()
                    .map(|x| x.into_body().read_to_string())
                {
                    (|| -> Option<()> {
                        let v: Vec<serde_json::Value> = serde_json::from_str(&body).ok()?;
                        if let Some(obj) = v.first() {
                            let obj = obj.as_object()?;
                            if let Ok(tag) = obj
                                .get("tag_name")?
                                .as_str()?
                                .trim_start_matches("v")
                                .parse::<u32>()
                            {
                                if tag > current_version {
                                    ::log::info!(
                                        "Updating lens profile database from v{current_version} to v{tag}."
                                    );
                                    if let Some(download_url) =
                                        obj["assets"][0]["browser_download_url"].as_str()
                                    {
                                        if let Ok(mut content) = crate::network::get(download_url)
                                            .call()
                                            .map(|x| x.into_body().into_reader())
                                        {
                                            let mut updated = false;
                                            if db_path.exists() {
                                                if let Ok(mut file) =
                                                    std::fs::File::create(&db_path)
                                                {
                                                    if std::io::copy(&mut content, &mut file)
                                                        .is_ok()
                                                    {
                                                        updated = true;
                                                        update(());
                                                    }
                                                }
                                            }
                                            if !updated {
                                                if let Ok(mut file) = std::fs::File::create(
                                                    gyroflow_core::settings::data_dir()
                                                        .join("lens_profiles")
                                                        .join("profiles.cbor.gz"),
                                                ) {
                                                    if std::io::copy(&mut content, &mut file)
                                                        .is_ok()
                                                    {
                                                        update(());
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Some(())
                    }());
                }
            });
        }
    }

    fn rate_profile(&self, name: QString, json: QString, checksum: QString, is_good: bool) {
        core::run_threaded(move || {
            let mut url = url::Url::parse(&format!(
                "https://api.gyroflow.xyz/rate?good={}&checksum={}",
                is_good, checksum
            ))
            .unwrap();
            url.query_pairs_mut()
                .append_pair("filename", &name.to_string());

            if let Ok(Ok(body)) = crate::network::post(url.to_string())
                .header("Content-Type", "application/json; charset=utf-8")
                .send(&json.to_string())
                .map(|x| x.into_body().read_to_string())
            {
                ::log::debug!("Lens profile rated: {}", body.as_str());
            }
        });
    }
    fn request_profile_ratings(&self) {
        let update = util::qt_queued_callback_mut(QPointer::from(self as &Self), |this, _| {
            this.lens_profiles_updated(false);
        });
        let db = self.stabilizer.lens_profile_db.clone();
        core::run_threaded(move || {
            if let Ok(Ok(body)) =
                crate::network::get("https://api.gyroflow.xyz/rate?get_ratings=1")
                    .call()
                    .map(|x| x.into_body().read_to_string())
            {
                db.write().set_profile_ratings(body.as_str());
                update(());
            }
        });
    }

    fn list_gpu_devices(&self) {
        let finished =
            util::qt_queued_callback(QPointer::from(self as &Self), |this, list: Vec<String>| {
                // Cache first GPU descriptor for feedback meta (Phase 4).
                // Submission's Meta::collect() reads this; without it gpu="?".
                if let Some(first) = list.first() {
                    crate::feedback::meta::set_gpu(first.clone());
                }
                this.gpu_list_loaded(util::serde_json_to_qt_array(&serde_json::json!(list)))
            });
        self.stabilizer.list_gpu_devices(finished);
    }
    fn set_rendering_gpu_type_from_name(&self, name: String) {
        rendering::set_gpu_type_from_name(&name);
    }

    fn export_preset(
        &self,
        url: QUrl,
        content: QJsonObject,
        save_type: QString,
        preset_name: QString,
    ) -> QString {
        let save_type = save_type.to_string();
        let mut url = util::qurl_to_encoded(url);
        if url.is_empty() {
            let path = gyroflow_core::settings::data_dir().join("lens_profiles");
            match save_type.as_ref() {
                "lens" => {
                    url = filesystem::path_to_url(
                        path.join(&format!("{preset_name}.gyroflow"))
                            .to_str()
                            .unwrap_or_default(),
                    )
                }
                "default" => {
                    url = filesystem::path_to_url(
                        path.join("default.gyroflow").to_str().unwrap_or_default(),
                    )
                }
                _ => {
                    ::log::error!("Unknown save_type: {save_type}");
                }
            }
        }
        let contents = content.to_json_pretty();
        if let Err(e) = filesystem::write(&url, contents.to_slice()) {
            self.error(
                QString::from("An error occured: %1"),
                QString::from(e.to_string()),
                QString::default(),
            );
        }
        QString::from(filesystem::display_url(&url))
    }

    fn export_full_metadata(&self, url: QUrl, gyro_url: QUrl) {
        let result = || -> Result<(), core::GyroflowCoreError> {
            let contents = gyroflow_core::gyro_export::export_full_metadata(
                &util::qurl_to_encoded(gyro_url),
                &self.stabilizer,
            )?;
            Ok(filesystem::write(
                &util::qurl_to_encoded(url),
                contents.as_bytes(),
            )?)
        };
        if let Err(e) = result() {
            self.error(
                QString::from("An error occured: %1"),
                QString::from(e.to_string()),
                QString::default(),
            );
        }
    }
    fn export_parsed_metadata(&self, url: QUrl) {
        if let Ok(contents) =
            serde_json::to_string_pretty(&self.stabilizer.gyro.read().file_metadata)
        {
            if let Err(e) = filesystem::write(&util::qurl_to_encoded(url), contents.as_bytes()) {
                self.error(
                    QString::from("An error occured: %1"),
                    QString::from(e.to_string()),
                    QString::default(),
                );
            }
        }
    }
    fn export_gyro_data(&self, url: QUrl, fields: QJsonObject) {
        let url = util::qurl_to_encoded(url);
        let filename = filesystem::get_filename(&url).to_ascii_lowercase();

        let contents = gyroflow_core::gyro_export::export_gyro_data(
            &filename,
            fields.to_json().to_str().unwrap(),
            &self.stabilizer,
        );
        if let Err(e) = filesystem::write(&url, contents.as_bytes()) {
            self.error(
                QString::from("An error occured: %1"),
                QString::from(e.to_string()),
                QString::default(),
            );
        }
    }

    fn set_keyframe(&self, typ: String, timestamp_us: i64, value: f64) {
        if let Ok(kf) = KeyframeType::from_str(&typ) {
            self.stabilizer.set_keyframe(&kf, timestamp_us, value);
            self.keyframes_changed();
            self.request_recompute();
            self.chart_data_changed();
        }
    }
    fn set_keyframe_easing(&self, typ: String, timestamp_us: i64, easing: String) {
        if let Ok(kf) = KeyframeType::from_str(&typ) {
            if let Ok(e) = Easing::from_str(&easing) {
                self.stabilizer.set_keyframe_easing(&kf, timestamp_us, e);
                self.keyframes_changed();
                self.request_recompute();
                self.chart_data_changed();
            }
        }
    }
    fn keyframe_easing(&self, typ: String, timestamp_us: i64) -> String {
        if let Ok(kf) = KeyframeType::from_str(&typ) {
            if let Some(e) = self.stabilizer.keyframe_easing(&kf, timestamp_us) {
                return e.to_string();
            }
        }
        String::new()
    }
    fn set_keyframe_timestamp(&self, typ: String, id: u32, timestamp_us: i64) {
        if let Ok(kf) = KeyframeType::from_str(&typ) {
            self.stabilizer
                .set_keyframe_timestamp(&kf, id, timestamp_us);
            self.keyframes_changed();
            self.request_recompute();
            self.chart_data_changed();
        }
    }
    fn keyframe_id(&self, typ: String, timestamp_us: i64) -> u32 {
        if let Ok(kf) = KeyframeType::from_str(&typ) {
            if let Some(e) = self.stabilizer.keyframe_id(&kf, timestamp_us) {
                return e;
            }
        }
        0
    }
    fn remove_keyframe(&self, typ: String, timestamp_us: i64) {
        if let Ok(kf) = KeyframeType::from_str(&typ) {
            self.stabilizer.remove_keyframe(&kf, timestamp_us);
            self.keyframes_changed();
            self.request_recompute();
            self.chart_data_changed();
        }
    }
    fn clear_keyframes_type(&self, typ: String) {
        if let Ok(kf) = KeyframeType::from_str(&typ) {
            self.stabilizer.clear_keyframes_type(&kf);
            self.keyframes_changed();
            self.request_recompute();
            self.chart_data_changed();
        }
    }
    fn keyframe_value_at_video_timestamp(&self, typ: String, timestamp_ms: f64) -> QJSValue {
        if let Ok(typ) = KeyframeType::from_str(&typ) {
            if let Some(v) = self
                .stabilizer
                .keyframe_value_at_video_timestamp(&typ, timestamp_ms)
            {
                return QJSValue::from(v);
            }
        }
        QJSValue::default()
    }
    fn is_keyframed(&self, typ: String) -> bool {
        if let Ok(typ) = KeyframeType::from_str(&typ) {
            return self.stabilizer.is_keyframed(&typ);
        }
        false
    }

    fn update_keyframe_values(&self, mut timestamp_ms: f64) {
        let keyframes = self.stabilizer.keyframes.read();
        timestamp_ms /= keyframes.timestamp_scale.unwrap_or(1.0);
        for kf in keyframes.get_all_keys() {
            if let Some(v) = keyframes.value_at_video_timestamp(kf, timestamp_ms) {
                self.keyframe_value_updated(kf.to_string(), v);
            }
        }
    }

    fn get_neuflow_available(&self) -> bool {
        #[cfg(feature = "neuflow-ort")]
        {
            return crate::core::neuflow::is_available();
        }
        #[cfg(feature = "neuflow-burn")]
        {
            return crate::core::neuflow_burn::is_available();
        }
        #[cfg(not(any(feature = "neuflow-ort", feature = "neuflow-burn")))]
        {
            false
        }
    }

    fn has_gravity_vectors(&self) -> bool {
        self.stabilizer
            .gyro
            .read()
            .file_metadata
            .read()
            .gravity_vectors
            .as_ref()
            .map(|v| !v.is_empty())
            .unwrap_or_default()
    }
    fn gyro_has_raw_imu(&self) -> bool {
        !self
            .stabilizer
            .gyro
            .read()
            .file_metadata
            .read()
            .raw_imu
            .is_empty()
    }
    fn gyro_has_quaternions(&self) -> bool {
        !self
            .stabilizer
            .gyro
            .read()
            .file_metadata
            .read()
            .quaternions
            .is_empty()
    }
    fn gyro_has_accurate_timestamps(&self) -> bool {
        self.stabilizer
            .gyro
            .read()
            .file_metadata
            .read()
            .has_accurate_timestamps
    }

    fn check_external_sdk(&self, filename: QString) -> bool {
        let filename = filename.to_string();
        crate::external_sdk::requires_install(&filename)
    }
    fn install_external_sdk(&self, url: QString) {
        let url = url.to_string();
        let filename = if url == "ffmpeg_gpl" {
            url.clone()
        } else {
            filesystem::get_filename(&url)
        };
        let started = std::time::Instant::now();
        let telemetry_once = Arc::new(AtomicBool::new(false));
        let telemetry_once2 = telemetry_once.clone();
        let progress = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, (percent, sdk_name, error_string): (f64, &'static str, String)| {
                let error_for_ui = QString::from(error_string.clone());
                this.external_sdk_progress(
                    percent,
                    QString::from(sdk_name),
                    error_for_ui,
                    QString::from(url.clone()),
                );
                if percent >= 1.0 && !telemetry_once2.swap(true, SeqCst) {
                    crate::distribution::report_download_event(
                        "sdk_download_result",
                        sdk_name,
                        "",
                        &url,
                        if error_string.is_empty() {
                            "success"
                        } else {
                            "fail"
                        },
                        started.elapsed().as_millis(),
                        0,
                        &error_string,
                    );
                }
            },
        );
        let sdkbase = crate::distribution::download_source_base();
        crate::external_sdk::install(&filename, &sdkbase, progress);
    }

    fn mp4_merge(&self, file_list: QStringList, output_folder: QUrl, output_filename: QString) {
        let output_folder = util::qurl_to_encoded(output_folder);
        let output_filename = output_filename.to_string();
        let output_url = filesystem::get_file_url(&output_folder, &output_filename, true);

        let mut file_list: Vec<String> = file_list.into_iter().map(QString::to_string).collect();
        file_list.sort_by(|a, b| human_sort::compare(a, b));

        ::log::debug!("Merging files: {:?}", &file_list);
        if file_list.len() < 2 {
            self.mp4_merge_progress(1.0, QString::from("Not enough files!"), QString::default());
            return;
        }
        if output_url.is_empty() {
            self.mp4_merge_progress(1.0, QString::from("Empty output path!"), QString::default());
            return;
        }
        let first_url = file_list.first().unwrap().clone();
        let out = output_url.clone();
        let progress = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, (percent, error_string): (f64, String)| {
                this.mp4_merge_progress(
                    percent,
                    QString::from(error_string),
                    QString::from(out.as_str()),
                );
            },
        );
        core::run_threaded(move || {
            let mut vidinfo = None;
            for x in &file_list {
                match rendering::ffmpeg_processor::FfmpegProcessor::get_video_info(x) {
                    Ok(x) => {
                        if vidinfo.is_none() {
                            vidinfo = Some(x);
                            continue;
                        }
                        if let Some(vidinfo) = &vidinfo {
                            if vidinfo.width != x.width
                                || vidinfo.height != x.height
                                || (vidinfo.fps * 100.0).round() as i32
                                    != (x.fps * 100.0).round() as i32
                                || vidinfo.rotation != x.rotation
                            {
                                progress((
                                    1.0,
                                    format!(
                                        "Video metadata mismatch: {}x{}@{:.2} {}° vs {}x{}@{:.2} {}°",
                                        vidinfo.width,
                                        vidinfo.height,
                                        vidinfo.fps,
                                        vidinfo.rotation,
                                        x.width,
                                        x.height,
                                        x.fps,
                                        x.rotation
                                    ),
                                ));
                                return;
                            }
                        }
                    }
                    Err(e) => {
                        progress((1.0, format!("Failed to read file metadata: {x}: {e:?}")));
                        return;
                    }
                }
            }

            let mut opened = Vec::with_capacity(file_list.len());
            for x in &file_list {
                match filesystem::open_file(&x, false, false) {
                    Ok(x) => {
                        opened.push(x);
                    }
                    Err(e) => {
                        progress((1.0, format!("Failed to open file: {x}: {e:?}")));
                        return;
                    }
                }
            }
            let mut file_references: Vec<(&mut std::fs::File, usize)> = opened
                .iter_mut()
                .map(|x| {
                    let s = x.size;
                    (x.get_file(), s)
                })
                .collect();
            let mut opened_output = match filesystem::open_file(&output_url, true, true) {
                Ok(x) => x,
                Err(e) => {
                    progress((1.0, format!("Failed to create file: {output_url}: {e:?}")));
                    return;
                }
            };
            let res =
                mp4_merge::join_file_streams(&mut file_references, opened_output.get_file(), |p| {
                    progress((p.min(0.9999), String::default()))
                });
            match res {
                Ok(_) => {
                    if let Err(e) = Self::merge_gcsv(&file_list, &output_folder, &output_filename) {
                        ::log::error!("Failed to merge .gcsv files: {:?}", e);
                    }

                    crate::util::update_file_times(&output_url, &first_url, None);

                    progress((1.0, String::default()))
                }
                Err(e) => progress((1.0, e.to_string())),
            }
        });
    }
    fn merge_gcsv(
        file_list: &[String],
        output_folder: &str,
        output_filename: &str,
    ) -> Result<(), gyroflow_core::GyroflowCoreError> {
        use std::io::{BufRead, Seek, SeekFrom, Write};
        let mut last_diff = 0.0;
        let mut last_timestamp = 0.0;
        let mut add_timestamp = 0.0;
        let mut output_gcsv = None;
        let mut first_file = true;
        let mut sync_points = Vec::new();
        let mut time_scale = 0.001; // default to millisecond
        let mut headers_end_position = None;

        let do_add_timestamp = || -> Option<bool> {
            let mut last_timestamp = None;
            for x in file_list {
                let gcsv_name =
                    filesystem::filename_with_extension(&filesystem::get_filename(x), "gcsv");
                let gcsv_url =
                    filesystem::get_file_url(&filesystem::get_folder(x), &gcsv_name, false);
                let mut file = filesystem::open_file(&gcsv_url, false, false).ok()?;
                let mut is_data = false;
                for line in std::io::BufReader::new(file.get_file()).lines() {
                    let line = line.ok()?;
                    if !is_data {
                        if line.starts_with("t,") || line.starts_with("time,") {
                            is_data = true;
                            continue;
                        }
                    } else if line.contains(',') {
                        if let Ok(timestamp) = line.split(',').next().unwrap().parse::<f64>() {
                            if let Some(last_timestamp) = last_timestamp {
                                // If timestamp is not continuous
                                if timestamp < last_timestamp {
                                    return Some(true);
                                }
                            }
                            last_timestamp = Some(timestamp);
                        }
                    }
                }
            }
            Some(false)
        }()
        .unwrap_or(true);

        for x in file_list {
            let filename = filesystem::get_filename(x);
            let folder = filesystem::get_folder(x);
            let gcsv_name = filesystem::filename_with_extension(&filename, "gcsv");
            let gcsv_url = filesystem::get_file_url(&folder, &gcsv_name, false);
            if filesystem::exists_in_folder(&folder, &gcsv_name) {
                let mut is_data = false;
                if let Ok(mut file) = filesystem::open_file(&gcsv_url, false, false) {
                    if output_gcsv.is_none() {
                        let out_url = filesystem::get_file_url(
                            &output_folder,
                            &filesystem::filename_with_extension(output_filename, "gcsv"),
                            true,
                        );
                        output_gcsv = Some(filesystem::open_file(&out_url, true, true)?);
                    }
                    for (i, line) in std::io::BufReader::new(file.get_file()).lines().enumerate() {
                        let mut line = line?;
                        if i == 0
                            && !line.contains("GYROFLOW IMU LOG")
                            && !line.contains("CAMERA IMU LOG")
                        {
                            return Ok(()); // not a .gcsv file
                        }
                        if !is_data {
                            if line.starts_with("tscale,") {
                                if let Ok(ts) = line.strip_prefix("tscale,").unwrap().parse::<f64>()
                                {
                                    time_scale = ts;
                                }
                            }
                            if line.starts_with("t,") || line.starts_with("time,") {
                                is_data = true;
                                if !first_file {
                                    sync_points.push((add_timestamp * time_scale - 0.5) * 1000.0);
                                    sync_points.push((add_timestamp * time_scale + 0.5) * 1000.0);
                                    sync_points.push((add_timestamp * time_scale + 1.0) * 1000.0);
                                    sync_points.push((add_timestamp * time_scale + 2.0) * 1000.0);
                                    sync_points.push((add_timestamp * time_scale + 2.5) * 1000.0);
                                    continue;
                                } else {
                                    headers_end_position = Some(
                                        output_gcsv
                                            .as_mut()
                                            .unwrap()
                                            .get_file()
                                            .stream_position()?,
                                    );
                                    writeln!(
                                        output_gcsv.as_mut().unwrap().get_file(),
                                        "additional_sync_points,{}",
                                        " ".repeat(1024)
                                    )?; // 1kb of placeholder spaces
                                }
                            }
                        } else if line.contains(',') {
                            if let Ok(timestamp) = line.split(',').next().unwrap().parse::<f64>() {
                                last_diff = timestamp - last_timestamp;
                                last_timestamp = timestamp;
                                let new_timestamp = timestamp + add_timestamp;
                                line = [new_timestamp.to_string()]
                                    .into_iter()
                                    .chain(line.split(',').skip(1).map(str::to_string))
                                    .join(",");
                            }
                        }
                        if first_file || is_data {
                            writeln!(output_gcsv.as_mut().unwrap().get_file(), "{}", line)?;
                        }
                    }
                }
                if do_add_timestamp {
                    add_timestamp += last_timestamp + last_diff;
                }
                last_timestamp = 0.0;
            }
            first_file = false;
        }
        if !sync_points.is_empty() && output_gcsv.is_some() && headers_end_position.is_some() {
            let output_gcsv = &mut output_gcsv.as_mut().unwrap().get_file();
            output_gcsv.seek(SeekFrom::Start(headers_end_position.unwrap()))?;
            write!(
                output_gcsv,
                "additional_sync_points,{}",
                sync_points
                    .into_iter()
                    .map(|x| format!("{:.3}", x))
                    .join(";")
            )?;
        }
        Ok(())
    }

    // ---------- REDline conversion ----------
    fn find_redline(&self) -> QString {
        QString::from(crate::external_sdk::r3d::REDSdk::find_redline())
    }
    // ---------- REDline conversion ----------

    fn play_sound(&self, typ: String) {
        core::run_threaded(move || {
            use std::io::{Cursor, Error, ErrorKind};
            let _ = (|| -> Result<(), Box<dyn std::error::Error>> {
                let source = match typ.as_ref() {
                    "success" => include_bytes!("../resources/success.ogg") as &[u8],
                    "error" => include_bytes!("../resources/error.ogg") as &[u8],
                    _ => return Err(Error::new(ErrorKind::Other, "").into()),
                };
                {
                    let stream_handle = rodio::DeviceSinkBuilder::open_default_sink()?;
                    let sink = rodio::Player::connect_new(stream_handle.mixer());
                    sink.append(rodio::Decoder::new(Cursor::new(source))?);
                    sink.sleep_until_end();
                }
                Ok(())
            })();
        });
    }

    fn has_per_frame_lens_data(&self) -> bool {
        let gyro = self.stabilizer.gyro.read();
        let md = gyro.file_metadata.read();
        md.camera_stab_data.len() > 1
            || md.lens_params.len() > 1
            || md.lens_positions.len() > 1
            || md.mesh_correction.len() > 1
    }
    fn export_stmap(&self, folder_url: QUrl, per_frame: bool) {
        let folder_url = util::qurl_to_encoded(folder_url);
        let frame_count = self.stabilizer.params.read().frame_count;

        let progress = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, (ready, total): (usize, usize)| {
                this.stmap_progress(ready as f64 / total as f64, ready, total);
            },
        );
        let err =
            util::qt_queued_callback_mut(QPointer::from(self as &Self), |this, msg: String| {
                this.error(
                    QString::from("An error occured: %1"),
                    QString::from(msg),
                    QString::default(),
                );
            });

        self.cancel_flag.store(false, SeqCst);
        let cancel_flag = self.cancel_flag.clone();

        let total = if per_frame { frame_count } else { 1 };
        let mut processed = 0;

        let stab = self.stabilizer.clone();
        {
            let params = stab.params.read();
            if params.size.0 <= 0 || params.size.1 <= 0 {
                self.error(
                    QString::from("An error occured: %1"),
                    QString::from("Video is not loaded"),
                    QString::default(),
                );
                return;
            }
        }

        core::run_threaded(move || {
            progress((0, total));
            for (fname_base, frame, dist, undist) in
                gyroflow_core::stmap::generate_stmaps(&stab, per_frame)
            {
                if let Err(e) = filesystem::write(
                    &filesystem::get_file_url(
                        &folder_url,
                        &format!("{fname_base}-undistort-{frame}.exr"),
                        true,
                    ),
                    &undist,
                ) {
                    return err(e.to_string());
                }
                if let Err(e) = filesystem::write(
                    &filesystem::get_file_url(
                        &folder_url,
                        &format!("{fname_base}-redistort-{frame}.exr"),
                        true,
                    ),
                    &dist,
                ) {
                    return err(e.to_string());
                }

                processed += 1;
                progress((processed, total));

                if cancel_flag.load(SeqCst) {
                    break;
                }
            }
            progress((total, total));
        });
    }

    fn is_nle_installed(&self) -> bool {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            crate::nle_plugins::is_nle_installed("openfx")
                || crate::nle_plugins::is_nle_installed("adobe")
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            false
        }
    }
    fn nle_plugins(&self, command: QString, typ: QString) -> QString {
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        {
            let typ = typ.to_string();
            let command = command.to_string();
            ::log::info!("[nle controller] enter command={command:?} typ={typ:?}");
            let result = match command.as_ref() {
                "install" | "latest_version" | "status" => {
                    let command2 = QString::from(command.clone());
                    let signal = util::qt_queued_callback_mut(
                        QPointer::from(self as &Self),
                        move |this, r: String| {
                            ::log::info!(
                                "[nle controller] signal -> QML command={:?} result_len={} preview={:?}",
                                command2.to_string(),
                                r.len(),
                                r.chars().take(200).collect::<String>()
                            );
                            this.nle_plugins_result(command2.clone(), QString::from(r));
                        },
                    );
                    core::run_threaded(move || {
                        let started = std::time::Instant::now();
                        let plugins_base = crate::distribution::plugin_source_base();
                        ::log::info!(
                            "[nle controller thread] dispatch command={command:?} typ={typ:?} plugins_base={plugins_base:?}"
                        );
                        let result = match command.as_ref() {
                            "install" => crate::nle_plugins::install(&typ, plugins_base),
                            "latest_version" => {
                                crate::nle_plugins::latest_version().ok_or(std::io::Error::new(
                                    std::io::ErrorKind::Other,
                                    "Failed to check version",
                                ))
                            }
                            "status" => crate::nle_plugins::status_json(&typ),
                            _ => Err(std::io::Error::new(
                                std::io::ErrorKind::Other,
                                format!("Unknown command {command}"),
                            )),
                        };
                        ::log::info!(
                            "[nle controller thread] command={command:?} typ={typ:?} elapsed_ms={} result_kind={}",
                            started.elapsed().as_millis(),
                            match &result {
                                Ok(_) => "Ok",
                                Err(_) => "Err",
                            }
                        );
                        match result {
                            Ok(r) => {
                                if command == "install" {
                                    crate::distribution::report_download_event(
                                        "plugin_download_result",
                                        &typ,
                                        "",
                                        "plugins",
                                        "success",
                                        started.elapsed().as_millis(),
                                        0,
                                        "",
                                    );
                                }
                                signal(r)
                            }
                            Err(e) => {
                                if command == "install" {
                                    crate::distribution::report_download_event(
                                        "plugin_download_result",
                                        &typ,
                                        "",
                                        "plugins",
                                        "fail",
                                        started.elapsed().as_millis(),
                                        0,
                                        &e.to_string(),
                                    );
                                }
                                signal(format!("An error occured: {e:?}"))
                            }
                        }
                    });
                    Ok(String::new())
                }
                "detect" => crate::nle_plugins::detect(&typ),
                "is_nle_installed" => Ok(format!("{}", crate::nle_plugins::is_nle_installed(&typ))),
                _ => Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Unknown command {command}"),
                )),
            };
            match result {
                Ok(r) => QString::from(r),
                Err(e) => QString::from(format!("An error occured: {e:?}")),
            }
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            QString::default()
        }
    }

    // Utilities
    fn get_username(&self) -> QString {
        let realname = whoami::realname().unwrap_or_default();
        QString::from(if realname.is_empty() {
            whoami::username().unwrap_or_default()
        } else {
            realname
        })
    }
    fn image_to_b64(&self, img: QImage) -> QString {
        util::image_to_b64(img)
    }
    fn copy_to_clipboard(&self, text: QString) {
        util::copy_to_clipboard(text)
    }
    fn data_folder(&self) -> QUrl {
        QUrl::from(QString::from(gyroflow_core::filesystem::path_to_url(
            gyroflow_core::settings::data_dir()
                .to_str()
                .unwrap_or_default(),
        )))
    }

    // ----- Feedback bridge (Phase 4) ----------------------------------

    fn build_feedback_inputs(&self) -> crate::feedback::PackageInputs {
        let logs = crate::logger::log_dir().map(|p| p.to_path_buf());
        let mut inputs = crate::feedback::packager::PackageInputs::default();
        if let Some(dir) = logs {
            let cur = dir.join("gyroflow.log");
            if cur.exists() { inputs.current_log = Some(cur); }
            for i in 1..=4 {
                let p = dir.join(format!("gyroflow.log.{i}"));
                if p.exists() { inputs.history_logs.push(p); }
            }
            let inc = dir.join("gyroflow-incidents.log");
            if inc.exists() { inputs.incidents_log = Some(inc); }
            inputs.crash_zips = crate::feedback::pending_crash_zips();
        }
        let data_dir = gyroflow_core::settings::data_dir();
        let lens = data_dir.join("lens.json");
        if lens.exists() { inputs.lens_file = Some(lens); }
        let queue = data_dir.join("render_queue.json");
        if queue.exists() { inputs.queue_file = Some(queue); }
        let settings = data_dir.join("settings.json");
        if settings.exists() { inputs.settings_file = Some(settings); }
        // project_file: omitted in Phase 4 baseline; controller exposes a
        // hook later if user wants the current .gyroflow snapshot wired.
        inputs
    }

    fn build_feedback_options(json: &str) -> crate::feedback::packager::PackageOptions {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(json) {
            let g = |k: &str, default: bool| v.get(k).and_then(|x| x.as_bool()).unwrap_or(default);
            crate::feedback::packager::PackageOptions {
                include_current_log:    g("current_log",    true),
                include_history_logs:   g("history_logs",   true),
                include_incidents:      g("incidents",      true),
                include_project:        g("project",        true),
                include_video_meta:     g("video_meta",     true),
                include_lens:           g("lens",           true),
                include_queue_settings: g("queue_settings", true),
                include_system_info:    g("system_info",    true),
                include_crashes:        g("crashes",        true),
            }
        } else {
            crate::feedback::packager::PackageOptions::default()
        }
    }

    #[allow(non_snake_case)]
    fn estimateFeedbackSize(&self, options_json: QString) -> i64 {
        let inputs = self.build_feedback_inputs();
        let opts = Self::build_feedback_options(&options_json.to_string());
        crate::feedback::packager::estimate_size(&inputs, &opts) as i64
    }

    #[allow(non_snake_case)]
    fn submitFeedback(&mut self, description: QString, email: QString, options_json: QString) {
        let inputs = self.build_feedback_inputs();
        let opts = Self::build_feedback_options(&options_json.to_string());
        let summary = description.to_string();
        let email = email.to_string();
        let meta = crate::feedback::meta::Meta::collect();

        // Channel for state events. Forwarded to QML via Qt-queued callback.
        let (tx, rx) = std::sync::mpsc::channel::<crate::feedback::FeedbackJobState>();
        let progress_cb = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            |this, st: crate::feedback::FeedbackJobState| {
                use crate::feedback::FeedbackJobState as S;
                match st {
                    S::Packaging       => this.feedbackProgress(QString::from("packaging"), 0),
                    S::RequestingToken => this.feedbackProgress(QString::from("requesting_token"), 5),
                    S::Uploading{pct}  => this.feedbackProgress(QString::from("uploading"), pct as i32),
                    S::Confirming      => this.feedbackProgress(QString::from("confirming"), 96),
                    S::Cleanup         => this.feedbackProgress(QString::from("cleanup"), 99),
                    S::Done{id}        => this.feedbackCompleted(true, QString::from(id), QString::default()),
                    S::Failed{reason, ..} => this.feedbackCompleted(false, QString::default(), QString::from(reason)),
                }
            },
        );
        // Forwarder thread: receive Sync events from worker → invoke Qt callback.
        std::thread::Builder::new().name("feedback-progress".into()).spawn(move || {
            while let Ok(st) = rx.recv() {
                progress_cb(st);
            }
        }).ok();

        // Worker thread: actual submit pipeline.
        std::thread::Builder::new().name("feedback-submit".into()).spawn(move || {
            let _ = crate::feedback::uploader::submit(crate::feedback::uploader::SubmitArgs {
                inputs, options: opts, summary, email, meta, events: tx,
            });
        }).ok();
    }

    #[allow(non_snake_case)]
    fn scanCrashCheckpoints(&mut self) {
        let count = crate::feedback::crash_pickup::scan().len() as i32;
        if count > 0 {
            self.crashCheckpointFound(count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_log_scheme_classifies_mobile_and_local_urls() {
        assert_eq!(video_log_scheme("content://media/external/video/media/42"), "content");
        assert_eq!(video_log_scheme("file:///sdcard/DCIM/clip.mp4"), "file");
        assert_eq!(video_log_scheme("C:/Users/Jhe/Videos/clip.mp4"), "path");
        assert_eq!(video_log_scheme(""), "empty");
    }

    #[test]
    fn video_log_decoder_label_keeps_values_coarse() {
        assert_eq!(video_log_decoder_label(""), "default");
        assert_eq!(video_log_decoder_label("FFmpeg:avformat_options=start_number=1"), "FFmpeg");
        assert_eq!(video_log_decoder_label("BRAW:gpu=no:scale=1920x1080"), "BRAW");
        assert_eq!(video_log_decoder_label("R3D:gpu=auto:scale=1920x1080"), "R3D");
    }

    #[test]
    fn qml_video_rs_pins_raw_preview_pipeline_fixes() {
        // Pins the qml-video-rs fork at the commit that contains the R3D/NEV
        // preview fix: Play immediately re-anchors to the last rendered frame
        // without a 500 ms timer, old R3D players are torn down off the GUI
        // thread, callbacks are bound to their originating player, and loaded
        // setup is de-duplicated. Reverting drops the fix and can reintroduce
        // R3D Play stalls or clip-switch freezes.
        const PINNED_REV: &str = "ea32009d086684a5a8c2e4e924a1e89e056df022";
        let lock_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.lock");
        let lock = std::fs::read_to_string(&lock_path).expect("read Cargo.lock");
        let expected = format!(
            "source = \"git+https://github.com/NiYien/qml-video-rs?rev={hash}#{hash}\"",
            hash = PINNED_REV,
        );

        assert!(
            lock.contains(&expected),
            "qml-video-rs must stay pinned to the NiYien fork commit that contains the R3D/NEV preview re-anchor and teardown isolation fix."
        );
    }
}

#[derive(Default, QObject)]
pub struct Filesystem {
    base: qt_base_class!(trait QObject),

    exists_in_folder: qt_method!(fn(&self, folder: QUrl, filename: QString) -> bool),
    can_create_file: qt_method!(fn(&self, folder: QUrl, filename: QString) -> bool),
    exists: qt_method!(fn(&self, url: QUrl) -> bool),
    is_dir: qt_method!(fn(&self, url: QUrl) -> bool),
    get_filename: qt_method!(fn(&self, url: QUrl) -> QString),
    get_folder: qt_method!(fn(&self, url: QUrl) -> QString),
    filename_with_extension: qt_method!(fn(&self, filename: QString, ext: QString) -> QString),
    filename_with_suffix: qt_method!(fn(&self, filename: QString, suffix: QString) -> QString),
    open_file_externally: qt_method!(fn(&self, url: QUrl)),
    path_to_url: qt_method!(fn(&self, path: QString) -> QUrl),
    get_file_url: qt_method!(fn(&self, folder: QUrl, filename: String, can_create: bool) -> QUrl),
    url_to_path: qt_method!(fn(&self, url: QUrl) -> QString),
    display_url: qt_method!(fn(&self, url: QUrl) -> QString),
    display_folder_filename: qt_method!(fn(&self, folder: QUrl, filename: QString) -> QString),
    catch_url_open: qt_method!(fn(&self, url: QUrl)),
    catch_urls_open: qt_method!(fn(&self, urls: QStringList)),
    remove_file: qt_method!(fn(&self, url: QUrl)),
    folder_access_granted: qt_method!(fn(&self, url: QUrl)),
    move_to_trash: qt_method!(fn(&self, url: QUrl)),
    save_allowed_folders: qt_method!(fn(&self)),
    restore_allowed_folders: qt_method!(fn(&self)),
    get_next_file_url: qt_method!(fn(&self, current_url: QUrl, index: i32) -> QUrl),
    url_opened: qt_signal!(url: QUrl),
    urls_opened: qt_signal!(urls: QStringList),
}
impl Filesystem {
    fn exists_in_folder(&self, folder: QUrl, filename: QString) -> bool {
        filesystem::exists_in_folder(&util::qurl_to_encoded(folder), &filename.to_string())
    }
    fn can_create_file(&self, folder: QUrl, filename: QString) -> bool {
        filesystem::can_create_file(&util::qurl_to_encoded(folder), &filename.to_string())
    }
    fn exists(&self, url: QUrl) -> bool {
        filesystem::exists(&util::qurl_to_encoded(url))
    }
    fn is_dir(&self, url: QUrl) -> bool {
        filesystem::is_dir(&util::qurl_to_encoded(url))
    }
    fn get_filename(&self, url: QUrl) -> QString {
        QString::from(filesystem::get_filename(&util::qurl_to_encoded(url)))
    }
    fn get_folder(&self, url: QUrl) -> QString {
        QString::from(filesystem::get_folder(&util::qurl_to_encoded(url)))
    }
    fn filename_with_extension(&self, filename: QString, ext: QString) -> QString {
        QString::from(filesystem::filename_with_extension(
            &filename.to_string(),
            &ext.to_string(),
        ))
    }
    fn filename_with_suffix(&self, filename: QString, suffix: QString) -> QString {
        QString::from(filesystem::filename_with_suffix(
            &filename.to_string(),
            &suffix.to_string(),
        ))
    }
    fn open_file_externally(&self, url: QUrl) {
        util::open_file_externally(url);
    }
    fn path_to_url(&self, path: QString) -> QUrl {
        QUrl::from(QString::from(filesystem::path_to_url(&path.to_string())))
    }
    fn get_file_url(&self, folder: QUrl, filename: String, can_create: bool) -> QUrl {
        QUrl::from(QString::from(filesystem::get_file_url(
            &util::qurl_to_encoded(folder),
            &filename,
            can_create,
        )))
    }
    fn url_to_path(&self, url: QUrl) -> QString {
        QString::from(filesystem::url_to_path(&util::qurl_to_encoded(url)))
    }
    fn display_url(&self, url: QUrl) -> QString {
        QString::from(filesystem::display_url(&util::qurl_to_encoded(url)))
    }
    fn display_folder_filename(&self, folder: QUrl, filename: QString) -> QString {
        QString::from(filesystem::display_folder_filename(
            &util::qurl_to_encoded(folder),
            &filename.to_string(),
        ))
    }
    fn catch_url_open(&self, url: QUrl) {
        util::dispatch_url_event(url.clone());
        self.url_opened(url);
    }
    fn catch_urls_open(&self, urls: QStringList) {
        // Multi-URL path used by Android's SAF picker bridge for multi-select.
        // Emits urls_opened so QML can route the whole list (e.g. to the render
        // queue batch loader) instead of collapsing to a single file.
        self.urls_opened(urls);
    }
    fn remove_file(&self, url: QUrl) {
        let _ = filesystem::remove_file(&util::qurl_to_encoded(url));
    }
    fn folder_access_granted(&self, url: QUrl) {
        filesystem::folder_access_granted(&util::qurl_to_encoded(url));
    }
    fn save_allowed_folders(&self) {
        let list = filesystem::get_allowed_folders();
        if !list.is_empty() {
            gyroflow_core::settings::set("allowedUrls", list.into());
        }
    }
    fn restore_allowed_folders(&self) {
        if let Ok(saved) = serde_json::from_value::<Vec<String>>(gyroflow_core::settings::get(
            "allowedUrls",
            Default::default(),
        )) {
            if !saved.is_empty() {
                filesystem::restore_allowed_folders(&saved);
            }
        }
    }

    fn move_to_trash(&self, url: QUrl) {
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let url = QString::from(url).to_string();
            if let Err(e) = filesystem::remove_file(&url) {
                ::log::error!("Failed to remove file: {e:?}");
            }
        }
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        {
            let path = filesystem::url_to_path(&util::qurl_to_encoded(url));
            ::log::info!("Moving file to trash: {path}");
            match trash::delete(path) {
                Ok(_) => {}
                Err(e) => ::log::error!("Failed to move file to trash: {e:?}"),
            }
        }
    }

    fn get_next_file_url(&self, current_url: QUrl, index: i32) -> QUrl {
        let current_url = util::qurl_to_encoded(current_url);

        let folder = filesystem::get_folder(&current_url);
        let filename = filesystem::get_filename(&current_url);

        let extensions = [
            "mp4", "mov", "mxf", "mkv", "webm", "insv", "braw", "r3d", "nev",
        ];

        let list: Vec<(String, String)> = filesystem::list_folder(&folder)
            .into_iter()
            .filter(|x| {
                let x = x.0.to_ascii_lowercase();
                extensions.iter().any(|ext| x.ends_with(ext))
            })
            .sorted_by(|a, b| human_sort::compare(&a.0, &b.0))
            .collect();

        if let Some(current_index) = list.iter().position(|x| x.0 == filename) {
            if let Some(next_entry) = list.get((current_index as i32 + index) as usize) {
                return QUrl::from(QString::from(next_entry.1.clone()));
            }
        }
        QUrl::default()
    }
}

/// Parse telemetry creation date string "yyyy:MM:dd HH:mm:ss" or "yyyy:MM:dd HH:mm:ss.SSS" to Unix milliseconds.
fn parse_creation_date_to_millis(date_str: &str) -> Option<i64> {
    let (base, subsec_ms) = if let Some(dot_pos) = date_str.rfind('.') {
        let subsec_str = &date_str[dot_pos + 1..];
        let ms: i64 = match subsec_str.len() {
            1 => subsec_str.parse::<i64>().ok()? * 100,
            2 => subsec_str.parse::<i64>().ok()? * 10,
            3 => subsec_str.parse::<i64>().ok()?,
            _ => subsec_str[..3].parse::<i64>().ok()?,
        };
        (&date_str[..dot_pos], ms)
    } else {
        (date_str, 0i64)
    };
    let naive = chrono::NaiveDateTime::parse_from_str(base, "%Y:%m:%d %H:%M:%S").ok()?;
    Some(naive.and_utc().timestamp_millis() + subsec_ms)
}
