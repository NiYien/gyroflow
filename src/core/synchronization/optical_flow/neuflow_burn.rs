// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

//! Burn/Vulkan backend — accepts preprocessed CHW tensors + gray,
//! uses GPU-side sparse sampling via infer_and_sample.

/// Sampled optical flow result from Burn GPU-side sparse sampling.
pub(super) struct BurnSampledResult {
    pub from_pts: Vec<(f32, f32)>,
    pub to_pts: Vec<(f32, f32)>,
    /// Grid positions iterated before the texture filter (flow-gate stats).
    pub candidates: usize,
}

/// Run Burn inference with GPU-side sparse sampling on preprocessed CHW tensors.
/// chw0/chw1 are moved (owned) into Burn's TensorData for zero-copy.
pub(super) fn neuflow_inference_burn_sampled(
    chw0: Vec<f32>,
    chw1: Vec<f32>,
    gray0: &[u8],
    proc_h: usize,
    proc_w: usize,
    step_div: u32,
) -> Result<BurnSampledResult, String> {
    // Compute texture-aware grid points from gray0
    let (grid_points, linear_indices, candidates) = {
        let _g = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::ComputeGridPoints,
        );
        compute_grid_points(gray0, proc_w, proc_h, step_div)
    };

    if grid_points.is_empty() {
        return Err("No textured grid points found".to_string());
    }

    let sampled = {
        let _g = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::InferAndSample,
        );
        crate::neuflow_burn::infer_and_sample(
            chw0,
            chw1,
            proc_h,
            proc_w,
            grid_points,
            linear_indices,
        )?
    };

    // Convert SampledFlow to point pairs
    let mut from_pts = Vec::with_capacity(sampled.grid_points.len());
    let mut to_pts = Vec::with_capacity(sampled.grid_points.len());
    for (i, &(gx, gy)) in sampled.grid_points.iter().enumerate() {
        let fx = gx as f32;
        let fy = gy as f32;
        if !sampled.dx[i].is_finite() || !sampled.dy[i].is_finite() {
            continue;
        }
        let tx = fx + sampled.dx[i];
        let ty = fy + sampled.dy[i];
        if tx >= 0.0 && tx < proc_w as f32 && ty >= 0.0 && ty < proc_h as f32 {
            from_pts.push((fx, fy));
            to_pts.push((tx, ty));
        }
    }

    Ok(BurnSampledResult {
        from_pts,
        to_pts,
        candidates,
    })
}

/// Sample the flow field img0→img1 at arbitrary fractional positions,
/// rounded to the nearest pixel (clamped to bounds). Used by the FB gate to
/// read the backward flow at forward endpoints without a dense readback —
/// `infer_and_sample` already supports arbitrary linear indices.
/// Returns a map keyed by the rounded (x, y) pixel.
pub(super) fn sample_flow_at_points(
    img0: Vec<f32>,
    img1: Vec<f32>,
    proc_h: usize,
    proc_w: usize,
    query: &[(f32, f32)],
) -> Result<std::collections::HashMap<(i32, i32), (f32, f32)>, String> {
    let mut grid_points = Vec::with_capacity(query.len());
    let mut linear_indices = Vec::with_capacity(query.len());
    let mut seen = std::collections::HashSet::with_capacity(query.len());
    for &(x, y) in query {
        if !x.is_finite() || !y.is_finite() {
            continue;
        }
        let xi = (x.round() as i64).clamp(0, proc_w as i64 - 1) as usize;
        let yi = (y.round() as i64).clamp(0, proc_h as i64 - 1) as usize;
        if seen.insert((xi, yi)) {
            grid_points.push((xi, yi));
            linear_indices.push((yi * proc_w + xi) as i32);
        }
    }
    if grid_points.is_empty() {
        return Ok(Default::default());
    }
    let sampled = {
        let _g = crate::synchronization::sync_perf::StageGuard::new(
            crate::synchronization::sync_perf::Stage::InferAndSample,
        );
        crate::neuflow_burn::infer_and_sample(
            img0,
            img1,
            proc_h,
            proc_w,
            grid_points,
            linear_indices,
        )?
    };
    let mut map = std::collections::HashMap::with_capacity(sampled.grid_points.len());
    for (i, &(gx, gy)) in sampled.grid_points.iter().enumerate() {
        if sampled.dx[i].is_finite() && sampled.dy[i].is_finite() {
            map.insert((gx as i32, gy as i32), (sampled.dx[i], sampled.dy[i]));
        }
    }
    Ok(map)
}

/// Compute texture-aware grid points for GPU sparse sampling.
/// Returns (grid_points, linear_indices, candidates-before-texture-filter).
fn compute_grid_points(
    gray: &[u8],
    w: usize,
    h: usize,
    step_div: u32,
) -> (Vec<(usize, usize)>, Vec<i32>, usize) {
    // Looser threshold than DIS (3.0): NeuFlow's learned spatial prior can
    // estimate flow at low-texture points (propagates from nearby textured
    // regions), unlike DIS which suffers aperture problem there. Keep more
    // samples so RANSAC inlier set is larger.
    let step = (w / (step_div.max(1) as usize)).max(4);
    let window_size = ((w as f32 * 0.02).round() as usize).max(10);
    let texture_threshold = 1.0;

    let mut grid_points = Vec::new();
    let mut linear_indices = Vec::new();
    let mut candidates = 0usize;

    for x in (0..w).step_by(step) {
        for y in (0..h).step_by(step) {
            candidates += 1;
            let variance = texture_variance(gray, x, y, w, h, window_size);
            if variance < texture_threshold {
                continue;
            }
            grid_points.push((x, y));
            linear_indices.push((y * w + x) as i32);
        }
    }

    (grid_points, linear_indices, candidates)
}

fn texture_variance(gray: &[u8], x: usize, y: usize, w: usize, h: usize, patch: usize) -> f32 {
    let half = patch / 2;
    let x0 = x.saturating_sub(half);
    let y0 = y.saturating_sub(half);
    let x1 = (x + half + 1).min(w);
    let y1 = (y + half + 1).min(h);

    let mut sum = 0.0f32;
    let mut sum_sq = 0.0f32;
    let mut count = 0.0f32;
    for py in y0..y1 {
        for px in x0..x1 {
            let v = gray[py * w + px] as f32;
            sum += v;
            sum_sq += v * v;
            count += 1.0;
        }
    }
    if count == 0.0 {
        return 0.0;
    }
    let mean = sum / count;
    sum_sq / count - mean * mean
}
