// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

use super::{get_max_angle, Lowpass, PoseEstimator, SyncParams, TimeIMU};
use crate::stabilization::ComputeParams;
use crate::synchronization::{deep_match as dm, posterior};
use crate::synchronization::find_offset::rs_sync::{covered_probe_radius, FindOffsetsRssync};
use rayon::prelude::*;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, atomic::{AtomicBool, Ordering::Relaxed}};

fn is_short(duration_ms: f64, window_ms: f64) -> bool {
    duration_ms.is_finite() && window_ms.is_finite() && duration_ms > 0.0
        && window_ms > 0.0 && duration_ms <= window_ms * 2.0
}

fn unique_ranges(ranges: &[(i64, i64)], measured: &BTreeSet<i64>) -> Option<(Vec<(i64, i64)>, usize)> {
    let mut seen = BTreeSet::new();
    let mut duplicate_count = 0;
    let mut valid = Vec::new();
    for &(lo, hi) in ranges {
        if lo >= hi { continue; }
        let mut has_samples = false;
        for &ts in measured.range(lo..hi) {
            has_samples = true;
            if !seen.insert(ts) { duplicate_count += 1; }
        }
        if has_samples { valid.push((lo, hi)); }
    }
    if duplicate_count == 0 { return None; }
    valid.sort_unstable();
    let mut merged: Vec<(i64, i64)> = Vec::new();
    for (lo, hi) in valid {
        if let Some(last) = merged.last_mut() {
            if lo <= last.1 { last.1 = last.1.max(hi); continue; }
        }
        merged.push((lo, hi));
    }
    Some((merged, duplicate_count))
}

fn fully_covered_cost(offset: f64, of: &[TimeIMU], gyro: &BTreeMap<usize, TimeIMU>) -> f64 {
    let Some((&first, _)) = gyro.first_key_value() else { return f64::MAX; };
    let Some((&last, _)) = gyro.last_key_value() else { return f64::MAX; };
    if !offset.is_finite() || of.is_empty() { return f64::MAX; }
    let mut sum = 0.0;
    for o in of {
        let ts = (o.timestamp_ms - offset) * 1000.0;
        if !ts.is_finite() || ts < first as f64 || ts > last as f64 { return f64::MAX; }
        let Some((_, g)) = gyro.range(ts as usize..).next() else { return f64::MAX; };
        let (Some(a), Some(b)) = (o.gyro, g.gyro) else { return f64::MAX; };
        sum += (a[0] - b[0]).powi(2) * 70.0 + (a[1] - b[1]).powi(2) * 70.0 + (a[2] - b[2]).powi(2) * 100.0;
    }
    sum / of.len() as f64
}

fn rejected() -> dm::DeepMatchVerdict {
    dm::DeepMatchVerdict::WeakValley { worst_ratio: 1.0 }
}

/// None is a strict no-op. Some owns this chunk's decision, including rejection.
pub(super) fn validate(
    estimator: &PoseEstimator, ranges: &[(i64, i64)], sp: &SyncParams,
    params: &ComputeParams, cancel: &Arc<AtomicBool>,
) -> Option<dm::DeepMatchVerdict> {
    if !dm::forward_armed() || !dm::posterior_enabled() || !is_short(params.scaled_duration_ms, sp.time_per_syncpoint) {
        return None;
    }
    let curves = dm::peek_curves();
    if curves.len() < 2 { return None; }
    let selected: Vec<_> = curves.iter().filter_map(|c| ranges.get(c.range_idx).copied()).collect();
    let estimated = estimator.estimated_gyro.read();
    let measured: BTreeSet<_> = estimated.iter().filter(|(_, o)| o.gyro.is_some_and(|v| v.iter().all(|x| x.is_finite())))
        .map(|(&ts, _)| ts).collect();
    let (merged, duplicates) = unique_ranges(&selected, &measured)?;
    let samples: Vec<Vec<TimeIMU>> = merged.iter().map(|&(lo, hi)| estimated.range(lo..hi)
        .filter(|(ts, _)| measured.contains(ts)).map(|(_, o)| o.clone()).collect()).collect();
    drop(estimated);
    let started = std::time::Instant::now();
    log::info!(target: "sync", "[deep-match] short-overlap: physical_ms={:.1} window_ms={:.1} windows={} unique_windows={} unique_samples={} duplicated_samples={}",
        params.scaled_duration_ms, sp.time_per_syncpoint, selected.len(), merged.len(), samples.iter().map(Vec::len).sum::<usize>(), duplicates);
    let verdict = validate_unique(estimator, &merged, samples, &curves, sp, params, cancel).unwrap_or_else(rejected);
    log::info!(target: "sync", "[deep-match] short-overlap: verdict={:?} elapsed_ms={:.0}", verdict, started.elapsed().as_secs_f64() * 1000.0);
    Some(verdict)
}

fn validate_unique(
    estimator: &PoseEstimator, ranges: &[(i64, i64)], samples: Vec<Vec<TimeIMU>>,
    original: &[dm::DeepMatchWindowCurve], sp: &SyncParams, params: &ComputeParams, cancel: &Arc<AtomicBool>,
) -> Option<dm::DeepMatchVerdict> {
    if cancel.load(Relaxed) || !dm::forward_enabled() { return None; }
    let raw = {
        let gyro = params.gyro.read();
        let metadata = gyro.file_metadata.read();
        gyro.raw_imu(&metadata).to_vec()
    };
    let (first, last) = (raw.first()?.timestamp_ms, raw.last()?.timestamp_ms);
    if raw.len() < 2 || last <= first { return None; }
    let rate = (raw.len() - 1) as f64 * 1000.0 / (last - first);
    let mut filtered = raw;
    let _ = Lowpass::filter_gyro_forward_backward(20.0, rate, &mut filtered);
    let gyro: BTreeMap<usize, TimeIMU> = filtered.into_iter().map(|g| ((g.timestamp_ms * 1000.0) as usize, g)).collect();
    let mut unique_curves = Vec::new();
    for (index, mut of) in samples.into_iter().enumerate() {
        if cancel.load(Relaxed) || of.len() < 2 || get_max_angle(&of) < dm::motion_gate_armed() { return None; }
        // The existing essential path keeps the original samples when 20 Hz
        // is unavailable at low frame rates (for example 25 fps).
        let _ = Lowpass::filter_gyro_forward_backward(20.0, params.scaled_fps, &mut of);
        // Only this isolated branch clips the search to complete coverage.
        let lo = (sp.initial_offset - sp.search_size).max(of.last()?.timestamp_ms - last);
        let hi = (sp.initial_offset + sp.search_size).min(of.first()?.timestamp_ms - first);
        if !lo.is_finite() || !hi.is_finite() || hi <= lo { return None; }
        let steps = ((hi - lo) / 25.0).floor() as usize;
        let mut curve: Vec<_> = (0..=steps).into_par_iter().filter_map(|i| {
            if cancel.load(Relaxed) { return None; }
            let x = lo + i as f64 * 25.0;
            let c = fully_covered_cost(x, &of, &gyro);
            (c.is_finite() && c < f64::MAX).then_some((x, c.max(f64::EPSILON)))
        }).collect();
        let best = curve.iter().min_by(|a, b| a.1.total_cmp(&b.1))?.0;
        // Densify both the unique coarse minimum and original proposals.
        for center in std::iter::once(best).chain(original.iter().map(|c| c.argmin_ms)) {
            for i in -6..=6 {
                let x = center + i as f64 * 5.0;
                if x < lo || x > hi { continue; }
                let c = fully_covered_cost(x, &of, &gyro);
                if c.is_finite() && c < f64::MAX { curve.push((x, c.max(f64::EPSILON))); }
            }
        }
        curve.sort_by(|a, b| a.0.total_cmp(&b.0));
        curve.dedup_by(|a, b| a.0 == b.0);
        let &(argmin_ms, cost_min) = curve.iter().min_by(|a, b| a.1.total_cmp(&b.1))?;
        unique_curves.push(dm::DeepMatchWindowCurve { range_idx: index,
            t_center_ms: (ranges[index].0 + ranges[index].1) as f64 / 2000.0,
            argmin_ms, cost_min, n_eff: of.len() as f64, curve });
    }
    if cancel.load(Relaxed) { return None; }
    let ll: Vec<(Vec<f64>, Vec<f64>)> = unique_curves.iter().map(|c| (
        c.curve.iter().map(|x| x.0).collect(),
        c.curve.iter().map(|x| posterior::approx_window_log_likelihood(x.1, c.cost_min, c.n_eff)).collect(),
    )).collect();
    let views: Vec<_> = ll.iter().map(|(x, y)| (x.as_slice(), y.as_slice())).collect();
    let (grid, joint) = posterior::combine_windows_on_common_grid(&views, 5.0)?;
    let post = posterior::posterior_decide(&grid, &joint, &posterior::Prior::Uniform)?;
    let width = post.ci95.1 - post.ci95.0;
    log::info!(target: "sync", "[deep-match] short-overlap unique posterior: offset={:.1}ms conf={:.3} width={:.1}ms (candidate only)",
        post.argmax_ms, post.conf_posterior, width);
    let width_gate = dm::post_ci95_base_ms() + dm::drift_tolerance_ms(params.scaled_duration_ms, dm::drift_rate_ms_per_min(), dm::drift_floor_ms());
    if post.conf_posterior < dm::post_conf_min() || width > width_gate { return None; }
    let mut candidates = dm::forward_candidates_for_unique_windows(&unique_curves, dm::fwd_lattice_ms(), dm::fwd_nms_ms(), dm::fwd_top_n());
    // Keep the posterior proposal exact, without counting a neighboring lattice
    // point as another background candidate.
    candidates.retain(|c| (c - post.argmax_ms).abs() > dm::fwd_nms_ms());
    candidates.push(post.argmax_ms);
    let mut finder = FindOffsetsRssync::new(ranges, estimator.sync_results.clone(), sp, params, Arc::new(|_| {}), cancel.clone());
    let bounds = finder.forward_coverage_ms()?;
    let costs = finder.forward_probe_covered(&candidates, dm::fwd_radius_ms(), dm::fwd_step_ms());
    let mut scored: Vec<_> = candidates.into_iter().zip(costs).filter_map(|(center, values)| {
        if values.len() != ranges.len() || values.iter().any(|x| !x.1.is_finite() || x.1 <= 0.0) { return None; }
        let candidate = if values.len() == 1 { values[0].0 } else { center };
        Some((candidate, values.iter().map(|x| x.1).sum::<f64>() / values.len() as f64))
    }).collect();
    scored.sort_by(|a, b| a.1.total_cmp(&b.1));
    let costs: Vec<_> = scored.iter().map(|x| x.1).collect();
    let floor = dm::forward_floor_decision(&costs, dm::fwd_accept_ratio(), dm::fwd_floor_dispersion_max(), dm::fwd_min_candidates());
    log::info!(target: "sync", "[deep-match] short-overlap forward: candidates={} ratio={:.3} dispersion={:.3} decision={:?}", scored.len(), floor.best_ratio, floor.dispersion, floor.decision);
    if cancel.load(Relaxed) || floor.decision != dm::ForwardFloorDecision::Accept { return None; }
    for &(center, cost) in scored.iter().filter(|x| x.1 / floor.floor <= dm::fwd_accept_ratio()).take(dm::fwd_confirm_n()) {
        if cancel.load(Relaxed) { return None; }
        if (center - post.argmax_ms).abs() > dm::fwd_radius_ms() { continue; }
        let Some(radius) = covered_probe_radius(center, bounds, dm::fwd_radius_ms()) else { continue; };
        if radius < dm::fwd_step_ms() { continue; }
        let mut local = sp.clone();
        local.initial_offset = center;
        local.search_size = radius;
        local.calc_initial_fast = false;
        let mut confirmation = FindOffsetsRssync::new(ranges, estimator.sync_results.clone(), &local, params, Arc::new(|_| {}), cancel.clone());
        let result = confirmation.full_sync();
        if result.len() != ranges.len() { continue; }
        let mut offsets: Vec<_> = result.iter().map(|x| x.1).collect();
        offsets.sort_by(f64::total_cmp);
        if offsets.iter().any(|x| !x.is_finite() || *x < bounds.0 || *x > bounds.1 || (*x - center).abs() > radius
            || (*x - post.argmax_ms).abs() > dm::fwd_radius_ms()) { continue; }
        if offsets.last()? - offsets.first()? > dm::spread_max_ms() { continue; }
        let offset = offsets[offsets.len() / 2];
        // Verify the final point again with the same unique forward problem.
        let checked = finder.forward_probe_covered(&[offset], 1.0, 0.5);
        let Some(values) = checked.first().filter(|v| v.len() == ranges.len()) else { continue; };
        let final_cost = values.iter().map(|x| x.1).sum::<f64>() / values.len() as f64;
        if final_cost / floor.floor > dm::fwd_accept_ratio() { continue; }
        log::info!(target: "sync", "[deep-match] short-overlap confirmed: offset={:.3}ms ratio={:.3} coarse_cost={:.4}", offset, final_cost / floor.floor, cost);
        return Some(dm::DeepMatchVerdict::Accepted { offset_ms: offset });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_overlap_scope_uses_physical_duration_and_actual_samples() {
        assert!(is_short(2880.0, 2500.0));
        assert!(is_short(5000.0, 2500.0));
        for d in [5000.001, 11520.0, 0.0, f64::NAN] { assert!(!is_short(d, 2500.0)); }
        let measured = [5, 195, 1000, 1965, 2685].into_iter().collect();
        let (ranges, duplicates) = unique_ranges(&[(0, 1970), (190, 2690)], &measured).unwrap();
        assert_eq!(ranges, [(0, 2690)]);
        assert_eq!(duplicates, 3);
        assert!(unique_ranges(&[(0, 190), (190, 2690)], &measured).is_none());
        assert!(unique_ranges(&[(0, 190), (100, 195)], &measured).is_none());
    }

    #[test]
    fn short_overlap_duplicate_windows_do_not_duplicate_measurements() {
        let measured = (0..100).collect();
        let one = unique_ranges(&[(0, 100), (0, 100)], &measured).unwrap().0;
        let many = unique_ranges(&[(0, 100); 5], &measured).unwrap().0;
        assert_eq!(one, many);
        assert_eq!(one, [(0, 100)]);
    }

    #[test]
    fn short_overlap_rejects_partial_and_negative_coverage() {
        let item = |t: f64| TimeIMU { timestamp_ms: t, gyro: Some([t, 0.0, 0.0]), ..Default::default() };
        let gyro: BTreeMap<_, _> = (0..=10).map(|i| (i * 1000, item(i as f64))).collect();
        let of: Vec<_> = (0..=10).map(|i| item(i as f64)).collect();
        assert_eq!(fully_covered_cost(0.0, &of, &gyro), 0.0);
        assert_eq!(fully_covered_cost(-1.0, &of, &gyro), f64::MAX);
        assert_eq!(fully_covered_cost(1.0, &of, &gyro), f64::MAX);
        assert_eq!(covered_probe_radius(5.0, (0.0, 10.0), 100.0), Some(5.0));
        assert_eq!(covered_probe_radius(1.0, (0.0, 10.0), 100.0), Some(1.0));
        assert_eq!(covered_probe_radius(0.0, (0.0, 10.0), 100.0), None);
        assert_eq!(covered_probe_radius(-1.0, (0.0, 10.0), 100.0), None);
    }

    #[test]
    fn short_overlap_known_rotation_is_confirmed_from_one_unique_window() {
        use crate::gyro_source::Quat64;
        use crate::synchronization::{FrameResult, optical_flow::OpticalFlowMethod};
        use crate::stabilization::distortion_models::DistortionModel;
        use nalgebra::Vector3;
        let _guard = dm::TEST_MTX.lock().unwrap();
        let estimator = PoseEstimator::default();
        let mut cp = ComputeParams::default();
        cp.width = 640; cp.height = 480;
        cp.scaled_fps = 50.0; cp.scaled_duration_ms = 3000.0;
        cp.lens.calib_dimension.w = 640; cp.lens.calib_dimension.h = 480;
        cp.lens.fisheye_params.camera_matrix = vec![[500.0, 0.0, 320.0], [0.0, 500.0, 240.0], [0.0, 0.0, 1.0]];
        cp.lens.global_shutter = true;
        cp.distortion_model = DistortionModel::from_name("opencv_standard");
        let angle = |ms: f64| { let t = ms / 1000.0; 0.17 * (2.3 * t).sin() + 0.11 * (3.9 * t + 0.013 * t * t).sin() };
        let rate = |ms: f64| { let t = ms / 1000.0; (0.391 * (2.3 * t).cos() + 0.11 * (3.9 + 0.026 * t) * (3.9 * t + 0.013 * t * t).cos()).to_degrees() };
        let q = |ms| Quat64::from_scaled_axis(Vector3::new(0.0, 0.0, angle(ms)));
        {
            let mut gyro = cp.gyro.write();
            gyro.file_metadata.write().raw_imu = (0..=12000).map(|i| TimeIMU {
                timestamp_ms: i as f64 * 10.0, gyro: Some([0.0, 0.0, rate(i as f64 * 10.0)]), ..Default::default()
            }).collect();
            gyro.quaternions = (0..=12000).map(|i| (i * 10000, q(i as f64 * 10.0))).collect();
        }
        let rot = Quat64::from_scaled_axis(Vector3::new(std::f64::consts::PI, 0.0, 0.0));
        let image = Arc::new(image::GrayImage::new(640, 480));
        for i in 0..150 {
            let ts = i * 20000;
            let ms = i as f64 * 20.0;
            estimator.estimated_gyro.write().insert(ts, TimeIMU {
                timestamp_ms: ms, gyro: Some([0.0, 0.0, rate(ms + 60000.0) + 0.001]), ..Default::default()
            });
            let mut a = Vec::new(); let mut b = Vec::new();
            for k in 0..24 {
                let world = Vector3::new((k as f64 * 0.37).sin() * 0.4, (k as f64 * 0.73).cos() * 0.3, -1.0);
                let project = |time: f64| {
                    let ray = (q(time) * rot).inverse().transform_vector(&world);
                    ((320.0 + 500.0 * ray.x / ray.z) as f32, (240.0 + 500.0 * ray.y / ray.z) as f32)
                };
                a.push(project(ms + 60000.0));
                b.push(project(ms + 60020.0));
            }
            estimator.sync_results.write().insert(ts, FrameResult {
                of_method: OpticalFlowMethod::detect_features(2, ts, image.clone(), None, 640, 480, 640),
                frame_no: i as usize, timestamp_us: ts, gyro_timestamp_us: ts, frame_size: (640, 480),
                rotation: None, quat: None, euler: None,
                optical_flow: std::cell::RefCell::new(BTreeMap::from([(1, Some(((ts, a), (ts + 20000, b))))])),
            });
        }
        let ranges = [(0, 2250000), (750000, 3000000)];
        let sp = SyncParams { time_per_syncpoint: 2500.0, initial_offset: -58500.0, search_size: 70000.0, ..Default::default() };
        dm::arm(2); dm::arm_forward();
        for i in 0..2 {
            dm::record_curve(dm::DeepMatchWindowCurve { range_idx: i, t_center_ms: 1500.0,
                argmin_ms: -60000.0, cost_min: 1.0, n_eff: 100.0, curve: vec![(-60000.0, 1.0)] });
        }
        let result = validate(&estimator, &ranges, &sp, &cp, &Arc::new(AtomicBool::new(false)));
        dm::take();
        match result {
            Some(dm::DeepMatchVerdict::Accepted { offset_ms }) => assert!((offset_ms + 60000.0).abs() < 3.0, "{offset_ms}"),
            other => panic!("known rotation must remain matchable: {other:?}"),
        }
    }
}
