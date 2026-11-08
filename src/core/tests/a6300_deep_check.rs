// SPDX-License-Identifier: GPL-3.0-or-later
use gyroflow_core::synchronization::{
    AutosyncProcess, AutosyncResult, SyncParams, deep_match as dm,
};
use gyroflow_core::{ExternalIoPolicy, StabilizationManager};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, Mutex, atomic::AtomicBool};

/// Play just the requested windows from full-length 1920x1080 gray8 frames at 25 fps.
fn measure(
    stab: &StabilizationManager,
    sp: SyncParams,
    centers: &[f64],
    frames: &Path,
) -> Vec<(f64, f64, f64, f64)> {
    let params = stab.params.read();
    let fractions: Vec<_> = centers.iter().map(|ms| ms / params.duration_ms).collect();
    let count = params.frame_count;
    drop(params);
    let mut raw = File::open(frames).unwrap();
    let mut pixels = vec![0; 1920 * 1080];
    assert_eq!(raw.metadata().unwrap().len(), (count * pixels.len()) as u64);
    let rows = Arc::new(Mutex::new(Vec::new()));
    let mut process = AutosyncProcess::from_manager(
        stab,
        &fractions,
        sp,
        "synchronize".into(),
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    let results = rows.clone();
    process.on_finished(move |result| {
        if let AutosyncResult::Offsets(offsets) = result {
            *results.lock().unwrap() = offsets;
        }
    });
    for i in 0..count {
        let ms = i as f64 * 40.0;
        if centers
            .iter()
            .any(|center| (center - ms).abs() <= dm::per_window_ms() / 2.0)
        {
            raw.seek(SeekFrom::Start((i * pixels.len()) as u64))
                .unwrap();
            raw.read_exact(&mut pixels).unwrap();
            process.feed_frame(i as i64 * 40000, i, 1920, 1080, 1920, &pixels);
        }
    }
    process.finished_feeding_frames();
    std::mem::take(&mut *rows.lock().unwrap())
}

#[test]
#[ignore = "requires A6300_SOURCE (projects and BINs), A6300_FRAMES (full-length gray8 frames)"]
fn a6300_pair() {
    struct RateLog;
    impl log::Log for RateLog {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.target() == "sync"
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                let message = record.args().to_string();
                if message.contains("cached angular rates") {
                    eprintln!("{message}");
                }
            }
        }
        fn flush(&self) {}
    }
    log::set_logger(&RateLog).unwrap();
    log::set_max_level(log::LevelFilter::Debug);
    let source = std::path::PathBuf::from(std::env::var_os("A6300_SOURCE").unwrap());
    let frames = std::path::PathBuf::from(std::env::var_os("A6300_FRAMES").unwrap());
    for (clip, bin, frame_count, is_match) in [
        (2, "01-00-27", 708, true),
        (2, "23-28-08", 708, false),
        (1, "23-28-08", 540, true),
        (1, "01-00-27", 540, false),
    ] {
        let stab = StabilizationManager::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let project = std::fs::read(source.join(format!("{clip}.gyroflow"))).unwrap();
        stab.import_gyroflow_data_with_policy(
            &project,
            true,
            None,
            |_| {},
            cancel.clone(),
            &mut false,
            false,
            ExternalIoPolicy::Deny,
        )
        .unwrap();
        let duration = frame_count as f64 * 40.0;
        stab.init_from_video_data(duration, 25.0, frame_count, (3840, 2160));
        let embedded = stab.gyro.read().file_metadata.read().raw_imu.clone();
        stab.gyro.write().clear_offsets();
        let path = source.join(format!("2026-11-08_{bin}_mix.bin"));
        let mut f = File::open(&path).unwrap();
        let size = f.metadata().unwrap().len() as usize;
        let opts = gyroflow_core::gyro_source::FileLoadOptions {
            bypass_builtin_gyro_arbitration: true,
            ..Default::default()
        };
        stab.load_gyro_data(
            &mut f,
            size,
            &gyroflow_core::filesystem::path_to_url(path.to_str().unwrap()),
            false,
            &opts,
            |_| {},
            cancel,
        )
        .unwrap();
        let (first, last) = {
            let g = stab.gyro.read();
            let md = g.file_metadata.read();
            let raw = &md.raw_imu;
            let origin = raw.windows(32).find(|w| {
                w.iter()
                    .zip(&embedded[32..64])
                    .all(|(a, b)| a.gyro == b.gyro && a.accl == b.accl)
            });
            assert_eq!(origin.is_some(), is_match);
            if clip == 2 && is_match {
                assert_eq!(
                    origin.unwrap()[0].timestamp_ms - embedded[32].timestamp_ms,
                    3226.0
                );
            }
            (
                raw.first().unwrap().timestamp_ms,
                raw.last().unwrap().timestamp_ms,
            )
        };
        let input = frames.join(format!("{clip}.gray"));
        let (initial_offset, search_size) =
            dm::search_domain(duration, first, last, dm::max_scan_ms());
        let sp = SyncParams {
            initial_offset,
            search_size,
            time_per_syncpoint: dm::per_window_ms(),
            every_nth_frame: 1,
            of_method: 2,
            offset_method: 0,
            pose_method: 0,
            ..Default::default()
        };
        let (n, k) = dm::plan_windows(
            duration,
            dm::candidates_count(),
            dm::scan_short(),
            dm::scan_long(),
            dm::long_min_ms(),
            dm::per_window_ms(),
        );
        dm::arm(k);
        dm::arm_forward();
        measure(
            &stab,
            sp.clone(),
            &dm::window_positions_ms(duration, n),
            &input,
        );
        let curves = dm::take_curves();
        let (scanned, gated) = dm::window_counts();
        let forward = dm::take_forward();
        dm::take();
        let primary = dm::decide_posterior(
            &curves,
            scanned,
            gated,
            duration,
            dm::post_conf_min(),
            dm::post_ci95_base_ms(),
            dm::drift_rate_ms_per_min(),
            dm::drift_floor_ms(),
        );
        if clip == 1 && is_match {
            assert!(matches!(primary, dm::DeepMatchVerdict::Accepted { .. }));
            eprintln!("clip={clip} gyro={bin} primary={primary:?} optical retry skipped");
            continue;
        }
        assert!(matches!(
            primary,
            dm::DeepMatchVerdict::Inconsistent { .. } | dm::DeepMatchVerdict::WeakValley { .. }
        ));
        if clip == 2 && is_match {
            let Some(dm::ForwardOutcome::AngularConfirmed { offset_ms }) = forward else {
                panic!("cached angular rates must confirm the correct BIN: {forward:?}");
            };
            // The saved project uses a slice whose time origin was verified above.
            assert!(
                (offset_ms - (-640.0908866629258 - 3226.0)).abs() <= 4.0,
                "{offset_ms}"
            );
            eprintln!(
                "clip={clip} gyro={bin} primary={primary:?} cached angular offset={offset_ms:.3} optical retry skipped"
            );
            continue;
        }
        assert!(matches!(
            forward,
            Some(dm::ForwardOutcome::Abstained {
                reason: "too_few_candidates"
            })
        ));
        let span = {
            let g = stab.gyro.read();
            (
                *g.quaternions.first_key_value().unwrap().0 as f64 / 1000.0,
                *g.quaternions.last_key_value().unwrap().0 as f64 / 1000.0,
            )
        };
        let search = dm::optical_retry_search(duration, span).unwrap();
        let optical = SyncParams {
            offset_method: 3,
            initial_offset: search.center_ms,
            initial_offset_inv: false,
            search_size: search.radius_ms,
            ..sp
        };
        dm::arm(0);
        let rows = measure(&stab, optical, &search.windows_ms, &input);
        dm::take();
        let confirmed = dm::optical_retry_offset(rows.iter().map(|r| (r.0, r.1, r.3)), duration);
        eprintln!(
            "clip={clip} gyro={bin} primary={primary:?} optical={rows:?} confirmed={confirmed:?}"
        );
        assert_eq!(confirmed, None);
    }
}
