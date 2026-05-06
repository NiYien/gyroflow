// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#[cfg(feature = "opencv")]
pub mod calibration;
pub mod camera_identifier;
pub mod distribution;
pub mod gyro_source;
pub mod imu_integration;
pub mod keyframes;
pub mod lens_profile;
pub mod lens_profile_database;
pub mod niyien_lens_presets;
pub mod stabilization;
pub mod stmap;
pub mod synchronization;

pub mod filesystem;
pub mod filtering;
pub mod gyro_export;
pub mod settings;
pub mod smoothing;
pub mod zooming;

pub mod gpu;
pub mod gyro_match;
#[cfg(feature = "neuflow-ort")]
pub mod neuflow;
#[cfg(feature = "neuflow-burn")]
pub mod neuflow_burn;

pub mod stabilization_params;
pub mod util;
pub mod log_context;
pub mod smooth_diag;

use camera_identifier::CameraIdentifier;
use gpu::Buffers;
use gpu::drawing::*;
use gyro_source::{GyroSource, Quat64, TimeQuat, TimeVec};
use keyframes::*;
use lens_profile::LensProfile;
use lens_profile_database::LensProfileDatabase;
use nalgebra::Vector4;
use niyien_lens_presets::{LensGroupConfig, LensGroupStatus};
use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use smoothing::Smoothing;
pub use stabilization::PixelType;
use stabilization::{ComputeParams, KernelParamsFlags, Stabilization};
use stabilization_params::{ReadoutDirection, StabilizationParams};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering::SeqCst},
};
pub use wgpu::TextureFormat as WgpuTextureFormat;

use std::io::{Read, Seek};
pub use telemetry_parser;

#[cfg(feature = "opencv")]
use calibration::LensCalibrator;

lazy_static::lazy_static! {
    static ref THREAD_POOL: rayon::ThreadPool = rayon::ThreadPoolBuilder::new().build().unwrap();
}

fn constrained_output_size(
    input_size: (usize, usize),
    requested_size: (usize, usize),
    video_rotation: f64,
    input_stretch: (f64, f64),
) -> Option<(usize, usize)> {
    if requested_size.0 == 0 || requested_size.1 == 0 {
        return None;
    }

    let stretch_h = if input_stretch.0 > 0.01 {
        input_stretch.0
    } else {
        1.0
    };
    let stretch_v = if input_stretch.1 > 0.01 {
        input_stretch.1
    } else {
        1.0
    };

    let r = video_rotation.abs();
    let (ow, oh) = if r == 90.0 || r == 270.0 {
        (
            input_size.1 as f64 * stretch_v,
            input_size.0 as f64 * stretch_h,
        )
    } else {
        (
            input_size.0 as f64 * stretch_h,
            input_size.1 as f64 * stretch_v,
        )
    };

    let wp = requested_size.0 as f64;
    let hp = requested_size.1 as f64;
    let scale = (ow / wp).min(oh / hp);

    let mut nw = (wp * scale).round() as usize;
    let mut nh = (hp * scale).round() as usize;

    if nw % 2 != 0 {
        nw -= 1;
    }
    if nh % 2 != 0 {
        nh -= 1;
    }

    Some((nw, nh))
}

#[derive(Default, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct InputFile {
    pub url: String,
    pub project_file_url: Option<String>,
    pub image_sequence_fps: f64,
    pub image_sequence_start: i32,
    pub preset_name: Option<String>,
    pub preset_output_size: Option<(usize, usize)>,
}

#[derive(Clone)]
pub struct SyncData {
    pub rank: Vec<f32>,
    pub ratio: f64,
    pub rank_window_center_offset_ms: f64,
}
impl Default for SyncData {
    fn default() -> Self {
        Self {
            rank: vec![],
            ratio: 0.016,
            rank_window_center_offset_ms: 0.0,
        }
    }
}

#[derive(Clone)]
pub struct StabilizationManager {
    pub gyro: Arc<RwLock<GyroSource>>,
    pub lens: Arc<RwLock<LensProfile>>,
    pub smoothing: Arc<RwLock<Smoothing>>,

    pub stabilization: Arc<RwLock<Stabilization>>,

    pub pose_estimator: Arc<synchronization::PoseEstimator>,
    #[cfg(feature = "opencv")]
    pub lens_calibrator: Arc<RwLock<Option<LensCalibrator>>>,

    pub current_compute_id: Arc<AtomicU64>,
    pub smoothing_checksum: Arc<AtomicU64>,
    pub zooming_checksum: Arc<AtomicU64>,
    pub prevent_recompute: Arc<AtomicBool>,
    pub smoothing_invalidated: Arc<AtomicBool>,
    pub zooming_invalidated: Arc<AtomicBool>,
    pub undistortion_invalidated: Arc<AtomicBool>,
    pub gpu_decoding: Arc<AtomicBool>,

    pub camera_id: Arc<RwLock<Option<CameraIdentifier>>>,
    pub lens_profile_db: Arc<RwLock<LensProfileDatabase>>,

    pub input_file: Arc<RwLock<InputFile>>,

    pub lens_group_config: Arc<RwLock<Vec<LensGroupConfig>>>,
    pub lens_group_status: Arc<RwLock<Vec<LensGroupStatus>>>,
    // Global "Manual edit" toggle for the lens group panel. Missing focal length
    // can still be filled from the group config; anamorphic replacement requires
    // this toggle — see `should_use_manual_config`.
    pub lens_group_manual_edit: Arc<AtomicBool>,
    // Snapshot of the lens profile taken the first time anamorphic is enabled, used to
    // restore the original fx/fy/cx/cy/distortion when anamorphic is switched off again.
    // Not serialized — purely runtime UI state.
    pub pre_anamorphic_backup: Arc<RwLock<Option<LensProfile>>>,

    pub keyframes: Arc<RwLock<KeyframeManager>>,

    pub params: Arc<RwLock<StabilizationParams>>,

    pub sync_data: Arc<RwLock<SyncData>>,
}

impl Default for StabilizationManager {
    fn default() -> Self {
        util::init_telemetry_parser();
        Self {
            smoothing: Arc::new(RwLock::new(Smoothing::default())),

            params: Arc::new(RwLock::new(StabilizationParams::default())),

            stabilization: Arc::new(RwLock::new(Stabilization::default())),
            gyro: Arc::new(RwLock::new(GyroSource::new())),
            lens: Arc::new(RwLock::new(LensProfile::default())),

            current_compute_id: Arc::new(AtomicU64::new(0)),
            smoothing_checksum: Arc::new(AtomicU64::new(0)),
            zooming_checksum: Arc::new(AtomicU64::new(0)),
            prevent_recompute: Arc::new(AtomicBool::new(false)),
            smoothing_invalidated: Arc::new(AtomicBool::new(false)),
            zooming_invalidated: Arc::new(AtomicBool::new(false)),
            undistortion_invalidated: Arc::new(AtomicBool::new(false)),

            gpu_decoding: Arc::new(AtomicBool::new(settings::get_bool("gpudecode", true))),

            pose_estimator: Arc::new(synchronization::PoseEstimator::default()),

            lens_profile_db: Arc::new(RwLock::new(LensProfileDatabase::default())),

            input_file: Arc::new(RwLock::new(InputFile::default())),
            lens_group_config: Arc::new(RwLock::new({
                // Restore persisted per-group lens configs (focal length, anamorphic, manual flag)
                // so user-made settings survive across sessions.
                let stored = settings::get_str("lens_group_configs_v1", "");
                if stored.is_empty() {
                    niyien_lens_presets::default_lens_group_configs()
                } else {
                    niyien_lens_presets::lens_group_configs_from_json(&stored)
                }
            })),
            lens_group_status: Arc::new(RwLock::new(
                niyien_lens_presets::default_lens_group_statuses(),
            )),
            lens_group_manual_edit: Arc::new(AtomicBool::new(settings::get_bool(
                "lens_group_manual_edit",
                false,
            ))),
            pre_anamorphic_backup: Arc::new(RwLock::new(None)),

            #[cfg(feature = "opencv")]
            lens_calibrator: Arc::new(RwLock::new(None)),

            keyframes: Arc::new(RwLock::new(KeyframeManager::new())),

            camera_id: Arc::new(RwLock::new(None)),

            sync_data: Arc::new(RwLock::new(SyncData::default())),
        }
    }
}

fn populate_lens_metadata_fields(
    lens: &mut LensProfile,
    md: &gyro_source::FileMetadata,
    size: (usize, usize),
) {
    if let Some(ref cam_id) = md.camera_identifier {
        if lens.camera_brand.is_empty() {
            lens.camera_brand = cam_id.brand.clone();
        }
        if lens.camera_model.is_empty() {
            lens.camera_model = cam_id.model.clone();
        }
        if lens.lens_model.is_empty() {
            lens.lens_model = cam_id.lens_model.clone();
        }
        if lens.camera_setting.is_empty() {
            lens.camera_setting = cam_id.camera_setting.clone();
        }
        if lens.focal_length.is_none() {
            lens.focal_length = cam_id.focal_length;
        }
    } else if let Some(detected) = md.detected_source.as_deref() {
        let parts: Vec<&str> = detected.splitn(2, ' ').collect();
        if lens.camera_brand.is_empty() {
            lens.camera_brand = parts.first().unwrap_or(&"").to_string();
        }
        if lens.camera_model.is_empty() {
            lens.camera_model = parts.get(1).unwrap_or(&"").to_string();
        }
    }

    if let Some(first_lp) = md.lens_params.values().next() {
        if lens.focal_length.is_none() {
            lens.focal_length = first_lp.focal_length.map(|v| v as f64);
        }
        if lens.fisheye_params.camera_matrix.is_empty() {
            if let Some(pfl) = first_lp.pixel_focal_length {
                lens.fisheye_params.camera_matrix = vec![
                    [pfl as f64, 0.0, size.0 as f64 / 2.0],
                    [0.0, pfl as f64, size.1 as f64 / 2.0],
                    [0.0, 0.0, 1.0],
                ];
            }
        }
    }

    if lens.calib_dimension.w == 0 || lens.calib_dimension.h == 0 {
        lens.calib_dimension = crate::lens_profile::Dimensions {
            w: size.0,
            h: size.1,
        };
    }
    if lens.orig_dimension.w == 0 || lens.orig_dimension.h == 0 {
        lens.orig_dimension = crate::lens_profile::Dimensions {
            w: size.0,
            h: size.1,
        };
    }

    if lens.calibrated_by.is_empty() {
        lens.calibrated_by = "NiYien".to_string();
    }
    let sanitized_readout_time = md.frame_readout_time.filter(|v| v.is_finite());
    if lens
        .frame_readout_time
        .map(|v| v.is_finite())
        .unwrap_or(false)
        == false
    {
        lens.frame_readout_time = sanitized_readout_time;
    }
    if lens.frame_readout_direction.is_none() && sanitized_readout_time.is_some() {
        lens.frame_readout_direction = Some(md.frame_readout_direction);
    }
    lens.official = true;
}

fn build_synthetic_lens_profile(
    md: &gyro_source::FileMetadata,
    size: (usize, usize),
) -> LensProfile {
    let mut lens = LensProfile::default();
    populate_lens_metadata_fields(&mut lens, md, size);
    lens
}

fn apply_effective_frame_rate(params: &mut StabilizationParams, effective_fps: f64) -> bool {
    if !effective_fps.is_finite()
        || effective_fps <= 0.0
        || !params.fps.is_finite()
        || params.fps <= 0.0
    {
        return false;
    }

    if (effective_fps - params.fps).abs() > 0.001 {
        params.fps_scale = Some(effective_fps / params.fps);
    } else {
        params.fps_scale = None;
    }
    true
}

impl StabilizationManager {
    fn apply_effective_video_fps(&self, effective_fps: f64) -> bool {
        let mut params = self.params.write();
        if !apply_effective_frame_rate(&mut params, effective_fps) {
            return false;
        }
        self.gyro.write().init_from_params(&params);
        self.keyframes.write().timestamp_scale = params.fps_scale;
        true
    }

    pub fn apply_main_video_telemetry(
        &self,
        md: &mut gyro_source::FileMetadata,
        url: &str,
        preserve_existing_readout_if_missing: bool,
    ) {
        if md
            .detected_source
            .as_ref()
            .map(|v| v.starts_with("GoPro "))
            .unwrap_or_default()
        {
            // If gopro reports rolling shutter value, it already applied it, ie. the video is already corrected
            md.frame_readout_time = None;
        }

        let size = {
            let p = self.params.read();
            p.size
        };
        let can_build_synthetic =
            !md.lens_params.is_empty() || md.unit_pixel_focal_length.is_some();
        let mut should_build_synthetic = false;

        if let Some(ref lens) = md.lens_profile {
            let mut l = self.lens.write();
            if let Some(lens_str) = lens.as_str() {
                let mut db = self.lens_profile_db.read();
                if !db.loaded {
                    drop(db);
                    {
                        let mut db = self.lens_profile_db.write();
                        db.load_all();
                    }
                    db = self.lens_profile_db.read();
                }
                if let Some(found) = db.find(lens_str) {
                    *l = found.clone();
                } else {
                    should_build_synthetic = can_build_synthetic;
                }
            } else if lens.is_object() {
                l.load_from_json_value(lens);
                l.path_to_file = filesystem::url_to_path(url);
                let db = self.lens_profile_db.read();
                l.resolve_interpolations(&db);
            }
        } else if can_build_synthetic {
            should_build_synthetic = true;
        }

        if should_build_synthetic {
            let synthetic = build_synthetic_lens_profile(md, size);
            *self.lens.write() = synthetic;
        }

        // Lens-group profile build. Pass the group config only when it should fill
        // missing focal length or apply manual-edit anamorphic; otherwise build from
        // telemetry auto focal only.
        let manual_edit = self.lens_group_manual_edit.load(SeqCst);
        if let Some(lens_index) = niyien_lens_presets::extract_lens_index(&md.additional_data) {
            let group_cfg = self.lens_group_config.read().get(lens_index).cloned();
            let cfg_for_build = group_cfg
                .as_ref()
                .and_then(|cfg| {
                    niyien_lens_presets::effective_lens_group_config_for_build(
                        manual_edit,
                        cfg,
                        md,
                    )
                });
            let baseline = self.lens.read().clone();
            if let Some(profile) = niyien_lens_presets::build_lens_profile(
                md,
                size,
                cfg_for_build.as_ref(),
                Some(&baseline),
            ) {
                *self.lens.write() = profile;
            }
        }

        let record_fps_applied = md
            .record_frame_rate
            .map(|record_fps| self.apply_effective_video_fps(record_fps))
            .unwrap_or(false);

        if !record_fps_applied {
            if let Some(md_fps) = md.frame_rate {
                let fps = self.params.read().fps;
                if (md_fps - fps).abs() > 1.0 {
                    self.override_video_fps(md_fps, false);
                }
            }
        } else if let Some(record_fps) = md.record_frame_rate {
            let fps = self.params.read().fps;
            log::info!(
                "[video_fps] record_fps={record_fps:.6}, playback_fps={fps:.6}, fps_scale={:?}",
                self.params.read().fps_scale
            );
        }

        let mut frame_readout_direction = md.frame_readout_direction;
        if md
            .detected_source
            .as_ref()
            .map(|v| v.starts_with("Blackmagic "))
            .unwrap_or_default()
        {
            if let Some(rot) = md.additional_data.get("rotation").and_then(|x| x.as_u64()) {
                if rot == 90 || rot == 270 {
                    log::info!("Using horizontal rolling shutter correction");
                    if rot == 90 {
                        frame_readout_direction = ReadoutDirection::RightToLeft;
                        md.imu_orientation = Some("xYz".into());
                    } else {
                        frame_readout_direction = ReadoutDirection::LeftToRight;
                        md.imu_orientation = Some("Xyz".into());
                    }
                }
                if rot == 180 {
                    frame_readout_direction = ReadoutDirection::BottomToTop;
                    md.imu_orientation = Some("YXz".into());
                }
            }
        }

        if let Some(frame_readout_time) = md.frame_readout_time.filter(|v| v.is_finite()) {
            let mut params = self.params.write();
            params.frame_readout_direction = frame_readout_direction;
            params.frame_readout_time = frame_readout_time;
        } else if !preserve_existing_readout_if_missing {
            let mut params = self.params.write();
            params.frame_readout_direction = frame_readout_direction;
            params.frame_readout_time = 0.0;
        }
    }

    pub fn autoload_lens_from_camera_id(&self) -> Result<bool, crate::GyroflowCoreError> {
        let has_builtin_profile = {
            let gyro = self.gyro.read();
            let file_metadata = gyro.file_metadata.read();
            file_metadata
                .lens_profile
                .as_ref()
                .map(|y| y.is_object())
                .unwrap_or_default()
        };

        let id_str = self
            .camera_id
            .read()
            .as_ref()
            .map(|v| v.get_identifier_for_autoload())
            .unwrap_or_default();
        if id_str.is_empty() || has_builtin_profile {
            return Ok(false);
        }

        let mut db = self.lens_profile_db.read();
        if !db.loaded {
            drop(db);
            {
                let mut db = self.lens_profile_db.write();
                db.load_all();
            }
            db = self.lens_profile_db.read();
        }
        if !db.contains_id(&id_str) {
            return Ok(false);
        }
        drop(db);

        self.load_lens_profile(&id_str)?;

        let (fr, frd) = {
            let lens = self.lens.read();
            (lens.frame_readout_time, lens.frame_readout_direction)
        };
        if let Some(fr) = fr {
            let mut params = self.params.write();
            params.frame_readout_time = fr.abs();
            params.frame_readout_direction = frd.unwrap_or(if fr < 0.0 {
                ReadoutDirection::BottomToTop
            } else {
                ReadoutDirection::TopToBottom
            });
        }
        Ok(true)
    }

    pub fn init_from_video_data(
        &self,
        duration_ms: f64,
        fps: f64,
        frame_count: usize,
        video_size: (usize, usize),
    ) {
        {
            let mut params = self.params.write();
            params.fps = fps;
            params.frame_count = frame_count;
            params.duration_ms = duration_ms;
            params.size = video_size;
        }
        if duration_ms < 10000.0 {
            // If the video is shorter than 10s, use Complementary
            let mut gyro_source = self.gyro.write();
            gyro_source.integration_method = 1; // Complementary
        }

        self.pose_estimator.sync_results.write().clear();
        self.keyframes.write().clear();
    }

    pub fn load_gyro_data<T: Read + Seek, F: Fn(f64)>(
        &self,
        stream: &mut T,
        filesize: usize,
        url: &str,
        is_main_video: bool,
        options: &gyro_source::FileLoadOptions,
        progress_cb: F,
        cancel_flag: Arc<AtomicBool>,
    ) -> std::result::Result<(), GyroflowCoreError> {
        let t_total = std::time::Instant::now();
        log::info!(
            "[load_gyro_data] begin url='{}' filesize={} is_main_video={} header_only={} time_range_ms={:?}",
            url,
            filesize,
            is_main_video,
            options.header_only,
            options.time_range_ms
        );
        let backup_lens_data = if !is_main_video {
            let gyro = self.gyro.read();
            let fm = gyro.file_metadata.read();
            // C3: Komodo main video keeps its own gyro and rejects external IMU.
            // Komodo's internal IMU is the only RED gyro we trust, so once the
            // main video has been classified as Komodo, any subsequent external
            // IMU load is silently ignored (no clear, no overwrite).
            if fm.is_komodo {
                log::info!(
                    "[red_arbitration] main video is RED Komodo, ignoring external IMU file: {url}"
                );
                log::info!(
                    "[load_gyro_data] end url='{}' elapsed_ms={:.1} skipped=komodo_external_imu",
                    url,
                    t_total.elapsed().as_secs_f64() * 1000.0
                );
                return Ok(());
            }
            Some((
                fm.lens_params.clone(),
                fm.lens_positions.clone(),
                fm.lens_profile.clone(),
            ))
        } else {
            None
        };
        {
            let params = self.params.read();
            let mut gyro = self.gyro.write();
            gyro.init_from_params(&params);
            gyro.clear();
            gyro.file_url = url.to_string();
            gyro.file_metadata = Default::default();
        }
        self.invalidate_smoothing();
        self.invalidate_zooming();

        let last_progress = std::cell::RefCell::new(std::time::Instant::now());
        let progress_cb = |p| {
            let now = std::time::Instant::now();
            if (now - *last_progress.borrow()).as_millis() > 100 {
                progress_cb(p);
                *last_progress.borrow_mut() = now;
            }
        };

        let (fps, size) = {
            let params = self.params.read();
            (params.fps, params.size)
        };

        let cancel_flag2 = cancel_flag.clone();
        let parse_result = GyroSource::parse_telemetry_file(
            stream,
            filesize,
            &url,
            options,
            size,
            fps,
            progress_cb,
            cancel_flag2,
        );
        let mut md = match parse_result {
            Ok(md) => md,
            Err(e) => {
                log::warn!(
                    "[load_gyro_data] error url='{}' elapsed_ms={:.1} error={}",
                    url,
                    t_total.elapsed().as_secs_f64() * 1000.0,
                    e
                );
                return Err(e);
            }
        };

        if is_main_video {
            self.apply_main_video_telemetry(&mut md, &url, false);
        } else {
            log::info!(
                "Not a main video, clearing {} per-frame offsets",
                md.per_frame_time_offsets.len()
            );
            md.per_frame_time_offsets.clear();
        }
        // Restore lens data from main video if external gyro file doesn't provide its own
        if let Some((backup_params, backup_positions, backup_profile)) = backup_lens_data {
            if md.lens_params.is_empty() {
                md.lens_params = backup_params;
            }
            if md.lens_positions.is_empty() {
                md.lens_positions = backup_positions;
            }
            if md.lens_profile.is_none() {
                md.lens_profile = backup_profile;
            }
        }
        let camera_id = md.camera_identifier.clone();
        if !cancel_flag.load(SeqCst) {
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
                let mut gyro = self.gyro.write();
                gyro.load_from_telemetry(md);
                gyro.file_load_options = options.clone();
                let fm = gyro.file_metadata.read();
                (
                    fm.raw_imu.len(),
                    fm.quaternions.len(),
                    fm.lens_params.len(),
                    fm.lens_positions.len(),
                    fm.creation_date_utc.clone(),
                    fm.has_accurate_timestamps,
                    fm.detected_source.clone(),
                    fm.is_komodo,
                )
            };
            log::info!(
                "[load_gyro_data] end url='{}' elapsed_ms={:.1} canceled=false raw_imu={} quats={} lens_params={} lens_positions={} creation_date_utc={:?} accurate_ts={} detected={:?} is_komodo={}",
                url,
                t_total.elapsed().as_secs_f64() * 1000.0,
                raw_imu,
                quaternions,
                lens_params,
                lens_positions,
                creation_date_utc,
                has_accurate_timestamps,
                detected_source,
                is_komodo
            );
        } else {
            log::info!(
                "[load_gyro_data] end url='{}' elapsed_ms={:.1} canceled=true",
                url,
                t_total.elapsed().as_secs_f64() * 1000.0
            );
        }

        if let Some(id) = camera_id {
            *self.camera_id.write() = Some(id);
        }
        Ok(())
    }

    pub fn load_lens_profile(&self, url: &str) -> Result<(), crate::GyroflowCoreError> {
        let url = if (url.starts_with('/')
            || url.starts_with('\\')
            || (url.len() > 3 && &url[1..2] == ":"))
            && !url.contains("://")
            && !url.starts_with('{')
        {
            crate::filesystem::path_to_url(url)
        } else {
            url.to_owned()
        };
        let db = self.lens_profile_db.read();
        let (result, from_db) = if let Some(lens) = db.get_by_id(&url) {
            *self.lens.write() = lens.clone();
            (Ok(()), true)
        } else if url.starts_with('{') {
            (self.lens.write().load_from_data(&url), false)
        } else {
            (self.lens.write().load_from_file(&url), false)
        };
        let (width, height, aspect, id, fps) = {
            let params = self.params.read();
            (
                params.size.0,
                params.size.1,
                ((params.size.0 * 100) as f64 / params.size.1.max(1) as f64).round() as u32,
                self.camera_id
                    .read()
                    .as_ref()
                    .map(|x| x.get_identifier_for_autoload())
                    .unwrap_or_default(),
                (params.fps * 100.0).round() as i32,
            )
        };

        let mut lens = self.lens.write();

        // Check if the lens profile needs to be swapped for vertical
        let lens_aspect_swapped = ((lens.calib_dimension.h * 100) as f64
            / lens.calib_dimension.w.max(1) as f64)
            .round() as u32;
        if (width == lens.calib_dimension.h && height == lens.calib_dimension.w)
            || lens_aspect_swapped == aspect
        {
            log::info!(
                "Lens profile swapped from {}x{} to {}x{} to match the video aspect",
                lens.calib_dimension.w,
                lens.calib_dimension.h,
                lens.calib_dimension.h,
                lens.calib_dimension.w
            );
            *lens = lens.swapped();
        }

        let matching = lens.get_all_matching_profiles();
        if matching.len() > 1 {
            let mut found = false;
            if !id.is_empty() && lens.identifier == id {
                found = true;
            }
            // Find best match for:
            if !found {
                // 1. Identifier
                for x in &matching {
                    if !id.is_empty() && x.identifier == id {
                        *lens = x.clone();
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                // 2. Resolution and fps
                for x in &matching {
                    if width == x.calib_dimension.w
                        && height == x.calib_dimension.h
                        && fps == (x.fps * 100.0).round() as i32
                    {
                        *lens = x.clone();
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                // 3. Aspect ratio and fps
                for x in &matching {
                    let a = ((x.calib_dimension.w * 100) as f64 / x.calib_dimension.h.max(1) as f64)
                        .round() as u32;
                    if a == aspect && fps == (x.fps * 100.0).round() as i32 {
                        *lens = x.clone();
                        break;
                    }
                }
            }
            if !found {
                // 4. Resolution
                for x in &matching {
                    if width == x.calib_dimension.w && height == x.calib_dimension.h {
                        *lens = x.clone();
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                // 5. Aspect ratio
                for x in &matching {
                    let a = ((x.calib_dimension.w * 100) as f64 / x.calib_dimension.h.max(1) as f64)
                        .round() as u32;
                    if a == aspect {
                        *lens = x.clone();
                        break;
                    }
                }
            }
        }
        if !from_db {
            lens.resolve_interpolations(&db);
        }
        result
    }

    pub fn init_size(&self) {
        let (w, h, ow, oh) = {
            let params = self.params.read();
            (
                params.size.0,
                params.size.1,
                params.output_size.0,
                params.output_size.1,
            )
        };

        if w > 0 && ow > 0 && h > 0 && oh > 0 {
            {
                let mut stab = self.stabilization.write();
                stab.init_size((w, h), (ow, oh));
            }
            self.lens.write().optimal_fov = None;

            // Refresh compute_params so FrameTransform uses the new output_size /
            // lens.camera_matrix when sampling (otherwise the preview keeps sampling
            // against stale cx/cy after anamorphic squeezes the output dimension).
            let compute_params = stabilization::ComputeParams::from_manager(self);
            self.stabilization
                .write()
                .set_compute_params(compute_params);

            self.invalidate_smoothing();
        }
    }

    pub fn set_size(&self, width: usize, height: usize) {
        {
            let mut params = self.params.write();
            params.size = (width, height);
        }
        self.init_size();
    }
    pub fn set_output_size(&self, width: usize, height: usize) -> bool {
        if width > 0 && height > 0 {
            let input_stretch = {
                let lens = self.lens.read();
                (lens.input_horizontal_stretch, lens.input_vertical_stretch)
            };
            let params = self.params.upgradable_read();
            let output_size = constrained_output_size(
                params.size,
                (width, height),
                params.video_rotation as f64,
                input_stretch,
            )
            .unwrap_or_default();

            if params.output_size != output_size {
                {
                    let mut params = RwLockUpgradableReadGuard::upgrade(params);
                    params.output_size = output_size;
                }
                self.init_size();

                return true;
            }
        }
        false
    }

    pub fn recompute_adaptive_zoom_static(
        compute_params: &ComputeParams,
        params: &RwLock<StabilizationParams>,
    ) -> (Vec<f64>, Vec<f64>, BTreeMap<i64, Vec<(f64, f64)>>) {
        let (frames, fps, method) = {
            let params = params.read();
            (
                params.frame_count,
                params.get_scaled_fps(),
                params.adaptive_zoom_method,
            )
        };
        let timestamps = (0..frames)
            .map(|i| (i, i as f64 * 1000.0 / fps))
            .collect::<Vec<(usize, f64)>>();

        zooming::calculate_fovs(compute_params, &timestamps, method.into())
    }
    pub fn recompute_adaptive_zoom(&self) {
        let mut params = stabilization::ComputeParams::from_manager(self);
        params.calculate_camera_fovs();

        let lens_fov_adjustment = params.lens.optimal_fov.unwrap_or(1.0);
        let (fovs, minimal_fovs, debug_points) =
            Self::recompute_adaptive_zoom_static(&params, &self.params);
        params.fovs = fovs;
        params.minimal_fovs = minimal_fovs;

        let (max_zoom_param, max_zoom_max, max_zoom_iters, scaling_factor) = {
            let mut stab_params = self.params.write();
            stab_params.set_fovs(params.fovs.clone(), lens_fov_adjustment);
            stab_params.minimal_fovs = params.minimal_fovs.clone();
            stab_params.zooming_debug_points = debug_points;
            (
                stab_params.max_zoom.unwrap_or(0.0),
                params
                    .keyframes
                    .get_keyframes(&KeyframeType::MaxZoom)
                    .map(|x| {
                        x.iter()
                            .map(|x| x.1.value)
                            .max_by(|a, b| a.total_cmp(b))
                            .unwrap_or(stab_params.max_zoom.unwrap_or(0.0))
                    })
                    .unwrap_or(stab_params.max_zoom.unwrap_or(0.0)),
                stab_params.max_zoom_iterations,
                stab_params.size.0 as f64 / stab_params.output_size.0 as f64,
            )
        };

        // Max zoom
        if max_zoom_max > 50.0 && max_zoom_iters > 0 {
            params.smoothing_fov_limit_per_frame.clear();
            for _ in params.fovs.iter() {
                params.smoothing_fov_limit_per_frame.push(1.0);
            }
            let thresholds = [0.95, 0.9, 0.85, 0.8];
            for iter in 0..max_zoom_iters {
                let mut any_above_limit = false;
                for (i, fov) in params.fovs.iter().enumerate() {
                    let ts = crate::timestamp_at_frame(i as i32, params.scaled_fps);
                    let mut zoom_limit = params
                        .keyframes
                        .value_at_video_timestamp(&KeyframeType::MaxZoom, ts)
                        .unwrap_or(max_zoom_param)
                        / 100.0;

                    if params.video_speed_affects_zooming_limit
                        && (params.video_speed != 1.0
                            || params.keyframes.is_keyframed(&KeyframeType::VideoSpeed))
                    {
                        let vid_speed = params
                            .keyframes
                            .value_at_video_timestamp(&KeyframeType::VideoSpeed, ts)
                            .unwrap_or(params.video_speed)
                            .abs();
                        zoom_limit *= (1.0 + ((vid_speed - 1.0) / 4.0)).min(1.8);
                    }

                    let fov_limit = 1.0 / (zoom_limit * scaling_factor);
                    if *fov < fov_limit {
                        any_above_limit = true;
                        params.smoothing_fov_limit_per_frame[i] *= (*fov / fov_limit)
                            .min(*thresholds.get(iter).unwrap_or(thresholds.last().unwrap()));
                    }
                }
                log::debug!(
                    "Max zoom iteration {iter}/{max_zoom_iters}, any above limit: {any_above_limit}"
                );
                if !any_above_limit {
                    if iter == 0 {
                        params.smoothing_fov_limit_per_frame.clear();
                    }
                    break;
                }

                // Smoothing
                {
                    let smoothing = self.smoothing.read();
                    let horizon_lock = smoothing.horizon_lock.clone();

                    let (quats, max_angles) = self.gyro.read().recompute_smoothness(
                        smoothing.current().as_ref(),
                        horizon_lock,
                        &params,
                    );
                    let mut gyro = self.gyro.write();
                    gyro.max_angles = max_angles;
                    gyro.smoothed_quaternions = quats;
                }

                // Zooming
                let lens_fov_adjustment = params.lens.optimal_fov.unwrap_or(1.0);
                let (fovs, minimal_fovs, debug_points) =
                    Self::recompute_adaptive_zoom_static(&params, &self.params);
                params.fovs = fovs;
                params.minimal_fovs = minimal_fovs;
                {
                    let mut stab_params = self.params.write();
                    stab_params.set_fovs(params.fovs.clone(), lens_fov_adjustment);
                    stab_params.minimal_fovs = params.minimal_fovs.clone();
                    stab_params.zooming_debug_points = debug_points;
                }
            }
        }

        // Smoothing diagnostics dump (gated by GYROFLOW_SMOOTH_DIAG=1).
        // Snapshot per-frame q_raw / q_smooth / fov pairs and forward to the
        // diag module after dropping all read locks so heavy quaternion math
        // does not block writers.
        if crate::smooth_diag::is_enabled() {
            let gyro = self.gyro.read();
            let stab_params = self.params.read();
            let smoothing = self.smoothing.read();

            let frame_count = stab_params.frame_count;
            let fps = stab_params.get_scaled_fps();

            let mut ts_ms = Vec::with_capacity(frame_count);
            let mut q_raw_v = Vec::with_capacity(frame_count);
            let mut q_smooth_v = Vec::with_capacity(frame_count);
            let mut fovs_pairs = Vec::with_capacity(frame_count);

            // Use interpolated lookup helpers that handle gyro-vs-video time offset
            // and interpolate between adjacent gyro samples — exact key match on the
            // raw BTreeMap would almost always miss (gyro at ~200 Hz vs video frame ts).
            //
            // IMPORTANT: gyro.smoothed_quaternions stores the correction rotation
            // q_smooth.inverse() * q_raw (see gyro_source/mod.rs:1683-1688), NOT the
            // absolute smoothed pose. Reconstruct the absolute q_smooth so that the
            // dump's delta = q_raw.inverse() * q_smooth_abs reflects the actual
            // virtual-camera deviation rather than q_raw's own absolute angle.
            for i in 0..frame_count {
                let ts_video_ms = i as f64 * 1000.0 / fps;
                let qr = gyro.org_quat_at_timestamp(ts_video_ms);
                let correction = gyro.smoothed_quat_at_timestamp(ts_video_ms);
                let qs = qr * correction.inverse();
                ts_ms.push(ts_video_ms);
                q_raw_v.push((qr.w, qr.i, qr.j, qr.k));
                q_smooth_v.push((qs.w, qs.i, qs.j, qs.k));
                let fov_final = stab_params.fovs.get(i).copied().unwrap_or(1.0);
                let fov_baseline = stab_params.minimal_fovs.get(i).copied().unwrap_or(fov_final);
                fovs_pairs.push((fov_baseline, fov_final));
            }

            let smoothing_meta = crate::smooth_diag::SmoothingMeta {
                method: smoothing.current().get_name(),
                method_id: smoothing.current_id(),
                params_json: smoothing.current().get_parameters_json(),
                adaptive_zoom_window: stab_params.adaptive_zoom_window,
                zoom_method: match stab_params.adaptive_zoom_method {
                    0 => "GaussianFilter".into(),
                    1 => "EnvelopeFollower".into(),
                    n => format!("Unknown({n})"),
                },
                max_zoom_pct: stab_params.max_zoom.unwrap_or(0.0),
                max_zoom_iterations: stab_params.max_zoom_iterations,
            };

            let url = self.input_file.read().url.clone();
            let path_basename = std::path::Path::new(&url)
                .file_name()
                .and_then(|n| n.to_str().map(String::from))
                .unwrap_or_default();

            let video_meta = crate::smooth_diag::VideoMeta {
                path_basename,
                duration_ms: stab_params.get_scaled_duration_ms(),
                frame_count,
                fps,
                width: stab_params.size.0,
                height: stab_params.size.1,
                gyro_sample_rate_hz: 0.0, // optional — reader can derive from quaternions count
            };

            drop(gyro);
            drop(stab_params);
            drop(smoothing);

            crate::smooth_diag::record_session(
                &ts_ms,
                &q_raw_v,
                &q_smooth_v,
                &fovs_pairs,
                &smoothing_meta,
                &video_meta,
            );
        }
    }

    pub fn recompute_smoothness(&self) {
        let mut params = stabilization::ComputeParams::from_manager(self);
        params.calculate_camera_fovs();

        let smoothing = self.smoothing.read();
        let horizon_lock = smoothing.horizon_lock.clone();

        let (quats, max_angles) = self.gyro.read().recompute_smoothness(
            smoothing.current().as_ref(),
            horizon_lock,
            &params,
        );
        let mut gyro = self.gyro.write();
        gyro.max_angles = max_angles;
        gyro.smoothed_quaternions = quats;
    }

    pub fn recompute_undistortion(&self) {
        let params = stabilization::ComputeParams::from_manager(self);
        self.stabilization.write().set_compute_params(params);
    }

    pub fn recompute_blocking(&self) {
        crate::smooth_diag::init_session();
        self.recompute_smoothness();
        self.recompute_adaptive_zoom();
        self.recompute_undistortion();
        crate::smooth_diag::flush_and_close();
    }

    pub fn invalidate_ongoing_computations(&self) {
        self.current_compute_id.store(fastrand::u64(..), SeqCst);
    }

    pub fn recompute_threaded<F: Fn((u64, bool)) + Send + Sync + Clone + 'static>(
        &self,
        cb: F,
    ) -> u64 {
        //self.recompute_smoothness();
        //self.recompute_adaptive_zoom();
        let mut params = stabilization::ComputeParams::from_manager(self);
        params.calculate_camera_fovs();

        let smoothing = self.smoothing.clone();
        let stabilization_params = self.params.clone();
        let gyro = self.gyro.clone();

        let compute_id = fastrand::u64(..);
        self.current_compute_id.store(compute_id, SeqCst);

        let mut gyro_checksum = gyro.read().get_checksum();

        let prevent_recompute = self.prevent_recompute.clone();
        let current_compute_id = self.current_compute_id.clone();
        let smoothing_checksum = self.smoothing_checksum.clone();
        let zooming_checksum = self.zooming_checksum.clone();

        let stabilization = self.stabilization.clone();
        THREAD_POOL.spawn(move || {
            // std::thread::sleep(std::time::Duration::from_millis(20));
            if prevent_recompute.load(SeqCst) { return cb((compute_id, true)); } // we're still loading, don't recompute
            if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }

            let mut smoothing_changed = false;
            if smoothing.read().get_state_checksum(gyro_checksum) != smoothing_checksum.load(SeqCst) {
                let (mut smoothing, horizon_lock) = {
                    let lock = smoothing.read();
                    (lock.current().clone(), lock.horizon_lock.clone())
                };

                let (quats, max_angles) = gyro.read().recompute_smoothness(smoothing.as_mut(), horizon_lock, &params);

                if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }
                if gyro_checksum != gyro.read().get_checksum() { return cb((compute_id, true)); }

                let mut lib_gyro = gyro.write();
                lib_gyro.max_angles = max_angles;
                lib_gyro.smoothed_quaternions = quats;
                lib_gyro.smoothing_status = smoothing.get_status_json();
                gyro_checksum = lib_gyro.get_checksum();
                smoothing_changed = true;
            }
            smoothing_checksum.store(smoothing.read().get_state_checksum(gyro_checksum), SeqCst);

            if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }

            if smoothing_changed || zooming::get_checksum(&params) != zooming_checksum.load(SeqCst) {
                let (fovs, minimal_fovs, debug_points) = Self::recompute_adaptive_zoom_static(&params, &stabilization_params);
                params.fovs = fovs;
                params.minimal_fovs = minimal_fovs;

                if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }

                let (max_zoom_param, max_zoom_max, max_zoom_iters, scaling_factor) = {
                    let mut stab_params = stabilization_params.write();
                    stab_params.set_fovs(params.fovs.clone(), params.lens.optimal_fov.unwrap_or(1.0));
                    stab_params.minimal_fovs = params.minimal_fovs.clone();
                    stab_params.zooming_debug_points = debug_points;
                    zooming_checksum.store(zooming::get_checksum(&params), SeqCst);
                    (
                        stab_params.max_zoom.unwrap_or(0.0),
                        params.keyframes.get_keyframes(&KeyframeType::MaxZoom).map(|x| x.iter().map(|x| x.1.value).max_by(|a, b| a.total_cmp(b)).unwrap_or(stab_params.max_zoom.unwrap_or(0.0))).unwrap_or(stab_params.max_zoom.unwrap_or(0.0)),
                        stab_params.max_zoom_iterations,
                        stab_params.size.0 as f64 / stab_params.output_size.0 as f64
                    )
                };

                // Max zoom
                if max_zoom_max > 50.0 && max_zoom_iters > 0 {
                    params.smoothing_fov_limit_per_frame.clear();
                    for _ in params.fovs.iter() {
                        params.smoothing_fov_limit_per_frame.push(1.0);
                    }
                    let thresholds = [0.95, 0.9, 0.85, 0.8];
                    for iter in 0..max_zoom_iters {
                        let mut any_above_limit = false;
                        for (i, fov) in params.fovs.iter().enumerate() {
                            let ts = crate::timestamp_at_frame(i as i32, params.scaled_fps);
                            let mut zoom_limit = params.keyframes.value_at_video_timestamp(&KeyframeType::MaxZoom, ts).unwrap_or(max_zoom_param) / 100.0;

                            if params.video_speed_affects_zooming_limit && (params.video_speed != 1.0 || params.keyframes.is_keyframed(&KeyframeType::VideoSpeed)) {
                                let vid_speed = params.keyframes.value_at_video_timestamp(&KeyframeType::VideoSpeed, ts).unwrap_or(params.video_speed).abs();
                                zoom_limit *= (1.0 + ((vid_speed - 1.0) / 4.0)).min(1.8);
                            }

                            let fov_limit = 1.0 / (zoom_limit * scaling_factor);
                            if *fov < fov_limit {
                                any_above_limit = true;
                                params.smoothing_fov_limit_per_frame[i] *= (*fov / fov_limit).min(*thresholds.get(iter).unwrap_or(thresholds.last().unwrap()));
                            }
                        }
                        log::debug!("Max zoom iteration {iter}/{max_zoom_iters}, any above limit: {any_above_limit}");
                        if !any_above_limit {
                            if iter == 0 {
                                params.smoothing_fov_limit_per_frame.clear();
                            }
                            break;
                        }

                        if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }

                        // Smoothing
                        let (mut smoothing, horizon_lock) = {
                            let lock = smoothing.read();
                            (lock.current().clone(), lock.horizon_lock.clone())
                        };
                        let (quats, max_angles) = gyro.read().recompute_smoothness(smoothing.as_mut(), horizon_lock, &params);

                        if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }

                        {
                            let mut lib_gyro = gyro.write();
                            lib_gyro.max_angles = max_angles;
                            lib_gyro.smoothed_quaternions = quats;
                            lib_gyro.smoothing_status = smoothing.get_status_json();
                        }

                        // Zooming
                        let (fovs, minimal_fovs, debug_points) = Self::recompute_adaptive_zoom_static(&params, &stabilization_params);
                        params.fovs = fovs;
                        params.minimal_fovs = minimal_fovs;

                        if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }

                        {
                            let mut stab_params = stabilization_params.write();
                            stab_params.set_fovs(params.fovs.clone(), params.lens.optimal_fov.unwrap_or(1.0));
                            stab_params.minimal_fovs = params.minimal_fovs.clone();
                            stab_params.zooming_debug_points = debug_points;
                            zooming_checksum.store(zooming::get_checksum(&params), SeqCst);
                        }
                    }
                }
            }

            if current_compute_id.load(SeqCst) != compute_id { return cb((compute_id, true)); }

            stabilization.write().set_compute_params(params);

            cb((compute_id, false));
        });
        compute_id
    }

    pub fn get_features_pixels(
        &self,
        timestamp_us: i64,
        size: (usize, usize),
    ) -> Option<Vec<(i32, i32)>> {
        // (x, y, alpha)
        let mut ret = None;
        use crate::util::MapClosest;
        use synchronization::OpticalFlowTrait;

        if let Some(l) = self.pose_estimator.sync_results.try_read() {
            if let Some(entry) = l.get_closest(&timestamp_us, 2000) {
                // closest within 2ms
                let ratio = size.1 as f32 / entry.frame_size.1.max(1) as f32;
                for pt in entry.of_method.features() {
                    if ret.is_none() {
                        // Only allocate if we actually have any points
                        ret = Some(Vec::with_capacity(2048));
                    }
                    ret.as_mut()
                        .unwrap()
                        .push(((pt.0 * ratio) as i32, (pt.1 * ratio) as i32));
                }
            }
        }
        ret
    }
    pub fn get_opticalflow_pixels(
        &self,
        timestamp_us: i64,
        num_frames: usize,
        size: (usize, usize),
    ) -> Option<Vec<(i32, i32, usize)>> {
        // (x, y, alpha)
        let mut ret = None;
        for i in 0..num_frames {
            match self
                .pose_estimator
                .get_of_lines_for_timestamp(&timestamp_us, i, 1.0, 1, false)
            {
                (Some(lines), Some(frame_size)) => {
                    let ratio = size.1 as f32 / frame_size.1.max(1) as f32;
                    lines
                        .0
                        .1
                        .into_iter()
                        .zip(lines.1.1.into_iter())
                        .for_each(|(p1, p2)| {
                            if ret.is_none() {
                                // Only allocate if we actually have any points
                                ret = Some(Vec::with_capacity(2048));
                            }
                            let line = line_drawing::Bresenham::new(
                                ((p1.0 * ratio) as isize, (p1.1 * ratio) as isize),
                                ((p2.0 * ratio) as isize, (p2.1 * ratio) as isize),
                            );
                            for point in line {
                                ret.as_mut()
                                    .unwrap()
                                    .push((point.0 as i32, point.1 as i32, i));
                            }
                        });
                }
                _ => {}
            }
        }
        ret
    }

    pub fn draw_overlays(&self, drawing: &mut DrawCanvas, timestamp_us: i64) {
        drawing.clear();

        if let Some(p) = self.params.try_read() {
            let y_inverted = p.framebuffer_inverted;
            let size = p.size;
            let frame =
                frame_at_timestamp(timestamp_us as f64 / 1000.0, p.get_scaled_fps()) as usize; // used only to draw features and OF

            if p.show_optical_flow {
                let num_frames = if p.of_method == 2 || p.of_method == 3 || p.of_method == 4 {
                    1
                } else {
                    3
                };
                if let Some(pxs) = self.get_opticalflow_pixels(timestamp_us, num_frames, size) {
                    for (x, y, a) in pxs {
                        let a = Alpha::from(a as u8);
                        drawing.put_pixel(x, y, Color::Yellow, a, Stage::OnInput, y_inverted, 1);
                    }
                }
            }
            if p.show_detected_features {
                if let Some(pxs) = self.get_features_pixels(timestamp_us, size) {
                    for (x, y) in pxs {
                        drawing.put_pixel(
                            x,
                            y,
                            Color::Green,
                            Alpha::Alpha100,
                            Stage::OnInput,
                            y_inverted,
                            3,
                        );
                    }
                }
            }
            #[cfg(feature = "opencv")]
            if p.is_calibrator {
                let lock = self.lens_calibrator.read();
                if let Some(ref cal) = *lock {
                    let points = cal.all_matches.read();
                    if let Some(entry) = points.get(&(frame as i32)) {
                        calibration::drawing::draw_chessboard_corners(
                            cal.width,
                            cal.height,
                            p.size.0,
                            p.size.1,
                            drawing,
                            (cal.columns, cal.rows),
                            &entry.points,
                            true,
                            y_inverted,
                        );
                    }
                }
            }
            if !p.zooming_debug_points.is_empty() {
                if let Some((_, points)) =
                    p.zooming_debug_points.range(timestamp_us - 1000..).next()
                {
                    for i in 0..points.len() {
                        let mut fov = ((p.fov + if p.fov_overview { 1.0 } else { 0.0 })
                            * p.fovs.get(frame).unwrap_or(&1.0))
                        .max(0.0001);
                        fov *= p.size.0 as f64 / p.output_size.0.max(1) as f64;
                        let mut pt = points[i];
                        let width_ratio = p.size.0 as f64 / p.output_size.0 as f64;
                        let height_ratio = p.size.1 as f64 / p.output_size.1 as f64;
                        pt = (pt.0 - 0.5, pt.1 - 0.5);
                        pt = (pt.0 / fov * width_ratio, pt.1 / fov * height_ratio);
                        pt = (pt.0 + 0.5, pt.1 + 0.5);
                        if pt.0 >= 0.0 && pt.1 >= 0.0 {
                            drawing.put_pixel(
                                (pt.0 * p.output_size.0 as f64) as i32,
                                (pt.1 * p.output_size.1 as f64) as i32,
                                Color::Red,
                                Alpha::Alpha100,
                                Stage::OnOutput,
                                y_inverted,
                                4,
                            );
                        }
                    }
                }
            }
        }
    }

    pub fn process_pixels<T: PixelType>(
        &self,
        mut timestamp_us: i64,
        frame: Option<usize>,
        buffers: &mut Buffers,
    ) -> Result<stabilization::ProcessedInfo, GyroflowCoreError> {
        if let gpu::BufferSource::Cpu { buffer } = &buffers.input.data {
            if buffer.is_empty() {
                return Err(GyroflowCoreError::InputBufferEmpty);
            }
        }
        if let gpu::BufferSource::Cpu { buffer } = &buffers.output.data {
            if buffer.is_empty() {
                return Err(GyroflowCoreError::OutputBufferEmpty);
            }
        }

        let (offset, fps) = {
            let params = self.params.read();
            (params.frame_offset, params.fps)
        };
        let frame = frame.map(|x| (x as i32 + offset).max(0) as usize);
        timestamp_us += (offset as f64 / fps * 1000000.0).round() as i64;

        if let Some(scale) = self.params.read().fps_scale {
            timestamp_us = (timestamp_us as f64 / scale).round() as i64;
        }

        if self.smoothing_invalidated.load(SeqCst) {
            self.recompute_smoothness();
            self.smoothing_invalidated.store(false, SeqCst);
        }
        if self.zooming_invalidated.load(SeqCst) {
            self.recompute_adaptive_zoom();
            self.zooming_invalidated.store(false, SeqCst);
        }
        if self.undistortion_invalidated.load(SeqCst) {
            self.recompute_undistortion();
            self.undistortion_invalidated.store(false, SeqCst);
        }

        let (use_cache, hash, current_hash) = {
            let stab = self.stabilization.read();
            (
                stab.cache_frame_transform,
                stab.get_current_checksum(buffers),
                stab.initialized_backend.get_hash(),
            )
        };

        if use_cache || hash != current_hash {
            if let Some(mut undist) = self
                .stabilization
                .try_write_for(std::time::Duration::from_millis(30000))
            {
                self.draw_overlays(&mut undist.drawing, timestamp_us);
                undist.ensure_ready_for_processing::<T>(timestamp_us, frame, buffers);
            } else {
                return Err(GyroflowCoreError::Unknown);
            }
        }

        if let Some(undist) = self
            .stabilization
            .try_read_for(std::time::Duration::from_millis(30000))
        {
            undist.process_pixels::<T>(timestamp_us, frame, buffers, None)
        } else {
            Err(GyroflowCoreError::Unknown)
        }
    }

    pub fn set_video_rotation(&self, v: f64) {
        self.params.write().video_rotation = v;
        self.invalidate_smoothing();
    }

    pub fn trim_ranges(&self) -> Vec<(f64, f64)> {
        self.params.read().trim_ranges.clone()
    }
    pub fn set_trim_ranges(&self, v: Vec<(f64, f64)>) {
        self.params.write().trim_ranges = if v.first() == Some(&(0.0, 1.0)) {
            Vec::new()
        } else {
            v
        };
        self.invalidate_smoothing();
    }

    pub fn set_of_method(&self, v: u32) {
        self.params.write().of_method = v;
        self.pose_estimator.clear();
    }
    pub fn set_show_detected_features(&self, v: bool) {
        self.params.write().show_detected_features = v;
    }
    pub fn set_show_optical_flow(&self, v: bool) {
        self.params.write().show_optical_flow = v;
    }
    pub fn set_stab_enabled(&self, v: bool) {
        self.params.write().stab_enabled = v;
    }
    pub fn set_frame_readout_time(&self, v: f64) {
        self.params.write().frame_readout_time = v;
    }
    pub fn set_frame_readout_direction(&self, v: impl Into<ReadoutDirection>) {
        self.params.write().frame_readout_direction = v.into();
    }
    pub fn set_adaptive_zoom(&self, v: f64) {
        self.params.write().adaptive_zoom_window = v;
        self.invalidate_zooming();
    }
    pub fn set_zooming_center_x(&self, v: f64) {
        self.params.write().adaptive_zoom_center_offset.0 = v;
        self.invalidate_zooming();
    }
    pub fn set_zooming_center_y(&self, v: f64) {
        self.params.write().adaptive_zoom_center_offset.1 = v;
        self.invalidate_zooming();
    }
    pub fn set_additional_rotation_x(&self, v: f64) {
        self.params.write().additional_rotation.0 = v;
        self.invalidate_smoothing();
    }
    pub fn set_additional_rotation_y(&self, v: f64) {
        self.params.write().additional_rotation.1 = v;
        self.invalidate_smoothing();
    }
    pub fn set_additional_rotation_z(&self, v: f64) {
        self.params.write().additional_rotation.2 = v;
        self.invalidate_smoothing();
    }
    pub fn set_additional_translation_x(&self, v: f64) {
        self.params.write().additional_translation.0 = v;
        self.invalidate_zooming();
    }
    pub fn set_additional_translation_y(&self, v: f64) {
        self.params.write().additional_translation.1 = v;
        self.invalidate_zooming();
    }
    pub fn set_additional_translation_z(&self, v: f64) {
        self.params.write().additional_translation.2 = v;
        self.invalidate_zooming();
    }
    pub fn set_zooming_method(&self, v: i32) {
        self.params.write().adaptive_zoom_method = v;
        self.invalidate_zooming();
    }
    pub fn set_fov(&self, v: f64) {
        self.params.write().fov = v;
    }
    pub fn set_fov_overview(&self, v: bool) {
        self.params.write().fov_overview = v;
    }
    pub fn set_show_safe_area(&self, v: bool) {
        self.params.write().show_safe_area = v;
    }
    pub fn set_lens_correction_amount(&self, v: f64) {
        self.params.write().lens_correction_amount = v;
        self.invalidate_zooming();
    }
    pub fn set_frame_offset(&self, v: i32) {
        self.params.write().frame_offset = v;
    }
    pub fn set_light_refraction_coefficient(&self, v: f64) {
        self.params.write().light_refraction_coefficient = v;
        self.invalidate_zooming();
    }
    pub fn set_background_color(&self, bg: Vector4<f32>) {
        self.params.write().background = bg;
    }
    pub fn set_background_mode(&self, v: i32) {
        self.params.write().background_mode = stabilization_params::BackgroundMode::from(v);
    }
    pub fn set_background_margin(&self, v: f64) {
        self.params.write().background_margin = v;
    }
    pub fn set_background_margin_feather(&self, v: f64) {
        self.params.write().background_margin_feather = v;
    }
    pub fn set_input_horizontal_stretch(&self, v: f64) {
        self.lens.write().input_horizontal_stretch = v;
        self.invalidate_zooming();
    }
    pub fn set_input_vertical_stretch(&self, v: f64) {
        self.lens.write().input_vertical_stretch = v;
        self.invalidate_zooming();
    }
    pub fn set_max_zoom(&self, v: f64, iters: usize) {
        let mut params = self.params.write();
        params.max_zoom = if v > 50.0 { Some(v) } else { None };
        params.max_zoom_iterations = iters.max(1);
        self.invalidate_smoothing();
    }

    pub fn set_video_speed(
        &self,
        v: f64,
        link_with_smoothness: bool,
        link_with_zooming: bool,
        link_with_zooming_limit: bool,
    ) {
        let mut params = self.params.write();
        params.video_speed = v;
        params.video_speed_affects_smoothing = link_with_smoothness;
        params.video_speed_affects_zooming = link_with_zooming;
        params.video_speed_affects_zooming_limit = link_with_zooming_limit;
        self.invalidate_smoothing();
    }

    pub fn disable_lens_stretch(&self, adjust_size: bool) {
        let (x_stretch, y_stretch) = {
            let lens = self.lens.read();
            (lens.input_horizontal_stretch, lens.input_vertical_stretch)
        };
        if (x_stretch > 0.01 && x_stretch != 1.0) || (y_stretch > 0.01 && y_stretch != 1.0) {
            if adjust_size {
                let mut params = self.params.write();
                params.size.0 = (params.size.0 as f64 * x_stretch).round() as usize;
                params.size.1 = (params.size.1 as f64 * y_stretch).round() as usize;
            }
            {
                let mut lens = self.lens.write();
                lens.input_horizontal_stretch = 1.0;
                lens.input_vertical_stretch = 1.0;
            }
        }
    }

    pub fn get_scaling_ratio(&self) -> f64 {
        let params = self.params.read();
        params.size.0 as f64 / params.output_size.0 as f64
    }
    pub fn get_min_fov(&self) -> f64 {
        self.params.read().min_fov
    }

    pub fn invalidate_smoothing(&self) {
        self.invalidate_ongoing_computations();
        self.smoothing_checksum.store(0, SeqCst);
        self.invalidate_zooming();
    }
    pub fn invalidate_zooming(&self) {
        self.invalidate_ongoing_computations();
        self.zooming_checksum.store(0, SeqCst);
    }

    pub fn invalidate_blocking_smoothing(&self) {
        self.invalidate_ongoing_computations();
        self.smoothing_invalidated.store(true, SeqCst);
        self.zooming_invalidated.store(true, SeqCst);
        self.undistortion_invalidated.store(true, SeqCst);
    }
    pub fn invalidate_blocking_zooming(&self) {
        self.invalidate_ongoing_computations();
        self.zooming_invalidated.store(true, SeqCst);
        self.undistortion_invalidated.store(true, SeqCst);
    }
    pub fn invalidate_blocking_undistortion(&self) {
        self.invalidate_ongoing_computations();
        self.undistortion_invalidated.store(true, SeqCst);
    }

    pub fn set_digital_lens_name(&self, v: String) {
        self.lens.write().digital_lens = if !v.is_empty() { Some(v.clone()) } else { None };
        #[cfg(feature = "opencv")]
        if let Some(ref mut calib) = *self.lens_calibrator.write() {
            calib.digital_lens = if !v.is_empty() { Some(v) } else { None };
        }
        self.invalidate_zooming();
    }
    pub fn set_digital_lens_param(&self, index: usize, value: f64) {
        let mut lens = self.lens.write();
        if lens.digital_lens_params.is_none() {
            lens.digital_lens_params = Some(vec![0f64; 4]);
        }
        lens.digital_lens_params.as_mut().unwrap()[index] = value;
        #[cfg(feature = "opencv")]
        if let Some(ref mut calib) = *self.lens_calibrator.write() {
            calib.digital_lens_params = lens.digital_lens_params.clone();
        }
        self.invalidate_zooming();
    }
    pub fn set_lens_is_asymmetrical(&self, v: bool) {
        self.lens.write().asymmetrical = v;
        #[cfg(feature = "opencv")]
        if let Some(ref mut calib) = *self.lens_calibrator.write() {
            calib.asymmetrical = v;
        }
        self.invalidate_zooming();
    }

    pub fn remove_offset(&self, timestamp_us: i64) {
        self.gyro.write().remove_offset(timestamp_us);
        self.keyframes.write().update_gyro(&self.gyro.read());
        self.invalidate_zooming();
    }
    pub fn set_offset(&self, timestamp_us: i64, offset_ms: f64) {
        self.gyro.write().set_offset(timestamp_us, offset_ms);
        self.keyframes.write().update_gyro(&self.gyro.read());
        self.invalidate_zooming();
    }
    pub fn clear_offsets(&self) {
        self.gyro.write().clear_offsets();
        self.keyframes.write().update_gyro(&self.gyro.read());
        self.invalidate_zooming();
    }
    pub fn offset_at_video_timestamp(&self, timestamp_us: i64) -> f64 {
        self.gyro
            .read()
            .offset_at_video_timestamp(timestamp_us as f64 / 1000.0)
    }

    pub fn set_imu_lpf(&self, lpf: f64) {
        self.gyro.write().imu_transforms.imu_lpf = lpf;
    }
    pub fn set_imu_median_filter(&self, size: i32) {
        self.gyro.write().imu_transforms.imu_mf = size;
    }
    pub fn set_imu_rotation(&self, pitch_deg: f64, roll_deg: f64, yaw_deg: f64) {
        self.gyro
            .write()
            .imu_transforms
            .set_imu_rotation(pitch_deg, roll_deg, yaw_deg);
    }
    pub fn set_acc_rotation(&self, pitch_deg: f64, roll_deg: f64, yaw_deg: f64) {
        self.gyro
            .write()
            .imu_transforms
            .set_acc_rotation(pitch_deg, roll_deg, yaw_deg);
    }
    pub fn set_imu_orientation(&self, orientation: String) {
        self.gyro.write().imu_transforms.imu_orientation = Some(orientation);
    }
    pub fn set_imu_bias(&self, bx: f64, by: f64, bz: f64) {
        self.gyro.write().imu_transforms.gyro_bias = Some([bx, by, bz]);
    }
    pub fn recompute_gyro(&self) {
        self.gyro.write().apply_transforms();
        self.invalidate_smoothing();
    }
    pub fn set_sync_lpf(&self, lpf: f64) {
        let params = self.params.read();
        self.pose_estimator.lowpass_filter(lpf, params.fps);
    }

    pub fn set_lens_param(&self, param: &str, value: f64) {
        let mut lens = self.lens.write();
        if lens.fisheye_params.distortion_coeffs.len() >= 4
            && lens.fisheye_params.camera_matrix.len() == 3
            && lens.fisheye_params.camera_matrix[0].len() == 3
            && lens.fisheye_params.camera_matrix[1].len() == 3
            && lens.fisheye_params.camera_matrix[2].len() == 3
        {
            match param {
                "fx" => lens.fisheye_params.camera_matrix[0][0] = value,
                "fy" => lens.fisheye_params.camera_matrix[1][1] = value,
                "cx" => lens.fisheye_params.camera_matrix[0][2] = value,
                "cy" => lens.fisheye_params.camera_matrix[1][2] = value,
                "k1" => lens.fisheye_params.distortion_coeffs[0] = value,
                "k2" => lens.fisheye_params.distortion_coeffs[1] = value,
                "k3" => lens.fisheye_params.distortion_coeffs[2] = value,
                "k4" => lens.fisheye_params.distortion_coeffs[3] = value,
                _ => {}
            }
        }
        self.invalidate_smoothing();
    }

    pub fn set_user_focal_length(&self, focal_length_mm: f64) {
        let (w, h, frame_count, fps) = {
            let p = self.params.read();
            (p.size.0, p.size.1, p.frame_count, p.fps)
        };
        let gyro = self.gyro.read();
        let mut md = gyro.file_metadata.0.write();
        let lens_upfl = md.unit_pixel_focal_length;
        if let Some(upfl) = lens_upfl {
            let pfl = (focal_length_mm * upfl) as f32;

            if md.lens_params.is_empty() {
                // Create lens_params entries for all frames
                for i in 0..frame_count {
                    let timestamp_us = (i as f64 * 1000000.0 / fps).round() as i64;
                    md.lens_params.insert(
                        timestamp_us,
                        gyro_source::LensParams {
                            pixel_focal_length: Some(pfl),
                            ..Default::default()
                        },
                    );
                }
            } else {
                for (_ts, params) in md.lens_params.iter_mut() {
                    params.pixel_focal_length = Some(pfl);
                }
            }
        }
        let metadata_snapshot_after = md.thin();
        drop(md);
        drop(gyro);

        // Update the synthetic lens profile
        let mut l = self.lens.write();
        populate_lens_metadata_fields(&mut l, &metadata_snapshot_after, (w, h));
        l.focal_length = Some(focal_length_mm);
        if let Some(upfl) = lens_upfl {
            let pfl = focal_length_mm * upfl;
            l.fisheye_params.camera_matrix = vec![
                [pfl, 0.0, w as f64 / 2.0],
                [0.0, pfl, h as f64 / 2.0],
                [0.0, 0.0, 1.0],
            ];
        }
        l.calib_dimension = crate::lens_profile::Dimensions { w, h };
        l.orig_dimension = crate::lens_profile::Dimensions { w, h };
        drop(l);
        self.invalidate_smoothing();
    }

    pub fn set_lens_group_config_json(&self, json: &str) {
        let configs = niyien_lens_presets::lens_group_configs_from_json(json);
        // Persist normalized JSON (not the raw input — raw input may be stale or malformed).
        let normalized = niyien_lens_presets::lens_group_config_to_json(&configs);
        *self.lens_group_config.write() = configs;
        settings::set(
            "lens_group_configs_v1",
            serde_json::Value::String(normalized),
        );
    }

    pub fn set_lens_group_manual_edit(&self, enabled: bool) {
        self.lens_group_manual_edit.store(enabled, SeqCst);
        settings::set("lens_group_manual_edit", serde_json::Value::Bool(enabled));
    }

    pub fn get_lens_group_manual_edit(&self) -> bool {
        self.lens_group_manual_edit.load(SeqCst)
    }

    fn lens_group_baseline_for_build(&self, applies_anamorphic: bool) -> LensProfile {
        let current = self.lens.read().clone();
        let mut backup = self.pre_anamorphic_backup.write();
        if applies_anamorphic {
            if backup.is_none() {
                *backup = Some(current.clone());
            }
            backup.clone().unwrap_or(current)
        } else {
            backup.take().unwrap_or(current)
        }
    }

    fn lens_group_preview_baseline_for_build(&self) -> LensProfile {
        self.pre_anamorphic_backup
            .read()
            .clone()
            .unwrap_or_else(|| self.lens.read().clone())
    }

    /// Apply one lens group config (focal length + anamorphic squeeze) to the main
    /// stabilizer so the live canvas preview reflects the edit. This mirrors the
    /// per-job reapply path in render_queue.rs (reapply_lens_group_config) but
    /// targets the main `self.lens` instead of a queue job's stab.
    ///
    /// Returns the new output dimension if one was pushed (so the UI can sync the
    /// Export settings' output width/height NumberFields).
    pub fn apply_lens_group_to_main(&self, lens_index: usize) -> Option<(usize, usize)> {
        let cfg = {
            let all = self.lens_group_config.read();
            all.get(lens_index).cloned()
        };
        let cfg = cfg?;

        self.apply_lens_group_config_to_main(lens_index, &cfg)
    }

    pub fn apply_lens_group_config_json_to_main(
        &self,
        json: &str,
        lens_index: usize,
    ) -> Option<(usize, usize)> {
        let configs = niyien_lens_presets::lens_group_configs_from_json(json);
        let cfg = configs.get(lens_index)?;
        self.apply_lens_group_config_to_main(lens_index, cfg)
    }

    fn apply_lens_group_config_to_main(
        &self,
        _lens_index: usize,
        cfg: &LensGroupConfig,
    ) -> Option<(usize, usize)> {
        let manual_edit = self.lens_group_manual_edit.load(SeqCst);
        let (metadata_snapshot, size) = {
            let gyro = self.gyro.read();
            let md = gyro.file_metadata.read().clone();
            let p = self.params.read();
            (md, (p.size.0, p.size.1))
        };
        let cfg_for_build = niyien_lens_presets::effective_lens_group_config_for_build(
            manual_edit,
            cfg,
            &metadata_snapshot,
        );
        let applies_anamorphic = cfg_for_build
            .as_ref()
            .map(|cfg| cfg.anamorphic_enabled)
            .unwrap_or(false);

        // 1. Resolve the baseline lens that build_lens_profile should use as fallback:
        //    - Entering anamorphic for the first time: snapshot current lens → backup,
        //      baseline = that snapshot. Subsequent anamorphic edits also start from it
        //      so different presets don't stack on top of each other.
        //    - Exiting anamorphic (cfg.anamorphic_enabled=false and a backup exists):
        //      baseline = the backup, and clear the backup. This ensures fx/fy/cx/cy +
        //      distortion_coeffs + distortion_model all revert to their pre-anamorphic
        //      values rather than keeping the previous preset's distortion parameters.
        //    - No anamorphic state ever (cfg=false, backup=None): baseline = current.
        let baseline = self.lens_group_baseline_for_build(applies_anamorphic);

        // 2. Rebuild the lens profile from baseline + cfg.
        //    build_lens_profile already has the correct distortion semantics:
        //    - anamorphic preset WITH distortion_coeffs → use preset's
        //    - anamorphic preset WITHOUT distortion_coeffs → keep baseline's
        //    - no anamorphic (cfg.anamorphic_enabled=false) → keep baseline's
        //    (baseline = the lens captured before the first anamorphic edit, restored
        //    from pre_anamorphic_backup when exiting anamorphic.)
        if let Some(profile) = niyien_lens_presets::build_lens_profile(
            &metadata_snapshot,
            size,
            cfg_for_build.as_ref(),
            Some(&baseline),
        ) {
            let out_dim = profile.output_dimension.clone();
            *self.lens.write() = profile;
            self.invalidate_smoothing();

            // Per-group lens correction: anamorphic ON uses the slider value (default 100),
            // anamorphic OFF always reverts to 100. This matches the user expectation that
            // turning off anamorphic fully clears the group-specific correction override.
            let correction_percent =
                niyien_lens_presets::effective_lens_correction_amount_percent(
                    &cfg,
                    applies_anamorphic,
                );
            self.set_lens_correction_amount(correction_percent / 100.0);

            if let Some(od) = out_dim {
                self.set_output_size(od.w, od.h);
                return Some((od.w, od.h));
            } else {
                // Revert to the source video's dimensions when the config has no
                // anamorphic output dim.
                self.set_output_size(size.0, size.1);
                return Some((size.0, size.1));
            }
        }
        None
    }
    pub fn preview_lens_group_config_json(&self, json: &str, lens_index: usize) -> Option<String> {
        let configs = niyien_lens_presets::lens_group_configs_from_json(json);
        let Some(cfg) = configs.get(lens_index) else {
            return None;
        };

        let manual_edit = self.lens_group_manual_edit.load(SeqCst);
        let (metadata_snapshot, size) = {
            let gyro = self.gyro.read();
            let md = gyro.file_metadata.read();
            let metadata_snapshot = md.clone();
            drop(md);
            drop(gyro);

            let p = self.params.read();
            (metadata_snapshot, (p.size.0, p.size.1))
        };
        let cfg_for_build = niyien_lens_presets::effective_lens_group_config_for_build(
            manual_edit,
            cfg,
            &metadata_snapshot,
        );

        let baseline = self.lens_group_preview_baseline_for_build();
        niyien_lens_presets::build_lens_profile(
            &metadata_snapshot,
            size,
            cfg_for_build.as_ref(),
            Some(&baseline),
        )
        .and_then(|profile| profile.get_json().ok())
    }
    pub fn get_lens_group_config_json(&self) -> String {
        niyien_lens_presets::lens_group_config_to_json(&self.lens_group_config.read())
    }
    pub fn clear_lens_group_config(&self) {
        *self.lens_group_config.write() = niyien_lens_presets::default_lens_group_configs();
    }
    pub fn set_lens_group_status(&self, statuses: Vec<LensGroupStatus>) {
        *self.lens_group_status.write() = statuses;
    }
    pub fn clear_lens_group_status(&self) {
        *self.lens_group_status.write() = niyien_lens_presets::default_lens_group_statuses();
    }
    pub fn get_lens_group_status_json(&self) -> String {
        niyien_lens_presets::lens_group_status_to_json(&self.lens_group_status.read())
    }
    pub fn get_lens_presets_json(&self) -> String {
        niyien_lens_presets::load_presets_json()
    }

    pub fn set_gpu_decoding(&self, v: bool) {
        self.gpu_decoding.store(v, SeqCst);
    }
    pub fn set_smoothing_method(&self, index: usize) -> serde_json::Value {
        let mut smooth = self.smoothing.write();
        smooth.set_current(index);

        self.invalidate_smoothing();

        smooth.current().get_parameters_json()
    }
    pub fn set_smoothing_param(&self, name: &str, val: f64) {
        self.smoothing
            .write()
            .current_mut()
            .as_mut()
            .set_parameter(name, val);
        self.invalidate_smoothing();
    }
    pub fn set_horizon_lock(
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
    ) {
        self.smoothing.write().horizon_lock.set_horizon(
            lock_percent,
            roll,
            lock_pitch,
            pitch,
            automatic_lock,
            turn_threshold,
            turn_smoothing_ms,
            turn_multiplier,
            tilt_accel_limit,
        );
        self.invalidate_smoothing();
    }
    pub fn set_use_gravity_vectors(&self, v: bool) {
        self.gyro.write().set_use_gravity_vectors(v);
        self.invalidate_smoothing();
    }
    pub fn set_horizon_lock_integration_method(&self, v: i32) {
        self.gyro.write().set_horizon_lock_integration_method(v);
        self.invalidate_smoothing();
    }
    pub fn get_smoothing_max_angles(&self) -> (f64, f64, f64) {
        self.gyro.read().max_angles
    }
    pub fn get_smoothing_status(&self) -> serde_json::Value {
        self.gyro.read().smoothing_status.clone()
    }
    pub fn get_smoothing_algs(&self) -> Vec<String> {
        self.smoothing.read().get_names()
    }

    pub fn get_cloned(&self) -> StabilizationManager {
        StabilizationManager {
            params: Arc::new(RwLock::new(self.params.read().clone())),
            gyro: Arc::new(RwLock::new(self.gyro.read().clone())),
            lens: Arc::new(RwLock::new(self.lens.read().clone())),
            keyframes: Arc::new(RwLock::new(self.keyframes.read().clone())),
            smoothing: Arc::new(RwLock::new(self.smoothing.read().clone())),
            input_file: Arc::new(RwLock::new(self.input_file.read().clone())),
            lens_group_config: self.lens_group_config.clone(),
            lens_group_status: self.lens_group_status.clone(),
            lens_group_manual_edit: self.lens_group_manual_edit.clone(),
            lens_profile_db: self.lens_profile_db.clone(),

            // NOT cloned:
            // stabilization
            // pose_estimator
            // lens_calibrator
            // current_compute_id
            // smoothing_checksum
            // zooming_checksum
            // prevent_recompute
            // camera_id
            ..Default::default()
        }
    }
    pub fn set_render_params(&self, size: (usize, usize), output_size: (usize, usize)) {
        self.params.write().framebuffer_inverted = false;
        self.params.write().fov_overview = false;
        self.params.write().show_safe_area = false;
        self.stabilization
            .write()
            .kernel_flags
            .set(KernelParamsFlags::DRAWING_ENABLED, false);
        self.set_size(size.0, size.1);
        self.set_output_size(output_size.0, output_size.1);

        self.recompute_undistortion();
    }

    pub fn clear(&self) {
        self.params.write().clear();
        self.invalidate_ongoing_computations();
        self.invalidate_smoothing();
        *self.input_file.write() = InputFile::default();
        *self.camera_id.write() = None;

        *self.gyro.write() = GyroSource::new();
        self.keyframes.write().clear();

        // Drop the anamorphic baseline snapshot so a new project doesn't inherit the
        // previous video's distortion coefficients when the user toggles anamorphic on.
        *self.pre_anamorphic_backup.write() = None;

        self.pose_estimator.clear();
    }

    pub fn override_video_fps(&self, fps: f64, recompute: bool) {
        {
            let mut params = self.params.write();
            if !apply_effective_frame_rate(&mut params, fps) {
                return;
            }
            self.gyro.write().init_from_params(&params);
            self.keyframes.write().timestamp_scale = params.fps_scale;
        }

        if recompute {
            self.stabilization
                .write()
                .set_compute_params(stabilization::ComputeParams::from_manager(self));

            self.invalidate_smoothing();
        }
    }

    pub fn list_gpu_devices<F: Fn(Vec<String>) + Send + Sync + 'static>(&self, cb: F) {
        let stab = self.stabilization.clone();
        run_threaded(move || {
            let list = stab.read().list_devices();

            log::info!("GPU list: {:?}", &list);

            *stabilization::GPU_LIST.write() = list.clone();

            cb(list);
        });
    }

    pub fn export_gyroflow_file(
        &self,
        url: &str,
        typ: GyroflowProjectType,
        additional_data: &str,
    ) -> Result<(), GyroflowCoreError> {
        let data = self.export_gyroflow_data(typ, additional_data, Some(url))?;
        filesystem::write(url, data.as_bytes())?;

        self.input_file.write().project_file_url = Some(url.to_string());

        Ok(())
    }
    pub fn export_gyroflow_data(
        &self,
        typ: GyroflowProjectType,
        additional_data: &str,
        _project_url: Option<&str>,
    ) -> Result<String, GyroflowCoreError> {
        let gyro = self.gyro.read();
        let params = self.params.read();
        let record_frame_rate = gyro.file_metadata.read().record_frame_rate;

        let (smoothing_name, smoothing_params, horizon_amount, horizon_lock) = {
            let smoothing_lock = self.smoothing.read();
            let smoothing = smoothing_lock.current();

            let mut parameters = smoothing.get_parameters_json();
            if let serde_json::Value::Array(ref mut arr) = parameters {
                for v in arr.iter_mut() {
                    if let serde_json::Value::Object(obj) = v {
                        *v = serde_json::json!({
                            "name": obj["name"],
                            "value": obj["value"]
                        });
                    }
                }
            }
            let mut horizon_amount = smoothing_lock.horizon_lock.horizonlockpercent;
            if !smoothing_lock.horizon_lock.lock_enabled {
                horizon_amount = 0.0;
            }

            (
                smoothing.get_name(),
                parameters,
                horizon_amount,
                smoothing_lock.horizon_lock.clone(),
            )
        };

        let input_file = self.input_file.read().clone();

        let trim_ranges_ms = params
            .trim_ranges
            .iter()
            .map(|(a, b)| (a * params.duration_ms, b * params.duration_ms))
            .collect::<Vec<_>>();

        let mut obj = serde_json::json!({
            "title": "Gyroflow data file",
            "version": 4,
            "app_version": env!("CARGO_PKG_VERSION").to_string(),
            "videofile": input_file.url,
            "calibration_data": self.lens.read().get_json_value().unwrap_or_else(|_| serde_json::json!({})),
            "date": time::OffsetDateTime::now_local().map(|v| v.date().to_string()).unwrap_or_default(),

            "image_sequence_start": input_file.image_sequence_start,
            "image_sequence_fps": input_file.image_sequence_fps,
            "background_color": params.background.as_slice(),
            "background_mode":  params.background_mode as i32,
            "background_margin":          params.background_margin,
            "background_margin_feather":  params.background_margin_feather,
            "light_refraction_coefficient": params.light_refraction_coefficient,

            "video_info": {
                "width":       params.size.0,
                "height":      params.size.1,
                "rotation":    params.video_rotation,
                "num_frames":  params.frame_count,
                "fps":         params.fps,
                "duration_ms": params.duration_ms,
                "fps_scale":   params.fps_scale,
                "record_frame_rate": record_frame_rate,
                "vfr_fps":     params.get_scaled_fps(),
                "vfr_duration_ms": params.get_scaled_duration_ms(),
                "created_at"   : params.video_created_at,
                "timezone"     : params.video_timezone,
            },
            "stabilization": {
                "fov":                    params.fov,
                "method":                 smoothing_name,
                "smoothing_params":       smoothing_params,
                "frame_readout_time":     params.frame_readout_time.abs(),
                "frame_readout_direction": params.frame_readout_direction,
                "adaptive_zoom_window":   params.adaptive_zoom_window,
                "adaptive_zoom_center_offset": params.adaptive_zoom_center_offset,
                "adaptive_zoom_method":   params.adaptive_zoom_method,
                "additional_rotation":    params.additional_rotation,
                "additional_translation": params.additional_translation,
                "lens_correction_amount": params.lens_correction_amount,
                "horizon_lock_amount":    horizon_amount,
                "horizon_lock_roll":      horizon_lock.horizonroll,
                "horizon_lock_pitch_enabled": horizon_lock.lock_pitch,
                "horizon_lock_pitch":     horizon_lock.horizonpitch,
                "use_gravity_vectors":    gyro.use_gravity_vectors,
                "horizon_lock_integration_method": gyro.horizon_lock_integration_method,
                "video_speed":                   params.video_speed,
                "video_speed_affects_smoothing": params.video_speed_affects_smoothing,
                "video_speed_affects_zooming":   params.video_speed_affects_zooming,
                "video_speed_affects_zooming_limit": params.video_speed_affects_zooming_limit,
                "max_zoom":               params.max_zoom,
                "max_zoom_iterations":    params.max_zoom_iterations,
                "frame_offset":           params.frame_offset,
            },
            "gyro_source": {
                "filepath":           gyro.file_url,
                "lpf":                gyro.imu_transforms.imu_lpf,
                "mf":                 gyro.imu_transforms.imu_mf,
                "rotation":           gyro.imu_transforms.imu_rotation_angles,
                "acc_rotation":       gyro.imu_transforms.acc_rotation_angles,
                "imu_orientation":    gyro.imu_transforms.imu_orientation,
                "gyro_bias":          gyro.imu_transforms.gyro_bias,
                "integration_method": gyro.integration_method,
                "sample_index":       gyro.file_load_options.sample_index,
                "detected_source":    gyro.file_metadata.read().detected_source,
            },

            "offsets": gyro.get_offsets(), // timestamp, offset value
            "keyframes": self.keyframes.read().serialize(),

            // "trim_ranges": params.trim_ranges,
            "trim_ranges_ms": trim_ranges_ms,
        });

        util::merge_json(
            &mut obj,
            &serde_json::from_str(additional_data).unwrap_or_default(),
        );

        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let serde_json::Value::Object(ref mut obj) = obj {
            obj.insert(
                "videofile_bookmark".into(),
                serde_json::Value::String(filesystem::apple::create_bookmark(
                    &input_file.url,
                    false,
                    _project_url,
                )),
            );
            if let Some(serde_json::Value::Object(obj)) = obj.get_mut("gyro_source") {
                obj.insert(
                    "filepath_bookmark".into(),
                    serde_json::Value::String(filesystem::apple::create_bookmark(
                        &gyro.file_url,
                        false,
                        _project_url,
                    )),
                );
            }
            if let Some(serde_json::Value::Object(obj)) = obj.get_mut("output") {
                if let Some(output_folder) = obj
                    .get("output_folder")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty())
                {
                    obj.insert(
                        "output_folder_bookmark".into(),
                        serde_json::Value::String(filesystem::apple::create_bookmark(
                            output_folder,
                            true,
                            _project_url,
                        )),
                    );
                }
            }
        }

        if let Some(serde_json::Value::Object(obj)) = obj.get_mut("gyro_source") {
            let file_metadata = gyro.file_metadata.read();
            if typ == GyroflowProjectType::Simple {
                if let Ok(val) = serde_json::to_value(file_metadata.thin()) {
                    obj.insert("file_metadata".into(), val);
                }
            } else {
                if let Some(q) = util::compress_to_base91_cbor(&*file_metadata) {
                    obj.insert("file_metadata".into(), serde_json::Value::String(q));
                }
            }

            if typ == GyroflowProjectType::WithProcessedData {
                let mut imu_timestamps = Vec::with_capacity(gyro.quaternions.len());
                let mut imu_timestamps_final = Vec::with_capacity(gyro.quaternions.len());
                for (t, _) in &gyro.quaternions {
                    let mut timestamp_ms = *t as f64 / 1000.0;
                    timestamp_ms += gyro.offset_at_gyro_timestamp(timestamp_ms);

                    imu_timestamps.push(timestamp_ms);

                    let frame = ((timestamp_ms - params.frame_readout_time / 2.0)
                        * (params.get_scaled_fps() / 1000.0))
                        .ceil() as usize;
                    imu_timestamps_final.push(
                        timestamp_ms
                            - file_metadata
                                .per_frame_time_offsets
                                .get(frame)
                                .unwrap_or(&0.0),
                    );
                }
                util::compress_to_base91_cbor(&imu_timestamps).and_then(|s| {
                    obj.insert("synced_imu_timestamps".into(), serde_json::Value::String(s))
                });
                util::compress_to_base91_cbor(&imu_timestamps_final).and_then(|s| {
                    obj.insert(
                        "synced_imu_timestamps_with_per_frame_offset".into(),
                        serde_json::Value::String(s),
                    )
                });
                util::compress_to_base91_cbor(&gyro.quaternions).and_then(|s| {
                    obj.insert(
                        "integrated_quaternions".into(),
                        serde_json::Value::String(s),
                    )
                });
                util::compress_to_base91_cbor(&gyro.smoothed_quaternions).and_then(|s| {
                    obj.insert("smoothed_quaternions".into(), serde_json::Value::String(s))
                });
                util::compress_to_base91_cbor(&params.fovs).and_then(|s| {
                    obj.insert("adaptive_zoom_fovs".into(), serde_json::Value::String(s))
                });
            }
        }

        Ok(serde_json::to_string_pretty(&obj)?)
    }

    pub fn get_new_videofile_url(
        org_video_url: &str,
        gf_file_url: Option<&str>,
        sequence_start: u32,
    ) -> String {
        if gf_file_url.is_some() && !filesystem::exists(org_video_url) {
            ::log::debug!("get_new_videofile_url: {org_video_url}");
            let folder = filesystem::get_folder(gf_file_url.unwrap());
            let filename = filesystem::get_filename(org_video_url);
            let mut filename_replaced = filename.clone();

            if let Some(num_pos) = filename.find('%') {
                if let Some(d_pos) = filename[num_pos + 1..].find('d') {
                    if d_pos <= 5 {
                        let num_str = &filename[num_pos + 1..num_pos + 1 + d_pos];
                        if let Ok(num) = num_str.parse::<u32>() {
                            let new_num = format!("{:01$}", sequence_start, num as usize);
                            let from = format!("%{}d", num_str);
                            filename_replaced = filename.replace(&from, &new_num);
                        }
                    }
                }
            }
            if filesystem::exists_in_folder(&folder, &filename_replaced) {
                return filesystem::get_file_url(&folder, &filename, false);
            }
        }
        org_video_url.to_string()
    }

    pub fn import_gyroflow_file<F: Fn(f64)>(
        &self,
        url: &str,
        blocking: bool,
        progress_cb: F,
        cancel_flag: Arc<AtomicBool>,
        is_plugin: bool,
    ) -> std::result::Result<serde_json::Value, GyroflowCoreError> {
        let data = filesystem::read(url)?;

        let mut is_preset = false;
        let result = self.import_gyroflow_data(
            &data,
            blocking,
            Some(url),
            progress_cb,
            cancel_flag,
            &mut is_preset,
            is_plugin,
        );
        if !is_preset && result.is_ok() {
            self.input_file.write().project_file_url = Some(url.to_string());
        }
        result
    }
    pub fn import_gyroflow_data<F: Fn(f64)>(
        &self,
        data: &[u8],
        blocking: bool,
        url: Option<&str>,
        progress_cb: F,
        cancel_flag: Arc<AtomicBool>,
        is_preset: &mut bool,
        is_plugin: bool,
    ) -> std::result::Result<serde_json::Value, GyroflowCoreError> {
        let mut obj: serde_json::Value = serde_json::from_slice(&data)?;
        let mut load_options = gyro_source::FileLoadOptions::default();
        if let serde_json::Value::Object(ref mut obj) = obj {
            let mut output_size = None;
            load_options.project_version = obj.get("version").and_then(|x| x.as_u64()).unwrap_or(2);
            let mut org_video_url = obj
                .get("videofile")
                .and_then(|x| x.as_str())
                .unwrap_or(&"")
                .to_string();
            if !org_video_url.is_empty() && !org_video_url.contains("://") {
                org_video_url = filesystem::path_to_url(&org_video_url);
                if let Some(videofile) = obj.get_mut("videofile") {
                    *videofile = serde_json::Value::String(org_video_url.clone());
                }
            }
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            if let Some(v) = obj
                .get("videofile_bookmark")
                .and_then(|x| x.as_str())
                .filter(|x| !x.is_empty())
            {
                let (resolved, _is_stale) = filesystem::apple::resolve_bookmark(v, url);
                if !resolved.is_empty() {
                    org_video_url = resolved;
                }
            }

            let full_data_included = blocking && Self::project_has_motion_data(&data);

            let sequence_start = obj
                .get("image_sequence_start")
                .and_then(|x| x.as_i64())
                .unwrap_or_default() as u32;

            let video_url = if full_data_included {
                org_video_url.clone()
            } else {
                Self::get_new_videofile_url(&org_video_url, url, sequence_start)
            };
            if let Some(videofile) = obj.get_mut("videofile") {
                *videofile = serde_json::Value::String(video_url.clone());
            }
            *is_preset = org_video_url.is_empty();

            if let Some(vid_info) = obj.get("video_info") {
                let loaded_record_frame_rate = {
                    let gyro = self.gyro.read();
                    gyro.file_metadata.read().record_frame_rate
                };
                let mut params = self.params.write();
                if let Some(w) = vid_info.get("width").and_then(|x| x.as_u64()) {
                    if let Some(h) = vid_info.get("height").and_then(|x| x.as_u64()) {
                        params.size = (w as usize, h as usize);
                    }
                }
                output_size = Some(params.size);
                if let Some(v) = vid_info.get("rotation").and_then(|x| x.as_f64()) {
                    params.video_rotation = v;
                }
                if let Some(v) = vid_info.get("num_frames").and_then(|x| x.as_u64()) {
                    params.frame_count = v as usize;
                }
                if let Some(v) = vid_info.get("fps").and_then(|x| x.as_f64()) {
                    params.fps = v;
                }
                if let Some(v) = vid_info.get("duration_ms").and_then(|x| x.as_f64()) {
                    params.duration_ms = v;
                }
                if let Some(v) = vid_info.get("fps_scale") {
                    params.fps_scale = v.as_f64();
                }
                if params.fps_scale.is_none() {
                    let record_fps = vid_info
                        .get("record_frame_rate")
                        .and_then(|x| x.as_f64())
                        .or(loaded_record_frame_rate);
                    if let Some(record_fps) = record_fps {
                        apply_effective_frame_rate(&mut params, record_fps);
                    }
                }

                self.gyro.write().init_from_params(&params);
                self.keyframes.write().timestamp_scale = params.fps_scale;
            }
            if let Some(serde_json::Value::Object(obj)) = obj.get_mut("gyro_source") {
                let mut org_gyro_url = obj
                    .get("filepath")
                    .and_then(|x| x.as_str())
                    .unwrap_or(&"")
                    .to_string();
                if !org_gyro_url.is_empty() && !org_gyro_url.contains("://") {
                    org_gyro_url = filesystem::path_to_url(&org_gyro_url);
                    if let Some(filepath) = obj.get_mut("filepath") {
                        *filepath = serde_json::Value::String(org_gyro_url.clone());
                    }
                }
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                if let Some(v) = obj
                    .get("filepath_bookmark")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty())
                {
                    let (resolved, _is_stale) = filesystem::apple::resolve_bookmark(v, url);
                    if !resolved.is_empty() {
                        org_gyro_url = resolved;
                    }
                }
                let gyro_url = if full_data_included {
                    org_gyro_url.clone()
                } else {
                    Self::get_new_videofile_url(&org_gyro_url, url.clone(), sequence_start)
                };
                if let Some(fp) = obj.get_mut("filepath") {
                    *fp = serde_json::Value::String(gyro_url.clone());
                }
                use crate::gyro_source::TimeIMU;

                let is_compressed = obj
                    .get("raw_imu")
                    .map(|x| x.is_string())
                    .unwrap_or_default();
                let is_main_video = org_gyro_url == org_video_url;

                let built_in_gyro: std::io::Result<crate::gyro_source::FileMetadata> =
                    util::decompress_from_base91_cbor(
                        obj.get("file_metadata")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default(),
                    );

                ::log::info!(
                    "[import_gyroflow] gyro_source: org_gyro_url='{}', gyro_url='{}', is_main_video={}, is_compressed={}, built_in_gyro_has_motion={}, has_raw_imu={}, has_quats={}, file_exists={}, blocking={}",
                    filesystem::get_filename(&org_gyro_url),
                    filesystem::get_filename(&gyro_url),
                    is_main_video,
                    is_compressed,
                    built_in_gyro
                        .as_ref()
                        .map(|x| x.has_motion())
                        .unwrap_or(false),
                    obj.contains_key("raw_imu"),
                    obj.contains_key("quaternions"),
                    filesystem::exists(&gyro_url),
                    blocking
                );

                // Load IMU data only if it's from another file or we are sure that built_in_gyro contains motion data
                if (!org_gyro_url.is_empty() && org_gyro_url != org_video_url)
                    || built_in_gyro
                        .as_ref()
                        .map(|x| x.has_motion())
                        .unwrap_or_default()
                {
                    let mut raw_imu = Vec::new();
                    let mut quaternions = TimeQuat::default();
                    let mut image_orientations = None;
                    let mut gravity_vectors = None;
                    if is_compressed {
                        if let Some(bytes) = util::decompress_from_base91(
                            obj.get("raw_imu")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default(),
                        ) {
                            if let Ok(data) = bincode::serde::decode_from_slice::<Vec<TimeIMU>, _>(
                                &bytes,
                                bincode::config::legacy(),
                            ) {
                                raw_imu = data.0;
                            }
                        }
                        if let Some(bytes) = util::decompress_from_base91(
                            obj.get("quaternions")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default(),
                        ) {
                            if let Ok(data) = bincode::serde::decode_from_slice::<TimeQuat, _>(
                                &bytes,
                                bincode::config::legacy(),
                            ) {
                                quaternions = data.0;
                            }
                        }
                        if let Some(bytes) = util::decompress_from_base91(
                            obj.get("image_orientations")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default(),
                        ) {
                            if let Ok(data) = bincode::serde::decode_from_slice::<TimeQuat, _>(
                                &bytes,
                                bincode::config::legacy(),
                            ) {
                                image_orientations = Some(data.0);
                            }
                        }
                        if let Some(bytes) = util::decompress_from_base91(
                            obj.get("gravity_vectors")
                                .and_then(|x| x.as_str())
                                .unwrap_or_default(),
                        ) {
                            if let Ok(data) = bincode::serde::decode_from_slice::<TimeVec, _>(
                                &bytes,
                                bincode::config::legacy(),
                            ) {
                                gravity_vectors = Some(data.0);
                            }
                        }
                    } else {
                        if let Some(ri) = obj.get("raw_imu") {
                            if ri.is_array() {
                                raw_imu = serde_json::from_value(ri.clone()).unwrap_or_default();
                            }
                        }
                        quaternions = obj
                            .get("quaternions")
                            .and_then(|x| x.as_object())
                            .and_then(|x| {
                                let mut ret = TimeQuat::new();
                                for (k, v) in x {
                                    if let Ok(ts) = k.parse::<i64>() {
                                        if let Some(v) = v.as_array() {
                                            let v = v
                                                .into_iter()
                                                .filter_map(|vv| vv.as_f64())
                                                .collect::<Vec<f64>>();
                                            if v.len() == 4 {
                                                let quat = Quat64::from_quaternion(
                                                    nalgebra::Quaternion::from_vector(
                                                        Vector4::new(v[0], v[1], v[2], v[3]),
                                                    ),
                                                );
                                                ret.insert(ts, quat);
                                            }
                                        }
                                    }
                                }
                                if !ret.is_empty() { Some(ret) } else { None }
                            })
                            .unwrap_or_default();
                    }

                    if !raw_imu.is_empty() {
                        ::log::info!(
                            "[import_gyroflow] → branch A: embedded raw_imu ({} samples, {} quats)",
                            raw_imu.len(),
                            quaternions.len()
                        );
                        let md = crate::gyro_source::FileMetadata {
                            imu_orientation: obj
                                .get("imu_orientation")
                                .and_then(|x| x.as_str().map(|x| x.to_string())),
                            detected_source: Some(
                                obj.get("detected_source")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("Gyroflow file")
                                    .to_string(),
                            ),
                            quaternions,
                            gravity_vectors,
                            image_orientations,
                            raw_imu,
                            ..Default::default()
                        };

                        let mut gyro = self.gyro.write();
                        gyro.load_from_telemetry(md);
                    } else if let Ok(ref md) = built_in_gyro {
                        ::log::info!(
                            "[import_gyroflow] → branch B: built_in_gyro (raw_imu={}, quats={}, has_motion={})",
                            md.raw_imu.len(),
                            md.quaternions.len(),
                            md.has_motion()
                        );
                        let mut gyro = self.gyro.write();
                        ::log::info!(
                            "[import_gyroflow] → branch B: gyro.duration_ms={}, imu_orientation={:?}",
                            gyro.duration_ms,
                            md.imu_orientation
                        );
                        gyro.load_from_telemetry(built_in_gyro.unwrap());
                        {
                            let fm = gyro.file_metadata.read();
                            ::log::info!(
                                "[import_gyroflow] → branch B after load: raw_imu={}, quats={}, self.raw_imu_len={}",
                                fm.raw_imu.len(),
                                gyro.quaternions.len(),
                                gyro.raw_imu(&fm).len()
                            );
                        }
                    } else if filesystem::exists(&gyro_url) && blocking {
                        ::log::info!(
                            "[import_gyroflow] → branch C: load from file '{}'",
                            filesystem::get_filename(&gyro_url)
                        );
                        let mut file = filesystem::open_file(&gyro_url, false, false)?;
                        let filesize = file.size;
                        if let Err(e) = self.load_gyro_data(
                            file.get_file(),
                            filesize,
                            &gyro_url,
                            is_main_video,
                            &load_options,
                            progress_cb,
                            cancel_flag,
                        ) {
                            ::log::warn!("Failed to load gyro data from {:?}: {:?}", gyro_url, e);
                        }
                    } else {
                        ::log::warn!(
                            "[import_gyroflow] → branch D: no gyro loaded! file_exists={} blocking={}",
                            filesystem::exists(&gyro_url),
                            blocking
                        );
                    }
                } else if filesystem::exists(&gyro_url) && blocking {
                    ::log::info!(
                        "[import_gyroflow] → branch E: is_main_video load from file '{}'",
                        filesystem::get_filename(&gyro_url)
                    );
                    let mut file = filesystem::open_file(&gyro_url, false, false)?;
                    let filesize = file.size;
                    if let Err(e) = self.load_gyro_data(
                        file.get_file(),
                        filesize,
                        &gyro_url,
                        is_main_video,
                        &load_options,
                        progress_cb,
                        cancel_flag,
                    ) {
                        ::log::warn!("Failed to load gyro data from {:?}: {:?}", gyro_url, e);
                    }
                } else {
                    ::log::warn!("[import_gyroflow] → branch F: skipped gyro loading entirely");
                }

                let mut gyro = self.gyro.write();
                if !org_gyro_url.is_empty() {
                    gyro.file_url = gyro_url.clone();
                }

                if let Some(v) = obj.get("lpf").and_then(|x| x.as_f64()) {
                    gyro.imu_transforms.imu_lpf = v;
                }
                if let Some(v) = obj.get("mf").and_then(|x| x.as_i64()) {
                    gyro.imu_transforms.imu_mf = v as _;
                }
                if let Some(v) = obj.get("integration_method").and_then(|x| x.as_u64()) {
                    gyro.integration_method = v as usize;
                }
                if let Some(v) = obj.get("imu_orientation").and_then(|x| x.as_str()) {
                    gyro.imu_transforms.imu_orientation = Some(v.to_string());
                }
                if let Some(v) = obj.get("rotation") {
                    let v: [f64; 3] = serde_json::from_value(v.clone()).unwrap_or_default();
                    gyro.imu_transforms.set_imu_rotation(v[0], v[1], v[2]);
                }
                if let Some(v) = obj.get("acc_rotation") {
                    let v: [f64; 3] = serde_json::from_value(v.clone()).unwrap_or_default();
                    gyro.imu_transforms.set_acc_rotation(v[0], v[1], v[2]);
                }
                if let Some(v) = obj.get("gyro_bias") {
                    gyro.imu_transforms.gyro_bias = serde_json::from_value(v.clone()).ok();
                }

                obj.remove("raw_imu");
                obj.remove("quaternions");
                obj.remove("smoothed_quaternions");
                obj.remove("image_orientations");
                obj.remove("gravity_vectors");
                obj.remove("file_metadata");
            }
            if let Some(lens) = obj.get("calibration_data") {
                let mut l = self.lens.write();
                l.load_from_json_value(&lens);
                let db = self.lens_profile_db.read();
                l.resolve_interpolations(&db);
            }
            if let Some(serde_json::Value::Object(obj)) = obj.get_mut("stabilization") {
                let mut params = self.params.write();
                if let Some(v) = obj.get("fov").and_then(|x| x.as_f64()) {
                    params.fov = v;
                }
                if let Some(v) = obj.get("frame_readout_time").and_then(|x| x.as_f64()) {
                    params.frame_readout_time = v;
                    if v < 0.0 {
                        params.frame_readout_direction = ReadoutDirection::BottomToTop;
                    }
                }
                if let Some(v) = obj.get("frame_readout_direction").and_then(|x| x.as_i64()) {
                    params.frame_readout_direction = (v as i32).into();
                }
                if let Some(v) = obj.get("frame_readout_direction").and_then(|x| x.as_str()) {
                    params.frame_readout_direction = v.into();
                }
                if let Some(v) = obj.get("adaptive_zoom_window").and_then(|x| x.as_f64()) {
                    params.adaptive_zoom_window = v;
                }
                if let Some(v) = obj.get("lens_correction_amount").and_then(|x| x.as_f64()) {
                    params.lens_correction_amount = v;
                }
                if let Some(v) = obj.get("frame_offset").and_then(|x| x.as_i64()) {
                    params.frame_offset = v as i32;
                }
                if let Some(v) = obj.get("horizontal_rs").and_then(|x| x.as_bool()) {
                    if v {
                        params.frame_readout_direction = if params.frame_readout_time < 0.0 {
                            ReadoutDirection::RightToLeft
                        } else {
                            ReadoutDirection::LeftToRight
                        };
                    }
                }
                if let Some(v) = obj.get("max_zoom").and_then(|x| x.as_f64()) {
                    params.max_zoom = Some(v);
                }
                if let Some(v) = obj.get("max_zoom_iterations").and_then(|x| x.as_i64()) {
                    params.max_zoom_iterations = v as _;
                }

                if let Some(v) = obj.get("video_speed").and_then(|x| x.as_f64()) {
                    params.video_speed = v;
                }
                if let Some(v) = obj
                    .get("video_speed_affects_smoothing")
                    .and_then(|x| x.as_bool())
                {
                    params.video_speed_affects_smoothing = v;
                }
                if let Some(v) = obj
                    .get("video_speed_affects_zooming")
                    .and_then(|x| x.as_bool())
                {
                    params.video_speed_affects_zooming = v;
                }
                if let Some(v) = obj
                    .get("video_speed_affects_zooming_limit")
                    .and_then(|x| x.as_bool())
                {
                    params.video_speed_affects_zooming_limit = v;
                }

                if let Some(center_offs) = obj
                    .get("adaptive_zoom_center_offset")
                    .and_then(|x| x.as_array())
                {
                    params.adaptive_zoom_center_offset = (
                        center_offs
                            .get(0)
                            .and_then(|x| x.as_f64())
                            .unwrap_or_default(),
                        center_offs
                            .get(1)
                            .and_then(|x| x.as_f64())
                            .unwrap_or_default(),
                    );
                }
                if let Some(x) = obj.get("additional_rotation").and_then(|x| x.as_array()) {
                    params.additional_rotation = (
                        x.get(0).and_then(|x| x.as_f64()).unwrap_or_default(),
                        x.get(1).and_then(|x| x.as_f64()).unwrap_or_default(),
                        x.get(2).and_then(|x| x.as_f64()).unwrap_or_default(),
                    );
                }
                if let Some(x) = obj.get("additional_translation").and_then(|x| x.as_array()) {
                    params.additional_translation = (
                        x.get(0).and_then(|x| x.as_f64()).unwrap_or_default(),
                        x.get(1).and_then(|x| x.as_f64()).unwrap_or_default(),
                        x.get(2).and_then(|x| x.as_f64()).unwrap_or_default(),
                    );
                }
                if let Some(zooming_method) =
                    obj.get("adaptive_zoom_method").and_then(|x| x.as_i64())
                {
                    params.adaptive_zoom_method = zooming_method as i32;
                }

                if let Some(method) = obj.get("method").and_then(|x| x.as_str()) {
                    let method_idx = self
                        .get_smoothing_algs()
                        .iter()
                        .enumerate()
                        .find(|(_, m)| method == m.as_str())
                        .map(|(idx, _)| idx)
                        .unwrap_or(1);

                    self.smoothing.write().set_current(method_idx);
                }

                let mut smoothing = self.smoothing.write();
                let empty_vec = Vec::new();
                let smoothing_params = obj
                    .get("smoothing_params")
                    .and_then(|x| x.as_array())
                    .unwrap_or(&empty_vec);
                let smoothing_alg = smoothing.current_mut();
                for param in smoothing_params {
                    (|| -> Option<()> {
                        let name = param.get("name").and_then(|x| x.as_str())?;
                        let value = param.get("value").and_then(|x| x.as_f64())?;
                        smoothing_alg.set_parameter(name, value);
                        Some(())
                    })();
                }
                if let Some(horizon_amount) =
                    obj.get("horizon_lock_amount").and_then(|x| x.as_f64())
                {
                    if let Some(horizon_roll) =
                        obj.get("horizon_lock_roll").and_then(|x| x.as_f64())
                    {
                        let horizon_pitch_enabled = obj
                            .get("horizon_lock_pitch_enabled")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false);
                        let horizon_pitch = obj
                            .get("horizon_lock_pitch")
                            .and_then(|x| x.as_f64())
                            .unwrap_or(0.0);
                        let turn_threshold = obj
                            .get("turn_threshold")
                            .and_then(|x| x.as_f64())
                            .unwrap_or(5.0);
                        let turn_smoothing_ms = obj
                            .get("turn_smoothing_ms")
                            .and_then(|x| x.as_f64())
                            .unwrap_or(500.0);
                        let turn_multiplier = obj
                            .get("turn_multiplier")
                            .and_then(|x| x.as_f64())
                            .unwrap_or(1.0);
                        let tilt_accel_limit = obj
                            .get("tilt_accel_limit")
                            .and_then(|x| x.as_f64())
                            .unwrap_or(f64::INFINITY);
                        let automatic_lock = obj
                            .get("automatic_lock")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(false);
                        smoothing.horizon_lock.set_horizon(
                            horizon_amount,
                            horizon_roll,
                            horizon_pitch_enabled,
                            horizon_pitch,
                            automatic_lock,
                            turn_threshold,
                            turn_smoothing_ms,
                            turn_multiplier,
                            tilt_accel_limit,
                        );
                    }
                }
                if let Some(v) = obj.get("use_gravity_vectors").and_then(|x| x.as_bool()) {
                    self.gyro.write().set_use_gravity_vectors(v);
                }
                if let Some(v) = obj
                    .get("horizon_lock_integration_method")
                    .and_then(|x| x.as_i64())
                {
                    self.gyro
                        .write()
                        .set_horizon_lock_integration_method(v as i32);
                }

                obj.remove("adaptive_zoom_fovs");
            }
            if let Some(serde_json::Value::Object(obj)) = obj.get_mut("output") {
                if let Some(w) = obj.get("output_width").and_then(|x| x.as_u64()) {
                    if let Some(h) = obj.get("output_height").and_then(|x| x.as_u64()) {
                        output_size = Some((w as usize, h as usize));
                        if *is_preset {
                            self.input_file.write().preset_output_size =
                                Some((w as usize, h as usize));
                        }
                    }
                }
                if is_plugin {
                    if let Some(i) = obj.get("interpolation").and_then(|x| x.as_str()) {
                        self.stabilization.write().interpolation = i.into();
                    }
                }
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                if let Some(v) = obj
                    .get("output_folder_bookmark")
                    .and_then(|x| x.as_str())
                    .filter(|x| !x.is_empty())
                {
                    let (resolved, _is_stale) = filesystem::apple::resolve_bookmark(v, url);
                    if !resolved.is_empty() {
                        filesystem::folder_access_granted(&resolved);
                        obj.insert("output_folder".into(), serde_json::Value::String(resolved));
                    }
                }
            }

            if let Some(serde_json::Value::Object(offsets)) = obj.get("offsets") {
                let mut gyro = self.gyro.write();
                gyro.set_offsets(
                    offsets
                        .iter()
                        .filter_map(|(k, v)| Some((k.parse().ok()?, v.as_f64()?)))
                        .collect(),
                );
                self.keyframes.write().update_gyro(&gyro);
            }
            obj.remove("offsets");

            if let Some(keyframes) = obj.get("keyframes") {
                self.keyframes.write().deserialize(keyframes);
            }

            if let Some(start) = obj.get("trim_start").and_then(|x| x.as_f64()) {
                if let Some(end) = obj.get("trim_end").and_then(|x| x.as_f64()) {
                    self.params.write().trim_ranges = vec![(start, end)];
                }
            }

            let duration_ms = self.params.read().duration_ms.max(1.0);
            if let Some(ranges) = obj.get("trim_ranges_ms").and_then(|x| x.as_array()) {
                let ranges = ranges
                    .iter()
                    .filter_map(|x| {
                        let x = x.as_array()?;
                        if x.len() == 2 {
                            let mut end_range = x[1].as_f64()?;
                            if end_range < 0.0 {
                                end_range = duration_ms + end_range;
                            }
                            Some((x[0].as_f64()? / duration_ms, end_range / duration_ms))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                self.params.write().trim_ranges = ranges;
            } else if let Some(ranges) = obj.get("trim_ranges").and_then(|x| x.as_array()) {
                // Deprecated
                let ranges = ranges
                    .iter()
                    .filter_map(|x| {
                        let x = x.as_array()?;
                        if x.len() == 2 {
                            Some((x[0].as_f64()?, x[1].as_f64()?))
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>();
                self.params.write().trim_ranges = ranges;
            }

            {
                let mut params = self.params.write();
                if let Some(v) = obj.get("background_color").and_then(|x| x.as_array()) {
                    if v.len() == 4 {
                        params.background = nalgebra::Vector4::new(
                            v[0].as_f64().unwrap_or_default() as f32,
                            v[1].as_f64().unwrap_or_default() as f32,
                            v[2].as_f64().unwrap_or_default() as f32,
                            v[3].as_f64().unwrap_or_default() as f32,
                        );
                    }
                }
                if let Some(v) = obj.get("background_mode").and_then(|x| x.as_i64()) {
                    params.background_mode = stabilization_params::BackgroundMode::from(v as i32);
                }
                if let Some(v) = obj.get("background_margin").and_then(|x| x.as_f64()) {
                    params.background_margin = v;
                }
                if let Some(v) = obj
                    .get("background_margin_feather")
                    .and_then(|x| x.as_f64())
                {
                    params.background_margin_feather = v;
                }
                if let Some(v) = obj
                    .get("light_refraction_coefficient")
                    .and_then(|x| x.as_f64())
                {
                    params.light_refraction_coefficient = v;
                }
            }

            {
                let mut input_file = self.input_file.write();
                if *is_preset {
                    if let Some(name) = obj.get("name").and_then(|x| x.as_str()) {
                        input_file.preset_name = Some(name.into());
                    } else if let Some(url) = url {
                        input_file.preset_name =
                            Some(filesystem::get_filename(url).replace(".gyroflow", ""));
                    } else {
                        input_file.preset_name = Some("Untitled".into());
                    }
                }
                if let Some(seq_start) = obj.get("image_sequence_start").and_then(|x| x.as_i64()) {
                    input_file.image_sequence_start = seq_start as i32;
                }
                if let Some(seq_fps) = obj.get("image_sequence_fps").and_then(|x| x.as_f64()) {
                    input_file.image_sequence_fps = seq_fps;
                }
                if !org_video_url.is_empty() {
                    if full_data_included {
                        input_file.url = org_video_url;
                    } else if filesystem::can_open_file(&video_url) {
                        input_file.url = video_url;
                    }
                }
            }

            if blocking {
                self.recompute_gyro();

                if let Some(output_size) = output_size {
                    if output_size.0 > 0 && output_size.1 > 0 {
                        self.set_output_size(output_size.0, output_size.1);
                    }
                }
                self.init_size();
                self.recompute_blocking();
            }
        }
        Ok(obj)
    }

    pub fn project_has_motion_data(data: &[u8]) -> bool {
        if let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice(&data) {
            if let Some(serde_json::Value::Object(obj)) = obj.get("gyro_source") {
                let built_in_gyro: std::io::Result<crate::gyro_source::FileMetadata> =
                    util::decompress_from_base91_cbor(
                        obj.get("file_metadata")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default(),
                    );
                if built_in_gyro
                    .as_ref()
                    .map(|x| x.has_motion())
                    .unwrap_or_default()
                {
                    return true;
                }

                // Compatibility with older formats
                let is_compressed = obj
                    .get("raw_imu")
                    .map(|x| x.is_string())
                    .unwrap_or_default();
                if is_compressed {
                    if let Some(bytes) = util::decompress_from_base91(
                        obj.get("raw_imu")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default(),
                    ) {
                        if let Ok(data) = bincode::serde::decode_from_slice::<
                            Vec<crate::gyro_source::TimeIMU>,
                            _,
                        >(
                            &bytes, bincode::config::legacy()
                        ) {
                            if !data.0.is_empty() {
                                return true;
                            }
                        }
                    }
                    if let Some(bytes) = util::decompress_from_base91(
                        obj.get("quaternions")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default(),
                    ) {
                        if let Ok(data) = bincode::serde::decode_from_slice::<TimeQuat, _>(
                            &bytes,
                            bincode::config::legacy(),
                        ) {
                            if !data.0.is_empty() {
                                return true;
                            }
                        }
                    }
                } else {
                    if let Some(ri) = obj.get("raw_imu") {
                        if let Some(ri) = ri.as_array() {
                            if !ri.is_empty() {
                                return true;
                            }
                        }
                    }
                    if let Some(x) = obj.get("quaternions").and_then(|x| x.as_object()) {
                        if !x.is_empty() {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    pub fn load_video_file<T: Read + Seek>(
        &self,
        stream: &mut T,
        filesize: usize,
        url: &str,
        mut metadata: Option<telemetry_parser::util::VideoMetadata>,
        is_plugin: bool,
    ) -> Result<telemetry_parser::util::VideoMetadata, GyroflowCoreError> {
        if metadata.is_none() {
            metadata = Some(util::get_video_metadata(stream, filesize, url)?);
        }
        let metadata = metadata.unwrap();
        log::info!("Loading video file: {metadata:?}");

        if metadata.width > 0
            && metadata.height > 0
            && metadata.duration_s > 0.0
            && metadata.fps > 0.0
        {
            let video_size = (metadata.width as usize, metadata.height as usize);
            let frame_count = (metadata.duration_s * metadata.fps).ceil() as usize;

            self.init_from_video_data(
                metadata.duration_s * 1000.0,
                metadata.fps,
                frame_count,
                video_size,
            );
            let _ = self.load_gyro_data(
                stream,
                filesize,
                url,
                true,
                &Default::default(),
                |_| (),
                Arc::new(AtomicBool::new(false)),
            );

            let has_builtin_profile = {
                let gyro = self.gyro.read();
                let file_metadata = gyro.file_metadata.read();
                file_metadata
                    .lens_profile
                    .as_ref()
                    .map(|y| y.is_object())
                    .unwrap_or_default()
            };

            let id_str = self
                .camera_id
                .read()
                .as_ref()
                .map(|v| v.get_identifier_for_autoload())
                .unwrap_or_default();
            if !id_str.is_empty() && !has_builtin_profile {
                let mut db = self.lens_profile_db.read();
                if !db.loaded {
                    drop(db);
                    {
                        let mut db = self.lens_profile_db.write();
                        db.load_all();
                    }
                    db = self.lens_profile_db.read();
                }
                if db.contains_id(&id_str) {
                    match self.load_lens_profile(&id_str) {
                        Ok(_) => {
                            let (fr, frd) = {
                                let lens = self.lens.read();
                                (lens.frame_readout_time, lens.frame_readout_direction)
                            };
                            if let Some(fr) = fr {
                                let mut params = self.params.write();
                                params.frame_readout_time = fr.abs();
                                params.frame_readout_direction = frd.unwrap_or(if fr < 0.0 {
                                    ReadoutDirection::BottomToTop
                                } else {
                                    ReadoutDirection::TopToBottom
                                });
                            }
                        }
                        Err(e) => {
                            log::error!("An error occured: {e:?}");
                            return Err(e);
                        }
                    }
                }
            }
            let mut output_width = metadata.width;
            let mut output_height = metadata.height;
            if let Some(output_dim) = self.lens.read().output_dimension.clone() {
                output_width = output_dim.w;
                output_height = output_dim.h;
            }
            self.set_size(video_size.0, video_size.1);
            self.set_output_size(output_width, output_height);

            // Apply default preset
            let local_path =
                lens_profile_database::LensProfileDatabase::get_path().join("default.gyroflow");
            let settings_path = settings::data_dir()
                .join("lens_profiles")
                .join("default.gyroflow");
            if settings_path.exists() {
                let _ = self.import_gyroflow_file(
                    &settings_path.to_str().unwrap(),
                    true,
                    |_| (),
                    Arc::new(AtomicBool::new(false)),
                    is_plugin,
                );
            } else if local_path.exists() {
                let _ = self.import_gyroflow_file(
                    &local_path.to_str().unwrap(),
                    true,
                    |_| (),
                    Arc::new(AtomicBool::new(false)),
                    is_plugin,
                );
            }
        }
        Ok(metadata)
    }

    pub fn set_device(&self, i: i32) {
        self.params.write().current_device = i;
        let mut l = self.stabilization.write();
        l.set_device(i as isize);
    }

    pub fn set_keyframe(&self, typ: &KeyframeType, timestamp_us: i64, value: f64) {
        self.keyframes.write().set(typ, timestamp_us, value);
        self.keyframes_updated(typ);
    }
    pub fn set_keyframe_easing(&self, typ: &KeyframeType, timestamp_us: i64, easing: Easing) {
        self.keyframes.write().set_easing(typ, timestamp_us, easing);
        self.keyframes_updated(typ);
    }
    pub fn keyframe_easing(&self, typ: &KeyframeType, timestamp_us: i64) -> Option<Easing> {
        self.keyframes.read().easing(typ, timestamp_us)
    }
    pub fn set_keyframe_timestamp(&self, typ: &KeyframeType, id: u32, timestamp_us: i64) {
        self.keyframes.write().set_timestamp(typ, id, timestamp_us);
        self.keyframes_updated(typ);
    }
    pub fn keyframe_id(&self, typ: &KeyframeType, timestamp_us: i64) -> Option<u32> {
        self.keyframes.read().id(typ, timestamp_us)
    }
    pub fn remove_keyframe(&self, typ: &KeyframeType, timestamp_us: i64) {
        self.keyframes.write().remove(typ, timestamp_us);
        self.keyframes_updated(typ);
    }
    pub fn clear_keyframes_type(&self, typ: &KeyframeType) {
        self.keyframes.write().clear_type(typ);
        self.keyframes_updated(typ);
    }
    pub fn keyframe_value_at_video_timestamp(
        &self,
        typ: &KeyframeType,
        timestamp_ms: f64,
    ) -> Option<f64> {
        self.keyframes
            .read()
            .value_at_video_timestamp(typ, timestamp_ms)
    }
    pub fn is_keyframed(&self, typ: &KeyframeType) -> bool {
        self.keyframes.read().is_keyframed(typ)
    }
    fn keyframes_updated(&self, typ: &KeyframeType) {
        match typ {
            KeyframeType::VideoRotation
            | KeyframeType::ZoomingSpeed
            | KeyframeType::AdditionalTranslationX
            | KeyframeType::AdditionalTranslationY
            | KeyframeType::AdditionalTranslationZ
            | KeyframeType::ZoomingCenterX
            | KeyframeType::ZoomingCenterY => self.invalidate_zooming(),

            KeyframeType::LockHorizonAmount
            | KeyframeType::LockHorizonRoll
            | KeyframeType::LockHorizonPitchEnabled
            | KeyframeType::LockHorizonPitch
            | KeyframeType::AdditionalRotationX
            | KeyframeType::AdditionalRotationY
            | KeyframeType::AdditionalRotationZ
            | KeyframeType::SmoothingParamTimeConstant
            | KeyframeType::SmoothingParamTimeConstant2
            | KeyframeType::SmoothingParamSmoothness
            | KeyframeType::SmoothingParamPitch
            | KeyframeType::SmoothingParamRoll
            | KeyframeType::SmoothingParamYaw => self.invalidate_smoothing(),
            _ => {}
        }
    }

    pub fn get_optimal_sync_points(
        &self,
        target_sync_points: usize,
        initial_offset: f64,
    ) -> Vec<f64> {
        let dur_ms = self.params.read().get_scaled_duration_ms();
        let trim_ranges = {
            let params = self.params.read();
            if params.trim_ranges.is_empty() {
                vec![(
                    (0.0 - initial_offset) / 1000.0,
                    (dur_ms - initial_offset) / 1000.0,
                )]
            } else {
                params
                    .trim_ranges
                    .iter()
                    .map(|x| {
                        (
                            (x.0 * dur_ms - initial_offset) / 1000.0,
                            (x.1 * dur_ms - initial_offset) / 1000.0,
                        )
                    })
                    .collect()
            }
        };

        if let Some(mut optsync) = synchronization::optimsync::OptimSync::new(&self.gyro.read()) {
            let (points, rank, ratio, rank_window_center_offset_ms) =
                optsync.run(target_sync_points, trim_ranges);
            {
                let mut sync_data = self.sync_data.write();
                sync_data.rank = rank;
                sync_data.ratio = ratio;
                sync_data.rank_window_center_offset_ms = rank_window_center_offset_ms;
            }
            points
                .iter()
                .map(|x| (x + initial_offset) / dur_ms)
                .filter(|&v| v >= 0.0 && v <= 1.0)
                .collect()
        } else {
            Vec::new()
        }
    }
}

pub fn timestamp_at_frame(frame: i32, fps: f64) -> f64 {
    frame as f64 * 1000.0 / fps
}
pub fn frame_at_timestamp(timestamp_ms: f64, fps: f64) -> i32 {
    (timestamp_ms * (fps / 1000.0)).round() as i32
}

pub fn run_threaded<F>(cb: F)
where
    F: FnOnce() + Send + 'static,
{
    THREAD_POOL.spawn(cb);
}

use std::str::FromStr;
#[derive(Debug, Clone, PartialEq, ::serde::Serialize, ::serde::Deserialize)]
pub enum GyroflowProjectType {
    Simple,
    WithGyroData,
    WithProcessedData,
}
impl FromStr for GyroflowProjectType {
    type Err = serde_json::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        serde_json::from_str(&format!("\"{}\"", s))
    }
}
impl ToString for GyroflowProjectType {
    fn to_string(&self) -> String {
        format!("{:?}", self)
    }
}

#[derive(thiserror::Error, Debug)]
pub enum GyroflowCoreError {
    #[error("No stabilization data at {0}. Make sure you called `ensure_ready_for_processing`")]
    NoStabilizationData(i64),

    #[error("Buffer too small")]
    BufferTooSmall,

    #[error("Size too small")]
    SizeTooSmall,

    #[error("Size mismatch ({0:?} != ({1:?})")]
    SizeMismatch((usize, usize), (usize, usize)),

    #[error("Invalid stride: {0} must be greater than width ({1})")]
    InvalidStride(i32, i32),

    #[error("Input buffer is empty")]
    InputBufferEmpty,

    #[error("Output buffer is empty")]
    OutputBufferEmpty,

    #[error("Failed to find cached wgpu in process_pixels. Key: {0}")]
    NoCachedWgpuInstance(String),

    #[error("Unsupported file format {0}")]
    UnsupportedFormat(String),

    #[error("Invalid data")]
    InvalidData,

    #[error("JSON error {0:?}")]
    JSONError(#[from] serde_json::Error),

    #[error("Filesystem error {0:?}")]
    FilesystemError(#[from] crate::filesystem::FilesystemError),

    #[error("IO error {0:?}")]
    IOError(#[from] std::io::Error),

    #[error("Unknown error")]
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_main_video_telemetry_falls_back_to_synthetic_when_lens_id_is_missing() {
        let manager = StabilizationManager::default();
        manager.params.write().size = (1920, 1080);
        manager.lens_profile_db.write().loaded = true;

        let mut md = gyro_source::FileMetadata {
            lens_profile: Some(serde_json::Value::String("missing-profile-id".to_owned())),
            unit_pixel_focal_length: Some(100.0),
            frame_readout_time: Some(12.5),
            frame_readout_direction: ReadoutDirection::BottomToTop,
            camera_identifier: Some(CameraIdentifier {
                brand: "Nikon".to_owned(),
                model: "ZR".to_owned(),
                lens_model: "NIKKOR Z 24-120mm f/4 S".to_owned(),
                camera_setting: "4K".to_owned(),
                ..Default::default()
            }),
            lens_params: BTreeMap::from([(
                0,
                gyro_source::LensParams {
                    focal_length: Some(35.0),
                    pixel_focal_length: Some(3500.0),
                    ..Default::default()
                },
            )]),
            ..Default::default()
        };

        manager.apply_main_video_telemetry(&mut md, "missing-profile", false);

        let lens = manager.lens.read();
        assert_eq!(lens.calibrated_by, "NiYien");
        assert_eq!(lens.camera_brand, "Nikon");
        assert_eq!(lens.camera_model, "ZR");
        assert_eq!(lens.lens_model, "NIKKOR Z 24-120mm f/4 S");
        assert_eq!(lens.frame_readout_time, Some(12.5));
        assert_eq!(
            lens.frame_readout_direction,
            Some(ReadoutDirection::BottomToTop)
        );
        assert_eq!(lens.fisheye_params.camera_matrix[0], [3500.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 3500.0, 540.0]);
    }

    #[test]
    fn set_user_focal_length_populates_empty_lens_identity_and_readout() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
            params.frame_count = 1;
            params.fps = 30.0;
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                unit_pixel_focal_length: Some(100.0),
                frame_readout_time: Some(10.0),
                frame_readout_direction: ReadoutDirection::BottomToTop,
                camera_identifier: Some(CameraIdentifier {
                    brand: "Nikon".to_owned(),
                    model: "ZR".to_owned(),
                    lens_model: "NIKKOR Z 24-120mm f/4 S".to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }
            .into();
        }

        manager.set_user_focal_length(24.0);

        let lens = manager.lens.read();
        assert_eq!(lens.calibrated_by, "NiYien");
        assert_eq!(lens.camera_brand, "Nikon");
        assert_eq!(lens.camera_model, "ZR");
        assert_eq!(lens.lens_model, "NIKKOR Z 24-120mm f/4 S");
        assert_eq!(lens.frame_readout_time, Some(10.0));
        assert_eq!(
            lens.frame_readout_direction,
            Some(ReadoutDirection::BottomToTop)
        );
        assert_eq!(lens.focal_length, Some(24.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [2400.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 2400.0, 540.0]);
    }

    #[test]
    fn set_user_focal_length_invalidates_smoothing_and_zooming() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
            params.frame_count = 1;
            params.fps = 30.0;
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                unit_pixel_focal_length: Some(100.0),
                ..Default::default()
            }
            .into();
        }
        manager.smoothing_checksum.store(123, SeqCst);
        manager.zooming_checksum.store(456, SeqCst);

        manager.set_user_focal_length(24.0);

        assert_eq!(manager.smoothing_checksum.load(SeqCst), 0);
        assert_eq!(manager.zooming_checksum.load(SeqCst), 0);
    }

    #[test]
    fn set_user_focal_length_does_not_make_lens_group_treat_video_as_auto_focal() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
            params.frame_count = 1;
            params.fps = 30.0;
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                ..Default::default()
            }
            .into();
        }

        manager.set_user_focal_length(24.0);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        *manager.lens_group_config.write() = configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((1920, 1080)));

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(50.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [5000.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 5000.0, 540.0]);
    }

    #[test]
    fn set_lens_param_invalidates_smoothing_and_zooming() {
        let manager = StabilizationManager::default();
        {
            let mut lens = manager.lens.write();
            lens.fisheye_params.camera_matrix = vec![
                [1000.0, 0.0, 960.0],
                [0.0, 1000.0, 540.0],
                [0.0, 0.0, 1.0],
            ];
            lens.fisheye_params.distortion_coeffs = vec![0.0, 0.0, 0.0, 0.0];
        }
        manager.smoothing_checksum.store(123, SeqCst);
        manager.zooming_checksum.store(456, SeqCst);

        manager.set_lens_param("fx", 2400.0);

        assert_eq!(manager.lens.read().fisheye_params.camera_matrix[0][0], 2400.0);
        assert_eq!(manager.smoothing_checksum.load(SeqCst), 0);
        assert_eq!(manager.zooming_checksum.load(SeqCst), 0);
    }

    #[test]
    fn preview_lens_group_config_builds_profile_without_persisting_config() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 2 }),
                unit_pixel_focal_length: Some(100.0),
                frame_readout_time: Some(10.0),
                camera_identifier: Some(CameraIdentifier {
                    brand: "Nikon".to_owned(),
                    model: "ZR".to_owned(),
                    lens_model: "NIKKOR Z 24-120mm f/4 S".to_owned(),
                    ..Default::default()
                }),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[2].focal_length_mm = Some(24.0);
        let json = niyien_lens_presets::lens_group_config_to_json(&configs);
        let stored_configs_before = manager.lens_group_config.read().clone();

        let profile =
            LensProfile::from_json(&manager.preview_lens_group_config_json(&json, 2).unwrap())
                .unwrap();

        assert_eq!(profile.focal_length, Some(24.0));
        assert_eq!(profile.fisheye_params.camera_matrix[0], [2400.0, 0.0, 960.0]);
        assert_eq!(profile.fisheye_params.camera_matrix[1], [0.0, 2400.0, 540.0]);

        assert_eq!(manager.lens.read().focal_length, None);
        assert_eq!(*manager.lens_group_config.read(), stored_configs_before);
    }

    #[test]
    fn preview_lens_group_config_does_not_mutate_main_lens_or_backup() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                lens_params: BTreeMap::from([(
                    0,
                    gyro_source::LensParams {
                        focal_length: Some(31.0),
                        pixel_focal_length: Some(3100.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        configs[1].anamorphic_enabled = true;
        configs[1].squeeze_ratio = Some(1.33);
        let json = niyien_lens_presets::lens_group_config_to_json(&configs);

        let profile =
            LensProfile::from_json(&manager.preview_lens_group_config_json(&json, 1).unwrap())
                .unwrap();

        assert_eq!(profile.focal_length, Some(50.0));
        assert_eq!(profile.fisheye_params.camera_matrix[0], [5000.0, 0.0, 1277.0]);
        assert_eq!(profile.fisheye_params.camera_matrix[1], [0.0, 5000.0, 540.0]);
        assert_eq!(manager.lens.read().focal_length, None);
        assert!(manager.pre_anamorphic_backup.read().is_none());
    }

    #[test]
    fn preview_lens_group_config_builds_anamorphic_manual_profile() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                lens_params: BTreeMap::from([(
                    0,
                    gyro_source::LensParams {
                        focal_length: Some(31.0),
                        pixel_focal_length: Some(3100.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        configs[1].anamorphic_enabled = true;
        configs[1].squeeze_ratio = Some(1.33);
        let json = niyien_lens_presets::lens_group_config_to_json(&configs);

        manager.smoothing_checksum.store(123, SeqCst);
        manager.zooming_checksum.store(456, SeqCst);

        let profile =
            LensProfile::from_json(&manager.preview_lens_group_config_json(&json, 1).unwrap())
                .unwrap();

        assert_eq!(profile.focal_length, Some(50.0));
        assert_eq!(profile.fisheye_params.camera_matrix[0], [5000.0, 0.0, 1277.0]);
        assert_eq!(profile.fisheye_params.camera_matrix[1], [0.0, 5000.0, 540.0]);
        assert_eq!(manager.smoothing_checksum.load(SeqCst), 123);
        assert_eq!(manager.zooming_checksum.load(SeqCst), 456);
    }

    #[test]
    fn preview_lens_group_config_builds_anamorphic_auto_profile() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                lens_params: BTreeMap::from([(
                    0,
                    gyro_source::LensParams {
                        focal_length: Some(31.0),
                        pixel_focal_length: Some(3100.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].anamorphic_enabled = true;
        configs[1].squeeze_ratio = Some(1.33);
        let json = niyien_lens_presets::lens_group_config_to_json(&configs);

        let profile =
            LensProfile::from_json(&manager.preview_lens_group_config_json(&json, 1).unwrap())
                .unwrap();

        assert_eq!(profile.focal_length, Some(31.0));
        assert_eq!(profile.fisheye_params.camera_matrix[0], [3100.0, 0.0, 1277.0]);
        assert_eq!(profile.fisheye_params.camera_matrix[1], [0.0, 3100.0, 540.0]);
    }

    #[test]
    fn lens_group_preview_does_not_block_followup_main_apply() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
            params.frame_count = 1;
            params.fps = 30.0;
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut preview_configs = niyien_lens_presets::default_lens_group_configs();
        preview_configs[1].focal_length_mm = Some(50.0);
        let preview_json = niyien_lens_presets::lens_group_config_to_json(&preview_configs);
        let preview_profile = LensProfile::from_json(
            &manager
                .preview_lens_group_config_json(&preview_json, 1)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(preview_profile.focal_length, Some(50.0));

        let mut main_configs = niyien_lens_presets::default_lens_group_configs();
        main_configs[1].focal_length_mm = Some(30.0);
        *manager.lens_group_config.write() = main_configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((1920, 1080)));

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(30.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [3000.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 3000.0, 540.0]);
    }

    #[test]
    fn apply_lens_group_config_json_to_main_rebuilds_lens_and_invalidates_smoothing() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);
        manager.smoothing_checksum.store(123, SeqCst);
        manager.zooming_checksum.store(456, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(80.0);
        let json = niyien_lens_presets::lens_group_config_to_json(&configs);
        let stored_configs_before = manager.lens_group_config.read().clone();

        assert_eq!(
            manager.apply_lens_group_config_json_to_main(&json, 1),
            Some((1920, 1080))
        );

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(80.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [8000.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 8000.0, 540.0]);
        drop(lens);
        assert_eq!(manager.smoothing_checksum.load(SeqCst), 0);
        assert_eq!(manager.zooming_checksum.load(SeqCst), 0);
        assert_eq!(*manager.lens_group_config.read(), stored_configs_before);
    }

    #[test]
    fn apply_lens_group_to_main_keeps_auto_focal_without_anamorphic() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                lens_params: BTreeMap::from([(
                    0,
                    gyro_source::LensParams {
                        focal_length: Some(31.0),
                        pixel_focal_length: Some(3100.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        *manager.lens_group_config.write() = configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((1920, 1080)));

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(31.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [3100.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 3100.0, 540.0]);
        assert!(!lens.lens_group_override);
    }

    #[test]
    fn apply_lens_group_to_main_uses_manual_focal_when_only_pixel_focal_exists() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                lens_params: BTreeMap::from([(
                    0,
                    gyro_source::LensParams {
                        pixel_focal_length: Some(3100.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(false, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        *manager.lens_group_config.write() = configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((1920, 1080)));

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(50.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [5000.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 5000.0, 540.0]);
        assert!(lens.lens_group_override);
    }

    #[test]
    fn apply_lens_group_to_main_fills_focal_without_anamorphic_when_manual_edit_off() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(false, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        configs[1].anamorphic_enabled = true;
        configs[1].squeeze_ratio = Some(1.33);
        *manager.lens_group_config.write() = configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((1920, 1080)));

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(50.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [5000.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 5000.0, 540.0]);
        assert_eq!(lens.input_horizontal_stretch, 1.0);
        assert_eq!(lens.input_vertical_stretch, 1.0);
        assert!(lens.output_dimension.is_none());
        assert!(lens.lens_group_override);
    }

    #[test]
    fn apply_lens_group_to_main_uses_manual_focal_with_anamorphic_when_metadata_has_auto_focal() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                lens_params: BTreeMap::from([(
                    0,
                    gyro_source::LensParams {
                        focal_length: Some(31.0),
                        pixel_focal_length: Some(3100.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        configs[1].anamorphic_enabled = true;
        configs[1].squeeze_ratio = Some(1.33);
        *manager.lens_group_config.write() = configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((2554, 1080)));

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(50.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [5000.0, 0.0, 1277.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 5000.0, 540.0]);
    }

    #[test]
    fn apply_lens_group_to_main_restores_auto_focal_after_anamorphic_is_disabled() {
        let manager = StabilizationManager::default();
        {
            let mut params = manager.params.write();
            params.size = (1920, 1080);
        }
        {
            let mut gyro = manager.gyro.write();
            gyro.file_metadata = gyro_source::FileMetadata {
                additional_data: serde_json::json!({ "lens_index": 1 }),
                unit_pixel_focal_length: Some(100.0),
                lens_params: BTreeMap::from([(
                    0,
                    gyro_source::LensParams {
                        focal_length: Some(31.0),
                        pixel_focal_length: Some(3100.0),
                        ..Default::default()
                    },
                )]),
                ..Default::default()
            }
            .into();
        }
        manager.lens_group_manual_edit.store(true, SeqCst);

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        configs[1].anamorphic_enabled = true;
        configs[1].squeeze_ratio = Some(1.33);
        *manager.lens_group_config.write() = configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((2554, 1080)));

        let mut configs = niyien_lens_presets::default_lens_group_configs();
        configs[1].focal_length_mm = Some(50.0);
        *manager.lens_group_config.write() = configs;

        assert_eq!(manager.apply_lens_group_to_main(1), Some((1920, 1080)));

        let lens = manager.lens.read();
        assert_eq!(lens.focal_length, Some(31.0));
        assert_eq!(lens.fisheye_params.camera_matrix[0], [3100.0, 0.0, 960.0]);
        assert_eq!(lens.fisheye_params.camera_matrix[1], [0.0, 3100.0, 540.0]);
        assert!(!lens.lens_group_override);
    }

    #[test]
    fn set_output_size_preserves_non_anamorphic_scaling_behavior() {
        assert_eq!(
            constrained_output_size((1920, 1080), (2554, 1080), 0.0, (1.0, 1.0)),
            Some((1920, 812))
        );
    }

    #[test]
    fn set_output_size_uses_anamorphic_effective_input_size() {
        assert_eq!(
            constrained_output_size((1920, 1080), (2554, 1080), 0.0, (1.33, 1.0)),
            Some((2554, 1080))
        );
    }
}
