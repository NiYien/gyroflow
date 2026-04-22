// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

use qmetaobject::*;

use crate::core::StabilizationManager;
use crate::{core, rendering, util};
use core::niyien_lens_presets;
use core::camera_identifier::CameraIdentifier;
use core::filesystem;
use core::gyro_source::GyroSource;
use core::lens_profile::LensProfile;
use core::stabilization_params::ReadoutDirection;
use parking_lot::{Mutex as ParkingMutex, RwLock};
use rayon::prelude::*;
use regex::Regex;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst},
    Arc,
};

#[derive(Default, Clone, SimpleListItem, Debug)]
pub struct RenderQueueItem {
    pub job_id: u32,
    pub input_file: QString,
    pub input_filename: QString,
    pub output_filename: QString,
    pub output_folder: QString,
    pub display_output_path: QString,
    pub export_settings: QString,
    pub thumbnail_url: QString,
    pub current_frame: u64,
    pub start_timestamp_frame: u64,
    pub total_frames: u64,
    pub start_timestamp: u64,
    pub start_timestamp2: u64,
    pub end_timestamp: u64,
    pub error_string: QString,
    pub processing_progress: f64,
    pub skip_reason: QString,

    frame_times: std::collections::VecDeque<(u64, u64)>,

    status: JobStatus,
}
impl RenderQueueItem {
    pub fn get_status(&self) -> &JobStatus {
        &self.status
    }
}

#[derive(Default, Clone, Debug, Eq, PartialEq)]
pub enum JobStatus {
    #[default]
    Queued,
    Rendering,
    Finished,
    Error,
    Skipped,
}
struct Job {
    queue_index: usize,
    render_options: RenderOptions,
    base_render_output_size: Option<(usize, usize)>,
    auto_rotate: bool,
    additional_data: String,
    cancel_flag: Arc<AtomicBool>,
    // [cancel-epoch] Monotonically bumped whenever a render is started, paused or stopped.
    // progress/err callbacks capture the epoch they were created under and early-return when
    // it no longer matches Job.render_epoch, killing stale cross-thread callbacks that would
    // otherwise mark the job Finished/Error during a fast Stop→Start restart.
    render_epoch: Arc<AtomicU64>,
    project_data: Option<String>,
    stab: Option<Arc<StabilizationManager>>,
    base_lens_metadata: Option<JobLensMetadataBackup>,
    lens_group_config_override: Option<JobLensGroupOverride>,
    lens_group_index: Option<usize>,
    // [T20] 保存 video_created_at，stab 释放后排序仍可用
    video_created_at: Option<i64>,
    original_video_rotation: f64,
    original_output_size: (usize, usize),
}

#[derive(Clone, Debug, Default)]
struct JobLensGroupOverride {
    configs: Vec<niyien_lens_presets::LensGroupConfig>,
    enabled_groups: Vec<bool>,
}
impl JobLensGroupOverride {
    fn is_group_enabled(&self, lens_index: usize) -> bool {
        self.enabled_groups
            .get(lens_index)
            .copied()
            .unwrap_or(false)
    }

    fn has_enabled_groups(&self) -> bool {
        self.enabled_groups.iter().any(|enabled| *enabled)
    }
}

#[derive(Clone, Debug)]
struct JobLensMetadataBackup {
    lens_params: BTreeMap<i64, core::gyro_source::LensParams>,
    lens_positions: BTreeMap<i64, f64>,
    lens_profile: Option<serde_json::Value>,
    unit_pixel_focal_length: Option<f64>,
    camera_identifier: Option<CameraIdentifier>,
    detected_source: Option<String>,
    frame_readout_time: Option<f64>,
    frame_readout_direction: ReadoutDirection,
}
impl JobLensMetadataBackup {
    fn from_metadata(md: &core::gyro_source::FileMetadata) -> Self {
        Self {
            lens_params: md.lens_params.clone(),
            lens_positions: md.lens_positions.clone(),
            lens_profile: md.lens_profile.clone(),
            unit_pixel_focal_length: md.unit_pixel_focal_length,
            camera_identifier: md.camera_identifier.clone(),
            detected_source: md.detected_source.clone(),
            frame_readout_time: md.frame_readout_time,
            frame_readout_direction: md.frame_readout_direction,
        }
    }

    fn apply_missing_to_metadata(&self, md: &mut core::gyro_source::FileMetadata) {
        if md.lens_params.is_empty() {
            md.lens_params = self.lens_params.clone();
        }
        if md.lens_positions.is_empty() {
            md.lens_positions = self.lens_positions.clone();
        }
        if md.lens_profile.is_none() {
            md.lens_profile = self.lens_profile.clone();
        }
        if md.unit_pixel_focal_length.is_none() {
            md.unit_pixel_focal_length = self.unit_pixel_focal_length;
        }

        let should_restore_camera_identifier = md
            .camera_identifier
            .as_ref()
            .map(|id| id.brand.trim().is_empty() || id.brand == "SenseFlow")
            .unwrap_or(true);
        if should_restore_camera_identifier {
            md.camera_identifier = self.camera_identifier.clone();
        }

        let should_restore_detected_source = md
            .detected_source
            .as_deref()
            .map(|source| source.trim().is_empty() || source.starts_with("SenseFlow"))
            .unwrap_or(true);
        if should_restore_detected_source {
            md.detected_source = self.detected_source.clone();
        }

        let should_restore_readout_time = md
            .frame_readout_time
            .map(|value| !value.is_finite())
            .unwrap_or(true);
        if should_restore_readout_time {
            md.frame_readout_time = self.frame_readout_time.filter(|value| value.is_finite());
            md.frame_readout_direction = self.frame_readout_direction;
        }
    }

    fn overwrite_metadata(&self, md: &mut core::gyro_source::FileMetadata) {
        md.lens_params = self.lens_params.clone();
        md.lens_positions = self.lens_positions.clone();
        md.lens_profile = self.lens_profile.clone();
        md.unit_pixel_focal_length = self.unit_pixel_focal_length;
        md.camera_identifier = self.camera_identifier.clone();
        md.detected_source = self.detected_source.clone();
        md.frame_readout_time = self.frame_readout_time;
        md.frame_readout_direction = self.frame_readout_direction;
    }
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RenderMetadata {
    pub comment: String,
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RenderOptions {
    pub codec: String,
    pub codec_options: String,
    pub output_folder: String,
    pub output_filename: String,
    pub output_width: usize,
    pub output_height: usize,
    pub input_filename: String,
    pub input_url: String,
    pub bitrate: f64,
    pub use_gpu: bool,
    pub audio: bool,
    pub pixel_format: String,

    // Advanced
    pub encoder_options: String,
    pub metadata: RenderMetadata,
    pub keyframe_distance: f64,
    pub preserve_other_tracks: bool,
    pub pad_with_black: bool,
    pub export_trims_separately: bool,
    pub audio_codec: String,
    pub interpolation: String,
}
impl RenderOptions {
    pub fn settings_string(&self, fps: f64) -> String {
        let codec_info = match self.codec.as_ref() {
            "H.264/AVC" | "H.265/HEVC" | "AV1" => {
                format!("{} {:.0} Mbps", self.codec, self.bitrate)
            }
            "DNxHD" => self.codec_options.clone(),
            "ProRes" => format!("{} {}", self.codec, self.codec_options),
            _ => self.codec.clone(),
        };

        format!(
            "{}x{} {:.3}fps | {}",
            self.output_width, self.output_height, fps, codec_info
        )
    }

    pub fn get_encoder_options_dict(&self) -> ffmpeg_next::Dictionary<'_> {
        let re = Regex::new(r#"-([^\s"]+)\s+("[^"]+"|[^\s"]+)"#).unwrap();

        let mut options = ffmpeg_next::Dictionary::new();
        for x in re.captures_iter(&self.encoder_options) {
            if let Some(k) = x.get(1) {
                if let Some(v) = x.get(2) {
                    let k = k.as_str();
                    let v = v.as_str().trim_matches('"');
                    options.set(k, v);
                }
            }
        }
        options
    }
    pub fn get_metadata_dict(&self) -> ffmpeg_next::Dictionary<'_> {
        let mut metadata = ffmpeg_next::Dictionary::new();
        metadata.set(
            "comment",
            format!(
                "Original filename: {}\n{}",
                self.input_filename, self.metadata.comment
            )
            .trim(),
        );
        metadata
    }
    pub fn update_from_json(&mut self, obj: &serde_json::Value) {
        if let serde_json::Value::Object(obj) = obj {
            if let Some(v) = obj.get("codec").and_then(|x| x.as_str()) {
                self.codec = v.to_string();
            }
            if let Some(v) = obj.get("codec_options").and_then(|x| x.as_str()) {
                self.codec_options = v.to_string();
            }
            if let Some(v) = obj.get("output_width").and_then(|x| x.as_u64()) {
                self.output_width = v as usize;
            }
            if let Some(v) = obj.get("output_height").and_then(|x| x.as_u64()) {
                self.output_height = v as usize;
            }
            if let Some(v) = obj.get("bitrate").and_then(|x| x.as_f64()) {
                self.bitrate = v;
            }
            if let Some(v) = obj.get("use_gpu").and_then(|x| x.as_bool()) {
                self.use_gpu = v;
            }
            if let Some(v) = obj.get("audio").and_then(|x| x.as_bool()) {
                self.audio = v;
            }
            if let Some(v) = obj.get("pixel_format").and_then(|x| x.as_str()) {
                self.pixel_format = v.to_string();
            }

            // Advanced
            if let Some(v) = obj.get("encoder_options").and_then(|x| x.as_str()) {
                self.encoder_options = v.to_string();
            }
            if let Some(v) = obj.get("keyframe_distance").and_then(|x| x.as_f64()) {
                self.keyframe_distance = v;
            }
            if let Some(v) = obj.get("preserve_other_tracks").and_then(|x| x.as_bool()) {
                self.preserve_other_tracks = v;
            }
            if let Some(v) = obj.get("pad_with_black").and_then(|x| x.as_bool()) {
                self.pad_with_black = v;
            }
            if let Some(v) = obj.get("export_trims_separately").and_then(|x| x.as_bool()) {
                self.export_trims_separately = v;
            }
            if let Some(v) = obj.get("audio_codec").and_then(|x| x.as_str()) {
                self.audio_codec = v.to_string();
            }
            if let Some(v) = obj.get("interpolation").and_then(|x| x.as_str()) {
                self.interpolation = v.to_string();
            }

            if let Some(v) = obj.get("metadata").and_then(|x| x.as_object()) {
                if let Some(s) = v.get("comment").and_then(|x| x.as_str()) {
                    self.metadata.comment = s.to_string();
                }
            }

            // Backwards compatibility
            if let Some(v) = obj.get("output_path").and_then(|x| x.as_str()) {
                let url = filesystem::path_to_url(v);
                let folder = filesystem::get_folder(&url);
                if !folder.is_empty() {
                    self.output_folder = folder;
                }
                let filename = filesystem::get_filename(&url);
                if !filename.is_empty() {
                    self.output_filename = filename;
                }
            }
            if let Some(v) = obj
                .get("output_folder")
                .and_then(|x| x.as_str())
                .filter(|x| !x.is_empty())
            {
                // If output_folder is a relative path, resolve it to an absolute path
                if !v.starts_with('/') && !v.contains(":/") && !v.contains(":\\") {
                    ::log::info!(
                        "Resolving relative url: {v}. Current url: {}",
                        self.input_url
                    );
                    let current_folder = filesystem::get_folder(&self.input_url);
                    let current_path = filesystem::url_to_path(&current_folder);
                    let mut new_folder = current_path.clone();
                    if !new_folder.ends_with('/') {
                        new_folder.push('/');
                    }
                    new_folder.push_str(v);
                    if !new_folder.ends_with('/') {
                        new_folder.push('/');
                    }
                    self.output_folder = filesystem::path_to_url(&new_folder);
                    ::log::info!("= {}", self.output_folder);
                } else {
                    self.output_folder = v.to_string();
                }
            }
            if let Some(v) = obj
                .get("output_filename")
                .and_then(|x| x.as_str())
                .filter(|x| !x.is_empty())
            {
                self.output_filename = v.to_string();
            }
        }
    }
}

#[derive(Default, Clone, Debug)]
struct GyroFileInfo {
    path: String,
    filename: String,
    created_at_ms: Option<i64>,
    duration_ms: Option<f64>,
    detected_source: Option<String>,
    parsed: bool,
    error: Option<String>,
    /// 缓存完整的 telemetry 数据，避免重复解析原始文件
    cached_metadata: Option<Arc<core::gyro_source::FileMetadata>>,
    /// 缓存不同时间区间的 telemetry 数据，避免大文件被整段重复解析
    cached_metadata_ranges: Vec<CachedGyroMetadataRange>,
}

#[derive(Clone, Debug)]
struct CachedGyroMetadataRange {
    range_ms: Option<(f64, f64)>,
    metadata: Arc<core::gyro_source::FileMetadata>,
}

fn denormalize_video_rotation_metadata(normalized_rotation: f64) -> i32 {
    let normalized = normalized_rotation.round() as i32;
    (360 - normalized).rem_euclid(360)
}

fn parse_job_ids_json(job_ids_json: &str) -> Vec<u32> {
    serde_json::from_str(job_ids_json).unwrap_or_default()
}

fn resolve_lens_group_focal_length(
    auto_focus_length_mm: Option<f64>,
    group_config: Option<&niyien_lens_presets::LensGroupConfig>,
) -> Option<(f64, niyien_lens_presets::FocalLengthSource)> {
    niyien_lens_presets::select_focal_length(auto_focus_length_mm, group_config)
}

fn effective_lens_group_configs(
    job: &Job,
    global_configs: &[niyien_lens_presets::LensGroupConfig],
) -> Vec<niyien_lens_presets::LensGroupConfig> {
    let mut configs = niyien_lens_presets::normalize_lens_group_configs(global_configs);
    if let Some(local_override) = job.lens_group_config_override.as_ref() {
        for lens_index in 0..niyien_lens_presets::LENS_GROUP_COUNT {
            if local_override.is_group_enabled(lens_index) {
                if let Some(config) = local_override.configs.get(lens_index) {
                    configs[lens_index] = config.clone();
                }
            }
        }
    }
    configs
}

fn effective_lens_group_config_for_group<'a>(
    job: &'a Job,
    global_configs: &'a [niyien_lens_presets::LensGroupConfig],
    lens_index: usize,
) -> Option<(&'a niyien_lens_presets::LensGroupConfig, bool)> {
    if let Some(local_override) = job.lens_group_config_override.as_ref() {
        if local_override.is_group_enabled(lens_index) {
            return local_override
                .configs
                .get(lens_index)
                .map(|config| (config, true));
        }
    }
    global_configs.get(lens_index).map(|config| (config, false))
}

fn metadata_snapshot_for_job(job: &Job) -> Option<core::gyro_source::FileMetadata> {
    let stab = job.stab.as_ref()?;
    let gyro = stab.gyro.read();
    let md = gyro.file_metadata.read();
    let mut snapshot = md.thin();
    if let Some(backup) = job.base_lens_metadata.as_ref() {
        backup.overwrite_metadata(&mut snapshot);
    }
    Some(snapshot)
}

fn build_job_lens_group_override(
    requested_configs: &[niyien_lens_presets::LensGroupConfig],
    global_configs: &[niyien_lens_presets::LensGroupConfig],
    existing_override: Option<&JobLensGroupOverride>,
) -> Option<JobLensGroupOverride> {
    let requested_configs = niyien_lens_presets::normalize_lens_group_configs(requested_configs);
    let global_configs = niyien_lens_presets::normalize_lens_group_configs(global_configs);
    let mut enabled_groups = vec![false; niyien_lens_presets::LENS_GROUP_COUNT];

    for lens_index in 0..niyien_lens_presets::LENS_GROUP_COUNT {
        let keep_existing_override = existing_override
            .map(|existing| {
                existing.is_group_enabled(lens_index)
                    && existing.configs.get(lens_index) == requested_configs.get(lens_index)
            })
            .unwrap_or(false);
        let differs_from_global =
            requested_configs.get(lens_index) != global_configs.get(lens_index);
        enabled_groups[lens_index] = keep_existing_override || differs_from_global;
    }

    let override_config = JobLensGroupOverride {
        configs: requested_configs,
        enabled_groups,
    };
    if override_config.has_enabled_groups() {
        Some(override_config)
    } else {
        None
    }
}

#[derive(Default, QObject)]
pub struct RenderQueue {
    base: qt_base_class!(trait QObject),

    pub queue: qt_property!(RefCell<SimpleListModel<RenderQueueItem>>; NOTIFY queue_changed),
    jobs: HashMap<u32, Job>,

    add: qt_method!(fn(&mut self, additional_data: String, thumbnail_url: QString) -> u32),
    remove: qt_method!(fn(&mut self, job_id: u32)),
    clear: qt_method!(fn(&mut self)),

    start: qt_method!(fn(&mut self)),
    pause: qt_method!(fn(&mut self)),
    resume: qt_method!(fn(&mut self)),
    stop: qt_method!(fn(&mut self)),

    render_job: qt_method!(fn(&mut self, job_id: u32)),
    cancel_job: qt_method!(fn(&self, job_id: u32)),
    reset_job: qt_method!(fn(&mut self, job_id: u32)),
    get_gyroflow_data: qt_method!(fn(&self, job_id: u32) -> QString),

    add_file:
        qt_method!(fn(&mut self, url: String, gyro_url: String, additional_data: String) -> u32),

    get_job_output_filename: qt_method!(fn(&self, job_id: u32) -> QString),
    get_job_output_folder: qt_method!(fn(&self, job_id: u32) -> QUrl),
    set_job_output_filename:
        qt_method!(fn(&mut self, job_id: u32, new_filename: QString, start: bool)),

    set_pixel_format: qt_method!(fn(&mut self, job_id: u32, format: String)),
    set_error_string: qt_method!(fn(&mut self, job_id: u32, err: QString)),
    set_processing_resolution: qt_method!(fn(&mut self, target_height: i32)),

    file_exists_in_folder: qt_method!(fn(&self, folder: QUrl, filename: QString) -> bool),
    move_item: qt_method!(fn(&mut self, job_id: u32, step: i32)),

    save_render_queue: qt_method!(fn(&self)),
    restore_render_queue: qt_method!(fn(&mut self, additional_data: String) -> bool),

    main_job_id: qt_property!(u32),
    editing_job_id: qt_property!(u32; NOTIFY queue_changed),

    pub start_timestamp: qt_property!(u64; NOTIFY progress_changed),
    pub end_timestamp: qt_property!(u64; NOTIFY progress_changed),
    current_frame: qt_property!(u64; READ get_current_frame NOTIFY progress_changed),
    total_frames: qt_property!(u64; READ get_total_frames NOTIFY queue_changed),
    pub status: qt_property!(QString; NOTIFY status_changed),
    pub auto_rotate: qt_property!(bool; NOTIFY auto_rotate_changed),
    pub simple_mode: qt_property!(bool; NOTIFY simple_mode_changed),

    pub progress_changed: qt_signal!(),
    pub queue_changed: qt_signal!(),
    pub status_changed: qt_signal!(),
    pub auto_rotate_changed: qt_signal!(),
    pub simple_mode_changed: qt_signal!(),

    pub render_progress: qt_signal!(job_id: u32, progress: f64, current_frame: usize, total_frames: usize, finished: bool, start_time: f64, is_conversion: bool),
    pub encoder_initialized: qt_signal!(job_id: u32, encoder_name: String),

    pub convert_format: qt_signal!(job_id: u32, format: QString, supported: QString, candidate: QString),
    pub error: qt_signal!(job_id: u32, text: QString, arg: QString, callback: QString),
    pub added: qt_signal!(job_id: u32),
    pub processing_done: qt_signal!(job_id: u32, by_preset: bool),
    pub processing_progress: qt_signal!(job_id: u32, progress: f64),

    get_prev_item_id: qt_method!(fn(&self, job_id: u32) -> u32),
    get_next_item_id: qt_method!(fn(&self, job_id: u32) -> u32),
    get_encoder_options: qt_method!(fn(&self, encoder: String) -> String),
    get_default_encoder: qt_method!(fn(&self, codec: String, gpu: bool) -> String),
    get_active_render_count: qt_method!(fn(&self) -> usize),

    apply_to_all: qt_method!(fn(&mut self, data: String, additional_data: String, to_job_id: u32)),

    pause_flag: Arc<AtomicBool>,

    pub default_suffix: qt_property!(QString),

    when_done: qt_property!(i32; WRITE set_when_done),

    parallel_renders: qt_property!(i32; WRITE set_parallel_renders),
    pub export_project: qt_property!(u32),
    pub export_metadata: Option<(usize, String, serde_json::Value)>,
    pub export_stmap: Option<(usize, String)>,
    pub overwrite_mode: qt_property!(u32),

    pub request_close: qt_signal!(),

    pub queue_finished: qt_signal!(),

    pub jobs_added: HashSet<u32>,

    paused_timestamp: Option<u64>,
    start_frame: u64,

    stabilizer: Arc<StabilizationManager>,

    processing_resolution: i32,

    // Batch gyro matching
    gyro_files: Vec<GyroFileInfo>,
    match_results: Option<core::gyro_match::BatchMatchResult>,
    pairing_mode_gyro_index: Option<usize>,
    // [queue-lifecycle T2] original_job_order 已废弃，不再保存/恢复原始顺序
    #[allow(dead_code)]
    original_job_order: Vec<u32>,
    manual_pairs: Vec<core::gyro_match::ManualCalibrationPair>,
    // [T22] 缓存每个 job 的 sameGyroAsPrev/Next，match 完成后一次性计算
    same_gyro_cache: HashMap<u32, (bool, bool)>, // job_id -> (sameAsPrev, sameAsNext)

    add_gyro_file: qt_method!(fn(&mut self, url: String)),
    add_gyro_folder: qt_method!(fn(&mut self, folder_url: String)),
    remove_gyro_file: qt_method!(fn(&mut self, index: usize)),
    clear_gyro_files: qt_method!(fn(&mut self)),
    get_gyro_file_count: qt_method!(fn(&self) -> usize),
    get_gyro_file_info_json: qt_method!(fn(&self, index: usize) -> QString),
    has_gyro_files: qt_method!(fn(&self) -> bool),
    batch_match_gyro: qt_method!(fn(&mut self)),
    apply_match_results: qt_method!(fn(&mut self)),
    reapply_batch_auto_rotate: qt_method!(fn(&mut self, job_ids_json: String)),
    reapply_lens_group_config: qt_method!(fn(&mut self)),
    reapply_selected_lens_group_config: qt_method!(fn(&mut self, job_ids_json: String)),
    get_selected_lens_group_status_json: qt_method!(fn(&self, job_ids_json: String) -> QString),
    get_selected_lens_group_config_json: qt_method!(fn(&self, job_ids_json: String) -> QString),
    set_selected_lens_group_config:
        qt_method!(fn(&mut self, job_ids_json: String, config_json: String)),
    clear_selected_lens_group_config:
        qt_method!(fn(&mut self, job_ids_json: String, lens_index: usize)),
    manual_set_calibration_pair: qt_method!(fn(&mut self, job_id: u32, gyro_index: usize)),
    get_manual_pair_gyro_index: qt_method!(fn(&self, job_id: u32) -> i32),
    unpair_video: qt_method!(fn(&mut self, job_id: u32)),
    get_match_status_json: qt_method!(fn(&self, job_id: u32) -> QString),
    get_adjacent_gyro_index: qt_method!(fn(&self, job_id: u32, offset: i32) -> i32),
    enter_pairing_mode: qt_method!(fn(&mut self, gyro_index: usize)),
    exit_pairing_mode: qt_method!(fn(&mut self)),
    is_in_pairing_mode: qt_method!(fn(&self) -> bool),
    sort_jobs_by_created_at: qt_method!(fn(&mut self)),
    restore_original_order: qt_method!(fn(&mut self)),
    has_match_results: qt_method!(fn(&self) -> bool),
    is_same_gyro_as_prev: qt_method!(fn(&self, job_id: u32) -> bool),
    is_same_gyro_as_next: qt_method!(fn(&self, job_id: u32) -> bool),
    // [T22] 缓存版：从 same_gyro_cache 读取，不实时查询
    get_cached_same_gyro_prev: qt_method!(fn(&self, job_id: u32) -> bool),
    get_cached_same_gyro_next: qt_method!(fn(&self, job_id: u32) -> bool),

    get_job_display_params: qt_method!(fn(&self, job_id: u32) -> QString),
    set_batch_auto_rotate: qt_method!(fn(&mut self, job_ids_json: String, enabled: bool)),
    batch_update_params: qt_method!(fn(&mut self, job_ids_json: String, params_json: String)),

    pub gyro_files_changed: qt_signal!(),
    pub match_results_changed: qt_signal!(),
    // [T22] 匹配+数据加载全部完成时触发（区别于 match_results_changed 可能在算法完成时就触发）
    pub match_apply_finished: qt_signal!(),
    pub pairing_mode_changed: qt_signal!(),
}

macro_rules! update_model {
    ($this:ident, $job_id:ident, $itm:ident $action:block) => {
        {
            if let Ok(mut q) = $this.queue.try_borrow_mut() {
                if let Some(job) = $this.jobs.get(&$job_id) {
                    if job.queue_index < q.row_count() as usize {
                        //let mut $itm = &mut q[job.queue_index];
                        let mut $itm = q[job.queue_index].clone();
                        $action
                        q.change_line(job.queue_index, $itm);
                        //q.data_changed(job.queue_index);
                    }
                }
            }
        }
    };
}

impl RenderQueue {
    pub fn new(stabilizer: Arc<StabilizationManager>) -> Self {
        Self {
            status: QString::from("stopped"),
            default_suffix: QString::from("_stabilized"),
            processing_resolution: 720,
            stabilizer,
            ..Default::default()
        }
    }

    pub fn set_processing_resolution(&mut self, target_height: i32) {
        self.processing_resolution = target_height;
    }
    pub fn get_stab_for_job(&self, job_id: u32) -> Option<Arc<StabilizationManager>> {
        self.jobs.get(&job_id)?.stab.clone()
    }

    pub fn get_total_frames(&self) -> u64 {
        self.queue
            .try_borrow()
            .map(|x| x.iter().map(|v| v.total_frames).sum::<u64>() - self.start_frame)
            .unwrap_or_default()
    }
    pub fn get_current_frame(&self) -> u64 {
        self.queue
            .try_borrow()
            .map(|x| x.iter().map(|v| v.current_frame).sum::<u64>() - self.start_frame)
            .unwrap_or_default()
    }

    pub fn set_pixel_format(&mut self, job_id: u32, format: String) {
        if let Some(job) = self.jobs.get_mut(&job_id) {
            if format == "cpu" {
                job.render_options.use_gpu = false;
            } else {
                job.render_options.pixel_format = format;
            }
        }
        update_model!(self, job_id, itm {
            itm.error_string = QString::default();
            itm.status = JobStatus::Queued;
        });
        if self.status.to_string() != "active" {
            self.start();
        }
    }

    pub fn set_job_output_filename(&mut self, job_id: u32, new_filename: QString, start: bool) {
        if let Some(job) = self.jobs.get_mut(&job_id) {
            job.render_options.output_filename = new_filename.to_string();
            if let Some(ref stab) = job.stab {
                job.project_data = Self::get_gyroflow_data_internal(
                    stab,
                    &job.additional_data,
                    &job.render_options,
                );
            }
        }
        update_model!(self, job_id, itm {
            itm.output_filename = new_filename;
            itm.display_output_path = QString::from(filesystem::display_folder_filename(&itm.output_folder.to_string(), &itm.output_filename.to_string()));
            itm.error_string = QString::default();
            itm.status = JobStatus::Queued;
        });
        if start && self.status.to_string() != "active" {
            self.start();
        }
    }

    pub fn move_item(&mut self, job_id: u32, step: i32) {
        if let Some(job) = self.jobs.get_mut(&job_id) {
            if let Ok(mut q) = self.queue.try_borrow_mut() {
                let old_index = job.queue_index;
                let new_index = ((old_index as i32) + step).max(0).min(q.row_count() - 1) as usize;
                let itm = q[old_index].clone();
                q.remove(old_index);
                q.insert(new_index, itm);

                // Update all indices
                for (i, v) in q.iter().enumerate() {
                    if let Some(job) = self.jobs.get_mut(&v.job_id) {
                        job.queue_index = i;
                    }
                }
            }
        }
        self.queue_changed();
    }

    pub fn set_error_string(&mut self, job_id: u32, err: QString) {
        update_model!(self, job_id, itm {
            itm.error_string = err;
            itm.status = JobStatus::Error;
        });
    }

    pub fn add(&mut self, additional_data: String, thumbnail_url: QString) -> u32 {
        let job_id = if self.editing_job_id > 0 {
            self.editing_job_id
        } else {
            fastrand::u32(1..2147483640)
        };
        if self.editing_job_id > 0 {
            self.editing_job_id = 0;
            self.queue_changed();
        }

        if let Ok(obj) =
            serde_json::from_str(&additional_data) as serde_json::Result<serde_json::Value>
        {
            if let Some(out) = obj.get("output") {
                if let Ok(mut render_options) =
                    serde_json::from_value(out.clone()) as serde_json::Result<RenderOptions>
                {
                    render_options.update_from_json(out);
                    let project_url = self.stabilizer.input_file.read().project_file_url.clone();
                    if let Some(project_url) = project_url {
                        // Save project file on disk
                        if let Err(e) = self.stabilizer.export_gyroflow_file(
                            &project_url,
                            core::GyroflowProjectType::WithGyroData,
                            &additional_data,
                        ) {
                            ::log::warn!("Failed to save project file: {}: {:?}", project_url, e);
                        }
                    }
                    let stab = self.stabilizer.get_cloned();

                    // If it's added from main UI, never do the additional autosync
                    if let Some(ref mut obj) = stab.lens.write().sync_settings {
                        obj.as_object_mut().and_then(|x| x.remove("do_autosync"));
                    }

                    self.add_internal(
                        job_id,
                        Arc::new(stab),
                        render_options,
                        additional_data,
                        thumbnail_url,
                    );
                }
            }
        }
        job_id
    }

    pub fn add_internal(
        &mut self,
        job_id: u32,
        stab: Arc<StabilizationManager>,
        mut render_options: RenderOptions,
        additional_data: String,
        thumbnail_url: QString,
    ) {
        let size = stab.params.read().size;
        stab.set_render_params(
            size,
            (render_options.output_width, render_options.output_height),
        );

        let params = stab.params.read();
        let trim_ratio = params.get_trim_ratio();
        let video_url = stab.input_file.read().url.clone();

        let editing = self.jobs.contains_key(&job_id);

        // [queue-batch-streamline T5] 输入视频去重：非编辑模式下跳过重复视频
        if !editing {
            let new_url_normalized = filesystem::url_to_path(&video_url);
            let q = self.queue.borrow();
            for itm in q.iter() {
                let existing_normalized = filesystem::url_to_path(&itm.input_file.to_string());
                if existing_normalized == new_url_normalized {
                    ::log::info!("[queue-batch-streamline T5] 跳过重复视频: {}", video_url);
                    return;
                }
            }
            drop(q);
        }

        if editing {
            update_model!(self, job_id, itm {
                itm.output_folder = QString::from(render_options.output_folder.as_str());
                itm.output_filename = QString::from(render_options.output_filename.as_str());
                itm.display_output_path = QString::from(filesystem::display_folder_filename(render_options.output_folder.as_str(), render_options.output_filename.as_str()));
                itm.export_settings = QString::from(render_options.settings_string(params.fps));
                itm.thumbnail_url = thumbnail_url;
                itm.current_frame = 0;
                itm.total_frames = (params.frame_count as f64 * trim_ratio).ceil() as u64;
                itm.start_timestamp = 0;
                itm.start_timestamp2 = 0;
                itm.start_timestamp_frame = 0;
                itm.end_timestamp = 0;
                itm.error_string = QString::default();
                itm.status = JobStatus::Queued;
                itm.frame_times.clear();
            });
        } else {
            let mut q = self.queue.borrow_mut();
            q.push(RenderQueueItem {
                job_id,
                input_file: QString::from(video_url.as_str()),
                input_filename: QString::from(filesystem::get_filename(&video_url)),
                output_folder: QString::from(render_options.output_folder.as_str()),
                output_filename: QString::from(render_options.output_filename.as_str()),
                display_output_path: QString::from(filesystem::display_folder_filename(
                    render_options.output_folder.as_str(),
                    render_options.output_filename.as_str(),
                )),
                export_settings: QString::from(render_options.settings_string(params.fps)),
                thumbnail_url,
                current_frame: 0,
                total_frames: (params.frame_count as f64 * trim_ratio).ceil() as u64,
                start_timestamp: 0,
                start_timestamp2: 0,
                start_timestamp_frame: 0,
                end_timestamp: 0,
                processing_progress: 0.0,
                error_string: QString::default(),
                skip_reason: QString::default(),
                frame_times: Default::default(),
                status: JobStatus::Queued,
            });
        }
        drop(params);

        let project_data =
            Self::get_gyroflow_data_internal(&stab, &additional_data, &render_options);

        render_options.input_url = stab.input_file.read().url.clone();
        render_options.input_filename = filesystem::get_filename(&stab.input_file.read().url);

        let base_lens_metadata = {
            let gyro = stab.gyro.read();
            let md = gyro.file_metadata.read();
            Some(JobLensMetadataBackup::from_metadata(&md))
        };
        let base_render_output_size = (render_options.output_width, render_options.output_height);
        let lens_group_index = {
            let gyro = stab.gyro.read();
            let md = gyro.file_metadata.read();
            niyien_lens_presets::extract_lens_index(&md.additional_data)
        };
        // [T20] 在 stab 释放前保存 video_created_at
        let video_created_at = stab.params.read().video_created_at;
        let original_video_rotation = stab.params.read().video_rotation;
        let original_output_size = (render_options.output_width, render_options.output_height);
        self.jobs.insert(
            job_id,
            Job {
                queue_index: 0,
                render_options,
                base_render_output_size: Some(base_render_output_size),
                auto_rotate: false,
                additional_data,
                cancel_flag: Default::default(),
                render_epoch: Default::default(),
                project_data,
                stab: Some(stab.clone()),
                base_lens_metadata,
                lens_group_config_override: None,
                lens_group_index,
                video_created_at,
                original_video_rotation,
                original_output_size,
            },
        );
        self.update_queue_indices();

        self.queue_changed();
        self.added(job_id);
    }

    pub fn get_job_output_folder(&self, job_id: u32) -> QUrl {
        let q = self.queue.borrow();
        if let Some(job) = self.jobs.get(&job_id) {
            if job.queue_index < q.row_count() as usize {
                return QUrl::from(q[job.queue_index].output_folder.clone());
            }
        }
        QUrl::default()
    }
    pub fn get_job_output_filename(&self, job_id: u32) -> QString {
        let q = self.queue.borrow();
        if let Some(job) = self.jobs.get(&job_id) {
            if job.queue_index < q.row_count() as usize {
                return q[job.queue_index].output_filename.clone();
            }
        }
        QString::default()
    }
    pub fn remove(&mut self, job_id: u32) {
        if let Some(job) = self.jobs.get(&job_id) {
            job.cancel_flag.store(true, SeqCst);
            self.queue.borrow_mut().remove(job.queue_index);
            if self.editing_job_id == job_id {
                self.editing_job_id = 0;
            }
            self.queue_changed();
        }
        self.jobs.remove(&job_id);
        self.update_queue_indices();

        if self.status.to_string() == "active" {
            self.start_frame = 0;
            self.start_timestamp = Self::current_timestamp();
            self.start_frame = self.get_current_frame();
            self.progress_changed();
        }
    }
    pub fn clear(&mut self) {
        let mut to_delete = Vec::new();
        for v in self.queue.borrow().iter() {
            if v.status != JobStatus::Rendering {
                to_delete.push(v.job_id);
            }
        }
        for job_id in to_delete {
            self.remove(job_id);
        }
    }
    fn update_queue_indices(&mut self) {
        for (i, v) in self.queue.borrow().iter().enumerate() {
            if let Some(job) = self.jobs.get_mut(&v.job_id) {
                job.queue_index = i;
            }
        }
    }
    fn current_timestamp() -> u64 {
        if let Ok(time) =
            std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH)
        {
            time.as_millis() as u64
        } else {
            0
        }
    }

    pub fn start(&mut self) {
        // No-op when paused. Previously this unconditionally cleared pause_flag,
        // so when multiple job-done callbacks hit start() concurrently, the first
        // call cleared the flag and subsequent calls saw paused=false and kicked
        // off the next job — the user-visible "pause stopped the current one but
        // others started anyway" bug. Explicit Resume goes through `resume()`.
        if self.pause_flag.load(SeqCst) {
            return;
        }

        for (_id, job) in self.jobs.iter() {
            job.cancel_flag.store(false, SeqCst);
        }

        self.status = QString::from("active");
        self.status_changed();

        if self.start_timestamp == 0 {
            self.start_frame = 0;
            self.start_timestamp = Self::current_timestamp();
            self.start_frame = self.get_current_frame();
            self.progress_changed();
        }

        loop {
            if self.get_active_render_count() >= self.parallel_renders.max(1) as usize {
                break;
            }

            let mut job_id = None;
            for v in self.queue.borrow().iter() {
                // [stop-restart] Queue selection only needs status==Queued + known frame count.
                // Previous tighter predicate (current_frame==0 && processing_progress∈{0,1}) was
                // necessary when Rendering→Queued reset also wiped those counters; now that reset
                // preserves them (see reset_rendering_jobs_to_queued), a Stopped job still has
                // current_frame>0 and must remain selectable. render_job has its own entry guard
                // against double-scheduling (status == Rendering/Finished/Skipped → return).
                if v.total_frames > 0 && v.status == JobStatus::Queued {
                    job_id = Some(v.job_id);
                    break;
                }
            }
            if let Some(job_id) = job_id {
                self.render_job(job_id);
            } else {
                if self.get_active_render_count() == 0 {
                    self.post_render_action();
                    self.queue_finished();

                    self.start_frame = 0;
                    self.start_timestamp = 0;
                    self.progress_changed();

                    self.status = QString::from("stopped");
                    self.status_changed();
                }
                break;
            }
        }
    }
    pub fn resume(&mut self) {
        // Explicit Resume: clear pause_flag, adjust timestamps, reset each
        // job's cancel_flag, then let start() schedule pending jobs normally.
        if !self.pause_flag.load(SeqCst) {
            return;
        }
        for (_id, job) in self.jobs.iter() {
            job.cancel_flag.store(false, SeqCst);
        }
        self.pause_flag.store(false, SeqCst);

        if let Some(paused_timestamp) = self.paused_timestamp.take() {
            let diff = Self::current_timestamp() - paused_timestamp;
            self.start_timestamp += diff;
            let mut q = self.queue.borrow_mut();
            for i in 0..q.row_count() as usize {
                let mut v = q[i].clone();
                if v.start_timestamp > 0 && v.current_frame < v.total_frames {
                    v.start_timestamp += diff;
                    v.frame_times.clear();
                    q.change_line(i, v);
                }
            }
        }

        self.start();
    }
    pub fn pause(&mut self) {
        self.pause_flag.store(true, SeqCst);
        self.paused_timestamp = Some(Self::current_timestamp());

        // The sync stage has no resumable checkpoint, so pausing must cancel any
        // in-flight autosync on every job — otherwise the UI appears frozen while
        // NeuFlow inference keeps running. resume() resets cancel_flag back to false.
        for (_id, job) in self.jobs.iter() {
            job.cancel_flag.store(true, SeqCst);
            // [cancel-epoch] Invalidate any in-flight progress/err callbacks; the new
            // render (after resume) will capture a fresh epoch.
            job.render_epoch.fetch_add(1, SeqCst);
        }

        // Proactively flip Rendering → Queued so a concurrent resume()/start()
        // can find the jobs to schedule. Otherwise the late callback races with
        // start()'s Queued scan and either side can lose.
        self.reset_rendering_jobs_to_queued();

        self.status = QString::from("paused");
        self.status_changed();
    }
    pub fn stop(&mut self) {
        self.pause_flag.store(false, SeqCst);
        for (_id, job) in self.jobs.iter() {
            job.cancel_flag.store(true, SeqCst);
            job.render_epoch.fetch_add(1, SeqCst);
        }

        self.reset_rendering_jobs_to_queued();

        self.status = QString::from("stopped");
        self.status_changed();
    }

    // Proactively flip every Rendering job back to Queued so a follow-up start() can
    // re-schedule them without waiting for the (now stale) render callback to land.
    //
    // We intentionally keep current_frame / processing_progress / frame_times /
    // timestamps intact. The follow-up render_job always starts ffmpeg encoding from
    // frame 0 (ffmpeg does not support partial resume), and the very first progress
    // callback will overwrite current_frame back to 0 — so the UI briefly shows the
    // prior progress before re-ticking. Clearing these fields here was rejected in
    // code review: it made pause/resume semantics lossy for Full mode's mainBtn.
    fn reset_rendering_jobs_to_queued(&self) {
        if let Ok(mut q) = self.queue.try_borrow_mut() {
            for i in 0..q.row_count() as usize {
                let mut v = q[i].clone();
                if v.status == JobStatus::Rendering {
                    v.status = JobStatus::Queued;
                    q.change_line(i, v);
                }
            }
        }
    }

    fn post_render_action(&self) {
        // If it was running for at least 1 minute
        if Self::current_timestamp() - self.start_timestamp > 60000 && self.when_done > 0 {
            self.request_close();

            #[cfg(not(any(target_os = "ios", target_os = "android")))]
            {
                fn system_shutdown(reboot: bool) {
                    #[cfg(target_os = "windows")]
                    {
                        let msg = util::tr(
                            "App",
                            &format!(
                                "Gyroflow will {} the computer in 60 seconds because all tasks have been completed.",
                                if reboot { "reboot" } else { "shut down" }
                            ),
                        );
                        let _ = if reboot {
                            system_shutdown::reboot_with_message(&msg, 60, false)
                        } else {
                            system_shutdown::shutdown_with_message(&msg, 60, false)
                        };
                    }

                    #[cfg(not(target_os = "windows"))]
                    let _ = if reboot {
                        system_shutdown::reboot()
                    } else {
                        system_shutdown::shutdown()
                    };
                }

                match self.when_done {
                    1 => {
                        system_shutdown(false);
                    }
                    2 => {
                        system_shutdown(true);
                    }
                    3 => {
                        let _ = system_shutdown::sleep();
                    }
                    4 => {
                        let _ = system_shutdown::hibernate();
                    }
                    5 => {
                        let _ = system_shutdown::logout();
                    }
                    _ => {}
                }
            }
        }
    }

    pub fn set_when_done(&mut self, v: i32) {
        self.when_done = v;
        #[cfg(target_os = "macos")]
        if v > 0 && v != 6 {
            let _ = system_shutdown::request_permission_dialog();
        }
    }
    pub fn get_active_render_count(&self) -> usize {
        self.queue
            .borrow()
            .iter()
            .filter(|v| v.total_frames > 0 && v.status == JobStatus::Rendering)
            .count()
    }
    pub fn get_pending_count(&self) -> usize {
        self.queue
            .borrow()
            .iter()
            .filter(|v| v.total_frames > 0 && v.status == JobStatus::Queued)
            .count()
    }
    pub fn set_parallel_renders(&mut self, v: i32) {
        self.parallel_renders = v;

        if self.status.to_string() == "active" {
            self.start();
        }
    }

    pub fn cancel_job(&self, job_id: u32) {
        if let Some(job) = self.jobs.get(&job_id) {
            job.cancel_flag.store(true, SeqCst);
        }
    }
    pub fn reset_job(&mut self, job_id: u32) {
        if let Some(job) = self.jobs.get(&job_id) {
            job.cancel_flag.store(false, SeqCst);
        }

        // Recreate StabilizationManager from project_data if it was released after rendering
        if self.jobs.get(&job_id).map_or(false, |j| j.stab.is_none()) {
            let project_data = self.jobs.get(&job_id).and_then(|j| j.project_data.clone());
            let render_options = self.jobs.get(&job_id).map(|j| j.render_options.clone());
            let lens_profile_db = self.stabilizer.lens_profile_db.clone();

            if let (Some(data), Some(opts)) = (project_data, render_options) {
                let stab = Arc::new(StabilizationManager {
                    lens_profile_db,
                    ..Default::default()
                });
                let mut is_preset = false;

                let result = if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&data) {
                    if let Some(project_file) = obj.get("project_file").and_then(|v| v.as_str()) {
                        stab.import_gyroflow_file(
                            project_file,
                            false,
                            |_| (),
                            Arc::new(AtomicBool::new(false)),
                            false,
                        )
                    } else {
                        stab.import_gyroflow_data(
                            data.as_bytes(),
                            false,
                            None,
                            |_| (),
                            Arc::new(AtomicBool::new(false)),
                            &mut is_preset,
                            false,
                        )
                    }
                } else {
                    stab.import_gyroflow_data(
                        data.as_bytes(),
                        false,
                        None,
                        |_| (),
                        Arc::new(AtomicBool::new(false)),
                        &mut is_preset,
                        false,
                    )
                };

                match result {
                    Ok(_) => {
                        stab.set_output_size(opts.output_width, opts.output_height);
                        if let Some(job) = self.jobs.get_mut(&job_id) {
                            job.stab = Some(stab);
                        }
                    }
                    Err(e) => {
                        ::log::error!(
                            "Failed to recreate StabilizationManager for job {}: {:?}",
                            job_id,
                            e
                        );
                        update_model!(self, job_id, itm {
                            itm.error_string = QString::from(format!("Failed to restore job state: {:?}", e));
                            itm.status = JobStatus::Error;
                        });
                        return;
                    }
                }
            }
        }

        update_model!(self, job_id, itm {
            itm.error_string = QString::default();
            itm.skip_reason = QString::default();
            itm.current_frame = 0;
            itm.frame_times.clear();
            itm.status = JobStatus::Queued;
        });
    }
    pub fn update_status(&mut self) {
        for v in self.queue.borrow().iter() {
            if v.total_frames > 0 && v.status == JobStatus::Rendering {
                self.status = QString::from("active");
                self.status_changed();
                return;
            }
        }

        self.status = QString::from("stopped");
        self.status_changed();
    }

    pub fn save_render_queue(&self) {
        // [queue-lifecycle T1] 不再持久化队列状态
    }

    pub fn restore_render_queue(&mut self, _additional_data: String) -> bool {
        // [queue-lifecycle T1] 不再从 settings 恢复历史队列
        false
    }

    fn get_gyroflow_data_internal(
        stab: &StabilizationManager,
        additional_data: &str,
        render_options: &RenderOptions,
    ) -> Option<String> {
        if let Some(url) = stab.input_file.read().project_file_url.as_ref() {
            if filesystem::exists(url) {
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                {
                    return Some(serde_json::json!({ "project_file": url, "project_file_bookmark": filesystem::apple::create_bookmark(&url, false, None) }).to_string());
                }
                #[cfg(not(any(target_os = "macos", target_os = "ios")))]
                {
                    return Some(serde_json::json!({ "project_file": url }).to_string());
                }
            }
        }
        let mut additional_data = additional_data.to_owned();
        if let Ok(serde_json::Value::Object(mut obj)) =
            serde_json::from_str(&additional_data) as serde_json::Result<serde_json::Value>
        {
            if let Ok(output) = serde_json::to_value(&render_options) {
                obj.insert("output".into(), output);
            }
            additional_data = serde_json::to_string(&obj).unwrap_or_default();
        }
        if let Ok(data) =
            stab.export_gyroflow_data(core::GyroflowProjectType::Simple, &additional_data, None)
        {
            return Some(data);
        }
        None
    }

    pub fn get_gyroflow_data(&self, job_id: u32) -> QString {
        if let Some(job) = self.jobs.get(&job_id) {
            job.project_data
                .clone()
                .map(QString::from)
                .unwrap_or_default()
        } else {
            QString::default()
        }
    }

    pub fn get_job_display_params(&self, job_id: u32) -> QString {
        if let Some(job) = self.jobs.get(&job_id) {
            let global_configs = self.stabilizer.lens_group_config.read().clone();
            let metadata_snapshot = metadata_snapshot_for_job(job);
            let lens_group_index = job.lens_group_index.or_else(|| {
                metadata_snapshot
                    .as_ref()
                    .and_then(|md| niyien_lens_presets::extract_lens_index(&md.additional_data))
            });
            let metadata_focal_length = metadata_snapshot
                .as_ref()
                .and_then(niyien_lens_presets::extract_video_focus_length_mm)
                .unwrap_or(0.0);
            let mut lens_group_mode = "auto";
            let mut lens_group_number = 0usize;
            let mut lens_group_focal_length = 0.0;
            let mut lens_group_ratio = 0.0;
            let mut lens_group_direction = String::new();

            if let Some(lens_index) = lens_group_index {
                if let Some((config, is_local)) =
                    effective_lens_group_config_for_group(job, &global_configs, lens_index)
                {
                    let has_manual_display = config.focal_length_mm.unwrap_or_default() > 0.0
                        || config.anamorphic_enabled;
                    if has_manual_display {
                        lens_group_mode = if is_local { "local" } else { "global" };
                        lens_group_number = lens_index + 1;
                        lens_group_focal_length = config.focal_length_mm.unwrap_or_default();
                        lens_group_ratio = config.squeeze_ratio.unwrap_or_default();
                        lens_group_direction = match config.squeeze_direction.unwrap_or_default() {
                            niyien_lens_presets::SqueezeDirection::Horizontal => "H".to_owned(),
                            niyien_lens_presets::SqueezeDirection::Vertical => "V".to_owned(),
                        };
                        if lens_group_focal_length <= 0.0 {
                            lens_group_focal_length = metadata_focal_length;
                        }
                    }
                }
            }

            if let Some(ref data) = job.project_data {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                    let stab = v.get("stabilization").cloned().unwrap_or_default();
                    let smoothness = stab
                        .get("smoothing_params")
                        .and_then(|p| p.as_array())
                        .and_then(|arr| {
                            arr.iter().find(|x| {
                                x.get("name").and_then(|n| n.as_str()) == Some("smoothness")
                            })
                        })
                        .and_then(|x| x.get("value").and_then(|v| v.as_f64()))
                        .unwrap_or(0.5);
                    let horizon_lock_amount = stab
                        .get("horizon_lock_amount")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let lens_correction = stab
                        .get("lens_correction_amount")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(1.0);
                    let az = stab
                        .get("adaptive_zoom_window")
                        .and_then(|v| v.as_f64())
                        .unwrap_or(0.0);
                    let zoom_mode = if az < -0.9 {
                        "static"
                    } else if az > 0.0 {
                        "dynamic"
                    } else {
                        "none"
                    };
                    let framerate = v
                        .get("video_info")
                        .and_then(|vi| vi.get("fps"))
                        .and_then(|f| f.as_f64())
                        .unwrap_or(0.0);
                    let focal_length = v
                        .get("video_info")
                        .and_then(|vi| vi.get("focal_length"))
                        .and_then(|f| f.as_f64())
                        .unwrap_or(0.0);
                    let display_focal_length = if focal_length > 0.0 {
                        focal_length
                    } else {
                        metadata_focal_length
                    };
                    if lens_group_mode != "auto" && lens_group_focal_length <= 0.0 {
                        lens_group_focal_length = display_focal_length;
                    }
                    let detected_source = v
                        .get("gyro_source")
                        .and_then(|gs| gs.get("detected_source"))
                        .and_then(|ds| ds.as_str())
                        .unwrap_or("");
                    let result = serde_json::json!({
                        "smoothness": smoothness,
                        "horizon_lock_amount": horizon_lock_amount,
                        "lens_correction": lens_correction,
                        "zoom_mode": zoom_mode,
                        "framerate": framerate,
                        "focal_length": display_focal_length,
                        "detected_source": detected_source,
                        "auto_rotate": job.auto_rotate,
                        "lens_group_display_mode": lens_group_mode,
                        "lens_group_display_number": lens_group_number,
                        "lens_group_display_focal_length": lens_group_focal_length,
                        "lens_group_display_ratio": lens_group_ratio,
                        "lens_group_display_direction": lens_group_direction,
                    });
                    return QString::from(result.to_string());
                }
            }

            let result = serde_json::json!({
                "auto_rotate": job.auto_rotate,
                "focal_length": metadata_focal_length,
                "lens_group_display_mode": lens_group_mode,
                "lens_group_display_number": lens_group_number,
                "lens_group_display_focal_length": lens_group_focal_length,
                "lens_group_display_ratio": lens_group_ratio,
                "lens_group_display_direction": lens_group_direction,
            });
            return QString::from(result.to_string());
        }
        QString::from("{}")
    }

    fn get_selected_lens_group_status_json(&self, job_ids_json: String) -> QString {
        let job_ids = parse_job_ids_json(&job_ids_json);
        if job_ids.is_empty() {
            return QString::from("[]");
        }

        let mut statuses = niyien_lens_presets::default_lens_group_statuses();
        for job_id in job_ids {
            if let Some(job) = self.jobs.get(&job_id) {
                if let Some(metadata) = metadata_snapshot_for_job(job) {
                    niyien_lens_presets::update_status_from_metadata(&mut statuses, &metadata);
                }
            }
        }
        QString::from(niyien_lens_presets::lens_group_status_to_json(&statuses))
    }

    fn get_selected_lens_group_config_json(&self, job_ids_json: String) -> QString {
        let job_ids = parse_job_ids_json(&job_ids_json);
        if job_ids.is_empty() {
            return QString::from("[]");
        }

        let global_configs = self.stabilizer.lens_group_config.read().clone();
        let default_configs = niyien_lens_presets::default_lens_group_configs();
        let mut aggregated = Vec::with_capacity(niyien_lens_presets::LENS_GROUP_COUNT);

        for lens_index in 0..niyien_lens_presets::LENS_GROUP_COUNT {
            let mut effective_configs = Vec::new();
            for job_id in &job_ids {
                if let Some(job) = self.jobs.get(job_id) {
                    let metadata = metadata_snapshot_for_job(job);
                    let current_lens_index = metadata
                        .as_ref()
                        .and_then(|md| niyien_lens_presets::extract_lens_index(&md.additional_data))
                        .or(job.lens_group_index);
                    if current_lens_index == Some(lens_index) {
                        effective_configs.push(
                            effective_lens_group_configs(job, &global_configs)[lens_index].clone(),
                        );
                    }
                }
            }

            let mut config = effective_configs
                .first()
                .cloned()
                .unwrap_or_else(|| default_configs[lens_index].clone());
            config.lens_index = lens_index;

            let mut mixed_focal_length = false;
            let mut mixed_anamorphic_enabled = false;
            let mut mixed_preset_id = false;
            let mut mixed_squeeze_direction = false;
            let mut mixed_squeeze_ratio = false;

            for other in effective_configs.iter().skip(1) {
                if other.focal_length_mm != config.focal_length_mm {
                    mixed_focal_length = true;
                }
                if other.anamorphic_enabled != config.anamorphic_enabled {
                    mixed_anamorphic_enabled = true;
                }
                if other.preset_id != config.preset_id {
                    mixed_preset_id = true;
                }
                if other.squeeze_direction != config.squeeze_direction {
                    mixed_squeeze_direction = true;
                }
                if other.squeeze_ratio != config.squeeze_ratio {
                    mixed_squeeze_ratio = true;
                }
            }

            if mixed_focal_length {
                config.focal_length_mm = None;
            }
            if mixed_anamorphic_enabled {
                config.anamorphic_enabled = false;
            }
            if mixed_preset_id {
                config.preset_id = None;
            }
            if mixed_squeeze_direction {
                config.squeeze_direction = None;
            }
            if mixed_squeeze_ratio {
                config.squeeze_ratio = None;
            }

            let mut value = serde_json::to_value(config).unwrap_or_default();
            if let Some(obj) = value.as_object_mut() {
                obj.insert(
                    "mixed_focal_length".into(),
                    serde_json::Value::Bool(mixed_focal_length),
                );
                obj.insert(
                    "mixed_anamorphic_enabled".into(),
                    serde_json::Value::Bool(mixed_anamorphic_enabled),
                );
                obj.insert(
                    "mixed_preset_id".into(),
                    serde_json::Value::Bool(mixed_preset_id),
                );
                obj.insert(
                    "mixed_squeeze_direction".into(),
                    serde_json::Value::Bool(mixed_squeeze_direction),
                );
                obj.insert(
                    "mixed_squeeze_ratio".into(),
                    serde_json::Value::Bool(mixed_squeeze_ratio),
                );
            }
            aggregated.push(value);
        }

        QString::from(serde_json::to_string(&aggregated).unwrap_or_else(|_| "[]".to_owned()))
    }

    fn set_selected_lens_group_config(&mut self, job_ids_json: String, config_json: String) {
        let job_ids = parse_job_ids_json(&job_ids_json);
        if job_ids.is_empty() {
            return;
        }

        let requested_configs = niyien_lens_presets::lens_group_configs_from_json(&config_json);
        let global_configs = self.stabilizer.lens_group_config.read().clone();

        for job_id in &job_ids {
            if let Some(job) = self.jobs.get_mut(job_id) {
                let existing_override = job.lens_group_config_override.clone();
                job.lens_group_config_override = build_job_lens_group_override(
                    &requested_configs,
                    &global_configs,
                    existing_override.as_ref(),
                );
            }
        }

        if self.has_match_results() {
            self.reapply_selected_lens_group_config(job_ids_json);
        } else {
            self.match_results_changed();
        }
    }

    fn clear_selected_lens_group_config(&mut self, job_ids_json: String, lens_index: usize) {
        let job_ids = parse_job_ids_json(&job_ids_json);
        if job_ids.is_empty() || lens_index >= niyien_lens_presets::LENS_GROUP_COUNT {
            return;
        }

        let global_configs = self.stabilizer.lens_group_config.read().clone();
        for job_id in &job_ids {
            if let Some(job) = self.jobs.get_mut(job_id) {
                let mut requested_configs = effective_lens_group_configs(job, &global_configs);
                if let Some(config) = requested_configs.get_mut(lens_index) {
                    config.focal_length_mm = None;
                }
                let existing_override = job.lens_group_config_override.clone();
                job.lens_group_config_override = build_job_lens_group_override(
                    &requested_configs,
                    &global_configs,
                    existing_override.as_ref(),
                );
            }
        }

        if self.has_match_results() {
            self.reapply_selected_lens_group_config(job_ids_json);
        } else {
            self.match_results_changed();
        }
    }

    fn reapply_selected_lens_group_config(&mut self, job_ids_json: String) {
        let job_ids: HashSet<u32> = parse_job_ids_json(&job_ids_json).into_iter().collect();
        if job_ids.is_empty() {
            return;
        }
        self.reapply_lens_group_config_filtered(Some(job_ids));
    }

    fn set_batch_auto_rotate(&mut self, job_ids_json: String, enabled: bool) {
        let job_ids: Vec<u32> = match serde_json::from_str(&job_ids_json) {
            Ok(ids) => ids,
            Err(_) => return,
        };
        for job_id in job_ids {
            if let Some(job) = self.jobs.get_mut(&job_id) {
                job.auto_rotate = enabled;
            }
        }
    }

    pub fn batch_update_params(&mut self, job_ids_json: String, params_json: String) {
        let job_ids: Vec<u32> = match serde_json::from_str(&job_ids_json) {
            Ok(ids) => ids,
            Err(_) => return,
        };
        let params: serde_json::Value = match serde_json::from_str(&params_json) {
            Ok(p) => p,
            Err(_) => return,
        };

        for &job_id in &job_ids {
            if let Some(job) = self.jobs.get_mut(&job_id) {
                if let Some(ref mut data_str) = job.project_data {
                    if let Ok(mut data) = serde_json::from_str::<serde_json::Value>(data_str) {
                        let stab = data
                            .get_mut("stabilization")
                            .and_then(|s| s.as_object_mut());
                        if let Some(stab) = stab {
                            if let Some(smoothness) =
                                params.get("smoothness").and_then(|v| v.as_f64())
                            {
                                // Update smoothing_params array
                                if let Some(sp) = stab
                                    .get_mut("smoothing_params")
                                    .and_then(|p| p.as_array_mut())
                                {
                                    for p in sp.iter_mut() {
                                        if p.get("name").and_then(|n| n.as_str())
                                            == Some("smoothness")
                                        {
                                            p.as_object_mut().map(|o| {
                                                o.insert(
                                                    "value".into(),
                                                    serde_json::json!(smoothness),
                                                )
                                            });
                                        }
                                    }
                                }
                            }
                            if let Some(amount) =
                                params.get("horizon_lock_amount").and_then(|v| v.as_f64())
                            {
                                stab.insert(
                                    "horizon_lock_amount".into(),
                                    serde_json::json!(amount),
                                );
                            }
                            if let Some(zoom_mode) =
                                params.get("zoom_mode").and_then(|v| v.as_str())
                            {
                                let az = match zoom_mode {
                                    "static" => -1.0,
                                    "dynamic" => 4.0,
                                    _ => 0.0,
                                };
                                stab.insert("adaptive_zoom_window".into(), serde_json::json!(az));
                            }
                            if let Some(zoom_speed) =
                                params.get("zoom_speed").and_then(|v| v.as_f64())
                            {
                                stab.insert(
                                    "adaptive_zoom_window".into(),
                                    serde_json::json!(zoom_speed),
                                );
                            }
                            if let Some(lc) = params.get("lens_correction").and_then(|v| v.as_f64())
                            {
                                stab.insert("lens_correction_amount".into(), serde_json::json!(lc));
                            }
                        }
                        if let Some(fps) = params.get("framerate").and_then(|v| v.as_f64()) {
                            if let Some(output) =
                                data.get_mut("output").and_then(|o| o.as_object_mut())
                            {
                                output.insert("output_fps".into(), serde_json::json!(fps));
                            }
                        }
                        *data_str = serde_json::to_string(&data).unwrap_or_default();
                    }
                }
            }
        }
        self.queue_changed();
    }

    pub fn render_job(&mut self, job_id: u32) {
        if let Some(job) = self.jobs.get(&job_id) {
            {
                let mut q = self.queue.borrow_mut();
                if job.queue_index < q.row_count() as usize {
                    //let mut itm = &mut q[job.queue_index];
                    let mut itm = q[job.queue_index].clone();
                    if itm.status == JobStatus::Rendering
                        || itm.status == JobStatus::Finished
                        || itm.status == JobStatus::Skipped
                    {
                        ::log::warn!("Job is already rendering or skipped {}", job_id);
                        return;
                    }
                    itm.status = JobStatus::Rendering;
                    //q.data_changed(job.queue_index);
                    q.change_line(job.queue_index, itm);
                }
            }
            job.cancel_flag.store(false, SeqCst);
            // [cancel-epoch] Bump epoch so any pending callbacks from a previous render cycle
            // for this job are ignored; capture_epoch is moved into both the progress and
            // err closures to compare on every callback invocation.
            let capture_epoch = job.render_epoch.fetch_add(1, SeqCst) + 1;

            let stab = match job.stab.clone() {
                Some(s) => s,
                None => {
                    ::log::error!(
                        "StabilizationManager is None for job {}, cannot render",
                        job_id
                    );
                    return;
                }
            };

            rendering::clear_log();

            let rendered_frames = Arc::new(AtomicUsize::new(0));
            let rendered_frames2 = rendered_frames.clone();
            let progress = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this,
                      (progress, current_frame, total_frames, finished, is_conversion): (
                    f64,
                    usize,
                    usize,
                    bool,
                    bool,
                )| {
                    // [cancel-epoch] Ignore any callback whose epoch has been superseded by
                    // pause()/stop()/new render_job. This is the critical guard that prevents
                    // a cancelled render's trailing finished=true callback from flipping the
                    // job to Finished and releasing stab after the user restarts.
                    let current_epoch = this
                        .jobs
                        .get(&job_id)
                        .map(|j| j.render_epoch.load(SeqCst))
                        .unwrap_or(0);
                    if current_epoch != capture_epoch {
                        return;
                    }

                    rendered_frames2.store(current_frame, SeqCst);

                    let mut start_time = 0;

                    update_model!(this, job_id, itm {
                        itm.current_frame = current_frame as u64;
                        itm.total_frames = total_frames as u64;
                        if itm.start_timestamp == 0 {
                            itm.start_timestamp = Self::current_timestamp();
                        }
                        start_time = itm.start_timestamp;
                        itm.end_timestamp = Self::current_timestamp();
                        itm.frame_times.push_back((itm.current_frame, itm.end_timestamp));
                        if itm.end_timestamp - itm.start_timestamp > 10000 { // 10s average
                            if let Some(el) = itm.frame_times.pop_front() {
                                itm.start_timestamp_frame = el.0;
                                itm.start_timestamp2 = el.1;
                            }
                        }
                        if finished {
                            itm.status = JobStatus::Finished;
                        }
                    });

                    this.end_timestamp = Self::current_timestamp();
                    this.render_progress(
                        job_id,
                        progress,
                        current_frame,
                        total_frames,
                        finished,
                        start_time as f64,
                        is_conversion,
                    );
                    this.progress_changed();

                    let is_queue_active = this.status == "active".into();
                    if finished {
                        // Update project_data with sync offsets before releasing stab
                        if let Some(job) = this.jobs.get_mut(&job_id) {
                            if let Some(ref stab) = job.stab {
                                job.project_data = Self::get_gyroflow_data_internal(
                                    stab,
                                    &job.additional_data,
                                    &job.render_options,
                                );
                            }
                        }
                        // Release StabilizationManager to reclaim GPU memory
                        if let Some(job) = this.jobs.get_mut(&job_id) {
                            job.stab = None;
                        }
                        if this.get_pending_count() > 0 && is_queue_active {
                            // Start the next one
                            this.start();
                        } else {
                            this.start_timestamp = 0;
                            this.start_frame = 0;
                            this.update_status();
                            if is_queue_active {
                                this.post_render_action();
                            }
                        }
                    }
                },
            );
            let processing = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, progress: f64| {
                    update_model!(this, job_id, itm {
                        itm.processing_progress = progress;
                    });
                    this.processing_progress(job_id, progress);
                },
            );
            let encoder_initialized = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, encoder_name: String| {
                    if let Some(job) = this.jobs.get(&job_id) {
                        if job.render_options.use_gpu
                            && (encoder_name == "libx264"
                                || encoder_name == "libx265"
                                || encoder_name == "prores_ks")
                        {
                            update_model!(this, job_id, itm {
                                itm.error_string = QString::from("uses_cpu");
                            });
                        }
                    }
                    this.encoder_initialized(job_id, encoder_name);
                },
            );

            let err = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, (msg, mut arg): (String, String)| {
                    // [cancel-epoch] Same guard as progress — a cancelled render may surface
                    // as an Err from ffmpeg; we must not mark the job Error after the user
                    // has already requested a restart (which bumped the epoch).
                    let current_epoch = this
                        .jobs
                        .get(&job_id)
                        .map(|j| j.render_epoch.load(SeqCst))
                        .unwrap_or(0);
                    if current_epoch != capture_epoch {
                        return;
                    }

                    arg.push_str("\n\n");
                    arg.push_str(&rendering::get_log());

                    update_model!(this, job_id, itm {
                        itm.error_string = QString::from(arg.clone());
                        itm.status = JobStatus::Error;
                    });

                    this.error(
                        job_id,
                        QString::from(msg),
                        QString::from(arg),
                        QString::default(),
                    );
                    this.render_progress(job_id, 1.0, 0, 0, true, 0.0, false);

                    // Release StabilizationManager to reclaim GPU memory
                    if let Some(job) = this.jobs.get_mut(&job_id) {
                        job.stab = None;
                    }

                    if this.get_pending_count() > 0 {
                        // Start the next one
                        this.start();
                    } else {
                        this.start_timestamp = 0;
                        this.start_frame = 0;
                    }
                    this.update_status();
                },
            );

            let convert_format = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, (format, mut supported, candidate): (String, String, String)| {
                    use itertools::Itertools;
                    supported = supported
                        .split(',')
                        .filter(|v| {
                            ![
                                "CUDA",
                                "D3D11",
                                "D3D12",
                                "BGRZ",
                                "RGBZ",
                                "BGRA",
                                "UYVY422",
                                "VIDEOTOOLBOX",
                                "DXVA2",
                                "MEDIACODEC",
                                "VULKAN",
                                "OPENCL",
                                "QSV",
                            ]
                            .contains(v)
                        })
                        .join(",");

                    update_model!(this, job_id, itm {
                        itm.error_string = QString::from(format!("convert_format:{format};{supported};{candidate}"));
                        itm.status = JobStatus::Error;
                    });

                    this.convert_format(
                        job_id,
                        QString::from(format),
                        QString::from(supported),
                        QString::from(candidate),
                    );
                    this.render_progress(job_id, 1.0, 0, 0, true, 0.0, false);

                    if this.get_pending_count() > 0 {
                        // Start the next one
                        this.start();
                    } else {
                        this.start_timestamp = 0;
                        this.start_frame = 0;
                    }
                    this.update_status();
                },
            );
            let params = stab.params.read();
            let trim_ratio = params.get_trim_ratio();
            let total_frame_count = params.frame_count;
            drop(params);
            let mut input_file = stab.input_file.read().clone();
            let filename = filesystem::get_filename(&input_file.url);
            let render_options = job.render_options.clone();

            progress((
                0.0,
                0,
                (total_frame_count as f64 * trim_ratio).round() as usize,
                false,
                false,
            ));

            job.cancel_flag.store(false, SeqCst);
            let cancel_flag = job.cancel_flag.clone();
            let pause_flag = self.pause_flag.clone();
            let export_project = self.export_project;
            let export_metadata = self.export_metadata.clone();
            let export_stmap = self.export_stmap.clone();
            let default_suffix = self.default_suffix.to_string();
            let mut additional_data = job.additional_data.clone();
            let mut proc_height = self.processing_resolution;
            let err2 = err.clone();
            if let Some(ref ss) = stab.lens.read().sync_settings {
                if let Some(pr) = ss.get("processing_resolution").and_then(|x| x.as_u64()) {
                    proc_height = pr as i32;
                }
            }

            let sync_cancel_flag = cancel_flag.clone();
            core::run_threaded(move || {
                Self::do_autosync(stab.clone(), processing, &input_file, err2, proc_height, sync_cancel_flag);
                stab.recompute_blocking();

                if let Some((opt, path, fields)) = export_metadata {
                    let result = || -> Result<(), core::GyroflowCoreError> {
                        let url = filesystem::path_to_url(&path);
                        match opt {
                            1 => {
                                let gyro_url = stab.input_file.read().url.clone();
                                let contents = gyroflow_core::gyro_export::export_full_metadata(
                                    &gyro_url, &stab,
                                )?;
                                filesystem::write(&url, contents.as_bytes())?;
                            }
                            2 => {
                                if let Ok(contents) =
                                    serde_json::to_string_pretty(&stab.gyro.read().file_metadata)
                                {
                                    filesystem::write(&url, contents.as_bytes())?;
                                }
                            }
                            3 => {
                                let filename = filesystem::get_filename(&url).to_ascii_lowercase();
                                let contents = gyroflow_core::gyro_export::export_gyro_data(
                                    &filename,
                                    &serde_json::to_string(&fields).unwrap(),
                                    &stab,
                                );
                                filesystem::write(&url, contents.as_bytes())?
                            }
                            _ => {}
                        }
                        Ok(())
                    };
                    if let Err(e) = result() {
                        err(("An error occured: %1".to_string(), e.to_string()));
                    } else {
                        progress((1.0, 1, 1, true, false));
                    }
                    return;
                }
                if let Some((opt, path)) = export_stmap {
                    let per_frame = opt == 2;
                    let folder_url = filesystem::path_to_url(&path);
                    let total = if per_frame {
                        stab.params.read().frame_count
                    } else {
                        1
                    };
                    let mut processed = 0;
                    progress((0.0, processed, total, false, false));
                    for (fname_base, frame, dist, undist) in
                        core::stmap::generate_stmaps(&stab, per_frame)
                    {
                        if let Err(e) = filesystem::write(
                            &filesystem::get_file_url(
                                &folder_url,
                                &format!("{fname_base}-undistort-{frame}.exr"),
                                true,
                            ),
                            &undist,
                        ) {
                            return err((e.to_string(), String::new()));
                        }
                        if let Err(e) = filesystem::write(
                            &filesystem::get_file_url(
                                &folder_url,
                                &format!("{fname_base}-redistort-{frame}.exr"),
                                true,
                            ),
                            &dist,
                        ) {
                            return err((e.to_string(), String::new()));
                        }
                        processed += 1;
                        progress((
                            processed as f64 / total as f64,
                            processed,
                            total,
                            false,
                            false,
                        ));

                        if cancel_flag.load(SeqCst) {
                            break;
                        }
                    }
                    progress((1.0, total, total, true, false));
                    return;
                }

                if export_project > 0 {
                    if let Ok(serde_json::Value::Object(mut obj)) =
                        serde_json::from_str(&additional_data)
                            as serde_json::Result<serde_json::Value>
                    {
                        if let Ok(output) = serde_json::to_value(&render_options) {
                            obj.insert("output".into(), output);
                        }
                        additional_data = serde_json::to_string(&obj).unwrap_or_default();
                    }
                    let gf_folder = render_options.output_folder.to_owned();
                    let gf_file = filesystem::filename_with_extension(
                        &render_options.output_filename.replace(&default_suffix, ""),
                        "gyroflow",
                    );
                    let gf_url = filesystem::get_file_url(&gf_folder, &gf_file, true);
                    let result = match export_project {
                        1 => stab.export_gyroflow_file(
                            &gf_url,
                            core::GyroflowProjectType::Simple,
                            &additional_data,
                        ),
                        2 => stab.export_gyroflow_file(
                            &gf_url,
                            core::GyroflowProjectType::WithGyroData,
                            &additional_data,
                        ),
                        3 => stab.export_gyroflow_file(
                            &gf_url,
                            core::GyroflowProjectType::WithProcessedData,
                            &additional_data,
                        ),
                        4 => stab.export_gyroflow_file(
                            &gf_url,
                            core::GyroflowProjectType::WithGyroData,
                            &additional_data,
                        ),
                        _ => Err(gyroflow_core::GyroflowCoreError::Unknown),
                    };
                    if export_project != 4 {
                        if let Err(e) = result {
                            err((e.to_string(), String::new()));
                        } else {
                            progress((1.0, 1, 1, true, false));
                        }
                        return;
                    }
                }

                // Assumes regular filesystem
                if filename.to_ascii_lowercase().ends_with(".r3d")
                    || filename.to_ascii_lowercase().ends_with(".nev")
                {
                    let mov_url = filesystem::get_file_url(
                        &filesystem::get_folder(&input_file.url),
                        &filesystem::filename_with_extension(
                            &filesystem::get_filename(&input_file.url),
                            "mov",
                        ),
                        false,
                    );
                    if filesystem::exists(&mov_url) {
                        input_file.url = mov_url.clone();
                    } else {
                        let in_file = input_file.url.clone();

                        let mut frame = 0;
                        let r3d_progress =
                            |(percent, error_str, out_url): (f64, String, String)| {
                                if !error_str.is_empty() {
                                    err(("An error occured: %1".to_string(), error_str));
                                } else {
                                    progress((
                                        percent * 0.98,
                                        frame,
                                        total_frame_count + 1,
                                        false,
                                        true,
                                    ));
                                    input_file.url = out_url;
                                    frame += 1;
                                }
                            };
                        let format = gyroflow_core::settings::get_u64("r3dConvertFormat", 0) as i32;
                        let force_primary =
                            gyroflow_core::settings::get_u64("r3dColorMode", 0) as i32;

                        let gamma_curves = [
                            -1, 1, 2, 3, 4, 5, 6, 14, 15, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36,
                            37,
                        ];
                        let color_spaces =
                            [2, 0, 1, 14, 15, 5, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27];
                        let gamma = gamma_curves
                            [gyroflow_core::settings::get_u64("r3dGammaCurve", 7) as usize];
                        let space = color_spaces
                            [gyroflow_core::settings::get_u64("r3dColorSpace", 0) as usize];
                        let additional_params =
                            gyroflow_core::settings::get_str("r3dRedlineParams", "");
                        crate::external_sdk::r3d::REDSdk::convert_r3d(
                            &in_file,
                            format,
                            force_primary > 0,
                            gamma,
                            space,
                            &additional_params,
                            r3d_progress,
                            cancel_flag.clone(),
                        );
                        if cancel_flag.load(SeqCst) {
                            std::thread::sleep(std::time::Duration::from_secs(2));
                            let _ = filesystem::remove_file(&mov_url);
                            err(("Conversion cancelled%1".to_string(), "".to_string()));
                            return;
                        }
                    }
                }

                let num_ranges = stab.params.read().trim_ranges.len();
                let ranges_to_render = if render_options.export_trims_separately && num_ranges > 0 {
                    (0..num_ranges).map(Some).collect::<Vec<_>>()
                } else {
                    vec![None]
                };
                let original_gpu_decode = stab.gpu_decoding.load(SeqCst);
                'ranges: for range in ranges_to_render {
                    if cancel_flag.load(SeqCst) {
                        break;
                    }
                    let mut i = 0;
                    loop {
                        let result = rendering::render(
                            stab.clone(),
                            progress.clone(),
                            &input_file,
                            &render_options,
                            i,
                            range,
                            cancel_flag.clone(),
                            pause_flag.clone(),
                            encoder_initialized.clone(),
                        );
                        if let Err(e) = result {
                            if let rendering::FFmpegError::PixelFormatNotSupported((
                                fmt,
                                supported,
                                candidate,
                            )) = e
                            {
                                let candidate = if let Some(c) = candidate {
                                    format!("{c:?}").to_ascii_lowercase().to_string()
                                } else {
                                    String::new()
                                };
                                convert_format((
                                    format!("{fmt:?}"),
                                    supported
                                        .into_iter()
                                        .map(|v| format!("{:?}", v))
                                        .collect::<Vec<String>>()
                                        .join(","),
                                    candidate,
                                ));
                                break 'ranges;
                            }
                            if original_gpu_decode
                                && stab.gpu_decoding.load(SeqCst)
                                && matches!(e, rendering::FFmpegError::GPUDecodingFailed)
                            {
                                stab.gpu_decoding.store(false, SeqCst);
                                continue;
                            }
                            if rendered_frames.load(SeqCst) == 0 {
                                if (0..4).contains(&i) {
                                    // Try 4 times with different GPU decoders
                                    i += 1;
                                    continue;
                                }
                                if (0..5).contains(&i) {
                                    // Try without GPU decoder
                                    i = -1;
                                    continue;
                                }
                            }
                            err(("An error occured: %1".to_string(), e.to_string()));
                            break 'ranges;
                        } else {
                            // Render ok
                            break;
                        }
                    }
                }
                stab.gpu_decoding.store(original_gpu_decode, SeqCst);
            });
        }
    }

    fn get_output_folder(input_url: &str, ui_output_folder: &str) -> String {
        if !ui_output_folder.is_empty() {
            return ui_output_folder.to_owned();
        }
        filesystem::get_folder(input_url)
    }
    fn get_output_filename(
        input_url: &str,
        suffix: &str,
        render_options: &RenderOptions,
        override_ext: Option<&str>,
    ) -> String {
        if !render_options.output_filename.is_empty() {
            return render_options.output_filename.to_owned();
        }
        let mut filename = filesystem::get_filename(input_url);

        let mut ext = override_ext.unwrap_or(match render_options.codec.as_ref() {
            "ProRes" => ".mov",
            "DNxHD" => ".mov",
            "CineForm" => ".mov",
            "EXR Sequence" => "_%05d.exr",
            "PNG Sequence" => "_%05d.png",
            _ => ".mp4",
        });
        if ext == ".mp4" && render_options.preserve_other_tracks {
            ext = ".mov";
        }
        if let Some(pos) = filename.rfind('.') {
            filename = filename[..pos].to_owned();
        }

        format!("{filename}{suffix}{ext}")
    }

    pub fn add_file(&mut self, url: String, gyro_url: String, additional_data: String) -> u32 {
        let job_id = fastrand::u32(1..2147483640);

        let is_gf_data = url.starts_with('{');

        let err = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, (msg, arg): (String, String)| {
                ::log::warn!("[add_file]: {}", arg);
                update_model!(this, job_id, itm {
                    itm.error_string = QString::from(arg.clone());
                    itm.status = JobStatus::Error;
                });
                this.error(
                    job_id,
                    QString::from(msg),
                    QString::from(arg),
                    QString::default(),
                );
            },
        );
        let is_rendering = self.export_metadata.is_none() && self.export_stmap.is_none();
        let processing_done = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, _: ()| {
                // overwrite_mode == 1 means silent overwrite (Simple mode default). Skip the
                // file-exists check entirely so no prompt/inline message is emitted.
                if this.overwrite_mode != 1 {
                    if let Some(job) = this.jobs.get(&job_id) {
                        if is_rendering
                            && filesystem::exists_in_folder(
                                &job.render_options.output_folder,
                                &job.render_options
                                    .output_filename
                                    .replace("_%05d", "_00001"),
                            )
                        {
                            let msg = QString::from(format!(
                                "file_exists:{}",
                                serde_json::json!({ "filename": job.render_options.output_filename, "folder": job.render_options.output_folder })
                            ));
                            update_model!(this, job_id, itm {
                                itm.error_string = msg.clone();
                                itm.status = JobStatus::Error;
                            });
                            this.error(job_id, msg, QString::default(), QString::default());
                        }
                    }
                }

                this.processing_done(job_id, false);
            },
        );

        let suffix = self.default_suffix.to_string();

        let stabilizer = self.stabilizer.clone();

        let additional_data2 = additional_data.clone();
        let additional_data3 = additional_data.clone();
        if let Ok(additional_data) =
            serde_json::from_str(&additional_data) as serde_json::Result<serde_json::Value>
        {
            let mut sync_options = serde_json::Value::default();
            if let Some(sync) = additional_data.get("synchronization") {
                sync_options = sync.clone();
            }
            if let Some(out) = additional_data.get("output") {
                let has_output_width = out
                    .as_object()
                    .map(|x| x.contains_key("output_width"))
                    .unwrap_or_default()
                    && out
                        .get("output_width")
                        .and_then(|x| x.as_i64())
                        .unwrap_or_default()
                        > 0;
                let has_output_height = out
                    .as_object()
                    .map(|x| x.contains_key("output_height"))
                    .unwrap_or_default()
                    && out
                        .get("output_height")
                        .and_then(|x| x.as_i64())
                        .unwrap_or_default()
                        > 0;

                let override_ext = out
                    .get("output_extension")
                    .and_then(|x| x.as_str())
                    .map(|x| x.to_owned());
                if let Ok(mut render_options) =
                    serde_json::from_value(out.clone()) as serde_json::Result<RenderOptions>
                {
                    render_options.update_from_json(out);
                    let smoothing = stabilizer.smoothing.read().clone();
                    let params = stabilizer.params.read();

                    let stab = StabilizationManager {
                        params: Arc::new(RwLock::new(
                            core::stabilization_params::StabilizationParams {
                                fov: params.fov,
                                background: params.background,
                                adaptive_zoom_window: params.adaptive_zoom_window,
                                lens_correction_amount: params.lens_correction_amount,
                                light_refraction_coefficient: params.light_refraction_coefficient,
                                background_mode: params.background_mode,
                                background_margin: params.background_margin,
                                background_margin_feather: params.background_margin_feather,
                                current_device: params.current_device,
                                video_speed: params.video_speed,
                                video_speed_affects_smoothing: params.video_speed_affects_smoothing,
                                video_speed_affects_zooming: params.video_speed_affects_zooming,
                                video_speed_affects_zooming_limit: params
                                    .video_speed_affects_zooming_limit,
                                of_method: params.of_method,
                                adaptive_zoom_method: params.adaptive_zoom_method,
                                max_zoom: params.max_zoom,
                                max_zoom_iterations: params.max_zoom_iterations,
                                ..Default::default()
                            },
                        )),
                        input_file: Arc::new(RwLock::new(gyroflow_core::InputFile {
                            url: if is_gf_data {
                                String::new()
                            } else {
                                url.clone()
                            },
                            project_file_url: None,
                            image_sequence_start: 0,
                            image_sequence_fps: 0.0,
                            preset_name: None,
                            preset_output_size: None,
                        })),
                        lens_profile_db: stabilizer.lens_profile_db.clone(),
                        ..Default::default()
                    };

                    *stab.smoothing.write() = smoothing;

                    let stab = Arc::new(stab);

                    let stab2 = stab.clone();
                    let loaded = util::qt_queued_callback_mut(
                        QPointer::from(self as &Self),
                        move |this, render_options: RenderOptions| {
                            this.add_internal(
                                job_id,
                                stab2.clone(),
                                render_options,
                                additional_data2.clone(),
                                QString::default(),
                            );
                        },
                    );
                    let thumb_fetched = util::qt_queued_callback_mut(
                        QPointer::from(self as &Self),
                        move |this, thumb: QString| {
                            update_model!(this, job_id, itm { itm.thumbnail_url = thumb; });
                        },
                    );
                    let apply_preset = util::qt_queued_callback_mut(
                        QPointer::from(self as &Self),
                        move |this, (preset, to_job_id): (String, u32)| {
                            this.apply_to_all(preset, additional_data3.clone(), to_job_id);
                            this.added(job_id);
                        },
                    );

                    core::run_threaded(move || {
                        let fetch_thumb =
                            |video_url: &str, ratio: f64| -> Result<(), rendering::FFmpegError> {
                                let mut fetched = false;
                                if !crate::cli::will_run_in_console() {
                                    // Don't fetch thumbs in the CLI
                                    let mut proc = rendering::VideoProcessor::from_file(
                                        video_url, false, 0, None,
                                    )?;
                                    proc.on_frame(move |_timestamp_us, input_frame, _output_frame, converter, _rate_control| {
                                    let sf = converter.scale(input_frame, ffmpeg_next::format::Pixel::RGBA, (50.0 * ratio).round() as u32, 50)?;

                                    if !fetched {
                                        thumb_fetched(util::image_data_to_base64(sf.plane_width(0), sf.plane_height(0), sf.stride(0) as u32, sf.data(0)));
                                        fetched = true;
                                    }

                                    Ok(())
                                });
                                    proc.start_decoder_only(
                                        vec![(0.0, 50.0)],
                                        Arc::new(AtomicBool::new(true)),
                                    )?;
                                }
                                Ok(())
                            };

                        if is_gf_data || filesystem::get_filename(&url).ends_with(".gyroflow") {
                            if !is_gf_data {
                                let video_url = || -> Option<String> {
                                    let data = filesystem::read(&url).ok()?;
                                    let obj: serde_json::Value =
                                        serde_json::from_slice(&data).ok()?;
                                    Some(obj.get("videofile")?.as_str()?.to_string())
                                }()
                                .unwrap_or_default();

                                if video_url.is_empty() {
                                    // It's a preset
                                    if let Ok(data) = filesystem::read_to_string(&url) {
                                        apply_preset((data, 0));
                                    }
                                    return;
                                }
                            }

                            let result = if is_gf_data {
                                let mut is_preset = false;
                                stab.import_gyroflow_data(
                                    url.as_bytes(),
                                    true,
                                    None,
                                    |_| (),
                                    Arc::new(AtomicBool::new(false)),
                                    &mut is_preset,
                                    false,
                                )
                            } else {
                                stab.import_gyroflow_file(
                                    &url,
                                    true,
                                    |_| (),
                                    Arc::new(AtomicBool::new(false)),
                                    false,
                                )
                            };

                            match result {
                                Ok(obj) => {
                                    if let Some(out) = obj.get("output") {
                                        if let Ok(mut render_options2) =
                                            serde_json::from_value(out.clone())
                                                as serde_json::Result<RenderOptions>
                                        {
                                            render_options2.update_from_json(out);
                                            loaded(render_options2);
                                        }
                                    }
                                    if let Some(out) = obj.get("videofile").and_then(|x| x.as_str())
                                    {
                                        let ratio = {
                                            let params = stab.params.read();
                                            params.size.0 as f64 / params.size.1 as f64
                                        };

                                        if let Err(e) = fetch_thumb(out, ratio) {
                                            err((
                                                "An error occured: %1".to_string(),
                                                e.to_string(),
                                            ));
                                        }
                                    }

                                    Self::update_sync_settings(&stab, &sync_options);
                                    if let Some(sync) =
                                        obj.get("synchronization").and_then(|x| x.as_object())
                                    {
                                        if !sync.is_empty() {
                                            Self::update_sync_settings(
                                                &stab,
                                                &serde_json::Value::Object(sync.clone()),
                                            );
                                        }
                                    }

                                    processing_done(());
                                }
                                Err(e) => {
                                    err((
                                        "An error occured: %1".to_string(),
                                        format!("Error loading {}: {:?}", url, e),
                                    ));
                                }
                            }
                        } else if let Ok(info) = rendering::VideoProcessor::get_video_info(&url) {
                            ::log::debug!("Loaded {:?}", &info);

                            render_options.bitrate = render_options.bitrate.max(info.bitrate);
                            if !has_output_width {
                                render_options.output_width = info.width as usize;
                            }
                            if !has_output_height {
                                render_options.output_height = info.height as usize;
                            }
                            render_options.output_folder =
                                Self::get_output_folder(&url, &render_options.output_folder);
                            render_options.output_filename = Self::get_output_filename(
                                &url,
                                &suffix,
                                &render_options,
                                override_ext.as_deref(),
                            );

                            let ratio = info.width as f64 / info.height as f64;

                            if info.duration_ms > 0.0 && info.fps > 0.0 {
                                let video_size = (info.width as usize, info.height as usize);

                                stab.init_from_video_data(
                                    info.duration_ms,
                                    info.fps,
                                    info.frame_count,
                                    video_size,
                                );
                                let normalized_metadata_rotation =
                                    ((360 - info.rotation) % 360) as f64;
                                ::log::info!(
                                    "[video_rotation] file='{}' metadata_raw={} metadata_normalized={}",
                                    filesystem::get_filename(&url),
                                    info.rotation,
                                    normalized_metadata_rotation
                                );
                                stab.set_video_rotation(normalized_metadata_rotation);
                                stab.params.write().video_created_at = info.created_at;

                                stab.input_file.write().url = url.clone();

                                let is_main_video = gyro_url.is_empty();
                                let gyro_url = if !gyro_url.is_empty() {
                                    &gyro_url
                                } else {
                                    &url
                                };
                                {
                                    if let Ok(mut file) =
                                        filesystem::open_file(&gyro_url, false, false)
                                    {
                                        let filesize = file.size;
                                        let _ = stab.load_gyro_data(
                                            file.get_file(),
                                            filesize,
                                            &gyro_url,
                                            is_main_video,
                                            &Default::default(),
                                            |_| (),
                                            Arc::new(AtomicBool::new(false)),
                                        );
                                    }
                                }
                                // Prefer telemetry-parser's creation_date_utc over ffmpeg's creation_time
                                {
                                    let file_metadata =
                                        stab.gyro.read().file_metadata.read().clone();
                                    if let Some(ref utc_str) = file_metadata.creation_date_utc {
                                        if let Some(ms) = parse_creation_date_to_millis(utc_str) {
                                            stab.params.write().video_created_at = Some(ms);
                                        }
                                    }
                                }

                                let camera_id = stab.camera_id.read();

                                let has_builtin_profile = {
                                    let gyro = stab.gyro.read();
                                    let file_metadata = gyro.file_metadata.read();
                                    file_metadata
                                        .lens_profile
                                        .as_ref()
                                        .map(|y| y.is_object())
                                        .unwrap_or_default()
                                };

                                let id_str = camera_id
                                    .as_ref()
                                    .map(|v| v.get_identifier_for_autoload())
                                    .unwrap_or_default();
                                if !id_str.is_empty() && !has_builtin_profile {
                                    let db = stab.lens_profile_db.read();
                                    if db.contains_id(&id_str) {
                                        match stab.load_lens_profile(&id_str) {
                                            Ok(_) => {
                                                let (fr, frd) = {
                                                    let lens = stab.lens.read();
                                                    (
                                                        lens.frame_readout_time,
                                                        lens.frame_readout_direction,
                                                    )
                                                };
                                                if let Some(fr) = fr {
                                                    let mut params = stab.params.write();
                                                    params.frame_readout_time = fr.abs();
                                                    params.frame_readout_direction =
                                                        frd.unwrap_or(if fr < 0.0 {
                                                            ReadoutDirection::BottomToTop
                                                        } else {
                                                            ReadoutDirection::TopToBottom
                                                        });
                                                }
                                            }
                                            Err(e) => {
                                                err((
                                                    "An error occured: %1".to_string(),
                                                    e.to_string(),
                                                ));
                                                return;
                                            }
                                        }
                                    }
                                }
                                if let Some(output_dim) = stab.lens.read().output_dimension.clone()
                                {
                                    if !has_output_width {
                                        render_options.output_width = output_dim.w;
                                    }
                                    if !has_output_height {
                                        render_options.output_height = output_dim.h;
                                    }
                                }

                                stab.set_size(video_size.0, video_size.1);
                                stab.set_output_size(
                                    render_options.output_width,
                                    render_options.output_height,
                                );

                                stab.recompute_blocking();

                                // println!("{}", stab.export_gyroflow_data(true, serde_json::to_string(&render_options).unwrap_or_default()));

                                loaded(render_options);

                                Self::update_sync_settings(&stab, &sync_options);

                                // Apply default preset
                                let default_preset = gyroflow_core::lens_profile_database::LensProfileDatabase::get_path().join("default.gyroflow");
                                let default_preset2 = gyroflow_core::settings::data_dir()
                                    .join("lens_profiles")
                                    .join("default.gyroflow");
                                if let Ok(data) = std::fs::read_to_string(default_preset2) {
                                    apply_preset((data, job_id));
                                } else if let Ok(data) = std::fs::read_to_string(default_preset) {
                                    apply_preset((data, job_id));
                                }

                                if let Err(e) = fetch_thumb(&url, ratio) {
                                    err(("An error occured: %1".to_string(), e.to_string()));
                                }

                                processing_done(());
                            }
                        } else {
                            err((
                                "An error occured: %1".to_string(),
                                "Unable to read the video file.".to_string(),
                            ));
                        }
                    });
                }
            }
        }
        self.jobs_added.insert(job_id);

        job_id
    }

    fn do_autosync<
        F: Fn(f64) + Send + Sync + Clone + 'static,
        F2: Fn((String, String)) + Send + Sync + Clone + 'static,
    >(
        stab: Arc<StabilizationManager>,
        processing_cb: F,
        input_file: &gyroflow_core::InputFile,
        err: F2,
        proc_height: i32,
        cancel_flag: Arc<AtomicBool>,
    ) {
        let (url, duration_ms) = {
            (
                stab.input_file.read().url.clone(),
                stab.params.read().duration_ms,
            )
        };

        let (has_sync_points, has_accurate_timestamps) = {
            let gyro = stab.gyro.read();
            let md = gyro.file_metadata.read();
            (
                !gyro.get_offsets().is_empty(),
                md.has_accurate_timestamps && !url.to_ascii_lowercase().ends_with(".braw"),
            )
        };
        let fps = stab.params.read().fps;

        let sync_settings = stab.lens.read().sync_settings.clone().unwrap_or_default();
        let force_autosync = sync_settings
            .get("do_autosync")
            .and_then(|v| v.as_bool())
            .unwrap_or_default();
        if force_autosync && has_accurate_timestamps {
            ::log::info!("do_autosync overriding has_accurate_timestamps for {}", url);
        }
        // force_autosync takes precedence over stale offsets: a reset + re-queue from the
        // render queue's Auto sync button must rerun the full sync even if the previous
        // pass left offsets on stab.gyro. Clear them so the pipeline recomputes fresh.
        if force_autosync && has_sync_points {
            let stale = stab.gyro.read().get_offsets().len();
            stab.gyro.write().clear_offsets();
            ::log::info!("do_autosync clearing {} stale sync point(s) for {}", stale, url);
        }
        if force_autosync || (!has_sync_points && !has_accurate_timestamps) {
            // ----------------------------------------------------------------------------
            // --------------------------------- Autosync ---------------------------------
            processing_cb(0.01);
            use crate::rendering::VideoProcessor;
            use gyroflow_core::synchronization;
            use gyroflow_core::synchronization::AutosyncProcess;
            use itertools::Either;

            if let Ok(mut sync_params) = serde_json::from_value(sync_settings)
                as serde_json::Result<synchronization::SyncParams>
            {
                if sync_params.max_sync_points > 0 {
                    let mut timestamps_fract = stab.get_optimal_sync_points(
                        sync_params.max_sync_points,
                        sync_params.initial_offset * 1000.0,
                    );

                    if timestamps_fract.is_empty() || !sync_params.auto_sync_points {
                        let chunks = 1.0 / sync_params.max_sync_points as f64;
                        let start = chunks / 2.0;
                        timestamps_fract = (0..sync_params.max_sync_points)
                            .map(|i| start + (i as f64 * chunks))
                            .collect();

                        if !sync_params.custom_sync_pattern.is_null() {
                            let v = Self::resolve_syncpoint_pattern(
                                &sync_params.custom_sync_pattern,
                                duration_ms,
                                fps,
                            );
                            timestamps_fract = v
                                .into_iter()
                                .filter(|v| *v <= duration_ms)
                                .map(|v| v / duration_ms)
                                .collect();
                        }
                    }

                    #[cfg(not(any(target_os = "ios", target_os = "android")))]
                    let _prevent_system_sleep =
                        keep_awake::inhibit_system("Gyroflow", "Autosyncing");
                    #[cfg(any(target_os = "ios", target_os = "android"))]
                    let _prevent_system_sleep =
                        keep_awake::inhibit_display("Gyroflow", "Autosyncing");

                    // cancel_flag is passed in from the caller (Job.cancel_flag) and
                    // is toggled by RenderQueue::pause/stop/cancel_job. Previously a
                    // local flag was created here, so pause/stop could never interrupt
                    // an in-flight NeuFlow sync.
                    sync_params.initial_offset *= 1000.0; // s to ms
                    sync_params.time_per_syncpoint *= 1000.0; // s to ms
                    sync_params.search_size *= 1000.0; // s to ms

                    let every_nth_frame = sync_params.every_nth_frame.max(1);
                    let of_method = sync_params.of_method;

                    let size = stab.params.read().size;

                    let sync_failure_detail = synchronization::describe_autosync_init_failure(
                        &stab,
                        &timestamps_fract,
                        &sync_params,
                    );

                    if let Ok(mut sync) = AutosyncProcess::from_manager(
                        &stab,
                        &timestamps_fract,
                        sync_params,
                        "synchronize".into(),
                        cancel_flag.clone(),
                    ) {
                        let processing_cb2 = processing_cb.clone();
                        sync.on_progress(move |percent, _ready, _total| {
                            processing_cb2(percent);
                        });
                        let stab2 = stab.clone();
                        sync.on_finished(move |arg| {
                            if let Either::Left(offsets) = arg {
                                let mut gyro = stab2.gyro.write();
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
                                    {
                                        // Check the offset — confidence ≥ 0.4 bypass rank
                                        if confidence < 0.4 {
                                            let sync_data = stab2.sync_data.read();
                                            if !sync_data.rank.is_empty() {
                                                let index = ((x.0 - x.1) as f64
                                                    / (sync_data.ratio * 1000.0))
                                                    .round()
                                                    as usize;
                                                if index < sync_data.rank.len()
                                                    && sync_data.rank[index] < 13.0
                                                {
                                                    continue;
                                                }
                                            }
                                        }
                                    }
                                    // Remove existing offsets within 100ms range
                                    gyro.remove_offsets_near(new_ts, 100.0);
                                    gyro.set_offset(new_ts, x.1);
                                }
                                // Switch from Complementary to VQF after sync completes
                                gyro.integration_method = 2; // VQF
                                gyro.prevent_recompute = false;
                                gyro.adjust_offsets();
                                stab2.keyframes.write().update_gyro(&gyro);
                            }
                        });

                        let (sw, sh) = (
                            (proc_height as f64 * (size.0 as f64 / size.1 as f64)).round() as u32,
                            proc_height as u32,
                        );

                        let gpu_decoding = stab.gpu_decoding.load(SeqCst);

                        let mut frame_no = 0;
                        let mut abs_frame_no = 0;
                        let sync = Arc::new(sync);

                        let mut decoder_options = ffmpeg_next::Dictionary::new();
                        if proc_height > 0 {
                            decoder_options.set(
                                "scale",
                                &format!("{}x{}", (proc_height * 16) / 9, proc_height),
                            );
                        }

                        if input_file.image_sequence_fps > 0.0 {
                            let fps = if input_file.image_sequence_fps.fract() > 0.1 {
                                ffmpeg_next::Rational::new((fps * 1001.0).round() as i32, 1001)
                            } else {
                                ffmpeg_next::Rational::new(fps.round() as i32, 1)
                            };
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
                        if cfg!(target_os = "android") {
                            decoder_options.set("ndk_codec", "1");
                        }
                        ::log::debug!("Decoder options: {:?}", decoder_options);

                        match VideoProcessor::from_file(
                            &url,
                            gpu_decoding,
                            0,
                            Some(decoder_options),
                        ) {
                            Ok(mut proc) => {
                                let err2 = err.clone();
                                let sync2 = sync.clone();
                                proc.on_frame(
                                    move |timestamp_us,
                                          input_frame,
                                          _output_frame,
                                          converter,
                                          _rate_control| {
                                        if abs_frame_no % every_nth_frame == 0 {
                                            // NeuFlow (of_method=3 or 4) needs NV12 for color data;
                                            // other methods use GRAY8.
                                            let pix_fmt = if of_method == 3 || of_method == 4 {
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
                                                    let (width, height, stride, pixels) = if of_method == 3 || of_method == 4 {
                                                        // NV12: pass all planes (Y + UV)
                                                        let total_len = small_frame.stride(0) * small_frame.plane_height(0) as usize
                                                                      + small_frame.stride(1) * small_frame.plane_height(1) as usize;
                                                        let mut all_data = Vec::with_capacity(total_len);
                                                        all_data.extend_from_slice(&small_frame.data(0)[..small_frame.stride(0) * small_frame.plane_height(0) as usize]);
                                                        all_data.extend_from_slice(&small_frame.data(1)[..small_frame.stride(1) * small_frame.plane_height(1) as usize]);
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
                                if let Err(e) =
                                    proc.start_decoder_only(sync.get_ranges(), cancel_flag)
                                {
                                    err(("An error occured: %1".to_string(), e.to_string()));
                                }

                                sync.finished_feeding_frames();
                            }
                            Err(error) => {
                                err(("An error occured: %1".to_string(), error.to_string()));
                            }
                        };
                    } else {
                        let detail = format!(
                            "Invalid autosync parameters (queue apply): {sync_failure_detail}"
                        );
                        ::log::warn!(
                            "[autosync] queue apply rejected for '{}': {detail}",
                            filesystem::get_filename(&url)
                        );
                        err(("An error occured: %1".to_string(), detail));
                    }

                    stab.recompute_blocking();
                }
            }
            processing_cb(1.0);
            // --------------------------------- Autosync ---------------------------------
            // ----------------------------------------------------------------------------
        }
    }

    pub fn apply_to_all(&mut self, data: String, additional_data: String, to_job_id: u32) {
        ::log::debug!("Applying preset {}", &data);
        let data_parsed: serde_json::Result<serde_json::Value> = serde_json::from_str(&data);
        let mut new_output_options = None;
        if let Ok(obj) = &data_parsed {
            if let Some(output) = obj.get("output") {
                new_output_options = Some(output.clone());
            }
        }
        let processing_done =
            util::qt_queued_callback_mut(QPointer::from(self as &Self), |this, job_id: u32| {
                this.processing_done(job_id, true);
            });
        let err = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, (job_id, msg): (u32, String)| {
                this.error(
                    job_id,
                    QString::from(msg),
                    QString::default(),
                    QString::default(),
                );
            },
        );
        ::log::debug!("new_output_options: {:?}", &new_output_options);
        let data = data.as_bytes();
        let data_vec = data.to_vec();
        // Snapshot overwrite_mode before taking the mutable borrow of self.jobs.
        let overwrite_mode = self.overwrite_mode;
        let mut q = self.queue.borrow_mut();
        for (job_id, job) in self.jobs.iter_mut() {
            if to_job_id > 0 && *job_id != to_job_id {
                continue;
            }
            if job.queue_index < q.row_count() as usize {
                let mut itm = q[job.queue_index].clone();
                if itm.status == JobStatus::Queued {
                    let mut sync_options = serde_json::Value::default();
                    if let Ok(additional_data) = serde_json::from_str(&additional_data)
                        as serde_json::Result<serde_json::Value>
                    {
                        if let Some(sync) = additional_data.get("synchronization") {
                            sync_options = sync.clone();
                        }
                    }
                    if let Ok(obj) = &data_parsed {
                        if let Some(sync) = obj.get("synchronization") {
                            sync_options = sync.clone();
                        }
                    }
                    let job_id = *job_id;
                    if let Some(ref new_output_options) = new_output_options {
                        let override_ext = new_output_options
                            .get("output_extension")
                            .and_then(|x| x.as_str());
                        job.render_options.update_from_json(new_output_options);
                        job.render_options.output_folder = Self::get_output_folder(
                            &itm.input_file.to_string(),
                            &job.render_options.output_folder,
                        );
                        job.render_options.output_filename = Self::get_output_filename(
                            &itm.input_file.to_string(),
                            &self.default_suffix.to_string(),
                            &job.render_options,
                            override_ext,
                        );
                        if let Some(ref stab) = job.stab {
                            itm.export_settings = QString::from(
                                job.render_options.settings_string(stab.params.read().fps),
                            );
                        }
                        itm.output_filename =
                            QString::from(job.render_options.output_filename.as_str());
                        itm.output_folder =
                            QString::from(job.render_options.output_folder.as_str());
                        itm.display_output_path =
                            QString::from(filesystem::display_folder_filename(
                                job.render_options.output_folder.as_str(),
                                job.render_options.output_filename.as_str(),
                            ));
                        // overwrite_mode == 1 (Simple silent overwrite) skips the file-exists check.
                        if overwrite_mode != 1
                            && filesystem::exists_in_folder(
                                &job.render_options.output_folder,
                                &job.render_options
                                    .output_filename
                                    .replace("_%05d", "_00001"),
                            )
                        {
                            let msg = QString::from(format!(
                                "file_exists:{}",
                                serde_json::json!({ "filename": job.render_options.output_filename, "folder": job.render_options.output_folder })
                            ));
                            itm.error_string = msg.clone();
                            itm.status = JobStatus::Error;
                            err((job_id, msg.to_string()));
                        }
                    }

                    if let Some(ref stab) = job.stab {
                        let mut is_preset = false;
                        if let Err(e) = stab.import_gyroflow_data(
                            &data_vec,
                            true,
                            None,
                            |_| (),
                            Arc::new(AtomicBool::new(false)),
                            &mut is_preset,
                            false,
                        ) {
                            ::log::error!("Failed to update queue stab data: {:?}", e);
                        }

                        Self::update_sync_settings(stab, &sync_options);
                        job.project_data = Self::get_gyroflow_data_internal(
                            stab,
                            &job.additional_data,
                            &job.render_options,
                        );
                    }
                    processing_done(job_id);

                    q.change_line(job.queue_index, itm);
                }
            }
        }
    }

    fn file_exists_in_folder(&self, folder: QUrl, filename: QString) -> bool {
        let folder = QString::from(folder).to_string();
        let filename = filename.to_string();
        for (_id, job) in self.jobs.iter() {
            if job.render_options.output_folder == folder
                && job.render_options.output_filename == filename
            {
                return true;
            }
        }
        false
    }

    fn get_default_encoder(&self, codec: String, gpu: bool) -> String {
        rendering::get_default_encoder(&codec, gpu)
    }
    fn get_encoder_options(&self, encoder: String) -> String {
        rendering::get_encoder_options(&encoder)
    }
    fn get_next_item_id(&self, job_id: u32) -> u32 {
        let q = self.queue.borrow();
        let mut qiter = q.iter();
        while let Some(itm) = qiter.next() {
            if job_id == itm.job_id {
                return qiter.next().map(|itm| itm.job_id).unwrap_or(0);
            }
        }
        0
    }
    fn get_prev_item_id(&self, job_id: u32) -> u32 {
        let q = self.queue.borrow();
        let mut qiter = q.iter();
        let mut prev_id = 0;
        while let Some(itm) = qiter.next() {
            if job_id == itm.job_id {
                return prev_id;
            }
            prev_id = itm.job_id;
        }
        0
    }

    // Keep in sync with Synchronization.qml
    fn resolve_syncpoint_pattern(o: &serde_json::Value, duration: f64, fps: f64) -> Vec<f64> {
        fn resolve_duration_to_ms(d: &serde_json::Value, fps: f64) -> Option<f64> {
            if !d.is_number() && !d.is_string() {
                return None;
            }
            if d.is_string() && d.as_str()?.ends_with("ms") {
                d.as_str()?.strip_suffix("ms")?.parse::<f64>().ok()
            } else if d.is_string() && d.as_str()?.ends_with('s') {
                d.as_str()?
                    .strip_suffix('s')?
                    .parse::<f64>()
                    .ok()
                    .map(|x| x * 1000.0)
            } else if d.is_string() {
                d.as_str()?.parse::<f64>().ok().map(|x| (x / fps) * 1000.0)
            } else {
                d.as_f64().map(|x| (x / fps) * 1000.0)
            }
        }
        fn resolve_item(x: &serde_json::Value, duration: f64, fps: f64) -> Vec<f64> {
            if let Some(x) = x.as_object() {
                let start = x
                    .get("start")
                    .and_then(|y| resolve_duration_to_ms(y, fps))
                    .unwrap_or_default();
                let interval = x
                    .get("interval")
                    .and_then(|y| resolve_duration_to_ms(y, fps))
                    .unwrap_or(duration);
                let gap = x
                    .get("gap")
                    .and_then(|y| resolve_duration_to_ms(y, fps))
                    .unwrap_or_default();
                let mut out = Vec::new();
                let mut i = start;
                while i < duration {
                    out.push(i - gap / 2.0);
                    if gap > 0.0 {
                        out.push(i + gap / 2.0);
                    }
                    i += interval;
                }
                out
            } else {
                Vec::new()
            }
        }

        let mut timestamps = Vec::new();
        if let Some(array) = o.as_array() {
            for x in array {
                timestamps.append(&mut resolve_item(x, duration, fps));
            }
        } else if o.is_object() {
            timestamps.append(&mut resolve_item(o, duration, fps));
        }
        timestamps.sort_by(|a, b| a.total_cmp(b));

        timestamps
    }

    fn update_sync_settings(stab: &StabilizationManager, sync_options: &serde_json::Value) {
        let mut sync_settings = stab
            .lens
            .read()
            .sync_settings
            .clone()
            .unwrap_or(sync_options.clone());
        if sync_settings.is_object() && sync_options.is_object() {
            crate::core::util::merge_json(&mut sync_settings, sync_options);
        }
        if sync_settings.is_object() && !sync_settings.as_object().unwrap().is_empty() {
            stab.lens.write().sync_settings = Some(sync_settings);
        }
    }

    // --- Batch gyro matching methods ---

    fn get_ordered_job_ids(&self) -> Vec<u32> {
        if let Ok(queue) = self.queue.try_borrow() {
            (0..queue.row_count())
                .map(|i| queue[i as usize].job_id)
                .collect()
        } else {
            Vec::new()
        }
    }

    // T1: Add a gyro file to the list and start background parsing (T2).
    fn add_gyro_file(&mut self, url: String) {
        let filename = url
            .rsplit('/')
            .next()
            .or_else(|| url.rsplit('\\').next())
            .unwrap_or(&url)
            .to_string();
        let index = self.gyro_files.len();
        self.gyro_files.push(GyroFileInfo {
            path: url.clone(),
            filename,
            ..Default::default()
        });
        self.gyro_files_changed();

        // T2: Background metadata parsing
        let on_parsed = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, result: (Option<i64>, Option<f64>, Option<String>, Option<String>)| {
                if let Some(info) = this.gyro_files.get_mut(index) {
                    info.created_at_ms = result.0;
                    info.duration_ms = result.1;
                    info.detected_source = result.2.clone();
                    info.error = result.3.clone();
                    info.parsed = true;
                }
                this.gyro_files_changed();
            },
        );

        core::run_threaded(move || {
            let t = std::time::Instant::now();
            match parse_gyro_metadata(&url) {
                Ok((created_at, duration, detected_source)) => {
                    ::log::info!(
                        "[add_gyro_file] parsed '{}': {:.1}ms (created_at={}, duration={:.0}ms)",
                        filesystem::get_filename(&url),
                        t.elapsed().as_secs_f64() * 1000.0,
                        created_at,
                        duration
                    );
                    on_parsed((Some(created_at), Some(duration), detected_source, None));
                }
                Err(e) => {
                    ::log::warn!(
                        "[add_gyro_file] parse failed '{}': {:.1}ms {:?}",
                        filesystem::get_filename(&url),
                        t.elapsed().as_secs_f64() * 1000.0,
                        e
                    );
                    on_parsed((None, None, None, Some(e.to_string())));
                }
            }
        });
    }

    // [queue-pair-ux T3] 文件夹递归遍历，添加所有 *_mix.bin 陀螺仪文件
    fn add_gyro_folder(&mut self, folder_url: String) {
        let path = filesystem::url_to_path(&folder_url);
        let dir = std::path::Path::new(&path);
        if !dir.is_dir() {
            ::log::warn!("[add_gyro_folder] 路径不是目录，忽略: {}", path);
            return;
        }
        ::log::info!("[add_gyro_folder] 开始扫描文件夹: {}", path);
        let files = self.scan_gyro_folder(dir, 0);
        ::log::info!(
            "[add_gyro_folder] 扫描完成，共找到 {} 个 _mix.bin 文件",
            files.len()
        );
        for f in files {
            let url = filesystem::path_to_url(&f.to_string_lossy());
            self.add_gyro_file(url);
        }
    }

    fn scan_gyro_folder(&self, dir: &std::path::Path, depth: usize) -> Vec<std::path::PathBuf> {
        let mut result = Vec::new();
        if depth > 3 {
            return result;
        }
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                let mut files: Vec<std::path::PathBuf> = Vec::new();
                let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        subdirs.push(p);
                    } else if p
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.ends_with("_mix.bin"))
                        .unwrap_or(false)
                    {
                        files.push(p);
                    }
                }
                files.sort();
                subdirs.sort();
                ::log::info!(
                    "[scan_gyro_folder] depth={}, dir={}, 文件={}, 子目录={}",
                    depth,
                    dir.display(),
                    files.len(),
                    subdirs.len()
                );
                result.extend(files);
                for d in subdirs {
                    result.extend(self.scan_gyro_folder(&d, depth + 1));
                }
            }
            Err(e) => {
                ::log::warn!("[scan_gyro_folder] 无法读取目录 {}: {:?}", dir.display(), e);
            }
        }
        result
    }

    fn remove_gyro_file(&mut self, index: usize) {
        if index < self.gyro_files.len() {
            self.gyro_files.remove(index);
            self.match_results = None;
            self.gyro_files_changed();
            self.match_results_changed();
        }
    }

    fn clear_gyro_files(&mut self) {
        // Clear external gyro data from all jobs
        for (_, job) in &self.jobs {
            if let Some(stab) = &job.stab {
                let is_external = {
                    let gyro = stab.gyro.read();
                    !gyro.file_url.is_empty() && gyro.file_url != stab.input_file.read().url
                };
                if is_external {
                    let mut gyro = stab.gyro.write();
                    gyro.clear();
                    gyro.file_url.clear();
                }
            }
        }
        // Update project_data for all jobs
        for (_, job) in &mut self.jobs {
            if let Some(ref stab) = job.stab {
                job.project_data = Self::get_gyroflow_data_internal(
                    stab,
                    &job.additional_data,
                    &job.render_options,
                );
            }
        }

        ::log::info!(
            "[clear_gyro_files] cleared {} gyro files and caches",
            self.gyro_files.len()
        );
        self.gyro_files.clear();
        self.match_results = None;
        self.stabilizer.clear_lens_group_status();
        self.manual_pairs.clear();
        self.restore_original_order();
        self.gyro_files_changed();
        self.match_results_changed();
    }

    fn get_gyro_file_count(&self) -> usize {
        self.gyro_files.len()
    }

    fn has_gyro_files(&self) -> bool {
        !self.gyro_files.is_empty()
    }

    fn get_gyro_file_info_json(&self, index: usize) -> QString {
        if let Some(info) = self.gyro_files.get(index) {
            QString::from(
                serde_json::json!({
                    "path": info.path,
                    "filename": info.filename,
                    "created_at_ms": info.created_at_ms,
                    "duration_ms": info.duration_ms,
                    "detected_source": info.detected_source,
                    "parsed": info.parsed,
                    "error": info.error,
                })
                .to_string(),
            )
        } else {
            QString::default()
        }
    }

    // T3: Collect metadata and run batch matching algorithm.
    fn batch_match_gyro(&mut self) {
        let t_total = std::time::Instant::now();
        self.stabilizer.clear_lens_group_status();

        // [queue-render-skip] 重新 match 前，清除所有已有的 Skipped 标记
        {
            let q = self.queue.borrow();
            let skipped_ids: Vec<u32> = q
                .iter()
                .filter(|v| v.status == JobStatus::Skipped)
                .map(|v| v.job_id)
                .collect();
            drop(q);
            for job_id in skipped_ids {
                update_model!(self, job_id, itm {
                    itm.skip_reason = QString::default();
                    itm.status = JobStatus::Queued;
                });
            }
        }

        let t0 = std::time::Instant::now();
        let job_ids = self.get_ordered_job_ids();
        ::log::info!(
            "[batch_match T16] ordered job_ids at match start: {:?}",
            job_ids
        );

        // Collect video metadata from jobs
        let mut videos = Vec::new();
        for (vi, &job_id) in job_ids.iter().enumerate() {
            if let Some(job) = self.jobs.get(&job_id) {
                if let Some(stab) = &job.stab {
                    let params = stab.params.read();
                    let created_at = params.video_created_at;
                    ::log::info!(
                        "[batch_match T20] video[{}] job_id={}, created_at={:?}, file={}",
                        vi,
                        job_id,
                        created_at,
                        filesystem::get_filename(&stab.input_file.read().url)
                    );
                    videos.push(core::gyro_match::VideoMatchInfo {
                        path: stab.input_file.read().url.clone(),
                        duration_ms: params.duration_ms,
                        created_at_ms: created_at,
                        pre_recording_ms: 0.0,
                    });
                } else {
                    // [T20] stab 已释放（渲染完成后），使用 job.video_created_at fallback
                    let created_at = job.video_created_at;
                    ::log::info!(
                        "[batch_match T20] video[{}] job_id={}, created_at={:?} (stab released, using cached)",
                        vi,
                        job_id,
                        created_at
                    );
                    // [match-regression] Use a sentinel duration ABOVE the 10s calibration
                    // threshold so find_calibration_videos skips this Finished job. A real
                    // duration is not recoverable here (stab released), and the prior 0.0
                    // wrongly qualified every already-rendered job as a calibration candidate,
                    // which polluted global_offset and caused every *subsequent* video in the
                    // queue to fall outside the resulting gyro time range and be marked
                    // Skipped ("no_gyro"). Reported symptom: "after rendering one video on
                    // the main canvas, adding more to the queue then auto-matching skips
                    // everything past that video."
                    videos.push(core::gyro_match::VideoMatchInfo {
                        path: job.render_options.input_url.clone(),
                        duration_ms: 10_001.0,
                        created_at_ms: created_at,
                        pre_recording_ms: 0.0,
                    });
                }
            }
        }

        // Collect gyro metadata (only parsed files with valid data)
        let gyros: Vec<_> = self
            .gyro_files
            .iter()
            .filter(|g| g.parsed && g.created_at_ms.is_some() && g.duration_ms.is_some())
            .map(|g| core::gyro_match::GyroMatchInfo {
                path: g.path.clone(),
                duration_ms: g.duration_ms.unwrap(),
                created_at_ms: g.created_at_ms.unwrap(),
            })
            .collect();
        ::log::info!(
            "[batch_match] collect metadata: {:.1}ms ({} videos, {} gyros parsed/{} total)",
            t0.elapsed().as_secs_f64() * 1000.0,
            videos.len(),
            gyros.len(),
            self.gyro_files.len()
        );

        // 将 manual_pairs 中的 job_id 转换为当前队列中的 video_index
        // 这样即使 remove/sort 改变了队列顺序，pair 关系仍然正确
        let mut resolved_pairs: Vec<core::gyro_match::ManualCalibrationPair> = Vec::new();
        for p in &self.manual_pairs {
            if let Some(video_index) = job_ids.iter().position(|&id| id == p.job_id) {
                resolved_pairs.push(core::gyro_match::ManualCalibrationPair {
                    job_id: p.job_id,
                    video_index,
                    gyro_index: p.gyro_index,
                });
                ::log::info!(
                    "[batch_match] manual pair: job_id={} -> video_index={}, gyro_index={}",
                    p.job_id,
                    video_index,
                    p.gyro_index
                );
            } else {
                ::log::warn!(
                    "[batch_match] manual pair job_id={} not found in queue, skipping",
                    p.job_id
                );
            }
        }
        let manual = if resolved_pairs.is_empty() {
            None
        } else {
            Some(resolved_pairs.as_slice())
        };

        let t1 = std::time::Instant::now();
        let mut result = core::gyro_match::batch_match(&videos, &gyros, manual);
        ::log::info!(
            "[batch_match] algorithm: {:.1}ms (offset={:?}, error={:?})",
            t1.elapsed().as_secs_f64() * 1000.0,
            result.global_offset_ms,
            result.error
        );
        for r in &result.results {
            if r.gyro_index.is_some() {
                ::log::info!(
                    "[batch_match]   video[{}] -> gyro[{}] {:?} range=[{:.0?}..{:.0?}]",
                    r.video_index,
                    r.gyro_index.unwrap(),
                    r.status,
                    r.gyro_start_ms,
                    r.gyro_end_ms
                );
            }
        }
        // [queue-lifecycle T4] 为每个 match result 填入 job_id，以便 remove 后仍能按 job_id 查找
        for r in &mut result.results {
            r.job_id = job_ids.get(r.video_index).copied();
        }
        self.match_results = Some(result);
        self.match_results_changed();

        let t2 = std::time::Instant::now();
        self.apply_match_results();
        ::log::info!(
            "[batch_match] apply_match_results setup: {:.1}ms",
            t2.elapsed().as_secs_f64() * 1000.0
        );
        ::log::info!(
            "[batch_match] total (main thread): {:.1}ms",
            t_total.elapsed().as_secs_f64() * 1000.0
        );
    }

    // T4: Apply match results by loading gyro data into each matched job.
    // Runs heavy gyro parsing on a background thread to avoid blocking the UI.
    fn apply_match_results(&mut self) {
        self.apply_match_results_filtered(None);
    }

    fn reapply_batch_auto_rotate(&mut self, job_ids_json: String) {
        let job_ids: HashSet<u32> = match serde_json::from_str::<Vec<u32>>(&job_ids_json) {
            Ok(ids) => ids.into_iter().collect(),
            Err(_) => return,
        };
        if job_ids.is_empty() {
            return;
        }
        self.apply_match_results_filtered(Some(job_ids));
    }

    fn reapply_lens_group_config(&mut self) {
        self.reapply_lens_group_config_filtered(None);
    }

    fn reapply_lens_group_config_filtered(&mut self, filter_job_ids: Option<HashSet<u32>>) {
        let global_configs = self.stabilizer.lens_group_config.read().clone();

        let items: Vec<(
            u32,
            Arc<StabilizationManager>,
            String,
            RenderOptions,
            (usize, usize),
            JobLensMetadataBackup,
            String,
            Vec<niyien_lens_presets::LensGroupConfig>,
        )> = self
            .jobs
            .iter()
            .filter_map(|(job_id, job)| {
                if let Some(filter_job_ids) = filter_job_ids.as_ref() {
                    if !filter_job_ids.contains(job_id) {
                        return None;
                    }
                }

                let stab = job.stab.as_ref()?.clone();
                let base_lens_metadata = job.base_lens_metadata.clone().or_else(|| {
                    let gyro = stab.gyro.read();
                    let md = gyro.file_metadata.read();
                    Some(JobLensMetadataBackup::from_metadata(&md))
                })?;
                let gyro_file_url = {
                    let gyro = stab.gyro.read();
                    gyro.file_url.clone()
                };
                Some((
                    *job_id,
                    stab,
                    job.additional_data.clone(),
                    job.render_options.clone(),
                    job.base_render_output_size.unwrap_or((
                        job.render_options.output_width,
                        job.render_options.output_height,
                    )),
                    base_lens_metadata,
                    gyro_file_url,
                    effective_lens_group_configs(job, &global_configs),
                ))
            })
            .collect();

        if items.is_empty() {
            return;
        }

        ::log::info!(
            "[reapply_lens_group_config] starting for {} jobs",
            items.len()
        );

        let on_done = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this,
                  job_updates: Vec<(
                u32,
                Option<String>,
                RenderOptions,
                (usize, usize),
                Option<usize>,
            )>| {
                let updated_count = job_updates
                    .iter()
                    .filter(|(_, data, _, _, _)| data.is_some())
                    .count();
                for (job_id, project_data, render_options, base_output_size, lens_group_index) in
                    job_updates
                {
                    if let Some(job) = this.jobs.get_mut(&job_id) {
                        if let Some(data) = project_data {
                            job.project_data = Some(data);
                        }
                        job.render_options = render_options;
                        job.base_render_output_size = Some(base_output_size);
                        job.lens_group_index = lens_group_index;
                    }
                }
                this.match_results_changed();
                ::log::info!(
                    "[reapply_lens_group_config] done, updated {} jobs",
                    updated_count
                );
            },
        );

        core::run_threaded(move || {
            let job_updates: Vec<(u32, Option<String>, RenderOptions, (usize, usize), Option<usize>)> =
                items
                    .par_iter()
                    .filter_map(
                        |(
                            job_id,
                            stab,
                            additional_data,
                            render_options,
                            base_output_size,
                            base_lens_metadata,
                            gyro_file_url,
                            effective_configs,
                        )| {
                            let (lens_index, size) = {
                                let gyro = stab.gyro.read();
                                let md = gyro.file_metadata.read();
                                let li = niyien_lens_presets::extract_lens_index(&md.additional_data);
                                let sz = stab.params.read().size;
                                (li, sz)
                            };

                            let mut updated_render_options = render_options.clone();
                            updated_render_options.output_width = base_output_size.0;
                            updated_render_options.output_height = base_output_size.1;

                            let mut base_metadata = {
                                let gyro = stab.gyro.read();
                                let mut md = gyro.file_metadata.write();
                                base_lens_metadata.overwrite_metadata(&mut md);
                                let mut snapshot = md.thin();
                                base_lens_metadata.overwrite_metadata(&mut snapshot);
                                snapshot
                            };
                            let auto_focus =
                                niyien_lens_presets::extract_video_focus_length_mm(&base_metadata);

                            // Preserve sync_settings across lens profile replacement
                            let saved_sync_settings = stab.lens.read().sync_settings.clone();

                            stab.apply_main_video_telemetry(&mut base_metadata, gyro_file_url, true);
                            *stab.camera_id.write() = base_metadata.camera_identifier.clone();
                            if let Err(err) = stab.autoload_lens_from_camera_id() {
                                ::log::warn!(
                                    "[reapply_lens_group_config] job[{}] autoload lens profile failed: {}",
                                    job_id,
                                    err
                                );
                            }
                            sync_readout_params_from_lens(stab.as_ref());

                            // Restore sync_settings that may have been lost during lens replacement
                            if let Some(ss) = saved_sync_settings {
                                stab.lens.write().sync_settings = Some(ss);
                            }

                            if let Some(lens_index) = lens_index {
                                if let Some(group_config) = effective_configs.get(lens_index) {
                                    let selected_focal_length =
                                        resolve_lens_group_focal_length(auto_focus, Some(group_config));
                                    let manual_focus_length_mm = selected_focal_length.and_then(
                                        |(focal_length_mm, source)| {
                                            (source == niyien_lens_presets::FocalLengthSource::Manual)
                                                .then_some(focal_length_mm)
                                        },
                                    );

                                    if let Some(focal_length_mm) = manual_focus_length_mm {
                                        niyien_lens_presets::apply_focal_length_fallback_to_metadata(
                                            &mut base_metadata,
                                            focal_length_mm,
                                        );
                                        {
                                            let gyro = stab.gyro.read();
                                            let mut md = gyro.file_metadata.write();
                                            niyien_lens_presets::apply_focal_length_fallback_to_metadata(
                                                &mut md,
                                                focal_length_mm,
                                            );
                                        }
                                        stab.set_user_focal_length(focal_length_mm);
                                        ::log::info!(
                                            "[reapply_lens_group_config] job[{}] applied manual focal length {:.1}mm",
                                            job_id,
                                            focal_length_mm
                                        );
                                    }

                                    if group_config.anamorphic_enabled {
                                        let existing_lens = stab.lens.read().clone();
                                        let profile = niyien_lens_presets::build_lens_profile(
                                            &base_metadata,
                                            size,
                                            Some(group_config),
                                            Some(&existing_lens),
                                        );
                                        if let Some(profile) = profile {
                                            if let Some(output_dim) = profile.output_dimension.clone() {
                                                updated_render_options.output_width = output_dim.w;
                                                updated_render_options.output_height = output_dim.h;
                                            }
                                            *stab.lens.write() = profile;
                                            ::log::info!(
                                                "[reapply_lens_group_config] job[{}] applied anamorphic profile for group #{}",
                                                job_id,
                                                lens_index
                                            );
                                        }
                                    }

                                    // Mirror apply_lens_group_to_main: anamorphic ON honors the
                                    // per-group slider value (default 100 when unset); anamorphic
                                    // OFF always reverts to 100%. Without this the queue renders
                                    // differ from the live preview.
                                    let correction_percent = if group_config.anamorphic_enabled {
                                        group_config
                                            .lens_correction_amount
                                            .filter(|v| v.is_finite())
                                            .map(|v| v.clamp(0.0, 100.0) as f64)
                                            .unwrap_or(100.0)
                                    } else {
                                        100.0
                                    };
                                    stab.set_lens_correction_amount(correction_percent / 100.0);
                                }
                            }

                            stab.set_output_size(
                                updated_render_options.output_width,
                                updated_render_options.output_height,
                            );
                            sync_readout_params_from_lens(stab.as_ref());
                            stab.invalidate_smoothing();
                            stab.invalidate_zooming();
                            let additional_data_str = prepare_project_additional_data(
                                additional_data,
                                &updated_render_options,
                            );
                            let data = stab
                                .export_gyroflow_data(
                                    core::GyroflowProjectType::WithGyroData,
                                    &additional_data_str,
                                    None,
                                )
                                .ok();

                            Some((
                                *job_id,
                                data,
                                updated_render_options,
                                *base_output_size,
                                lens_index,
                            ))
                        },
                    )
                    .collect();

            on_done(job_updates);
        });
    }

    fn apply_match_results_filtered(&mut self, filter_job_ids: Option<HashSet<u32>>) {
        let results = match &self.match_results {
            Some(r) => r.results.clone(),
            None => return,
        };

        let global_lens_group_config = self.stabilizer.lens_group_config.read().clone();
        // Build a mapping from parsed gyro index back to gyro_files index.
        let parsed_gyro_indices: Vec<usize> = self
            .gyro_files
            .iter()
            .enumerate()
            .filter(|(_, g)| g.parsed && g.created_at_ms.is_some() && g.duration_ms.is_some())
            .map(|(i, _)| i)
            .collect();

        // Collect all info needed for background processing
        struct ApplyInfo {
            job_id: u32,
            gyro_files_idx: usize,
            gyro_path: String,
            gyro_start_ms: Option<f64>,
            gyro_end_ms: Option<f64>,
            additional_data: String,
            render_options: RenderOptions,
            base_render_output_size: (usize, usize),
            lens_group_index: Option<usize>,
            auto_rotate: bool,
            original_video_rotation: f64,
            original_output_size: (usize, usize),
            base_lens_metadata: Option<JobLensMetadataBackup>,
            effective_lens_group_configs: Vec<niyien_lens_presets::LensGroupConfig>,
            stab: Arc<StabilizationManager>,
        }
        #[derive(Clone)]
        struct GyroParseInfo {
            path: String,
            fps: f64,
            size: (usize, usize),
            requested_ranges: Vec<Option<(f64, f64)>>,
        }
        let mut apply_items: Vec<ApplyInfo> = Vec::new();
        let mut unique_gyro_paths: HashMap<usize, GyroParseInfo> = HashMap::new();

        for result in &results {
            let gyro_batch_idx = match result.gyro_index {
                Some(idx) => idx,
                None => continue,
            };
            if result.status == core::gyro_match::MatchStatus::Unmatched
                || result.status == core::gyro_match::MatchStatus::NoCreationTime
            {
                continue;
            }
            let gyro_files_idx = match parsed_gyro_indices.get(gyro_batch_idx) {
                Some(&idx) => idx,
                None => continue,
            };
            // Use pre-resolved job_id from batch_match (line 3159) instead of
            // re-looking up via video_index, because the queue order may have
            // changed between batch_match and apply_match_results (e.g. QML
            // sort_jobs_by_created_at triggered by match_results_changed signal).
            let job_id = match result.job_id {
                Some(id) => id,
                None => continue,
            };
            if let Some(filter_job_ids) = filter_job_ids.as_ref() {
                if !filter_job_ids.contains(&job_id) {
                    continue;
                }
            }
            let requested_range =
                normalize_time_range_ms(result.gyro_start_ms.zip(result.gyro_end_ms));
            let (
                stab,
                gyro_path,
                additional_data,
                render_options,
                base_render_output_size,
                auto_rotate,
                original_video_rotation,
                original_output_size,
                base_lens_metadata,
                effective_lens_group_configs,
            ) = match self.jobs.get(&job_id) {
                Some(job) => match (&job.stab, self.gyro_files.get(gyro_files_idx)) {
                    (Some(stab), Some(gyro_info)) => (
                        stab.clone(),
                        gyro_info.path.clone(),
                        job.additional_data.clone(),
                        job.render_options.clone(),
                        job.base_render_output_size.unwrap_or((
                            job.render_options.output_width,
                            job.render_options.output_height,
                        )),
                        job.auto_rotate,
                        job.original_video_rotation,
                        job.original_output_size,
                        job.base_lens_metadata.clone().or_else(|| {
                            let gyro = stab.gyro.read();
                            let md = gyro.file_metadata.read();
                            Some(JobLensMetadataBackup::from_metadata(&md))
                        }),
                        effective_lens_group_configs(job, &global_lens_group_config),
                    ),
                    _ => continue,
                },
                None => continue,
            };

            unique_gyro_paths
                .entry(gyro_files_idx)
                .and_modify(|entry| entry.requested_ranges.push(requested_range))
                .or_insert_with(|| {
                    let (fps, size) = {
                        let params = stab.params.read();
                        (params.fps, params.size)
                    };
                    GyroParseInfo {
                        path: gyro_path.clone(),
                        fps,
                        size,
                        requested_ranges: vec![requested_range],
                    }
                });

            apply_items.push(ApplyInfo {
                job_id,
                gyro_files_idx,
                gyro_path,
                gyro_start_ms: result.gyro_start_ms,
                gyro_end_ms: result.gyro_end_ms,
                additional_data,
                render_options,
                base_render_output_size,
                lens_group_index: None,
                auto_rotate,
                original_video_rotation,
                original_output_size,
                base_lens_metadata,
                effective_lens_group_configs,
                stab,
            });
        }

        // 收集已有缓存的 telemetry 区间，传入后台线程避免重复解析
        let existing_caches: HashMap<usize, Vec<CachedGyroMetadataRange>> = unique_gyro_paths
            .keys()
            .filter_map(|&idx| {
                self.gyro_files.get(idx).and_then(|info| {
                    (!info.cached_metadata_ranges.is_empty())
                        .then(|| (idx, info.cached_metadata_ranges.clone()))
                })
            })
            .collect();

        // Run heavy work on background thread
        let on_done = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this,
                  (job_updates, cache_updates, lens_group_status, t_bg_end): (
                Vec<(
                    u32,
                    Option<String>,
                    RenderOptions,
                    Option<JobLensMetadataBackup>,
                    (usize, usize),
                    Option<usize>,
                )>,
                Vec<(usize, Vec<CachedGyroMetadataRange>)>,
                Vec<niyien_lens_presets::LensGroupStatus>,
                std::time::Instant,
            )| {
                let t_cb = std::time::Instant::now();
                ::log::info!(
                    "[apply_match] bg->main callback delay: {:.1}ms",
                    (t_cb - t_bg_end).as_secs_f64() * 1000.0
                );

                let t_cache_write = std::time::Instant::now();
                for (idx, new_entries) in cache_updates {
                    if let Some(info) = this.gyro_files.get_mut(idx) {
                        merge_metadata_cache_entries(&mut info.cached_metadata_ranges, new_entries);
                        if let Some(best_entry) =
                            info.cached_metadata_ranges.iter().max_by(|a, b| {
                                time_range_span_ms(a.range_ms)
                                    .partial_cmp(&time_range_span_ms(b.range_ms))
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                        {
                            info.cached_metadata = Some(Arc::clone(&best_entry.metadata));
                        }
                        ::log::info!(
                            "[apply_match] cached metadata ranges stored for gyro {}: {} entries",
                            idx,
                            info.cached_metadata_ranges.len()
                        );
                    }
                }
                ::log::info!(
                    "[apply_match] cache writeback: {:.1}ms",
                    t_cache_write.elapsed().as_secs_f64() * 1000.0
                );

                this.stabilizer.set_lens_group_status(lens_group_status);

                let applied_job_ids: Vec<u32> = job_updates
                    .iter()
                    .map(|(job_id, _, _, _, _, _)| *job_id)
                    .collect();
                let t_project = std::time::Instant::now();
                for (
                    job_id,
                    project_data,
                    render_options,
                    base_lens_metadata,
                    base_output_size,
                    lens_group_index,
                ) in job_updates
                {
                    let mut export_settings = None;
                    if let Some(job) = this.jobs.get_mut(&job_id) {
                        if let Some(data) = project_data {
                            job.project_data = Some(data);
                        }
                        job.render_options = render_options;
                        job.base_render_output_size = Some(base_output_size);
                        job.lens_group_index = lens_group_index;
                        if let Some(base_lens_metadata) = base_lens_metadata {
                            job.base_lens_metadata = Some(base_lens_metadata);
                        }
                        if let Some(ref stab) = job.stab {
                            export_settings =
                                Some(job.render_options.settings_string(stab.params.read().fps));
                        }
                    }
                    if let Some(export_settings) = export_settings {
                        update_model!(this, job_id, itm {
                            itm.export_settings = QString::from(export_settings.as_str());
                        });
                    }
                }
                ::log::info!(
                    "[apply_match] project/render_options update: {:.1}ms ({} jobs)",
                    t_project.elapsed().as_secs_f64() * 1000.0,
                    applied_job_ids.len()
                );

                let t_sort = std::time::Instant::now();
                this.sort_jobs_by_created_at();
                ::log::info!(
                    "[apply_match] sort_jobs_by_created_at: {:.1}ms",
                    t_sort.elapsed().as_secs_f64() * 1000.0
                );

                let t_cache = std::time::Instant::now();
                // [T22] 排序完成后构建 sameGyro 缓存
                this.build_same_gyro_cache();
                ::log::info!(
                    "[apply_match] build_same_gyro_cache: {:.1}ms",
                    t_cache.elapsed().as_secs_f64() * 1000.0
                );

                // [queue-render-skip] 标记无陀螺仪数据和校准对的视频为 Skipped
                if let Some(ref match_results) = this.match_results {
                    let job_ids_now = this.get_ordered_job_ids();
                    for result in &match_results.results {
                        let job_id = result
                            .job_id
                            .or_else(|| job_ids_now.get(result.video_index).copied());
                        if let Some(job_id) = job_id {
                            match result.status {
                                core::gyro_match::MatchStatus::Unmatched
                                | core::gyro_match::MatchStatus::NoCreationTime => {
                                    update_model!(this, job_id, itm {
                                        itm.skip_reason = QString::from("no_gyro");
                                        itm.status = JobStatus::Skipped;
                                    });
                                    ::log::info!(
                                        "[queue-render-skip] job {} marked Skipped (no_gyro)",
                                        job_id
                                    );
                                }
                                core::gyro_match::MatchStatus::CalibrationPair => {
                                    update_model!(this, job_id, itm {
                                        itm.skip_reason = QString::from("calibration");
                                        itm.status = JobStatus::Skipped;
                                    });
                                    ::log::info!(
                                        "[queue-render-skip] job {} marked Skipped (calibration)",
                                        job_id
                                    );
                                }
                                _ => {}
                            }
                        }
                    }
                }

                this.match_results_changed();
                // [T22] 数据加载全部完成，触发专用信号（遮罩在此关闭）
                this.match_apply_finished();
                ::log::info!(
                    "[apply_match] on_done callback total: {:.1}ms",
                    t_cb.elapsed().as_secs_f64() * 1000.0
                );
            },
        );

        // Default optical flow: OpenCV DIS (method=2). neuflow feature 关闭时
        // 依然可用；开启时用户可在 Advanced 下拉手动切到 NeuFlow。
        let default_of_method: u64 = 2;

        core::run_threaded(move || {
            let t_bg = std::time::Instant::now();
            let lens_group_status = Arc::new(ParkingMutex::new(
                niyien_lens_presets::default_lens_group_statuses(),
            ));

            // 按区间缓存 telemetry 数据，避免超大 gyro 文件被整段解析
            let mut gyro_cache = existing_caches.clone();
            let mut cache_hit = 0usize;
            let mut parsed_chunks = 0usize;
            let mut cache_updates: Vec<(usize, Vec<CachedGyroMetadataRange>)> = Vec::new();
            let mut parse_jobs: Vec<(usize, GyroParseInfo)> =
                unique_gyro_paths.into_iter().collect();
            parse_jobs.sort_by_key(|(idx, _)| *idx);

            for (gyro_files_idx, parse_info) in parse_jobs {
                let existing_entries = gyro_cache.get(&gyro_files_idx).cloned().unwrap_or_default();
                let parse_requests =
                    build_parse_requests(&parse_info.requested_ranges, &existing_entries);
                if parse_requests.is_empty() {
                    if !existing_entries.is_empty() {
                        cache_hit += 1;
                        ::log::info!(
                            "[apply_match] using cached metadata for gyro {} ({} cached ranges)",
                            gyro_files_idx,
                            existing_entries.len()
                        );
                    }
                    continue;
                }

                let total_chunks = parse_requests.len();
                ::log::info!(
                    "[apply_match] parsing gyro file {} mode={} chunks={}",
                    gyro_files_idx,
                    if total_chunks > 1 {
                        "chunked"
                    } else if parse_requests[0].is_none() {
                        "full"
                    } else {
                        "range"
                    },
                    total_chunks
                );

                let mut new_entries = Vec::new();
                let mut fallback_to_full_parse = false;
                for (chunk_idx, request_range) in parse_requests.into_iter().enumerate() {
                    let t_parse = std::time::Instant::now();
                    match filesystem::open_file(&parse_info.path, false, false) {
                        Ok(mut file) => {
                            let filesize = file.size;
                            let load_options = core::gyro_source::FileLoadOptions {
                                time_range_ms: request_range,
                                ..Default::default()
                            };
                            match GyroSource::parse_telemetry_file(
                                file.get_file(),
                                filesize,
                                &parse_info.path,
                                &load_options,
                                parse_info.size,
                                parse_info.fps,
                                |_| {},
                                Arc::new(AtomicBool::new(false)),
                            ) {
                                Ok(md) => {
                                    ::log::info!(
                                        "[apply_match] parse gyro[{}] chunk {}/{} '{}': {:.1}ms ({} imu samples, {} quats, range={:?})",
                                        gyro_files_idx,
                                        chunk_idx + 1,
                                        total_chunks,
                                        filesystem::get_filename(&parse_info.path),
                                        t_parse.elapsed().as_secs_f64() * 1000.0,
                                        md.raw_imu.len(),
                                        md.quaternions.len(),
                                        request_range
                                    );
                                    new_entries.push(CachedGyroMetadataRange {
                                        range_ms: request_range,
                                        metadata: Arc::new(md),
                                    });
                                    parsed_chunks += 1;
                                }
                                Err(e) => {
                                    fallback_to_full_parse = true;
                                    ::log::warn!(
                                        "[apply_match] parse gyro[{}] chunk {}/{} '{}' failed after {:.1}ms, fallback to full parse: {} (range={:?})",
                                        gyro_files_idx,
                                        chunk_idx + 1,
                                        total_chunks,
                                        filesystem::get_filename(&parse_info.path),
                                        t_parse.elapsed().as_secs_f64() * 1000.0,
                                        e,
                                        request_range
                                    );
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            fallback_to_full_parse = true;
                            ::log::warn!(
                                "[apply_match] open gyro[{}] '{}' failed after {:.1}ms, fallback to full parse: {} (range={:?})",
                                gyro_files_idx,
                                filesystem::get_filename(&parse_info.path),
                                t_parse.elapsed().as_secs_f64() * 1000.0,
                                e,
                                request_range
                            );
                            break;
                        }
                    }
                }

                if fallback_to_full_parse
                    && !existing_entries
                        .iter()
                        .any(|entry| entry.range_ms.is_none())
                {
                    let t_parse = std::time::Instant::now();
                    match filesystem::open_file(&parse_info.path, false, false) {
                        Ok(mut file) => {
                            let filesize = file.size;
                            match GyroSource::parse_telemetry_file(
                                file.get_file(),
                                filesize,
                                &parse_info.path,
                                &Default::default(),
                                parse_info.size,
                                parse_info.fps,
                                |_| {},
                                Arc::new(AtomicBool::new(false)),
                            ) {
                                Ok(md) => {
                                    ::log::info!(
                                        "[apply_match] parse gyro[{}] fallback full '{}': {:.1}ms ({} imu samples, {} quats)",
                                        gyro_files_idx,
                                        filesystem::get_filename(&parse_info.path),
                                        t_parse.elapsed().as_secs_f64() * 1000.0,
                                        md.raw_imu.len(),
                                        md.quaternions.len()
                                    );
                                    new_entries.clear();
                                    new_entries.push(CachedGyroMetadataRange {
                                        range_ms: None,
                                        metadata: Arc::new(md),
                                    });
                                    parsed_chunks += 1;
                                }
                                Err(e) => {
                                    ::log::warn!(
                                        "[apply_match] parse gyro[{}] fallback full '{}' failed after {:.1}ms: {}",
                                        gyro_files_idx,
                                        filesystem::get_filename(&parse_info.path),
                                        t_parse.elapsed().as_secs_f64() * 1000.0,
                                        e
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            ::log::warn!(
                                "[apply_match] open gyro[{}] fallback full '{}' failed after {:.1}ms: {}",
                                gyro_files_idx,
                                filesystem::get_filename(&parse_info.path),
                                t_parse.elapsed().as_secs_f64() * 1000.0,
                                e
                            );
                        }
                    }
                }

                if !new_entries.is_empty() {
                    merge_metadata_cache_entries(
                        gyro_cache.entry(gyro_files_idx).or_default(),
                        new_entries.clone(),
                    );
                    cache_updates.push((gyro_files_idx, new_entries));
                }
            }
            ::log::info!(
                "[apply_match] all gyro parsing: {:.1}ms ({} gyro files, {} cached, {} parsed chunks)",
                t_bg.elapsed().as_secs_f64() * 1000.0,
                gyro_cache.len(),
                cache_hit,
                parsed_chunks
            );

            // Apply cached gyro data to each job
            let t_apply = std::time::Instant::now();
            let gyro_cache = Arc::new(gyro_cache);
            let mut auto_rotation_results: HashMap<u32, Option<i32>> = HashMap::new();
            let mut auto_rotate_groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
            for (item_idx, item) in apply_items.iter().enumerate() {
                auto_rotate_groups
                    .entry(item.gyro_files_idx)
                    .or_default()
                    .push(item_idx);
            }
            let mut auto_rotate_state = core::gyro_source::SenseFlowAutoRotationState::default();
            for item_indices in auto_rotate_groups.values_mut() {
                item_indices.sort_by(|a, b| {
                    let a_start = apply_items[*a].gyro_start_ms.unwrap_or(f64::MIN);
                    let b_start = apply_items[*b].gyro_start_ms.unwrap_or(f64::MIN);
                    a_start
                        .partial_cmp(&b_start)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });

                for &item_idx in item_indices.iter() {
                    let item = &apply_items[item_idx];
                    let requested_range =
                        normalize_time_range_ms(item.gyro_start_ms.zip(item.gyro_end_ms));
                    let Some(cached_entries) = gyro_cache.get(&item.gyro_files_idx) else {
                        continue;
                    };
                    let Some(cache_entry) =
                        select_best_cached_metadata(cached_entries, requested_range)
                    else {
                        continue;
                    };

                    let adjusted_range_ms = get_adjusted_match_range_ms(
                        cache_entry.range_ms,
                        item.gyro_start_ms,
                        item.gyro_end_ms,
                    );
                    let md =
                        clone_metadata_for_job(cache_entry.metadata.as_ref(), adjusted_range_ms);
                    let detected_source = md.detected_source.as_deref().unwrap_or("");
                    let is_r3d = item.render_options.input_filename.to_ascii_lowercase().ends_with(".r3d");
                    let has_metadata_rotation = item.original_video_rotation.round() as i32 != 0 && !is_r3d;
                    if !is_r3d && !has_metadata_rotation && item.auto_rotate && detected_source.starts_with("SenseFlow") {
                        ::log::info!(
                            "[auto_rotate input] file='{}' adjusted_range_ms={:?} md_duration_ms={:.1} imu_samples={}",
                            item.render_options.input_filename,
                            adjusted_range_ms,
                            md.duration_ms,
                            md.raw_imu.len()
                        );
                        let rotation =
                            core::gyro_source::compute_auto_rotation_for_segment_with_state(
                                &mut auto_rotate_state,
                                &md.raw_imu,
                                Some(&md.additional_data),
                                &item.render_options.input_filename,
                            );
                        auto_rotation_results.insert(item.job_id, rotation);
                    }
                }
            }
            let auto_rotation_results = Arc::new(auto_rotation_results);
            apply_items.par_iter_mut().enumerate().for_each(|(idx, item)| {
                let t_item = std::time::Instant::now();
                let requested_range = normalize_time_range_ms(item.gyro_start_ms.zip(item.gyro_end_ms));
                if let Some(cached_entries) = gyro_cache.get(&item.gyro_files_idx) {
                    if let Some(cache_entry) =
                        select_best_cached_metadata(cached_entries, requested_range)
                    {
                        let adjusted_range_ms = get_adjusted_match_range_ms(
                            cache_entry.range_ms,
                            item.gyro_start_ms,
                            item.gyro_end_ms,
                        );
                        let mut md =
                            clone_metadata_for_job(cache_entry.metadata.as_ref(), adjusted_range_ms);
                        let imu_count = md.raw_imu.len();
                        let quat_count = md.quaternions.len();
                        let size = item.stab.params.read().size;

                        if let Some(base_lens_metadata) = item.base_lens_metadata.as_ref() {
                            base_lens_metadata.apply_missing_to_metadata(&mut md);
                        }
                        item.base_lens_metadata = Some(JobLensMetadataBackup::from_metadata(&md));
                        {
                            let mut statuses = lens_group_status.lock();
                            niyien_lens_presets::update_status_from_metadata(&mut statuses, &md);
                        }

                        let lens_index =
                            niyien_lens_presets::extract_lens_index(&md.additional_data);
                        item.lens_group_index = lens_index;
                        let group_config = lens_index
                            .and_then(|index| item.effective_lens_group_configs.get(index))
                            .cloned();
                        let auto_focus_length_mm =
                            niyien_lens_presets::extract_video_focus_length_mm(&md);
                        let selected_focal_length = resolve_lens_group_focal_length(
                            auto_focus_length_mm,
                            group_config.as_ref(),
                        );
                        let manual_focus_length_mm = selected_focal_length.and_then(
                            |(focal_length_mm, source)| {
                                (source == niyien_lens_presets::FocalLengthSource::Manual)
                                    .then_some(focal_length_mm)
                            },
                        );

                        if let Some(focal_length_mm) = manual_focus_length_mm {
                            niyien_lens_presets::apply_focal_length_fallback_to_metadata(
                                &mut md,
                                focal_length_mm,
                            );
                            ::log::info!(
                                "[apply_match] job[{}] applied lens group manual focal length {:.1}mm to metadata",
                                idx,
                                focal_length_mm
                            );
                        }

                        item.stab
                            .apply_main_video_telemetry(&mut md, &item.gyro_path, true);
                        let camera_id = md.camera_identifier.clone();

                        let detected_source =
                            md.detected_source.as_deref().unwrap_or("").to_string();
                        let metadata_raw_rotation =
                            denormalize_video_rotation_metadata(item.original_video_rotation);

                        // Always reset to original (pre-rotation) dimensions
                        item.render_options.output_width = item.original_output_size.0;
                        item.render_options.output_height = item.original_output_size.1;
                        item.stab.set_video_rotation(item.original_video_rotation);

                        // Priority 1: metadata rotation dimension swap (R3D excluded)
                        let metadata_rot = item.original_video_rotation.round() as i32;
                        let is_r3d = item.render_options.input_filename.to_ascii_lowercase().ends_with(".r3d");
                        let has_metadata_rotation = metadata_rot != 0 && !is_r3d;
                        if has_metadata_rotation && (metadata_rot == 90 || metadata_rot == 270) {
                            std::mem::swap(
                                &mut item.render_options.output_width,
                                &mut item.render_options.output_height,
                            );
                            ::log::info!(
                                "[apply_match] job[{}] metadata rotation {} → dimension swap ({}x{})",
                                idx, metadata_rot,
                                item.render_options.output_width,
                                item.render_options.output_height
                            );
                        }
                        item.stab.set_output_size(
                            item.render_options.output_width,
                            item.render_options.output_height,
                        );

                        // Priority 2: gyroscope rotation (only when no metadata rotation)
                        let should_apply_auto_rotate =
                            !has_metadata_rotation && item.auto_rotate && detected_source.starts_with("SenseFlow");
                        let auto_rotation = if should_apply_auto_rotate {
                            auto_rotation_results
                                .get(&item.job_id)
                                .copied()
                                .flatten()
                        } else {
                            None
                        };

                        let existing_lens = item.stab.lens.read().clone();
                        let custom_lens_profile = group_config
                            .as_ref()
                            .filter(|cfg| cfg.anamorphic_enabled)
                            .and_then(|cfg| {
                                niyien_lens_presets::build_lens_profile(
                                    &md,
                                    size,
                                    Some(cfg),
                                    Some(&existing_lens),
                                )
                            });

                        {
                            let params = item.stab.params.read();
                            let mut gyro = item.stab.gyro.write();
                            gyro.init_from_params(&params);
                            gyro.clear();
                            gyro.file_url = String::new();
                            ::log::info!(
                                "[apply_match T18] job[{}] gyro.file_url cleared (data in memory)",
                                idx
                            );
                            gyro.file_metadata = Default::default();
                            drop(params);
                            gyro.load_from_telemetry(md);
                            gyro.file_load_options = Default::default();
                        }
                        *item.stab.camera_id.write() = camera_id;
                        match item.stab.autoload_lens_from_camera_id() {
                            Ok(true) => {
                                ::log::info!(
                                    "[apply_match] job[{}] autoloaded lens profile from camera id",
                                    idx
                                );
                            }
                            Ok(false) => {}
                            Err(err) => {
                                ::log::warn!(
                                    "[apply_match] job[{}] autoload lens profile failed: {}",
                                    idx,
                                    err
                                );
                            }
                        }

                        if let Some(focal_length_mm) = manual_focus_length_mm {
                            item.stab.set_user_focal_length(focal_length_mm);
                            sync_readout_params_from_lens(item.stab.as_ref());
                            ::log::info!(
                                "[apply_match] job[{}] applied lens group manual focal length {:.1}mm",
                                idx,
                                focal_length_mm
                            );
                        }

                        if let Some(rotation) = auto_rotation {
                            ::log::info!(
                                "[auto_rotate compare] file='{}' detected_source='{}' metadata_raw={} metadata_normalized={} auto_rotate_result={} matches_normalized={}",
                                item.render_options.input_filename,
                                detected_source,
                                metadata_raw_rotation,
                                item.original_video_rotation,
                                rotation,
                                (rotation as f64 - item.original_video_rotation).abs() < f64::EPSILON
                            );
                            item.stab.set_video_rotation(rotation as f64);
                            if rotation == 90 || rotation == 270 {
                                std::mem::swap(
                                    &mut item.render_options.output_width,
                                    &mut item.render_options.output_height,
                                );
                            }
                            item.stab.set_output_size(
                                item.render_options.output_width,
                                item.render_options.output_height,
                            );
                            ::log::info!(
                                "[apply_match] job[{}] auto_rotate applied: {} ({}x{})",
                                idx,
                                rotation,
                                item.render_options.output_width,
                                item.render_options.output_height
                            );
                        } else if should_apply_auto_rotate {
                            ::log::info!(
                                "[auto_rotate compare] file='{}' detected_source='{}' metadata_raw={} metadata_normalized={} auto_rotate_result=None matches_normalized=false",
                                item.render_options.input_filename,
                                detected_source,
                                metadata_raw_rotation,
                                item.original_video_rotation
                            );
                        }

                        item.base_render_output_size = (
                            item.render_options.output_width,
                            item.render_options.output_height,
                        );

                        if let Some(lens_index) = lens_index {
                            if let Some(profile) = custom_lens_profile {
                                if let Some(output_dim) = profile.output_dimension.clone() {
                                    item.render_options.output_width = output_dim.w;
                                    item.render_options.output_height = output_dim.h;
                                } else {
                                    item.render_options.output_width = item.base_render_output_size.0;
                                    item.render_options.output_height = item.base_render_output_size.1;
                                }
                                {
                                    let mut lens = item.stab.lens.write();
                                    *lens = profile;
                                }
                                item.stab.set_output_size(
                                    item.render_options.output_width,
                                    item.render_options.output_height,
                                );
                                sync_readout_params_from_lens(item.stab.as_ref());
                                ::log::info!(
                                    "[apply_match] job[{}] applied lens group #{} profile",
                                    idx,
                                    lens_index
                                );
                            } else {
                                item.render_options.output_width = item.base_render_output_size.0;
                                item.render_options.output_height = item.base_render_output_size.1;
                                item.stab.set_output_size(
                                    item.render_options.output_width,
                                    item.render_options.output_height,
                                );
                                ::log::info!(
                                    "[apply_match] job[{}] lens group #{} skipped (keeping existing lens flow)",
                                    idx,
                                    lens_index
                                );
                            }
                        } else {
                            item.render_options.output_width = item.base_render_output_size.0;
                            item.render_options.output_height = item.base_render_output_size.1;
                            item.stab.set_output_size(
                                item.render_options.output_width,
                                item.render_options.output_height,
                            );
                        }

                        item.stab.invalidate_smoothing();
                        item.stab.invalidate_zooming();
                        ::log::info!(
                            "[apply_match] job[{}] gyro[{}] slice+load: {:.1}ms ({} imu, {} quats, range={:?}, cache_range={:?})",
                            idx,
                            item.gyro_files_idx,
                            t_item.elapsed().as_secs_f64() * 1000.0,
                            imu_count,
                            quat_count,
                            adjusted_range_ms,
                            cache_entry.range_ms
                        );
                    } else {
                        ::log::warn!(
                            "[apply_match] no cached metadata matched gyro[{}] range={:?}",
                            item.gyro_files_idx,
                            requested_range
                        );
                    }
                }
            });
            ::log::info!(
                "[apply_match] all jobs apply: {:.1}ms ({} items)",
                t_apply.elapsed().as_secs_f64() * 1000.0,
                apply_items.len()
            );

            let t_sync = std::time::Instant::now();
            apply_items.par_iter().for_each(|item| {
                let (duration_s, fps) = {
                    let params = item.stab.params.read();
                    (params.duration_ms / 1000.0, params.fps)
                };
                let max_sync_points = if duration_s > 30.0 * 60.0 {
                    5
                } else if duration_s > 10.0 * 60.0 {
                    4
                } else {
                    2
                };
                let every_nth_frame = ((fps / 49.0).floor() as i64).max(1);

                item.stab.gyro.write().integration_method = 1; // Complementary

                let mut lens = item.stab.lens.write();
                lens.sync_settings = Some(serde_json::json!({
                    "do_autosync": true,
                    "max_sync_points": max_sync_points,
                    "search_size": 5.0,
                    "time_per_syncpoint": 2.5,
                    "every_nth_frame": every_nth_frame,
                    "initial_offset": 0.0,
                    "pose_method": 0,
                    "of_method": default_of_method,
                    "offset_method": 2
                }));
                drop(lens);
                item.stab.recompute_gyro();
            });
            ::log::info!(
                "[apply_match] sync settings: {:.1}ms ({} jobs)",
                t_sync.elapsed().as_secs_f64() * 1000.0,
                apply_items.len()
            );

            let t_export = std::time::Instant::now();
            let job_updates: Vec<(
                u32,
                Option<String>,
                RenderOptions,
                Option<JobLensMetadataBackup>,
                (usize, usize),
                Option<usize>,
            )> =
                apply_items
                    .into_par_iter()
                    .map(|item| {
                    item.stab.gyro.write().file_url = item.gyro_path.clone();
                    let additional_data =
                        prepare_project_additional_data(&item.additional_data, &item.render_options);
                    match item.stab.export_gyroflow_data(
                        core::GyroflowProjectType::WithGyroData,
                        &additional_data,
                        None,
                    ) {
                        Ok(data) => {
                            ::log::info!(
                                "[apply_match T20] job {} project_data updated with embedded gyro data ({} bytes)",
                                item.job_id,
                                data.len()
                            );
                            (
                                item.job_id,
                                Some(data),
                                item.render_options,
                                item.base_lens_metadata,
                                item.base_render_output_size,
                                item.lens_group_index,
                            )
                        }
                        Err(e) => {
                            ::log::warn!(
                                "[apply_match T20] job {} export_gyroflow_data failed: {}",
                                item.job_id,
                                e
                            );
                            (
                                item.job_id,
                                None,
                                item.render_options,
                                item.base_lens_metadata,
                                item.base_render_output_size,
                                item.lens_group_index,
                            )
                        }
                    }
                })
                .collect();
            ::log::info!(
                "[apply_match] export_gyroflow_data: {:.1}ms ({} jobs)",
                t_export.elapsed().as_secs_f64() * 1000.0,
                job_updates
                    .iter()
                    .filter(|(_, project_data, _, _, _, _)| project_data.is_some())
                    .count()
            );

            ::log::info!(
                "[apply_match] background total: {:.1}ms",
                t_bg.elapsed().as_secs_f64() * 1000.0
            );

            let lens_group_status = lens_group_status.lock().clone();
            let t_bg_end = std::time::Instant::now();
            on_done((job_updates, cache_updates, lens_group_status, t_bg_end));
        });
    }

    // [queue-lifecycle T2] 按视频创建时间排序（升序），无时间戳的排最后
    fn sort_jobs_by_created_at(&mut self) {
        // Collect (job_id, created_at) pairs
        let mut items: Vec<(u32, Option<i64>)> = {
            if let Ok(queue) = self.queue.try_borrow() {
                (0..queue.row_count())
                    .map(|i| {
                        let job_id = queue[i as usize].job_id;
                        // [T20] 使用 job.video_created_at（stab 释放后仍可用）
                        let created_at = self.jobs.get(&job_id).and_then(|j| j.video_created_at);
                        (job_id, created_at)
                    })
                    .collect()
            } else {
                return;
            }
        };

        // [T16] log 排序前的顺序
        let before_ids: Vec<_> = items.iter().map(|(id, _)| *id).collect();

        // Sort: timestamped ascending, no-timestamp at end
        // Rust sort_by 是稳定排序，相同创建时间的 job 保持原有相对顺序
        items.sort_by(|a, b| match (a.1, b.1) {
            (Some(ta), Some(tb)) => ta.cmp(&tb),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        });

        // Reorder the queue model to match the sorted order
        let sorted_ids: Vec<_> = items.iter().map(|(id, _)| *id).collect();
        ::log::info!(
            "[queue-lifecycle T16] sort_jobs_by_created_at: before={:?}, after={:?}",
            before_ids,
            sorted_ids
        );
        self.reorder_queue_by_job_ids(&sorted_ids);
        self.queue_changed();
    }

    fn restore_original_order(&mut self) {
        // [queue-lifecycle T2] 不再需要恢复原始顺序，永远按时间排序
    }

    /// Reorder the queue model to match the given job_id sequence.
    fn reorder_queue_by_job_ids(&mut self, desired_order: &[u32]) {
        if let Ok(mut q) = self.queue.try_borrow_mut() {
            let count = q.row_count() as usize;
            if desired_order.len() != count {
                return;
            }

            // Build a position lookup from current queue
            for target_pos in 0..count {
                let desired_id = desired_order[target_pos];
                // Find where this job_id currently is in the queue
                let current_pos = (target_pos..count).find(|&i| q[i].job_id == desired_id);
                if let Some(current_pos) = current_pos {
                    if current_pos != target_pos {
                        let itm = q[current_pos].clone();
                        q.remove(current_pos);
                        q.insert(target_pos, itm);
                    }
                }
            }

            // Update all job indices
            for (i, v) in q.iter().enumerate() {
                if let Some(job) = self.jobs.get_mut(&v.job_id) {
                    job.queue_index = i;
                }
            }
        }
    }

    // T6: Manually pair a video job with a specific gyro file.
    // 使用 job_id 标识视频，避免 remove/sort 后 video_index 错位
    fn manual_set_calibration_pair(&mut self, job_id: u32, gyro_index: usize) {
        // 直接按 job_id 去重并存储，不依赖队列位置
        self.manual_pairs.retain(|p| p.job_id != job_id);
        self.manual_pairs
            .push(core::gyro_match::ManualCalibrationPair {
                job_id,
                video_index: 0, // 占位，batch_match 前会重新计算
                gyro_index,
            });
        ::log::info!(
            "[manual_pair] set: job_id={}, gyro_index={}",
            job_id,
            gyro_index
        );
        self.match_results_changed();
    }

    fn get_manual_pair_gyro_index(&self, job_id: u32) -> i32 {
        // 直接按 job_id 查找，不再依赖队列位置
        if let Some(pair) = self.manual_pairs.iter().find(|p| p.job_id == job_id) {
            ::log::debug!(
                "[manual_pair] found: job_id={}, gyro_index={}",
                job_id,
                pair.gyro_index
            );
            return pair.gyro_index as i32;
        }
        -1
    }

    // T7: Unpair a video job, clearing its external gyro data.
    fn unpair_video(&mut self, job_id: u32) {
        // Clear gyro data from the job
        if let Some(job) = self.jobs.get(&job_id) {
            if let Some(stab) = &job.stab {
                stab.gyro.write().clear();
            }
        }
        // 直接按 job_id 移除 manual pair
        self.manual_pairs.retain(|p| p.job_id != job_id);
        // [queue-lifecycle T4] 按 job_id 查找 match result，避免 remove 后 video_index 错位
        let ordered_ids = self.get_ordered_job_ids();
        if let Some(ref mut results) = self.match_results {
            let idx = results
                .results
                .iter()
                .position(|r| r.job_id == Some(job_id))
                .or_else(|| {
                    let video_index = ordered_ids.iter().position(|&id| id == job_id)?;
                    results
                        .results
                        .iter()
                        .position(|r| r.video_index == video_index)
                });
            if let Some(i) = idx {
                results.results[i].status = core::gyro_match::MatchStatus::Unmatched;
                results.results[i].gyro_index = None;
                results.results[i].gyro_start_ms = None;
                results.results[i].gyro_end_ms = None;
            }
        }
        self.match_results_changed();
    }

    fn enter_pairing_mode(&mut self, gyro_index: usize) {
        self.pairing_mode_gyro_index = Some(gyro_index);
        self.pairing_mode_changed();
    }

    fn exit_pairing_mode(&mut self) {
        self.pairing_mode_gyro_index = None;
        self.pairing_mode_changed();
    }

    fn is_in_pairing_mode(&self) -> bool {
        self.pairing_mode_gyro_index.is_some()
    }

    fn get_match_status_json(&self, job_id: u32) -> QString {
        // [queue-lifecycle T4] 优先按 job_id 查找（remove 后 video_index 会错位）
        if let Some(ref results) = self.match_results {
            let r_opt = results
                .results
                .iter()
                .find(|r| r.job_id == Some(job_id))
                .or_else(|| {
                    // 兼容 fallback：job_id 未设置时按 video_index 查找
                    let job_ids = self.get_ordered_job_ids();
                    let video_index = job_ids.iter().position(|&id| id == job_id)?;
                    results
                        .results
                        .iter()
                        .find(|r| r.video_index == video_index)
                });
            if let Some(r) = r_opt {
                let parsed_gyro_indices: Vec<usize> = self
                    .gyro_files
                    .iter()
                    .enumerate()
                    .filter(|(_, g)| {
                        g.parsed && g.created_at_ms.is_some() && g.duration_ms.is_some()
                    })
                    .map(|(i, _)| i)
                    .collect();

                let matched_gyro = r
                    .gyro_index
                    .and_then(|gi| parsed_gyro_indices.get(gi))
                    .and_then(|&fi| self.gyro_files.get(fi));

                let gyro_filename = matched_gyro.map(|g| g.filename.as_str()).unwrap_or("");

                // [queue-batch-streamline T1] 从 cached_metadata 提取 detected_source
                let detected_source = matched_gyro
                    .and_then(|g| g.cached_metadata.as_ref())
                    .and_then(|md| md.detected_source.as_deref())
                    .unwrap_or("");

                return QString::from(
                    serde_json::json!({
                        "status": format!("{:?}", r.status),
                        "gyro_index": r.gyro_index,
                        "gyro_start_ms": r.gyro_start_ms,
                        "gyro_end_ms": r.gyro_end_ms,
                        "gyro_filename": gyro_filename,
                        "detected_source": detected_source,
                    })
                    .to_string(),
                );
            }
        }
        QString::from("{\"status\":\"none\"}")
    }

    /// 获取相邻 job 的 matchGyroIndex，用于 QML 判断同组 gyro。
    /// offset=-1 为前一个 job，offset=1 为后一个 job。
    /// 返回 -1 表示不存在或无匹配。
    fn get_adjacent_gyro_index(&self, job_id: u32, offset: i32) -> i32 {
        let job_ids = self.get_ordered_job_ids();
        let pos = match job_ids.iter().position(|&id| id == job_id) {
            Some(p) => p as i32,
            None => {
                ::log::debug!(
                    "[queue-gyro-column T9] get_adjacent_gyro_index: job_id {} not found in ordered_ids",
                    job_id
                );
                return -1;
            }
        };
        let adj_pos = pos + offset;
        if adj_pos < 0 || adj_pos >= job_ids.len() as i32 {
            return -1;
        }
        let adj_job_id = job_ids[adj_pos as usize];
        // 复用 get_match_status_json 的查找逻辑：优先按 job_id，再 fallback video_index
        let result = if let Some(ref results) = self.match_results {
            let r_opt = results
                .results
                .iter()
                .find(|r| r.job_id == Some(adj_job_id))
                .or_else(|| {
                    let video_index = job_ids.iter().position(|&id| id == adj_job_id)?;
                    results
                        .results
                        .iter()
                        .find(|r| r.video_index == video_index)
                });
            if let Some(r) = r_opt {
                r.gyro_index.map(|gi| gi as i32).unwrap_or(-1)
            } else {
                -1
            }
        } else {
            -1
        };
        ::log::debug!(
            "[queue-gyro-column T9] get_adjacent_gyro_index: job_id={}, offset={}, adj_job_id={}, result={}",
            job_id,
            offset,
            adj_job_id,
            result
        );
        result
    }

    // [T14] 全局 matchExecuted 标志：是否已执行过 match
    fn has_match_results(&self) -> bool {
        self.match_results.is_some()
    }

    // [T15] 内部辅助：获取指定 job 的 gyro_index（复用 get_match_status_json 的查找逻辑）
    fn get_gyro_index_for_job(&self, job_id: u32) -> i32 {
        if let Some(ref results) = self.match_results {
            let job_ids = self.get_ordered_job_ids();
            let r_opt = results
                .results
                .iter()
                .find(|r| r.job_id == Some(job_id))
                .or_else(|| {
                    let video_index = job_ids.iter().position(|&id| id == job_id)?;
                    results
                        .results
                        .iter()
                        .find(|r| r.video_index == video_index)
                });
            if let Some(r) = r_opt {
                return r.gyro_index.map(|gi| gi as i32).unwrap_or(-1);
            }
        }
        -1
    }

    // [T15] 判断当前 job 是否和前一个 job 使用相同 gyro
    fn is_same_gyro_as_prev(&self, job_id: u32) -> bool {
        let my_idx = self.get_gyro_index_for_job(job_id);
        let prev_idx = self.get_adjacent_gyro_index(job_id, -1);
        let result = my_idx >= 0 && my_idx == prev_idx;
        ::log::debug!(
            "[T15] is_same_gyro_as_prev: job_id={}, my_idx={}, prev_idx={}, result={}",
            job_id,
            my_idx,
            prev_idx,
            result
        );
        result
    }

    // [T15] 判断当前 job 是否和后一个 job 使用相同 gyro
    fn is_same_gyro_as_next(&self, job_id: u32) -> bool {
        let my_idx = self.get_gyro_index_for_job(job_id);
        let next_idx = self.get_adjacent_gyro_index(job_id, 1);
        let result = my_idx >= 0 && my_idx == next_idx;
        ::log::debug!(
            "[T15] is_same_gyro_as_next: job_id={}, my_idx={}, next_idx={}, result={}",
            job_id,
            my_idx,
            next_idx,
            result
        );
        result
    }

    // [T22] 一次性构建所有 job 的 sameGyro 缓存，排序完成后调用
    fn build_same_gyro_cache(&mut self) {
        self.same_gyro_cache.clear();
        let job_ids = self.get_ordered_job_ids();
        // 收集每个 job 的 gyro_index
        let gyro_indices: Vec<i32> = job_ids
            .iter()
            .map(|&jid| self.get_gyro_index_for_job(jid))
            .collect();
        for (i, &jid) in job_ids.iter().enumerate() {
            let my_idx = gyro_indices[i];
            let prev_same = i > 0 && my_idx >= 0 && gyro_indices[i - 1] == my_idx;
            let next_same =
                i + 1 < gyro_indices.len() && my_idx >= 0 && gyro_indices[i + 1] == my_idx;
            self.same_gyro_cache.insert(jid, (prev_same, next_same));
        }
        ::log::info!(
            "[T22] build_same_gyro_cache: {} jobs cached",
            self.same_gyro_cache.len()
        );
    }

    // [T22] 从缓存读取 sameGyroAsPrev（不实时查询，不受 queue 状态影响）
    fn get_cached_same_gyro_prev(&self, job_id: u32) -> bool {
        self.same_gyro_cache
            .get(&job_id)
            .map(|&(prev, _)| prev)
            .unwrap_or(false)
    }

    // [T22] 从缓存读取 sameGyroAsNext（不实时查询，不受 queue 状态影响）
    fn get_cached_same_gyro_next(&self, job_id: u32) -> bool {
        self.same_gyro_cache
            .get(&job_id)
            .map(|&(_, next)| next)
            .unwrap_or(false)
    }
}

const APPLY_MATCH_PARSE_CHUNK_MAX_SPAN_MS: f64 = 120_000.0;
const APPLY_MATCH_PARSE_CHUNK_MERGE_GAP_MS: f64 = 15_000.0;
const APPLY_MATCH_RANGE_EPSILON_MS: f64 = 0.5;

fn sync_readout_params_from_lens(stab: &StabilizationManager) {
    let (frame_readout_time, frame_readout_direction) = {
        let lens = stab.lens.read();
        (lens.frame_readout_time, lens.frame_readout_direction)
    };

    if let Some(frame_readout_time) = frame_readout_time {
        let mut params = stab.params.write();
        params.frame_readout_time = frame_readout_time.abs();
        params.frame_readout_direction =
            frame_readout_direction.unwrap_or(if frame_readout_time < 0.0 {
                ReadoutDirection::BottomToTop
            } else {
                ReadoutDirection::TopToBottom
            });
    }
}

fn normalize_time_range_ms(range: Option<(f64, f64)>) -> Option<(f64, f64)> {
    range.map(|(start, end)| {
        let start = start.max(0.0);
        let end = end.max(start);
        (start, end)
    })
}

fn time_range_span_ms(range: Option<(f64, f64)>) -> f64 {
    range
        .map(|(start, end)| (end - start).max(0.0))
        .unwrap_or(f64::INFINITY)
}

fn metadata_cache_covers(
    cached_range_ms: Option<(f64, f64)>,
    requested_range_ms: Option<(f64, f64)>,
) -> bool {
    match (
        normalize_time_range_ms(cached_range_ms),
        normalize_time_range_ms(requested_range_ms),
    ) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some((cached_start, cached_end)), Some((requested_start, requested_end))) => {
            cached_start <= requested_start + APPLY_MATCH_RANGE_EPSILON_MS
                && cached_end + APPLY_MATCH_RANGE_EPSILON_MS >= requested_end
        }
    }
}

fn merge_metadata_cache_entries(
    cache_entries: &mut Vec<CachedGyroMetadataRange>,
    new_entries: impl IntoIterator<Item = CachedGyroMetadataRange>,
) {
    for mut entry in new_entries {
        entry.range_ms = normalize_time_range_ms(entry.range_ms);
        if cache_entries
            .iter()
            .any(|existing| metadata_cache_covers(existing.range_ms, entry.range_ms))
        {
            continue;
        }
        if entry.range_ms.is_none() {
            cache_entries.clear();
            cache_entries.push(entry);
            continue;
        }
        cache_entries.retain(|existing| !metadata_cache_covers(entry.range_ms, existing.range_ms));
        cache_entries.push(entry);
    }
    cache_entries.sort_by(|a, b| {
        let ka = a
            .range_ms
            .map(|(start, _)| start)
            .unwrap_or(f64::NEG_INFINITY);
        let kb = b
            .range_ms
            .map(|(start, _)| start)
            .unwrap_or(f64::NEG_INFINITY);
        ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn build_parse_requests(
    requested_ranges: &[Option<(f64, f64)>],
    existing_entries: &[CachedGyroMetadataRange],
) -> Vec<Option<(f64, f64)>> {
    if requested_ranges.is_empty() {
        return Vec::new();
    }

    let normalized_requests: Vec<Option<(f64, f64)>> = requested_ranges
        .iter()
        .copied()
        .map(normalize_time_range_ms)
        .collect();

    if normalized_requests.iter().any(|range| range.is_none()) {
        if existing_entries
            .iter()
            .any(|entry| entry.range_ms.is_none())
        {
            return Vec::new();
        }
        return vec![None];
    }

    let mut uncovered_ranges: Vec<(f64, f64)> = normalized_requests
        .into_iter()
        .flatten()
        .filter(|range| {
            !existing_entries
                .iter()
                .any(|entry| metadata_cache_covers(entry.range_ms, Some(*range)))
        })
        .collect();

    if uncovered_ranges.is_empty() {
        return Vec::new();
    }

    uncovered_ranges.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut merged_ranges = Vec::new();
    let mut current = uncovered_ranges[0];
    for range in uncovered_ranges.into_iter().skip(1) {
        let gap_ms = range.0 - current.1;
        let merged_end = current.1.max(range.1);
        let merged_span_ms = merged_end - current.0;
        if gap_ms <= APPLY_MATCH_PARSE_CHUNK_MERGE_GAP_MS
            && merged_span_ms <= APPLY_MATCH_PARSE_CHUNK_MAX_SPAN_MS
        {
            current.1 = merged_end;
        } else {
            merged_ranges.push(current);
            current = range;
        }
    }
    merged_ranges.push(current);

    merged_ranges.into_iter().map(Some).collect()
}

fn select_best_cached_metadata<'a>(
    cache_entries: &'a [CachedGyroMetadataRange],
    requested_range_ms: Option<(f64, f64)>,
) -> Option<&'a CachedGyroMetadataRange> {
    let requested_range_ms = normalize_time_range_ms(requested_range_ms);
    cache_entries
        .iter()
        .filter(|entry| metadata_cache_covers(entry.range_ms, requested_range_ms))
        .min_by(|a, b| {
            time_range_span_ms(a.range_ms)
                .partial_cmp(&time_range_span_ms(b.range_ms))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

fn prepare_project_additional_data(
    additional_data: &str,
    render_options: &RenderOptions,
) -> String {
    let mut additional_data = additional_data.to_owned();
    if let Ok(serde_json::Value::Object(mut obj)) =
        serde_json::from_str(&additional_data) as serde_json::Result<serde_json::Value>
    {
        if let Ok(output) = serde_json::to_value(render_options) {
            obj.insert("output".into(), output);
        }
        additional_data = serde_json::to_string(&obj).unwrap_or_default();
    }
    additional_data
}

fn get_adjusted_match_range_ms(
    merged_time_range: Option<(f64, f64)>,
    gyro_start_ms: Option<f64>,
    gyro_end_ms: Option<f64>,
) -> Option<(f64, f64)> {
    let (start, end) = match normalize_time_range_ms(gyro_start_ms.zip(gyro_end_ms)) {
        Some(range) => range,
        _ => return None,
    };
    let range_offset_ms = merged_time_range.map(|(s, _)| s.max(0.0)).unwrap_or(0.0);
    let adjusted_start = (start - range_offset_ms).max(0.0);
    let adjusted_end = (end - range_offset_ms).max(adjusted_start);
    Some((adjusted_start, adjusted_end))
}

fn update_metadata_duration(md: &mut core::gyro_source::FileMetadata) {
    if !md.raw_imu.is_empty() {
        let len = md.raw_imu.len() as f64;
        let first = md
            .raw_imu
            .first()
            .map(|x| x.timestamp_ms)
            .unwrap_or_default();
        let last = md
            .raw_imu
            .last()
            .map(|x| x.timestamp_ms)
            .unwrap_or_default();
        md.duration_ms = (last - first) * ((len + 1.0) / len.max(1.0));
    } else if !md.quaternions.is_empty() {
        let len = md.quaternions.len() as f64;
        let first = md
            .quaternions
            .iter()
            .next()
            .map(|(k, _)| *k as f64 / 1000.0)
            .unwrap_or_default();
        let last = md
            .quaternions
            .iter()
            .next_back()
            .map(|(k, _)| *k as f64 / 1000.0)
            .unwrap_or_default();
        md.duration_ms = (last - first) * ((len + 1.0) / len.max(1.0));
    } else {
        md.duration_ms = 0.0;
    }
}

fn zero_base_metadata(md: &mut core::gyro_source::FileMetadata) {
    let first_ts_ms = md
        .raw_imu
        .first()
        .map(|x| x.timestamp_ms)
        .or_else(|| {
            md.quaternions
                .iter()
                .next()
                .map(|(k, _)| *k as f64 / 1000.0)
        })
        .or_else(|| {
            md.gravity_vectors
                .as_ref()
                .and_then(|gv| gv.iter().next().map(|(k, _)| *k as f64 / 1000.0))
        })
        .or_else(|| {
            md.image_orientations
                .as_ref()
                .and_then(|io| io.iter().next().map(|(k, _)| *k as f64 / 1000.0))
        });

    if let Some(first_ts_ms) = first_ts_ms {
        let first_ts_us = (first_ts_ms * 1000.0).round() as i64;
        for sample in md.raw_imu.iter_mut() {
            sample.timestamp_ms -= first_ts_ms;
        }
        md.quaternions = md
            .quaternions
            .iter()
            .map(|(&k, &v)| (k - first_ts_us, v))
            .collect();
        if let Some(gravity_vectors) = md.gravity_vectors.take() {
            md.gravity_vectors = Some(
                gravity_vectors
                    .iter()
                    .map(|(&k, &v)| (k - first_ts_us, v))
                    .collect(),
            );
        }
        if let Some(image_orientations) = md.image_orientations.take() {
            md.image_orientations = Some(
                image_orientations
                    .iter()
                    .map(|(&k, &v)| (k - first_ts_us, v))
                    .collect(),
            );
        }
    }
    update_metadata_duration(md);
}

fn clone_metadata_for_job(
    cached_md: &core::gyro_source::FileMetadata,
    adjusted_range_ms: Option<(f64, f64)>,
) -> core::gyro_source::FileMetadata {
    let mut md = if let Some((start_ms, end_ms)) = adjusted_range_ms {
        let start_us = (start_ms * 1000.0).round() as i64;
        let end_us = (end_ms * 1000.0).round() as i64;
        let mut md = cached_md.thin();
        md.per_frame_time_offsets.clear();
        md.raw_imu = cached_md
            .raw_imu
            .iter()
            .filter(|sample| sample.timestamp_ms >= start_ms && sample.timestamp_ms <= end_ms)
            .cloned()
            .collect();
        md.quaternions = cached_md
            .quaternions
            .range(start_us..=end_us)
            .map(|(&k, &v)| (k, v))
            .collect();
        md.gravity_vectors = cached_md.gravity_vectors.as_ref().map(|gravity_vectors| {
            gravity_vectors
                .range(start_us..=end_us)
                .map(|(&k, &v)| (k, v))
                .collect()
        });
        md.image_orientations = cached_md
            .image_orientations
            .as_ref()
            .map(|image_orientations| {
                image_orientations
                    .range(start_us..=end_us)
                    .map(|(&k, &v)| (k, v))
                    .collect()
            });
        zero_base_metadata(&mut md);
        md
    } else {
        let mut md = cached_md.clone();
        md.per_frame_time_offsets.clear();
        md
    };
    update_metadata_duration(&mut md);
    md
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

/// Parse gyro file metadata (creation date, IMU duration and detected source) using telemetry-parser.
/// Uses header_only mode for SenseFlow files: reads only 512 bytes, computes duration from filesize.
fn parse_gyro_metadata(
    url: &str,
) -> Result<(i64, f64, Option<String>), Box<dyn std::error::Error + Send + Sync>> {
    let mut file = filesystem::open_file(url, false, false)?;
    let filesize = file.size;
    let options = core::gyro_source::FileLoadOptions {
        header_only: true,
        ..Default::default()
    };
    let md = GyroSource::parse_telemetry_file(
        file.get_file(),
        filesize,
        url,
        &options,
        (0, 0),
        0.0,
        |_| {},
        Arc::new(AtomicBool::new(false)),
    )?;

    let created_at = md
        .creation_date_utc
        .as_ref()
        .and_then(|s| parse_creation_date_to_millis(s))
        .ok_or("No creation date found in gyro file")?;

    // In header_only mode, duration comes from SampleInfo.duration_ms (computed from filesize).
    // In full parse mode, fall back to raw_imu timestamps.
    let duration = if md.duration_ms > 0.0 {
        md.duration_ms
    } else if !md.raw_imu.is_empty() {
        let first = md.raw_imu.first().unwrap().timestamp_ms;
        let last = md.raw_imu.last().unwrap().timestamp_ms;
        last - first
    } else {
        0.0
    };

    Ok((created_at, duration, md.detected_source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lens_metadata_backup_restores_missing_fields_without_overwriting_real_values() {
        let backup = JobLensMetadataBackup::from_metadata(&core::gyro_source::FileMetadata {
            lens_params: BTreeMap::from([(
                0,
                core::gyro_source::LensParams {
                    focal_length: Some(35.0),
                    pixel_focal_length: Some(3500.0),
                    ..Default::default()
                },
            )]),
            lens_positions: BTreeMap::from([(0, 1.2)]),
            lens_profile: Some(serde_json::json!({ "identifier": "base" })),
            unit_pixel_focal_length: Some(100.0),
            camera_identifier: Some(CameraIdentifier {
                brand: "Canon".to_owned(),
                model: "R5".to_owned(),
                ..Default::default()
            }),
            detected_source: Some("Canon R5".to_owned()),
            frame_readout_time: Some(12.5),
            frame_readout_direction: ReadoutDirection::BottomToTop,
            ..Default::default()
        });

        let mut md = core::gyro_source::FileMetadata {
            lens_params: Default::default(),
            lens_positions: Default::default(),
            lens_profile: None,
            unit_pixel_focal_length: None,
            camera_identifier: Some(CameraIdentifier {
                brand: String::new(),
                ..Default::default()
            }),
            detected_source: Some("SenseFlow Mini".to_owned()),
            frame_readout_time: None,
            frame_readout_direction: ReadoutDirection::TopToBottom,
            ..Default::default()
        };

        backup.apply_missing_to_metadata(&mut md);

        assert_eq!(md.lens_params.len(), 1);
        assert_eq!(md.lens_positions.len(), 1);
        assert_eq!(md.unit_pixel_focal_length, Some(100.0));
        assert_eq!(
            md.camera_identifier.as_ref().map(|v| v.brand.as_str()),
            Some("Canon")
        );
        assert_eq!(md.detected_source.as_deref(), Some("Canon R5"));
        assert_eq!(md.frame_readout_time, Some(12.5));
        assert_eq!(md.frame_readout_direction, ReadoutDirection::BottomToTop);
    }

    #[test]
    fn lens_metadata_backup_overwrite_restores_clean_state() {
        let backup = JobLensMetadataBackup::from_metadata(&core::gyro_source::FileMetadata {
            lens_params: BTreeMap::from([(
                0,
                core::gyro_source::LensParams {
                    focal_length: Some(24.0),
                    ..Default::default()
                },
            )]),
            detected_source: Some("Sony FX3".to_owned()),
            frame_readout_time: Some(8.3),
            frame_readout_direction: ReadoutDirection::LeftToRight,
            ..Default::default()
        });

        let mut md = core::gyro_source::FileMetadata {
            lens_params: BTreeMap::from([(
                0,
                core::gyro_source::LensParams {
                    focal_length: Some(55.0),
                    ..Default::default()
                },
            )]),
            detected_source: Some("SenseFlow".to_owned()),
            frame_readout_time: Some(99.0),
            frame_readout_direction: ReadoutDirection::TopToBottom,
            ..Default::default()
        };

        backup.overwrite_metadata(&mut md);

        assert_eq!(
            md.lens_params.get(&0).and_then(|v| v.focal_length),
            Some(24.0)
        );
        assert_eq!(md.detected_source.as_deref(), Some("Sony FX3"));
        assert_eq!(md.frame_readout_time, Some(8.3));
        assert_eq!(md.frame_readout_direction, ReadoutDirection::LeftToRight);
    }

    #[test]
    fn resolve_lens_group_focal_length_prefers_manual_override() {
        let config = niyien_lens_presets::LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(50.0),
            ..Default::default()
        };

        let selected = resolve_lens_group_focal_length(Some(35.0), Some(&config)).unwrap();
        assert_eq!(selected.0, 50.0);
        assert_eq!(selected.1, niyien_lens_presets::FocalLengthSource::Manual);
    }

    #[test]
    fn resolve_lens_group_focal_length_keeps_auto_when_manual_empty() {
        let config = niyien_lens_presets::LensGroupConfig {
            lens_index: 0,
            ..Default::default()
        };

        let selected = resolve_lens_group_focal_length(Some(35.0), Some(&config)).unwrap();
        assert_eq!(selected.0, 35.0);
        assert_eq!(selected.1, niyien_lens_presets::FocalLengthSource::Auto);
    }

    #[test]
    fn resolve_lens_group_focal_length_falls_back_to_manual_when_auto_missing() {
        let config = niyien_lens_presets::LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(24.0),
            ..Default::default()
        };

        let selected = resolve_lens_group_focal_length(None, Some(&config)).unwrap();
        assert_eq!(selected.0, 24.0);
        assert_eq!(selected.1, niyien_lens_presets::FocalLengthSource::Manual);
    }

    #[test]
    fn build_job_lens_group_override_keeps_local_auto_detect_against_global_manual() {
        let mut global = niyien_lens_presets::default_lens_group_configs();
        global[0].focal_length_mm = Some(35.0);

        let requested = niyien_lens_presets::default_lens_group_configs();
        let local_override = build_job_lens_group_override(&requested, &global, None).unwrap();

        assert!(local_override.is_group_enabled(0));
        assert_eq!(local_override.configs[0].focal_length_mm, None);
    }

    #[test]
    fn effective_lens_group_configs_only_override_enabled_groups() {
        let mut global = niyien_lens_presets::default_lens_group_configs();
        global[0].focal_length_mm = Some(35.0);
        global[1].focal_length_mm = Some(50.0);

        let mut local_configs = niyien_lens_presets::default_lens_group_configs();
        local_configs[0].focal_length_mm = Some(24.0);

        let job = Job {
            queue_index: 0,
            render_options: RenderOptions::default(),
            base_render_output_size: None,
            auto_rotate: false,
            additional_data: String::new(),
            cancel_flag: Default::default(),
            render_epoch: Default::default(),
            project_data: None,
            stab: None,
            base_lens_metadata: None,
            lens_group_config_override: Some(JobLensGroupOverride {
                configs: local_configs,
                enabled_groups: vec![true, false, false, false, false, false],
            }),
            lens_group_index: None,
            video_created_at: None,
            original_video_rotation: 0.0,
        };

        let effective = effective_lens_group_configs(&job, &global);
        assert_eq!(effective[0].focal_length_mm, Some(24.0));
        assert_eq!(effective[1].focal_length_mm, Some(50.0));
    }
}
