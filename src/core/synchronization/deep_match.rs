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

// ---------------------------------------------------------------------------
// Pool-wide search + timestamp prelocation (deep-match-gyro-pool-prelocate)
// ---------------------------------------------------------------------------

/// One planned probe of a pool-wide deep-match run: which gyro file to load
/// and (for the focused tiers) which slice of its timeline to search.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProbeTask {
    /// Index into the render queue's gyro pool (`gyro_files`).
    pub gyro_index: usize,
    /// 0 = learned clock shift, 1 = zero clock shift, 2 = pool alignment,
    /// 3 = exhaustive full-span fallback.
    pub tier: u8,
    /// Focused window [start, end] on the gyro file's own timeline (ms);
    /// None = full span (tier 3).
    pub window_ms: Option<(f64, f64)>,
}

/// The planner's view of one gyro pool entry. Entries missing either field
/// only participate in the tier-3 exhaustive fallback.
#[derive(Debug, Clone, Copy)]
pub struct GyroPoolEntry {
    pub gyro_index: usize,
    pub created_at_ms: Option<i64>,
    pub duration_ms: Option<f64>,
}

/// Recordings separated by more than this gap belong to different sessions
/// ("days") for the ordinal pool-alignment candidates. Gap clustering instead
/// of calendar days keeps the split independent of the unknown timezones.
const SESSION_GAP_MS: i64 = 6 * 3_600_000;

/// Predicted position of the video's content start on the gyro file's own
/// timeline, under a hypothesised clock shift (gyro clock minus video clock,
/// = the session offset `derive_session_offset_from_deep_match` returns).
/// Algebraic inverse of that function: `deep_offset = -position`.
pub fn predicted_gyro_position_ms(
    video_created_at_ms: i64,
    gyro_created_at_ms: i64,
    clock_shift_ms: i64,
) -> f64 {
    (video_created_at_ms - gyro_created_at_ms + clock_shift_ms) as f64
}

/// Wall-clock overlap (ms) between the video's [created, created+duration]
/// window and a gyro file's, at zero clock shift. 0 = disjoint.
fn wall_overlap_ms(
    video_created_at_ms: i64,
    video_duration_ms: f64,
    gyro_created_at_ms: i64,
    gyro_duration_ms: f64,
) -> f64 {
    let v0 = video_created_at_ms as f64;
    let v1 = v0 + video_duration_ms.max(0.0);
    let g0 = gyro_created_at_ms as f64;
    let g1 = g0 + gyro_duration_ms.max(0.0);
    (v1.min(g1) - v0.max(g0)).max(0.0)
}

/// Order the pool candidates best-first for the probe plan: overlap/containment
/// first (largest wall-clock overlap with the video), then nearest start
/// (`|Δcreated|`), then pool order for determinism. Entries missing
/// created_at/duration sort last (in pool order) — they can only be probed by
/// the tier-3 exhaustive fallback. Returns `gyro_index` values.
pub fn rank_gyro_candidates(
    video_created_at_ms: Option<i64>,
    video_duration_ms: f64,
    pool: &[GyroPoolEntry],
) -> Vec<usize> {
    let mut with_meta: Vec<(&GyroPoolEntry, f64, i64)> = Vec::new();
    let mut without_meta: Vec<usize> = Vec::new();
    for e in pool {
        match (video_created_at_ms, e.created_at_ms, e.duration_ms) {
            (Some(vc), Some(gc), Some(gd)) => {
                let overlap = wall_overlap_ms(vc, video_duration_ms, gc, gd);
                with_meta.push((e, overlap, (gc - vc).abs()));
            }
            _ => without_meta.push(e.gyro_index),
        }
    }
    with_meta.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.2.cmp(&b.2))
            .then(a.0.gyro_index.cmp(&b.0.gyro_index))
    });
    with_meta
        .into_iter()
        .map(|(e, _, _)| e.gyro_index)
        .chain(without_meta)
        .collect()
}

/// Split sorted start timestamps into session clusters at gaps larger than
/// `SESSION_GAP_MS`; returns each cluster's first start.
fn session_cluster_starts(starts: &mut Vec<i64>) -> Vec<i64> {
    starts.sort_unstable();
    let mut out: Vec<i64> = Vec::new();
    let mut prev: Option<i64> = None;
    for &s in starts.iter() {
        match prev {
            None => out.push(s),
            Some(p) if s - p > SESSION_GAP_MS => out.push(s),
            _ => {}
        }
        prev = Some(s);
    }
    out
}

/// Pool-alignment clock-shift candidates (tier 2): the shifts (gyro clock
/// minus video clock) suggested by aligning the two pools' overall structure —
/// start-to-start, end-to-end, and per-session ordinal starts when both pools
/// cluster into the same number of sessions. Candidates within `tol_ms` of an
/// earlier one (or of zero — tier 1 already covers that) are dropped.
/// Each pool entry is `(created_at_ms, duration_ms)`.
pub fn pool_shift_candidates(
    video_pool: &[(i64, f64)],
    gyro_pool: &[(i64, f64)],
    tol_ms: f64,
) -> Vec<i64> {
    if video_pool.is_empty() || gyro_pool.is_empty() {
        return Vec::new();
    }
    let vs = video_pool.iter().map(|(c, _)| *c).min().unwrap();
    let gs = gyro_pool.iter().map(|(c, _)| *c).min().unwrap();
    let ve = video_pool
        .iter()
        .map(|(c, d)| *c + d.max(0.0).round() as i64)
        .max()
        .unwrap();
    let ge = gyro_pool
        .iter()
        .map(|(c, d)| *c + d.max(0.0).round() as i64)
        .max()
        .unwrap();
    let mut raw: Vec<i64> = vec![gs - vs, ge - ve];
    // Ordinal session alignment ("day 2 of video pool ↔ day 2 of gyro pool").
    let mut v_starts: Vec<i64> = video_pool.iter().map(|(c, _)| *c).collect();
    let mut g_starts: Vec<i64> = gyro_pool.iter().map(|(c, _)| *c).collect();
    let v_sessions = session_cluster_starts(&mut v_starts);
    let g_sessions = session_cluster_starts(&mut g_starts);
    if v_sessions.len() == g_sessions.len() && v_sessions.len() >= 2 {
        for (v, g) in v_sessions.iter().zip(g_sessions.iter()) {
            raw.push(g - v);
        }
    }
    let mut out: Vec<i64> = Vec::new();
    for c in raw {
        let near_zero = (c as f64).abs() < tol_ms;
        let near_prev = out.iter().any(|&p| ((c - p) as f64).abs() < tol_ms);
        if !near_zero && !near_prev {
            out.push(c);
        }
    }
    out
}

/// Focused search window on the gyro file's timeline for one candidate under
/// one clock-shift hypothesis: the predicted content span padded by `tol_ms`
/// on both sides, clamped to `[0, gyro_duration]`. None = no intersection
/// (this hypothesis says the video cannot be in this file — no probe).
pub fn focused_window(
    video_created_at_ms: i64,
    video_duration_ms: f64,
    gyro_created_at_ms: i64,
    gyro_duration_ms: f64,
    clock_shift_ms: i64,
    tol_ms: f64,
) -> Option<(f64, f64)> {
    if !gyro_duration_ms.is_finite() || gyro_duration_ms <= 0.0 || !tol_ms.is_finite() || tol_ms <= 0.0 {
        return None;
    }
    let p = predicted_gyro_position_ms(video_created_at_ms, gyro_created_at_ms, clock_shift_ms);
    let start = (p - tol_ms).max(0.0);
    let end = (p + video_duration_ms.max(0.0) + tol_ms).min(gyro_duration_ms);
    if end > start {
        Some((start, end))
    } else {
        None
    }
}

/// Chunk plan for one probe: a focused window is chunked within itself (chunk
/// starts stay file-relative — the accepted-offset absolutization contract is
/// unchanged); a full-span probe (window None) chunks `[0, total]` exactly as
/// the pre-change single-file path did.
pub fn probe_chunk_plan(
    window_ms: Option<(f64, f64)>,
    total_ms: f64,
    chunk_ms: f64,
    overlap_ms: f64,
    max_chunks: usize,
) -> Vec<(f64, f64)> {
    match window_ms {
        Some((s, e)) => {
            let s = s.max(0.0);
            let e = if total_ms > 0.0 { e.min(total_ms) } else { e };
            chunk_plan(e - s, chunk_ms, overlap_ms, max_chunks)
                .into_iter()
                .map(|(a, b)| (a + s, b + s))
                .collect()
        }
        None => chunk_plan(total_ms, chunk_ms, overlap_ms, max_chunks),
    }
}

/// Build the ordered probe plan for a pool-wide deep-match run (spec: tiered
/// timestamp prelocation). Tiers 0-2 place focused probes at predicted
/// positions under successively weaker clock assumptions; tier 3 is the
/// exhaustive full-span fallback over every candidate. Probes on the same file
/// whose predicted positions agree within `tol_ms` are deduplicated across
/// tiers (first tier wins). Correctness is untouched: the plan only orders
/// where to search — every hit still passes the posterior/double gate.
///
/// `video_pool` (all queue videos, `(created_at, duration)`) feeds the tier-2
/// pool alignment. `prelocate = false` (env kill-switch) or a video without
/// created_at degrades to a pure tier-3 plan.
pub fn build_probe_plan(
    video_created_at_ms: Option<i64>,
    video_duration_ms: f64,
    pool: &[GyroPoolEntry],
    video_pool: &[(i64, f64)],
    learned_shift_ms: Option<i64>,
    tol_ms: f64,
    prelocate: bool,
) -> Vec<ProbeTask> {
    let ranked = rank_gyro_candidates(video_created_at_ms, video_duration_ms, pool);
    let by_index: std::collections::HashMap<usize, &GyroPoolEntry> =
        pool.iter().map(|e| (e.gyro_index, e)).collect();
    let mut plan: Vec<ProbeTask> = Vec::new();

    if prelocate && tol_ms > 0.0 {
        if let Some(vc) = video_created_at_ms {
            // (gyro_index, predicted position) of accepted focused probes, for
            // the cross-tier dedup.
            let mut centers: Vec<(usize, f64)> = Vec::new();
            let mut shifts: Vec<(u8, i64)> = Vec::new();
            if let Some(l) = learned_shift_ms {
                shifts.push((0, l));
            }
            shifts.push((1, 0));
            let gyro_meta_pool: Vec<(i64, f64)> = pool
                .iter()
                .filter_map(|e| Some((e.created_at_ms?, e.duration_ms?)))
                .collect();
            for s in pool_shift_candidates(video_pool, &gyro_meta_pool, tol_ms) {
                shifts.push((2, s));
            }
            for (tier, shift) in shifts {
                for &gi in &ranked {
                    let Some(e) = by_index.get(&gi) else { continue };
                    let (Some(gc), Some(gd)) = (e.created_at_ms, e.duration_ms) else {
                        continue;
                    };
                    let Some(w) = focused_window(vc, video_duration_ms, gc, gd, shift, tol_ms)
                    else {
                        continue;
                    };
                    let p = predicted_gyro_position_ms(vc, gc, shift);
                    if centers
                        .iter()
                        .any(|&(g, c)| g == gi && (c - p).abs() < tol_ms)
                    {
                        continue;
                    }
                    centers.push((gi, p));
                    plan.push(ProbeTask { gyro_index: gi, tier, window_ms: Some(w) });
                }
            }
        }
    }
    // Tier 3: exhaustive fallback over every candidate, ranked order (entries
    // without timestamps already sort last).
    for &gi in &ranked {
        plan.push(ProbeTask { gyro_index: gi, tier: 3, window_ms: None });
    }
    plan
}

/// Master switch for the timestamp prelocation tiers of the pool-wide search.
/// `GYROFLOW_DEEP_PRELOCATE=0|off|false` degrades "search all" to the pure
/// tier-3 exhaustive plan. The manual per-file entry never consults this.
pub fn prelocate_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_DEEP_PRELOCATE").ok();
        let (v, source) = match raw.as_deref().map(str::trim) {
            None | Some("") => (true, "default"),
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => (true, "env"),
                "0" | "false" | "no" | "off" => (false, "env"),
                _ => {
                    log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_DEEP_PRELOCATE={} invalid, falling back to default (on)",
                        s
                    );
                    (true, "default")
                }
            },
        };
        log::info!(target: "lifecycle", "deep_prelocate resolved={} source={}", v, source);
        v
    })
}

/// Half-width (ms) of the focused search window around a predicted position
/// (default ±2h — covers "roughly set" clocks; anything larger falls through
/// to the pool-alignment tier or the exhaustive fallback).
pub fn prelocate_tol_ms() -> f64 {
    static RESOLVED: std::sync::OnceLock<f64> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let v = env_f64("GYROFLOW_DEEP_PRELOCATE_TOL_MS", 7_200_000.0);
        log::info!(target: "lifecycle", "deep_prelocate_tol_ms resolved={}", v);
        v
    })
}

/// Floor of the bootstrap auto-probe's focused-window half-width
/// (batch-sync-frontier-recovery): even a near-zero drift estimate keeps a
/// ±30s window so second-level file-timestamp quantisation cannot starve it.
pub const AUTO_PROBE_TOL_FLOOR_MS: f64 = 30_000.0;

/// Half-width of the auto-probe's focused window: twice the expected relay
/// drift over the gap to the nearest confirmed value, floored at 30s. The
/// ladder's last rung widens the same window by `widen_factor` (×1 = base).
/// Compose with [`focused_window`] for the actual clamped file window —
/// far tighter than the pool search's ±2h prelocate tolerance, one probe
/// runs in ~10-20s.
pub fn auto_probe_tol_ms(expected_drift_ms: f64, widen_factor: f64) -> f64 {
    let drift = if expected_drift_ms.is_finite() { expected_drift_ms.abs() } else { 0.0 };
    let widen = if widen_factor.is_finite() { widen_factor.max(1.0) } else { 1.0 };
    (2.0 * drift).max(AUTO_PROBE_TOL_FLOOR_MS) * widen
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
/// Calibrated against real S1H-via-BRAW ground truth (2026-06-20): normal clips
/// whose two probe windows agree on the correct offset landed at ci95≈40ms (blurred
/// by fast/noisy optical flow) and were false-rejected at the old base=25 (gate 35ms),
/// while genuinely ambiguous matches sat at ci95≈100000ms. base=35 (gate 45ms for a
/// single clip, T(D)=10) admits the former and still rejects the latter by a wide margin.
/// `GYROFLOW_DEEP_MATCH_POST_CI95_BASE_MS` overrides.
pub fn post_ci95_base_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_POST_CI95_BASE_MS", 35.0)
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

    // ---- pool-wide prelocation (deep-match-gyro-pool-prelocate) ----

    const HOUR: i64 = 3_600_000;
    const TOL: f64 = 7_200_000.0;

    fn entry(gyro_index: usize, created_h: i64, dur_h: f64) -> GyroPoolEntry {
        GyroPoolEntry {
            gyro_index,
            created_at_ms: Some(created_h * HOUR),
            duration_ms: Some(dur_h * HOUR as f64),
        }
    }

    fn bare_entry(gyro_index: usize) -> GyroPoolEntry {
        GyroPoolEntry { gyro_index, created_at_ms: None, duration_ms: None }
    }

    #[test]
    fn predicted_position_roundtrips_with_derive_session_offset() {
        // The prediction is the algebraic inverse of
        // gyro_match::derive_session_offset_from_deep_match: learning the
        // session offset back from a hit at the predicted position must
        // return the hypothesised clock shift exactly.
        use crate::gyro_match::derive_session_offset_from_deep_match;
        for (vc, gc, shift) in [
            (10 * HOUR, 8 * HOUR, 0i64),
            (10 * HOUR, 8 * HOUR, 8 * HOUR),
            (5 * HOUR, 20 * HOUR, -3 * HOUR - 17 * 60_000),
        ] {
            let p = predicted_gyro_position_ms(vc, gc, shift);
            assert_eq!(derive_session_offset_from_deep_match(gc, vc, -p), shift);
        }
    }

    #[test]
    fn rank_prefers_overlap_then_proximity_and_parks_bare_entries_last() {
        // Video at hour 10, 30min long. File 1 contains it, file 0 is near
        // but disjoint, file 2 is far, file 3 has no metadata.
        let pool = vec![
            entry(0, 8, 1.5), // ends 9.5h — near, no overlap
            entry(1, 9, 2.0), // 9..11h — contains the video
            entry(2, 20, 2.0),
            bare_entry(3),
        ];
        let ranked = rank_gyro_candidates(Some(10 * HOUR), 1_800_000.0, &pool);
        assert_eq!(ranked, vec![1, 0, 2, 3]);
        // No video timestamp -> nothing to rank by, pool order preserved.
        let ranked = rank_gyro_candidates(None, 1_800_000.0, &pool);
        assert_eq!(ranked, vec![0, 1, 2, 3]);
    }

    #[test]
    fn pool_shift_candidates_start_end_and_sessions() {
        // Gyro clocks run 8h ahead of video clocks (timezone misconfig):
        // three sessions on both sides, gyro = video + 8h everywhere.
        let videos: Vec<(i64, f64)> = vec![
            (10 * HOUR, 3_600_000.0),
            (34 * HOUR, 3_600_000.0),
            (58 * HOUR, 3_600_000.0),
        ];
        let gyros: Vec<(i64, f64)> = videos.iter().map(|(c, d)| (c + 8 * HOUR, *d)).collect();
        let cands = pool_shift_candidates(&videos, &gyros, TOL);
        // start/end/session diffs all equal 8h -> a single deduped candidate.
        assert_eq!(cands, vec![8 * HOUR]);
        // Aligned pools (shift ~0) produce no candidates (tier 1 covers zero).
        assert!(pool_shift_candidates(&videos, &videos, TOL).is_empty());
        // Empty side -> empty.
        assert!(pool_shift_candidates(&[], &gyros, TOL).is_empty());
    }

    #[test]
    fn pool_shift_candidates_distinct_start_end() {
        // Gyro pool starts 10h before the video pool but ends 10h after it:
        // start and end diffs disagree beyond tol -> both survive.
        let videos: Vec<(i64, f64)> = vec![(20 * HOUR, 3_600_000.0)];
        let gyros: Vec<(i64, f64)> = vec![(10 * HOUR, 21.0 * HOUR as f64)];
        let cands = pool_shift_candidates(&videos, &gyros, TOL);
        assert_eq!(cands, vec![-10 * HOUR, 10 * HOUR]);
    }

    #[test]
    fn focused_window_clamps_and_rejects_disjoint() {
        // Video at hour 10 (30min), gyro file covers 9..11h, zero shift:
        // predicted position = 1h into the file.
        let w = focused_window(10 * HOUR, 1_800_000.0, 9 * HOUR, 2.0 * HOUR as f64, 0, TOL)
            .expect("window");
        assert_eq!(w.0, 0.0); // 1h - 2h tol clamps to file start
        assert_eq!(w.1, 2.0 * HOUR as f64); // clamps to file end
        // Prediction far outside the file (video 20h after the file ends).
        assert!(
            focused_window(31 * HOUR, 1_800_000.0, 9 * HOUR, 2.0 * HOUR as f64, 0, TOL).is_none()
        );
        // The right shift brings it back inside.
        assert!(focused_window(
            31 * HOUR,
            1_800_000.0,
            9 * HOUR,
            2.0 * HOUR as f64,
            -21 * HOUR,
            TOL
        )
        .is_some());
        // Guards.
        assert!(focused_window(0, 1000.0, 0, 0.0, 0, TOL).is_none());
        assert!(focused_window(0, 1000.0, 0, 1000.0, 0, 0.0).is_none());
    }

    #[test]
    fn auto_probe_tol_floors_scales_and_widens() {
        // Small drift estimates keep the 30s floor; large ones take 2×drift;
        // the ladder's widen rung multiplies the same tolerance once.
        assert_eq!(auto_probe_tol_ms(0.0, 1.0), 30_000.0);
        assert_eq!(auto_probe_tol_ms(5_000.0, 1.0), 30_000.0); // 2×5s < floor
        // 48.4h out at the observed drift (~12.1s expected): 2×12105 = 24.2s
        // still sits under the 30s floor…
        assert_eq!(auto_probe_tol_ms(12_105.0, 1.0), 30_000.0);
        // …but a wider gap (2×18000 = 36s) exceeds it.
        assert_eq!(auto_probe_tol_ms(18_000.0, 1.0), 36_000.0);
        assert_eq!(auto_probe_tol_ms(18_000.0, 4.0), 144_000.0);
        // Guards: non-finite inputs degrade to the floor / no widening.
        assert_eq!(auto_probe_tol_ms(f64::NAN, 1.0), 30_000.0);
        assert_eq!(auto_probe_tol_ms(1_000.0, f64::NAN), 30_000.0);
        assert_eq!(auto_probe_tol_ms(1_000.0, 0.5), 30_000.0);
        // Composes with focused_window: predicted centre ± tol, clamped.
        let tol = auto_probe_tol_ms(18_000.0, 1.0);
        let w = focused_window(10 * HOUR, 60_000.0, 9 * HOUR, 2.0 * HOUR as f64, 0, tol)
            .expect("window");
        assert_eq!(w.0, HOUR as f64 - tol);
        assert_eq!(w.1, HOUR as f64 + 60_000.0 + tol);
    }

    #[test]
    fn probe_chunk_plan_focused_stays_file_relative() {
        // 24h file, focused window [4h, 8h], 2h chunks: chunk starts must be
        // file-relative (the accepted-offset absolutization subtracts them).
        let total = 24.0 * HOUR as f64;
        let window = (4.0 * HOUR as f64, 8.0 * HOUR as f64);
        let plan = probe_chunk_plan(Some(window), total, 7_200_000.0, 100_000.0, 0);
        assert!(plan.len() >= 2);
        assert_eq!(plan[0].0, window.0);
        assert_eq!(plan.last().unwrap().1, window.1);
        // Full-span probe = the pre-change plan.
        assert_eq!(
            probe_chunk_plan(None, total, 7_200_000.0, 100_000.0, 0),
            chunk_plan(total, 7_200_000.0, 100_000.0, 0)
        );
        // Window clamped to the file; degenerate window -> empty plan.
        assert!(probe_chunk_plan(Some((30.0 * HOUR as f64, 31.0 * HOUR as f64)), total, 7_200_000.0, 100_000.0, 0).is_empty());
    }

    #[test]
    fn build_probe_plan_tier_order_dedup_and_fallback() {
        // Two-file pool; the target video sits inside file 0 at zero shift and
        // a second queue video sits inside file 1, so the pools are aligned
        // (all pool-shift candidates collapse to ~0 -> no tier-2 probes).
        // Learned shift ~0 -> tier 0 takes the slot, tier 1's identical
        // prediction dedups away. Tier 3 covers every file.
        let pool = vec![entry(0, 9, 2.0), entry(1, 20, 2.0)];
        let videos: Vec<(i64, f64)> =
            vec![(10 * HOUR, 1_800_000.0), (20 * HOUR + HOUR / 2, 1_800_000.0)];
        let plan = build_probe_plan(
            Some(10 * HOUR),
            1_800_000.0,
            &pool,
            &videos,
            Some(60_000), // learned shift 1min — within tol of tier 1's zero
            TOL,
            true,
        );
        let tiers: Vec<(usize, u8, bool)> = plan
            .iter()
            .map(|t| (t.gyro_index, t.tier, t.window_ms.is_some()))
            .collect();
        // Tier 0 focused probe on file 0 only (file 1 disjoint under ~0 shift),
        // tier 1 deduped, then the two tier-3 full spans in ranked order.
        assert_eq!(tiers, vec![(0, 0, true), (0, 3, false), (1, 3, false)]);

        // Without a learned shift the same probe arrives as tier 1.
        let plan = build_probe_plan(Some(10 * HOUR), 1_800_000.0, &pool, &videos, None, TOL, true);
        assert_eq!(plan[0].tier, 1);

        // prelocate=false -> pure tier-3 plan.
        let plan = build_probe_plan(Some(10 * HOUR), 1_800_000.0, &pool, &videos, None, TOL, false);
        assert!(plan.iter().all(|t| t.tier == 3 && t.window_ms.is_none()));
        assert_eq!(plan.len(), 2);

        // Video without created_at -> pure tier-3 plan too.
        let plan = build_probe_plan(None, 1_800_000.0, &pool, &videos, None, TOL, true);
        assert!(plan.iter().all(|t| t.tier == 3));
    }

    #[test]
    fn build_probe_plan_timezone_case_hits_tier_2() {
        // Gyro clocks 8h ahead: tier 1 finds no intersection anywhere, the
        // pool-alignment shift (8h) predicts file 0 -> tier 2 focused probe.
        let pool = vec![entry(0, 17, 2.0), entry(1, 40, 2.0)];
        let videos: Vec<(i64, f64)> = vec![(10 * HOUR, 1_800_000.0), (33 * HOUR, 1_800_000.0)];
        let gyro_pool_wall: Vec<(i64, f64)> = Vec::new();
        let _ = gyro_pool_wall;
        let plan = build_probe_plan(Some(10 * HOUR), 1_800_000.0, &pool, &videos, None, TOL, true);
        let focused: Vec<&ProbeTask> = plan.iter().filter(|t| t.window_ms.is_some()).collect();
        assert!(!focused.is_empty(), "tier-2 probes expected, plan={plan:?}");
        assert!(focused.iter().all(|t| t.tier == 2), "plan={plan:?}");
        assert_eq!(focused[0].gyro_index, 0);
        // Fallback still covers both files.
        assert_eq!(plan.iter().filter(|t| t.tier == 3).count(), 2);
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
