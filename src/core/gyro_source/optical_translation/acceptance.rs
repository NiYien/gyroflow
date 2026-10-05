// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use crate::{StabilizationManager, stabilization::{ComputeParams, FrameTransform, Stabilization}};
use std::{fs::File, io::{BufWriter, Write}, sync::{Arc, atomic::AtomicBool}};

#[test]
#[ignore = "requires an explicitly supplied project and external reference curves"]
fn translation_video_projection_acceptance() {
    let config_path = std::env::var("GYROFLOW_TRANSLATION_ACCEPTANCE").expect("set GYROFLOW_TRANSLATION_ACCEPTANCE to a diagnostic JSON file");
    let config: serde_json::Value = serde_json::from_reader(File::open(config_path).unwrap()).unwrap();
    let manager = StabilizationManager::default();
    manager.import_gyroflow_file(&crate::filesystem::path_to_url(config["project"].as_str().unwrap()), true, |_| {}, Arc::new(AtomicBool::new(false)), false).unwrap();
    manager.recompute_blocking();
    let params = ComputeParams::from_manager(&manager);
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
                let (curve,_)=smoothing::constrain(&times,&request,&geometry,&fixed,sigma,0.02,result.settings.along_axis,&||false).unwrap();
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
    let metadata=serde_json::json!({"frames":params.frame_count,"fps":params.scaled_fps,"variants":names,"grid":[GW,GH],"size":[640,360],"info":manager.translation_stabilization_info(),"crop":"same optimized-project crop for every variant",
        "actual_row_budget":{"raw_peak_pct":raw_max,"applied_peak_pct":applied_max,"nonlinear_rows":limited_rows,"total_rows":total_rows}});
    std::fs::write(config["metadata"].as_str().unwrap(),serde_json::to_string_pretty(&metadata).unwrap()).unwrap();
}
