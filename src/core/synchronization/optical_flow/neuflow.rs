// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

#![allow(unused_variables, dead_code)]
use super::super::OpticalFlowPair;
#[cfg(feature = "use-opencv")]
use super::OFOpenCVDis;
use super::{OpticalFlowMethod, OpticalFlowTrait};

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

pub struct OFNeuFlowV2 {
    timestamp_us: i64,
    pub(crate) rgb_frame: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    /// Sampled feature points from the most recent NeuFlow inference.
    features: Arc<std::cell::UnsafeCell<Vec<(f32, f32)>>>,
    /// Cache of optical flow results, keyed by target timestamp.
    /// Prevents re-inference when cache_optical_flow() calls optical_flow_to() again
    /// after process_detected_frames() has already cleaned up the RGB frames.
    matched_points: Arc<parking_lot::RwLock<std::collections::BTreeMap<i64, (Vec<(f32, f32)>, Vec<(f32, f32)>)>>>,
    used: Arc<AtomicU32>,
}

// Safety: UnsafeCell<Vec<(f32, f32)>> is only mutated during sync processing (optical_flow_to)
// and only read after processing is complete (features()). No concurrent read+write.
unsafe impl Send for OFNeuFlowV2 {}
unsafe impl Sync for OFNeuFlowV2 {}

impl Clone for OFNeuFlowV2 {
    fn clone(&self) -> Self {
        Self {
            timestamp_us: self.timestamp_us,
            rgb_frame: self.rgb_frame.clone(),
            width: self.width,
            height: self.height,
            features: self.features.clone(),
            matched_points: self.matched_points.clone(),
            used: self.used.clone(),
        }
    }
}

impl OFNeuFlowV2 {
    pub fn new(
        timestamp_us: i64,
        rgb_frame: Arc<Vec<u8>>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            timestamp_us,
            rgb_frame,
            width,
            height,
            features: Arc::new(std::cell::UnsafeCell::new(Vec::new())),
            matched_points: Default::default(),
            used: Default::default(),
        }
    }
}

impl OpticalFlowTrait for OFNeuFlowV2 {
    fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn features(&self) -> &Vec<(f32, f32)> {
        // Safety: see UnsafeCell safety comment on the struct definition.
        unsafe { &*self.features.get() }
    }

    fn optical_flow_to(&self, _to: &OpticalFlowMethod) -> OpticalFlowPair {
        #[cfg(feature = "neuflow")]
        if let OpticalFlowMethod::OFNeuFlowV2(next) = _to {
            // Return cached result if available (needed because cache_optical_flow
            // calls this again after process_detected_frames has cleaned up RGB frames)
            if let Some(cached) = self.matched_points.read().get(&next.timestamp_us) {
                return Some(cached.clone());
            }

            if self.rgb_frame.is_empty() || next.rgb_frame.is_empty() {
                return fallback_to_dis(self, next);
            }

            let t_start = std::time::Instant::now();

            match neuflow_inference(
                &self.rgb_frame, self.width, self.height,
                &next.rgb_frame, next.width, next.height,
            ) {
                Ok((sampled_flow, gray_data)) => {
                    let t_infer = std::time::Instant::now();

                    // Letterbox parameters (must match preprocess_frame)
                    let model_w = 640.0f32;
                    let model_h = 480.0f32;
                    let scale = (model_w / self.width as f32).min(model_h / self.height as f32);
                    let new_w = self.width as f32 * scale;
                    let new_h = self.height as f32 * scale;
                    let pad_left = (model_w - new_w) / 2.0;
                    let pad_top = (model_h - new_h) / 2.0;

                    let result = filter_sampled_flow(sampled_flow, gray_data.as_slice(), model_w as u32, model_h as u32);
                    let t_sample = std::time::Instant::now();

                    if let Some((ref from_pts, ref to_pts)) = result {
                        // Remove padding offset and scale back to original resolution
                        let from_scaled: Vec<(f32, f32)> = from_pts.iter()
                            .filter(|(x, y)| *x >= pad_left && *x < pad_left + new_w && *y >= pad_top && *y < pad_top + new_h)
                            .map(|(x, y)| ((x - pad_left) / scale, (y - pad_top) / scale))
                            .collect();
                        let to_scaled: Vec<(f32, f32)> = to_pts.iter()
                            .zip(from_pts.iter())
                            .filter(|(_, (x, y))| *x >= pad_left && *x < pad_left + new_w && *y >= pad_top && *y < pad_top + new_h)
                            .map(|((tx, ty), _)| ((tx - pad_left) / scale, (ty - pad_top) / scale))
                            .collect();

                        let t_scale = std::time::Instant::now();

                        let infer_ms = (t_infer - t_start).as_secs_f64() * 1000.0;
                        let sample_ms = (t_sample - t_infer).as_secs_f64() * 1000.0;
                        let scale_ms = (t_scale - t_sample).as_secs_f64() * 1000.0;
                        let total_ms = (t_scale - t_start).as_secs_f64() * 1000.0;
                        log::debug!("[NeuFlow perf] of_to: inference={infer_ms:.1}ms sample={sample_ms:.1}ms scale={scale_ms:.1}ms total={total_ms:.1}ms pts={}", from_scaled.len());

                        if from_scaled.len() >= 10 {
                            unsafe { *self.features.get() = from_scaled.clone(); }
                            self.matched_points.write().insert(
                                next.timestamp_us,
                                (from_scaled.clone(), to_scaled.clone()),
                            );
                            self.used.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            next.used.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            return Some((from_scaled, to_scaled));
                        }
                    }
                    log::warn!("NeuFlow: too few points, falling back to DIS");
                    return fallback_to_dis(self, next);
                }
                Err(e) => {
                    log::warn!("NeuFlow inference failed: {e}, falling back to DIS");
                    return fallback_to_dis(self, next);
                }
            }
        }
        None
    }

    fn can_cleanup(&self) -> bool {
        self.used.load(std::sync::atomic::Ordering::SeqCst) == 2
    }

    fn has_data(&self) -> bool {
        !self.rgb_frame.is_empty()
    }

    fn cleanup(&mut self) {
        self.rgb_frame = Arc::new(Vec::new());
    }
}

#[cfg(feature = "neuflow")]
fn neuflow_inference(
    rgb0: &[u8], w0: u32, h0: u32,
    rgb1: &[u8], w1: u32, h1: u32,
) -> Result<(crate::neuflow::SampledFlow, Vec<u8>), String> {
    let t0 = std::time::Instant::now();

    let (img0, gray0, proc_h, proc_w) = preprocess_frame(rgb0, w0, h0)?;
    let (img1, _, _, _) = preprocess_frame(rgb1, w1, h1)?;

    let t1 = std::time::Instant::now();

    let t_grid_start = std::time::Instant::now();
    let (grid_points, linear_indices) = build_sample_grid(&gray0, proc_w, proc_h);
    let t_grid_done = std::time::Instant::now();

    if grid_points.is_empty() {
        return Err("NeuFlow sample grid is empty".to_string());
    }

    // Run sparse inference via Burn (zero-copy: owned Vecs moved into channel)
    let sampled = crate::neuflow::infer_and_sample(img0, img1, proc_h, proc_w, grid_points, linear_indices)?;

    let t2 = std::time::Instant::now();
    let preprocess_ms = (t1 - t0).as_secs_f64() * 1000.0;
    let grid_ms = (t_grid_done - t1).as_secs_f64() * 1000.0;
    let infer_ms = (t2 - t_grid_done).as_secs_f64() * 1000.0;
    let total_ms = (t2 - t0).as_secs_f64() * 1000.0;

    if (t2 - t1).as_secs() > 60 {
        return Err(format!("NeuFlow inference timeout: {infer_ms:.0}ms"));
    }

    log::debug!(
        "[NeuFlow perf] preprocess={preprocess_ms:.1}ms grid={grid_ms:.1}ms infer_sample={infer_ms:.1}ms total={total_ms:.1}ms ({}x{}) points={}",
        proc_w,
        proc_h,
        sampled.grid_points.len()
    );
    Ok((sampled, gray0))
}

#[cfg(feature = "neuflow")]
fn preprocess_frame(rgb: &[u8], width: u32, height: u32) -> Result<(Vec<f32>, Vec<u8>, usize, usize), String> {
    let w = width as usize;
    let h = height as usize;

    if rgb.len() < w * h * 3 {
        return Err(format!("RGB buffer too small: {} < {}", rgb.len(), w * h * 3));
    }

    // ONNX model fixed at 480x640. Letterbox: fit within, maintain aspect ratio.
    let target_h = 480usize;
    let target_w = 640usize;

    let scale = (target_w as f32 / w as f32).min(target_h as f32 / h as f32);
    let new_w = (w as f32 * scale) as usize;
    let new_h = (h as f32 * scale) as usize;

    // Pad offset (center the image)
    let pad_top = (target_h - new_h) / 2;
    let pad_left = (target_w - new_w) / 2;

    // Fused resize + letterbox + CHW conversion + grayscale extraction
    // Uses bilinear interpolation directly on the source buffer (no intermediate allocation)
    let inv_scale = 1.0 / scale;
    let mut chw = vec![0.0f32; 3 * target_h * target_w];
    let mut gray = vec![0u8; target_h * target_w];
    let plane = target_h * target_w;

    for out_y in pad_top..(pad_top + new_h) {
        let src_yf = (out_y - pad_top) as f32 * inv_scale;
        let sy0 = (src_yf as usize).min(h - 1);
        let sy1 = (sy0 + 1).min(h - 1);
        let fy = src_yf - sy0 as f32;

        for out_x in pad_left..(pad_left + new_w) {
            let src_xf = (out_x - pad_left) as f32 * inv_scale;
            let sx0 = (src_xf as usize).min(w - 1);
            let sx1 = (sx0 + 1).min(w - 1);
            let fx = src_xf - sx0 as f32;

            // Bilinear interpolation weights
            let w00 = (1.0 - fx) * (1.0 - fy);
            let w10 = fx * (1.0 - fy);
            let w01 = (1.0 - fx) * fy;
            let w11 = fx * fy;

            let i00 = (sy0 * w + sx0) * 3;
            let i10 = (sy0 * w + sx1) * 3;
            let i01 = (sy1 * w + sx0) * 3;
            let i11 = (sy1 * w + sx1) * 3;

            let out_idx = out_y * target_w + out_x;
            for c in 0..3 {
                let v = rgb[i00 + c] as f32 * w00
                      + rgb[i10 + c] as f32 * w10
                      + rgb[i01 + c] as f32 * w01
                      + rgb[i11 + c] as f32 * w11;
                chw[c * plane + out_idx] = v;
            }
            gray[out_idx] = chw[out_idx] as u8; // R channel as grayscale
        }
    }

    Ok((chw, gray, target_h, target_w))
}

/// Build a texture-aware sampling grid in model space.
fn build_sample_grid(gray: &[u8], width: usize, height: usize) -> (Vec<(usize, usize)>, Vec<i32>) {
    let step = (width / 15).max(4);
    let window_size = ((width as f32 * 0.02).round() as usize).max(10);
    let texture_threshold = 3.0;

    let mut grid_points = Vec::new();
    let mut linear_indices = Vec::new();

    for x in (0..width).step_by(step) {
        for y in (0..height).step_by(step) {
            let variance = texture_variance(gray, x, y, width, height, window_size);
            if variance < texture_threshold {
                continue;
            }
            grid_points.push((x, y));
            linear_indices.push((y * width + x) as i32);
        }
    }

    (grid_points, linear_indices)
}

/// Filter sparse sampled flow results and produce matched point pairs.
fn filter_sampled_flow(sampled: crate::neuflow::SampledFlow, gray: &[u8], width: u32, height: u32) -> OpticalFlowPair {
    let w = width as usize;
    let h = height as usize;
    let plane = w * h;

    if gray.len() < plane {
        return None;
    }

    let mut from_pts = Vec::new();
    let mut to_pts = Vec::new();

    if sampled.grid_points.len() != sampled.dx.len() || sampled.grid_points.len() != sampled.dy.len() {
        return None;
    }

    for (i, &(x, y)) in sampled.grid_points.iter().enumerate() {
        let dx = sampled.dx[i];
        let dy = sampled.dy[i];

        if !dx.is_finite() || !dy.is_finite() {
            continue;
        }

        let to_x = x as f32 + dx;
        let to_y = y as f32 + dy;

        if to_x >= 0.0 && to_x < w as f32 && to_y >= 0.0 && to_y < h as f32 {
            from_pts.push((x as f32, y as f32));
            to_pts.push((to_x, to_y));
        }
    }

    if from_pts.len() < 10 {
        return None;
    }

    // Median-consistency filter: reject points whose flow deviates too far
    // from the median. This removes non-rigid motion (water, reflections)
    // while preserving the dominant rigid camera motion.
    let mut dxs: Vec<f32> = from_pts.iter().zip(to_pts.iter()).map(|(f, t)| t.0 - f.0).collect();
    let mut dys: Vec<f32> = from_pts.iter().zip(to_pts.iter()).map(|(f, t)| t.1 - f.1).collect();
    dxs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    dys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_dx = dxs[dxs.len() / 2];
    let median_dy = dys[dys.len() / 2];

    let max_dev = 2.0f32;
    let mut filtered_from = Vec::new();
    let mut filtered_to = Vec::new();
    for (f, t) in from_pts.iter().zip(to_pts.iter()) {
        let dx = t.0 - f.0;
        let dy = t.1 - f.1;
        if (dx - median_dx).abs() <= max_dev && (dy - median_dy).abs() <= max_dev {
            filtered_from.push(*f);
            filtered_to.push(*t);
        }
    }

    if filtered_from.len() >= 10 {
        Some((filtered_from, filtered_to))
    } else {
        None
    }
}

/// Compute grayscale variance in a patch around (x, y).
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

/// Fallback: convert RGB frames to grayscale and use OpenCV DIS for dense optical flow.
#[cfg(feature = "use-opencv")]
fn fallback_to_dis(self_frame: &OFNeuFlowV2, next_frame: &OFNeuFlowV2) -> OpticalFlowPair {
    if self_frame.rgb_frame.is_empty() || next_frame.rgb_frame.is_empty() {
        log::warn!("NeuFlow fallback: one or both RGB frames are empty");
        return None;
    }
    if self_frame.width == 0 || self_frame.height == 0 {
        return None;
    }

    let gray1 = rgb_to_gray(&self_frame.rgb_frame, self_frame.width, self_frame.height);
    let gray2 = rgb_to_gray(&next_frame.rgb_frame, next_frame.width, next_frame.height);

    let dis1 = OFOpenCVDis::detect_features(
        self_frame.timestamp_us,
        Arc::new(gray1),
        self_frame.width,
        self_frame.height,
    );
    let dis2 = OFOpenCVDis::detect_features(
        next_frame.timestamp_us,
        Arc::new(gray2),
        next_frame.width,
        next_frame.height,
    );

    let result = dis1.optical_flow_to(&OpticalFlowMethod::OFOpenCVDis(dis2));

    if result.is_some() {
        self_frame.used.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        next_frame.used.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    result
}

#[cfg(not(feature = "use-opencv"))]
fn fallback_to_dis(_self_frame: &OFNeuFlowV2, _next_frame: &OFNeuFlowV2) -> OpticalFlowPair {
    log::warn!("NeuFlow fallback: OpenCV DIS not available (use-opencv feature disabled)");
    None
}

/// Convert an RGB buffer to a grayscale image using standard luminance coefficients.
/// Y = 0.299*R + 0.587*G + 0.114*B
fn rgb_to_gray(rgb: &[u8], width: u32, height: u32) -> image::GrayImage {
    let expected_len = (width * height * 3) as usize;
    if rgb.len() < expected_len {
        log::warn!(
            "NeuFlow rgb_to_gray: buffer too small (expected {}, got {}), returning empty image",
            expected_len,
            rgb.len()
        );
        return image::GrayImage::new(width, height);
    }

    let mut gray = image::GrayImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 3) as usize;
            let r = rgb[idx] as f32;
            let g = rgb[idx + 1] as f32;
            let b = rgb[idx + 2] as f32;
            let luma = (0.299 * r + 0.587 * g + 0.114 * b) as u8;
            gray.put_pixel(x, y, image::Luma([luma]));
        }
    }
    gray
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgb_to_gray_basic() {
        // 2x2 white image
        let rgb = vec![255u8; 2 * 2 * 3];
        let gray = rgb_to_gray(&rgb, 2, 2);
        assert_eq!(gray.width(), 2);
        assert_eq!(gray.height(), 2);
        // White RGB -> gray should be ~255
        for p in gray.pixels() {
            assert!(p.0[0] >= 254); // rounding might cause 254
        }
    }

    #[test]
    fn test_rgb_to_gray_black() {
        let rgb = vec![0u8; 4 * 4 * 3];
        let gray = rgb_to_gray(&rgb, 4, 4);
        for p in gray.pixels() {
            assert_eq!(p.0[0], 0);
        }
    }

    #[test]
    fn test_rgb_to_gray_short_buffer() {
        // Buffer too small — should return empty (all zeros) image
        let rgb = vec![128u8; 5]; // way too small for 4x4
        let gray = rgb_to_gray(&rgb, 4, 4);
        assert_eq!(gray.width(), 4);
        assert_eq!(gray.height(), 4);
    }

    #[test]
    fn test_sample_from_dense_flow_empty() {
    let result = filter_sampled_flow(
        crate::neuflow::SampledFlow { grid_points: vec![], dx: vec![], dy: vec![] },
        &[],
        10,
        10,
    );
    assert!(result.is_none());
}

    #[test]
    fn test_sample_from_dense_flow_uniform() {
        let w = 32u32;
        let h = 32u32;
    let n = (w * h) as usize;
    // Textured gray: alternating pattern to ensure variance > 3.0
    let gray: Vec<u8> = (0..n).map(|i| if i % 2 == 0 { 200 } else { 50 }).collect();
    let (grid_points, _) = build_sample_grid(&gray, w as usize, h as usize);
    let dx = vec![1.0f32; grid_points.len()];
    let dy = vec![0.5f32; grid_points.len()];
    let result = filter_sampled_flow(
        crate::neuflow::SampledFlow { grid_points, dx, dy },
        &gray,
        w,
        h,
    );
    assert!(result.is_some());
    let (from_pts, to_pts) = result.unwrap();
        assert!(!from_pts.is_empty());
        // Verify to = from + flow
        for (f, t) in from_pts.iter().zip(to_pts.iter()) {
            assert!((t.0 - f.0 - 1.0).abs() < 1e-5);
            assert!((t.1 - f.1 - 0.5).abs() < 1e-5);
        }
    }

    #[test]
    fn test_ofneuflowv2_new() {
        let frame = Arc::new(vec![0u8; 100 * 100 * 3]);
        let nf = OFNeuFlowV2::new(12345, frame, 100, 100);
        assert_eq!(nf.size(), (100, 100));
        assert!(nf.features().is_empty());
        assert!(!nf.can_cleanup());
    }

    #[test]
    fn test_ofneuflowv2_cleanup() {
        let frame = Arc::new(vec![128u8; 10 * 10 * 3]);
        let mut nf = OFNeuFlowV2::new(0, frame, 10, 10);
        assert!(!nf.rgb_frame.is_empty());
        nf.cleanup();
        assert!(nf.rgb_frame.is_empty());
    }

    #[test]
    fn test_sample_uniform_flow() {
        // Constant flow [10, 5] everywhere — CHW layout
        let w = 100u32;
        let h = 80u32;
        let n = (w * h) as usize;
        let mut flow_data = vec![0.0f32; 2 * n];
        for i in 0..n {
            flow_data[i] = 10.0;       // dx plane
            flow_data[n + i] = 5.0;    // dy plane
        }
        let gray: Vec<u8> = (0..n).map(|i| if i % 2 == 0 { 200 } else { 50 }).collect();
        let result = sample_from_dense_flow(&flow_data, &gray, w, h);
        assert!(result.is_some(), "uniform flow should produce valid result");
        let (from_pts, to_pts) = result.unwrap();
        assert!(!from_pts.is_empty());
        for (f, t) in from_pts.iter().zip(to_pts.iter()) {
            let dx = t.0 - f.0;
            let dy = t.1 - f.1;
            assert!((dx - 10.0).abs() < 1e-3, "expected dx≈10, got {dx}");
            assert!((dy - 5.0).abs() < 1e-3, "expected dy≈5, got {dy}");
        }
    }

    #[test]
    fn test_sample_low_texture_rejection() {
        // Uniform gray (no texture) should produce no points
        let w = 100u32;
        let h = 80u32;
    let n = (w * h) as usize;
    let gray = vec![128u8; n];
    let (grid_points, _) = build_sample_grid(&gray, w as usize, h as usize);
    assert!(grid_points.is_empty(), "uniform gray should not produce sample points");
    let result = filter_sampled_flow(
        crate::neuflow::SampledFlow { grid_points, dx: vec![], dy: vec![] },
        &gray,
        w,
        h,
    );
    assert!(result.is_none(), "uniform gray should produce no points (low texture)");
}

    #[test]
    fn test_rgb_to_gray() {
        // 3 pixels: pure R, pure G, pure B
        let rgb = vec![
            255, 0, 0,   // Red
            0, 255, 0,   // Green
            0, 0, 255,   // Blue
        ];
        let gray = rgb_to_gray(&rgb, 3, 1);
        assert_eq!(gray.width(), 3);
        assert_eq!(gray.height(), 1);

        let r_gray = gray.get_pixel(0, 0).0[0]; // R → 0.299*255 ≈ 76
        let g_gray = gray.get_pixel(1, 0).0[0]; // G → 0.587*255 ≈ 149
        let b_gray = gray.get_pixel(2, 0).0[0]; // B → 0.114*255 ≈ 29

        assert!((r_gray as i32 - 76).abs() <= 1, "Red channel gray: expected ~76, got {r_gray}");
        assert!((g_gray as i32 - 149).abs() <= 1, "Green channel gray: expected ~149, got {g_gray}");
        assert!((b_gray as i32 - 29).abs() <= 1, "Blue channel gray: expected ~29, got {b_gray}");
    }

    #[test]
    fn test_of_method_3_dispatch() {
        // method=3 should create OFNeuFlowV2 variant
        let img = Arc::new(image::GrayImage::new(100, 80));
        let method = OpticalFlowMethod::detect_features(3, 0, img, None, 100, 80);
        assert!(
            matches!(method, OpticalFlowMethod::OFNeuFlowV2(_)),
            "method=3 should produce OFNeuFlowV2"
        );
    }

    #[cfg(feature = "neuflow")]
    #[test]
    fn test_preprocess_frame_perf() {
        let rgb = vec![128u8; 1920 * 1080 * 3];
        let start = std::time::Instant::now();
        let iterations = 100;
        for _ in 0..iterations {
            let _ = preprocess_frame(&rgb, 1920, 1080).unwrap();
        }
        let avg_ms = start.elapsed().as_secs_f64() * 1000.0 / iterations as f64;
        eprintln!("preprocess_frame 1920x1080→480x640 avg: {avg_ms:.2}ms ({iterations} runs)");
        assert!(avg_ms <= 10.0, "preprocess too slow: {avg_ms:.2}ms (target ≤ 10ms)");
    }

    #[test]
    fn test_sample_from_dense_flow_perf() {
        let w = 640u32;
        let h = 480u32;
        let n = (w * h) as usize;
        // CHW layout
    let gray: Vec<u8> = (0..n).map(|i| if i % 2 == 0 { 200 } else { 50 }).collect();
    let (grid_points, _) = build_sample_grid(&gray, w as usize, h as usize);
    let dx = vec![2.0f32; grid_points.len()];
    let dy = vec![1.0f32; grid_points.len()];

    let start = std::time::Instant::now();
    let iterations = 1000;
    for _ in 0..iterations {
        let _ = filter_sampled_flow(
            crate::neuflow::SampledFlow {
                grid_points: grid_points.clone(),
                dx: dx.clone(),
                dy: dy.clone(),
            },
            &gray,
            w,
            h,
        );
    }
    let avg_ms = start.elapsed().as_secs_f64() * 1000.0 / iterations as f64;
    eprintln!("filter_sampled_flow 640x480 avg: {avg_ms:.3}ms ({iterations} runs)");
    assert!(avg_ms <= 1.0, "sample too slow: {avg_ms:.3}ms (target ≤ 1.0ms)");
}
}
