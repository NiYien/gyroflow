// SPDX-License-Identifier: GPL-3.0-or-later

//! Offset search (spec §6.2–§6.6) over cost functions passed in as closures. `run_search` goes through these steps:
//!
//! 1. A window with fewer than MIN_PAIRS frame pairs fails with `WindowTooShort` before anything is evaluated.
//! 2. Coarse scan: the coarse cost at every point of `grid(search_intervals(..))`, in parallel on the given pool. A grid
//!    point whose coarse cost is None or not finite is invalid. Without a valid point the search fails with
//!    `NoGyroOverlap` (the caller, which knows the gyro coverage, turns that into `FewMeasurements` when the points
//!    were covered but nothing could be fitted).
//! 3. c1 is the lowest valid grid point (see `local_minima` for why that is also the lowest local minimum). Within
//!    EDGE_STEPS grid steps of an end of a search interval or of an invalid grid point it fails with `Edge`.
//! 4. Candidates and refinement (§6.4): c1 refined by `brent_min` on full evaluations; up to FAR_CANDIDATES far
//!    candidates from the coarse curve, three full evaluations each; the near-twin check around refined c1. The lowest
//!    of these valleys is the output valley (refined once more when it is not c1); all others are competitors.
//! 5. No competitor fails with `NoContrast`; too few measured pairs at the output valley fails with `FewMeasurements`.
//! 6. G = lowest competitor cost / output valley cost, conf = `conf_from_g(G, g_full)`.
//!
//! A full evaluation that returns None or a non-finite cost counts as +inf with no measured pairs: such a point is
//! never a refined minimum, a near low point, a far valley or a competitor. Cancellation is checked before every
//! evaluation, coarse or full; once it is seen nothing more is evaluated and `run_search` returns None.

use std::ops::Range;
use std::sync::atomic::{ AtomicBool, Ordering };

use rayon::prelude::*;

/// A window needs this many frame pairs, and its output valley this many measured ones
const MIN_PAIRS: usize = 20;
/// The output valley also needs this fraction of the window's frame pairs measured
const MIN_MEASURED_FRACTION: f64 = 0.3;
/// The best coarse point may not lie within this many grid steps of a search interval end or an invalid grid point
const EDGE_STEPS: f64 = 2.0;
/// Valleys closer to c1 than this are left to the near check, ms
const MIN_VALLEY_SEP_MS: f64 = 50.0;
/// Far candidates taken from the coarse curve
const FAR_CANDIDATES: usize = 3;
/// Full evaluations of a far candidate, relative to its grid point, ms
const FAR_PROBES_MS: [f64; 3] = [0.0, -5.0, 5.0];
/// Half-widths of the Brent refinement around c1, a far candidate and a near low point, ms
const C1_REFINE_MS: f64 = 10.0;
const FAR_REFINE_MS: f64 = 10.0;
const NEAR_REFINE_MS: f64 = 5.0;
/// Brent tolerance, ms, and evaluation budget
const REFINE_TOL_MS: f64 = 0.2;
const REFINE_MAX_EVALS: usize = 10;
/// Near check: probes every NEAR_STEP_MS out to NEAR_PROBES steps on both sides of refined c1. A probe NEAR_LOW_STEPS
/// steps away that costs less than both its neighbours in that sequence is a near low point.
const NEAR_STEP_MS: f64 = 10.0;
const NEAR_PROBES: usize = 4;
const NEAR_LOW_STEPS: [usize; 2] = [2, 3];
/// Slack for distances built from grid arithmetic, ms
const EPS_MS: f64 = 1e-6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailReason { WindowTooShort, Edge, FewMeasurements, NoGyroOverlap, NoContrast, NoOpencv }

impl FailReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WindowTooShort  => "window_too_short",
            Self::Edge            => "edge",
            Self::FewMeasurements => "few_measurements",
            Self::NoGyroOverlap   => "no_gyro_overlap",
            Self::NoContrast      => "no_contrast",
            Self::NoOpencv        => "no_opencv",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SearchParams {
    pub init_ms: f64, pub search_ms: f64, pub check_negative: bool, pub step_ms: f64, pub g_full: f64,
    /// Frame pairs in the window
    pub total_pairs: usize,
}

/// A full evaluation as the caller's closure reports it
#[derive(Clone, Copy, Debug)]
pub struct FullEval { pub cost: f64, pub pairs_measured: usize }

#[derive(Clone, Debug)]
pub struct SearchOutcome {
    pub offset_ms: f64, pub cost_px: f64, pub conf: f64, pub g: f64,
    /// Position of the lowest-cost competitor
    pub second_ms: Option<f64>,
    /// Far candidates evaluated
    pub far_candidates: usize,
    /// The near twin of the output valley, relative to it (see `near_twin_ms`); None without a near low point
    pub near_ms: Option<f64>,
    pub fail: Option<FailReason>,
    /// (offset, coarse cost) at every grid point in grid order, NaN where invalid; empty when no scan ran
    pub coarse: Vec<(f64, f64)>,
    /// (offset, full cost) of every full evaluation in evaluation order, NaN where the closure returned None
    pub fine: Vec<(f64, f64)>,
}

/// `[init − search, init + search]`, plus `[−init − search, −init + search]` when `check_negative` and |init| > 1 ms.
/// Sorted by start; intervals that overlap or touch are merged into one.
pub fn search_intervals(init_ms: f64, search_ms: f64, check_negative: bool) -> Vec<(f64, f64)> {
    let mut all = vec![(init_ms - search_ms, init_ms + search_ms)];
    if check_negative && init_ms.abs() > 1.0 {
        all.push((-init_ms - search_ms, -init_ms + search_ms));
    }
    all.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut merged: Vec<(f64, f64)> = Vec::with_capacity(all.len());
    for (lo, hi) in all {
        match merged.last_mut() {
            Some(last) if lo <= last.1 => last.1 = last.1.max(hi),
            _ => merged.push((lo, hi)),
        }
    }
    merged
}

/// `lo + k·step` for every k that stays within `hi`, interval by interval. An interval with `hi <= lo` or a step that
/// is not positive gives its `lo` only.
pub fn grid(intervals: &[(f64, f64)], step_ms: f64) -> Vec<f64> {
    intervals.iter().flat_map(|&(lo, hi)| {
        let n = if step_ms > 0.0 && hi > lo { ((hi - lo) / step_ms + 1e-9).floor() as usize } else { 0 };
        (0..=n).map(move |k| lo + k as f64 * step_ms)
    }).collect()
}

/// Bounded 1-D minimisation of `f` over [lo, hi] (Brent's fmin: golden section with parabolic interpolation). Stops
/// once the minimum is bracketed to about `tol` or after `max_evals` evaluations (at least 1). Returns the evaluated
/// point with the lowest value, the later one on ties, and that value. NaN counts as +inf, and +inf values are
/// handled: a parabola through one is not finite and the step falls back to golden section.
pub fn brent_min(f: &mut dyn FnMut(f64) -> f64, lo: f64, hi: f64, tol: f64, max_evals: usize) -> (f64, f64) {
    /// (3 − √5) / 2
    const CGOLD: f64 = 0.381_966_011_250_105_1;
    let mut eval = |x: f64| { let y = f(x); if y.is_nan() { f64::INFINITY } else { y } };
    let eps = f64::EPSILON.sqrt();
    let (mut a, mut b) = if lo <= hi { (lo, hi) } else { (hi, lo) };
    let mut x = a + CGOLD * (b - a);
    let mut fx = eval(x);
    let (mut w, mut fw, mut v, mut fv) = (x, fx, x, fx);
    // The last step and the one before it
    let (mut d, mut e) = (0.0f64, 0.0f64);
    for _ in 1..max_evals.max(1) {
        let xm = 0.5 * (a + b);
        let tol1 = eps * x.abs() + tol / 3.0;
        let tol2 = 2.0 * tol1;
        if (x - xm).abs() <= tol2 - 0.5 * (b - a) { break; }
        let mut parabolic = false;
        if e.abs() > tol1 {
            // Parabola through x, w and v; taken when finite, inside (a, b) and shorter than half the step before last
            let r = (x - w) * (fx - fv);
            let mut q = (x - v) * (fx - fw);
            let mut p = (x - v) * q - (x - w) * r;
            q = 2.0 * (q - r);
            if q > 0.0 { p = -p; }
            q = q.abs();
            if p.is_finite() && q.is_finite() && p.abs() < (0.5 * q * e).abs() && p > q * (a - x) && p < q * (b - x) {
                e = d;
                d = p / q;
                let u = x + d;
                if u - a < tol2 || b - u < tol2 { d = tol1.copysign(xm - x); }
                parabolic = true;
            }
        }
        if !parabolic {
            e = if x >= xm { a - x } else { b - x };
            d = CGOLD * e;
        }
        let u = if d.abs() >= tol1 { x + d } else { x + tol1.copysign(d) };
        let fu = eval(u);
        if fu <= fx {
            if u >= x { a = x; } else { b = x; }
            (v, fv, w, fw, x, fx) = (w, fw, x, fx, u, fu);
        } else {
            if u < x { a = u; } else { b = u; }
            if fu <= fw || w == x {
                (v, fv, w, fw) = (w, fw, u, fu);
            } else if fu <= fv || v == x || v == w {
                (v, fv) = (u, fu);
            }
        }
    }
    (x, fx)
}

/// clamp((g − 1) / (g_full − 1), 0, 1); 0 when that is NaN
pub fn conf_from_g(g: f64, g_full: f64) -> f64 {
    let c = (g - 1.0) / (g_full - 1.0);
    if c.is_nan() { 0.0 } else { c.clamp(0.0, 1.0) }
}

/// offset = init, cost 0, conf 0, G 1, no curves
pub fn failed(reason: FailReason, init_ms: f64) -> SearchOutcome {
    SearchOutcome {
        offset_ms: init_ms, cost_px: 0.0, conf: 0.0, g: 1.0,
        second_ms: None, far_candidates: 0, near_ms: None,
        fail: Some(reason), coarse: Vec::new(), fine: Vec::new(),
    }
}

/// `coarse`: None = not covered by gyro data. `full`: same. Returns None when cancelled.
///
/// The steps are in the module documentation. On failure the outcome is `failed(reason, init)` with the curves
/// evaluated so far kept for diagnostics. Everything, the full evaluations' own parallelism included, runs on `pool`.
pub fn run_search(p: &SearchParams, coarse: &(dyn Fn(f64) -> Option<f64> + Sync), full: &(dyn Fn(f64) -> Option<FullEval> + Sync),
                  pool: &rayon::ThreadPool, cancel: &AtomicBool) -> Option<SearchOutcome> {
    if p.total_pairs < MIN_PAIRS { return Some(failed(FailReason::WindowTooShort, p.init_ms)); }
    pool.install(|| search(p, coarse, full, cancel))
}

/// A full evaluation as the search uses it: +inf and no measured pairs when the closure gave None or a non-finite cost
#[derive(Clone, Copy, Debug)]
struct Sample { x: f64, cost: f64, pairs: usize }

impl Sample {
    fn new(x: f64, e: Option<FullEval>) -> Self {
        match e {
            Some(e) if e.cost.is_finite() => Self { x, cost: e.cost, pairs: e.pairs_measured },
            _ => Self { x, cost: f64::INFINITY, pairs: 0 },
        }
    }
    fn measured(&self) -> bool { self.cost.is_finite() }
}

/// Runs the full evaluations one after another, logs them for `SearchOutcome::fine` and stops at cancellation
struct FullEvals<'a> {
    full: &'a (dyn Fn(f64) -> Option<FullEval> + Sync),
    cancel: &'a AtomicBool,
    log: Vec<(f64, f64)>,
    cancelled: bool,
}

impl FullEvals<'_> {
    /// None, without evaluating, once cancellation has been seen
    fn eval(&mut self, x: f64) -> Option<Sample> {
        if self.cancelled || self.cancel.load(Ordering::Relaxed) {
            self.cancelled = true;
            return None;
        }
        let e = (self.full)(x);
        self.log.push((x, e.map_or(f64::NAN, |e| e.cost)));
        Some(Sample::new(x, e))
    }

    /// The lowest-cost evaluation at `x + dx` for every `dx`, the first on ties; None when cancelled
    fn lowest(&mut self, x: f64, dxs: &[f64]) -> Option<Sample> {
        let mut best: Option<Sample> = None;
        for &dx in dxs {
            let s = self.eval(x + dx)?;
            if best.is_none_or(|b| s.cost < b.cost) { best = Some(s); }
        }
        best
    }

    /// `brent_min` over [x − half, x + half] with REFINE_TOL_MS and REFINE_MAX_EVALS, and the evaluation at the point
    /// it returns; None when cancelled
    fn refine(&mut self, x: f64, half: f64) -> Option<Sample> {
        let mut samples: Vec<Sample> = Vec::with_capacity(REFINE_MAX_EVALS);
        let (bx, _) = brent_min(&mut |u| match self.eval(u) {
            Some(s) => { samples.push(s); s.cost }
            None => f64::INFINITY,
        }, x - half, x + half, REFINE_TOL_MS, REFINE_MAX_EVALS);
        if self.cancelled { return None; }
        // brent_min's own choice: the lowest evaluated point, the later one on ties
        let best = samples.into_iter().reduce(|best, s| if s.cost <= best.cost { s } else { best })?;
        debug_assert!(best.x == bx);
        Some(best)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind { C1, Far, Near }

/// Local minima of the coarse curve, as grid indices in grid order. `costs` is NaN at invalid grid points and
/// `intervals` holds each search interval's range of grid indices.
///
/// Each interval is cut into runs of consecutive valid grid points: an invalid point or the interval's end ends a run.
/// Inside a run, a plateau (a maximal stretch of equal costs, usually a single point) is a local minimum when every
/// neighbour it has inside the run costs more. A plateau at a run's end is compared with its one inner neighbour only,
/// and a run that is a single plateau is a local minimum. A plateau is represented by its first grid point.
///
/// So the lowest valid grid point (the first one on ties) is always a local minimum: its run's points next to it
/// cost more, the one before it strictly because it is the first lowest. It is therefore c1, the lowest local minimum,
/// and the spec's fallback for a curve without local minima cannot arise.
fn local_minima(costs: &[f64], intervals: &[Range<usize>]) -> Vec<usize> {
    let mut minima = Vec::new();
    for interval in intervals {
        let mut i = interval.start;
        while i < interval.end {
            if costs[i].is_nan() { i += 1; continue; }
            let run = i..(i..interval.end).find(|&k| costs[k].is_nan()).unwrap_or(interval.end);
            let mut s = run.start;
            while s < run.end {
                let e = (s + 1..run.end).find(|&k| costs[k] != costs[s]).unwrap_or(run.end);
                let left = s == run.start || costs[s - 1] > costs[s];
                let right = e == run.end || costs[e] > costs[s];
                if left && right { minima.push(s); }
                s = e;
            }
            i = run.end;
        }
    }
    minima
}

/// Whether grid point `i` lies within EDGE_STEPS grid steps of an end of a search interval or of an invalid grid point
fn is_edge(xs: &[f64], costs: &[f64], intervals: &[(f64, f64)], i: usize, step_ms: f64) -> bool {
    let reach = EDGE_STEPS * step_ms + EPS_MS;
    let x = xs[i];
    intervals.iter().any(|&(lo, hi)| (x - lo).abs() <= reach || (hi - x).abs() <= reach)
        || xs.iter().zip(costs).any(|(&y, c)| c.is_nan() && (y - x).abs() <= reach)
}

/// G = lowest competitor cost / output valley cost. Both are finite and the output is not the higher. When the output
/// costs 0 (or less), G is +inf if the competitor costs more than 0 and 1 if it does not.
fn g_ratio(competitor: f64, output: f64) -> f64 {
    if output > 0.0 { competitor / output } else if competitor > 0.0 { f64::INFINITY } else { 1.0 }
}

/// `near_ms`: None without a near low point. Otherwise the position, relative to the output valley, of the output
/// valley's near twin: the lowest-cost near low point, or c1 when that near low point became the output valley.
fn near_twin_ms(near_lows: &[Sample], output: (Kind, Sample), c1: &Sample) -> Option<f64> {
    let lowest = near_lows.iter().reduce(|best, s| if s.cost < best.cost { s } else { best })?;
    let twin = if output.0 == Kind::Near { c1 } else { lowest };
    Some(twin.x - output.1.x)
}

fn search(p: &SearchParams, coarse: &(dyn Fn(f64) -> Option<f64> + Sync), full: &(dyn Fn(f64) -> Option<FullEval> + Sync),
          cancel: &AtomicBool) -> Option<SearchOutcome> {
    // Coarse scan
    let intervals = search_intervals(p.init_ms, p.search_ms, p.check_negative);
    let mut xs = Vec::new();
    let mut ranges = Vec::with_capacity(intervals.len());
    for interval in &intervals {
        let start = xs.len();
        xs.extend(grid(std::slice::from_ref(interval), p.step_ms));
        ranges.push(start..xs.len());
    }
    let costs: Vec<Option<f64>> = xs.par_iter().map(|&x| {
        if cancel.load(Ordering::Relaxed) { return None; }
        Some(coarse(x).filter(|c| c.is_finite()).unwrap_or(f64::NAN))
    }).collect();
    let costs: Vec<f64> = costs.into_iter().collect::<Option<_>>()?;
    let curve: Vec<(f64, f64)> = xs.iter().copied().zip(costs.iter().copied()).collect();
    let fail = |reason: FailReason, fine: Vec<(f64, f64)>| Some(SearchOutcome { coarse: curve.clone(), fine, ..failed(reason, p.init_ms) });

    let Some(c1_grid) = (0..xs.len()).filter(|&i| !costs[i].is_nan()).min_by(|&a, &b| costs[a].total_cmp(&costs[b])) else {
        return fail(FailReason::NoGyroOverlap, Vec::new());
    };
    if is_edge(&xs, &costs, &intervals, c1_grid, p.step_ms) { return fail(FailReason::Edge, Vec::new()); }

    // Far candidates: the lowest local minima at least MIN_VALLEY_SEP_MS from c1, else the lowest valid grid point there
    let apart = |i: usize| (xs[i] - xs[c1_grid]).abs() >= MIN_VALLEY_SEP_MS - EPS_MS;
    let mut far: Vec<usize> = local_minima(&costs, &ranges).into_iter().filter(|&i| apart(i)).collect();
    far.sort_by(|&a, &b| costs[a].total_cmp(&costs[b]).then(a.cmp(&b)));
    far.truncate(FAR_CANDIDATES);
    if far.is_empty() {
        far.extend((0..xs.len()).filter(|&i| !costs[i].is_nan() && apart(i)).min_by(|&a, &b| costs[a].total_cmp(&costs[b])));
    }

    let mut evals = FullEvals { full, cancel, log: Vec::new(), cancelled: false };
    let c1 = evals.refine(xs[c1_grid], C1_REFINE_MS)?;
    let mut far_valleys = Vec::with_capacity(far.len());
    for &i in &far {
        far_valleys.push(evals.lowest(xs[i], &FAR_PROBES_MS)?);
    }

    // Near check: refined c1 and the probes around it as one sequence at NEAR_STEP_MS spacing
    let mut probes = [c1; 2 * NEAR_PROBES + 1];
    for (k, probe) in probes.iter_mut().enumerate() {
        if k != NEAR_PROBES { *probe = evals.eval(c1.x + (k as f64 - NEAR_PROBES as f64) * NEAR_STEP_MS)?; }
    }
    let near_lows: Vec<Sample> = NEAR_LOW_STEPS.iter().rev().map(|&s| NEAR_PROBES - s).chain(NEAR_LOW_STEPS.iter().map(|&s| NEAR_PROBES + s))
        .filter(|&k| probes[k].measured() && probes[k].cost < probes[k - 1].cost && probes[k].cost < probes[k + 1].cost)
        .map(|k| probes[k])
        .collect();

    // Output valley: the lowest measured valley, c1 first on ties; the others are competitors
    let mut valleys: Vec<(Kind, Sample)> = std::iter::once((Kind::C1, c1))
        .chain(far_valleys.iter().map(|&s| (Kind::Far, s)))
        .chain(near_lows.iter().map(|&s| (Kind::Near, s)))
        .filter(|v| v.1.measured())
        .collect();
    // Nothing measured, not even at c1: the output valley's own evaluation is None
    let Some(out) = (0..valleys.len()).reduce(|best, i| if valleys[i].1.cost < valleys[best].1.cost { i } else { best }) else {
        return fail(FailReason::FewMeasurements, evals.log);
    };
    let half = match valleys[out].0 { Kind::C1 => None, Kind::Far => Some(FAR_REFINE_MS), Kind::Near => Some(NEAR_REFINE_MS) };
    if let Some(half) = half {
        let refined = evals.refine(valleys[out].1.x, half)?;
        if refined.cost < valleys[out].1.cost { valleys[out].1 = refined; }
    }
    let output = valleys.remove(out);
    let competitors = valleys;

    let Some(second) = competitors.iter().map(|v| v.1).reduce(|best, s| if s.cost < best.cost { s } else { best }) else {
        return fail(FailReason::NoContrast, evals.log);
    };
    let pairs = output.1.pairs;
    if pairs < MIN_PAIRS || (pairs as f64) < MIN_MEASURED_FRACTION * p.total_pairs as f64 {
        return fail(FailReason::FewMeasurements, evals.log);
    }
    let g = g_ratio(second.cost, output.1.cost);
    Some(SearchOutcome {
        offset_ms: output.1.x, cost_px: output.1.cost, conf: conf_from_g(g, p.g_full), g,
        second_ms: Some(second.x), far_candidates: far.len(), near_ms: near_twin_ms(&near_lows, output, &c1),
        fail: None, coarse: curve, fine: evals.log,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::SeqCst;
    use crate::synchronization::optical_motion::cost::{ eval_coarse, eval_full, select_tracks, CostContext, SgCache };
    use crate::synchronization::optical_motion::quat_table::QuatTable;
    use crate::synchronization::optical_motion::testutil::{ synth_window, SynthSpec };
    use crate::synchronization::optical_motion::tracks::row_time_ms;

    fn pool() -> rayon::ThreadPool { rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap() }
    fn params() -> SearchParams { SearchParams { init_ms: 0.0, search_ms: 5000.0, check_negative: false, step_ms: 10.0, g_full: 2.0, total_pairs: 90 } }
    /// Flat cost 1.0 with gaussian valleys (centre, depth, sigma)
    fn valleys(v: &'static [(f64, f64, f64)]) -> impl Fn(f64) -> f64 + Sync { move |d| 1.0 - v.iter().map(|(c, a, s)| a * (-((d - c) / s).powi(2)).exp()).sum::<f64>() }
    fn run(p: &SearchParams, f: impl Fn(f64) -> f64 + Sync) -> SearchOutcome {
        run_search(p, &|d| Some(f(d)), &|d| Some(FullEval { cost: f(d), pairs_measured: 90 }), &pool(), &AtomicBool::new(false)).unwrap()
    }
    #[test] fn finds_single_valley_with_full_confidence() {
        let o = run(&params(), valleys(&[(-703.4, 0.9, 12.0)]));
        assert!(o.fail.is_none() && (o.offset_ms + 703.4).abs() < 0.3 && o.conf == 1.0 && o.g > 5.0);
    }
    #[test] fn far_competitor_lowers_confidence() {
        let o = run(&params(), valleys(&[(-703.4, 0.5, 12.0), (1210.0, 0.4, 12.0)]));   // costs 0.5 vs 0.6 -> G = 1.2
        assert!((o.offset_ms + 703.4).abs() < 0.3 && (o.g - 1.2).abs() < 0.02 && o.conf < 0.4);
        assert!((o.second_ms.unwrap() - 1210.0).abs() <= 5.0);
    }
    #[test] fn near_twin_is_detected() {
        let o = run(&params(), valleys(&[(-700.0, 0.5, 6.0), (-680.0, 0.48, 6.0)]));     // 20 ms apart, costs 0.50 vs 0.52
        assert!(o.near_ms.is_some() && o.conf < 0.4);
    }
    #[test] fn flat_curve_has_no_confidence() { assert!(run(&params(), |_| 1.0).conf < 0.4); }
    #[test] fn valley_at_search_boundary_is_edge() {
        let o = run(&params(), valleys(&[(-4995.0, 0.9, 12.0)]));
        assert_eq!((o.fail, o.conf, o.offset_ms), (Some(FailReason::Edge), 0.0, 0.0));
    }
    #[test] fn valley_next_to_uncovered_region_is_edge() {
        let f = valleys(&[(-2010.0, 0.9, 12.0)]);
        let o = run_search(&params(), &|d| (d > -2025.0).then(|| f(d)), &|d| (d > -2025.0).then(|| FullEval { cost: f(d), pairs_measured: 90 }), &pool(), &AtomicBool::new(false)).unwrap();
        assert_eq!(o.fail, Some(FailReason::Edge));
    }
    #[test] fn all_uncovered_is_no_gyro_overlap() {
        let o = run_search(&params(), &|_| None, &|_| None, &pool(), &AtomicBool::new(false)).unwrap();
        assert_eq!(o.fail, Some(FailReason::NoGyroOverlap));
    }
    #[test] fn short_window_fails_before_scanning() {
        let o = run(&SearchParams { total_pairs: 19, ..params() }, |_| panic!("must not evaluate"));
        assert_eq!(o.fail, Some(FailReason::WindowTooShort));
    }
    #[test] fn too_few_measured_pairs_fails() {
        let f = valleys(&[(-703.4, 0.9, 12.0)]);
        let o = run_search(&params(), &|d| Some(f(d)), &|d| Some(FullEval { cost: f(d), pairs_measured: 19 }), &pool(), &AtomicBool::new(false)).unwrap();
        assert_eq!(o.fail, Some(FailReason::FewMeasurements));
    }
    #[test] fn tiny_search_range_is_no_contrast() {
        let o = run(&SearchParams { search_ms: 40.0, ..params() }, valleys(&[(3.0, 0.9, 12.0)]));
        assert!(matches!(o.fail, Some(FailReason::NoContrast) | Some(FailReason::Edge)));
    }
    #[test] fn intervals_merge_when_overlapping() {
        assert_eq!(search_intervals(100.0, 5000.0, true), vec![(-5100.0, 5100.0)]);
        assert_eq!(search_intervals(8000.0, 5000.0, true), vec![(-13000.0, -3000.0), (3000.0, 13000.0)]);
        assert_eq!(search_intervals(0.5, 5000.0, true), vec![(-4999.5, 5000.5)]);          // |init| <= 1 ms: no second interval
        let g = grid(&search_intervals(100.0, 5000.0, true), 10.0);
        assert_eq!(g.len(), 1021);
        assert!(g.windows(2).all(|w| w[1] > w[0]));
    }
    #[test] fn negative_initial_offset_side_is_searched() {
        let o = run(&SearchParams { init_ms: 8000.0, check_negative: true, ..params() }, valleys(&[(-8300.0, 0.9, 12.0)]));
        assert!((o.offset_ms + 8300.0).abs() < 0.3 && o.conf == 1.0);
    }
    #[test] fn brent_converges_within_budget() {
        let mut n = 0;
        let (x, _) = brent_min(&mut |x| { n += 1; (x - 3.37).powi(2) }, -10.0, 10.0, 0.2, 10);
        assert!((x - 3.37).abs() < 0.2 && n <= 10);
    }
    #[test] fn cancel_stops_search() {
        let cancel = AtomicBool::new(true);
        let p = SearchParams { search_ms: 60000.0, ..params() };
        assert!(run_search(&p, &|_| Some(1.0), &|_| Some(FullEval { cost: 1.0, pairs_measured: 90 }), &pool(), &cancel).is_none());
    }

    #[test] fn brent_copes_with_unmeasured_points() {
        let mut n = 0;
        let (x, fx) = brent_min(&mut |x| { n += 1; if x < 0.0 { f64::INFINITY } else { (x - 3.37).powi(2) } }, -10.0, 10.0, 0.2, 10);
        assert!((x - 3.37).abs() < 0.2 && fx.is_finite() && n <= 10, "x {x} f {fx} evals {n}");
        let (x, fx) = brent_min(&mut |_| f64::NAN, -10.0, 10.0, 0.2, 10);
        assert!((-10.0..=10.0).contains(&x) && fx == f64::INFINITY);
    }
    #[test] fn local_minimum_rule_for_plateaus_runs_and_intervals() {
        let nan = f64::NAN;
        // Interval 1: run 3,1,1,2 (plateau minimum at 1), an invalid point, run 0.5,2,2,1,1 (both ends); interval 2: one plateau
        let costs = [3.0, 1.0, 1.0, 2.0, nan, 0.5, 2.0, 2.0, 1.0, 1.0, 4.0, 4.0];
        assert_eq!(local_minima(&costs, &[0..10, 10..12]), vec![1, 5, 8, 10]);
        // A plateau that goes on down is not a minimum
        assert_eq!(local_minima(&[3.0, 1.0, 1.0, 0.0], std::slice::from_ref(&(0..4))), vec![3]);
    }
    #[test] fn full_cost_can_overrule_the_coarse_ranking() {
        // The coarse curve ranks -703.4 first, the full cost 1210: the far candidate is the output, c1 a competitor
        let coarse = valleys(&[(-703.4, 0.5, 12.0), (1210.0, 0.4, 12.0)]);
        let full = valleys(&[(-703.4, 0.4, 12.0), (1210.0, 0.5, 12.0)]);
        let o = run_search(&params(), &|d| Some(coarse(d)), &|d| Some(FullEval { cost: full(d), pairs_measured: 90 }), &pool(), &AtomicBool::new(false)).unwrap();
        assert!(o.fail.is_none() && (o.offset_ms - 1210.0).abs() < 0.3 && (o.g - 1.2).abs() < 0.02, "offset {} G {}", o.offset_ms, o.g);
        assert!((o.second_ms.unwrap() + 703.4).abs() < 0.3);
    }
    #[test] fn near_low_point_can_become_the_output() {
        // Only the full cost has the twin 20 ms away, and it is the lower one: c1 is then the near twin
        let coarse = valleys(&[(-700.0, 0.5, 6.0)]);
        let full = valleys(&[(-700.0, 0.48, 6.0), (-680.0, 0.5, 6.0)]);
        let o = run_search(&params(), &|d| Some(coarse(d)), &|d| Some(FullEval { cost: full(d), pairs_measured: 90 }), &pool(), &AtomicBool::new(false)).unwrap();
        assert!(o.fail.is_none() && (o.offset_ms + 680.0).abs() < 0.3 && o.conf < 0.4, "offset {} conf {}", o.offset_ms, o.conf);
        assert!((o.near_ms.unwrap() + 20.0).abs() < 0.5 && (o.second_ms.unwrap() + 700.0).abs() < 0.5);
    }
    #[test] fn unmeasured_output_is_few_measurements_and_curves_stay() {
        // Covered, but no full evaluation measured anything
        let f = valleys(&[(-703.4, 0.9, 12.0)]);
        let o = run_search(&params(), &|d| Some(f(d)), &|_| None, &pool(), &AtomicBool::new(false)).unwrap();
        assert_eq!((o.fail, o.offset_ms, o.cost_px, o.conf), (Some(FailReason::FewMeasurements), 0.0, 0.0, 0.0));
        assert!(o.coarse.len() == 1001 && !o.fine.is_empty() && o.fine.iter().all(|c| c.1.is_nan()));
        let o = run(&params(), valleys(&[(-4995.0, 0.9, 12.0)]));
        assert!(o.fail == Some(FailReason::Edge) && o.coarse.len() == 1001 && o.fine.is_empty());
    }
    #[test] fn cancel_during_refinement_stops_search() {
        let cancel = AtomicBool::new(false);
        let calls = AtomicUsize::new(0);
        let f = valleys(&[(-703.4, 0.9, 12.0)]);
        let full = |d: f64| {
            calls.fetch_add(1, SeqCst);
            cancel.store(true, SeqCst);
            Some(FullEval { cost: f(d), pairs_measured: 90 })
        };
        assert!(run_search(&params(), &|d| Some(f(d)), &full, &pool(), &cancel).is_none());
        assert_eq!(calls.load(SeqCst), 1);
    }
    #[test] fn confidence_mapping_and_failure_rows() {
        assert!((conf_from_g(1.2, 2.0) - 0.2).abs() < 1e-12);
        assert_eq!([0.9, 3.0, f64::INFINITY, f64::NAN].map(|g| conf_from_g(g, 2.0)), [0.0, 1.0, 1.0, 0.0]);
        assert_eq!((g_ratio(1.0, 0.5), g_ratio(1.0, 0.0), g_ratio(0.0, 0.0)), (2.0, f64::INFINITY, 1.0));
        use FailReason::*;
        let names = [WindowTooShort, Edge, FewMeasurements, NoGyroOverlap, NoContrast, NoOpencv].map(FailReason::as_str);
        assert_eq!(names, ["window_too_short", "edge", "few_measurements", "no_gyro_overlap", "no_contrast", "no_opencv"]);
        let f = failed(NoOpencv, 12.5);
        assert_eq!((f.offset_ms, f.cost_px, f.conf, f.g, f.fail, f.second_ms, f.near_ms), (12.5, 0.0, 0.0, 1.0, Some(NoOpencv), None, None));
    }

    /// The search on a synthetic window, set up the way the caller sets it up: the quaternion table covers the window's
    /// row-time span minus the search intervals with 50 ms margins, the coarse scan takes 200 points per pair
    /// (COARSE_POINTS default) and 5 IRLS rounds, and a covered offset where no band could be fitted is unmeasured.
    /// Search range ±1 s (10 ms steps) on a pool of all cores, to keep the debug-profile coarse scan short.
    fn search_synthetic(spec: &SynthSpec) -> SearchOutcome {
        let (w, quats) = synth_window(spec);
        let p = SearchParams { search_ms: 1000.0, total_pairs: w.pairs.len(), ..params() };
        let (lo, hi) = w.pairs.iter()
            .flat_map(|pd| pd.fa.iter().map(|&f| row_time_ms(&pd.a, f)).chain(pd.fb.iter().map(|&f| row_time_ms(&pd.b, f))))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| (lo.min(t), hi.max(t)));
        let intervals = search_intervals(p.init_ms, p.search_ms, p.check_negative);
        let (first, last) = (intervals[0].0, intervals[intervals.len() - 1].1);
        // Gyro time = video time - offset
        let table = QuatTable::build(&quats, lo - last - 50.0, hi - first + 50.0);
        let sg = SgCache::new();
        let ctx = CostContext { window: &w, quats: &table, sg: &sg };
        let subset = select_tracks(&w, 200);
        let coarse = |d: f64| eval_coarse(&ctx, &subset, d, 5);
        let full = |d: f64| eval_full(&ctx, d).filter(|r| r.bands > 0).map(|r| FullEval { cost: r.cost_px, pairs_measured: r.pairs_measured });
        let all_cores = rayon::ThreadPoolBuilder::new().build().unwrap();
        run_search(&p, &coarse, &full, &all_cores, &AtomicBool::new(false)).unwrap()
    }
    #[test] fn synthetic_window_end_to_end() {
        let spec = SynthSpec::default();
        let o = search_synthetic(&spec);
        assert!(o.fail.is_none() && (o.offset_ms - spec.true_offset_ms).abs() < 1.0 && o.conf >= 0.9,
            "offset {} conf {} G {} fail {:?}", o.offset_ms, o.conf, o.g, o.fail);
        // Gentle low-frequency motion only: what the high-pass leaves is below the tracking noise at every offset
        let o = search_synthetic(&SynthSpec { freq_hz: (0.05, 0.15), amp_dps: 2.0, noise_px: 0.5, ..Default::default() });
        assert!(o.conf < 0.4, "offset {} conf {} G {} fail {:?}", o.offset_ms, o.conf, o.g, o.fail);
    }

    /// Release benchmark against the spec §7 budget: coarse scan <= 1500 ms, refinement <= 1000 ms.
    /// Run: just test-core "--release optical_motion::search::tests::bench_synthetic_window -- --ignored --nocapture"
    #[test] #[ignore]
    fn bench_synthetic_window() {
        use std::time::Instant;
        let spec = SynthSpec { fps: 60.0, duration_ms: 1500.0, tracks: 1200, ..Default::default() };
        let (w, quats) = synth_window(&spec);
        let p = SearchParams { search_ms: 5000.0, total_pairs: w.pairs.len(), ..params() };
        let (lo, hi) = w.pairs.iter()
            .flat_map(|pd| pd.fa.iter().map(|&f| row_time_ms(&pd.a, f)).chain(pd.fb.iter().map(|&f| row_time_ms(&pd.b, f))))
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| (lo.min(t), hi.max(t)));
        let intervals = search_intervals(p.init_ms, p.search_ms, p.check_negative);
        let (first, last) = (intervals[0].0, intervals[intervals.len() - 1].1);
        let table = QuatTable::build(&quats, lo - last - 50.0, hi - first + 50.0);
        let sg = SgCache::new();
        let ctx = CostContext { window: &w, quats: &table, sg: &sg };
        let coarse_points = crate::synchronization::optical_motion::config::config().coarse_points;
        let subset = select_tracks(&w, coarse_points);
        let coarse_calls = AtomicUsize::new(0);
        let full_calls = AtomicUsize::new(0);
        let full_ns = AtomicUsize::new(0);
        let coarse = |d: f64| { coarse_calls.fetch_add(1, SeqCst); eval_coarse(&ctx, &subset, d, 5) };
        // Full evaluations run one after another at search level, so their summed wall time is the refinement time
        let full = |d: f64| {
            let t = Instant::now();
            let r = eval_full(&ctx, d).filter(|r| r.bands > 0).map(|r| FullEval { cost: r.cost_px, pairs_measured: r.pairs_measured });
            full_ns.fetch_add(t.elapsed().as_nanos() as usize, SeqCst);
            full_calls.fetch_add(1, SeqCst);
            r
        };
        let pool = rayon::ThreadPoolBuilder::new().build().unwrap();
        let cancel = AtomicBool::new(false);

        // One cold run_search, measured from inside: refinement = sum of full-evaluation wall time, coarse = the rest
        let t = Instant::now();
        let o = run_search(&p, &coarse, &full, &pool, &cancel).unwrap();
        let total_ms = t.elapsed().as_secs_f64() * 1000.0;
        let refine_ms = full_ns.load(SeqCst) as f64 / 1e6;
        let coarse_ms = total_ms - refine_ms;
        let n_full = full_calls.load(SeqCst);

        println!("cores {}", pool.current_num_threads());
        println!("pairs {}, pts/pair {}, coarse_pts/pair {}", w.pairs.len(), w.pairs[0].ids.len(), subset.per_pair[0].len());
        println!("in-search coarse {coarse_ms:.0} ms ({} evals), refinement {refine_ms:.0} ms ({n_full} full evals, {:.1} ms each), run_search total {total_ms:.0} ms", coarse_calls.load(SeqCst), refine_ms / n_full as f64);
        println!("result offset {:.2} conf {:.2} fail {:?}", o.offset_ms, o.conf, o.fail);
        println!("budget: coarse <= 1500 ms: {}, refinement <= 1000 ms: {}", coarse_ms <= 1500.0, refine_ms <= 1000.0);
    }
}
