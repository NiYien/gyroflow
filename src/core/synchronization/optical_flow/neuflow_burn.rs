// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

//! Burn/Vulkan backend — accepts preprocessed CHW tensors + gray,
//! uses GPU-side sparse sampling via infer_and_sample.

/// Sampled optical flow result from Burn GPU-side sparse sampling.
pub(super) struct BurnSampledResult {
    pub from_pts: Vec<(f32, f32)>,
    pub to_pts: Vec<(f32, f32)>,
}

/// Run Burn inference with GPU-side sparse sampling on preprocessed CHW tensors.
/// chw0/chw1 are moved (owned) into Burn's TensorData for zero-copy.
pub(super) fn neuflow_inference_burn_sampled(
    chw0: Vec<f32>, chw1: Vec<f32>, gray0: &[u8], proc_h: usize, proc_w: usize,
) -> Result<BurnSampledResult, String> {
    // Compute texture-aware grid points from gray0
    let (grid_points, linear_indices) = compute_grid_points(gray0, proc_w, proc_h);

    if grid_points.is_empty() {
        return Err("No textured grid points found".to_string());
    }

    let start = std::time::Instant::now();

    let sampled = crate::neuflow_burn::infer_and_sample(
        chw0, chw1, proc_h, proc_w,
        grid_points, linear_indices,
    )?;

    let elapsed = start.elapsed();
    log::debug!("NeuFlow Burn inference+sample: {}x{} ({} pts) in {:?}",
        proc_w, proc_h, sampled.grid_points.len(), elapsed);

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

    Ok(BurnSampledResult { from_pts, to_pts })
}

/// Compute texture-aware grid points for GPU sparse sampling.
fn compute_grid_points(gray: &[u8], w: usize, h: usize) -> (Vec<(usize, usize)>, Vec<i32>) {
    let step = (w / 15).max(4);
    let window_size = ((w as f32 * 0.02).round() as usize).max(10);
    let texture_threshold = 3.0;

    let mut grid_points = Vec::new();
    let mut linear_indices = Vec::new();

    for x in (0..w).step_by(step) {
        for y in (0..h).step_by(step) {
            let variance = texture_variance(gray, x, y, w, h, window_size);
            if variance < texture_threshold {
                continue;
            }
            grid_points.push((x, y));
            linear_indices.push((y * w + x) as i32);
        }
    }

    (grid_points, linear_indices)
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
    if count == 0.0 { return 0.0; }
    let mean = sum / count;
    sum_sq / count - mean * mean
}
