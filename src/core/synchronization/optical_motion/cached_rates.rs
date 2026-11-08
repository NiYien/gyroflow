// SPDX-License-Identifier: GPL-3.0-or-later

use super::{
    bearings::bearings,
    config,
    cost::pair_rotation_fit,
    quat_table::QuatTable,
    search::{FullEval, SearchParams, search},
    tracks::{FrameTiming, readout_pos},
};
use crate::stabilization::{ComputeParams, FrameTransform};
use crate::synchronization::{
    PoseEstimator, deep_match as dm, find_offset::rs_sync::FindOffsetsRssync,
    lens_delay::frame_of_stamp,
};
use nalgebra::{Matrix3, Vector3};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

struct PairRate {
    stamps: (i64, i64),
    ta: f64,
    tb: f64,
    rate: Vector3<f64>,
    information: Matrix3<f64>,
}

fn timing(params: &ComputeParams, stamp: i64) -> Option<FrameTiming> {
    let gyro = params.gyro.read();
    let md = gyro.file_metadata.read();
    let index = frame_of_stamp(stamp, &md.per_frame_time_offsets, params.scaled_fps)?;
    let mid_ms = stamp as f64 / 1000.0;
    let ts_ms = mid_ms - md.frame_time_offset(index);
    let readout_ms = FrameTransform::get_frame_readout_time(params, false, ts_ms, &md);
    Some(FrameTiming {
        index,
        ts_ms,
        mid_ms,
        readout_ms,
    })
}

fn fit_pair(
    params: &ComputeParams,
    size: (u32, u32),
    a: (i64, Vec<(f32, f32)>),
    b: (i64, Vec<(f32, f32)>),
) -> Option<PairRate> {
    let fa = timing(params, a.0)?;
    let fb = timing(params, b.0)?;
    let horizontal = params.frame_readout_direction.is_horizontal();
    let mut va = Vec::new();
    let mut vb = Vec::new();
    let mut positions = Vec::new();
    for (i, (pa, pb)) in bearings(params, size, &a.1, &fa)
        .into_iter()
        .zip(bearings(params, size, &b.1, &fb))
        .enumerate()
    {
        if let (Some(pa), Some(pb)) = (pa, pb) {
            va.push(pa);
            vb.push(pb);
            positions.push((
                readout_pos(a.1[i].into(), size, horizontal),
                readout_pos(b.1[i].into(), size, horizontal),
            ));
        }
    }
    let (angle, information, weights) = pair_rotation_fit(&va, &vb)?;
    let sw: f64 = weights.iter().sum();
    let (ra, rb) = weights
        .iter()
        .zip(positions)
        .fold((0.0, 0.0), |(ra, rb), (w, (a, b))| {
            (ra + w * a as f64, rb + w * b as f64)
        });
    let ta = fa.mid_ms + fa.readout_ms * (ra / sw - 0.5);
    let tb = fb.mid_ms + fb.readout_ms * (rb / sw - 0.5);
    if tb <= ta {
        return None;
    }
    Some(PairRate {
        stamps: (a.0, b.0),
        ta,
        tb,
        rate: angle * (1000.0 / (tb - ta)),
        information,
    })
}

/// Fit the change in angular velocity across consecutive frame pairs. The normal
/// matrix measures how much each axis actually moves this window's image points.
fn cost(rates: &[PairRate], table: &QuatTable, offset: f64, norm: f64) -> f64 {
    let mut sum = 0.0;
    let mut previous: Option<(&PairRate, Vector3<f64>)> = None;
    for pair in rates {
        let rotation = table.at(pair.tb - offset).inverse() * table.at(pair.ta - offset);
        let error = pair.rate - rotation.scaled_axis() * (1000.0 / (pair.tb - pair.ta));
        if let Some((last, last_error)) = previous {
            if last.stamps.1 == pair.stamps.0 {
                let change = error - last_error;
                sum += change.dot(&((last.information + pair.information) * change));
            }
        }
        previous = Some((pair, error));
    }
    (sum / norm).sqrt()
}

pub(crate) fn confirm(
    estimator: &PoseEstimator,
    ranges: &[(i64, i64)],
    params: &ComputeParams,
    bounds: (f64, f64),
    cancel: &AtomicBool,
) -> Option<f64> {
    let gyro_span = {
        let g = params.gyro.read();
        (
            *g.quaternions.first_key_value()?.0 as f64 / 1000.0,
            *g.quaternions.last_key_value()?.0 as f64 / 1000.0,
        )
    };
    let plan = dm::optical_retry_search(params.scaled_duration_ms, gyro_span)?;
    let lo = plan.center_ms - plan.radius_ms;
    let hi = plan.center_ms + plan.radius_ms;
    if lo < bounds.0 || hi > bounds.1 {
        return None;
    }
    // Queue window positions are serialized in integer milliseconds. Only use
    // the ordinary non-overlapping windows for which this domain was planned.
    let half_window = dm::per_window_ms() / 2.0;
    if plan.windows_ms.len() != ranges.len()
        || !ranges
            .iter()
            .zip(&plan.windows_ms)
            .all(|(&(a, b), center)| {
                (a as f64 / 1000.0 - (center - half_window)).abs() <= 1.0
                    && (b as f64 / 1000.0 - (center + half_window)).abs() <= 1.0
            })
    {
        return None;
    }
    confirm_windows(estimator, ranges, params, &plan, None, cancel)
}

pub(crate) fn confirm_short(
    estimator: &PoseEstimator,
    range: (i64, i64),
    params: &ComputeParams,
    bounds: (f64, f64),
    cancel: &AtomicBool,
) -> Option<f64> {
    let (start, end) = range;
    let middle = start + (end - start) / 2;
    let halves = [(start, middle), (middle, end)];
    let (lo, hi) = {
        let g = params.gyro.read();
        (
            (end as f64 / 1000.0 - *g.quaternions.last_key_value()?.0 as f64 / 1000.0)
                .max(bounds.0),
            (start as f64 / 1000.0 - *g.quaternions.first_key_value()?.0 as f64 / 1000.0)
                .min(bounds.1),
        )
    };
    let centers = halves
        .iter()
        .map(|&(a, b)| (a + b) as f64 / 2000.0)
        .collect();
    let Some(plan) = dm::OpticalRetrySearch::within_budget(centers, lo, hi) else {
        log::info!(target:"sync","[deep-match] short angular skip: offset range [{lo:.1},{hi:.1}]ms outside retry budget");
        return None;
    };
    confirm_windows(
        estimator,
        &halves,
        params,
        &plan,
        Some((middle - start) as f64 / 1000.0),
        cancel,
    )
}

fn confirm_windows(
    estimator: &PoseEstimator,
    ranges: &[(i64, i64)],
    params: &ComputeParams,
    plan: &dm::OpticalRetrySearch,
    short_half_ms: Option<f64>,
    cancel: &AtomicBool,
) -> Option<f64> {
    let lo = plan.center_ms - plan.radius_ms;
    let hi = plan.center_ms + plan.radius_ms;
    let mut rows = Vec::new();
    let start = std::time::Instant::now();
    let mut measurement = params.clone();
    measurement.framebuffer_inverted = false;
    let cfg = config::config();
    let windows = FindOffsetsRssync::collect_points(estimator.sync_results.clone(), ranges);
    let level = if short_half_ms.is_some() {
        log::Level::Info
    } else {
        log::Level::Debug
    };
    for ((mut points, center), &(start, end)) in
        windows.into_iter().zip(&plan.windows_ms).zip(ranges)
    {
        if short_half_ms.is_some() {
            // collect_points selects by start stamp. A pair ending at the split
            // would share a frame with the other half.
            points.retain(|((a, b), _)| a.0 >= start && b.0 < end);
        }
        let total = points.len();
        let mut rates = Vec::new();
        for ((a, b), size) in points {
            if cancel.load(Relaxed) {
                return None;
            }
            if let Some(rate) = fit_pair(&measurement, size, a, b) {
                rates.push(rate);
            }
        }
        let mut count = 0;
        let mut norm = 0.0;
        for w in rates.windows(2).filter(|w| w[0].stamps.1 == w[1].stamps.0) {
            count += 1;
            norm += w[0].information.trace() + w[1].information.trace();
        }
        if count < super::search::MIN_PAIRS {
            log::log!(target:"sync", level,
                "[deep-match] cached angular rates window={center:.1} too few changes: {count}/{total}");
            continue;
        }
        let first = rates.iter().map(|p| p.ta).reduce(f64::min)?;
        let last = rates.iter().map(|p| p.tb).reduce(f64::max)?;
        let table = QuatTable::build(&params.gyro.read().quaternions, first - hi, last - lo);
        let evaluate = |offset: f64| {
            ((lo..=hi).contains(&offset) && table.covers(first - offset, last - offset))
                .then(|| cost(&rates, &table, offset, norm))
        };
        let p = SearchParams {
            init_ms: plan.center_ms,
            search_ms: plan.radius_ms,
            check_negative: false,
            step_ms: cfg.coarse_step_ms,
            g_full: cfg.g_full,
            total_pairs: total,
        };
        // The relative valley test is scale invariant. Both evaluations use
        // RMS angular-rate change, and count temporal differences, not features.
        let outcome = search(
            &p,
            &evaluate,
            &|x| {
                evaluate(x).map(|cost| FullEval {
                    cost,
                    pairs_measured: count,
                })
            },
            cancel,
            true,
        )?;
        log::log!(target:"sync", level,
            "[deep-match] cached angular rates window={center:.1} offset={:.2} conf={:.3} pairs={count}/{total}",
            outcome.offset_ms,outcome.conf);
        rows.push((*center, outcome.offset_ms, outcome.conf));
    }
    if cancel.load(Relaxed) {
        return None;
    }
    let measured = rows.len();
    let offset = dm::independent_retry_offset(
        rows.into_iter(),
        params.scaled_duration_ms,
        short_half_ms.unwrap_or_else(dm::per_window_ms),
    );
    log::info!(target: "sync", "[deep-match] cached angular rates: windows={measured} offset={offset:?} elapsed_ms={:.0}",
        start.elapsed().as_secs_f64() * 1000.0);
    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gyro_source::{Quat64, TimeQuat};

    #[test]
    fn repeated_angular_motion_cannot_confirm_a_window() {
        // The 457ms period is not a multiple of the coarse search step.
        let orientation = |t: f64| {
            Quat64::from_scaled_axis(Vector3::new(
                0.0,
                0.03 * (t * std::f64::consts::TAU / 457.0).sin(),
                0.0,
            ))
        };
        let quats: TimeQuat = (0..6000)
            .map(|t| (t * 1000, orientation(t as f64)))
            .collect();
        let table = QuatTable::build(&quats, 0.0, 5999.0);
        let rates: Vec<_> = (0..61)
            .map(|i| {
                let ta = 1500.0 + i as f64 * 40.0;
                let tb = ta + 40.0;
                let rotation = orientation(tb - 13.0).inverse() * orientation(ta - 13.0);
                PairRate {
                    stamps: (i, i + 1),
                    ta,
                    tb,
                    rate: rotation.scaled_axis() * 25.0
                        + Vector3::new(0.0, 0.001 * (i as f64).sin(), 0.0),
                    information: Matrix3::identity(),
                }
            })
            .collect();
        let evaluate = |x: f64| Some(cost(&rates, &table, x, 360.0));
        let p = SearchParams {
            init_ms: 0.0,
            search_ms: 800.0,
            check_negative: false,
            step_ms: 10.0,
            g_full: 2.0,
            total_pairs: 61,
        };
        let result = search(
            &p,
            &evaluate,
            &|x| {
                evaluate(x).map(|cost| FullEval {
                    cost,
                    pairs_measured: 60,
                })
            },
            &AtomicBool::new(false),
            true,
        )
        .unwrap();
        assert!(
            result.conf < 0.4,
            "repeated angular motion yielded {result:?}"
        );
    }
}
