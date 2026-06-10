// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

//! Twin-minimum ambiguity guard for the rs-sync fusion layer
//! (sync-parallax-suppression M3).
//!
//! Parallax / foreground contamination can split the rs cost surface into two
//! near-equal shallow valleys ~±10ms apart (geometrically degenerate
//! non-rotational flow pulls one axis's aggregate decision). The existing
//! `periodic_ambiguity` detector cannot see them: its second-peak search uses
//! `min_sep = max(FWHM, 50ms)`, structurally excluding twins within ±25ms.
//! All four fusion candidates share the same aggregated data, so they vote
//! unanimously into one of the twin valleys → cfrac=1.0, conf≈0.95 coin flip.
//!
//! This guard runs after the fusion output is selected: when a non-chosen
//! local minimum sits within `TWIN_RADIUS_MS`, its cost is within
//! `TWIN_MARGIN` of the chosen one, and either valley is shallow
//! (`sharpness < TWIN_SHARP_MAX`), confidence is ceiled to 0.3 and the
//! conf_path becomes `twin_ambiguity`. The offset itself is never changed.
//!
//! Rollback: `GYROFLOW_SYNC_TWIN_GUARD=0` disables detection and ceiling.

use crate::synchronization::sync_metric::CostMinimum;
use std::sync::OnceLock;

/// Cost-curve scan step in ms (must match `presync_step` in rs_sync.rs).
/// Minima closer than 1.5× this to the chosen minimum are treated as the
/// same valley (grid jitter), not a twin.
pub const COST_SCAN_STEP_MS: f64 = 5.0;

#[derive(Debug, Clone, Copy)]
pub struct TwinParams {
    /// Detection + confidence ceiling on/off (`GYROFLOW_SYNC_TWIN_GUARD`, default true).
    pub enabled: bool,
    /// Twin search radius in ms around the chosen minimum (`GYROFLOW_SYNC_TWIN_RADIUS_MS`).
    pub radius_ms: f64,
    /// Cost-ratio closeness: ratio must be within [1−margin, 1+margin] (`GYROFLOW_SYNC_TWIN_MARGIN`).
    pub margin: f64,
    /// Trigger requires min(sharpness(chosen), sharpness(twin)) below this
    /// (`GYROFLOW_SYNC_TWIN_SHARP_MAX`). Calibrated on C50 ground-truth data:
    /// bad-window twin valleys 2.0-2.7, good-window valleys 13.2-18.1.
    pub sharp_max: f64,
    /// Cross-validation resolution distance (`GYROFLOW_SYNC_TWIN_RESOLVE_MS`):
    /// when M2 pass-2 re-solves the segment from independently filtered data
    /// and both passes land within this distance, the twin ambiguity counts
    /// as resolved — confidence is restored instead of ceiled (a genuine coin
    /// flip would land in the other valley, ≥ 1.5×scan-step away). `0`
    /// disables resolution (pure ceiling behavior).
    pub resolve_dist_ms: f64,
}

const DEFAULT_PARAMS: TwinParams = TwinParams {
    enabled: true,
    radius_ms: 25.0,
    margin: 0.05,
    sharp_max: 8.0,
    resolve_dist_ms: 4.0,
};

static RESOLVED: OnceLock<TwinParams> = OnceLock::new();
static LOGGED: OnceLock<()> = OnceLock::new();

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}
fn parse_f64_pos(raw: &str) -> Option<f64> {
    raw.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v > 0.0)
}
fn parse_f64_nonneg(raw: &str) -> Option<f64> {
    raw.trim().parse::<f64>().ok().filter(|v| v.is_finite() && *v >= 0.0)
}

fn resolve_inner() -> (TwinParams, Vec<&'static str>) {
    let mut p = DEFAULT_PARAMS;
    let mut overrides: Vec<&'static str> = Vec::new();
    let mut read = |name: &'static str, apply: &mut dyn FnMut(&str) -> bool| {
        match std::env::var(name) {
            Ok(raw) if !raw.is_empty() => {
                if apply(&raw) {
                    overrides.push(name);
                } else {
                    log::warn!(target: "lifecycle", "{}={} invalid, falling back to default", name, raw);
                }
            }
            _ => {}
        }
    };
    read("GYROFLOW_SYNC_TWIN_GUARD", &mut |raw| {
        parse_bool(raw).map(|v| p.enabled = v).is_some()
    });
    read("GYROFLOW_SYNC_TWIN_RADIUS_MS", &mut |raw| {
        parse_f64_pos(raw).map(|v| p.radius_ms = v).is_some()
    });
    read("GYROFLOW_SYNC_TWIN_MARGIN", &mut |raw| {
        parse_f64_pos(raw).map(|v| p.margin = v).is_some()
    });
    read("GYROFLOW_SYNC_TWIN_SHARP_MAX", &mut |raw| {
        parse_f64_pos(raw).map(|v| p.sharp_max = v).is_some()
    });
    read("GYROFLOW_SYNC_TWIN_RESOLVE_MS", &mut |raw| {
        parse_f64_nonneg(raw).map(|v| p.resolve_dist_ms = v).is_some()
    });
    (p, overrides)
}

/// Resolve twin-guard params from env vars (cached after first call; restart to change).
pub fn params() -> TwinParams {
    *RESOLVED.get_or_init(|| {
        let (p, overrides) = resolve_inner();
        if LOGGED.set(()).is_ok() {
            log::info!(
                target: "lifecycle",
                "twin_guard resolved enabled={} radius_ms={:.0} margin={:.3} sharp_max={:.1} resolve_ms={:.1} source={}",
                p.enabled,
                p.radius_ms,
                p.margin,
                p.sharp_max,
                p.resolve_dist_ms,
                if overrides.is_empty() { "default".to_string() } else { format!("env[{}]", overrides.join(",")) }
            );
        }
        p
    })
}

/// A detected twin minimum (the non-chosen valley of a near-equal pair).
#[derive(Debug, Clone, Copy)]
pub struct TwinInfo {
    /// Twin valley position in ms (external offset convention).
    pub offset_ms: f64,
    /// |chosen_cost / twin_cost − 1| — how close the two valleys are.
    pub margin: f64,
    /// Sharpness of the chosen valley.
    pub sharp_chosen: f64,
    /// Sharpness of the twin valley.
    pub sharp_twin: f64,
}

/// Detect a twin minimum near the chosen one (design D1 of
/// sync-parallax-suppression).
///
/// `chosen_ms` / `chosen_cost` / `chosen_sharp` describe the local minimum
/// the fusion output was associated with (caller resolves the nearest
/// minimum to its output). A candidate `m` is a twin when ALL hold:
///   - distance ∈ (1.5 × [`COST_SCAN_STEP_MS`], `radius_ms`]  (same-valley re-detection excluded)
///   - `chosen_cost / m.cost` ∈ [1−margin, 1+margin]           (near-equal cost)
///   - `min(chosen_sharp, m.sharpness) < sharp_max`            (shallow pair — deep
///     well-separated valleys like the C50 good window must NOT trigger)
///
/// Among multiple matches the one with the smallest cost margin (most
/// ambiguous) is returned.
pub fn detect_twin_minimum(
    minima: &[CostMinimum],
    chosen_ms: f64,
    chosen_cost: f64,
    chosen_sharp: f64,
    params: &TwinParams,
) -> Option<TwinInfo> {
    if !params.enabled
        || !chosen_ms.is_finite()
        || !chosen_cost.is_finite()
        || !chosen_sharp.is_finite()
    {
        return None;
    }
    let exclusion_ms = COST_SCAN_STEP_MS * 1.5;
    let mut best: Option<TwinInfo> = None;
    for m in minima {
        let d = (m.offset_ms - chosen_ms).abs();
        if d <= exclusion_ms || d > params.radius_ms {
            continue;
        }
        if !m.cost.is_finite() || m.cost.abs() < 1e-12 {
            continue;
        }
        let ratio = chosen_cost / m.cost;
        let margin = (ratio - 1.0).abs();
        if margin > params.margin {
            continue;
        }
        if chosen_sharp.min(m.sharpness) >= params.sharp_max {
            continue;
        }
        if best.is_none_or(|b| margin < b.margin) {
            best = Some(TwinInfo {
                offset_ms: m.offset_ms,
                margin,
                sharp_chosen: chosen_sharp,
                sharp_twin: m.sharpness,
            });
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn min_at(offset_ms: f64, cost: f64, sharpness: f64) -> CostMinimum {
        CostMinimum {
            offset_ms,
            cost,
            depth: 10.0,
            width_ms: 15.0,
            sharpness,
        }
    }

    fn test_params() -> TwinParams {
        DEFAULT_PARAMS
    }

    #[test]
    fn c50_bad_window_triggers() {
        // Measured numbers from sync_diag session 1781056962 (burn, 8.1-11.0s):
        // chosen +9.6ms (cost 417, sharp 2.74), twin −10.4ms (cost 426, sharp 2.32).
        let minima = vec![min_at(9.6, 417.0, 2.74), min_at(-10.4, 426.0, 2.32)];
        let t = detect_twin_minimum(&minima, 9.6, 417.0, 2.74, &test_params())
            .expect("C50 bad window must trigger");
        assert!((t.offset_ms - -10.4).abs() < 1e-9);
        // cost ratio 417/426 → margin ≈ 0.0211
        assert!((t.margin - (1.0 - 417.0 / 426.0)).abs() < 1e-9);
        assert!(t.margin <= 0.05);
        assert!((t.sharp_twin - 2.32).abs() < 1e-9);
    }

    #[test]
    fn good_window_deep_valleys_do_not_trigger() {
        // Good window: chosen −0.4ms (cost 297, sharp 18.1), next +9.6ms
        // (cost 304.7, sharp 13.2). Cost ratio is within margin (2.5%) but
        // both valleys are deep/sharp → min(sharp)=13.2 ≥ 8.0 → no trigger.
        let minima = vec![min_at(-0.4, 297.0, 18.1), min_at(9.6, 304.7, 13.2)];
        assert!(detect_twin_minimum(&minima, -0.4, 297.0, 18.1, &test_params()).is_none());
    }

    #[test]
    fn far_minimum_outside_radius_does_not_trigger() {
        // 40ms away > TWIN_RADIUS_MS(25) — periodic_ambiguity's jurisdiction.
        let minima = vec![min_at(0.0, 400.0, 2.5), min_at(40.0, 402.0, 2.5)];
        assert!(detect_twin_minimum(&minima, 0.0, 400.0, 2.5, &test_params()).is_none());
    }

    #[test]
    fn same_valley_grid_jitter_excluded() {
        // 5ms away ≤ 1.5×scan step (7.5ms) — same valley, not a twin.
        let minima = vec![min_at(0.0, 400.0, 2.5), min_at(5.0, 401.0, 2.5)];
        assert!(detect_twin_minimum(&minima, 0.0, 400.0, 2.5, &test_params()).is_none());
    }

    #[test]
    fn cost_margin_gate() {
        // 10% cost difference > TWIN_MARGIN(5%) → no trigger.
        let minima = vec![min_at(0.0, 400.0, 2.5), min_at(15.0, 440.0, 2.5)];
        assert!(detect_twin_minimum(&minima, 0.0, 400.0, 2.5, &test_params()).is_none());
    }

    #[test]
    fn disabled_params_short_circuit() {
        let mut p = test_params();
        p.enabled = false;
        let minima = vec![min_at(9.6, 417.0, 2.74), min_at(-10.4, 426.0, 2.32)];
        assert!(detect_twin_minimum(&minima, 9.6, 417.0, 2.74, &p).is_none());
    }

    #[test]
    fn closest_cost_margin_wins_among_multiple() {
        let minima = vec![
            min_at(0.0, 400.0, 2.5),
            min_at(12.0, 412.0, 2.5), // margin ≈ 0.029
            min_at(-10.0, 404.0, 2.5), // margin ≈ 0.0099 → most ambiguous
        ];
        let t = detect_twin_minimum(&minima, 0.0, 400.0, 2.5, &test_params()).unwrap();
        assert!((t.offset_ms - -10.0).abs() < 1e-9);
    }

    #[test]
    fn env_parsers_reject_invalid() {
        assert_eq!(parse_bool("on"), Some(true));
        assert_eq!(parse_bool("0"), Some(false));
        assert_eq!(parse_bool("abc"), None);
        assert_eq!(parse_f64_pos("25"), Some(25.0));
        assert_eq!(parse_f64_pos("-1"), None);
        assert_eq!(parse_f64_pos("NaN"), None);
    }
}
