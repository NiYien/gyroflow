// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

#![allow(unused_variables, dead_code)]
use super::super::OpticalFlowPair;
use super::{OpticalFlowMethod, OpticalFlowTrait};

#[cfg(feature = "use-opencv")]
use opencv::{
    core::{CV_8UC1, Mat, Size, Vec2f},
    prelude::{DenseOpticalFlowTrait, MatTraitConst},
};
use parking_lot::RwLock;
use std::collections::BTreeMap;
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, OnceLock};

#[derive(Clone)]
pub struct OFOpenCVDis {
    features: Vec<(f32, f32)>,
    img: Arc<image::GrayImage>,
    pair_results: Arc<RwLock<BTreeMap<i64, Arc<OnceLock<OpticalFlowPair>>>>>,
    timestamp_us: i64,
    size: (i32, i32),
    used: Arc<AtomicU32>,
    #[cfg(test)]
    calculations: Arc<AtomicU32>,
}

impl OFOpenCVDis {
    pub fn detect_features(
        timestamp_us: i64,
        img: Arc<image::GrayImage>,
        width: u32,
        height: u32,
    ) -> Self {
        Self {
            features: Vec::new(),
            timestamp_us,
            size: (width as i32, height as i32),
            pair_results: Default::default(),
            img,
            used: Default::default(),
            #[cfg(test)]
            calculations: Default::default(),
        }
    }
}

impl OpticalFlowTrait for OFOpenCVDis {
    fn size(&self) -> (u32, u32) {
        (self.size.0 as u32, self.size.1 as u32)
    }
    fn features(&self) -> &Vec<(f32, f32)> {
        &self.features
    }

    fn optical_flow_to(&self, _to: &OpticalFlowMethod) -> OpticalFlowPair {
        #[cfg(feature = "use-opencv")]
        if let OpticalFlowMethod::OFOpenCVDis(next) = _to {
            let (w, h) = self.size;
            let cached = self.pair_results.read().get(&next.timestamp_us).cloned();
            let cached = match cached {
                Some(cached) => cached,
                None => {
                    if self.img.is_empty() || next.img.is_empty() || w <= 0 || h <= 0 {
                        return None;
                    }
                    self.pair_results
                        .write()
                        .entry(next.timestamp_us)
                        .or_default()
                        .clone()
                }
            };

            // Failed pairs are final for these immutable images too. Share one
            // calculation across overlapping batches without locking other pairs.
            return cached
                .get_or_init(|| {
                    #[cfg(test)]
                    self.calculations
                        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let gate = super::flow_gate::params();
                    let result = || -> Result<
                (Vec<(f32, f32)>, Vec<(f32, f32)>, super::flow_gate::GateStats),
                opencv::Error,
            > {
                let a1_img = unsafe {
                    Mat::new_size_with_data_unsafe(
                        Size::new(self.img.width() as i32, self.img.height() as i32),
                        CV_8UC1,
                        self.img.as_raw().as_ptr() as *mut std::ffi::c_void,
                        0,
                    )
                }?;
                let a2_img = unsafe {
                    Mat::new_size_with_data_unsafe(
                        Size::new(next.img.width() as i32, next.img.height() as i32),
                        CV_8UC1,
                        next.img.as_raw().as_ptr() as *mut std::ffi::c_void,
                        0,
                    )
                }?;

                let mut of = Mat::default();
                let mut optflow = opencv::video::DISOpticalFlow::create(
                    opencv::video::DISOpticalFlow_PRESET_FAST,
                )?;
                optflow.calc(&a1_img, &a2_img, &mut of)?;

                let mut points_a = Vec::new();
                let mut points_b = Vec::new();
                // Dense candidate grid (default w/40 ≈ 900 candidates @720p);
                // GYROFLOW_SYNC_OF_STEP_DIV=15 restores the legacy grid.
                let step_div = gate.step_div(super::flow_gate::DEFAULT_STEP_DIV_DIS);
                let step = (w as usize / step_div as usize).max(1);

                // Calculate window size as 2% of image width, minimum 10
                let window_size = (w as f32 * 0.02).round() as usize;
                let window_size = window_size.max(10);
                let texture_threshold = 3.0; // Threshold for texture clarity

                // Pre-calculate half window size for efficiency
                let half_win = window_size / 2;

                // Function to calculate variance of grayscale values in a window (more accurate than gradient)
                let calculate_texture = |img: &image::GrayImage, x: usize, y: usize| -> f32 {
                    let mut sum = 0.0;
                    let mut sum_sq = 0.0;
                    let mut count = 0.0;

                    // Cache image dimensions as isize for faster comparisons
                    let img_width = img.width() as isize;
                    let img_height = img.height() as isize;
                    let x_isize = x as isize;
                    let y_isize = y as isize;

                    // Calculate valid pixel boundaries once
                    let start_y = (y_isize - half_win as isize).max(0);
                    let end_y = (y_isize + half_win as isize).min(img_height - 1);
                    let start_x = (x_isize - half_win as isize).max(0);
                    let end_x = (x_isize + half_win as isize).min(img_width - 1);

                    // Iterate only over valid pixels, avoiding repeated boundary checks
                    for ny in start_y..=end_y {
                        for nx in start_x..=end_x {
                            let pixel = img.get_pixel(nx as u32, ny as u32).0[0] as f32;
                            sum += pixel;
                            sum_sq += pixel * pixel;
                            count += 1.0;
                        }
                    }

                    if count == 0.0 {
                        return 0.0;
                    }

                    let mean = sum / count;
                    let variance = (sum_sq / count) - (mean * mean);
                    variance
                };

                let mut candidates = 0usize;
                for i in (0..a1_img.cols()).step_by(step) {
                    for j in (0..a1_img.rows()).step_by(step) {
                        candidates += 1;
                        // Check texture clarity using accurate variance method
                        let texture = calculate_texture(&self.img, i as usize, j as usize);
                        if texture > texture_threshold {
                            let pt = of.at_2d::<Vec2f>(j, i)?;
                            points_a.push((i as f32, j as f32));
                            points_b.push((i as f32 + pt[0] as f32, j as f32 + pt[1] as f32));
                        }
                    }
                }
                let texture_pass = points_a.len();

                // Forward-backward consistency: backward flow + roundtrip
                // residuals, then gate enforcement (threshold filter +
                // stratified cap, or top-K degradation when FB pass is thin).
                let (fb_pass_count, fb_residuals, sel) = if gate.fb_check && !points_a.is_empty()
                {
                    let mut of_bwd = Mat::default();
                    optflow.calc(&a2_img, &a1_img, &mut of_bwd)?;
                    let mut backward_at = |qx: f32, qy: f32| bilinear_flow_at(&of_bwd, qx, qy);
                    let (pass, residuals) =
                        super::flow_gate::fb_filter(&points_a, &points_b, &mut backward_at, &gate);
                    let sel = super::flow_gate::apply_gate(
                        &points_a,
                        &pass,
                        &residuals,
                        (w as u32, h as u32),
                        &gate,
                    );
                    (pass.len(), residuals, sel)
                } else {
                    let sel = super::flow_gate::apply_gate(
                        &points_a,
                        &[],
                        &[],
                        (w as u32, h as u32),
                        &gate,
                    );
                    (0, Vec::new(), sel)
                };

                let (fb_p50, fb_p95) = super::flow_gate::residual_percentiles(&fb_residuals);
                let kept_a: Vec<(f32, f32)> = sel.indices.iter().map(|&i| points_a[i]).collect();
                let kept_b: Vec<(f32, f32)> = sel.indices.iter().map(|&i| points_b[i]).collect();
                let stats = super::flow_gate::GateStats {
                    candidates,
                    texture_pass,
                    fb_pass: fb_pass_count,
                    kept: kept_a.len(),
                    fb_p50,
                    fb_p95,
                    degraded: sel.degraded,
                    // Only recorded when fb_check is on (see below), so DIS
                    // pairs always count as FB-enabled in the run summary.
                    fb_enabled: gate.fb_check,
                };
                Ok((kept_a, kept_b, stats))
            }();

                    match result {
                        Ok((points_a, points_b, stats)) => {
                            // Stats recording is gated on fb_check so FB_CHECK=0 restores
                            // the pre-gate behavior exactly (no extra DIS calc, no logs).
                            if gate.fb_check {
                                super::flow_gate::record_pair_stats(
                                    "dis",
                                    self.timestamp_us,
                                    &stats,
                                );
                            }
                            let res = (points_a, points_b);
                            // Keep the existing minimum of ten valid correspondences.
                            if res.0.len() >= 10 && res.1.len() >= 10 {
                                self.used.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                next.used.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                return Some(res);
                            }
                        }
                        Err(e) => {
                            log::error!("OpenCV error: {:?}", e);
                        }
                    }
                    None
                })
                .clone();
        }
        None
    }
    fn can_cleanup(&self) -> bool {
        self.used.load(std::sync::atomic::Ordering::SeqCst) == 2
    }
    fn cleanup(&mut self) {
        self.img = Arc::new(image::GrayImage::default());
    }
}

/// Bilinear sample of a CV_32FC2 dense flow field at a fractional position.
/// Returns None when the position is outside the field (the FB gate treats
/// that as an automatic rejection).
#[cfg(feature = "use-opencv")]
fn bilinear_flow_at(of: &Mat, x: f32, y: f32) -> Option<(f32, f32)> {
    let (w, h) = (of.cols(), of.rows());
    if !(x.is_finite() && y.is_finite())
        || x < 0.0
        || y < 0.0
        || x > (w - 1) as f32
        || y > (h - 1) as f32
    {
        return None;
    }
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let v00 = *of.at_2d::<Vec2f>(y0, x0).ok()?;
    let v10 = *of.at_2d::<Vec2f>(y0, x1).ok()?;
    let v01 = *of.at_2d::<Vec2f>(y1, x0).ok()?;
    let v11 = *of.at_2d::<Vec2f>(y1, x1).ok()?;
    let lerp = |a: f32, b: f32, t: f32| a + (b - a) * t;
    let dx = lerp(lerp(v00[0], v10[0], fx), lerp(v01[0], v11[0], fx), fy);
    let dy = lerp(lerp(v00[1], v10[1], fx), lerp(v01[1], v11[1], fx), fy);
    Some((dx, dy))
}

#[cfg(all(test, feature = "use-opencv"))]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering::SeqCst;

    fn frame(ts: i64, image: image::GrayImage) -> OFOpenCVDis {
        let (w, h) = image.dimensions();
        OFOpenCVDis::detect_features(ts, Arc::new(image), w, h)
    }

    fn textured_image() -> image::GrayImage {
        let mut rng = fastrand::Rng::with_seed(7823);
        image::GrayImage::from_fn(128, 96, |_, _| image::Luma([rng.u8(..)]))
    }

    #[test]
    fn dis_failed_pair_is_calculated_once_across_concurrent_clones() {
        let source = frame(0, image::GrayImage::new(128, 96));
        let next = OpticalFlowMethod::OFOpenCVDis(frame(1000, image::GrayImage::new(128, 96)));
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let source = source.clone();
                let next = &next;
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    for _ in 0..5 {
                        assert!(source.optical_flow_to(next).is_none());
                    }
                });
            }
        });
        assert_eq!(source.calculations.load(SeqCst), 1);
        assert_eq!(source.used.load(SeqCst), 0);
    }

    #[test]
    fn dis_successful_pair_keeps_points_after_cleanup() {
        let image = textured_image();
        let mut source = frame(0, image.clone());
        let next = frame(1000, image);
        let target = OpticalFlowMethod::OFOpenCVDis(next.clone());
        let points = source
            .optical_flow_to(&target)
            .expect("textured pair must succeed");
        assert!(points.0.len() >= 10);
        for (a, b) in points.0.iter().zip(&points.1) {
            assert!((a.0 - b.0).abs() < 0.1 && (a.1 - b.1).abs() < 0.1);
        }
        source.cleanup();
        assert_eq!(source.optical_flow_to(&target), Some(points));
        assert_eq!(source.calculations.load(SeqCst), 1);
        assert_eq!(source.used.load(SeqCst), 1);
        assert_eq!(next.used.load(SeqCst), 1);
    }

    #[test]
    fn dis_failure_cache_is_scoped_to_pair_and_run() {
        let source = frame(0, image::GrayImage::new(128, 96));
        for ts in [1000, 2000, 1000, 2000] {
            let next = OpticalFlowMethod::OFOpenCVDis(frame(ts, image::GrayImage::new(128, 96)));
            assert!(source.optical_flow_to(&next).is_none());
        }
        assert_eq!(source.calculations.load(SeqCst), 2);
        let fresh = frame(0, textured_image());
        let next = OpticalFlowMethod::OFOpenCVDis(frame(1000, textured_image()));
        assert!(fresh.optical_flow_to(&next).is_some());
        assert_eq!(fresh.calculations.load(SeqCst), 1);
    }

    #[test]
    fn dis_opencv_error_is_cached_without_poisoning_other_pairs() {
        let source = frame(0, textured_image());
        let bad = OpticalFlowMethod::OFOpenCVDis(frame(1000, image::GrayImage::new(128, 80)));
        for _ in 0..3 {
            assert!(source.optical_flow_to(&bad).is_none());
        }
        assert_eq!(source.calculations.load(SeqCst), 1);
        let good = OpticalFlowMethod::OFOpenCVDis(frame(2000, textured_image()));
        assert!(source.optical_flow_to(&good).is_some());
        assert_eq!(source.calculations.load(SeqCst), 2);
    }
}
