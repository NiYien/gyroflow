// SPDX-License-Identifier: GPL-3.0-or-later

//! rotHP cost (spec §6.1): the residual of every tracked bearing against the gyro rotation at a given offset, a
//! temporal high-pass along each track (local quadratic, Savitzky-Golay) that takes the slow parallax out, and a
//! robust per-band rotation fit `r ≈ ρ × p`. The cost is the rms displacement the band rotations cause at their own
//! points. The band fits are kept as such for the optical correction (layer A) to reuse.

use std::collections::HashMap;

use nalgebra::{ DMatrix, Matrix3, Vector3 };
use rayon::prelude::*;

use super::quat_table::QuatTable;
use super::tracks::{ row_time_ms, PairData, WindowTracks };

/// Timing bands along the readout per frame pair; the full evaluation fits each of them
pub const BANDS: usize = 6;
/// Fit bands of the coarse evaluation (BANDS / COARSE_FIT_BANDS timing bands each)
pub const COARSE_FIT_BANDS: usize = 2;
/// Temporal high-pass, in frame pairs: the shortest run of a track that takes part and the longest SG window
pub const HP_MIN: usize = 15;
pub const HP_MAX: usize = 61;
/// The fewest points a band is fitted with
pub const MIN_BAND_POINTS: usize = 25;
/// Reweighting rounds of the band fit
const IRLS_ROUNDS: usize = 5;
/// The smallest odd SG window length
const SG_FIRST: usize = HP_MIN | 1;

/// Savitzky-Golay projection matrices for every odd window length in HP_MIN..=HP_MAX
pub struct SgCache(Vec<DMatrix<f64>>);

impl SgCache {
    pub fn new() -> Self {
        Self((SG_FIRST..=HP_MAX).step_by(2).map(sg_projection).collect())
    }
    /// The projection of an odd window length `l` in HP_MIN..=HP_MAX
    fn get(&self, l: usize) -> &DMatrix<f64> {
        debug_assert!(l % 2 == 1 && (SG_FIRST..=HP_MAX).contains(&l));
        &self.0[(l - SG_FIRST) / 2]
    }
}

impl Default for SgCache {
    fn default() -> Self { Self::new() }
}

/// Maps the samples of a window of `l` to their least-squares quadratic: row `k` gives the fit at sample `k`
pub fn sg_projection(l: usize) -> DMatrix<f64> {
    let c = (l as f64 - 1.0) / 2.0;
    let x = DMatrix::from_fn(l, 3, |i, j| ((i as f64 - c) / c.max(1.0)).powi(j as i32));
    let xtx = x.transpose() * &x;
    let inv = xtx.try_inverse().unwrap_or_else(|| DMatrix::zeros(3, 3));
    &x * inv * x.transpose()
}

/// One band of one frame pair. `h`, `var`, `ta_ms`, `tb_ms` are what correction layer A needs (cov = h⁻¹·var).
#[derive(Clone, Debug)]
pub struct BandFit {
    pub seq: usize,
    pub band: u8,
    /// Rotation the band's points moved by beyond the quaternions, `r ≈ ρ × p`, rad
    pub rho: Vector3<f64>,
    /// Points fitted
    pub n: usize,
    /// Sum of the final IRLS weights
    pub weight_sum: f64,
    /// Weighted normal matrix Σ w·(I − p·pᵀ) of the last solve
    pub h: Matrix3<f64>,
    /// Weighted residual variance per component, rad²
    pub var: f64,
    /// Weighted mean row times of the two frames, video ms
    pub ta_ms: f64,
    pub tb_ms: f64,
    /// Mean of |ρ × p|² over the band's points, rad²
    pub disp2: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct CostResult { pub cost_px: f64, pub bands: usize, pub pairs_measured: usize, pub points: usize }

pub struct CostContext<'a> { pub window: &'a WindowTracks, pub quats: &'a QuatTable, pub sg: &'a SgCache }

impl CostContext<'_> {
    /// Min / max row time (video ms) over both frames of every observation; (+inf, -inf) when there are none
    pub fn time_span_ms(&self) -> (f64, f64) {
        self.window.pairs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |span, pd| {
            let a = pd.fa.iter().map(|&f| row_time_ms(&pd.a, f));
            let b = pd.fb.iter().map(|&f| row_time_ms(&pd.b, f));
            a.chain(b).fold(span, |(lo, hi), t| (lo.min(t), hi.max(t)))
        })
    }
}

/// One observation against the quaternions at a given offset
struct Derived {
    id: u32,
    seq: usize,
    band: u8,
    /// Frame a's bearing carried to frame b by the quaternions
    p: Vector3<f64>,
    /// Frame b's bearing minus `p`
    r: Vector3<f64>,
    ta_ms: f64,
    tb_ms: f64,
}

/// Timing band of a position along the readout in 0..1
fn timing_band(pos: f32) -> u8 {
    (pos * BANDS as f32).floor().clamp(0.0, (BANDS - 1) as f32) as u8
}

/// Every observation of a pair at its own row times; gyro time = video time − `offset_ms`. The band is frame a's.
fn derive_pair(pd: &PairData, quats: &QuatTable, offset_ms: f64) -> Vec<Derived> {
    (0..pd.ids.len()).map(|i| {
        let (ta, tb) = (row_time_ms(&pd.a, pd.fa[i]), row_time_ms(&pd.b, pd.fb[i]));
        let m = quats.at(tb - offset_ms).inverse() * quats.at(ta - offset_ms);
        let p = m * pd.va[i];
        Derived { id: pd.ids[i], seq: pd.seq, band: timing_band(pd.fa[i]), p, r: pd.vb[i] - p, ta_ms: ta, tb_ms: tb }
    }).collect()
}

/// Takes the slow part out of each track's residuals: a local quadratic fit (Savitzky-Golay, the fit of the window's
/// edge at the run's ends) over a run of the same id in consecutive pairs. None for the points outside a run of at
/// least HP_MIN pairs.
fn high_pass(derived: &[Derived], sg: &SgCache) -> Vec<Option<Vector3<f64>>> {
    let mut order: Vec<usize> = (0..derived.len()).collect();
    order.par_sort_unstable_by_key(|&i| (derived[i].id, derived[i].seq));
    let mut out = vec![None; derived.len()];
    let mut s = 0;
    while s < order.len() {
        let mut e = s + 1;
        while e < order.len() && derived[order[e]].id == derived[order[s]].id && derived[order[e]].seq == derived[order[e - 1]].seq + 1 { e += 1; }
        let len = e - s;
        if len >= HP_MIN {
            let l = { let l = len.min(HP_MAX); if l % 2 == 0 { l - 1 } else { l } };
            let proj = sg.get(l);
            for k in 0..len {
                let w0 = (k as isize - (l / 2) as isize).clamp(0, (len - l) as isize) as usize;
                let pos = k - w0;
                let mut smooth = Vector3::zeros();
                for j in 0..l { smooth += derived[order[s + w0 + j]].r * proj[(pos, j)]; }
                out[order[s + k]] = Some(derived[order[s + k]].r - smooth);
            }
        }
        s = e;
    }
    out
}

/// One band of one frame pair: the rotation its points moved by beyond the quaternions, `r ≈ ρ × p`, by IRLS
/// (weights 1/(1+(e/2.5s)²), s = 1.4826·MAD). None with fewer than MIN_BAND_POINTS points, a point without a
/// residual, a singular system or an effective weight sum below MIN_BAND_POINTS / 2.
fn fit_band(derived: &[Derived], r: &[Option<Vector3<f64>>], idx: &[usize], seq: usize, band: u8) -> Option<BandFit> {
    if idx.len() < MIN_BAND_POINTS { return None; }
    let mut w = vec![1.0f64; idx.len()];
    let mut rho = Vector3::zeros();
    let mut h = Matrix3::zeros();
    let mut res = vec![0.0f64; idx.len()];
    for _ in 0..IRLS_ROUNDS {
        h = Matrix3::zeros();
        let mut g = Vector3::zeros();
        for (k, &i) in idx.iter().enumerate() {
            let (p, ri) = (derived[i].p, r[i]?);
            h += (Matrix3::identity() - p * p.transpose()) * w[k];
            g += p.cross(&ri) * w[k];
        }
        rho = h.try_inverse()? * g;
        for (k, &i) in idx.iter().enumerate() {
            res[k] = (r[i]? - rho.cross(&derived[i].p)).norm();
        }
        let mut sorted = res.clone();
        sorted.sort_unstable_by(f64::total_cmp);
        let scale = (1.4826 * sorted[sorted.len() / 2]).max(1e-9);
        for (w, e) in w.iter_mut().zip(&res) { *w = 1.0 / (1.0 + (e / (2.5 * scale)).powi(2)); }
    }
    let sw: f64 = w.iter().sum();
    if sw < MIN_BAND_POINTS as f64 * 0.5 { return None; }
    let var = res.iter().zip(&w).map(|(e, w)| w * e * e).sum::<f64>() / sw / 2.0;
    let ta_ms = idx.iter().zip(&w).map(|(&i, w)| derived[i].ta_ms * w).sum::<f64>() / sw;
    let tb_ms = idx.iter().zip(&w).map(|(&i, w)| derived[i].tb_ms * w).sum::<f64>() / sw;
    // Mean squared displacement the band's rotation causes at its own points: weighs roll by its lever arm
    let disp2 = idx.iter().map(|&i| rho.cross(&derived[i].p).norm_squared()).sum::<f64>() / idx.len() as f64;
    Some(BandFit { seq, band, rho, n: idx.len(), weight_sum: sw, h, var, ta_ms, tb_ms, disp2 })
}

/// Gyro time = video time − offset_ms. None when the window's time span minus the offset is not fully covered by gyro
/// data. A window without observations has nothing to cover and gives Some(empty). The fits are sorted by (seq, band).
pub fn band_fits_full(ctx: &CostContext, offset_ms: f64) -> Option<Vec<BandFit>> {
    let (lo, hi) = ctx.time_span_ms();
    if lo <= hi && !ctx.quats.covers(lo - offset_ms, hi - offset_ms) { return None; }

    let derived: Vec<Derived> = ctx.window.pairs.par_iter().flat_map_iter(|pd| derive_pair(pd, ctx.quats, offset_ms)).collect();
    let rhp = high_pass(&derived, ctx.sg);

    let mut groups: HashMap<(usize, u8), Vec<usize>> = HashMap::new();
    for (i, d) in derived.iter().enumerate() {
        if rhp[i].is_some() { groups.entry((d.seq, d.band)).or_default().push(i); }
    }
    let mut fits: Vec<BandFit> = groups.par_iter().filter_map(|(&(seq, band), idx)| fit_band(&derived, &rhp, idx, seq, band)).collect();
    fits.sort_unstable_by_key(|f| (f.seq, f.band));
    Some(fits)
}

/// sqrt(Σ n·disp2 / Σ n) · f; pairs_measured = distinct seq. The cost is 0 when there are no fits.
pub fn summarize(fits: &[BandFit], focal_px: f64) -> CostResult {
    let points: usize = fits.iter().map(|f| f.n).sum();
    let sum: f64 = fits.iter().map(|f| f.n as f64 * f.disp2).sum();
    let cost_px = if points == 0 { 0.0 } else { (sum / points as f64).sqrt() * focal_px };
    let mut seqs: Vec<usize> = fits.iter().map(|f| f.seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    CostResult { cost_px, bands: fits.len(), pairs_measured: seqs.len(), points }
}

/// Full evaluation (spec §6.1): every point at its own row time, BANDS bands per pair. None only when the offset is
/// not covered by gyro data (see `band_fits_full`). When nothing could be fitted the result is still Some, with zero
/// bands, pairs and points and a cost of 0: callers judge it by `pairs_measured`.
pub fn eval_full(ctx: &CostContext, offset_ms: f64) -> Option<CostResult> {
    band_fits_full(ctx, offset_ms).map(|fits| summarize(&fits, ctx.window.focal_px))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synchronization::optical_motion::testutil::{ synth_window, SynthSpec, XorShift64 };

    #[test] fn sg_removes_quadratics_keeps_high_frequency() {
        let p = sg_projection(31);
        let quad: Vec<f64> = (0..31).map(|i| 2.0 + 0.3 * i as f64 - 0.01 * (i * i) as f64).collect();
        let fast: Vec<f64> = (0..31).map(|i| if i % 2 == 0 { 1.0 } else { -1.0 }).collect();
        let smooth = |x: &[f64], k: usize| (0..31).map(|j| p[(k, j)] * x[j]).sum::<f64>();
        for k in 0..31 { assert!((quad[k] - smooth(&quad, k)).abs() < 1e-9); }
        assert!((fast[15] - smooth(&fast, 15)).abs() > 0.9);
    }
    #[test] fn cost_minimum_is_at_true_offset() {
        let spec = SynthSpec::default();
        let (w, quats) = synth_window(&spec);
        let table = QuatTable::build(&quats, -3000.0, 15000.0);
        let ctx = CostContext { window: &w, quats: &table, sg: &SgCache::new() };
        let at = |d: f64| eval_full(&ctx, spec.true_offset_ms + d).unwrap().cost_px;
        assert!(at(0.0) < 0.2 * at(20.0) && at(0.0) < 0.2 * at(-20.0));
        assert!(at(0.0) < at(2.0) && at(0.0) < at(-2.0));
    }
    #[test] fn parallax_alone_costs_nothing() {      // slow per-track drift is removed by the high-pass
        let (w, quats) = synth_window(&SynthSpec { parallax: 2e-2, noise_px: 0.0, ..Default::default() });
        let table = QuatTable::build(&quats, -3000.0, 15000.0);
        let ctx = CostContext { window: &w, quats: &table, sg: &SgCache::new() };
        assert!(eval_full(&ctx, -700.0).unwrap().cost_px < 0.05);
    }
    #[test] fn band_fit_recovers_known_rotation() {
        // 200 unit bearings p, r_i = rho × p_i with rho = (1e-3, -2e-3, 5e-4): fitted rho within 1e-9, var ≈ 0
        let rho = Vector3::new(1e-3, -2e-3, 5e-4);
        let mut rng = XorShift64::new(7);
        let derived: Vec<Derived> = (0..200u32).map(|id| {
            // Bearings within about ±25° of the optical axis (-z)
            let (x, y) = ((rng.uniform() - 0.5) * 0.93, (rng.uniform() - 0.5) * 0.93);
            let p = Vector3::new(x, y, -1.0).normalize();
            let ta_ms = 100.0 + rng.uniform() * 10.0;
            Derived { id, seq: 3, band: 2, p, r: rho.cross(&p), ta_ms, tb_ms: ta_ms + 1000.0 / 60.0 }
        }).collect();
        let r: Vec<Option<Vector3<f64>>> = derived.iter().map(|d| Some(d.r)).collect();
        let idx: Vec<usize> = (0..derived.len()).collect();
        let fit = fit_band(&derived, &r, &idx, 3, 2).expect("band fitted");

        assert!((fit.rho - rho).norm() < 1e-9, "rho {:?}", fit.rho);
        assert!(fit.var < 1e-24, "var {}", fit.var);
        assert_eq!((fit.seq, fit.band, fit.n), (3, 2, 200));
        assert!((fit.weight_sum - 200.0).abs() < 1e-9, "weight sum {}", fit.weight_sum);
        let h: Matrix3<f64> = derived.iter().map(|d| Matrix3::identity() - d.p * d.p.transpose()).sum();
        assert!((fit.h - h).norm() < 1e-9);
        let mean = |f: fn(&Derived) -> f64| derived.iter().map(f).sum::<f64>() / derived.len() as f64;
        assert!((fit.ta_ms - mean(|d| d.ta_ms)).abs() < 1e-9 && (fit.tb_ms - mean(|d| d.tb_ms)).abs() < 1e-9);
        let disp2 = derived.iter().map(|d| rho.cross(&d.p).norm_squared()).sum::<f64>() / derived.len() as f64;
        assert!((fit.disp2 - disp2).abs() < 1e-6 * disp2, "disp2 {} vs {}", fit.disp2, disp2);
    }
    #[test] fn uncovered_offset_is_none() {
        let (w, quats) = synth_window(&SynthSpec::default());
        let table = QuatTable::build(&quats, -3000.0, 15000.0);
        let ctx = CostContext { window: &w, quats: &table, sg: &SgCache::new() };
        assert!(eval_full(&ctx, 14000.0).is_none());   // window 5000..8000 minus 14000 is before the gyro start
    }
    #[test] fn empty_window_is_covered_with_nothing_fitted() {
        let w = WindowTracks { pairs: Vec::new(), focal_px: 1500.0 };
        let (_, quats) = synth_window(&SynthSpec::default());
        let table = QuatTable::build(&quats, 0.0, 10.0);
        let ctx = CostContext { window: &w, quats: &table, sg: &SgCache::new() };
        let c = eval_full(&ctx, 0.0).expect("nothing to cover");
        assert_eq!((c.cost_px, c.bands, c.pairs_measured, c.points), (0.0, 0, 0, 0));
    }
}
