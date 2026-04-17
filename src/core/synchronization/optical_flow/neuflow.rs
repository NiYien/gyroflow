// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

#![allow(unused_variables, dead_code)]
use super::super::OpticalFlowPair;
use super::{OFOpenCVDis, OpticalFlowMethod, OpticalFlowTrait};

use std::sync::Arc;
use std::sync::atomic::AtomicU32;

/// Cached result of preprocess_frame_nv12: (chw, gray, proc_h, proc_w).
type PreprocessedFrame = (Vec<f32>, Vec<u8>, usize, usize);

pub struct OFNeuFlowV2 {
    timestamp_us: i64,
    /// Raw NV12 frame data (Y plane + interleaved UV plane).
    /// Fused NV12→CHW conversion in preprocess avoids intermediate RGB allocation.
    pub(crate) nv12_frame: Arc<Vec<u8>>,
    width: u32,
    height: u32,
    stride: usize,
    /// Backend selector: 3 = ORT/CUDA, 4 = Burn/Vulkan
    backend: u32,
    /// Cached NV12→CHW preprocessing result. Avoids recomputing when the same
    /// frame appears as img0 in one pair and img1 in another.
    preprocessed: Arc<parking_lot::Mutex<Option<PreprocessedFrame>>>,
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
            nv12_frame: self.nv12_frame.clone(),
            width: self.width,
            height: self.height,
            stride: self.stride,
            backend: self.backend,
            preprocessed: self.preprocessed.clone(),
            features: self.features.clone(),
            matched_points: self.matched_points.clone(),
            used: self.used.clone(),
        }
    }
}

impl OFNeuFlowV2 {
    pub fn new(
        timestamp_us: i64,
        nv12_frame: Arc<Vec<u8>>,
        width: u32,
        height: u32,
        stride: usize,
        backend: u32,
    ) -> Self {
        Self {
            timestamp_us,
            nv12_frame,
            width,
            height,
            stride,
            backend,
            preprocessed: Default::default(),
            features: Arc::new(std::cell::UnsafeCell::new(Vec::new())),
            matched_points: Default::default(),
            used: Default::default(),
        }
    }

    /// Get cached preprocessed CHW tensor + gray, or compute from NV12 and cache.
    /// Thread-safe: Mutex ensures only one thread computes; others wait and get the cached result.
    #[cfg(any(feature = "neuflow-ort", feature = "neuflow-burn"))]
    fn get_or_preprocess(&self) -> Result<PreprocessedFrame, String> {
        let mut cache = self.preprocessed.lock();
        if let Some(ref cached) = *cache {
            return Ok(cached.clone());
        }
        let result = preprocess_frame_nv12(&self.nv12_frame, self.width, self.height, self.stride)?;
        *cache = Some(result.clone());
        Ok(result)
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
        #[cfg(any(feature = "neuflow-ort", feature = "neuflow-burn"))]
        if let OpticalFlowMethod::OFNeuFlowV2(next) = _to {
            // Return cached result if available (needed because cache_optical_flow
            // calls this again after process_detected_frames has cleaned up frames)
            if let Some(cached) = self.matched_points.read().get(&next.timestamp_us) {
                return Some(cached.clone());
            }

            // Preprocess both frames (cached — each frame preprocessed at most once).
            // Uses cache if available, even after cleanup() has freed the NV12 data.
            let (chw0, gray0, proc_h, proc_w) = {
                let _g = crate::synchronization::sync_perf::StageGuard::new(
                    crate::synchronization::sync_perf::Stage::PreprocessNv12,
                );
                match self.get_or_preprocess() {
                    Ok(v) => v,
                    Err(e) => { log::warn!("NeuFlow preprocess failed: {e}, falling back to DIS"); return fallback_to_dis(self, next); }
                }
            };
            let (chw1, _, _, _) = {
                let _g = crate::synchronization::sync_perf::StageGuard::new(
                    crate::synchronization::sync_perf::Stage::PreprocessNv12,
                );
                match next.get_or_preprocess() {
                    Ok(v) => v,
                    Err(e) => { log::warn!("NeuFlow preprocess failed: {e}, falling back to DIS"); return fallback_to_dis(self, next); }
                }
            };

            let model_w = proc_w as f32;
            let model_h = proc_h as f32;
            let scale = (model_w / self.width as f32).min(model_h / self.height as f32);
            let new_w = self.width as f32 * scale;
            let new_h = self.height as f32 * scale;
            let pad_left = (model_w - new_w) / 2.0;
            let pad_top = (model_h - new_h) / 2.0;

            // Dispatch to backend-specific inference
            let result = match self.backend {
                #[cfg(feature = "neuflow-ort")]
                3 => {
                    // ORT: full tensor readback → CPU dense sampling
                    match super::neuflow_ort::neuflow_inference_ort(&chw0, &chw1, proc_h, proc_w) {
                        Ok(flow_data) => {
                            sample_from_dense_flow(&flow_data, &gray0, proc_w as u32, proc_h as u32)
                        }
                        Err(e) => {
                            log::warn!("NeuFlow ORT inference failed: {e}, falling back to DIS");
                            return fallback_to_dis(self, next);
                        }
                    }
                }
                #[cfg(feature = "neuflow-burn")]
                4 => {
                    // Burn: GPU-side sparse sampling (no full tensor readback)
                    match super::neuflow_burn::neuflow_inference_burn_sampled(
                        chw0, chw1, &gray0, proc_h, proc_w,
                    ) {
                        Ok(sampled) => {
                            if sampled.from_pts.len() >= 10 {
                                Some(median_filter_points(sampled.from_pts, sampled.to_pts))
                            } else {
                                None
                            }
                        }
                        Err(e) => {
                            log::warn!("NeuFlow Burn inference failed: {e}, falling back to DIS");
                            return fallback_to_dis(self, next);
                        }
                    }
                }
                _ => {
                    log::warn!("Unknown NeuFlow backend: {}, falling back to DIS", self.backend);
                    return fallback_to_dis(self, next);
                }
            };

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
        None
    }

    fn can_cleanup(&self) -> bool {
        self.used.load(std::sync::atomic::Ordering::SeqCst) == 2
    }

    fn has_data(&self) -> bool {
        !self.nv12_frame.is_empty()
    }

    fn cleanup(&mut self) {
        self.nv12_frame = Arc::new(Vec::new());
        // Note: preprocessed cache is intentionally kept — other threads in par_iter
        // may still need the CHW tensor after NV12 is freed.
    }
}

/// Fused NV12→CHW preprocessing: reads NV12 data directly and produces
/// model-ready CHW float32 tensor + grayscale in a single pass.
///
/// This eliminates the intermediate RGB buffer allocation (w×h×3) and converts
/// only the output-resolution pixels (640×480) instead of all source pixels.
/// For a 960×540 source, this is ~60% fewer pixel operations.
#[cfg(any(feature = "neuflow-ort", feature = "neuflow-burn"))]
pub(super) fn preprocess_frame_nv12(nv12: &[u8], width: u32, height: u32, stride: usize) -> Result<(Vec<f32>, Vec<u8>, usize, usize), String> {
    let w = width as usize;
    let h = height as usize;
    let s = stride;
    let uv_start = s * h;

    if nv12.len() < uv_start + s * (h / 2) {
        return Err(format!("NV12 buffer too small: {} < {}", nv12.len(), uv_start + s * (h / 2)));
    }

    // ONNX model fixed at 432x768. Letterbox: fit within, maintain aspect ratio.
    let target_h = 432usize;
    let target_w = 768usize;

    let scale = (target_w as f32 / w as f32).min(target_h as f32 / h as f32);
    let new_w = (w as f32 * scale) as usize;
    let new_h = (h as f32 * scale) as usize;

    let pad_top = (target_h - new_h) / 2;
    let pad_left = (target_w - new_w) / 2;

    // Fused NV12→RGB + resize + letterbox + CHW conversion + grayscale extraction.
    // Bilinear interpolation on Y; nearest-neighbor on UV (already 2x subsampled).
    let inv_scale = 1.0 / scale;
    let mut chw = vec![0.0f32; 3 * target_h * target_w];
    let mut gray = vec![0u8; target_h * target_w];
    let plane = target_h * target_w;
    let max_uv_x = if w > 0 { w / 2 - 1 } else { 0 };
    let max_uv_y = if h > 0 { h / 2 - 1 } else { 0 };

    for out_y in pad_top..(pad_top + new_h) {
        let src_yf = (out_y - pad_top) as f32 * inv_scale;
        let sy0 = (src_yf as usize).min(h - 1);
        let sy1 = (sy0 + 1).min(h - 1);
        let fy = src_yf - sy0 as f32;

        // UV row (nearest to interpolated source position)
        let uv_y = ((src_yf * 0.5) as usize).min(max_uv_y);

        for out_x in pad_left..(pad_left + new_w) {
            let src_xf = (out_x - pad_left) as f32 * inv_scale;
            let sx0 = (src_xf as usize).min(w - 1);
            let sx1 = (sx0 + 1).min(w - 1);
            let fx = src_xf - sx0 as f32;

            // Bilinear interpolation on Y plane (full resolution)
            let w00 = (1.0 - fx) * (1.0 - fy);
            let w10 = fx * (1.0 - fy);
            let w01 = (1.0 - fx) * fy;
            let w11 = fx * fy;

            let y_val = nv12[sy0 * s + sx0] as f32 * w00
                      + nv12[sy0 * s + sx1] as f32 * w10
                      + nv12[sy1 * s + sx0] as f32 * w01
                      + nv12[sy1 * s + sx1] as f32 * w11;

            // Nearest-neighbor UV from interleaved UV plane (half resolution)
            let uv_x = ((src_xf * 0.5) as usize).min(max_uv_x);
            let uv_offset = uv_start + uv_y * s + uv_x * 2;
            let u_val = nv12[uv_offset] as f32 - 128.0;
            let v_val = nv12[uv_offset + 1] as f32 - 128.0;

            // YUV→RGB (BT.601 coefficients, matches original nv12_to_rgb)
            let r = (y_val + 1.402 * v_val).clamp(0.0, 255.0);
            let g = (y_val - 0.344136 * u_val - 0.714136 * v_val).clamp(0.0, 255.0);
            let b = (y_val + 1.772 * u_val).clamp(0.0, 255.0);

            let out_idx = out_y * target_w + out_x;
            chw[out_idx] = r;               // R plane
            chw[plane + out_idx] = g;        // G plane
            chw[2 * plane + out_idx] = b;    // B plane
            gray[out_idx] = r as u8;         // R channel as grayscale (matches original)
        }
    }

    Ok((chw, gray, target_h, target_w))
}

/// Median-consistency filter: reject points whose flow deviates too far from the median.
/// Shared by both ORT (via sample_from_dense_flow) and Burn (via GPU sparse sampling).
fn median_filter_points(from_pts: Vec<(f32, f32)>, to_pts: Vec<(f32, f32)>) -> (Vec<(f32, f32)>, Vec<(f32, f32)>) {
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
    (filtered_from, filtered_to)
}

/// Sample sparse point correspondences from a dense optical flow field
/// using texture-aware dense sampling for RANSAC-based pose estimation.
///
/// Algorithm:
/// 1. Dense grid sample (~1000+ candidates at w/40 step)
/// 2. Skip low-texture regions (patch variance < 2.0)
/// 3. Skip invalid flow (non-finite values, out-of-bounds destinations)
/// 4. Return (from_pts, to_pts) where to = from + flow
/// 5. RANSAC in estimate_pose handles foreground/background separation
fn sample_from_dense_flow(flow_data: &[f32], gray: &[u8], width: u32, height: u32) -> OpticalFlowPair {
    let w = width as usize;
    let h = height as usize;

    if flow_data.len() < w * h * 2 || gray.len() < w * h {
        return None;
    }

    // Grid step and texture window matching DIS (opencv_dis.rs)
    let step = (w / 15).max(4);
    let window_size = ((w as f32 * 0.02).round() as usize).max(10);
    let texture_threshold = 3.0;

    let mut from_pts = Vec::new();
    let mut to_pts = Vec::new();

    for x in (0..w).step_by(step) {
        for y in (0..h).step_by(step) {
            let variance = texture_variance(gray, x, y, w, h, window_size);
            if variance < texture_threshold {
                continue;
            }

            let idx = (y * w + x) * 2;
            let dx = flow_data[idx];
            let dy = flow_data[idx + 1];

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

/// Fallback: extract grayscale from NV12 Y plane and use OpenCV DIS for optical flow.
fn fallback_to_dis(self_frame: &OFNeuFlowV2, next_frame: &OFNeuFlowV2) -> OpticalFlowPair {
    if self_frame.nv12_frame.is_empty() || next_frame.nv12_frame.is_empty() {
        log::warn!("NeuFlow fallback: one or both NV12 frames are empty");
        return None;
    }
    if self_frame.width == 0 || self_frame.height == 0 {
        return None;
    }

    let gray1 = nv12_to_gray(&self_frame.nv12_frame, self_frame.width, self_frame.height, self_frame.stride);
    let gray2 = nv12_to_gray(&next_frame.nv12_frame, next_frame.width, next_frame.height, next_frame.stride);

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

/// Extract grayscale from NV12 Y plane (zero-cost: Y is already luma).
fn nv12_to_gray(nv12: &[u8], width: u32, height: u32, stride: usize) -> image::GrayImage {
    let w = width as usize;
    let h = height as usize;
    if nv12.len() < stride * h {
        log::warn!("NeuFlow nv12_to_gray: buffer too small (expected {}, got {})", stride * h, nv12.len());
        return image::GrayImage::new(width, height);
    }
    let mut gray = image::GrayImage::new(width, height);
    for y in 0..h {
        let src_row = &nv12[y * stride..y * stride + w];
        let dst_row = &mut gray.as_mut()[(y * w)..((y + 1) * w)];
        dst_row.copy_from_slice(src_row);
    }
    gray
}

/// Convert an RGB buffer to a grayscale image using standard luminance coefficients.
/// Y = 0.299*R + 0.587*G + 0.114*B
#[cfg(test)]
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
        let result = sample_from_dense_flow(&[], &[], 10, 10);
        assert!(result.is_none());
    }

    #[test]
    fn test_sample_from_dense_flow_uniform() {
        let w = 32u32;
        let h = 32u32;
        // Uniform flow: dx=1.0, dy=0.5 everywhere
        let mut flow = vec![0.0f32; (w * h * 2) as usize];
        for i in 0..(w * h) as usize {
            flow[i * 2] = 1.0;
            flow[i * 2 + 1] = 0.5;
        }
        // Textured gray: alternating pattern to ensure variance > 3.0
        let gray: Vec<u8> = (0..(w * h) as usize).map(|i| if i % 2 == 0 { 200 } else { 50 }).collect();
        let result = sample_from_dense_flow(&flow, &gray, w, h);
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
        // NV12: stride*h (Y) + stride*(h/2) (UV)
        let stride = 100;
        let frame = Arc::new(vec![0u8; stride * 100 + stride * 50]);
        let nf = OFNeuFlowV2::new(12345, frame, 100, 100, stride, 3);
        assert_eq!(nf.size(), (100, 100));
        assert!(nf.features().is_empty());
        assert!(!nf.can_cleanup());
    }

    #[test]
    fn test_ofneuflowv2_cleanup() {
        let stride = 10;
        let frame = Arc::new(vec![128u8; stride * 10 + stride * 5]);
        let mut nf = OFNeuFlowV2::new(0, frame, 10, 10, stride, 3);
        assert!(!nf.nv12_frame.is_empty());
        nf.cleanup();
        assert!(nf.nv12_frame.is_empty());
    }

    #[test]
    fn test_sample_uniform_flow() {
        // Constant flow [10, 5] everywhere — interleaved layout
        let w = 100u32;
        let h = 80u32;
        let n = (w * h) as usize;
        let mut flow_data = vec![0.0f32; 2 * n];
        for i in 0..n {
            flow_data[i * 2]     = 10.0; // dx
            flow_data[i * 2 + 1] = 5.0;  // dy
        }
        // Textured gray: alternating pattern to ensure variance > 3.0
        let gray: Vec<u8> = (0..n).map(|i| if i % 2 == 0 { 200 } else { 50 }).collect();
        let result = sample_from_dense_flow(&flow_data, &gray, w, h);
        assert!(result.is_some(), "uniform flow should produce valid result");
        let (from_pts, to_pts) = result.unwrap();
        assert!(!from_pts.is_empty());
        // Every sampled point should have dx≈10, dy≈5
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
        let mut flow_data = vec![0.0f32; 2 * n];
        for i in 0..n {
            flow_data[i * 2]     = 1.0;
            flow_data[i * 2 + 1] = 1.0;
        }
        // Uniform gray: all same value → variance = 0
        let gray = vec![128u8; n];
        let result = sample_from_dense_flow(&flow_data, &gray, w, h);
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
        let method = OpticalFlowMethod::detect_features(3, 0, img, None, 100, 80, 100);
        assert!(
            matches!(method, OpticalFlowMethod::OFNeuFlowV2(_)),
            "method=3 should produce OFNeuFlowV2"
        );
    }
}
