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
