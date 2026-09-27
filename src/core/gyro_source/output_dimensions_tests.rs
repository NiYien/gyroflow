// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::{ExternalIoPolicy, GyroflowProjectType, StabilizationManager};
use serde_json::json;
use telemetry_parser::tags_impl::{GroupedTagMap, TagDescription, TagValue, ValueType};

const SIZE: (usize, usize) = (3840, 2160);
const IMU_LOG: &[u8] = b"GYROFLOW IMU LOG\nversion,1.3\nid,rotation-test\norientation,XYZ\ntscale,0.001\ngscale,1\nascale,1\nt,gx,gy,gz,ax,ay,az\n0,0,0,0,0,0,1\n1,0,0,0,0,0,1\n";

fn insert(tags: &mut GroupedTagMap, group: GroupId, id: TagId, value: TagValue) {
    tags.entry(group.clone()).or_default().insert(
        id.clone(),
        TagDescription {
            group,
            id,
            value,
            native_id: None,
            description: String::new(),
        },
    );
}

fn camera_metadata(brand: &str, rotation: i32, sony_fallback: bool) -> FileMetadata {
    // Input supplies only the optional model name here. Camera tags below exercise
    // the real Sony/Canon lens builders without requiring external video fixtures.
    let input = Input::from_stream(
        &mut std::io::Cursor::new(IMU_LOG),
        IMU_LOG.len(),
        "rotation-test.gcsv",
        |_| {},
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let mut md = FileMetadata {
        detected_source: Some(format!("{brand} test")),
        unit_pixel_focal_length: Some(100.0),
        ..Default::default()
    };
    md.lens_params.insert(
        0,
        LensParams {
            focal_length: Some(35.0),
            pixel_focal_length: Some((3500.0, 3500.0)),
            ..Default::default()
        },
    );
    let mut tags = GroupedTagMap::new();
    insert(
        &mut tags,
        GroupId::Lens,
        TagId::FocalLength,
        TagValue::f32(ValueType::new_parsed(|x| x.to_string(), 35.0, vec![])),
    );
    let info = telemetry_parser::util::SampleInfo {
        video_rotation: Some(rotation),
        ..Default::default()
    };
    if brand == "Sony" {
        insert(
            &mut tags,
            GroupId::Imager,
            TagId::PixelPitch,
            TagValue::u32x2(ValueType::new_parsed(
                |x| format!("{x:?}"),
                (9000, 9000),
                vec![],
            )),
        );
        insert(
            &mut tags,
            GroupId::Imager,
            TagId::CaptureAreaSize,
            TagValue::f32x2(ValueType::new_parsed(
                |x| format!("{x:?}"),
                (3840.0, 2160.0),
                vec![],
            )),
        );
        insert(
            &mut tags,
            GroupId::Imager,
            TagId::SensorSizePixels,
            TagValue::u32x2(ValueType::new_parsed(
                |x| format!("{x:?}"),
                (3840, 2160),
                vec![],
            )),
        );
        for (id, value) in [(TagId::SensorWidth, 34.56), (TagId::SensorHeight, 19.44)] {
            insert(
                &mut tags,
                GroupId::Default,
                id,
                TagValue::f32(ValueType::new_parsed(|x| x.to_string(), value, vec![])),
            );
        }
        let coeffs: Vec<f64> = if sony_fallback {
            vec![]
        } else {
            (1..=10).map(|x| x as f64 * 2.0).collect()
        };
        insert(
            &mut tags,
            GroupId::Custom("LensDistortion".into()),
            TagId::Data,
            TagValue::Json(ValueType::new_parsed(
                |x| x.to_string(),
                json!({
                    "focal_length_nm": 35000000.0, "effective_sensor_height_nm": 20000000.0,
                    "coeff_scale": 1.0, "coeffs": coeffs
                }),
                vec![],
            )),
        );
        sony::init_lens_profile(&mut md, &input, &tags, SIZE, &info);
        assert!(md.lens_profile.is_some());
    } else {
        insert(
            &mut tags,
            GroupId::Lens,
            TagId::PixelFocalLength,
            TagValue::Vec_f32(ValueType::new_parsed(
                |x| format!("{x:?}"),
                vec![3500.0, 3500.0],
                vec![],
            )),
        );
        canon::init_lens_profile(&mut md, &input, &tags, SIZE, &info);
        assert!(
            md.lens_profile.is_none(),
            "Canon must still defer its lens until batch matching"
        );
        assert!(md.canon_auto_lens_profile.is_some());
    }
    md
}

#[test]
fn automatic_camera_output_dimensions_do_not_encode_video_rotation() {
    for (brand, fallback) in [("Sony", false), ("Sony", true), ("Canon", false)] {
        for rotation in [0, 90, 180, 270] {
            let md = camera_metadata(brand, rotation, fallback);
            let profile = md.lens_profile.or(md.canon_auto_lens_profile).unwrap();
            assert!(
                profile.get("output_dimension").is_none_or(|x| x.is_null()),
                "{brand} fallback={fallback} rotation={rotation}: {profile}"
            );
            assert_eq!(profile["calib_dimension"], json!({"w":3840,"h":2160}));
        }
    }
}

#[test]
fn legacy_camera_output_dimensions_survive_project_restore_without_double_rotation() {
    let dir = tempfile::tempdir().unwrap();
    crate::settings::with_test_settings_file(dir.path().join("settings.json"), || {
        for (brand, fallback) in [("Sony", false), ("Sony", true), ("Canon", false)] {
            for rotation in [0, 90, 180, 270] {
                let md = camera_metadata(brand, rotation, fallback);
                let mut legacy_lens = md.lens_profile.or(md.canon_auto_lens_profile).unwrap();
                let expected = if matches!(rotation, 90 | 270) {
                    (2160, 3840)
                } else {
                    SIZE
                };
                legacy_lens["output_dimension"] = json!({"w":expected.0,"h":expected.1});
                let camera_matrix: Vec<[f64; 3]> =
                    serde_json::from_value(legacy_lens["fisheye_params"]["camera_matrix"].clone())
                        .unwrap();
                let project = json!({
                    "title":"Gyroflow data file", "version":4, "videofile":"file:///rotation-test.mp4",
                    "video_info":{"width":3840,"height":2160,"rotation":rotation,"fps":60.0},
                    "calibration_data":legacy_lens,
                    "output":{"output_width":expected.0,"output_height":expected.1}
                });
                let stab = StabilizationManager::default();
                let mut is_preset = false;
                stab.import_gyroflow_data_with_policy(
                    project.to_string().as_bytes(),
                    true,
                    None,
                    |_| {},
                    Arc::new(AtomicBool::new(false)),
                    &mut is_preset,
                    false,
                    ExternalIoPolicy::Deny,
                )
                .unwrap();
                assert!(!is_preset);
                assert_eq!(stab.params.read().output_size, expected);
                assert_eq!(stab.restore_project_lens_to_main(), Some(expected));
                assert_eq!(
                    stab.params.read().output_size,
                    expected,
                    "{brand} fallback={fallback} rotation={rotation}"
                );
                let exported: serde_json::Value = serde_json::from_str(
                    &stab
                        .export_gyroflow_data(GyroflowProjectType::Simple, "{}", None)
                        .unwrap(),
                )
                .unwrap();
                assert!(exported["calibration_data"]["output_dimension"].is_null());
                let exported_matrix: Vec<[f64; 3]> = serde_json::from_value(
                    exported["calibration_data"]["fisheye_params"]["camera_matrix"].clone(),
                )
                .unwrap();
                // JSON round-trips can change the last floating-point bit.
                for (actual, expected) in exported_matrix
                    .iter()
                    .flatten()
                    .zip(camera_matrix.iter().flatten())
                {
                    assert!((actual - expected).abs() < 1e-9);
                }
            }
        }
    });
}

#[test]
fn legacy_camera_output_dimensions_keep_explicit_project_crop() {
    let dir = tempfile::tempdir().unwrap();
    crate::settings::with_test_settings_file(dir.path().join("settings.json"), || {
        let md = camera_metadata("Canon", 270, false);
        let mut lens = md.canon_auto_lens_profile.unwrap();
        lens["output_dimension"] = json!({"w":2160,"h":3840});
        let project = json!({
            "title":"Gyroflow data file", "version":4, "videofile":"file:///rotation-test.mp4",
            "video_info":{"width":3840,"height":2160,"rotation":90,"fps":60.0},
            "calibration_data":lens, "output":{"output_width":1080,"output_height":1080}
        });
        let stab = StabilizationManager::default();
        stab.import_gyroflow_data_with_policy(
            project.to_string().as_bytes(),
            true,
            None,
            |_| {},
            Arc::new(AtomicBool::new(false)),
            &mut false,
            false,
            ExternalIoPolicy::Deny,
        )
        .unwrap();
        assert_eq!(stab.params.read().output_size, (2160, 2160));
    });
}

#[test]
fn bare_video_output_dimensions_follow_container_rotation() {
    for raw_rotation in [0, 90, 180, 270] {
        let stab = StabilizationManager::default();
        stab.lens_profile_db.write().loaded = true;
        let metadata = telemetry_parser::util::VideoMetadata {
            width: 3840,
            height: 2160,
            fps: 60.0,
            duration_s: 0.05,
            rotation: raw_rotation,
        };
        stab.load_video_file(
            &mut std::io::Cursor::new(IMU_LOG),
            IMU_LOG.len(),
            "rotation-test.gcsv",
            Some(metadata),
            true,
        )
        .unwrap();
        let params = stab.params.read();
        let expected = if matches!(raw_rotation, 90 | 270) {
            (2160, 3840)
        } else {
            SIZE
        };
        assert_eq!(params.output_size, expected);
        assert_eq!(params.video_rotation, ((360 - raw_rotation) % 360) as f64);
    }
}
