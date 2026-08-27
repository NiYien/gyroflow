// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

use std::collections::HashMap;
use std::io;

use telemetry_parser::camera_db::CameraDatabase;

use crate::camera_identifier::CameraIdentifier;
use crate::stabilization_params::ReadoutDirection;

const STANDARD_READOUT_FPS: [f64; 8] = [25.0, 30.0, 50.0, 60.0, 100.0, 120.0, 200.0, 240.0];
pub const MANUAL_CAMERA_BRAND_SETTING: &str = "queue_manual_camera_brand";
pub const MANUAL_CAMERA_MODEL_SETTING: &str = "queue_manual_camera_model";

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ManualCameraSelection {
    pub brand: String,
    pub model: String,
}

impl ManualCameraSelection {
    pub fn new(brand: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            brand: brand.into(),
            model: model.into(),
        }
    }

    pub fn is_complete(&self) -> bool {
        !self.brand.trim().is_empty() && !self.model.trim().is_empty()
    }

    pub fn load_persisted() -> Self {
        Self {
            brand: crate::settings::get_str(MANUAL_CAMERA_BRAND_SETTING, ""),
            model: crate::settings::get_str(MANUAL_CAMERA_MODEL_SETTING, ""),
        }
    }

    pub fn persist(&self) {
        crate::settings::set(
            MANUAL_CAMERA_BRAND_SETTING,
            serde_json::Value::String(self.brand.clone()),
        );
        crate::settings::set(
            MANUAL_CAMERA_MODEL_SETTING,
            serde_json::Value::String(self.model.clone()),
        );
        crate::settings::flush();
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ManualCameraModel {
    pub id: String,
    pub label: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ManualCameraBrand {
    pub id: String,
    pub label: String,
    pub models: Vec<ManualCameraModel>,
}

#[derive(Clone, Debug)]
pub struct ManualCameraInput {
    pub size: (usize, usize),
    pub fps: f64,
    pub additional_data: serde_json::Value,
    pub existing_direction: ReadoutDirection,
}

#[derive(Clone, Debug)]
pub struct ManualCameraResolution {
    pub camera_identifier: CameraIdentifier,
    pub crop_factor: f64,
    pub unit_pixel_focal_length: f64,
    pub frame_readout_time: f64,
    pub frame_readout_direction: ReadoutDirection,
    pub readout_estimated: bool,
}

pub struct ManualCameraCatalog {
    database: CameraDatabase,
    brands: Vec<ManualCameraBrand>,
}

impl ManualCameraCatalog {
    pub fn load(path: &str) -> io::Result<Self> {
        let database = CameraDatabase::load(path)?;
        let mut brand_ids = std::fs::read_dir(path)?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let path = entry.path();
                (path.extension().and_then(|value| value.to_str()) == Some("json"))
                    .then(|| path.file_stem()?.to_str().map(str::to_ascii_uppercase))
                    .flatten()
            })
            .collect::<Vec<_>>();
        brand_ids.sort();
        brand_ids.dedup();

        let brands = brand_ids
            .into_iter()
            .filter_map(|brand_id| {
                let brand_data = database.get_brand(&brand_id)?;
                let mut models = brand_data
                    .models
                    .iter()
                    .map(|(model, _)| ManualCameraModel {
                        id: model.clone(),
                        label: model.clone(),
                        enabled: brand_data
                            .readout
                            .data
                            .get(model)
                            .is_some_and(|row| row.iter().any(Option::is_some)),
                    })
                    .collect::<Vec<_>>();
                models.sort_by(|left, right| left.label.cmp(&right.label));
                Some(ManualCameraBrand {
                    label: brand_label(&brand_id),
                    id: brand_id,
                    models,
                })
            })
            .collect();

        Ok(Self { database, brands })
    }

    pub fn load_active() -> io::Result<Self> {
        let path = crate::gyro_source::get_camera_db_path().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "Active camera database not found")
        })?;
        Self::load(&path)
    }

    pub fn brands(&self) -> &[ManualCameraBrand] {
        &self.brands
    }

    pub fn is_selectable(&self, selection: &ManualCameraSelection) -> bool {
        selection.is_complete()
            && self
                .brands
                .iter()
                .find(|brand| brand.id.eq_ignore_ascii_case(selection.brand.trim()))
                .and_then(|brand| {
                    brand
                        .models
                        .iter()
                        .find(|model| model.id == selection.model.trim())
                })
                .is_some_and(|model| model.enabled)
    }

    pub fn resolve(
        &self,
        selection: &ManualCameraSelection,
        input: &ManualCameraInput,
    ) -> Option<ManualCameraResolution> {
        if !self.is_selectable(selection)
            || input.size.0 == 0
            || input.size.1 == 0
            || !input.fps.is_finite()
            || input.fps <= 0.0
        {
            return None;
        }

        let brand = self
            .brands
            .iter()
            .find(|brand| brand.id.eq_ignore_ascii_case(selection.brand.trim()))?;
        let brand_data = self.database.get_brand(&brand.id)?;
        let (model_name, model_data) = brand_data
            .models
            .iter()
            .find(|(model, _)| model == selection.model.trim())?;
        let sensor_w = model_data.sw as f64;
        if !sensor_w.is_finite() || sensor_w <= 0.0 {
            return None;
        }

        let res_w = u32::try_from(input.size.0).ok()?;
        let res_h = u32::try_from(input.size.1).ok()?;
        let tags = top_level_tags(&input.additional_data);
        let explicit_scale = numeric_hint(&input.additional_data, &["scale_35mm"])
            .filter(|value| value.is_finite() && *value > 0.0);
        let lumix_scale = if brand.id == "LUMIX" {
            lumix_scale_35mm(&input.additional_data, model_data, res_w)
        } else {
            None
        };
        let nikon_scale = if brand.id == "NIKON" {
            nikon_scale_35mm(&input.additional_data)
        } else {
            None
        };
        let scale_35mm = explicit_scale.or(nikon_scale).or(lumix_scale);
        let mut view_angle = string_hint(
            &input.additional_data,
            &["view_angle", "crop_type", "camera_setting", "image_format"],
        )
        .map(|value| normalize_view_angle(&value).to_owned())
        .or_else(|| {
            input
                .additional_data
                .get("crop_type")
                .or_else(|| input.additional_data.get("crop_hi_speed_type"))
                .and_then(|value| value.as_u64())
                .and_then(|value| u16::try_from(value).ok())
                .and_then(|value| self.database.lookup_crop_type(&brand.id, value))
                .map(str::to_owned)
        });
        if view_angle.is_none() && brand.id == "SONY" {
            view_angle = Some(if model_data.sw < 30.0 { "APSC" } else { "FULL" }.to_owned());
        }

        let (crop_factor, unit_pixel_focal_length) = if brand.id == "BLACKMAGIC" {
            let native_width = blackmagic_native_width(model_name)? as f64;
            (native_width / res_w as f64, native_width / sensor_w)
        } else if brand.id == "RED" {
            let native_width = red_native_width(model_name)? as f64;
            let crop = if (res_w as f64) < native_width {
                native_width / res_w as f64
            } else {
                1.0
            };
            (crop, res_w as f64 * crop / sensor_w)
        } else if brand.id == "KINEFINITY" {
            if let Some(effective_sensor_w) =
                kinefinity_sensor_width(model_name, view_angle.as_deref())
            {
                (
                    sensor_w / effective_sensor_w,
                    res_w as f64 / effective_sensor_w,
                )
            } else {
                let crop = self
                    .database
                    .match_crop(
                        &brand.id,
                        model_name,
                        res_w,
                        res_h,
                        input.fps,
                        view_angle.as_deref(),
                        &tags,
                    )
                    .unwrap_or(1.0);
                (crop, res_w as f64 * crop / sensor_w)
            }
        } else if let Some(scale) = scale_35mm.filter(|scale| (*scale - 1.0).abs() > 0.01) {
            let unit_pixel_focal_length = if brand.id == "NIKON" {
                res_w as f64 * scale / sensor_w
            } else {
                scale / 36.0 * res_w as f64
            };
            (scale, unit_pixel_focal_length)
        } else {
            let crop = numeric_hint(&input.additional_data, &["crop_factor"])
                .filter(|value| value.is_finite() && *value > 0.0)
                .or_else(|| {
                    self.database.match_crop(
                        &brand.id,
                        model_name,
                        res_w,
                        res_h,
                        input.fps,
                        view_angle.as_deref(),
                        &tags,
                    )
                })
                .unwrap_or(1.0);
            (crop, res_w as f64 * crop / sensor_w)
        };
        if !crop_factor.is_finite()
            || crop_factor <= 0.0
            || !unit_pixel_focal_length.is_finite()
            || unit_pixel_focal_length <= 0.0
        {
            return None;
        }

        let (readout_w, readout_h, nraw_subsampled_ratio) = if brand.id == "NIKON" {
            nikon_readout_hints(&input.additional_data, res_w, res_h)
        } else {
            (
                res_w,
                res_h,
                numeric_hint(
                    &input.additional_data,
                    &["nraw_subsampled_ratio", "nraw_subsample_ratio"],
                ),
            )
        };
        let readout_scale = if brand.id == "KINEFINITY" {
            crop_factor
        } else {
            scale_35mm.unwrap_or(0.0)
        };
        let readout = self.database.lookup_readout(
            &brand.id,
            model_name,
            readout_w,
            readout_h,
            input.fps,
            readout_scale,
            model_data.sw,
            nraw_subsampled_ratio,
            &tags,
        );
        let (frame_readout_time, readout_estimated) = match readout {
            Some(result) => (result.readout_time_ms, result.is_estimated),
            None => (half_frame_readout_time(input.fps)?, true),
        };
        if !frame_readout_time.is_finite() || frame_readout_time < 0.0 {
            return None;
        }

        Some(ManualCameraResolution {
            camera_identifier: CameraIdentifier {
                brand: brand.label.clone(),
                model: model_name.clone(),
                camera_setting: view_angle.unwrap_or_default(),
                fps: (input.fps * 1000.0).round() as usize,
                video_width: input.size.0,
                video_height: input.size.1,
                ..Default::default()
            },
            crop_factor,
            unit_pixel_focal_length,
            frame_readout_time,
            frame_readout_direction: input.existing_direction,
            readout_estimated,
        })
    }
}

fn top_level_tags(value: &serde_json::Value) -> HashMap<String, serde_json::Value> {
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default()
}

fn numeric_hint(value: &serde_json::Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| value.get(*key)?.as_f64())
}

fn string_hint(value: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str().map(str::to_owned))
        .filter(|value| !value.trim().is_empty())
}

fn normalize_view_angle(value: &str) -> &str {
    match value.trim() {
        "FF" => "FULL",
        value => value,
    }
}

fn lumix_scale_35mm(
    additional_data: &serde_json::Value,
    model_data: &telemetry_parser::camera_db::ModelData,
    res_w: u32,
) -> Option<f64> {
    match additional_data
        .get("crop_mode")
        .and_then(|value| value.as_u64())
    {
        Some(2) if model_data.sw > 30.0 => Some(1.5),
        Some(255) => model_data
            .extra
            .get("pc")
            .and_then(|value| value.as_u64())
            .filter(|pixel_count| *pixel_count > 0 && res_w > 0)
            .map(|pixel_count| {
                let sensor_width_px = (pixel_count as f64 * 1.5).sqrt();
                sensor_width_px / res_w as f64 * model_data.sw as f64 / 36.0
            }),
        _ => None,
    }
}

fn nikon_scale_35mm(additional_data: &serde_json::Value) -> Option<f64> {
    let full_w = numeric_hint(additional_data, &["crop_hi_speed_full_w"])?;
    let crop_w = numeric_hint(additional_data, &["crop_hi_speed_crop_w"])?;
    (full_w.is_finite() && crop_w.is_finite() && full_w > 0.0 && crop_w > 0.0)
        .then_some(full_w / crop_w)
}

fn positive_u32_hint(value: &serde_json::Value, key: &str) -> Option<u32> {
    value
        .get(key)?
        .as_u64()
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn nikon_readout_hints(
    additional_data: &serde_json::Value,
    video_w: u32,
    video_h: u32,
) -> (u32, u32, Option<f64>) {
    let nraw_size = positive_u32_hint(additional_data, "nraw_width")
        .zip(positive_u32_hint(additional_data, "nraw_height"));
    let is_proxy = additional_data
        .get("proxy_output")
        .and_then(serde_json::Value::as_u64)
        == Some(1);
    let crop_size = positive_u32_hint(additional_data, "crop_hi_speed_crop_w")
        .zip(positive_u32_hint(additional_data, "crop_hi_speed_crop_h"));
    let (readout_w, readout_h) = nraw_size
        .or_else(|| is_proxy.then_some(crop_size).flatten())
        .unwrap_or((video_w, video_h));
    let ratio = nraw_size
        .or_else(|| is_proxy.then_some((readout_w, readout_h)))
        .and_then(|(_, height)| crop_size.map(|(_, crop_h)| height as f64 / crop_h as f64));
    (readout_w, readout_h, ratio)
}

fn blackmagic_native_width(model: &str) -> Option<u32> {
    match model {
        "BMPCC" => Some(1920),
        "BMCC" => Some(2432),
        "BMCC 4K" => Some(3840),
        "BMCC 6K" => Some(6048),
        "BMPCC 4K" | "BMSC 4K G2" => Some(4096),
        "BMPCC 6K" => Some(6144),
        "URSA Mini 4K" => Some(3840),
        "URSA Mini 4.6K" | "URSA Mini Pro 4.6K G2" => Some(4608),
        "URSA Mini Pro 12K" => Some(12288),
        _ => None,
    }
}

fn red_native_width(model: &str) -> Option<u32> {
    let model = model.to_ascii_uppercase();
    if matches!(model.as_str(), "KOMODO" | "KOMODO-X") {
        Some(6144)
    } else if model.contains("8K") {
        Some(8192)
    } else if model.contains("6K") {
        Some(6144)
    } else if model.contains("5K") {
        Some(5120)
    } else if model.contains("4.5K") {
        Some(4608)
    } else if model.contains("4K") {
        Some(4096)
    } else {
        match model.as_str() {
            "EPIC" => Some(5120),
            "SCARLET" => Some(4096),
            _ => None,
        }
    }
}

fn kinefinity_sensor_width(model: &str, view_angle: Option<&str>) -> Option<f64> {
    let view_angle = view_angle?;
    match (model, view_angle) {
        ("MAVO Edge 8K", "FULL") => Some(36.0),
        ("MAVO Edge 8K", "S35") => Some(27.0),
        ("MAVO Edge 6K", "FULL") => Some(36.0),
        ("MAVO Edge 6K", "S35") => Some(24.5),
        ("MAVO", "S35") | ("MAVO2 S35", "S35") => Some(24.0),
        ("MAVO", "M43") | ("MAVO2 S35", "M43") => Some(16.0),
        ("MAVO", "S16") | ("MAVO2 S35", "S16") => Some(12.0),
        ("MAVO", "16mm") | ("MAVO2 S35", "16mm") => Some(8.0),
        ("MAVO LF", "FULL") | ("MAVO2 LF", "FULL") => Some(36.0),
        ("MAVO LF", "S35") | ("MAVO2 LF", "S35") => Some(24.5),
        ("TERRA 4K", "S35") => Some(19.5),
        ("TERRA 4K", "M43") => Some(14.62),
        ("TERRA 4K", "S16") => Some(9.7),
        _ => None,
    }
}

fn half_frame_readout_time(fps: f64) -> Option<f64> {
    if !fps.is_finite() || fps <= 0.0 {
        return None;
    }
    let nearest = STANDARD_READOUT_FPS.into_iter().min_by(|left, right| {
        let left_distance = (fps - *left).abs();
        let right_distance = (fps - *right).abs();
        left_distance
            .total_cmp(&right_distance)
            .then_with(|| left.total_cmp(right))
    })?;
    Some(500.0 / nearest)
}

fn brand_label(id: &str) -> String {
    match id {
        "BLACKMAGIC" => "Blackmagic".to_owned(),
        "LUMIX" => "Lumix".to_owned(),
        "RED" => "RED".to_owned(),
        _ => {
            let mut chars = id.chars();
            chars
                .next()
                .map(|first| {
                    first
                        .to_uppercase()
                        .chain(chars.flat_map(char::to_lowercase))
                        .collect()
                })
                .unwrap_or_default()
        }
    }
}

pub fn source_camera_identity_complete(identifier: Option<&CameraIdentifier>) -> bool {
    identifier.is_some_and(|identifier| {
        !identifier.brand.trim().is_empty() && !identifier.model.trim().is_empty()
    })
}

pub fn all_video_source_cameras_incomplete<'a>(
    identifiers: impl IntoIterator<Item = Option<&'a CameraIdentifier>>,
) -> bool {
    let mut has_video = false;
    for identifier in identifiers {
        has_video = true;
        if source_camera_identity_complete(identifier) {
            return false;
        }
    }
    has_video
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera_identifier::CameraIdentifier;
    use serial_test::serial;

    fn write_catalog_fixture(dir: &std::path::Path) {
        let fixture = serde_json::json!({
            "version": 1,
            "models": {
                "Measured": { "sw": 36.0 },
                "Global": { "sw": 36.0 },
                "No data": { "sw": 36.0 },
                "Broken sensor": { "sw": 0.0 }
            },
            "crop_type_map": { "6": "FX" },
            "crop": [
                { "m": ["Measured"], "w": [6000], "c": 1.5 },
                { "m": ["Global"], "va": "FX", "c": 1.2 }
            ],
            "readout": {
                "columns": ["4K60", "4K30"],
                "data": {
                    "Measured": [12.5, null],
                    "Global": [0.0, null],
                    "No data": [null, null],
                    "Broken sensor": [10.0, null]
                }
            }
        });
        std::fs::write(
            dir.join("testbrand.json"),
            serde_json::to_vec_pretty(&fixture).unwrap(),
        )
        .unwrap();

        let red = serde_json::json!({
            "version": 1,
            "models": {
                "KOMODO": { "sw": 27.0 },
                "KOMODO-X": { "sw": 27.0 },
                "V-RAPTOR 8K VV": { "sw": 40.96 }
            },
            "readout": {
                "columns": ["4K60"],
                "data": {
                    "KOMODO": [0.0],
                    "KOMODO-X": [0.0],
                    "V-RAPTOR 8K VV": [8.0]
                }
            }
        });
        std::fs::write(
            dir.join("red.json"),
            serde_json::to_vec_pretty(&red).unwrap(),
        )
        .unwrap();

        let kinefinity = serde_json::json!({
            "version": 1,
            "models": { "MAVO Edge 8K": { "sw": 36.0 } },
            "readout": {
                "columns": ["4K60"],
                "data": { "MAVO Edge 8K": [11.0] }
            }
        });
        std::fs::write(
            dir.join("kinefinity.json"),
            serde_json::to_vec_pretty(&kinefinity).unwrap(),
        )
        .unwrap();

        let blackmagic = serde_json::json!({
            "version": 1,
            "models": { "BMPCC 4K": { "sw": 18.96 } },
            "crop": [],
            "readout": {
                "columns": ["4K60"],
                "data": { "BMPCC 4K": [15.0] }
            }
        });
        std::fs::write(
            dir.join("blackmagic.json"),
            serde_json::to_vec_pretty(&blackmagic).unwrap(),
        )
        .unwrap();

        let lumix = serde_json::json!({
            "version": 1,
            "models": { "S5II": { "sw": 35.6, "pc": 24000000 } },
            "crop": [],
            "readout": {
                "columns": ["4K60"],
                "data": { "S5II": [14.0] }
            }
        });
        std::fs::write(
            dir.join("lumix.json"),
            serde_json::to_vec_pretty(&lumix).unwrap(),
        )
        .unwrap();

        let sony = serde_json::json!({
            "version": 1,
            "models": {
                "ILCE-FX3": { "sw": 35.9 },
                "ILCE-FX30": { "sw": 23.5 }
            },
            "crop": [
                { "m": ["ILCE-FX3"], "va": "FULL", "w": [3840], "fps": [80, 160], "c": 1.2 },
                { "m": ["ILCE-FX3"], "va": "APSC", "c": 1.523 }
            ],
            "readout": {
                "columns": ["4K120", "4K60"],
                "data": {
                    "ILCE-FX3": [7.6, 8.7],
                    "ILCE-FX30": [7.6, 8.7]
                }
            }
        });
        std::fs::write(
            dir.join("sony.json"),
            serde_json::to_vec_pretty(&sony).unwrap(),
        )
        .unwrap();

        let nikon = serde_json::json!({
            "version": 1,
            "crop_type_map": { "3": "DX", "6": "FX" },
            "models": { "Z 8": { "sw": 35.9 } },
            "crop": [{ "m": ["Z 8"], "va": "DX", "c": 1.527 }],
            "readout": {
                "columns": ["4KN60", "4K60"],
                "data": { "Z 8": [7.18, 14.5] }
            },
            "readout_adjust": [{ "type": "scale", "expr": "scale_35mm", "min_w": 1920 }]
        });
        std::fs::write(
            dir.join("nikon.json"),
            serde_json::to_vec_pretty(&nikon).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn manual_camera_catalog_keeps_zero_readout_selectable_and_disables_empty_rows() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());

        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();
        let brand = catalog
            .brands()
            .iter()
            .find(|brand| brand.id == "TESTBRAND")
            .unwrap();

        assert_eq!(
            brand
                .models
                .iter()
                .map(|model| (model.id.as_str(), model.enabled))
                .collect::<Vec<_>>(),
            vec![
                ("Broken sensor", true),
                ("Global", true),
                ("Measured", true),
                ("No data", false),
            ]
        );
        assert!(catalog.is_selectable(&ManualCameraSelection::new("TESTBRAND", "Measured")));
        assert!(catalog.is_selectable(&ManualCameraSelection::new("TESTBRAND", "Global")));
        assert!(!catalog.is_selectable(&ManualCameraSelection::new("TESTBRAND", "No data")));
        assert!(!catalog.is_selectable(&ManualCameraSelection::new("MISSING", "Measured")));
    }

    fn identifier(brand: &str, model: &str) -> CameraIdentifier {
        CameraIdentifier {
            brand: brand.to_owned(),
            model: model.to_owned(),
            ..Default::default()
        }
    }

    #[test]
    fn manual_camera_eligibility_requires_nonempty_all_incomplete_source_identities() {
        let empty: Vec<Option<&CameraIdentifier>> = Vec::new();
        assert!(!all_video_source_cameras_incomplete(empty));

        let missing_model = identifier("Sony", "");
        let missing_brand = identifier("", "FX3");
        assert!(all_video_source_cameras_incomplete([
            Some(&missing_model),
            Some(&missing_brand),
            None,
        ]));

        let complete = identifier("Sony", "FX3");
        assert!(!all_video_source_cameras_incomplete([
            Some(&missing_model),
            Some(&complete),
        ]));
        assert!(!all_video_source_cameras_incomplete([Some(&complete)]));
    }

    #[test]
    fn manual_camera_eligibility_uses_source_identity_not_effective_overlay() {
        let source = identifier("", "");
        let effective = identifier("SONY", "FX3");

        assert!(all_video_source_cameras_incomplete([Some(&source)]));
        assert!(source_camera_identity_complete(Some(&effective)));
    }

    fn input(size: (usize, usize), fps: f64) -> ManualCameraInput {
        ManualCameraInput {
            size,
            fps,
            additional_data: serde_json::json!({}),
            existing_direction: crate::stabilization_params::ReadoutDirection::TopToBottom,
        }
    }

    #[test]
    fn manual_camera_geometry_uses_camera_db_crop_and_sensor_width() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        let resolved = catalog
            .resolve(
                &ManualCameraSelection::new("TESTBRAND", "Measured"),
                &input((6000, 4000), 59.94),
            )
            .unwrap();

        assert!((resolved.crop_factor - 1.5).abs() < 1e-9);
        assert!((resolved.unit_pixel_focal_length - 250.0).abs() < 1e-9);
    }

    #[test]
    fn manual_camera_geometry_matches_blackmagic_native_sensor_width_rule() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        let resolved = catalog
            .resolve(
                &ManualCameraSelection::new("BLACKMAGIC", "BMPCC 4K"),
                &input((1920, 1080), 59.94),
            )
            .unwrap();

        assert!((resolved.unit_pixel_focal_length - 4096.0 / 18.96).abs() < 1e-4);
    }

    #[test]
    fn manual_camera_geometry_uses_lumix_crop_mode_hint() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();
        let mut manual_input = input((3840, 2160), 59.94);
        manual_input.additional_data = serde_json::json!({ "crop_mode": 2 });

        let resolved = catalog
            .resolve(&ManualCameraSelection::new("LUMIX", "S5II"), &manual_input)
            .unwrap();

        assert!((resolved.crop_factor - 1.5).abs() < 1e-9);
        assert!((resolved.unit_pixel_focal_length - 160.0).abs() < 1e-9);
    }

    #[test]
    fn manual_camera_geometry_matches_red_native_sensor_width_rule() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        let resolved = catalog
            .resolve(
                &ManualCameraSelection::new("RED", "V-RAPTOR 8K VV"),
                &input((3840, 2160), 59.94),
            )
            .unwrap();

        assert!((resolved.crop_factor - 8192.0 / 3840.0).abs() < 1e-9);
        assert!((resolved.unit_pixel_focal_length - 200.0).abs() < 1e-4);
    }

    #[test]
    fn manual_camera_geometry_supports_red_komodo_canonical_names() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        for model in ["KOMODO", "KOMODO-X"] {
            let resolved = catalog
                .resolve(
                    &ManualCameraSelection::new("RED", model),
                    &input((4096, 2160), 59.94),
                )
                .unwrap();
            assert!((resolved.crop_factor - 1.5).abs() < 1e-9, "{model}");
            assert!((resolved.unit_pixel_focal_length - 6144.0 / 27.0).abs() < 1e-9);
            assert_eq!(resolved.frame_readout_time, 0.0);
        }
    }

    #[test]
    fn manual_camera_sony_uses_selected_sensor_class_for_full_frame_crop_rules() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        let resolved = catalog
            .resolve(
                &ManualCameraSelection::new("SONY", "ILCE-FX3"),
                &input((3840, 2160), 120.0),
            )
            .unwrap();

        assert_eq!(resolved.camera_identifier.camera_setting, "FULL");
        assert!((resolved.crop_factor - 1.2).abs() < 1e-9);
        assert!((resolved.unit_pixel_focal_length - (3840.0 * 1.2 / 35.9)).abs() < 1e-4);
        assert_eq!(resolved.frame_readout_time, 7.6);
    }

    #[test]
    fn manual_camera_nikon_uses_real_crop_and_nraw_hints() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();
        let mut manual_input = input((1920, 1080), 60.0);
        manual_input.additional_data = serde_json::json!({
            "crop_hi_speed_type": 3,
            "crop_hi_speed_full_w": 8256,
            "crop_hi_speed_crop_w": 5408,
            "crop_hi_speed_crop_h": 3040,
            "nraw_width": 4128,
            "nraw_height": 2322,
            "proxy_output": 1
        });

        let resolved = catalog
            .resolve(&ManualCameraSelection::new("NIKON", "Z 8"), &manual_input)
            .unwrap();

        let expected_scale = 8256.0 / 5408.0;
        assert_eq!(resolved.camera_identifier.camera_setting, "DX");
        assert!((resolved.crop_factor - expected_scale).abs() < 1e-9);
        assert!((resolved.unit_pixel_focal_length - expected_scale / 35.9 * 1920.0).abs() < 1e-4);
        // 2322/3040 is an oversampled (not integer-binned) N-RAW scan, so the
        // database keeps the 4K60 value and applies Nikon's DX scale adjustment.
        assert!((resolved.frame_readout_time - 14.5 / expected_scale).abs() < 1e-9);
        assert!(!resolved.readout_estimated);

        let (readout_w, readout_h, ratio) =
            nikon_readout_hints(&manual_input.additional_data, 1920, 1080);
        assert_eq!((readout_w, readout_h), (4128, 2322));
        assert!((ratio.unwrap() - 2322.0 / 3040.0).abs() < 1e-9);
    }

    #[test]
    fn manual_camera_geometry_matches_kinefinity_view_angle_rule() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();
        let mut manual_input = input((4096, 2160), 59.94);
        manual_input.additional_data = serde_json::json!({ "image_format": "S35" });

        let resolved = catalog
            .resolve(
                &ManualCameraSelection::new("KINEFINITY", "MAVO Edge 8K"),
                &manual_input,
            )
            .unwrap();

        assert!((resolved.crop_factor - 36.0 / 27.0).abs() < 1e-9);
        assert!((resolved.unit_pixel_focal_length - 4096.0 / 27.0).abs() < 1e-9);
    }

    #[test]
    fn manual_camera_geometry_uses_numeric_camera_db_crop_type_hint() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();
        let mut manual_input = input((3840, 2160), 59.94);
        manual_input.additional_data = serde_json::json!({ "crop_type": 6 });

        let resolved = catalog
            .resolve(
                &ManualCameraSelection::new("TESTBRAND", "Global"),
                &manual_input,
            )
            .unwrap();

        assert!((resolved.crop_factor - 1.2).abs() < 1e-9);
        assert!((resolved.unit_pixel_focal_length - 128.0).abs() < 1e-9);
    }

    #[test]
    fn manual_camera_geometry_rejects_invalid_or_unselectable_inputs() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        assert!(
            catalog
                .resolve(
                    &ManualCameraSelection::new("TESTBRAND", "Measured"),
                    &input((0, 2160), 59.94),
                )
                .is_none()
        );
        assert!(
            catalog
                .resolve(
                    &ManualCameraSelection::new("TESTBRAND", "Broken sensor"),
                    &input((3840, 2160), 59.94),
                )
                .is_none()
        );
        assert!(
            catalog
                .resolve(
                    &ManualCameraSelection::new("TESTBRAND", "No data"),
                    &input((3840, 2160), 59.94),
                )
                .is_none()
        );
        assert!(
            catalog
                .resolve(
                    &ManualCameraSelection::new("TESTBRAND", "Missing"),
                    &input((3840, 2160), 59.94),
                )
                .is_none()
        );
    }

    #[test]
    fn manual_camera_readout_prefers_database_and_keeps_zero() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        let measured = catalog
            .resolve(
                &ManualCameraSelection::new("TESTBRAND", "Measured"),
                &input((3840, 2160), 59.94),
            )
            .unwrap();
        assert_eq!(measured.frame_readout_time, 12.5);
        assert!(!measured.readout_estimated);

        let global = catalog
            .resolve(
                &ManualCameraSelection::new("TESTBRAND", "Global"),
                &input((3840, 2160), 59.94),
            )
            .unwrap();
        assert_eq!(global.frame_readout_time, 0.0);
        assert!(!global.readout_estimated);
    }

    #[test]
    fn manual_camera_readout_uses_nearest_standard_half_frame_with_lower_tie() {
        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();

        let near_sixty = catalog
            .resolve(
                &ManualCameraSelection::new("TESTBRAND", "Measured"),
                &input((1280, 720), 59.94),
            )
            .unwrap();
        assert!((near_sixty.frame_readout_time - 500.0 / 60.0).abs() < 1e-9);
        assert!(near_sixty.readout_estimated);

        let tied = catalog
            .resolve(
                &ManualCameraSelection::new("TESTBRAND", "Measured"),
                &input((1280, 720), 55.0),
            )
            .unwrap();
        assert_eq!(tied.frame_readout_time, 10.0);
        assert!(tied.readout_estimated);
    }

    #[test]
    fn manual_camera_readout_preserves_existing_direction_and_rejects_invalid_fps() {
        use crate::stabilization_params::ReadoutDirection;

        let tmp = tempfile::tempdir().unwrap();
        write_catalog_fixture(tmp.path());
        let catalog = ManualCameraCatalog::load(tmp.path().to_str().unwrap()).unwrap();
        let mut directional = input((1280, 720), 59.94);
        directional.existing_direction = ReadoutDirection::BottomToTop;

        let resolved = catalog
            .resolve(
                &ManualCameraSelection::new("TESTBRAND", "Measured"),
                &directional,
            )
            .unwrap();
        assert_eq!(
            resolved.frame_readout_direction,
            ReadoutDirection::BottomToTop
        );
        assert!(
            catalog
                .resolve(
                    &ManualCameraSelection::new("TESTBRAND", "Measured"),
                    &input((1280, 720), 0.0),
                )
                .is_none()
        );
    }

    #[test]
    #[serial]
    fn manual_camera_selection_persists_independently_of_validity() {
        let tmp = tempfile::tempdir().unwrap();
        let settings_file = tmp.path().join("settings.json");

        crate::settings::with_test_settings_file(settings_file.clone(), || {
            let selection = ManualCameraSelection::new("TESTBRAND", "Missing model");
            selection.persist();

            let persisted: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&settings_file).unwrap()).unwrap();
            assert_eq!(persisted["queue_manual_camera_brand"], "TESTBRAND");
            assert_eq!(persisted["queue_manual_camera_model"], "Missing model");
            assert_eq!(ManualCameraSelection::load_persisted(), selection);
        });
    }
}
