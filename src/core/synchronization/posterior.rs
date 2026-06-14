// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Generative offset-posterior building blocks (change
//! `sync-likelihood-nuisance`, Phase 0 — design D1-D5).
//!
//! Pure functions only: the per-point residuals come from rs-sync's new
//! `eval_residuals` / `solve_gain` API (or, for historical diag sessions,
//! from a curve-based approximation), and this module turns them into a
//! posterior over the shared 5ms offset grid:
//!
//! - robust noise scale σ = 1.4826 × MAD of the residuals at the grid-best δ*
//! - Tukey biweight pseudo-likelihood on standardized residuals u = r/σ
//!   (cutoff |r| ≥ 4.685σ), same robust family as rs-sync's existing kernel
//!   but dimensionless — the evidence scale (nats) must not depend on the
//!   residual unit
//! - effective-degrees-of-freedom curvature scaling: `n_eff = #frame pairs`,
//!   not #points (points within a frame pair are correlated; scaling by point
//!   count would make any 1% cost difference look like overwhelming evidence)
//! - cross-window product: log-likelihoods of different windows (independent
//!   data) are added on the shared grid
//! - prior (design D4): anchor / stored / uniform — added as a log term only,
//!   never narrowing the search domain
//! - `conf_posterior`: posterior mass within ±12.5ms of the argmax
//!
//! NOT wired into the product decision path — tasks §3 (the
//! `GYROFLOW_SYNC_POSTERIOR` switch) is gated on the §2.7 acceptance gates.

/// Half-width of the confidence integration window (design D5).
pub const CONF_WINDOW_MS: f64 = 12.5;
/// Tukey biweight tuning constant (in units of σ) — 95% Gaussian efficiency.
pub const TUKEY_C: f64 = 4.685;
/// Anchor-prior width: batch/deep-match anchors have COMP_TIME ±1.5s
/// granularity (design D4) — a real calibration, not a heuristic.
pub const ANCHOR_PRIOR_SIGMA_MS: f64 = 1500.0;

/// Robust noise scale: `1.4826 × median(|r - median(r)|)`.
/// Non-finite residuals are ignored; returns a small floor (never 0/NaN) so
/// downstream divisions stay finite. Per design D2 this is evaluated once per
/// window at the grid-best δ*.
pub fn sigma_mad(residuals: &[f64]) -> f64 {
    const SIGMA_FLOOR: f64 = 1e-9;
    let mut v: Vec<f64> = residuals.iter().copied().filter(|r| r.is_finite()).collect();
    if v.is_empty() {
        return SIGMA_FLOOR;
    }
    let med = median_in_place(&mut v);
    let mut dev: Vec<f64> = v.iter().map(|r| (r - med).abs()).collect();
    let mad = median_in_place(&mut dev);
    (1.4826 * mad).max(SIGMA_FLOOR)
}

fn median_in_place(v: &mut [f64]) -> f64 {
    let k = v.len() / 2;
    v.select_nth_unstable_by(k, |a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    if v.len() % 2 == 1 {
        v[k]
    } else {
        // Lower half's max + v[k], averaged — exact median for even length.
        let lo = v[..k].iter().copied().fold(f64::NEG_INFINITY, f64::max);
        0.5 * (lo + v[k])
    }
}

/// Tukey biweight loss ρ on the STANDARDIZED residual `u = r/σ`, with the
/// usual cutoff at `|r| ≥ TUKEY_C·σ`. Bounded: ρ ∈ [0, TUKEY_C²/6 ≈ 3.66];
/// residuals beyond the cutoff contribute that constant (the ε-outlier
/// mixture equivalent of design D2 — no EM needed). Small residuals give
/// ρ ≈ u²/2, i.e. the Gaussian-core −logL up to an additive constant.
///
/// The loss is dimensionless and scale-invariant — `ρ(k·r, k·σ) = ρ(r, σ)`
/// — so the likelihood's nat scale never depends on the residual unit.
/// (An earlier draft evaluated ρ in raw residual units, multiplying every
/// logL by σ²; with rs-sync's normalized residuals σ ≈ 0.1 that collapsed a
/// >80-nat truth-vs-plateau separation to ~1 nat and flattened the full-mode
/// posterior to conf ≈ 1/n_grid — the 2026-06-12 P1004620 replay finding.)
pub fn tukey_rho(r: f64, sigma: f64) -> f64 {
    const BOUND: f64 = TUKEY_C * TUKEY_C / 6.0;
    if !r.is_finite() {
        return BOUND;
    }
    let u = r / (TUKEY_C * sigma.max(1e-12));
    if u.abs() >= 1.0 {
        BOUND
    } else {
        let t = 1.0 - u * u;
        BOUND * (1.0 - t * t * t)
    }
}

/// Per-window robust log-likelihood (up to an additive δ-independent
/// constant) at one grid offset, with curvature scaled by the effective
/// degrees of freedom: `logL = -(n_eff / n_points) · Σ_i ρ(r_i; σ)` where
/// `n_eff = groups.len()` (frame pairs). Within-pair residuals are correlated,
/// so confidence must not grow with raw point count (design D2).
///
/// Returns 0.0 for an empty window (no evidence — flat likelihood).
pub fn window_log_likelihood(groups: &[Vec<f64>], sigma: f64) -> f64 {
    let n_points: usize = groups.iter().map(|g| g.len()).sum();
    if n_points == 0 {
        return 0.0;
    }
    let n_eff = groups.len() as f64;
    let rho_sum: f64 = groups.iter().flatten().map(|&r| tukey_rho(r, sigma)).sum();
    -(n_eff / n_points as f64) * rho_sum
}

/// Curve-approximation log-likelihood for historical diag sessions that only
/// recorded the aggregated cost curve (no residuals.csv): Gaussian profile
/// likelihood with σ profiled out exactly,
/// `logL(δ) = -(n_eff/2) · ln(cost(δ) / cost_min)`.
/// Returns NaN when inputs are unusable (caller filters).
pub fn approx_window_log_likelihood(cost: f64, cost_min: f64, n_eff: f64) -> f64 {
    if !cost.is_finite() || !cost_min.is_finite() || cost <= 0.0 || cost_min <= 0.0 {
        return f64::NAN;
    }
    -(n_eff / 2.0) * (cost / cost_min).ln()
}

/// Cross-window aggregation: different windows are independent data, so their
/// log-likelihoods add elementwise on the shared grid (design D3).
/// Returns `None` when window lengths disagree or the input is empty.
pub fn combine_windows(per_window: &[Vec<f64>]) -> Option<Vec<f64>> {
    let first = per_window.first()?;
    let n = first.len();
    if n == 0 || per_window.iter().any(|w| w.len() != n) {
        return None;
    }
    let mut out = vec![0.0f64; n];
    for w in per_window {
        for (acc, v) in out.iter_mut().zip(w.iter()) {
            *acc += *v;
        }
    }
    Some(out)
}

/// Morphological dilation (sliding-window max) of a logL curve by `half_pts`
/// grid points on each side. Represents bounded drift uncertainty: window w's
/// true reference-offset could sit anywhere within ±half_pts of its measured
/// peak, so the drift-marginalized log-likelihood at each grid point is the
/// max over the band (max logL = max likelihood). `half_pts = 0` is identity.
pub fn dilate_logl(logl: &[f64], half_pts: usize) -> Vec<f64> {
    if half_pts == 0 {
        return logl.to_vec();
    }
    let n = logl.len();
    (0..n)
        .map(|i| {
            let lo = i.saturating_sub(half_pts);
            let hi = (i + half_pts + 1).min(n);
            logl[lo..hi]
                .iter()
                .copied()
                .filter(|v| v.is_finite())
                .fold(f64::NEG_INFINITY, f64::max)
        })
        .collect()
}

/// Linearly resample a log-likelihood (or log-posterior) curve onto a
/// uniform `step_ms` grid spanning the input domain. Non-finite entries are
/// dropped, the input may be unsorted and non-uniform (e.g. the residual
/// dump's ~50ms sampled lattice plus the off-lattice refined δ*), duplicate
/// offsets collapse to their max logL.
///
/// Rationale (conf-integration convention, design D5): `posterior_decide`
/// treats every grid point as one unit of probability mass, so the
/// ±`CONF_WINDOW_MS` integral is only calibrated on the shared 5ms lattice.
/// On a 50ms-sampled grid the window covers a single point that actually
/// represents 50ms of mass — conf comes out ~an order of magnitude off
/// versus approx mode (2026-06-12 P1004620 replay finding). Resampling to
/// the 5ms lattice first aligns both modes on one convention.
///
/// Returns `None` for fewer than 2 usable points or a non-positive step.
pub fn resample_logl_to_uniform_grid(grid_ms: &[f64], logl: &[f64], step_ms: f64) -> Option<(Vec<f64>, Vec<f64>)> {
    if grid_ms.len() != logl.len() || !(step_ms > 0.0) {
        return None;
    }
    let mut pts: Vec<(f64, f64)> = grid_ms
        .iter()
        .zip(logl.iter())
        .filter(|(g, l)| g.is_finite() && l.is_finite())
        .map(|(g, l)| (*g, *l))
        .collect();
    if pts.len() < 2 {
        return None;
    }
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup_by(|a, b| {
        if (a.0 - b.0).abs() < 1e-9 {
            b.1 = b.1.max(a.1);
            true
        } else {
            false
        }
    });
    if pts.len() < 2 {
        return None;
    }
    let lo = pts[0].0;
    let hi = pts[pts.len() - 1].0;
    let n = ((hi - lo) / step_ms).floor() as usize + 1;
    let mut out_g = Vec::with_capacity(n);
    let mut out_l = Vec::with_capacity(n);
    let mut seg = 0usize;
    for i in 0..n {
        let x = lo + i as f64 * step_ms;
        while seg + 2 < pts.len() && pts[seg + 1].0 < x {
            seg += 1;
        }
        let (x0, y0) = pts[seg];
        let (x1, y1) = pts[seg + 1];
        let y = if x1 > x0 {
            let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            y0 + t * (y1 - y0)
        } else {
            y0.max(y1)
        };
        out_g.push(x);
        out_l.push(y);
    }
    Some((out_g, out_l))
}

/// Clean one (grid_ms, logL) curve for interpolation: drop non-finite pairs,
/// sort ascending, collapse duplicate offsets to their max logL — the same
/// convention as `resample_logl_to_uniform_grid`. Returns `None` for fewer
/// than 2 usable points or a length mismatch.
fn clean_curve(grid_ms: &[f64], logl: &[f64]) -> Option<Vec<(f64, f64)>> {
    if grid_ms.len() != logl.len() {
        return None;
    }
    let mut pts: Vec<(f64, f64)> = grid_ms
        .iter()
        .zip(logl.iter())
        .filter(|(g, l)| g.is_finite() && l.is_finite())
        .map(|(g, l)| (*g, *l))
        .collect();
    if pts.len() < 2 {
        return None;
    }
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    pts.dedup_by(|a, b| {
        if (a.0 - b.0).abs() < 1e-9 {
            b.1 = b.1.max(a.1);
            true
        } else {
            false
        }
    });
    if pts.len() < 2 {
        return None;
    }
    Some(pts)
}

/// Linear interpolation of a cleaned (sorted, deduped) curve at ascending
/// query points; queries are clamped to the curve's span.
fn interp_ascending(pts: &[(f64, f64)], xs: &[f64]) -> Vec<f64> {
    let mut seg = 0usize;
    xs.iter()
        .map(|&x| {
            while seg + 2 < pts.len() && pts[seg + 1].0 < x {
                seg += 1;
            }
            let (x0, y0) = pts[seg];
            let (x1, y1) = pts[seg + 1];
            if x1 > x0 {
                let t = ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                y0 + t * (y1 - y0)
            } else {
                y0.max(y1)
            }
        })
        .collect()
}

/// Resample every window curve to the shared 5ms-snapped lattice over the
/// intersection of their spans. Returns (common_grid, per_window_resampled).
/// Shared by `combine_windows_on_common_grid` and the dilated variant.
fn resample_windows_to_common_grid(
    windows: &[(&[f64], &[f64])],
    step_ms: f64,
) -> Option<(Vec<f64>, Vec<Vec<f64>>)> {
    if windows.is_empty() || !(step_ms > 0.0) || !step_ms.is_finite() {
        return None;
    }
    let cleaned: Vec<Vec<(f64, f64)>> = windows
        .iter()
        .map(|(g, l)| clean_curve(g, l))
        .collect::<Option<Vec<_>>>()?;
    let lo = cleaned.iter().map(|p| p[0].0).fold(f64::NEG_INFINITY, f64::max);
    let hi = cleaned.iter().map(|p| p[p.len() - 1].0).fold(f64::INFINITY, f64::min);
    let k0 = (lo / step_ms - 1e-9).ceil() as i64;
    let k1 = (hi / step_ms + 1e-9).floor() as i64;
    if k1 < k0 + 1 {
        return None;
    }
    let grid: Vec<f64> = (k0..=k1).map(|k| k as f64 * step_ms).collect();
    let per_window: Vec<Vec<f64>> = cleaned.iter().map(|pts| interp_ascending(pts, &grid)).collect();
    Some((grid, per_window))
}

/// Cross-window joint likelihood on one shared ALIGNED lattice (design D3 —
/// the `sync_replay --join` entry point). Each window is an independent
/// prior-free (grid_ms, logL) curve, possibly non-uniform / unsorted / with
/// non-finite entries (filtered like `resample_logl_to_uniform_grid`). All
/// windows are linearly resampled onto the lattice of integer multiples of
/// `step_ms` restricted to the INTERSECTION of their spans, then added
/// elementwise via `combine_windows`. Snapping to absolute multiples (rather
/// than starting each lattice at a window's own lo) keeps the windows
/// phase-aligned so they contribute mass at identical offsets.
///
/// The prior is NOT applied here — the caller adds it exactly once at
/// decision time (`posterior_decide`), which is what makes the joint product
/// "prior counted once" by construction.
///
/// Returns `None` when the input is empty, the step is not positive-finite,
/// any window has fewer than 2 usable points, or the common span contains
/// fewer than 2 lattice points (no usable overlap).
pub fn combine_windows_on_common_grid(windows: &[(&[f64], &[f64])], step_ms: f64) -> Option<(Vec<f64>, Vec<f64>)> {
    let (grid, per_window) = resample_windows_to_common_grid(windows, step_ms)?;
    let joint = combine_windows(&per_window)?;
    Some((grid, joint))
}

/// Cross-window joint with bounded drift tolerance (deep-match, design §3.8).
/// Same as `combine_windows_on_common_grid`, but each resampled window is
/// max-dilated by `round(dilate_half_ms / step_ms)` points before summing, so
/// windows whose peaks disagree by up to `2 * dilate_half_ms` (= the clip's
/// allowed drift range T(D)) still produce a joint peak. `dilate_half_ms = 0`
/// is byte-identical to `combine_windows_on_common_grid`.
pub fn combine_windows_on_common_grid_dilated(
    windows: &[(&[f64], &[f64])],
    step_ms: f64,
    dilate_half_ms: f64,
) -> Option<(Vec<f64>, Vec<f64>)> {
    let (grid, per_window) = resample_windows_to_common_grid(windows, step_ms)?;
    let half_pts = if dilate_half_ms > 0.0 {
        (dilate_half_ms / step_ms).round() as usize
    } else {
        0
    };
    let dilated: Vec<Vec<f64>> = per_window.iter().map(|w| dilate_logl(w, half_pts)).collect();
    let joint = combine_windows(&dilated)?;
    Some((grid, joint))
}

/// Prior over the offset (design D4). Only ever added as a log term — it
/// MUST NOT narrow the search domain.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Prior {
    /// Batch-match / deep-match anchor: Gaussian(init, σ = 1500ms).
    Anchor { init_ms: f64 },
    /// .gyroflow project value / user-typed initial offset: weakly
    /// informative Gaussian(init, σ = search_size/2).
    Stored { init_ms: f64, search_size_ms: f64 },
    /// No initial offset: uniform over the search domain.
    Uniform,
}

impl Prior {
    /// Log prior density up to an additive constant (normalization is
    /// irrelevant for the posterior softmax).
    pub fn log_prior(&self, delta_ms: f64) -> f64 {
        match *self {
            Prior::Anchor { init_ms } => {
                let s = ANCHOR_PRIOR_SIGMA_MS;
                -((delta_ms - init_ms) * (delta_ms - init_ms)) / (2.0 * s * s)
            }
            Prior::Stored { init_ms, search_size_ms } => {
                let s = (search_size_ms / 2.0).max(1e-9);
                -((delta_ms - init_ms) * (delta_ms - init_ms)) / (2.0 * s * s)
            }
            Prior::Uniform => 0.0,
        }
    }
}

/// Posterior decision summary over one grid.
#[derive(Debug, Clone, Copy)]
pub struct Posterior {
    /// Grid argmax of the log posterior (ms, external offset convention).
    pub argmax_ms: f64,
    /// Posterior mass within ±`CONF_WINDOW_MS` of the argmax — natural [0,1].
    pub conf_posterior: f64,
    /// Central 95% credible interval (grid-resolution, (lo_ms, hi_ms)).
    pub ci95: (f64, f64),
}

/// Normalize `log_likelihood + log_prior` over the grid and integrate the
/// decision quantities. Grid points with non-finite log-likelihood are
/// dropped (not treated as zero-probability walls). Returns `None` when no
/// usable grid point remains.
///
/// The prior is added pointwise — the grid (search domain) itself is never
/// modified or truncated.
pub fn posterior_decide(grid_ms: &[f64], log_likelihood: &[f64], prior: &Prior) -> Option<Posterior> {
    if grid_ms.len() != log_likelihood.len() || grid_ms.is_empty() {
        return None;
    }
    // Sort by offset so argmax tie-breaking and the CI scan are
    // deterministic regardless of input order.
    let mut pts: Vec<(f64, f64)> = grid_ms
        .iter()
        .zip(log_likelihood.iter())
        .filter(|(g, l)| g.is_finite() && l.is_finite())
        .map(|(g, l)| (*g, *l + prior.log_prior(*g)))
        .collect();
    if pts.is_empty() {
        return None;
    }
    pts.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    let m = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max);
    let weights: Vec<f64> = pts.iter().map(|p| (p.1 - m).exp()).collect();
    let total: f64 = weights.iter().sum();
    if !(total > 0.0) || !total.is_finite() {
        return None;
    }

    // First maximum in ascending-offset order (deterministic ties).
    let mut argmax_idx = 0usize;
    let mut best = f64::NEG_INFINITY;
    for (i, p) in pts.iter().enumerate() {
        if p.1 > best {
            best = p.1;
            argmax_idx = i;
        }
    }
    let argmax_ms = pts[argmax_idx].0;

    let conf_mass: f64 = pts
        .iter()
        .zip(weights.iter())
        .filter(|((g, _), _)| (g - argmax_ms).abs() <= CONF_WINDOW_MS)
        .map(|(_, w)| *w)
        .sum();
    let conf_posterior = (conf_mass / total).clamp(0.0, 1.0);

    // Central 95% credible interval from the grid CDF.
    let mut cum = 0.0f64;
    let mut lo = pts[0].0;
    let mut hi = pts[pts.len() - 1].0;
    let mut lo_set = false;
    for (p, w) in pts.iter().zip(weights.iter()) {
        cum += *w;
        if !lo_set && cum >= 0.025 * total {
            lo = p.0;
            lo_set = true;
        }
        if cum >= 0.975 * total {
            hi = p.0;
            break;
        }
    }
    Some(Posterior { argmax_ms, conf_posterior, ci95: (lo, hi) })
}

/// Thin wrapper over rs-sync's closed-form gain profile (design D1) so the
/// likelihood layer has a single entry point for the nuisance solve.
/// `delay_s` follows rs-sync's internal delay convention (same argument as
/// `pre_sync`). Returns g ∈ [0.3, 3.0]; 1.0 on degenerate input.
pub fn solve_gain_for_window(problem: &rs_sync::SyncProblem, delay_s: f64, ts_from: i64, ts_to: i64) -> f64 {
    problem.solve_gain(delay_s, ts_from, ts_to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(step: f64, lo: f64, hi: f64) -> Vec<f64> {
        let mut g = Vec::new();
        let mut x = lo;
        while x <= hi + 1e-9 {
            g.push(x);
            x += step;
        }
        g
    }

    /// Gaussian-bump log-likelihood centered at `mu` with width `w` and depth
    /// `depth` (logL 0 at peak, −depth far away).
    fn bump_logl(grid: &[f64], mu: f64, w: f64, depth: f64) -> Vec<f64> {
        grid.iter()
            .map(|&x| {
                let d = (x - mu) / w;
                -depth * (1.0 - (-0.5 * d * d).exp())
            })
            .collect()
    }

    #[test]
    fn sigma_mad_matches_known_values() {
        // Symmetric set: median 0, MAD 2 → σ = 2.9652.
        let r = [-4.0, -2.0, 0.0, 2.0, 4.0];
        assert!((sigma_mad(&r) - 1.4826 * 2.0).abs() < 1e-12);
        // NaN-safe, constant input → floor (not 0, not NaN).
        let c = [5.0, 5.0, f64::NAN, 5.0];
        let s = sigma_mad(&c);
        assert!(s.is_finite() && s > 0.0);
        assert!(sigma_mad(&[]).is_finite());
    }

    /// The 2026-06-12 conf-underestimation fix: the loss must be evaluated
    /// on standardized residuals so the likelihood's nat scale is invariant
    /// to the residual unit (σ ≈ 0.1 had been multiplying all evidence by
    /// σ² ≈ 0.01, flattening the posterior).
    #[test]
    fn tukey_rho_is_standardized_and_scale_invariant() {
        // Dimensionless: ρ(k·r, k·σ) = ρ(r, σ) for any unit change k.
        for (r, s, k) in [(0.3, 1.0, 0.01), (2.0, 0.7, 100.0), (5.0, 1.0, 0.1)] {
            assert!((tukey_rho(r * k, s * k) - tukey_rho(r, s)).abs() < 1e-12,
                "rho not scale-invariant at r={r} s={s} k={k}");
        }
        // Saturation bound is the constant TUKEY_C²/6 regardless of σ.
        let bound = TUKEY_C * TUKEY_C / 6.0;
        assert!((tukey_rho(1.0, 0.01) - bound).abs() < 1e-12);
        assert!((tukey_rho(1000.0, 10.0) - bound).abs() < 1e-12);
        // Gaussian core: ρ ≈ u²/2 for small standardized residuals.
        let (r, s) = (0.001, 0.1);
        let u: f64 = r / s;
        assert!((tukey_rho(r, s) - u * u / 2.0).abs() < 1e-6);
    }

    /// Same data measured in different units (residuals AND σ scaled
    /// together) must yield identical likelihood separations, and a clearly
    /// saturated wrong-δ window must stay O(n_eff) nats below a good one —
    /// this fails on the pre-fix raw-unit ρ (separation collapsed to
    /// ~σ²·n_eff ≈ 0.04 nats at σ = 0.1).
    #[test]
    fn window_logl_separation_invariant_to_residual_scale() {
        let sigma = 0.1;
        let good: Vec<f64> = (0..100).map(|i| 0.05 * ((i as f64 / 50.0) - 1.0)).collect();
        let bad: Vec<f64> = (0..100).map(|i| 1.0 + 0.3 * ((i as f64 / 50.0) - 1.0)).collect();
        let sep = window_log_likelihood(&[good.clone()], sigma) - window_log_likelihood(&[bad.clone()], sigma);
        let k = 0.01;
        let good_k: Vec<f64> = good.iter().map(|r| r * k).collect();
        let bad_k: Vec<f64> = bad.iter().map(|r| r * k).collect();
        let sep_k = window_log_likelihood(&[good_k], sigma * k) - window_log_likelihood(&[bad_k], sigma * k);
        assert!((sep - sep_k).abs() < 1e-9, "unit change altered evidence: {sep} vs {sep_k}");
        assert!(sep > 1.0, "saturated-window separation collapsed: {sep}");
    }

    #[test]
    fn tukey_rho_saturates_beyond_c() {
        let sigma = 1.0;
        let bound = TUKEY_C * TUKEY_C / 6.0;
        assert!((tukey_rho(10.0 * sigma, sigma) - bound).abs() < 1e-12);
        assert!((tukey_rho(100.0 * sigma, sigma) - tukey_rho(10.0 * sigma, sigma)).abs() < 1e-12);
        assert!((tukey_rho(0.0, sigma)).abs() < 1e-12);
        assert!(tukey_rho(f64::NAN, sigma).is_finite());
        // Monotone within [0, c].
        assert!(tukey_rho(0.5, sigma) < tukey_rho(1.0, sigma));
    }

    /// Design D2 scenario "置信不随点数膨胀": duplicating points WITHIN one
    /// frame pair must not change the likelihood; adding frame pairs (real
    /// new evidence) must scale it.
    #[test]
    fn n_eff_scales_by_frame_pairs_not_points() {
        let sigma = 1.0;
        let pair: Vec<f64> = (0..100).map(|i| (i as f64 / 50.0) - 1.0).collect();
        let one = window_log_likelihood(&[pair.clone()], sigma);
        // Same pair with every point duplicated: n_points ×2, n_eff unchanged.
        let mut doubled = pair.clone();
        doubled.extend_from_slice(&pair);
        let inflated = window_log_likelihood(&[doubled], sigma);
        assert!((one - inflated).abs() < 1e-9, "point duplication changed logL: {one} vs {inflated}");
        // Two independent pairs with the same residuals: logL ×2.
        let two = window_log_likelihood(&[pair.clone(), pair.clone()], sigma);
        assert!((two - 2.0 * one).abs() < 1e-9);
        // Empty window → flat (0) likelihood.
        assert_eq!(window_log_likelihood(&[], sigma), 0.0);
    }

    /// residuals.csv thinning compatibility (GYROFLOW_SYNC_DIAG=2 thins grid
    /// δ to ≤2000 points while δ* stays full): `window_log_likelihood` is
    /// `-n_pairs × mean(ρ)` — a per-point AVERAGE over the same dumped frame
    /// pairs, invariant to the per-δ point count, so thinned and full δ are
    /// directly comparable on the same likelihood scale. `sync_replay` only
    /// multiplies this by the constant `n_eff_cli / 150` (gate-6
    /// perturbation knob), which cannot break the invariance. Constructed
    /// exactly: the full set repeats every kept value `stride` times
    /// adjacently, so equidistant stride-thinning keeps each distinct value
    /// once and the means match to within float associativity.
    #[test]
    fn replay_rescale_invariant_to_per_delta_point_count() {
        let sigma = 1.0;
        let n_eff_cli = 75.0;
        let stride = 18usize;
        let kept: Vec<f64> = (0..200).map(|i| (i as f64) * 0.01 - 1.0).collect();
        let full: Vec<f64> = kept
            .iter()
            .flat_map(|&v| std::iter::repeat(v).take(stride))
            .collect();
        let rescale = |groups: &[Vec<f64>]| -> f64 {
            window_log_likelihood(groups, sigma) * (n_eff_cli / 150.0)
        };
        let l_full = rescale(&[full.clone(), full.clone()]);
        let l_thin = rescale(&[kept.clone(), kept.clone()]);
        assert!(
            (l_full - l_thin).abs() < 1e-9,
            "full-δ logL {l_full} must equal thinned-δ logL {l_thin}"
        );
    }

    /// Approx-mode n_eff direction: larger n_eff → sharper posterior →
    /// higher conf_posterior on the same cost curve.
    #[test]
    fn n_eff_scaling_direction_in_approx_mode() {
        let g = grid(5.0, -500.0, 500.0);
        // Single shallow basin at -100: cost 1.0 at minimum, up to 1.05 away.
        let make = |n_eff: f64| -> Vec<f64> {
            g.iter()
                .map(|&x| {
                    let d: f64 = (x + 100.0) / 150.0;
                    let cost = 1.0 + 0.05 * (1.0 - (-0.5 * d * d).exp());
                    approx_window_log_likelihood(cost, 1.0, n_eff)
                })
                .collect()
        };
        let p75 = posterior_decide(&g, &make(75.0), &Prior::Uniform).unwrap();
        let p300 = posterior_decide(&g, &make(300.0), &Prior::Uniform).unwrap();
        assert!((p75.argmax_ms - -100.0).abs() <= 5.0);
        assert!((p300.argmax_ms - -100.0).abs() <= 5.0);
        assert!(p300.conf_posterior > p75.conf_posterior,
            "n_eff=300 conf {} should exceed n_eff=75 conf {}", p300.conf_posterior, p75.conf_posterior);
        // CI must shrink with more evidence.
        assert!((p300.ci95.1 - p300.ci95.0) <= (p75.ci95.1 - p75.ci95.0));
    }

    /// Synthetic bimodal evidence: two equal basins → mass splits ≈ 50/50 →
    /// conf_posterior ≈ 0.5 (the "multi-peak posterior is naturally low
    /// confidence" scenario of find-offset-confidence).
    #[test]
    fn bimodal_posterior_splits_mass() {
        let g = grid(5.0, -500.0, 500.0);
        let a = bump_logl(&g, -200.0, 8.0, 30.0);
        let b = bump_logl(&g, 300.0, 8.0, 30.0);
        // Pointwise max of the two bumps ≈ mixture of two equal sharp modes.
        let logl: Vec<f64> = a.iter().zip(b.iter()).map(|(x, y)| x.max(*y)).collect();
        let p = posterior_decide(&g, &logl, &Prior::Uniform).unwrap();
        assert!(p.conf_posterior > 0.35 && p.conf_posterior < 0.65,
            "bimodal conf should be ≈0.5, got {}", p.conf_posterior);
        assert!((p.argmax_ms - -200.0).abs() <= 5.0 || (p.argmax_ms - 300.0).abs() <= 5.0);
        // CI must span both modes.
        assert!(p.ci95.0 <= -195.0 && p.ci95.1 >= 295.0);
    }

    /// Prior is a log-term only: it must not truncate or shift the search
    /// domain, and an off-grid anchor must not produce -inf walls.
    #[test]
    fn prior_does_not_change_search_domain() {
        let g = grid(5.0, -500.0, 500.0);
        let logl = bump_logl(&g, 100.0, 10.0, 25.0);
        let uni = posterior_decide(&g, &logl, &Prior::Uniform).unwrap();
        // Anchor far outside the grid: posterior still defined on the full
        // grid, argmax still the likelihood mode (likelihood dominates a
        // 1.5s-wide prior over a ±0.5s grid).
        let anch = posterior_decide(&g, &logl, &Prior::Anchor { init_ms: -5000.0 }).unwrap();
        assert!((anch.argmax_ms - uni.argmax_ms).abs() <= 5.0);
        assert!(anch.ci95.0 >= g[0] && anch.ci95.1 <= g[g.len() - 1]);
        assert!(anch.conf_posterior.is_finite() && (0.0..=1.0).contains(&anch.conf_posterior));

        // Design D4 magnitude check (P1004620 numbers): anchor at -1506.43,
        // log-prior penalty of -6414 relative to -1506 ≈ 5.4 nats.
        let pr = Prior::Anchor { init_ms: -1506.43 };
        let penalty = pr.log_prior(-1506.43) - pr.log_prior(-6414.1);
        assert!((penalty - 5.35).abs() < 0.3, "penalty = {penalty}");
    }

    /// Robustness of the decision integrals: NaN/inf grid entries are
    /// dropped, outputs stay finite and clamped.
    #[test]
    fn posterior_handles_nonfinite_inputs() {
        let g = vec![-10.0, -5.0, 0.0, 5.0, 10.0];
        let logl = vec![f64::NAN, -1.0, 0.0, f64::NEG_INFINITY, -2.0];
        let p = posterior_decide(&g, &logl, &Prior::Uniform).unwrap();
        assert_eq!(p.argmax_ms, 0.0);
        assert!(p.conf_posterior.is_finite() && (0.0..=1.0).contains(&p.conf_posterior));
        // All-NaN → None, never a panic.
        assert!(posterior_decide(&g, &[f64::NAN; 5], &Prior::Uniform).is_none());
        assert!(posterior_decide(&[], &[], &Prior::Uniform).is_none());
        // Length mismatch → None.
        assert!(posterior_decide(&g, &[0.0; 3], &Prior::Uniform).is_none());
    }

    /// Conf-integration convention (2026-06-12 fix): a coarsely sampled
    /// logL must be resampled to the 5ms lattice before `posterior_decide`,
    /// after which conf matches the natively fine-sampled value; integrating
    /// directly on the coarse grid is visibly miscalibrated.
    #[test]
    fn resample_aligns_conf_convention_with_fine_grid() {
        let fine = grid(5.0, -500.0, 500.0);
        let coarse = grid(50.0, -500.0, 500.0);
        // Wide smooth basin: near-linear between 50ms samples, so linear
        // interpolation error is negligible and the convention dominates.
        let f = |x: f64| {
            let d: f64 = x / 200.0;
            -8.0 * (1.0 - (-0.5 * d * d).exp())
        };
        let logl_f: Vec<f64> = fine.iter().map(|&x| f(x)).collect();
        let logl_c: Vec<f64> = coarse.iter().map(|&x| f(x)).collect();
        let native = posterior_decide(&fine, &logl_f, &Prior::Uniform).unwrap();
        let raw_coarse = posterior_decide(&coarse, &logl_c, &Prior::Uniform).unwrap();
        let (rg, rl) = resample_logl_to_uniform_grid(&coarse, &logl_c, 5.0).unwrap();
        let res = posterior_decide(&rg, &rl, &Prior::Uniform).unwrap();
        assert!((res.argmax_ms - native.argmax_ms).abs() <= 5.0);
        assert!((res.conf_posterior - native.conf_posterior).abs() < 0.02,
            "resampled conf {} must match native 5ms conf {}", res.conf_posterior, native.conf_posterior);
        assert!((raw_coarse.conf_posterior - native.conf_posterior).abs() > 0.1,
            "coarse-grid integration should be visibly off ({} vs {})",
            raw_coarse.conf_posterior, native.conf_posterior);
    }

    /// Non-uniform input (sampled lattice + off-lattice refined δ*, the real
    /// residuals.csv shape), degenerate inputs, and non-finite filtering.
    #[test]
    fn resample_handles_nonuniform_and_degenerate_input() {
        let g = vec![-100.0, -50.0, -13.4, 0.0, 50.0, 100.0];
        let l = vec![-5.0, -3.0, -0.2, -1.0, -3.0, -5.0];
        let (rg, rl) = resample_logl_to_uniform_grid(&g, &l, 5.0).unwrap();
        assert_eq!(rg.len(), rl.len());
        assert_eq!(rg.len(), 41); // -100..=100 step 5
        assert!((rg[0] - -100.0).abs() < 1e-9 && (rg[40] - 100.0).abs() < 1e-9);
        // The interpolated max must bracket the off-lattice peak.
        let max_i = rl
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .unwrap()
            .0;
        assert!((rg[max_i] - -13.4).abs() <= 5.0, "peak drifted to {}", rg[max_i]);
        // Unsorted input is sorted internally; duplicates keep the max.
        let (rg2, rl2) = resample_logl_to_uniform_grid(&[50.0, -50.0, 0.0, 0.0], &[-2.0, -2.0, -9.0, -1.0], 5.0).unwrap();
        assert_eq!(rg2.len(), 21);
        assert!((rl2[10] - -1.0).abs() < 1e-9, "duplicate offset must keep max logL");
        // Degenerate inputs → None, never a panic.
        assert!(resample_logl_to_uniform_grid(&[0.0], &[1.0], 5.0).is_none());
        assert!(resample_logl_to_uniform_grid(&[0.0, 5.0], &[f64::NAN, 1.0], 5.0).is_none());
        assert!(resample_logl_to_uniform_grid(&[0.0, 5.0], &[0.0, 1.0], 0.0).is_none());
        assert!(resample_logl_to_uniform_grid(&[0.0, 5.0], &[0.0], 5.0).is_none());
    }

    #[test]
    fn combine_windows_adds_elementwise() {
        let a = vec![0.0, -1.0, -2.0];
        let b = vec![-3.0, 0.0, -1.0];
        let j = combine_windows(&[a.clone(), b.clone()]).unwrap();
        assert_eq!(j, vec![-3.0, -1.0, -3.0]);
        // Mismatched lengths rejected.
        assert!(combine_windows(&[a, vec![0.0; 2]]).is_none());
        assert!(combine_windows(&[]).is_none());
    }

    /// Cross-window product beats a single contaminated window (D3 shape of
    /// the P1004620 case): window 1 has twin basins (echo), window 2 is
    /// clean — the sum is unimodal at the shared true offset.
    #[test]
    fn cross_window_product_suppresses_echo_peak() {
        let g = grid(5.0, -7000.0, 0.0);
        // Window 1: true basin at -1500 and an echo basin at -6400, echo
        // slightly deeper (the single-window coin flip).
        let w1: Vec<f64> = g
            .iter()
            .map(|&x| {
                let t: f64 = (x + 1500.0) / 30.0;
                let e: f64 = (x + 6400.0) / 30.0;
                let truth = 20.0 * (-0.5 * t * t).exp();
                let echo = 22.0 * (-0.5 * e * e).exp();
                truth.max(echo) - 22.0
            })
            .collect();
        // Window 2: clean single basin at -1500.
        let w2 = bump_logl(&g, -1500.0, 30.0, 20.0);
        let joint = combine_windows(&[w1.clone(), w2]).unwrap();
        let p1 = posterior_decide(&g, &w1, &Prior::Uniform).unwrap();
        let pj = posterior_decide(&g, &joint, &Prior::Uniform).unwrap();
        assert!((p1.argmax_ms - -6400.0).abs() <= 5.0, "window 1 alone picks the echo");
        assert!((pj.argmax_ms - -1500.0).abs() <= 5.0, "joint must recover the shared truth");
    }

    /// `--join` joint product (task 2.6 shape): window A is bimodal with the
    /// echo basin DEEPER (biased argmax) on a coarse 50ms grid; window B is
    /// clean unimodal at the truth on a fine 5ms grid with a different span.
    /// Window A alone picks the echo; the joint product on the common
    /// aligned lattice must recover the truth.
    #[test]
    fn joint_on_common_grid_recovers_truth_from_biased_window() {
        let ga = grid(50.0, -7000.0, 0.0);
        let wa: Vec<f64> = ga
            .iter()
            .map(|&x| {
                let t: f64 = (x + 1500.0) / 30.0;
                let e: f64 = (x + 6400.0) / 30.0;
                let truth = 20.0 * (-0.5 * t * t).exp();
                let echo = 22.0 * (-0.5 * e * e).exp();
                truth.max(echo) - 22.0
            })
            .collect();
        let gb = grid(5.0, -6985.0, -15.0);
        let wb = bump_logl(&gb, -1500.0, 30.0, 20.0);
        // Window A alone (resampled to its own 5ms lattice) picks the echo.
        let (ra, la) = resample_logl_to_uniform_grid(&ga, &wa, 5.0).unwrap();
        let pa = posterior_decide(&ra, &la, &Prior::Uniform).unwrap();
        assert!((pa.argmax_ms - -6400.0).abs() <= 50.0, "window A alone should pick the echo, got {}", pa.argmax_ms);
        // Joint on the common aligned grid recovers the shared truth.
        let (g, l) = combine_windows_on_common_grid(&[(&ga, &wa), (&gb, &wb)], 5.0).unwrap();
        // Common span = intersection of [-7000,0] and [-6985,-15].
        assert!((g[0] - -6985.0).abs() < 1e-9 && (g[g.len() - 1] - -15.0).abs() < 1e-9);
        let pj = posterior_decide(&g, &l, &Prior::Uniform).unwrap();
        assert!((pj.argmax_ms - -1500.0).abs() <= 5.0, "joint must recover the truth, got {}", pj.argmax_ms);
    }

    /// One-window degradation: the joint product of a single window on an
    /// already-aligned grid is exactly that window's own posterior.
    #[test]
    fn joint_of_single_window_equals_single_window() {
        let g = grid(5.0, -500.0, 500.0);
        let l = bump_logl(&g, 100.0, 10.0, 25.0);
        let (jg, jl) = combine_windows_on_common_grid(&[(&g, &l)], 5.0).unwrap();
        assert_eq!(jg.len(), g.len());
        for (a, b) in jg.iter().zip(g.iter()) {
            assert!((a - b).abs() < 1e-9);
        }
        let p0 = posterior_decide(&g, &l, &Prior::Uniform).unwrap();
        let pj = posterior_decide(&jg, &jl, &Prior::Uniform).unwrap();
        assert_eq!(p0.argmax_ms, pj.argmax_ms);
        assert!((p0.conf_posterior - pj.conf_posterior).abs() < 1e-12);
        assert_eq!(p0.ci95, pj.ci95);
    }

    /// No common overlap or degenerate input → None, never a panic.
    #[test]
    fn joint_rejects_disjoint_and_degenerate_input() {
        let a = grid(5.0, 0.0, 100.0);
        let la = bump_logl(&a, 50.0, 10.0, 5.0);
        let b = grid(5.0, 200.0, 300.0);
        let lb = bump_logl(&b, 250.0, 10.0, 5.0);
        // Disjoint spans.
        assert!(combine_windows_on_common_grid(&[(&a, &la), (&b, &lb)], 5.0).is_none());
        // Touching spans share a single lattice point — still no usable overlap.
        let c = grid(5.0, 100.0, 200.0);
        let lc = bump_logl(&c, 150.0, 10.0, 5.0);
        assert!(combine_windows_on_common_grid(&[(&a, &la), (&c, &lc)], 5.0).is_none());
        // Empty input, bad step, length mismatch, too few finite points.
        assert!(combine_windows_on_common_grid(&[], 5.0).is_none());
        assert!(combine_windows_on_common_grid(&[(&a, &la)], 0.0).is_none());
        assert!(combine_windows_on_common_grid(&[(&a[..3], &la[..2])], 5.0).is_none());
        assert!(combine_windows_on_common_grid(&[(&[0.0, 5.0][..], &[f64::NAN, 1.0][..])], 5.0).is_none());
    }

    #[test]
    fn dilate_logl_is_sliding_window_max() {
        // half_pts=0 is identity.
        let v = vec![-3.0, 0.0, -1.0, -5.0];
        assert_eq!(dilate_logl(&v, 0), v);
        // half_pts=1: each point becomes max of itself and neighbors.
        assert_eq!(dilate_logl(&v, 1), vec![0.0, 0.0, 0.0, -1.0]);
        // A single sharp peak widens into a plateau of width 2*half_pts+1.
        let mut spike = vec![-10.0; 7];
        spike[3] = 0.0;
        assert_eq!(dilate_logl(&spike, 2), vec![-10.0, 0.0, 0.0, 0.0, 0.0, 0.0, -10.0]);
        // Empty input -> empty.
        assert!(dilate_logl(&[], 3).is_empty());
    }

    /// Drift-separated peaks (design §3.8): two clean windows whose sharp
    /// peaks sit 60ms apart (real drift). Un-dilated, the joint splits into
    /// two bumps. Dilated by T/2=40ms half-width (T=80ms ≥ 60ms gap), the
    /// joint becomes a single plateau covering both → argmax between them,
    /// high conf. dilate=0 must equal the plain combine.
    #[test]
    fn dilated_combine_merges_drift_separated_peaks() {
        let g = grid(5.0, -500.0, 500.0);
        let wa = bump_logl(&g, -30.0, 6.0, 25.0);
        let wb = bump_logl(&g, 30.0, 6.0, 25.0);
        // Plain combine: two separated peaks → bimodal, low conf.
        let (pg, pl) = combine_windows_on_common_grid(&[(&g, &wa), (&g, &wb)], 5.0).unwrap();
        let plain = posterior_decide(&pg, &pl, &Prior::Uniform).unwrap();
        assert!(plain.conf_posterior < 0.5, "plain combine should be low-conf bimodal, got {}", plain.conf_posterior);
        // Dilated by 40ms half-width (T=80ms ≥ 60ms gap): merged single peak.
        let (dg, dl) = combine_windows_on_common_grid_dilated(&[(&g, &wa), (&g, &wb)], 5.0, 40.0).unwrap();
        let dil = posterior_decide(&dg, &dl, &Prior::Uniform).unwrap();
        assert!(dil.argmax_ms.abs() <= 35.0, "merged argmax should sit between the peaks, got {}", dil.argmax_ms);
        assert!(dil.conf_posterior > plain.conf_posterior, "dilation must raise conf: {} vs {}", dil.conf_posterior, plain.conf_posterior);
        // dilate=0 byte-identical to plain combine.
        let (zg, zl) = combine_windows_on_common_grid_dilated(&[(&g, &wa), (&g, &wb)], 5.0, 0.0).unwrap();
        assert_eq!(zg, pg);
        assert_eq!(zl, pl);
    }

    /// g-profile entry point: degenerate problem returns the neutral gain.
    #[test]
    fn solve_gain_wrapper_neutral_on_degenerate() {
        let sp = rs_sync::SyncProblem::new();
        assert_eq!(solve_gain_for_window(&sp, 0.0, 0, 1_000_000), 1.0);
    }
}
