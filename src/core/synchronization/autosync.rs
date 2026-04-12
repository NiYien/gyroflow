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
                dyn Fn(Either<Vec<(f64, f64, f64)>, Option<(String, f64)>>) + Send + Sync + 'static,
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

    /// Convert NV12 pixel data to RGB for NeuFlow optical flow.
    /// NV12 layout: Y plane (stride * height bytes), then interleaved UV plane (stride * height/2 bytes).
    fn nv12_to_rgb(width: u32, height: u32, stride: usize, pixels: &[u8]) -> Option<Vec<u8>> {
        let w = width as usize;
        let h = height as usize;
        let s = stride;
        let uv_start = s * h;
        if pixels.len() < uv_start + s * h / 2 {
            return None;
        }
        let mut rgb = vec![0u8; w * h * 3];
        for row in 0..h {
            for col in 0..w {
                let y = pixels[row * s + col] as f32;
                let uv_row = row / 2;
                let uv_col = (col / 2) * 2;
                let u = pixels[uv_start + uv_row * s + uv_col] as f32 - 128.0;
                let v = pixels[uv_start + uv_row * s + uv_col + 1] as f32 - 128.0;
                let r = (y + 1.402 * v).clamp(0.0, 255.0) as u8;
                let g = (y - 0.344136 * u - 0.714136 * v).clamp(0.0, 255.0) as u8;
                let b = (y + 1.772 * u).clamp(0.0, 255.0) as u8;
                let idx = (row * w + col) * 3;
                rgb[idx] = r;
                rgb[idx + 1] = g;
                rgb[idx + 2] = b;
            }
        }
        Some(rgb)
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
        let img = PoseEstimator::yuv_to_gray(width, height, stride as u32, pixels).map(Arc::new);
        if width > stride as u32 {
            width = stride as u32;
        }

        let method = self.sync_params.of_method as u32;

        // For NeuFlow (method=3), extract RGB data before entering the 'static closure.
        // Try NV12→RGB first; if pixel buffer doesn't have UV data, fall back to grayscale→RGB.
        let rgb_data: Option<Arc<Vec<u8>>> = if method == 3 {
            if let Some(rgb) = Self::nv12_to_rgb(width, height, stride, pixels) {
                log::debug!("NeuFlow: extracted RGB from NV12 ({width}x{height}, stride={stride})");
                Some(Arc::new(rgb))
            } else if let Some(ref gray_img) = img {
                // Fallback: construct pseudo-RGB from grayscale (R=G=B=Y)
                log::debug!("NeuFlow: NV12→RGB failed (pixels.len={}), using grayscale fallback", pixels.len());
                let gray_bytes = gray_img.as_raw();
                let mut rgb = Vec::with_capacity(gray_bytes.len() * 3);
                for &g in gray_bytes.iter().take((width * height) as usize) {
                    rgb.push(g); rgb.push(g); rgb.push(g);
                }
                Some(Arc::new(rgb))
            } else {
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

            self.thread_pool.spawn(move || {
                if cancel_flag.load(Relaxed) {
                    total_detected_frames.fetch_add(1, SeqCst);
                    return;
                }
                if let Some(img) = img {
                    estimator.detect_features(frame_no, timestamp_us, img, rgb_data, width, height, method);
                    total_detected_frames.fetch_add(1, SeqCst);

                    if frame_no % 7 == 0 {
                        estimator.process_detected_frames(
                            org_fps,
                            scaled_fps,
                            &compute_params.read(),
                        );
                        estimator.recalculate_gyro_data(org_fps, false);
                    }

                    if let Some(cb) = &progress_cb {
                        let d = total_detected_frames.load(SeqCst);
                        let t = total_read_frames.load(SeqCst).max(frame_count);
                        cb((d as f64 / t.max(1) as f64) * 0.58, d, t);
                    }
                } else {
                    log::warn!("Failed to get image {:?}", img);
                }
            });
        }
    }

    pub fn finished_feeding_frames(&self) {
        while self.total_detected_frames.load(SeqCst) < self.total_read_frames.load(SeqCst) - 1 {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        let offset_method = self.sync_params.offset_method;

        let progress_cb = self.progress_cb.clone();

        self.estimator.process_detected_frames(
            self.org_fps,
            self.scaled_fps,
            &self.compute_params.read(),
        );
        self.estimator.recalculate_gyro_data(self.org_fps, true);
        self.estimator
            .cache_optical_flow(if offset_method == 1 { 2 } else { 1 });
        self.estimator.cleanup();

        let mut scaled_ranges_us = Cow::Borrowed(&self.scaled_ranges_us);

        if self.mode == "synchronize" && !self.compute_params.read().gyro.read().has_motion() {
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

        if let Some(cb) = &self.finished_cb {
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
                        let sum1: f64 = offsets.iter().map(|(_, _, cost)| *cost).sum();
                        let sum2: f64 = offsets2.iter().map(|(_, _, cost)| *cost).sum();
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
    }

    pub fn on_progress<F>(&mut self, cb: F)
    where
        F: Fn(f64, usize, usize) + Send + Sync + 'static,
    {
        self.progress_cb = Some(Arc::new(Box::new(cb)));
    }
    pub fn on_finished<F>(&mut self, cb: F)
    where
        F: Fn(Either<Vec<(f64, f64, f64)>, Option<(String, f64)>>) + Send + Sync + 'static,
    {
        self.finished_cb = Some(Arc::new(Box::new(cb)));
    }
}
