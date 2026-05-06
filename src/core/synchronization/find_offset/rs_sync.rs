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
        let finder_t0 = std::time::Instant::now();
        let mut finder = {
            let _g_new = crate::synchronization::sync_perf::StageGuard::new(
                crate::synchronization::sync_perf::Stage::RsSyncFinderNew,
            );
            FindOffsetsRssync::new(
                ranges,
                estimator.sync_results.clone(),
                &sync_params,
                params,
                progress_cb,
                cancel_flag,
            )
        };
        log::info!(
            "[rssync-timing] FindOffsetsRssync::new done in {:.1}ms",
            finder_t0.elapsed().as_secs_f64() * 1000.0
        );
        let fs_t0 = std::time::Instant::now();
        let mut offsets = {
            let _g_fs = crate::synchronization::sync_perf::StageGuard::new(
                crate::synchronization::sync_perf::Stage::RsSyncCoreFullSync,
            );
            finder.full_sync()
        };
        log::info!(
            "[rssync-timing] full_sync() done in {:.1}ms ({} segments)",
            fs_t0.elapsed().as_secs_f64() * 1000.0,
            offsets.len()
        );
        let bypass_fusion = std::env::var("GYROFLOW_BYPASS_FUSION")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        let use_old_rerank = std::env::var("GYROFLOW_SYNC_OLD_RERANK")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        if bypass_fusion {
            log::info!("[rssync] BYPASS FUSION — using raw full_sync() output (matches upstream main)");
        } else if use_old_rerank {
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

    /// Per-range pre_sync grid cost curve produced during `full_sync()`.
    /// Each inner Vec is `(cost, delay_s)` in scan order — reused by
    /// `scan_cost_curve_per_seg` to avoid re-scanning the same grid.
    presync_curves: Vec<Vec<(f64, f64)>>,
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
            presync_curves: Vec::new(),
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

        // Pre-size presync_curves so per-range indices align with sync_points order.
        self.presync_curves.clear();
        self.presync_curves
            .resize(self.sync_points.len(), Vec::new());
        let sync_points = self.sync_points.clone();
        for (range_idx, (from_ts, to_ts)) in sync_points.iter().enumerate() {
            let range_t0 = std::time::Instant::now();
            let presync_step = 5.0;
            let presync_radius = self.sync_params.search_size;
            let initial_delay = -self.sync_params.initial_offset;

            let sync_call_t0 = std::time::Instant::now();
            let delay_res = self.sync.full_sync_with_curve(
                initial_delay / 1000.0,
                *from_ts,
                *to_ts,
                presync_step / 1000.0,
                presync_radius / 1000.0,
                2,
            );
            let sync_call_ms = sync_call_t0.elapsed().as_secs_f64() * 1000.0;
            if let Some((delay, curve)) = delay_res {
                let offset = delay.1 * 1000.0;
                // Only accept offsets that are within 90% of search size range
                let final_offset_external_ms;
                let bounded_max = presync_radius * 0.9;
                if (offset - initial_delay).abs() < bounded_max {
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
                        "LBFGS out of bounds {:.1} > {:.1} — fallback to grid argmin within bounds",
                        (offset - initial_delay).abs(),
                        bounded_max
                    );
                    // Fallback: scan grid curve for lowest cost within bounds.
                    // curve is Vec<(cost, delay_s)> from full_sync_with_curve's
                    // pre-search grid (5ms step). delay_s is rs-sync internal
                    // (pre-frt-comp), same convention as `offset` here.
                    let mut best: Option<(f64, f64)> = None; // (cost, delay_s)
                    for &(c, d_s) in curve.iter() {
                        let off_ms = d_s * 1000.0;
                        if (off_ms - initial_delay).abs() < bounded_max && c.is_finite() {
                            match best {
                                None => best = Some((c, d_s)),
                                Some((bc, _)) if c < bc => best = Some((c, d_s)),
                                _ => {}
                            }
                        }
                    }
                    if let Some((b_cost, b_delay_s)) = best {
                        let b_offset = b_delay_s * 1000.0;
                        let final_offset =
                            -b_offset - (self.frame_readout_time * 1000.0 / 2.0);
                        log::info!(
                            "[rssync] grid fallback: offset={:.1}ms cost={:.4}",
                            -b_offset, b_cost
                        );
                        final_offset_external_ms = final_offset;
                        offsets.push((
                            (from_ts + to_ts) as f64 / 2.0 / 1000.0,
                            final_offset,
                            b_cost,
                            0.5,
                        ));
                    } else {
                        log::warn!("[rssync] no grid candidate within bounds, segment dropped");
                        final_offset_external_ms =
                            -offset - (self.frame_readout_time * 1000.0 / 2.0);
                    }
                }
                self.presync_curves[range_idx] = curve;

                // Note: cost curve scan (5ms step, 600 pre_sync calls) + diag logging
                // moved to `scan_cost_curve_per_seg` in `ncc_fusion_decide`. Reason:
                // scanning here triggers rs-sync's on_progress callback, causing the
                // outer progress bar to jump back to ~50% (each pre_sync resets its
                // internal counter). ncc_fusion_decide suppresses the callback on
                // entry to avoid this side effect.
                let _ = final_offset_external_ms;
            }
            self.current_sync_point.fetch_add(1, SeqCst);
            log::info!(
                "[rssync-timing] range {}: sync.full_sync={:.1}ms total_range={:.1}ms ({}→{} us, radius={:.0}ms)",
                range_idx,
                sync_call_ms,
                range_t0.elapsed().as_secs_f64() * 1000.0,
                from_ts,
                to_ts,
                presync_radius
            );
        }
        offsets
    }

    /// Read rs-sync cost curve cached from `full_sync_with_curve` and return
    /// (best_external_ms, 2nd_best/best). Before Step 2 this used to scan the
    /// grid itself via `self.sync.pre_sync(...)` ~1400 times (~1.9s); the grid
    /// is now computed once inside `full_sync_with_curve` and reused here.
    /// When diag is enabled, also writes sync_diag's cost_curves_rssync.csv /
    /// summary / local_minima.
    fn scan_cost_curve_per_seg(
        &self,
        range_idx: usize,
        _from_ts: i64,
        _to_ts: i64,
        final_offset_external_ms: f64,
    ) -> (f64, f64) {
        let _g = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::NccCostScan,
        );
        let frt_offset_ms = self.frame_readout_time * 1000.0 / 2.0;
        // Raw curve from rs-sync: Vec<(cost, delay_s)> in scan order.
        // Convert to external (offset_ms, cost) for downstream consumers.
        let raw = self
            .presync_curves
            .get(range_idx)
            .cloned()
            .unwrap_or_default();
        let curve: Vec<(f64, f64)> = raw
            .iter()
            .map(|(cost, delay_s)| {
                let external_offset_ms = -delay_s * 1000.0 - frt_offset_ms;
                (external_offset_ms, *cost)
            })
            .collect();
        if curve.is_empty() {
            // Fallback: if for some reason full_sync_with_curve wasn't called
            // (shouldn't happen in the normal flow), just signal no second-best
            // information. `final_offset_external_ms` is used as argmin.
            return (final_offset_external_ms, 1.0);
        }
        let (best_offs, best_cost) = curve
            .iter()
            .filter(|p| !p.1.is_nan())
            .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
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
            crate::synchronization::sync_diag::record_cost_curve_rssync(range_idx, &curve);
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
        let _g = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::CorrelationRerank,
        );
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
            let (from_us, to_us) = match ranges.iter().find(|(f, t)| mid_us >= *f && mid_us <= *t) {
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
            raw_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
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
                    i,
                    cost_final_ext_ms,
                    corr_at_final
                );
                continue;
            }
            if corr_at_final > CORR_BAD {
                log::warn!(
                    "[corr-rerank] seg {}: cost_final={:.1}ms corr={:.3} (ambiguous, kept)",
                    i,
                    cost_final_ext_ms,
                    corr_at_final
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
                        let refined_ext_ms = -refined_internal_s * 1000.0 - frt_offset_ms;
                        log::warn!(
                            "[corr-rerank] seg {}: cost_final={:.1}ms (corr={:.3}) overridden → candidate {:.1}ms (corr={:.3}) refined to {:.3}ms (cost {:.3} → {:.3})",
                            i,
                            cost_final_ext_ms,
                            corr_at_final,
                            best_ext_ms,
                            best_corr,
                            refined_ext_ms,
                            cost_final_value,
                            refined_cost
                        );
                        offsets[i] = (mid_ms, refined_ext_ms, refined_cost, 0.5);
                    } else {
                        log::warn!(
                            "[corr-rerank] seg {}: cost_final={:.1}ms (corr={:.3}) overridden → candidate {:.1}ms (corr={:.3}) [refine failed, using candidate cost {:.3}]",
                            i,
                            cost_final_ext_ms,
                            corr_at_final,
                            best_ext_ms,
                            best_corr,
                            best_cost
                        );
                        offsets[i] = (mid_ms, best_ext_ms, best_cost, 0.5);
                    }
                }
                None => {
                    log::warn!(
                        "[corr-rerank] seg {}: cost_final={:.1}ms corr={:.3}; no point on curve reached corr≥{:.2}, keeping cost-based final (sync unreliable)",
                        i,
                        cost_final_ext_ms,
                        corr_at_final,
                        CORR_SWITCH_THRESHOLD
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
        let _g_fuse = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::NccFusionDecide,
        );
        let fuse_t0 = std::time::Instant::now();
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

        // Hybrid cost threshold: per-segment, if rs-sync internal LBFGS cost
        // is below this, trust raw rs_argmin (skip fusion 4-candidate logic
        // which can wrongly reject correct rs_argmin in segments where
        // est_gyro Pearson r is negative due to small frame-pair noise).
        // Empirical from long-focal video: rs-sync-converged segments cost
        // 1-100, non-converged cost > 1000. 100 is a clean separator.
        // Override via GYROFLOW_FUSION_COST_THRESHOLD=<f64>.
        let fusion_cost_threshold: f64 = std::env::var("GYROFLOW_FUSION_COST_THRESHOLD")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(100.0);

        for i in 0..offsets.len() {
            // Hybrid bypass: trust raw rs_argmin when rs-sync converged well
            let raw_cost = offsets[i].2;
            if raw_cost.is_finite() && raw_cost <= fusion_cost_threshold {
                log::info!(
                    "[ncc-fuse] seg {}: cost={:.2} ≤ {:.0} → bypass fusion (trust rs_argmin offset={:.1}ms)",
                    i,
                    raw_cost,
                    fusion_cost_threshold,
                    offsets[i].1
                );
                // offsets[i] already (mid_ms, rs_argmin_offset, cost, 0.5);
                // keep conf=0.5 as raw rs_argmin trust signal.
                continue;
            }
            let seg_t0 = std::time::Instant::now();
            let mut tik_ns: u64 = 0;
            let cost_scan_ns: u64;
            let ncc_fft_ns: u64;
            let mut pearson_scan_ns: u64 = 0;
            let output_pre_sync_ns: u64;
            let (mid_ms, cost_final_ext_ms, _cost_final_value, _conf) = offsets[i];
            let mid_us = (mid_ms * 1000.0) as i64;

            let (from_us, to_us) = match ranges.iter().find(|(f, t)| mid_us >= *f && mid_us <= *t) {
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
            let mut est: Vec<(f64, [f64; 3])> = estimated_map
                .range(from_us..to_us)
                .filter_map(|(_, imu)| imu.gyro.map(|g| (imu.timestamp_ms, g)))
                .collect();
            if est.len() < 10 {
                continue;
            }

            // Compute max angular magnitude BEFORE smoothing (used for both
            // adaptive Tikhonov λ and Path 0 motion-too-weak gate).
            let max_axis_angle_deg = est
                .iter()
                .map(|(_, g)| (g[0] * g[0] + g[1] * g[1] + g[2] * g[2]).sqrt())
                .fold(0.0f64, f64::max);

            // Tikhonov-regularized est_gyro smoothing (global joint optimization).
            // Solves: min_ω Σ ||ω_i - ω̂_i||² + λ · Σ ||ω_{i+1} - 2ω_i + ω_{i-1}||²
            // Equivalent to (I + λ·LᵀL) ω = ω̂, where L is 2nd-difference operator.
            // Boundary-aware (all frames included). λ controls smoothness strength.
            // Env: GYROFLOW_SYNC_NO_SMOOTH=1 disables (keeps original est_gyro);
            //      GYROFLOW_SYNC_SMOOTH_LAMBDA=<f64> overrides adaptive λ.
            let smooth_enabled = std::env::var("GYROFLOW_SYNC_NO_SMOOTH")
                .map(|v| !matches!(v.trim(), "1" | "true" | "yes" | "on"))
                .unwrap_or(true);
            if smooth_enabled && est.len() >= 5 {
                let _g_tik = crate::synchronization::sync_perf::StageGuard::new(
                    crate::synchronization::sync_perf::Stage::NccTikhonov,
                );
                let tik_t0 = std::time::Instant::now();
                // Adaptive λ: weaker motion → stronger smoothing (correct RANSAC
                // outliers); stronger motion → weaker smoothing (preserve
                // high-freq real motion). max_axis_angle_deg is per-frame angular
                // magnitude max across the segment.
                let lambda_default = (3.0 / max_axis_angle_deg.max(0.5)).clamp(0.1, 5.0);
                let lambda = std::env::var("GYROFLOW_SYNC_SMOOTH_LAMBDA")
                    .ok()
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .unwrap_or(lambda_default);
                log::info!(
                    "[tikhonov] seg {}: λ={:.3} (max_axis_angle={:.3}°)",
                    i,
                    lambda,
                    max_axis_angle_deg
                );
                let n = est.len();
                // A = I + λ·LᵀL is symmetric pentadiagonal (bandwidth 2):
                //   interior row: (λ, -4λ, 1+6λ, -4λ, λ)
                //   i=0,n-1 edge: diag = 1+λ
                //   i=1,n-2 near-edge: diag = 1+5λ, off-by-1 = -2λ (toward the corner)
                // Stored as three diagonals:
                //   a0[i] = A[i][i],   a1[i] = A[i][i-1] (i≥1),   a2[i] = A[i][i-2] (i≥2).
                let mut a0 = vec![1.0 + 6.0 * lambda; n];
                let mut a1 = vec![-4.0 * lambda; n];
                let a2 = vec![lambda; n];
                a0[0] = 1.0 + lambda;
                a0[n - 1] = 1.0 + lambda;
                if n >= 3 {
                    a0[1] = 1.0 + 5.0 * lambda;
                    a0[n - 2] = 1.0 + 5.0 * lambda;
                    a1[1] = -2.0 * lambda;
                    a1[n - 1] = -2.0 * lambda;
                }

                // Symmetric pentadiagonal LDLᵀ factorization: A = L·D·Lᵀ,
                // L unit lower-triangular with bandwidth 2, D diagonal.
                //   l2[i] = L[i][i-2] = A[i][i-2] / D[i-2]
                //   l1[i] = L[i][i-1] = (A[i][i-1] - l2[i]·l1[i-1]·D[i-2]) / D[i-1]
                //   D[i]  = A[i][i] - l1[i]²·D[i-1] - l2[i]²·D[i-2]
                let mut d = vec![0.0f64; n];
                let mut l1f = vec![0.0f64; n];
                let mut l2f = vec![0.0f64; n];
                for ii in 0..n {
                    let l2i = if ii >= 2 { a2[ii] / d[ii - 2] } else { 0.0 };
                    let l1i = if ii >= 1 {
                        let cross = if ii >= 2 {
                            l2i * l1f[ii - 1] * d[ii - 2]
                        } else {
                            0.0
                        };
                        (a1[ii] - cross) / d[ii - 1]
                    } else {
                        0.0
                    };
                    let mut dii = a0[ii];
                    if ii >= 1 {
                        dii -= l1i * l1i * d[ii - 1];
                    }
                    if ii >= 2 {
                        dii -= l2i * l2i * d[ii - 2];
                    }
                    l1f[ii] = l1i;
                    l2f[ii] = l2i;
                    d[ii] = dii;
                }

                // Solve A·x = b for each axis: L·z=b, then y = z/D, then Lᵀ·x = y.
                let mut z = vec![0.0f64; n];
                let mut y = vec![0.0f64; n];
                let mut x = vec![0.0f64; n];
                for axis in 0..3 {
                    z[0] = est[0].1[axis];
                    if n >= 2 {
                        z[1] = est[1].1[axis] - l1f[1] * z[0];
                    }
                    for ii in 2..n {
                        z[ii] = est[ii].1[axis] - l1f[ii] * z[ii - 1] - l2f[ii] * z[ii - 2];
                    }
                    for ii in 0..n {
                        y[ii] = z[ii] / d[ii];
                    }
                    x[n - 1] = y[n - 1];
                    if n >= 2 {
                        x[n - 2] = y[n - 2] - l1f[n - 1] * x[n - 1];
                    }
                    if n >= 3 {
                        for ii in (0..=n - 3).rev() {
                            x[ii] = y[ii] - l1f[ii + 1] * x[ii + 1] - l2f[ii + 2] * x[ii + 2];
                        }
                    }
                    for ii in 0..n {
                        est[ii].1[axis] = x[ii];
                    }
                }
                tik_ns = tik_t0.elapsed().as_nanos() as u64;
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
            raw_pairs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            if raw_pairs.len() < 10 {
                continue;
            }

            let from_ms = from_us as f64 / 1000.0;
            let to_ms = to_us as f64 / 1000.0;
            let initial_offset = self.sync_params.initial_offset;
            let rs_argmin_ms = cost_final_ext_ms; // full_sync's external offset

            // Scan rs-sync cost curve (5ms step) to get best_offs + ratio (local use;
            // also writes to sync_diag output when diag is enabled)
            let cost_scan_t0 = std::time::Instant::now();
            let (rs_best_offs, rs_2nd_over_best) =
                self.scan_cost_curve_per_seg(i, sp_from, sp_to, cost_final_ext_ms);
            cost_scan_ns = cost_scan_t0.elapsed().as_nanos() as u64;

            // ── Path 0: Motion-too-weak early exit ──────────────────────
            // (max_axis_angle_deg computed earlier, before smoothing)
            if max_axis_angle_deg < MIN_AXIS_ANGLE_DEG {
                log::warn!(
                    "[ncc-fuse] seg {}: motion too weak (max |ω|={:.4} < {}), fallback initial",
                    i,
                    max_axis_angle_deg,
                    MIN_AXIS_ANGLE_DEG
                );
                offsets[i] = (mid_ms, initial_offset, f64::INFINITY, 0.0);
                crate::synchronization::sync_diag::record_fusion_decision(
                    i,
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    f64::NAN,
                    cost_final_ext_ms,
                    initial_offset,
                    f64::INFINITY,
                    rs_argmin_ms,
                    rs_2nd_over_best,
                    f64::NAN,
                    "fallback_initial",
                    Some("motion_too_weak"),
                );
                continue;
            }

            // ── Path 0: NCC FFT localization ────────────────────────────
            let ncc_fft_t0 = std::time::Instant::now();
            let ncc_res = {
                let _g_ncc = crate::synchronization::sync_perf::StageGuard::new(
                    crate::synchronization::sync_perf::Stage::NccFftAlign,
                );
                crate::synchronization::sync_diag::ncc_fft_align(
                    &est,
                    &raw_pairs,
                    from_ms,
                    to_ms,
                    self.sync_params.search_size,
                )
            };
            ncc_fft_ns = ncc_fft_t0.elapsed().as_nanos() as u64;
            let ncc = match ncc_res {
                Some(r) => r,
                None => {
                    log::warn!(
                        "[ncc-fuse] seg {}: ncc_fft_align returned None, fallback initial",
                        i
                    );
                    offsets[i] = (mid_ms, initial_offset, f64::INFINITY, 0.0);
                    crate::synchronization::sync_diag::record_fusion_decision(
                        i,
                        f64::NAN,
                        f64::NAN,
                        f64::NAN,
                        f64::NAN,
                        f64::NAN,
                        cost_final_ext_ms,
                        initial_offset,
                        f64::INFINITY,
                        rs_argmin_ms,
                        rs_2nd_over_best,
                        f64::NAN,
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
            let _sigma_ncc_ms = if fwhm_ms.is_finite() && fwhm_ms > 0.0 && peak_h > 0.0 {
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
                    i,
                    reason,
                    peak_h,
                    w_ms,
                    r2
                );
            }

            // ═══ V2: Scene-adaptive signal fusion ════════════════════════════
            // 3 candidate positions with Pearson-r reliability multipliers.
            // Each candidate's weight = scene_feature × Pearson_r_at_position.
            // Pearson is computed as a SINGLE POINT per candidate (~10µs each);
            // no full-curve scan → cost is negligible (<0.1ms/segment).
            //
            // Signals:
            //   rs_argmin     — LBFGS cost minimum
            //   rs_best_offs  — 5ms-step cost scan argmin
            //   ncc_peak      — NCC FFT peak (known edge-ghost bug, penalized
            //                   when peak is far from initial_offset)
            //
            // 1D clustering + weighted mean → pre_sync 0.1ms refine.
            const NEAREST_TOL_MS_V2: f64 = 10.0;
            const CLUSTER_MERGE_MS: f64 = 30.0;

            let pearson_at = |offset_ms: f64| -> f64 {
                if !offset_ms.is_finite() {
                    return 0.0;
                }
                let (_, _, _, r, n) =
                    crate::synchronization::sync_diag::compute_triaxis_correlation(
                        &est,
                        &raw_pairs,
                        offset_ms,
                        NEAREST_TOL_MS_V2,
                    );
                if n >= 10 && r.is_finite() { r } else { 0.0 }
            };
            let r_at_rs_argmin = pearson_at(rs_argmin_ms);
            let r_at_rs_best = pearson_at(rs_best_offs);
            let r_at_ncc_peak = pearson_at(ncc_peak_ms);

            // Scene-adaptive base weights.
            // cost_sharpness: (ratio-1)*50 clamped [0,1] — rs signals meaningful
            // when cost landscape has a distinguishable basin (ratio>1.02 → >1.0).
            let cost_sharpness = ((rs_2nd_over_best - 1.0) * 50.0).clamp(0.0, 1.0);
            // NCC edge penalty: FFT cross-correlation has a known bug where
            // shifts near search_radius edge produce artificial peaks (normalized
            // by full-segment energy but with minimal overlap). Penalize NCC
            // weight when |ncc_peak - initial_offset| approaches search_radius.
            let tau_ratio =
                (ncc_peak_ms - initial_offset).abs() / self.sync_params.search_size.max(1.0);
            let ncc_edge_penalty = (1.0 - 2.0 * tau_ratio).clamp(0.0, 1.0);

            let w_rs = cost_sharpness * r_at_rs_argmin.max(0.0);
            let w_rs_cost = cost_sharpness * 0.8 * r_at_rs_best.max(0.0);
            let w_ncc = peak_h * (1.0 - r2).max(0.0) * ncc_edge_penalty * r_at_ncc_peak.max(0.0);

            // Gather candidates with non-negligible weight.
            let mut cand: Vec<(f64, f64, &'static str)> = Vec::new();
            if w_rs > 1e-6 && rs_argmin_ms.is_finite() {
                cand.push((rs_argmin_ms, w_rs, "rs_argmin"));
            }
            if w_rs_cost > 1e-6 && rs_best_offs.is_finite() {
                cand.push((rs_best_offs, w_rs_cost, "rs_best_offs"));
            }
            if w_ncc > 1e-6 && ncc_peak_ms.is_finite() {
                cand.push((ncc_peak_ms, w_ncc, "ncc_peak"));
            }

            // ═══ Pearson curve argmax (4th candidate) ════════════════════════
            // Scan Pearson r across full search window (5ms step, ~1200 points,
            // ~5-10ms total). Pearson is 1st-order sensitive to delay (direct
            // est_gyro vs raw_gyro shape match) → in many scenarios gives a
            // more stable argmax than NCC (edge-ghost prone) or cost (flat).
            // Env var GYROFLOW_SYNC_NO_PEARSON_CANDIDATE=1 disables for rollback.
            let pearson_candidate_enabled = std::env::var("GYROFLOW_SYNC_NO_PEARSON_CANDIDATE")
                .map(|v| !matches!(v.trim(), "1" | "true" | "yes" | "on"))
                .unwrap_or(true);

            let mut pearson_peak_ms = f64::NAN;
            let mut pearson_peak_r = 0.0f64;
            let mut pearson_prominence = 0.0f64;
            let mut pearson_second_r = 0.0f64;
            let mut w_pearson_peak = 0.0f64;

            if pearson_candidate_enabled {
                let _g_ps = crate::synchronization::sync_perf::StageGuard::new(
                    crate::synchronization::sync_perf::Stage::NccPearsonScan,
                );
                let pearson_t0 = std::time::Instant::now();
                const PEARSON_SCAN_STEP_MS: f64 = 5.0;
                // Second peak must be >= 200ms away to count as a real alternate
                // basin (typical Pearson plateau around the true peak is 100-150ms
                // wide; within that is just the same mode, not multi-modal).
                const SECOND_PEAK_MIN_GAP_MS: f64 = 200.0;

                let scan_radius = self.sync_params.search_size;
                let n_steps = ((scan_radius * 2.0) / PEARSON_SCAN_STEP_MS) as i32;

                let mut samples: Vec<(f64, f64)> = Vec::with_capacity((n_steps + 1) as usize);
                for k in 0..=n_steps {
                    let cand_ms = initial_offset - scan_radius + (k as f64) * PEARSON_SCAN_STEP_MS;
                    let r = pearson_at(cand_ms);
                    if r.is_finite() {
                        samples.push((cand_ms, r));
                    }
                }
                if !samples.is_empty() {
                    // peak
                    let (pk_ms, pk_r) =
                        samples
                            .iter()
                            .cloned()
                            .fold((f64::NAN, f64::NEG_INFINITY), |acc, x| {
                                if x.1 > acc.1 { x } else { acc }
                            });
                    // Parabolic 3-point interpolation for sub-grid peak precision
                    // (P1 refinement). Pearson curve around true peak is locally
                    // quadratic; fit y = a(x-x0)² + y0 using r(k-1), r(k), r(k+1).
                    // Fallback to bin center if peak is on window edge or neighbors
                    // are not lower (not a true interior peak).
                    let peak_idx = samples.iter().position(|&(m, _)| m == pk_ms);
                    let refined_pk_ms = match peak_idx {
                        Some(idx) if idx > 0 && idx < samples.len() - 1 => {
                            let r_prev = samples[idx - 1].1;
                            let r_next = samples[idx + 1].1;
                            let dr_left = r_prev - pk_r;
                            let dr_right = r_next - pk_r;
                            let denom = dr_left + dr_right;
                            if denom < -1e-9 {
                                // Both neighbors lower (real interior peak)
                                let frac = 0.5 * (dr_left - dr_right) / denom;
                                // Clamp fractional shift to [-1, +1] bin
                                let frac = frac.clamp(-1.0, 1.0);
                                pk_ms + frac * PEARSON_SCAN_STEP_MS
                            } else {
                                pk_ms
                            }
                        }
                        _ => pk_ms,
                    };
                    pearson_peak_ms = refined_pk_ms;
                    pearson_peak_r = pk_r;
                    // median r
                    let mut rs: Vec<f64> = samples.iter().map(|x| x.1).collect();
                    rs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let median_r = rs[rs.len() / 2];
                    pearson_prominence = (pk_r - median_r).max(0.0);
                    // second peak (>= SECOND_PEAK_MIN_GAP_MS away from peak)
                    pearson_second_r = samples
                        .iter()
                        .filter(|x| (x.0 - pk_ms).abs() >= SECOND_PEAK_MIN_GAP_MS)
                        .map(|x| x.1)
                        .fold(f64::NEG_INFINITY, f64::max);
                    if !pearson_second_r.is_finite() {
                        pearson_second_r = 0.0;
                    }
                }

                // Scene-adaptive weight for Pearson peak.
                // Range ~[0, 1.5]: can exceed 1 when prominence is strong,
                // reflecting Pearson's first-order delay sensitivity advantage
                // over 2nd-order rs-sync cost.
                if pearson_peak_r > 0.0 && pearson_peak_ms.is_finite() {
                    let prominence_factor = (pearson_prominence / 0.15).max(0.0).powf(1.5).min(1.5);
                    let est_len_clamped = est.len().min(60).max(10) as f64;
                    // Use the same n_paired as single-point pearson (close enough; scan
                    // samples have same n since est+raw bounds are identical).
                    let n_factor = 1.0; // accept scan samples as full-n (est and raw overlap fully in window)
                    let _ = est_len_clamped;
                    // Lower motion gate: even weak-motion sync ranges give
                    // reliable Pearson peaks (the shape match exists regardless
                    // of motion magnitude). Floor 0.3 prevents over-penalty.
                    let motion_factor = (max_axis_angle_deg / 0.15).clamp(0.3, 1.0);
                    let unimodal_factor = if pearson_second_r >= 0.85 * pearson_peak_r {
                        0.0
                    } else {
                        let ratio = (pearson_second_r / pearson_peak_r).max(0.0);
                        (1.0 - (ratio - 0.5).max(0.0) * 2.0).clamp(0.0, 1.0)
                    };
                    w_pearson_peak = pearson_peak_r
                        * prominence_factor
                        * n_factor
                        * motion_factor
                        * unimodal_factor;
                }

                if w_pearson_peak > 1e-6 && pearson_peak_ms.is_finite() {
                    cand.push((pearson_peak_ms, w_pearson_peak, "pearson_peak"));
                }

                // Diagnostic log: factors contributing to w_pearson_peak
                let prom_f = (pearson_prominence / 0.15).max(0.0).powf(1.5).min(1.5);
                let mot_f = (max_axis_angle_deg / 0.3).clamp(0.0, 1.0);
                let uni_f = if pearson_second_r >= 0.85 * pearson_peak_r {
                    0.0
                } else {
                    let ratio = (pearson_second_r / pearson_peak_r.max(1e-9)).max(0.0);
                    (1.0 - (ratio - 0.5).max(0.0) * 2.0).clamp(0.0, 1.0)
                };
                log::info!(
                    "[pearson-scan] seg {}: peak={:.1}ms r={:.3} 2nd_r={:.3} prom={:.3} (factors: prom={:.2} mot={:.2} uni={:.2} | max_axis_angle={:.3}°) → w_pearson={:.3}",
                    i,
                    pearson_peak_ms,
                    pearson_peak_r,
                    pearson_second_r,
                    pearson_prominence,
                    prom_f,
                    mot_f,
                    uni_f,
                    max_axis_angle_deg,
                    w_pearson_peak
                );
                pearson_scan_ns = pearson_t0.elapsed().as_nanos() as u64;
            }

            // 1D clustering (greedy, merge if gap to running cluster mean < threshold).
            cand.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let mut clusters: Vec<Vec<(f64, f64, &'static str)>> = Vec::new();
            for c in &cand {
                let push_new = match clusters.last() {
                    Some(last) => {
                        let wsum: f64 = last.iter().map(|x| x.1).sum();
                        let mean: f64 = if wsum > 1e-9 {
                            last.iter().map(|x| x.0 * x.1).sum::<f64>() / wsum
                        } else {
                            last[0].0
                        };
                        (c.0 - mean).abs() >= CLUSTER_MERGE_MS
                    }
                    None => true,
                };
                if push_new {
                    clusters.push(vec![*c]);
                } else {
                    clusters.last_mut().unwrap().push(*c);
                }
            }

            // Pick best cluster (max total weight).
            let (coarse_ms, cluster_weight, cluster_signals) =
                match clusters.iter().max_by(|a, b| {
                    let wa: f64 = a.iter().map(|x| x.1).sum();
                    let wb: f64 = b.iter().map(|x| x.1).sum();
                    wa.partial_cmp(&wb).unwrap_or(std::cmp::Ordering::Equal)
                }) {
                    Some(c) if !c.is_empty() => {
                        let w_sum: f64 = c.iter().map(|x| x.1).sum();
                        let coarse: f64 = c.iter().map(|x| x.0 * x.1).sum::<f64>() / w_sum;
                        let signals = c.iter().map(|x| x.2).collect::<Vec<_>>().join("+");
                        (coarse, w_sum, signals)
                    }
                    _ => {
                        // No usable signal (all weights near zero).
                        // Fallback: prefer NCC peak if it's at least finite, else initial.
                        let fallback = if ncc_peak_ms.is_finite() {
                            ncc_peak_ms
                        } else {
                            initial_offset
                        };
                        (fallback, 0.0, "fallback".to_string())
                    }
                };

            // Output = coarse (weighted cluster centroid). No 0.5ms Pearson refine:
            // empirically the 0.5ms scan introduces interpolation noise that shifts
            // the apparent argmax by 5-8ms systematically to one side (observed:
            // coarse consistently within ±2ms of truth, refine systematically +5-7ms
            // off). Cluster coarse is more stable.
            let total_weight_pre: f64 = cand.iter().map(|x| x.1).sum();
            let cluster_frac_pre = if total_weight_pre > 1e-9 {
                cluster_weight / total_weight_pre
            } else {
                0.0
            };
            // Shortcut: when all 4 candidates unanimously cluster (cfrac≈1.0) and
            // rs_argmin Pearson correlation is high, rs_argmin (LBFGS f64) is the
            // highest-precision estimate — centroiding with grid-quantized
            // candidates only dilutes it. Guard with a unimodality check: when
            // Pearson's 2nd peak is close to the main peak (multi-basin cost
            // surface), LBFGS may have converged to the wrong basin — V2 centroid
            // is safer in that case. Fall back whenever unanimity breaks, signal
            // is weak, or the cost surface is multi-modal.
            let r_rs_for_shortcut = pearson_at(rs_argmin_ms);
            let unimodal_ok = pearson_peak_r > 1e-9 && pearson_second_r < 0.7 * pearson_peak_r;
            let use_rs_shortcut = quality_warn.is_none()
                && cluster_frac_pre >= 0.999
                && r_rs_for_shortcut.is_finite()
                && r_rs_for_shortcut > 0.85
                && rs_argmin_ms.is_finite()
                && unimodal_ok
                && (rs_argmin_ms - coarse_ms).abs() < CLUSTER_MERGE_MS;
            let output_ms = if use_rs_shortcut {
                rs_argmin_ms
            } else {
                coarse_ms
            };
            let best_r_refined = pearson_at(output_ms);
            let refine_ok = best_r_refined.is_finite() && best_r_refined > 0.0;

            // Diagnostic: cost at output position (pre_sync 0.1ms step in ±1ms)
            let center_internal_s = -(output_ms + frt_offset_ms) / 1000.0;
            let diag_radius_s = 0.001_f64.max(FINE_STEP_S * 2.0);
            let output_pre_sync_t0 = std::time::Instant::now();
            let output_cost = {
                let _g_ops = crate::synchronization::sync_perf::StageGuard::new(
                    crate::synchronization::sync_perf::Stage::NccOutputPreSync,
                );
                self.sync
                    .pre_sync(
                        center_internal_s,
                        sp_from,
                        sp_to,
                        FINE_STEP_S,
                        diag_radius_s,
                    )
                    .map(|(c, _)| c)
                    .unwrap_or(f64::NAN)
            };
            output_pre_sync_ns = output_pre_sync_t0.elapsed().as_nanos() as u64;

            // Confidence: cluster_fraction × max_pearson_in_cluster, with
            // quality_warn / refine_failed clamped to low confidence for UI filter.
            // 4-candidate unanimous bonus: when all 4 candidates (rs_argmin,
            // rs_best_offs, ncc_peak, pearson_peak) cluster within 30ms, the
            // output is highly trustworthy even if best_r is moderate (e.g.
            // long-focal weak-signal segments where r naturally caps ~0.35).
            // Lift conf above 0.4 filter threshold to avoid mis-dropping.
            let cluster_frac = cluster_frac_pre;
            let confidence = if quality_warn.is_some() || !refine_ok {
                peak_h.min(0.2).max(0.05)
            } else {
                // Use Pearson r at the refined output (most direct signal-quality
                // measure), weighted by cluster agreement fraction.
                let base = cluster_frac * best_r_refined;
                let unanimous_bonus = if cluster_frac >= 0.95 { 0.15 } else { 0.0 };
                (base + unanimous_bonus).clamp(0.05, 1.0)
            };

            let path_str_owned = if use_rs_shortcut {
                format!("v2_consensus[{}]|rs_shortcut", cluster_signals)
            } else {
                format!("v2_consensus[{}]", cluster_signals)
            };
            offsets[i] = (mid_ms, output_ms, output_cost, confidence);

            log::info!(
                "[ncc-fuse] seg {}: {} coarse={:.1}ms → output={:.1}ms r={:.3} (r_rs={:.3}/{:.3}, r_ncc={:.3}, pearson_peak={:.1}ms r={:.3} prom={:.3}, w=[rs={:.3}/rs_cost={:.3}/ncc={:.3}/p={:.3}], cfrac={:.2}, conf={:.3})",
                i,
                path_str_owned,
                coarse_ms,
                output_ms,
                best_r_refined,
                r_at_rs_argmin,
                r_at_rs_best,
                r_at_ncc_peak,
                pearson_peak_ms,
                pearson_peak_r,
                pearson_prominence,
                w_rs,
                w_rs_cost,
                w_ncc,
                w_pearson_peak,
                cluster_frac,
                confidence
            );

            let total_seg_ms = seg_t0.elapsed().as_secs_f64() * 1000.0;
            let accounted_ms =
                (tik_ns + cost_scan_ns + ncc_fft_ns + pearson_scan_ns + output_pre_sync_ns) as f64
                    / 1_000_000.0;
            let other_ms = (total_seg_ms - accounted_ms).max(0.0);
            log::info!(
                "[ncc-fuse-timing] seg {}: total={:.1}ms tikhonov={:.2}ms cost_scan={:.1}ms ncc_fft={:.2}ms pearson_scan={:.1}ms pre_sync={:.2}ms other={:.2}ms",
                i,
                total_seg_ms,
                tik_ns as f64 / 1_000_000.0,
                cost_scan_ns as f64 / 1_000_000.0,
                ncc_fft_ns as f64 / 1_000_000.0,
                pearson_scan_ns as f64 / 1_000_000.0,
                output_pre_sync_ns as f64 / 1_000_000.0,
                other_ms,
            );

            let combined_fb: Option<String> = match (quality_warn, refine_ok) {
                (Some(q), true) => Some(q.to_string()),
                (Some(q), false) => Some(format!("{}|refine_failed", q)),
                (None, false) => Some("refine_failed".to_string()),
                (None, true) => None,
            };
            crate::synchronization::sync_diag::record_fusion_decision(
                i,
                ncc_peak_ms,
                peak_h,
                fwhm_ms,
                w_ms,
                r2,
                cost_final_ext_ms,
                output_ms,
                output_cost,
                rs_argmin_ms,
                rs_2nd_over_best,
                output_ms,
                &path_str_owned,
                combined_fb.as_deref(),
            );
        }

        log::info!(
            "[ncc-fuse-timing] total: {} segments processed in {:.1}ms",
            offsets.len(),
            fuse_t0.elapsed().as_secs_f64() * 1000.0
        );
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

#[cfg(test)]
mod penta_solver_tests {
    // Verify pentadiagonal LDLᵀ solver matches a reference dense Gauss solver
    // on A = I + λ·LᵀL (same system used by Tikhonov est_gyro smoothing).
    fn solve_penta(n: usize, lambda: f64, b: &[f64]) -> Vec<f64> {
        let mut a0 = vec![1.0 + 6.0 * lambda; n];
        let mut a1 = vec![-4.0 * lambda; n];
        let a2 = vec![lambda; n];
        a0[0] = 1.0 + lambda;
        a0[n - 1] = 1.0 + lambda;
        if n >= 3 {
            a0[1] = 1.0 + 5.0 * lambda;
            a0[n - 2] = 1.0 + 5.0 * lambda;
            a1[1] = -2.0 * lambda;
            a1[n - 1] = -2.0 * lambda;
        }
        let mut d = vec![0.0f64; n];
        let mut l1f = vec![0.0f64; n];
        let mut l2f = vec![0.0f64; n];
        for i in 0..n {
            let l2i = if i >= 2 { a2[i] / d[i - 2] } else { 0.0 };
            let l1i = if i >= 1 {
                let cross = if i >= 2 {
                    l2i * l1f[i - 1] * d[i - 2]
                } else {
                    0.0
                };
                (a1[i] - cross) / d[i - 1]
            } else {
                0.0
            };
            let mut dii = a0[i];
            if i >= 1 {
                dii -= l1i * l1i * d[i - 1];
            }
            if i >= 2 {
                dii -= l2i * l2i * d[i - 2];
            }
            l1f[i] = l1i;
            l2f[i] = l2i;
            d[i] = dii;
        }
        let mut z = vec![0.0f64; n];
        z[0] = b[0];
        if n >= 2 {
            z[1] = b[1] - l1f[1] * z[0];
        }
        for i in 2..n {
            z[i] = b[i] - l1f[i] * z[i - 1] - l2f[i] * z[i - 2];
        }
        let mut y = vec![0.0f64; n];
        for i in 0..n {
            y[i] = z[i] / d[i];
        }
        let mut x = vec![0.0f64; n];
        x[n - 1] = y[n - 1];
        if n >= 2 {
            x[n - 2] = y[n - 2] - l1f[n - 1] * x[n - 1];
        }
        if n >= 3 {
            for i in (0..=n - 3).rev() {
                x[i] = y[i] - l1f[i + 1] * x[i + 1] - l2f[i + 2] * x[i + 2];
            }
        }
        x
    }

    fn solve_dense(n: usize, lambda: f64, b: &[f64]) -> Vec<f64> {
        let mut a = vec![vec![0.0f64; n]; n];
        for i in 0..n {
            a[i][i] = 1.0;
        }
        for k in 1..n - 1 {
            let idx = [k - 1, k, k + 1];
            let val = [1.0, -2.0, 1.0];
            for ii in 0..3 {
                for jj in 0..3 {
                    a[idx[ii]][idx[jj]] += lambda * val[ii] * val[jj];
                }
            }
        }
        let mut aug = a;
        let mut rhs = b.to_vec();
        for p in 0..n {
            let mut mr = p;
            let mut mv = aug[p][p].abs();
            for r in p + 1..n {
                if aug[r][p].abs() > mv {
                    mv = aug[r][p].abs();
                    mr = r;
                }
            }
            if mr != p {
                aug.swap(p, mr);
                rhs.swap(p, mr);
            }
            for r in p + 1..n {
                let f = aug[r][p] / aug[p][p];
                for c in p..n {
                    aug[r][c] -= f * aug[p][c];
                }
                rhs[r] -= f * rhs[p];
            }
        }
        let mut x = vec![0.0f64; n];
        for i in (0..n).rev() {
            let mut s = rhs[i];
            for j in i + 1..n {
                s -= aug[i][j] * x[j];
            }
            x[i] = s / aug[i][i];
        }
        x
    }

    #[test]
    fn penta_matches_dense() {
        // Deterministic pseudo-random b across sizes and λ values.
        for &n in &[5usize, 12, 60, 100] {
            for &lambda in &[0.1f64, 1.0, 5.0] {
                let mut b = Vec::with_capacity(n);
                let mut s: u64 = 0x9E3779B97F4A7C15;
                for _ in 0..n {
                    s = s
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    b.push(((s >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0);
                }
                let xp = solve_penta(n, lambda, &b);
                let xd = solve_dense(n, lambda, &b);
                let diff: f64 = xp
                    .iter()
                    .zip(&xd)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f64::max);
                assert!(diff < 1e-9, "n={} λ={} diff={}", n, lambda, diff);
            }
        }
    }
}
