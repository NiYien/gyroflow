// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::{StabilizationManager, stabilization::{ComputeParams, FrameTransform, Stabilization}};
use std::{fs::File, io::{BufWriter, Write}, sync::{Arc, atomic::AtomicBool}};

fn preview_export_grid(params: &ComputeParams, transform: &FrameTransform) -> Vec<Option<(f64, f64)>> {
    let kernel = &transform.kernel_params;
    let mesh: Vec<f64> = transform.mesh_data.iter().map(|value| *value as f64).collect();
    let mut points = Vec::with_capacity(65 * 37);
    for y in 0..37 { for x in 0..65 {
        let output = (x as f32 * (params.output_width - 1) as f32 / 64.0,
            y as f32 * (params.output_height - 1) as f32 / 36.0);
        let mut row = transform.matrices.len() / 2;
        let mut source = None;
        for _ in 0..3 {
            source = Stabilization::rotate_and_distort(output, row, kernel, &transform.matrices,
                &params.distortion_model, params.digital_lens.as_ref(), kernel.r_limit * kernel.r_limit, &mesh);
            if let Some((sx, sy)) = source.filter(|(sx, sy)| sx.is_finite() && sy.is_finite()) {
                let coordinate = if params.frame_readout_direction.is_horizontal() { sx } else { sy };
                row = coordinate.round().clamp(0.0, (transform.matrices.len() - 1) as f32) as usize;
            } else {
                source = None;
                break;
            }
        }
        points.push(source.map(|(sx, sy)| (sx as f64 * 3840.0 / params.width as f64,
            sy as f64 * 2160.0 / params.height as f64)));
    }}
    points
}

#[test]
#[ignore = "requires the explicitly supplied GUI-saved R50 V project"]
fn translation_preview_export_core_acceptance() {
    use crate::gpu::{BufferDescription, Buffers};
    use crate::stabilization::RGBA8;
    const THRESHOLD_4K_PX: f64 = 0.2;
    const FRAMES: [usize; 4] = [246, 520, 778, 940];
    let project = std::env::var("GYROFLOW_TRANSLATION_PREVIEW_EXPORT_PROJECT")
        .expect("set GYROFLOW_TRANSLATION_PREVIEW_EXPORT_PROJECT to the GUI-saved project");
    let preview = StabilizationManager::default();
    preview.import_gyroflow_file(&crate::filesystem::path_to_url(&project), true,
        |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();

    // Import initializes the media geometry. Force a fresh preview worker instead of testing its import cache.
    preview.invalidate_smoothing();
    let (sender, receiver) = std::sync::mpsc::channel();
    let compute_id = preview.recompute_threaded(move |result| { let _ = sender.send(result); });
    assert_eq!(receiver.recv_timeout(std::time::Duration::from_secs(60)).unwrap(), (compute_id, false));
    let preview_params = ComputeParams::from_manager(&preview);
    let preview_translation = preview.gyro.read().optical_translation.clone().unwrap();

    let export = preview.get_cloned();
    // Also exercise a fresh blocking pass after the clone, even when its normal queue pass could reuse the cache.
    export.invalidate_smoothing();
    export.recompute_blocking();
    let export_params = ComputeParams::from_manager(&export);
    let export_translation = export.gyro.read().optical_translation.clone().unwrap();
    assert!(preview_params.frame_count > FRAMES[3]);
    assert_eq!((preview_params.width, preview_params.height), (3840, 2160));
    assert_eq!((preview_params.width, preview_params.height, preview_params.output_width, preview_params.output_height),
        (export_params.width, export_params.height, export_params.output_width, export_params.output_height));
    assert_eq!(preview_params.scaled_fps, export_params.scaled_fps);
    let independent_gyro = !Arc::ptr_eq(&preview_params.gyro, &export_params.gyro);
    let translation_active = preview_translation.is_active() && export_translation.is_active();
    let curve_equal = preview_translation.samples == export_translation.samples
        && preview_translation.samples.iter().all(|sample| {
            let time = sample.timestamp_us as f64 / 1000.0;
            preview_translation.shift_at(time) == export_translation.shift_at(time)
        });
    let buffers = Buffers {
        input: BufferDescription { size: (preview_params.width, preview_params.height, preview_params.width * 4), ..Default::default() },
        output: BufferDescription { size: (preview_params.output_width, preview_params.output_height, preview_params.output_width * 4), ..Default::default() },
    };
    let mut checks = Vec::new();
    let mut maximum = 0.0f64;
    let mut all_valid = true;
    for frame in FRAMES {
        let timestamp_us = (crate::timestamp_at_frame(frame as i32, preview_params.scaled_fps) * 1000.0).round() as i64;
        // Read the actual parameters published to the preview renderer by the threaded worker.
        let preview_transform = preview.stabilization.read().get_frame_transform_at::<RGBA8>(timestamp_us, Some(frame), &buffers);
        let mut export_transform = FrameTransform::at_timestamp(&export_params, timestamp_us as f64 / 1000.0, frame);
        // These dimensions are filled by the render buffers after FrameTransform builds its matrices.
        export_transform.kernel_params.width = export_params.width as i32;
        export_transform.kernel_params.height = export_params.height as i32;
        export_transform.kernel_params.output_width = export_params.output_width as i32;
        export_transform.kernel_params.output_height = export_params.output_height as i32;
        let a = preview_export_grid(&preview_params, &preview_transform);
        let b = preview_export_grid(&export_params, &export_transform);
        let (mut compared, mut invalid, mut frame_max, mut squared_sum) = (0usize, 0usize, 0.0f64, 0.0f64);
        let mut worst = serde_json::Value::Null;
        for (index, (a, b)) in a.iter().zip(&b).enumerate() {
            if let (Some(a), Some(b)) = (a, b) {
                let distance = (a.0 - b.0).hypot(a.1 - b.1);
                if worst.is_null() || distance > frame_max {
                    frame_max = distance;
                    worst = serde_json::json!({"grid_x":index % 65,"grid_y":index / 65,
                        "preview_source_4k_px":[a.0,a.1],"export_source_4k_px":[b.0,b.1]});
                }
                squared_sum += distance * distance;
                compared += 1;
            } else { invalid += 1; }
        }
        maximum = maximum.max(frame_max);
        let passed = invalid == 0 && compared == 65 * 37 && frame_max < THRESHOLD_4K_PX;
        all_valid &= passed;
        checks.push(serde_json::json!({"frame":frame,"timestamp_us":timestamp_us,"compared_points":compared,
            "invalid_points":invalid,"max_difference_4k_px":frame_max,
            "rms_difference_4k_px":(compared > 0).then(|| (squared_sum / compared as f64).sqrt()),
            "preview_fov":preview_transform.fov,"export_fov":export_transform.fov,"worst_point":worst,"passed":passed}));
    }
    let passed = independent_gyro && translation_active && curve_equal && all_valid;
    let report = serde_json::json!({"project":project,"grid":[65,37],"input_size":[preview_params.width,preview_params.height],
        "output_size":[preview_params.output_width,preview_params.output_height],"fps":preview_params.scaled_fps,
        "preview_path":"import -> invalidate_smoothing -> recompute_threaded -> published FrameTransform",
        "export_path":"get_cloned -> invalidate_smoothing -> recompute_blocking -> from_manager -> FrameTransform",
        "independent_gyro":independent_gyro,"translation_active":translation_active,"curve_samples":preview_translation.samples.len(),
        "curves_samplewise_equal":curve_equal,"fixed_crop_override":false,"threshold_4k_px":THRESHOLD_4K_PX,
        "max_difference_4k_px":maximum,"checks":checks,"passed":passed});
    let output = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/translation-image-space/preview-export-core.json");
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    std::fs::write(&output, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("preview/export core maximum={maximum:.12} 4K px, passed={passed}, report={}", output.display());
    assert!(passed, "preview/export core comparison failed; see {}", output.display());
}

#[test]
#[ignore = "requires a reanalyzed project and maps from the frozen baseline binary"]
fn translation_image_space_projection_acceptance() {
    let config_path = std::env::var("GYROFLOW_TRANSLATION_ACCEPTANCE").unwrap();
    let config: serde_json::Value = serde_json::from_reader(File::open(config_path).unwrap()).unwrap();
    let manager = StabilizationManager::default();
    manager.import_gyroflow_file(&crate::filesystem::path_to_url(config["project"].as_str().unwrap()), true,
        |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();
    manager.set_translation_stabilization_enabled(true);
    manager.set_translation_reference(1.0);
    manager.set_translation_smoothness(0.962716);
    manager.set_translation_along_axis(true);
    manager.recompute_blocking();
    let mut params = ComputeParams::from_manager(&manager);
    if let Some(values) = config["fixed_fovs"].as_array() {
        params.fovs = values.iter().map(|v| v.as_f64().unwrap()).collect();
        assert_eq!(params.fovs.len(), params.frame_count);
    }
    let result = manager.gyro.read().optical_translation.clone().unwrap();
    assert!(result.is_active() && result.geometry_version == 2);
    let zooms: Vec<_> = params.fovs.iter().map(|f| 1.0 / f).collect();
    let mut sorted_zooms = zooms.clone();
    let median_zoom = median(&mut sorted_zooms);
    let curves: Vec<_> = result.samples.iter().map(|s| {
        let shift = result.shift_at(s.timestamp_us as f64 / 1000.0);
        serde_json::json!([s.timestamp_us, shift.x, shift.y, shift.z])
    }).collect();
    let mut metadata = serde_json::json!({"frames":params.frame_count,"fps":params.scaled_fps,
        "fixed_fovs":params.fovs,"maximum_zoom":zooms.iter().copied().fold(0.0, f64::max),"median_zoom":median_zoom,
        "grid":[65,37],"size":[640,360],"variants":["off","image_space","previous"],
        "info":manager.translation_stabilization_info(),"samples":result.samples,"curve":curves});
    if config["crop_only"].as_bool() != Some(true) {
        let baseline = std::fs::read(config["previous_maps"].as_str().unwrap()).unwrap();
        const BYTES: usize = 65 * 37 * 2 * 4;
        assert_eq!(baseline.len(), params.frame_count * 3 * BYTES);
        let mut output = BufWriter::new(File::create(config["maps"].as_str().unwrap()).unwrap());
        params.output_width = 640;
        params.output_height = 360;
        for frame in 0..params.frame_count {
            for enabled in [false, true] {
                params.apply_optical_translation = enabled;
                let transform = FrameTransform::at_timestamp(&params, frame as f64 * 1000.0 / params.scaled_fps, frame);
                let mut kernel = transform.kernel_params;
                kernel.width = params.width as i32; kernel.height = params.height as i32;
                kernel.output_width = 640; kernel.output_height = 360;
                let mesh: Vec<f64> = transform.mesh_data.iter().map(|v| *v as f64).collect();
                let mut bytes = Vec::with_capacity(BYTES);
                for y in 0..37 { for x in 0..65 {
                    let point = (x as f32 * 639.0 / 64.0, y as f32 * 359.0 / 36.0);
                    let mut row = transform.matrices.len() / 2;
                    let mut value = None;
                    for _ in 0..3 {
                        value = Stabilization::rotate_and_distort(point, row, &kernel, &transform.matrices,
                            &params.distortion_model, None, kernel.r_limit * kernel.r_limit, &mesh);
                        row = value.map(|v| v.1.round().clamp(0.0, (transform.matrices.len()-1) as f32) as usize).unwrap_or(row);
                    }
                    let value = value.unwrap_or((-60000.0, -60000.0));
                    bytes.extend_from_slice(&(value.0 / (params.width as f32 / 640.0)).to_le_bytes());
                    bytes.extend_from_slice(&(value.1 / (params.height as f32 / 360.0)).to_le_bytes());
                }}
                if !enabled {
                    assert!(bytes == baseline[frame*3*BYTES..(frame*3+1)*BYTES], "off maps differ at frame {frame}");
                }
                output.write_all(&bytes).unwrap();
            }
            output.write_all(&baseline[(frame*3+1)*BYTES..(frame*3+2)*BYTES]).unwrap();
        }
        output.flush().unwrap();
        metadata["off_maps_bit_exact"] = true.into();
    }
    std::fs::write(config["metadata"].as_str().unwrap(), serde_json::to_string_pretty(&metadata).unwrap()).unwrap();
}

#[test]
#[ignore = "requires an explicitly supplied project and external reference curves"]
fn translation_video_projection_acceptance() {
    let config_path = std::env::var("GYROFLOW_TRANSLATION_ACCEPTANCE").expect("set GYROFLOW_TRANSLATION_ACCEPTANCE to a diagnostic JSON file");
    let config: serde_json::Value = serde_json::from_reader(File::open(config_path).unwrap()).unwrap();
    let manager = StabilizationManager::default();
    manager.import_gyroflow_file(&crate::filesystem::path_to_url(config["project"].as_str().unwrap()), true, |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();
    if let Some(seconds) = config["smoothness_s"].as_f64() { manager.set_translation_smoothness(seconds); }
    manager.recompute_blocking();
    let normal_params = ComputeParams::from_manager(&manager);
    let mut params = normal_params.clone();
    if let Some(values) = config["fixed_fovs"].as_array() {
        params.fovs = values.iter().map(|v| v.as_f64().unwrap()).collect();
        assert_eq!(params.fovs.len(), params.frame_count);
    }
    if config["metadata_only"].as_bool() == Some(true) {
        let metadata = serde_json::json!({"fovs": normal_params.fovs, "frames": params.frame_count, "fps": params.scaled_fps});
        std::fs::write(config["metadata"].as_str().unwrap(), serde_json::to_string_pretty(&metadata).unwrap()).unwrap();
        return;
    }
    let original = manager.gyro.read().clone();
    let result = original.optical_translation.as_ref().unwrap();
    assert!(result.is_active(), "the project must reopen with an applicable translation result");
    let mut variants = Vec::new();
    let mut names = Vec::new();
    let mut off = params.clone();
    off.apply_optical_translation = false;
    variants.push(off);
    names.push("off".to_owned());
    for input in config["references"].as_array().unwrap() {
        let name = input["name"].as_str().unwrap();
        let csv = std::fs::read_to_string(input["path"].as_str().unwrap()).unwrap();
        let mut points = Vec::new();
        for line in csv.lines().filter(|line| !line.starts_with("timestamp")) {
            let row: Vec<f64> = line.split(',').map(|v| v.parse().unwrap()).collect();
            points.push((row[0].round() as i64, [row[1], row[2], row[3]]));
        }
        if name == "raw_depth_solver" {
            let samples = &result.samples;
            assert_eq!(points.len(), samples.len());
            let mut start = 0;
            while start < samples.len() {
                let mut end = start+1;
                while end < samples.len() && samples[end].segment==samples[start].segment {end+=1;}
                let times: Vec<_> = samples[start..end].iter().map(|s| time_difference_s(s.timestamp_us,samples[start].timestamp_us)).collect();
                let request: Vec<_> = points[start..end].iter().map(|p| nalgebra::Vector3::from(p.1)).collect();
                let geometry: Vec<_> = samples[start..end].iter().map(|s|s.geometry().unwrap()).collect();
                let fixed: Vec<_> = request.iter().enumerate().map(|(i,p)| i==0||i+1==request.len()||p.norm()==0.0).collect();
                let mut ages:Vec<_>=samples[start..end].iter().map(|s|s.track_age_s as f64).collect();
                let sigma=result.settings.smoothness_s.min(2.0*median(&mut ages)).max(0.001);
                let limits=TranslationConfig::resolved();
                let (curve,_)=smoothing::constrain(&times,&request,&geometry,&fixed,sigma,limits.max_shift*0.5,limits.max_axial_shift()*0.5,result.settings.along_axis,&||false).unwrap();
                for (p,v) in points[start..end].iter_mut().zip(curve) {p.1=[v.x,v.y,v.z];}
                start=end;
            }
        }
        let mut replacement = OpticalTranslation::with_curve(points);
        replacement.settings = result.settings;
        let mut gyro = original.clone();
        gyro.optical_translation = Some(replacement);
        let mut p = params.clone();
        p.gyro = Arc::new(parking_lot::RwLock::new(gyro));
        variants.push(p);
        names.push(name.to_owned());
    }
    variants.push(params.clone());
    names.push("optimized".to_owned());
    if config["fixed_fovs"].is_array() {
        variants.push(normal_params.clone());
        names.push("normal_crop".to_owned());
    }
    let mut map = BufWriter::new(File::create(config["maps"].as_str().unwrap()).unwrap());
    const GW:usize=65;
    const GH:usize=37;
    for p in &mut variants {p.output_width=640;p.output_height=360;}
    for frame in 0..params.frame_count {
        for p in &variants {
            let transform=FrameTransform::at_timestamp(p,frame as f64*1000.0/p.scaled_fps,frame);
            let mut kernel=transform.kernel_params;
            kernel.width=p.width as i32;kernel.height=p.height as i32;
            kernel.output_width=640;kernel.output_height=360;
            let mesh:Vec<f64>=transform.mesh_data.iter().map(|v|*v as f64).collect();
            for y in 0..GH {for x in 0..GW {
                let point=(x as f32*639.0/(GW-1) as f32,y as f32*359.0/(GH-1) as f32);
                let mut row=transform.matrices.len()/2;
                let mut value=None;
                for _ in 0..3 {
                    value=Stabilization::rotate_and_distort(point,row,&kernel,&transform.matrices,&p.distortion_model,None,kernel.r_limit*kernel.r_limit,&mesh);
                    row=value.map(|v|v.1.round().clamp(0.0,(transform.matrices.len()-1) as f32) as usize).unwrap_or(row);
                }
                let value=value.unwrap_or((-60000.0,-60000.0));
                map.write_all(&(value.0/(p.width as f32/640.0)).to_le_bytes()).unwrap();
                map.write_all(&(value.1/(p.height as f32/360.0)).to_le_bytes()).unwrap();
            }}
        }
    }
    map.flush().unwrap();
    let (mut raw_max,mut applied_max,mut limited_rows,mut total_rows)=(0.0f64,0.0f64,0usize,0usize);
    let limit_config=TranslationConfig::resolved();
    for frame in 0..params.frame_count {
        let frame_time=frame as f64*1000.0/params.scaled_fps;
        let (mut k,..)=FrameTransform::get_lens_data_at_timestamp(&params,frame_time,false);
        FrameTransform::dequantize_camera_matrix(&params,frame,&mut k);
        let gyro=params.gyro.read();let md=gyro.file_metadata.read();
        let readout=FrameTransform::get_frame_readout_time(&params,true,frame_time,&md);
        let centre=frame_time+md.per_frame_time_offsets.get(frame).unwrap_or(&0.0);
        let rows=if params.frame_readout_direction.is_horizontal(){params.width}else{params.height};
        let short=params.width.min(params.height) as f64;
        let t=gyro.optical_translation.as_ref().unwrap();
        for row in 0..rows {
            let time=centre-readout/2.0+readout*row as f64/rows as f64;
            let source=gyro.org_quat_at_timestamp(time);
            let raw=(source.inverse()*t.shift_at(time)).xy().norm()*k[(0,0)]/short*100.0;
            let applied=t.camera_shift_at(&source,time,short,k[(0,0)],false,&limit_config).xy().norm()*k[(0,0)]/short*100.0;
            raw_max=raw_max.max(raw);applied_max=applied_max.max(applied);
            limited_rows+=usize::from(raw>limit_config.max_shift*50.0+1e-6);total_rows+=1;
        }
    }
    assert!(applied_max<=limit_config.max_shift*100.0+1e-8);
    let metadata=serde_json::json!({"frames":params.frame_count,"fps":params.scaled_fps,"variants":names,"grid":[GW,GH],"size":[640,360],"info":manager.translation_stabilization_info(),"crop":"fixed crop except the explicitly named normal_crop variant", "normal_fovs":normal_params.fovs, "fixed_fovs":params.fovs,
        "actual_row_budget":{"raw_peak_pct":raw_max,"applied_peak_pct":applied_max,"nonlinear_rows":limited_rows,"total_rows":total_rows}});
    std::fs::write(config["metadata"].as_str().unwrap(),serde_json::to_string_pretty(&metadata).unwrap()).unwrap();
}

#[test]
#[ignore = "requires a project analyzed with the automatic parameters"]
fn translation_auto_parameters_report() {
    let project = std::env::var("GYROFLOW_TRANSLATION_AUTO_PROJECT").expect("set GYROFLOW_TRANSLATION_AUTO_PROJECT to an analyzed project");
    let output = std::env::var("GYROFLOW_TRANSLATION_AUTO_REPORT").expect("set GYROFLOW_TRANSLATION_AUTO_REPORT to the JSON file to write");
    let manager = StabilizationManager::default();
    manager.import_gyroflow_file(&crate::filesystem::path_to_url(&project), true, |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();
    manager.recompute_blocking();
    let result = manager.gyro.read().optical_translation.clone().expect("the project carries a translation result");
    assert!(result.settings.auto && result.is_active(), "{:?}", result.settings);
    let mut samples = result.samples.clone();
    samples.sort_by_key(|s| s.timestamp_us);
    let mut rows = Vec::new();
    let mut start = 0;
    while start < samples.len() {
        let mut end = start + 1;
        while end < samples.len() && samples[end].segment == samples[start].segment { end += 1; }
        let segment = &samples[start..end];
        for (s, (far, auto)) in segment.iter().zip(filtered_far_beta(segment).into_iter().zip(filtered_auto_beta(segment))) {
            rows.push(serde_json::json!([s.timestamp_us, s.segment, s.far_beta, s.auto_beta, far, auto, s.weight]));
        }
        start = end;
    }
    // Share of time over the planning budget for a range of smoothness values, holding either layer
    let (path, _) = result.output_path(&manager.gyro.read());
    let config = TranslationConfig::resolved();
    let mut sweep = serde_json::Map::new();
    for (name, depths) in [("far", filtered_far_beta(&samples)), ("auto", filtered_auto_beta(&samples))] {
        let times: Vec<_> = samples.iter().map(|s| time_difference_s(s.timestamp_us, samples[0].timestamp_us)).collect();
        let geometry: Vec<_> = samples.iter().map(|s| s.geometry().unwrap()).collect();
        let mut position = nalgebra::Vector3::zeros();
        let positions: Vec<_> = samples.iter().enumerate().map(|(i, s)| {
            if i > 0 {
                let rotation = retained_rotation(&path[&samples[i - 1].timestamp_us], &path[&s.timestamp_us]);
                let gain = depths[i] * s.weight.clamp(0.0, 1.0) as f64;
                position += nalgebra::Vector3::new(rotation.x + gain * s.layer_motion[0] as f64, rotation.y + gain * s.layer_motion[1] as f64, gain * s.layer_scale_rate as f64);
            }
            position
        }).collect();
        let rows: Vec<_> = [0.1, 0.2, 0.3, 0.4, 0.5, 0.63, 0.8, 1.0, 1.26, 1.6, 2.0, 4.0].iter().map(|sigma| {
            let request = high_pass(&times, &positions, *sigma, &|| false).unwrap();
            serde_json::json!([sigma, over_budget_time(&times, &request, &geometry, config.max_shift * 0.5, config.max_axial_shift() * 0.5, true),
                over_budget_time(&times, &request, &geometry, config.max_shift * 0.5, f64::INFINITY, true)])
        }).collect();
        sweep.insert(name.into(), rows.into());
    }
    // Requested (before the budget) against planned compensation at the frame centre, in % of the short side
    let mut budget_rows = Vec::new();
    {
        let depths = filtered_auto_beta(&samples);
        let times: Vec<_> = samples.iter().map(|s| time_difference_s(s.timestamp_us, samples[0].timestamp_us)).collect();
        let mut position = nalgebra::Vector3::zeros();
        let positions: Vec<_> = samples.iter().enumerate().map(|(i, s)| {
            if i > 0 {
                let rotation = retained_rotation(&path[&samples[i - 1].timestamp_us], &path[&s.timestamp_us]);
                let gain = depths[i] * s.weight.clamp(0.0, 1.0) as f64;
                position += nalgebra::Vector3::new(rotation.x + gain * s.layer_motion[0] as f64, rotation.y + gain * s.layer_motion[1] as f64, gain * s.layer_scale_rate as f64);
            }
            position
        }).collect();
        let request = high_pass(&times, &positions, result.effective_smoothness_s(), &|| false).unwrap();
        for (s, r) in samples.iter().zip(&request) {
            let planned = result.shift_at(s.timestamp_us as f64 / 1000.0);
            let focal = s.focal_length_over_short_side as f64 * 100.0;
            budget_rows.push(serde_json::json!([s.timestamp_us, r.x * focal, r.y * focal, r.z * 100.0, planned.x * focal, planned.y * focal, planned.z * 100.0]));
        }
    }
    let report = serde_json::json!({"effective_smoothness_s": result.effective_smoothness_s(),
        "columns": ["timestamp_us", "segment", "far_beta", "auto_beta", "far_filtered", "auto_filtered", "weight"],
        "over_budget_columns": ["sigma_s", "lateral_or_axial", "lateral_only"], "over_budget": sweep,
        "budget_columns": ["timestamp_us", "request_x_pct", "request_y_pct", "request_z_pct", "planned_x_pct", "planned_y_pct", "planned_z_pct"],
        "budget": budget_rows,
        "info": manager.translation_stabilization_info(), "samples": rows});
    std::fs::write(&output, serde_json::to_string_pretty(&report).unwrap()).unwrap();
    println!("effective smoothness {} s, report {}", result.effective_smoothness_s(), output);
}

#[test]
#[ignore = "writes the translation inputs of analyzed projects for the offline smoothing study"]
fn translation_smoothing_study_dump() {
    // GYROFLOW_TRANSLATION_STUDY_PROJECTS: analyzed projects separated by ';'; each writes <project>.samples.json
    let projects = std::env::var("GYROFLOW_TRANSLATION_STUDY_PROJECTS").expect("set GYROFLOW_TRANSLATION_STUDY_PROJECTS");
    for project in projects.split(';').filter(|p| !p.is_empty()) {
        let manager = StabilizationManager::default();
        manager.import_gyroflow_file(&crate::filesystem::path_to_url(project), true, |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();
        manager.recompute_blocking();
        let result = manager.gyro.read().optical_translation.clone().expect("the project carries a translation result");
        let (path, _) = result.output_path(&manager.gyro.read());
        let mut samples = result.samples.clone();
        samples.sort_by_key(|s| s.timestamp_us);
        let mut rows = Vec::new();
        let mut start = 0;
        while start < samples.len() {
            let mut end = start + 1;
            while end < samples.len() && samples[end].segment == samples[start].segment { end += 1; }
            let segment = &samples[start..end];
            for (i, (s, (far, auto))) in segment.iter().zip(filtered_far_beta(segment).into_iter().zip(filtered_auto_beta(segment))).enumerate() {
                let rotation = if i > 0 { retained_rotation(&path[&segment[i - 1].timestamp_us], &path[&s.timestamp_us]) } else { nalgebra::Vector2::zeros() };
                let shift = result.shift_at(s.timestamp_us as f64 / 1000.0);
                let q = path[&s.timestamp_us];
                rows.push(serde_json::json!([s.timestamp_us, s.segment, rotation.x, rotation.y, s.layer_motion[0], s.layer_motion[1],
                    s.layer_scale_rate, s.far_beta, s.auto_beta, far, auto, s.weight, s.track_age_s, s.focal_length_over_short_side,
                    shift.x, shift.y, shift.z, q.w, q.i, q.j, q.k]));
            }
            start = end;
        }
        // Per-frame zoom with the translation applied and without it; output px per normalized unit = focal / (fov * fov_scale)
        let (fovs, fov_scale) = { let p = manager.params.read(); (p.fovs.clone(), p.fov) };
        manager.set_translation_stabilization_enabled(false);
        manager.recompute_blocking();
        let fovs_off = manager.params.read().fovs.clone();
        let report = serde_json::json!({"project": project, "settings": result.settings, "effective_smoothness_s": result.effective_smoothness_s(),
            "fovs": fovs, "fovs_off": fovs_off, "fov_scale": fov_scale,
            "config": {"track_age_k": TranslationConfig::resolved().track_age_k, "max_shift": TranslationConfig::resolved().max_shift,
                "max_axial_shift": TranslationConfig::resolved().max_axial_shift()},
            "columns": ["timestamp_us", "segment", "rotation_x", "rotation_y", "layer_x", "layer_y", "layer_scale_rate", "far_beta",
                "auto_beta", "far_filtered", "auto_filtered", "weight", "track_age_s", "focal_over_short", "shift_x", "shift_y", "shift_z", "output_w", "output_x", "output_y", "output_z"],
            "samples": rows});
        let output = format!("{project}.samples.json");
        std::fs::write(&output, serde_json::to_string(&report).unwrap()).unwrap();
        println!("{output}: {} samples, smoothness {} s", samples.len(), result.effective_smoothness_s());
    }
}

#[test]
#[ignore = "writes output-to-source grid maps of analyzed projects for the offline reference-layer study"]
fn translation_projection_maps_dump() {
    // GYROFLOW_TRANSLATION_STUDY_PROJECTS: analyzed projects separated by ';'. For each, <project>.maps-off.bin and
    // <project>.maps-prod.bin: per frame a 37 x 65 grid over a 960x540 output, the source point in 960x540 tracking
    // coordinates (f32 x, y; -1e4 where unmapped), without and with the translation applied.
    let projects = std::env::var("GYROFLOW_TRANSLATION_STUDY_PROJECTS").expect("set GYROFLOW_TRANSLATION_STUDY_PROJECTS");
    for project in projects.split(';').filter(|p| !p.is_empty()) {
        let manager = StabilizationManager::default();
        manager.import_gyroflow_file(&crate::filesystem::path_to_url(project), true, |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();
        manager.recompute_blocking();
        let prod = ComputeParams::from_manager(&manager);
        manager.set_translation_stabilization_enabled(false);
        manager.recompute_blocking();
        let off = ComputeParams::from_manager(&manager);
        for (name, mut params) in [("off", off), ("prod", prod)] {
            params.output_width = 960;
            params.output_height = 540;
            let mut output = BufWriter::new(File::create(format!("{project}.maps-{name}.bin")).unwrap());
            for frame in 0..params.frame_count {
                let transform = FrameTransform::at_timestamp(&params, frame as f64 * 1000.0 / params.scaled_fps, frame);
                let mut kernel = transform.kernel_params;
                kernel.width = params.width as i32; kernel.height = params.height as i32;
                kernel.output_width = 960; kernel.output_height = 540;
                let mesh: Vec<f64> = transform.mesh_data.iter().map(|v| *v as f64).collect();
                for y in 0..37 { for x in 0..65 {
                    let point = (x as f32 * 959.0 / 64.0, y as f32 * 539.0 / 36.0);
                    let mut row = transform.matrices.len() / 2;
                    let mut value = None;
                    for _ in 0..3 {
                        value = Stabilization::rotate_and_distort(point, row, &kernel, &transform.matrices,
                            &params.distortion_model, params.digital_lens.as_ref(), kernel.r_limit * kernel.r_limit, &mesh);
                        let coordinate = value.map(|v| if params.frame_readout_direction.is_horizontal() { v.0 } else { v.1 });
                        row = coordinate.map(|c| c.round().clamp(0.0, (transform.matrices.len() - 1) as f32) as usize).unwrap_or(row);
                    }
                    let value = value.filter(|v| v.0.is_finite() && v.1.is_finite()).unwrap_or((-4e4, -4e4));
                    output.write_all(&(value.0 * 960.0 / params.width as f32).to_le_bytes()).unwrap();
                    output.write_all(&(value.1 * 540.0 / params.height as f32).to_le_bytes()).unwrap();
                }}
            }
            output.flush().unwrap();
        }
        println!("{project}: maps written, {} frames", ComputeParams::from_manager(&manager).frame_count);
    }
}

#[test]
#[ignore = "writes the joint (shared smoothing law) translation target of analyzed projects for the offline study"]
fn translation_joint_target_study() {
    // GYROFLOW_TRANSLATION_STUDY_PROJECTS: analyzed projects separated by ';'; each writes <project>.joint.json.
    // The reference layer's translation-induced image motion is folded into the camera orientation as an equivalent
    // rotation, the result is smoothed by the project's own rotation smoother, and the translation stage is asked for
    // the difference between today's output path P and that smoothed path.
    let projects = std::env::var("GYROFLOW_TRANSLATION_STUDY_PROJECTS").expect("set GYROFLOW_TRANSLATION_STUDY_PROJECTS");
    for project in projects.split(';').filter(|p| !p.is_empty()) {
        let manager = StabilizationManager::default();
        manager.import_gyroflow_file(&crate::filesystem::path_to_url(project), true, |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();
        manager.recompute_blocking();
        let result = manager.gyro.read().optical_translation.clone().expect("the project carries a translation result");
        let mut samples = result.samples.clone();
        samples.sort_by_key(|s| s.timestamp_us);
        let gyro = manager.gyro.read().clone();
        let (path, _) = result.output_path(&gyro);
        let n = samples.len();
        let auto = result.settings.auto && samples.iter().any(|s| s.auto_beta > 0.0 && s.weight > 0.0);
        let depths = if auto { filtered_auto_beta(&samples) } else { filtered_far_beta(&samples) };
        let reference = if auto { 1.0 } else { result.settings.reference };
        let mut tref = vec![nalgebra::Vector2::zeros(); n];
        let mut p = vec![nalgebra::Vector2::zeros(); n];
        for i in 1..n {
            let s = &samples[i];
            let gain = reference * depths[i] * s.weight.clamp(0.0, 1.0) as f64;
            let step = nalgebra::Vector2::new(gain * s.layer_motion[0] as f64, gain * s.layer_motion[1] as f64);
            tref[i] = tref[i - 1] + step;
            p[i] = p[i - 1] + retained_rotation(&path[&samples[i - 1].timestamp_us], &path[&s.timestamp_us]) + step;
        }
        let times_ms: Vec<f64> = samples.iter().map(|s| s.timestamp_us as f64 / 1000.0).collect();
        let tref_at = |ms: f64| -> nalgebra::Vector2<f64> {
            let i = times_ms.partition_point(|t| *t < ms);
            if i == 0 { return tref[0]; }
            if i >= n { return tref[n - 1]; }
            let f = (ms - times_ms[i - 1]) / (times_ms[i] - times_ms[i - 1]);
            tref[i - 1] * (1.0 - f) + tref[i] * f
        };
        let mut params = ComputeParams::from_manager(&manager);
        params.calculate_camera_fovs();
        let smoothing = manager.smoothing.read();
        let mut best = None;
        for sign in [1.0, -1.0] {
            // Built step by step so that every added rotation stays small: each gyro step takes the camera's own
            // relative rotation and adds the rotation that moves the centre ray by that step's tref increment.
            let mut virt = gyro.clone();
            let mut previous: Option<(crate::gyro_source::Quat64, crate::gyro_source::Quat64, nalgebra::Vector2<f64>)> = None;
            virt.quaternions = gyro.quaternions.iter().map(|(ts, q)| {
                let gyro_ms = *ts as f64 / 1000.0;
                let t = tref_at(gyro_ms + gyro.offset_at_gyro_timestamp(gyro_ms)) * sign;
                let v = match previous {
                    None => *q,
                    Some((q_prev, v_prev, t_prev)) => {
                        let d = t - t_prev;
                        let ray = nalgebra::Vector3::new(d.x, -d.y, -1.0);
                        let r = nalgebra::UnitQuaternion::rotation_between(&nalgebra::Vector3::new(0.0, 0.0, -1.0), &ray).unwrap_or_else(nalgebra::UnitQuaternion::identity);
                        v_prev * (q_prev.inverse() * q) * r.inverse()
                    }
                };
                previous = Some((*q, v, t));
                (*ts, v)
            }).collect();
            // The unsmoothed virtual camera must see the reference layer move by the camera rotation plus tref
            let mut err = 0.0f64;
            let (mut cam, mut vir) = (nalgebra::Vector2::zeros(), nalgebra::Vector2::zeros());
            for i in 1..n {
                cam += retained_rotation(&gyro.org_quat_at_timestamp(times_ms[i - 1]), &gyro.org_quat_at_timestamp(times_ms[i]));
                vir += retained_rotation(&virt.org_quat_at_timestamp(times_ms[i - 1]), &virt.org_quat_at_timestamp(times_ms[i]));
                err = err.max((vir - cam - tref[i]).norm() * samples[i].focal_length_over_short_side as f64 * 100.0);
            }
            if best.as_ref().map_or(true, |(e, _, _)| err < *e) { best = Some((err, sign, virt)); }
        }
        let (composition_error_pct, sign, mut virt) = best.unwrap();
        let (corrections, _) = virt.recompute_smoothness(smoothing.current().as_ref(), smoothing.horizon_lock.clone(), &params);
        virt.smoothed_quaternions = corrections;
        let mut target = vec![nalgebra::Vector2::zeros(); n];
        for i in 1..n {
            target[i] = target[i - 1] + retained_rotation(&output_orientation(&virt, times_ms[i - 1]), &output_orientation(&virt, times_ms[i]));
        }
        let times: Vec<f64> = times_ms.iter().map(|t| (t - times_ms[0]) / 1000.0).collect();
        let last = times[n - 1];
        let request: Vec<_> = (0..n).map(|i| {
            let ramp = 1.0_f64.min(times[i] / 0.25).min((last - times[i]) / 0.25).max(0.0);
            let d = (p[i] - target[i]) * ramp;
            nalgebra::Vector3::new(d.x, d.y, 0.0)
        }).collect();
        let geometry: Vec<_> = samples.iter().map(|s| s.geometry().unwrap()).collect();
        let fixed: Vec<_> = (0..n).map(|i| i == 0 || i == n - 1).collect();
        let config = TranslationConfig::resolved();
        let sigma = result.effective_smoothness_s();
        let planned = super::smoothing::constrain(&times, &request, &geometry, &fixed, sigma, config.max_shift * 0.5, config.max_axial_shift() * 0.5, false, &|| false);
        let (shift, ok) = match planned { Ok((s, _)) => (s, true), Err(_) => (request.clone(), false) };
        let rows: Vec<_> = (0..n).map(|i| {
            let prod = result.shift_at(times_ms[i]);
            serde_json::json!([samples[i].timestamp_us, p[i].x, p[i].y, target[i].x, target[i].y, request[i].x, request[i].y,
                shift[i].x, shift[i].y, prod.x, prod.y, samples[i].focal_length_over_short_side])
        }).collect();
        let report = serde_json::json!({"project": project, "sign": sign, "composition_error_pct": composition_error_pct,
            "sigma": sigma, "planner_ok": ok, "columns": ["timestamp_us", "p_x", "p_y", "target_x", "target_y", "request_x", "request_y",
            "joint_shift_x", "joint_shift_y", "prod_shift_x", "prod_shift_y", "focal_over_short"], "samples": rows});
        std::fs::write(format!("{project}.joint.json"), serde_json::to_string(&report).unwrap()).unwrap();
        println!("{project}: composition error {composition_error_pct:.4}% of the short side (sign {sign}), planner ok {ok}");
    }
}
