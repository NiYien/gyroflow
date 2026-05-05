// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use crate::filesystem;
use crate::gyro_source::GyroSource;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

pub fn export_full_metadata(
    gyro_url: &str,
    _stab: &Arc<crate::StabilizationManager>,
) -> Result<String, crate::GyroflowCoreError> {
    let mut file = filesystem::open_file(&gyro_url, false, false)?;
    let filesize = file.size;
    let mut input = telemetry_parser::Input::from_stream(
        file.get_file(),
        filesize,
        &gyro_url,
        |_| (),
        Arc::new(AtomicBool::new(false)),
    )?;

    let mut output = Vec::new();
    if let Some(ref mut samples) = input.samples {
        output.reserve(samples.len());
        for info in samples {
            if info.tag_map.is_none() {
                continue;
            }

            let mut groups = BTreeMap::new();
            let groups_map = info.tag_map.as_ref().unwrap();

            for (group, map) in groups_map {
                let group_map = groups.entry(group).or_insert_with(BTreeMap::new);
                for (tagid, info) in map {
                    let value = serde_json::to_value(&info.value).unwrap();
                    group_map.insert(tagid, value);
                }
            }
            output.push(groups);
        }
    }
    Ok(serde_json::to_string_pretty(&output)?)
}

pub fn export_gyro_data(
    filename: &str,
    fields_json: &str,
    stab: &Arc<crate::StabilizationManager>,
) -> String {
    use crate::util::MapClosest;
    use std::fmt::Write;
    const RAD2DEG: f64 = 180.0 / std::f64::consts::PI;
    enum TimestampType {
        Milliseconds(f64),
        Microseconds(i64),
    }
    #[derive(PartialEq, Eq)]
    enum Format {
        Csv,
        Json,
        Usd,
        Jsx,
    }
    fn get(f: Option<[f64; 3]>, i: usize) -> f64 {
        f.map(|x| x[i]).unwrap_or_default()
    }

    let format = match filename.split('.').last().unwrap_or_default() {
        "csv" => Format::Csv,
        "json" => Format::Json,
        "usd" => Format::Usd,
        "jsx" => Format::Jsx,
        _ => Format::Csv,
    };

    let fields: serde_json::Value = serde_json::from_str(fields_json).unwrap();

    let all_samples = fields
        .get("export_all_samples")
        .and_then(|x| x.as_bool())
        .unwrap_or_default()
        && format != Format::Usd
        && format != Format::Jsx;

    let original = fields.get("original").and_then(|x| x.as_object()).unwrap();
    let stabilized = fields
        .get("stabilized")
        .and_then(|x| x.as_object())
        .unwrap();
    let zooming = fields.get("zooming").and_then(|x| x.as_object()).unwrap();

    let oaccl = original
        .get("accelerometer")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();
    let oeulr = original
        .get("euler_angles")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();
    let ogyro = original
        .get("gyroscope")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();
    let oquat = original
        .get("quaternion")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();
    let ofd = original
        .get("focus_distances")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();

    let seulr = stabilized
        .get("euler_angles")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();
    let squat = stabilized
        .get("quaternion")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();

    let focal_length = zooming
        .get("focal_length")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();
    let fovs = zooming
        .get("fovs")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();
    let minimal_fovs = zooming
        .get("minimal_fovs")
        .and_then(|x| x.as_bool())
        .unwrap_or_default();

    let mut output = String::new();
    let mut json = Vec::<std::collections::HashMap<&str, serde_json::Value>>::new();
    let mut jsx = std::collections::HashMap::<&str, serde_json::Value>::new();
    if format == Format::Csv {
        let _ = write!(output, "frame,timestamp_ms");
        if oaccl {
            let _ = write!(output, ",org_acc_x,org_acc_y,org_acc_z");
        }
        if oeulr {
            let _ = write!(output, ",org_pitch,org_yaw,org_roll");
        }
        if ogyro {
            let _ = write!(output, ",org_gyro_x,org_gyro_y,org_gyro_z");
        }
        if oquat {
            let _ = write!(output, ",org_quat_w,org_quat_x,org_quat_y,org_quat_z");
        }
        if ofd {
            let _ = write!(output, ",focus_distance");
        }
        if seulr {
            let _ = write!(output, ",stab_pitch,stab_yaw,stab_roll");
        }
        if squat {
            let _ = write!(output, ",stab_quat_w,stab_quat_x,stab_quat_y,stab_quat_z");
        }
        if focal_length {
            let _ = write!(output, ",focal_length");
        }
        if fovs {
            let _ = write!(output, ",fov_scale");
        }
        if minimal_fovs {
            let _ = write!(output, ",minimal_fov_scale");
        }
        let _ = write!(output, "\n");
    }

    let params = stab.params.read();
    let scaled_fps = params.get_scaled_fps();
    let frame_duration = 1000.0 / scaled_fps;

    let gyro = stab.gyro.read();
    let file_metadata = gyro.file_metadata.read();
    let mut focal_length_value = stab.lens.read().focal_length;

    let timestamps: Vec<(Option<usize>, usize, TimestampType, f64)> = if all_samples {
        let mut frame = 0;
        gyro.quaternions
            .keys()
            .enumerate()
            .map(|(i, ts)| {
                let mut timestamp_ms = *ts as f64 / 1000.0;
                timestamp_ms += gyro.offset_at_gyro_timestamp(timestamp_ms);

                let final_timestamp = timestamp_ms
                    - file_metadata
                        .per_frame_time_offsets
                        .get(frame)
                        .unwrap_or(&0.0);
                if final_timestamp >= (frame + 1) as f64 * frame_duration {
                    frame += 1;
                }

                (
                    Some(i),
                    frame,
                    TimestampType::Microseconds(*ts),
                    final_timestamp,
                )
            })
            .collect()
    } else {
        (0..params.frame_count)
            .map(|frame| {
                let timestamp_ms =
                    frame as f64 / scaled_fps * 1000.0 + (params.frame_readout_time / 2.0);

                let middle_timestamp = timestamp_ms
                    + file_metadata
                        .per_frame_time_offsets
                        .get(frame)
                        .unwrap_or(&0.0);

                (
                    None,
                    frame,
                    TimestampType::Milliseconds(middle_timestamp),
                    timestamp_ms,
                )
            })
            .collect()
    };
    let num_frames = params.frame_count;
    let fps = params.get_scaled_fps();
    let frame_times = (0..num_frames).map(|i| i as f64 / fps).collect::<Vec<_>>();

    if format == Format::Usd {
        output.push_str(&format!(
            r#"#usda 1.0
            (
                defaultPrim = "root"
                doc = "Gyroflow"
                endTimeCode = {num_frames}
                metersPerUnit = 1
                startTimeCode = 1
                timeCodesPerSecond = {fps:.6}
                upAxis = "Z"
            )
            def Xform "root" (
                customData = {{
                    dictionary Blender = {{
                        bool generated = 1
                    }}
                }}
            )
            {{
                def Xform "GyroflowCamera"
                {{
                    matrix4d xformOp:transform.timeSamples = {{
            "#
        ));
    }

    if format == Format::Jsx {
        let duration_s = params.duration_ms / 1000.0;
        jsx.insert("duration_s", duration_s.into());
        jsx.insert("frame_times", frame_times.into());
        jsx.insert("orientations", Vec::<serde_json::Value>::new().into());
    }

    let raw_imu = gyro.raw_imu(&file_metadata);

    for (i, frame, ts, timestamp_ms) in timestamps {
        let raw_imu = raw_imu
            .get(i.unwrap_or(usize::MAX))
            .cloned()
            .unwrap_or_default();
        let quat_org = match ts {
            TimestampType::Microseconds(ts) => *gyro.quaternions.get(&ts).unwrap(),
            TimestampType::Milliseconds(ts) => gyro.org_quat_at_timestamp(ts),
        };
        let quate = quat_org.euler_angles();
        let quatv = quat_org.as_vector();
        let val_oaccl = [
            get(raw_imu.accl, 0),
            get(raw_imu.accl, 1),
            get(raw_imu.accl, 2),
        ];
        let val_oeulr = [quate.0 * RAD2DEG, quate.1 * RAD2DEG, quate.2 * RAD2DEG];
        let val_ogyro = [
            get(raw_imu.gyro, 0),
            get(raw_imu.gyro, 1),
            get(raw_imu.gyro, 2),
        ];
        let val_oquat = [quatv[3], quatv[0], quatv[1], quatv[2]];
        let mut val_ofd = 0.0_f32;

        if format == Format::Jsx && !(seulr && !oeulr) {
            jsx.get_mut("orientations")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(serde_json::to_value([val_oeulr[0], -val_oeulr[2], val_oeulr[1]]).unwrap());
        }
        if format == Format::Usd && !(seulr && !oeulr) {
            let matrix = nalgebra::Matrix4::from(quat_org);
            output.push_str(&format!("                {}: ( ({},{},{}, 0), ({}, {}, {}, 0), ({}, {}, {}, 0), (7.0, -7.0, 1.5, 1) ),\n",
                frame + 1,
                matrix[(0, 0)], matrix[(1, 0)], matrix[(2, 0)],
                matrix[(0, 1)], matrix[(1, 1)], matrix[(2, 1)],
                matrix[(0, 2)], matrix[(1, 2)], matrix[(2, 2)]
            ));
        }

        let quat_smooth = match ts {
            TimestampType::Microseconds(ts) => {
                let in_smoothed_range = gyro
                    .smoothed_quaternions
                    .keys()
                    .next()
                    .zip(gyro.smoothed_quaternions.keys().next_back())
                    .map(|(&first_ts, &last_ts)| ts >= first_ts && ts <= last_ts)
                    .unwrap_or(false);
                in_smoothed_range.then(|| {
                    GyroSource::clamped_quat_at_gyro_timestamp(
                        &gyro.smoothed_quaternions,
                        ts as f64 / 1000.0,
                    )
                })
            }
            TimestampType::Milliseconds(ts) => Some(gyro.smoothed_quat_at_timestamp(ts)),
        };

        // smoothed_quaternions is the quaternion needed to stabilize, but in this case we want to get the stabilized camera motion
        // we need to reverse the calculation done by gyroflow to get original smoothed quaternion
        let quat_smooth = quat_smooth.map(|quat_smooth| (quat_smooth / quat_org).inverse());
        let (val_seulr, val_squat) = if let Some(quat_smooth) = quat_smooth {
            let quate = quat_smooth.euler_angles();
            let quatv = quat_smooth.as_vector();
            (
                Some([quate.0 * RAD2DEG, quate.1 * RAD2DEG, quate.2 * RAD2DEG]),
                Some([quatv[3], quatv[0], quatv[1], quatv[2]]),
            )
        } else {
            (None, None)
        };

        if format == Format::Jsx && (seulr && !oeulr) {
            jsx.get_mut("orientations")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(serde_json::to_value(val_seulr.map(|x| [x[0], -x[2], x[1]])).unwrap());
        }
        if format == Format::Usd && (seulr && !oeulr) && quat_smooth.is_some() {
            let quat_smooth = quat_smooth.unwrap();
            let matrix = nalgebra::Matrix4::from(quat_smooth);
            output.push_str(&format!("                {}: ( ({},{},{}, 0), ({}, {}, {}, 0), ({}, {}, {}, 0), (7.0, -7.0, 1.5, 1) ),\n",
                frame + 1,
                matrix[(0, 0)], matrix[(1, 0)], matrix[(2, 0)],
                matrix[(0, 1)], matrix[(1, 1)], matrix[(2, 1)],
                matrix[(0, 2)], matrix[(1, 2)], matrix[(2, 2)]
            ));
        }

        if let Some(val) = file_metadata
            .lens_params
            .get_closest(&((timestamp_ms * 1000.0).round() as i64), 100000)
        {
            // closest within 100ms
            if let Some(fl) = val.focal_length {
                focal_length_value = Some(fl as f64);
            }
            if let Some(fd) = val.focus_distance {
                val_ofd = fd;
            }
        }
        let val_fl = focal_length_value.unwrap_or(0.0);
        let val_fov = *params.fovs.get(frame).unwrap_or(&0.0);
        let val_minimal_fov = *params.minimal_fovs.get(frame).unwrap_or(&0.0);

        if format == Format::Csv {
            let _ = write!(output, "{frame},{timestamp_ms:.3}");
            if oaccl {
                let _ = write!(
                    output,
                    ",{:.6},{:.6},{:.6}",
                    val_oaccl[0], val_oaccl[1], val_oaccl[2]
                );
            }
            if oeulr {
                let _ = write!(
                    output,
                    ",{:.3},{:.3},{:.3}",
                    val_oeulr[0], val_oeulr[1], val_oeulr[2]
                );
            }
            if ogyro {
                let _ = write!(
                    output,
                    ",{:.6},{:.6},{:.6}",
                    val_ogyro[0], val_ogyro[1], val_ogyro[2]
                );
            }
            if oquat {
                let _ = write!(
                    output,
                    ",{:.6},{:.6},{:.6},{:.6}",
                    val_oquat[0], val_oquat[1], val_oquat[2], val_oquat[3]
                );
            }
            if ofd {
                let _ = write!(output, ",{:.3}", val_ofd);
            }
            if seulr {
                if let Some(val_seulr) = val_seulr {
                    let _ = write!(
                        output,
                        ",{:.3},{:.3},{:.3}",
                        val_seulr[0], val_seulr[1], val_seulr[2]
                    );
                } else {
                    let _ = write!(output, ",,,");
                }
            }
            if squat {
                if let Some(val_squat) = val_squat {
                    let _ = write!(
                        output,
                        ",{:.6},{:.6},{:.6},{:.6}",
                        val_squat[0], val_squat[1], val_squat[2], val_squat[3]
                    );
                } else {
                    let _ = write!(output, ",,,,");
                }
            }
            if focal_length {
                let _ = write!(output, ",{val_fl:.3}");
            }
            if fovs {
                let _ = write!(output, ",{val_fov:.3}");
            }
            if minimal_fovs {
                let _ = write!(output, ",{val_minimal_fov:.3}");
            }
            let _ = write!(output, "\n");
        } else if format == Format::Json {
            let mut obj = std::collections::HashMap::new();
            obj.insert("frame", frame.into());
            obj.insert("timestamp_ms", timestamp_ms.into());
            if oaccl {
                obj.insert("org_acc", serde_json::to_value(val_oaccl).unwrap());
            }
            if oeulr {
                obj.insert("org_euler", serde_json::to_value(val_oeulr).unwrap());
            }
            if ogyro {
                obj.insert("org_gyro", serde_json::to_value(val_ogyro).unwrap());
            }
            if oquat {
                obj.insert("org_quat", serde_json::to_value(val_oquat).unwrap());
            }
            if ofd {
                obj.insert("focus_distance", serde_json::to_value(val_ofd).unwrap());
            }
            if seulr && val_seulr.is_some() {
                obj.insert("stab_euler", serde_json::to_value(val_seulr.unwrap()).unwrap());
            }
            if squat && val_squat.is_some() {
                obj.insert("stab_quat", serde_json::to_value(val_squat.unwrap()).unwrap());
            }
            if focal_length {
                obj.insert("focal_length", val_fl.into());
            }
            if fovs {
                obj.insert("fov_scale", val_fov.into());
            }
            if minimal_fovs {
                obj.insert("minimal_fov_scale", val_minimal_fov.into());
            }
            json.push(obj);
        }
    }
    let mut comp_params = crate::stabilization::ComputeParams::from_manager(stab);

    if format == Format::Jsx {
        output = output.trim_end_matches(",\n").to_string();
        output.push_str("]);\n");

        let (camera_matrix, _, _, _, _, _) =
            crate::stabilization::FrameTransform::get_lens_data_at_timestamp(
                &comp_params,
                0.0,
                false,
            );
        jsx.insert("zoom", camera_matrix[(0, 0)].into());
        if comp_params
            .gyro
            .read()
            .file_metadata
            .read()
            .lens_params
            .len()
            > 1
        {
            jsx.insert("zooms", Vec::<serde_json::Value>::new().into());
            for f in 0..num_frames as i32 {
                let timestamp = crate::timestamp_at_frame(f, fps);
                let (camera_matrix, _, _, _, _, _) =
                    crate::stabilization::FrameTransform::get_lens_data_at_timestamp(
                        &comp_params,
                        timestamp,
                        false,
                    );
                jsx.get_mut("zooms")
                    .unwrap()
                    .as_array_mut()
                    .unwrap()
                    .push(camera_matrix[(0, 0)].into());
            }
        }
    }

    if format == Format::Jsx {
        format!(
            r#"var data = {};
            var comp = app.project.activeItem;
            var GyroflowCamera = comp.layers.addCamera("GyroflowCamera",[0,0]);
            GyroflowCamera.inPoint = 0.0;
            GyroflowCamera.outPoint = data["duration_s"];
            GyroflowCamera.property("orientation").setValuesAtTimes(data["frame_times"], data["orientations"]);
            GyroflowCamera.property("zoom").setValue(data["zoom"]);
            if (data["zooms"].length)
                GyroflowCamera.property("zoom").setValuesAtTimes(data["frame_times"], data["zooms"]);"#,
            serde_json::to_string(&jsx).unwrap()
        )
    } else if format == Format::Csv {
        output
    } else if format == Format::Usd {
        let aspect = params.size.0 as f64 / params.size.1 as f64;
        let width_mm = 35.0;
        let height_mm = width_mm / aspect;

        comp_params.calculate_camera_fovs();

        output.push_str("\n}");
        let fov = comp_params.camera_diagonal_fovs.first().unwrap();
        let diagonal_mm = (width_mm.powi(2) + height_mm.powi(2)).sqrt();
        let focal_length_mm = diagonal_mm / (2.0 * (fov.to_radians() / 2.0).tan()) / 100.0;

        let focal_lengths = {
            let mut fls = String::new();
            if comp_params.camera_diagonal_fovs.len() > 1 {
                fls.push_str("float focalLength.timeSamples = {\n");
                for (frame, fov) in comp_params.camera_diagonal_fovs.iter().enumerate() {
                    let focal_length_mm =
                        diagonal_mm / (2.0 * (fov.to_radians() / 2.0).tan()) / 100.0;
                    fls.push_str(&format!(
                        "                {}: {focal_length_mm},\n",
                        frame + 1
                    ));
                }
                fls.push_str("}");
            }
            fls
        };

        output.push_str(&format!(
            r#"
                uniform token[] xformOpOrder = ["xformOp:transform"]

                def Camera "GyroflowCamera"
                {{
                    float2 clippingRange = (0.1, 100)
                    float focalLength = {focal_length_mm}
                    {focal_lengths}
                    float horizontalAperture = {}
                    float horizontalApertureOffset = 0
                    token projection = "perspective"
                    float verticalAperture = {}
                    float verticalApertureOffset = 0
                }}
            }}
        }}"#,
            width_mm / 100.0,
            height_mm / 100.0
        ));

        output
    } else {
        serde_json::to_string(&json).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gyro_source::Quat64;

    #[test]
    fn all_samples_export_leaves_stabilized_fields_empty_outside_smoothed_range() {
        let stab = Arc::new(crate::StabilizationManager::default());
        {
            let mut params = stab.params.write();
            params.fps = 1.0;
            params.frame_count = 3;
            params.duration_ms = 3_000.0;
        }
        {
            let mut gyro = stab.gyro.write();
            gyro.duration_ms = 5_000.0;
            gyro.quaternions = (0..=4)
                .map(|second| {
                    (
                        second * 1_000_000,
                        Quat64::from_euler_angles(0.0, second as f64 * 0.1, 0.0),
                    )
                })
                .collect();
            gyro.smoothed_quaternions = (0..=2)
                .map(|second| {
                    (
                        second * 1_000_000,
                        Quat64::from_euler_angles(0.0, second as f64 * 0.05, 0.0),
                    )
                })
                .collect();
        }

        let fields = serde_json::json!({
            "export_all_samples": true,
            "original": { "quaternion": true },
            "stabilized": { "quaternion": true },
            "zooming": {}
        });

        let csv = export_gyro_data("gyro.csv", &fields.to_string(), &stab);
        let out_of_range_row = csv
            .lines()
            .find(|line| line.starts_with("3,3000.000"))
            .unwrap();

        assert!(out_of_range_row.ends_with(",,,,"));
    }
}
