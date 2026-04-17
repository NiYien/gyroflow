// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

use super::super::{FrameResult, OpticalFlowPoints, PoseEstimator, SyncParams};
use crate::gyro_source::{GyroSource, Quat64, TimeQuat};
use crate::stabilization::{ComputeParams, undistort_points_for_optical_flow};
use nalgebra::Vector3;
use parking_lot::RwLock;
use rs_sync::SyncProblem;
use std::collections::BTreeMap;
use std::f64::consts::PI;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed, Ordering::SeqCst},
};

pub fn find_offsets<F: Fn(f64) + Sync>(
    estimator: &PoseEstimator,
    ranges: &[(i64, i64)],
    sync_params: &SyncParams,
    params: &ComputeParams,
    progress_cb: F,
    cancel_flag: Arc<AtomicBool>,
) -> Vec<(f64, f64, f64, f64)> {
    // Vec<(timestamp, offset, cost, confidence)>
    // confidence ∈ [0, 1]: high-confidence offsets bypass sync_data.rank filter in controller.rs
    // Try essential matrix first, because it's much faster
    let mut sync_params = sync_params.clone();

    let raw_imu_len = {
        let gyro = params.gyro.read();
        let md = gyro.file_metadata.read();
        gyro.raw_imu(&md).len()
    };
    if sync_params.calc_initial_fast && !ranges.is_empty() && raw_imu_len > 0 {
        fn median(mut v: Vec<f64>) -> f64 {
            v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let len = v.len();
            if (len % 2) == 0 {
                (v[len / 2 - 1] + v[len / 2]) / 2.0
            } else {
                v[len / 2]
            }
        }

        let offsets = super::essential_matrix::find_offsets(
            estimator,
            &ranges,
            &sync_params,
            params,
            &progress_cb,
            cancel_flag.clone(),
        );
        if !offsets.is_empty() {
            let median_offset = median(offsets.iter().map(|x| x.1).collect());
            sync_params.initial_offset = median_offset;
            sync_params.initial_offset_inv = false;
            sync_params.search_size = 3000.0;
            log::debug!("Initial offset: {}", median_offset);
        }
    }

    let offsets = {
        let _g = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::RsSyncFullSync,
        );
        let mut finder = FindOffsetsRssync::new(
            ranges,
            estimator.sync_results.clone(),
            &sync_params,
            params,
            progress_cb,
            cancel_flag,
        );
        let mut offsets = finder.full_sync();
        let use_old_rerank = std::env::var("GYROFLOW_SYNC_OLD_RERANK")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        if use_old_rerank {
            finder.correlation_rerank(&mut offsets, estimator, ranges, params);
        } else {
            finder.ncc_fusion_decide(&mut offsets, estimator, ranges, params);
        }
        offsets
    };

    if crate::synchronization::sync_diag::is_enabled() {
        dump_correlation_curves(estimator, ranges, &offsets, &sync_params, params);
    }

    offsets
}

fn dump_correlation_curves(
    estimator: &PoseEstimator,
    ranges: &[(i64, i64)],
    offsets: &[(f64, f64, f64, f64)],
    sync_params: &SyncParams,
    params: &ComputeParams,
) {
    let estimated_map = estimator.estimated_gyro.read();
    let gyro = params.gyro.read();
    let md = gyro.file_metadata.read();
    let raw = gyro.raw_imu(&md);

    for (range_idx, (from_us, to_us)) in ranges.iter().enumerate() {
        let from_ms = *from_us as f64 / 1000.0;
        let to_ms = *to_us as f64 / 1000.0;
        let final_off = offsets
            .iter()
            .find(|(t, _, _, _)| *t >= from_ms && *t <= to_ms)
            .map(|(_, o, _, _)| *o);
        let final_offset_ms = match final_off {
            Some(v) => v,
            None => {
                log::debug!(
                    "[SyncDiag] correlation: range {} cost-final out of acceptable bounds, using initial as placeholder for corr@final",
                    range_idx
                );
                sync_params.initial_offset
            }
        };

        let est: Vec<(f64, [f64; 3])> = estimated_map
            .range(*from_us..*to_us)
            .filter_map(|(_, imu)| imu.gyro.map(|g| (imu.timestamp_ms, g)))
            .collect();
        if est.len() < 10 {
            continue;
        }

        let win_lo = (*from_us as f64 / 1000.0) - sync_params.search_size - 200.0;
        let win_hi = (*to_us as f64 / 1000.0) + sync_params.search_size + 200.0;
        let mut raw_pairs: Vec<(f64, [f64; 3])> = raw
            .iter()
            .filter_map(|x| {
                if x.timestamp_ms >= win_lo && x.timestamp_ms <= win_hi {
                    x.gyro.map(|g| (x.timestamp_ms, g))
                } else {
                    None
                }
            })
            .collect();
        raw_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        if raw_pairs.len() < 10 {
            continue;
        }

        crate::synchronization::sync_diag::analyze_correlation_and_record(
            range_idx,
            &est,
            &raw_pairs,
            sync_params.initial_offset,
            final_offset_ms,
            sync_params.search_size,
            5.0,
        );
    }
}

pub struct FindOffsetsRssync<'a> {
    sync: SyncProblem<'a>,
    gyro_source: Arc<RwLock<GyroSource>>,
    frame_readout_time: f64,
    sync_points: Vec<(i64, i64)>,
    sync_params: &'a SyncParams,
    is_guess_orient: Arc<AtomicBool>,

    current_sync_point: Arc<AtomicUsize>,
    current_orientation: Arc<AtomicUsize>,

}

impl FindOffsetsRssync<'_> {
    pub fn new<'a, F: Fn(f64) + Sync + 'a>(
        ranges: &'a [(i64, i64)],
        sync_results: Arc<RwLock<BTreeMap<i64, FrameResult>>>,
        sync_params: &'a SyncParams,
        params: &'a ComputeParams,
        progress_cb: F,
        cancel_flag: Arc<AtomicBool>,
    ) -> FindOffsetsRssync<'a> {
        let matched_points = Self::collect_points(sync_results, ranges);

        let mut frame_readout_time = params.frame_readout_time;
        if frame_readout_time == 0.0 {
            frame_readout_time = 1000.0 / params.scaled_fps / 2.0;
        }
        if params.lens.global_shutter {
            frame_readout_time = 0.01;
        }
        frame_readout_time /= 1000.0;

        let mut ret = FindOffsetsRssync {
            sync: SyncProblem::new(),
            gyro_source: params.gyro.clone(),
            frame_readout_time: frame_readout_time,
            sync_points: Vec::new(),
            sync_params,
            is_guess_orient: Arc::new(AtomicBool::new(false)),
            current_sync_point: Arc::new(AtomicUsize::new(0)),
            current_orientation: Arc::new(AtomicUsize::new(0)),
        };

        {
            let num_sync_points = matched_points.len() as f64;
            let is_guess_orient = ret.is_guess_orient.clone();
            let cur_sync_point = ret.current_sync_point.clone();
            let cur_orientation = ret.current_orientation.clone();
            ret.sync.on_progress(move |progress| -> bool {
                let num_orientations = if is_guess_orient.load(SeqCst) {
                    48.0
                } else {
                    1.0
                };
                progress_cb(
                    (cur_orientation.load(SeqCst) as f64
                        + ((cur_sync_point.load(SeqCst) as f64 + progress) / num_sync_points))
                        / num_orientations,
                );
                !cancel_flag.load(Relaxed)
            });
        }

        for range in matched_points {
            if range.len() < 2 {
                log::warn!("Not enough data for sync! range.len: {}", range.len());
                continue;
            }

            let mut from_ts = -1;
            let mut to_ts = 0;
            for (((a_t, a_p), (b_t, b_p)), frame_size) in range {
                if from_ts == -1 {
                    from_ts = a_t;
                }
                to_ts = b_t;
                let a = undistort_points_for_optical_flow(&a_p, a_t, &params, frame_size);
                let b = undistort_points_for_optical_flow(&b_p, to_ts, &params, frame_size);

                let mut points3d_a = Vec::new();
                let mut points3d_b = Vec::new();
                let mut tss_a = Vec::new();
                let mut tss_b = Vec::new();

                assert!(a.len() == b.len());

                let height = frame_size.1 as f64;
                for (i, (ap, bp)) in a.iter().zip(b.iter()).enumerate() {
                    let ts_a =
                        a_t as f64 / 1000_000.0 + frame_readout_time * (a_p[i].1 as f64 / height);
                    let ts_b =
                        b_t as f64 / 1000_000.0 + frame_readout_time * (b_p[i].1 as f64 / height);

                    let ap = Vector3::new(ap.0 as f64, ap.1 as f64, 1.0).normalize();
                    let bp = Vector3::new(bp.0 as f64, bp.1 as f64, 1.0).normalize();

                    points3d_a.push((ap[0], ap[1], ap[2]));
                    points3d_b.push((bp[0], bp[1], bp[2]));

                    tss_a.push(ts_a);
                    tss_b.push(ts_b);
                }

                ret.sync
                    .set_track_result(a_t, &tss_a, &tss_b, &points3d_a, &points3d_b);
            }
            ret.sync_points.push((from_ts, to_ts));
        }
        ret
    }

    pub fn full_sync(&mut self) -> Vec<(f64, f64, f64, f64)> {
        // Vec<(timestamp, offset, cost, confidence)>
        // Initial confidence = 0.5 (placeholder, updated by subsequent fusion/rerank stage)
        self.is_guess_orient.store(false, SeqCst);

        let mut offsets = Vec::new();
        {
            let gyro = self.gyro_source.read();
            set_quats(&mut self.sync, &gyro.quaternions);
        }

        for (range_idx, (from_ts, to_ts)) in self.sync_points.iter().enumerate() {
            let presync_step = 3.0;
            let presync_radius = self.sync_params.search_size;
            let initial_delay = -self.sync_params.initial_offset;

            if let Some(delay) = self.sync.full_sync(
                initial_delay / 1000.0,
                *from_ts,
                *to_ts,
                presync_step / 1000.0,
                presync_radius / 1000.0,
                4,
            ) {
                let offset = delay.1 * 1000.0;
                // Only accept offsets that are within 90% of search size range
                let final_offset_external_ms;
                if (offset - initial_delay).abs() < presync_radius * 0.9 {
                    let final_offset = -offset - (self.frame_readout_time * 1000.0 / 2.0);
                    final_offset_external_ms = final_offset;
                    offsets.push((
                        (from_ts + to_ts) as f64 / 2.0 / 1000.0,
                        final_offset,
                        delay.0,
                        0.5, // confidence placeholder; overwritten by fusion stage
                    ));
                } else {
                    log::warn!(
                        "Sync point out of acceptable range {} < {}",
                        (offset - initial_delay).abs(),
                        presync_radius * 0.9
                    );
                    final_offset_external_ms = -offset - (self.frame_readout_time * 1000.0 / 2.0);
                }

                // Note: cost curve scan (5ms step, 600 pre_sync calls) + diag logging
                // moved to `scan_cost_curve_per_seg` in `ncc_fusion_decide`. Reason:
                // scanning here triggers rs-sync's on_progress callback, causing the
                // outer progress bar to jump back to ~50% (each pre_sync resets its
                // internal counter). ncc_fusion_decide suppresses the callback on
                // entry to avoid this side effect.
                let _ = final_offset_external_ms;
            }
            self.current_sync_point.fetch_add(1, SeqCst);
        }
        offsets
    }

    /// Scan rs-sync cost curve (5ms step) and return (best_external_ms, 2nd_best/best).
    /// When diag is enabled, also writes sync_diag's cost_curves_rssync.csv / summary /
    /// local_minima.
    fn scan_cost_curve_per_seg(
        &self,
        range_idx: usize,
        from_ts: i64,
        to_ts: i64,
        final_offset_external_ms: f64,
    ) -> (f64, f64) {
        let frt_offset_ms = self.frame_readout_time * 1000.0 / 2.0;
        let init_delay_s = -self.sync_params.initial_offset / 1000.0;
        let presync_radius = self.sync_params.search_size;
        let half_window_s = 2.5 / 1000.0;
        let step_s = 5.0 / 1000.0;
        let n_steps = (presync_radius * 2.0 / 5.0) as usize;
        let mut curve = Vec::with_capacity(n_steps + 1);
        for k in 0..=n_steps {
            let center_delay_s =
                init_delay_s - presync_radius / 1000.0 + (k as f64) * step_s;
            let cost = self
                .sync
                .pre_sync(center_delay_s, from_ts, to_ts, step_s, half_window_s)
                .map(|(c, _)| c)
                .unwrap_or(f64::NAN);
            let external_offset_ms = -center_delay_s * 1000.0 - frt_offset_ms;
            curve.push((external_offset_ms, cost));
        }
        let (best_offs, best_cost) = curve
            .iter()
            .filter(|p| !p.1.is_nan())
            .min_by(|a, b| {
                a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
            .unwrap_or((final_offset_external_ms, f64::NAN));
        let second_best_cost = curve
            .iter()
            .filter(|p| !p.1.is_nan() && (p.0 - best_offs).abs() > 50.0)
            .map(|p| p.1)
            .fold(f64::INFINITY, f64::min);
        let ratio = if best_cost.abs() > 1e-9 && second_best_cost.is_finite() {
            second_best_cost / best_cost
        } else {
            1.0
        };

        if crate::synchronization::sync_diag::is_enabled() {
            crate::synchronization::sync_diag::record_cost_curve_rssync(
                range_idx, &curve,
            );
            crate::synchronization::sync_diag::record_rssync_summary(
                range_idx,
                self.sync_params.initial_offset,
                final_offset_external_ms,
                best_cost,
                second_best_cost,
            );
            crate::synchronization::sync_diag::analyze_curve_and_record(
                range_idx,
                &curve,
                final_offset_external_ms,
                0.05,
            );
        }
        (best_offs, ratio)
    }

    /// Top-N correlation rerank: for each selected offset, check corr@final; if low,
    /// use debug_pre_sync to obtain the full cost curve, find the lowest-cost point
    /// with correlation≥0.3 among top-N candidates, and locally refine at that point
    /// to replace the original offset.
    ///
    /// Thresholds (determined from 12-sample analysis, with 0.37 wide safety margin):
    /// - corr@final ≥ 0.30: cost and shape consistent → keep
    /// - corr@final ∈ (0.20, 0.30): ambiguous middle region → keep but warn
    /// - corr@final ≤ 0.20: cost chose wrong → trigger rerank
    pub fn correlation_rerank(
        &self,
        offsets: &mut Vec<(f64, f64, f64, f64)>,
        estimator: &super::super::PoseEstimator,
        ranges: &[(i64, i64)],
        params: &ComputeParams,
    ) {
        const CORR_OK: f64 = 0.30;
        const CORR_BAD: f64 = 0.20;
        const CORR_SWITCH_THRESHOLD: f64 = 0.30;
        const DEBUG_POINT_COUNT: usize = 1200;
        const LOCAL_REFINE_RADIUS_MS: f64 = 100.0;
        const NEAREST_TOL_MS: f64 = 10.0;

        let estimated_map = estimator.estimated_gyro.read();
        let gyro = params.gyro.read();
        let md = gyro.file_metadata.read();
        let raw_imu = gyro.raw_imu(&md);

        for i in 0..offsets.len() {
            let (mid_ms, cost_final_ext_ms, cost_final_value, _conf) = offsets[i];
            let mid_us = (mid_ms * 1000.0) as i64;

            // Match the original range (mid falls within it)
            let (from_us, to_us) = match ranges
                .iter()
                .find(|(f, t)| mid_us >= *f && mid_us <= *t)
            {
                Some(r) => *r,
                None => continue,
            };

            // Match sync_points (same condition)
            let sp_match = self.sync_points.iter().find(|(f, t)| {
                let mid_sp = (*f + *t) / 2;
                mid_sp >= from_us && mid_sp <= to_us
            });
            let (sp_from, sp_to) = match sp_match {
                Some(s) => *s,
                None => continue,
            };

            // Prepare estimated / raw sequences
            let est: Vec<(f64, [f64; 3])> = estimated_map
                .range(from_us..to_us)
                .filter_map(|(_, imu)| imu.gyro.map(|g| (imu.timestamp_ms, g)))
                .collect();
            if est.len() < 10 {
                continue;
            }

            let win_lo = (from_us as f64 / 1000.0) - self.sync_params.search_size - 200.0;
            let win_hi = (to_us as f64 / 1000.0) + self.sync_params.search_size + 200.0;
            let mut raw_pairs: Vec<(f64, [f64; 3])> = raw_imu
                .iter()
                .filter_map(|x| {
                    if x.timestamp_ms >= win_lo && x.timestamp_ms <= win_hi {
                        x.gyro.map(|g| (x.timestamp_ms, g))
                    } else {
                        None
                    }
                })
                .collect();
            raw_pairs
                .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            if raw_pairs.len() < 10 {
                continue;
            }

            // corr @ cost_final
            let (_, _, _, corr_at_final, n_paired) =
                crate::synchronization::sync_diag::compute_triaxis_correlation(
                    &est,
                    &raw_pairs,
                    cost_final_ext_ms,
                    NEAREST_TOL_MS,
                );
            if n_paired < 10 {
                continue;
            }

            if corr_at_final >= CORR_OK {
                log::debug!(
                    "[corr-rerank] seg {}: cost_final={:.1}ms corr={:.3} → keep",
                    i, cost_final_ext_ms, corr_at_final
                );
                continue;
            }
            if corr_at_final > CORR_BAD {
                log::warn!(
                    "[corr-rerank] seg {}: cost_final={:.1}ms corr={:.3} (ambiguous, kept)",
                    i, cost_final_ext_ms, corr_at_final
                );
                continue;
            }

            // Trigger rerank
            let initial_delay_s = -self.sync_params.initial_offset / 1000.0;
            let search_radius_s = self.sync_params.search_size / 1000.0;
            let frt_offset_ms = self.frame_readout_time * 1000.0 / 2.0;

            let mut delays = vec![0.0f64; DEBUG_POINT_COUNT];
            let mut costs = vec![0.0f64; DEBUG_POINT_COUNT];
            self.sync.debug_pre_sync(
                initial_delay_s,
                sp_from,
                sp_to,
                search_radius_s,
                &mut delays,
                &mut costs,
                DEBUG_POINT_COUNT,
            );

            // Correlation-first filter: compute correlation over the full curve, keep
            // points with corr>=threshold, and pick the lowest-cost among these
            // "shape-matching" candidates. This covers the case where the true
            // alignment ranks low by cost.
            let mut qualified: Vec<(f64, f64, f64, f64)> = Vec::new();
            // (cost, internal_delay_s, external_ms, corr_r)
            for (&internal_delay_s, &cost_c) in delays.iter().zip(costs.iter()) {
                if !cost_c.is_finite() {
                    continue;
                }
                let external_offset_ms = -internal_delay_s * 1000.0 - frt_offset_ms;
                let (_, _, _, corr_r, n) =
                    crate::synchronization::sync_diag::compute_triaxis_correlation(
                        &est,
                        &raw_pairs,
                        external_offset_ms,
                        NEAREST_TOL_MS,
                    );
                if n >= 10 && corr_r >= CORR_SWITCH_THRESHOLD {
                    qualified.push((cost_c, internal_delay_s, external_offset_ms, corr_r));
                }
            }

            let best = qualified
                .iter()
                .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
                .copied();

            match best {
                Some((best_cost, best_internal_s, best_ext_ms, best_corr)) => {
                    // Near the candidate, use pre_sync to do a fine-step local scan
                    // (not sync's LBFGS, to avoid global optimization drifting to another
                    // cost valley).
                    // radius=5ms covers the 5ms discrete-step neighborhood; step=0.1ms
                    // gives sub-millisecond precision.
                    let fine_radius_s = LOCAL_REFINE_RADIUS_MS / 1000.0 / 20.0; // 5ms
                    let fine_step_s = 0.0001; // 0.1ms
                    if let Some((refined_cost, refined_internal_s)) = self.sync.pre_sync(
                        best_internal_s,
                        sp_from,
                        sp_to,
                        fine_step_s,
                        fine_radius_s,
                    ) {
                        let refined_ext_ms =
                            -refined_internal_s * 1000.0 - frt_offset_ms;
                        log::warn!(
                            "[corr-rerank] seg {}: cost_final={:.1}ms (corr={:.3}) overridden → candidate {:.1}ms (corr={:.3}) refined to {:.3}ms (cost {:.3} → {:.3})",
                            i, cost_final_ext_ms, corr_at_final, best_ext_ms, best_corr,
                            refined_ext_ms, cost_final_value, refined_cost
                        );
                        offsets[i] = (mid_ms, refined_ext_ms, refined_cost, 0.5);
                    } else {
                        log::warn!(
                            "[corr-rerank] seg {}: cost_final={:.1}ms (corr={:.3}) overridden → candidate {:.1}ms (corr={:.3}) [refine failed, using candidate cost {:.3}]",
                            i, cost_final_ext_ms, corr_at_final, best_ext_ms, best_corr,
                            best_cost
                        );
                        offsets[i] = (mid_ms, best_ext_ms, best_cost, 0.5);
                    }
                }
                None => {
                    log::warn!(
                        "[corr-rerank] seg {}: cost_final={:.1}ms corr={:.3}; no point on curve reached corr≥{:.2}, keeping cost-based final (sync unreliable)",
                        i, cost_final_ext_ms, corr_at_final, CORR_SWITCH_THRESHOLD
                    );
                }
            }
        }
    }

    /// Plan B 3-path decision: trust rs-sync when reliable, refine within the NCC
    /// window when it drifts.
    ///
    /// For each sync range:
    ///   Path 0: NCC FFT localization (peak_h < 0.20 or motion too weak → fallback initial)
    ///   Path A: rs-sync cost argmin inside NCC window + 2nd/best>1.05 + NCC OK →
    ///           keep rs-sync offset as-is (rs-sync is most accurate)
    ///   Path B: rs-sync drifted → `pre_sync` 0.1ms fine scan around NCC peak
    ///
    /// **No** Kalman fusion; cost_flat safety is removed (user explicitly requires
    /// fine search even when cost is flat).
    pub fn ncc_fusion_decide(
        &mut self,
        offsets: &mut Vec<(f64, f64, f64, f64)>,
        estimator: &super::super::PoseEstimator,
        ranges: &[(i64, i64)],
        params: &ComputeParams,
    ) {
        // Suppress rs-sync progress callback during this post-processing phase.
        // Both cost-curve scan (600× pre_sync) and NCC-window refine (one pre_sync)
        // trigger the original callback, causing the outer progress bar to jump back.
        // full_sync has already reached 100%; set noop here to keep it stable.
        self.sync.on_progress(|_| true);
        const MIN_PEAK_HEIGHT: f64 = 0.20;
        const MAX_FWHM_MS: f64 = 500.0;
        const SECOND_PEAK_THRESH: f64 = 0.95;
        const MIN_AXIS_ANGLE_DEG: f64 = 0.10;
        const FINE_STEP_S: f64 = 0.0001; // 0.1ms
        const W_MULTIPLIER: f64 = 1.5;

        let estimated_map = estimator.estimated_gyro.read();
        let gyro = params.gyro.read();
        let md = gyro.file_metadata.read();
        let raw_imu = gyro.raw_imu(&md);
        let frt_offset_ms = self.frame_readout_time * 1000.0 / 2.0;

        for i in 0..offsets.len() {
            let (mid_ms, cost_final_ext_ms, cost_final_value, _conf) = offsets[i];
            let mid_us = (mid_ms * 1000.0) as i64;

            let (from_us, to_us) = match ranges
                .iter()
                .find(|(f, t)| mid_us >= *f && mid_us <= *t)
            {
                Some(r) => *r,
                None => continue,
            };
            let sp_match = self.sync_points.iter().find(|(f, t)| {
                let mid_sp = (*f + *t) / 2;
                mid_sp >= from_us && mid_sp <= to_us
            });
            let (sp_from, sp_to) = match sp_match {
                Some(s) => *s,
                None => continue,
            };

            // estimated / raw sequences
            let est: Vec<(f64, [f64; 3])> = estimated_map
                .range(from_us..to_us)
                .filter_map(|(_, imu)| imu.gyro.map(|g| (imu.timestamp_ms, g)))
                .collect();
            if est.len() < 10 {
                continue;
            }
            let win_lo = (from_us as f64 / 1000.0)
                - self.sync_params.search_size
                - 200.0;
            let win_hi = (to_us as f64 / 1000.0)
                + self.sync_params.search_size
                + 200.0;
            let mut raw_pairs: Vec<(f64, [f64; 3])> = raw_imu
                .iter()
                .filter_map(|x| {
                    if x.timestamp_ms >= win_lo && x.timestamp_ms <= win_hi {
                        x.gyro.map(|g| (x.timestamp_ms, g))
                    } else {
                        None
                    }
                })
                .collect();
            raw_pairs
                .sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            if raw_pairs.len() < 10 {
                continue;
            }

            let from_ms = from_us as f64 / 1000.0;
            let to_ms = to_us as f64 / 1000.0;
            let initial_offset = self.sync_params.initial_offset;
            let rs_argmin_ms = cost_final_ext_ms; // full_sync's external offset

            // Scan rs-sync cost curve (5ms step) to get best_offs + ratio (local use;
            // also writes to sync_diag output when diag is enabled)
            let (rs_best_offs, rs_2nd_over_best) =
                self.scan_cost_curve_per_seg(i, sp_from, sp_to, cost_final_ext_ms);

            // ── Path 0: Motion-too-weak early exit ──────────────────────
            let max_axis_angle_deg = est
                .iter()
                .map(|(_, g)| (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt())
                .fold(0.0f64, f64::max);
            if max_axis_angle_deg < MIN_AXIS_ANGLE_DEG {
                log::warn!(
                    "[ncc-fuse] seg {}: motion too weak (max |ω|={:.4} < {}), fallback initial",
                    i, max_axis_angle_deg, MIN_AXIS_ANGLE_DEG
                );
                offsets[i] = (mid_ms, initial_offset, f64::INFINITY, 0.0);
                crate::synchronization::sync_diag::record_fusion_decision(
                    i,
                    f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN,
                    cost_final_ext_ms,
                    initial_offset, f64::INFINITY,
                    rs_argmin_ms, rs_2nd_over_best, f64::NAN,
                    "fallback_initial",
                    Some("motion_too_weak"),
                );
                continue;
            }

            // ── Path 0: NCC FFT localization ────────────────────────────
            let ncc = match crate::synchronization::sync_diag::ncc_fft_align(
                &est,
                &raw_pairs,
                from_ms,
                to_ms,
                self.sync_params.search_size,
            ) {
                Some(r) => r,
                None => {
                    log::warn!(
                        "[ncc-fuse] seg {}: ncc_fft_align returned None, fallback initial",
                        i
                    );
                    offsets[i] = (mid_ms, initial_offset, f64::INFINITY, 0.0);
                    crate::synchronization::sync_diag::record_fusion_decision(
                        i,
                        f64::NAN, f64::NAN, f64::NAN, f64::NAN, f64::NAN,
                        cost_final_ext_ms,
                        initial_offset, f64::INFINITY,
                        rs_argmin_ms, rs_2nd_over_best, f64::NAN,
                        "fallback_initial",
                        Some("ncc_fft_failed"),
                    );
                    continue;
                }
            };

            // Add frt/2 compensation to NCC peak (see note below)
            let ncc_peak_ms = ncc.peak_offset_ms + frt_offset_ms;
            let peak_h = ncc.peak_height;
            let fwhm_ms = ncc.fwhm_ms;
            let r2 = ncc.second_peak_ratio;
            let w_ms = if fwhm_ms.is_finite() && fwhm_ms > 0.0 {
                fwhm_ms * 0.5 * W_MULTIPLIER
            } else {
                self.sync_params.search_size
            };
            let sigma_ncc_ms = if fwhm_ms.is_finite() && fwhm_ms > 0.0 && peak_h > 0.0 {
                ((fwhm_ms / 2.355) / peak_h.sqrt()).max(0.5)
            } else {
                999.0
            };

            // ── NCC quality warning (no longer fallback initial; continue to Path A/B
            //    with best-effort offset but reduced confidence marking unreliable) ─────
            //
            // User feedback: fallback to initial_offset is "giving up" and semantically
            // wrong. Better to pick the most reliable among NCC peak / rs_argmin /
            // refined argmin; just reduce confidence so GUI/rank filter flags as
            // "unreliable".
            let quality_warn: Option<&str> = if peak_h < MIN_PEAK_HEIGHT {
                Some("weak_signal")
            } else if w_ms > MAX_FWHM_MS {
                Some("wide_W")
            } else if r2 > SECOND_PEAK_THRESH {
                Some("periodic_ambiguity")
            } else {
                None
            };
            if let Some(reason) = quality_warn {
                log::warn!(
                    "[ncc-fuse] seg {}: LOW QUALITY {} (peak_h={:.3}, W={:.1}ms, r2={:.3}) — applying best-effort offset with reduced confidence",
                    i, reason, peak_h, w_ms, r2
                );
            }

            // ── Path A: rs-sync trusted (LBFGS consistent with cost-scan argmin) ──
            // Conditions:
            //   A1. rs_argmin within NCC peak ± ncc_check_window (correct basin)
            //   A2. rs_argmin within 10ms of the 5ms-scan argmin (LBFGS did not
            //       fall into a local minimum)
            //
            // Empirically (6380ms sample): rs-sync LBFGS may converge to a local
            // minimum — within the NCC window but cost not globally minimal. When
            // LBFGS argmin differs significantly from the 5ms-scan argmin, we must
            // run fine search to locate the true argmin.
            const RS_CONSISTENCY_TOL_MS: f64 = 10.0;
            let ncc_check_window = (3.0 * sigma_ncc_ms)
                .max(w_ms)
                .min(self.sync_params.search_size);
            let rs_in_window = (rs_argmin_ms - ncc_peak_ms).abs() <= ncc_check_window;
            let rs_lbfgs_consistent = rs_best_offs.is_finite()
                && (rs_argmin_ms - rs_best_offs).abs() < RS_CONSISTENCY_TOL_MS;

            if rs_in_window && rs_lbfgs_consistent {
                // Low-quality sample: reduce confidence to [0.1, 0.2] so GUI/rank filter flags it
                let confidence = if quality_warn.is_some() {
                    peak_h.min(0.2).max(0.05)
                } else {
                    peak_h.max(0.5).clamp(0.0, 1.0)
                };
                let path_str = if quality_warn.is_some() {
                    "rssync_trusted_low_quality"
                } else {
                    "rssync_trusted"
                };
                offsets[i] = (mid_ms, rs_argmin_ms, cost_final_value, confidence);
                log::info!(
                    "[ncc-fuse] seg {}: {} (argmin={:.1}ms ≈ 5ms-scan {:.1}ms, within NCC ±{:.1}ms, 2nd/best={:.3}, h={:.3}, conf={:.3})",
                    i, path_str, rs_argmin_ms, rs_best_offs, ncc_check_window, rs_2nd_over_best, peak_h, confidence
                );
                crate::synchronization::sync_diag::record_fusion_decision(
                    i,
                    ncc_peak_ms, peak_h, fwhm_ms, w_ms, r2,
                    cost_final_ext_ms,
                    rs_argmin_ms, cost_final_value,
                    rs_argmin_ms, rs_2nd_over_best, f64::NAN,
                    path_str,
                    quality_warn,
                );
                continue;
            }
            if rs_in_window && !rs_lbfgs_consistent {
                log::warn!(
                    "[ncc-fuse] seg {}: rs-sync LBFGS argmin={:.1}ms but 5ms-scan argmin={:.1}ms (diff {:.1}ms > {}), going to refine",
                    i, rs_argmin_ms, rs_best_offs,
                    (rs_argmin_ms - rs_best_offs).abs(), RS_CONSISTENCY_TOL_MS
                );
            }

            // ── Path B: rs-sync drifted → fine search near NCC peak ────────────
            // (external_offset_ms = -internal_s * 1000 - frt_offset)
            // → internal_s = -(external_ms + frt_offset) / 1000
            let center_internal_s = -(ncc_peak_ms + frt_offset_ms) / 1000.0;
            let fine_radius_s = (w_ms / 1000.0).max(FINE_STEP_S * 2.0);
            let refine = self.sync.pre_sync(
                center_internal_s,
                sp_from,
                sp_to,
                FINE_STEP_S,
                fine_radius_s,
            );

            let (fused_offset_ms, refined_cost, path_name, fb) = if let Some((c, s_best)) = refine {
                let refined_ext_ms = -s_best * 1000.0 - frt_offset_ms;
                // Boundary fallback: fine argmin landing on window edge likely signals wrong peak
                let boundary_tol = FINE_STEP_S * 1000.0 * 2.0; // 0.2ms
                let at_left = (refined_ext_ms - (ncc_peak_ms - w_ms)).abs() < boundary_tol;
                let at_right = (refined_ext_ms - (ncc_peak_ms + w_ms)).abs() < boundary_tol;
                if at_left || at_right {
                    (ncc_peak_ms, c, "ncc_peak_only", Some("refine_at_boundary"))
                } else {
                    (refined_ext_ms, c, "ncc_window_refine", None)
                }
            } else {
                log::warn!(
                    "[ncc-fuse] seg {}: pre_sync refine failed, use NCC peak",
                    i
                );
                (ncc_peak_ms, f64::NAN, "ncc_peak_only", Some("pre_sync_failed"))
            };

            // Low-quality sample: discount confidence
            let confidence = if quality_warn.is_some() {
                peak_h.min(0.2).max(0.05)
            } else {
                peak_h.clamp(0.0, 1.0)
            };
            let path_str_owned: String = if quality_warn.is_some() {
                format!("{}_low_quality", path_name)
            } else {
                path_name.to_string()
            };
            offsets[i] = (mid_ms, fused_offset_ms, refined_cost, confidence);

            log::info!(
                "[ncc-fuse] seg {}: {} — rs_argmin={:.1}ms (ratio={:.3}), NCC peak={:.1}ms (h={:.3}, FWHM={:.1}ms) → output={:.1}ms (cost={:.3}, conf={:.3})",
                i,
                path_str_owned,
                rs_argmin_ms,
                rs_2nd_over_best,
                ncc_peak_ms,
                peak_h,
                fwhm_ms,
                fused_offset_ms,
                refined_cost,
                confidence
            );
            let combined_fb = match (quality_warn, fb) {
                (Some(q), Some(f)) => Some(format!("{}|{}", q, f)),
                (Some(q), None) => Some(q.to_string()),
                (None, Some(f)) => Some(f.to_string()),
                (None, None) => None,
            };
            crate::synchronization::sync_diag::record_fusion_decision(
                i,
                ncc_peak_ms, peak_h, fwhm_ms, w_ms, r2,
                cost_final_ext_ms,
                fused_offset_ms, refined_cost,
                rs_argmin_ms, rs_2nd_over_best, fused_offset_ms,
                &path_str_owned,
                combined_fb.as_deref(),
            );
        }
    }

    pub fn guess_orient(&mut self) -> Option<(String, f64)> {
        let _g = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::RsSyncGuessOrient,
        );
        self.is_guess_orient.store(true, SeqCst);

        let mut clone_source = self.gyro_source.read().clone();

        let possible_orientations = [
            "YxZ", "Xyz", "XZy", "Zxy", "zyX", "yxZ", "ZXY", "zYx", "ZYX", "yXz", "YZX", "XyZ",
            "Yzx", "zXy", "YXz", "xyz", "yZx", "XYZ", "zxy", "xYz", "XYz", "zxY", "zXY", "xZy",
            "zyx", "xyZ", "Yxz", "xzy", "yZX", "yzX", "ZYx", "xYZ", "zYX", "ZxY", "yzx", "xZY",
            "Xzy", "XzY", "YzX", "Zyx", "XZY", "yxz", "xzY", "ZyX", "YXZ", "yXZ", "YZx", "ZXy",
        ];

        possible_orientations
            .iter()
            .map(|orient| {
                clone_source.imu_transforms.imu_orientation = Some(orient.to_string());
                clone_source.apply_transforms();

                set_quats(&mut self.sync, &clone_source.quaternions);

                let total_cost: f64 = self
                    .sync_points
                    .iter()
                    .map(|(from_ts, to_ts)| {
                        self.sync
                            .pre_sync(
                                -self.sync_params.initial_offset / 1000.0,
                                *from_ts,
                                *to_ts,
                                3.0 / 1000.0,
                                self.sync_params.search_size / 1000.0,
                            )
                            .unwrap_or((0.0, 0.0))
                    })
                    .map(|v| v.0)
                    .sum();

                self.current_orientation.fetch_add(1, SeqCst);

                (orient.to_string(), total_cost)
            })
            .reduce(|a: (String, f64), b: (String, f64)| -> (String, f64) {
                if a.1 < b.1 { a } else { b }
            })
    }

    fn collect_points(
        sync_results: Arc<RwLock<BTreeMap<i64, FrameResult>>>,
        ranges: &[(i64, i64)],
    ) -> Vec<
        Vec<(
            ((i64, OpticalFlowPoints), (i64, OpticalFlowPoints)),
            (u32, u32),
        )>,
    > {
        let mut points = Vec::new();
        for (from_ts, to_ts) in ranges {
            let mut points_per_range = Vec::new();
            if to_ts > from_ts {
                let l = sync_results.read();
                for (_ts, x) in l.range(from_ts..to_ts) {
                    if let Ok(of) = x.optical_flow.try_borrow() {
                        if let Some(Some(opt_pts)) = of.get(&1) {
                            points_per_range.push((opt_pts.clone(), x.frame_size));
                        }
                    }
                }
            }
            points.push(points_per_range);
        }
        points
    }
}

fn set_quats(sync: &mut SyncProblem, source_quats: &TimeQuat) {
    let mut quats = Vec::new();
    let mut timestamps = Vec::new();
    let rotation = *Quat64::from_scaled_axis(Vector3::new(PI, 0.0, 0.0)).quaternion();

    for (ts, q) in source_quats {
        let q = Quat64::from(*q).quaternion() * rotation;
        let qv = q.as_vector();

        quats.push((qv[3], -qv[0], -qv[1], -qv[2])); // w, x, y, z
        timestamps.push(*ts);
    }
    sync.set_gyro_quaternions(&timestamps, &quats);
}
