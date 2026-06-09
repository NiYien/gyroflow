// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2025 Adrian <adrian.eddy at gmail>

use crate::gyro_source::FileMetadata;
use telemetry_parser::tags_impl::{GetWithType, GroupId, GroupedTagMap, TagId};

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

    let focal_length_str = tag_map
        .get(&GroupId::Lens)
        .and_then(|x| x.get_t(TagId::FocalLength) as Option<&f32>)
        .map(|x| format!("{:.2} mm", *x));

    let mut lens_name = String::new();
    if let Some(v) = tag_map.get(&GroupId::Lens).and_then(|map| {
        map.get_t(TagId::DisplayName) as Option<&String>
    }) {
        lens_name = v.clone();
    }
    md.lens_profile = Some(serde_json::json!({
        "calibrated_by": "Canon",
        "camera_brand": "Canon",
        "camera_model": input.camera_model().map(|x| x.as_str()).unwrap_or(&""),
        "lens_model":   if !lens_name.is_empty() && focal_length_str.is_some() { format!("{lens_name} ({})", focal_length_str.unwrap()) } else if !lens_name.is_empty() { lens_name } else { focal_length_str.unwrap_or_default() },
        "calib_dimension":  { "w": size.0, "h": size.1 },
        "orig_dimension":   { "w": size.0, "h": size.1 },
        "output_dimension": { "w": if is_vertical { size.1 } else { size.0 }, "h": if is_vertical { size.0 } else { size.1 } },
        "frame_readout_time": md.frame_readout_time,
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
    }));
}

pub fn get_time_offset(
    md: &FileMetadata,
    _input: &telemetry_parser::Input,
    tag_map: &GroupedTagMap,
    sample_rate: f64,
    fps: f64,
) -> Option<f64> {
    let exposure = (tag_map.get(&GroupId::Imager)?.get_t(TagId::ExposureTime) as Option<&f64>)?;
    let frame_time = 1000.0 / md.frame_rate.unwrap_or(fps);
    let frame_readout_time = md.frame_readout_time.unwrap_or(14.0); // better approx than nothing
    let dt = 1000.0 / sample_rate.max(1.0);
    Some(frame_time + frame_readout_time / 2.0 - (*exposure) / 2.0 - dt / 2.0)
}
