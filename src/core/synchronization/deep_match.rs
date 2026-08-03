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
static FORWARD_ARMED: Mutex<bool> = Mutex::new(false);
static FORWARD_RESULT: Mutex<Option<ForwardOutcome>> = Mutex::new(None);

/// Arm both collectors and record the scan-K target the essential scan uses to
/// cap how many (highest-motion) windows it fully scans. Only one deep match
/// runs at a time (render queue enforces), so single global slots suffice.
/// Forward re-scoring is disarmed here; the chunk-scan launch opts in via
/// `arm_forward()` (verification probes and the auto-probe never do).
pub fn arm(scan_k: usize) {
    *COLLECTOR.lock() = Some(Vec::new());
    *CURVE_COLLECTOR.lock() = Some(Vec::new());
    *SCAN_K.lock() = scan_k;
    *FORWARD_ARMED.lock() = false;
    *FORWARD_RESULT.lock() = None;
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

/// Take the collected stats and disarm (both collectors + scan-K + forward
/// slots reset). Call `take_curves()` (and `take_forward()`) BEFORE this if
/// you need them — this resets everything.
pub fn take() -> Vec<DeepMatchSegStats> {
    *SCAN_K.lock() = 0;
    *CURVE_COLLECTOR.lock() = None;
    *FORWARD_ARMED.lock() = false;
    *FORWARD_RESULT.lock() = None;
    COLLECTOR.lock().take().unwrap_or_default()
}

/// Drain the curve collector only (does NOT disarm). Call this BEFORE `take()`
/// in the consumer — `take()` then clears the (now-empty) curve slot and
/// disarms. Drain-only here keeps `take()`'s `DeepMatchSegStats` intact for the
/// legacy POSTERIOR=0 path.
pub fn take_curves() -> Vec<DeepMatchWindowCurve> {
    CURVE_COLLECTOR.lock().take().unwrap_or_default()
}

/// Read the collected curves WITHOUT draining them (the forward re-scoring
/// tail inside the essential scan reads them before the run finishes — the
/// normal consumer still gets its curves via `take_curves()`).
pub fn peek_curves() -> Vec<DeepMatchWindowCurve> {
    CURVE_COLLECTOR.lock().clone().unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Forward re-scoring side channel (change deep-match-forward-rescoring)
// ---------------------------------------------------------------------------

/// Outcome of the per-chunk forward re-scoring cascade, recorded by the
/// essential scan tail and consumed by the chunk orchestration
/// (`finish_deep_match`). All quantities are per-chunk: forward costs and the
/// noise floor are NEVER comparable across chunks (different chunks are
/// different gyro data — the same discipline that forbids cross-window
/// `cost_min` comparison).
#[derive(Debug, Clone, PartialEq)]
pub enum ForwardOutcome {
    /// A forward-accepted candidate was confirmed by a full rs-sync call.
    /// `offset_ms` is the median of the confirmation's per-window offsets
    /// (LBFGS precision — this is what gets written back).
    Confirmed {
        offset_ms: f64,
        /// Forward cost of the confirmed candidate over this chunk's noise floor.
        fwd_ratio: f64,
        /// Mean per-window cost of the full rs-sync confirmation run.
        full_cost: f64,
        /// Cross-window spread of the confirmation run (consistency side-gate
        /// only — never the acceptance criterion on its own).
        spread_ms: f64,
        windows: usize,
    },
    /// The forward stage scored the candidates and none sat significantly
    /// below this chunk's noise floor (or confirmation failed) — no hit here.
    Rejected { best_ratio: f64 },
    /// The forward stage declined to decide (dispersed floor / too few
    /// candidates / no scorable windows) — the pre-existing verdict path
    /// drives orchestration unchanged.
    Abstained { reason: &'static str },
}

/// Opt the current armed run into forward re-scoring. Called only by the
/// chunk-scan launch; verification probes and the headless auto-probe re-arm
/// without it (their decision profiles stay unchanged).
pub fn arm_forward() {
    *FORWARD_ARMED.lock() = true;
}

pub fn forward_armed() -> bool {
    is_armed() && *FORWARD_ARMED.lock()
}

pub fn record_forward(outcome: ForwardOutcome) {
    if is_armed() {
        *FORWARD_RESULT.lock() = Some(outcome);
    }
}

/// Drain the forward outcome. Call BEFORE `take()` (which resets the slot).
pub fn take_forward() -> Option<ForwardOutcome> {
    FORWARD_RESULT.lock().take()
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
    // Focused probes dropped because their window covers the entire file and
    // would therefore duplicate the tier-3 fallback verbatim.
    let mut degenerate = 0usize;

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
                    // A focused window clamped to the whole file is the exact
                    // same search as the tier-3 fallback appended below — the
                    // tolerance is simply wider than the file. Running both
                    // scans the file twice for byte-identical results, so drop
                    // the focused probe and let tier 3 cover it. Search
                    // coverage is unchanged; only the duplicate is removed.
                    if w.0 <= 0.0 && w.1 >= gd {
                        degenerate += 1;
                        continue;
                    }
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
    if degenerate > 0 {
        log::info!(
            target: "sync",
            "[deep-match] probe plan: dropped {} focused probe(s) whose window spans the whole file (tol {:.0}ms exceeds the file) — tier 3 covers them",
            degenerate, tol_ms
        );
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
    use crate::synchronization::posterior::{posterior_decide, Prior};
    if curves.is_empty() {
        return DeepMatchVerdict::ProbeNotRun;
    }
    if curves.len() < 2 {
        return DeepMatchVerdict::TooFewWindows;
    }
    let t_d = drift_tolerance_ms(clip_duration_ms, drift_rate_ms_per_min, drift_floor_ms);
    let Some((joint_grid, joint_logl)) = build_joint_grid(curves, t_d) else {
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

/// Shared joint construction for `decide_posterior` and `posterior_peaks`:
/// per-window nuisance-normalized log-likelihoods, drift-dilated by T(D)/2,
/// log-added on a common 5ms grid. Returns None when the windows share no
/// usable offset span.
fn build_joint_grid(curves: &[DeepMatchWindowCurve], t_d_ms: f64) -> Option<(Vec<f64>, Vec<f64>)> {
    use crate::synchronization::posterior::{
        approx_window_log_likelihood, combine_windows_on_common_grid_dilated,
    };
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
    const GRID_STEP_MS: f64 = 5.0;
    combine_windows_on_common_grid_dilated(&views, GRID_STEP_MS, t_d_ms / 2.0)
}

/// Top-K peaks of the joint posterior over the recorded window curves, by
/// descending posterior weight, with non-maximum suppression (peaks closer
/// than `min_separation_ms` to an already-kept stronger peak are dropped).
/// Used as verification hypotheses; cross-window cost_min comparison MUST NOT
/// be used instead (cost magnitude tracks motion strength, not match quality —
/// scale normalization is owned by the nuisance conversion above).
/// Returns an empty vec when no joint can be built or it is entirely flat.
pub fn posterior_peaks(
    curves: &[DeepMatchWindowCurve],
    clip_duration_ms: f64,
    drift_rate_ms_per_min: f64,
    drift_floor_ms: f64,
    k: usize,
    min_separation_ms: f64,
) -> Vec<f64> {
    if curves.len() < 2 || k == 0 {
        return Vec::new();
    }
    let t_d = drift_tolerance_ms(clip_duration_ms, drift_rate_ms_per_min, drift_floor_ms);
    let Some((grid, logl)) = build_joint_grid(curves, t_d) else {
        return Vec::new();
    };
    // Local maxima on the (already sorted, uniform) joint grid. Plateau runs
    // keep their first point. Endpoints qualify against their single neighbor.
    let mut peaks: Vec<(f64, f64)> = Vec::new(); // (offset_ms, logl)
    let n = grid.len();
    for i in 0..n {
        if !logl[i].is_finite() {
            continue;
        }
        let left_ok = i == 0 || !logl[i - 1].is_finite() || logl[i] > logl[i - 1];
        let right_ok = i + 1 >= n || !logl[i + 1].is_finite() || logl[i] >= logl[i + 1];
        if left_ok && right_ok {
            peaks.push((grid[i], logl[i]));
        }
    }
    if peaks.is_empty() {
        return Vec::new();
    }
    // A (near-)flat joint has no evidence — a "peak" on it is numerical
    // noise, not a hypothesis. Require the joint's log-likelihood dynamic
    // range to exceed 0.5 (likelihood ratio ~1.65 between best and worst
    // grid point); genuine valleys sit orders of magnitude above this.
    // The same floor also kills plateau artifacts: flat far-field runs
    // produce "peaks" (first-point-of-plateau rule) sitting exactly on the
    // joint's floor — a real peak must rise above it.
    let max_l = peaks.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let min_l = logl.iter().copied().filter(|v| v.is_finite()).fold(f64::INFINITY, f64::min);
    if !(max_l - min_l > 0.5) {
        return Vec::new();
    }
    peaks.retain(|p| p.1 - min_l > 0.5);
    peaks.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept: Vec<f64> = Vec::new();
    for (pos, _) in peaks {
        if kept.len() >= k {
            break;
        }
        if kept.iter().all(|kp| (kp - pos).abs() >= min_separation_ms) {
            kept.push(pos);
        }
    }
    kept
}

/// Linear interpolation of a sorted `(x, y)` curve at `x`, clamped at the ends.
fn interp_curve_at(curve: &[(f64, f64)], x: f64) -> f64 {
    match curve.binary_search_by(|p| p.0.partial_cmp(&x).unwrap_or(std::cmp::Ordering::Equal)) {
        Ok(i) => curve[i].1,
        Err(0) => curve[0].1,
        Err(i) if i >= curve.len() => curve[curve.len() - 1].1,
        Err(i) => {
            let (x0, y0) = curve[i - 1];
            let (x1, y1) = curve[i];
            if (x1 - x0).abs() < f64::EPSILON {
                y0
            } else {
                y0 + (y1 - y0) * (x - x0) / (x1 - x0)
            }
        }
    }
}

/// Candidate extraction for the forward re-scoring cascade (change
/// deep-match-forward-rescoring): the per-window curves are combined into a
/// joint log-likelihood on a `lattice_step_ms` lattice over the windows'
/// shared offset domain, and the top-N lattice positions are kept with greedy
/// non-maximum suppression at `nms_radius_ms`. Returned centers are ordered by
/// descending joint value.
///
/// Deliberately different from `posterior_peaks` (verification hypotheses:
/// only genuine peaks above the joint floor): the forward stage NEEDS
/// noise-floor candidates too — they form this chunk's per-chunk reference
/// statistics for the relative acceptance criterion. N, the NMS radius and
/// the lattice step are configured together because the true offset's
/// candidate rank depends on all three.
pub fn forward_candidates(
    curves: &[DeepMatchWindowCurve],
    lattice_step_ms: f64,
    nms_radius_ms: f64,
    top_n: usize,
) -> Vec<f64> {
    use crate::synchronization::posterior::approx_window_log_likelihood;
    if curves.len() < 2
        || top_n == 0
        || !lattice_step_ms.is_finite()
        || lattice_step_ms <= 0.0
        || !nms_radius_ms.is_finite()
        || nms_radius_ms < 0.0
    {
        return Vec::new();
    }
    // Sorted copies (recorded curves interleave the coarse lattice, the dense
    // refinement points and the refined argmin — order is not guaranteed).
    let sorted: Vec<Vec<(f64, f64)>> = curves
        .iter()
        .map(|c| {
            let mut v: Vec<(f64, f64)> = c
                .curve
                .iter()
                .copied()
                .filter(|(x, y)| x.is_finite() && y.is_finite())
                .collect();
            v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            v
        })
        .collect();
    let lo = sorted
        .iter()
        .filter_map(|c| c.first().map(|p| p.0))
        .fold(f64::NEG_INFINITY, f64::max);
    let hi = sorted
        .iter()
        .filter_map(|c| c.last().map(|p| p.0))
        .fold(f64::INFINITY, f64::min);
    if !(hi > lo) {
        return Vec::new();
    }
    let n = ((hi - lo) / lattice_step_ms) as usize + 1;
    let mut joint = vec![0.0f64; n];
    for (c, sc) in curves.iter().zip(sorted.iter()) {
        if sc.is_empty() || !c.cost_min.is_finite() || c.cost_min <= 0.0 {
            continue;
        }
        for (k, j) in joint.iter_mut().enumerate() {
            let cost = interp_curve_at(sc, lo + k as f64 * lattice_step_ms);
            let l = approx_window_log_likelihood(cost, c.cost_min, c.n_eff);
            if l.is_finite() {
                *j += l;
            }
        }
    }
    // Greedy NMS: walk lattice positions by descending joint value, keep a
    // position only when nothing within the NMS radius was kept before it.
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| joint[b].partial_cmp(&joint[a]).unwrap_or(std::cmp::Ordering::Equal));
    let rpts = (nms_radius_ms / lattice_step_ms).ceil() as usize;
    let mut avail = vec![true; n];
    let mut kept: Vec<f64> = Vec::new();
    for i in order {
        if !avail[i] {
            continue;
        }
        kept.push(lo + i as f64 * lattice_step_ms);
        for a in avail[i.saturating_sub(rpts)..(i + rpts + 1).min(n)].iter_mut() {
            *a = false;
        }
        if kept.len() >= top_n {
            break;
        }
    }
    kept
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForwardFloorDecision {
    /// The best candidate sits significantly below the noise floor.
    Accept,
    /// No candidate separates from the floor — no hit in this chunk.
    Reject,
    /// The floor is unusable (dispersed / too few candidates) — the relative
    /// criterion cannot judge; the pre-existing verdict path decides.
    Abstain,
}

/// Statistics behind a forward floor decision (all per-chunk; NaN when the
/// input was unusable).
#[derive(Debug, Clone, Copy)]
pub struct ForwardFloorVerdict {
    pub decision: ForwardFloorDecision,
    /// Median of the non-best candidates' forward costs (the noise floor).
    pub floor: f64,
    /// Relative IQR of the non-best candidates: (p75 - p25) / median.
    pub dispersion: f64,
    /// Best forward cost over the floor.
    pub best_ratio: f64,
}

fn percentile_sorted(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let idx = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Relative acceptance criterion for the forward re-scoring stage (change
/// deep-match-forward-rescoring, design D2): the decision is whether the best
/// candidate is significantly below the noise floor formed by the REMAINING
/// candidates of the same chunk — never an absolute cost threshold (forward
/// cost magnitude varies with footage and gyro segment). A dispersed floor
/// (relative IQR above `dispersion_max`) abstains instead of guessing;
/// fewer than `min_candidates` usable costs also abstain (no floor statistics).
pub fn forward_floor_decision(
    costs: &[f64],
    accept_max_ratio: f64,
    dispersion_max: f64,
    min_candidates: usize,
) -> ForwardFloorVerdict {
    let mut v: Vec<f64> = costs
        .iter()
        .copied()
        .filter(|c| c.is_finite() && *c > 0.0)
        .collect();
    let abstain = |floor: f64, dispersion: f64, best_ratio: f64| ForwardFloorVerdict {
        decision: ForwardFloorDecision::Abstain,
        floor,
        dispersion,
        best_ratio,
    };
    if v.len() < min_candidates.max(3) {
        return abstain(f64::NAN, f64::NAN, f64::NAN);
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let best = v[0];
    let others = &v[1..];
    let floor = percentile_sorted(others, 0.5);
    if !floor.is_finite() || floor <= 0.0 {
        return abstain(floor, f64::NAN, f64::NAN);
    }
    let dispersion = (percentile_sorted(others, 0.75) - percentile_sorted(others, 0.25)) / floor;
    let best_ratio = best / floor;
    let decision = if dispersion > dispersion_max {
        ForwardFloorDecision::Abstain
    } else if best_ratio <= accept_max_ratio {
        ForwardFloorDecision::Accept
    } else {
        ForwardFloorDecision::Reject
    };
    ForwardFloorVerdict { decision, floor, dispersion, best_ratio }
}

/// Pick verification-window positions (video timestamps, ms) by scanning the
/// loaded chunk gyro's angular-rate series for motion hotspots under the
/// hypothesis `video_ts = gyro_ts + delta_ms` (same offset convention as
/// `search_domain`: delta = video_ts − gyro_ts).
///
/// `gyro_rate_dps` is (gyro_ts_ms, |angular rate| in °/s), ascending, may be
/// downsampled by the caller. Hotspot strength is the sliding mean over
/// `window_ms`. Constraints: mapped position stays inside
/// [window_ms/2, duration − window_ms/2]; ≥ `max(60s, duration/10)` away from
/// every already-scanned center and every other pick. Picks are greedy by
/// strength with a tail-coverage swap (if nothing lands past 70% of the clip
/// but an eligible tail candidate exists, the weakest pick is replaced).
/// Returns fewer than `n` (possibly none) when candidates run out.
pub fn pick_verify_windows(
    gyro_rate_dps: &[(f64, f64)],
    delta_ms: f64,
    video_duration_ms: f64,
    scanned_centers_ms: &[f64],
    n: usize,
    window_ms: f64,
    hot_min_dps: f64,
) -> Vec<f64> {
    if n == 0 || !video_duration_ms.is_finite() || video_duration_ms <= 0.0 || gyro_rate_dps.len() < 2 {
        return Vec::new();
    }
    let half_win = (window_ms.max(0.0)) / 2.0;
    let lo = half_win;
    let hi = video_duration_ms - half_win;
    if hi <= lo {
        return Vec::new();
    }
    // Sliding-window mean via two pointers (series may be irregularly sampled).
    let mut candidates: Vec<(f64, f64)> = Vec::new(); // (video_ts, strength)
    let mut start = 0usize;
    let mut sum = 0.0f64;
    let mut count = 0usize;
    let mut end = 0usize;
    for i in 0..gyro_rate_dps.len() {
        let center_ts = gyro_rate_dps[i].0;
        while end < gyro_rate_dps.len() && gyro_rate_dps[end].0 <= center_ts + half_win {
            sum += gyro_rate_dps[end].1;
            count += 1;
            end += 1;
        }
        while start < gyro_rate_dps.len() && gyro_rate_dps[start].0 < center_ts - half_win {
            sum -= gyro_rate_dps[start].1;
            count -= 1;
            start += 1;
        }
        if count == 0 {
            continue;
        }
        let strength = sum / count as f64;
        if strength < hot_min_dps {
            continue;
        }
        let video_ts = center_ts + delta_ms;
        if video_ts >= lo && video_ts <= hi {
            candidates.push((video_ts, strength));
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let spacing = (video_duration_ms / 10.0).max(60_000.0);
    let ok = |pos: f64, picked: &[(f64, f64)]| {
        scanned_centers_ms.iter().all(|c| (c - pos).abs() >= spacing)
            && picked.iter().all(|(p, _)| (p - pos).abs() >= spacing)
    };
    let mut picked: Vec<(f64, f64)> = Vec::new();
    for &(pos, s) in &candidates {
        if picked.len() >= n {
            break;
        }
        if ok(pos, &picked) {
            picked.push((pos, s));
        }
    }
    // Tail coverage: mutually-distant placement multiplies down the
    // false-alignment probability; make sure the clip tail participates when
    // it can.
    let tail_from = 0.7 * video_duration_ms;
    if picked.len() >= 2 && !picked.iter().any(|(p, _)| *p >= tail_from) {
        if let Some(&(tp, ts)) = candidates.iter().find(|(pos, _)| {
            *pos >= tail_from && {
                let others: Vec<(f64, f64)> =
                    picked[..picked.len() - 1].to_vec();
                ok(*pos, &others)
            }
        }) {
            let last = picked.len() - 1; // weakest kept pick (strength-ordered)
            picked[last] = (tp, ts);
        }
    }
    picked.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    picked.into_iter().map(|(p, _)| p).collect()
}

/// Window-level verification gate: the verification window individually
/// confirms the hypothesis iff its refined argmin lands within
/// `align_tol_ms` of `delta_ms` AND its local curve is non-flat
/// (`cost_min / p25(local domain) <= ratio_max`). Within-window ratio is
/// scale-legitimate (single window); cross-window cost comparison is not.
pub fn verify_window_aligned(
    curve: &DeepMatchWindowCurve,
    delta_ms: f64,
    align_tol_ms: f64,
    ratio_max: f64,
) -> bool {
    if !(curve.argmin_ms - delta_ms).abs().is_finite() || (curve.argmin_ms - delta_ms).abs() > align_tol_ms {
        return false;
    }
    let mut costs: Vec<f64> = curve
        .curve
        .iter()
        .map(|p| p.1)
        .filter(|c| c.is_finite() && *c > 0.0)
        .collect();
    if costs.len() < 4 || !curve.cost_min.is_finite() || curve.cost_min <= 0.0 {
        return false;
    }
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let p25 = costs[costs.len() / 4];
    p25 > 0.0 && curve.cost_min / p25 <= ratio_max
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

/// Master switch for the posterior-peak verification pass
/// (change deep-match-peak-verification). `GYROFLOW_DEEP_MATCH_VERIFY=0|off|
/// false|no` disables it byte-for-byte; any other value (or unset) enables.
/// OnceLock-cached; first resolve logs to `target="lifecycle"`.
pub fn verify_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_DEEP_MATCH_VERIFY").ok();
        let (v, source) = match raw.as_deref().map(str::trim) {
            None | Some("") => (true, "default"),
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => (true, "env"),
                "0" | "false" | "no" | "off" => (false, "env"),
                _ => {
                    log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_DEEP_MATCH_VERIFY={} invalid, falling back to default (on)",
                        s
                    );
                    (true, "default")
                }
            },
        };
        log::info!(target: "lifecycle", "deep_match_verify resolved={} source={}", v, source);
        v
    })
}

/// Confidence floor for the merged (base + verification) re-decision.
/// Tighten-only: clamped to be no lower than the regular `post_conf_min()`.
pub fn verify_conf_min() -> f64 {
    let raw = env_f64("GYROFLOW_DEEP_MATCH_VERIFY_CONF_MIN", 0.6);
    let floor = post_conf_min();
    if raw < floor {
        log::warn!(
            target: "sync",
            "[deep-match] GYROFLOW_DEEP_MATCH_VERIFY_CONF_MIN={} below regular conf gate {}, clamping (verification must not loosen)",
            raw, floor
        );
        floor
    } else {
        raw
    }
}

/// Number of posterior peaks tried as verification hypotheses (clamp 1-3).
pub fn verify_top_k() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_VERIFY_TOP_K", 1.0) as usize).clamp(1, 3)
}

/// Target verification-window count per hypothesis (clamp 2-4).
pub fn verify_windows() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_VERIFY_WINDOWS", 3.0) as usize).clamp(2, 4)
}

/// Minimum individually-aligned verification windows required to upgrade
/// (window-level gate M; clamp 1-4).
pub fn verify_min_aligned() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_VERIFY_MIN_ALIGNED", 2.0) as usize).clamp(1, 4)
}

/// Local scan radius around the hypothesis (T(D) is added on top by the
/// orchestration) — a local scan, not a full-domain scan.
pub fn verify_local_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_VERIFY_LOCAL_MS", 1500.0)
}

/// Window-level argmin alignment tolerance (T(D) is added on top).
pub fn verify_align_tol_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_VERIFY_ALIGN_TOL_MS", 250.0)
}

/// Window-level non-flatness ceiling (cost_min / local p25) for verification
/// windows.
pub fn verify_ratio() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_VERIFY_RATIO", 0.6)
}

/// Gyro hotspot strength floor (°/s) for verification-window placement. Gyro
/// rates are ground truth (unaffected by lens-matrix scaling), so this sits
/// above the OF-side motion gate.
pub fn verify_gyro_hot_min_dps() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_VERIFY_GYRO_HOT_MIN_DPS", 5.0)
}

/// Master switch for the forward re-scoring cascade (change
/// deep-match-forward-rescoring). `GYROFLOW_DEEP_MATCH_FORWARD=0|off|false|no`
/// disables it byte-for-byte (no forward scoring, no rs-sync problem assembled;
/// the chunk verdict comes from the pre-existing joint-posterior path); any
/// other value (or unset) enables. OnceLock-cached; first resolve logs to
/// `target="lifecycle"`.
pub fn forward_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        let raw = std::env::var("GYROFLOW_DEEP_MATCH_FORWARD").ok();
        let (v, source) = match raw.as_deref().map(str::trim) {
            None | Some("") => (true, "default"),
            Some(s) => match s.to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => (true, "env"),
                "0" | "false" | "no" | "off" => (false, "env"),
                _ => {
                    log::warn!(
                        target: "lifecycle",
                        "GYROFLOW_DEEP_MATCH_FORWARD={} invalid, falling back to default (on)",
                        s
                    );
                    (true, "default")
                }
            },
        };
        log::info!(target: "lifecycle", "deep_match_forward resolved: enabled={} source={}", v, source);
        v
    })
}

/// Candidate count extracted from the joint posterior (clamp 4-400). Coupled
/// with the NMS radius and lattice step: the observed true-offset candidate
/// rank was #29 on a 25ms lattice with ±10s NMS — 50 leaves headroom.
pub fn fwd_top_n() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_FWD_TOP_N", 50.0) as usize).clamp(4, 400)
}
/// Non-maximum-suppression radius for candidate extraction; also the default
/// full-confirmation search radius (candidates are distinct at this scale).
pub fn fwd_nms_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_FWD_NMS_MS", 10_000.0)
}
/// Joint-posterior lattice step for candidate extraction. 25ms matches the
/// recorded coarse curves; peak positions only need to land inside the
/// forward local-grid radius.
pub fn fwd_lattice_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_FWD_LATTICE_MS", 25.0)
}
/// Forward `pre_sync` local-grid radius around each candidate (±ms).
pub fn fwd_radius_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_FWD_RADIUS_MS", 100.0)
}
/// Forward `pre_sync` local-grid step (ms).
pub fn fwd_step_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_FWD_STEP_MS", 5.0)
}
/// How many forward-accepted candidates may be tried with a full rs-sync
/// confirmation call (design D4: top 1-3 by forward rank).
pub fn fwd_confirm_n() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_FWD_CONFIRM_N", 2.0) as usize).clamp(1, 3)
}
/// Search radius of the full rs-sync confirmation call around a candidate.
pub fn fwd_confirm_radius_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_FWD_CONFIRM_RADIUS_MS", 10_000.0)
}
/// Relative acceptance ceiling: best forward cost over the chunk's noise
/// floor must be at or below this. Conservative starting point from the
/// single calibrated case (true hit 0.63, noise-only best ~0.97); pending
/// corpus calibration (tasks 5.4).
pub fn fwd_accept_ratio() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_FWD_ACCEPT_RATIO", 0.8)
}
/// Floor-dispersion abstain threshold: relative IQR of the non-best
/// candidates above this means the "tight noise floor" assumption does not
/// hold — abstain instead of guessing (observed tight floors span ~4.5%).
pub fn fwd_floor_dispersion_max() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_FWD_FLOOR_DISPERSION", 0.25)
}
/// Minimum scored candidates required for usable floor statistics (clamp >=3).
pub fn fwd_min_candidates() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_FWD_MIN_CANDIDATES", 8.0) as usize).max(3)
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
    fn forward_slot_roundtrip_and_arming() {
        let _g = TEST_MTX.lock().unwrap_or_else(|e| e.into_inner());
        arm(2);
        // arm() alone must NOT arm the forward stage (verify probes and the
        // auto-probe re-arm without it).
        assert!(!forward_armed());
        arm_forward();
        assert!(forward_armed());
        record_forward(ForwardOutcome::Rejected { best_ratio: 0.95 });
        assert_eq!(take_forward(), Some(ForwardOutcome::Rejected { best_ratio: 0.95 }));
        assert_eq!(take_forward(), None, "take_forward drains");
        // take() resets the forward arming for the next run.
        let _ = take();
        assert!(!forward_armed());
        // Recording while disarmed is dropped.
        record_forward(ForwardOutcome::Abstained { reason: "test" });
        assert_eq!(take_forward(), None);
        // Re-arming resets any stale outcome.
        arm(2);
        arm_forward();
        record_forward(ForwardOutcome::Abstained { reason: "stale" });
        arm(2);
        assert_eq!(take_forward(), None, "arm() clears the previous outcome");
        let _ = take();
    }

    // Synthetic window curve: flat plateau `plateau` with a triangular valley
    // of half-width `valley_half_ms` down to `valley_cost` at `valley_at`.
    fn synth_curve(
        range_idx: usize,
        valley_at: f64,
        lo: f64,
        hi: f64,
        step: f64,
        plateau: f64,
        valley_cost: f64,
        valley_half_ms: f64,
    ) -> DeepMatchWindowCurve {
        let mut curve = Vec::new();
        let mut x = lo;
        while x <= hi {
            let d = (x - valley_at).abs();
            let cost = if d < valley_half_ms {
                valley_cost + (plateau - valley_cost) * d / valley_half_ms
            } else {
                plateau
            };
            curve.push((x, cost));
            x += step;
        }
        DeepMatchWindowCurve {
            range_idx,
            t_center_ms: 0.0,
            argmin_ms: valley_at,
            cost_min: valley_cost,
            n_eff: 100.0,
            curve,
        }
    }

    #[test]
    fn forward_candidates_top_candidate_hits_shared_valley() {
        let c0 = synth_curve(0, -5000.0, -100_000.0, 100_000.0, 100.0, 100.0, 10.0, 300.0);
        let c1 = synth_curve(1, -5000.0, -100_000.0, 100_000.0, 100.0, 100.0, 10.0, 300.0);
        let cands = forward_candidates(&[c0, c1], 25.0, 10_000.0, 50);
        assert!(!cands.is_empty());
        assert!(cands.len() <= 50);
        // Strongest joint peak lands on the shared valley (within one lattice step).
        assert!(
            (cands[0] + 5000.0).abs() <= 25.0 + 1e-9,
            "top candidate {} not at the valley",
            cands[0]
        );
        // NMS separation holds pairwise.
        for i in 0..cands.len() {
            for j in (i + 1)..cands.len() {
                assert!(
                    (cands[i] - cands[j]).abs() >= 10_000.0 - 1e-6,
                    "candidates {} and {} violate the NMS radius",
                    cands[i],
                    cands[j]
                );
            }
        }
    }

    #[test]
    fn forward_candidates_lattice_nms_topn_configured_together() {
        let c0 = synth_curve(0, 0.0, -50_000.0, 50_000.0, 100.0, 100.0, 10.0, 300.0);
        let c1 = synth_curve(1, 0.0, -50_000.0, 50_000.0, 100.0, 100.0, 10.0, 300.0);
        let curves = vec![c0, c1];
        // top_n caps the count.
        assert!(forward_candidates(&curves, 25.0, 5_000.0, 5).len() <= 5);
        // A wider NMS radius spreads the candidates further apart.
        let wide = forward_candidates(&curves, 25.0, 30_000.0, 10);
        for i in 0..wide.len() {
            for j in (i + 1)..wide.len() {
                assert!((wide[i] - wide[j]).abs() >= 30_000.0 - 1e-6);
            }
        }
        // Candidates sit on the configured lattice.
        let coarse = forward_candidates(&curves, 200.0, 5_000.0, 10);
        let lo = -50_000.0;
        for c in &coarse {
            let steps = (c - lo) / 200.0;
            assert!(
                (steps - steps.round()).abs() < 1e-9,
                "candidate {} off the 200ms lattice",
                c
            );
        }
    }

    #[test]
    fn forward_candidates_degenerate_inputs_yield_empty() {
        let c0 = synth_curve(0, 0.0, -10_000.0, 10_000.0, 100.0, 100.0, 10.0, 300.0);
        // Fewer than 2 curves.
        assert!(forward_candidates(&[c0.clone()], 25.0, 10_000.0, 50).is_empty());
        // Disjoint domains (no shared offset span).
        let far = synth_curve(1, 90_000.0, 80_000.0, 100_000.0, 100.0, 100.0, 10.0, 300.0);
        assert!(forward_candidates(&[c0.clone(), far], 25.0, 10_000.0, 50).is_empty());
        // Invalid lattice/top_n.
        let c1 = synth_curve(1, 0.0, -10_000.0, 10_000.0, 100.0, 100.0, 10.0, 300.0);
        assert!(forward_candidates(&[c0.clone(), c1.clone()], 0.0, 10_000.0, 50).is_empty());
        assert!(forward_candidates(&[c0, c1], 25.0, 10_000.0, 0).is_empty());
    }

    #[test]
    fn forward_floor_accepts_the_case_study_shape() {
        // 2026-08-02 Fuji case: true candidate 197, 49 noise candidates
        // clustered in 312..=326 (floor span ~4.5%).
        let mut costs = vec![197.0];
        costs.extend((0..49).map(|i| 312.0 + 14.0 * i as f64 / 48.0));
        let v = forward_floor_decision(&costs, 0.8, 0.25, 8);
        assert_eq!(v.decision, ForwardFloorDecision::Accept);
        assert!(v.best_ratio < 0.65, "ratio {}", v.best_ratio);
        assert!(v.dispersion < 0.05, "dispersion {}", v.dispersion);
    }

    #[test]
    fn forward_floor_abstains_on_dispersed_floor() {
        let costs = vec![100.0, 150.0, 250.0, 400.0, 700.0, 1200.0, 2000.0, 3300.0, 5000.0];
        let v = forward_floor_decision(&costs, 0.8, 0.25, 8);
        assert_eq!(v.decision, ForwardFloorDecision::Abstain);
        assert!(v.dispersion > 0.25);
    }

    #[test]
    fn forward_floor_rejects_when_no_candidate_separates() {
        let costs: Vec<f64> = (0..17).map(|i| 310.0 + i as f64).collect();
        let v = forward_floor_decision(&costs, 0.8, 0.25, 8);
        assert_eq!(v.decision, ForwardFloorDecision::Reject);
        assert!(v.best_ratio > 0.9);
    }

    #[test]
    fn forward_floor_abstains_without_enough_candidates() {
        let v = forward_floor_decision(&[1.0, 2.0, 3.0], 0.8, 0.25, 8);
        assert_eq!(v.decision, ForwardFloorDecision::Abstain);
        // Non-finite / non-positive entries are filtered before the count gate.
        let v = forward_floor_decision(
            &[f64::NAN, -1.0, 0.0, 197.0, 312.0, 315.0, 318.0, 321.0, 324.0],
            0.8,
            0.25,
            8,
        );
        assert_eq!(v.decision, ForwardFloorDecision::Abstain);
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

    // ---- posterior-peak verification (deep-match-peak-verification) ----

    #[test]
    fn posterior_peaks_single_peak_and_kill_conditions() {
        // Two agreeing windows -> one clear joint peak near -100.
        let curves = vec![wc(0, 1000.0, -100.0, 75.0), wc(1, 2000.0, -98.0, 75.0)];
        let peaks = posterior_peaks(&curves, 120_000.0, 2.0, 10.0, 3, 30.0);
        assert_eq!(peaks.len(), 1, "one true valley must yield exactly one peak, got {peaks:?}");
        assert!((peaks[0] + 100.0).abs() <= 15.0, "peak should sit near -100, got {}", peaks[0]);
        // k=0 and <2 curves return empty.
        assert!(posterior_peaks(&curves, 120_000.0, 2.0, 10.0, 0, 30.0).is_empty());
        assert!(posterior_peaks(&curves[..1], 120_000.0, 2.0, 10.0, 3, 30.0).is_empty());
    }

    #[test]
    fn posterior_peaks_two_separated_peaks_ordered_by_mass() {
        // Short clip (T(D)=10ms): 80ms-apart window peaks stay separated in
        // the joint -> two local maxima. n_eff asymmetry makes the -140 peak
        // strictly stronger, so it must come first (descending posterior).
        let curves = vec![wc(0, 1_000.0, -140.0, 150.0), wc(1, 59_000.0, -60.0, 75.0)];
        let peaks = posterior_peaks(&curves, 60_000.0, 2.0, 10.0, 2, 30.0);
        assert_eq!(peaks.len(), 2, "expected both separated peaks, got {peaks:?}");
        assert!((peaks[0] + 140.0).abs() <= 15.0, "stronger peak first, got {peaks:?}");
        assert!((peaks[1] + 60.0).abs() <= 15.0, "weaker peak second, got {peaks:?}");
        // k=1 keeps only the strongest.
        let top1 = posterior_peaks(&curves, 60_000.0, 2.0, 10.0, 1, 30.0);
        assert_eq!(top1.len(), 1);
        assert!((top1[0] + 140.0).abs() <= 15.0);
    }

    #[test]
    fn posterior_peaks_flat_noise_returns_empty() {
        let curves = vec![flat_wc(0, 1000.0), flat_wc(1, 2000.0)];
        assert!(
            posterior_peaks(&curves, 120_000.0, 2.0, 10.0, 3, 30.0).is_empty(),
            "flat joint must produce no hypothesis"
        );
    }

    #[test]
    fn posterior_peaks_nms_suppresses_same_peak_resamples() {
        // Two windows whose valleys are 5ms apart merge into one joint peak
        // region; with min_separation=30ms only one hypothesis may survive.
        let curves = vec![wc(0, 1000.0, -100.0, 75.0), wc(1, 2000.0, -95.0, 75.0)];
        let peaks = posterior_peaks(&curves, 120_000.0, 2.0, 10.0, 3, 30.0);
        assert_eq!(peaks.len(), 1, "NMS must keep a single peak, got {peaks:?}");
    }

    fn gyro_series_with_hotspots(hotspots: &[(f64, f64)]) -> Vec<(f64, f64)> {
        // 0..700s at 1Hz, 1°/s background, boxcar hotspots of ±5s.
        (0..=700)
            .map(|k| {
                let ts = k as f64 * 1000.0;
                let dps = hotspots
                    .iter()
                    .find(|(c, _)| (ts - c).abs() <= 5_000.0)
                    .map(|(_, s)| *s)
                    .unwrap_or(1.0);
                (ts, dps)
            })
            .collect()
    }

    #[test]
    fn pick_verify_windows_maps_hotspots_and_orders_ascending() {
        // delta = video_ts - gyro_ts = -100s: gyro hotspot at 200s -> video 100s.
        let gyro = gyro_series_with_hotspots(&[(200_000.0, 30.0), (400_000.0, 25.0), (600_000.0, 20.0)]);
        let picks = pick_verify_windows(&gyro, -100_000.0, 600_000.0, &[], 3, 2_500.0, 5.0);
        assert_eq!(picks.len(), 3, "three hotspots should yield three windows, got {picks:?}");
        assert!((picks[0] - 100_000.0).abs() <= 6_000.0, "got {picks:?}");
        assert!((picks[1] - 300_000.0).abs() <= 6_000.0, "got {picks:?}");
        assert!((picks[2] - 500_000.0).abs() <= 6_000.0, "got {picks:?}");
        assert!(picks.windows(2).all(|w| w[0] < w[1]), "ascending order required");
    }

    #[test]
    fn pick_verify_windows_weak_gyro_returns_empty() {
        // Background 1°/s everywhere, floor 5°/s -> nothing qualifies.
        let gyro = gyro_series_with_hotspots(&[]);
        assert!(pick_verify_windows(&gyro, -100_000.0, 600_000.0, &[], 3, 2_500.0, 5.0).is_empty());
    }

    #[test]
    fn pick_verify_windows_respects_spacing_and_scanned_centers() {
        // Two hotspots 20s apart (< spacing 60s) -> only the stronger one kept.
        let gyro = gyro_series_with_hotspots(&[(200_000.0, 30.0), (220_000.0, 25.0)]);
        let picks = pick_verify_windows(&gyro, -100_000.0, 600_000.0, &[], 3, 2_500.0, 5.0);
        assert_eq!(picks.len(), 1, "spacing must collapse near hotspots, got {picks:?}");
        assert!((picks[0] - 100_000.0).abs() <= 6_000.0);
        // A scanned center on top of the mapped hotspot excludes it entirely.
        let picks2 = pick_verify_windows(&gyro, -100_000.0, 600_000.0, &[100_000.0], 3, 2_500.0, 5.0);
        assert!(picks2.is_empty(), "scanned-center avoidance failed: {picks2:?}");
    }

    #[test]
    fn pick_verify_windows_covers_the_tail_when_possible() {
        // Strong hotspots early, a weaker one in the tail (>70% of 600s = 420s).
        // n=2 greedy would pick the two strong early ones; tail coverage must
        // swap the weakest pick for the tail candidate.
        let gyro = gyro_series_with_hotspots(&[(150_000.0, 30.0), (300_000.0, 28.0), (620_000.0, 10.0)]);
        let picks = pick_verify_windows(&gyro, -100_000.0, 600_000.0, &[], 2, 2_500.0, 5.0);
        assert_eq!(picks.len(), 2);
        assert!(picks.iter().any(|p| *p >= 420_000.0), "tail must be covered, got {picks:?}");
    }

    #[test]
    fn pick_verify_windows_clamps_to_video_domain() {
        // Hotspot mapping outside [half_win, duration-half_win] is dropped.
        let gyro = gyro_series_with_hotspots(&[(50_000.0, 30.0)]);
        // delta -100s -> video -50s: out of domain -> empty.
        assert!(pick_verify_windows(&gyro, -100_000.0, 600_000.0, &[], 3, 2_500.0, 5.0).is_empty());
    }

    #[test]
    fn verify_window_aligned_gates() {
        // Aligned deep valley (wc: cost_min 1.0 at peak, plateau ~2.0 -> ratio ~0.5).
        let good = wc(100, 100_000.0, -19_800.0, 58.0);
        assert!(verify_window_aligned(&good, -19_855.0, 250.0, 0.6));
        // Misplaced valley: argmin 400ms away from the hypothesis.
        let misplaced = wc(101, 200_000.0, -19_400.0, 58.0);
        assert!(!verify_window_aligned(&misplaced, -19_855.0, 250.0, 0.6));
        // Flat local curve: ratio ~1.0 fails the non-flatness gate even when
        // the argmin happens to sit on the hypothesis.
        let mut flat = flat_wc(102, 300_000.0);
        flat.argmin_ms = -19_855.0;
        assert!(!verify_window_aligned(&flat, -19_855.0, 250.0, 0.6));
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
        // File 0 is 10h long so the ±2h focused window genuinely narrows the
        // search (a window clamped to the whole file is dropped as a tier-3
        // duplicate, which would hide the tier ordering this test asserts).
        let pool = vec![entry(0, 9, 10.0), entry(1, 20, 2.0)];
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
    fn build_probe_plan_drops_focused_probes_that_span_the_whole_file() {
        // Field case (2026-08-02 Fujifilm + 1.81h gyro bin): the ±2h tolerance
        // is wider than the file, so every focused window clamps to [0, dur]
        // and re-runs the tier-3 scan verbatim — three probes returning
        // byte-identical results. The planner must drop those duplicates while
        // leaving search coverage intact.
        const F0: f64 = 1.81 * HOUR as f64;
        let pool = vec![entry(0, 9, 1.81), entry(1, 11, 1.5)];
        let videos: Vec<(i64, f64)> = vec![(10 * HOUR, 1_800_000.0)];
        let plan = build_probe_plan(Some(10 * HOUR), 1_800_000.0, &pool, &videos, None, TOL, true);

        // The invariant: no surviving focused probe may span its entire file —
        // such a window IS the tier-3 scan and would run the file twice.
        for t in plan.iter().filter(|t| t.gyro_index == 0) {
            if let Some((s, e)) = t.window_ms {
                assert!(s > 0.0 || e < F0, "probe spans the whole file: {t:?}");
            }
        }
        // Zero-shift (tier 1) is the degenerate one here and must be gone.
        assert!(
            !plan.iter().any(|t| t.tier == 1),
            "degenerate tier-1 probe survived: {plan:?}"
        );
        // A shift that moves the prediction off the file start still yields a
        // genuinely narrowed window, which must be kept.
        assert!(
            plan.iter().any(|t| matches!(t.window_ms, Some((s, _)) if s > 0.0)),
            "a genuinely focused probe must survive: {plan:?}"
        );
        // Coverage unchanged: every pool file still gets its tier-3 scan.
        let mut covered: Vec<usize> =
            plan.iter().filter(|t| t.tier == 3).map(|t| t.gyro_index).collect();
        covered.sort_unstable();
        assert_eq!(covered, vec![0, 1]);
    }

    #[test]
    fn build_probe_plan_timezone_case_hits_tier_2() {
        // Gyro clocks 8h ahead: tier 1 finds no intersection anywhere, the
        // pool-alignment shift predicts file 0 -> tier 2 focused probe.
        // File 0 is 10h long so that focused window genuinely narrows the
        // search; a window clamped to the whole file is dropped as a tier-3
        // duplicate and would leave nothing for this test to assert on.
        let pool = vec![entry(0, 17, 10.0), entry(1, 40, 2.0)];
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
