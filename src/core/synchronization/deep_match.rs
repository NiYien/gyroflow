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

static COLLECTOR: Mutex<Option<Vec<DeepMatchSegStats>>> = Mutex::new(None);

/// Arm the collector. Only one deep match runs at a time (enforced by the
/// render queue), so a single global slot is sufficient.
pub fn arm() {
    *COLLECTOR.lock() = Some(Vec::new());
}

pub fn is_armed() -> bool {
    COLLECTOR.lock().is_some()
}

pub fn record(stats: DeepMatchSegStats) {
    if let Some(v) = COLLECTOR.lock().as_mut() {
        v.push(stats);
    }
}

/// Take the collected stats and disarm.
pub fn take() -> Vec<DeepMatchSegStats> {
    COLLECTOR.lock().take().unwrap_or_default()
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

#[derive(Debug, Clone, PartialEq)]
pub enum DeepMatchVerdict {
    /// median of per-window offsets
    Accepted { offset_ms: f64 },
    /// fewer than 2 windows survived the motion gate inside essential
    TooFewWindows,
    /// per-window offsets disagree → video likely not in this gyro file
    Inconsistent { spread_ms: f64 },
    /// valleys too shallow vs plateau → noise match
    WeakValley { worst_ratio: f64 },
}

/// Double acceptance gate (spec §3.3). `offsets_ms` are the per-window
/// offsets returned by the sync run; `stats` come from the collector.
pub fn evaluate(
    offsets_ms: &[f64],
    stats: &[DeepMatchSegStats],
    spread_max_ms: f64,
    cost_ratio_max: f64,
) -> DeepMatchVerdict {
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
    let mut worst_ratio = 0.0f64;
    for s in stats {
        if !s.cost_p25.is_finite() || s.cost_p25 <= 0.0 {
            return DeepMatchVerdict::WeakValley { worst_ratio: 1.0 };
        }
        let ratio = s.cost_min / s.cost_p25;
        worst_ratio = worst_ratio.max(ratio);
    }
    if stats.is_empty() || worst_ratio > cost_ratio_max {
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

fn env_f64(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite() && *v > 0.0)
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
pub fn max_scan_ms() -> f64 {
    env_f64("GYROFLOW_DEEP_MATCH_MAX_SCAN_S", 7200.0) * 1000.0
}
pub fn window_count() -> usize {
    (env_f64("GYROFLOW_DEEP_MATCH_WINDOWS", 3.0) as usize).clamp(2, 8)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        );
        assert_eq!(v, DeepMatchVerdict::Accepted { offset_ms: -204692.0 });
    }

    #[test]
    fn evaluate_rejects_single_window() {
        assert_eq!(
            evaluate(&[-100.0], &[stats(0.1)], 200.0, 0.35),
            DeepMatchVerdict::TooFewWindows
        );
    }

    #[test]
    fn evaluate_rejects_inconsistent_offsets() {
        match evaluate(&[0.0, 5000.0], &[stats(0.1), stats(0.1)], 200.0, 0.35) {
            DeepMatchVerdict::Inconsistent { spread_ms } => assert_eq!(spread_ms, 5000.0),
            v => panic!("expected Inconsistent, got {v:?}"),
        }
    }

    #[test]
    fn evaluate_rejects_shallow_valleys() {
        match evaluate(&[0.0, 1.0], &[stats(0.1), stats(0.9)], 200.0, 0.35) {
            DeepMatchVerdict::WeakValley { worst_ratio } => {
                assert!((worst_ratio - 0.9).abs() < 1e-9)
            }
            v => panic!("expected WeakValley, got {v:?}"),
        }
    }

    #[test]
    fn evaluate_rejects_missing_stats() {
        assert_eq!(
            evaluate(&[0.0, 1.0], &[], 200.0, 0.35),
            DeepMatchVerdict::WeakValley { worst_ratio: 1.0 }
        );
    }

    #[test]
    fn collector_roundtrip() {
        arm();
        assert!(is_armed());
        record(stats(0.1));
        let v = take();
        assert_eq!(v.len(), 1);
        assert!(!is_armed());
        // take() when disarmed yields empty
        assert!(take().is_empty());
    }
}
