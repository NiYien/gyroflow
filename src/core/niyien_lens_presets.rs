// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::gyro_source::{FileMetadata, LensParams};
use crate::lens_profile::{CameraParams, Dimensions, LensProfile};

pub const LENS_GROUP_COUNT: usize = 6;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SqueezeDirection {
    #[default]
    Horizontal,
    Vertical,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnamorphicPreset {
    pub id: String,
    pub name: String,
    pub focal_length_mm: Option<f64>,
    pub squeeze_ratio: f64,
    pub distortion_coeffs: Vec<f64>,
    pub distortion_model: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensGroupConfig {
    pub lens_index: usize,
    pub focal_length_mm: Option<f64>,
    pub pre_anamorphic_focal_length_mm: Option<f64>,
    pub pre_anamorphic_focal_length_captured: bool,
    pub anamorphic_enabled: bool,
    pub preset_id: Option<String>,
    pub squeeze_direction: Option<SqueezeDirection>,
    pub squeeze_ratio: Option<f64>,
    // Per-group lens correction amount in percent (0-100). None => no override, default 100.
    pub lens_correction_amount: Option<f32>,
}

/// Minimum manual focal length that is considered sensible. Below this threshold the
/// user has likely left the field empty / at a placeholder value and we fall back to
/// the auto path regardless of the global manual_edit toggle.
pub const MANUAL_FOCAL_LENGTH_MIN_MM: f64 = 5.0;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LensGroupStatus {
    pub lens_index: usize,
    pub used: bool,
    pub has_auto_focus: bool,
    pub has_missing_focus: bool,
    pub auto_focus_length_mm: Option<f64>,
    pub video_count: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FocalLengthSource {
    Auto,
    Manual,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedAnamorphic {
    pub squeeze_direction: SqueezeDirection,
    pub squeeze_ratio: f64,
    pub distortion_coeffs: Vec<f64>,
    pub distortion_model: Option<String>,
    pub lens_model_label: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct PresetIndexFile {
    version: u32,
    presets: Vec<PresetIndexEntry>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct PresetIndexEntry {
    id: String,
    name: String,
    file: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
struct PresetFile {
    name: String,
    focal_length_mm: Option<f64>,
    squeeze_ratio: f64,
    distortion_coeffs: Vec<f64>,
    distortion_model: String,
}

const BUILTIN_INDEX_JSON: &str = include_str!("../../resources/lens_presets/index.json");
const LEGACY_AIVASCOPE_PRESET_ID: &str = "aivascope_58mm_1_50x_vertical";
const CANONICAL_AIVASCOPE_PRESET_ID: &str = "aivascope_58mm_1_50x";

fn normalize_preset_id(preset_id: &str) -> &str {
    match preset_id.trim() {
        LEGACY_AIVASCOPE_PRESET_ID => CANONICAL_AIVASCOPE_PRESET_ID,
        other => other,
    }
}

static BUILTIN_PRESET_FILES: &[(&str, &str)] =
    include!(concat!(env!("OUT_DIR"), "/builtin_lens_preset_files.rs"));

fn builtin_preset_file(name: &str) -> Option<&'static str> {
    // Legacy `sirui_xingchen_*.json` aliases were never materialized as separate files;
    // index.json always pointed at the canonical `sirui_astra_*` file. The compile-time
    // table now mirrors the on-disk filenames 1:1, no aliasing needed.
    BUILTIN_PRESET_FILES
        .iter()
        .find_map(|(n, c)| (*n == name).then_some(*c))
}

pub fn default_lens_group_configs() -> Vec<LensGroupConfig> {
    (0..LENS_GROUP_COUNT)
        .map(|lens_index| LensGroupConfig {
            lens_index,
            ..Default::default()
        })
        .collect()
}

pub fn default_lens_group_statuses() -> Vec<LensGroupStatus> {
    (0..LENS_GROUP_COUNT)
        .map(|lens_index| LensGroupStatus {
            lens_index,
            ..Default::default()
        })
        .collect()
}

pub fn normalize_lens_group_configs(input: &[LensGroupConfig]) -> Vec<LensGroupConfig> {
    let mut normalized = default_lens_group_configs();
    for (position, cfg) in input.iter().enumerate() {
        let lens_index = if cfg.lens_index < LENS_GROUP_COUNT {
            cfg.lens_index
        } else if position < LENS_GROUP_COUNT {
            position
        } else {
            continue;
        };
        let mut next = cfg.clone();
        next.lens_index = lens_index;
        next.focal_length_mm = sanitize_manual_focal_length_mm(next.focal_length_mm);
        next.pre_anamorphic_focal_length_mm =
            sanitize_manual_focal_length_mm(next.pre_anamorphic_focal_length_mm);
        next.squeeze_ratio = sanitize_positive(next.squeeze_ratio);
        next.preset_id = next
            .preset_id
            .as_deref()
            .map(normalize_preset_id)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if !next.anamorphic_enabled {
            next.preset_id = None;
            next.squeeze_direction = None;
            next.squeeze_ratio = None;
            next.pre_anamorphic_focal_length_mm = None;
            next.pre_anamorphic_focal_length_captured = false;
        }
        normalized[lens_index] = next;
    }
    normalized
}

pub fn lens_group_config_to_json(configs: &[LensGroupConfig]) -> String {
    let normalized = normalize_lens_group_configs(configs);
    if !normalized.iter().any(LensGroupConfig::has_values) {
        return "[]".to_owned();
    }
    serde_json::to_string(&normalized).unwrap_or_else(|_| "[]".to_owned())
}

pub fn lens_group_status_to_json(statuses: &[LensGroupStatus]) -> String {
    let mut normalized = default_lens_group_statuses();
    for status in statuses {
        if status.lens_index < LENS_GROUP_COUNT {
            normalized[status.lens_index] = status.clone();
        }
    }
    serde_json::to_string(&normalized).unwrap_or_else(|_| "[]".to_owned())
}

pub fn lens_group_configs_from_json(json: &str) -> Vec<LensGroupConfig> {
    if json.trim().is_empty() || json.trim() == "[]" {
        return default_lens_group_configs();
    }
    match serde_json::from_str::<Vec<LensGroupConfig>>(json) {
        Ok(configs) => normalize_lens_group_configs(&configs),
        Err(err) => {
            log::warn!("Failed to parse lens group config JSON: {err}");
            default_lens_group_configs()
        }
    }
}

pub fn lens_group_statuses_from_json(json: &str) -> Vec<LensGroupStatus> {
    if json.trim().is_empty() {
        return default_lens_group_statuses();
    }
    match serde_json::from_str::<Vec<LensGroupStatus>>(json) {
        Ok(statuses) => {
            let mut normalized = default_lens_group_statuses();
            for status in statuses {
                let lens_index = status.lens_index;
                if lens_index < LENS_GROUP_COUNT {
                    normalized[lens_index] = status;
                }
            }
            normalized
        }
        Err(err) => {
            log::warn!("Failed to parse lens group status JSON: {err}");
            default_lens_group_statuses()
        }
    }
}

impl LensGroupConfig {
    pub fn has_values(&self) -> bool {
        sanitize_manual_focal_length_mm(self.focal_length_mm).is_some()
            || self.anamorphic_enabled
            || self
                .preset_id
                .as_ref()
                .map(|v| !v.is_empty())
                .unwrap_or(false)
    }
}

/// Decide whether to generate the lens profile via the lens-group path for this group.
/// Either condition is sufficient:
///   A. Fill missing focal length: video has no telemetry focal length AND the group's
///      focal length is above the MANUAL_FOCAL_LENGTH_MIN_MM sanity threshold.
///   B. Apply anamorphic: manual_edit is on AND group has anamorphic enabled. Focal
///      length can still come from telemetry in this case.
pub fn should_use_manual_config(
    manual_edit: bool,
    config: &LensGroupConfig,
    metadata: &FileMetadata,
) -> bool {
    let (fills_missing_focal, applies_anamorphic) =
        lens_group_build_decision(manual_edit, config, metadata);
    fills_missing_focal || applies_anamorphic
}

pub fn effective_lens_group_config_for_build(
    manual_edit: bool,
    config: &LensGroupConfig,
    metadata: &FileMetadata,
) -> Option<LensGroupConfig> {
    let (fills_missing_focal, applies_anamorphic) =
        lens_group_build_decision(manual_edit, config, metadata);
    if !fills_missing_focal && !applies_anamorphic {
        return None;
    }

    let mut effective = config.clone();
    if !applies_anamorphic {
        effective.anamorphic_enabled = false;
    }
    Some(effective)
}

pub fn effective_lens_correction_amount_percent(
    config: &LensGroupConfig,
    applies_anamorphic: bool,
) -> f64 {
    if applies_anamorphic {
        config
            .lens_correction_amount
            .filter(|v| v.is_finite())
            .map(|v| v.clamp(0.0, 100.0) as f64)
            .unwrap_or(100.0)
    } else {
        100.0
    }
}

fn lens_group_build_decision(
    manual_edit: bool,
    config: &LensGroupConfig,
    metadata: &FileMetadata,
) -> (bool, bool) {
    let auto_has_focal = extract_video_focus_length_mm(metadata).is_some();
    let manual_focal_sufficient =
        sanitize_manual_focal_length_mm(config.focal_length_mm).is_some();
    let fills_missing_focal = !auto_has_focal && manual_focal_sufficient;
    let applies_anamorphic = manual_edit && config.anamorphic_enabled;
    (fills_missing_focal, applies_anamorphic)
}

pub fn extract_lens_index(additional_data: &serde_json::Value) -> Option<usize> {
    additional_data
        .get("lens_index")
        .and_then(value_to_u64)
        .map(|value| value as usize)
        .filter(|value| *value < LENS_GROUP_COUNT)
}

pub fn extract_video_focus_length_mm(metadata: &FileMetadata) -> Option<f64> {
    metadata
        .lens_params
        .values()
        .find_map(|params| {
            params
                .focal_length
                .map(|value| value as f64)
                .and_then(|value| sanitize_video_focal_length_mm(Some(value)))
        })
        .or_else(|| {
            sanitize_video_focal_length_mm(
                metadata
                    .camera_identifier
                    .as_ref()
                    .and_then(|id| id.focal_length),
            )
        })
}

pub fn update_status_from_metadata(statuses: &mut [LensGroupStatus], metadata: &FileMetadata) {
    let Some(lens_index) = extract_lens_index(&metadata.additional_data) else {
        return;
    };
    let Some(status) = statuses.get_mut(lens_index) else {
        return;
    };
    status.used = true;
    status.video_count += 1;
    if let Some(focal_length_mm) = extract_video_focus_length_mm(metadata) {
        status.has_auto_focus = true;
        if status.auto_focus_length_mm.is_none() {
            status.auto_focus_length_mm = Some(focal_length_mm);
        }
    } else {
        status.has_missing_focus = true;
    }
}

pub fn select_focal_length(
    auto_focus_length_mm: Option<f64>,
    config: Option<&LensGroupConfig>,
) -> Option<(f64, FocalLengthSource)> {
    let manual_focus_length_mm =
        config.and_then(|cfg| sanitize_manual_focal_length_mm(cfg.focal_length_mm));
    if let Some(value) = manual_focus_length_mm {
        return Some((value, FocalLengthSource::Manual));
    }
    if let Some(value) = sanitize_video_focal_length_mm(auto_focus_length_mm) {
        return Some((value, FocalLengthSource::Auto));
    }
    None
}

pub fn apply_focal_length_fallback_to_metadata(metadata: &mut FileMetadata, focal_length_mm: f64) {
    let Some(focal_length_mm) = sanitize_manual_focal_length_mm(Some(focal_length_mm)) else {
        return;
    };
    let pixel_focal_length = metadata
        .unit_pixel_focal_length
        .map(|upfl| (focal_length_mm * upfl) as f32);

    if metadata.lens_params.is_empty() {
        metadata.lens_params.insert(
            0,
            LensParams {
                focal_length: Some(focal_length_mm as f32),
                pixel_focal_length,
                ..Default::default()
            },
        );
        return;
    }

    for params in metadata.lens_params.values_mut() {
        params.focal_length = Some(focal_length_mm as f32);
        params.pixel_focal_length = pixel_focal_length;
    }
}

pub fn build_camera_matrix(
    focal_length_mm: f64,
    unit_pixel_focal_length: Option<f64>,
    size: (usize, usize),
) -> Option<Vec<[f64; 3]>> {
    let upfl = sanitize_positive(unit_pixel_focal_length)?;
    let fx = focal_length_mm * upfl;
    build_camera_matrix_from_params(fx, fx, size.0 as f64 / 2.0, size.1 as f64 / 2.0)
}

fn build_camera_matrix_from_params(fx: f64, fy: f64, cx: f64, cy: f64) -> Option<Vec<[f64; 3]>> {
    if !fx.is_finite() || !fy.is_finite() || fx <= 0.0 || fy <= 0.0 {
        return None;
    }
    Some(vec![[fx, 0.0, cx], [0.0, fy, cy], [0.0, 0.0, 1.0]])
}

fn even_dimension(value: f64) -> usize {
    let rounded = value.round().max(2.0) as usize;
    if rounded % 2 == 0 {
        rounded
    } else {
        rounded - 1
    }
}

pub fn resolve_anamorphic_config(config: Option<&LensGroupConfig>) -> Option<ResolvedAnamorphic> {
    let config = config?;
    if !config.anamorphic_enabled {
        return None;
    }
    let squeeze_direction = config.squeeze_direction.unwrap_or_default();

    if let Some(preset_id) = config
        .preset_id
        .as_deref()
        .map(normalize_preset_id)
        .filter(|value| !value.is_empty())
    {
        if let Some(preset) = find_preset_by_id(preset_id) {
            return Some(ResolvedAnamorphic {
                squeeze_direction,
                squeeze_ratio: preset.squeeze_ratio,
                distortion_coeffs: preset.distortion_coeffs,
                distortion_model: Some(preset.distortion_model),
                lens_model_label: Some(preset.name),
            });
        }
        log::warn!("Anamorphic preset not found: {preset_id}");
    }

    let squeeze_ratio = sanitize_positive(config.squeeze_ratio)?;
    Some(ResolvedAnamorphic {
        squeeze_direction,
        squeeze_ratio,
        distortion_coeffs: Vec::new(),
        distortion_model: None,
        lens_model_label: Some(manual_anamorphic_label(squeeze_ratio, squeeze_direction)),
    })
}

fn find_preset_by_id(preset_id: &str) -> Option<AnamorphicPreset> {
    load_presets()
        .into_iter()
        .find(|preset| preset.id == preset_id)
        .or_else(|| {
            load_builtin_presets()
                .into_iter()
                .find(|preset| preset.id == preset_id)
        })
}

fn find_preset_by_name(name: &str) -> Option<AnamorphicPreset> {
    let normalized_name = name.trim();
    if normalized_name.is_empty() {
        return None;
    }
    load_presets()
        .into_iter()
        .find(|preset| preset.name == normalized_name)
        .or_else(|| {
            load_builtin_presets()
                .into_iter()
                .find(|preset| preset.name == normalized_name)
        })
}

pub fn effective_anamorphic_label(config: Option<&LensGroupConfig>) -> Option<String> {
    resolve_anamorphic_config(config).and_then(|anamorphic| anamorphic.lens_model_label)
}

pub fn lens_group_config_from_lens_profile(
    profile: &LensProfile,
    lens_index: usize,
) -> Option<LensGroupConfig> {
    if lens_index >= LENS_GROUP_COUNT {
        return None;
    }

    let horizontal_stretch =
        sanitize_positive(Some(profile.input_horizontal_stretch)).unwrap_or(1.0);
    let vertical_stretch = sanitize_positive(Some(profile.input_vertical_stretch)).unwrap_or(1.0);
    let (anamorphic_enabled, squeeze_direction, squeeze_ratio) = if horizontal_stretch > 1.01 {
        (
            true,
            Some(SqueezeDirection::Horizontal),
            Some(horizontal_stretch),
        )
    } else if vertical_stretch > 1.01 {
        (true, Some(SqueezeDirection::Vertical), Some(vertical_stretch))
    } else {
        return None;
    };

    let preset_id = find_preset_by_name(&profile.lens_model).map(|preset| preset.id);
    let manual_label = is_manual_anamorphic_label(&profile.lens_model);
    if preset_id.is_none() && !manual_label {
        return None;
    }

    let mut config = LensGroupConfig {
        lens_index,
        focal_length_mm: sanitize_manual_focal_length_mm(profile.focal_length),
        pre_anamorphic_focal_length_mm: None,
        pre_anamorphic_focal_length_captured: false,
        anamorphic_enabled,
        preset_id,
        squeeze_direction,
        squeeze_ratio,
        lens_correction_amount: None,
    };

    if config.anamorphic_enabled && config.preset_id.is_none() && config.squeeze_ratio.is_none() {
        config.anamorphic_enabled = false;
        config.squeeze_direction = None;
    }

    if config.anamorphic_enabled {
        Some(config)
    } else {
        None
    }
}

pub fn build_lens_profile(
    metadata: &FileMetadata,
    size: (usize, usize),
    config: Option<&LensGroupConfig>,
    fallback_lens: Option<&LensProfile>,
) -> Option<LensProfile> {
    let auto_focus_length_mm = extract_video_focus_length_mm(metadata);
    let Some((focal_length_mm, _)) = select_focal_length(auto_focus_length_mm, config)
    else {
        return None;
    };

    // Fallback chain for unit_pixel_focal_length:
    //   1. metadata.unit_pixel_focal_length (telemetry-parser path)
    //   2. derive from fallback_lens.fisheye_params.camera_matrix[0][0] / fallback_lens.focal_length
    //      (recovers .gyroflow load path where the embedded file_metadata cbor lacks upfl)
    let metadata_upfl = sanitize_positive(metadata.unit_pixel_focal_length);
    let derived_upfl = if metadata_upfl.is_none() {
        fallback_lens.and_then(|fb| {
            let fx = fb.fisheye_params.camera_matrix.first()
                .and_then(|row| row.first())
                .copied()?;
            let focal = fb.focal_length?;
            // Anamorphic squeeze does not affect the relationship fx_sensor = focal * upfl
            // because LensProfile.fisheye_params.camera_matrix stores sensor-space fx
            // (build_lens_profile writes fx = focal * upfl regardless of stretch).
            sanitize_positive(Some(fx / focal))
        })
    } else {
        None
    };
    let Some(upfl) = metadata_upfl.or(derived_upfl) else {
        return None;
    };
    if metadata_upfl.is_none() {
        log::info!(target: "lens", "lens-group: upfl fallback-derived from fallback_lens (metadata had None): {upfl}");
    }
    let base_focal_px = focal_length_mm * upfl;

    let mut profile = fallback_lens.cloned().unwrap_or_default();
    populate_profile_metadata(&mut profile, metadata, fallback_lens, size);
    profile.lens_group_override = config.is_some();
    profile.focal_length = Some(focal_length_mm);
    profile.input_horizontal_stretch = 1.0;
    profile.input_vertical_stretch = 1.0;
    if fallback_lens.is_none() {
        profile.fisheye_params = CameraParams {
            RMS_error: 0.0,
            camera_matrix: Vec::new(),
            distortion_coeffs: Vec::new(),
            radial_distortion_limit: None,
        };
        profile.distortion_model = None;
    }

    let mut calib_dimension = Dimensions {
        w: size.0,
        h: size.1,
    };
    let mut orig_dimension = Dimensions {
        w: size.0,
        h: size.1,
    };
    let mut output_dimension = None;
    let fx = base_focal_px;
    let fy = base_focal_px;
    let mut cx = size.0 as f64 / 2.0;
    let mut cy = size.1 as f64 / 2.0;

    if let Some(anamorphic) = resolve_anamorphic_config(config) {
        match anamorphic.squeeze_direction {
            SqueezeDirection::Horizontal => {
                profile.input_horizontal_stretch = anamorphic.squeeze_ratio;
                let stretched_w = even_dimension(size.0 as f64 * anamorphic.squeeze_ratio);
                calib_dimension.w = stretched_w;
                orig_dimension.w = stretched_w;
                output_dimension = Some(Dimensions {
                    w: stretched_w,
                    h: size.1,
                });
                cx = stretched_w as f64 / 2.0;
                cy = size.1 as f64 / 2.0;
            }
            SqueezeDirection::Vertical => {
                profile.input_vertical_stretch = anamorphic.squeeze_ratio;
                let stretched_h = even_dimension(size.1 as f64 * anamorphic.squeeze_ratio);
                calib_dimension.h = stretched_h;
                orig_dimension.h = stretched_h;
                output_dimension = Some(Dimensions {
                    w: size.0,
                    h: stretched_h,
                });
                cx = size.0 as f64 / 2.0;
                cy = stretched_h as f64 / 2.0;
            }
        }
        if !anamorphic.distortion_coeffs.is_empty() || anamorphic.distortion_model.is_some() {
            profile.fisheye_params.distortion_coeffs = anamorphic.distortion_coeffs;
            // Empty distortion_model behaves like a missing field: do not override the
            // upstream profile's value (fallback lens / built-in defaults stay in effect).
            if let Some(model) = anamorphic.distortion_model.filter(|s| !s.is_empty()) {
                profile.distortion_model = Some(model);
            }
        }
        if let Some(label) = anamorphic.lens_model_label {
            profile.lens_model = label;
        }
    }

    profile.fisheye_params.camera_matrix = build_camera_matrix_from_params(fx, fy, cx, cy)?;
    profile.calib_dimension = calib_dimension;
    profile.orig_dimension = orig_dimension;
    profile.output_dimension = output_dimension;
    profile.init();
    Some(profile)
}

/// Load lens presets with four-level priority:
///   P1: lens update package `<data_dir>/lens/versions/<N>/lens_presets/`
///       (directory presence means full override)
///   P2: `<data_dir>/lens_presets/` (user local override layer; same id wins + new ids allowed)
///       + `<data_dir>/anamorphic_presets/` (legacy path for existing user files)
///   P3: `<exe>/lens_presets/` (portable build path)
///   P4: compile-time `include_str!` built-in snapshot (offline fallback)
///
/// The last three layers merge as "later same id overrides earlier", so final
/// priority is P2(new) > P2(legacy) > P3 > P4.
pub fn load_presets() -> Vec<AnamorphicPreset> {
    // P1: lens update package, full-directory override.
    if let Some(pkg_presets) = load_from_lens_package() {
        return pkg_presets;
    }

    // P4: built-in base.
    let mut presets = load_builtin_presets();

    // P3: portable path next to the executable.
    if let Some(exe_presets_dir) = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("lens_presets")))
    {
        merge_presets(&mut presets, load_from_user_dir(&exe_presets_dir));
    }

    // P2-legacy: keep existing user custom files from the old path.
    merge_presets(
        &mut presets,
        load_from_user_dir(&settings_dir().join("anamorphic_presets")),
    );

    // P2-new: new user path; same ids override the legacy path.
    merge_presets(
        &mut presets,
        load_from_user_dir(&settings_dir().join("lens_presets")),
    );

    presets
}

pub fn load_presets_json() -> String {
    serde_json::to_string(&load_presets()).unwrap_or_else(|_| "[]".to_owned())
}

fn load_builtin_presets() -> Vec<AnamorphicPreset> {
    load_presets_from_index(BUILTIN_INDEX_JSON, None, true)
}

/// P1: load from the currently active lens update package.
/// Existing directory with valid `index.json` -> Some(presets), fully overriding
/// later layers. Missing directory/index or parse failure -> None.
fn load_from_lens_package() -> Option<Vec<AnamorphicPreset>> {
    let dir = crate::distribution::resolve_package_subdir("lens", "lens_presets")?;
    let index_path = dir.join("index.json");
    if !index_path.is_file() {
        log::debug!(
            "lens package has lens_presets/ but no index.json at {}",
            index_path.display()
        );
        return None;
    }
    match std::fs::read_to_string(&index_path) {
        Ok(index_json) => {
            // Logged once per process — this function is called per-frame from
            // lens-group reapply paths, so repeated emission was the single
            // noisiest line in the log. Subsequent calls are completely silent.
            static LOGGED_DIR: std::sync::OnceLock<()> = std::sync::OnceLock::new();
            if LOGGED_DIR.set(()).is_ok() {
                log::info!(
                    "loading lens presets from lens package at {}",
                    dir.display()
                );
            }
            let presets = load_presets_from_index(&index_json, Some(&dir), false);
            if presets.is_empty() {
                log::warn!(
                    "lens package lens_presets/index.json parsed but yielded 0 presets; \
                     falling back to built-in"
                );
                None
            } else {
                Some(presets)
            }
        }
        Err(err) => {
            log::warn!(
                "lens package lens_presets/index.json unreadable ({}): {err}; falling back to built-in",
                index_path.display()
            );
            None
        }
    }
}

/// Shared user-directory loader for P2/P3. Prefer index.json; scan the directory
/// when no index exists.
fn load_from_user_dir(root: &Path) -> Vec<AnamorphicPreset> {
    if !root.exists() {
        return Vec::new();
    }
    let index_path = root.join("index.json");
    if index_path.is_file() {
        match std::fs::read_to_string(&index_path) {
            Ok(index_json) => load_presets_from_index(&index_json, Some(root), false),
            Err(err) => {
                log::warn!(
                    "Failed to read lens preset index {}: {err}",
                    index_path.display()
                );
                Vec::new()
            }
        }
    } else {
        load_presets_from_dir_without_index(root)
    }
}

fn load_presets_from_index(
    index_json: &str,
    root: Option<&Path>,
    built_in: bool,
) -> Vec<AnamorphicPreset> {
    let index = match serde_json::from_str::<PresetIndexFile>(index_json) {
        Ok(index) => index,
        Err(err) => {
            log::warn!("Failed to parse anamorphic preset index: {err}");
            return Vec::new();
        }
    };
    if index.version == 0 {
        log::warn!("Anamorphic preset index version is missing or invalid");
    }

    let mut presets = Vec::new();
    for entry in index.presets {
        let source = if built_in {
            builtin_preset_file(&entry.file).map(|contents| contents.to_owned())
        } else {
            root.and_then(|dir| std::fs::read_to_string(dir.join(&entry.file)).ok())
        };

        let Some(contents) = source else {
            log::warn!(
                "Anamorphic preset file is missing: {}",
                format_source_path(root, &entry.file, built_in)
            );
            continue;
        };

        if let Some(preset) = parse_preset_file(&entry.id, &entry.name, &contents) {
            presets.push(preset);
        }
    }
    presets
}

fn load_presets_from_dir_without_index(root: &Path) -> Vec<AnamorphicPreset> {
    let mut presets = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) => {
            log::warn!(
                "Failed to read anamorphic preset directory {}: {err}",
                root.display()
            );
            return presets;
        }
    };

    let mut files = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter(|entry| entry.file_name() != "index.json")
        .collect::<Vec<_>>();
    files.sort_by_key(|entry| entry.file_name());

    for entry in files {
        let path = entry.path();
        match std::fs::read_to_string(&path) {
            Ok(contents) => {
                let id = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or_default()
                    .to_owned();
                if let Some(preset) = parse_preset_file(&id, "", &contents) {
                    presets.push(preset);
                }
            }
            Err(err) => {
                log::warn!("Failed to read anamorphic preset {}: {err}", path.display());
            }
        }
    }
    presets
}

fn parse_preset_file(id: &str, fallback_name: &str, contents: &str) -> Option<AnamorphicPreset> {
    let parsed = match serde_json::from_str::<PresetFile>(contents) {
        Ok(parsed) => parsed,
        Err(err) => {
            log::warn!("Failed to parse anamorphic preset {id}: {err}");
            return None;
        }
    };

    let name = if parsed.name.trim().is_empty() {
        fallback_name.to_owned()
    } else {
        parsed.name
    };

    // `distortion_model == ""` is accepted verbatim: `DistortionModel::from_name("")` falls
    // through to `DistortionModel::default()`, so the value travels the chain without
    // normalization and current runtime behavior matches `"opencv_fisheye"`.
    if name.trim().is_empty()
        || parsed.squeeze_ratio <= 0.0
        || parsed.distortion_coeffs.len() != 4
    {
        log::warn!("Anamorphic preset {id} is missing required fields");
        return None;
    }

    let focal_length_mm = sanitize_manual_focal_length_mm(parsed.focal_length_mm)
        .or_else(|| preset_focal_length_from_text(&name))
        .or_else(|| preset_focal_length_from_text(id));

    Some(AnamorphicPreset {
        id: normalize_preset_id(id).to_owned(),
        name,
        focal_length_mm,
        squeeze_ratio: parsed.squeeze_ratio,
        distortion_coeffs: parsed.distortion_coeffs,
        distortion_model: parsed.distortion_model,
    })
}

fn merge_presets(target: &mut Vec<AnamorphicPreset>, incoming: Vec<AnamorphicPreset>) {
    let mut index_map: HashMap<String, usize> = target
        .iter()
        .enumerate()
        .map(|(index, preset)| (preset.id.clone(), index))
        .collect();

    for preset in incoming {
        if let Some(index) = index_map.get(&preset.id).copied() {
            target[index] = preset;
        } else {
            index_map.insert(preset.id.clone(), target.len());
            target.push(preset);
        }
    }
}

fn populate_profile_metadata(
    profile: &mut LensProfile,
    metadata: &FileMetadata,
    fallback_lens: Option<&LensProfile>,
    size: (usize, usize),
) {
    if let Some(fallback) = fallback_lens {
        profile.camera_brand = fallback.camera_brand.clone();
        profile.camera_model = fallback.camera_model.clone();
        profile.lens_model = fallback.lens_model.clone();
        profile.camera_setting = fallback.camera_setting.clone();
        profile.calibrated_by = fallback.calibrated_by.clone();
        profile.frame_readout_time = fallback.frame_readout_time;
        profile.frame_readout_direction = fallback.frame_readout_direction;
        profile.global_shutter = fallback.global_shutter;
        profile.crop_factor = fallback.crop_factor;
    }

    if profile.camera_brand.is_empty() || profile.camera_model.is_empty() {
        if let Some(camera_identifier) = &metadata.camera_identifier {
            if profile.camera_brand.is_empty() {
                profile.camera_brand = camera_identifier.brand.clone();
            }
            if profile.camera_model.is_empty() {
                profile.camera_model = camera_identifier.model.clone();
            }
            if profile.lens_model.is_empty() && !camera_identifier.lens_model.is_empty() {
                profile.lens_model = camera_identifier.lens_model.clone();
            }
        } else if let Some(detected) = metadata.detected_source.as_deref() {
            let mut parts = detected.splitn(2, ' ');
            if profile.camera_brand.is_empty() {
                profile.camera_brand = parts.next().unwrap_or_default().to_owned();
            }
            if profile.camera_model.is_empty() {
                profile.camera_model = parts.next().unwrap_or_default().to_owned();
            }
        }
    }

    if profile.calibrated_by.is_empty() {
        profile.calibrated_by = "NiYien".to_owned();
    }
    if profile.frame_readout_time.is_none() {
        profile.frame_readout_time = metadata.frame_readout_time;
    }
    if profile.frame_readout_direction.is_none() && profile.frame_readout_time.is_some() {
        profile.frame_readout_direction = Some(metadata.frame_readout_direction);
    }

    profile.calib_dimension = Dimensions {
        w: size.0,
        h: size.1,
    };
    profile.orig_dimension = Dimensions {
        w: size.0,
        h: size.1,
    };
    profile.output_dimension = None;
    profile.official = true;
    profile.asymmetrical = false;
}

fn sanitize_positive(value: Option<f64>) -> Option<f64> {
    match value {
        Some(value) if value.is_finite() && value > 0.0 => Some(value),
        _ => None,
    }
}

fn sanitize_manual_focal_length_mm(value: Option<f64>) -> Option<f64> {
    match value {
        Some(value) if value.is_finite() && value > MANUAL_FOCAL_LENGTH_MIN_MM => Some(value),
        _ => None,
    }
}

fn sanitize_video_focal_length_mm(value: Option<f64>) -> Option<f64> {
    sanitize_positive(value)
}

fn preset_focal_length_from_text(text: &str) -> Option<f64> {
    for part in text.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '.') {
        let Some(number) = part.strip_suffix("mm") else {
            continue;
        };
        if number.is_empty() {
            continue;
        }
        if let Ok(value) = number.parse::<f64>() {
            if let Some(value) = sanitize_manual_focal_length_mm(Some(value)) {
                return Some(value);
            }
        }
    }
    None
}

fn manual_anamorphic_label(squeeze_ratio: f64, direction: SqueezeDirection) -> String {
    let direction = match direction {
        SqueezeDirection::Horizontal => "H",
        SqueezeDirection::Vertical => "V",
    };
    format!("Manual anamorphic {squeeze_ratio:.2}x {direction}")
}

fn is_manual_anamorphic_label(label: &str) -> bool {
    let mut parts = label.split_whitespace();
    matches!(parts.next(), Some("Manual"))
        && matches!(parts.next(), Some("anamorphic"))
        && matches!(parts.next(), Some(ratio) if ratio.ends_with('x'))
        && matches!(parts.next(), Some("H" | "V"))
        && parts.next().is_none()
}

fn value_to_u64(value: &serde_json::Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| {
            value
                .as_i64()
                .filter(|value| *value >= 0)
                .map(|value| value as u64)
        })
        .or_else(|| value.as_str().and_then(|value| value.parse::<u64>().ok()))
}

fn format_source_path(root: Option<&Path>, file: &str, built_in: bool) -> String {
    if built_in {
        format!("builtin:{file}")
    } else {
        root.map(|root| root.join(file).display().to_string())
            .unwrap_or_else(|| file.to_owned())
    }
}

fn settings_dir() -> PathBuf {
    crate::settings::data_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    const BUILTIN_1_50X_TEST_PRESET_ID: &str = "blazar_viper_35mm_1_50x";

    #[test]
    fn camera_matrix_uses_unit_pixel_focal_length() {
        let matrix = build_camera_matrix(35.0, Some(107.0), (3840, 2160)).unwrap();
        assert_eq!(matrix[0], [3745.0, 0.0, 1920.0]);
        assert_eq!(matrix[1], [0.0, 3745.0, 1080.0]);
        assert_eq!(matrix[2], [0.0, 0.0, 1.0]);
    }

    #[test]
    fn focal_length_prefers_manual_when_present() {
        let selected = select_focal_length(
            Some(35.0),
            Some(&LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(50.0),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(selected.0, 50.0);
        assert_eq!(selected.1, FocalLengthSource::Manual);
    }

    #[test]
    fn focal_length_uses_auto_when_no_manual_value() {
        let selected = select_focal_length(
            Some(35.0),
            Some(&LensGroupConfig {
                lens_index: 0,
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(selected.0, 35.0);
        assert_eq!(selected.1, FocalLengthSource::Auto);
    }

    #[test]
    fn focal_length_falls_back_to_manual_when_auto_missing() {
        let selected = select_focal_length(
            None,
            Some(&LensGroupConfig {
                lens_index: 0,
                focal_length_mm: Some(28.0),
                ..Default::default()
            }),
        )
        .unwrap();
        assert_eq!(selected.0, 28.0);
        assert_eq!(selected.1, FocalLengthSource::Manual);
    }

    #[test]
    fn manual_config_fills_missing_focal_without_manual_edit() {
        let metadata = FileMetadata {
            unit_pixel_focal_length: Some(100.0),
            ..Default::default()
        };
        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(28.0),
            ..Default::default()
        };

        assert!(should_use_manual_config(false, &config, &metadata));
    }

    #[test]
    fn manual_config_fills_missing_focal_when_only_pixel_focal_exists() {
        let mut metadata = FileMetadata {
            unit_pixel_focal_length: Some(100.0),
            ..Default::default()
        };
        metadata.lens_params.insert(
            0,
            LensParams {
                pixel_focal_length: Some(3100.0),
                ..Default::default()
            },
        );
        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(28.0),
            ..Default::default()
        };

        assert!(should_use_manual_config(false, &config, &metadata));
    }

    #[test]
    fn effective_config_fills_focal_without_anamorphic_when_manual_edit_off() {
        let metadata = FileMetadata {
            unit_pixel_focal_length: Some(100.0),
            ..Default::default()
        };
        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(28.0),
            anamorphic_enabled: true,
            squeeze_ratio: Some(1.33),
            ..Default::default()
        };

        let effective =
            effective_lens_group_config_for_build(false, &config, &metadata).unwrap();

        assert_eq!(effective.focal_length_mm, Some(28.0));
        assert!(!effective.anamorphic_enabled);
    }

    #[test]
    fn lens_correction_amount_only_applies_with_effective_anamorphic() {
        let config = LensGroupConfig {
            lens_correction_amount: Some(42.0),
            ..Default::default()
        };

        assert_eq!(effective_lens_correction_amount_percent(&config, true), 42.0);
        assert_eq!(effective_lens_correction_amount_percent(&config, false), 100.0);
    }

    #[test]
    fn manual_config_requires_manual_edit_for_anamorphic_when_focal_exists() {
        let mut metadata = FileMetadata {
            unit_pixel_focal_length: Some(100.0),
            ..Default::default()
        };
        metadata.lens_params.insert(
            0,
            LensParams {
                focal_length: Some(31.0),
                pixel_focal_length: Some(3100.0),
                ..Default::default()
            },
        );
        let config = LensGroupConfig {
            lens_index: 0,
            anamorphic_enabled: true,
            squeeze_ratio: Some(1.33),
            ..Default::default()
        };

        assert!(!should_use_manual_config(false, &config, &metadata));
        assert!(should_use_manual_config(true, &config, &metadata));
    }

    #[test]
    fn manual_config_ignores_additional_focus_length_without_video_focal() {
        let metadata = FileMetadata {
            additional_data: serde_json::json!({ "focus_length": 310 }),
            unit_pixel_focal_length: Some(100.0),
            ..Default::default()
        };
        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(50.0),
            ..Default::default()
        };

        assert_eq!(extract_video_focus_length_mm(&metadata), None);
        assert!(should_use_manual_config(true, &config, &metadata));
    }

    #[test]
    fn manual_config_keeps_small_auto_focus_without_anamorphic() {
        let mut metadata = FileMetadata {
            unit_pixel_focal_length: Some(100.0),
            ..Default::default()
        };
        metadata.lens_params.insert(
            0,
            LensParams {
                focal_length: Some(3.5),
                ..Default::default()
            },
        );
        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(28.0),
            ..Default::default()
        };

        assert_eq!(extract_video_focus_length_mm(&metadata), Some(3.5));
        assert!(!should_use_manual_config(true, &config, &metadata));
        assert_eq!(select_focal_length(Some(3.5), None).unwrap().0, 3.5);
    }

    #[test]
    fn lens_group_config_json_ignores_legacy_manual_override_field() {
        let parsed = lens_group_configs_from_json(
            r#"[{"lens_index":0,"focal_length_mm":35.0,"manual_override_enabled":true}]"#,
        );
        assert_eq!(parsed.len(), LENS_GROUP_COUNT);
        assert_eq!(parsed[0].focal_length_mm, Some(35.0));
    }

    #[test]
    fn extracts_video_focus_length_from_lens_params() {
        let mut metadata = FileMetadata::default();
        metadata.lens_params.insert(
            0,
            crate::gyro_source::LensParams {
                focal_length: Some(35.0),
                ..Default::default()
            },
        );

        assert_eq!(extract_video_focus_length_mm(&metadata), Some(35.0));
    }

    #[test]
    fn extracts_video_focus_length_from_any_lens_param() {
        let mut metadata = FileMetadata::default();
        metadata.lens_params.insert(
            0,
            crate::gyro_source::LensParams {
                pixel_focal_length: Some(3100.0),
                ..Default::default()
            },
        );
        metadata.lens_params.insert(
            10,
            crate::gyro_source::LensParams {
                focal_length: Some(35.0),
                ..Default::default()
            },
        );

        assert_eq!(extract_video_focus_length_mm(&metadata), Some(35.0));
    }

    #[test]
    fn extract_video_focus_length_ignores_additional_focus_length() {
        let mut metadata = FileMetadata::default();
        metadata.additional_data = serde_json::json!({
            "focus_length": 300
        });
        metadata.lens_params.insert(
            0,
            crate::gyro_source::LensParams {
                focal_length: Some(35.0),
                ..Default::default()
            },
        );

        assert_eq!(extract_video_focus_length_mm(&metadata), Some(35.0));
    }

    #[test]
    fn manual_anamorphic_only_sets_stretch() {
        let config = LensGroupConfig {
            lens_index: 0,
            anamorphic_enabled: true,
            squeeze_direction: Some(SqueezeDirection::Horizontal),
            squeeze_ratio: Some(1.5),
            ..Default::default()
        };
        let resolved = resolve_anamorphic_config(Some(&config)).unwrap();
        assert_eq!(resolved.squeeze_direction, SqueezeDirection::Horizontal);
        assert_eq!(resolved.squeeze_ratio, 1.5);
        assert!(resolved.distortion_coeffs.is_empty());
        assert_eq!(resolved.distortion_model, None);
    }

    #[test]
    fn preset_anamorphic_uses_builtin_coeffs() {
        let config = LensGroupConfig {
            lens_index: 0,
            anamorphic_enabled: true,
            preset_id: Some(BUILTIN_1_50X_TEST_PRESET_ID.to_owned()),
            squeeze_direction: Some(SqueezeDirection::Horizontal),
            ..Default::default()
        };
        let resolved = resolve_anamorphic_config(Some(&config)).unwrap();
        assert_eq!(resolved.squeeze_direction, SqueezeDirection::Horizontal);
        assert_eq!(resolved.squeeze_ratio, 1.5);
        assert_eq!(resolved.distortion_coeffs.len(), 4);
        assert!(resolved.distortion_model.is_some());
    }

    #[test]
    fn parses_preset_without_squeeze_direction() {
        let preset = parse_preset_file(
            "demo_preset",
            "",
            r#"{
                "name": "Demo",
                "focal_length_mm": 42.0,
                "squeeze_ratio": 1.5,
                "distortion_coeffs": [0.1, 0.2, 0.3, 0.4],
                "distortion_model": "opencv_fisheye"
            }"#,
        )
        .unwrap();

        assert_eq!(preset.id, "demo_preset");
        assert_eq!(preset.name, "Demo");
        assert_eq!(preset.focal_length_mm, Some(42.0));
        assert_eq!(preset.squeeze_ratio, 1.5);
        assert_eq!(preset.distortion_coeffs, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(preset.distortion_model, "opencv_fisheye");
    }

    #[test]
    fn parses_preset_focal_length_from_name_when_field_missing() {
        let preset = parse_preset_file(
            "blazar_mantis_25mm_1_33x",
            "",
            r#"{
                "name": "Blazar Mantis 25mm 1.33x",
                "squeeze_ratio": 1.33,
                "distortion_coeffs": [0.1, 0.2, 0.3, 0.4],
                "distortion_model": ""
            }"#,
        )
        .unwrap();

        assert_eq!(preset.focal_length_mm, Some(25.0));
    }

    #[test]
    fn parses_preset_focal_length_from_id_when_name_has_no_mm() {
        let preset = parse_preset_file(
            "blazar_viper_75mm_1_50x",
            "",
            r#"{
                "name": "Custom preset",
                "squeeze_ratio": 1.5,
                "distortion_coeffs": [0.1, 0.2, 0.3, 0.4],
                "distortion_model": ""
            }"#,
        )
        .unwrap();

        assert_eq!(preset.focal_length_mm, Some(75.0));
    }

    #[test]
    fn lens_group_config_json_preserves_pre_anamorphic_focal_length() {
        let configs = lens_group_configs_from_json(
            r#"[{
                "lens_index": 2,
                "focal_length_mm": 35.0,
                "anamorphic_enabled": true,
                "preset_id": "blazar_viper_75mm_1_50x",
                "squeeze_direction": "horizontal",
                "squeeze_ratio": 1.5,
                "pre_anamorphic_focal_length_mm": 31.0,
                "pre_anamorphic_focal_length_captured": true
            }]"#,
        );

        assert_eq!(configs[2].pre_anamorphic_focal_length_mm, Some(31.0));
        assert!(configs[2].pre_anamorphic_focal_length_captured);

        let value: serde_json::Value =
            serde_json::from_str(&lens_group_config_to_json(&configs)).unwrap();
        assert_eq!(value[2]["pre_anamorphic_focal_length_mm"], 31.0);
        assert_eq!(value[2]["pre_anamorphic_focal_length_captured"], true);
    }

    #[test]
    fn parses_preset_with_empty_distortion_model() {
        let preset = parse_preset_file(
            "demo_empty_model",
            "",
            r#"{
                "name": "Demo Empty",
                "squeeze_ratio": 1.33,
                "distortion_coeffs": [0.0, 0.8, -1.1, 0.0],
                "distortion_model": ""
            }"#,
        )
        .unwrap();

        assert_eq!(preset.id, "demo_empty_model");
        assert_eq!(preset.name, "Demo Empty");
        assert_eq!(preset.squeeze_ratio, 1.33);
        assert_eq!(preset.distortion_coeffs, vec![0.0, 0.8, -1.1, 0.0]);
        assert_eq!(preset.distortion_model, "");
    }

    #[test]
    fn parses_preset_without_distortion_model_field() {
        let preset = parse_preset_file(
            "demo_no_model_field",
            "",
            r#"{
                "name": "Demo Missing",
                "squeeze_ratio": 1.5,
                "distortion_coeffs": [0.1, 0.2, 0.3, 0.4]
            }"#,
        )
        .unwrap();

        assert_eq!(preset.distortion_model, String::new());
    }

    #[test]
    fn rejects_preset_with_invalid_coeffs_still() {
        let preset = parse_preset_file(
            "demo_bad_coeffs",
            "",
            r#"{
                "name": "Demo Bad",
                "squeeze_ratio": 1.5,
                "distortion_coeffs": [0.1, 0.2],
                "distortion_model": ""
            }"#,
        );
        assert!(preset.is_none());
    }

    #[test]
    fn parses_legacy_preset_with_squeeze_direction_ignored() {
        let preset = parse_preset_file(
            LEGACY_AIVASCOPE_PRESET_ID,
            "",
            r#"{
                "name": "Legacy Demo",
                "squeeze_direction": "vertical",
                "squeeze_ratio": 1.5,
                "distortion_coeffs": [0.1, 0.2, 0.3, 0.4],
                "distortion_model": "opencv_fisheye"
            }"#,
        )
        .unwrap();

        assert_eq!(preset.id, CANONICAL_AIVASCOPE_PRESET_ID);
        assert_eq!(preset.name, "Legacy Demo");
        assert_eq!(preset.squeeze_ratio, 1.5);
        assert_eq!(preset.distortion_coeffs, vec![0.1, 0.2, 0.3, 0.4]);
        assert_eq!(preset.distortion_model, "opencv_fisheye");
    }

    #[test]
    fn preset_with_empty_model_preserves_empty_string() {
        // Exercise the disk -> AnamorphicPreset -> ResolvedAnamorphic.distortion_model
        // chain end-to-end via load_presets_from_index, the shared loader used by every
        // disk-backed path (P1/P2/P3). resolve_anamorphic_config takes its preset from the
        // same source, so showing the empty string survives load_presets_from_index is
        // equivalent to showing it survives resolve_anamorphic_config's mapping at line 439.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("preset_a.json"),
            r#"{
                "name": "Demo Empty",
                "squeeze_ratio": 1.33,
                "distortion_coeffs": [0.0, 0.8, -1.1, 0.0],
                "distortion_model": ""
            }"#,
        )
        .unwrap();
        let index = r#"{"version": 1, "presets": [{"id": "demo_empty", "name": "Demo Empty", "file": "preset_a.json"}]}"#;
        let presets = load_presets_from_index(index, Some(dir.path()), false);
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].distortion_model, "");

        // Mirror the single mapping at resolve_anamorphic_config line 439 to lock the
        // "no normalization" property without having to monkey-patch global preset lookup.
        let preset = presets.into_iter().next().unwrap();
        let resolved = ResolvedAnamorphic {
            squeeze_direction: SqueezeDirection::Horizontal,
            squeeze_ratio: preset.squeeze_ratio,
            distortion_coeffs: preset.distortion_coeffs,
            distortion_model: Some(preset.distortion_model),
            lens_model_label: Some(preset.name),
        };
        assert_eq!(resolved.distortion_model, Some(String::new()));
    }

    #[test]
    fn lens_profile_init_does_not_panic_on_legacy_empty_distortion_model() {
        // Edge case: a .gyroflow file or hand-built LensProfile may directly hold
        // `Some("")` for distortion_model (build_lens_profile no longer produces this
        // state after the §1b guard). `init()` must remain safe in that case, relying
        // on `DistortionModel::from_name("")` falling through to default.
        let mut profile = LensProfile::default();
        profile.focal_length = Some(35.0);
        profile.input_horizontal_stretch = 1.33;
        profile.fisheye_params.distortion_coeffs = vec![0.0, 0.8, -1.1, 0.0];
        profile.distortion_model = Some(String::new());
        profile.init();
        // No normalization at init() — stored value remains verbatim.
        assert_eq!(profile.distortion_model, Some(String::new()));
    }

    #[test]
    fn build_lens_profile_empty_preset_model_keeps_fallback_distortion_model() {
        // §1b contract: when a preset carries `distortion_model == ""`, build_lens_profile
        // applies the preset's distortion_coeffs but does NOT overwrite the upstream
        // (fallback) profile's distortion_model. This covers data-v20260513.1-style
        // packages where presets may ship an empty distortion_model while still carrying
        // valid distortion coefficients. We can't reach find_preset_by_id without polluting
        // load_presets(), so we exercise the assignment site by building the inner state
        // build_lens_profile would have constructed and replaying its post-resolve logic.
        let preset_coeffs = vec![0.0, 0.8, -1.1, 0.0];
        let anamorphic = ResolvedAnamorphic {
            squeeze_direction: SqueezeDirection::Horizontal,
            squeeze_ratio: 1.33,
            distortion_coeffs: preset_coeffs.clone(),
            // mimic resolve_anamorphic_config's line `Some(preset.distortion_model)` for an
            // AnamorphicPreset whose distortion_model is "".
            distortion_model: Some(String::new()),
            lens_model_label: Some("Demo Lens".to_owned()),
        };

        let mut profile = LensProfile::default();
        profile.distortion_model = Some("opencv_fisheye".to_owned());
        profile.fisheye_params.distortion_coeffs = vec![1.0, 2.0, 3.0, 4.0]; // sentinel

        // Replay the guarded assignment from build_lens_profile lines 602-607.
        if !anamorphic.distortion_coeffs.is_empty() || anamorphic.distortion_model.is_some() {
            profile.fisheye_params.distortion_coeffs = anamorphic.distortion_coeffs;
            if let Some(model) = anamorphic.distortion_model.filter(|s| !s.is_empty()) {
                profile.distortion_model = Some(model);
            }
        }

        // Preset coeffs applied (overrides sentinel).
        assert_eq!(profile.fisheye_params.distortion_coeffs, preset_coeffs);
        // Upstream model preserved — NOT overwritten with `Some("")` or `None`.
        assert_eq!(profile.distortion_model.as_deref(), Some("opencv_fisheye"));
    }

    #[test]
    fn build_lens_profile_non_empty_preset_model_overrides_fallback() {
        // Companion to the guard test: when preset.distortion_model is a real id, the
        // existing override semantics still apply.
        let preset_coeffs = vec![0.5, 0.6, 0.7, 0.8];
        let anamorphic = ResolvedAnamorphic {
            squeeze_direction: SqueezeDirection::Horizontal,
            squeeze_ratio: 1.5,
            distortion_coeffs: preset_coeffs.clone(),
            distortion_model: Some("opencv_standard".to_owned()),
            lens_model_label: Some("Demo Lens".to_owned()),
        };

        let mut profile = LensProfile::default();
        profile.distortion_model = Some("opencv_fisheye".to_owned());

        if !anamorphic.distortion_coeffs.is_empty() || anamorphic.distortion_model.is_some() {
            profile.fisheye_params.distortion_coeffs = anamorphic.distortion_coeffs;
            if let Some(model) = anamorphic.distortion_model.filter(|s| !s.is_empty()) {
                profile.distortion_model = Some(model);
            }
        }

        assert_eq!(profile.distortion_model.as_deref(), Some("opencv_standard"));
        assert_eq!(profile.fisheye_params.distortion_coeffs, preset_coeffs);
    }

    #[test]
    fn builtin_lookup_covers_every_index_entry() {
        let index: PresetIndexFile = serde_json::from_str(BUILTIN_INDEX_JSON)
            .expect("BUILTIN_INDEX_JSON must parse");
        assert!(!index.presets.is_empty(), "built-in index should not be empty");
        for entry in &index.presets {
            assert!(
                builtin_preset_file(&entry.file).is_some(),
                "builtin_preset_file missed `{}` referenced by index.json",
                entry.file
            );
        }
    }

    #[test]
    fn builtin_fallback_yields_full_preset_list() {
        let index: PresetIndexFile = serde_json::from_str(BUILTIN_INDEX_JSON)
            .expect("BUILTIN_INDEX_JSON must parse");
        let expected = index.presets.len();
        let presets = load_builtin_presets();
        assert_eq!(
            presets.len(),
            expected,
            "load_builtin_presets() lost entries (expected {expected}, got {})",
            presets.len()
        );
    }

    #[test]
    fn legacy_preset_id_migrates_to_canonical() {
        let parsed = lens_group_configs_from_json(
            r#"[{
                "lens_index": 0,
                "anamorphic_enabled": true,
                "preset_id": "aivascope_58mm_1_50x_vertical",
                "squeeze_direction": "vertical"
            }]"#,
        );

        assert_eq!(
            parsed[0].preset_id.as_deref(),
            Some(CANONICAL_AIVASCOPE_PRESET_ID)
        );
    }

    #[test]
    fn preset_anamorphic_uses_configured_direction() {
        let config = LensGroupConfig {
            lens_index: 0,
            anamorphic_enabled: true,
            preset_id: Some(BUILTIN_1_50X_TEST_PRESET_ID.to_owned()),
            squeeze_direction: Some(SqueezeDirection::Vertical),
            ..Default::default()
        };
        let resolved = resolve_anamorphic_config(Some(&config)).unwrap();
        assert_eq!(resolved.squeeze_direction, SqueezeDirection::Vertical);
        assert_eq!(resolved.squeeze_ratio, 1.5);
        assert_eq!(resolved.distortion_coeffs.len(), 4);
        assert!(resolved.distortion_model.is_some());
    }

    #[test]
    fn status_keeps_missing_focus_if_any_video_in_group_is_missing() {
        let mut statuses = default_lens_group_statuses();

        let mut with_focus = FileMetadata::default();
        with_focus.additional_data = serde_json::json!({
            "lens_index": 1
        });
        with_focus.lens_params.insert(
            0,
            crate::gyro_source::LensParams {
                focal_length: Some(35.0),
                ..Default::default()
            },
        );
        update_status_from_metadata(&mut statuses, &with_focus);

        let mut without_focus = FileMetadata::default();
        without_focus.additional_data = serde_json::json!({
            "lens_index": 1
        });
        update_status_from_metadata(&mut statuses, &without_focus);

        assert!(statuses[1].used);
        assert!(statuses[1].has_auto_focus);
        assert!(statuses[1].has_missing_focus);
        assert_eq!(statuses[1].auto_focus_length_mm, Some(35.0));
    }

    #[test]
    fn build_lens_profile_preserves_fallback_readout_and_distortion() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let mut fallback = LensProfile::default();
        fallback.frame_readout_time = Some(12.5);
        fallback.frame_readout_direction =
            Some(crate::stabilization_params::ReadoutDirection::BottomToTop);
        fallback.distortion_model = Some("opencv_fisheye".to_owned());
        fallback.fisheye_params = CameraParams {
            RMS_error: 0.42,
            camera_matrix: vec![[2000.0, 0.0, 960.0], [0.0, 2000.0, 540.0], [0.0, 0.0, 1.0]],
            distortion_coeffs: vec![0.1, 0.2, 0.3, 0.4],
            radial_distortion_limit: Some(0.9),
        };

        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(30.0),
            ..Default::default()
        };

        let profile =
            build_lens_profile(&metadata, (1920, 1080), Some(&config), Some(&fallback)).unwrap();

        assert_eq!(profile.focal_length, Some(30.0));
        assert_eq!(profile.frame_readout_time, Some(12.5));
        assert_eq!(
            profile.frame_readout_direction,
            Some(crate::stabilization_params::ReadoutDirection::BottomToTop)
        );
        assert_eq!(profile.distortion_model.as_deref(), Some("opencv_fisheye"));
        assert_eq!(
            profile.fisheye_params.distortion_coeffs,
            vec![0.1, 0.2, 0.3, 0.4]
        );
        assert_eq!(
            profile.fisheye_params.camera_matrix[0],
            [3000.0, 0.0, 960.0]
        );
        assert_eq!(
            profile.fisheye_params.camera_matrix[1],
            [0.0, 3000.0, 540.0]
        );
    }

    #[test]
    fn build_lens_profile_uses_metadata_readout_direction_when_fallback_missing() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);
        metadata.frame_readout_time = Some(8.4);
        metadata.frame_readout_direction =
            crate::stabilization_params::ReadoutDirection::BottomToTop;

        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(28.0),
            ..Default::default()
        };

        let profile = build_lens_profile(&metadata, (1920, 1080), Some(&config), None).unwrap();

        assert_eq!(profile.frame_readout_time, Some(8.4));
        assert_eq!(
            profile.frame_readout_direction,
            Some(crate::stabilization_params::ReadoutDirection::BottomToTop)
        );
    }

    #[test]
    fn build_lens_profile_uses_preset_name_as_anamorphic_lens_model() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(35.0),
            anamorphic_enabled: true,
            preset_id: Some(BUILTIN_1_50X_TEST_PRESET_ID.to_owned()),
            squeeze_direction: Some(SqueezeDirection::Horizontal),
            ..Default::default()
        };

        let profile = build_lens_profile(&metadata, (1920, 1080), Some(&config), None).unwrap();

        assert_eq!(profile.lens_model, "Blazar Viper 35mm 1.50x");
    }

    #[test]
    fn build_lens_profile_uses_manual_horizontal_anamorphic_lens_model() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(35.0),
            anamorphic_enabled: true,
            squeeze_direction: Some(SqueezeDirection::Horizontal),
            squeeze_ratio: Some(1.5),
            ..Default::default()
        };

        let profile = build_lens_profile(&metadata, (1920, 1080), Some(&config), None).unwrap();

        assert_eq!(profile.lens_model, "Manual anamorphic 1.50x H");
    }

    #[test]
    fn build_lens_profile_uses_manual_vertical_anamorphic_lens_model() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(35.0),
            anamorphic_enabled: true,
            squeeze_direction: Some(SqueezeDirection::Vertical),
            squeeze_ratio: Some(1.33),
            ..Default::default()
        };

        let profile = build_lens_profile(&metadata, (1920, 1080), Some(&config), None).unwrap();

        assert_eq!(profile.lens_model, "Manual anamorphic 1.33x V");
    }

    #[test]
    fn build_lens_profile_preserves_fallback_lens_model_without_anamorphic() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let mut fallback = LensProfile::default();
        fallback.lens_model = "Fallback Lens".to_owned();

        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(35.0),
            ..Default::default()
        };

        let profile =
            build_lens_profile(&metadata, (1920, 1080), Some(&config), Some(&fallback)).unwrap();

        assert_eq!(profile.lens_model, "Fallback Lens");
    }

    #[test]
    fn build_lens_profile_does_not_use_anamorphic_label_when_config_is_not_effective() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(35.0),
            anamorphic_enabled: true,
            squeeze_direction: Some(SqueezeDirection::Horizontal),
            squeeze_ratio: Some(1.5),
            ..Default::default()
        };
        let effective = effective_lens_group_config_for_build(false, &config, &metadata).unwrap();

        let profile =
            build_lens_profile(&metadata, (1920, 1080), Some(&effective), None).unwrap();

        assert_ne!(profile.lens_model, "Manual anamorphic 1.50x H");
        assert_eq!(profile.input_horizontal_stretch, 1.0);
    }

    #[test]
    fn lens_group_config_from_lens_profile_matches_builtin_preset_name() {
        let mut profile = LensProfile::default();
        profile.lens_model = "Sirui star 50mm 1.33x".to_owned();
        profile.focal_length = Some(16.0);
        profile.input_horizontal_stretch = 1.33;
        profile.input_vertical_stretch = 1.0;

        let config = lens_group_config_from_lens_profile(&profile, 0).unwrap();

        assert_eq!(config.lens_index, 0);
        assert_eq!(config.focal_length_mm, Some(16.0));
        assert!(config.anamorphic_enabled);
        assert_eq!(
            config.preset_id.as_deref(),
            Some("sirui_xingchen_50mm_1_33x")
        );
        assert_eq!(config.squeeze_direction, Some(SqueezeDirection::Horizontal));
        assert_eq!(config.squeeze_ratio, Some(1.33));
    }

    #[test]
    fn lens_group_config_from_lens_profile_ignores_focal_length_only() {
        let mut profile = LensProfile::default();
        profile.lens_model = "Ordinary lens".to_owned();
        profile.focal_length = Some(35.0);
        profile.input_horizontal_stretch = 1.0;
        profile.input_vertical_stretch = 1.0;

        assert!(lens_group_config_from_lens_profile(&profile, 0).is_none());
    }

    #[test]
    fn lens_group_config_from_lens_profile_matches_manual_anamorphic_label() {
        let mut profile = LensProfile::default();
        profile.lens_model = "Manual anamorphic 1.50x H".to_owned();
        profile.focal_length = Some(35.0);
        profile.input_horizontal_stretch = 1.5;
        profile.input_vertical_stretch = 1.0;

        let config = lens_group_config_from_lens_profile(&profile, 0).unwrap();

        assert_eq!(config.lens_index, 0);
        assert_eq!(config.focal_length_mm, Some(35.0));
        assert!(config.anamorphic_enabled);
        assert_eq!(config.preset_id, None);
        assert_eq!(config.squeeze_direction, Some(SqueezeDirection::Horizontal));
        assert_eq!(config.squeeze_ratio, Some(1.5));
    }

    #[test]
    fn lens_group_config_from_lens_profile_ignores_unknown_anamorphic_label() {
        let mut profile = LensProfile::default();
        profile.lens_model = "Ordinary stretched lens".to_owned();
        profile.focal_length = Some(35.0);
        profile.input_horizontal_stretch = 1.5;
        profile.input_vertical_stretch = 1.0;

        assert!(lens_group_config_from_lens_profile(&profile, 0).is_none());
    }

    #[test]
    fn build_lens_profile_preset_direction_only_changes_stretch_axis() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let horizontal_config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(35.0),
            anamorphic_enabled: true,
            preset_id: Some(BUILTIN_1_50X_TEST_PRESET_ID.to_owned()),
            squeeze_direction: Some(SqueezeDirection::Horizontal),
            ..Default::default()
        };
        let vertical_config = LensGroupConfig {
            squeeze_direction: Some(SqueezeDirection::Vertical),
            ..horizontal_config.clone()
        };

        let horizontal_profile =
            build_lens_profile(&metadata, (1920, 1080), Some(&horizontal_config), None).unwrap();
        let vertical_profile =
            build_lens_profile(&metadata, (1920, 1080), Some(&vertical_config), None).unwrap();

        assert_eq!(
            horizontal_profile.fisheye_params.distortion_coeffs,
            vertical_profile.fisheye_params.distortion_coeffs
        );
        assert_eq!(
            horizontal_profile.distortion_model,
            vertical_profile.distortion_model
        );
        assert_eq!(horizontal_profile.input_horizontal_stretch, 1.5);
        assert_eq!(horizontal_profile.input_vertical_stretch, 1.0);
        assert_eq!(vertical_profile.input_horizontal_stretch, 1.0);
        assert_eq!(vertical_profile.input_vertical_stretch, 1.5);
        assert_eq!(horizontal_profile.calib_dimension.w, 2880);
        assert_eq!(horizontal_profile.calib_dimension.h, 1080);
        assert_eq!(horizontal_profile.orig_dimension.w, 2880);
        assert_eq!(horizontal_profile.orig_dimension.h, 1080);
        assert_eq!(
            horizontal_profile
                .output_dimension
                .as_ref()
                .map(|dim| (dim.w, dim.h)),
            Some((2880, 1080))
        );
        assert_eq!(
            horizontal_profile.fisheye_params.camera_matrix[0],
            [3500.0, 0.0, 1440.0]
        );
        assert_eq!(
            horizontal_profile.fisheye_params.camera_matrix[1],
            [0.0, 3500.0, 540.0]
        );
        assert_eq!(vertical_profile.calib_dimension.w, 1920);
        assert_eq!(vertical_profile.calib_dimension.h, 1620);
        assert_eq!(vertical_profile.orig_dimension.w, 1920);
        assert_eq!(vertical_profile.orig_dimension.h, 1620);
        assert_eq!(
            vertical_profile
                .output_dimension
                .as_ref()
                .map(|dim| (dim.w, dim.h)),
            Some((1920, 1620))
        );
        assert_eq!(
            vertical_profile.fisheye_params.camera_matrix[0],
            [3500.0, 0.0, 960.0]
        );
        assert_eq!(
            vertical_profile.fisheye_params.camera_matrix[1],
            [0.0, 3500.0, 810.0]
        );
    }

    #[test]
    fn build_lens_profile_resets_stale_fallback_stretch_axis() {
        let mut metadata = FileMetadata::default();
        metadata.unit_pixel_focal_length = Some(100.0);

        let mut fallback = LensProfile::default();
        fallback.input_horizontal_stretch = 1.5;
        fallback.input_vertical_stretch = 1.0;
        let config = LensGroupConfig {
            lens_index: 0,
            focal_length_mm: Some(35.0),
            anamorphic_enabled: true,
            preset_id: Some(BUILTIN_1_50X_TEST_PRESET_ID.to_owned()),
            squeeze_direction: Some(SqueezeDirection::Vertical),
            ..Default::default()
        };

        let profile =
            build_lens_profile(&metadata, (1920, 1080), Some(&config), Some(&fallback)).unwrap();

        assert_eq!(profile.input_horizontal_stretch, 1.0);
        assert_eq!(profile.input_vertical_stretch, 1.5);
    }

    #[test]
    fn apply_focal_length_fallback_populates_empty_lens_params() {
        let mut metadata = FileMetadata {
            unit_pixel_focal_length: Some(100.0),
            ..Default::default()
        };

        apply_focal_length_fallback_to_metadata(&mut metadata, 30.0);

        let params = metadata.lens_params.get(&0).unwrap();
        assert_eq!(params.focal_length, Some(30.0));
        assert_eq!(params.pixel_focal_length, Some(3000.0));
    }

    #[test]
    fn apply_focal_length_fallback_updates_existing_lens_params() {
        let mut metadata = FileMetadata {
            unit_pixel_focal_length: Some(50.0),
            ..Default::default()
        };
        metadata.lens_params.insert(
            10,
            LensParams {
                focal_length: None,
                pixel_focal_length: None,
                ..Default::default()
            },
        );

        apply_focal_length_fallback_to_metadata(&mut metadata, 24.0);

        let params = metadata.lens_params.get(&10).unwrap();
        assert_eq!(params.focal_length, Some(24.0));
        assert_eq!(params.pixel_focal_length, Some(1200.0));
    }
}
