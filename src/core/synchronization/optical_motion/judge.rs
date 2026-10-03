// SPDX-License-Identifier: GPL-3.0-or-later

//! Pure decisions from the optical measurements of one deep-match chunk.

use super::search::FailReason;

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

    const P: DecideParams = DecideParams { t_d_ms: 10.0, radius_ms: 105.0, g_strong: 1.4, support_ratio: 1.1 };
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
