// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

//! Cost curve metric helpers — sharpness, depth, width of local minima.
//!
//! Algorithm originally lived in `sync_diag.rs::analyze_curve_and_record` and
//! was used only for diagnostic CSV dumps. Lifted here as a first-class
//! decision tool so `rs_sync.rs::full_sync` grid_fallback and
//! `ncc_fusion_decide` anchor pool can score local minima by sharpness
//! (`depth / width`) instead of raw cost value.
//!
//! Spec: `openspec/changes/sync-fusion-sharpness-and-cross-prior/specs/sync-offset-estimation/spec.md`.

/// A local minimum on a cost curve with its quality metrics.
///
/// `depth = baseline_p75 - cost` (resistant to deep minima pulling the
/// reference down).
/// `width_ms` is the span where cost is below `cost_min × (1 + width_tolerance)`.
/// `sharpness = depth / width_ms` — narrow + deep wins over wide + deep.
#[derive(Debug, Clone, Copy)]
pub struct CostMinimum {
    pub offset_ms: f64,
    pub cost: f64,
    pub depth: f64,
    pub width_ms: f64,
    pub sharpness: f64,
}

/// Find every local minimum on a cost curve and score it.
///
/// `curve` is `(offset_ms, cost)` pairs in any order; the function sorts
/// internally by `offset_ms`. NaN / non-finite cost samples are dropped.
///
/// Returns `(baseline_p75, minima)`. `baseline_p75` is the 75th percentile
/// of finite costs (used as the reference level for `depth`). Returns
/// `(0.0, vec![])` when there are fewer than 3 usable samples.
pub fn find_local_minima(curve: &[(f64, f64)], width_tolerance: f64) -> (f64, Vec<CostMinimum>) {
    let mut sorted: Vec<(f64, f64)> = curve
        .iter()
        .filter(|(_, c)| !c.is_nan() && c.is_finite())
        .copied()
        .collect();
    if sorted.len() < 3 {
        return (0.0, Vec::new());
    }
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // baseline = P75 of cost
    let mut costs: Vec<f64> = sorted.iter().map(|p| p.1).collect();
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let baseline_p75 = costs[(costs.len() as f64 * 0.75) as usize];

    // Step size estimate: median of adjacent-offset gaps. Used as floor for
    // very narrow minima width so single-bin spikes still get a finite width.
    let mut step_samples: Vec<f64> =
        sorted.windows(2).map(|w| (w[1].0 - w[0].0).abs()).collect();
    step_samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let step_ms = if step_samples.is_empty() {
        5.0
    } else {
        step_samples[step_samples.len() / 2].max(0.001)
    };

    let n = sorted.len();
    let mut minima: Vec<CostMinimum> = Vec::new();
    for i in 1..(n - 1) {
        if sorted[i].1 < sorted[i - 1].1 && sorted[i].1 < sorted[i + 1].1 {
            let cost_i = sorted[i].1;
            let threshold = cost_i * (1.0 + width_tolerance);
            // Expand left while neighbor cost is still inside the valley.
            let mut l = i;
            while l > 0 && sorted[l - 1].1 < threshold {
                l -= 1;
            }
            // Expand right.
            let mut r = i;
            while r + 1 < n && sorted[r + 1].1 < threshold {
                r += 1;
            }
            let width_ms = (sorted[r].0 - sorted[l].0).abs().max(step_ms);
            let depth = (baseline_p75 - cost_i).max(0.0);
            let sharpness = depth / width_ms;
            minima.push(CostMinimum {
                offset_ms: sorted[i].0,
                cost: cost_i,
                depth,
                width_ms,
                sharpness,
            });
        }
    }

    (baseline_p75, minima)
}

/// Find the local minimum nearest to `target_ms` within `radius_ms`.
///
/// Returns `None` if no local minimum is inside the search radius. When
/// multiple minima qualify, returns the one closest to `target_ms`.
pub fn nearest_minimum(
    curve: &[(f64, f64)],
    target_ms: f64,
    radius_ms: f64,
) -> Option<CostMinimum> {
    let (_, minima) = find_local_minima(curve, 0.05);
    minima
        .into_iter()
        .filter(|m| (m.offset_ms - target_ms).abs() <= radius_ms)
        .min_by(|a, b| {
            (a.offset_ms - target_ms)
                .abs()
                .partial_cmp(&(b.offset_ms - target_ms).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Score how well `cand_ms` lines up with a real cost-valley using a
/// pre-computed minima list. Companion to [`sharpness_factor_at`] for
/// callers that already obtained `(baseline, minima)` from
/// [`find_local_minima`] and want to evaluate several candidates without
/// re-scanning the curve each time.
///
/// `max_sharp_ref` is normally the maximum `sharpness` over `minima` for
/// the current curve; passing a per-segment reference keeps the factor
/// comparable across baselines.
pub fn factor_from_minima(
    minima: &[CostMinimum],
    cand_ms: f64,
    radius_ms: f64,
    max_sharp_ref: f64,
    floor: f64,
) -> f64 {
    if !cand_ms.is_finite() || max_sharp_ref < 1e-9 {
        return floor;
    }
    let mut best: Option<&CostMinimum> = None;
    let mut best_dist = f64::INFINITY;
    for m in minima {
        let d = (m.offset_ms - cand_ms).abs();
        if d <= radius_ms && d < best_dist {
            best_dist = d;
            best = Some(m);
        }
    }
    match best {
        Some(m) => (m.sharpness / max_sharp_ref).clamp(0.0, 1.0),
        None => floor,
    }
}

/// Score how "real" `cand_ms` is relative to the cost curve's narrowest
/// minimum. Returns `(nearest_minimum.sharpness / max_sharp_ref).clamp(0,1)`
/// when a local minimum exists within `radius_ms` of `cand_ms`; otherwise
/// returns `floor`. Returns `floor` on non-finite `cand_ms` or
/// `max_sharp_ref < 1e-9` (avoids divide-by-zero on flat curves).
///
/// This is the high-level convenience entry point. Callers that score many
/// candidates against the same curve should use [`find_local_minima`] +
/// [`factor_from_minima`] to avoid re-running the minima scan.
pub fn sharpness_factor_at(
    curve: &[(f64, f64)],
    cand_ms: f64,
    radius_ms: f64,
    max_sharp_ref: f64,
    floor: f64,
) -> f64 {
    if !cand_ms.is_finite() || max_sharp_ref < 1e-9 {
        return floor;
    }
    let (_, minima) = find_local_minima(curve, 0.05);
    factor_from_minima(&minima, cand_ms, radius_ms, max_sharp_ref, floor)
}

/// Pick the sharpest minimum within `|offset - center_ms| < max_dist_ms`
/// that passes the depth gate `depth >= baseline_p75 × depth_gate_ratio`.
///
/// Returns `None` if no minimum is in range or none pass the depth gate.
/// Callers should fall back to legacy cost-min behavior on `None`.
pub fn sharpest_minimum_in_bound(
    curve: &[(f64, f64)],
    center_ms: f64,
    max_dist_ms: f64,
    depth_gate_ratio: f64,
) -> Option<CostMinimum> {
    let (baseline_p75, minima) = find_local_minima(curve, 0.05);
    if minima.is_empty() {
        return None;
    }
    let depth_floor = baseline_p75 * depth_gate_ratio;
    minima
        .into_iter()
        .filter(|m| (m.offset_ms - center_ms).abs() < max_dist_ms)
        .filter(|m| m.depth >= depth_floor)
        .max_by(|a, b| {
            a.sharpness
                .partial_cmp(&b.sharpness)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synthesize_curve_narrow_and_wide() -> Vec<(f64, f64)> {
        // Build a synthetic cost curve mirroring the C50 case:
        // - global cost-min at +2889 (wide basin, depth ~54, width ~1090ms)
        // - sharper local min at -950 (narrow valley, depth ~23, width ~30ms)
        // Flat baseline (no spurious extra minima from oscillations).
        let mut pts: Vec<(f64, f64)> = Vec::new();
        let baseline = 230.0_f64;

        // Sample uniformly at 5ms step across [-5000, 5000].
        for k in -1000..=1000_i32 {
            let off = k as f64 * 5.0;
            let mut cost = baseline;

            // Narrow valley at -950: width ~30ms, depth ~23.
            let d_narrow = (off - (-950.0)).abs();
            if d_narrow < 30.0 {
                cost = baseline - 23.0 * (1.0 - d_narrow / 30.0);
            }

            // Wide basin at +2889: width ~1090ms, depth ~54.
            let d_wide = (off - 2889.0).abs();
            if d_wide < 545.0 {
                let parabolic = 1.0 - (d_wide / 545.0).powi(2);
                let wide_cost = baseline - 54.0 * parabolic;
                if wide_cost < cost {
                    cost = wide_cost;
                }
            }

            pts.push((off, cost));
        }
        pts
    }

    #[test]
    fn finds_local_minima_and_baseline_p75() {
        let curve = synthesize_curve_narrow_and_wide();
        let (baseline, minima) = find_local_minima(&curve, 0.05);
        assert!(baseline > 220.0 && baseline < 240.0, "baseline_p75={}", baseline);
        assert!(!minima.is_empty(), "expected at least one local minimum");

        // Narrow minimum near -950 should exist.
        let near_narrow = minima
            .iter()
            .find(|m| (m.offset_ms - (-950.0)).abs() < 20.0);
        assert!(near_narrow.is_some(), "narrow minimum at -950 not found");
        let narrow = near_narrow.unwrap();
        assert!(narrow.depth > 18.0, "narrow depth too small: {}", narrow.depth);

        // Wide basin near +2889 should also exist.
        let near_wide = minima
            .iter()
            .find(|m| (m.offset_ms - 2889.0).abs() < 50.0);
        assert!(near_wide.is_some(), "wide basin at +2889 not found");
        let wide = near_wide.unwrap();
        // Wide basin width is much larger than narrow valley width.
        assert!(wide.width_ms > narrow.width_ms * 5.0,
                "wide width {} not >> narrow width {}", wide.width_ms, narrow.width_ms);
    }

    #[test]
    fn sharpest_in_bound_picks_narrow_over_wide() {
        let curve = synthesize_curve_narrow_and_wide();
        // Bound covers both minima; depth gate 0.05 (5% of baseline_p75 ≈ 230 → 11.5)
        // lets the narrow (depth ≈ 23) pass.
        let pick = sharpest_minimum_in_bound(&curve, 0.0, 5000.0, 0.05);
        assert!(pick.is_some(), "expected a winner");
        let m = pick.unwrap();
        assert!((m.offset_ms - (-950.0)).abs() < 20.0,
                "expected narrow at -950, got {}", m.offset_ms);
        assert!(m.sharpness > 0.5, "narrow sharpness should be > 0.5, got {}", m.sharpness);
    }

    #[test]
    fn nearest_minimum_respects_radius() {
        let curve = synthesize_curve_narrow_and_wide();
        // Search around -950 with 50ms radius → narrow minimum should be picked.
        let m = nearest_minimum(&curve, -950.0, 50.0);
        assert!(m.is_some());
        assert!((m.unwrap().offset_ms - (-950.0)).abs() < 20.0);

        // Search around 0 with tiny radius → no minimum in range.
        let none = nearest_minimum(&curve, 0.0, 10.0);
        assert!(none.is_none());
    }

    #[test]
    fn depth_gate_blocks_shallow_spikes() {
        // Build a flat-ish curve with a tiny noise spike (depth < 10% baseline).
        let mut curve: Vec<(f64, f64)> = (0..400)
            .map(|k| {
                let off = k as f64 * 5.0;
                (off, 100.0)
            })
            .collect();
        // Place a single-sample dip at index 200 (offset 1000).
        curve[200] = (1000.0, 95.0); // depth = 100 - 95 = 5, only 5% of baseline.

        let pick = sharpest_minimum_in_bound(&curve, 0.0, 5000.0, 0.30);
        assert!(pick.is_none(), "5%-deep spike must fail 30% depth gate");

        // Lowering gate to 0.04 lets it through.
        let pick_low = sharpest_minimum_in_bound(&curve, 0.0, 5000.0, 0.04);
        assert!(pick_low.is_some());
    }

    #[test]
    fn sharpness_factor_at_picks_narrow() {
        let curve = synthesize_curve_narrow_and_wide();
        let (_, minima) = find_local_minima(&curve, 0.05);
        let max_sharp_ref = minima
            .iter()
            .map(|m| m.sharpness)
            .fold(0.0_f64, f64::max);
        assert!(max_sharp_ref > 0.0, "synthetic curve must have a sharpness");

        // Candidate sitting in the narrow valley at -950ms gets full credit.
        let f = sharpness_factor_at(&curve, -950.0, 20.0, max_sharp_ref, 0.0);
        assert!(f > 0.95, "narrow-valley factor should be ≈1.0, got {}", f);
    }

    #[test]
    fn sharpness_factor_at_punishes_wide() {
        let curve = synthesize_curve_narrow_and_wide();
        let (_, minima) = find_local_minima(&curve, 0.05);
        let max_sharp_ref = minima
            .iter()
            .map(|m| m.sharpness)
            .fold(0.0_f64, f64::max);
        let f = sharpness_factor_at(&curve, 2889.0, 20.0, max_sharp_ref, 0.0);
        // Wide basin (parabolic 545ms half-width) is at least ~5x less sharp
        // than the synthesized narrow valley; require a clear separation rather
        // than nailing the exact numeric ratio (which depends on grid step,
        // baseline-P75 placement, and parabolic-vs-triangular profile).
        assert!(
            f < 0.2,
            "wide-basin factor should be much smaller than the narrow one, got {}",
            f
        );
        let f_narrow = sharpness_factor_at(&curve, -950.0, 20.0, max_sharp_ref, 0.0);
        assert!(
            f_narrow > 5.0 * f,
            "narrow factor {} should dominate wide factor {} by ≥5x",
            f_narrow,
            f
        );
    }

    #[test]
    fn sharpness_factor_at_uses_floor() {
        let curve = synthesize_curve_narrow_and_wide();
        let (_, minima) = find_local_minima(&curve, 0.05);
        let max_sharp_ref = minima
            .iter()
            .map(|m| m.sharpness)
            .fold(0.0_f64, f64::max);
        // Sit far from any minimum (radius 5ms around 0 is empty for this curve).
        let f = sharpness_factor_at(&curve, 0.0, 5.0, max_sharp_ref, 0.25);
        assert_eq!(f, 0.25, "out-of-range candidate must return the supplied floor");

        // Non-finite candidate also returns floor.
        let f_nan = sharpness_factor_at(&curve, f64::NAN, 20.0, max_sharp_ref, 0.7);
        assert_eq!(f_nan, 0.7);
    }

    #[test]
    fn sharpness_factor_at_zero_max_returns_floor() {
        let curve = synthesize_curve_narrow_and_wide();
        // max_sharp_ref = 0 → divide-by-zero guard returns floor regardless of position.
        let f = sharpness_factor_at(&curve, -950.0, 20.0, 0.0, 0.4);
        assert_eq!(f, 0.4);

        // Same expectation for the low-level helper.
        let (_, minima) = find_local_minima(&curve, 0.05);
        let f2 = factor_from_minima(&minima, -950.0, 20.0, 1e-12, 0.42);
        assert_eq!(f2, 0.42);
    }

    #[test]
    fn empty_or_tiny_curve_returns_empty() {
        let (b, m) = find_local_minima(&[], 0.05);
        assert_eq!(b, 0.0);
        assert!(m.is_empty());

        let (b, m) = find_local_minima(&[(0.0, 1.0), (1.0, 2.0)], 0.05);
        assert_eq!(b, 0.0);
        assert!(m.is_empty());

        let none = nearest_minimum(&[], 0.0, 100.0);
        assert!(none.is_none());
    }
}
