// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use itertools::Either;
use parking_lot::RwLock;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::AcqRel, Ordering::Relaxed, Ordering::SeqCst};

use super::PoseEstimator;
use super::SyncParams;
use crate::StabilizationManager;
use crate::stabilization::ComputeParams;

pub struct AutosyncProcess {
    frame_count: usize,
    scaled_fps: f64,
    org_fps: f64,
    fps_scale: Option<f64>,
    mode: String, // synchronize, guess_imu_orientation, estimate_rolling_shutter
    ranges_us: Vec<(i64, i64)>,
    scaled_ranges_us: Vec<(i64, i64)>,
    /// sync-likelihood-nuisance §3.2: scaled-µs range of the probe-only
    /// window appended in `from_manager` (posterior single-window runs).
    /// Offsets whose mid falls inside are stripped before the finished cb.
    probe_range_us: Option<(i64, i64)>,
    /// Lazy probe candidate (decode-domain µs, unscaled). Set in
    /// `from_manager` when the lazy probe is armed; decoded only on
    /// escalation.
    probe_candidate_us: Option<(i64, i64)>,
    /// Same candidate in the scaled domain (feed gate / find_offsets / strip).
    probe_candidate_scaled_us: Option<(i64, i64)>,
    /// Phase-1 verdict requested escalation; cleared by `pending_probe_ranges`.
    probe_pending: AtomicBool,
    /// Probe frames are accepted and the probe range joins find_offsets.
    probe_active: AtomicBool,
    /// Lazy run wanted a probe but no disjoint position fit — an ambiguous
    /// single-window result here is dropped instead of escalated.
    lazy_probe_unavailable: bool,
    estimator: Arc<PoseEstimator>,
    total_read_frames: Arc<AtomicUsize>,
    total_detected_frames: Arc<AtomicUsize>,
    compute_params: Arc<RwLock<ComputeParams>>,
    cancel_flag: Arc<AtomicBool>,
    progress_cb: Option<Arc<Box<dyn Fn(f64, usize, usize) + Send + Sync + 'static>>>,
    finished_cb: Option<
        Arc<
            Box<
                dyn Fn(Either<Vec<(f64, f64, f64, f64)>, Option<(String, f64)>>)
                    + Send
                    + Sync
                    + 'static,
            >,
        >,
    >,

    pub sync_params: SyncParams,

    thread_pool: rayon::ThreadPool,
}

pub fn describe_autosync_init_failure(
    stab: &StabilizationManager,
    timestamps_fract: &[f64],
    sync_params: &SyncParams,
) -> String {
    let params = stab.params.read();
    let org_fps = params.fps;
    let scaled_fps = params.get_scaled_fps();
    let org_duration_ms = params.duration_ms;
    let fps_scale = params.fps_scale;
    let scaled_duration_ms = params.get_scaled_duration_ms();

    let mut time_per_syncpoint = sync_params.time_per_syncpoint;
    if let Some(scale) = fps_scale {
        time_per_syncpoint *= scale;
    }
    let every_nth_frame = sync_params.every_nth_frame.max(1);
    let effective_frame_count =
        ((timestamps_fract.len() as f64 * (time_per_syncpoint / 1000.0) * org_fps).ceil() as usize)
            .min(params.frame_count)
            / every_nth_frame;

    let mut reasons = Vec::new();
    if scaled_duration_ms < 10.0 {
        reasons.push(format!("scaled_duration_ms({scaled_duration_ms:.3}) < 10"));
    }
    if effective_frame_count < 2 {
        reasons.push(format!(
            "effective_frame_count({effective_frame_count}) < 2"
        ));
    }
    if time_per_syncpoint < 10.0 {
        reasons.push(format!(
            "time_per_syncpoint_ms({time_per_syncpoint:.3}) < 10"
        ));
    }
    if sync_params.search_size < 10.0 {
        reasons.push(format!(
            "search_size_ms({:.3}) < 10",
            sync_params.search_size
        ));
    }

    format!(
        "reasons=[{}], timestamps={}, org_duration_ms={:.3}, scaled_duration_ms={:.3}, params_frame_count={}, effective_frame_count={}, org_fps={:.6}, scaled_fps={:.6}, fps_scale={:?}, every_nth_frame={}, time_per_syncpoint_ms={:.3}, search_size_ms={:.3}, max_sync_points={}, auto_sync_points={}",
        if reasons.is_empty() {
            "none".to_owned()
        } else {
            reasons.join(", ")
        },
        timestamps_fract.len(),
        org_duration_ms,
        scaled_duration_ms,
        params.frame_count,
        effective_frame_count,
        org_fps,
        scaled_fps,
        fps_scale,
        every_nth_frame,
        time_per_syncpoint,
        sync_params.search_size,
        sync_params.max_sync_points,
        sync_params.auto_sync_points
    )
}

impl AutosyncProcess {
    pub fn from_manager(
        stab: &StabilizationManager,
        timestamps_fract: &[f64],
        sync_params: SyncParams,
        mode: String,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<Self, ()> {
        let params = stab.params.read();
        let org_fps = params.fps;
        let scaled_fps = params.get_scaled_fps();
        let org_duration_ms = params.duration_ms;
        let fps_scale = params.fps_scale;
        let duration_ms = params.get_scaled_duration_ms();

        let SyncParams {
            search_size,
            mut time_per_syncpoint,
            every_nth_frame,
            ..
        } = sync_params;

        if let Some(scale) = &fps_scale {
            time_per_syncpoint *= scale;
        }

        // sync-likelihood-nuisance §3.2: a single-window rs-sync run gets one
        // extra probe-only window so the posterior's cross-window product has
        // independent evidence (echo suppression, design D3). The probe
        // participates in the likelihood only — its offset row is stripped
        // before the finished callback (`strip_probe_offsets`).
        let mut timestamps_fract: Vec<f64> = timestamps_fract.to_vec();
        let mut probe_fract: Option<f64> = None;
        let mut lazy_probe_fract: Option<f64> = None;
        // True when a lazy run wanted a probe but no disjoint position fits
        // (the window already spans most of a short clip). Such a single window
        // cannot be disambiguated, so an ambiguous result must be dropped
        // rather than baked in (see finished_feeding_frames).
        let mut lazy_probe_unavailable = false;
        if mode == "synchronize"
            && sync_params.offset_method == 2
            && crate::synchronization::find_offset::rs_sync::posterior_enabled()
            && timestamps_fract.len() == 1
            && stab.gyro.read().has_motion()
        {
            if let Some(far) =
                pick_probe_fraction(timestamps_fract[0], org_duration_ms, time_per_syncpoint)
            {
                if lazy_probe_enabled() {
                    // Lazy (default): hold the candidate. It is decoded only
                    // when the first pass lands below the conf gate.
                    log::info!(
                        target: "sync",
                        "[posterior] probe candidate at {:.0}% held for escalation (single sync point at {:.0}%)",
                        far * 100.0,
                        timestamps_fract[0] * 100.0
                    );
                    lazy_probe_fract = Some(far);
                } else {
                    log::info!(
                        target: "sync",
                        "[posterior] probe window added at {:.0}% (single sync point at {:.0}%) — likelihood evidence only, no sync point output",
                        far * 100.0,
                        timestamps_fract[0] * 100.0
                    );
                    timestamps_fract.push(far);
                    probe_fract = Some(far);
                    // Decode ranges must stay in ascending time order: the ffmpeg
                    // range walker only seeks forward through the list, and a probe
                    // placed before the user's sync point would otherwise be
                    // requested after it and deliver zero frames (probe silently
                    // missing from the joint posterior).
                    timestamps_fract
                        .sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                }
            } else {
                log::info!(
                    target: "sync",
                    "[posterior] no disjoint probe position available — single-window posterior (conf one tier down)"
                );
                lazy_probe_unavailable = lazy_probe_enabled();
            }
        }
        let timestamps_fract: &[f64] = &timestamps_fract;

        let frame_count = ((timestamps_fract.len() as f64 * (time_per_syncpoint / 1000.0) * org_fps)
            .ceil() as usize)
            .min(params.frame_count) / every_nth_frame as usize;

        drop(params);

        if duration_ms < 10.0 || frame_count < 2 || time_per_syncpoint < 10.0 || search_size < 10.0
        {
            return Err(());
        }

        let mut ranges_us: Vec<(i64, i64)> = timestamps_fract
            .iter()
            .map(|x| {
                let range = (
                    ((x * org_duration_ms) - (time_per_syncpoint / 2.0)).max(0.0),
                    ((x * org_duration_ms) + (time_per_syncpoint / 2.0)).min(org_duration_ms),
                );
                (
                    (range.0 * 1000.0).round() as i64,
                    (range.1 * 1000.0).round() as i64,
                )
            })
            .collect();

        if mode == "synchronize" && !stab.gyro.read().has_motion() {
            // If no gyro data in file, analyze the entire video
            ranges_us.clear();
            ranges_us.push((0, (org_duration_ms * 1000.0).round() as i64));
        }

        let scaled_ranges_us: Vec<(i64, i64)> = ranges_us
            .iter()
            .map(|(f, t)| {
                (
                    (*f as f64 / fps_scale.unwrap_or(1.0)) as i64,
                    (*t as f64 / fps_scale.unwrap_or(1.0)) as i64,
                )
            })
            .collect();
        // The fraction list was re-sorted after the probe was appended, so the
        // probe's range is located by value, not by position (the no-motion
        // branch above never runs when a probe was added — probe insertion is
        // gated on has_motion()).
        let probe_range_us = probe_fract
            .and_then(|f| timestamps_fract.iter().position(|x| *x == f))
            .and_then(|i| scaled_ranges_us.get(i).copied());

        // Lazy probe: pre-compute both range domains for the candidate using
        // the same formulas as the main range map above.
        let (probe_candidate_us, probe_candidate_scaled_us) = match lazy_probe_fract {
            Some(x) => {
                let range = (
                    ((x * org_duration_ms) - (time_per_syncpoint / 2.0)).max(0.0),
                    ((x * org_duration_ms) + (time_per_syncpoint / 2.0)).min(org_duration_ms),
                );
                let us = (
                    (range.0 * 1000.0).round() as i64,
                    (range.1 * 1000.0).round() as i64,
                );
                let scaled = (
                    (us.0 as f64 / fps_scale.unwrap_or(1.0)) as i64,
                    (us.1 as f64 / fps_scale.unwrap_or(1.0)) as i64,
                );
                (Some(us), Some(scaled))
            }
            None => (None, None),
        };

        let estimator = stab.pose_estimator.clone();

        estimator
            .every_nth_frame
            .store(every_nth_frame.max(1) as u32, SeqCst);
        estimator
            .offset_method
            .store(sync_params.offset_method as u32, SeqCst);
        estimator
            .pose_method
            .store(sync_params.pose_method as u32, SeqCst);

        let mut comp_params = ComputeParams::from_manager(stab);
        comp_params.keyframes.clear();
        // Make sure we apply full correction for autosync
        comp_params.lens_correction_amount = 1.0;

        let thread_pool = rayon::ThreadPoolBuilder::new()
            .thread_name(move |i| format!("Sync {}", i))
            .stack_size(10 * 1024 * 1024) // 10 MB
            .panic_handler(move |e| {
                if let Some(s) = e.downcast_ref::<&str>() {
                    log::error!("Sync thread panic! {}", s);
                } else if let Some(s) = e.downcast_ref::<String>() {
                    log::error!("Sync thread panic! {}", s);
                } else {
                    log::error!("Sync thread panic! {:?}", e);
                }
            })
            // spawn_handler overrides rayon's own thread spawn, so the
            // outer stack_size above is preserved only by mirroring it on
            // std::thread::Builder here. Worker priority is dropped right
            // before entering the rayon work loop.
            .spawn_handler(|thread| {
                let mut b = std::thread::Builder::new().stack_size(10 * 1024 * 1024);
                if let Some(name) = thread.name() {
                    b = b.name(name.to_string());
                }
                b.spawn(move || {
                    crate::worker_priority::apply_to_current_thread();
                    thread.run();
                })?;
                Ok(())
            })
            .build()
            .unwrap();

        crate::synchronization::sync_perf::reset();
        crate::synchronization::flow_gate::reset_stats();
        crate::synchronization::sync_diag::init_session();

        Ok(Self {
            frame_count,
            org_fps,
            scaled_fps,
            sync_params,
            mode,
            ranges_us,
            scaled_ranges_us,
            probe_range_us,
            probe_candidate_us,
            probe_candidate_scaled_us,
            probe_pending: AtomicBool::new(false),
            probe_active: AtomicBool::new(false),
            lazy_probe_unavailable,
            estimator,
            fps_scale,
            total_read_frames: Arc::new(AtomicUsize::new(1)), // Start with 1 to keep the loader active until `finished_feeding_frames` overrides it with final value
            total_detected_frames: Arc::new(AtomicUsize::new(0)),
            compute_params: Arc::new(RwLock::new(comp_params)),
            finished_cb: None,
            progress_cb: None,
            cancel_flag,
            thread_pool,
        })
    }

    pub fn get_ranges(&self) -> Vec<(f64, f64)> {
        self.ranges_us
            .iter()
            .map(|&v| (v.0 as f64 / 1000.0, v.1 as f64 / 1000.0))
            .collect()
    }

    pub fn feed_frame(
        &self,
        mut timestamp_us: i64,
        frame_no: usize,
        mut width: u32,
        height: u32,
        stride: usize,
        pixels: &[u8],
    ) {
        use crate::synchronization::sync_perf::{Stage, StageGuard};
        let _feed_guard = StageGuard::new(Stage::FeedFrame);

        let img = {
            let _g = StageGuard::new(Stage::YuvToGray);
            PoseEstimator::yuv_to_gray(width, height, stride as u32, pixels).map(Arc::new)
        };
        if width > stride as u32 {
            width = stride as u32;
        }

        let method = self.sync_params.of_method as u32;

        // For NeuFlow (method=3 or 4), pass raw NV12 data directly.
        // The fused preprocess_frame_nv12 in neuflow.rs does NV12→CHW conversion
        // during resize, avoiding an intermediate full-frame RGB allocation.
        let frame_data: Option<Arc<Vec<u8>>> = if method == 3 || method == 4 {
            let _g = StageGuard::new(Stage::Nv12Clone);
            let uv_start = stride * height as usize;
            let total_len = uv_start + stride * (height as usize / 2);
            if pixels.len() >= total_len {
                Some(Arc::new(pixels[..total_len].to_vec()))
            } else {
                log::warn!(
                    "NeuFlow: NV12 buffer incomplete (pixels.len={}, need={}) — falling back",
                    pixels.len(),
                    total_len
                );
                None
            }
        } else {
            None
        };
        let estimator = self.estimator.clone();
        let total_detected_frames = self.total_detected_frames.clone();
        let total_read_frames = self.total_read_frames.clone();
        let progress_cb = self.progress_cb.clone();
        let frame_count = self.frame_count;
        let scaled_fps = self.scaled_fps;
        let org_fps = self.org_fps;
        let compute_params = self.compute_params.clone();
        let cancel_flag = self.cancel_flag.clone();
        if let Some(scale) = self.fps_scale {
            timestamp_us = (timestamp_us as f64 / scale) as i64;
        }

        {
            let compute_params = compute_params.read();
            let frame =
                crate::frame_at_timestamp(timestamp_us as f64 / 1000.0, compute_params.scaled_fps)
                    as usize;
            timestamp_us += (compute_params
                .gyro
                .read()
                .file_metadata
                .read()
                .per_frame_time_offsets
                .get(frame)
                .unwrap_or(&0.0)
                * 1000.0)
                .round() as i64;
        }

        let in_user_ranges = self
            .scaled_ranges_us
            .iter()
            .any(|(from, to)| (*from..=*to).contains(&timestamp_us));
        let in_probe = self
            .lazy_probe_scaled_range()
            .is_some_and(|(from, to)| (from..=to).contains(&timestamp_us));
        if in_user_ranges || in_probe {
            self.total_read_frames.fetch_add(1, SeqCst);

            let spawn_at = std::time::Instant::now();
            self.thread_pool.spawn(move || {
                let queued_ns = spawn_at.elapsed().as_nanos() as u64;
                crate::synchronization::sync_perf::record_ns(
                    crate::synchronization::sync_perf::Stage::TaskQueueLatency,
                    queued_ns,
                );
                if cancel_flag.load(Relaxed) {
                    total_detected_frames.fetch_add(1, SeqCst);
                    return;
                }
                if let Some(img) = img {
                    estimator.detect_features(
                        frame_no,
                        timestamp_us,
                        img,
                        frame_data,
                        width,
                        height,
                        stride,
                        method,
                    );
                    total_detected_frames.fetch_add(1, SeqCst);

                    if frame_no % 7 == 0 {
                        estimator.process_detected_frames(
                            org_fps,
                            scaled_fps,
                            &compute_params.read(),
                            Some(cancel_flag.clone()),
                            None,
                        );
                        estimator.recalculate_gyro_data(org_fps, false);
                    }

                    // Suppress stale progress fires on cancel: tasks that
                    // were already in-flight when cancel arrived can outlive
                    // `finished_feeding_frames`'s emit_canceled_progress and
                    // queue a fresh `progress(0.X, ...)` AFTER the 1.0 fire,
                    // resetting `sync_in_progress` to true on the QML side.
                    if let Some(cb) = &progress_cb {
                        if !cancel_flag.load(Relaxed) {
                            let d = total_detected_frames.load(SeqCst);
                            let t = total_read_frames.load(SeqCst).max(frame_count);
                            cb((d as f64 / t.max(1) as f64) * 0.58, d, t);
                        }
                    }
                } else {
                    log::warn!("Failed to get image {:?}", img);
                }
            });
        }
    }

    // §5 helper: fire progress(1.0, n, n) so the controller side clears
    // `sync_in_progress` and re-enables the autosync UI on every cancel path.
    // Without this, lifecycle-canceled autosync leaves the button greyed out.
    fn emit_canceled_progress(&self) {
        if let Some(cb) = &self.progress_cb {
            let d = self.total_detected_frames.load(SeqCst);
            let t = self.total_read_frames.load(SeqCst);
            // Force ready==total so the QML-side condition
            // `ready < total || percent < 1.0` evaluates to false.
            let total = d.max(t);
            log::info!(
                target: "lifecycle",
                "emit_canceled_progress: cb(1.0, {}, {}) — clearing sync_in_progress",
                total,
                total
            );
            cb(1.0, total, total);
        } else {
            log::warn!(
                target: "lifecycle",
                "emit_canceled_progress called but progress_cb is None — sync_in_progress will NOT clear"
            );
        }
    }

    pub fn finished_feeding_frames(&self) {
        // §5.1/§5.2 were once early-return cancel checks but they leaked
        // stale rayon-pool tasks: the run_threaded OpGuard would drop while
        // tasks remained queued, wait_until_idle in the racing load_video
        // would observe count=0 and reset cancel_flag to false, the tasks
        // would then see cancel_flag=false at the line ~360 progress guard
        // and fire stale `progress(<1.0, …)` events AFTER the 1.0 emit —
        // re-greying the sync button on the QML side. The spin-wait below
        // is now the single drain point; under cancel each task fast-exits
        // via the line 326 cancel check (~ms) and the counter catches up
        // within one ~100ms sleep cycle.
        {
            let _g = crate::synchronization::sync_perf::StageGuard::new(
                crate::synchronization::sync_perf::Stage::SpinWait,
            );
            while self.total_detected_frames.load(SeqCst) < self.total_read_frames.load(SeqCst) - 1
            {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
        // Drain done. NOW honor cancel — at this point all in-flight tasks
        // have completed and their progress events are already queued ahead
        // of our emit on the QML thread, so emit_canceled_progress is the
        // last progress event the QML side processes.
        if self.cancel_flag.load(SeqCst) {
            log::info!(target: "lifecycle", "autosync canceled after spin-wait drain");
            self.emit_canceled_progress();
            return;
        }

        let offset_method = self.sync_params.offset_method;

        let progress_cb = self.progress_cb.clone();

        // Wait for any in-progress NeuFlow drain loop to finish before final sweep
        while self.estimator.neuflow_processing.load(SeqCst) {
            if self.cancel_flag.load(SeqCst) {
                log::info!(
                    target: "lifecycle",
                    "autosync canceled during neuflow drain spin-wait"
                );
                self.emit_canceled_progress();
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // §5.3 before process_detected_frames
        if self.cancel_flag.load(SeqCst) {
            log::info!(
                target: "lifecycle",
                "autosync canceled before final process_detected_frames"
            );
            self.emit_canceled_progress();
            return;
        }
        let t_final = std::time::Instant::now();
        log::info!(
            "[autosync timing] finished_feeding_frames: calling final process_detected_frames"
        );
        self.estimator.process_detected_frames(
            self.org_fps,
            self.scaled_fps,
            &self.compute_params.read(),
            Some(self.cancel_flag.clone()),
            None,
        );
        log::info!(
            "[autosync timing] finished_feeding_frames: process_detected_frames done in {:.1}ms",
            t_final.elapsed().as_secs_f64() * 1000.0
        );
        // §5.4 before recalculate_gyro_data
        if self.cancel_flag.load(SeqCst) {
            log::info!(target: "lifecycle", "autosync canceled before recalculate_gyro_data");
            self.emit_canceled_progress();
            return;
        }
        let t_recalc = std::time::Instant::now();
        {
            let _g = crate::synchronization::sync_perf::StageGuard::new(
                crate::synchronization::sync_perf::Stage::RecalculateGyro,
            );
            self.estimator.recalculate_gyro_data(self.org_fps, true);
        }
        log::info!(
            "[autosync timing] finished_feeding_frames: recalculate_gyro_data done in {:.1}ms",
            t_recalc.elapsed().as_secs_f64() * 1000.0
        );
        // §5.5 before cache_optical_flow
        if self.cancel_flag.load(SeqCst) {
            log::info!(target: "lifecycle", "autosync canceled before cache_optical_flow");
            self.emit_canceled_progress();
            return;
        }
        let t_cache = std::time::Instant::now();
        self.estimator
            .cache_optical_flow(if offset_method == 1 { 2 } else { 1 }, self.cancel_flag.clone());
        log::info!(
            "[autosync timing] finished_feeding_frames: cache_optical_flow done in {:.1}ms",
            t_cache.elapsed().as_secs_f64() * 1000.0
        );
        self.estimator.cleanup();

        let mut scaled_ranges_us = Cow::Borrowed(&self.scaled_ranges_us);

        if self.mode == "synchronize" && !self.compute_params.read().gyro.read().has_motion() {
            // §5.6 no-motion fallback entry
            if self.cancel_flag.load(SeqCst) {
                log::info!(
                    target: "lifecycle",
                    "autosync canceled at no-motion fallback entry"
                );
                self.emit_canceled_progress();
                return;
            }
            // If no gyro data in file, set the computed optical flow as gyro data
            let compute_params = self.compute_params.write();
            let mut gyro = compute_params.gyro.write();

            gyro.file_metadata.set_raw_imu(
                self.estimator
                    .estimated_gyro
                    .read()
                    .values()
                    .cloned()
                    .collect::<Vec<_>>(),
            );
            // §5.7 before apply_transforms (the vqf.rs:1120 panic site)
            if self.cancel_flag.load(SeqCst) {
                log::info!(
                    target: "lifecycle",
                    "autosync canceled before apply_transforms in no-motion fallback"
                );
                drop(gyro);
                drop(compute_params);
                self.emit_canceled_progress();
                return;
            }
            gyro.apply_transforms();

            let timestamps_fract = [0.5];
            let time_per_syncpoint = 500.0;

            scaled_ranges_us = Cow::Owned(
                timestamps_fract
                    .into_iter()
                    .map(|x| {
                        (
                            (((x * gyro.duration_ms) - (time_per_syncpoint / 2.0)).max(0.0)
                                * 1000.0
                                / self.fps_scale.unwrap_or(1.0))
                            .round() as i64,
                            (((x * gyro.duration_ms) + (time_per_syncpoint / 2.0))
                                .min(gyro.duration_ms)
                                * 1000.0
                                / self.fps_scale.unwrap_or(1.0))
                            .round() as i64,
                        )
                    })
                    .collect(),
            );
        }

        if let Some(cb) = &progress_cb {
            let d = self.total_detected_frames.load(SeqCst);
            let t = self.total_read_frames.load(SeqCst);
            cb(0.6, d, t);
        }

        let check_negative =
            self.sync_params.initial_offset_inv && self.sync_params.initial_offset.abs() > 1.0;

        let for_negative = AtomicBool::new(false);

        // Locus E (H2 fix): throttle rs-sync / rolling_shutter find_offsets progress
        // forwarding to ≤30 Hz. State is closure-captured (per AutosyncProcess instance);
        // `is_first` and `is_final` bypass the gap check so the UI always sees first +
        // 100% emits. Sync math is unaffected — only this UI notification path is gated.
        const PROGRESS_THROTTLE_MIN_GAP_NS: u64 = 33_000_000;
        let progress_throttle_epoch = std::time::Instant::now();
        let progress_throttle_last_ns = AtomicU64::new(0);
        let progress_throttle_init_logged = AtomicBool::new(false);

        let progress_cb2 = |mut progress| {
            if let Some(cb) = &progress_cb {
                let d = self.total_detected_frames.load(SeqCst);
                let t = self.total_read_frames.load(SeqCst);
                if check_negative {
                    progress += if for_negative.load(SeqCst) { 1.0 } else { 0.0 };
                    progress /= 2.0;
                }
                let scaled = 0.6 + (progress * 0.4);

                let now_ns = (progress_throttle_epoch.elapsed().as_nanos() as u64).max(1);
                let prev = progress_throttle_last_ns.load(Relaxed);
                let is_first = prev == 0;
                let is_final = scaled >= 0.9999;
                let due = now_ns.saturating_sub(prev) >= PROGRESS_THROTTLE_MIN_GAP_NS;

                if is_first || is_final || due {
                    if progress_throttle_last_ns
                        .compare_exchange_weak(prev, now_ns, AcqRel, Relaxed)
                        .is_ok()
                    {
                        if is_first
                            && !progress_throttle_init_logged.swap(true, Relaxed)
                        {
                            log::info!(
                                target: "lifecycle",
                                "batch_sync.progress_throttle_init min_gap_ns={}",
                                PROGRESS_THROTTLE_MIN_GAP_NS
                            );
                        }
                        cb(scaled, d, t);
                    }
                }
            }
        };

        let t_find = std::time::Instant::now();
        let _g_find = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::FindOffsetsTotal,
        );
        if let Some(cb) = &self.finished_cb {
            // §5.8 before find_offsets entry
            if self.cancel_flag.load(SeqCst) {
                log::info!(
                    target: "lifecycle",
                    "autosync canceled before find_offsets dispatch"
                );
                self.emit_canceled_progress();
                return;
            }
            if self.mode == "estimate_rolling_shutter" {
                use super::find_offset::visual_features::find_offsets;
                cb(Either::Left(find_offsets(
                    &self.estimator,
                    &scaled_ranges_us,
                    &self.sync_params,
                    &self.compute_params.read(),
                    true,
                    progress_cb2,
                    self.cancel_flag.clone(),
                )));
            } else if self.mode == "guess_imu_orientation" {
                use super::find_offset::rs_sync::FindOffsetsRssync;
                // FindOffsetsRssync::new now shares the callback as a trait
                // object (the free find_offsets fn wraps it the same way).
                let probe_cb: std::sync::Arc<dyn Fn(f64) + Send + Sync> =
                    std::sync::Arc::new(progress_cb2);
                let guessed = FindOffsetsRssync::new(
                    &scaled_ranges_us,
                    self.estimator.sync_results.clone(),
                    &self.sync_params,
                    &self.compute_params.read(),
                    probe_cb,
                    self.cancel_flag.clone(),
                )
                .guess_orient();
                if !self.cancel_flag.load(SeqCst) {
                    cb(Either::Right(guessed));
                }
            } else {
                // An activated lazy probe joins the sync ranges; its OF was fed
                // in the escalation pass, the user windows' OF persists from
                // pass 1 in estimator.sync_results.
                let mut sync_ranges: Vec<(i64, i64)> = scaled_ranges_us.to_vec();
                if let Some(r) = self.lazy_probe_scaled_range() {
                    sync_ranges.push(r);
                }
                let mut offsets = self.strip_probe_offsets(self.estimator.find_offsets(
                    &sync_ranges,
                    &self.sync_params,
                    &self.compute_params.read(),
                    progress_cb2,
                    self.cancel_flag.clone(),
                ));
                if check_negative {
                    // §5.8 before second find_offsets retry pass
                    if self.cancel_flag.load(SeqCst) {
                        log::info!(
                            target: "lifecycle",
                            "autosync canceled before negative-offset find_offsets retry"
                        );
                        self.emit_canceled_progress();
                        return;
                    }
                    for_negative.store(true, SeqCst);
                    // Try also negative rough offset
                    let mut sync_params = self.sync_params.clone();
                    sync_params.initial_offset = -sync_params.initial_offset;
                    let offsets2 = self.strip_probe_offsets(self.estimator.find_offsets(
                        &sync_ranges,
                        &sync_params,
                        &self.compute_params.read(),
                        progress_cb2,
                        self.cancel_flag.clone(),
                    ));
                    if offsets2.len() > offsets.len() {
                        cb(Either::Left(offsets2));
                    } else if offsets2.len() == offsets.len() {
                        let sum1: f64 = offsets.iter().map(|(_, _, cost, _)| *cost).sum();
                        let sum2: f64 = offsets2.iter().map(|(_, _, cost, _)| *cost).sum();
                        if sum1 < sum2 {
                            cb(Either::Left(offsets));
                        } else {
                            cb(Either::Left(offsets2));
                        }
                    }
                } else {
                    if self.should_escalate_to_probe(check_negative, &offsets) {
                        // Hold the finished callback: the caller decodes the
                        // probe range and calls finished_feeding_frames again
                        // for the joint pass. Skips the final progress(1.0)
                        // below, so the UI stays in the analyzing state.
                        self.probe_pending.store(true, SeqCst);
                        let reason = if self.estimator.probe_escalation_hint.load(SeqCst) {
                            "ambiguous single window (LOW QUALITY / wide CI)"
                        } else {
                            "below conf gate"
                        };
                        log::info!(
                            target: "sync",
                            "[posterior] first pass {} ({} offsets) — escalating to probe window",
                            reason,
                            offsets.len()
                        );
                        return;
                    }
                    // Ambiguous single window that could not get a probe to
                    // disambiguate (short clip, no disjoint position): drop it
                    // rather than bake a confidently-wrong offset. The conf<0.4
                    // filter in the controller/queue discards it.
                    if self.lazy_probe_unavailable
                        && self.estimator.probe_escalation_hint.load(SeqCst)
                    {
                        for o in offsets.iter_mut() {
                            o.3 = o.3.min(0.39);
                        }
                        log::info!(
                            target: "sync",
                            "[posterior] ambiguous single window with no disjoint probe position — demoting confidence so it is dropped ({} offsets)",
                            offsets.len()
                        );
                    }
                    cb(Either::Left(offsets));
                }
            }
        }
        if let Some(cb) = &self.progress_cb {
            let len = self.total_detected_frames.load(SeqCst);
            cb(1.0, len, len);
        }
        drop(_g_find);
        log::info!(
            "[autosync timing] finished_feeding_frames: find_offsets total done in {:.1}ms",
            t_find.elapsed().as_secs_f64() * 1000.0
        );
        crate::synchronization::flow_gate::dump_and_reset_stats();
        crate::synchronization::sync_perf::dump_and_reset();
        crate::synchronization::sync_diag::flush_and_close();
    }

    /// The lazy probe's scaled range, only once activated by escalation.
    fn lazy_probe_scaled_range(&self) -> Option<(i64, i64)> {
        if self.probe_active.load(SeqCst) {
            self.probe_candidate_scaled_us
        } else {
            None
        }
    }

    /// The probe range to strip from results: eager probe (in the range
    /// list from construction) or an activated lazy probe.
    fn probe_strip_range(&self) -> Option<(i64, i64)> {
        self.probe_range_us.or_else(|| self.lazy_probe_scaled_range())
    }

    /// sync-likelihood-nuisance §3.2: drop the probe-only window's offset row
    /// (it contributed likelihood evidence inside `find_offsets`; it must not
    /// become a user-visible sync point).
    fn strip_probe_offsets(&self, mut offsets: Vec<(f64, f64, f64, f64)>) -> Vec<(f64, f64, f64, f64)> {
        if let Some((pf, pt)) = self.probe_strip_range() {
            let before = offsets.len();
            offsets.retain(|(mid_ms, ..)| {
                let mid_us = (mid_ms * 1000.0).round() as i64;
                !(mid_us >= pf && mid_us <= pt)
            });
            if offsets.len() != before {
                log::info!(
                    target: "sync",
                    "[posterior] probe-only window offset stripped from results ({} -> {})",
                    before,
                    offsets.len()
                );
            }
        }
        offsets
    }

    /// Phase-1 verdict: escalate only for single-window synchronize runs that
    /// hold an unactivated lazy candidate and either landed below the conf gate
    /// or were flagged ambiguous by rs-sync (fusion LOW QUALITY / wide
    /// posterior CI — a sharply-wrong single window scores high conf but a
    /// probe window disambiguates it). `check_negative` (legacy
    /// initial_offset_inv retry) opts out.
    fn should_escalate_to_probe(
        &self,
        check_negative: bool,
        offsets: &[(f64, f64, f64, f64)],
    ) -> bool {
        !check_negative
            && self.probe_candidate_us.is_some()
            && !self.probe_active.load(SeqCst)
            && !self.cancel_flag.load(SeqCst)
            && (probe_escalation_needed(offsets)
                || self.estimator.probe_escalation_hint.load(SeqCst))
    }

    /// Caller-side escalation hook: returns the probe decode range (ms) once
    /// after a held phase 1, activating the probe and resetting the feed
    /// counters so the progress bar honestly walks the second pass.
    ///
    /// SAFETY: must be called sequentially on the single decode thread after
    /// `finished_feeding_frames` has drained all in-flight frame tasks. It
    /// mutates atomics through `&self`; concurrent invocation would race the
    /// counter resets. The `probe_pending.swap` guard makes repeat calls a
    /// no-op but does not make concurrent calls safe.
    pub fn pending_probe_ranges(&self) -> Option<Vec<(f64, f64)>> {
        if !self.probe_pending.swap(false, SeqCst) {
            return None;
        }
        if self.cancel_flag.load(SeqCst) {
            // Phase 1 held the finished callback; close the UI loop the same
            // way every other cancel path does.
            self.emit_canceled_progress();
            return None;
        }
        let (f, t) = match self.probe_candidate_us {
            Some(r) => r,
            None => {
                self.emit_canceled_progress();
                return None;
            }
        };
        self.probe_active.store(true, SeqCst);
        self.total_read_frames.store(1, SeqCst);
        self.total_detected_frames.store(0, SeqCst);
        if let Some(cb) = &self.progress_cb {
            cb(0.0, 0, self.frame_count);
        }
        log::info!(
            target: "sync",
            "[posterior] probe escalation: decoding probe window {:.1}-{:.1}ms",
            f as f64 / 1000.0,
            t as f64 / 1000.0
        );
        Some(vec![(f as f64 / 1000.0, t as f64 / 1000.0)])
    }

    pub fn on_progress<F>(&mut self, cb: F)
    where
        F: Fn(f64, usize, usize) + Send + Sync + 'static,
    {
        self.progress_cb = Some(Arc::new(Box::new(cb)));
    }
    pub fn on_finished<F>(&mut self, cb: F)
    where
        F: Fn(Either<Vec<(f64, f64, f64, f64)>, Option<(String, f64)>>) + Send + Sync + 'static,
    {
        self.finished_cb = Some(Arc::new(Box::new(cb)));
    }
}

/// `GYROFLOW_SYNC_LAZY_PROBE`: when enabled (default), the posterior probe
/// window is decoded only after the first pass lands below the conf gate.
/// `0` restores the eager probe (always decoded upfront).
pub(crate) fn lazy_probe_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_SYNC_LAZY_PROBE");
        let (enabled, source) = match raw.as_deref().map(str::trim) {
            Err(_) | Ok("") => (true, "default"),
            Ok(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => (true, "env"),
                "0" | "false" | "no" | "off" => (false, "env"),
                _ => {
                    log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_SYNC_LAZY_PROBE={} invalid, falling back to default (on)",
                        s
                    );
                    (true, "default")
                }
            },
        };
        log::info!(
            target: "lifecycle",
            "sync_lazy_probe resolved enabled={} source={}",
            enabled,
            source
        );
        enabled
    })
}

/// Escalation verdict for the lazy probe (design: trigger line = the
/// controller's conf<0.4 drop filter; grey-zone results land as-is).
/// Empty results count as "no correct point found".
pub(crate) fn probe_escalation_needed(offsets: &[(f64, f64, f64, f64)]) -> bool {
    offsets.is_empty() || offsets.iter().all(|o| o.3 < 0.4)
}

/// Minimum NEW (non-overlapping) frames a probe window must add to be useful
/// independent evidence (`GYROFLOW_SYNC_PROBE_MIN_NEW_MS`, last-resort floor,
/// default 2000ms). The placement already anchors at the farthest clip extreme,
/// so a fully disjoint probe (≈5s total independent span) is used whenever the
/// clip allows; this floor only governs how much overlap is tolerated on short
/// clips before the single window is abandoned.
pub(crate) fn probe_min_new_ms() -> f64 {
    static RESOLVED: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        std::env::var("GYROFLOW_SYNC_PROBE_MIN_NEW_MS")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v > 0.0)
            .unwrap_or(2000.0)
    })
}

/// Probe-window placement (sync-likelihood-nuisance §3.2): anchor a probe at
/// the clip extreme farthest from the existing single sync point so the two
/// windows are as independent as the clip allows. A fully disjoint probe is
/// preferred, but when the user window already spans most of a short clip a
/// partial overlap is allowed as long as the probe still contributes at least
/// `probe_min_new_ms()` of NEW data (capped at the window width so a sub-2.5s
/// window can still use a fully disjoint probe). Returns `None` only when even
/// the extreme placement cannot reach that much new data — the posterior then
/// degrades to single-window (and an ambiguous result is dropped upstream).
pub(crate) fn pick_probe_fraction(
    existing_fract: f64,
    duration_ms: f64,
    window_ms: f64,
) -> Option<f64> {
    if duration_ms <= 0.0 || window_ms <= 0.0 || duration_ms < window_ms {
        return None;
    }
    let required_new = probe_min_new_ms().min(window_ms);
    let half = window_ms / 2.0;
    let e_center = existing_fract * duration_ms;
    let (e_start, e_end) = (e_center - half, e_center + half);
    // Candidate centers clamped so the window fits inside [0, duration].
    let start_center = half;
    let end_center = duration_ms - half;
    let new_data = |p_center: f64| -> f64 {
        let (p_start, p_end) = (p_center - half, p_center + half);
        let overlap = (p_end.min(e_end) - p_start.max(e_start)).max(0.0);
        window_ms - overlap
    };
    // Prefer the end farther from the existing window (more independence).
    let order = if existing_fract >= 0.5 {
        [start_center, end_center]
    } else {
        [end_center, start_center]
    };
    for c in order {
        if new_data(c) >= required_new {
            return Some((c / duration_ms).clamp(0.0, 1.0));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_escalation_gate() {
        // Empty result = "no correct point found" → escalate.
        assert!(probe_escalation_needed(&[]));
        // Below the controller drop line → escalate.
        assert!(probe_escalation_needed(&[(5447.0, -1497.8, 992.8, 0.39)]));
        // At/above the drop line → land as-is (grey zone is NOT re-verified).
        assert!(!probe_escalation_needed(&[(5447.0, -1497.8, 992.8, 0.4)]));
        assert!(!probe_escalation_needed(&[(5447.0, -1497.8, 992.8, 0.85)]));
        // Multi-row: any row at/above the line keeps the result.
        assert!(!probe_escalation_needed(&[
            (1000.0, -1500.0, 10.0, 0.1),
            (5000.0, -1500.0, 10.0, 0.5),
        ]));
        assert!(probe_escalation_needed(&[
            (1000.0, -1500.0, 10.0, 0.1),
            (5000.0, -1500.0, 10.0, 0.39),
        ]));
    }

    #[test]
    fn probe_anchors_at_far_clip_extreme() {
        let approx = |a: Option<f64>, b: f64| {
            assert!(a.is_some_and(|x| (x - b).abs() < 1e-6), "got {a:?}, want ~{b}");
        };
        // Long clip, narrow window: probe anchors at the extreme opposite the
        // existing point (window half-width in from the boundary), fully
        // disjoint. Existing late -> probe near start; early -> near end.
        approx(pick_probe_fraction(0.72, 10_000.0, 1_500.0), 0.075); // near start
        approx(pick_probe_fraction(0.2, 10_000.0, 1_500.0), 0.925); // near end
        approx(pick_probe_fraction(0.5, 10_000.0, 1_500.0), 0.075);
        // 3s window on a 7.5s clip (P1004620 shape): start-anchored probe is
        // fully disjoint (3s new data).
        approx(pick_probe_fraction(0.72, 7_500.0, 3_000.0), 0.2);
    }

    #[test]
    fn probe_allows_overlap_when_min_new_data_met() {
        // R52 shape: 5.4s clip, ~2.8s window at 74% leaves <2.8s before it, so
        // a fully disjoint probe doesn't fit — but a start-anchored probe still
        // adds ~2.57s of new data (265ms overlap), above the 2.5s floor.
        let f = pick_probe_fraction(0.738, 5405.0, 2836.0).expect("overlap probe");
        assert!((f - 1418.0 / 5405.0).abs() < 1e-3, "start-anchored, got {f}");
        // 4s clip, 3s window: only ~1.4s of space outside the window -> below
        // the 2.5s new-data floor -> no probe.
        assert_eq!(pick_probe_fraction(0.72, 4_000.0, 3_000.0), None);
        // Window wider than the clip, and degenerate duration.
        assert_eq!(pick_probe_fraction(0.5, 2_000.0, 3_000.0), None);
        assert_eq!(pick_probe_fraction(0.5, 0.0, 500.0), None);
    }
}
