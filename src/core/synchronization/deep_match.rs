// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

//! Deep gyro match: coarse-offset discovery for one render-queue job
//! against one (long) gyro file, without any creation-time assumptions.
//!
//! This module holds the pure logic (search domain, window placement,
//! acceptance gates) plus a side-channel collector that lets
//! `essential_matrix::find_offsets` report per-window valley-quality
//! stats without changing its return type. The orchestration lives in
//! `render_queue.rs` (job prep / completion divert).

use parking_lot::Mutex;

/// Per-window stats recorded inside the essential coarse scan when the
/// collector is armed. `cost_p25` is the 25th percentile of a decimated
/// (25ms step) cost curve — the "plateau" reference the valley is judged
/// against.
#[derive(Debug, Clone)]
pub struct DeepMatchSegStats {
    pub range_idx: usize,
    pub offset_ms: f64,
    pub cost_min: f64,
    pub cost_p25: f64,
    pub max_angle: f64,
}

/// Full per-window cost curve + metadata for the posterior path. `curve` is
/// (offset_ms, cost): 25ms coarse over the search domain, densified to 5ms
/// within ±DENSE_MS of `argmin_ms`, plus the refined (argmin, cost_min) point.
/// `n_eff` = optical-flow frame-pair count in the window.
#[derive(Debug, Clone)]
pub struct DeepMatchWindowCurve {
    pub range_idx: usize,
    pub t_center_ms: f64,
    pub argmin_ms: f64,
    pub cost_min: f64,
    pub n_eff: f64,
    pub curve: Vec<(f64, f64)>,
}

static COLLECTOR: Mutex<Option<Vec<DeepMatchSegStats>>> = Mutex::new(None);
static CURVE_COLLECTOR: Mutex<Option<Vec<DeepMatchWindowCurve>>> = Mutex::new(None);
static SCAN_K: Mutex<usize> = Mutex::new(0);

/// Arm both collectors and record the scan-K target the essential scan uses to
/// cap how many (highest-motion) windows it fully scans. Only one deep match
/// runs at a time (render queue enforces), so single global slots suffice.
pub fn arm(scan_k: usize) {
    *COLLECTOR.lock() = Some(Vec::new());
    *CURVE_COLLECTOR.lock() = Some(Vec::new());
    *SCAN_K.lock() = scan_k;
}

pub fn is_armed() -> bool {
    COLLECTOR.lock().is_some()
}

/// Scan-K target while armed; 0 when disarmed (essential scan then keeps its
/// pre-change all-windows behavior — only the POSTERIOR=0 path leaves K at 0).
pub fn scan_k_target() -> usize {
    if COLLECTOR.lock().is_some() {
        *SCAN_K.lock()
    } else {
        0
    }
}

pub fn record(stats: DeepMatchSegStats) {
    if let Some(v) = COLLECTOR.lock().as_mut() {
        v.push(stats);
    }
}

pub fn record_curve(c: DeepMatchWindowCurve) {
    if let Some(v) = CURVE_COLLECTOR.lock().as_mut() {
        v.push(c);
    }
}

/// Take the collected stats and disarm (both collectors + scan-K reset).
/// Call `take_curves()` BEFORE this if you need the curves — this resets them.
pub fn take() -> Vec<DeepMatchSegStats> {
    *SCAN_K.lock() = 0;
    *CURVE_COLLECTOR.lock() = None;
    COLLECTOR.lock().take().unwrap_or_default()
}

/// Drain the curve collector only (does NOT disarm). Call this BEFORE `take()`
/// in the consumer — `take()` then clears the (now-empty) curve slot and
/// disarms. Drain-only here keeps `take()`'s `DeepMatchSegStats` intact for the
/// legacy POSTERIOR=0 path.
pub fn take_curves() -> Vec<DeepMatchWindowCurve> {
    CURVE_COLLECTOR.lock().take().unwrap_or_default()
}

/// Compute the global (run-level) essential search domain so that every
/// window's offset scan covers the whole (capped) gyro span.
/// All inputs/outputs in milliseconds.
/// Returns (initial_offset_ms, search_size_ms).
pub fn search_domain(
    video_duration_ms: f64,
    gyro_start_ms: f64,
    gyro_end_ms: f64,
    cap_ms: f64,
) -> (f64, f64) {
    let g_cap_end = gyro_end_ms.min(gyro_start_ms + cap_ms);
    let video_mid = video_duration_ms / 2.0;
    let gyro_center = (gyro_start_ms + g_cap_end) / 2.0;
    let initial_offset = video_mid - gyro_center;
    // The 15% margin keeps essential's "within 90% of search size" bounds
    // check from rejecting true offsets near the gyro span edges.
    let search_size = ((g_cap_end - gyro_start_ms) / 2.0 + video_duration_ms / 2.0) * 1.15;
    (initial_offset, search_size)
}

/// Evenly spread window start positions inside the video.
/// k/(n+1) fractions: n=3 → 25% / 50% / 75%.
pub fn window_positions_ms(video_duration_ms: f64, n: usize) -> Vec<f64> {
    (1..=n)
        .map(|k| video_duration_ms * (k as f64) / ((n + 1) as f64))
        .collect()
}

/// Overlap between consecutive scan chunks: the search-margin factor applied
/// to the video duration plus a fixed safety pad, so a true offset sitting on
/// a chunk boundary is fully covered by at least one chunk.
pub fn chunk_overlap_ms(video_duration_ms: f64) -> f64 {
    video_duration_ms.max(0.0) * 1.15 + 60_000.0
}

/// Split a gyro file's `[0, total_ms]` timeline into consecutive scan chunks
/// of `chunk_ms` length, each overlapping the previous one by `overlap_ms`.
/// `max_chunks > 0` caps the count (1 = legacy first-chunk-only behavior).
/// Degenerate inputs (overlap >= chunk) fall back to half-chunk stepping so
/// the plan always advances and terminates.
pub fn chunk_plan(
    total_ms: f64,
    chunk_ms: f64,
    overlap_ms: f64,
    max_chunks: usize,
) -> Vec<(f64, f64)> {
    if !total_ms.is_finite() || total_ms <= 0.0 || !chunk_ms.is_finite() || chunk_ms <= 0.0 {
        return Vec::new();
    }
    let step = if overlap_ms.is_finite() && overlap_ms >= 0.0 && overlap_ms < chunk_ms {
        chunk_ms - overlap_ms
    } else {
        chunk_ms / 2.0
    };
    let mut chunks = Vec::new();
    let mut start = 0.0f64;
    loop {
        let end = (start + chunk_ms).min(total_ms);
        chunks.push((start, end));
        if end >= total_ms || (max_chunks > 0 && chunks.len() >= max_chunks) {
            break;
        }
        start += step;
    }
    chunks
}

#[derive(Debug, Clone, PartialEq)]
pub enum DeepMatchVerdict {
    /// median of per-window offsets
    Accepted { offset_ms: f64 },
    /// the probe produced no usable window at all (no offsets, no collector
    /// stats) — assembly/arbitration failure, not low camera motion
    ProbeNotRun,
    /// fewer than 2 windows survived the motion gate inside essential
    TooFewWindows,
    /// per-window offsets disagree → video likely not in this gyro file
    Inconsistent { spread_ms: f64 },
    /// valleys too shallow vs plateau → noise match
    WeakValley { worst_ratio: f64 },
}

/// Double acceptance gate (spec §3.3). `offsets_ms` are the per-window
/// offsets returned by the sync run; `stats` come from the collector.
/// The valley-quality ceiling is two-tier: when the windows agree within
/// `tight_spread_ms`, `cost_ratio_tight` applies instead of
/// `cost_ratio_max` — multi-window agreement at ms scale over a
/// ±thousands-of-seconds domain is overwhelming evidence, while the
/// per-window ratio floor is physically elevated under bare/approximate
/// lens matrices. `tight_spread_ms = 0` disables the tight tier.
pub fn evaluate(
    offsets_ms: &[f64],
    stats: &[DeepMatchSegStats],
    spread_max_ms: f64,
    cost_ratio_max: f64,
    tight_spread_ms: f64,
    cost_ratio_tight: f64,
) -> DeepMatchVerdict {
    if offsets_ms.is_empty() && stats.is_empty() {
        return DeepMatchVerdict::ProbeNotRun;
    }
    if offsets_ms.len() < 2 {
        return DeepMatchVerdict::TooFewWindows;
    }
    let mut sorted: Vec<f64> = offsets_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let spread = sorted.last().unwrap() - sorted.first().unwrap();
    if spread > spread_max_ms {
        return DeepMatchVerdict::Inconsistent { spread_ms: spread };
    }
    // Gate 2: every window must show a real valley. ratio = cost_min/p25,
    // lower = deeper valley; near 1.0 = flat curve (noise match).
    let ratio_max_effective = if tight_spread_ms > 0.0 && spread <= tight_spread_ms {
        cost_ratio_tight
    } else {
        cost_ratio_max
    };
    let mut worst_ratio = 0.0f64;
    for s in stats {
        if !s.cost_p25.is_finite() || s.cost_p25 <= 0.0 {
            return DeepMatchVerdict::WeakValley { worst_ratio: 1.0 };
        }
        let ratio = s.cost_min / s.cost_p25;
        worst_ratio = worst_ratio.max(ratio);
    }
    if stats.is_empty() || worst_ratio > ratio_max_effective {
        return DeepMatchVerdict::WeakValley {
            worst_ratio: if stats.is_empty() { 1.0 } else { worst_ratio },
        };
    }
    let median = if sorted.len() % 2 == 0 {
        (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
    } else {
        sorted[sorted.len() / 2]
    };
    DeepMatchVerdict::Accepted { offset_ms: median }
}

/// Joint-posterior deep-match decision (design §3.4-3.8). Converts each
/// window's essential cost curve to a log-likelihood, max-dilates by the
/// clip's drift half-width T(D)/2, log-adds on the shared 5ms grid, and gates
/// on `conf >= conf_min` and `ci95_width <= ci95_base_ms + T(D)`. Maps to the
/// existing `DeepMatchVerdict` so the chunk orchestration is untouched.
/// All inputs/outputs in ms.
pub fn decide_posterior(
    curves: &[DeepMatchWindowCurve],
    clip_duration_ms: f64,
    conf_min: f64,
    ci95_base_ms: f64,
    drift_rate_ms_per_min: f64,
    drift_floor_ms: f64,
) -> DeepMatchVerdict {
    use crate::synchronization::posterior::{
        approx_window_log_likelihood, combine_windows_on_common_grid_dilated, posterior_decide, Prior,
    };
    if curves.is_empty() {
        return DeepMatchVerdict::ProbeNotRun;
    }
    if curves.len() < 2 {
        return DeepMatchVerdict::TooFewWindows;
    }
    // Per-window (grid, logL). The combine re-cleans (sort/dedup/drop
    // non-finite) so an unsorted curve is fine.
    let per_window: Vec<(Vec<f64>, Vec<f64>)> = curves
        .iter()
        .map(|c| {
            let grid: Vec<f64> = c.curve.iter().map(|p| p.0).collect();
            let logl: Vec<f64> = c
                .curve
                .iter()
                .map(|p| approx_window_log_likelihood(p.1, c.cost_min, c.n_eff))
                .collect();
            (grid, logl)
        })
        .collect();
    let views: Vec<(&[f64], &[f64])> = per_window.iter().map(|(g, l)| (g.as_slice(), l.as_slice())).collect();

    let t_d = drift_tolerance_ms(clip_duration_ms, drift_rate_ms_per_min, drift_floor_ms);
    const GRID_STEP_MS: f64 = 5.0;
    let Some((joint_grid, joint_logl)) =
        combine_windows_on_common_grid_dilated(&views, GRID_STEP_MS, t_d / 2.0)
    else {
        // No usable span overlap -> windows do not share an offset domain.
        return DeepMatchVerdict::Inconsistent { spread_ms: f64::INFINITY };
    };
    let Some(post) = posterior_decide(&joint_grid, &joint_logl, &Prior::Uniform) else {
        // worst_ratio 1.0 = vacuous joint (no usable grid point); matches evaluate()'s convention
        return DeepMatchVerdict::WeakValley { worst_ratio: 1.0 };
    };
    let ci95_width = post.ci95.1 - post.ci95.0;
    let ci95_gate = ci95_base_ms + t_d;
    ::log::info!(
        target: "sync",
        "[deep-match] posterior: argmax={:.1}ms conf={:.3} ci95=[{:.1},{:.1}] width={:.1}ms gate={:.1}ms windows={} T(D)={:.1}ms n_eff={:?}",
        post.argmax_ms, post.conf_posterior, post.ci95.0, post.ci95.1,
        ci95_width, ci95_gate, curves.len(), t_d,
        curves.iter().map(|c| c.n_eff as usize).collect::<Vec<_>>()
    );
    if post.conf_posterior >= conf_min && ci95_width <= ci95_gate {
        DeepMatchVerdict::Accepted { offset_ms: post.argmax_ms }
    } else if ci95_width > ci95_gate {
        // Wide / multimodal posterior = windows disagree beyond drift -> advance.
        DeepMatchVerdict::Inconsistent { spread_ms: ci95_width }
    } else {
        // Narrow but low-confidence = flat joint (noise match) -> advance.
        DeepMatchVerdict::WeakValley { worst_ratio: 1.0 - post.conf_posterior }
    }
}

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
        .unwrap_or(default)
}

// Same as env_f64 but accepts 0 (used by kill-switch style knobs).
fn env_f64_nonneg(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v >= 0.0)
        .unwrap_or(default)
}

pub fn spread_max_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_SPREAD_MS", 200.0)
}
pub fn cost_ratio_max() -> f64 {
    // Default calibrated in Task E2 against ground-truth material; 0.35
    // is the pre-calibration starting point (valley ≥ ~3x deeper than
    // plateau p25).
    env_f64("GYROFLOW_DEEP_MATCH_COST_RATIO", 0.35)
}
/// Spread below which the relaxed `cost_ratio_tight` ceiling applies.
/// 0 disables the tight tier (single-gate pre-change behavior).
pub fn tight_spread_ms() -> f64 {
    env_f64_nonneg("GYROFLOW_DEEP_MATCH_TIGHT_SPREAD_MS", 10.0)
}
/// Relaxed valley-ratio ceiling for tight-spread agreement (bare/approximate
/// lens matrices elevate the physical ratio floor at the true offset).
/// Calibration points: bare-lens true hits 0.448/0.471; R5 Mark II 70mm
/// long-lens true hit (3 windows agreeing within 3ms) worst_ratio 0.637;
/// known noise/flat-curve windows 0.875-0.925. 0.7 splits true hits from
/// noise with margin on both sides.
pub fn cost_ratio_tight() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_COST_RATIO_TIGHT", 0.7)
}
/// Per-window motion gate (°/s) used by the essential scan while the
/// deep-match collector is armed; regular autosync keeps the fixed 3.0.
/// OF-estimated rates scale by f_true/f_assumed under approximate lens
/// matrices, so the probe runs a lower economy floor — correctness is
/// carried by the consistency gate.
pub fn motion_gate_armed() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_MOTION_GATE", 1.5)
}
/// Scan chunk length (chunked scan: per-chunk parse + search span; formerly
/// a hard cap on the searched span).
pub fn max_scan_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_MAX_SCAN_S", 7200.0) * 1000.0
}
/// Chunk-count cap. 0 = unlimited (cover the whole gyro file);
/// 1 = legacy first-chunk-only behavior.
pub fn max_chunks() -> usize {
    env_f64_nonneg("GYROFLOW_DEEP_MATCH_MAX_CHUNKS", 0.0) as usize
}
pub fn window_count() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_WINDOWS", 3.0) as usize).clamp(2, 8)
}

/// Decide candidate-window count and scan count for a clip of `duration_ms`.
/// `candidates` candidates are placed; the top `scan_short`/`scan_long`
/// (by motion) are essential-scanned depending on the `long_min_ms` threshold.
/// Short clips that cannot fit `candidates` non-overlapping OF windows of
/// `per_window_ms` each degrade the candidate count; below 2 windows deep
/// match is impossible (returns (0, 0) -> caller emits `TooFewWindows`).
pub fn plan_windows(
    duration_ms: f64,
    candidates: usize,
    scan_short: usize,
    scan_long: usize,
    long_min_ms: f64,
    per_window_ms: f64,
) -> (usize, usize) {
    if !duration_ms.is_finite() || duration_ms <= 0.0 || per_window_ms <= 0.0 {
        return (0, 0);
    }
    // Windows sit at k/(N+1) fractions and may overlap (each analyses
    // ~time_per_syncpoint of footage); `per_window_ms` is a soft density target
    // for how many candidates to place, NOT a hard non-overlap requirement.
    // Any valid-duration clip gets at least 2 windows — a clip with too little
    // motion is then rejected honestly at scan time (TooFewWindows), matching
    // the pre-change behaviour that always scanned its fixed windows.
    let fit = ((duration_ms / per_window_ms).floor() as i64 - 1).max(0) as usize;
    let n = candidates.min(fit).max(2);
    let k_target = if duration_ms > long_min_ms { scan_long } else { scan_short };
    (n, k_target.min(n).max(2))
}

/// Allowed drift range T(D) for a clip of `duration_ms` (design §3.8):
/// `max(floor_ms, rate_ms_per_min * minutes)`. The blur half-width is T(D)/2.
pub fn drift_tolerance_ms(duration_ms: f64, rate_ms_per_min: f64, floor_ms: f64) -> f64 {
    if !duration_ms.is_finite() || duration_ms <= 0.0 {
        return floor_ms.max(0.0);
    }
    let minutes = duration_ms / 60_000.0;
    (rate_ms_per_min.max(0.0) * minutes).max(floor_ms.max(0.0))
}

/// Master switch for the posterior acceptance gate in deep-match.
/// `GYROFLOW_DEEP_MATCH_POSTERIOR=0|off|false` disables it; any other value
/// (or unset) keeps it enabled. OnceLock-cached; first resolve logs to
/// `target="lifecycle"`.
pub fn posterior_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_DEEP_MATCH_POSTERIOR").ok();
        let (v, source) = match raw.as_deref().map(str::trim) {
            None | Some("") => (true, "default"),
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => (true, "env"),
                "0" | "false" | "no" | "off" => (false, "env"),
                _ => {
                    log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_DEEP_MATCH_POSTERIOR={} invalid, falling back to default (on)",
                        s
                    );
                    (true, "default")
                }
            },
        };
        log::info!(target: "lifecycle", "deep_match_posterior resolved={} source={}", v, source);
        v
    })
}

/// Number of candidate windows placed for motion pre-screening (env, clamp 2-8).
pub fn candidates_count() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_CANDIDATES", 4.0) as usize).clamp(2, 8)
}

/// Windows essential-scanned on short clips (top-K by motion; env, clamp 2-8).
pub fn scan_short() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_SCAN_SHORT", 2.0) as usize).clamp(2, 8)
}

/// Windows essential-scanned on long clips (top-K by motion; env, clamp 2-8).
pub fn scan_long() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_SCAN_LONG", 3.0) as usize).clamp(2, 8)
}
pub fn long_min_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_LONG_MIN_S", 480.0) * 1000.0
}
pub fn drift_rate_ms_per_min() -> f64 {
    env_f64_nonneg("GYROFLOW_DEEP_MATCH_DRIFT_RATE_MS_PER_MIN", 2.0)
}
pub fn drift_floor_ms() -> f64 {
    env_f64_nonneg("GYROFLOW_DEEP_MATCH_DRIFT_FLOOR_MS", 10.0)
}

/// Conservative starting confidence bar against the ground-truth ledger.
/// Wrong queue-wide anchor is costly — start strict.
/// Calibration pending (spec §5): compare against ground-truth ledger before relaxing.
/// `GYROFLOW_DEEP_MATCH_POST_CONF_MIN` overrides.
pub fn post_conf_min() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_POST_CONF_MIN", 0.5)
}

/// Effective CI95 gate = this base + drift_tolerance T(D) (design §3.5/§3.8).
/// Calibration pending (spec §5): sweep against recorded corpus before tightening.
/// `GYROFLOW_DEEP_MATCH_POST_CI95_BASE_MS` overrides.
pub fn post_ci95_base_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_POST_CI95_BASE_MS", 25.0)
}
pub fn post_dense_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_POST_DENSE_MS", 30.0)
}
/// Per-window OF footage length used by `plan_windows` fit math; mirrors the
/// `time_per_syncpoint` written into the probe sync_settings (2.5s).
pub fn per_window_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_PER_WINDOW_MS", 2500.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Serializes tests that share the global collector statics so they do not
    // race each other when the test harness runs them on multiple threads.
    static TEST_MTX: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn search_domain_covers_capped_gyro_span() {
        // 60s video, gyro 0..5820s, cap 7200s (no capping applied)
        let (init, search) = search_domain(60_000.0, 0.0, 5_820_000.0, 7_200_000.0);
        assert!((init - (30_000.0 - 2_910_000.0)).abs() < 1e-6);
        assert!((search - (2_910_000.0 + 30_000.0) * 1.15).abs() < 1e-6);
    }

    #[test]
    fn search_domain_caps_long_gyro() {
        // 4h gyro capped at 2h
        let (_, search) = search_domain(60_000.0, 0.0, 14_400_000.0, 7_200_000.0);
        assert!((search - (3_600_000.0 + 30_000.0) * 1.15).abs() < 1e-6);
    }

    #[test]
    fn window_positions_spread() {
        let p = window_positions_ms(100_000.0, 3);
        assert_eq!(p, vec![25_000.0, 50_000.0, 75_000.0]);
    }

    #[test]
    fn chunk_plan_single_chunk_when_total_fits() {
        assert_eq!(
            chunk_plan(3_600_000.0, 7_200_000.0, 100_000.0, 0),
            vec![(0.0, 3_600_000.0)]
        );
    }

    #[test]
    fn chunk_plan_exact_multiple_ends_at_total() {
        // total = 2 chunks exactly, no overlap: second chunk ends at total.
        let plan = chunk_plan(14_400_000.0, 7_200_000.0, 0.0, 0);
        assert_eq!(plan, vec![(0.0, 7_200_000.0), (7_200_000.0, 14_400_000.0)]);
    }

    #[test]
    fn chunk_plan_covers_long_file_with_overlap() {
        // 8.5h file, 2h chunks, 30s-video overlap (94.5s).
        let total = 30_600_000.0;
        let chunk = 7_200_000.0;
        let overlap = chunk_overlap_ms(30_000.0);
        let plan = chunk_plan(total, chunk, overlap, 0);
        assert_eq!(plan.len(), 5);
        assert_eq!(plan[0], (0.0, chunk));
        for w in plan.windows(2) {
            // Consecutive chunks overlap by exactly `overlap`.
            assert!((w[0].1 - w[1].0 - overlap).abs() < 1e-6);
        }
        assert_eq!(plan.last().unwrap().1, total, "last chunk must end at total");
    }

    #[test]
    fn chunk_plan_max_chunks_caps_the_plan() {
        let plan = chunk_plan(30_600_000.0, 7_200_000.0, 100_000.0, 1);
        assert_eq!(plan, vec![(0.0, 7_200_000.0)]);
    }

    #[test]
    fn chunk_plan_degenerate_overlap_still_advances() {
        // overlap >= chunk falls back to half-chunk stepping — must terminate
        // and still cover the file end.
        let plan = chunk_plan(10_000_000.0, 1_000_000.0, 2_000_000.0, 0);
        assert!(plan.len() < 100, "plan must not blow up");
        assert_eq!(plan.last().unwrap().1, 10_000_000.0);
        for w in plan.windows(2) {
            assert!(w[1].0 > w[0].0, "chunks must advance monotonically");
        }
    }

    #[test]
    fn chunk_plan_rejects_invalid_inputs() {
        assert!(chunk_plan(0.0, 7_200_000.0, 0.0, 0).is_empty());
        assert!(chunk_plan(f64::NAN, 7_200_000.0, 0.0, 0).is_empty());
        assert!(chunk_plan(1000.0, 0.0, 0.0, 0).is_empty());
    }

    fn stats(ratio: f64) -> DeepMatchSegStats {
        DeepMatchSegStats {
            range_idx: 0,
            offset_ms: 0.0,
            cost_min: ratio * 100.0,
            cost_p25: 100.0,
            max_angle: 10.0,
        }
    }

    #[test]
    fn evaluate_accepts_consistent_deep_valleys() {
        let v = evaluate(
            &[-204692.0, -204690.5, -204693.0],
            &[stats(0.1), stats(0.2), stats(0.15)],
            200.0,
            0.35,
            10.0,
            0.6,
        );
        assert_eq!(v, DeepMatchVerdict::Accepted { offset_ms: -204692.0 });
    }

    #[test]
    fn evaluate_rejects_single_window() {
        assert_eq!(
            evaluate(&[-100.0], &[stats(0.1)], 200.0, 0.35, 10.0, 0.6),
            DeepMatchVerdict::TooFewWindows
        );
    }

    #[test]
    fn evaluate_rejects_inconsistent_offsets() {
        match evaluate(
            &[0.0, 5000.0],
            &[stats(0.1), stats(0.1)],
            200.0,
            0.35,
            10.0,
            0.6,
        ) {
            DeepMatchVerdict::Inconsistent { spread_ms } => assert_eq!(spread_ms, 5000.0),
            v => panic!("expected Inconsistent, got {v:?}"),
        }
    }

    #[test]
    fn evaluate_rejects_shallow_valleys() {
        match evaluate(
            &[0.0, 1.0],
            &[stats(0.1), stats(0.9)],
            200.0,
            0.35,
            10.0,
            0.6,
        ) {
            DeepMatchVerdict::WeakValley { worst_ratio } => {
                assert!((worst_ratio - 0.9).abs() < 1e-9)
            }
            v => panic!("expected WeakValley, got {v:?}"),
        }
    }

    #[test]
    fn evaluate_rejects_missing_stats() {
        assert_eq!(
            evaluate(&[0.0, 1.0], &[], 200.0, 0.35, 10.0, 0.6),
            DeepMatchVerdict::WeakValley { worst_ratio: 1.0 }
        );
    }

    #[test]
    fn evaluate_tight_spread_relaxes_ratio_ceiling() {
        // Bare manual-lens true hit: ms-scale agreement, elevated ratios.
        let v = evaluate(
            &[0.0, 1.0, 2.0],
            &[stats(0.45), stats(0.47), stats(0.40)],
            200.0,
            0.35,
            10.0,
            0.7,
        );
        assert_eq!(v, DeepMatchVerdict::Accepted { offset_ms: 1.0 });
        // R5 Mark II 70mm long-lens true hit (2026-06-12 log): windows
        // agreeing within 3ms with worst ratio 0.637 must pass the default
        // tight ceiling; flat-curve noise (0.875+) must still be rejected.
        let v = evaluate(
            &[-1316494.0, -1316497.0, -1316496.3],
            &[stats(0.523), stats(0.637), stats(0.626)],
            200.0,
            0.35,
            10.0,
            0.7,
        );
        assert_eq!(v, DeepMatchVerdict::Accepted { offset_ms: -1316496.3 });
        match evaluate(
            &[0.0, 1.0],
            &[stats(0.875), stats(0.925)],
            200.0,
            0.35,
            10.0,
            0.7,
        ) {
            DeepMatchVerdict::WeakValley { .. } => {}
            v => panic!("flat-curve noise must stay rejected, got {v:?}"),
        }
    }

    #[test]
    fn evaluate_loose_spread_keeps_strict_ceiling() {
        match evaluate(
            &[0.0, 150.0],
            &[stats(0.45), stats(0.45)],
            200.0,
            0.35,
            10.0,
            0.6,
        ) {
            DeepMatchVerdict::WeakValley { worst_ratio } => {
                assert!((worst_ratio - 0.45).abs() < 1e-9)
            }
            v => panic!("expected WeakValley, got {v:?}"),
        }
    }

    #[test]
    fn evaluate_tight_zero_degrades_to_single_gate() {
        match evaluate(
            &[0.0, 0.0],
            &[stats(0.45), stats(0.45)],
            200.0,
            0.35,
            0.0,
            0.6,
        ) {
            DeepMatchVerdict::WeakValley { worst_ratio } => {
                assert!((worst_ratio - 0.45).abs() < 1e-9)
            }
            v => panic!("expected WeakValley (tight tier disabled), got {v:?}"),
        }
    }

    #[test]
    fn evaluate_reports_probe_not_run_on_empty_inputs() {
        assert_eq!(
            evaluate(&[], &[], 200.0, 0.35, 10.0, 0.6),
            DeepMatchVerdict::ProbeNotRun
        );
        // A probe that ran (stats recorded) but produced no offsets is still
        // the low-motion case, not ProbeNotRun.
        assert_eq!(
            evaluate(&[], &[stats(0.1)], 200.0, 0.35, 10.0, 0.6),
            DeepMatchVerdict::TooFewWindows
        );
    }

    #[test]
    fn collector_roundtrip() {
        let _g = TEST_MTX.lock().unwrap_or_else(|e| e.into_inner());
        arm(2);
        assert!(is_armed());
        record(stats(0.1));
        let v = take();
        assert_eq!(v.len(), 1);
        assert!(!is_armed());
        // take() when disarmed yields empty
        assert!(take().is_empty());
    }

    #[test]
    fn plan_windows_short_long_and_degrade() {
        // Long clip (>8min) with room: 4 candidates, scan 3.
        assert_eq!(plan_windows(600_000.0, 4, 2, 3, 480_000.0, 2500.0), (4, 3));
        // Short clip (<=8min) with room: 4 candidates, scan 2.
        assert_eq!(plan_windows(120_000.0, 4, 2, 3, 480_000.0, 2500.0), (4, 2));
        // Boundary: exactly 8min is "short" (> threshold is long).
        assert_eq!(plan_windows(480_000.0, 4, 2, 3, 480_000.0, 2500.0), (4, 2));
        // Too short to fit 4 windows: fit = floor(D/per) - 1. 10s/2.5 - 1 = 3.
        assert_eq!(plan_windows(10_000.0, 4, 2, 3, 480_000.0, 2500.0), (3, 2));
        // 7.5s -> fit 2; scan min(2, 2) = 2.
        assert_eq!(plan_windows(7_500.0, 4, 2, 3, 480_000.0, 2500.0), (2, 2));
        // 5s -> fit 1, but any valid-duration clip is clamped to >=2 windows
        // (rejection happens honestly at scan time, not here).
        assert_eq!(plan_windows(5_000.0, 4, 2, 3, 480_000.0, 2500.0), (2, 2));
        // 4.2s (the DSC_0383 regression case) must also run with 2 windows.
        assert_eq!(plan_windows(4_204.0, 4, 2, 3, 480_000.0, 2500.0), (2, 2));
        // Guard inputs → (0, 0).
        assert_eq!(plan_windows(f64::NAN, 4, 2, 3, 480_000.0, 2500.0), (0, 0));
        assert_eq!(plan_windows(600_000.0, 4, 2, 3, 480_000.0, 0.0), (0, 0));
    }

    #[test]
    fn drift_tolerance_floor_and_rate() {
        // <=5min: floored at 10ms.
        assert!((drift_tolerance_ms(60_000.0, 2.0, 10.0) - 10.0).abs() < 1e-9);
        assert!((drift_tolerance_ms(300_000.0, 2.0, 10.0) - 10.0).abs() < 1e-9);
        // >5min: 2ms per minute.
        assert!((drift_tolerance_ms(600_000.0, 2.0, 10.0) - 20.0).abs() < 1e-9);
        assert!((drift_tolerance_ms(1_800_000.0, 2.0, 10.0) - 60.0).abs() < 1e-9);
        assert!((drift_tolerance_ms(3_600_000.0, 2.0, 10.0) - 120.0).abs() < 1e-9);
        // rate=0 disables the rate term, floor still applies.
        assert!((drift_tolerance_ms(3_600_000.0, 0.0, 10.0) - 10.0).abs() < 1e-9);
    }

    fn wc(idx: usize, t_center: f64, peak: f64, n_eff: f64) -> DeepMatchWindowCurve {
        // Synthetic cost curve over [-600, 600] @25ms step.  Min cost 1.0 at
        // `peak`; rises steeply toward ~2.0 away from it.  Gaussian width
        // divisor 13 gives sigma ~13ms so two peaks >30ms apart are clearly
        // separated; only explicit drift dilation (max-dilation in
        // combine_windows_on_common_grid_dilated) can merge them.
        let curve: Vec<(f64, f64)> = (0..=48)
            .map(|k| {
                let off = -600.0 + k as f64 * 25.0;
                let d = (off - peak) / 13.0;
                (off, 1.0 + (1.0 - (-0.5 * d * d).exp()))
            })
            .collect();
        DeepMatchWindowCurve { range_idx: idx, t_center_ms: t_center, argmin_ms: peak, cost_min: 1.0, n_eff, curve }
    }

    #[test]
    fn decide_posterior_accepts_consistent_windows() {
        // Peaks within 3ms of each other -> always merge regardless of drift;
        // should be Accepted with any reasonable ci95_base.
        let curves = vec![wc(0, 1000.0, -100.0, 75.0), wc(1, 2000.0, -98.0, 75.0), wc(2, 3000.0, -101.0, 75.0)];
        match decide_posterior(&curves, 120_000.0, 0.4, 50.0, 2.0, 10.0) {
            DeepMatchVerdict::Accepted { offset_ms } => assert!((offset_ms + 100.0).abs() <= 10.0, "got {offset_ms}"),
            v => panic!("expected Accepted, got {v:?}"),
        }
    }

    #[test]
    fn decide_posterior_too_few_and_empty() {
        assert_eq!(decide_posterior(&[], 120_000.0, 0.4, 50.0, 2.0, 10.0), DeepMatchVerdict::ProbeNotRun);
        assert_eq!(decide_posterior(&[wc(0, 1000.0, -100.0, 75.0)], 120_000.0, 0.4, 50.0, 2.0, 10.0), DeepMatchVerdict::TooFewWindows);
    }

    #[test]
    fn decide_posterior_drift_separated_long_clip_accepts() {
        // Two windows whose cost-curve peaks are 80ms apart (a real clock-drift
        // offset between the video clock and the gyro clock on a long clip).
        //
        // LONG clip (60 min): T(D) = max(10, 2.0*60) = 120ms.
        //   Drift dilation half-width = 60ms > 40ms (half the separation) ->
        //   the max-dilation smears each window's likelihood over ±60ms,
        //   merging the two 80ms-apart peaks into a single unimodal joint.
        //   ci95_gate = 20 + 120 = 140ms -> narrow joint -> Accepted.
        //
        // SHORT clip (1 min): T(D) = max(10, 2.0*1) = 10ms.
        //   Blur half-width = 5ms << 40ms -> peaks stay separated ->
        //   bimodal/wide joint -> ci95_width >> (20 + 10) = 30ms -> Inconsistent.
        //
        // The sharp Gaussian sigma (~13ms) ensures the curves do NOT overlap
        // by shape alone; only explicit drift dilation can bridge the 80ms gap.
        let long_curves = vec![
            wc(0, 100_000.0, -140.0, 150.0),
            wc(1, 3_500_000.0, -60.0, 150.0),
        ];
        match decide_posterior(&long_curves, 3_600_000.0, 0.4, 20.0, 2.0, 10.0) {
            DeepMatchVerdict::Accepted { .. } => {}
            v => panic!("long clip (T=120ms) within drift tolerance should accept, got {v:?}"),
        }
        let short_curves = vec![
            wc(0, 1_000.0, -140.0, 150.0),
            wc(1, 59_000.0, -60.0, 150.0),
        ];
        match decide_posterior(&short_curves, 60_000.0, 0.4, 20.0, 2.0, 10.0) {
            DeepMatchVerdict::Inconsistent { .. } => {}
            v => panic!("short clip (T=10ms) beyond drift tolerance should be Inconsistent, got {v:?}"),
        }
    }

    fn flat_wc(idx: usize, t_center: f64) -> DeepMatchWindowCurve {
        // Near-flat cost curve (no real valley): cost barely above cost_min
        // across the whole domain -> logL ~= 0 everywhere -> uniform posterior ->
        // low conf_posterior -> WeakValley ("noise match" rejection).
        let curve: Vec<(f64, f64)> = (0..=240)
            .map(|k| {
                let off = -600.0 + k as f64 * 5.0;
                (off, 1.0 + 1e-4 * (off / 600.0).abs()) // essentially flat
            })
            .collect();
        DeepMatchWindowCurve { range_idx: idx, t_center_ms: t_center, argmin_ms: 0.0, cost_min: 1.0, n_eff: 75.0, curve }
    }

    #[test]
    fn decide_posterior_flat_curves_are_weak_valley() {
        // Two flat (no-valley) windows -> uniform joint -> conf far below
        // conf_min, ci95 narrow enough to not be "Inconsistent" — the noise
        // match must be rejected as WeakValley, never Accepted.
        let curves = vec![flat_wc(0, 1000.0), flat_wc(1, 2000.0)];
        let v = decide_posterior(&curves, 120_000.0, 0.4, 50.0, 2.0, 10.0);
        assert!(
            !matches!(v, DeepMatchVerdict::Accepted { .. }),
            "flat noise curves must not be accepted, got {v:?}"
        );
    }

    #[test]
    fn curve_collector_roundtrip_and_scan_k() {
        let _g = TEST_MTX.lock().unwrap_or_else(|e| e.into_inner());
        arm(3);
        assert!(is_armed());
        assert_eq!(scan_k_target(), 3);
        record_curve(DeepMatchWindowCurve {
            range_idx: 0,
            t_center_ms: 1000.0,
            argmin_ms: -204692.0,
            cost_min: 1.0,
            n_eff: 75.0,
            curve: vec![(-204700.0, 2.0), (-204692.0, 1.0), (-204680.0, 2.0)],
        });
        // take_curves is drain-only: curves come out, collector stays armed.
        let cs = take_curves();
        assert_eq!(cs.len(), 1);
        assert!(is_armed());
        assert_eq!(scan_k_target(), 3);
        // A second drain is empty; still armed.
        assert!(take_curves().is_empty());
        assert!(is_armed());
        // take() disarms everything.
        let _ = take();
        assert!(!is_armed());
        assert_eq!(scan_k_target(), 0);
    }
}
