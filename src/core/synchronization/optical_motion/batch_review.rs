// SPDX-License-Identifier: GPL-3.0-or-later

//! Verify a weak batch offset on the same tracked frames. Changes in image angular velocity can
//! distinguish delays even with smooth unmodeled motion. The unfiltered rotation also retains
//! evidence that the track high-pass can remove. Both checks keep the original optical offset.

use super::quat_table::QuatTable;
use super::tracks::{PairData, WindowTracks, row_time_ms};
use super::{config, cost, interval_tables, row_time_span_ms, search, table_for};
use crate::gyro_source::TimeQuat;
use crate::synchronization::{
    SyncParams,
    sync_repair::{MIN_BATCH_SYNC_POINT_CONFIDENCE, MIN_BATCH_SYNC_POINT_RANK},
};
use nalgebra::{Matrix3, Vector3};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

struct PairRotation {
    angle: Vector3<f64>,
    information: Matrix3<f64>,
    from_ms: f64,
    to_ms: f64,
    frames: (usize, usize),
    center_ms: f64,
    inv_dt: f64,
}

impl PairRotation {
    fn fit(pair: &PairData) -> Option<Self> {
        let dt = pair.b.mid_ms - pair.a.mid_ms;
        if dt <= 0.0 {
            return None;
        }
        let (angle, information, weights) = cost::pair_rotation_fit(&pair.va, &pair.vb)?;
        let weight = weights.iter().sum::<f64>();
        let mean_row = |rows: &[f32]| {
            (weights
                .iter()
                .zip(rows)
                .map(|(w, &r)| w * f64::from(r))
                .sum::<f64>()
                / weight) as f32
        };
        Some(Self {
            angle,
            information: information / weight,
            from_ms: row_time_ms(&pair.a, mean_row(&pair.fa)),
            to_ms: row_time_ms(&pair.b, mean_row(&pair.fb)),
            frames: (pair.a.index, pair.b.index),
            center_ms: (pair.a.mid_ms + pair.b.mid_ms) / 2.0,
            inv_dt: 1.0 / dt,
        })
    }

    fn residual(&self, table: &QuatTable, offset: f64) -> Vector3<f64> {
        let predicted = table.at(self.to_ms - offset).inverse() * table.at(self.from_ms - offset);
        self.angle - predicted.scaled_axis()
    }
}

/// RMS frame rotation residual in bearing space, before the track high-pass.
fn angular_cost(pairs: &[PairRotation], table: &QuatTable, offset: f64) -> f64 {
    let sum = pairs
        .iter()
        .map(|p| {
            let error = p.residual(table, offset);
            error.dot(&(p.information * error))
        })
        .sum::<f64>();
    (sum / pairs.len() as f64).sqrt()
}

struct VelocityResidual {
    rms: f64,
    axis_energy: Vector3<f64>,
}

/// Compare the middle image velocity with linear interpolation between its neighbors.
/// Smooth unmodeled motion cancels. A break in tracking never supplies a difference.
fn velocity_curvature(pairs: &[PairRotation], rotation: Option<(&QuatTable, f64)>) -> Option<VelocityResidual> {
    let (mut left, mut middle) = (Vector3::zeros(), Vector3::zeros());
    let mut sum = 0.0;
    let mut axis_energy = Vector3::zeros();
    let mut triples = 0;
    for (i, p) in pairs.iter().enumerate() {
        let right = rotation.map_or(p.angle, |(table, d)| p.residual(table, d)) * p.inv_dt;
        if i >= 2 {
            let (a, b) = (&pairs[i - 2], &pairs[i - 1]);
            if a.frames.1 == b.frames.0 && b.frames.1 == p.frames.0 {
                let weight = (b.center_ms - a.center_ms) / (p.center_ms - a.center_ms);
                let error = middle - left.lerp(&right, weight);
                sum += error.dot(&(b.information * error));
                axis_energy += error.component_mul(&error);
                triples += 1;
            }
        }
        left = middle;
        middle = right;
    }
    (triples >= search::MIN_PAIRS).then(|| VelocityResidual {
        rms: (sum / triples as f64).sqrt(),
        axis_energy,
    })
}

fn verify_velocity(
    pairs: &[PairRotation],
    tables: &[QuatTable],
    intervals: &[(f64, f64)],
    span: (f64, f64),
    outcome: &search::SearchOutcome,
    ratio: f64,
    cancel: &AtomicBool,
) -> Option<bool> {
    use rayon::prelude::*;
    let no_rotation = velocity_curvature(pairs, None)?;
    let measured = outcome.offset_ms;
    let proposed = velocity_curvature(pairs, Some((&tables[table_for(intervals, measured)], measured)))?;
    // A periodic alias can improve the dominant axis yet make another axis worse than no correction.
    if proposed.rms * ratio > no_rotation.rms
        || (0..3).any(|i| proposed.axis_energy[i] > no_rotation.axis_energy[i])
    {
        return Some(false);
    }
    let cost = |d: f64| {
        let table = &tables[table_for(intervals, d)];
        if cancel.load(Relaxed) || !table.covers(span.0 - d, span.1 - d) {
            return f64::NAN;
        }
        velocity_curvature(pairs, Some((table, d))).map_or(f64::NAN, |r| r.rms)
    };
    // Rate changes can produce a narrow valley between the 10 ms optical grid points.
    const STEP_MS: f64 = 2.0;
    let mut offsets = Vec::new();
    let mut ranges = Vec::new();
    for interval in intervals {
        let start = offsets.len();
        offsets.extend(search::grid(std::slice::from_ref(interval), STEP_MS));
        ranges.push(start..offsets.len());
    }
    let costs: Vec<_> = offsets.par_iter().map(|&d| cost(d)).collect();
    let minima = search::local_minima(&costs, &ranges);
    let mut valleys: Vec<_> = minima
        .par_iter()
        .filter_map(|&i| {
            let d = offsets[i];
            let (lo, hi) = intervals[table_for(intervals, d)];
            let (x, y) = search::brent_min(
                &mut |d| cost(d),
                (d - STEP_MS).max(lo),
                (d + STEP_MS).min(hi),
                search::REFINE_TOL_MS,
                search::REFINE_MAX_EVALS,
            );
            let (x, y) = if costs[i] < y { (d, costs[i]) } else { (x, y) };
            y.is_finite().then_some((x, y, i))
        })
        .collect();
    if cancel.load(Relaxed) {
        return None;
    }
    valleys.sort_by(|a, b| a.1.total_cmp(&b.1));
    let &(best, value, i) = valleys.first()?;
    if (best - outcome.offset_ms).abs() > search::C1_REFINE_MS
        || search::is_edge(&offsets, &costs, intervals, i, STEP_MS)
    {
        return Some(false);
    }
    let &(other, rival, _) = valleys
        .iter()
        .find(|v| (v.0 - best).abs() > search::C1_REFINE_MS)?;
    // Even a unique valley must explain the observed motion better than zero gyro rotation.
    let contrast = rival.min(no_rotation.rms) / proposed.rms.max(value);
    if contrast >= ratio {
        log::info!(target:"sync","[optical] velocity review: measured={:.3} fitted={best:.3} competitor={other:.3} ratio={contrast:.3}",outcome.offset_ms);
    }
    Some(contrast >= ratio)
}

fn verifies_offset(
    window: &WindowTracks,
    outcome: &search::SearchOutcome,
    quats: &TimeQuat,
    intervals: &[(f64, f64)],
    sg: &cost::SgCache,
    cancel: &AtomicBool,
) -> Option<bool> {
    let best = outcome.offset_ms;
    let second = outcome.second_ms?;
    let span = row_time_span_ms(window)?;
    let tables = interval_tables(quats, span, intervals);
    let cfg = config::config();
    let context = |d| cost::CostContext {
        window,
        quats: &tables[table_for(intervals, d)],
        sg,
    };
    let fits = cost::band_fits_full(&context(best), best)?;
    // Each measured frame pair votes once, regardless of how many readout bands it fitted.
    let mut pairs = Vec::with_capacity(outcome.pairs_measured);
    for bands in fits.chunk_by(|a, b| a.seq == b.seq) {
        if cancel.load(Relaxed) {
            return None;
        }
        pairs.push(PairRotation::fit(&window.pairs[bands[0].seq])?);
    }
    let ratio = 1.0 + MIN_BATCH_SYNC_POINT_CONFIDENCE * (cfg.g_full - 1.0);
    if verify_velocity(&pairs, &tables, intervals, span, outcome, ratio, cancel) == Some(true) {
        return Some(true);
    }
    if cancel.load(Relaxed) {
        return None;
    }
    let angular = |d| angular_cost(&pairs, context(d).quats, d);
    let raw_best = angular(best);
    let limit = raw_best * ratio;
    if !(raw_best > 0.0 && outcome.cost_px > 0.0 && angular(second) >= limit) {
        return Some(false);
    }

    let subset = sg.select_tracks(window, cfg.coarse_points);
    let coarse_best = cost::eval_coarse(&context(best), &subset, best, super::COARSE_IRLS_ROUNDS)?;
    if coarse_best <= 0.0 {
        return None;
    }
    let mut joint = Vec::with_capacity(outcome.coarse.len());
    let mut raw_floor = f64::INFINITY;
    for &(d, hp) in &outcome.coarse {
        if cancel.load(Relaxed) {
            return None;
        }
        if hp.is_finite() {
            let raw = angular(d);
            raw_floor = raw_floor.min(raw);
            joint.push((hp / coarse_best).max(raw / raw_best));
        } else {
            joint.push(f64::NAN);
        }
    }
    if raw_best > ratio * raw_floor {
        return Some(false);
    }
    let ranges: Vec<_> = intervals
        .iter()
        .map(|&(lo, hi)| {
            outcome.coarse.partition_point(|&(d, _)| d < lo)
                ..outcome.coarse.partition_point(|&(d, _)| d <= hi + 1e-6)
        })
        .collect();
    let minima = search::local_minima(&joint, &ranges);
    let radius = search::C1_REFINE_MS;
    let mut seeds: Vec<_> = minima.iter().map(|&i| outcome.coarse[i].0).collect();
    seeds.extend([second, best]);
    for seed in seeds {
        if cancel.load(Relaxed) {
            return None;
        }
        let (left, right) = intervals[table_for(intervals, seed)];
        let lo = (seed - radius).max(left);
        let hi = (seed + radius).min(right);
        if hi <= lo {
            continue;
        }
        let n = ((hi - lo) / search::REFINE_TOL_MS).ceil() as usize;
        // Only pay for full geometry where angular motion still permits a rival.
        if seed != best
            && !(0..=n).any(|i| {
                let d = lo + (hi - lo) * i as f64 / n as f64;
                (d - best).abs() > radius && angular(d) <= limit
            })
        {
            continue;
        }
        let mut score = |d| {
            if cancel.load(Relaxed) {
                return f64::INFINITY;
            }
            cost::eval_full(&context(d), d)
                .filter(|r| r.bands > 0)
                .map_or(f64::INFINITY, |hp| {
                    (hp.cost_px / outcome.cost_px).max(angular(d) / raw_best)
                })
        };
        let (d, value) = search::brent_min(
            &mut score,
            lo,
            hi,
            search::REFINE_TOL_MS,
            search::REFINE_MAX_EVALS,
        );
        if (d - best).abs() > radius && value < ratio {
            return Some(false);
        }
    }
    Some(!cancel.load(Relaxed))
}

pub(in crate::synchronization) fn review_weak_rows(
    windows: &[WindowTracks],
    outcomes: &[search::SearchOutcome],
    rows: &mut [(f64, f64, f64, f64)],
    ranks: &[f32],
    quats: &TimeQuat,
    sp: &SyncParams,
    cancel: &AtomicBool,
) {
    if ranks.is_empty()
        || rows
            .iter()
            .zip(ranks)
            .any(|(row, &rank)| rank >= MIN_BATCH_SYNC_POINT_RANK && row.3 >= MIN_BATCH_SYNC_POINT_CONFIDENCE)
    {
        return;
    }
    let mut weak = (0..rows.len())
        .zip(ranks)
        .filter_map(|(i, &rank)| {
            (rank >= MIN_BATCH_SYNC_POINT_RANK
                && outcomes[i].fail.is_none()
                && outcomes[i].has_high_support(windows[i].pairs.len()))
            .then_some(i)
        })
        .peekable();
    if weak.peek().is_none() {
        return;
    }
    let intervals = search::search_intervals(
        sp.initial_offset,
        sp.search_size,
        sp.initial_offset_inv && sp.initial_offset.abs() > 1.0,
    );
    let sg = cost::SgCache::with_frame_step(sp.every_nth_frame);
    for i in weak {
        if cancel.load(Relaxed) {
            return;
        }
        let outcome = &outcomes[i];
        let start = std::time::Instant::now();
        let verified = verifies_offset(&windows[i], outcome, quats, &intervals, &sg, cancel) == Some(true);
        log::info!(target: "sync", "[optical] batch review seg {i}: offset={:.3} old_conf={:.3} verified={verified} elapsed_ms={:.1}",
            outcome.offset_ms, outcome.conf, start.elapsed().as_secs_f64() * 1000.0);
        if verified {
            // Admit the original measurement at the batch floor. Cross-video confirmation still follows.
            rows[i].3 = MIN_BATCH_SYNC_POINT_CONFIDENCE;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn velocity_curvature_uses_elapsed_time_and_consecutive_pairs() {
        let mut start = 0.0;
        let mut pairs: Vec<_> = (0..search::MIN_PAIRS + 2)
            .map(|i| {
                let dt = if i % 3 == 0 { 30.0 } else { 42.0 };
                let center_ms = start + dt / 2.0;
                let p = PairRotation {
                    angle: Vector3::new(0.01 + center_ms * 1e-5, 0.0, 0.0) * dt,
                    information: Matrix3::identity(),
                    from_ms: start,
                    to_ms: start + dt,
                    frames: (i, i + 1),
                    center_ms,
                    inv_dt: 1.0 / dt,
                };
                start += dt;
                p
            })
            .collect();
        assert!(velocity_curvature(&pairs, None).unwrap().rms < 1e-12);
        pairs[8].angle.x += 0.1;
        assert!(velocity_curvature(&pairs, None).unwrap().rms > 1e-4);
        for p in &mut pairs[8..] {
            p.frames.0 += 1;
            p.frames.1 += 1;
        }
        assert!(velocity_curvature(&pairs, None).is_none());
    }
}
