// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use super::super::{PoseEstimator, SyncParams};
use crate::filtering::Lowpass;
use crate::stabilization::ComputeParams;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering::Relaxed},
};

use crate::gyro_source::TimeIMU;

pub fn find_offsets<F: Fn(f64) + Send + Sync>(
    estimator: &PoseEstimator,
    ranges: &[(i64, i64)],
    sync_params: &SyncParams,
    params: &ComputeParams,
    progress_cb: F,
    cancel_flag: Arc<AtomicBool>,
) -> Vec<(f64, f64, f64, f64)> {
    // Vec<(timestamp, offset, cost, confidence)>
    // essential_matrix path: confidence placeholder 0.5 (no NCC, no natural confidence metric)
    let estimated_gyro = estimator.estimated_gyro.read().clone();

    let mut offsets = Vec::new();
    let gyro = params.gyro.read();
    let ranges_len = ranges.len() as f64;

    // rs-sync candidate-verification probe (diagnostic, see rssync_peak_probe).
    let probe_spec = std::env::var("GYROFLOW_RSSYNC_PROBE")
        .ok()
        .filter(|_| crate::synchronization::deep_match::is_armed());
    let mut probe_cache: Vec<(Vec<TimeIMU>, BTreeMap<usize, TimeIMU>)> = Vec::new();

    let raw_imu_len = gyro.raw_imu(&gyro.file_metadata.read()).len();

    if !estimated_gyro.is_empty() && gyro.duration_ms > 0.0 && raw_imu_len > 0 {
        // Deep-match top-K motion selection (design §3.2): when armed, rank
        // ranges by OF-estimated motion and fully scan only the K highest
        // (K = deep_match::scan_k_target()). Weak windows contribute ~0
        // evidence to the joint posterior, so scanning them is wasted work.
        // K = 0 (regular autosync, or POSTERIOR=0 left it 0) -> keep all ranges.
        let scan_keep: Option<std::collections::BTreeSet<usize>> = {
            let k = crate::synchronization::deep_match::scan_k_target();
            if k > 0 {
                let mut scored: Vec<(usize, f64)> = ranges
                    .iter()
                    .enumerate()
                    .filter_map(|(i, (from_ts, to_ts))| {
                        if to_ts <= from_ts {
                            return None;
                        }
                        let item: Vec<TimeIMU> =
                            estimated_gyro.range(from_ts..to_ts).map(|v| v.1.clone()).collect();
                        if item.is_empty() {
                            return None;
                        }
                        Some((i, get_max_angle(&item)))
                    })
                    .collect();
                scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                // BTreeSet iterates in sorted order, so the log line needs no
                // extra sort and lookups stay O(log n) (n <= 8 windows).
                let keep: std::collections::BTreeSet<usize> =
                    scored.into_iter().take(k).map(|(i, _)| i).collect();
                ::log::info!(
                    target: "sync",
                    "[deep-match] window select: ranges={} scan_k={} kept={:?}",
                    ranges.len(), k, keep.iter().copied().collect::<Vec<_>>()
                );
                Some(keep)
            } else {
                None
            }
        };
        for (i, (from_ts, to_ts)) in ranges.iter().enumerate() {
            if cancel_flag.load(Relaxed) {
                break;
            }
            // Skip non-kept windows before reporting progress so the bar
            // advances only on windows that actually run the scan.
            if let Some(ref keep) = scan_keep {
                if !keep.contains(&i) {
                    continue;
                }
            }
            progress_cb(i as f64 / ranges_len);
            if to_ts <= from_ts {
                continue;
            }

            let mut of_item: Vec<TimeIMU> = estimated_gyro
                .range(from_ts..to_ts)
                .map(|v| v.1.clone())
                .collect();
            if !of_item.is_empty() {
                let last_of_timestamp = of_item.last().map(|x| x.timestamp_ms).unwrap_or_default();
                let mut gyro_item: Vec<TimeIMU> = gyro
                    .raw_imu(&gyro.file_metadata.read())
                    .iter()
                    .filter_map(|x| {
                        let ts = x.timestamp_ms + sync_params.initial_offset;
                        if ts >= of_item[0].timestamp_ms - sync_params.search_size
                            && ts <= last_of_timestamp + sync_params.search_size
                        {
                            Some(x.clone())
                        } else {
                            None
                        }
                    })
                    .collect();

                let max_angle = get_max_angle(&of_item);
                // The gate reads OF-estimated rates, which scale by
                // f_true/f_assumed under approximate lens matrices — armed
                // deep-match probes run a lower economy floor (consistency
                // gate carries correctness there).
                let motion_gate = if crate::synchronization::deep_match::is_armed() {
                    crate::synchronization::deep_match::motion_gate_armed()
                } else {
                    3.0
                };
                if max_angle < motion_gate {
                    ::log::info!(
                        "No movement detected, max OF-estimated angle: {} (gate {}). Skipping sync point.",
                        max_angle,
                        motion_gate
                    );
                    // A chunk whose windows were ALL gated out here carries no
                    // information about the gyro data — it is a video-side
                    // motion verdict, not the "probe never ran" sentinel.
                    crate::synchronization::deep_match::record_window_gated();
                    continue;
                }

                let gyro_bintree: BTreeMap<usize, TimeIMU> = {
                    let _g = crate::synchronization::sync_perf::StageGuard::new(
                        crate::synchronization::sync_perf::Stage::FindOffPrep,
                    );
                    let sample_rate = raw_imu_len as f64 / (gyro.duration_ms / 1000.0);
                    let _ = Lowpass::filter_gyro_forward_backward(
                        20.0,
                        params.scaled_fps,
                        &mut of_item,
                    );
                    let _ =
                        Lowpass::filter_gyro_forward_backward(20.0, sample_rate, &mut gyro_item);
                    gyro_item
                        .into_iter()
                        .map(|x| ((x.timestamp_ms * 1000.0) as usize, x))
                        .collect()
                };

                // Keep the filtered pair for the rs-sync probe so the cheap
                // prefilter scores the exact same data the scan saw.
                if probe_spec.is_some() {
                    probe_cache.push((of_item.clone(), gyro_bintree.clone()));
                }

                let find_min =
                    |a: (f64, f64), b: (f64, f64)| -> (f64, f64) { if a.1 < b.1 { a } else { b } };

                // First search every 1 ms
                let steps = sync_params.search_size as usize * 2;
                let coarse_lowest = {
                    let _g = crate::synchronization::sync_perf::StageGuard::new(
                        crate::synchronization::sync_perf::Stage::FindOffCoarse,
                    );
                    // Whole-file scans (deep match) take seconds per window
                    // with no other progress source, which reads as a stuck
                    // bar in the UI — report sub-window progress from inside
                    // the scan, throttled to 1% buckets (~100 fires/window).
                    let scan_done = std::sync::atomic::AtomicUsize::new(0);
                    let scan_bucket = std::sync::atomic::AtomicUsize::new(0);
                    (0..steps)
                        .into_par_iter()
                        .map(|j| {
                            let offs =
                                sync_params.initial_offset - sync_params.search_size + (j as f64);
                            let cost = calculate_cost(offs, &of_item, &gyro_bintree);
                            let done = scan_done.fetch_add(1, Relaxed) + 1;
                            let bucket = done * 100 / steps.max(1);
                            if scan_bucket.fetch_max(bucket, Relaxed) < bucket {
                                progress_cb(
                                    (i as f64 + done as f64 / steps.max(1) as f64) / ranges_len,
                                );
                            }
                            (offs, cost)
                        })
                        .reduce_with(find_min)
                };
                let lowest = coarse_lowest.and_then(|lowest| {
                    let _g = crate::synchronization::sync_perf::StageGuard::new(
                        crate::synchronization::sync_perf::Stage::FindOffRefine,
                    );
                    // Then refine to 0.01 ms accuracy
                    let search_size = 2.0; // ms
                    let steps = (search_size * 100.0) as usize; // 100 times per ms
                    let step = search_size / steps as f64;
                    (0..steps)
                        .into_par_iter()
                        .map(|i| {
                            let offs = lowest.0 + (-search_size + (i as f64 * step));
                            (offs, calculate_cost(offs, &of_item, &gyro_bintree))
                        })
                        .reduce_with(find_min)
                });

                if let Some(lowest) = lowest {
                    // Counted OUTSIDE the bounds check below on purpose: this
                    // records that the scan produced an argmin, not that the
                    // argmin was accepted. A chunk where every argmin is
                    // bounds-rejected has still run, and must advance rather
                    // than terminate the whole probe plan.
                    crate::synchronization::deep_match::record_window_scanned();

                    let middle_timestamp =
                        (*from_ts as f64 + (to_ts - from_ts) as f64 / 2.0) / 1000.0;

                    // Only accept offsets that are within 90% of search size range
                    if (lowest.0 - sync_params.initial_offset).abs() < sync_params.search_size * 0.9
                    {
                        offsets.push((middle_timestamp, lowest.0, lowest.1, 0.5));
                        if crate::synchronization::deep_match::is_armed() {
                            use crate::synchronization::deep_match as dm;
                            if dm::posterior_enabled() {
                                // Full 25ms coarse curve over the search domain
                                // + 5ms densification within ±DENSE_MS of the
                                // refined argmin + the refined point itself
                                // (design §3.3). Fed to the joint posterior.
                                let step_ms = 25.0;
                                let n_steps = ((sync_params.search_size * 2.0) / step_ms) as usize;
                                let mut curve: Vec<(f64, f64)> = (0..n_steps)
                                    .into_par_iter()
                                    .map(|k| {
                                        let offs = sync_params.initial_offset
                                            - sync_params.search_size
                                            + k as f64 * step_ms;
                                        (offs, calculate_cost(offs, &of_item, &gyro_bintree))
                                    })
                                    .filter(|(_, c)| c.is_finite() && *c != f64::MAX)
                                    .collect();
                                let dense_r = dm::post_dense_ms();
                                let dense: Vec<(f64, f64)> = {
                                    // ~12 points — plain iterator; rayon dispatch
                                    // overhead would exceed the gain on a block
                                    // this small.
                                    let dn = ((dense_r * 2.0) / 5.0) as usize;
                                    (0..=dn)
                                        .map(|k| {
                                            let offs = lowest.0 - dense_r + k as f64 * 5.0;
                                            (offs, calculate_cost(offs, &of_item, &gyro_bintree))
                                        })
                                        .filter(|(_, c)| c.is_finite() && *c != f64::MAX)
                                        .collect()
                                };
                                curve.extend(dense);
                                curve.push((lowest.0, lowest.1));
                                dm::record_curve(dm::DeepMatchWindowCurve {
                                    range_idx: i,
                                    t_center_ms: middle_timestamp,
                                    argmin_ms: lowest.0,
                                    cost_min: lowest.1,
                                    n_eff: of_item.len() as f64,
                                    curve,
                                });
                                ::log::info!(
                                    target: "sync",
                                    "[deep-match] window {} offset={:.1}ms cost_min={:.2} n_eff={} max_angle={:.1} (posterior)",
                                    i, lowest.0, lowest.1, of_item.len(), max_angle
                                );
                            } else {
                                // Legacy path (POSTERIOR=0): decimated cost curve
                                // for valley-quality stats. 25ms step keeps this
                                // pass at ~2.5% of the 1ms main scan cost.
                                let step_ms = 25.0;
                                let n_steps =
                                    ((sync_params.search_size * 2.0) / step_ms) as usize;
                                let mut costs: Vec<f64> = (0..n_steps)
                                    .into_par_iter()
                                    .map(|k| {
                                        let offs = sync_params.initial_offset
                                            - sync_params.search_size
                                            + k as f64 * step_ms;
                                        calculate_cost(offs, &of_item, &gyro_bintree)
                                    })
                                    .filter(|c| c.is_finite() && *c != f64::MAX)
                                    .collect();
                                costs.sort_by(|a, b| {
                                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                                });
                                let p25 = costs
                                    .get(costs.len() / 4)
                                    .copied()
                                    .unwrap_or(f64::MAX);
                                crate::synchronization::deep_match::record(
                                    crate::synchronization::deep_match::DeepMatchSegStats {
                                        range_idx: i,
                                        offset_ms: lowest.0,
                                        cost_min: lowest.1,
                                        cost_p25: p25,
                                        max_angle,
                                    },
                                );
                                ::log::info!(
                                    target: "sync",
                                    "[deep-match] window {} offset={:.1}ms cost_min={:.2} p25={:.2} ratio={:.3} max_angle={:.1}",
                                    i, lowest.0, lowest.1, p25,
                                    if p25 > 0.0 { lowest.1 / p25 } else { f64::NAN },
                                    max_angle
                                );
                            }
                        }
                    } else {
                        log::warn!(
                            "Sync point out of acceptable range {} < {}",
                            (lowest.0 - sync_params.initial_offset).abs(),
                            sync_params.search_size * 0.9
                        );
                    }

                    if crate::synchronization::sync_diag::is_enabled() {
                        crate::synchronization::sync_diag::record_initial_offset_segment(
                            i,
                            lowest.0,
                            lowest.1,
                            max_angle,
                            of_item.len(),
                        );
                        // The full essmat curve is recomputed serially just for
                        // this dump (deep-match probes span millions of 1ms
                        // steps) — opt-in via GYROFLOW_SYNC_DIAG_ESSMAT so that
                        // residual-corpus recording does not stall the probe.
                        if crate::synchronization::sync_diag::essmat_curve_enabled() {
                            let cost_steps = (sync_params.search_size as usize) * 2;
                            let curve: Vec<(f64, f64)> = (0..cost_steps)
                                .map(|k| {
                                    let offs = sync_params.initial_offset
                                        - sync_params.search_size
                                        + (k as f64);
                                    (offs, calculate_cost(offs, &of_item, &gyro_bintree))
                                })
                                .collect();
                            crate::synchronization::sync_diag::record_cost_curve_essmat(i, &curve);
                        }
                        for (k, o) in of_item.iter().enumerate() {
                            if k % 10 != 0 {
                                continue;
                            }
                            let est = o.gyro.unwrap_or([0.0; 3]);
                            let raw = gyro_at_timestamp(o.timestamp_ms - lowest.0, &gyro_bintree)
                                .and_then(|g| g.gyro)
                                .unwrap_or([0.0; 3]);
                            crate::synchronization::sync_diag::record_estimated_vs_raw_gyro(
                                o.timestamp_ms,
                                est[0],
                                est[1],
                                est[2],
                                raw[0],
                                raw[1],
                                raw[2],
                            );
                        }
                    }
                }
            }
        }
    }

    // The main scan is done with the gyro store; release the read guard before
    // any rs-sync stage below re-acquires it (parking_lot read locks are not
    // recursion-safe when a writer is queued in between).
    drop(gyro);

    // Diagnostic: two-stage "essential proposes / rs-sync verifies" probe.
    // Enabled with GYROFLOW_RSSYNC_PROBE=<top_n>:<radius_ms>:<max_rssync>:
    // <fwd_radius_ms> while a deep-match run is armed. Takes the joint
    // posterior's top-N candidate peaks and re-scores each with rs-sync in a
    // local window, reporting valley quality and wall-clock cost per
    // candidate. Kept as a long-term calibration/comparison tool for the
    // production cascade below; its Pearson prefilter is comparison-only
    // (design D3: retired from the product path).
    if let Some(spec) = probe_spec.as_deref() {
        rssync_peak_probe(estimator, ranges, sync_params, params, spec, &cancel_flag, &probe_cache);
    }

    // Forward re-scoring cascade (change deep-match-forward-rescoring): when a
    // deep-match chunk scan is armed for it, the essential scan above is only
    // the candidate generator — the chunk decision is made by rs-sync forward
    // re-scoring plus a full-call confirmation, recorded into the deep_match
    // side channel for the chunk orchestration. The env kill-switch restores
    // the decide-directly-from-essential path byte-for-byte (no forward
    // scoring, no rs-sync problem assembled).
    {
        use crate::synchronization::deep_match as dm;
        if dm::forward_armed()
            && dm::forward_enabled()
            && dm::posterior_enabled()
            && !cancel_flag.load(Relaxed)
        {
            forward_rescore(estimator, ranges, sync_params, params, &cancel_flag);
        }
    }

    offsets
}

/// Forward re-scoring cascade (change deep-match-forward-rescoring):
/// joint-posterior top-N candidate extraction → one rs-sync spline build +
/// per-candidate local `pre_sync` grids → per-chunk relative noise-floor
/// criterion → full rs-sync confirmation of the forward-accepted top ranks.
///
/// Everything here is per-chunk closed: candidates, forward costs and floor
/// statistics are never carried across chunks (different chunks are different
/// gyro data — the same discipline as the cross-window `cost_min` ban).
fn forward_rescore(
    estimator: &PoseEstimator,
    ranges: &[(i64, i64)],
    sync_params: &SyncParams,
    params: &ComputeParams,
    cancel_flag: &Arc<AtomicBool>,
) {
    use crate::synchronization::deep_match as dm;
    let t_total = std::time::Instant::now();
    let curves = dm::peek_curves();
    if curves.len() < 2 {
        // Leave no outcome: fewer than 2 windows is pre-existing
        // TooFewWindows/ProbeNotRun territory and must stay that way.
        ::log::info!(
            target: "sync",
            "[deep-match] forward skipped: only {} window curve(s)",
            curves.len()
        );
        return;
    }
    // Fast path: the pre-existing posterior already accepts this chunk. Forward
    // re-scoring is confirm-only, so it could only agree with that — running it
    // would bolt seconds of rs-sync work onto a path that already succeeded.
    // Skipping keeps every previously-working clip exactly as fast as before.
    // Re-deciding here can differ from the queue-side verdict only through
    // `scaled_duration_ms` vs `duration_ms` (video speed), and either way the
    // outcome is unchanged: a spurious skip lands on the same Accepted verdict,
    // a spurious run merely costs time it cannot misuse.
    if matches!(
        // Re-decision over the curves this scan just produced (the guard above
        // returned already when there were too few), so the empty-chunk
        // classification is unreachable here.
        dm::decide_posterior(
            &curves,
            curves.len(),
            0,
            params.scaled_duration_ms,
            dm::post_conf_min(),
            dm::post_ci95_base_ms(),
            dm::drift_rate_ms_per_min(),
            dm::drift_floor_ms(),
        ),
        dm::DeepMatchVerdict::Accepted { .. }
    ) {
        ::log::info!(
            target: "sync",
            "[deep-match] forward skipped: posterior already accepts this chunk"
        );
        return;
    }

    let top_n = dm::fwd_top_n();
    let nms_ms = dm::fwd_nms_ms();
    let lattice_ms = dm::fwd_lattice_ms();
    let min_cands = dm::fwd_min_candidates();
    let candidates = dm::forward_candidates(&curves, lattice_ms, nms_ms, top_n);
    if candidates.len() < min_cands {
        dm::record_forward(dm::ForwardOutcome::Abstained { reason: "too_few_candidates" });
        ::log::info!(
            target: "sync",
            "[deep-match] forward abstained: candidates={} < min {} (lattice={:.0}ms nms=±{:.0}ms top_n={})",
            candidates.len(), min_cands, lattice_ms, nms_ms, top_n
        );
        return;
    }

    // Stage 1: one gyro spline build, then a tight local pre_sync grid per
    // candidate — no LBFGS, no full-domain grid (that is what makes this ~95x
    // cheaper than scoring every candidate with a real rs-sync call).
    let t_fwd = std::time::Instant::now();
    let radius_ms = dm::fwd_radius_ms();
    let step_ms = dm::fwd_step_ms();
    let per_candidate = {
        let noop: Arc<dyn Fn(f64) + Send + Sync> = Arc::new(|_| {});
        let mut finder = super::rs_sync::FindOffsetsRssync::new(
            ranges,
            estimator.sync_results.clone(),
            sync_params,
            params,
            noop,
            cancel_flag.clone(),
        );
        finder.forward_probe(&candidates, radius_ms, step_ms)
    };
    // (center, mean forward cost across scorable windows, window count),
    // ranked by ascending forward cost.
    let mut scored: Vec<(f64, f64, usize)> = candidates
        .iter()
        .zip(per_candidate.iter())
        .filter_map(|(&center, wins)| {
            let costs: Vec<f64> =
                wins.iter().map(|w| w.1).filter(|c| c.is_finite() && *c > 0.0).collect();
            if costs.is_empty() {
                return None;
            }
            Some((center, costs.iter().sum::<f64>() / costs.len() as f64, costs.len()))
        })
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
    let fwd_ms = t_fwd.elapsed().as_secs_f64() * 1000.0;
    ::log::info!(
        target: "sync",
        "[deep-match] forward stage: {} candidates scored in {:.0}ms ({:.1}ms each) local=±{:.0}ms step={:.0}ms",
        scored.len(), fwd_ms, fwd_ms / scored.len().max(1) as f64, radius_ms, step_ms
    );
    for (rank, (center, cost, wins)) in scored.iter().enumerate().take(12) {
        ::log::info!(
            target: "sync",
            "[deep-match]   fwd#{:<2} center={:>12.0}ms fwd_cost={:.4} windows={}",
            rank + 1, center, cost, wins
        );
    }
    if cancel_flag.load(Relaxed) {
        return; // cancelled runs roll back wholesale; no outcome needed
    }
    if scored.len() < min_cands {
        dm::record_forward(dm::ForwardOutcome::Abstained { reason: "no_forward_windows" });
        ::log::info!(
            target: "sync",
            "[deep-match] forward abstained: only {} candidate(s) scorable (< min {})",
            scored.len(), min_cands
        );
        return;
    }

    let costs: Vec<f64> = scored.iter().map(|s| s.1).collect();
    let fv = dm::forward_floor_decision(
        &costs,
        dm::fwd_accept_ratio(),
        dm::fwd_floor_dispersion_max(),
        min_cands,
    );
    ::log::info!(
        target: "sync",
        "[deep-match] forward floor: best_ratio={:.3} floor={:.4} dispersion={:.3} decision={:?}",
        fv.best_ratio, fv.floor, fv.dispersion, fv.decision
    );
    match fv.decision {
        dm::ForwardFloorDecision::Abstain => {
            dm::record_forward(dm::ForwardOutcome::Abstained { reason: "dispersed_floor" });
            return;
        }
        dm::ForwardFloorDecision::Reject => {
            dm::record_forward(dm::ForwardOutcome::Rejected { best_ratio: fv.best_ratio });
            return;
        }
        dm::ForwardFloorDecision::Accept => {}
    }

    // Stage 2: full rs-sync confirmation of the forward-accepted top ranks.
    // The full call supplies the write-back offset (LBFGS precision). Its
    // cross-window spread is a consistency side-gate only — acceptance was
    // carried by the COST criterion (forward cost vs this chunk's noise
    // floor); spread alone must never accept (tightly-agreeing wrong
    // candidates exist), and the essential-path `confidence` output is a
    // constant placeholder and is not consulted.
    let confirm_n = dm::fwd_confirm_n();
    let confirm_radius = dm::fwd_confirm_radius_ms();
    let t_d = dm::drift_tolerance_ms(
        params.scaled_duration_ms,
        dm::drift_rate_ms_per_min(),
        dm::drift_floor_ms(),
    );
    let spread_gate = dm::spread_max_ms() + t_d;
    let eligible: Vec<(f64, f64)> = scored
        .iter()
        .filter(|&&(_, cost, _)| fv.floor > 0.0 && cost / fv.floor <= dm::fwd_accept_ratio())
        .take(confirm_n)
        .map(|&(center, cost, _)| (center, cost))
        .collect();
    for (center, fwd_cost) in eligible {
        if cancel_flag.load(Relaxed) {
            return;
        }
        let t0 = std::time::Instant::now();
        let mut sp = sync_params.clone();
        sp.initial_offset = center;
        sp.search_size = confirm_radius;
        sp.calc_initial_fast = false;
        let res = super::rs_sync::find_offsets(
            estimator,
            ranges,
            &sp,
            params,
            |_| {},
            cancel_flag.clone(),
        );
        let el_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if res.len() < 2 {
            ::log::info!(
                target: "sync",
                "[deep-match] forward confirm center={:.0}ms -> {} window(s), not confirmable took={:.0}ms",
                center, res.len(), el_ms
            );
            continue;
        }
        let mut offs: Vec<f64> = res.iter().map(|x| x.1).collect();
        offs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let spread = offs.last().unwrap() - offs.first().unwrap();
        let median = if offs.len() % 2 == 0 {
            (offs[offs.len() / 2 - 1] + offs[offs.len() / 2]) / 2.0
        } else {
            offs[offs.len() / 2]
        };
        let full_cost = res.iter().map(|x| x.2).sum::<f64>() / res.len() as f64;
        ::log::info!(
            target: "sync",
            "[deep-match] forward confirm center={:.0}ms -> offset={:.1}ms cost={:.3} spread={:.1}ms windows={} took={:.0}ms offs={:?}",
            center, median, full_cost, spread, res.len(), el_ms, offs
        );
        if spread <= spread_gate {
            dm::record_forward(dm::ForwardOutcome::Confirmed {
                offset_ms: median,
                fwd_ratio: fwd_cost / fv.floor,
                full_cost,
                spread_ms: spread,
                windows: res.len(),
            });
            ::log::info!(
                target: "sync",
                "[deep-match] forward total: {:.1}s (extract + score + confirm)",
                t_total.elapsed().as_secs_f64()
            );
            return;
        }
        ::log::info!(
            target: "sync",
            "[deep-match] forward confirm rejected: spread {:.1}ms > gate {:.1}ms (windows disagree)",
            spread, spread_gate
        );
    }
    dm::record_forward(dm::ForwardOutcome::Rejected { best_ratio: fv.best_ratio });
    ::log::info!(
        target: "sync",
        "[deep-match] forward: no candidate confirmed (total {:.1}s)",
        t_total.elapsed().as_secs_f64()
    );
}

/// Mean per-axis Pearson correlation between the OF-estimated gyro and the raw
/// gyro at a candidate offset. One pass, no optimisation — orders of magnitude
/// cheaper than an rs-sync solve, so it serves as the cascade prefilter.
fn pearson_at(offs: f64, of: &[TimeIMU], gyro: &BTreeMap<usize, TimeIMU>) -> Option<f64> {
    let (mut so, mut sg) = ([0f64; 3], [0f64; 3]);
    let (mut soo, mut sgg, mut sog) = ([0f64; 3], [0f64; 3], [0f64; 3]);
    let mut n = 0usize;
    for o in of {
        let Some(g) = gyro_at_timestamp(o.timestamp_ms - offs, gyro) else { continue };
        let (Some(gg), Some(og)) = (g.gyro, o.gyro) else { continue };
        n += 1;
        for k in 0..3 {
            so[k] += og[k];
            sg[k] += gg[k];
            soo[k] += og[k] * og[k];
            sgg[k] += gg[k] * gg[k];
            sog[k] += og[k] * gg[k];
        }
    }
    if n < 8 {
        return None;
    }
    let nf = n as f64;
    let (mut acc, mut cnt) = (0.0, 0);
    for k in 0..3 {
        let cov = sog[k] - so[k] * sg[k] / nf;
        let vo = soo[k] - so[k] * so[k] / nf;
        let vg = sgg[k] - sg[k] * sg[k] / nf;
        if vo > 0.0 && vg > 0.0 {
            acc += cov / (vo * vg).sqrt();
            cnt += 1;
        }
    }
    if cnt == 0 { None } else { Some(acc / cnt as f64) }
}

/// Linear interpolation of a sorted (x, y) curve at `x`, clamped at the ends.
fn interp_at(curve: &[(f64, f64)], x: f64) -> f64 {
    match curve.binary_search_by(|p| p.0.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal)) {
        Ok(i) => curve[i].1,
        Err(0) => curve[0].1,
        Err(i) if i >= curve.len() => curve[curve.len() - 1].1,
        Err(i) => {
            let (x0, y0) = curve[i - 1];
            let (x1, y1) = curve[i];
            if (x1 - x0).abs() < f64::EPSILON { y0 } else { y0 + (y1 - y0) * (x - x0) / (x1 - x0) }
        }
    }
}

fn rssync_peak_probe(
    estimator: &PoseEstimator,
    ranges: &[(i64, i64)],
    sync_params: &SyncParams,
    params: &ComputeParams,
    spec: &str,
    cancel_flag: &Arc<AtomicBool>,
    probe_cache: &[(Vec<TimeIMU>, BTreeMap<usize, TimeIMU>)],
) {
    let mut it = spec.split(':');
    let top_n: usize = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(20);
    let radius_ms: f64 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(10_000.0);
    let max_rssync: usize = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(10);
    let fwd_radius_ms: f64 = it.next().and_then(|v| v.trim().parse().ok()).unwrap_or(100.0);
    if top_n == 0 || radius_ms <= 0.0 || fwd_radius_ms <= 0.0 {
        return;
    }

    let mut curves = crate::synchronization::deep_match::peek_curves();
    if curves.len() < 2 {
        log::info!(target: "sync", "[rssync-probe] skipped: only {} curve(s)", curves.len());
        return;
    }
    for c in curves.iter_mut() {
        c.curve.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    }

    // Joint log-likelihood on a shared 25ms lattice (the curves are 25ms
    // coarse anyway; peak position only needs to land inside the rs-sync
    // verification radius).
    let step = 25.0;
    let lo = curves.iter().filter_map(|c| c.curve.first().map(|p| p.0)).fold(f64::NEG_INFINITY, f64::max);
    let hi = curves.iter().filter_map(|c| c.curve.last().map(|p| p.0)).fold(f64::INFINITY, f64::min);
    if !(hi > lo) {
        log::info!(target: "sync", "[rssync-probe] skipped: empty joint domain");
        return;
    }
    let n = ((hi - lo) / step) as usize + 1;
    let mut joint = vec![0.0f64; n];
    for c in &curves {
        if c.cost_min <= 0.0 {
            continue;
        }
        for (k, j) in joint.iter_mut().enumerate() {
            let cost = interp_at(&c.curve, lo + k as f64 * step);
            if cost > 0.0 {
                *j += -(c.n_eff / 2.0) * (cost / c.cost_min).ln();
            }
        }
    }

    // Greedy NMS at the verification radius.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| joint[b].partial_cmp(&joint[a]).unwrap_or(std::cmp::Ordering::Equal));
    let rpts = (radius_ms / step).ceil() as usize;
    let mut avail = vec![true; n];
    let mut peaks: Vec<usize> = Vec::new();
    for i in order {
        if !avail[i] {
            continue;
        }
        peaks.push(i);
        for a in avail[i.saturating_sub(rpts)..(i + rpts + 1).min(n)].iter_mut() {
            *a = false;
        }
        if peaks.len() >= top_n {
            break;
        }
    }

    log::info!(
        target: "sync",
        "[rssync-probe] start: top_n={} radius={:.0}ms max_rssync={} lattice={} domain=[{:.0},{:.0}]ms windows={} cached={}",
        top_n, radius_ms, max_rssync, n, lo, hi, curves.len(), probe_cache.len()
    );

    // ---- stage 1: cheap Pearson prefilter over every candidate ----
    let t_pre = std::time::Instant::now();
    let mut scored: Vec<(usize, f64, f64, f64)> = Vec::with_capacity(peaks.len());
    for (rank, &pi) in peaks.iter().enumerate() {
        let center = lo + pi as f64 * step;
        let (mut acc, mut cnt) = (0.0, 0);
        for (of, gt) in probe_cache {
            if let Some(r) = pearson_at(center, of, gt) {
                acc += r;
                cnt += 1;
            }
        }
        scored.push((rank + 1, center, joint[pi], if cnt > 0 { acc / cnt as f64 } else { f64::NAN }));
    }
    let pre_ms = t_pre.elapsed().as_secs_f64() * 1000.0;
    log::info!(
        target: "sync",
        "[rssync-probe] stage1 pearson: {} candidates in {:.1}ms ({:.3}ms each)",
        scored.len(), pre_ms, pre_ms / scored.len().max(1) as f64
    );
    for (jr, center, j, r) in &scored {
        log::info!(
            target: "sync",
            "[rssync-probe]   joint#{:<3} center={:>12.0}ms joint={:8.2} pearson={:+.4}",
            jr, center, j, r
        );
    }

    // ---- stage 1.5: forward-only rs-sync scoring over every candidate ----
    // One gyro spline build + a tight local pre_sync grid per candidate. The
    // cost of a real rs-sync call is dominated by its 4000-point full-radius
    // grid scan plus LBFGS, neither of which happens here.
    let t_fwd = std::time::Instant::now();
    let centers: Vec<f64> = scored.iter().map(|s| s.1).collect();
    let fwd = {
        let noop: std::sync::Arc<dyn Fn(f64) + Send + Sync> = std::sync::Arc::new(|_| {});
        let mut finder = super::rs_sync::FindOffsetsRssync::new(
            ranges,
            estimator.sync_results.clone(),
            sync_params,
            params,
            noop,
            cancel_flag.clone(),
        );
        finder.forward_probe(&centers, fwd_radius_ms, 5.0)
    };
    let fwd_ms = t_fwd.elapsed().as_secs_f64() * 1000.0;

    // (joint_rank, center, pearson, fwd_cost, fwd_spread)
    let mut fwd_scored: Vec<(usize, f64, f64, f64, f64)> = Vec::new();
    for (i, per_win) in fwd.iter().enumerate() {
        if per_win.is_empty() {
            continue;
        }
        let cost = per_win.iter().map(|x| x.1).sum::<f64>() / per_win.len() as f64;
        let (mn, mx) = per_win
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &(o, _)| (a.min(o), b.max(o)));
        fwd_scored.push((scored[i].0, scored[i].1, scored[i].3, cost, mx - mn));
    }
    fwd_scored.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
    log::info!(
        target: "sync",
        "[rssync-probe] stage1.5 forward: {} scored in {:.1}ms ({:.2}ms each) radius={:.0}ms",
        fwd_scored.len(), fwd_ms, fwd_ms / fwd_scored.len().max(1) as f64, fwd_radius_ms
    );
    for (rank, (jr, center, r, cost, spread)) in fwd_scored.iter().enumerate().take(15) {
        log::info!(
            target: "sync",
            "[rssync-probe]   fwd#{:<3} (joint#{:<3} pearson={:+.4}) center={:>12.0}ms fwd_cost={:12.6} fwd_spread={:9.1}ms",
            rank + 1, jr, r, center, cost, spread
        );
    }

    // ---- stage 2: full rs-sync on the best candidates by forward score ----
    let by_r: Vec<(usize, f64, f64, f64)> =
        fwd_scored.iter().map(|&(jr, c, r, _, _)| (jr, c, 0.0, r)).collect();
    let t_rs = std::time::Instant::now();
    let mut ran = 0usize;
    for (i, (jr, center, _j, r)) in by_r.iter().take(max_rssync).enumerate() {
        if cancel_flag.load(Relaxed) {
            break;
        }
        let mut sp = sync_params.clone();
        sp.initial_offset = *center;
        sp.search_size = radius_ms;
        let t0 = std::time::Instant::now();
        let res =
            super::rs_sync::find_offsets(estimator, ranges, &sp, params, |_| {}, cancel_flag.clone());
        let el = t0.elapsed().as_secs_f64() * 1000.0;
        ran += 1;
        if res.is_empty() {
            log::info!(
                target: "sync",
                "[rssync-probe] rs#{:<3} (joint#{:<3} pearson={:+.4}) center={:>12.0}ms -> nothing took={:.0}ms",
                i + 1, jr, r, center, el
            );
            continue;
        }
        // Cross-window agreement is the discriminator: a real valley pulls every
        // window to the same offset, a noise peak lets them drift apart. Cost
        // magnitude alone does not separate true from false candidates.
        let offs: Vec<f64> = res.iter().map(|x| x.1).collect();
        let (mn, mx) = offs
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), &v| (a.min(v), b.max(v)));
        let cost_mean = res.iter().map(|x| x.2).sum::<f64>() / res.len() as f64;
        let conf_mean = res.iter().map(|x| x.3).sum::<f64>() / res.len() as f64;
        let dev = offs.iter().map(|&o| (o - *center).abs()).fold(f64::INFINITY, f64::min);
        log::info!(
            target: "sync",
            "[rssync-probe] rs#{:<3} (joint#{:<3} pearson={:+.4}) center={:>12.0}ms | SPREAD={:9.1}ms cost={:10.4} conf={:.3} dev={:9.1}ms n={} took={:.0}ms offs=[{}]",
            i + 1, jr, r, center, mx - mn, cost_mean, conf_mean, dev, res.len(), el,
            offs.iter().map(|v| format!("{v:.1}")).collect::<Vec<_>>().join(", ")
        );
    }
    log::info!(
        target: "sync",
        "[rssync-probe] done: prefilter {:.1}ms for {} cands + {} rs-sync in {:.1}s",
        pre_ms, scored.len(), ran, t_rs.elapsed().as_secs_f64()
    );
}

fn get_max_angle(item: &[TimeIMU]) -> f64 {
    let mut max = 0.0;
    for x in item {
        if let Some(g) = x.gyro {
            if g[0].abs() > max {
                max = g[0].abs();
            }
            if g[1].abs() > max {
                max = g[1].abs();
            }
            if g[2].abs() > max {
                max = g[2].abs();
            }
        }
    }
    max
}

fn gyro_at_timestamp(ts: f64, gyro: &BTreeMap<usize, TimeIMU>) -> Option<&TimeIMU> {
    gyro.range((ts * 1000.0) as usize..).next().map(|x| x.1)
}

fn calculate_cost(offs: f64, of: &[TimeIMU], gyro: &BTreeMap<usize, TimeIMU>) -> f64 {
    let mut sum = 0.0;
    let mut matches_count = 0;
    for o in of {
        if let Some(g) = gyro_at_timestamp(o.timestamp_ms - offs, gyro) {
            if let Some(gg) = g.gyro.as_ref() {
                if let Some(og) = o.gyro.as_ref() {
                    matches_count += 1;
                    sum += (gg[0] - og[0]).powi(2) * 70.0;
                    sum += (gg[1] - og[1]).powi(2) * 70.0;
                    sum += (gg[2] - og[2]).powi(2) * 100.0;
                }
            }
        }
    }
    if !of.is_empty() && matches_count > of.len() / 2 {
        // Return average sum per match, if we tested at least half of the samples
        sum / matches_count as f64
    } else {
        // Otherwise not a good match
        f64::MAX
    }
}

/*struct Translation(Vector2<f32>);
struct TranslationEstimator;

impl sample_consensus::Model<Vector2<f32>> for Translation {
    fn residual(&self, data: &Vector2<f32>) -> f64 {
        (self.0 - data).norm() as f64
    }
}

impl sample_consensus::Estimator<Vector2<f32>> for TranslationEstimator {
    type Model = Translation;
    type ModelIter = std::iter::Once<Translation>;
    const MIN_SAMPLES: usize = 1;
    fn estimate<I>(&self, mut data: I) -> Self::ModelIter
    where
        I: Iterator<Item = Vector2<f32>> + Clone,
    {
        let tr = data.next().unwrap();
        std::iter::once(Translation(tr))
    }
}

/// Return the estimated translation and the inlier matches.
fn estimate_translation(
    kp1: &[Vector2<f32>],
    kp2: &[Vector2<f32>],
    matches: &[(usize, usize)],
) -> (Vector2<f32>, Vec<usize>) {
    let mut arrsac = Arrsac::new(50.0, Xoshiro256PlusPlus::seed_from_u64(0));
    let data: Vec<_> = matches.iter().map(|(id1, id2)| {
        kp2[*id2] - kp1[*id1]
    }).collect();

    // Find inliers with RANSAC.
    let (_translation, inliers) = arrsac
        .model_inliers(&TranslationEstimator, data.iter().cloned())
        .unwrap();

    // Re-estimate translation with inliers only.
    let mut tr_sum = Vector2::zeros();
    inliers.iter().for_each(|&i| {
        tr_sum += data[i];
    });
    let tr = tr_sum / inliers.len() as f32;

    (tr, inliers)
}*/
