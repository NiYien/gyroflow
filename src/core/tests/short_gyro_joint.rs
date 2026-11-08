// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

use gyroflow_core::{ExternalIoPolicy, StabilizationManager};
use gyroflow_core::synchronization::{deep_match as dm, AutosyncProcess, SyncParams};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

#[test]
#[ignore = "requires GYROFLOW_SHORT_GYRO_CASES with real frames, projects and gyro recordings"]
fn short_gyro_joint_real_cases() {
    struct AuditLog;
    impl log::Log for AuditLog {
        fn enabled(&self, _: &log::Metadata) -> bool { true }
        fn log(&self, record: &log::Record) {
            if record.target() == "sync" { eprintln!("{}", record.args()); }
        }
        fn flush(&self) {}
    }
    static LOGGER: AuditLog = AuditLog;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
    let manifest = std::fs::read(std::env::var("GYROFLOW_SHORT_GYRO_CASES").expect("fixture manifest")).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let tolerance = manifest["tolerance_ms"].as_f64().unwrap();
    let mut report = Vec::new();
    for case in manifest["cases"].as_array().unwrap() {
        eprintln!("CASE {} kind={}", case["name"], case["kind"]);
        let project = std::fs::read(case["project"].as_str().unwrap()).unwrap();
        let project_json: serde_json::Value = serde_json::from_slice(&project).unwrap();
        let frames = std::fs::read(case["frames"].as_str().unwrap()).unwrap();
        let width = case["width"].as_u64().unwrap() as u32;
        let height = case["height"].as_u64().unwrap() as u32;
        let fps = case["fps"].as_f64().unwrap();
        let frame_count = case["frame_count"].as_u64().unwrap() as usize;
        let frame_bytes = width as usize * height as usize;
        assert_eq!(frames.len(), frame_bytes * frame_count);
        let stab = StabilizationManager::default();
        let cancel = Arc::new(AtomicBool::new(false));
        stab.import_gyroflow_data_with_policy(&project, true, None, |_| {}, cancel.clone(), &mut false,
            false, ExternalIoPolicy::Deny).unwrap();
        stab.init_from_video_data(case["duration_ms"].as_f64().unwrap(), fps, frame_count,
            (project_json["video_info"]["width"].as_u64().unwrap() as usize,
             project_json["video_info"]["height"].as_u64().unwrap() as usize));
        stab.gyro.write().clear_offsets();
        let path = case["gyro"].as_str().unwrap();
        let mut file = std::fs::File::open(path).unwrap();
        let size = file.metadata().unwrap().len() as usize;
        let options = gyroflow_core::gyro_source::FileLoadOptions { bypass_builtin_gyro_arbitration: true, ..Default::default() };
        stab.load_gyro_data(&mut file, size, &gyroflow_core::filesystem::path_to_url(path), false, &options,
            |_| {}, cancel.clone()).unwrap();
        let duration = {
            let gyro = stab.gyro.read();
            let md = gyro.file_metadata.read();
            let raw = gyro.raw_imu(&md);
            raw.last().unwrap().timestamp_ms - raw.first().unwrap().timestamp_ms
        };
        let video_ms = stab.params.read().get_scaled_duration_ms();
        let sp = SyncParams { initial_offset: -(duration - video_ms) / 2.0, search_size: duration,
            time_per_syncpoint: 2500.0, every_nth_frame: 1, of_method: 2, offset_method: 0, pose_method: 0,
            ..Default::default() };
        dm::arm(2); dm::arm_forward();
        let mut process = AutosyncProcess::from_manager(&stab, &[1.0 / 3.0, 2.0 / 3.0], sp,
            "synchronize".into(), cancel).unwrap();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_cb = finished.clone();
        process.on_finished(move |_| { finished_cb.store(true, Ordering::SeqCst); });
        let started = std::time::Instant::now();
        let mut fed = 0;
        for (i, pixels) in frames.chunks_exact(frame_bytes).enumerate() {
            let pixels = if case["static_frame"].as_bool() == Some(true) { &frames[..frame_bytes] } else { pixels };
            if process.feed_frame((i as f64 * 1_000_000.0 / fps).round() as i64, i, width, height, width as usize, pixels) { fed += 1; }
        }
        process.finished_feeding_frames();
        let verdict = dm::take_short_overlap();
        dm::take();
        assert!(fed > 0 && finished.load(Ordering::SeqCst), "case did not run: {}", case["name"]);
        let offset = match verdict { Some(dm::DeepMatchVerdict::Accepted { offset_ms }) => Some(offset_ms), _ => None };
        let truth = case["truth_ms"].as_f64();
        let error = offset.zip(truth).map(|(actual, truth)| actual - truth);
        let must_accept = case["must_accept"].as_bool().unwrap();
        let correct = error.is_some_and(|x| x.abs() <= tolerance);
        let ok = if must_accept { correct } else if truth.is_some() { offset.is_none() || correct } else { offset.is_none() };
        let row = serde_json::json!({ "name": case["name"], "kind": case["kind"], "must_accept": must_accept,
            "truth_ms": truth, "offset_ms": offset, "error_ms": error, "miss": truth.is_some() && offset.is_none(),
            "verdict": format!("{verdict:?}"), "ok": ok, "elapsed_ms": started.elapsed().as_millis(), "fed_frames": fed });
        eprintln!("REAL {row}");
        report.push(row);
    }
    std::fs::write(std::env::var("GYROFLOW_SHORT_GYRO_REPORT").expect("report path"),
        serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    assert!(report.iter().all(|row| row["ok"].as_bool() == Some(true)), "{report:?}");
}
