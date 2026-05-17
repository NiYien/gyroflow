// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use itertools::Either;
use parking_lot::RwLock;
use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed, Ordering::SeqCst};

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

        let scaled_ranges_us = ranges_us
            .iter()
            .map(|(f, t)| {
                (
                    (*f as f64 / fps_scale.unwrap_or(1.0)) as i64,
                    (*t as f64 / fps_scale.unwrap_or(1.0)) as i64,
                )
            })
            .collect();

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
            .build()
            .unwrap();

        crate::synchronization::sync_perf::reset();
        crate::synchronization::sync_diag::init_session();

        Ok(Self {
            frame_count,
            org_fps,
            scaled_fps,
            sync_params,
            mode,
            ranges_us,
            scaled_ranges_us,
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
                log::debug!("NeuFlow: passing NV12 directly ({width}x{height}, stride={stride})");
                Some(Arc::new(pixels[..total_len].to_vec()))
            } else {
                log::debug!(
                    "NeuFlow: NV12 buffer incomplete (pixels.len={}, need={})",
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

        if let Some(_current_range) = self
            .scaled_ranges_us
            .iter()
            .find(|(from, to)| (*from..=*to).contains(&timestamp_us))
        {
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

        let progress_cb2 = |mut progress| {
            if let Some(cb) = &progress_cb {
                let d = self.total_detected_frames.load(SeqCst);
                let t = self.total_read_frames.load(SeqCst);
                if check_negative {
                    progress += if for_negative.load(SeqCst) { 1.0 } else { 0.0 };
                    progress /= 2.0;
                }
                cb(0.6 + (progress * 0.4), d, t);
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
                let guessed = FindOffsetsRssync::new(
                    &scaled_ranges_us,
                    self.estimator.sync_results.clone(),
                    &self.sync_params,
                    &self.compute_params.read(),
                    progress_cb2,
                    self.cancel_flag.clone(),
                )
                .guess_orient();
                if !self.cancel_flag.load(SeqCst) {
                    cb(Either::Right(guessed));
                }
            } else {
                let offsets = self.estimator.find_offsets(
                    &scaled_ranges_us,
                    &self.sync_params,
                    &self.compute_params.read(),
                    progress_cb2,
                    self.cancel_flag.clone(),
                );
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
                    let offsets2 = self.estimator.find_offsets(
                        &scaled_ranges_us,
                        &sync_params,
                        &self.compute_params.read(),
                        progress_cb2,
                        self.cancel_flag.clone(),
                    );
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
        crate::synchronization::sync_perf::dump_and_reset();
        crate::synchronization::sync_diag::flush_and_close();
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
