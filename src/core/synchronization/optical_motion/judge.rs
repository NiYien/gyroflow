// SPDX-License-Identifier: GPL-3.0-or-later

//! Optical measurements and pure decisions for one deep-match chunk.

use std::{ cell::RefCell, sync::atomic::{ AtomicBool, Ordering::Relaxed }, time::Instant };

use rayon::prelude::*;

use crate::gyro_source::TimeQuat;
use crate::synchronization::{ deep_match, sync_diag };
use deep_match::{ DeepMatchVerdict, DeepMatchWindowCurve };
use super::{ interval_tables, row_time_span_ms, table_for, COARSE_IRLS_ROUNDS };
use super::cost::{ eval_coarse, eval_full, select_tracks, CostContext, SgCache };
use super::search::{ brent_min, grid, is_edge, local_minima, FailReason, MIN_MEASURED_FRACTION, MIN_PAIRS };
use super::tracks::WindowTracks;

const REFINE_HALF_MS: f64 = 10.0;
const REFINE_TOL_MS: f64 = 1.0;
const REFINE_MAX_EVALS: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JudgeMode { On, Shadow }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Valley { pub ms: f64, pub cost: f64 }

#[derive(Clone, Debug, PartialEq)]
pub enum WindowOutcome {
    Unmeasured { reason: FailReason },
    Measured { best: Option<Valley>, g: Option<f64>, second_ms: Option<f64>, at_x: Option<Valley> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct WindowJudge { pub range_us: (i64, i64), pub outcome: WindowOutcome }

#[derive(Clone, Debug, PartialEq)]
pub enum JudgeVerdict {
    Found { offset_ms: f64, windows: Vec<usize> },
    NoWinner { support_ms: Option<f64> },
    CannotJudge { measured: usize, reason: &'static str },
}

#[derive(Clone, Debug, PartialEq)]
pub struct JudgeOutcome { pub mode: JudgeMode, pub verdict: JudgeVerdict }

#[derive(Clone, Copy, Debug)]
pub struct DecideParams { pub t_d_ms: f64, pub radius_ms: f64, pub g_strong: f64, pub support_ratio: f64 }

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IntervalStat { pub lo: f64, pub hi: f64, pub min_ms: f64, pub min_cost: f64, pub interior: bool }

pub struct HandedTracks {
    pub windows: Vec<WindowTracks>,
    pub ranges_us: Vec<(i64, i64)>,
    pub has_tracker: bool,
    pub every_nth: usize,
    pub frames_fed: usize,
    pub track_ms: f64,
}

thread_local! {
    static HANDED_TRACKS: RefCell<Option<HandedTracks>> = const { RefCell::new(None) };
}

/// Keep the handoff on the synchronous caller's thread, including unwinding.
pub fn with_handed_tracks<R>(tracks: Option<HandedTracks>, f: impl FnOnce() -> R) -> R {
    struct ClearHandoff;
    impl Drop for ClearHandoff {
        fn drop(&mut self) { HANDED_TRACKS.with(|slot| { slot.borrow_mut().take(); }); }
    }
    HANDED_TRACKS.with(|slot| { *slot.borrow_mut() = tracks; });
    let _guard = ClearHandoff;
    f()
}

pub fn take_handed_tracks() -> Option<HandedTracks> {
    HANDED_TRACKS.with(|slot| slot.borrow_mut().take())
}

/// Reuse one smoothing cache across all windows in the chunk.
#[allow(clippy::too_many_arguments)]
pub fn judge_candidates(tracks: &HandedTracks, quats: &TimeQuat, candidates: &[f64], posterior_x: Option<f64>,
                        p: &DecideParams, coarse_points: usize, cancel: &AtomicBool, progress: &(dyn Fn(f64) + Sync))
                        -> Option<(JudgeVerdict, Vec<WindowJudge>, Vec<Vec<IntervalStat>>)> {
    if cancel.load(Relaxed) { return None; }
    let intervals = candidate_intervals(candidates, posterior_x, p.radius_ms);
    let sg = SgCache::new();
    let mut windows = Vec::with_capacity(tracks.windows.len());
    let mut stats = Vec::with_capacity(tracks.windows.len());
    for (i, w) in tracks.windows.iter().enumerate() {
        let (outcome, interval_stats) = judge_window(w, quats, &intervals, posterior_x, coarse_points,
            super::config::config().coarse_step_ms, &sg, cancel)?;
        windows.push(WindowJudge { range_us: tracks.ranges_us[i], outcome });
        stats.push(interval_stats);
        progress((i + 1) as f64 / tracks.windows.len() as f64);
    }
    if cancel.load(Relaxed) { return None; }
    Some((decide_chunk(&windows, posterior_x, p), windows, stats))
}

fn log_verdict(verdict: &JudgeVerdict, mode: JudgeMode, track_ms: f64, judge_ms: f64) {
    let mode = match mode { JudgeMode::On => "on", JudgeMode::Shadow => "shadow" };
    let suffix = format!("mode={mode} track_ms={track_ms:.0} judge_ms={judge_ms:.0}");
    match verdict {
        JudgeVerdict::Found { offset_ms, windows } => log::info!(target: "sync",
            "[deep-match] optical judge verdict=found offset={offset_ms:.1}ms agree={windows:?} {suffix}"),
        JudgeVerdict::NoWinner { support_ms } => {
            let support = support_ms.map_or_else(|| "none".to_owned(), |s| format!("{s:.1}ms"));
            log::info!(target: "sync", "[deep-match] optical judge verdict=no_winner support={support} {suffix}");
        }
        JudgeVerdict::CannotJudge { measured, reason } => log::info!(target: "sync",
            "[deep-match] optical judge verdict=cannot_judge measured={measured} reason={reason} {suffix}"),
    }
}

pub fn run_chunk_judge(curves: &[DeepMatchWindowCurve], scaled_duration_ms: f64, quats: &TimeQuat,
                       mode: JudgeMode, cancel: &AtomicBool, progress: &(dyn Fn(f64) + Sync)) -> Option<JudgeOutcome> {
    let tracks = take_handed_tracks();
    if cancel.load(Relaxed) { return None; }
    let start = Instant::now();
    let cannot = |reason, track_ms| {
        let verdict = JudgeVerdict::CannotJudge { measured: 0, reason };
        log_verdict(&verdict, mode, track_ms, start.elapsed().as_secs_f64() * 1000.0);
        Some(JudgeOutcome { mode, verdict })
    };
    let Some(tracks) = tracks else { return cannot("no_tracks", 0.0); };
    if !tracks.has_tracker { return cannot("no_opencv", tracks.track_ms); }
    if curves.len() < 2 { return cannot("too_few_curves", tracks.track_ms); }
    let rate = deep_match::drift_rate_ms_per_min();
    let floor = deep_match::drift_floor_ms();
    let t_d_ms = deep_match::drift_tolerance_ms(scaled_duration_ms, rate, floor);
    let posterior_x = match deep_match::decide_posterior(curves, curves.len(), 0, scaled_duration_ms,
        deep_match::post_conf_min(), deep_match::post_ci95_base_ms(), rate, floor) {
        DeepMatchVerdict::Accepted { offset_ms } => Some(offset_ms),
        _ => None,
    };
    let candidates = deep_match::forward_candidates(curves, deep_match::fwd_lattice_ms(), deep_match::fwd_nms_ms(), deep_match::optical_top_n());
    let p = DecideParams { t_d_ms, radius_ms: deep_match::optical_radius_ms() + t_d_ms / 2.0,
        g_strong: deep_match::optical_g_strong(), support_ratio: deep_match::optical_support_ratio() };
    let (verdict, windows, stats) = judge_candidates(&tracks, quats, &candidates, posterior_x, &p,
        deep_match::optical_coarse_points(), cancel, progress)?;
    if cancel.load(Relaxed) { return None; }
    let judge_ms = start.elapsed().as_secs_f64() * 1000.0;
    let posterior = posterior_x.map_or_else(|| "none".to_owned(), |x| format!("accepted@{x:.1}ms"));
    let cands = format!("[{}]", candidates.iter().map(|x| format!("{x:.0}")).collect::<Vec<_>>().join(", "));
    log::info!(target: "sync", "[deep-match] optical judge: windows={} candidates={} radius=±{:.0}ms t_d={:.1}ms posterior={} every_nth={} quats={} frames_fed={} cands={}",
        tracks.windows.len(), candidates.len(), p.radius_ms, t_d_ms, posterior, tracks.every_nth, quats.len(), tracks.frames_fed, cands);
    for (i, window) in windows.iter().enumerate() {
        match window.outcome {
            WindowOutcome::Unmeasured { reason } => log::info!(target: "sync",
                "[deep-match] optical judge win {i}: unmeasured reason={}", reason.as_str()),
            WindowOutcome::Measured { best: None, .. } => log::info!(target: "sync",
                "[deep-match] optical judge win {i}: measured best=- (no interior valley)"),
            WindowOutcome::Measured { best: Some(best), g, second_ms, at_x } => {
                let second = second_ms.map_or_else(|| "-".to_owned(), |x| format!("{x:.1}ms"));
                let strong = if g.is_some_and(|g| g >= p.g_strong) { "yes" } else { "no" };
                let at_x = at_x.map_or_else(|| "-".to_owned(), |v| format!("{:.1}ms/{:.3}px", v.ms, v.cost));
                log::info!(target: "sync", "[deep-match] optical judge win {i}: measured best={:.1}ms cost={:.3}px G={:.3} second={} strong={} at_x={}",
                    best.ms, best.cost, g.unwrap_or(f64::NAN), second, strong, at_x);
            }
        }
        sync_diag::record_deep_match_optical(i, &stats[i]);
    }
    log_verdict(&verdict, mode, tracks.track_ms, judge_ms);
    Some(JudgeOutcome { mode, verdict })
}

/// Refine with full observations, keeping the measured-pair count of Brent's chosen evaluation.
fn refine_valley(ctx: &CostContext, center_ms: f64, cancel: &AtomicBool) -> Option<(Valley, usize)> {
    let mut best = (Valley { ms: center_ms, cost: f64::INFINITY }, 0);
    brent_min(&mut |d| {
        if cancel.load(Relaxed) { return f64::INFINITY; }
        let sample = eval_full(ctx, d).filter(|r| r.bands > 0 && r.cost_px.is_finite());
        let cost = sample.map_or(f64::INFINITY, |r| r.cost_px);
        if cost <= best.0.cost {
            best = (Valley { ms: d, cost }, sample.map_or(0, |r| r.pairs_measured));
        }
        cost
    }, center_ms - REFINE_HALF_MS, center_ms + REFINE_HALF_MS, REFINE_TOL_MS, REFINE_MAX_EVALS);
    if cancel.load(Relaxed) { None } else { Some(best) }
}

/// Judge candidate intervals within one window, on the caller's current Rayon pool.
pub fn judge_window(w: &WindowTracks, quats: &TimeQuat, intervals: &[(f64, f64)], x: Option<f64>,
                    coarse_points: usize, step_ms: f64, sg: &SgCache, cancel: &AtomicBool)
                    -> Option<(WindowOutcome, Vec<IntervalStat>)> {
    if cancel.load(Relaxed) { return None; }
    let mut stats: Vec<IntervalStat> = intervals.iter().map(|&(lo, hi)| IntervalStat {
        lo, hi, min_ms: f64::NAN, min_cost: f64::NAN, interior: false,
    }).collect();
    let unmeasured = |reason| WindowOutcome::Unmeasured { reason };
    if w.pairs.len() < MIN_PAIRS { return Some((unmeasured(FailReason::WindowTooShort), stats)); }
    let Some((lo, hi)) = row_time_span_ms(w) else { return Some((unmeasured(FailReason::FewMeasurements), stats)); };
    let tables = interval_tables(quats, (lo, hi), intervals);
    let ctxs: Vec<CostContext> = tables.iter().map(|quats| CostContext { window: w, quats, sg }).collect();
    let subset = select_tracks(w, coarse_points);
    let mut xs = Vec::new();
    let mut ranges = Vec::with_capacity(intervals.len());
    for interval in intervals {
        let start = xs.len();
        xs.extend(grid(std::slice::from_ref(interval), step_ms));
        ranges.push(start..xs.len());
    }
    let costs: Option<Vec<f64>> = xs.par_iter().map(|&d| {
        if cancel.load(Relaxed) { return None; }
        Some(eval_coarse(&ctxs[table_for(intervals, d)], &subset, d, COARSE_IRLS_ROUNDS)
            .filter(|c| c.is_finite()).unwrap_or(f64::NAN))
    }).collect();
    let costs = costs?;
    if cancel.load(Relaxed) { return None; }
    if costs.iter().all(|c| c.is_nan()) {
        let covered = ranges.iter().enumerate().any(|(i, range)| range.clone().any(|k| tables[i].covers(lo - xs[k], hi - xs[k])));
        let reason = if covered { FailReason::FewMeasurements } else { FailReason::NoGyroOverlap };
        return Some((unmeasured(reason), stats));
    }

    let minima = local_minima(&costs, &ranges);
    let mut levels = Vec::with_capacity(intervals.len());
    let mut valleys = Vec::with_capacity(intervals.len());
    for (i, range) in ranges.iter().enumerate() {
        let level = range.clone().filter(|&k| costs[k].is_finite()).min_by(|&a, &b| costs[a].total_cmp(&costs[b]));
        let interior = |k: usize| !is_edge(&xs[range.clone()], &costs[range.clone()],
            std::slice::from_ref(&intervals[i]), k - range.start, step_ms);
        let valley = minima.iter().copied().filter(|k| range.contains(k) && interior(*k))
            .min_by(|&a, &b| costs[a].total_cmp(&costs[b]));
        if let Some(k) = level {
            stats[i].min_ms = xs[k];
            stats[i].min_cost = costs[k];
            stats[i].interior = minima.contains(&k) && interior(k);
        }
        levels.push(level);
        valleys.push(valley);
    }
    let Some((best_interval, best_grid)) = valleys.iter().enumerate().filter_map(|(i, &k)| k.map(|k| (i, k)))
        .min_by(|a, b| costs[a.1].total_cmp(&costs[b.1])) else {
        return Some((WindowOutcome::Measured { best: None, g: None, second_ms: None, at_x: None }, stats));
    };
    let (best, pairs) = refine_valley(&ctxs[best_interval], xs[best_grid], cancel)?;
    if !best.cost.is_finite() || pairs < MIN_PAIRS || (pairs as f64) < MIN_MEASURED_FRACTION * w.pairs.len() as f64 {
        return Some((unmeasured(FailReason::FewMeasurements), stats));
    }
    // All valid coarse points in other intervals compete, including boundary minima and interval levels.
    let second_grid = levels.iter().enumerate().filter(|&(i, _)| i != best_interval)
        .filter_map(|(i, &k)| k.map(|k| (i, k))).min_by(|a, b| costs[a.1].total_cmp(&costs[b.1]));
    let second = match second_grid {
        Some((i, k)) => Some((i, refine_valley(&ctxs[i], xs[k], cancel)?.0)),
        None => None,
    };
    let measured_second = second.filter(|(_, v)| v.cost.is_finite());
    let g = measured_second.map(|(_, v)| {
        if best.cost > 0.0 { v.cost / best.cost } else if v.cost > 0.0 { f64::INFINITY } else { 1.0 }
    });
    let at_x = match x.and_then(|x| intervals.iter().position(|&(lo, hi)| lo <= x && x <= hi)) {
        Some(i) if i == best_interval => Some(best),
        Some(i) if second.is_some_and(|(j, _)| j == i) => second.map(|(_, v)| v).filter(|v| v.cost.is_finite()),
        Some(i) => match levels[i] {
            Some(k) => Some(refine_valley(&ctxs[i], xs[k], cancel)?.0).filter(|v| v.cost.is_finite()),
            None => None,
        },
        None => None,
    };
    if cancel.load(Relaxed) { return None; }
    Some((WindowOutcome::Measured { best: Some(best), g, second_ms: measured_second.map(|(_, v)| v.ms), at_x }, stats))
}

/// Candidate neighbourhoods, with a separate posterior neighbourhood when needed.
pub fn candidate_intervals(candidates: &[f64], posterior_x: Option<f64>, radius_ms: f64) -> Vec<(f64, f64)> {
    let mut centers: Vec<f64> = candidates.iter().copied().filter(|x| x.is_finite()).collect();
    if let Some(x) = posterior_x.filter(|x| x.is_finite()) {
        if centers.iter().all(|c| (c - x).abs() > radius_ms) {
            centers.push(x);
        }
    }
    centers.sort_by(f64::total_cmp);
    let mut intervals: Vec<(f64, f64)> = Vec::new();
    for center in centers {
        let (lo, hi) = (center - radius_ms, center + radius_ms);
        if let Some(last) = intervals.last_mut() {
            if lo <= last.1 {
                last.1 = last.1.max(hi);
                continue;
            }
        }
        intervals.push((lo, hi));
    }
    intervals
}

/// A pair is independent when at most half of the shorter window overlaps.
pub fn windows_independent(a: (i64, i64), b: (i64, i64)) -> bool {
    let shorter = (i128::from(a.1) - i128::from(a.0)).min(i128::from(b.1) - i128::from(b.0));
    if shorter <= 0 { return false; }
    let overlap = (i128::from(a.1.min(b.1)) - i128::from(a.0.max(b.0))).max(0);
    overlap as f64 <= shorter as f64 / 2.0
}

pub fn judge_every_nth(scaled_fps: f64, target_fps: f64) -> usize {
    if !scaled_fps.is_finite() || scaled_fps <= 0.0 || !target_fps.is_finite() || target_fps <= 0.0 { return 1; }
    (scaled_fps / target_fps).round().max(1.0) as usize
}

fn has_independent_pair(windows: &[WindowJudge], indices: &[usize]) -> bool {
    indices.iter().enumerate().any(|(i, &a)| indices[i + 1..].iter()
        .any(|&b| windows_independent(windows[a].range_us, windows[b].range_us)))
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    let middle = values.len() / 2;
    if values.len() % 2 == 0 { (values[middle - 1] + values[middle]) / 2.0 } else { values[middle] }
}

/// Strong windows must all agree; weaker windows can only support the posterior.
pub fn decide_chunk(windows: &[WindowJudge], posterior_x: Option<f64>, p: &DecideParams) -> JudgeVerdict {
    let measured: Vec<usize> = windows.iter().enumerate()
        .filter_map(|(i, w)| matches!(w.outcome, WindowOutcome::Measured { .. }).then_some(i)).collect();
    if !has_independent_pair(windows, &measured) {
        return JudgeVerdict::CannotJudge { measured: measured.len(), reason: "too_few_measured" };
    }
    let strong: Vec<(usize, Valley)> = windows.iter().enumerate().filter_map(|(i, w)| match w.outcome {
        WindowOutcome::Measured { best: Some(best), g: Some(g), .. } if g >= p.g_strong => Some((i, best)),
        _ => None,
    }).collect();
    let strong_indices: Vec<usize> = strong.iter().map(|&(i, _)| i).collect();
    if has_independent_pair(windows, &strong_indices) {
        let positions: Vec<f64> = strong.iter().map(|&(_, best)| best.ms).collect();
        let lo = positions.iter().copied().fold(f64::INFINITY, f64::min);
        let hi = positions.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if hi - lo <= p.t_d_ms {
            return JudgeVerdict::Found { offset_ms: median(positions), windows: strong_indices };
        }
    }
    let support_ms = posterior_x.and_then(|x| {
        if strong.iter().any(|&(_, best)| (best.ms - x).abs() > p.radius_ms) { return None; }
        let supporters: Vec<(usize, f64)> = windows.iter().enumerate().filter_map(|(i, w)| match w.outcome {
            WindowOutcome::Measured { best: Some(best), at_x: Some(at_x), .. }
                if at_x.cost <= p.support_ratio * best.cost => Some((i, at_x.ms)),
            _ => None,
        }).collect();
        let indices: Vec<usize> = supporters.iter().map(|&(i, _)| i).collect();
        has_independent_pair(windows, &indices).then(|| median(supporters.iter().map(|&(_, ms)| ms).collect()))
    });
    JudgeVerdict::NoWinner { support_ms }
}

pub fn is_decisive(outcome: Option<&JudgeOutcome>) -> bool {
    matches!(outcome, Some(JudgeOutcome { mode: JudgeMode::On,
        verdict: JudgeVerdict::Found { .. } | JudgeVerdict::NoWinner { .. } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::search::FailReason;
    use super::super::testutil::{ synth_windows, SynthSpec };

    fn window_fixture() -> (Vec<WindowTracks>, TimeQuat, Vec<(f64, f64)>) {
        let spec = SynthSpec { fps: 30.0, duration_ms: 2000.0, tracks: 200, true_offset_ms: -700.0, ..Default::default() };
        let (ws, quats) = synth_windows(&spec, &[5000.0]);
        let intervals = candidate_intervals(&[-3700.0, -2700.0, -1700.0, -740.0, 300.0, 1300.0, 2300.0, 3300.0], None, 105.0);
        (ws, quats, intervals)
    }
    #[test] fn judge_window_picks_the_truth_interval() {
        let (ws, quats, intervals) = window_fixture();
        let (out, stats) = judge_window(&ws[0], &quats, &intervals, None, 200, 10.0, &SgCache::new(), &AtomicBool::new(false)).unwrap();
        let WindowOutcome::Measured { best: Some(best), g: Some(g), .. } = out else { panic!("{out:?}"); };
        assert!((best.ms + 700.0).abs() <= 3.0 && g >= 1.4, "best {best:?} G {g}");
        assert!(stats.iter().find(|s| s.lo == -845.0).unwrap().interior);
    }
    #[test] fn judge_window_edge_minimum_is_not_a_valley() {
        let (ws, quats, _) = window_fixture();
        let (out, _) = judge_window(&ws[0], &quats, &[(-680.0, -470.0)], None, 200, 10.0, &SgCache::new(), &AtomicBool::new(false)).unwrap();
        assert!(matches!(out, WindowOutcome::Measured { .. }), "{out:?}");
        if let WindowOutcome::Measured { best: Some(best), .. } = out {
            assert!(best.ms - -680.0 > 20.0 && -470.0 - best.ms > 20.0, "{best:?}");
        }
    }
    #[test] fn judge_window_partial_coverage_still_finds_truth() {
        let (ws, quats, mut intervals) = window_fixture();
        intervals.push((-13100.0, -12800.0));
        let sg = SgCache::new();
        let (out, stats) = judge_window(&ws[0], &quats, &intervals, None, 200, 10.0, &sg, &AtomicBool::new(false)).unwrap();
        let WindowOutcome::Measured { best: Some(best), .. } = out else { panic!("{out:?}"); };
        assert!((best.ms + 700.0).abs() <= 3.0, "{best:?}");
        let stat = stats.last().unwrap();
        let table = super::super::interval_tables(&quats, super::super::row_time_span_ms(&ws[0]).unwrap(), &[(-13100.0, -12800.0)]);
        let ctx = CostContext { window: &ws[0], quats: &table[0], sg: &sg };
        let subset = select_tracks(&ws[0], 200);
        let invalid: Vec<f64> = super::super::search::grid(&[(-13100.0, -12800.0)], 10.0).into_iter()
            .filter(|&d| eval_coarse(&ctx, &subset, d, super::super::COARSE_IRLS_ROUNDS).is_none_or(|c| !c.is_finite())).collect();
        assert!(!invalid.is_empty());
        if stat.min_ms.is_finite() && invalid.iter().any(|d| (stat.min_ms - d).abs() <= 20.0) {
            assert!(!stat.interior, "{stat:?}");
        }
    }
    #[test] fn judge_window_too_few_pairs_is_unmeasured() {
        let (ws, quats) = synth_windows(&SynthSpec { fps: 30.0, duration_ms: 500.0, tracks: 200, ..Default::default() }, &[5000.0]);
        assert_eq!(ws[0].pairs.len(), 14);
        let (out, _) = judge_window(&ws[0], &quats, &[(-845.0, -635.0)], None, 200, 10.0, &SgCache::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(out, WindowOutcome::Unmeasured { reason: FailReason::WindowTooShort });
    }
    #[test] fn judge_window_uncovered_is_unmeasured() {
        let (ws, quats, _) = window_fixture();
        let (out, _) = judge_window(&ws[0], &quats, &[(49_900.0, 50_100.0)], None, 200, 10.0, &SgCache::new(), &AtomicBool::new(false)).unwrap();
        assert_eq!(out, WindowOutcome::Unmeasured { reason: FailReason::NoGyroOverlap });
    }
    #[test] fn judge_window_reports_cost_at_x() {
        let (ws, quats, intervals) = window_fixture();
        let (out, _) = judge_window(&ws[0], &quats, &intervals, Some(1300.0), 200, 10.0, &SgCache::new(), &AtomicBool::new(false)).unwrap();
        let WindowOutcome::Measured { best: Some(best), at_x: Some(at_x), .. } = out else { panic!("{out:?}"); };
        assert!((1195.0..=1405.0).contains(&at_x.ms) && at_x.cost >= best.cost, "best {best:?} at_x {at_x:?}");
    }
    #[test] fn judge_window_cancelled_returns_none() {
        let (ws, quats, intervals) = window_fixture();
        assert!(judge_window(&ws[0], &quats, &intervals, None, 200, 10.0, &SgCache::new(), &AtomicBool::new(true)).is_none());
    }

    const P: DecideParams = DecideParams { t_d_ms: 10.0, radius_ms: 105.0, g_strong: 1.4, support_ratio: 1.1 };
    const CANDIDATES: [f64; 8] = [-3700.0, -2700.0, -1700.0, -740.0, 300.0, 1300.0, 2300.0, 3300.0];

    fn handed_fixture() -> (HandedTracks, TimeQuat) {
        let spec = SynthSpec { fps: 30.0, duration_ms: 2000.0, tracks: 200, true_offset_ms: -700.0, ..Default::default() };
        let (windows, quats) = synth_windows(&spec, &[5000.0, 8000.0, 11000.0]);
        (HandedTracks { windows, ranges_us: vec![(5_000_000, 7_000_000), (8_000_000, 10_000_000), (11_000_000, 13_000_000)],
            has_tracker: true, every_nth: 1, frames_fed: 0, track_ms: 0.0 }, quats)
    }

    fn empty_handoff() -> HandedTracks {
        HandedTracks { windows: Vec::new(), ranges_us: Vec::new(), has_tracker: true, every_nth: 1, frames_fed: 0, track_ms: 0.0 }
    }

    fn candidate_curves() -> Vec<crate::synchronization::deep_match::DeepMatchWindowCurve> {
        (0..2).map(|range_idx| crate::synchronization::deep_match::DeepMatchWindowCurve {
            range_idx, t_center_ms: 6000.0 + 3000.0 * range_idx as f64, argmin_ms: -700.0, cost_min: 10.0, n_eff: 100.0,
            curve: (-160..=120).map(|i| { let x = i as f64 * 25.0;
                (x, 10.0 + 90.0 * ((x + 700.0).abs() / 300.0).min(1.0)) }).collect(),
        }).collect()
    }

    #[test] fn judge_candidates_finds_truth_among_noise() {
        let (tracks, quats) = handed_fixture();
        let (verdict, windows, stats) = judge_candidates(&tracks, &quats, &CANDIDATES, None, &P, 200, &AtomicBool::new(false), &|_| {}).unwrap();
        eprintln!("truth verdict={verdict:?} windows={windows:?} stats={stats:?}");
        let JudgeVerdict::Found { offset_ms, windows } = verdict else { panic!("STOP: truth candidate was not Found"); };
        assert!((offset_ms + 700.0).abs() <= 3.0 && windows.len() >= 2);
    }
    #[test] fn judge_candidates_without_truth_never_found() {
        let (tracks, quats) = handed_fixture();
        let candidates: Vec<f64> = CANDIDATES.into_iter().filter(|&x| x != -740.0).collect();
        let (verdict, windows, stats) = judge_candidates(&tracks, &quats, &candidates, None, &P, 200, &AtomicBool::new(false), &|_| {}).unwrap();
        eprintln!("no truth verdict={verdict:?} windows={windows:?} stats={stats:?}");
        assert!(!matches!(verdict, JudgeVerdict::Found { .. }), "STOP: false Found");
    }
    #[test] fn judge_candidates_supports_posterior_x_at_truth() {
        let (tracks, quats) = handed_fixture();
        let p = DecideParams { g_strong: 1e9, ..P };
        let (verdict, windows, _) = judge_candidates(&tracks, &quats, &CANDIDATES, Some(-740.0), &p, 200, &AtomicBool::new(false), &|_| {}).unwrap();
        let JudgeVerdict::NoWinner { support_ms: Some(s) } = verdict else { panic!("{verdict:?} {windows:?}"); };
        assert!((s + 700.0).abs() <= 3.0);
    }
    #[test] fn handed_tracks_are_scoped_to_the_call() {
        with_handed_tracks(Some(empty_handoff()), || {
            assert!(take_handed_tracks().is_some());
            assert!(take_handed_tracks().is_none());
        });
        with_handed_tracks(Some(empty_handoff()), || {});
        assert!(take_handed_tracks().is_none());
        let panic = std::panic::catch_unwind(|| with_handed_tracks(Some(empty_handoff()), || panic!("handoff panic")));
        assert!(panic.is_err());
        assert!(take_handed_tracks().is_none());
    }
    #[test] fn run_chunk_judge_without_tracks_cannot_judge() {
        let cancel = AtomicBool::new(false);
        let run = || run_chunk_judge(&[], 15000.0, &TimeQuat::new(), JudgeMode::On, &cancel, &|_| {}).unwrap();
        assert_eq!(run(), JudgeOutcome { mode: JudgeMode::On, verdict: JudgeVerdict::CannotJudge { measured: 0, reason: "no_tracks" } });
        with_handed_tracks(Some(HandedTracks { has_tracker: false, ..empty_handoff() }), || {
            assert_eq!(run().verdict, JudgeVerdict::CannotJudge { measured: 0, reason: "no_opencv" });
        });
        with_handed_tracks(Some(empty_handoff()), || {
            assert_eq!(run().verdict, JudgeVerdict::CannotJudge { measured: 0, reason: "too_few_curves" });
        });
    }
    #[test] fn run_chunk_judge_without_quaternions_cannot_judge() {
        let curves = candidate_curves();
        assert!(!crate::synchronization::deep_match::forward_candidates(&curves, 25.0, 10000.0, 50).is_empty());
        let (tracks, _) = handed_fixture();
        with_handed_tracks(Some(tracks), || {
            let outcome = run_chunk_judge(&curves, 15000.0, &TimeQuat::new(), JudgeMode::On, &AtomicBool::new(false), &|_| {}).unwrap();
            assert_eq!(outcome.verdict, JudgeVerdict::CannotJudge { measured: 0, reason: "too_few_measured" });
        });
    }
    #[test] fn run_chunk_judge_cancelled_records_nothing() {
        with_handed_tracks(Some(empty_handoff()), || {
            assert!(run_chunk_judge(&candidate_curves(), 15000.0, &TimeQuat::new(), JudgeMode::On, &AtomicBool::new(true), &|_| panic!("cancelled progress")).is_none());
            assert!(take_handed_tracks().is_none());
        });
    }
    const A: (i64, i64) = (0, 2500);
    const B: (i64, i64) = (3000, 5500);
    const C: (i64, i64) = (6000, 8500);
    const D: (i64, i64) = (9000, 11500);

    fn w(range_ms: (i64, i64), outcome: WindowOutcome) -> WindowJudge {
        WindowJudge { range_us: (range_ms.0 * 1000, range_ms.1 * 1000), outcome }
    }
    fn m(best: Option<(f64, f64)>, g: Option<f64>, at_x: Option<(f64, f64)>) -> WindowOutcome {
        let valley = |(ms, cost)| Valley { ms, cost };
        WindowOutcome::Measured { best: best.map(valley), g, second_ms: None, at_x: at_x.map(valley) }
    }

    #[test] fn candidate_intervals_merge_and_include_posterior() {
        assert_eq!(candidate_intervals(&[1000.0, -500.0], None, 100.0), vec![(-600.0, -400.0), (900.0, 1100.0)]);
        // X farther than R from every candidate is appended; X within R is not
        assert_eq!(candidate_intervals(&[0.0], Some(5000.0), 100.0), vec![(-100.0, 100.0), (4900.0, 5100.0)]);
        assert_eq!(candidate_intervals(&[0.0], Some(60.0), 100.0), vec![(-100.0, 100.0)]);
        // Touching / overlapping intervals merge; non-finite candidates are dropped
        assert_eq!(candidate_intervals(&[0.0, 200.0, f64::NAN], None, 100.0), vec![(-100.0, 300.0)]);
    }
    #[test] fn independent_windows_by_half_overlap() {
        assert!(windows_independent((0, 2_500_000), (1_250_000, 3_750_000)));   // overlap exactly half
        assert!(!windows_independent((0, 2_500_000), (1_000_000, 3_500_000)));
        assert!(!windows_independent((0, 0), (5, 10)));                         // zero-length window
    }
    #[test] fn judge_every_nth_targets_the_fps() {
        for (fps, n) in [(23.976, 1), (30.0, 1), (50.0, 2), (59.94, 2), (120.0, 4), (240.0, 8), (f64::NAN, 1)] {
            assert_eq!(judge_every_nth(fps, 30.0), n, "fps {fps}");
        }
    }
    #[test] fn decide_cannot_judge_with_fewer_than_two_independent_measured() {
        let ws = [w(A, m(Some((-700.0, 1.0)), Some(3.0), None)), w(B, WindowOutcome::Unmeasured { reason: FailReason::FewMeasurements })];
        assert_eq!(decide_chunk(&ws, None, &P), JudgeVerdict::CannotJudge { measured: 1, reason: "too_few_measured" });
    }
    #[test] fn decide_overlapping_windows_cannot_judge() {
        // Review Focus 5: two strong, agreeing, but heavily overlapping windows
        let ws = [w((0, 2500), m(Some((-700.0, 1.0)), Some(3.0), None)), w((500, 3000), m(Some((-701.0, 1.0)), Some(3.0), None))];
        assert!(matches!(decide_chunk(&ws, None, &P), JudgeVerdict::CannotJudge { measured: 2, .. }));
    }
    #[test] fn decide_found_when_all_strong_agree() {
        let ws = [w(A, m(Some((-700.0, 1.0)), Some(2.0), None)), w(B, m(Some((-704.0, 1.1)), Some(1.5), None)),
                  w(C, m(Some((900.0, 2.0)), Some(1.2), None)), w(D, m(None, None, None))];
        assert_eq!(decide_chunk(&ws, None, &P), JudgeVerdict::Found { offset_ms: -702.0, windows: vec![0, 1] });
    }
    #[test] fn decide_found_respects_the_drift_tolerance() {
        let at = |d: f64| [w(A, m(Some((-700.0, 1.0)), Some(2.0), None)), w(B, m(Some((-700.0 + d, 1.0)), Some(2.0), None))];
        assert!(matches!(decide_chunk(&at(10.0), None, &P), JudgeVerdict::Found { .. }));
        assert!(matches!(decide_chunk(&at(10.1), None, &P), JudgeVerdict::NoWinner { support_ms: None }));
    }
    #[test] fn decide_strong_window_elsewhere_blocks_found() {
        let ws = [w(A, m(Some((-700.0, 1.0)), Some(2.0), None)), w(B, m(Some((-702.0, 1.0)), Some(2.0), None)),
                  w(C, m(Some((4300.0, 1.0)), Some(1.5), None))];
        assert_eq!(decide_chunk(&ws, None, &P), JudgeVerdict::NoWinner { support_ms: None });
    }
    #[test] fn decide_single_strong_is_no_winner() {
        let ws = [w(A, m(Some((-700.0, 1.0)), Some(2.0), None)), w(B, m(Some((300.0, 1.0)), Some(1.1), None))];
        assert_eq!(decide_chunk(&ws, None, &P), JudgeVerdict::NoWinner { support_ms: None });
    }
    #[test] fn decide_support_accepts_posterior_x() {
        // No strong window; both windows put X within 10% of their best
        let ws = [w(A, m(Some((300.0, 1.0)), Some(1.1), Some((-698.0, 1.05)))), w(B, m(Some((-702.0, 1.0)), Some(1.2), Some((-702.0, 1.0))))];
        assert_eq!(decide_chunk(&ws, Some(-700.0), &P), JudgeVerdict::NoWinner { support_ms: Some(-700.0) });
        // 1.11 > 1.1 for window A: only one supporter left
        let ws2 = [w(A, m(Some((300.0, 1.0)), Some(1.1), Some((-698.0, 1.11)))), ws[1].clone()];
        assert_eq!(decide_chunk(&ws2, Some(-700.0), &P), JudgeVerdict::NoWinner { support_ms: None });
    }
    #[test] fn decide_support_vetoed_by_strong_window_outside_x() {
        let ws = [w(A, m(Some((-700.0, 1.0)), Some(1.2), Some((-700.0, 1.0)))), w(B, m(Some((-701.0, 1.0)), Some(1.2), Some((-701.0, 1.0)))),
                  w(C, m(Some((4300.0, 1.0)), Some(1.6), Some((-695.0, 1.05))))];
        assert_eq!(decide_chunk(&ws, Some(-700.0), &P), JudgeVerdict::NoWinner { support_ms: None });
    }
    #[test] fn decisive_only_for_on_mode_found_or_no_winner() {
        let o = |mode, verdict| JudgeOutcome { mode, verdict };
        assert!(is_decisive(Some(&o(JudgeMode::On, JudgeVerdict::NoWinner { support_ms: None }))));
        assert!(is_decisive(Some(&o(JudgeMode::On, JudgeVerdict::Found { offset_ms: 0.0, windows: vec![0, 1] }))));
        assert!(!is_decisive(Some(&o(JudgeMode::Shadow, JudgeVerdict::Found { offset_ms: 0.0, windows: vec![0, 1] }))));
        assert!(!is_decisive(Some(&o(JudgeMode::On, JudgeVerdict::CannotJudge { measured: 0, reason: "no_tracks" }))));
        assert!(!is_decisive(None));
    }
}
