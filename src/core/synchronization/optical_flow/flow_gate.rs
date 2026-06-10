// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

//! Flow quality gate for the sync optical-flow front-end.
//!
//! Provides forward-backward (FB) consistency filtering, spatially stratified
//! point capping and per-pair quality statistics for the point pairs produced
//! by `optical_flow_to` implementations (DIS / NeuFlow).
//!
//! The gate runs *inside* the optical-flow layer so the `OpticalFlowPair`
//! contract is unchanged — both downstream consumers (rs-sync ray cost and
//! findEssentialMat pose estimation) benefit without interface changes.
//!
//! Rollback: `GYROFLOW_SYNC_FB_CHECK=0` disables the FB computation entirely;
//! `GYROFLOW_SYNC_OF_STEP_DIV=15` restores the legacy DIS sampling grid.

use parking_lot::Mutex;
use std::sync::OnceLock;

/// Default sampling-grid divisor for the DIS path (legacy value was 15).
pub const DEFAULT_STEP_DIV_DIS: u32 = 40;
/// Default sampling-grid divisor for the NeuFlow path (legacy value was 18).
pub const DEFAULT_STEP_DIV_NEUFLOW: u32 = 32;
/// Below this many FB-passing points the gate degrades to top-K by residual.
pub const MIN_POINTS: usize = 20;
/// Number of points kept (best residuals first) in the degraded mode.
pub const TOPK_FALLBACK: usize = 40;

#[derive(Debug, Clone, Copy)]
pub struct GateParams {
    /// Compute + apply FB consistency check (`GYROFLOW_SYNC_FB_CHECK`, default true).
    pub fb_check: bool,
    /// Absolute FB residual threshold in px of the flow resolution (`GYROFLOW_SYNC_FB_ABS_PX`).
    pub fb_abs_px: f32,
    /// Relative FB residual threshold coefficient on |flow| (`GYROFLOW_SYNC_FB_REL`).
    pub fb_rel: f32,
    /// Optional override of the sampling-grid divisor for ALL paths (`GYROFLOW_SYNC_OF_STEP_DIV`).
    pub step_div_override: Option<u32>,
    /// Stratified cap on output points (`GYROFLOW_SYNC_OF_MAX_POINTS`).
    pub max_points: usize,
}

impl GateParams {
    /// Effective grid divisor for a path with the given default.
    pub fn step_div(&self, path_default: u32) -> u32 {
        self.step_div_override.unwrap_or(path_default).max(1)
    }
    /// FB residual threshold for a point with flow magnitude `flow_mag`.
    pub fn fb_threshold(&self, flow_mag: f32) -> f32 {
        self.fb_abs_px.max(self.fb_rel * flow_mag)
    }
}

const DEFAULT_PARAMS: GateParams = GateParams {
    fb_check: true,
    fb_abs_px: 0.8,
    fb_rel: 0.05,
    step_div_override: None,
    max_points: 200,
};

static RESOLVED: OnceLock<GateParams> = OnceLock::new();
static LOGGED: OnceLock<()> = OnceLock::new();

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
fn parse_f32_pos(raw: &str) -> Option<f32> {
    raw.trim().parse::<f32>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}
fn parse_u32_pos(raw: &str) -> Option<u32> {
    raw.trim().parse::<u32>().ok().filter(|v| *v >= 1)
}
fn parse_usize_pos(raw: &str) -> Option<usize> {
    raw.trim().parse::<usize>().ok().filter(|v| *v >= 1)
}

fn resolve_inner() -> (GateParams, Vec<&'static str>) {
    let mut p = DEFAULT_PARAMS;
    let mut overrides: Vec<&'static str> = Vec::new();
    let mut read = |name: &'static str, apply: &mut dyn FnMut(&str) -> bool| {
        if let Ok(raw) = std::env::var(name) {
            if !raw.is_empty() {
                if apply(&raw) {
                    overrides.push(name);
                } else {
                    log::warn!(target: "lifecycle", "{}={} invalid, falling back to default", name, raw);
                }
            }
        }
    };
    read("GYROFLOW_SYNC_FB_CHECK", &mut |raw| {
        parse_bool(raw).map(|v| p.fb_check = v).is_some()
    });
    read("GYROFLOW_SYNC_FB_ABS_PX", &mut |raw| {
        parse_f32_pos(raw).map(|v| p.fb_abs_px = v).is_some()
    });
    read("GYROFLOW_SYNC_FB_REL", &mut |raw| {
        parse_f32_pos(raw).map(|v| p.fb_rel = v).is_some()
    });
    read("GYROFLOW_SYNC_OF_STEP_DIV", &mut |raw| {
        parse_u32_pos(raw).map(|v| p.step_div_override = Some(v)).is_some()
    });
    read("GYROFLOW_SYNC_OF_MAX_POINTS", &mut |raw| {
        parse_usize_pos(raw).map(|v| p.max_points = v).is_some()
    });
    (p, overrides)
}

/// Resolve gate params from env vars (cached after first call; restart to change).
pub fn params() -> GateParams {
    let p = *RESOLVED.get_or_init(|| {
        let (p, overrides) = resolve_inner();
        if LOGGED.set(()).is_ok() {
            log::info!(
                target: "lifecycle",
                "flow_gate resolved fb_check={} fb_abs_px={:.2} fb_rel={:.3} step_div={} max_points={} source={}",
                p.fb_check,
                p.fb_abs_px,
                p.fb_rel,
                p.step_div_override.map(|v| v.to_string()).unwrap_or_else(|| "default".into()),
                p.max_points,
                if overrides.is_empty() { "default".to_string() } else { format!("env[{}]", overrides.join(",")) }
            );
        }
        p
    });
    p
}

// ── FB consistency ─────────────────────────────────────────────────────────

/// Per-pair forward-backward roundtrip residuals.
///
/// For each pair `(p, q)` with `q = p + f(p)`, queries the backward flow at q
/// via `backward_at(qx, qy)` and computes `|q + b(q) - p|`. Pairs whose
/// backward flow cannot be sampled (out of bounds / non-finite) get
/// `f32::INFINITY` so they always fail the threshold.
pub fn fb_residuals(
    pts_a: &[(f32, f32)],
    pts_b: &[(f32, f32)],
    backward_at: &mut dyn FnMut(f32, f32) -> Option<(f32, f32)>,
) -> Vec<f32> {
    pts_a
        .iter()
        .zip(pts_b.iter())
        .map(|(p, q)| match backward_at(q.0, q.1) {
            Some((bx, by)) if bx.is_finite() && by.is_finite() => {
                let rx = q.0 + bx - p.0;
                let ry = q.1 + by - p.1;
                (rx * rx + ry * ry).sqrt()
            }
            _ => f32::INFINITY,
        })
        .collect()
}

/// FB filter: residuals + threshold pass-indices in one call.
/// Returns `(pass_indices, residual_per_input_pair)`.
pub fn fb_filter(
    pts_a: &[(f32, f32)],
    pts_b: &[(f32, f32)],
    backward_at: &mut dyn FnMut(f32, f32) -> Option<(f32, f32)>,
    params: &GateParams,
) -> (Vec<usize>, Vec<f32>) {
    let residuals = fb_residuals(pts_a, pts_b, backward_at);
    let pass = (0..pts_a.len())
        .filter(|&i| {
            let fx = pts_b[i].0 - pts_a[i].0;
            let fy = pts_b[i].1 - pts_a[i].1;
            residuals[i] <= params.fb_threshold((fx * fx + fy * fy).sqrt())
        })
        .collect();
    (pass, residuals)
}

// ── Degradation decision ───────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateOutcome {
    /// Enough FB-passing points: enforce threshold + stratified cap.
    Enforce,
    /// FB pass too thin but texture pass is healthy: keep top-K by residual.
    DegradeTopK,
    /// Too few textured candidates: produce no pair (matches legacy behavior).
    Reject,
}

pub fn degradation_outcome(texture_pass: usize, fb_pass: usize) -> GateOutcome {
    if texture_pass < 10 {
        GateOutcome::Reject
    } else if fb_pass < MIN_POINTS {
        GateOutcome::DegradeTopK
    } else {
        GateOutcome::Enforce
    }
}

/// Indices of the `k` smallest residuals (tie-break: candidate index),
/// returned in ascending index order. Used by the DegradeTopK path.
pub fn topk_by_residual(residuals: &[f32], k: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..residuals.len()).collect();
    idx.sort_by(|&a, &b| {
        residuals[a]
            .partial_cmp(&residuals[b])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    idx.truncate(k);
    idx.sort_unstable();
    idx
}

// ── Stratified cap ─────────────────────────────────────────────────────────

const CAP_GRID: usize = 4;

/// Spatially stratified cap over a 4×4 cell grid.
///
/// Each cell gets a quota of `ceil(max_points / 16)` selected by ascending FB
/// residual (tie-break: candidate index). Unused quota is backfilled globally
/// by residual. The result is returned in ascending index order so the output
/// point ordering is deterministic and stable across runs.
pub fn stratified_cap(
    pts_a: &[(f32, f32)],
    residuals: &[f32],
    eligible: &[usize],
    frame_size: (u32, u32),
    max_points: usize,
) -> Vec<usize> {
    if eligible.len() <= max_points {
        let mut out = eligible.to_vec();
        out.sort_unstable();
        return out;
    }
    let cell_w = (frame_size.0.max(1) as f32 / CAP_GRID as f32).max(1.0);
    let cell_h = (frame_size.1.max(1) as f32 / CAP_GRID as f32).max(1.0);
    let quota = max_points.div_ceil(CAP_GRID * CAP_GRID);

    let by_residual = |&a: &usize, &b: &usize| {
        residuals[a]
            .partial_cmp(&residuals[b])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    };

    let mut cells: Vec<Vec<usize>> = vec![Vec::new(); CAP_GRID * CAP_GRID];
    for &i in eligible {
        let cx = ((pts_a[i].0 / cell_w) as usize).min(CAP_GRID - 1);
        let cy = ((pts_a[i].1 / cell_h) as usize).min(CAP_GRID - 1);
        cells[cy * CAP_GRID + cx].push(i);
    }

    let mut selected: Vec<usize> = Vec::with_capacity(max_points + CAP_GRID * CAP_GRID);
    let mut leftover: Vec<usize> = Vec::new();
    for cell in &mut cells {
        cell.sort_by(by_residual);
        let take = quota.min(cell.len());
        selected.extend_from_slice(&cell[..take]);
        leftover.extend_from_slice(&cell[take..]);
    }
    if selected.len() < max_points {
        leftover.sort_by(by_residual);
        let need = max_points - selected.len();
        selected.extend(leftover.into_iter().take(need));
    } else if selected.len() > max_points {
        // ceil quota × 16 can exceed max_points — trim worst residuals.
        selected.sort_by(by_residual);
        selected.truncate(max_points);
    }
    selected.sort_unstable();
    selected
}

// ── Gate application ───────────────────────────────────────────────────────

pub struct GateSelection {
    /// Selected candidate indices, ascending (deterministic output order).
    pub indices: Vec<usize>,
    /// True when the DegradeTopK fallback was taken (FB pass too thin).
    pub degraded: bool,
}

/// Apply the full gate decision to a texture-passed candidate set.
///
/// `fb_pass` / `residuals` come from [`fb_filter`]. When `params.fb_check` is
/// false the FB outcome is ignored and only the stratified cap applies
/// (uniform ranking → index order), so `FB_CHECK=0` + legacy step density is
/// point-for-point equivalent to the pre-gate pipeline (cap is a passthrough
/// below `max_points`).
pub fn apply_gate(
    pts_a: &[(f32, f32)],
    fb_pass: &[usize],
    residuals: &[f32],
    frame_size: (u32, u32),
    params: &GateParams,
) -> GateSelection {
    if !params.fb_check {
        let all: Vec<usize> = (0..pts_a.len()).collect();
        let zeros = vec![0.0f32; pts_a.len()];
        return GateSelection {
            indices: stratified_cap(pts_a, &zeros, &all, frame_size, params.max_points),
            degraded: false,
        };
    }
    match degradation_outcome(pts_a.len(), fb_pass.len()) {
        GateOutcome::Reject => GateSelection {
            // Too few textured candidates — pass through unchanged; the
            // caller's legacy `>= 10` check turns this into a None pair.
            indices: (0..pts_a.len()).collect(),
            degraded: false,
        },
        GateOutcome::DegradeTopK => GateSelection {
            indices: topk_by_residual(residuals, TOPK_FALLBACK),
            degraded: true,
        },
        GateOutcome::Enforce => GateSelection {
            indices: stratified_cap(pts_a, residuals, fb_pass, frame_size, params.max_points),
            degraded: false,
        },
    }
}

// ── Per-pair stats + run-level accumulator ─────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct GateStats {
    pub candidates: usize,
    pub texture_pass: usize,
    pub fb_pass: usize,
    pub kept: usize,
    pub fb_p50: f32,
    pub fb_p95: f32,
    pub degraded: bool,
}

/// Nearest-rank percentile over the finite residuals. NaN when none are finite.
pub fn residual_percentiles(residuals: &[f32]) -> (f32, f32) {
    let mut finite: Vec<f32> = residuals.iter().copied().filter(|r| r.is_finite()).collect();
    if finite.is_empty() {
        return (f32::NAN, f32::NAN);
    }
    finite.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pick = |p: f32| -> f32 {
        let rank = ((p * finite.len() as f32).ceil() as usize).clamp(1, finite.len());
        finite[rank - 1]
    };
    (pick(0.50), pick(0.95))
}

#[derive(Default)]
struct Accum {
    pairs: u64,
    degraded_pairs: u64,
    pairs_dis: u64,
    pairs_ort: u64,
    pairs_burn: u64,
    sum_candidates: u64,
    sum_texture: u64,
    sum_fb: u64,
    sum_kept: u64,
    sum_p50: f64,
    sum_p95: f64,
    percentile_pairs: u64,
}

static ACCUM: Mutex<Accum> = Mutex::new(Accum {
    pairs: 0,
    degraded_pairs: 0,
    pairs_dis: 0,
    pairs_ort: 0,
    pairs_burn: 0,
    sum_candidates: 0,
    sum_texture: 0,
    sum_fb: 0,
    sum_kept: 0,
    sum_p50: 0.0,
    sum_p95: 0.0,
    percentile_pairs: 0,
});

/// Record one pair's gate stats: per-pair debug log, run accumulator and
/// (when `GYROFLOW_SYNC_DIAG=1`) a flow_quality.csv row.
pub fn record_pair_stats(method: &'static str, pair_ts_us: i64, stats: &GateStats) {
    log::debug!(
        target: "sync",
        "[flow-gate] ts={} method={} cand={} tex={} fb={} kept={} fb_p50={:.2} fb_p95={:.2}{}",
        pair_ts_us,
        method,
        stats.candidates,
        stats.texture_pass,
        stats.fb_pass,
        stats.kept,
        stats.fb_p50,
        stats.fb_p95,
        if stats.degraded { " degraded" } else { "" }
    );
    if stats.degraded {
        log::warn!(
            target: "sync",
            "[flow-gate] ts={} method={} degraded: fb_pass={} < {} with texture_pass={} — keeping top-{} by FB residual",
            pair_ts_us,
            method,
            stats.fb_pass,
            MIN_POINTS,
            stats.texture_pass,
            stats.kept
        );
    }
    {
        let mut a = ACCUM.lock();
        a.pairs += 1;
        if stats.degraded {
            a.degraded_pairs += 1;
        }
        match method {
            "dis" => a.pairs_dis += 1,
            "ort" => a.pairs_ort += 1,
            "burn" => a.pairs_burn += 1,
            _ => {}
        }
        a.sum_candidates += stats.candidates as u64;
        a.sum_texture += stats.texture_pass as u64;
        a.sum_fb += stats.fb_pass as u64;
        a.sum_kept += stats.kept as u64;
        if stats.fb_p50.is_finite() && stats.fb_p95.is_finite() {
            a.sum_p50 += stats.fb_p50 as f64;
            a.sum_p95 += stats.fb_p95 as f64;
            a.percentile_pairs += 1;
        }
    }
    crate::synchronization::sync_diag::record_flow_quality(
        pair_ts_us,
        method,
        stats.candidates,
        stats.texture_pass,
        stats.fb_pass,
        stats.kept,
        stats.fb_p50,
        stats.fb_p95,
        stats.degraded,
    );
}

/// Reset the run-level accumulator. Called at sync start.
pub fn reset_stats() {
    *ACCUM.lock() = Accum::default();
}

/// Emit one info-level run summary line and reset. Called at sync end.
pub fn dump_and_reset_stats() {
    let a = std::mem::take(&mut *ACCUM.lock());
    if a.pairs == 0 {
        return;
    }
    let avg = |sum: u64| sum as f64 / a.pairs as f64;
    let p_avg = |sum: f64| {
        if a.percentile_pairs > 0 {
            sum / a.percentile_pairs as f64
        } else {
            f64::NAN
        }
    };
    log::info!(
        target: "sync",
        "[flow-gate] run summary: pairs={} (dis={} ort={} burn={}) degraded={} cand_avg={:.0} tex_avg={:.0} fb_avg={:.0} kept_avg={:.0} fb_p50_avg={:.2} fb_p95_avg={:.2}",
        a.pairs,
        a.pairs_dis,
        a.pairs_ort,
        a.pairs_burn,
        a.degraded_pairs,
        avg(a.sum_candidates),
        avg(a.sum_texture),
        avg(a.sum_fb),
        avg(a.sum_kept),
        p_avg(a.sum_p50),
        p_avg(a.sum_p95),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_params() -> GateParams {
        GateParams {
            fb_check: true,
            fb_abs_px: 0.8,
            fb_rel: 0.05,
            step_div_override: None,
            max_points: 200,
        }
    }

    #[test]
    fn fb_threshold_abs_floor_and_rel_growth() {
        let p = test_params();
        // Small flow: absolute floor dominates.
        assert_eq!(p.fb_threshold(1.0), 0.8);
        // Large flow: relative term dominates (0.05 × 30 = 1.5).
        assert!((p.fb_threshold(30.0) - 1.5).abs() < 1e-6);
    }

    #[test]
    fn fb_filter_keeps_consistent_rejects_outlier_and_oob() {
        let p = test_params();
        // Three pairs: consistent, outlier (5px roundtrip), out-of-bounds lookup.
        let pts_a = vec![(10.0, 10.0), (50.0, 50.0), (90.0, 90.0)];
        let pts_b = vec![(12.0, 11.0), (55.0, 55.0), (95.0, 95.0)];
        let mut backward = |qx: f32, _qy: f32| -> Option<(f32, f32)> {
            if qx < 60.0 {
                if qx < 20.0 {
                    Some((-2.1, -0.95)) // roundtrip ≈ (12-2.1, 11-0.95) → residual ≈ 0.11
                } else {
                    Some((-1.0, -1.0)) // roundtrip (54, 54) vs (50, 50) → residual ≈ 5.66
                }
            } else {
                None // out of bounds
            }
        };
        let (pass, residuals) = fb_filter(&pts_a, &pts_b, &mut backward, &p);
        assert_eq!(pass, vec![0]);
        assert!(residuals[0] < 0.2);
        assert!(residuals[1] > 5.0);
        assert!(residuals[2].is_infinite());
    }

    #[test]
    fn fb_filter_relative_term_admits_large_motion() {
        let p = test_params();
        // |f| = 30px, roundtrip residual 1.2px → threshold 1.5px → pass.
        let pts_a = vec![(10.0, 10.0)];
        let pts_b = vec![(40.0, 10.0)];
        let mut backward = |_qx: f32, _qy: f32| Some((-28.8, 0.0));
        let (pass, residuals) = fb_filter(&pts_a, &pts_b, &mut backward, &p);
        assert!((residuals[0] - 1.2).abs() < 1e-4);
        assert_eq!(pass, vec![0]);
    }

    #[test]
    fn degradation_outcomes() {
        assert_eq!(degradation_outcome(5, 3), GateOutcome::Reject);
        assert_eq!(degradation_outcome(150, 8), GateOutcome::DegradeTopK);
        assert_eq!(degradation_outcome(150, 20), GateOutcome::Enforce);
    }

    #[test]
    fn topk_by_residual_orders_and_truncates() {
        let residuals = vec![3.0, 0.5, f32::INFINITY, 0.5, 1.0];
        // Ties on 0.5 break by index: 1 before 3.
        assert_eq!(topk_by_residual(&residuals, 3), vec![1, 3, 4]);
        assert_eq!(topk_by_residual(&residuals, 10), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn stratified_cap_passthrough_below_limit() {
        let pts: Vec<(f32, f32)> = (0..50).map(|i| (i as f32, i as f32)).collect();
        let residuals = vec![0.1; 50];
        let eligible: Vec<usize> = (0..50).collect();
        let out = stratified_cap(&pts, &residuals, &eligible, (100, 100), 200);
        assert_eq!(out, eligible);
    }

    #[test]
    fn stratified_cap_prevents_clustering() {
        // 500 points clustered in the top-left cell + 15 spread elsewhere.
        let mut pts: Vec<(f32, f32)> = (0..500)
            .map(|i| ((i % 20) as f32, (i / 20) as f32)) // all within (0..20, 0..25) → cell (0,0) of 400×400
            .collect();
        let spread: Vec<(f32, f32)> = (1..16)
            .map(|c| {
                let cx = (c % CAP_GRID) as f32 * 100.0 + 50.0;
                let cy = (c / CAP_GRID) as f32 * 100.0 + 50.0;
                (cx, cy)
            })
            .collect();
        pts.extend_from_slice(&spread);
        let residuals = vec![0.1; pts.len()];
        let eligible: Vec<usize> = (0..pts.len()).collect();
        let out = stratified_cap(&pts, &residuals, &eligible, (400, 400), 200);
        assert_eq!(out.len(), 200);
        // Every spread point (one per non-corner cell) must survive the cap.
        for i in 500..pts.len() {
            assert!(out.contains(&i), "spread point {} should be kept", i);
        }
        // The clustered cell must not exceed the backfilled remainder
        // (200 - 15 spread = 185 from cluster).
        let cluster_kept = out.iter().filter(|&&i| i < 500).count();
        assert_eq!(cluster_kept, 185);
    }

    #[test]
    fn stratified_cap_deterministic_with_ties() {
        let pts: Vec<(f32, f32)> = (0..400)
            .map(|i| ((i % 40) as f32 * 10.0, (i / 40) as f32 * 40.0))
            .collect();
        let residuals = vec![0.25; 400]; // all ties → index tie-break
        let eligible: Vec<usize> = (0..400).collect();
        let a = stratified_cap(&pts, &residuals, &eligible, (400, 400), 100);
        let b = stratified_cap(&pts, &residuals, &eligible, (400, 400), 100);
        assert_eq!(a, b);
        assert_eq!(a.len(), 100);
    }

    #[test]
    fn residual_percentiles_basic_and_nonfinite() {
        let r: Vec<f32> = (1..=100).map(|i| i as f32).collect();
        let (p50, p95) = residual_percentiles(&r);
        assert_eq!(p50, 50.0);
        assert_eq!(p95, 95.0);
        let with_inf = vec![1.0, f32::INFINITY, 3.0];
        let (p50, _) = residual_percentiles(&with_inf);
        assert_eq!(p50, 1.0); // nearest-rank over [1.0, 3.0]
        let (n50, n95) = residual_percentiles(&[f32::INFINITY]);
        assert!(n50.is_nan() && n95.is_nan());
    }

    #[test]
    fn env_parsers_reject_invalid() {
        assert_eq!(parse_bool("1"), Some(true));
        assert_eq!(parse_bool("off"), Some(false));
        assert_eq!(parse_bool("abc"), None);
        assert_eq!(parse_f32_pos("0.8"), Some(0.8));
        assert_eq!(parse_f32_pos("-1"), None);
        assert_eq!(parse_f32_pos("NaN"), None);
        assert_eq!(parse_u32_pos("40"), Some(40));
        assert_eq!(parse_u32_pos("0"), None);
        assert_eq!(parse_usize_pos("abc"), None);
    }

    #[test]
    fn step_div_override_applies_to_all_paths() {
        let mut p = test_params();
        assert_eq!(p.step_div(DEFAULT_STEP_DIV_DIS), 40);
        assert_eq!(p.step_div(DEFAULT_STEP_DIV_NEUFLOW), 32);
        p.step_div_override = Some(15);
        assert_eq!(p.step_div(DEFAULT_STEP_DIV_DIS), 15);
        assert_eq!(p.step_div(DEFAULT_STEP_DIV_NEUFLOW), 15);
    }

    #[test]
    fn apply_gate_fb_off_is_identity_below_cap() {
        // Rollback equivalence: FB_CHECK=0 with fewer candidates than the cap
        // must select every point in original order (legacy pipeline output).
        let mut p = test_params();
        p.fb_check = false;
        let pts: Vec<(f32, f32)> = (0..67).map(|i| (i as f32, i as f32)).collect();
        let sel = apply_gate(&pts, &[], &[], (1280, 720), &p);
        assert!(!sel.degraded);
        assert_eq!(sel.indices, (0..67).collect::<Vec<_>>());
    }

    #[test]
    fn apply_gate_enforce_filters_outliers_and_caps() {
        let p = test_params(); // max_points = 200
        let n = 300;
        let pts: Vec<(f32, f32)> = (0..n)
            .map(|i| ((i % 20) as f32 * 20.0, (i / 20) as f32 * 25.0))
            .collect();
        let mut residuals = vec![0.2f32; n];
        for r in residuals.iter_mut().take(30) {
            *r = 5.0; // FB outliers
        }
        let fb_pass: Vec<usize> = (30..n).collect();
        let sel = apply_gate(&pts, &fb_pass, &residuals, (400, 400), &p);
        assert!(!sel.degraded);
        assert_eq!(sel.indices.len(), 200);
        assert!(sel.indices.iter().all(|&i| i >= 30), "FB outliers must not survive");
    }

    #[test]
    fn apply_gate_degrades_when_fb_thin() {
        let p = test_params();
        let n = 100;
        let pts: Vec<(f32, f32)> = (0..n).map(|i| (i as f32, i as f32)).collect();
        let residuals: Vec<f32> = (0..n).map(|i| i as f32 * 0.1).collect();
        let fb_pass: Vec<usize> = (0..5).collect(); // below MIN_POINTS
        let sel = apply_gate(&pts, &fb_pass, &residuals, (100, 100), &p);
        assert!(sel.degraded);
        assert_eq!(sel.indices.len(), TOPK_FALLBACK);
        // Residuals grow with index here → top-K = lowest indices.
        assert_eq!(sel.indices, (0..TOPK_FALLBACK).collect::<Vec<_>>());
    }

    #[test]
    fn apply_gate_reject_passthrough_when_low_texture() {
        let p = test_params();
        let pts: Vec<(f32, f32)> = (0..5).map(|i| (i as f32, 0.0)).collect();
        let sel = apply_gate(&pts, &[], &[f32::INFINITY; 5], (100, 100), &p);
        assert!(!sel.degraded);
        // Passthrough (< 10 points) so the caller's legacy check yields None.
        assert_eq!(sel.indices.len(), 5);
    }

    #[test]
    fn accumulator_aggregates_and_resets() {
        reset_stats();
        let s = GateStats {
            candidates: 900,
            texture_pass: 500,
            fb_pass: 380,
            kept: 200,
            fb_p50: 0.2,
            fb_p95: 1.8,
            degraded: false,
        };
        record_pair_stats("dis", 1000, &s);
        record_pair_stats("dis", 2000, &s);
        {
            let a = ACCUM.lock();
            assert_eq!(a.pairs, 2);
            assert_eq!(a.pairs_dis, 2);
            assert_eq!(a.sum_kept, 400);
            assert_eq!(a.percentile_pairs, 2);
        }
        dump_and_reset_stats();
        assert_eq!(ACCUM.lock().pairs, 0);
    }
}
