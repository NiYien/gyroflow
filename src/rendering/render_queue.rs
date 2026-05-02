// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

use qmetaobject::*;

use crate::core::StabilizationManager;
use crate::{core, rendering, util};
use core::camera_identifier::CameraIdentifier;
use core::filesystem;
use core::gyro_source::{FileMetadata, GyroSource};
use core::niyien_lens_presets;
use core::stabilization_params::ReadoutDirection;
use parking_lot::{Mutex as ParkingMutex, RwLock};
use rayon::prelude::*;
use regex::Regex;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::SeqCst},
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

#[derive(Default, Clone, Copy, Debug)]
struct QueueEtaSample {
    sync_frames: usize,
    sync_ms: f64,
    render_frames: usize,
    render_ms: f64,
}

#[derive(Default, Clone, Copy, Debug)]
struct QueueAutosyncStats {
    frames: usize,
    completed: bool,
}

#[derive(Default, Clone, Copy, Debug)]
struct QueueProgressSnapshot {
    done_units: f64,
    total_units: f64,
    done_jobs: u64,
    total_jobs: u64,
}

#[derive(Default, Clone, Copy, Debug)]
struct QueueEtaEstimateModel {
    sync_ms_per_frame: Option<f64>,
    render_ms_per_frame: Option<f64>,
    completed_job_samples: usize,
}
impl QueueEtaEstimateModel {
    fn observe_completed_job(&mut self, sample: QueueEtaSample) {
        let mut observed = false;

        if sample.sync_frames > 0 && sample.sync_ms.is_finite() && sample.sync_ms > 0.0 {
            Self::update_average(
                &mut self.sync_ms_per_frame,
                sample.sync_ms / sample.sync_frames as f64,
            );
            observed = true;
        }
        if sample.render_frames > 0 && sample.render_ms.is_finite() && sample.render_ms > 0.0 {
            Self::update_average(
                &mut self.render_ms_per_frame,
                sample.render_ms / sample.render_frames as f64,
            );
            observed = true;
        }
        if observed {
            self.completed_job_samples += 1;
        }
    }

    fn estimate_remaining_ms(
        &self,
        sync_frames: usize,
        render_frames: usize,
        parallel_renders: usize,
    ) -> Option<u64> {
        if sync_frames == 0 && render_frames == 0 {
            return None;
        }

        let sync_ms = if sync_frames > 0 {
            self.sync_ms_per_frame? * sync_frames as f64
        } else {
            0.0
        };
        let render_ms = if render_frames > 0 {
            self.render_ms_per_frame? * render_frames as f64
                / parallel_renders.max(1) as f64
        } else {
            0.0
        };
        let total = sync_ms + render_ms;
        total.is_finite().then(|| total.round().max(0.0) as u64)
    }

    fn update_average(avg: &mut Option<f64>, sample: f64) {
        const SAMPLE_WEIGHT: f64 = 0.3;
        *avg = Some(match *avg {
            Some(current) => current * (1.0 - SAMPLE_WEIGHT) + sample * SAMPLE_WEIGHT,
            None => sample,
        });
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
    last_finished_export_project: Option<u32>,
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

    fn to_metadata(&self) -> core::gyro_source::FileMetadata {
        let mut md = core::gyro_source::FileMetadata::default();
        self.overwrite_metadata(&mut md);
        md
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
    id: u64,
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

fn should_apply_auto_rotate(
    has_metadata_rotation: bool,
    job_auto_rotate: bool,
    queue_auto_rotate: bool,
    gyro_detected_source: &str,
) -> bool {
    !has_metadata_rotation
        && (job_auto_rotate || queue_auto_rotate)
        && gyro_detected_source.starts_with("SenseFlow")
}

fn parse_job_ids_json(job_ids_json: &str) -> Vec<u32> {
    serde_json::from_str(job_ids_json).unwrap_or_default()
}

fn update_project_data_batch_params(data: &mut serde_json::Value, params: &serde_json::Value) {
    let Some(obj) = data.as_object_mut() else {
        return;
    };

    if let Some(stab) = obj.get_mut("stabilization").and_then(|s| s.as_object_mut()) {
        if let Some(smoothness) = params.get("smoothness").and_then(|v| v.as_f64()) {
            if let Some(sp) = stab
                .get_mut("smoothing_params")
                .and_then(|p| p.as_array_mut())
            {
                for p in sp.iter_mut() {
                    if p.get("name").and_then(|n| n.as_str()) == Some("smoothness") {
                        p.as_object_mut().map(|o| {
                            o.insert("value".into(), serde_json::json!(smoothness))
                        });
                    }
                }
            }
        }
        if let Some(amount) = params.get("horizon_lock_amount").and_then(|v| v.as_f64()) {
            stab.insert("horizon_lock_amount".into(), serde_json::json!(amount));
        }
        if let Some(zoom_mode) = params.get("zoom_mode").and_then(|v| v.as_str()) {
            let az = match zoom_mode {
                "static" => -1.0,
                "dynamic" => 4.0,
                _ => 0.0,
            };
            stab.insert("adaptive_zoom_window".into(), serde_json::json!(az));
        }
        if let Some(zoom_speed) = params.get("zoom_speed").and_then(|v| v.as_f64()) {
            stab.insert(
                "adaptive_zoom_window".into(),
                serde_json::json!(zoom_speed),
            );
        }
        if let Some(lc) = params.get("lens_correction").and_then(|v| v.as_f64()) {
            stab.insert("lens_correction_amount".into(), serde_json::json!(lc));
        }
    }

    if let Some(video_info) = obj.get_mut("video_info").and_then(|v| v.as_object_mut()) {
        if let Some(fps) = params.get("framerate").and_then(|v| v.as_f64()) {
            let source_fps = video_info.get("fps").and_then(|v| v.as_f64()).unwrap_or(0.0);
            if source_fps > 0.0 {
                let fps_scale = fps / source_fps;
                if (fps_scale - 1.0).abs() > 0.0001 {
                    video_info.insert("fps_scale".into(), serde_json::json!(fps_scale));
                    video_info.insert("vfr_fps".into(), serde_json::json!(fps));
                    if let Some(duration_ms) =
                        video_info.get("duration_ms").and_then(|v| v.as_f64())
                    {
                        video_info.insert(
                            "vfr_duration_ms".into(),
                            serde_json::json!(duration_ms / fps_scale),
                        );
                    }
                } else {
                    video_info.remove("fps_scale");
                    video_info.insert("vfr_fps".into(), serde_json::json!(source_fps));
                    if let Some(duration_ms) =
                        video_info.get("duration_ms").and_then(|v| v.as_f64())
                    {
                        video_info.insert("vfr_duration_ms".into(), serde_json::json!(duration_ms));
                    }
                }
            } else {
                video_info.insert("vfr_fps".into(), serde_json::json!(fps));
            }
        }
    }
}

fn apply_batch_params_to_stab(stab: &StabilizationManager, params: &serde_json::Value) {
    if let Some(smoothness) = params.get("smoothness").and_then(|v| v.as_f64()) {
        stab.set_smoothing_param("smoothness", smoothness);
    }
    if let Some(amount) = params.get("horizon_lock_amount").and_then(|v| v.as_f64()) {
        let horizon = stab.smoothing.read().horizon_lock.clone();
        stab.set_horizon_lock(
            amount,
            horizon.horizonroll,
            horizon.lock_pitch,
            horizon.horizonpitch,
            horizon.automatic_lock,
            horizon.turn_threshold,
            horizon.turn_smoothing_ms,
            horizon.turn_multiplier,
            horizon.tilt_accel_limit,
        );
    }
    if let Some(zoom_speed) = params.get("zoom_speed").and_then(|v| v.as_f64()) {
        stab.set_adaptive_zoom(zoom_speed);
    } else if let Some(zoom_mode) = params.get("zoom_mode").and_then(|v| v.as_str()) {
        let az = match zoom_mode {
            "static" => -1.0,
            "dynamic" => 4.0,
            _ => 0.0,
        };
        stab.set_adaptive_zoom(az);
    }
    if let Some(lc) = params.get("lens_correction").and_then(|v| v.as_f64()) {
        stab.set_lens_correction_amount(lc);
    }
    if let Some(fps) = params.get("framerate").and_then(|v| v.as_f64()) {
        stab.override_video_fps(fps, true);
    }
}

fn lens_profile_metadata_for_group_build(metadata: &FileMetadata) -> FileMetadata {
    let mut snapshot = metadata.thin();
    snapshot.lens_params = metadata.lens_params.clone();
    snapshot
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
    if let Some(stab) = job.stab.as_ref() {
        let gyro = stab.gyro.read();
        let md = gyro.file_metadata.read();
        let mut snapshot = md.thin();
        if let Some(backup) = job.base_lens_metadata.as_ref() {
            backup.overwrite_metadata(&mut snapshot);
        }
        return Some(snapshot);
    }
    job.base_lens_metadata
        .as_ref()
        .map(JobLensMetadataBackup::to_metadata)
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
    prepare_finished_jobs_for_video_export: qt_method!(fn(&mut self)),
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
    queue_progress: qt_property!(f64; READ get_queue_progress NOTIFY progress_changed),
    queue_done_jobs: qt_property!(u64; READ get_queue_done_jobs NOTIFY progress_changed),
    queue_total_jobs: qt_property!(u64; READ get_queue_total_jobs NOTIFY progress_changed),
    queue_progress_uses_jobs: qt_property!(bool; READ get_queue_progress_uses_jobs NOTIFY progress_changed),
    estimated_remaining_ms: qt_property!(f64; READ get_estimated_remaining_ms NOTIFY progress_changed),
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
    start_queue_work_units: f64,
    eta_model: QueueEtaEstimateModel,

    stabilizer: Arc<StabilizationManager>,

    processing_resolution: i32,

    // Batch gyro matching
    gyro_files: Vec<GyroFileInfo>,
    next_gyro_file_id: u64,
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
    list_video_files_in_folder:
        qt_method!(fn(&self, folder_url: String, extensions_json: String) -> QString),
    filter_paired_gyroflow_siblings:
        qt_method!(fn(&self, urls_json: String, extensions_json: String) -> QString),
    first_renderable_video_file:
        qt_method!(fn(&self, urls_json: String, extensions_json: String) -> QString),
    is_gyro_mix_file: qt_method!(fn(&self, url: String) -> bool),
    has_supported_drop_item:
        qt_method!(fn(&self, urls_json: String, extensions_json: String) -> bool),
    filter_supported_drop_items:
        qt_method!(fn(&self, urls_json: String, extensions_json: String) -> QString),
    first_file_requiring_external_sdk: qt_method!(fn(&self, urls_json: String) -> QString),
    remove_gyro_file: qt_method!(fn(&mut self, index: usize)),
    clear_gyro_files: qt_method!(fn(&mut self)),
    get_gyro_file_count: qt_method!(fn(&self) -> usize),
    get_gyro_file_info_json: qt_method!(fn(&self, index: usize) -> QString),
    has_gyro_files: qt_method!(fn(&self) -> bool),
    batch_motion_ready: qt_method!(fn(&self) -> bool),
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
    get_assigned_gyro_job_ids_json: qt_method!(fn(&self) -> QString),
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
    pub fn get_queue_progress_uses_jobs(&self) -> bool {
        self.queue_progress_uses_weighted_work()
    }
    pub fn get_queue_progress(&self) -> f64 {
        if !self.queue_progress_uses_weighted_work() {
            let total = self.get_total_frames();
            if total == 0 {
                return 0.0;
            }
            return (self.get_current_frame() as f64 / total as f64).clamp(0.0, 1.0);
        }

        let snapshot = self.queue_progress_snapshot();
        let start_units = self.start_queue_work_units.min(snapshot.total_units);
        let total_units = (snapshot.total_units - start_units).max(0.0);
        if total_units <= f64::EPSILON {
            return if snapshot.total_units > 0.0
                && snapshot.done_units >= snapshot.total_units
            {
                1.0
            } else {
                0.0
            };
        }

        ((snapshot.done_units - start_units).max(0.0) / total_units).clamp(0.0, 1.0)
    }
    pub fn get_queue_done_jobs(&self) -> u64 {
        self.queue_progress_snapshot().done_jobs
    }
    pub fn get_queue_total_jobs(&self) -> u64 {
        self.queue_progress_snapshot().total_jobs
    }
    pub fn get_estimated_remaining_ms(&self) -> f64 {
        if self.status.to_string() != "active" {
            return -1.0;
        }
        self.estimated_remaining_ms()
            .map(|v| v as f64)
            .unwrap_or(-1.0)
    }

    fn estimated_remaining_ms(&self) -> Option<u64> {
        let q = self.queue.try_borrow().ok()?;
        let mut sync_frames = 0usize;
        let mut render_frames = 0usize;
        let exports_video = self.exports_video();

        for item in q.iter() {
            match item.status {
                JobStatus::Queued => {
                    if exports_video {
                        render_frames = render_frames.saturating_add(item.total_frames as usize);
                    }
                    if let Some(job) = self.jobs.get(&item.job_id) {
                        sync_frames =
                            sync_frames.saturating_add(Self::estimated_sync_frames_for_job(job));
                    }
                }
                JobStatus::Rendering => {
                    if exports_video {
                        render_frames = render_frames.saturating_add(
                            item.total_frames.saturating_sub(item.current_frame) as usize,
                        );
                    }
                    if item.current_frame == 0
                        && item.processing_progress > 0.0
                        && item.processing_progress < 1.0
                    {
                        if let Some(job) = self.jobs.get(&item.job_id) {
                            let estimated_sync = Self::estimated_sync_frames_for_job(job);
                            sync_frames = sync_frames.saturating_add(
                                (estimated_sync as f64 * (1.0 - item.processing_progress))
                                    .ceil()
                                    .max(0.0) as usize,
                            );
                        }
                    }
                }
                JobStatus::Finished | JobStatus::Error | JobStatus::Skipped => {}
            }
        }

        self.eta_model.estimate_remaining_ms(
            sync_frames,
            render_frames,
            self.parallel_renders.max(1) as usize,
        )
    }

    fn queue_progress_uses_weighted_work(&self) -> bool {
        self.export_project == 2 || self.export_project == 4
    }

    fn queue_progress_snapshot(&self) -> QueueProgressSnapshot {
        let q = match self.queue.try_borrow() {
            Ok(q) => q,
            Err(_) => return QueueProgressSnapshot::default(),
        };
        let exports_video = self.exports_video();
        let mut snapshot = QueueProgressSnapshot::default();

        for item in q.iter() {
            let estimated_sync_units = self
                .jobs
                .get(&item.job_id)
                .map(Self::estimated_sync_frames_for_job)
                .unwrap_or_default() as f64;
            let render_units = if exports_video {
                item.total_frames as f64
            } else {
                0.0
            };
            let sync_units =
                Self::queue_item_sync_work_units(item, estimated_sync_units, render_units);
            let mut total_units = sync_units + render_units;
            if total_units <= f64::EPSILON {
                total_units = 1.0;
            }

            let done_units = match item.status {
                JobStatus::Finished | JobStatus::Skipped => total_units,
                JobStatus::Queued | JobStatus::Rendering | JobStatus::Error => {
                    Self::queue_item_active_work_units(item, sync_units, render_units)
                        .min(total_units)
                }
            };

            snapshot.done_units += done_units;
            snapshot.total_units += total_units;
            snapshot.total_jobs += 1;
            if matches!(item.status, JobStatus::Finished | JobStatus::Skipped) {
                snapshot.done_jobs += 1;
            }
        }

        snapshot
    }

    fn queue_item_sync_work_units(
        item: &RenderQueueItem,
        estimated_sync_units: f64,
        render_units: f64,
    ) -> f64 {
        if estimated_sync_units > f64::EPSILON {
            estimated_sync_units
        } else if render_units <= f64::EPSILON || item.processing_progress > 0.0 {
            1.0
        } else {
            0.0
        }
    }

    fn queue_item_active_work_units(
        item: &RenderQueueItem,
        sync_units: f64,
        render_units: f64,
    ) -> f64 {
        let processing = item.processing_progress.clamp(0.0, 1.0);
        if render_units > 0.0 && item.current_frame > 0 {
            sync_units + (item.current_frame.min(item.total_frames) as f64)
        } else {
            sync_units * processing
        }
    }

    fn exports_video(&self) -> bool {
        self.export_metadata.is_none()
            && self.export_stmap.is_none()
            && (self.export_project == 0 || self.export_project == 4)
    }

    fn observe_eta_sample_for_epoch(
        &mut self,
        job_id: u32,
        capture_epoch: u64,
        sample: QueueEtaSample,
    ) -> bool {
        let current_epoch = self
            .jobs
            .get(&job_id)
            .map(|j| j.render_epoch.load(SeqCst))
            .unwrap_or(0);
        if current_epoch != capture_epoch {
            return false;
        }
        self.eta_model.observe_completed_job(sample);
        true
    }

    fn submit_sync_eta_sample<F>(eta_sample: &ParkingMutex<QueueEtaSample>, eta_sample_done: &F)
    where
        F: Fn(QueueEtaSample),
    {
        let sample = *eta_sample.lock();
        if sample.sync_frames > 0 {
            eta_sample_done(sample);
        }
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
                itm.export_settings = QString::from(render_options.settings_string(params.get_scaled_fps()));
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
                export_settings: QString::from(render_options.settings_string(params.get_scaled_fps())),
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
                last_finished_export_project: None,
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
        ::log::info!(
            "[queue_signal] added job_id={} source=add_internal input='{}'",
            job_id,
            self.jobs
                .get(&job_id)
                .map(|job| job.render_options.input_filename.as_str())
                .unwrap_or_default()
        );
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
            self.start_queue_work_units = self.queue_progress_snapshot().done_units;
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
            self.start_queue_work_units = 0.0;
            self.start_timestamp = Self::current_timestamp();
            self.start_frame = self.get_current_frame();
            self.start_queue_work_units = self.queue_progress_snapshot().done_units;
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
                    self.start_queue_work_units = 0.0;
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

        self.start_timestamp = 0;
        self.start_frame = 0;
        self.start_queue_work_units = 0.0;
        self.status = QString::from("stopped");
        self.status_changed();
        self.progress_changed();
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
        if let Some(job) = self.jobs.get_mut(&job_id) {
            job.cancel_flag.store(false, SeqCst);
            job.last_finished_export_project = None;
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
            itm.processing_progress = 0.0;
            itm.current_frame = 0;
            itm.start_timestamp = 0;
            itm.start_timestamp2 = 0;
            itm.start_timestamp_frame = 0;
            itm.end_timestamp = 0;
            itm.frame_times.clear();
            itm.status = JobStatus::Queued;
        });
    }
    pub fn prepare_finished_jobs_for_video_export(&mut self) {
        let finished_job_ids = {
            let q = self.queue.borrow();
            q.iter()
                .filter(|v| v.status == JobStatus::Finished)
                .map(|v| v.job_id)
                .collect::<Vec<_>>()
        };

        let sync_only_job_ids = finished_job_ids
            .into_iter()
            .filter(|job_id| {
                self.jobs
                    .get(job_id)
                    .map(|job| job.last_finished_export_project == Some(2))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();

        for job_id in sync_only_job_ids {
            if let Some(job) = self.jobs.get_mut(&job_id) {
                remove_do_autosync_from_project_json(&mut job.additional_data);
                if let Some(ref mut project_data) = job.project_data {
                    remove_do_autosync_from_project_json(project_data);
                }
                if let Some(ref stab) = job.stab {
                    remove_do_autosync_from_stab(stab);
                }
            }
            self.reset_job(job_id);
            if let Some(job) = self.jobs.get(&job_id) {
                if let Some(ref stab) = job.stab {
                    remove_do_autosync_from_stab(stab);
                }
            }
        }
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
        Self::get_gyroflow_data_internal_with_type(
            stab,
            additional_data,
            render_options,
            core::GyroflowProjectType::Simple,
            true,
        )
    }

    fn get_gyroflow_data_internal_with_type(
        stab: &StabilizationManager,
        additional_data: &str,
        render_options: &RenderOptions,
        typ: core::GyroflowProjectType,
        allow_project_file_reference: bool,
    ) -> Option<String> {
        if allow_project_file_reference {
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
        if let Ok(data) = stab.export_gyroflow_data(typ, &additional_data, None) {
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

            if let (Some(lens_index), Some(metadata)) =
                (lens_group_index, metadata_snapshot.as_ref())
            {
                if let Some((config, is_local)) =
                    effective_lens_group_config_for_group(job, &global_configs, lens_index)
                {
                    if let Some(display_config) =
                        niyien_lens_presets::effective_lens_group_config_for_build(
                            self.stabilizer.get_lens_group_manual_edit(),
                            config,
                            metadata,
                        )
                    {
                        lens_group_mode = if is_local { "local" } else { "global" };
                        lens_group_number = lens_index + 1;
                        lens_group_focal_length =
                            display_config.focal_length_mm.unwrap_or_default();
                        if display_config.anamorphic_enabled {
                            if let Some(anamorphic) =
                                niyien_lens_presets::resolve_anamorphic_config(
                                    Some(&display_config),
                                )
                            {
                                lens_group_ratio = anamorphic.squeeze_ratio;
                                lens_group_direction = match anamorphic.squeeze_direction {
                                    niyien_lens_presets::SqueezeDirection::Horizontal => {
                                        "H".to_owned()
                                    }
                                    niyien_lens_presets::SqueezeDirection::Vertical => {
                                        "V".to_owned()
                                    }
                                };
                            }
                        }
                        if lens_group_focal_length
                            <= niyien_lens_presets::MANUAL_FOCAL_LENGTH_MIN_MM
                        {
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
                    let focal_length = v
                        .get("video_info")
                        .and_then(|vi| vi.get("focal_length"))
                        .and_then(|f| f.as_f64())
                        .unwrap_or(0.0);
                    let source_fps = v
                        .get("video_info")
                        .and_then(|vi| vi.get("fps"))
                        .and_then(|f| f.as_f64())
                        .unwrap_or(0.0);
                    let fps_scale = v
                        .get("video_info")
                        .and_then(|vi| vi.get("fps_scale"))
                        .and_then(|f| f.as_f64());
                    let framerate = v
                        .get("video_info")
                        .and_then(|vi| vi.get("vfr_fps"))
                        .and_then(|f| f.as_f64())
                        .or_else(|| fps_scale.map(|scale| source_fps * scale))
                        .unwrap_or(source_fps);
                    let display_focal_length = if focal_length
                        > niyien_lens_presets::MANUAL_FOCAL_LENGTH_MIN_MM
                    {
                        focal_length
                    } else {
                        metadata_focal_length
                    };
                    if lens_group_mode != "auto"
                        && lens_group_focal_length
                            <= niyien_lens_presets::MANUAL_FOCAL_LENGTH_MIN_MM
                    {
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
                        "source_fps": source_fps,
                        "fps_scale": fps_scale,
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
                "source_fps": 0.0,
                "framerate": 0.0,
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
            let mut export_settings = None;
            if let Some(job) = self.jobs.get_mut(&job_id) {
                if let Some(ref mut data_str) = job.project_data {
                    if let Ok(mut data) = serde_json::from_str::<serde_json::Value>(data_str) {
                        update_project_data_batch_params(&mut data, &params);
                        *data_str = serde_json::to_string(&data).unwrap_or_default();
                    }
                }
                if let Some(ref stab) = job.stab {
                    apply_batch_params_to_stab(stab, &params);
                    export_settings =
                        Some(job.render_options.settings_string(stab.params.read().get_scaled_fps()));
                }
            }
            if let Some(export_settings) = export_settings {
                update_model!(self, job_id, itm {
                    itm.export_settings = QString::from(export_settings.as_str());
                });
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
            let eta_sample = Arc::new(ParkingMutex::new(QueueEtaSample::default()));
            let eta_sample_done = util::qt_queued_callback_mut(
                QPointer::from(self as &Self),
                move |this, sample: QueueEtaSample| {
                    if this.observe_eta_sample_for_epoch(job_id, capture_epoch, sample) {
                        this.progress_changed();
                    }
                },
            );
            let finished_export_project = self.export_project;
            let finished_project_type = match finished_export_project {
                2 | 4 => core::GyroflowProjectType::WithGyroData,
                3 => core::GyroflowProjectType::WithProcessedData,
                _ => core::GyroflowProjectType::Simple,
            };
            let allow_finished_project_file_reference = finished_export_project != 2;
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
                                job.project_data = Self::get_gyroflow_data_internal_with_type(
                                    stab,
                                    &job.additional_data,
                                    &job.render_options,
                                    finished_project_type.clone(),
                                    allow_finished_project_file_reference,
                                );
                            }
                            job.last_finished_export_project = Some(finished_export_project);
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
                            this.start_queue_work_units = 0.0;
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
                    this.progress_changed();
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
                        this.start_queue_work_units = 0.0;
                    }
                    this.update_status();
                    this.progress_changed();
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
                        this.start_queue_work_units = 0.0;
                    }
                    this.update_status();
                    this.progress_changed();
                },
            );
            let params = stab.params.read();
            let trim_ratio = params.get_trim_ratio();
            let total_frame_count = params.frame_count;
            let render_frame_count = (total_frame_count as f64 * trim_ratio).round() as usize;
            drop(params);
            let mut input_file = stab.input_file.read().clone();
            let filename = filesystem::get_filename(&input_file.url);
            let render_options = job.render_options.clone();

            progress((
                0.0,
                0,
                render_frame_count,
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
                let sync_start = std::time::Instant::now();
                let sync_stats = Self::do_autosync(
                    stab.clone(),
                    processing,
                    &input_file,
                    err2,
                    proc_height,
                    sync_cancel_flag,
                );
                if sync_stats.completed && sync_stats.frames > 0 {
                    let mut sample = eta_sample.lock();
                    sample.sync_frames = sync_stats.frames;
                    sample.sync_ms = sync_start.elapsed().as_secs_f64() * 1000.0;
                }
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
                        Self::submit_sync_eta_sample(eta_sample.as_ref(), &eta_sample_done);
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
                    Self::submit_sync_eta_sample(eta_sample.as_ref(), &eta_sample_done);
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
                            Self::submit_sync_eta_sample(eta_sample.as_ref(), &eta_sample_done);
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
                let render_start = std::time::Instant::now();
                let mut render_ok = true;
                'ranges: for range in ranges_to_render {
                    if cancel_flag.load(SeqCst) {
                        render_ok = false;
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
                                render_ok = false;
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
                            render_ok = false;
                            break 'ranges;
                        } else {
                            // Render ok
                            break;
                        }
                    }
                }
                stab.gpu_decoding.store(original_gpu_decode, SeqCst);
                if render_ok && !cancel_flag.load(SeqCst) {
                    let sample = {
                        let mut sample = eta_sample.lock();
                        sample.render_frames = render_frame_count;
                        sample.render_ms = render_start.elapsed().as_secs_f64() * 1000.0;
                        *sample
                    };
                    eta_sample_done(sample);
                }
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

    fn estimated_sync_frames_for_job(job: &Job) -> usize {
        job.stab
            .as_ref()
            .map(|stab| Self::estimated_sync_frames_for_stab(stab))
            .unwrap_or_default()
    }

    fn estimated_sync_frames_for_stab(stab: &StabilizationManager) -> usize {
        if stab.gyro.read().file_metadata.read().is_komodo {
            return 0;
        }

        let (url, duration_ms, fps, frame_count, fps_scale) = {
            let params = stab.params.read();
            (
                stab.input_file.read().url.clone(),
                params.duration_ms,
                params.fps,
                params.frame_count,
                params.fps_scale,
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

        let sync_settings = stab.lens.read().sync_settings.clone().unwrap_or_default();
        let force_autosync = sync_settings
            .get("do_autosync")
            .and_then(|v| v.as_bool())
            .unwrap_or_default();
        if !(force_autosync || (!has_sync_points && !has_accurate_timestamps)) {
            return 0;
        }

        let Ok(sync_params) = serde_json::from_value::<gyroflow_core::synchronization::SyncParams>(
            sync_settings,
        ) else {
            return 0;
        };
        if sync_params.max_sync_points == 0 {
            return 0;
        }

        let mut sync_point_count = sync_params.max_sync_points;
        if !sync_params.custom_sync_pattern.is_null() {
            let custom_count = Self::resolve_syncpoint_pattern(
                &sync_params.custom_sync_pattern,
                duration_ms,
                fps,
            )
            .into_iter()
            .filter(|v| *v <= duration_ms)
            .count();
            if custom_count > 0 {
                sync_point_count = custom_count;
            }
        }

        let mut time_per_syncpoint_ms = sync_params.time_per_syncpoint * 1000.0;
        if let Some(scale) = fps_scale {
            time_per_syncpoint_ms *= scale;
        }
        let every_nth_frame = sync_params.every_nth_frame.max(1);
        let frame_count = ((sync_point_count as f64 * (time_per_syncpoint_ms / 1000.0) * fps)
            .ceil() as usize)
            .min(frame_count)
            / every_nth_frame;

        let search_size_ms = sync_params.search_size * 1000.0;
        if duration_ms < 10.0 || frame_count < 2 || time_per_syncpoint_ms < 10.0 || search_size_ms < 10.0 {
            return 0;
        }

        frame_count
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
                ::log::warn!(
                    "[queue_signal] error job_id={} source=add_file msg='{}' arg='{}'",
                    job_id,
                    msg,
                    arg
                );
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
                            ::log::warn!(
                                "[queue_signal] error job_id={} source=add_file_exists arg='{}'",
                                job_id,
                                msg
                            );
                            this.error(job_id, msg, QString::default(), QString::default());
                        }
                    }
                }

                ::log::info!(
                    "[queue_signal] processing_done job_id={} by_preset=false source=add_file",
                    job_id
                );
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
                            ::log::info!(
                                "[queue_signal] added emitted_job_id={} source=apply_preset preset_target_job_id={}",
                                job_id,
                                to_job_id
                            );
                            this.added(job_id);
                        },
                    );

                    core::run_threaded(move || {
                        let fetch_thumb =
                            |video_url: &str, ratio: f64| -> Result<(), rendering::FFmpegError> {
                                let t_thumb = std::time::Instant::now();
                                ::log::info!(
                                    "[queue_add:fetch_thumb] begin job_id={} file='{}' ratio={:.6}",
                                    job_id,
                                    filesystem::get_filename(video_url),
                                    ratio
                                );
                                let mut fetched = false;
                                if !crate::cli::will_run_in_console() {
                                    // Don't fetch thumbs in the CLI
                                    let t_proc = std::time::Instant::now();
                                    let mut proc = match rendering::VideoProcessor::from_file(
                                        video_url, false, 0, None,
                                    ) {
                                        Ok(proc) => proc,
                                        Err(e) => {
                                            ::log::warn!(
                                                "[queue_add:fetch_thumb] processor_error job_id={} file='{}' elapsed_ms={:.1} total_elapsed_ms={:.1} error={}",
                                                job_id,
                                                filesystem::get_filename(video_url),
                                                t_proc.elapsed().as_secs_f64() * 1000.0,
                                                t_thumb.elapsed().as_secs_f64() * 1000.0,
                                                e
                                            );
                                            return Err(e);
                                        }
                                    };
                                    ::log::info!(
                                        "[queue_add:fetch_thumb] processor_ready job_id={} file='{}' elapsed_ms={:.1}",
                                        job_id,
                                        filesystem::get_filename(video_url),
                                        t_proc.elapsed().as_secs_f64() * 1000.0
                                    );
                                    proc.on_frame(move |_timestamp_us, input_frame, _output_frame, converter, _rate_control| {
                                    let sf = converter.scale(input_frame, ffmpeg_next::format::Pixel::RGBA, (50.0 * ratio).round() as u32, 50)?;

                                    if !fetched {
                                        thumb_fetched(util::image_data_to_base64(sf.plane_width(0), sf.plane_height(0), sf.stride(0) as u32, sf.data(0)));
                                        fetched = true;
                                    }

                                    Ok(())
                                });
                                    let t_decode = std::time::Instant::now();
                                    ::log::info!(
                                        "[thumb_decoder] start job_id={} file='{}' ranges={:?}",
                                        job_id,
                                        filesystem::get_filename(video_url),
                                        vec![(0.0, 50.0)]
                                    );
                                    proc.start_decoder_only(
                                        vec![(0.0, 50.0)],
                                        Arc::new(AtomicBool::new(true)),
                                    )
                                    .map_err(|e| {
                                        ::log::warn!(
                                            "[thumb_decoder] error job_id={} file='{}' elapsed_ms={:.1} total_elapsed_ms={:.1} error={}",
                                            job_id,
                                            filesystem::get_filename(video_url),
                                            t_decode.elapsed().as_secs_f64() * 1000.0,
                                            t_thumb.elapsed().as_secs_f64() * 1000.0,
                                            e
                                        );
                                        e
                                    })?;
                                    ::log::info!(
                                        "[thumb_decoder] end job_id={} file='{}' elapsed_ms={:.1}",
                                        job_id,
                                        filesystem::get_filename(video_url),
                                        t_decode.elapsed().as_secs_f64() * 1000.0
                                    );
                                }
                                ::log::info!(
                                    "[queue_add:fetch_thumb] end job_id={} file='{}' elapsed_ms={:.1}",
                                    job_id,
                                    filesystem::get_filename(video_url),
                                    t_thumb.elapsed().as_secs_f64() * 1000.0
                                );
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

                                        let t_thumb_call = std::time::Instant::now();
                                        if let Err(e) = fetch_thumb(out, ratio) {
                                            ::log::warn!(
                                                "[queue_add:fetch_thumb] error job_id={} file='{}' elapsed_ms={:.1} error={}",
                                                job_id,
                                                filesystem::get_filename(out),
                                                t_thumb_call.elapsed().as_secs_f64() * 1000.0,
                                                e
                                            );
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
                        } else {
                            let t_add = std::time::Instant::now();
                            ::log::info!(
                                "[queue_add] start job_id={} file='{}' url='{}' gyro_url='{}' is_gf_data={}",
                                job_id,
                                filesystem::get_filename(&url),
                                url,
                                gyro_url,
                                is_gf_data
                            );
                            let t_info = std::time::Instant::now();
                            ::log::info!(
                                "[queue_add:get_video_info] begin job_id={} file='{}'",
                                job_id,
                                filesystem::get_filename(&url)
                            );
                            match rendering::VideoProcessor::get_video_info(&url) {
                                Ok(info) => {
                                    ::log::info!(
                                        "[queue_add:get_video_info] end job_id={} file='{}' elapsed_ms={:.1} width={} height={} fps={:.6} duration_ms={:.3} frame_count={} created_at={:?} rotation={} bitrate={}",
                                        job_id,
                                        filesystem::get_filename(&url),
                                        t_info.elapsed().as_secs_f64() * 1000.0,
                                        info.width,
                                        info.height,
                                        info.fps,
                                        info.duration_ms,
                                        info.frame_count,
                                        info.created_at,
                                        info.rotation,
                                        info.bitrate
                                    );
                                    ::log::debug!("Loaded {:?}", &info);

                                    render_options.bitrate =
                                        render_options.bitrate.max(info.bitrate);
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
                                    let t_open = std::time::Instant::now();
                                    ::log::info!(
                                        "[queue_add:open_gyro] begin job_id={} file='{}' url='{}'",
                                        job_id,
                                        filesystem::get_filename(&gyro_url),
                                        gyro_url
                                    );
                                    match filesystem::open_file(&gyro_url, false, false) {
                                        Ok(mut file) => {
                                            let filesize = file.size;
                                            ::log::info!(
                                                "[queue_add:open_gyro] end job_id={} file='{}' filesize={} elapsed_ms={:.1}",
                                                job_id,
                                                filesystem::get_filename(&gyro_url),
                                                filesize,
                                                t_open.elapsed().as_secs_f64() * 1000.0
                                            );
                                            let t_load = std::time::Instant::now();
                                            ::log::info!(
                                                "[queue_add:load_gyro_data] begin job_id={} file='{}' filesize={} is_main_video={} header_only=false time_range_ms=None",
                                                job_id,
                                                filesystem::get_filename(&gyro_url),
                                                filesize,
                                                is_main_video
                                            );
                                            let load_result = stab.load_gyro_data(
                                            file.get_file(),
                                            filesize,
                                            &gyro_url,
                                            is_main_video,
                                            &Default::default(),
                                            |_| (),
                                            Arc::new(AtomicBool::new(false)),
                                            );
                                            match load_result {
                                                Ok(()) => {
                                                    let (
                                                        raw_imu,
                                                        quaternions,
                                                        lens_params,
                                                        lens_positions,
                                                        creation_date_utc,
                                                        has_accurate_timestamps,
                                                        detected_source,
                                                        is_komodo,
                                                    ) = {
                                                        let gyro = stab.gyro.read();
                                                        let md = gyro.file_metadata.read();
                                                        (
                                                            md.raw_imu.len(),
                                                            md.quaternions.len(),
                                                            md.lens_params.len(),
                                                            md.lens_positions.len(),
                                                            md.creation_date_utc.clone(),
                                                            md.has_accurate_timestamps,
                                                            md.detected_source.clone(),
                                                            md.is_komodo,
                                                        )
                                                    };
                                                    ::log::info!(
                                                        "[queue_add:load_gyro_data] end job_id={} file='{}' elapsed_ms={:.1} raw_imu={} quats={} lens_params={} lens_positions={} creation_date_utc={:?} accurate_ts={} detected={:?} is_komodo={}",
                                                        job_id,
                                                        filesystem::get_filename(&gyro_url),
                                                        t_load.elapsed().as_secs_f64() * 1000.0,
                                                        raw_imu,
                                                        quaternions,
                                                        lens_params,
                                                        lens_positions,
                                                        creation_date_utc,
                                                        has_accurate_timestamps,
                                                        detected_source,
                                                        is_komodo
                                                    );
                                                }
                                                Err(e) => {
                                                    ::log::warn!(
                                                        "[queue_add:load_gyro_data] error job_id={} file='{}' elapsed_ms={:.1} error={}",
                                                        job_id,
                                                        filesystem::get_filename(&gyro_url),
                                                        t_load.elapsed().as_secs_f64() * 1000.0,
                                                        e
                                                    );
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            ::log::warn!(
                                                "[queue_add:open_gyro] error job_id={} file='{}' elapsed_ms={:.1} error={}",
                                                job_id,
                                                filesystem::get_filename(&gyro_url),
                                                t_open.elapsed().as_secs_f64() * 1000.0,
                                                e
                                            );
                                        }
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

                                let t_recompute = std::time::Instant::now();
                                ::log::info!(
                                    "[queue_add:recompute] begin job_id={} file='{}'",
                                    job_id,
                                    filesystem::get_filename(&url)
                                );
                                stab.recompute_blocking();
                                ::log::info!(
                                    "[queue_add:recompute] end job_id={} file='{}' elapsed_ms={:.1}",
                                    job_id,
                                    filesystem::get_filename(&url),
                                    t_recompute.elapsed().as_secs_f64() * 1000.0
                                );

                                // println!("{}", stab.export_gyroflow_data(true, serde_json::to_string(&render_options).unwrap_or_default()));

                                ::log::info!(
                                    "[queue_add:loaded] emit job_id={} file='{}'",
                                    job_id,
                                    filesystem::get_filename(&url)
                                );
                                loaded(render_options);

                                Self::update_sync_settings(&stab, &sync_options);

                                // Apply default preset
                                let default_preset = gyroflow_core::lens_profile_database::LensProfileDatabase::get_path().join("default.gyroflow");
                                let default_preset2 = gyroflow_core::settings::data_dir()
                                    .join("lens_profiles")
                                    .join("default.gyroflow");
                                let t_preset = std::time::Instant::now();
                                if let Ok(data) = std::fs::read_to_string(default_preset2) {
                                    ::log::info!(
                                        "[queue_add:default_preset] apply user preset job_id={} file='{}' read_elapsed_ms={:.1}",
                                        job_id,
                                        filesystem::get_filename(&url),
                                        t_preset.elapsed().as_secs_f64() * 1000.0
                                    );
                                    apply_preset((data, job_id));
                                } else if let Ok(data) = std::fs::read_to_string(default_preset) {
                                    ::log::info!(
                                        "[queue_add:default_preset] apply bundled preset job_id={} file='{}' read_elapsed_ms={:.1}",
                                        job_id,
                                        filesystem::get_filename(&url),
                                        t_preset.elapsed().as_secs_f64() * 1000.0
                                    );
                                    apply_preset((data, job_id));
                                } else {
                                    ::log::info!(
                                        "[queue_add:default_preset] none job_id={} file='{}' elapsed_ms={:.1}",
                                        job_id,
                                        filesystem::get_filename(&url),
                                        t_preset.elapsed().as_secs_f64() * 1000.0
                                    );
                                }

                                let t_thumb_call = std::time::Instant::now();
                                if let Err(e) = fetch_thumb(&url, ratio) {
                                    ::log::warn!(
                                        "[queue_add:fetch_thumb] error job_id={} file='{}' elapsed_ms={:.1} error={}",
                                        job_id,
                                        filesystem::get_filename(&url),
                                        t_thumb_call.elapsed().as_secs_f64() * 1000.0,
                                        e
                                    );
                                    err(("An error occured: %1".to_string(), e.to_string()));
                                }

                                ::log::info!(
                                    "[queue_add] end job_id={} file='{}' elapsed_ms={:.1}",
                                    job_id,
                                    filesystem::get_filename(&url),
                                    t_add.elapsed().as_secs_f64() * 1000.0
                                );
                                        processing_done(());
                                    } else {
                                        ::log::warn!(
                                            "[queue_add] invalid_video_info job_id={} file='{}' elapsed_ms={:.1} duration_ms={:.3} fps={:.6}",
                                            job_id,
                                            filesystem::get_filename(&url),
                                            t_add.elapsed().as_secs_f64() * 1000.0,
                                            info.duration_ms,
                                            info.fps
                                        );
                                    }
                                }
                                Err(e) => {
                                    ::log::warn!(
                                        "[queue_add:get_video_info] error job_id={} file='{}' elapsed_ms={:.1} error={}",
                                        job_id,
                                        filesystem::get_filename(&url),
                                        t_info.elapsed().as_secs_f64() * 1000.0,
                                        e
                                    );
                                    err((
                                        "An error occured: %1".to_string(),
                                        "Unable to read the video file.".to_string(),
                                    ));
                                }
                            }
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
    ) -> QueueAutosyncStats {
        // C3: Komodo trusts its own internal IMU; auto-sync against external IMU
        // is unnecessary and would compute a meaningless offset. Skip entirely.
        if stab.gyro.read().file_metadata.read().is_komodo {
            let url = stab.input_file.read().url.clone();
            ::log::info!(
                "[red_arbitration] Komodo main video, skipping auto-sync: {url}"
            );
            return QueueAutosyncStats::default();
        }

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
            ::log::info!(
                "do_autosync clearing {} stale sync point(s) for {}",
                stale,
                url
            );
        }
        if force_autosync || (!has_sync_points && !has_accurate_timestamps) {
            // ----------------------------------------------------------------------------
            // --------------------------------- Autosync ---------------------------------
            let mut sync_frames = 0usize;
            let sync_failed = Arc::new(AtomicBool::new(false));
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
                        let sync_frame_count = Arc::new(AtomicUsize::new(0));
                        let processing_cb2 = processing_cb.clone();
                        let sync_frame_count2 = sync_frame_count.clone();
                        sync.on_progress(move |percent, ready, total| {
                            sync_frame_count2.store(total.max(ready), SeqCst);
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
                                    if confidence < 0.4 {
                                        // Drop low-confidence sync points unconditionally
                                        // (NCC fusion's weak_signal pearson_peak can pick
                                        // noise peaks; previously rank-bypass let stale
                                        // high-rank entries through after auto sync filled
                                        // sync_data.rank).
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
                                let sync_failed2 = sync_failed.clone();
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
                                            match converter.scale(input_frame, pix_fmt, sw, sh) {
                                                Ok(small_frame) => {
                                                    let (width, height, stride, pixels) =
                                                        if of_method == 3 || of_method == 4 {
                                                            // NV12: pass all planes (Y + UV)
                                                            let total_len = small_frame.stride(0)
                                                                * small_frame.plane_height(0)
                                                                    as usize
                                                                + small_frame.stride(1)
                                                                    * small_frame.plane_height(1)
                                                                        as usize;
                                                            let mut all_data =
                                                                Vec::with_capacity(total_len);
                                                            all_data.extend_from_slice(
                                                                &small_frame.data(0)[..small_frame
                                                                    .stride(0)
                                                                    * small_frame.plane_height(0)
                                                                        as usize],
                                                            );
                                                            all_data.extend_from_slice(
                                                                &small_frame.data(1)[..small_frame
                                                                    .stride(1)
                                                                    * small_frame.plane_height(1)
                                                                        as usize],
                                                            );
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
                                                Err(e) => {
                                                    sync_failed2.store(true, SeqCst);
                                                    err2((
                                                        "An error occured: %1".to_string(),
                                                        e.to_string(),
                                                    ));
                                                }
                                            }
                                            frame_no += 1;
                                        }
                                        abs_frame_no += 1;
                                        Ok(())
                                    },
                                );
                                if let Err(e) =
                                    proc.start_decoder_only(sync.get_ranges(), cancel_flag.clone())
                                {
                                    sync_failed.store(true, SeqCst);
                                    err(("An error occured: %1".to_string(), e.to_string()));
                                }

                                sync.finished_feeding_frames();
                                sync_frames = sync_frame_count.load(SeqCst);
                            }
                            Err(error) => {
                                sync_failed.store(true, SeqCst);
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
                        sync_failed.store(true, SeqCst);
                        err(("An error occured: %1".to_string(), detail));
                    }

                    stab.recompute_blocking();
                }
            }
            processing_cb(1.0);
            // --------------------------------- Autosync ---------------------------------
            // ----------------------------------------------------------------------------
            return QueueAutosyncStats {
                frames: sync_frames,
                completed: sync_frames > 0
                    && !sync_failed.load(SeqCst)
                    && !cancel_flag.load(SeqCst),
            };
        }
        QueueAutosyncStats::default()
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
                ::log::info!(
                    "[queue_signal] processing_done job_id={} by_preset=true source=apply_to_all",
                    job_id
                );
                this.processing_done(job_id, true);
            });
        let err = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, (job_id, msg): (u32, String)| {
                ::log::warn!(
                    "[queue_signal] error job_id={} source=apply_to_all msg='{}'",
                    job_id,
                    msg
                );
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
                                job.render_options
                                    .settings_string(stab.params.read().get_scaled_fps()),
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
        let gyro_file_id = self.next_gyro_file_id;
        self.next_gyro_file_id = self.next_gyro_file_id.wrapping_add(1);
        let index = self.gyro_files.len();
        self.gyro_files.push(GyroFileInfo {
            id: gyro_file_id,
            path: url.clone(),
            filename,
            ..Default::default()
        });
        self.gyro_files_changed();

        // T2: Background metadata parsing
        let callback_url = url.clone();
        let on_parsed = util::qt_queued_callback_mut(
            QPointer::from(self as &Self),
            move |this, result: (Option<i64>, Option<f64>, Option<String>, Option<String>)| {
                if this.update_gyro_file_parse_result(
                    index,
                    gyro_file_id,
                    &callback_url,
                    result,
                ) {
                    this.gyro_files_changed();
                }
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

    // Recursively list video files under a folder, filtered by extension whitelist
    // and excluding files whose stem ends with the configured default output suffix
    // (e.g. "_stabilized"). Returns a JSON array of URL strings.
    fn list_video_files_in_folder(&self, folder_url: String, extensions_json: String) -> QString {
        const MAX_VIDEO_FOLDER_DEPTH: usize = 3;
        const MAX_VIDEO_FOLDER_RESULTS: usize = 600;

        let path = filesystem::url_to_path(&folder_url);
        let dir = std::path::Path::new(&path);
        if !dir.is_dir() {
            ::log::warn!("[list_video_files_in_folder] not a directory: {}", path);
            return QString::from("[]");
        }

        let exts_lower: Vec<String> = serde_json::from_str::<Vec<String>>(&extensions_json)
            .unwrap_or_default()
            .into_iter()
            .map(|e| e.to_ascii_lowercase())
            .collect();

        let suffix_lower = self.default_suffix.to_string().to_ascii_lowercase();

        let mut found: Vec<std::path::PathBuf> = Vec::new();
        Self::scan_video_folder(
            dir,
            0,
            MAX_VIDEO_FOLDER_DEPTH,
            MAX_VIDEO_FOLDER_RESULTS,
            &exts_lower,
            &suffix_lower,
            &mut found,
        );

        let urls: Vec<String> = found
            .iter()
            .map(|p| filesystem::path_to_url(&p.to_string_lossy()))
            .collect();

        ::log::info!(
            "[list_video_files_in_folder] root={}, returned {} videos (max_depth={}, cap={})",
            path,
            urls.len(),
            MAX_VIDEO_FOLDER_DEPTH,
            MAX_VIDEO_FOLDER_RESULTS
        );
        QString::from(serde_json::to_string(&urls).unwrap_or_else(|_| "[]".to_string()))
    }

    fn scan_video_folder(
        dir: &std::path::Path,
        depth: usize,
        max_depth: usize,
        max_results: usize,
        exts_lower: &[String],
        suffix_lower: &str,
        out: &mut Vec<std::path::PathBuf>,
    ) {
        if depth > max_depth {
            return;
        }
        if out.len() >= max_results {
            return;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                ::log::warn!(
                    "[scan_video_folder] cannot read dir {}: {:?}",
                    dir.display(),
                    e
                );
                return;
            }
        };

        let mut files: Vec<std::path::PathBuf> = Vec::new();
        let mut subdirs: Vec<std::path::PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                subdirs.push(p);
            } else if p.is_file() {
                files.push(p);
            }
        }
        files.sort();
        subdirs.sort();

        for p in files {
            if out.len() >= max_results {
                return;
            }
            let ext_ok = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let el = e.to_ascii_lowercase();
                    exts_lower.iter().any(|x| x == &el)
                })
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            if !suffix_lower.is_empty() {
                let stem_matches = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_ascii_lowercase().ends_with(suffix_lower))
                    .unwrap_or(false);
                if stem_matches {
                    continue;
                }
            }
            out.push(p);
        }

        for d in subdirs {
            if out.len() >= max_results {
                return;
            }
            Self::scan_video_folder(
                &d,
                depth + 1,
                max_depth,
                max_results,
                exts_lower,
                suffix_lower,
                out,
            );
        }
    }

    // Given a JSON array of URLs, drop any .gyroflow whose stem matches a
    // sibling video URL in the same batch (same directory, same stem — case-
    // sensitive, OS-agnostic). Keep the video so add_file runs its video
    // branch, invokes stab.load_gyro_data, and triggers telemetry-parser to
    // extract creation_date_utc — which batch-gyro-match needs for timestamp
    // alignment. Lone .gyroflow files (no matching video in batch) are
    // preserved and go through the project/preset branch as usual.
    //
    // `extensions_json` is the caller's video-extension whitelist (typically
    // `fileDialog.extensions` from QML — single source of truth). "gyroflow"
    // is always treated as the project extension regardless of whether it
    // appears in the list.
    fn filter_paired_gyroflow_siblings(
        &self,
        urls_json: String,
        extensions_json: String,
    ) -> QString {
        let urls: Vec<String> = serde_json::from_str(&urls_json).unwrap_or_default();
        let extensions: Vec<String> = serde_json::from_str(&extensions_json).unwrap_or_default();
        let result = filter_paired_gyroflow_siblings_impl(&urls, &extensions);
        let dropped = urls.len().saturating_sub(result.len());
        if dropped > 0 {
            ::log::info!(
                "[filter_paired_gyroflow_siblings] dropped {} .gyroflow siblings ({} → {} urls)",
                dropped,
                urls.len(),
                result.len()
            );
        }
        QString::from(serde_json::to_string(&result).unwrap_or_else(|_| "[]".to_string()))
    }

    fn first_renderable_video_file(&self, urls_json: String, extensions_json: String) -> QString {
        let urls: Vec<String> = serde_json::from_str(&urls_json).unwrap_or_default();
        let extensions: Vec<String> = serde_json::from_str(&extensions_json).unwrap_or_default();
        QString::from(first_renderable_video_file_impl(&urls, &extensions).unwrap_or_default())
    }

    fn is_gyro_mix_file(&self, url: String) -> bool {
        is_gyro_mix_file_url_impl(&url)
    }

    fn has_supported_drop_item(&self, urls_json: String, extensions_json: String) -> bool {
        let urls: Vec<String> = serde_json::from_str(&urls_json).unwrap_or_default();
        let extensions: Vec<String> = serde_json::from_str(&extensions_json).unwrap_or_default();
        has_supported_drop_item_impl(&urls, &extensions)
    }

    fn filter_supported_drop_items(&self, urls_json: String, extensions_json: String) -> QString {
        let urls: Vec<String> = serde_json::from_str(&urls_json).unwrap_or_default();
        let extensions: Vec<String> = serde_json::from_str(&extensions_json).unwrap_or_default();
        let result = filter_supported_drop_items_impl(&urls, &extensions);
        QString::from(serde_json::to_string(&result).unwrap_or_else(|_| "[]".to_string()))
    }

    fn first_file_requiring_external_sdk(&self, urls_json: String) -> QString {
        let urls: Vec<String> = serde_json::from_str(&urls_json).unwrap_or_default();
        let result = first_url_requiring_external_sdk_impl(&urls, |filename| {
            crate::external_sdk::requires_install(filename)
        });
        QString::from(result.unwrap_or_default())
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
        self.same_gyro_cache.clear();
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

    fn batch_motion_ready(&self) -> bool {
        let Ok(queue) = self.queue.try_borrow() else {
            return false;
        };
        let mut has_renderable_job = false;
        for item in queue.iter() {
            if matches!(item.status, JobStatus::Error | JobStatus::Skipped) {
                continue;
            }
            let Some(job) = self.jobs.get(&item.job_id) else {
                return false;
            };
            if item.status == JobStatus::Finished {
                if job.last_finished_export_project == Some(2) {
                    has_renderable_job = true;
                    if !job
                        .project_data
                        .as_ref()
                        .map(|data| StabilizationManager::project_has_motion_data(data.as_bytes()))
                        .unwrap_or(false)
                    {
                        return false;
                    }
                }
                continue;
            }
            if item.status != JobStatus::Queued || item.total_frames == 0 {
                continue;
            }
            has_renderable_job = true;
            let Some(stab) = job.stab.as_ref() else {
                return false;
            };
            let gyro = stab.gyro.read();
            let file_metadata = gyro.file_metadata.read();
            if gyro.raw_imu(&file_metadata).is_empty()
                && gyro.quaternions.is_empty()
                && file_metadata.quaternions.is_empty()
            {
                return false;
            }
        }
        has_renderable_job
    }

    fn update_gyro_file_parse_result(
        &mut self,
        index: usize,
        id: u64,
        path: &str,
        result: (Option<i64>, Option<f64>, Option<String>, Option<String>),
    ) -> bool {
        let Some(info) = self.gyro_files.get_mut(index) else {
            return false;
        };
        if info.id != id || info.path != path {
            ::log::debug!(
                "[add_gyro_file] ignored stale parse result: index={}, path={}",
                index,
                path
            );
            return false;
        }

        info.created_at_ms = result.0;
        info.duration_ms = result.1;
        info.detected_source = result.2;
        info.error = result.3;
        info.parsed = true;
        true
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
                    let (
                        created_at,
                        duration_ms,
                        playback_duration_ms,
                        playback_fps,
                        frame_count,
                        record_frame_rate,
                    ) = {
                        let params = stab.params.read();
                        let gyro = stab.gyro.read();
                        let md = gyro.file_metadata.read();
                        (
                            params.video_created_at,
                            video_match_duration_ms(&params, &md),
                            params.duration_ms,
                            params.fps,
                            params.frame_count,
                            md.record_frame_rate,
                        )
                    };
                    ::log::info!(
                        "[batch_match T20] video[{}] job_id={}, created_at={:?}, playback_duration={:.1}ms, match_duration={:.1}ms, playback_fps={:.3}, record_fps={:?}, frames={}, file={}",
                        vi,
                        job_id,
                        created_at,
                        playback_duration_ms,
                        duration_ms,
                        playback_fps,
                        record_frame_rate,
                        frame_count,
                        filesystem::get_filename(&stab.input_file.read().url)
                    );
                    videos.push(core::gyro_match::VideoMatchInfo {
                        path: stab.input_file.read().url.clone(),
                        duration_ms,
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
                    "[batch_match]   video[{}] -> gyro[{}] {:?} range=[{:.0?}..{:.0?}] init_offset={:.0?}ms",
                    r.video_index,
                    r.gyro_index.unwrap(),
                    r.status,
                    r.gyro_start_ms,
                    r.gyro_end_ms,
                    r.init_offset_ms
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
                            // Preserve sync_settings across lens profile replacement
                            let saved_sync_settings = stab.lens.read().sync_settings.clone();
                            let manual_edit =
                                core::settings::get_bool("lens_group_manual_edit", false);
                            *stab.lens_group_config.write() = effective_configs.clone();
                            stab.lens_group_manual_edit.store(manual_edit, SeqCst);

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
                                    let cfg_for_build =
                                        niyien_lens_presets::effective_lens_group_config_for_build(
                                            manual_edit,
                                            group_config,
                                            &base_metadata,
                                        );
                                    let existing_lens = stab.lens.read().clone();
                                    let profile = niyien_lens_presets::build_lens_profile(
                                        &base_metadata,
                                        size,
                                        cfg_for_build.as_ref(),
                                        Some(&existing_lens),
                                    );
                                    if let Some(profile) = profile {
                                        if let Some(output_dim) = profile.output_dimension.clone() {
                                            updated_render_options.output_width = output_dim.w;
                                            updated_render_options.output_height = output_dim.h;
                                        }
                                        *stab.lens.write() = profile;
                                    }

                                    // Mirror apply_lens_group_to_main: correction override only
                                    // applies when manual override is effectively applied AND
                                    // anamorphic is on. Otherwise revert to 100% so queue renders
                                    // match the live preview.
                                    let applies_anamorphic = cfg_for_build
                                        .as_ref()
                                        .map(|cfg| cfg.anamorphic_enabled)
                                        .unwrap_or(false);
                                    let correction_percent =
                                        niyien_lens_presets::effective_lens_correction_amount_percent(
                                            group_config,
                                            applies_anamorphic,
                                        );
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
        let queue_auto_rotate = filter_job_ids.is_none() && self.auto_rotate;

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
            // Sync search center derived from front_comp (= -front_comp). Per-clip,
            // grows with drift distance from the calibration video.
            init_offset_ms: Option<f64>,
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
            let raw_requested_range = result.gyro_start_ms.zip(result.gyro_end_ms);
            let requested_range = normalize_time_range_ms(raw_requested_range);
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
            ::log::info!(
                "[batch_match_diag] apply_item job_id={} video='{}' gyro_files_idx={} gyro_file='{}' status={:?} global_offset_ms={:?} init_offset_ms={:?} raw_range_ms={:?} normalized_range_ms={:?} auto_rotate={} original_rotation={:.1} base_output={:?}",
                job_id,
                render_options.input_filename,
                gyro_files_idx,
                filesystem::get_filename(&gyro_path),
                result.status,
                result.global_offset_ms,
                result.init_offset_ms,
                raw_requested_range,
                requested_range,
                auto_rotate,
                original_video_rotation,
                base_render_output_size
            );

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
                init_offset_ms: result.init_offset_ms,
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
                    // Patched additional_data carrying per-clip synchronization
                    // (initial_offset, search_size, calc_initial_fast=false). Written
                    // back to job.additional_data so a later export_gyroflow_file
                    // (which reads job.additional_data) sees the per-clip values
                    // instead of the stale UI-global synchronization block.
                    String,
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
                    .map(|(job_id, _, _, _, _, _, _)| *job_id)
                    .collect();
                let t_project = std::time::Instant::now();
                for (
                    job_id,
                    project_data,
                    render_options,
                    base_lens_metadata,
                    base_output_size,
                    lens_group_index,
                    additional_data,
                ) in job_updates
                {
                    let mut export_settings = None;
                    if let Some(job) = this.jobs.get_mut(&job_id) {
                        if let Some(data) = project_data {
                            job.project_data = Some(data);
                        }
                        job.render_options = render_options;
                        job.additional_data = additional_data;
                        job.base_render_output_size = Some(base_output_size);
                        job.lens_group_index = lens_group_index;
                        if let Some(base_lens_metadata) = base_lens_metadata {
                            job.base_lens_metadata = Some(base_lens_metadata);
                        }
                        if let Some(ref stab) = job.stab {
                            export_settings =
                                Some(job.render_options.settings_string(stab.params.read().get_scaled_fps()));
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
                let existing_ranges: Vec<_> =
                    existing_entries.iter().map(|entry| entry.range_ms).collect();
                ::log::info!(
                    "[batch_match_diag] parse_plan gyro_files_idx={} gyro_file='{}' requested_ranges={:?} existing_cache_ranges={:?} parse_requests={:?}",
                    gyro_files_idx,
                    filesystem::get_filename(&parse_info.path),
                    parse_info.requested_ranges,
                    existing_ranges,
                    parse_requests
                );
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
                    ::log::info!(
                        "[batch_match_diag] auto_rotate_slice job_id={} video='{}' gyro_files_idx={} requested_range={:?} cache_range={:?} adjusted_range={:?} cache_bounds={:?} cloned_bounds={:?} init_offset_ms={:?}",
                        item.job_id,
                        item.render_options.input_filename,
                        item.gyro_files_idx,
                        requested_range,
                        cache_entry.range_ms,
                        adjusted_range_ms,
                        metadata_time_bounds_ms(cache_entry.metadata.as_ref()),
                        metadata_time_bounds_ms(&md),
                        item.init_offset_ms
                    );
                    let detected_source = md.detected_source.as_deref().unwrap_or("");
                    let is_r3d = item
                        .render_options
                        .input_filename
                        .to_ascii_lowercase()
                        .ends_with(".r3d");
                    let has_metadata_rotation =
                        item.original_video_rotation.round() as i32 != 0 && !is_r3d;
                    if !is_r3d
                        && should_apply_auto_rotate(
                            has_metadata_rotation,
                            item.auto_rotate,
                            queue_auto_rotate,
                            detected_source,
                        )
                    {
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
                // C3: Komodo main video keeps its own internal gyro + camera identity.
                // We still run the niyien lens flow (index detection, focal length,
                // lens profile) but skip the IMU/quaternion + camera_id overwrites —
                // those would replace Komodo's trusted state with matched external
                // data. Auto-sync is gated separately in do_autosync.
                let main_is_komodo = item.stab.gyro.read().file_metadata.read().is_komodo;
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
                        let auto_rotate_detected_source =
                            md.detected_source.as_deref().unwrap_or("").to_string();
                        let imu_count = md.raw_imu.len();
                        let quat_count = md.quaternions.len();
                        ::log::info!(
                            "[batch_match_diag] apply_slice job_id={} worker_idx={} video='{}' gyro_files_idx={} gyro_file='{}' requested_range={:?} cache_range={:?} adjusted_range={:?} cache_bounds={:?} cloned_bounds={:?} imu_count={} quat_count={} init_offset_ms={:?}",
                            item.job_id,
                            idx,
                            item.render_options.input_filename,
                            item.gyro_files_idx,
                            filesystem::get_filename(&item.gyro_path),
                            requested_range,
                            cache_entry.range_ms,
                            adjusted_range_ms,
                            metadata_time_bounds_ms(cache_entry.metadata.as_ref()),
                            metadata_time_bounds_ms(&md),
                            imu_count,
                            quat_count,
                            item.init_offset_ms
                        );
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
                        let manual_edit =
                            core::settings::get_bool("lens_group_manual_edit", false);
                        *item.stab.lens_group_config.write() =
                            item.effective_lens_group_configs.clone();
                        item.stab.lens_group_manual_edit.store(manual_edit, SeqCst);
                        let cfg_for_build = group_config
                            .as_ref()
                            .and_then(|cfg| {
                                niyien_lens_presets::effective_lens_group_config_for_build(
                                    manual_edit,
                                    cfg,
                                    &md,
                                )
                            });
                        item.stab
                            .apply_main_video_telemetry(&mut md, &item.gyro_path, true);
                        let camera_id = md.camera_identifier.clone();
                        let lens_profile_metadata = lens_profile_metadata_for_group_build(&md);

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
                        let should_apply_auto_rotation = !is_r3d
                            && should_apply_auto_rotate(
                                has_metadata_rotation,
                                item.auto_rotate,
                                queue_auto_rotate,
                                &auto_rotate_detected_source,
                            );
                        let auto_rotation = if should_apply_auto_rotation {
                            auto_rotation_results
                                .get(&item.job_id)
                                .copied()
                                .flatten()
                        } else {
                            None
                        };

                        if main_is_komodo {
                            // Komodo: keep video gyro (raw_imu/quaternions) + camera_id,
                            // but merge .bin's lens-related metadata into stab so the
                            // niyien lens flow (metadata_snapshot_for_job →
                            // extract_video_focus_length_mm / extract_lens_index +
                            // queue display fallback) sees the right info, parallel
                            // to non-Komodo's load_from_telemetry overwrite.
                            // additional_data uses merge_json (asymmetric — .bin keys
                            // layered on top of RED's recording_settings/image_stabilizer),
                            // not unconditional overwrite, to preserve RED-recorded fields.
                            // frame_readout_time is merged because RED telemetry doesn't
                            // supply it; .bin's value (or downstream camera_db lookup) is
                            // the only source.
                            {
                                let gyro = item.stab.gyro.read();
                                let mut fm = gyro.file_metadata.write();
                                core::util::merge_json(
                                    &mut fm.additional_data,
                                    &md.additional_data,
                                );
                                if fm.lens_params.is_empty() {
                                    fm.lens_params = md.lens_params.clone();
                                }
                                if fm.lens_positions.is_empty() {
                                    fm.lens_positions = md.lens_positions.clone();
                                }
                                if fm.lens_profile.is_none() {
                                    fm.lens_profile = md.lens_profile.clone();
                                }
                                if fm.unit_pixel_focal_length.is_none() {
                                    fm.unit_pixel_focal_length = md.unit_pixel_focal_length;
                                }
                                if fm.frame_readout_time.is_none() {
                                    fm.frame_readout_time = md.frame_readout_time;
                                }
                            }
                            ::log::info!(
                                "[red_arbitration] job[{}] Komodo: kept video gyro + camera_id, merged .bin lens metadata",
                                idx
                            );
                        } else {
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
                        }
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

                        if let Some(rotation) = auto_rotation {
                            ::log::info!(
                                "[auto_rotate compare] file='{}' detected_source='{}' metadata_raw={} metadata_normalized={} auto_rotate_result={} matches_normalized={}",
                                item.render_options.input_filename,
                                auto_rotate_detected_source,
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
                        } else if should_apply_auto_rotation {
                            ::log::info!(
                                "[auto_rotate compare] file='{}' detected_source='{}' metadata_raw={} metadata_normalized={} auto_rotate_result=None matches_normalized=false",
                                item.render_options.input_filename,
                                auto_rotate_detected_source,
                                metadata_raw_rotation,
                                item.original_video_rotation
                            );
                        }

                        item.base_render_output_size = (
                            item.render_options.output_width,
                            item.render_options.output_height,
                        );

                        if let Some(lens_index) = lens_index {
                            let applies_anamorphic = cfg_for_build
                                .as_ref()
                                .map(|cfg| cfg.anamorphic_enabled)
                                .unwrap_or(false);
                            let custom_lens_profile = group_config.as_ref().and_then(|_| {
                                let existing_lens = item.stab.lens.read().clone();
                                niyien_lens_presets::build_lens_profile(
                                    &lens_profile_metadata,
                                    size,
                                    cfg_for_build.as_ref(),
                                    Some(&existing_lens),
                                )
                            });
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
                                let correction_percent = group_config
                                    .as_ref()
                                    .map(|cfg| {
                                        niyien_lens_presets::effective_lens_correction_amount_percent(
                                            cfg,
                                            applies_anamorphic,
                                        )
                                    })
                                    .unwrap_or(100.0);
                                item.stab
                                    .set_lens_correction_amount(correction_percent / 100.0);
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
                                item.stab.set_lens_correction_amount(1.0);
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
                            item.stab.set_lens_correction_amount(1.0);
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

                // sync_settings stores seconds; SyncParams parser at
                // render_queue.rs:3015 multiplies by 1000 to ms. The init_offset/
                // search_size pair comes from batch_match_sync_overrides so this
                // write site and the additional_data patch site (par_iter#3 below)
                // stay in sync.
                let (init_offset_s, search_size_s) =
                    batch_match_sync_overrides(item.init_offset_ms);
                ::log::info!(
                    "[batch_match_diag] sync_override job_id={} video='{}' gyro_file='{}' raw_range_ms={:?} normalized_range_ms={:?} init_offset_ms={:?} initial_offset_s={:.3} search_size_s={:.3} duration_s={:.3} fps={:.3} max_sync_points={} every_nth_frame={}",
                    item.job_id,
                    item.render_options.input_filename,
                    filesystem::get_filename(&item.gyro_path),
                    item.gyro_start_ms.zip(item.gyro_end_ms),
                    normalize_time_range_ms(item.gyro_start_ms.zip(item.gyro_end_ms)),
                    item.init_offset_ms,
                    init_offset_s,
                    search_size_s,
                    duration_s,
                    fps,
                    max_sync_points,
                    every_nth_frame
                );

                let mut lens = item.stab.lens.write();
                lens.sync_settings = Some(serde_json::json!({
                    "do_autosync": true,
                    "max_sync_points": max_sync_points,
                    "search_size": search_size_s,
                    "time_per_syncpoint": 2.5,
                    "every_nth_frame": every_nth_frame,
                    "initial_offset": init_offset_s,
                    // Disable essential_matrix pre-computation so it doesn't
                    // overwrite our per-clip initial_offset and force search_size=3000ms.
                    "calc_initial_fast": false,
                    "pose_method": 0,
                    "of_method": default_of_method,
                    "offset_method": 2
                }));
                drop(lens);
                ::log::info!(
                    "[batch_match] job={} init_offset_ms={:.1} search_size_ms={:.0}",
                    item.job_id,
                    init_offset_s * 1000.0,
                    search_size_s * 1000.0
                );
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
                String,
            )> =
                apply_items
                    .into_par_iter()
                    .map(|mut item| {
                    item.stab.gyro.write().file_url = item.gyro_path.clone();

                    // Patch additional_data["synchronization"] so the exported
                    // .gyroflow file's top-level synchronization block matches
                    // the per-clip values we wrote to lens.sync_settings. Without
                    // this, export_gyroflow_data's merge_json would overlay the
                    // UI-global synchronization (e.g. initial_offset=-1) on top
                    // of our per-clip data, and reloading the project would also
                    // overwrite lens.sync_settings via update_sync_settings.
                    let (init_offset_s, search_size_s) =
                        batch_match_sync_overrides(item.init_offset_ms);
                    if let Ok(serde_json::Value::Object(mut ad_obj)) =
                        serde_json::from_str::<serde_json::Value>(&item.additional_data)
                    {
                        let sync_entry = ad_obj
                            .entry("synchronization".to_string())
                            .or_insert_with(|| serde_json::json!({}));
                        if let Some(sync_obj) = sync_entry.as_object_mut() {
                            sync_obj.insert(
                                "initial_offset".into(),
                                serde_json::json!(init_offset_s),
                            );
                            sync_obj.insert(
                                "search_size".into(),
                                serde_json::json!(search_size_s),
                            );
                            sync_obj.insert(
                                "calc_initial_fast".into(),
                                serde_json::json!(false),
                            );
                        }
                        if let Ok(s) = serde_json::to_string(&serde_json::Value::Object(ad_obj))
                        {
                            item.additional_data = s;
                        }
                    }

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
                                item.additional_data,
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
                                item.additional_data,
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
                    .filter(|(_, project_data, _, _, _, _, _)| project_data.is_some())
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

    fn get_assigned_gyro_job_ids_json(&self) -> QString {
        let job_ids = self.get_ordered_job_ids();
        let queue_ids: HashSet<u32> = job_ids.iter().copied().collect();
        let mut assigned = Vec::new();

        if let Some(ref results) = self.match_results {
            for result in &results.results {
                if result.gyro_index.is_none()
                    || !matches!(result.status, core::gyro_match::MatchStatus::Matched)
                {
                    continue;
                }

                let job_id = result
                    .job_id
                    .or_else(|| job_ids.get(result.video_index).copied());
                if let Some(job_id) = job_id {
                    if queue_ids.contains(&job_id) && !assigned.contains(&job_id) {
                        assigned.push(job_id);
                    }
                }
            }
        }

        QString::from(serde_json::to_string(&assigned).unwrap_or_else(|_| "[]".to_owned()))
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

// Per-clip sync overrides derived from the batch match result. Returned in
// seconds (sync_settings unit). Single source of truth so the lens.sync_settings
// write site and the additional_data["synchronization"] patch site can't drift.
//
// initial_offset sign: positive = gyro late vs video. We pre-loaded front_comp
// ms before the video start (gyro is "earlier") so we negate.
//
// search_size: floored at 5s (sync default); grows when |init_offset| pushes the
// search center far from 0, so the window still covers the true peak ± slack.
fn batch_match_sync_overrides(init_offset_ms: Option<f64>) -> (f64, f64) {
    let init_offset_s = init_offset_ms.unwrap_or(0.0) / 1000.0;
    let search_size_s = 5.0_f64.max(init_offset_s.abs() * 1.5);
    (init_offset_s, search_size_s)
}

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

fn video_match_duration_ms(
    params: &core::stabilization_params::StabilizationParams,
    md: &core::gyro_source::FileMetadata,
) -> f64 {
    let fallback_duration_ms = params.get_scaled_duration_ms();

    if let Some(record_fps) = md.record_frame_rate {
        if record_fps.is_finite() && record_fps > 0.0 {
            let duration_ms = if params.frame_count > 0 {
                Some(params.frame_count as f64 * 1000.0 / record_fps)
            } else if params.duration_ms.is_finite()
                && params.duration_ms > 0.0
                && params.fps.is_finite()
                && params.fps > 0.0
            {
                Some(params.duration_ms * params.fps / record_fps)
            } else {
                None
            };

            if let Some(duration_ms) = duration_ms {
                if duration_ms.is_finite() && duration_ms > 0.0 {
                    return duration_ms;
                }
            }
        }
    }

    fallback_duration_ms
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

fn remove_do_autosync_from_project_json(data: &mut String) -> bool {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(data) else {
        return false;
    };

    let changed = remove_do_autosync_from_project_value(&mut value);
    if changed {
        if let Ok(updated) = serde_json::to_string(&value) {
            *data = updated;
        }
    }
    changed
}

fn remove_do_autosync_from_project_value(value: &mut serde_json::Value) -> bool {
    let mut changed = false;
    if let Some(sync) = value.get_mut("synchronization") {
        changed |= remove_do_autosync_from_sync_value(sync);
    }
    if let Some(sync) = value
        .get_mut("calibration_data")
        .and_then(|v| v.get_mut("sync_settings"))
    {
        changed |= remove_do_autosync_from_sync_value(sync);
    }
    changed
}

fn remove_do_autosync_from_stab(stab: &StabilizationManager) -> bool {
    let mut lens = stab.lens.write();
    lens.sync_settings
        .as_mut()
        .map(remove_do_autosync_from_sync_value)
        .unwrap_or_default()
}

fn remove_do_autosync_from_sync_value(value: &mut serde_json::Value) -> bool {
    value
        .as_object_mut()
        .and_then(|obj| obj.remove("do_autosync"))
        .is_some()
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

fn metadata_time_bounds_ms(md: &core::gyro_source::FileMetadata) -> Option<(f64, f64)> {
    md.raw_imu
        .first()
        .zip(md.raw_imu.last())
        .map(|(first, last)| (first.timestamp_ms, last.timestamp_ms))
        .or_else(|| {
            let first = *md.quaternions.keys().next()? as f64 / 1000.0;
            let last = *md.quaternions.keys().next_back()? as f64 / 1000.0;
            Some((first, last))
        })
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

// Rules:
//   - Group URLs by everything before the final '.' (key is case-sensitive
//     to stay correct on case-sensitive filesystems).
//   - Within a group, if a .gyroflow project *and* a video (extension ∈
//     `extensions` after lowercasing, minus "gyroflow") both exist, drop the
//     .gyroflow.
//   - Also drop .gyroflow projects whose `videofile` points to a video URL
//     already present in the batch.
//   - Lone .gyroflow (no sibling video) is preserved.
//   - URLs with no extension, or whose dot sits inside the directory part,
//     are passed through untouched.
//   - Output preserves the original order.
fn filter_paired_gyroflow_siblings_impl(urls: &[String], extensions: &[String]) -> Vec<String> {
    filter_paired_gyroflow_siblings_impl_with_project_reader(
        urls,
        extensions,
        read_gyroflow_project_video_url,
    )
}

fn filter_paired_gyroflow_siblings_impl_with_project_reader<F>(
    urls: &[String],
    extensions: &[String],
    mut project_video_url: F,
) -> Vec<String>
where
    F: FnMut(&str) -> Option<String>,
{
    if urls.len() <= 1 {
        return urls.to_vec();
    }
    let video_exts: HashSet<String> = extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| e != "gyroflow")
        .collect();

    let mut groups: HashMap<String, (Option<String>, Option<String>)> = HashMap::new();
    let mut video_url_keys: HashSet<String> = HashSet::new();
    let mut gyroflow_urls: Vec<String> = Vec::new();
    for u in urls {
        let Some(dot) = u.rfind('.') else { continue };
        let slash_idx = u.rfind(['/', '\\']).unwrap_or(0);
        if dot <= slash_idx {
            continue;
        }
        let ext_lower = u[dot + 1..].to_ascii_lowercase();
        let key = u[..dot].to_string();
        let entry = groups.entry(key).or_insert((None, None));
        if ext_lower == "gyroflow" {
            entry.1 = Some(u.clone());
            gyroflow_urls.push(u.clone());
        } else if video_exts.contains(&ext_lower) {
            entry.0 = Some(u.clone());
            video_url_keys.insert(comparable_video_url_key(u));
        }
    }

    if video_url_keys.is_empty() {
        return urls.to_vec();
    }

    let mut same_stem_gyroflows: HashSet<String> = HashSet::new();
    for (_, (video, gyroflow)) in &groups {
        if let (Some(_), Some(g)) = (video, gyroflow) {
            same_stem_gyroflows.insert(g.clone());
        }
    }
    let mut drop_set: HashSet<String> = HashSet::new();
    for gyroflow_url in gyroflow_urls {
        if let Some(video_url) = project_video_url(&gyroflow_url) {
            let video_key = comparable_video_url_key(&video_url);
            if same_stem_gyroflows.contains(&gyroflow_url) || video_url_keys.contains(&video_key) {
                drop_set.insert(gyroflow_url);
            }
        }
    }
    urls.iter()
        .filter(|u| !drop_set.contains(*u))
        .cloned()
        .collect()
}

#[derive(serde::Deserialize)]
struct GyroflowProjectVideoRef {
    videofile: Option<String>,
    image_sequence_start: Option<i64>,
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    videofile_bookmark: Option<String>,
}

fn read_gyroflow_project_video_url(url: &str) -> Option<String> {
    if file_extension(url)? != "gyroflow" {
        return None;
    }
    let data = filesystem::read(url).ok()?;
    let project: GyroflowProjectVideoRef = serde_json::from_slice(&data).ok()?;
    let mut video_url = project.videofile.filter(|x| !x.is_empty())?;
    if !video_url.contains("://") {
        video_url = filesystem::path_to_url(&video_url);
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    if let Some(v) = project
        .videofile_bookmark
        .as_deref()
        .filter(|x| !x.is_empty())
    {
        let (resolved, _is_stale) = filesystem::apple::resolve_bookmark(v, Some(url));
        if !resolved.is_empty() {
            video_url = resolved;
        }
    }
    let sequence_start = project.image_sequence_start.unwrap_or_default() as u32;
    Some(StabilizationManager::get_new_videofile_url(
        &video_url,
        Some(url),
        sequence_start,
    ))
}

fn comparable_video_url_key(url: &str) -> String {
    let url = if url.contains("://") {
        url.replace(' ', "%20")
    } else {
        filesystem::path_to_url(url)
    };
    if url.to_ascii_lowercase().starts_with("file://") {
        let path = filesystem::url_to_path(&url);
        if !path.is_empty() && path != url {
            let path = path.replace('\\', "/");
            if cfg!(windows) {
                return path.to_ascii_lowercase();
            }
            return path;
        }
    }
    url
}

fn file_extension(url: &str) -> Option<String> {
    let dot = url.rfind('.')?;
    let slash_idx = url.rfind(['/', '\\']).unwrap_or(0);
    if dot <= slash_idx {
        return None;
    }
    Some(url[dot + 1..].to_ascii_lowercase())
}

fn first_renderable_video_file_impl(urls: &[String], extensions: &[String]) -> Option<String> {
    let video_exts: std::collections::HashSet<String> = extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| e != "gyroflow")
        .collect();

    urls.iter().find_map(|url| {
        file_extension(url)
            .filter(|ext| video_exts.contains(ext))
            .map(|_| url.clone())
    })
}

fn is_gyro_mix_file_url_impl(url: &str) -> bool {
    filesystem::get_filename(url)
        .to_ascii_lowercase()
        .ends_with("_mix.bin")
}

fn is_supported_drop_item_impl(url: &str, accepted_exts: &HashSet<String>) -> bool {
    if is_gyro_mix_file_url_impl(url) {
        return true;
    }
    match file_extension(url) {
        Some(ext) => ext == "rdc" || accepted_exts.contains(&ext),
        None => true,
    }
}

fn accepted_drop_extensions(extensions: &[String]) -> HashSet<String> {
    extensions
        .iter()
        .map(|e| e.trim_start_matches('.').to_ascii_lowercase())
        .collect()
}

fn filter_supported_drop_items_impl(urls: &[String], extensions: &[String]) -> Vec<String> {
    let accepted_exts = accepted_drop_extensions(extensions);
    urls.iter()
        .filter(|url| is_supported_drop_item_impl(url, &accepted_exts))
        .cloned()
        .collect()
}

fn has_supported_drop_item_impl(urls: &[String], extensions: &[String]) -> bool {
    let accepted_exts = accepted_drop_extensions(extensions);

    urls.iter()
        .any(|url| is_supported_drop_item_impl(url, &accepted_exts))
}

fn first_url_requiring_external_sdk_impl<F>(
    urls: &[String],
    mut requires_install: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    urls.iter().find_map(|url| {
        let filename = filesystem::get_filename(url);
        requires_install(&filename).then(|| url.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_with_lens_display_job(
        manual_edit: bool,
        config: niyien_lens_presets::LensGroupConfig,
        metadata: core::gyro_source::FileMetadata,
    ) -> RenderQueue {
        let stabilizer = Arc::new(StabilizationManager::default());
        stabilizer
            .lens_group_manual_edit
            .store(manual_edit, SeqCst);
        let mut configs = niyien_lens_presets::default_lens_group_configs();
        let lens_index = config.lens_index;
        configs[lens_index] = config;
        *stabilizer.lens_group_config.write() = configs;

        let base_lens_metadata = JobLensMetadataBackup::from_metadata(&metadata);
        let job_stab = Arc::new(StabilizationManager::default());
        {
            let mut gyro = job_stab.gyro.write();
            gyro.file_metadata = metadata.into();
        }

        let mut queue = RenderQueue::new(stabilizer);
        queue.jobs.insert(
            1,
            Job {
                queue_index: 0,
                render_options: RenderOptions::default(),
                base_render_output_size: None,
                original_output_size: (0, 0),
                auto_rotate: false,
                additional_data: String::new(),
                cancel_flag: Default::default(),
                render_epoch: Default::default(),
                project_data: None,
                last_finished_export_project: None,
                stab: Some(job_stab),
                base_lens_metadata: Some(base_lens_metadata),
                lens_group_config_override: None,
                lens_group_index: Some(0),
                video_created_at: None,
                original_video_rotation: 0.0,
            },
        );
        queue
    }

    fn job_display_params(queue: &RenderQueue) -> serde_json::Value {
        serde_json::from_str(&queue.get_job_display_params(1).to_string()).unwrap()
    }

    fn smoothing_param_value(data: &serde_json::Value, name: &str) -> Option<f64> {
        data.get("stabilization")
            .and_then(|stab| stab.get("smoothing_params"))
            .and_then(|params| params.as_array())
            .and_then(|params| {
                params.iter().find(|param| {
                    param.get("name").and_then(|n| n.as_str()) == Some(name)
                })
            })
            .and_then(|param| param.get("value").and_then(|v| v.as_f64()))
    }

    fn auto_focal_metadata() -> core::gyro_source::FileMetadata {
        core::gyro_source::FileMetadata {
            additional_data: serde_json::json!({ "lens_index": 0 }),
            lens_params: BTreeMap::from([(
                0,
                core::gyro_source::LensParams {
                    focal_length: Some(31.0),
                    pixel_focal_length: Some(3100.0),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        }
    }

    fn queue_with_eta_job(status: JobStatus) -> RenderQueue {
        let stab = Arc::new(StabilizationManager::default());
        {
            let mut params = stab.params.write();
            params.frame_count = 100;
            params.duration_ms = 10_000.0;
            params.fps = 10.0;
        }
        stab.input_file.write().url = "file:///eta-test.mp4".to_owned();
        stab.lens.write().sync_settings = Some(serde_json::json!({
            "do_autosync": true,
            "max_sync_points": 2,
            "search_size": 5.0,
            "time_per_syncpoint": 1.0,
            "every_nth_frame": 1,
            "initial_offset": 0.0,
            "pose_method": 0,
            "of_method": 2,
            "offset_method": 2
        }));

        let mut queue = RenderQueue::default();
        queue.queue.borrow_mut().push(RenderQueueItem {
            job_id: 1,
            total_frames: 100,
            status,
            ..Default::default()
        });
        queue.jobs.insert(
            1,
            Job {
                queue_index: 0,
                render_options: RenderOptions::default(),
                base_render_output_size: None,
                original_output_size: (0, 0),
                auto_rotate: false,
                additional_data: String::new(),
                cancel_flag: Default::default(),
                render_epoch: Default::default(),
                project_data: None,
                last_finished_export_project: None,
                stab: Some(stab),
                base_lens_metadata: None,
                lens_group_config_override: None,
                lens_group_index: None,
                video_created_at: None,
                original_video_rotation: 0.0,
            },
        );
        queue
    }

    fn add_motion_to_job(queue: &mut RenderQueue, job_id: u32, use_quats: bool) {
        let stab = queue
            .jobs
            .get(&job_id)
            .and_then(|job| job.stab.as_ref())
            .cloned()
            .expect("test job has stab");
        {
            let mut params = stab.params.write();
            params.duration_ms = 1_000.0;
            params.fps = 10.0;
            params.frame_count = 10;
        }
        stab.gyro.write().init_from_params(&stab.params.read());
        let mut metadata = core::gyro_source::FileMetadata {
            duration_ms: 1_000.0,
            ..Default::default()
        };
        if use_quats {
            metadata
                .quaternions
                .insert(0, core::gyro_source::Quat64::identity());
        } else {
            metadata.raw_imu.push(core::gyro_source::TimeIMU {
                timestamp_ms: 0.0,
                gyro: Some([0.0, 0.0, 0.0]),
                accl: None,
                magn: None,
            });
        }
        stab.gyro.write().load_from_telemetry(metadata);
    }

    fn autosync_additional_data() -> String {
        serde_json::json!({
            "synchronization": {
                "do_autosync": true,
                "max_sync_points": 2,
                "search_size": 5.0,
                "time_per_syncpoint": 1.0,
                "every_nth_frame": 1,
                "initial_offset": 0.0,
                "pose_method": 0,
                "of_method": 2,
                "offset_method": 2
            }
        })
        .to_string()
    }

    fn queue_with_autosync_project(
        status: JobStatus,
        with_offsets: bool,
        last_finished_export_project: Option<u32>,
    ) -> RenderQueue {
        let release_stab = status == JobStatus::Finished;
        let mut queue = queue_with_eta_job(status);
        let additional_data = autosync_additional_data();
        add_motion_to_job(&mut queue, 1, false);
        let (stab, render_options) = {
            let job = queue.jobs.get(&1).unwrap();
            (job.stab.as_ref().unwrap().clone(), job.render_options.clone())
        };

        if with_offsets {
            stab.gyro.write().set_offset(1_000_000, 42.0);
        }

        let project_data = RenderQueue::get_gyroflow_data_internal_with_type(
            &stab,
            &additional_data,
            &render_options,
            core::GyroflowProjectType::WithGyroData,
            false,
        )
        .expect("project data export succeeds");

        let job = queue.jobs.get_mut(&1).unwrap();
        job.additional_data = additional_data;
        job.project_data = Some(project_data);
        job.last_finished_export_project = last_finished_export_project;
        if release_stab {
            job.stab = None;
        }

        queue
    }

    fn has_top_level_do_autosync(data: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .and_then(|v| {
                v.get("synchronization")?
                    .get("do_autosync")?
                    .as_bool()
            })
            .unwrap_or_default()
    }

    fn has_calibration_do_autosync(data: &str) -> bool {
        serde_json::from_str::<serde_json::Value>(data)
            .ok()
            .and_then(|v| {
                v.get("calibration_data")?
                    .get("sync_settings")?
                    .get("do_autosync")?
                    .as_bool()
            })
            .unwrap_or_default()
    }

    fn job_lens_has_do_autosync(job: &Job) -> bool {
        job.stab
            .as_ref()
            .and_then(|stab| {
                stab.lens
                    .read()
                    .sync_settings
                    .clone()
                    .and_then(|v| v.get("do_autosync").and_then(|x| x.as_bool()))
            })
            .unwrap_or_default()
    }

    #[test]
    fn finished_sync_snapshot_bypasses_project_file_reference() {
        let stab = StabilizationManager::default();
        let dir = tempfile::tempdir().unwrap();
        let project_path = dir.path().join("existing.gyroflow");
        std::fs::write(&project_path, "{}").unwrap();
        let project_url = filesystem::path_to_url(&project_path.to_string_lossy());
        stab.input_file.write().project_file_url = Some(project_url.clone());

        let shortcut = RenderQueue::get_gyroflow_data_internal_with_type(
            &stab,
            "{}",
            &RenderOptions::default(),
            core::GyroflowProjectType::Simple,
            true,
        )
        .expect("project file reference succeeds");
        let shortcut: serde_json::Value = serde_json::from_str(&shortcut).unwrap();
        assert_eq!(shortcut["project_file"].as_str(), Some(project_url.as_str()));

        let snapshot = RenderQueue::get_gyroflow_data_internal_with_type(
            &stab,
            &autosync_additional_data(),
            &RenderOptions::default(),
            core::GyroflowProjectType::WithGyroData,
            false,
        )
        .expect("inline project snapshot succeeds");
        let snapshot: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
        assert!(snapshot.get("project_file").is_none());
        assert_eq!(snapshot["title"].as_str(), Some("Gyroflow data file"));
        assert_eq!(
            snapshot["synchronization"]["do_autosync"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn prepare_finished_jobs_for_video_export_requeues_synced_jobs_without_force_autosync() {
        let mut queue = queue_with_autosync_project(JobStatus::Finished, true, Some(2));
        let project_data = queue.jobs.get(&1).unwrap().project_data.as_ref().unwrap();
        assert!(has_top_level_do_autosync(project_data));
        assert!(has_calibration_do_autosync(project_data));
        assert!(queue.jobs.get(&1).unwrap().stab.is_none());

        queue.prepare_finished_jobs_for_video_export();

        assert_eq!(queue.queue.borrow()[0].get_status(), &JobStatus::Queued);
        let job = queue.jobs.get(&1).unwrap();
        let project_data = job.project_data.as_ref().unwrap();
        assert!(job.stab.is_some());
        assert!(!has_top_level_do_autosync(project_data));
        assert!(!has_calibration_do_autosync(project_data));
        assert!(!has_top_level_do_autosync(&job.additional_data));
        assert!(!job_lens_has_do_autosync(job));
        assert!(!job
            .stab
            .as_ref()
            .unwrap()
            .gyro
            .read()
            .get_offsets()
            .is_empty());
        assert_eq!(job.last_finished_export_project, None);
        assert_eq!(RenderQueue::estimated_sync_frames_for_job(job), 0);
    }

    #[test]
    fn prepare_finished_jobs_for_video_export_leaves_error_and_skipped_jobs_unchanged() {
        for status in [JobStatus::Error, JobStatus::Skipped] {
            let mut queue = queue_with_autosync_project(status.clone(), true, Some(2));

            queue.prepare_finished_jobs_for_video_export();

            assert_eq!(queue.queue.borrow()[0].get_status(), &status);
            let job = queue.jobs.get(&1).unwrap();
            let project_data = job.project_data.as_ref().unwrap();
            assert!(has_top_level_do_autosync(project_data));
            assert!(has_calibration_do_autosync(project_data));
            assert!(has_top_level_do_autosync(&job.additional_data));
            assert!(job_lens_has_do_autosync(job));
        }
    }

    #[test]
    fn prepare_finished_jobs_for_video_export_leaves_finished_video_exports_unchanged() {
        let mut queue = queue_with_autosync_project(JobStatus::Finished, true, Some(4));

        queue.prepare_finished_jobs_for_video_export();

        assert_eq!(queue.queue.borrow()[0].get_status(), &JobStatus::Finished);
        assert!(queue.jobs.get(&1).unwrap().stab.is_none());
        let job = queue.jobs.get(&1).unwrap();
        let project_data = job.project_data.as_ref().unwrap();
        assert!(has_top_level_do_autosync(project_data));
        assert!(has_calibration_do_autosync(project_data));
        assert_eq!(job.last_finished_export_project, Some(4));
    }

    #[test]
    fn prepare_finished_jobs_for_video_export_leaves_unknown_finished_jobs_unchanged() {
        let mut queue = queue_with_autosync_project(JobStatus::Finished, true, None);

        queue.prepare_finished_jobs_for_video_export();

        assert_eq!(queue.queue.borrow()[0].get_status(), &JobStatus::Finished);
        assert!(queue.jobs.get(&1).unwrap().stab.is_none());
    }

    #[test]
    fn prepare_finished_jobs_for_video_export_keeps_sync_estimate_when_offsets_are_missing() {
        let mut queue = queue_with_autosync_project(JobStatus::Finished, false, Some(2));

        queue.prepare_finished_jobs_for_video_export();

        let job = queue.jobs.get(&1).unwrap();
        assert!(RenderQueue::estimated_sync_frames_for_job(job) > 0);
    }

    #[test]
    fn batch_update_params_updates_live_stab_and_display_params() {
        let mut queue = queue_with_eta_job(JobStatus::Queued);
        let (stab, render_options) = {
            let job = queue.jobs.get(&1).unwrap();
            (job.stab.as_ref().unwrap().clone(), job.render_options.clone())
        };

        let project_data = RenderQueue::get_gyroflow_data_internal(
            &stab,
            "{}",
            &render_options,
        )
        .expect("project data export succeeds");

        {
            let job = queue.jobs.get_mut(&1).unwrap();
            job.project_data = Some(project_data);
        }

        queue.batch_update_params(
            serde_json::json!([1]).to_string(),
            serde_json::json!({
                "smoothness": 0.8,
                "horizon_lock_amount": 75.0,
                "zoom_mode": "static",
                "lens_correction": 0.25,
                "framerate": 25.0,
            })
            .to_string(),
        );

        let display = serde_json::from_str::<serde_json::Value>(
            &queue.get_job_display_params(1).to_string(),
        )
        .expect("display params parse");
        assert_eq!(display["smoothness"].as_f64(), Some(0.8));
        assert_eq!(display["horizon_lock_amount"].as_f64(), Some(75.0));
        assert_eq!(display["zoom_mode"].as_str(), Some("static"));
        assert_eq!(display["lens_correction"].as_f64(), Some(0.25));
        assert_eq!(display["source_fps"].as_f64(), Some(10.0));
        assert_eq!(display["framerate"].as_f64(), Some(25.0));

        let job = queue.jobs.get(&1).unwrap();
        let stab = job.stab.as_ref().unwrap();
        let live_data = serde_json::from_str::<serde_json::Value>(
            &stab
                .export_gyroflow_data(core::GyroflowProjectType::Simple, "{}", None)
                .expect("live stab export succeeds"),
        )
        .expect("live stab export parses");
        assert_eq!(smoothing_param_value(&live_data, "smoothness"), Some(0.8));
        assert_eq!(
            live_data["stabilization"]["horizon_lock_amount"].as_f64(),
            Some(75.0)
        );
        assert_eq!(
            live_data["stabilization"]["adaptive_zoom_window"].as_f64(),
            Some(-1.0)
        );
        assert_eq!(
            live_data["stabilization"]["lens_correction_amount"].as_f64(),
            Some(0.25)
        );
        assert_eq!(live_data["video_info"]["vfr_fps"].as_f64(), Some(25.0));
    }

    #[test]
    fn queue_eta_model_waits_for_required_video_sample() {
        let model = QueueEtaEstimateModel::default();

        assert_eq!(model.estimate_remaining_ms(0, 100, 1), None);
        assert_eq!(model.estimate_remaining_ms(10, 100, 1), None);
    }

    #[test]
    fn queue_eta_model_estimates_from_completed_video_sample() {
        let mut model = QueueEtaEstimateModel::default();

        model.observe_completed_job(QueueEtaSample {
            sync_frames: 10,
            sync_ms: 1_000.0,
            render_frames: 100,
            render_ms: 10_000.0,
        });

        assert_eq!(model.completed_job_samples, 1);
        assert_eq!(model.estimate_remaining_ms(20, 200, 2), Some(12_000));
    }

    #[test]
    fn queue_eta_model_estimates_from_sync_only_sample() {
        let mut model = QueueEtaEstimateModel::default();

        model.observe_completed_job(QueueEtaSample {
            sync_frames: 10,
            sync_ms: 1_000.0,
            render_frames: 0,
            render_ms: 0.0,
        });

        assert_eq!(model.completed_job_samples, 1);
        assert_eq!(model.estimate_remaining_ms(20, 0, 1), Some(2_000));
    }

    #[test]
    fn queue_eta_model_returns_none_without_remaining_work() {
        let mut model = QueueEtaEstimateModel::default();

        model.observe_completed_job(QueueEtaSample {
            sync_frames: 10,
            sync_ms: 1_000.0,
            render_frames: 0,
            render_ms: 0.0,
        });

        assert_eq!(model.estimate_remaining_ms(0, 0, 1), None);
    }

    #[test]
    fn batch_motion_ready_requires_motion_for_each_renderable_job() {
        let mut queue = RenderQueue::default();

        assert!(!queue.batch_motion_ready());

        queue = queue_with_eta_job(JobStatus::Queued);
        assert!(!queue.batch_motion_ready());

        add_motion_to_job(&mut queue, 1, false);
        assert!(queue.batch_motion_ready());

        queue.queue.borrow_mut().push(RenderQueueItem {
            job_id: 2,
            total_frames: 100,
            status: JobStatus::Queued,
            ..Default::default()
        });
        queue.jobs.insert(
            2,
            Job {
                queue_index: 1,
                render_options: RenderOptions::default(),
                base_render_output_size: None,
                original_output_size: (0, 0),
                auto_rotate: false,
                additional_data: String::new(),
                cancel_flag: Default::default(),
                render_epoch: Default::default(),
                project_data: None,
                last_finished_export_project: None,
                stab: Some(Arc::new(StabilizationManager::default())),
                base_lens_metadata: None,
                lens_group_config_override: None,
                lens_group_index: None,
                video_created_at: None,
                original_video_rotation: 0.0,
            },
        );
        assert!(!queue.batch_motion_ready());

        add_motion_to_job(&mut queue, 2, true);
        assert!(queue.batch_motion_ready());
    }

    #[test]
    fn batch_motion_ready_accepts_finished_sync_only_jobs() {
        let queue = queue_with_autosync_project(JobStatus::Finished, true, Some(2));

        assert!(queue.batch_motion_ready());
    }

    #[test]
    fn batch_motion_ready_skips_finished_video_exports() {
        let mut queue = queue_with_autosync_project(JobStatus::Finished, true, Some(4));

        queue.queue.borrow_mut().push(RenderQueueItem {
            job_id: 2,
            total_frames: 100,
            status: JobStatus::Queued,
            ..Default::default()
        });
        queue.jobs.insert(
            2,
            Job {
                queue_index: 1,
                render_options: RenderOptions::default(),
                base_render_output_size: None,
                original_output_size: (0, 0),
                auto_rotate: false,
                additional_data: String::new(),
                cancel_flag: Default::default(),
                render_epoch: Default::default(),
                project_data: None,
                last_finished_export_project: None,
                stab: Some(Arc::new(StabilizationManager::default())),
                base_lens_metadata: None,
                lens_group_config_override: None,
                lens_group_index: None,
                video_created_at: None,
                original_video_rotation: 0.0,
            },
        );
        assert!(!queue.batch_motion_ready());

        add_motion_to_job(&mut queue, 2, false);
        assert!(queue.batch_motion_ready());
    }

    #[test]
    fn batch_motion_ready_requires_motion_for_finished_sync_only_jobs() {
        let mut queue = queue_with_autosync_project(JobStatus::Finished, true, Some(2));
        queue.jobs.get_mut(&1).unwrap().project_data = Some("{}".to_owned());

        assert!(!queue.batch_motion_ready());
    }

    #[test]
    fn queue_eta_for_sync_only_export_does_not_wait_for_render_sample() {
        let mut queue = queue_with_eta_job(JobStatus::Queued);
        queue.export_project = 2;

        queue.observe_eta_sample_for_epoch(
            1,
            0,
            QueueEtaSample {
                sync_frames: 10,
                sync_ms: 1_000.0,
                render_frames: 0,
                render_ms: 0.0,
            },
        );

        assert_eq!(queue.estimated_remaining_ms(), Some(2_000));
    }

    #[test]
    fn queue_progress_tracks_sync_only_processing_progress() {
        let mut queue = queue_with_eta_job(JobStatus::Rendering);
        queue.export_project = 2;
        {
            let mut q = queue.queue.borrow_mut();
            let mut item = q[0].clone();
            item.processing_progress = 0.5;
            q.change_line(0, item);
        }

        assert!((queue.get_queue_progress() - 0.5).abs() < 0.001);
        assert_eq!(queue.get_queue_done_jobs(), 0);
        assert_eq!(queue.get_queue_total_jobs(), 1);
    }

    #[test]
    fn queue_progress_tracks_processing_when_sync_estimate_is_unknown() {
        let mut queue = queue_with_eta_job(JobStatus::Rendering);
        queue.export_project = 2;
        queue
            .jobs
            .get(&1)
            .unwrap()
            .stab
            .as_ref()
            .unwrap()
            .lens
            .write()
            .sync_settings = Some(serde_json::json!({
                "do_autosync": true,
                "max_sync_points": 0
            }));
        {
            let mut q = queue.queue.borrow_mut();
            let mut item = q[0].clone();
            item.processing_progress = 0.25;
            q.change_line(0, item);
        }

        assert_eq!(RenderQueue::estimated_sync_frames_for_job(queue.jobs.get(&1).unwrap()), 0);
        assert!((queue.get_queue_progress() - 0.25).abs() < 0.001);
    }

    #[test]
    fn queue_progress_weights_autosync_and_render_work_for_video_export() {
        let mut queue = queue_with_eta_job(JobStatus::Rendering);
        queue.export_project = 4;
        {
            let mut q = queue.queue.borrow_mut();
            let mut item = q[0].clone();
            item.processing_progress = 0.5;
            q.change_line(0, item);
        }

        assert!((queue.get_queue_progress() - (10.0 / 120.0)).abs() < 0.001);

        {
            let mut q = queue.queue.borrow_mut();
            let mut item = q[0].clone();
            item.processing_progress = 1.0;
            item.current_frame = 50;
            q.change_line(0, item);
        }

        assert!((queue.get_queue_progress() - (70.0 / 120.0)).abs() < 0.001);
    }

    #[test]
    fn queue_progress_uses_frame_progress_for_regular_video_export() {
        let mut queue = queue_with_eta_job(JobStatus::Rendering);
        queue.export_project = 0;
        {
            let mut q = queue.queue.borrow_mut();
            let mut item = q[0].clone();
            item.current_frame = 25;
            q.change_line(0, item);
        }

        assert!(!queue.get_queue_progress_uses_jobs());
        assert!((queue.get_queue_progress() - 0.25).abs() < 0.001);
    }

    #[test]
    fn queue_progress_does_not_count_error_as_complete() {
        let mut queue = queue_with_eta_job(JobStatus::Error);
        queue.export_project = 2;
        {
            let mut q = queue.queue.borrow_mut();
            let mut item = q[0].clone();
            item.processing_progress = 0.4;
            q.change_line(0, item);
        }

        assert!((queue.get_queue_progress() - 0.4).abs() < 0.001);
        assert_eq!(queue.get_queue_done_jobs(), 0);
        assert_eq!(queue.get_queue_total_jobs(), 1);
    }

    #[test]
    fn reset_job_clears_stale_processing_progress() {
        let mut queue = queue_with_eta_job(JobStatus::Finished);
        {
            let mut q = queue.queue.borrow_mut();
            let mut item = q[0].clone();
            item.processing_progress = 1.0;
            item.current_frame = 1;
            item.total_frames = 1;
            item.start_timestamp = 123;
            item.end_timestamp = 456;
            q.change_line(0, item);
        }

        queue.reset_job(1);

        let q = queue.queue.borrow();
        assert_eq!(q[0].get_status(), &JobStatus::Queued);
        assert_eq!(q[0].processing_progress, 0.0);
        assert_eq!(q[0].current_frame, 0);
        assert_eq!(q[0].start_timestamp, 0);
        assert_eq!(q[0].end_timestamp, 0);
    }

    #[test]
    fn queue_eta_model_smooths_later_video_samples() {
        let mut model = QueueEtaEstimateModel::default();

        model.observe_completed_job(QueueEtaSample {
            sync_frames: 10,
            sync_ms: 1_000.0,
            render_frames: 100,
            render_ms: 10_000.0,
        });
        model.observe_completed_job(QueueEtaSample {
            sync_frames: 10,
            sync_ms: 2_000.0,
            render_frames: 100,
            render_ms: 20_000.0,
        });

        assert_eq!(model.completed_job_samples, 2);
        assert!((model.sync_ms_per_frame.unwrap() - 130.0).abs() < 0.01);
        assert!((model.render_ms_per_frame.unwrap() - 130.0).abs() < 0.01);
        assert_eq!(model.estimate_remaining_ms(10, 100, 1), Some(14_300));
    }

    #[test]
    fn queue_eta_sample_ignores_stale_render_epoch() {
        let mut queue = RenderQueue::default();
        let render_epoch = Arc::new(AtomicU64::new(2));
        queue.jobs.insert(
            1,
            Job {
                queue_index: 0,
                render_options: RenderOptions::default(),
                base_render_output_size: None,
                original_output_size: (0, 0),
                auto_rotate: false,
                additional_data: String::new(),
                cancel_flag: Default::default(),
                render_epoch: render_epoch.clone(),
                project_data: None,
                last_finished_export_project: None,
                stab: None,
                base_lens_metadata: None,
                lens_group_config_override: None,
                lens_group_index: None,
                video_created_at: None,
                original_video_rotation: 0.0,
            },
        );

        let sample = QueueEtaSample {
            sync_frames: 10,
            sync_ms: 1_000.0,
            render_frames: 100,
            render_ms: 10_000.0,
        };

        assert!(!queue.observe_eta_sample_for_epoch(1, 1, sample));
        assert_eq!(queue.eta_model.completed_job_samples, 0);

        assert!(queue.observe_eta_sample_for_epoch(1, 2, sample));
        assert_eq!(queue.eta_model.completed_job_samples, 1);
    }

    #[test]
    fn display_params_skip_manual_focal_when_video_has_auto_focal() {
        let queue = queue_with_lens_display_job(
            true,
            niyien_lens_presets::LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(50.0),
                ..Default::default()
            },
            auto_focal_metadata(),
        );

        let display = job_display_params(&queue);

        assert_eq!(display["lens_group_display_mode"], "auto");
        assert_eq!(display["lens_group_display_number"], 0);
        assert_eq!(display["lens_group_display_focal_length"], 0.0);
    }

    #[test]
    fn display_params_show_anamorphic_when_manual_edit_and_auto_focal() {
        let queue = queue_with_lens_display_job(
            true,
            niyien_lens_presets::LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(50.0),
                anamorphic_enabled: true,
                squeeze_ratio: Some(1.33),
                squeeze_direction: Some(niyien_lens_presets::SqueezeDirection::Vertical),
                ..Default::default()
            },
            auto_focal_metadata(),
        );

        let display = job_display_params(&queue);

        assert_eq!(display["lens_group_display_mode"], "global");
        assert_eq!(display["lens_group_display_number"], 1);
        assert_eq!(display["lens_group_display_focal_length"], 50.0);
        assert_eq!(display["lens_group_display_ratio"], 1.33);
        assert_eq!(display["lens_group_display_direction"], "V");
    }

    #[test]
    fn display_params_show_anamorphic_preset_ratio() {
        let queue = queue_with_lens_display_job(
            true,
            niyien_lens_presets::LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(50.0),
                anamorphic_enabled: true,
                preset_id: Some("sirui_xingchen_50mm_1_33x".to_owned()),
                squeeze_direction: Some(niyien_lens_presets::SqueezeDirection::Horizontal),
                ..Default::default()
            },
            auto_focal_metadata(),
        );

        let display = job_display_params(&queue);

        assert_eq!(display["lens_group_display_mode"], "global");
        assert_eq!(display["lens_group_display_ratio"], 1.33);
        assert_eq!(display["lens_group_display_direction"], "H");
    }

    #[test]
    fn display_params_use_backup_metadata_after_stab_release() {
        let mut queue = queue_with_lens_display_job(
            false,
            niyien_lens_presets::LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(50.0),
                ..Default::default()
            },
            core::gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 0 }),
                ..Default::default()
            },
        );
        queue.jobs.get_mut(&1).unwrap().stab = None;

        let display = job_display_params(&queue);

        assert_eq!(display["lens_group_display_mode"], "global");
        assert_eq!(display["lens_group_display_number"], 1);
        assert_eq!(display["lens_group_display_focal_length"], 50.0);
    }

    #[test]
    fn display_params_skip_anamorphic_when_manual_edit_off_and_auto_focal() {
        let queue = queue_with_lens_display_job(
            false,
            niyien_lens_presets::LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(50.0),
                anamorphic_enabled: true,
                squeeze_ratio: Some(1.33),
                ..Default::default()
            },
            auto_focal_metadata(),
        );

        let display = job_display_params(&queue);

        assert_eq!(display["lens_group_display_mode"], "auto");
        assert_eq!(display["lens_group_display_number"], 0);
        assert_eq!(display["lens_group_display_ratio"], 0.0);
    }

    #[test]
    fn display_params_show_manual_focal_when_video_auto_focal_missing() {
        let queue = queue_with_lens_display_job(
            false,
            niyien_lens_presets::LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(50.0),
                ..Default::default()
            },
            core::gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 0 }),
                ..Default::default()
            },
        );

        let display = job_display_params(&queue);

        assert_eq!(display["lens_group_display_mode"], "global");
        assert_eq!(display["lens_group_display_number"], 1);
        assert_eq!(display["lens_group_display_focal_length"], 50.0);
    }

    #[test]
    fn stale_gyro_parse_result_does_not_update_reused_index() {
        let mut queue = RenderQueue::default();
        queue.gyro_files.push(GyroFileInfo {
            id: 1,
            path: "file:///old_mix.bin".to_owned(),
            filename: "old_mix.bin".to_owned(),
            ..Default::default()
        });
        queue.gyro_files.clear();
        queue.gyro_files.push(GyroFileInfo {
            id: 2,
            path: "file:///new_mix.bin".to_owned(),
            filename: "new_mix.bin".to_owned(),
            ..Default::default()
        });

        let updated = queue.update_gyro_file_parse_result(
            0,
            1,
            "file:///old_mix.bin",
            (Some(1000), Some(2000.0), Some("old".to_owned()), None),
        );

        assert!(!updated);
        assert_eq!(queue.gyro_files[0].path, "file:///new_mix.bin");
        assert!(!queue.gyro_files[0].parsed);
        assert_eq!(queue.gyro_files[0].created_at_ms, None);
        assert_eq!(queue.gyro_files[0].duration_ms, None);
    }

    #[test]
    fn clear_gyro_files_clears_same_gyro_cache() {
        let mut queue = RenderQueue::default();
        queue.same_gyro_cache.insert(7, (true, true));

        queue.clear_gyro_files();

        assert!(queue.same_gyro_cache.is_empty());
    }

    #[test]
    fn assigned_gyro_job_ids_include_only_matched_render_jobs() {
        let mut queue = RenderQueue::default();
        for job_id in [10, 11, 12, 13] {
            queue.queue.borrow_mut().push(RenderQueueItem {
                job_id,
                ..Default::default()
            });
        }
        queue.match_results = Some(core::gyro_match::BatchMatchResult {
            results: vec![
                core::gyro_match::MatchResult {
                    video_index: 0,
                    job_id: Some(10),
                    gyro_index: Some(0),
                    status: core::gyro_match::MatchStatus::Matched,
                    global_offset_ms: None,
                    gyro_start_ms: None,
                    gyro_end_ms: None,
                    init_offset_ms: None,
                },
                core::gyro_match::MatchResult {
                    video_index: 1,
                    job_id: Some(11),
                    gyro_index: Some(0),
                    status: core::gyro_match::MatchStatus::CalibrationPair,
                    global_offset_ms: None,
                    gyro_start_ms: None,
                    gyro_end_ms: None,
                    init_offset_ms: None,
                },
                core::gyro_match::MatchResult {
                    video_index: 2,
                    job_id: Some(12),
                    gyro_index: None,
                    status: core::gyro_match::MatchStatus::Unmatched,
                    global_offset_ms: None,
                    gyro_start_ms: None,
                    gyro_end_ms: None,
                    init_offset_ms: None,
                },
                core::gyro_match::MatchResult {
                    video_index: 3,
                    job_id: Some(13),
                    gyro_index: None,
                    status: core::gyro_match::MatchStatus::NoCreationTime,
                    global_offset_ms: None,
                    gyro_start_ms: None,
                    gyro_end_ms: None,
                    init_offset_ms: None,
                },
            ],
            global_offset_ms: None,
            error: None,
        });

        let ids: Vec<u32> =
            serde_json::from_str(&queue.get_assigned_gyro_job_ids_json().to_string()).unwrap();

        assert_eq!(ids, vec![10]);
    }

    #[test]
    fn auto_rotate_decision_accepts_job_or_queue_state_for_senseflow_only() {
        assert!(should_apply_auto_rotate(false, true, false, "SenseFlow Mini"));
        assert!(should_apply_auto_rotate(false, false, true, "SenseFlow"));
        assert!(!should_apply_auto_rotate(false, false, false, "SenseFlow Mini"));
        assert!(!should_apply_auto_rotate(true, true, true, "SenseFlow Mini"));
        assert!(!should_apply_auto_rotate(false, true, true, "Sony FX3"));
    }

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
            original_output_size: (0, 0),
            auto_rotate: false,
            additional_data: String::new(),
            cancel_flag: Default::default(),
            render_epoch: Default::default(),
            project_data: None,
            last_finished_export_project: None,
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

    #[test]
    fn lens_profile_metadata_for_group_build_preserves_auto_focal_from_lens_params() {
        let metadata = core::gyro_source::FileMetadata {
            additional_data: serde_json::json!({ "lens_index": 0 }),
            unit_pixel_focal_length: Some(100.0),
            lens_params: BTreeMap::from([(
                0,
                core::gyro_source::LensParams {
                    focal_length: Some(31.0),
                    pixel_focal_length: Some(3100.0),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };
        let snapshot = lens_profile_metadata_for_group_build(&metadata);
        let config = niyien_lens_presets::LensGroupConfig {
            anamorphic_enabled: true,
            squeeze_ratio: Some(1.33),
            ..Default::default()
        };
        let cfg_for_build =
            niyien_lens_presets::effective_lens_group_config_for_build(true, &config, &metadata)
                .unwrap();

        let profile = niyien_lens_presets::build_lens_profile(
            &snapshot,
            (1920, 1080),
            Some(&cfg_for_build),
            Some(&core::lens_profile::LensProfile::default()),
        )
        .unwrap();

        assert_eq!(
            niyien_lens_presets::extract_video_focus_length_mm(&snapshot),
            Some(31.0)
        );
        assert_eq!(profile.focal_length, Some(31.0));
        assert_eq!(profile.fisheye_params.camera_matrix[0], [3100.0, 0.0, 1277.0]);
        assert_eq!(profile.fisheye_params.camera_matrix[1], [0.0, 3100.0, 540.0]);
    }

    fn default_exts() -> Vec<String> {
        vec![
            "mp4", "mov", "mxf", "mkv", "webm", "insv", "gyroflow", "png", "jpg", "exr", "dng",
            "braw", "r3d", "nev",
        ]
        .into_iter()
        .map(String::from)
        .collect()
    }

    #[test]
    fn filter_pairs_drops_gyroflow_when_sibling_video_present() {
        let urls = vec![
            "file:///C:/clips/a.mp4".to_string(),
            "file:///C:/clips/a.gyroflow".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl_with_project_reader(
            &urls,
            &default_exts(),
            |url| {
                if url == "file:///C:/clips/a.gyroflow" {
                    Some("file:///C:/clips/a.mp4".to_string())
                } else {
                    None
                }
            },
        );
        assert_eq!(out, vec!["file:///C:/clips/a.mp4".to_string()]);
    }

    #[test]
    fn filter_pairs_preserves_lone_gyroflow_without_sibling_video() {
        let urls = vec![
            "file:///C:/clips/preset.gyroflow".to_string(),
            "file:///C:/clips/clip1.mp4".to_string(),
            "file:///C:/clips/clip2.mp4".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl(&urls, &default_exts());
        assert_eq!(out, urls, "lone preset.gyroflow must be preserved");
    }

    #[test]
    fn filter_pairs_does_not_match_across_directories() {
        let urls = vec![
            "file:///C:/a/clip.mp4".to_string(),
            "file:///C:/b/clip.gyroflow".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl(&urls, &default_exts());
        assert_eq!(out, urls, "different dirs must not be paired");
    }

    #[test]
    fn filter_pairs_preserves_input_order() {
        let urls = vec![
            "file:///C:/x/c.mp4".to_string(),
            "file:///C:/x/a.mp4".to_string(),
            "file:///C:/x/a.gyroflow".to_string(),
            "file:///C:/x/b.mp4".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl_with_project_reader(
            &urls,
            &default_exts(),
            |url| {
                if url == "file:///C:/x/a.gyroflow" {
                    Some("file:///C:/x/a.mp4".to_string())
                } else {
                    None
                }
            },
        );
        assert_eq!(
            out,
            vec![
                "file:///C:/x/c.mp4".to_string(),
                "file:///C:/x/a.mp4".to_string(),
                "file:///C:/x/b.mp4".to_string(),
            ]
        );
    }

    #[test]
    fn filter_pairs_is_case_insensitive_on_extension_only() {
        let urls = vec![
            "file:///C:/x/Clip.MP4".to_string(),
            "file:///C:/x/Clip.Gyroflow".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl_with_project_reader(
            &urls,
            &default_exts(),
            |url| {
                if url == "file:///C:/x/Clip.Gyroflow" {
                    Some("file:///C:/x/Clip.MP4".to_string())
                } else {
                    None
                }
            },
        );
        assert_eq!(out, vec!["file:///C:/x/Clip.MP4".to_string()]);
    }

    #[test]
    fn filter_pairs_preserves_same_stem_gyroflow_preset_without_videofile() {
        let urls = vec![
            "file:///C:/clips/clip.mp4".to_string(),
            "file:///C:/clips/clip.gyroflow".to_string(),
        ];
        let out =
            filter_paired_gyroflow_siblings_impl_with_project_reader(&urls, &default_exts(), |_| {
                None
            });

        assert_eq!(out, urls);
    }

    #[test]
    fn filter_pairs_does_not_read_gyroflow_projects_when_no_video_is_loaded() {
        let urls = vec![
            "file:///C:/clips/a.gyroflow".to_string(),
            "file:///C:/clips/b.gyroflow".to_string(),
        ];
        let mut calls = 0;
        let out = filter_paired_gyroflow_siblings_impl_with_project_reader(
            &urls,
            &default_exts(),
            |_| {
                calls += 1;
                Some("file:///C:/clips/a.mp4".to_string())
            },
        );

        assert_eq!(out, urls);
        assert_eq!(calls, 0);
    }

    #[test]
    fn filter_pairs_matches_project_video_url_after_file_url_normalization() {
        let urls = vec![
            "file:///C:/clips/My%20Clip.mp4".to_string(),
            "file:///C:/clips/session.gyroflow".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl_with_project_reader(
            &urls,
            &default_exts(),
            |url| {
                if url == "file:///C:/clips/session.gyroflow" {
                    Some("file:///C:/clips/My Clip.mp4".to_string())
                } else {
                    None
                }
            },
        );

        assert_eq!(out, vec!["file:///C:/clips/My%20Clip.mp4".to_string()]);
    }

    #[test]
    fn read_gyroflow_project_video_url_reads_only_project_video_reference() {
        let dir = tempfile::tempdir().unwrap();
        let video_path = dir.path().join("clip.mp4");
        std::fs::write(&video_path, []).unwrap();
        let video_url = filesystem::path_to_url(&video_path.to_string_lossy());

        let project_path = dir.path().join("session.gyroflow");
        let project_json = serde_json::json!({
            "videofile": video_url,
            "image_sequence_start": 0,
            "raw_imu": [{ "timestamp_ms": 0.0, "gyro": [0.0, 0.0, 0.0] }]
        });
        std::fs::write(&project_path, project_json.to_string()).unwrap();
        let project_url = filesystem::path_to_url(&project_path.to_string_lossy());

        assert_eq!(read_gyroflow_project_video_url(&project_url), Some(video_url));
    }

    #[test]
    fn filter_pairs_case_sensitive_on_stem_for_posix_safety() {
        // Stem is compared case-sensitive so `Clip.mp4` and `clip.gyroflow`
        // on a case-sensitive filesystem are not wrongly paired.
        let urls = vec![
            "file:///srv/x/Clip.mp4".to_string(),
            "file:///srv/x/clip.gyroflow".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl(&urls, &default_exts());
        assert_eq!(out, urls);
    }

    #[test]
    fn filter_pairs_ignores_unknown_extensions() {
        let urls = vec![
            "file:///C:/x/a.txt".to_string(),
            "file:///C:/x/a.gyroflow".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl(&urls, &default_exts());
        assert_eq!(out, urls, ".txt is not a video, so .gyroflow is not paired");
    }

    #[test]
    fn filter_pairs_empty_and_single_are_noop() {
        assert!(filter_paired_gyroflow_siblings_impl(&[], &default_exts()).is_empty());
        let one = vec!["file:///C:/a.mp4".to_string()];
        assert_eq!(
            filter_paired_gyroflow_siblings_impl(&one, &default_exts()),
            one
        );
    }

    #[test]
    fn filter_pairs_respects_custom_extensions_whitelist() {
        // Caller can pass a narrower list; any ext outside it is treated as
        // non-video and won't pair.
        let urls = vec![
            "file:///C:/x/a.r3d".to_string(),
            "file:///C:/x/a.gyroflow".to_string(),
        ];
        let narrow = vec!["mp4".to_string(), "gyroflow".to_string()];
        let out = filter_paired_gyroflow_siblings_impl(&urls, &narrow);
        assert_eq!(out, urls, "r3d not in narrow whitelist → no pair");
    }

    #[test]
    fn filter_pairs_drops_gyroflow_when_project_points_to_loaded_video() {
        let urls = vec![
            "file:///C:/clips/clip.mp4".to_string(),
            "file:///C:/clips/session.gyroflow".to_string(),
        ];
        let out = filter_paired_gyroflow_siblings_impl_with_project_reader(
            &urls,
            &default_exts(),
            |url| {
                if url == "file:///C:/clips/session.gyroflow" {
                    Some("file:///C:/clips/clip.mp4".to_string())
                } else {
                    None
                }
            },
        );

        assert_eq!(out, vec!["file:///C:/clips/clip.mp4".to_string()]);
    }

    #[test]
    fn first_url_requiring_external_sdk_preserves_queue_order_and_passes_filename() {
        let urls = vec![
            "file:///C:/clips/a.mp4".to_string(),
            "file:///C:/clips/needs-red.R3D".to_string(),
            "file:///C:/clips/needs-braw.braw".to_string(),
        ];
        let mut checked = Vec::new();

        let out = first_url_requiring_external_sdk_impl(&urls, |filename| {
            checked.push(filename.to_string());
            filename.eq_ignore_ascii_case("needs-red.r3d")
        });

        assert_eq!(out, Some("file:///C:/clips/needs-red.R3D".to_string()));
        assert_eq!(checked, vec!["a.mp4", "needs-red.R3D"]);
    }

    #[test]
    fn first_renderable_video_file_uses_later_video_when_first_is_gyroflow() {
        let urls = vec![
            "file:///C:/clips/session.gyroflow".to_string(),
            "file:///C:/clips/clip.mp4".to_string(),
        ];

        let out = first_renderable_video_file_impl(&urls, &default_exts());

        assert_eq!(out, Some("file:///C:/clips/clip.mp4".to_string()));
    }

    #[test]
    fn gyro_mix_file_detection_accepts_only_mix_bin_suffix() {
        assert!(is_gyro_mix_file_url_impl("file:///C:/clips/cam_mix.bin"));
        assert!(is_gyro_mix_file_url_impl("file:///C:/clips/CAM_MIX.BIN"));
        assert!(!is_gyro_mix_file_url_impl("file:///C:/clips/cam.bin"));
        assert!(!is_gyro_mix_file_url_impl("file:///C:/clips/cam_mix.txt"));
    }

    #[test]
    fn supported_drop_item_accepts_mix_bin_when_it_is_first_url() {
        let urls = vec![
            "file:///C:/clips/cam_mix.bin".to_string(),
            "file:///C:/clips/clip.mov".to_string(),
        ];

        assert!(has_supported_drop_item_impl(&urls, &default_exts()));
    }

    #[test]
    fn supported_drop_item_rejects_plain_bin_without_video_or_folder() {
        let urls = vec!["file:///C:/clips/cam.bin".to_string()];

        assert!(!has_supported_drop_item_impl(&urls, &default_exts()));
    }

    #[test]
    fn supported_drop_filter_drops_unknown_file_when_video_is_present() {
        let urls = vec![
            "file:///C:/clips/notes.txt".to_string(),
            "file:///C:/clips/clip.mov".to_string(),
        ];

        let out = filter_supported_drop_items_impl(&urls, &default_exts());

        assert_eq!(out, vec!["file:///C:/clips/clip.mov".to_string()]);
    }

    #[test]
    fn supported_drop_filter_drops_plain_bin_but_keeps_mix_bin() {
        let urls = vec![
            "file:///C:/clips/plain.bin".to_string(),
            "file:///C:/clips/cam_mix.bin".to_string(),
            "file:///C:/clips/clip.mov".to_string(),
        ];

        let out = filter_supported_drop_items_impl(&urls, &default_exts());

        assert_eq!(
            out,
            vec![
                "file:///C:/clips/cam_mix.bin".to_string(),
                "file:///C:/clips/clip.mov".to_string(),
            ]
        );
    }

    #[test]
    fn simple_mode_batch_stabilized_export_writes_project_with_gyro_data() {
        let qml = include_str!("../ui/App.qml");
        let marker_idx = qml
            .find("Batch path")
            .expect("simple batch path marker exists");
        let remaining = &qml[marker_idx..];
        let branch_end = remaining
            .find("Single-video path")
            .expect("single-video path marker exists after batch branch");
        let branch = &remaining[..branch_end];

        assert!(branch.contains("videoArea.queue && videoArea.queue.shown"));
        assert!(branch.contains("simpleExportBtnRow.queueRowCount > 0"));
        assert!(
            branch.contains("render_queue.batch_motion_ready()"),
            "simple-mode batch stabilized export must require batch motion data"
        );
        assert!(
            branch.contains("render_queue.export_project = 4;"),
            "simple-mode batch stabilized export must use export_project=4"
        );
        assert!(branch.contains("render_queue.start();"));
        let prepare_idx = branch
            .find("render_queue.prepare_finished_jobs_for_video_export();")
            .expect("batch export prepares finished sync-only jobs before starting");
        let start_idx = branch
            .find("render_queue.start();")
            .expect("batch export starts the queue");
        assert!(
            prepare_idx < start_idx,
            "simple-mode batch stabilized export must prepare finished jobs before starting"
        );
        assert!(
            !branch.contains("render_queue.export_project = 0;"),
            "simple-mode batch stabilized export must not use export_project=0"
        );
    }

    #[test]
    fn simple_mode_batch_auto_sync_requires_motion_data() {
        let qml = include_str!("../ui/App.qml");
        let marker_idx = qml
            .find("id: simpleAutoSyncBtn")
            .expect("simple auto sync button exists");
        let remaining = &qml[marker_idx..];
        let branch_end = remaining
            .find("id: simpleExportStabilizedBtn")
            .expect("simple export button exists after auto sync button");
        let branch = &remaining[..branch_end];

        assert!(branch.contains("render_queue.batch_motion_ready()"));
        let click_idx = branch
            .find("onClicked:")
            .expect("auto sync button has click handler");
        let click_branch = &branch[click_idx..];
        let ready_idx = click_branch
            .find("if (!simpleAutoSyncBtn._queueMotionReady) return;")
            .expect("auto sync click branch hard-checks batch motion readiness");
        let start_idx = click_branch
            .find("render_queue.start();")
            .expect("auto sync branch starts the queue");
        assert!(
            ready_idx < start_idx,
            "simple-mode batch auto sync must check motion readiness before starting"
        );
        assert!(branch.contains("render_queue.export_project = 2;"));
    }

    #[test]
    fn render_queue_match_apply_refreshes_batch_motion_bindings() {
        let qml = include_str!("../ui/RenderQueue.qml");
        let marker_idx = qml
            .find("function onMatch_apply_finished")
            .expect("match apply finished handler exists");
        let remaining = &qml[marker_idx..];
        let branch_end = remaining
            .find("function onPairing_mode_changed")
            .expect("pairing mode handler follows match apply handler");
        let handler = &remaining[..branch_end];

        assert!(
            handler.contains("root.matchVersion++"),
            "match apply completion must refresh batch motion readiness bindings"
        );
    }

    #[test]
    fn render_queue_processing_done_refreshes_batch_motion_bindings() {
        let qml = include_str!("../ui/RenderQueue.qml");
        let marker_idx = qml
            .find("function onProcessing_done")
            .expect("processing done handler exists");
        let remaining = &qml[marker_idx..];
        let branch_end = remaining
            .find("function onPairing_mode_changed")
            .expect("pairing mode handler follows processing done handler");
        let handler = &remaining[..branch_end];

        assert!(
            handler.contains("root.matchVersion++"),
            "queue processing completion must refresh embedded-motion readiness bindings"
        );
    }

    #[test]
    fn render_queue_top_progress_uses_backend_queue_progress() {
        let qml = include_str!("../ui/RenderQueue.qml");
        let marker_idx = qml
            .find("id: topCol")
            .expect("top progress column exists");
        let remaining = &qml[marker_idx..];
        let branch_end = remaining
            .find("Connections {")
            .expect("top progress column ends before queue connections");
        let top_progress = &remaining[..branch_end];

        assert!(
            top_progress.contains("render_queue.queue_progress"),
            "top queue progress must use the backend aggregate progress"
        );
        assert!(
            top_progress.contains("render_queue.queue_progress_uses_jobs"),
            "top queue progress must use the backend display mode"
        );
        assert!(
            top_progress.contains("render_queue.queue_done_jobs")
                && top_progress.contains("render_queue.queue_total_jobs"),
            "job-weighted top queue progress must display completed jobs over total jobs"
        );
        assert!(
            !top_progress.contains("processing_progress"),
            "top queue progress must not scan per-job processing progress in QML"
        );
    }

    #[test]
    fn render_queue_display_flows_do_not_bind_height_to_children_rect() {
        let qml = include_str!("../ui/RenderQueue.qml");
        let marker_idx = qml
            .find("Aligned display params")
            .expect("display params block exists");
        let remaining = &qml[marker_idx..];
        let block_end = remaining
            .find("Match status annotation")
            .expect("display params block ends before match status");
        let display_params_block = &remaining[..block_end];

        assert!(
            !display_params_block.contains("height: visible ? childrenRect.height : 0;"),
            "display parameter Flow blocks must not bind height directly to childrenRect"
        );
    }

    #[test]
    fn render_queue_selection_uses_model_job_ids_not_delegate_instances() {
        let qml = include_str!("../ui/RenderQueue.qml");

        assert!(
            !qml.contains("lv.itemAtIndex("),
            "batch selection must use render_queue.queue job ids instead of virtualized ListView delegates"
        );
        assert!(
            qml.contains("render_queue.queue"),
            "batch selection must read job ids from the queue model"
        );
    }

    #[test]
    fn render_queue_checkbox_shift_select_has_explicit_modifier_handler() {
        let qml = include_str!("../ui/RenderQueue.qml");

        assert!(
            qml.contains("acceptedModifiers: Qt.ShiftModifier"),
            "checkbox selection must have a Shift-specific tap handler"
        );
        assert!(
            qml.contains("root.handleSelectionClick(dlg.jobId, index, Qt.ShiftModifier)"),
            "checkbox Shift tap must pass ShiftModifier into range selection"
        );
    }

    #[test]
    fn render_queue_drag_select_uses_content_coordinates_for_index_lookup() {
        let qml = include_str!("../ui/RenderQueue.qml");

        assert!(
            qml.contains("mapToItem(lv.contentItem"),
            "drag-select pointer movement must map to ListView content coordinates"
        );
        assert!(
            qml.contains("function updateDragSelectionAtContentY(contentY)"),
            "drag-select must update from stable content coordinates"
        );
        assert!(
            !qml.contains("lv.indexAt(1, lv.contentY + viewY)"),
            "drag-select must not mix ListView viewport and content coordinates"
        );
    }

    #[test]
    fn batch_stabilization_controls_write_batch_state_directly() {
        let simple_qml = include_str!("../ui/menu/SimpleStabilization.qml");
        let full_qml = include_str!("../ui/menu/Stabilization.qml");

        for needle in [
            "window.batchState.smoothness = value;",
            "window.batchState.zoomMode = currentIndex;",
            "window.batchState.lensCorrection = checked ? 1.0 : 0.0;",
            "window.batchState.framerate = value;",
        ] {
            assert!(
                simple_qml.contains(needle),
                "SimpleStabilization.qml must directly update batch state: {needle}"
            );
        }

        for needle in [
            "window.batchState.smoothness = value * 100.0;",
            "window.batchState.zoomMode = currentIndex;",
            "window.batchState.lensCorrection = value;",
            "window.batchState.framerate = value;",
        ] {
            assert!(
                full_qml.contains(needle),
                "Stabilization.qml must directly update batch state: {needle}"
            );
        }
    }

    #[test]
    fn app_batch_sync_uses_primary_selection_and_explicit_framerate_controls() {
        let qml = include_str!("../ui/App.qml");

        assert!(
            qml.contains("videoArea.queue.getPrimarySelectedJobId()"),
            "batch parameter loading must use a stable primary selected job"
        );
        assert!(
            !qml.contains("render_queue.get_job_display_params(+keys[0])"),
            "batch parameter loading must not depend on Object.keys(selectedJobs)[0]"
        );
        assert!(
            qml.contains("window.stab.batchFramerateField.value = batchState.framerate")
                && qml.contains("simpleStab.batchFramerateField.value = batchState.framerate"),
            "batch framerate controls must be explicitly synchronized from batchState"
        );
    }

    #[test]
    fn app_batch_state_changes_apply_to_selected_jobs_immediately() {
        let qml = include_str!("../ui/App.qml");

        assert!(
            qml.contains("function scheduleApplyBatchParams()"),
            "batch state edits must schedule applying params to selected render queue jobs"
        );
        assert!(
            qml.contains("property bool _batchApplySuppressed"),
            "batch state loading/syncing must suppress auto-apply"
        );

        for needle in [
            "onSmoothnessChanged: window.scheduleApplyBatchParams()",
            "onHorizonLockChanged: window.scheduleApplyBatchParams()",
            "onHorizonLockAmountChanged: window.scheduleApplyBatchParams()",
            "onZoomModeChanged: window.scheduleApplyBatchParams()",
            "onLensCorrectionChanged: window.scheduleApplyBatchParams()",
            "onFramerateChanged: window.scheduleApplyBatchParams()",
        ] {
            assert!(
                qml.contains(needle),
                "batch state change must auto-apply: {needle}"
            );
        }
    }
}
