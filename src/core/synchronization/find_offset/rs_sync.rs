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

/// Default cap on |rs_argmin - cluster centroid| for the rs_argmin shortcut.
/// The shortcut exists to recover the sub-grid precision the 5ms candidate
/// scan quantization (±2.5ms) takes away from the centroid, so a legitimate
/// correction is bounded by that scale. Override: `GYROFLOW_SYNC_RS_SHORTCUT_MAX_DEV_MS`
/// (set 30 to restore the pre-2026-06-10 CLUSTER_MERGE_MS-wide guard).
pub(super) const RS_SHORTCUT_MAX_DEV_MS_DEFAULT: f64 = 3.0;

/// Decide whether the rs_argmin shortcut may replace the cluster centroid.
///
/// A deviation beyond `max_dev_ms` means rs_argmin genuinely disagrees with
/// the consensus rather than refining its quantization — and on a flat
/// Pearson curve the absolute `r > 0.85` guard cannot tell the two apart
/// (observed 2026-06-10, C50 truth=0ms: r=0.851 at rs_argmin vs 0.853 at
/// peak let a +5.8ms parallax-biased argmin replace a +1.8ms consensus).
/// Free function so unit tests can exercise it without the fusion pipeline.
#[allow(clippy::too_many_arguments)]
pub(super) fn should_use_rs_shortcut(
    quality_warn_none: bool,
    cluster_frac_pre: f64,
    r_rs: f64,
    pearson_peak_r: f64,
    pearson_second_r: f64,
    rs_argmin_ms: f64,
    coarse_ms: f64,
    max_dev_ms: f64,
) -> bool {
    let unimodal_ok = pearson_peak_r > 1e-9 && pearson_second_r < 0.7 * pearson_peak_r;
    quality_warn_none
        && cluster_frac_pre >= 0.999
        && r_rs.is_finite()
        && r_rs > 0.85
        && rs_argmin_ms.is_finite()
        && unimodal_ok
        && (rs_argmin_ms - coarse_ms).abs() < max_dev_ms
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

/// Confidence path classification — emitted to `[ncc-fuse]` log line for
/// post-hoc traceability. Spec: `openspec/specs/find-offset-confidence/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfPath {
    Consensus,
    WarnFloor,
    PeriodicAmbiguity,
    Normal,
    LegacyCeiling,
    /// Chosen cluster was elevated by cross-segment prior decay (weak base
    /// candidates whose final weight only crossed the cluster-vote threshold
    /// because they sit close to `global_prior`). Confidence is clamped to
    /// [0.05, 0.5] so downstream rank filtering can distinguish "borrowed
    /// from anchor" from self-evidenced consensus.
    AnchorPrior,
    /// A near-twin local minimum (within ±TWIN_RADIUS_MS, near-equal cost,
    /// both valleys shallow) was detected next to the chosen one — picking
    /// between them is a coin flip (parallax/foreground contamination).
    /// Confidence is ceiled to 0.3 so the controller conf≥0.4 bypass and
    /// batch rank filter treat the point as unreliable. Applied AFTER all
    /// other paths as a ceiling, never changes the offset.
    /// Spec: `openspec/changes/sync-parallax-suppression/specs/find-offset-confidence/`.
    TwinAmbiguity,
}

impl ConfPath {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            ConfPath::Consensus => "consensus",
            ConfPath::WarnFloor => "warn_floor",
            ConfPath::PeriodicAmbiguity => "periodic_ambiguity",
            ConfPath::Normal => "normal",
            ConfPath::LegacyCeiling => "legacy_ceiling",
            ConfPath::AnchorPrior => "anchor_prior",
            ConfPath::TwinAmbiguity => "twin_ambiguity",
        }
    }
}

/// Compute final confidence scalar and path classification for a single sync
/// segment after v2 fusion.
///
/// Semantics (see `openspec/specs/find-offset-confidence/`): NCC `quality_warn`
/// is a confidence FLOOR (lower bound), not a CEILING (upper bound). When the
/// multi-estimator consensus signal `cluster_frac × max_pearson_r` is strong,
/// it lifts confidence above the NCC-derived warn_floor. The
/// `periodic_ambiguity` warning is the only quality_warn that retains ceiling
/// behavior — `r2 > 0.95` indicates true geometric ambiguity (cost surface has
/// two near-equal-height peaks), and consensus may all step into the wrong one.
///
/// Free function so unit tests can exercise it without constructing the full
/// fusion pipeline.
pub(super) fn decide_confidence(
    cluster_frac: f64,
    max_pearson_r: f64,
    best_r_refined: f64,
    peak_h: f64,
    quality_warn: Option<&str>,
    refine_ok: bool,
    legacy_ceiling: bool,
) -> (f64, ConfPath) {
    let warn_floor = peak_h.min(0.2).max(0.05);

    // Legacy escape hatch: full rollback to pre-floor ceiling behavior.
    if legacy_ceiling && (quality_warn.is_some() || !refine_ok) {
        return (warn_floor, ConfPath::LegacyCeiling);
    }

    // Geometric-ambiguity exception: keep ceiling so consensus can't all
    // step into the same wrong cost-surface peak — UNLESS Pearson signal is
    // strong. r2 ambiguity is computed on NCC's cost surface; when Pearson
    // peak is independently strong (max_pearson_r >= 0.6), it's an
    // independent rotation-correlation signal not affected by NCC's
    // geometric ambiguity, so fall through to consensus path instead of
    // hard-ceiling conf. Previously this hard ceiling dropped conf to 0.2
    // for low-motion cases where Pearson was the dominant deciding signal,
    // causing Auto Sync to silently filter out a correct offset.
    if quality_warn == Some("periodic_ambiguity") && max_pearson_r < 0.6 {
        return (warn_floor, ConfPath::PeriodicAmbiguity);
    }

    if quality_warn.is_some() || !refine_ok {
        let consensus_conf = if cluster_frac.is_finite() && max_pearson_r.is_finite() {
            (cluster_frac * max_pearson_r).clamp(0.0, 1.0)
        } else {
            0.0
        };
        if consensus_conf > warn_floor {
            return (consensus_conf.clamp(0.05, 1.0), ConfPath::Consensus);
        }
        return (warn_floor, ConfPath::WarnFloor);
    }

    // Normal path (unchanged): cluster_frac × best_r_refined + unanimous bonus.
    // Note: this branch's "救场" role for long-focal/weak-signal segments is
    // now also covered by the floor path above; the unanimous_bonus here only
    // applies when NCC quality is acceptable in the first place.
    let base = cluster_frac * best_r_refined;
    let unanimous_bonus = if cluster_frac >= 0.95 { 0.15 } else { 0.0 };
    ((base + unanimous_bonus).clamp(0.05, 1.0), ConfPath::Normal)
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
                    // Sharpness-aware fallback (Step A of sync-fusion-sharpness-and-cross-prior):
                    // Prefer the local minimum with the highest sharpness (= depth / width)
                    // over the absolute cost-min. C50 case study: cost-min picks a wide
                    // shallow basin at +2889ms while the true value is a sharp narrow
                    // valley at -950ms — sharpness 0.77 vs 0.05 (15× higher).
                    //
                    // Env vars:
                    //   GYROFLOW_SYNC_SHARPNESS_GRID=0       → bypass, revert to cost-min
                    //   GYROFLOW_SYNC_DEPTH_GATE=<f>         → depth gate (default 0.30 of baseline_p75)
                    let sharpness_mode = std::env::var("GYROFLOW_SYNC_SHARPNESS_GRID")
                        .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
                        .unwrap_or(true);
                    let depth_gate_ratio: f64 = std::env::var("GYROFLOW_SYNC_DEPTH_GATE")
                        .ok()
                        .and_then(|v| v.trim().parse::<f64>().ok())
                        .unwrap_or(0.30);

                    // Convert curve to external convention so sync_metric works in
                    // external offset space (matching `initial_offset` and the output).
                    // Source curve is Vec<(cost, delay_s)>; external offset =
                    //   -delay_s * 1000 - frt/2.
                    let frt_offset_ms = self.frame_readout_time * 1000.0 / 2.0;
                    let initial_offset_ext = self.sync_params.initial_offset;
                    let curve_external: Vec<(f64, f64)> = curve
                        .iter()
                        .filter(|(c, _)| c.is_finite())
                        .map(|&(c, d_s)| (-d_s * 1000.0 - frt_offset_ms, c))
                        .collect();

                    let sharp_pick = if sharpness_mode {
                        crate::synchronization::sync_metric::sharpest_minimum_in_bound(
                            &curve_external,
                            initial_offset_ext,
                            bounded_max,
                            depth_gate_ratio,
                        )
                    } else {
                        None
                    };

                    if let Some(m) = sharp_pick {
                        log::info!(
                            "[rssync] sharpness fallback: offset={:.1}ms sharp={:.3} cost={:.2}",
                            m.offset_ms, m.sharpness, m.cost
                        );
                        final_offset_external_ms = m.offset_ms;
                        offsets.push((
                            (from_ts + to_ts) as f64 / 2.0 / 1000.0,
                            m.offset_ms,
                            m.cost,
                            0.5,
                        ));
                    } else {
                        // Legacy cost-min path. Triggered when sharpness mode is
                        // disabled, or no local minimum passed the depth gate, or the
                        // curve had no local minimum at all.
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
                            let final_offset = -b_offset - frt_offset_ms;
                            if sharpness_mode {
                                log::info!(
                                    "[rssync] cost-min fallback (no minima passed depth gate): offset={:.1}ms cost={:.4}",
                                    final_offset, b_cost
                                );
                            } else {
                                log::info!(
                                    "[rssync] grid fallback: offset={:.1}ms cost={:.4}",
                                    -b_offset, b_cost
                                );
                            }
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
                                -offset - frt_offset_ms;
                        }
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
        // Env: GYROFLOW_SYNC_CONF_OLD_CEILING=1 reverts to pre-floor ceiling
        // behavior in `decide_confidence`. Read once per fusion run.
        let legacy_ceiling = std::env::var("GYROFLOW_SYNC_CONF_OLD_CEILING")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
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

        // ═══ Per-fusion env vars (sync-fusion-candidate-sharpness-gate) ═══
        // Read once per fusion run. Per-segment behavior derives from these
        // booleans + the cached cost-curve minima built right below.
        let cand_sharpness_gate_enabled = std::env::var("GYROFLOW_SYNC_CAND_SHARPNESS_GATE")
            .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        let cost_sharp_boost_enabled = std::env::var("GYROFLOW_SYNC_COST_SHARP_BOOST")
            .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        // Step 5/6 (always-on cluster refinement) was reverted after observing
        // Run 2 regression: ±10ms pre_sync around cluster centroid jumped to a
        // neighbor cost-curve minimum (-936 instead of cluster -945, truth ~-949).
        // The pre-existing anchor_prior refinement (sync-fusion-sharpness-and-cross-prior)
        // is kept intact because it only fires when prior_dominated == true,
        // which is its built-in safety lock.
        let ncc_sharp_floor: f64 = std::env::var("GYROFLOW_SYNC_NCC_SHARP_FLOOR")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.2)
            .clamp(0.0, 1.0);
        let sharpness_gate_min_ref: f64 = std::env::var("GYROFLOW_SYNC_SHARPNESS_GATE_MIN_REF")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.05)
            .max(0.0);

        // Precompute per-segment external cost curves + local minima once,
        // shared by both the anchor pool (Phase A) and the main fusion loop
        // below. Avoids running find_local_minima twice per segment.
        // Each entry is (curve_external, max_sharp_ref, minima).
        use crate::synchronization::sync_metric::{
            find_local_minima as _find_local_minima, CostMinimum as _CostMinimum,
        };
        let curve_cache: Vec<(Vec<(f64, f64)>, f64, Vec<_CostMinimum>)> = (0..offsets.len())
            .map(|idx| {
                if idx >= self.presync_curves.len() {
                    return (Vec::new(), 0.0_f64, Vec::new());
                }
                let curve_internal = &self.presync_curves[idx];
                if curve_internal.is_empty() {
                    return (Vec::new(), 0.0_f64, Vec::new());
                }
                let curve_external: Vec<(f64, f64)> = curve_internal
                    .iter()
                    .filter(|(c, _)| c.is_finite())
                    .map(|&(c, d_s)| (-d_s * 1000.0 - frt_offset_ms, c))
                    .collect();
                let (_baseline, minima) = _find_local_minima(&curve_external, 0.05);
                let max_sharp_ref = minima
                    .iter()
                    .map(|m| m.sharpness)
                    .fold(0.0_f64, f64::max);
                (curve_external, max_sharp_ref, minima)
            })
            .collect();

        // ═══ Phase A (sync-fusion-sharpness-and-cross-prior) ═══════════════
        // Cross-segment anchor pool: collect strong-signal segments whose
        // (cost ≤ threshold) AND (sharpness ≥ gate) qualify as trusted
        // anchors. Their median becomes a global prior that decays weak
        // segments' off-target candidates in the main fusion loop below.
        //
        // Env vars (defaults per design.md D3/D4/D5):
        //   GYROFLOW_SYNC_CROSS_PRIOR=0       → bypass entirely (no prior)
        //   GYROFLOW_SYNC_ANCHOR_MIN_SHARP    → anchor sharpness gate (default 0.30)
        //   GYROFLOW_SYNC_PRIOR_SPAN_MAX      → max anchor pool span ms (default 100)
        //   GYROFLOW_SYNC_PRIOR_SIGMA         → decay σ ms (default 200), read per-segment
        let cross_prior_enabled = std::env::var("GYROFLOW_SYNC_CROSS_PRIOR")
            .map(|v| !matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(true);
        let anchor_min_sharp: f64 = std::env::var("GYROFLOW_SYNC_ANCHOR_MIN_SHARP")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(0.30);
        let prior_span_max: f64 = std::env::var("GYROFLOW_SYNC_PRIOR_SPAN_MAX")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(100.0);

        // anchors: (seg_idx, offset_ms, cost, sharpness_at_anchor)
        let mut anchors: Vec<(usize, f64, f64, f64)> = Vec::new();
        if cross_prior_enabled {
            for (idx, &(_mid, off, cost, _conf)) in offsets.iter().enumerate() {
                if !cost.is_finite() || cost > fusion_cost_threshold {
                    continue;
                }
                if idx >= curve_cache.len() {
                    continue;
                }
                let (_curve_external, _max_sharp, minima) = &curve_cache[idx];
                if minima.is_empty() {
                    continue;
                }
                // Nearest local minimum within 50ms of the LBFGS-converged offset.
                let mut best_sharp: Option<f64> = None;
                let mut best_dist = f64::INFINITY;
                for m in minima.iter() {
                    let d = (m.offset_ms - off).abs();
                    if d <= 50.0 && d < best_dist {
                        best_dist = d;
                        best_sharp = Some(m.sharpness);
                    }
                }
                if let Some(sharp) = best_sharp {
                    if sharp >= anchor_min_sharp {
                        anchors.push((idx, off, cost, sharp));
                    }
                }
            }
        }

        // Phase B: compute global_prior from the anchor pool.
        // - empty pool → None (main loop behaves as if cross-prior were off)
        // - 1 anchor  → None (single-segment self-reference is equivalent to
        //                trust-rs_argmin bypass — anchor IS the segment being
        //                refined, so anchor_prior just pulls fusion back to
        //                rs-sync's potentially-wrong-basin output. Real cross-
        //                prior value requires ≥2 anchors from independent segs.)
        // - ≥2 anchors → span check first; if span > prior_span_max give up,
        //                otherwise median of anchor offsets.
        let global_prior: Option<f64> = if !cross_prior_enabled || anchors.is_empty() {
            None
        } else if anchors.len() == 1 {
            log::info!(
                "[anchor-pool] single anchor (seg {} offset={:.1}ms) — ignored to avoid self-reference",
                anchors[0].0, anchors[0].1
            );
            None
        } else {
            let mut vals: Vec<f64> = anchors.iter().map(|a| a.1).collect();
            vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let span = vals.last().unwrap() - vals.first().unwrap();
            if span > prior_span_max {
                log::warn!(
                    "[anchor-pool] span {:.1}ms > {:.0}ms, no global prior",
                    span,
                    prior_span_max
                );
                None
            } else {
                Some(vals[vals.len() / 2])
            }
        };

        // Diagnostic line so post-hoc log triage can see anchor pool state.
        if !anchors.is_empty() {
            let brief: Vec<String> = anchors
                .iter()
                .map(|(i, off, cost, s)| {
                    format!("seg {}: {:.1}ms cost={:.2} sharp={:.2}", i, off, cost, s)
                })
                .collect();
            log::info!(
                "[anchor-pool] collected {} anchors: [{}] global_prior={:?}",
                anchors.len(),
                brief.join(", "),
                global_prior
            );
        } else if cross_prior_enabled {
            log::info!(
                "[anchor-pool] no anchors collected (no segment met cost ≤ {} and sharp ≥ {:.2})",
                fusion_cost_threshold,
                anchor_min_sharp
            );
        }

        // Per-fusion σ read once. Used by closures in the main loop.
        let sigma_prior: f64 = std::env::var("GYROFLOW_SYNC_PRIOR_SIGMA")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or(200.0);

        for i in 0..offsets.len() {
            // Reverted aaaa3e1f's cost ≤ fusion_cost_threshold bypass: rs-sync
            // cost surface in low-motion / rotation-dominated cases is not a
            // reliable basin indicator (wrong basin cost can be lower than true
            // basin). 4-candidate consensus must always run to provide rotation-
            // based correlation signals (NCC, Pearson) that recover the true
            // offset. Anchor-pool gate above still uses fusion_cost_threshold
            // for cross-prior anchor selection, which is correct usage.
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
                    None,
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
                        None,
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

            // ── Candidate position sharpness gate (sync-fusion-candidate-sharpness-gate) ──
            // Pre-scan this segment's cost curve once for local minima. Each
            // candidate's weight gets multiplied by a sharpness_factor based
            // on whether its position sits on a sharp valley (factor=1.0) or
            // out in a wide / featureless region (factor→0 or NCC floor).
            // Cached above so anchor pool + main loop share the scan.
            let (max_sharp_ref, minima_ref): (f64, &[_CostMinimum]) = if i < curve_cache.len() {
                let (_curve_ext, m, mins) = &curve_cache[i];
                (*m, mins.as_slice())
            } else {
                (0.0_f64, &[][..])
            };
            let gate_active = cand_sharpness_gate_enabled
                && max_sharp_ref > sharpness_gate_min_ref
                && !minima_ref.is_empty();
            if cand_sharpness_gate_enabled && !gate_active {
                log::info!(
                    "[ncc-fuse] seg {}: sharpness gate disabled (max_sharp={:.3} < {:.3})",
                    i,
                    max_sharp_ref,
                    sharpness_gate_min_ref
                );
            }
            let sharpness_at = |cand_ms: f64, floor: f64| -> f64 {
                if !gate_active {
                    return 1.0;
                }
                crate::synchronization::sync_metric::factor_from_minima(
                    minima_ref,
                    cand_ms,
                    20.0,
                    max_sharp_ref,
                    floor,
                )
            };

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
            let cost_sharpness_old = ((rs_2nd_over_best - 1.0) * 50.0).clamp(0.0, 1.0);
            // Candidate-position sharpness factors (per Step 1+3). Each one
            // multiplies the corresponding candidate's weight below.
            let sf_rs = sharpness_at(rs_argmin_ms, 0.0);
            let sf_rs_cost = sharpness_at(rs_best_offs, 0.0);
            // NCC peak is a correlation-based signal independent of rs-sync's
            // cost surface — rs cost sharpness at ncc_peak position has no
            // veto authority over NCC's reliability. Force sf=1.0 so NCC
            // weight reflects only its own signal quality (peak_h, r2,
            // ncc_edge_penalty, r_at_ncc_peak). Was: sharpness_at(ncc_peak_ms,
            // ncc_sharp_floor) which suppressed NCC to 0.01 in low-motion
            // cases where pearson_peak / ncc_peak are the only valid signals.
            let sf_ncc = 1.0;
            let _ = ncc_sharp_floor; // keep var for future re-enable via env
            // Step 4: cost_sharpness fallback. When the global rs cost basin
            // is flat (ratio≈1.0 → cost_sharpness_old≈0) but rs_argmin still
            // sits on a sharp local valley, the per-position sharpness lifts
            // cost_sharpness so the rs signals are not zeroed out.
            let cost_sharpness = if cost_sharp_boost_enabled {
                cost_sharpness_old.max(sf_rs)
            } else {
                cost_sharpness_old
            };
            // NCC edge penalty: FFT cross-correlation has a known bug where
            // shifts near search_radius edge produce artificial peaks (normalized
            // by full-segment energy but with minimal overlap). Penalize NCC
            // weight when |ncc_peak - initial_offset| approaches search_radius.
            let tau_ratio =
                (ncc_peak_ms - initial_offset).abs() / self.sync_params.search_size.max(1.0);
            let ncc_edge_penalty = (1.0 - 2.0 * tau_ratio).clamp(0.0, 1.0);

            let w_rs = cost_sharpness * r_at_rs_argmin.max(0.0) * sf_rs;
            let w_rs_cost = cost_sharpness * 0.8 * r_at_rs_best.max(0.0) * sf_rs_cost;
            let w_ncc =
                peak_h * (1.0 - r2).max(0.0) * ncc_edge_penalty * r_at_ncc_peak.max(0.0) * sf_ncc;

            // Phase C (sync-fusion-sharpness-and-cross-prior): every candidate's
            // base weight is multiplied by `prior_decay = exp(-d² / σ²)` where
            // d = |cand_ms - global_prior|. When `global_prior.is_none()` decay
            // is identically 1.0 → behavior matches pre-change byte-for-byte.
            let prior_decay_at = |c_ms: f64| -> f64 {
                match global_prior {
                    Some(p) if c_ms.is_finite() && p.is_finite() => {
                        (-(c_ms - p).powi(2) / (sigma_prior * sigma_prior)).exp()
                    }
                    _ => 1.0,
                }
            };

            // cand stores FINAL weight (= base × decay) for cluster voting.
            // cand_decomp tracks (base, decay) per source for the anchor_prior
            // classification step after cluster selection.
            let mut cand: Vec<(f64, f64, &'static str)> = Vec::new();
            let mut cand_decomp: std::collections::HashMap<&'static str, (f64, f64)> =
                std::collections::HashMap::new();

            let p_dec_rs = prior_decay_at(rs_argmin_ms);
            let w_rs_final = w_rs * p_dec_rs;
            if w_rs_final > 1e-6 && rs_argmin_ms.is_finite() {
                cand.push((rs_argmin_ms, w_rs_final, "rs_argmin"));
                cand_decomp.insert("rs_argmin", (w_rs, p_dec_rs));
            }

            let p_dec_rs_best = prior_decay_at(rs_best_offs);
            let w_rs_cost_final = w_rs_cost * p_dec_rs_best;
            if w_rs_cost_final > 1e-6 && rs_best_offs.is_finite() {
                cand.push((rs_best_offs, w_rs_cost_final, "rs_best_offs"));
                cand_decomp.insert("rs_best_offs", (w_rs_cost, p_dec_rs_best));
            }

            let p_dec_ncc = prior_decay_at(ncc_peak_ms);
            let w_ncc_final = w_ncc * p_dec_ncc;
            if w_ncc_final > 1e-6 && ncc_peak_ms.is_finite() {
                cand.push((ncc_peak_ms, w_ncc_final, "ncc_peak"));
                cand_decomp.insert("ncc_peak", (w_ncc, p_dec_ncc));
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
            let mut pearson_second_ms = f64::NAN; // Tracked for cross-prior Pearson 2nd-peak candidate
            let mut w_pearson_peak = 0.0f64;
            // Per-candidate sharpness factors for the Pearson candidates,
            // captured here so the [ncc-fuse] decision log can surface them.
            let mut sf_p: f64 = 1.0;
            let mut sf_p2: f64 = 1.0;

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
                    // second peak (>= SECOND_PEAK_MIN_GAP_MS away from peak) — capture
                    // both the offset and the r so cross-segment prior can use the
                    // 2nd peak as a refinement candidate (D7).
                    let (second_ms, second_r) = samples
                        .iter()
                        .filter(|x| (x.0 - pk_ms).abs() >= SECOND_PEAK_MIN_GAP_MS)
                        .copied()
                        .fold(
                            (f64::NAN, f64::NEG_INFINITY),
                            |acc, x| if x.1 > acc.1 { x } else { acc },
                        );
                    pearson_second_r = if second_r.is_finite() { second_r } else { 0.0 };
                    pearson_second_ms = second_ms;
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
                    // Pearson peak is a correlation-based signal independent of
                    // rs-sync cost surface — sf gate has no veto authority.
                    // Was: sharpness_at(pearson_peak_ms, 0.0) which dropped
                    // pearson_peak weight to 0.007 in low-motion cases where
                    // it's actually the only reliable signal (r=0.8, prom=0.8).
                    sf_p = 1.0;
                    w_pearson_peak = pearson_peak_r
                        * prominence_factor
                        * n_factor
                        * motion_factor
                        * unimodal_factor
                        * sf_p;
                }

                if w_pearson_peak > 1e-6 && pearson_peak_ms.is_finite() {
                    let p_dec_p = prior_decay_at(pearson_peak_ms);
                    let w_pearson_final = w_pearson_peak * p_dec_p;
                    if w_pearson_final > 1e-6 {
                        cand.push((pearson_peak_ms, w_pearson_final, "pearson_peak"));
                        cand_decomp.insert("pearson_peak", (w_pearson_peak, p_dec_p));
                    }
                }

                // Pearson 2nd-peak candidate (D7): only joins the vote when a
                // global prior is active (prior decay can lift a 2nd peak near
                // the prior over a far main peak — exactly the C50 seg 0 case).
                // Env var GYROFLOW_SYNC_PEARSON_SECOND_ALWAYS=1 forces it on
                // even without a prior (regression A/B comparison).
                let pearson_second_always = std::env::var("GYROFLOW_SYNC_PEARSON_SECOND_ALWAYS")
                    .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
                    .unwrap_or(false);
                let allow_second = pearson_second_always || global_prior.is_some();
                if allow_second
                    && pearson_second_ms.is_finite()
                    && pearson_second_r > 0.0
                    && pearson_peak_r > 1e-9
                    && pearson_second_r >= 0.6 * pearson_peak_r
                    && (pearson_second_ms - pearson_peak_ms).abs() >= SECOND_PEAK_MIN_GAP_MS
                {
                    sf_p2 = sharpness_at(pearson_second_ms, 0.0);
                    let w_second_base = w_pearson_peak * 0.5 * sf_p2;
                    let p_dec_second = prior_decay_at(pearson_second_ms);
                    let w_second_final = w_second_base * p_dec_second;
                    if w_second_final > 1e-6 {
                        cand.push((pearson_second_ms, w_second_final, "pearson_2nd"));
                        cand_decomp.insert("pearson_2nd", (w_second_base, p_dec_second));
                    }
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
            let shortcut_max_dev_ms: f64 = std::env::var("GYROFLOW_SYNC_RS_SHORTCUT_MAX_DEV_MS")
                .ok()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .filter(|v| v.is_finite() && *v > 0.0)
                .unwrap_or(RS_SHORTCUT_MAX_DEV_MS_DEFAULT);
            let use_rs_shortcut = should_use_rs_shortcut(
                quality_warn.is_none(),
                cluster_frac_pre,
                r_rs_for_shortcut,
                pearson_peak_r,
                pearson_second_r,
                rs_argmin_ms,
                coarse_ms,
                shortcut_max_dev_ms,
            );
            // Traceability for the tightened guard: emit when the distance cap
            // is the sole reason the shortcut no longer fires (it would have
            // fired under the old CLUSTER_MERGE_MS-wide rule).
            if !use_rs_shortcut
                && should_use_rs_shortcut(
                    quality_warn.is_none(),
                    cluster_frac_pre,
                    r_rs_for_shortcut,
                    pearson_peak_r,
                    pearson_second_r,
                    rs_argmin_ms,
                    coarse_ms,
                    CLUSTER_MERGE_MS,
                )
            {
                log::info!(
                    target: "sync",
                    "[ncc-fuse] seg {}: rs_shortcut suppressed: |rs_argmin({:.1}) - coarse({:.1})| = {:.1}ms >= max_dev {:.1}ms",
                    i,
                    rs_argmin_ms,
                    coarse_ms,
                    (rs_argmin_ms - coarse_ms).abs(),
                    shortcut_max_dev_ms
                );
            }
            let cluster_output_ms = if use_rs_shortcut {
                rs_argmin_ms
            } else {
                coarse_ms
            };

            // AnchorPrior detection + sub-grid refinement
            // (sync-fusion-sharpness-and-cross-prior).
            //
            // When the chosen cluster's candidates ALL have weak base weight
            // (< 0.3) AND substantial prior decay (> 0.5), the cluster won
            // solely because it sits near global_prior — its centroid is
            // grid-quantized to Pearson's 5ms scan step (±2.5ms quantization
            // error). The anchor segment's offset itself came from LBFGS f64
            // (sub-millisecond), so a `pre_sync` 0.1ms scan around the prior
            // recovers that precision for the weak segment.
            let chosen_sources: Vec<&str> = cluster_signals
                .split('+')
                .filter(|s| !s.is_empty() && *s != "fallback")
                .collect();
            let prior_dominated = global_prior.is_some()
                && !chosen_sources.is_empty()
                && chosen_sources.iter().all(|s| {
                    cand_decomp
                        .get(*s)
                        .map_or(false, |&(b, d)| b < 0.3 && d > 0.5)
                });

            // Pre-existing anchor_prior refinement (sync-fusion-sharpness-and-cross-prior).
            // Only fires when prior_dominated == true (cluster signals all weak +
            // sit near global_prior). Step 5/6 of sync-fusion-candidate-sharpness-gate
            // proposed extending this to always-on around cluster_centroid, but Run 2
            // regression showed the ±10ms window can jump to a neighbor cost-curve
            // minimum that's worse than the cluster centroid. Reverted; the general
            // cluster centroid is kept as-is (5ms Pearson grid + parabolic interp gives
            // sub-grid precision for the Pearson candidate already).
            let output_ms = if prior_dominated {
                if let Some(p) = global_prior {
                    let center_int_s = -(p + frt_offset_ms) / 1000.0;
                    let radius_int_s = (2.0 * sigma_prior) / 1000.0;
                    match self.sync.pre_sync(
                        center_int_s,
                        sp_from,
                        sp_to,
                        FINE_STEP_S,
                        radius_int_s,
                    ) {
                        Some((_c, d_s)) => {
                            let refined = -d_s * 1000.0 - frt_offset_ms;
                            log::info!(
                                "[ncc-fuse] seg {}: anchor_prior refine: cluster={:.2}ms → pre_sync_argmin={:.2}ms (center=prior={:.2}ms ±{:.0}ms)",
                                i,
                                cluster_output_ms,
                                refined,
                                p,
                                2.0 * sigma_prior
                            );
                            refined
                        }
                        None => cluster_output_ms,
                    }
                } else {
                    cluster_output_ms
                }
            } else {
                cluster_output_ms
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

            // Confidence: floor-not-ceiling — multi-estimator consensus
            // (`cluster_frac × max_pearson_r`) is the primary signal; NCC's
            // quality_warn only sets a lower bound (`peak_h.clamp(0.05, 0.2)`),
            // not an upper bound. `periodic_ambiguity` is the only quality_warn
            // that retains ceiling behavior (true geometric ambiguity).
            // See `openspec/specs/find-offset-confidence/` for the full contract.
            let cluster_frac = cluster_frac_pre;
            let max_pearson_r = [
                r_at_rs_argmin,
                r_at_rs_best,
                r_at_ncc_peak,
                pearson_peak_r,
                best_r_refined,
            ]
            .into_iter()
            .filter(|r| r.is_finite() && *r >= 0.0)
            .fold(0.0_f64, f64::max);
            let (raw_confidence, raw_path) = decide_confidence(
                cluster_frac,
                max_pearson_r,
                best_r_refined,
                peak_h,
                quality_warn,
                refine_ok,
                legacy_ceiling,
            );

            // AnchorPrior confidence clamp (prior_dominated computed above
            // alongside the precision refinement). Confidence ceiling 0.5
            // so sync_repair/rank can tell "borrowed from anchor" apart
            // from self-evidenced consensus (≥ 0.5 by construction).
            let (confidence, conf_path) = if prior_dominated {
                (raw_confidence.clamp(0.05, 0.5), ConfPath::AnchorPrior)
            } else {
                (raw_confidence, raw_path)
            };

            // ── Twin-minimum ambiguity guard (sync-parallax-suppression M3) ──
            // Applied last, as a pure confidence ceiling: a near-twin local
            // minimum within ±TWIN_RADIUS_MS with near-equal cost and shallow
            // sharpness means the fusion output is a coin flip between two
            // valleys (parallax-contaminated aggregate decision surface) —
            // periodic_ambiguity's second-peak search (min_sep = max(FWHM,
            // 50ms)) structurally cannot see this. Offset is never changed.
            let twin_params = super::twin_guard::params();
            let twin_info = if twin_params.enabled && !minima_ref.is_empty() {
                // Associate the fusion output with its nearest cost-curve
                // minimum (same 50ms association radius as the anchor pool)
                // to get the chosen valley's cost/sharpness.
                minima_ref
                    .iter()
                    .filter(|m| (m.offset_ms - output_ms).abs() <= 50.0)
                    .min_by(|a, b| {
                        (a.offset_ms - output_ms)
                            .abs()
                            .partial_cmp(&(b.offset_ms - output_ms).abs())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .and_then(|cm| {
                        super::twin_guard::detect_twin_minimum(
                            minima_ref,
                            cm.offset_ms,
                            cm.cost,
                            cm.sharpness,
                            &twin_params,
                        )
                    })
            } else {
                None
            };
            let (confidence, conf_path) = if twin_info.is_some() {
                (confidence.min(0.3), ConfPath::TwinAmbiguity)
            } else {
                (confidence, conf_path)
            };

            let path_str_owned = if use_rs_shortcut {
                format!("v2_consensus[{}]|rs_shortcut", cluster_signals)
            } else {
                format!("v2_consensus[{}]", cluster_signals)
            };
            offsets[i] = (mid_ms, output_ms, output_cost, confidence);

            // Prior-state tail for log traceability. Only emit when prior is
            // active to keep the no-prior log line byte-identical to the
            // pre-change format.
            let prior_tail = match global_prior {
                Some(p) => {
                    let chosen_decay: String = chosen_sources
                        .iter()
                        .filter_map(|s| {
                            cand_decomp.get(*s).map(|&(_, d)| format!("{}={:.3}", s, d))
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    format!(
                        ", anchor_pool={} global_prior={:.1}ms σ={:.0} prior_decay=[{}]",
                        anchors.len(),
                        p,
                        sigma_prior,
                        chosen_decay
                    )
                }
                None => String::new(),
            };
            // Twin-guard tail — empty when no twin so the log line stays
            // byte-identical to the pre-change format.
            let twin_tail = match &twin_info {
                Some(t) => format!(
                    ", twin=@{:.1}ms margin={:.1}% sharp=[{:.1}/{:.1}]",
                    t.offset_ms,
                    t.margin * 100.0,
                    t.sharp_chosen,
                    t.sharp_twin
                ),
                None => String::new(),
            };
            log::info!(
                "[ncc-fuse] seg {}: {} coarse={:.1}ms → output={:.1}ms r={:.3} (r_rs={:.3}/{:.3}, r_ncc={:.3}, pearson_peak={:.1}ms r={:.3} prom={:.3}, w=[rs={:.3}/rs_cost={:.3}/ncc={:.3}/p={:.3}], sf=[rs={:.2}/rs_cost={:.2}/ncc={:.2}/p={:.2}/p2={:.2}], cost_sharp={:.2} (old={:.2}, sf_rs={:.2}), cfrac={:.2}, max_r={:.3}, conf={:.3}, conf_path={}{}{})",
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
                sf_rs,
                sf_rs_cost,
                sf_ncc,
                sf_p,
                sf_p2,
                cost_sharpness,
                cost_sharpness_old,
                sf_rs,
                cluster_frac,
                max_pearson_r,
                confidence,
                conf_path.as_str(),
                prior_tail,
                twin_tail
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
                twin_info.map(|t| (t.offset_ms, t.margin, t.sharp_chosen.min(t.sharp_twin))),
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
mod rs_shortcut_tests {
    use super::{RS_SHORTCUT_MAX_DEV_MS_DEFAULT, should_use_rs_shortcut};

    /// Regression: 2026-06-10 C50 (truth 0ms) — flat Pearson curve, r=0.851
    /// at rs_argmin(+5.8) vs 0.853 at peak(+1.7), coarse consensus +1.8ms.
    /// Old 30ms-wide guard fired and replaced 1.8 with 5.8; the 3ms cap must not.
    #[test]
    fn flat_pearson_far_argmin_does_not_fire() {
        assert!(!should_use_rs_shortcut(
            true, 1.0, 0.851, 0.853, 0.3, 5.8, 1.8,
            RS_SHORTCUT_MAX_DEV_MS_DEFAULT
        ));
        // Same inputs under the old-width guard (env rollback) do fire.
        assert!(should_use_rs_shortcut(true, 1.0, 0.851, 0.853, 0.3, 5.8, 1.8, 30.0));
    }

    /// Legitimate quantization refinement: rs_argmin 1.5ms from the centroid
    /// (within the ±2.5ms scan quantization) keeps firing.
    #[test]
    fn close_argmin_still_fires() {
        assert!(should_use_rs_shortcut(
            true, 1.0, 0.90, 0.91, 0.3, 3.3, 1.8,
            RS_SHORTCUT_MAX_DEV_MS_DEFAULT
        ));
    }

    #[test]
    fn other_guards_unchanged() {
        // quality_warn present → no shortcut.
        assert!(!should_use_rs_shortcut(false, 1.0, 0.90, 0.91, 0.3, 2.0, 1.8, 3.0));
        // broken unanimity → no shortcut.
        assert!(!should_use_rs_shortcut(true, 0.8, 0.90, 0.91, 0.3, 2.0, 1.8, 3.0));
        // weak r at argmin → no shortcut.
        assert!(!should_use_rs_shortcut(true, 1.0, 0.70, 0.91, 0.3, 2.0, 1.8, 3.0));
        // multi-modal Pearson (2nd peak ≥ 0.7×main) → no shortcut.
        assert!(!should_use_rs_shortcut(true, 1.0, 0.90, 0.91, 0.80, 2.0, 1.8, 3.0));
        // non-finite argmin → no shortcut.
        assert!(!should_use_rs_shortcut(true, 1.0, 0.90, 0.91, 0.3, f64::NAN, 1.8, 3.0));
    }
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
