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
                cancel_flag.clone(),
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
        let auto_bypass_disabled = std::env::var("GYROFLOW_SYNC_AUTO_BYPASS")
            .map(|v| matches!(v.trim(), "0" | "false" | "no" | "off"))
            .unwrap_or(false);
        // Evaluated on the LOCAL sync_params clone, i.e. AFTER the
        // calc_initial_fast block above possibly replaced initial_offset
        // with the essential-matrix median and clamped search_size to 3000.
        let auto_bypass = should_auto_bypass_fusion(
            sync_params.initial_offset,
            sync_params.search_size,
            auto_bypass_disabled,
        );
        let use_old_rerank = std::env::var("GYROFLOW_SYNC_OLD_RERANK")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false);
        if bypass_fusion {
            log::info!("[rssync] BYPASS FUSION — using raw full_sync() output (matches upstream main)");
        } else if auto_bypass {
            log::info!(
                target: "sync",
                "[rssync] fusion auto-bypassed: |initial_offset|={:.1}ms > search_size={:.1}ms (raw full_sync output)",
                sync_params.initial_offset.abs(),
                sync_params.search_size
            );
        } else if use_old_rerank {
            finder.correlation_rerank(&mut offsets, estimator, ranges, params);
        } else {
            finder.ncc_fusion_decide(&mut offsets, estimator, ranges, params);
            // sync-likelihood-nuisance §3.1: the generative posterior takes
            // over the per-segment output. Fusion above still ran in full so
            // `[ncc-fuse]` stays as the double-write comparison log; any
            // failure or cancellation inside leaves the fusion output
            // untouched (graceful no-op). Bypass branches above (env bypass,
            // auto-bypass on |initial| > search, legacy rerank) skip the
            // posterior too — same interlock as the fusion itself.
            if posterior_enabled() {
                finder.posterior_override(&mut offsets, &cancel_flag);
            }
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

// Auto-bypass fusion when the search neighborhood lies outside the fusion
// data window. ncc_fusion_decide's raw_pairs window only covers
// video_ts ± (search_size + 200ms) and ignores initial_offset, so with
// |initial_offset| > search_size the gyro samples around the true offset
// are never loaded — every correlation-based weight collapses to zero and
// the fallback emits garbage near 0ms. Raw full_sync output (which handles
// initial_offset correctly via initial_delay) matches upstream behavior.
pub(crate) fn should_auto_bypass_fusion(
    initial_offset_ms: f64,
    search_size_ms: f64,
    env_disable: bool,
) -> bool {
    if env_disable {
        return false;
    }
    initial_offset_ms.abs() > search_size_ms
}

/// `GYROFLOW_SYNC_POSTERIOR` master switch (change sync-likelihood-nuisance
/// §3). Default ON (explicit user decision, 2026-06-12 late session: the fix
/// goes live now; the D7 acceptance gates run as regression validation on
/// the recorded corpus instead of blocking enablement). The generative
/// posterior takes over each segment's output offset + confidence after
/// `ncc_fusion_decide` (which keeps running in full as the `[ncc-fuse]`
/// comparison-log producer). `0|false|no|off` reverts the decision to the
/// pre-change fusion output byte-for-byte.
/// OnceLock-cached; first resolve logs to `target="lifecycle"`.
pub(crate) fn posterior_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_SYNC_POSTERIOR").ok();
        let (v, source) = match raw.as_deref().map(str::trim) {
            None | Some("") => (true, "default"),
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => (true, "env"),
                "0" | "false" | "no" | "off" => (false, "env"),
                _ => {
                    log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_SYNC_POSTERIOR={} invalid, falling back to default (on)",
                        s
                    );
                    (true, "default")
                }
            },
        };
        log::info!(target: "lifecycle", "sync_posterior resolved enabled={} source={}", v, source);
        v
    })
}

/// M1 axis-quality weighting on/off (`GYROFLOW_SYNC_AXIS_WEIGHT`, default on).
/// `0` reverts every aggregation point (pearson_at, Pearson scan, NCC) to the
/// legacy equal-weight mean. OnceLock-cached; first resolve logs to
/// `target="lifecycle"` (sync-parallax-suppression M1).
fn axis_weight_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_SYNC_AXIS_WEIGHT").ok();
        let (v, source) = match raw.as_deref().map(str::trim) {
            None => (true, "default"),
            Some("") => (true, "default"),
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => (true, "env"),
                "0" | "false" | "no" | "off" => (false, "env"),
                _ => {
                    log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_SYNC_AXIS_WEIGHT={} invalid, falling back to default",
                        s
                    );
                    (true, "default")
                }
            },
        };
        log::info!(target: "lifecycle", "axis_weight resolved enabled={} source={}", v, source);
        v
    })
}

/// Derive per-axis aggregation weights from full-window axis qualities
/// (sync-parallax-suppression M1, design D2): `w_i = clamp(q_i,0,1)²` floored
/// at 0.05, normalized to Σw = 1. Squaring suppresses mid-quality axes
/// (r=0.5 → 0.25 weight ratio); the floor keeps all-weak segments close to
/// equal-weight (and guards the division). Free function for unit tests.
pub(super) fn axis_weights_from_quality(q: [f64; 3]) -> [f64; 3] {
    let mut w = [0.0f64; 3];
    for i in 0..3 {
        let qi = if q[i].is_finite() { q[i].clamp(0.0, 1.0) } else { 0.0 };
        w[i] = (qi * qi).max(0.05);
    }
    let sum: f64 = w.iter().sum();
    [w[0] / sum, w[1] / sum, w[2] / sum]
}

// ═══ M2 gyro-prior two-pass reweighting (sync-parallax-suppression) ════════

/// Pass-2 mode (`GYROFLOW_SYNC_PRIOR_REWEIGHT`): `0` never, `1` on
/// twin/low-conf trigger (default), `always` unconditionally per segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pass2Mode {
    Off,
    On,
    Always,
}

impl Pass2Mode {
    /// Parse the env value; `None` for invalid input (caller falls back + warns).
    pub(super) fn parse(raw: &str) -> Option<Pass2Mode> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "0" | "false" | "no" | "off" => Some(Pass2Mode::Off),
            "1" | "true" | "yes" | "on" => Some(Pass2Mode::On),
            "always" => Some(Pass2Mode::Always),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(super) struct Pass2Params {
    pub mode: Pass2Mode,
    /// MAD multiplier for the residual gate (`GYROFLOW_SYNC_PRIOR_REWEIGHT_K`).
    pub k: f64,
    /// Pass-2 confidence trigger threshold (`GYROFLOW_SYNC_PASS2_CONF`).
    pub conf_thresh: f64,
    /// Minimum removed-point fraction for the re-solve to be meaningful
    /// (`GYROFLOW_SYNC_PASS2_MIN_REMOVED`, default 0.02). Below this the
    /// problem is essentially unchanged and LBFGS just jumps between
    /// near-equal basins (observed: removing 5-155 points sent argmin to
    /// ±15ms while pass-1 sat at the truth) — instead, clean residuals are
    /// treated as evidence FOR pass-1 (rs twin is noise-intrinsic, the
    /// correlation-led consensus stands).
    pub min_removed_frac: f64,
}

/// Resolve pass-2 params from env (cached; restart to change).
pub(super) fn pass2_params() -> Pass2Params {
    static RESOLVED: std::sync::OnceLock<Pass2Params> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let mut p = Pass2Params {
            mode: Pass2Mode::On,
            k: 4.0,
            conf_thresh: 0.5,
            min_removed_frac: 0.02,
        };
        let mut overrides: Vec<&'static str> = Vec::new();
        if let Ok(raw) = std::env::var("GYROFLOW_SYNC_PRIOR_REWEIGHT") {
            if !raw.is_empty() {
                match Pass2Mode::parse(&raw) {
                    Some(m) => {
                        p.mode = m;
                        overrides.push("GYROFLOW_SYNC_PRIOR_REWEIGHT");
                    }
                    None => log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_SYNC_PRIOR_REWEIGHT={} invalid, falling back to default",
                        raw
                    ),
                }
            }
        }
        let mut read_f64 = |name: &'static str, dst: &mut f64| {
            if let Ok(raw) = std::env::var(name) {
                if !raw.is_empty() {
                    match raw.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0) {
                        Some(v) => {
                            *dst = v;
                            overrides.push(name);
                        }
                        None => log::warn!(
                            target: "lifecycle",
                            "{}={} invalid, falling back to default",
                            name,
                            raw
                        ),
                    }
                }
            }
        };
        read_f64("GYROFLOW_SYNC_PRIOR_REWEIGHT_K", &mut p.k);
        read_f64("GYROFLOW_SYNC_PASS2_CONF", &mut p.conf_thresh);
        read_f64("GYROFLOW_SYNC_PASS2_MIN_REMOVED", &mut p.min_removed_frac);
        log::info!(
            target: "lifecycle",
            "pass2_reweight resolved mode={:?} k={:.1} conf_thresh={:.2} min_removed={:.3} source={}",
            p.mode,
            p.k,
            p.conf_thresh,
            p.min_removed_frac,
            if overrides.is_empty() { "default".to_string() } else { format!("env[{}]", overrides.join(",")) }
        );
        p
    })
}

/// Robust residual gate: `median + k × MAD` (design D3; the additive-median
/// form keeps the gate safe when pass-1's offset error shifts the whole
/// residual distribution by a common ~0.09° bias). Returns INFINITY for
/// degenerate inputs (nothing gets filtered).
pub(super) fn pass2_threshold(residuals: &[f64], k: f64) -> f64 {
    let mut v: Vec<f64> = residuals.iter().copied().filter(|r| r.is_finite()).collect();
    if v.len() < 10 {
        return f64::INFINITY;
    }
    let median = |s: &mut [f64]| -> f64 {
        s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = s.len();
        if n % 2 == 0 { (s[n / 2 - 1] + s[n / 2]) / 2.0 } else { s[n / 2] }
    };
    let med = median(&mut v);
    let mut dev: Vec<f64> = v.iter().map(|r| (r - med).abs()).collect();
    let mad = median(&mut dev);
    // MAD floor at 5% of the median guards against quantized / near-constant
    // residual sets collapsing the gate onto the bulk itself.
    med + k * mad.max(med * 0.05)
}

/// Adoption rule (design D3): pass-2 replaces pass-1 only when strictly more
/// confident. NaN pass-2 confidence never wins.
pub(super) fn adopt_pass2(pass1_conf: f64, pass2_conf: f64) -> bool {
    pass2_conf.is_finite() && pass2_conf > pass1_conf
}

/// Indices passing the residual gate; when fewer than `min_keep` survive,
/// degrade to the `min_keep` lowest-residual points (tie-break by index) so
/// rs-sync still has a usable ray set for the pair.
pub(super) fn pass2_keep_indices(residuals: &[f64], threshold: f64, min_keep: usize) -> Vec<usize> {
    let pass: Vec<usize> = (0..residuals.len())
        .filter(|&i| residuals[i].is_finite() && residuals[i] <= threshold)
        .collect();
    if pass.len() >= min_keep || residuals.len() <= min_keep {
        return pass;
    }
    let mut idx: Vec<usize> = (0..residuals.len())
        .filter(|&i| residuals[i].is_finite())
        .collect();
    idx.sort_by(|&a, &b| {
        residuals[a]
            .partial_cmp(&residuals[b])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    idx.truncate(min_keep);
    idx.sort_unstable();
    idx
}

/// Pure-rotation angular residuals (degrees) for one pair's rays at the given
/// external offset, replicating rs-sync's `opt_compute_problem` geometry:
/// both rays are rotated into the gyro/world frame by the conjugated spline
/// quat — which, given `set_quats` feeds `conj(q_src · rot_PI_x)`, equals
/// `q_src(t + delay) · rot_PI_x` — and a rotation-consistent point has
/// `ar ∥ br`. Parallax / foreground points violate this by 0.2-1° while a
/// ±10ms pass-1 offset error mismatches true points by only ~|ω|·0.01s.
fn rotation_residuals_deg(
    quats: &crate::gyro_source::TimeQuat,
    pair: &PairTracks,
    offset_external_ms: f64,
    frt_offset_ms: f64,
) -> Vec<f64> {
    // External → internal rs-sync delay: external = -d_s·1000 − frt/2.
    let d_s = -(offset_external_ms + frt_offset_ms) / 1000.0;
    let rot = Quat64::from_scaled_axis(Vector3::new(PI, 0.0, 0.0));
    pair.tss_a
        .iter()
        .zip(pair.tss_b.iter())
        .zip(pair.rays_a.iter().zip(pair.rays_b.iter()))
        .map(|((&ts_a, &ts_b), (&ra, &rb))| {
            let q_a = GyroSource::clamped_quat_at_gyro_timestamp(quats, (ts_a + d_s) * 1000.0);
            let q_b = GyroSource::clamped_quat_at_gyro_timestamp(quats, (ts_b + d_s) * 1000.0);
            let ar = (q_a * rot).transform_vector(&Vector3::new(ra.0, ra.1, ra.2));
            let br = (q_b * rot).transform_vector(&Vector3::new(rb.0, rb.1, rb.2));
            ar.angle(&br).to_degrees()
        })
        .collect()
}

/// Result of a successful pass-2 segment rebuild (M2).
struct Pass2Rebuild {
    new_argmin_ext_ms: f64,
    new_cost: f64,
    kept: usize,
    total: usize,
}

/// Outcome of the pass-2 residual analysis for a segment (M2).
enum Pass2Outcome {
    /// Substantial parallax suspects removed; the problem was re-solved and
    /// the fusion body should re-run on the cleaned curve.
    Rebuilt(Pass2Rebuild),
    /// The rotation model found (almost) nothing to remove — the data is
    /// clean, the rs twin is noise-intrinsic, and re-solving a near-identical
    /// problem only adds LBFGS basin jitter. Evidence FOR pass-1.
    CleanResiduals { removed: usize, total: usize },
    /// Too few points / degenerate gate / re-solve failure — no information.
    NotApplicable,
}

/// Confidence cap for twin ambiguities resolved by pass-2 cross-validation:
/// the point passes the controller's 0.4 drop gate but ranks below
/// self-evidenced full-confidence consensus.
const TWIN_RESOLVED_CONF_CAP: f64 = 0.6;
/// Confidence cap when the two passes DISAGREE and the Pearson-anchored
/// arbitration picks a side — weaker evidence than agreement, still above
/// the 0.4 drop gate.
const TWIN_ARBITRATED_CONF_CAP: f64 = 0.5;

/// Pass-1 fusion result snapshot for the M2 adoption rule.
struct Pass1Snapshot {
    output_ms: f64,
    output_cost: f64,
    confidence: f64,
    conf_path: ConfPath,
    /// Confidence/path before the twin ceiling — restored (capped) when
    /// pass-2 cross-validation resolves the twin ambiguity.
    pre_twin_conf: f64,
    pre_twin_path: ConfPath,
    path_str: String,
    twin: Option<super::twin_guard::TwinInfo>,
}

/// One frame-pair's tracks exactly as fed to `SyncProblem::set_track_result`
/// (sync-parallax-suppression M2 keeps a copy so a segment's problem can be
/// rebuilt with gyro-prior-filtered points and re-solved in pass 2).
struct PairTracks {
    timestamp_us: i64,
    tss_a: Vec<f64>,
    tss_b: Vec<f64>,
    rays_a: Vec<(f64, f64, f64)>,
    rays_b: Vec<(f64, f64, f64)>,
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

    /// Per-range copies of the tracks fed to `SyncProblem` in `new()`,
    /// aligned with `sync_points` (M2). ~200 pts × ~90 pairs × 5×f64 < 1MB
    /// per segment.
    track_data: Vec<Vec<PairTracks>>,

    /// `GYROFLOW_SYNC_DIAG=2` only: ranges whose residual grid has already
    /// been dumped this run. M2 pass-2 re-runs the fusion body (and
    /// `scan_cost_curve_per_seg`) on the same segment — without this flag the
    /// per-window dump doubles, and it would capture the gyro-prior-filtered
    /// pass-2 problem instead of the raw corpus data.
    residuals_dumped: parking_lot::Mutex<std::collections::HashSet<usize>>,
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
            track_data: Vec::new(),
            residuals_dumped: parking_lot::Mutex::new(std::collections::HashSet::new()),
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
            let mut seg_tracks: Vec<PairTracks> = Vec::new();
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
                // M2: keep a copy so pass 2 can rebuild this segment's problem
                // from gyro-prior-filtered points.
                seg_tracks.push(PairTracks {
                    timestamp_us: a_t,
                    tss_a,
                    tss_b,
                    rays_a: points3d_a,
                    rays_b: points3d_b,
                });
            }
            ret.sync_points.push((from_ts, to_ts));
            ret.track_data.push(seg_tracks);
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

    /// M2 pass-2 core (sync-parallax-suppression): compute pure-rotation
    /// residuals for the segment's stored tracks at the pass-1 offset, drop
    /// points above `median + k×MAD` (parallax / foreground suspects that FB
    /// checking cannot catch — they roundtrip consistently but violate the
    /// rotation model), re-feed the filtered tracks into the live
    /// `SyncProblem` and re-solve the segment with `full_sync_with_curve`.
    ///
    /// On success `self.presync_curves[curve_idx]` holds the pass-2 curve and
    /// the problem holds the filtered tracks — caller re-runs the fusion body
    /// and restores both via the returned state if pass 2 is not adopted.
    /// On failure the original tracks are restored before returning `None`.
    fn pass2_rebuild_segment(
        &mut self,
        curve_idx: usize,
        sp_from: i64,
        sp_to: i64,
        pass1_output_ms: f64,
    ) -> Pass2Outcome {
        let range_idx =
            match self.sync_points.iter().position(|sp| *sp == (sp_from, sp_to)) {
                Some(r) => r,
                None => return Pass2Outcome::NotApplicable,
            };
        if range_idx >= self.track_data.len() || self.track_data[range_idx].is_empty() {
            return Pass2Outcome::NotApplicable;
        }
        let frt_offset_ms = self.frame_readout_time * 1000.0 / 2.0;

        // Phase 1 (immutable): residuals → global gate → owned filtered tracks.
        let (filtered, kept, total) = {
            let gyro = self.gyro_source.read();
            let quats = &gyro.quaternions;
            if quats.len() < 2 {
                return Pass2Outcome::NotApplicable;
            }
            let tracks = &self.track_data[range_idx];
            let per_pair_residuals: Vec<Vec<f64>> = tracks
                .iter()
                .map(|p| rotation_residuals_deg(quats, p, pass1_output_ms, frt_offset_ms))
                .collect();
            let all: Vec<f64> = per_pair_residuals.iter().flatten().copied().collect();
            let total = all.len();
            if total < 30 {
                log::info!(
                    "[pass2] seg {}: skipped — only {} ray pairs (need ≥30)",
                    curve_idx,
                    total
                );
                return Pass2Outcome::NotApplicable;
            }
            let threshold = pass2_threshold(&all, pass2_params().k);
            let mut filtered: Vec<PairTracks> = Vec::with_capacity(tracks.len());
            let mut kept = 0usize;
            for (pair, residuals) in tracks.iter().zip(per_pair_residuals.iter()) {
                let keep = pass2_keep_indices(residuals, threshold, 10);
                kept += keep.len();
                filtered.push(PairTracks {
                    timestamp_us: pair.timestamp_us,
                    tss_a: keep.iter().map(|&j| pair.tss_a[j]).collect(),
                    tss_b: keep.iter().map(|&j| pair.tss_b[j]).collect(),
                    rays_a: keep.iter().map(|&j| pair.rays_a[j]).collect(),
                    rays_b: keep.iter().map(|&j| pair.rays_b[j]).collect(),
                });
            }
            let removed = total - kept;
            let min_removed =
                ((total as f64) * pass2_params().min_removed_frac).ceil() as usize;
            if removed < min_removed.max(1) {
                // Clean residuals: no meaningful parallax evidence. Skip the
                // re-solve entirely (it would be the same problem + LBFGS
                // basin jitter) and report the cleanliness as pass-1 evidence.
                return Pass2Outcome::CleanResiduals { removed, total };
            }
            if (kept as f64) < (total as f64) * 0.3 {
                log::warn!(
                    "[pass2] seg {}: skipped — gate would drop {}/{} points (>70%), residual model unreliable here",
                    curve_idx,
                    removed,
                    total
                );
                return Pass2Outcome::NotApplicable;
            }
            (filtered, kept, total)
        };

        // Phase 2 (mutable): feed filtered tracks and re-solve the segment.
        for p in &filtered {
            self.sync
                .set_track_result(p.timestamp_us, &p.tss_a, &p.tss_b, &p.rays_a, &p.rays_b);
        }
        let initial_delay = -self.sync_params.initial_offset;
        let presync_radius = self.sync_params.search_size;
        let resync_t0 = std::time::Instant::now();
        let delay_res = self.sync.full_sync_with_curve(
            initial_delay / 1000.0,
            sp_from,
            sp_to,
            5.0 / 1000.0,
            presync_radius / 1000.0,
            2,
        );
        let resync_ms = resync_t0.elapsed().as_secs_f64() * 1000.0;
        match delay_res {
            Some((delay, curve)) => {
                let offset = delay.1 * 1000.0;
                if (offset - initial_delay).abs() >= presync_radius * 0.9 {
                    log::warn!(
                        "[pass2] seg {}: re-solve LBFGS out of bounds ({:.1}ms) — restoring pass1 problem",
                        curve_idx,
                        offset
                    );
                    self.restore_segment_tracks(range_idx);
                    return Pass2Outcome::NotApplicable;
                }
                let new_argmin_ext_ms = -offset - frt_offset_ms;
                if crate::synchronization::sync_diag::is_enabled() {
                    // Pass-2 curve dumped under range_idx + 1000 so it can be
                    // compared against the pass-1 curve of the same segment.
                    let curve_ext: Vec<(f64, f64)> = curve
                        .iter()
                        .filter(|(c, _)| c.is_finite())
                        .map(|&(c, d_s)| (-d_s * 1000.0 - frt_offset_ms, c))
                        .collect();
                    crate::synchronization::sync_diag::record_cost_curve_rssync(
                        1000 + curve_idx,
                        &curve_ext,
                    );
                }
                if curve_idx < self.presync_curves.len() {
                    self.presync_curves[curve_idx] = curve;
                }
                log::info!(
                    "[pass2] seg {}: re-solved in {:.0}ms with {}/{} points → rs_argmin {:.1}ms (cost {:.2})",
                    curve_idx,
                    resync_ms,
                    kept,
                    total,
                    new_argmin_ext_ms,
                    delay.0
                );
                Pass2Outcome::Rebuilt(Pass2Rebuild {
                    new_argmin_ext_ms,
                    new_cost: delay.0,
                    kept,
                    total,
                })
            }
            None => {
                log::warn!(
                    "[pass2] seg {}: full_sync_with_curve returned None — restoring pass1 problem",
                    curve_idx
                );
                self.restore_segment_tracks(range_idx);
                Pass2Outcome::NotApplicable
            }
        }
    }

    /// Re-feed the original (unfiltered) tracks of a segment into the live
    /// `SyncProblem` (`set_track_result` overwrites by frame timestamp key).
    fn restore_segment_tracks(&mut self, range_idx: usize) {
        let tracks = std::mem::take(&mut self.track_data[range_idx]);
        for p in &tracks {
            self.sync
                .set_track_result(p.timestamp_us, &p.tss_a, &p.tss_b, &p.rays_a, &p.rays_b);
        }
        self.track_data[range_idx] = tracks;
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
        from_ts: i64,
        to_ts: i64,
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
            // GYROFLOW_SYNC_DIAG=2: per-point residual dump for the offline
            // likelihood rebuild (change sync-likelihood-nuisance, task 1.3).
            self.dump_residual_grid(range_idx, from_ts, to_ts, &raw, final_offset_external_ms, frt_offset_ms);
        }
        (best_offs, ratio)
    }

    /// `GYROFLOW_SYNC_DIAG=2` only: stream per-point rs-sync residuals to
    /// `residuals.csv` — every 10th grid δ of the cached pre-search curve
    /// plus the final δ*. At every sampled δ TWO row groups are dumped:
    /// gain = 1.0 (baseline, matches the existing cost curve) and gain = ĝ
    /// from rs-sync's closed-form gain solve (design D1) — the latter is
    /// what lets the offline replay rebuild the gain-profiled likelihood
    /// without live SyncProblem geometry.
    ///
    /// Volume control (a full dump measured 14.46M rows = 1.16 GB / 22.5 s
    /// per window): grid-δ groups are thinned to ≤ `RESIDUAL_DUMP_MAX_POINTS`
    /// rows each via a deterministic equidistant stride over the flattened
    /// (pair, point) sequence — both gain groups share the stride (computed
    /// from the g = 1 group) so rows stay comparable, and `point_idx` keeps
    /// the ORIGINAL per-pair index. δ* keeps the full point set for both
    /// groups (σ/MAD analysis needs the complete population). The gain solve
    /// on thinned δ runs on the same strided subset (`solve_gain_strided`,
    /// fed the already-evaluated g = 1 base — the gain is a single scalar,
    /// ~2000 points are statistically plenty), cutting its ~19 full residual
    /// passes per δ to ~1/stride. None of this touches the progress callback
    /// (no progress-bar jumps); diag-only, not on any hot path.
    ///
    /// M2 pass-2 re-runs of the same segment skip the dump entirely (the
    /// `residuals_dumped` flag): the corpus wants the pass-1 (unfiltered)
    /// residuals exactly once — a second dump would double the file and
    /// record a derived (gyro-prior-filtered) problem state the offline
    /// replay can re-derive from the pass-1 rows itself.
    fn dump_residual_grid(
        &self,
        range_idx: usize,
        from_ts: i64,
        to_ts: i64,
        raw_curve: &[(f64, f64)],
        final_offset_external_ms: f64,
        frt_offset_ms: f64,
    ) {
        /// Per-(δ, gain-group) row cap for grid-sampled δ (δ* is exempt).
        const RESIDUAL_DUMP_MAX_POINTS: usize = 2000;

        if !crate::synchronization::sync_diag::residual_dump_enabled() {
            return;
        }
        if !self.residuals_dumped.lock().insert(range_idx) {
            log::debug!(
                target: "sync",
                "[SyncDiag] residual dump: range={} already dumped (M2 pass-2 re-run), skipping",
                range_idx
            );
            return;
        }
        let t0 = std::time::Instant::now();
        let mut n_deltas = 0usize;
        let mut n_rows = 0usize;
        // Equidistant thinning over the flattened (pair, point) sequence;
        // stride == 1 keeps everything. Original per-pair point indices are
        // preserved so rows stay join-able across gain groups and δ.
        let thin_rows = |groups: &[rs_sync::PairResiduals], stride: usize| -> Vec<(i64, usize, f64)> {
            let mut rows = Vec::new();
            let mut flat = 0usize;
            for g in groups {
                for (idx, r) in g.residuals.iter().enumerate() {
                    if flat % stride == 0 {
                        rows.push((g.timestamp_us, idx, *r));
                    }
                    flat += 1;
                }
            }
            rows
        };
        let mut dump_at = |delay_s: f64, full: bool| {
            let ext_ms = -delay_s * 1000.0 - frt_offset_ms;
            // Baseline group: g ≡ 1 (aggregates back to the cost curve).
            // Also provides the fixed (m, k) pairs + stride for the strided
            // gain solve below.
            let base = self.sync.eval_residuals(delay_s, from_ts, to_ts);
            let n_total: usize = base.iter().map(|g| g.residuals.len()).sum();
            let stride = if full {
                1
            } else {
                n_total.div_ceil(RESIDUAL_DUMP_MAX_POINTS).max(1)
            };
            let mut record = |gain: f64, groups: &[rs_sync::PairResiduals]| {
                let rows = thin_rows(groups, stride);
                n_rows += rows.len();
                crate::synchronization::sync_diag::record_residuals(range_idx, ext_ms, gain, &rows);
            };
            record(1.0, &base);
            // Profiled group: residuals at the closed-form gain ĝ(δ).
            let g_hat = if stride > 1 {
                self.sync.solve_gain_strided(delay_s, &base, stride)
            } else {
                self.sync.solve_gain(delay_s, from_ts, to_ts)
            };
            record(g_hat, &self.sync.eval_residuals_with_gain(delay_s, from_ts, to_ts, g_hat));
            n_deltas += 1;
        };
        for (i, &(_cost, delay_s)) in raw_curve.iter().enumerate() {
            if i % 10 == 0 {
                dump_at(delay_s, false);
            }
        }
        // δ* (LBFGS-refined final offset; generally off-grid) — full dump.
        let final_delay_s = -(final_offset_external_ms + frt_offset_ms) / 1000.0;
        dump_at(final_delay_s, true);
        log::info!(
            target: "sync",
            "[SyncDiag] residual dump: range={} deltas={} rows={} (g1 + profiled-gain groups; grid δ thinned to ≤{} pts/group, δ* full) took {:.0}ms",
            range_idx,
            n_deltas,
            n_rows,
            RESIDUAL_DUMP_MAX_POINTS,
            t0.elapsed().as_secs_f64() * 1000.0
        );
    }

    /// sync-likelihood-nuisance §3 — generative-posterior decision layer.
    ///
    /// Runs AFTER `ncc_fusion_decide` (kept intact as the `[ncc-fuse]`
    /// comparison-log producer) and REPLACES each segment's output offset +
    /// confidence with the joint-posterior decision:
    ///
    /// 1. per window: robust likelihood on a sampled δ set — every 10th point
    ///    of the cached pre-search curve (≈50ms) densified ±30ms around the
    ///    curve argmin and the fusion output (candidates double as grid
    ///    densifiers, design D6). Residuals are thinned to ≤2000 points/δ
    ///    (same budget as the DIAG=2 dump), the amplitude gain is profiled
    ///    per δ on the same strided subset (`solve_gain_strided`, fixed
    ///    (m, k) per pair), σ = 1.4826×MAD at the best sampled δ, Tukey on
    ///    standardized residuals, curvature scaled by the frame-pair count
    ///    (designs D1/D2);
    /// 2. windows are resampled onto the common 5ms lattice and summed
    ///    (design D3 cross-window product — probe-only windows added by
    ///    `AutosyncProcess` participate here and are stripped after
    ///    `find_offsets` returns);
    /// 3. prior (design D4): Stored Gaussian(initial_offset, σ=search/2)
    ///    when an initial offset exists, else Uniform. Batch/deep-match
    ///    anchors write search_size = 3000ms, so the stored tier carries
    ///    exactly the anchor tier's σ = 1500ms — no separate source flag
    ///    is needed at this layer;
    /// 4. joint argmax → per-segment `pre_sync` sub-grid refinement (±7.5ms,
    ///    0.1ms step); confidence = posterior mass within ±12.5ms of the
    ///    argmax (D5), single-window runs scaled one tier down (×0.85).
    ///
    /// Guards / candidate votes DO NOT modify this output (D6). Any failure
    /// or cancellation leaves the fusion output untouched (graceful no-op).
    pub fn posterior_override(&mut self, offsets: &mut [(f64, f64, f64, f64)], cancel_flag: &AtomicBool) {
        use crate::synchronization::posterior::{
            combine_windows_on_common_grid, posterior_decide, sigma_mad, window_log_likelihood,
            Prior,
        };
        const GRID_STEP_MS: f64 = 5.0;
        const SAMPLE_EVERY_NTH: usize = 10;
        const THIN_MAX_POINTS: usize = 2000;
        const DENSIFY_RADIUS_MS: f64 = 30.0;
        const REFINE_RADIUS_S: f64 = 0.0075;
        const FINE_STEP_S: f64 = 0.0001;
        const SINGLE_WINDOW_CONF_FACTOR: f64 = 0.85;

        if offsets.is_empty() {
            return;
        }
        let t0 = std::time::Instant::now();
        let frt_offset_ms = self.frame_readout_time * 1000.0 / 2.0;
        let ext_of = |d_s: f64| -d_s * 1000.0 - frt_offset_ms;

        struct WindowEval {
            seg: usize,
            sp: (i64, i64),
            grid_ms: Vec<f64>,
            logl: Vec<f64>,
            sigma: f64,
            n_pairs: usize,
            gain_star: f64,
            star_ms: f64,
        }
        let mut windows: Vec<WindowEval> = Vec::new();

        for i in 0..offsets.len() {
            if cancel_flag.load(Relaxed) {
                log::info!(target: "sync", "[posterior] canceled — keeping fusion outputs");
                return;
            }
            let (mid_ms, fusion_ms, _cost, _conf) = offsets[i];
            let mid_us = (mid_ms * 1000.0) as i64;
            let sp = match self
                .sync_points
                .iter()
                .find(|(f, t)| mid_us >= *f && mid_us <= *t)
            {
                Some(s) => *s,
                None => continue,
            };
            let curve = match self.presync_curves.get(i) {
                Some(c) if !c.is_empty() => c.clone(),
                _ => continue,
            };

            // Sampled δ indices: coarse lattice + dense neighborhoods around
            // the curve argmin and the fusion output. The true basin can be
            // ~10-15ms wide — the coarse lattice alone could miss it, the
            // argmin densification guarantees it is sampled.
            let mut idxs: std::collections::BTreeSet<usize> =
                (0..curve.len()).step_by(SAMPLE_EVERY_NTH).collect();
            let argmin_idx = curve
                .iter()
                .enumerate()
                .filter(|(_, (c, _))| c.is_finite())
                .min_by(|a, b| {
                    a.1 .0
                        .partial_cmp(&b.1 .0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(k, _)| k);
            let mut densify_centers_ms: Vec<f64> = Vec::new();
            if let Some(k) = argmin_idx {
                densify_centers_ms.push(ext_of(curve[k].1));
            }
            if fusion_ms.is_finite() {
                densify_centers_ms.push(fusion_ms);
            }
            for center in &densify_centers_ms {
                for (k, &(_c, d_s)) in curve.iter().enumerate() {
                    if (ext_of(d_s) - center).abs() <= DENSIFY_RADIUS_MS {
                        idxs.insert(k);
                    }
                }
            }
            let deltas: Vec<f64> = idxs.iter().map(|&k| curve[k].1).collect();

            // Evaluate the robust residual groups at every sampled δ in
            // parallel (SyncProblem evaluation is read-only; workers carry
            // the log context).
            let sync = &self.sync;
            let (sp_from, sp_to) = sp;
            let evals: Vec<Option<(f64, f64, Vec<Vec<f64>>)>> =
                crate::log_context::par_with_ctx(deltas, |delay_s: f64| {
                    if cancel_flag.load(Relaxed) {
                        return None;
                    }
                    // g ≡ 1 baseline: provides per-pair (m, k) and the
                    // thinning stride, exactly like the DIAG=2 dump.
                    let base = sync.eval_residuals(delay_s, sp_from, sp_to);
                    let n_total: usize = base.iter().map(|g| g.residuals.len()).sum();
                    if n_total == 0 {
                        return None;
                    }
                    let stride = n_total.div_ceil(THIN_MAX_POINTS).max(1);
                    let g_hat = sync.solve_gain_strided(delay_s, &base, stride);
                    // Residuals at ĝ on the SAME strided subset with fixed
                    // (m, k) per pair — the gain must explain the amplitude
                    // under the same motion model, and the subset keeps the
                    // per-δ cost bounded.
                    let mut flat = 0usize;
                    let mut groups: Vec<Vec<f64>> = Vec::new();
                    for p in &base {
                        if p.residuals.is_empty() {
                            continue;
                        }
                        let mut sel = Vec::new();
                        for k in 0..p.residuals.len() {
                            if flat % stride == 0 {
                                sel.push(k);
                            }
                            flat += 1;
                        }
                        if sel.is_empty() || p.motion.len() != 3 || !p.var_k.is_finite() {
                            continue;
                        }
                        let r = sync.eval_pair_residuals_fixed_subset(
                            p.timestamp_us,
                            delay_s,
                            g_hat,
                            &p.motion,
                            Some(p.var_k),
                            &sel,
                        );
                        if !r.is_empty() {
                            groups.push(r);
                        }
                    }
                    if groups.is_empty() {
                        return None;
                    }
                    Some((ext_of(delay_s), g_hat, groups))
                });
            if cancel_flag.load(Relaxed) {
                log::info!(target: "sync", "[posterior] canceled — keeping fusion outputs");
                return;
            }
            let mut evals: Vec<(f64, f64, Vec<Vec<f64>>)> = evals.into_iter().flatten().collect();
            if evals.len() < 2 {
                continue;
            }
            evals.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

            // δ*: smallest median |r| (same selection as the offline replay),
            // σ = robust MAD scale of its residual population.
            let med_abs = |groups: &[Vec<f64>]| -> f64 {
                let mut v: Vec<f64> = groups
                    .iter()
                    .flatten()
                    .map(|r| r.abs())
                    .filter(|r| r.is_finite())
                    .collect();
                if v.is_empty() {
                    return f64::INFINITY;
                }
                let k = v.len() / 2;
                v.select_nth_unstable_by(k, |a, b| {
                    a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal)
                });
                v[k]
            };
            let star_i = match evals
                .iter()
                .enumerate()
                .map(|(k, e)| (k, med_abs(&e.2)))
                .filter(|(_, m)| m.is_finite())
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            {
                Some((k, _)) => k,
                None => continue,
            };
            let flat_star: Vec<f64> = evals[star_i].2.iter().flatten().copied().collect();
            let sigma = sigma_mad(&flat_star);
            let n_pairs = evals[star_i].2.len();
            let gain_star = evals[star_i].1;
            let star_ms = evals[star_i].0;
            let grid_ms: Vec<f64> = evals.iter().map(|e| e.0).collect();
            let logl: Vec<f64> = evals
                .iter()
                .map(|e| window_log_likelihood(&e.2, sigma))
                .collect();
            windows.push(WindowEval { seg: i, sp, grid_ms, logl, sigma, n_pairs, gain_star, star_ms });
        }

        if windows.is_empty() {
            log::warn!(
                target: "sync",
                "[posterior] no usable window likelihood — keeping fusion outputs"
            );
            return;
        }

        // D3: cross-window product on the common 5ms lattice.
        let views: Vec<(&[f64], &[f64])> = windows
            .iter()
            .map(|w| (w.grid_ms.as_slice(), w.logl.as_slice()))
            .collect();
        let Some((joint_grid, joint_logl)) = combine_windows_on_common_grid(&views, GRID_STEP_MS)
        else {
            log::warn!(
                target: "sync",
                "[posterior] window grids share no common span — keeping fusion outputs"
            );
            return;
        };

        // D4 prior. `initial_offset` here is the effective value (it already
        // reflects the calc_initial_fast essential median when that ran).
        let init = self.sync_params.initial_offset;
        let prior = if init.abs() > 1e-9 {
            Prior::Stored { init_ms: init, search_size_ms: self.sync_params.search_size }
        } else {
            Prior::Uniform
        };
        let prior_str = match prior {
            Prior::Stored { init_ms, search_size_ms } => {
                format!("stored(init={:.1},sigma={:.0})", init_ms, search_size_ms / 2.0)
            }
            _ => "uniform".to_string(),
        };

        let Some(post) = posterior_decide(&joint_grid, &joint_logl, &prior) else {
            log::warn!(
                target: "sync",
                "[posterior] posterior_decide failed — keeping fusion outputs"
            );
            return;
        };
        let n_windows = windows.len();
        let conf = (post.conf_posterior
            * if n_windows == 1 { SINGLE_WINDOW_CONF_FACTOR } else { 1.0 })
        .clamp(0.0, 1.0);

        // Per-segment sub-grid refinement at the shared joint argmax (D6:
        // same pre_sync refinement the fusion output uses).
        for w in &windows {
            let center_s = -(post.argmax_ms + frt_offset_ms) / 1000.0;
            let refined = self
                .sync
                .pre_sync(center_s, w.sp.0, w.sp.1, FINE_STEP_S, REFINE_RADIUS_S);
            let (out_ms, out_cost) = match refined {
                Some((c, d_s)) if c.is_finite() => (ext_of(d_s), c),
                _ => (post.argmax_ms, offsets[w.seg].2),
            };
            let fusion_ms = offsets[w.seg].1;
            log::info!(
                target: "sync",
                "[posterior] seg {}: argmax={:.1}ms refined={:.1}ms ci95=[{:.0},{:.0}] conf={:.3} windows={} win=[{}-{}ms] g*={:.3} sigma={:.5} n_pairs={} star={:.1}ms prior={} fusion={:.1}ms diff={:+.1}ms",
                w.seg,
                post.argmax_ms,
                out_ms,
                post.ci95.0,
                post.ci95.1,
                conf,
                n_windows,
                w.sp.0 / 1000,
                w.sp.1 / 1000,
                w.gain_star,
                w.sigma,
                w.n_pairs,
                w.star_ms,
                prior_str,
                fusion_ms,
                out_ms - fusion_ms
            );
            offsets[w.seg] = (offsets[w.seg].0, out_ms, out_cost, conf);
        }
        log::info!(
            target: "sync",
            "[posterior] joint decision done in {:.0}ms (windows={}, joint_grid={} pts, argmax={:.1}ms conf={:.3})",
            t0.elapsed().as_secs_f64() * 1000.0,
            n_windows,
            joint_grid.len(),
            post.argmax_ms,
            conf
        );
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
        // Pairing tolerance for est↔raw Pearson (shared by the V2 candidate
        // closures below and the M1 axis-quality scan).
        const NEAREST_TOL_MS_V2: f64 = 10.0;
        // M1 (sync-parallax-suppression): per-axis quality is the max |r_axis|
        // over a coarse full-window scan — decoupled from any specific offset
        // so the weights cannot self-reinforce a wrong candidate.
        const AXIS_SCAN_STEP_MS: f64 = 25.0;
        let axis_weighting = axis_weight_enabled();

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
        // `mut` + extracted builder: M2 pass-2 re-solves a segment's curve and
        // rebuilds its cache entry in place before re-running the fusion body.
        use crate::synchronization::sync_metric::{
            find_local_minima as _find_local_minima, CostMinimum as _CostMinimum,
        };
        let build_curve_entry =
            |curve_internal: &[(f64, f64)]| -> (Vec<(f64, f64)>, f64, Vec<_CostMinimum>) {
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
            };
        let mut curve_cache: Vec<(Vec<(f64, f64)>, f64, Vec<_CostMinimum>)> = (0..offsets.len())
            .map(|idx| {
                if idx >= self.presync_curves.len() {
                    return (Vec::new(), 0.0_f64, Vec::new());
                }
                build_curve_entry(&self.presync_curves[idx])
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

        'seg: for i in 0..offsets.len() {
            // Reverted aaaa3e1f's cost ≤ fusion_cost_threshold bypass: rs-sync
            // cost surface in low-motion / rotation-dominated cases is not a
            // reliable basin indicator (wrong basin cost can be lower than true
            // basin). 4-candidate consensus must always run to provide rotation-
            // based correlation signals (NCC, Pearson) that recover the true
            // offset. Anchor-pool gate above still uses fusion_cost_threshold
            // for cross-prior anchor selection, which is correct usage.
            //
            // M2 (sync-parallax-suppression): the body below runs once (pass 1);
            // when the pass-2 trigger fires, the segment's problem is rebuilt
            // with gyro-prior-filtered tracks and the body re-runs (pass 2),
            // then the adoption rule picks the better of the two results.
            // Bare `continue` inside the body MUST target 'seg, never 'pass.
            let mut pass: u8 = 1;
            let mut pass1_snapshot: Option<Pass1Snapshot> = None;
            let mut pass1_curve_backup: Option<Vec<(f64, f64)>> = None;
            let mut pass2_note: Option<(usize, usize)> = None; // (kept, total)
            'pass: loop {
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
                None => continue 'seg,
            };
            let sp_match = self.sync_points.iter().find(|(f, t)| {
                let mid_sp = (*f + *t) / 2;
                mid_sp >= from_us && mid_sp <= to_us
            });
            let (sp_from, sp_to) = match sp_match {
                Some(s) => *s,
                None => continue 'seg,
            };

            // estimated / raw sequences
            let mut est: Vec<(f64, [f64; 3])> = estimated_map
                .range(from_us..to_us)
                .filter_map(|(_, imu)| imu.gyro.map(|g| (imu.timestamp_ms, g)))
                .collect();
            if est.len() < 10 {
                continue 'seg;
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
                continue 'seg;
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
                continue 'seg;
            }

            // ── M1 axis-quality scan (sync-parallax-suppression) ────────
            // Coarse 25ms-step sweep of the full search window; per-axis
            // quality = max |r_axis| anywhere in the window. Healthy axes
            // (flow pattern not degenerate with parallax/foreground
            // translation, e.g. roll) keep r high; contaminated pan/tilt
            // axes stay low everywhere. Weights derived BEFORE any candidate
            // is evaluated — no self-reinforcement.
            let mut axis_q = [f64::NAN; 3];
            let axis_w: Option<[f64; 3]> = if axis_weighting {
                let mut q = [0.0f64; 3];
                let mut any = false;
                let scan_radius = self.sync_params.search_size;
                let n_steps = ((scan_radius * 2.0) / AXIS_SCAN_STEP_MS) as i32;
                for k in 0..=n_steps {
                    let off = initial_offset - scan_radius + (k as f64) * AXIS_SCAN_STEP_MS;
                    let (rx, ry, rz, _rm, n) =
                        crate::synchronization::sync_diag::compute_triaxis_correlation(
                            &est,
                            &raw_pairs,
                            off,
                            NEAREST_TOL_MS_V2,
                        );
                    if n >= 10 {
                        for (qi, r) in q.iter_mut().zip([rx, ry, rz]) {
                            if r.is_finite() {
                                *qi = qi.max(r.abs());
                                any = true;
                            }
                        }
                    }
                }
                if any {
                    axis_q = q;
                    let w = axis_weights_from_quality(q);
                    log::debug!(
                        "[axis-w] seg {}: q=[{:.3}/{:.3}/{:.3}] w=[{:.3}/{:.3}/{:.3}]",
                        i, q[0], q[1], q[2], w[0], w[1], w[2]
                    );
                    crate::synchronization::sync_diag::record_axis_weights(i, q, w);
                    Some(w)
                } else {
                    None
                }
            } else {
                None
            };
            let _ = axis_q; // surfaced via [axis-w] log + axis_weights.csv

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
                    axis_w,
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
                    continue 'seg;
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
            // (NEAREST_TOL_MS_V2 hoisted to fn scope — shared with the M1
            // axis-quality scan above.)
            const CLUSTER_MERGE_MS: f64 = 30.0;

            // M1: when axis weights are active every Pearson aggregation point
            // (single-point candidates, the full scan below, best_r_refined)
            // uses the weighted mean — healthy axes decide the shape match.
            let pearson_at = |offset_ms: f64| -> f64 {
                if !offset_ms.is_finite() {
                    return 0.0;
                }
                let (_, _, _, r, n) = match axis_w {
                    Some(w) => {
                        crate::synchronization::sync_diag::compute_triaxis_correlation_weighted(
                            &est,
                            &raw_pairs,
                            offset_ms,
                            NEAREST_TOL_MS_V2,
                            w,
                        )
                    }
                    None => crate::synchronization::sync_diag::compute_triaxis_correlation(
                        &est,
                        &raw_pairs,
                        offset_ms,
                        NEAREST_TOL_MS_V2,
                    ),
                };
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
            // Pre-ceiling values kept for the M2 cross-validation resolution:
            // when pass-2 independently lands in the same valley, the twin is
            // resolved and this confidence is restored (capped).
            let (pre_twin_conf, pre_twin_path) = (confidence, conf_path);
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

            // ── M2 pass-2 trigger (sync-parallax-suppression) ────────────
            // After pass 1 produced (output, confidence, twin): rebuild the
            // segment's problem with gyro-prior-filtered tracks and re-run
            // the body when ambiguity or low confidence indicates the rs
            // cost surface may be parallax-contaminated.
            let p2 = pass2_params();
            let p2_trigger: Option<&'static str> = if pass != 1 {
                None
            } else {
                match p2.mode {
                    Pass2Mode::Off => None,
                    Pass2Mode::Always => Some("always"),
                    Pass2Mode::On => {
                        if twin_info.is_some() {
                            Some("twin_ambiguity")
                        } else if confidence < p2.conf_thresh {
                            Some("low_confidence")
                        } else {
                            None
                        }
                    }
                }
            };
            let mut twin_clean_residuals: Option<(usize, usize)> = None;
            if let Some(reason) = p2_trigger {
                log::info!(
                    "[pass2] seg {}: trigger={} (pass1 output={:.1}ms conf={:.3}) — gyro-prior reweight attempt",
                    i,
                    reason,
                    output_ms,
                    confidence
                );
                let curve_backup = self.presync_curves.get(i).cloned().unwrap_or_default();
                match self.pass2_rebuild_segment(i, sp_from, sp_to, output_ms) {
                    Pass2Outcome::Rebuilt(rb) => {
                        log::info!(
                            "[pass2] seg {}: removed {}/{} rotation-model violators, re-running fusion",
                            i,
                            rb.total - rb.kept,
                            rb.total
                        );
                        pass1_snapshot = Some(Pass1Snapshot {
                            output_ms,
                            output_cost,
                            confidence,
                            conf_path,
                            pre_twin_conf,
                            pre_twin_path,
                            path_str: path_str_owned.clone(),
                            twin: twin_info,
                        });
                        pass1_curve_backup = Some(curve_backup);
                        pass2_note = Some((rb.kept, rb.total));
                        // Mimic full_sync's per-segment output so the body re-reads
                        // the pass-2 rs argmin from offsets[i] at the top.
                        offsets[i] = (mid_ms, rb.new_argmin_ext_ms, rb.new_cost, 0.5);
                        if i < curve_cache.len() {
                            curve_cache[i] = build_curve_entry(&self.presync_curves[i]);
                        }
                        pass = 2;
                        continue 'pass;
                    }
                    Pass2Outcome::CleanResiduals { removed, total } => {
                        twin_clean_residuals = Some((removed, total));
                    }
                    Pass2Outcome::NotApplicable => {}
                }
            }

            // ── M2 adoption rule: pass 2 replaces pass 1 only when strictly
            // more confident; otherwise restore the pass-1 problem state.
            // When both passes carry the twin ceiling but landed in the SAME
            // valley from independently filtered data, the ambiguity is
            // resolved by cross-validation (a genuine coin flip would land in
            // the other valley, ≥1.5×scan-step away) — the pre-twin
            // confidence is restored (capped) instead of dropping the point.
            let (output_ms, output_cost, confidence, conf_path, path_str_owned, twin_info) =
                if pass == 2 {
                    let snap = pass1_snapshot.take().expect("pass2 requires pass1 snapshot");
                    let (kept, total) = pass2_note.unwrap_or((0, 0));
                    // Adoption must compare NATURAL confidences: when pass 1
                    // was twin-ceiled, its 0.3 is an artificial handicap —
                    // comparing pass 2's un-ceiled conf against it let a
                    // wrong-basin pass-2 re-solve replace a correct pass-1
                    // output (observed: pass1 −0.0ms/pre-twin ~0.9 ceiled to
                    // 0.3, pass2 LBFGS jumped to the +10 valley, conf 0.810
                    // "won" → the exact disease this change exists to fix).
                    if adopt_pass2(snap.pre_twin_conf, confidence) {
                        log::info!(
                            "[pass2] seg {}: ADOPTED (kept {}/{}): offset {:.1} → {:.1}ms, conf {:.3} (pre-twin {:.3}) → {:.3}",
                            i, kept, total, snap.output_ms, output_ms, snap.confidence, snap.pre_twin_conf, confidence
                        );
                        (
                            output_ms,
                            output_cost,
                            confidence,
                            conf_path,
                            format!("{}|pass2", path_str_owned),
                            twin_info,
                        )
                    } else {
                        // Keep pass-1 output: restore the pass-1 problem state.
                        if let Some(c) = pass1_curve_backup.take() {
                            if i < self.presync_curves.len() {
                                self.presync_curves[i] = c;
                            }
                        }
                        if let Some(range_idx) =
                            self.sync_points.iter().position(|sp| *sp == (sp_from, sp_to))
                        {
                            self.restore_segment_tracks(range_idx);
                        }
                        if i < curve_cache.len() && i < self.presync_curves.len() {
                            curve_cache[i] = build_curve_entry(&self.presync_curves[i]);
                        }
                        let resolve_ms = twin_params.resolve_dist_ms;
                        let agree_d = (output_ms - snap.output_ms).abs();
                        let twin_handicapped =
                            snap.twin.is_some() && snap.pre_twin_conf > snap.confidence;
                        if twin_handicapped && resolve_ms > 0.0 && agree_d <= resolve_ms {
                            let restored_conf = snap.pre_twin_conf.min(TWIN_RESOLVED_CONF_CAP);
                            log::info!(
                                "[pass2] seg {}: twin RESOLVED by cross-validation (pass1 {:.1}ms vs pass2 {:.1}ms, d={:.1} ≤ {:.1}ms) — conf {:.3} → {:.3}",
                                i, snap.output_ms, output_ms, agree_d, resolve_ms, snap.confidence, restored_conf
                            );
                            (
                                snap.output_ms,
                                snap.output_cost,
                                restored_conf,
                                snap.pre_twin_path,
                                format!("{}|twin_resolved(d={:.1}ms)", snap.path_str, agree_d),
                                snap.twin,
                            )
                        } else if twin_handicapped && resolve_ms > 0.0 {
                            // The two passes landed in DIFFERENT valleys.
                            // Keep pass-1 (full-data fusion, the reference
                            // everywhere else) at a reduced cap — never let
                            // an unstable re-solve overwrite it. (An earlier
                            // Pearson-anchored arbitration was dropped: with
                            // the anchor sitting mid-way between valleys it
                            // picked the wrong side in live testing.)
                            let arb_conf = snap.pre_twin_conf.min(TWIN_ARBITRATED_CONF_CAP);
                            log::info!(
                                "[pass2] seg {}: twin UNRESOLVED (passes disagree: pass1 {:.1}ms vs pass2 {:.1}ms, d={:.1} > {:.1}ms) — keeping pass1 at conf {:.3}",
                                i, snap.output_ms, output_ms, agree_d, resolve_ms, arb_conf
                            );
                            (
                                snap.output_ms,
                                snap.output_cost,
                                arb_conf,
                                snap.pre_twin_path,
                                format!("{}|twin_unresolved(d={:.1}ms)", snap.path_str, agree_d),
                                snap.twin,
                            )
                        } else {
                            log::info!(
                                "[pass2] seg {}: NOT adopted (pass2 {:.1}ms conf {:.3} ≤ pass1 {:.1}ms pre-twin conf {:.3}, kept {}/{}) — keeping pass1",
                                i, output_ms, confidence, snap.output_ms, snap.pre_twin_conf, kept, total
                            );
                            (
                                snap.output_ms,
                                snap.output_cost,
                                snap.confidence,
                                snap.conf_path,
                                format!("{}|pass2_rejected", snap.path_str),
                                snap.twin,
                            )
                        }
                    }
                } else if let Some((removed, total)) = twin_clean_residuals {
                    // Twin fired but the gyro-prior residual analysis found
                    // (almost) nothing violating the rotation model — the rs
                    // twin is noise-intrinsic, not parallax, and the
                    // correlation-led consensus stands. Restore confidence.
                    if twin_info.is_some() && pre_twin_conf > confidence {
                        let restored = pre_twin_conf.min(TWIN_RESOLVED_CONF_CAP);
                        log::info!(
                            "[pass2] seg {}: twin CLEAN residuals ({}/{} removable) — rotation model holds, restoring conf {:.3} → {:.3}",
                            i, removed, total, confidence, restored
                        );
                        (
                            output_ms,
                            output_cost,
                            restored,
                            pre_twin_path,
                            format!("{}|twin_clean_residuals", path_str_owned),
                            twin_info,
                        )
                    } else {
                        (output_ms, output_cost, confidence, conf_path, path_str_owned, twin_info)
                    }
                } else {
                    (output_ms, output_cost, confidence, conf_path, path_str_owned, twin_info)
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
            break 'pass;
            } // 'pass loop (M2 two-pass body)
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
mod pass2_tests {
    use super::*;

    #[test]
    fn pass2_mode_parse_tristate() {
        assert_eq!(Pass2Mode::parse("0"), Some(Pass2Mode::Off));
        assert_eq!(Pass2Mode::parse("off"), Some(Pass2Mode::Off));
        assert_eq!(Pass2Mode::parse("1"), Some(Pass2Mode::On));
        assert_eq!(Pass2Mode::parse("ALWAYS"), Some(Pass2Mode::Always));
        assert_eq!(Pass2Mode::parse("junk"), None);
    }

    #[test]
    fn adoption_rule_requires_strictly_higher_finite_conf() {
        assert!(adopt_pass2(0.3, 0.8));
        assert!(!adopt_pass2(0.8, 0.8)); // tie keeps pass1
        assert!(!adopt_pass2(0.8, 0.3));
        assert!(!adopt_pass2(0.3, f64::NAN));
    }

    #[test]
    fn mad_threshold_separates_parallax_band() {
        // Bulk spread over 0.02-0.08° (rotation-consistent noise incl. a
        // 0.09°-scale systematic shift from a wrong-valley pass-1 offset),
        // outliers at 0.5° (parallax). Gate must sit between the bands.
        let mut residuals: Vec<f64> = (0..90).map(|i| 0.02 + 0.06 * ((i % 10) as f64) / 9.0).collect();
        for r in residuals.iter_mut().take(8) {
            *r = 0.5;
        }
        let t = pass2_threshold(&residuals, 4.0);
        assert!(t > 0.09, "true points must survive, t={}", t);
        assert!(t < 0.5, "parallax points must be cut, t={}", t);
        let keep = pass2_keep_indices(&residuals, t, 10);
        assert_eq!(keep.len(), 90 - 8);
        assert!(!keep.contains(&0));
        assert!(keep.contains(&89));
    }

    #[test]
    fn mad_threshold_degenerate_inputs_filter_nothing() {
        assert!(pass2_threshold(&[0.1; 5], 4.0).is_infinite());
        let keep = pass2_keep_indices(&[0.1; 5], f64::INFINITY, 10);
        assert_eq!(keep.len(), 5);
    }

    #[test]
    fn keep_indices_degrades_to_min_keep_lowest() {
        // Gate so tight only 3 pass → degrade to the 10 lowest residuals.
        let residuals: Vec<f64> = (0..30).map(|i| 0.01 * (i as f64 + 1.0)).collect();
        let keep = pass2_keep_indices(&residuals, 0.035, 10);
        assert_eq!(keep, (0..10).collect::<Vec<_>>());
    }

    /// Oscillating rotation (time-VARYING angular velocity). Constant-ω data
    /// is useless here: relative rotation over a fixed window is then
    /// time-shift invariant, so residuals would not react to offset error —
    /// the same reason real sync needs varying motion.
    fn make_oscillating_quats(amp_deg: f64, freq_hz: f64) -> TimeQuat {
        let mut quats = TimeQuat::new();
        let axis = Vector3::new(0.3f64, 1.0, -0.2).normalize();
        let mut t_ms = -1000i64;
        while t_ms <= 3000 {
            let t_s = t_ms as f64 / 1000.0;
            let angle = amp_deg.to_radians() * (std::f64::consts::TAU * freq_hz * t_s).sin();
            quats.insert(t_ms * 1000, Quat64::from_scaled_axis(axis * angle));
            t_ms += 1;
        }
        quats
    }

    /// Build a pair whose rays are exactly rotation-consistent with the quats
    /// (points at infinity): ray(t) = (q(t)·rot_PI_x)⁻¹ · v_world.
    fn make_consistent_pair(quats: &TimeQuat, ts_a: f64, ts_b: f64, n: usize) -> PairTracks {
        let rot = Quat64::from_scaled_axis(Vector3::new(PI, 0.0, 0.0));
        let q_a = GyroSource::clamped_quat_at_gyro_timestamp(quats, ts_a * 1000.0);
        let q_b = GyroSource::clamped_quat_at_gyro_timestamp(quats, ts_b * 1000.0);
        let mut pair = PairTracks {
            timestamp_us: (ts_a * 1e6) as i64,
            tss_a: Vec::new(),
            tss_b: Vec::new(),
            rays_a: Vec::new(),
            rays_b: Vec::new(),
        };
        for k in 0..n {
            let v = Vector3::new(
                (k as f64 * 0.37).sin() * 0.4,
                (k as f64 * 0.73).cos() * 0.4,
                1.0,
            )
            .normalize();
            let ra = (q_a * rot).inverse().transform_vector(&v);
            let rb = (q_b * rot).inverse().transform_vector(&v);
            pair.tss_a.push(ts_a);
            pair.tss_b.push(ts_b);
            pair.rays_a.push((ra.x, ra.y, ra.z));
            pair.rays_b.push((rb.x, rb.y, rb.z));
        }
        pair
    }

    #[test]
    fn residuals_zero_for_rotation_consistent_rays() {
        let quats = make_oscillating_quats(20.0, 1.0);
        let pair = make_consistent_pair(&quats, 1.0, 1.04, 20);
        // frt term passed as 0 so offset 0 ⇔ internal delay 0.
        let r = rotation_residuals_deg(&quats, &pair, 0.0, 0.0);
        assert_eq!(r.len(), 20);
        for v in &r {
            assert!(*v < 0.01, "residual {} should be ≈0", v);
        }
    }

    /// Residual grows monotonically with offset error — same trend as the
    /// rs-sync cost surface around the true alignment.
    #[test]
    fn residuals_grow_with_offset_error_like_rs_cost() {
        let quats = make_oscillating_quats(20.0, 1.0);
        let pair = make_consistent_pair(&quats, 1.0, 1.04, 20);
        let mean = |off: f64| -> f64 {
            let r = rotation_residuals_deg(&quats, &pair, off, 0.0);
            r.iter().sum::<f64>() / r.len() as f64
        };
        let r0 = mean(0.0);
        let r25 = mean(25.0);
        let r50 = mean(50.0);
        assert!(r0 < r25 && r25 < r50, "r0={} r25={} r50={}", r0, r25, r50);
        assert!(r50 > 0.01 && r50 < 10.0, "r50={}", r50);
    }

    /// Parallax-like rays (violating the rotation model) stand out from
    /// rotation-consistent ones even when pass-1 is 10ms off the truth —
    /// the spec scenario's robustness argument (0.09° ≪ gate ≪ 0.2-1°).
    #[test]
    fn parallax_points_filtered_at_wrong_pass1_offset() {
        // Peak |ω| = amp·2πf ≈ 25°/s — same order as the C50 segments.
        let quats = make_oscillating_quats(4.0, 1.0);
        let mut pair = make_consistent_pair(&quats, 1.0, 1.04, 80);
        // Inject 12 parallax points: bend ray_b by 0.5°.
        let bend = Quat64::from_scaled_axis(Vector3::new(0.0, 0.5f64.to_radians(), 0.0));
        for k in 0..12 {
            let (x, y, z) = pair.rays_b[k];
            let v = bend.transform_vector(&Vector3::new(x, y, z));
            pair.rays_b[k] = (v.x, v.y, v.z);
        }
        // Pass-1 offset 10ms off the truth.
        let residuals = rotation_residuals_deg(&quats, &pair, 10.0, 0.0);
        let t = pass2_threshold(&residuals, 4.0);
        let keep = pass2_keep_indices(&residuals, t, 10);
        for k in 0..12 {
            assert!(!keep.contains(&k), "parallax point {} must be cut", k);
        }
        assert!(keep.len() >= 60, "true points must survive, kept {}", keep.len());
    }
}

#[cfg(test)]
mod axis_weight_tests {
    use super::axis_weights_from_quality;

    /// Design D2 numbers (C50 bad window): q_x=0.26, q_y=0.78, q_z=0.955.
    /// Squared: 0.0676 / 0.6084 / 0.912 → w_z : w_x ≈ 0.912 : 0.068 (≈13.5×).
    #[test]
    fn c50_bad_window_z_dominates() {
        let w = axis_weights_from_quality([0.26, 0.78, 0.955]);
        assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(w[2] > w[1] && w[1] > w[0]);
        let ratio = w[2] / w[0];
        assert!((ratio - 0.912025 / 0.0676).abs() < 0.1, "ratio={}", ratio);
    }

    /// All-weak segment: every q² < floor → exact equal weights, so the
    /// weighted aggregate degenerates to the legacy equal-weight mean.
    #[test]
    fn all_weak_floors_to_equal_weights() {
        let w = axis_weights_from_quality([0.05, 0.08, 0.09]);
        for wi in w {
            assert!((wi - 1.0 / 3.0).abs() < 1e-12);
        }
    }

    /// NaN / out-of-range qualities are clamped, never poison the weights.
    #[test]
    fn nan_and_overrange_quality_handled() {
        let w = axis_weights_from_quality([f64::NAN, 1.7, -0.3]);
        assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(w.iter().all(|wi| wi.is_finite() && *wi > 0.0));
        // NaN → floor; 1.7 → clamp(1)² = 1; −0.3 → floor.
        assert!((w[1] - 1.0 / 1.1).abs() < 1e-12);
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

#[cfg(test)]
mod auto_bypass_tests {
    use super::should_auto_bypass_fusion;

    #[test]
    fn large_initial_offset_bypasses() {
        // The 2026-06-11 field case: essential pre-pass found -204692.5ms,
        // search clamped to 3000ms by calc_initial_fast.
        assert!(should_auto_bypass_fusion(-204692.5, 3000.0, false));
        assert!(should_auto_bypass_fusion(204692.5, 3000.0, false));
    }

    #[test]
    fn small_initial_offset_keeps_fusion() {
        assert!(!should_auto_bypass_fusion(0.0, 5000.0, false));
        assert!(!should_auto_bypass_fusion(-1800.0, 5000.0, false));
        // Boundary: equal magnitude does NOT bypass (fusion window still
        // marginally covers the truth neighborhood).
        assert!(!should_auto_bypass_fusion(3000.0, 3000.0, false));
    }

    #[test]
    fn env_disable_forces_fusion() {
        assert!(!should_auto_bypass_fusion(-204692.5, 3000.0, true));
    }
}
