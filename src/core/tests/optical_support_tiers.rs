// SPDX-License-Identifier: GPL-3.0-or-later

use gyroflow_core::{ExternalIoPolicy, StabilizationManager};
use gyroflow_core::synchronization::{AutosyncProcess, AutosyncResult, SyncParams};
use serde::Deserialize;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex, atomic::AtomicBool};

#[derive(Deserialize)]
struct Segment { path: String, first: usize }
#[derive(Deserialize)]
struct Case {
    name: String, project: String, frame_count: usize, centers_ms: Vec<f64>, initial_ms: f64,
    segments: Vec<Segment>, expected_match: bool, expected_probe: bool,
}
#[derive(Deserialize)]
struct Cases { cases: Vec<Case> }

fn feed(process: &AutosyncProcess, segments: &[Segment], ranges: &[(f64, f64)]) {
    let mut pixels = vec![0u8; 1920 * 1080];
    for segment in segments {
        let mut file = File::open(&segment.path).unwrap();
        let size = file.metadata().unwrap().len() as usize;
        assert_eq!(size % pixels.len(), 0);
        for i in 0..size / pixels.len() {
            let frame = segment.first + i;
            let ms = frame as f64 * 40.0;
            if !ranges.iter().any(|&(from, to)| (from..=to).contains(&ms)) { continue; }
            file.seek(SeekFrom::Start((i * pixels.len()) as u64)).unwrap();
            file.read_exact(&mut pixels).unwrap();
            process.feed_frame(frame as i64 * 40_000, frame, 1920, 1080, 1920, &pixels);
        }
    }
}

#[test]
#[ignore = "requires OPTICAL_SUPPORT_CASES with frozen native gray frames and projects"]
fn real_optical_support_tiers() {
    let cases: Cases = serde_json::from_slice(&std::fs::read(std::env::var("OPTICAL_SUPPORT_CASES").unwrap()).unwrap()).unwrap();
    let mut report = Vec::new();
    for case in cases.cases {
        let stab = StabilizationManager::default();
        stab.import_gyroflow_data_with_policy(&std::fs::read(&case.project).unwrap(), true, None, |_| {},
            Arc::new(AtomicBool::new(false)), &mut false, false, ExternalIoPolicy::Deny).unwrap();
        let duration = case.frame_count as f64 * 40.0;
        stab.init_from_video_data(duration, 25.0, case.frame_count, (3840, 2160));
        stab.gyro.write().clear_offsets();
        let points: Vec<_> = case.centers_ms.iter().map(|ts| ts / duration).collect();
        let sp = SyncParams { initial_offset: case.initial_ms, search_size: 5000.0, time_per_syncpoint: 2500.0,
            offset_method: 3, every_nth_frame: 1, of_method: 2, max_sync_points: points.len(), ..Default::default() };
        let mut process = AutosyncProcess::from_manager(&stab, &points, sp, "synchronize".into(), Arc::new(AtomicBool::new(false))).unwrap();
        let rows = Arc::new(Mutex::new(Vec::new()));
        let output = rows.clone();
        process.on_finished(move |result| {
            if let AutosyncResult::Offsets(values) = result { *output.lock().unwrap() = values; }
        });
        feed(&process, &case.segments, &process.get_ranges());
        process.finished_feeding_frames();
        let mut probes = 0;
        while let Some(ranges) = process.pending_probe_ranges() {
            probes += 1;
            assert!(probes <= 1, "{} repeated its verification", case.name);
            feed(&process, &case.segments, &ranges);
            process.finished_feeding_frames();
        }
        let rows = rows.lock().unwrap().clone();
        let found = rows.iter().any(|row| row.3 >= 0.4);
        println!("CASE {} probes={probes} rows={rows:?}", case.name);
        report.push(serde_json::json!({"name":case.name,"probes":probes,"rows":rows,"matched":found}));
        if let Ok(path) = std::env::var("OPTICAL_SUPPORT_REPORT") {
            std::fs::write(path, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
        }
        assert_eq!(found, case.expected_match, "{}", case.name);
        if case.expected_match { assert_eq!(probes > 0, case.expected_probe, "{}", case.name); }
    }
}
