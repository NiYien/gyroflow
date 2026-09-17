// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2025 Adrian <adrian.eddy at gmail>

use crate::gyro_source::FileMetadata;
use telemetry_parser::tags_impl::{GetWithType, GroupId, GroupedTagMap, TagId};

/// Map full-frame CMT metadata to an aspect-preserving proxy canvas.
pub(super) fn cmt_proxy_scale(input: &telemetry_parser::Input, size: (usize, usize)) -> Option<(f64, f64)> {
    if input.camera_type() != "Canon" { return None; }
    let map = input.samples.as_ref()?.first()?.tag_map.as_ref()?.get(&GroupId::Default)?;
    if map.get_t(TagId::Custom("canon_cmt".into())) as Option<&bool> != Some(&true) {
        return None;
    }
    let width = *(map.get_t(TagId::Custom("video_width".into())) as Option<&u32>)?;
    let height = *(map.get_t(TagId::Custom("video_height".into())) as Option<&u32>)?;
    proxy_canvas_scale((width as usize, height as usize), size)
}

fn proxy_canvas_scale(source: (usize, usize), target: (usize, usize)) -> Option<(f64, f64)> {
    if source.0 == 0 || source.1 == 0 || target.0 == 0 || target.1 == 0 || source == target {
        return None;
    }
    let scale = (target.0 as f64 / source.0 as f64).min(target.1 as f64 / source.1 as f64);
    // Readout is expressed across the target canvas; padded rows contain no exposure.
    Some((scale, target.1 as f64 / (source.1 as f64 * scale)))
}

pub fn init_lens_profile(
    md: &mut FileMetadata,
    input: &telemetry_parser::Input,
    tag_map: &GroupedTagMap,
    size: (usize, usize),
    info: &telemetry_parser::util::SampleInfo,
) {
    if let Some(lens) = tag_map.get(&GroupId::Lens) {
        if let Some(corrections) =
            lens.get_t(TagId::Custom("EnabledCorrections".into())) as Option<&Vec<u8>>
        {
            if corrections.len() == 4 && corrections[2] == 0 {
                // No internal distortion correction - use OpenCV params
                let timestamp_us = (info.timestamp_ms * 1000.0).round() as i64;
                if let Some(distortion) = lens.get_t(TagId::Distortion) as Option<&Vec<f32>> {
                    if let Some(lp) = md.lens_params.get_mut(&timestamp_us) {
                        if distortion.len() == 8 {
                            lp.distortion_coefficients.clear();
                            lp.distortion_coefficients.push(distortion[0] as f64); // k1
                            lp.distortion_coefficients.push(distortion[1] as f64); // k2
                            lp.distortion_coefficients.push(distortion[6] as f64); // p1
                            lp.distortion_coefficients.push(distortion[7] as f64); // p2
                            lp.distortion_coefficients.push(distortion[2] as f64); // k3
                            lp.distortion_coefficients.push(distortion[3] as f64); // k4
                            lp.distortion_coefficients.push(distortion[4] as f64); // k5
                            lp.distortion_coefficients.push(distortion[5] as f64);
                            // k6
                        }
                    }
                }
            }
        }
    }

    if md.lens_profile.is_none() {
        // (fx, fy) for the synthetic camera_matrix, in priority order:
        //  Tier 1 — RF lens: CNDM PixelFocalLength [fx, fy] (electronic RF only).
        //  Tier 2 — EF-adapted / other electronic lens without PixelFocalLength:
        //           derive from FocalLength(mm) and the effective sensor size(mm),
        //           fx = focal / sensor_w * width_px. Pinhole (no distortion).
        //  Tier 3 (manual focal, no FocalLength) is applied later by
        //  StabilizationManager::set_user_focal_length via camera_db upfl.
        let lens = tag_map.get(&GroupId::Lens);
        let pixel_fl = lens.and_then(|m| m.get_t(TagId::PixelFocalLength) as Option<&Vec<f32>>);
        let fxfy: Option<(f32, f32)> = match pixel_fl {
            Some(v) if v.len() == 2 && v[0] > 1.0 && v[1] > 1.0 => Some((v[0], v[1])),
            _ => {
                let focal = lens.and_then(|m| m.get_t(TagId::FocalLength) as Option<&f32>).copied();
                let def = tag_map.get(&GroupId::Default);
                let sw = def.and_then(|m| m.get_t(TagId::SensorWidth) as Option<&f32>).copied();
                let sh = def.and_then(|m| m.get_t(TagId::SensorHeight) as Option<&f32>).copied();
                match (focal, sw, sh) {
                    (Some(focal), Some(sw), Some(sh))
                        if focal > 1.0 && sw > 0.0 && sh > 0.0 && size.0 > 0 && size.1 > 0 =>
                    {
                        Some((focal / sw * size.0 as f32, focal / sh * size.1 as f32))
                    }
                    _ => None,
                }
            }
        };
        if let Some((fx, fy)) = fxfy {
            build_synthetic_canon_lens_profile(md, input, tag_map, size, info, fx, fy);
        }
    }
}

/// Build the synthetic Canon lens profile (camera_matrix from fx/fy in pixels) and
/// store it on `md.lens_profile`. Shared by the PixelFocalLength (RF) path and the
/// FocalLength + sensor-size derivation so both produce an identical profile shape
/// (auto-sync disabled, official, pinhole distortion).
fn build_synthetic_canon_lens_profile(
    md: &mut FileMetadata,
    input: &telemetry_parser::Input,
    tag_map: &GroupedTagMap,
    size: (usize, usize),
    info: &telemetry_parser::util::SampleInfo,
    fx: f32,
    fy: f32,
) {
    let video_rotation = info.video_rotation.unwrap_or_default().abs();
    let is_vertical = video_rotation == 90 || video_rotation == 270;

    let focal_length = tag_map
        .get(&GroupId::Lens)
        .and_then(|x| x.get_t(TagId::FocalLength) as Option<&f32>)
        .copied();

    let lens_name = tag_map
        .get(&GroupId::Lens)
        .and_then(|map| map.get_t(TagId::DisplayName) as Option<&String>)
        .cloned()
        .unwrap_or_default();

    let camera_model = input
        .camera_model()
        .map(|x| x.to_string())
        .unwrap_or_default();

    // Held in canon_auto_lens_profile (not lens_profile) so plain load stays bare;
    // the batch senseflow apply activates it. See FileMetadata::canon_auto_lens_profile.
    md.canon_auto_lens_profile = Some(build_canon_lens_json(
        &camera_model,
        &lens_name,
        focal_length,
        size,
        is_vertical,
        md.frame_readout_time,
        fx,
        fy,
    ));
}

pub fn get_time_offset(
    md: &FileMetadata,
    _input: &telemetry_parser::Input,
    tag_map: &GroupedTagMap,
    sample_rate: f64,
    fps: f64,
) -> Option<f64> {
    let exposure = exposure_time_ms(tag_map)?;
    let frame_time = 1000.0 / md.frame_rate.unwrap_or(fps);
    let frame_readout_time = md.frame_readout_time.unwrap_or(14.0); // better approx than nothing
    let dt = 1000.0 / sample_rate.max(1.0);
    Some(frame_time + frame_readout_time / 2.0 - exposure / 2.0 - dt / 2.0)
}

fn exposure_time_ms(tag_map: &GroupedTagMap) -> Option<f64> {
    if let Some(exposure_ms) = tag_map
        .get(&GroupId::Imager)
        .and_then(|map| map.get_t(TagId::ExposureTime) as Option<&f64>)
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        return Some(exposure_ms);
    }
    if let Some(exposure_s) = tag_map
        .get(&GroupId::Exposure)
        .and_then(|map| map.get_t(TagId::ExposureTime) as Option<&f64>)
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0)
    {
        return Some(exposure_s * 1000.0);
    }

    let exposure = tag_map.get(&GroupId::Exposure)?;
    let (numerator, denominator) = (exposure.get_t(TagId::ShutterSpeed)
        as Option<&(u32, u32)>)
        .or_else(|| {
            exposure.get_t(TagId::Custom("ShutterSpeed2".into())) as Option<&(u32, u32)>
        })
        .copied()?;
    shutter_speed_ms(numerator, denominator)
}

fn shutter_speed_ms(numerator: u32, denominator: u32) -> Option<f64> {
    (numerator > 0 && denominator > 0)
        .then_some(numerator as f64 / denominator as f64 * 1000.0)
}

/// Pure builder for the synthetic Canon opencv_standard lens JSON. Split out of
/// `build_synthetic_canon_lens_profile` so the JSON shape (camera_matrix from
/// fx/fy in pixels, pinhole distortion, auto-sync disabled, official) is
/// unit-testable without a telemetry-parser `Input`.
fn build_canon_lens_json(
    camera_model: &str,
    lens_name: &str,
    focal_length_mm: Option<f32>,
    size: (usize, usize),
    is_vertical: bool,
    frame_readout_time: Option<f64>,
    fx: f32,
    fy: f32,
) -> serde_json::Value {
    let focal_length_str = focal_length_mm.map(|x| format!("{:.2} mm", x));
    let lens_model = if !lens_name.is_empty() {
        match &focal_length_str {
            Some(f) => format!("{lens_name} ({f})"),
            None => lens_name.to_string(),
        }
    } else {
        focal_length_str.clone().unwrap_or_default()
    };
    serde_json::json!({
        "calibrated_by": "Canon",
        "camera_brand": "Canon",
        "camera_model": camera_model,
        "lens_model": lens_model,
        "calib_dimension":  { "w": size.0, "h": size.1 },
        "orig_dimension":   { "w": size.0, "h": size.1 },
        "output_dimension": { "w": if is_vertical { size.1 } else { size.0 }, "h": if is_vertical { size.0 } else { size.1 } },
        "frame_readout_time": frame_readout_time,
        "official": true,
        "asymmetrical": false,
        "note": "",
        "fisheye_params": {
            "camera_matrix": [
                [ fx, 0.0, size.0 / 2 ],
                [ 0.0, fy, size.1 / 2 ],
                [ 0.0, 0.0, 1.0 ]
            ],
            "distortion_coeffs": []
        },
        "distortion_model": "opencv_standard",
        "sync_settings": {
            "initial_offset": 0,
            "initial_offset_inv": false,
            "search_size": 0.3,
            "max_sync_points": 5,
            "every_nth_frame": 1,
            "time_per_syncpoint": 0.5,
            "do_autosync": false
        },
        "calibrator_version": "---"
    })
}

#[cfg(test)]
mod tests {
    use super::{build_canon_lens_json, shutter_speed_ms};

    #[test]
    fn crm_proxy_canvas_preserves_focal_length_and_scan_time() {
        let (pixel_scale, readout_scale) = super::proxy_canvas_scale((6000, 3164), (1920, 1080)).unwrap();
        assert_eq!(pixel_scale, 0.32);
        let source_focal_px = 24.0 * 6000.0 / 35.9;
        assert!((source_focal_px * pixel_scale - 24.0 * 1920.0 / 35.9).abs() < 1e-9);
        let active_height = 3164.0 * pixel_scale;
        assert!((9.5 * readout_scale * active_height / 1080.0 - 9.5).abs() < 1e-9);
        assert_eq!(super::proxy_canvas_scale((6000, 3164), (6000, 3164)), None);
        assert_eq!(super::proxy_canvas_scale((6000, 3164), (0, 0)), None);
        assert_eq!(super::proxy_canvas_scale((0, 0), (1920, 1080)), None);
        assert_eq!(super::proxy_canvas_scale((3840, 2160), (1920, 1080)), Some((0.5, 1.0)));
        assert_eq!(super::proxy_canvas_scale((3840, 2160), (2048, 1080)), Some((0.5, 1.0)));
    }

    #[test]
    #[ignore = "Requires the R3 CRM fixture in GYROFLOW_CRM_FIXTURE"]
    fn crm_cmt_fixture_reaches_proxy_metadata() {
        use crate::gyro_source::{FileLoadOptions, GyroSource};
        use std::sync::{Arc, atomic::AtomicBool};
        let path = std::env::var("GYROFLOW_CRM_FIXTURE").unwrap();
        let parse = |size| {
            let mut file = std::io::BufReader::new(std::fs::File::open(&path).unwrap());
            let len = file.get_ref().metadata().unwrap().len() as usize;
            GyroSource::parse_telemetry_file(&mut file, len, &path, &FileLoadOptions::default(), size,
                60000.0 / 1001.0, |_| {}, Arc::new(AtomicBool::new(false))).unwrap()
        };
        let raw = parse((6000, 3164));
        let proxy = parse((1920, 1080));
        assert_eq!(proxy.camera_identifier.as_ref().unwrap().model, "R3");
        assert!(proxy.creation_date_utc.as_deref().unwrap().starts_with("2026:09:13 09:33:14"));
        assert!(proxy.lens_params.len() > 1);
        assert_eq!(proxy.lens_params.len(), raw.lens_params.len());
        let raw_focal = raw.lens_params.first_key_value().unwrap().1;
        let proxy_focal = proxy.lens_params.first_key_value().unwrap().1;
        assert_eq!(proxy_focal.focal_length, Some(24.0));
        assert!((proxy_focal.pixel_focal_length.unwrap().0 as f64 - 24.0 * 1920.0 / 35.9).abs() < 0.01);
        assert!((proxy_focal.pixel_focal_length.unwrap().0 / raw_focal.pixel_focal_length.unwrap().0 - 0.32).abs() < 1e-6);
        assert!((proxy.unit_pixel_focal_length.unwrap() - 1920.0 / 35.9).abs() < 1e-4);
        assert_eq!(raw.frame_readout_time, Some(9.5));
        assert!((proxy.frame_readout_time.unwrap() * 3164.0 * 0.32 / 1080.0 - 9.5).abs() < 1e-9);
        assert!(!proxy.keep_video_gyro);
        println!("CRM_PROXY_FIXTURE_PASS model=R3 focal=24mm samples={} upfl={:?} readout={:?} utc={:?}",
            proxy.lens_params.len(), proxy.unit_pixel_focal_length, proxy.frame_readout_time, proxy.creation_date_utc);
    }

    #[test]
    fn shutter_speed_ratio_is_converted_to_milliseconds() {
        assert_eq!(shutter_speed_ms(1, 200), Some(5.0));
        assert_eq!(shutter_speed_ms(1, 125), Some(8.0));
        assert_eq!(shutter_speed_ms(0, 200), None);
        assert_eq!(shutter_speed_ms(1, 0), None);
    }

    #[test]
    fn canon_lens_json_is_opencv_standard() {
        let v = build_canon_lens_json(
            "EOS R5 Mark II",
            "RF24-70mm F2.8",
            Some(50.0),
            (1920, 1080),
            false,
            Some(15.0),
            1000.0,
            1000.0,
        );
        assert_eq!(v["distortion_model"], "opencv_standard");
        assert_eq!(v["calibrated_by"], "Canon");
        assert_eq!(v["official"], true);
        // Auto-sync stays off: the built-in gyro is frame-aligned, never sync it.
        assert_eq!(v["sync_settings"]["do_autosync"], false);
        assert_eq!(v["camera_model"], "EOS R5 Mark II");
        assert_eq!(v["lens_model"], "RF24-70mm F2.8 (50.00 mm)");
        // Principal point is size / 2.
        assert_eq!(v["fisheye_params"]["camera_matrix"][0][2], 960);
        assert_eq!(v["fisheye_params"]["camera_matrix"][1][2], 540);
    }

    #[test]
    fn canon_lens_json_vertical_swaps_output_dimension() {
        let v = build_canon_lens_json(
            "EOS R5 Mark II",
            "",
            None,
            (1920, 1080),
            true,
            None,
            1000.0,
            1000.0,
        );
        assert_eq!(v["output_dimension"]["w"], 1080);
        assert_eq!(v["output_dimension"]["h"], 1920);
        // Empty lens name + no focal: lens_model degrades to an empty string.
        assert_eq!(v["lens_model"], "");
    }
}
