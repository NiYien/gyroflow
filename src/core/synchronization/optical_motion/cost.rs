// SPDX-License-Identifier: GPL-3.0-or-later

//! rotHP cost (spec §6.1): the residual of every tracked bearing against the gyro rotation at a given offset, a
//! temporal high-pass along each track (local quadratic, Savitzky-Golay) that takes the slow parallax out, and a
//! robust per-band rotation fit `r ≈ ρ × p`. The cost is the rms displacement the band rotations cause at their own
//! points. The band fits are kept as such for the optical correction (layer A) to reuse.
//!
//! The coarse scan (spec §6.2) runs the same math on a fixed subset of whole track segments, with one rotation per
//! timing band and COARSE_FIT_BANDS fit bands per pair, and without parallelism inside one evaluation.

use std::collections::HashMap;
use std::ops::Range;

use nalgebra::{ DMatrix, Matrix3, Vector3 };
use rayon::prelude::*;

use crate::gyro_source::Quat64;
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
/// The fewest bearings a frame pair's rotation rate is fitted with
pub const MIN_RATE_POINTS: usize = 25;
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
        window_time_span_ms(self.window)
    }
}

/// Min / max row time (video ms) over both frames of every observation; (+inf, -inf) when there are none
fn window_time_span_ms(window: &WindowTracks) -> (f64, f64) {
    window.pairs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |span, pd| {
        let a = pd.fa.iter().map(|&f| row_time_ms(&pd.a, f));
        let b = pd.fb.iter().map(|&f| row_time_ms(&pd.b, f));
        a.chain(b).fold(span, |(lo, hi), t| (lo.min(t), hi.max(t)))
    })
}

/// Whether the gyro data covers the row-time span `(lo, hi)` minus `offset_ms`. An empty span (a window without
/// observations) has nothing to cover.
fn span_covered(quats: &QuatTable, (lo, hi): (f64, f64), offset_ms: f64) -> bool {
    if lo <= hi { quats.covers(lo - offset_ms, hi - offset_ms) } else { true }
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

/// The rotation `m = qb⁻¹·qa` from frame a at row time `ta_ms` to frame b at `tb_ms`; gyro time = video time − `offset_ms`
fn relative_rotation(quats: &QuatTable, ta_ms: f64, tb_ms: f64, offset_ms: f64) -> Quat64 {
    quats.at(tb_ms - offset_ms).inverse() * quats.at(ta_ms - offset_ms)
}

/// Observation `i` of a pair against the rotation `m`: `p = m·va`, `r = vb − p`
fn observe(pd: &PairData, i: usize, band: u8, m: &Quat64, ta_ms: f64, tb_ms: f64) -> Derived {
    let p = m * pd.va[i];
    Derived { id: pd.ids[i], seq: pd.seq, band, p, r: pd.vb[i] - p, ta_ms, tb_ms }
}

/// Every observation of a pair at its own row times; gyro time = video time − `offset_ms`. The band is frame a's.
fn derive_pair(pd: &PairData, quats: &QuatTable, offset_ms: f64) -> Vec<Derived> {
    (0..pd.ids.len()).map(|i| {
        let (ta, tb) = (row_time_ms(&pd.a, pd.fa[i]), row_time_ms(&pd.b, pd.fb[i]));
        observe(pd, i, timing_band(pd.fa[i]), &relative_rotation(quats, ta, tb, offset_ms), ta, tb)
    }).collect()
}

/// Splits `n` observations sorted by (id, seq), `key(k)` being the k-th one's, into runs of one id in consecutive pairs
fn split_runs(n: usize, key: impl Fn(usize) -> (u32, usize)) -> Vec<Range<usize>> {
    let mut runs = Vec::new();
    let mut s = 0;
    while s < n {
        let mut e = s + 1;
        while e < n && key(e).0 == key(s).0 && key(e).1 == key(e - 1).1 + 1 { e += 1; }
        runs.push(s..e);
        s = e;
    }
    runs
}

/// Takes the slow part out of each track's residuals: a local quadratic fit (Savitzky-Golay, the fit of the window's
/// edge at the run's ends) over a run of the same id in consecutive pairs. None for the points outside a run of at
/// least HP_MIN pairs.
fn high_pass(derived: &[Derived], sg: &SgCache) -> Vec<Option<Vector3<f64>>> {
    let mut order: Vec<usize> = (0..derived.len()).collect();
    order.par_sort_unstable_by_key(|&i| (derived[i].id, derived[i].seq));
    let runs = split_runs(order.len(), |k| (derived[order[k]].id, derived[order[k]].seq));
    high_pass_runs(derived, &order, &runs, sg)
}

/// The high-pass of `high_pass` over runs given as ranges of `order` (indices into `derived`, each run one track in
/// consecutive pairs, in seq order). Points outside a run of at least HP_MIN pairs stay None.
fn high_pass_runs(derived: &[Derived], order: &[usize], runs: &[Range<usize>], sg: &SgCache) -> Vec<Option<Vector3<f64>>> {
    let mut out = vec![None; derived.len()];
    for run in runs {
        let (s, len) = (run.start, run.len());
        if len < HP_MIN { continue; }
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
    out
}

/// One band of one frame pair: the rotation its points moved by beyond the quaternions, `r ≈ ρ × p`, by IRLS
/// (weights 1/(1+(e/2.5s)²), s = 1.4826·MAD). None with fewer than MIN_BAND_POINTS points, a point without a
/// residual, a singular system or an effective weight sum below MIN_BAND_POINTS / 2.
fn fit_band(derived: &[Derived], r: &[Option<Vector3<f64>>], idx: &[usize], seq: usize, band: u8) -> Option<BandFit> {
    fit_band_rounds(derived, r, idx, seq, band, IRLS_ROUNDS)
}

/// `fit_band` with `rounds` reweighting rounds
fn fit_band_rounds(derived: &[Derived], r: &[Option<Vector3<f64>>], idx: &[usize], seq: usize, band: u8, rounds: usize) -> Option<BandFit> {
    if idx.len() < MIN_BAND_POINTS { return None; }
    if idx.iter().any(|&i| r[i].is_none()) { return None; }
    let (rho, h, w, res) = irls_rotation(idx.len(), |k| (derived[idx[k]].p, r[idx[k]].unwrap()), rounds)?;
    let sw: f64 = w.iter().sum();
    if sw < MIN_BAND_POINTS as f64 * 0.5 { return None; }
    let var = res.iter().zip(&w).map(|(e, w)| w * e * e).sum::<f64>() / sw / 2.0;
    let ta_ms = idx.iter().zip(&w).map(|(&i, w)| derived[i].ta_ms * w).sum::<f64>() / sw;
    let tb_ms = idx.iter().zip(&w).map(|(&i, w)| derived[i].tb_ms * w).sum::<f64>() / sw;
    // Mean squared displacement the band's rotation causes at its own points: weighs roll by its lever arm
    let disp2 = idx.iter().map(|&i| rho.cross(&derived[i].p).norm_squared()).sum::<f64>() / idx.len() as f64;
    Some(BandFit { seq, band, rho, n: idx.len(), weight_sum: sw, h, var, ta_ms, tb_ms, disp2 })
}

/// IRLS of r ≈ ρ × p over `n` points, `point(k)` giving the k-th (p, r): ρ, the normal matrix of the last solve, the
/// final weights and the residuals. None when the normal matrix cannot be inverted.
fn irls_rotation(n: usize, point: impl Fn(usize) -> (Vector3<f64>, Vector3<f64>), rounds: usize) -> Option<(Vector3<f64>, Matrix3<f64>, Vec<f64>, Vec<f64>)> {
    let mut w = vec![1.0f64; n];
    let mut rho = Vector3::zeros();
    let mut h = Matrix3::zeros();
    let mut res = vec![0.0f64; n];
    for _ in 0..rounds {
        h = Matrix3::zeros();
        let mut g = Vector3::zeros();
        for k in 0..n {
            let (p, ri) = point(k);
            h += (Matrix3::identity() - p * p.transpose()) * w[k];
            g += p.cross(&ri) * w[k];
        }
        rho = h.try_inverse()? * g;
        for k in 0..n {
            let (p, ri) = point(k);
            res[k] = (ri - rho.cross(&p)).norm();
        }
        let mut sorted = res.clone();
        sorted.sort_unstable_by(f64::total_cmp);
        let scale = (1.4826 * sorted[sorted.len() / 2]).max(1e-9);
        for (w, e) in w.iter_mut().zip(&res) { *w = 1.0 / (1.0 + (e / (2.5 * scale)).powi(2)); }
    }
    Some((rho, h, w, res))
}

/// Pure rotation between the two frames of a pair, fitted to all of its bearings without the high-pass, as an
/// angular velocity in the quaternions' frame (rad/s). None with fewer than MIN_RATE_POINTS bearings or a degenerate fit.
pub fn pair_rotation_rate(pair: &PairData) -> Option<Vector3<f64>> {
    if pair.va.len() < MIN_RATE_POINTS { return None; }
    let dt = (pair.b.mid_ms - pair.a.mid_ms) / 1000.0;
    if dt <= 0.0 { return None; }
    let (rho, _, w, _) = irls_rotation(pair.va.len(), |k| (pair.va[k], pair.vb[k] - pair.va[k]), IRLS_ROUNDS)?;
    let sw: f64 = w.iter().sum();
    if sw < MIN_RATE_POINTS as f64 * 0.5 { return None; }
    let rate = rho / dt;
    if rate.iter().all(|v| v.is_finite()) { Some(rate) } else { None }
}
/// Gyro time = video time − offset_ms. None when the window's time span minus the offset is not fully covered by gyro
/// data. A window without observations has nothing to cover and gives Some(empty). The fits are sorted by (seq, band).
pub fn band_fits_full(ctx: &CostContext, offset_ms: f64) -> Option<Vec<BandFit>> {
    if !span_covered(ctx.quats, ctx.time_span_ms(), offset_ms) { return None; }

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

/// Per pair, the indices (into PairData::ids) of the points kept for the coarse scan. Fixed for every offset.
pub struct TrackSubset {
    pub per_pair: Vec<Vec<u32>>,
    /// The window's row-time span, video ms (as `CostContext::time_span_ms`)
    span_ms: (f64, f64),
    /// The subset's points are numbered pair by pair in `per_pair` order, the order `eval_coarse` derives them in.
    /// `hp_order` lists them by (id, seq) and `hp_runs` are its runs of one track in consecutive pairs.
    hp_order: Vec<usize>,
    hp_runs: Vec<Range<usize>>,
    /// (seq, fit band, point numbers) per pair and fit band, by (seq, fit band)
    groups: Vec<(usize, u8, Vec<usize>)>,
}

/// Fit band of the coarse evaluation: BANDS / COARSE_FIT_BANDS adjacent timing bands each
fn coarse_fit_band(pos: f32) -> u8 {
    timing_band(pos) / (BANDS / COARSE_FIT_BANDS) as u8
}

/// Picks the coarse scan's points by whole track segments (spec §6.2), so that the high-pass keeps its runs: a segment
/// is a track's observations in consecutive pairs, and only segments of at least HP_MIN pairs count. Longest first (then
/// by id), a segment is taken whole when any (pair, fit band) it passes through has fewer than
/// `coarse_points / COARSE_FIT_BANDS` points so far.
pub fn select_tracks(window: &WindowTracks, coarse_points: usize) -> TrackSubset {
    // Every observation as (id, seq, pair, index in pair); sorted, a track's observations follow each other
    let mut obs: Vec<(u32, usize, usize, u32)> = window.pairs.iter().enumerate()
        .flat_map(|(p, pd)| pd.ids.iter().enumerate().map(move |(i, &id)| (id, pd.seq, p, i as u32)))
        .collect();
    obs.sort_unstable();
    let mut segments = split_runs(obs.len(), |k| (obs[k].0, obs[k].1));
    segments.retain(|s| s.len() >= HP_MIN);
    // A track split by a gap has several segments: the start seq orders those of equal length
    segments.sort_unstable_by_key(|s| (std::cmp::Reverse(s.len()), obs[s.start].0, obs[s.start].1));

    let quota = coarse_points / COARSE_FIT_BANDS;
    let band_of = |&(_, _, p, i): &(u32, usize, usize, u32)| coarse_fit_band(window.pairs[p].fa[i as usize]) as usize;
    let mut count = vec![[0usize; COARSE_FIT_BANDS]; window.pairs.len()];
    let mut per_pair = vec![Vec::new(); window.pairs.len()];
    for seg in segments {
        let seg = &obs[seg];
        if seg.iter().any(|o| count[o.2][band_of(o)] < quota) {
            for o in seg {
                count[o.2][band_of(o)] += 1;
                per_pair[o.2].push(o.3);
            }
        }
    }
    for sel in &mut per_pair { sel.sort_unstable(); }

    // The layout `eval_coarse` reuses for every offset
    let mut keys: Vec<(u32, usize)> = Vec::new();
    let mut groups = Vec::new();
    for (pd, sel) in window.pairs.iter().zip(&per_pair) {
        let mut by_band: [Vec<usize>; COARSE_FIT_BANDS] = Default::default();
        for &i in sel {
            by_band[coarse_fit_band(pd.fa[i as usize]) as usize].push(keys.len());
            keys.push((pd.ids[i as usize], pd.seq));
        }
        groups.extend(by_band.into_iter().enumerate().map(|(band, idx)| (pd.seq, band as u8, idx)));
    }
    let mut hp_order: Vec<usize> = (0..keys.len()).collect();
    hp_order.sort_unstable_by_key(|&k| (keys[k], k));
    let hp_runs = split_runs(hp_order.len(), |k| keys[hp_order[k]]);

    TrackSubset { per_pair, span_ms: window_time_span_ms(window), hp_order, hp_runs, groups }
}

/// Coarse evaluation: subset only, one quaternion pair per (pair, timing band) at the band's centre row time, COARSE_FIT_BANDS fit bands, no inner parallelism.
///
/// The band is the point's frame a position as in the full evaluation, and both frames of the pair take their row
/// time at that band's centre. Otherwise the residual, high-pass and band fit are those of the full evaluation, with
/// `irls_iters` reweighting rounds (at least 1). `subset` must come from `ctx.window`. Returns the cost in px; None
/// when the window's row-time span minus `offset_ms` is not fully covered by gyro data (the full evaluation's rule)
/// or when no fit band could be fitted.
pub fn eval_coarse(ctx: &CostContext, subset: &TrackSubset, offset_ms: f64, irls_iters: usize) -> Option<f64> {
    debug_assert_eq!(subset.per_pair.len(), ctx.window.pairs.len(), "subset of another window");
    if !span_covered(ctx.quats, subset.span_ms, offset_ms) { return None; }

    let mut derived = Vec::with_capacity(subset.hp_order.len());
    for (pd, sel) in ctx.window.pairs.iter().zip(&subset.per_pair) {
        if sel.is_empty() { continue; }
        let rot: [(Quat64, f64, f64); BANDS] = std::array::from_fn(|band| {
            let pos = (band as f32 + 0.5) / BANDS as f32;
            let (ta, tb) = (row_time_ms(&pd.a, pos), row_time_ms(&pd.b, pos));
            (relative_rotation(ctx.quats, ta, tb, offset_ms), ta, tb)
        });
        for &i in sel {
            let band = timing_band(pd.fa[i as usize]);
            let (m, ta, tb) = &rot[band as usize];
            derived.push(observe(pd, i as usize, band, m, *ta, *tb));
        }
    }
    let rhp = high_pass_runs(&derived, &subset.hp_order, &subset.hp_runs, ctx.sg);
    let fits: Vec<BandFit> = subset.groups.iter()
        .filter_map(|(seq, band, idx)| fit_band_rounds(&derived, &rhp, idx, *seq, *band, irls_iters.max(1)))
        .collect();
    if fits.is_empty() { None } else { Some(summarize(&fits, ctx.window.focal_px).cost_px) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synchronization::optical_motion::testutil::{ synth_window, SynthSpec, XorShift64 };

    // Pins the band fit output across the IRLS core extraction (spec §5.1). The constants depend on the toolchain's
    // float library: to refresh them, set both to 0, run the test and copy the values from the failure message.
    const GOLDEN_LEN: usize = 1074;
    const GOLDEN_HASH: u64 = 15466644730790657508;

    #[test]
    fn band_fits_bits_unchanged() {
        let spec = SynthSpec::default();
        let (window, quats) = synth_window(&spec);
        let table = QuatTable::build(&quats, -3000.0, 15000.0);
        let sg = SgCache::new();
        let ctx = CostContext { window: &window, quats: &table, sg: &sg };
        let fits = band_fits_full(&ctx, spec.true_offset_ms).expect("covered");
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for f in &fits {
            for v in [f.rho.x, f.rho.y, f.rho.z, f.var, f.weight_sum] {
                h = (h ^ v.to_bits()).wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        assert_eq!((fits.len(), h), (GOLDEN_LEN, GOLDEN_HASH));
    }
    #[test]
    fn pair_rotation_rate_matches_quaternion_rate() {
        // The fit r ≈ ρ × p is a linearization: its second-order bias is about half the rotation between the two frames
        // times an axis factor, up to ~1.2 % for this fixture's motion. Hence 3 %, not 1 %.
        let spec = SynthSpec { noise_px: 0.0, parallax: 0.0, readout_ms: 0.0, ..Default::default() };
        let (window, quats) = synth_window(&spec);
        let table = QuatTable::build(&quats, -3000.0, 15000.0);
        for pd in &window.pairs {
            let dt = (pd.b.mid_ms - pd.a.mid_ms) / 1000.0;
            let m = table.at(pd.b.mid_ms - spec.true_offset_ms).inverse() * table.at(pd.a.mid_ms - spec.true_offset_ms);
            let expected = m.scaled_axis() / dt;
            let got = pair_rotation_rate(pd).expect("enough points");
            assert!((got - expected).norm() <= 0.03 * expected.norm() + 1e-4, "seq {}: {got:?} vs {expected:?}", pd.seq);
        }
    }

    #[test]
    fn pair_rotation_rate_needs_min_points() {
        let (window, _) = synth_window(&SynthSpec::default());
        let mut pd = window.pairs[0].clone();
        let n = MIN_RATE_POINTS - 1;
        pd.ids.truncate(n); pd.va.truncate(n); pd.vb.truncate(n); pd.fa.truncate(n); pd.fb.truncate(n);
        assert!(pair_rotation_rate(&pd).is_none());
    }

    #[test]
    fn pair_rotation_rate_uses_the_pair_time_span() {
        let (window, _) = synth_window(&SynthSpec::default());
        let pd = window.pairs[3].clone();
        let base = pair_rotation_rate(&pd).unwrap();
        let mut wide = pd.clone();
        wide.b.mid_ms = wide.a.mid_ms + 2.0 * (pd.b.mid_ms - pd.a.mid_ms);
        let half = pair_rotation_rate(&wide).unwrap();
        assert!((half * 2.0 - base).norm() <= 1e-12 * base.norm().max(1.0));
    }
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
    #[test] fn subset_keeps_tracks_continuous_and_is_deterministic() {
        let (w, _) = synth_window(&SynthSpec { tracks: 1200, ..Default::default() });
        let s = select_tracks(&w, 200);
        assert_eq!(s.per_pair, select_tracks(&w, 200).per_pair);
        // every selected id appears in >= HP_MIN consecutive pairs
        let mut runs: std::collections::HashMap<u32, usize> = Default::default();
        for (pd, sel) in w.pairs.iter().zip(&s.per_pair) { for &i in sel { *runs.entry(pd.ids[i as usize]).or_default() += 1; } }
        assert!(runs.values().all(|&n| n >= HP_MIN));
        let per_pair = s.per_pair.iter().map(|v| v.len()).max().unwrap();
        assert!(per_pair >= 200 && per_pair < 400, "{per_pair}");
    }
    #[test] fn coarse_cost_agrees_with_full_near_truth() {
        let spec = SynthSpec { tracks: 1200, ..Default::default() };
        let (w, quats) = synth_window(&spec);
        let table = QuatTable::build(&quats, -3000.0, 15000.0);
        let ctx = CostContext { window: &w, quats: &table, sg: &SgCache::new() };
        let s = select_tracks(&w, 200);
        let c = |d: f64| eval_coarse(&ctx, &s, spec.true_offset_ms + d, 5).unwrap();
        assert!(c(0.0) < 0.3 * c(30.0) && c(0.0) < 0.3 * c(-30.0));
        assert!(c(5.0) < c(30.0));          // a grid point 5 ms off the truth still sits inside the basin
    }
    /// Drops the observations `drop(id, seq)` picks from every pair of `w`
    fn drop_obs(w: &mut WindowTracks, drop: impl Fn(u32, usize) -> bool) {
        for pd in &mut w.pairs {
            let keep: Vec<usize> = (0..pd.ids.len()).filter(|&i| !drop(pd.ids[i], pd.seq)).collect();
            pd.ids = keep.iter().map(|&i| pd.ids[i]).collect();
            pd.va = keep.iter().map(|&i| pd.va[i]).collect();
            pd.vb = keep.iter().map(|&i| pd.vb[i]).collect();
            pd.fa = keep.iter().map(|&i| pd.fa[i]).collect();
            pd.fb = keep.iter().map(|&i| pd.fb[i]).collect();
        }
    }
    #[test] fn subset_takes_whole_segments_of_hp_min_or_more() {
        // Every track breaks at seq 20 + id % 7, leaving a 20..26 pair head and a 152..158 pair tail; every fifth track
        // also breaks at seq 5, which splits its head into 5 and 14..20 pairs
        let (mut w, _) = synth_window(&SynthSpec::default());
        drop_obs(&mut w, |id, seq| seq == 20 + id as usize % 7 || (id % 5 == 0 && seq == 5));
        let s = select_tracks(&w, 200);
        let present: std::collections::HashSet<(u32, usize)> = w.pairs.iter().flat_map(|pd| pd.ids.iter().map(move |&id| (id, pd.seq))).collect();
        let selected: std::collections::HashSet<(u32, usize)> = w.pairs.iter().zip(&s.per_pair)
            .flat_map(|(pd, sel)| sel.iter().map(move |&i| (pd.ids[i as usize], pd.seq))).collect();
        assert_eq!(selected.len(), s.per_pair.iter().map(Vec::len).sum::<usize>());   // no point twice
        for &(id, seq) in &selected {
            // The maximal segment of consecutive pairs the observation belongs to is taken whole and is long enough
            let first = (0..=seq).rev().take_while(|&q| present.contains(&(id, q))).last().unwrap();
            let last = (seq..).take_while(|&q| present.contains(&(id, q))).last().unwrap();
            assert!(last + 1 - first >= HP_MIN, "id {id}: segment {first}..={last}");
            assert!((first..=last).all(|q| selected.contains(&(id, q))), "id {id}: segment {first}..={last} taken in part");
        }
        // There are enough eligible points everywhere, so every (pair, fit band) reaches the quota
        for (pd, sel) in w.pairs.iter().zip(&s.per_pair) {
            let mut n = [0usize; COARSE_FIT_BANDS];
            for &i in sel { n[timing_band(pd.fa[i as usize]) as usize / (BANDS / COARSE_FIT_BANDS)] += 1; }
            assert!(n.iter().all(|&n| n >= 200 / COARSE_FIT_BANDS), "seq {}: {n:?}", pd.seq);
        }
    }
    #[test] fn coarse_is_none_when_uncovered_or_empty() {
        let (w, quats) = synth_window(&SynthSpec::default());
        let table = QuatTable::build(&quats, -8000.0, 20000.0);
        let ctx = CostContext { window: &w, quats: &table, sg: &SgCache::new() };
        let s = select_tracks(&w, 200);
        // Gyro data spans -8000..20000 ms and the window about 5000..8000 ms of video time
        assert!(eval_coarse(&ctx, &s, -12000.0, 5).is_some());
        assert!(eval_coarse(&ctx, &s, -12500.0, 5).is_none());   // partly past the gyro end
        assert!(eval_coarse(&ctx, &s, 14000.0, 5).is_none());    // wholly before the gyro start
        let empty = WindowTracks { pairs: Vec::new(), focal_px: 1500.0 };
        let ctx = CostContext { window: &empty, quats: &table, sg: &SgCache::new() };
        assert!(eval_coarse(&ctx, &select_tracks(&empty, 200), 0.0, 5).is_none());   // nothing to cover, nothing fitted
    }
}
