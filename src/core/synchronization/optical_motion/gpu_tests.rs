// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

//! Numerical and lifecycle checks for the shared GPU tracker.
use super::FrameTracker;
use super::gpu_klt::GpuKlt;
use super::gpu_tracker::*;
use super::tracker::KltTracker;
use crate::synchronization::GrayImage;
use opencv::{
    core::{self, Mat, Point2f, Size, TermCriteria, Vector},
    imgproc,
    prelude::*,
    video,
};
const FB_MAX: f32 = 0.1;

fn image_of(frame: &Mat) -> GrayImage {
    GrayImage::from_raw(
        frame.cols() as u32,
        frame.rows() as u32,
        frame.data_bytes().unwrap().to_vec(),
    )
    .unwrap()
}

#[test]
fn gpu_corner_responses_match_full_image_and_cell_filters() {
    let Some(adapter) = gpu() else {
        return;
    };
    for (w, h) in [(960, 540), (853, 481)] {
        let first = texture();
        let mut frame = Mat::default();
        imgproc::resize(
            &first,
            &mut frame,
            Size::new(w, h),
            0.0,
            0.0,
            imgproc::INTER_AREA,
        )
        .unwrap();
        let mut gpu = GpuKlt::new(&adapter, w as u32, h as u32, w as u32, 3000).unwrap();
        gpu.submit(frame.data_bytes().unwrap(), std::iter::empty())
            .unwrap();
        gpu.finish().unwrap();
        let compare = |actual: f32, expected: f32| {
            assert!(
                (actual - expected).abs() <= 2e-9 + 2e-4 * expected.abs(),
                "response {actual} vs {expected}"
            )
        };
        let mut global = Mat::default();
        imgproc::corner_min_eigen_val(&frame, &mut global, 7, 3, core::BORDER_DEFAULT).unwrap();
        for (a, b) in gpu
            .corners
            .global
            .iter()
            .zip(global.data_typed::<f32>().unwrap())
        {
            compare(*a, *b);
        }
        assert_eq!(gpu.corners.global_candidates, super::gpu_corners::local_maxima(&gpu.corners.global, w as usize, h as usize, false));
        assert_eq!(gpu.corners.local_candidates, super::gpu_corners::local_maxima(&gpu.corners.local, w as usize, h as usize, true));
        for r in 0..6 {
            for c in 0..4 {
                let (x0, y0, x1, y1) = (w * c / 4, h * r / 6, w * (c + 1) / 4, h * (r + 1) / 6);
                let roi = Mat::roi(&frame, core::Rect::new(x0, y0, x1 - x0, y1 - y0)).unwrap();
                let mut local = Mat::default();
                imgproc::corner_min_eigen_val(&roi, &mut local, 7, 3, core::BORDER_DEFAULT)
                    .unwrap();
                for y in y0..y1 {
                    for x in x0..x1 {
                        compare(
                            gpu.corners.local[(y * w + x) as usize],
                            *local.at_2d::<f32>(y - y0, x - x0).unwrap(),
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn gpu_failure_restarts_cpu_tracks_without_reusing_ids() {
    if gpu().is_none() {
        return;
    }
    let a = texture();
    let frames: Vec<_> = (0..6)
        .map(|i| image_of(&moved(&a, 0.15 * i as f64, (1.1 * i as f64, -0.7 * i as f64)).0))
        .collect();
    let mut tracker = KltTracker::with_device(1500, 0.85, 960, KltDevice::Gpu(None));
    assert!(tracker.track(&frames[0]).unwrap().0.is_empty());
    let before = tracker.track(&frames[1]).unwrap().0;
    assert!(tracker.uses_gpu() && before.len() > 500);
    let preparing_on_gpu = tracker.gpu_preparation();
    assert!(preparing_on_gpu.load(std::sync::atomic::Ordering::Relaxed));
    let last_id = before.iter().map(|o| o.id).max().unwrap();
    tracker.fail_gpu_once();
    assert!(tracker.track(&frames[2]).unwrap().0.is_empty());
    assert!(!tracker.uses_gpu());
    assert!(!preparing_on_gpu.load(std::sync::atomic::Ordering::Relaxed));
    let recovered = tracker.track(&frames[3]).unwrap().0;
    assert!(recovered.len() > 500 && recovered.iter().all(|o| o.id > last_id));
    let mut cpu = KltTracker::new(1500, 0.85, 960);
    cpu.track(&frames[2]).unwrap();
    let expected = cpu.track(&frames[3]).unwrap().0;
    assert_eq!(recovered.len(), expected.len());
    for (a, b) in recovered.iter().zip(&expected) {
        assert_eq!((a.a, a.b), (b.a, b.b));
    }
    tracker.reset();
    assert!(tracker.track(&frames[4]).unwrap().0.is_empty());
    assert!(
        tracker
            .track(&frames[5])
            .unwrap()
            .0
            .iter()
            .all(|o| o.id > recovered.iter().map(|o| o.id).max().unwrap())
    );
}

#[test]
fn gpu_reset_and_resize_do_not_connect_old_frames() {
    if gpu().is_none() {
        return;
    }
    let a = texture();
    let mut tracker = KltTracker::with_device(1500, 1.0, 960, KltDevice::Gpu(None));
    tracker.track(&image_of(&a)).unwrap();
    let before = tracker
        .track(&image_of(&moved(&a, 0.15, (1.1, -0.7)).0))
        .unwrap()
        .0;
    let id = before.iter().map(|o| o.id).max().unwrap();
    tracker.reset();
    assert!(tracker.track(&image_of(&a)).unwrap().0.is_empty());
    let after = tracker
        .track(&image_of(&moved(&a, -0.15, (-1.1, 0.7)).0))
        .unwrap()
        .0;
    assert!(after.len() > 500 && after.iter().all(|o| o.id > id));
    let small = image::imageops::resize(
        &image_of(&a),
        640,
        360,
        image::imageops::FilterType::Triangle,
    );
    assert!(tracker.track(&small).unwrap().0.is_empty());
    assert!(tracker.track(&small).unwrap().0.len() > 500);
    assert!(tracker.uses_gpu());
}

#[test]
fn gpu_concurrent_trackers_keep_independent_results() {
    if gpu().is_none() {
        return;
    }
    let a = texture();
    let clips: Vec<Vec<GrayImage>> = [-1.0, 1.0]
        .into_iter()
        .map(|direction| {
            (0..4)
                .map(|i| {
                    image_of(
                        &moved(
                            &a,
                            direction * 0.15 * i as f64,
                            (direction * 1.1 * i as f64, -0.7 * i as f64),
                        )
                        .0,
                    )
                })
                .collect()
        })
        .collect();
    let run = |frames: &[GrayImage]| {
        let mut tracker = KltTracker::with_device(1500, 0.85, 960, KltDevice::Gpu(None));
        let result: Vec<_> = frames
            .iter()
            .flat_map(|frame| tracker.track(frame).unwrap().0)
            .map(|o| {
                (
                    o.id,
                    o.a.map(f32::to_bits),
                    o.b.map(f32::to_bits),
                    o.texture.to_bits(),
                )
            })
            .collect();
        assert!(tracker.uses_gpu());
        result
    };
    let expected: Vec<_> = clips.iter().map(|c| run(c)).collect();
    let actual = std::thread::scope(|scope| {
        let jobs: Vec<_> = clips
            .iter()
            .map(|clip| scope.spawn(move || run(clip)))
            .collect();
        jobs.into_iter()
            .map(|j| j.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(expected, actual);
    assert_ne!(actual[0], actual[1]);
}

const W: i32 = 960;
const H: i32 = 540;

/// Detail like footage has: noise blurred into blobs of a few pixels
fn texture() -> Mat {
    let mut state = 0x2545_f491_4f6c_dd1du64;
    let noise: Vec<u8> = (0..W * H)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 56) as u8
        })
        .collect();
    let noise = Mat::new_rows_cols_with_data(H, W, &noise)
        .unwrap()
        .try_clone()
        .unwrap();
    let (mut blurred, mut out) = (Mat::default(), Mat::default());
    imgproc::gaussian_blur_def(&noise, &mut blurred, Size::new(0, 0), 2.0).unwrap();
    core::normalize(
        &blurred,
        &mut out,
        0.0,
        255.0,
        core::NORM_MINMAX,
        -1,
        &core::no_array(),
    )
    .unwrap();
    out
}

/// The frame turned by `angle` degrees around its middle and moved by `shift`, and that affine transform
fn moved(frame: &Mat, angle: f64, shift: (f64, f64)) -> (Mat, [f64; 6]) {
    let mut m =
        imgproc::get_rotation_matrix_2d(Point2f::new(W as f32 / 2.0, H as f32 / 2.0), angle, 1.0)
            .unwrap();
    *m.at_2d_mut::<f64>(0, 2).unwrap() += shift.0;
    *m.at_2d_mut::<f64>(1, 2).unwrap() += shift.1;
    let mut out = Mat::default();
    imgproc::warp_affine(
        frame,
        &mut out,
        &m,
        Size::new(W, H),
        imgproc::INTER_LINEAR,
        core::BORDER_REFLECT_101,
        Default::default(),
    )
    .unwrap();
    (
        out,
        std::array::from_fn(|i| *m.at_2d::<f64>(i as i32 / 3, i as i32 % 3).unwrap()),
    )
}

fn apply(m: &[f64; 6], p: (f64, f64)) -> (f64, f64) {
    (
        m[0] * p.0 + m[1] * p.1 + m[2],
        m[3] * p.0 + m[4] * p.1 + m[5],
    )
}

fn invert(m: &[f64; 6]) -> [f64; 6] {
    let d = m[0] * m[4] - m[1] * m[3];
    let (a, b, c, e) = (m[4] / d, -m[1] / d, -m[3] / d, m[0] / d);
    [a, b, -(a * m[2] + b * m[5]), c, e, -(c * m[2] + e * m[5])]
}

fn gpu() -> Option<wgpu::Adapter> {
    let adapter = crate::gpu::wgpu::WgpuWrapper::compute_adapter(None);
    if adapter.is_none() {
        eprintln!("no GPU, nothing to test");
    }
    adapter
}

fn median(mut v: Vec<f32>) -> f32 {
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

#[test]
fn gpu_tracks_the_same_points_as_opencv() {
    let Some(adapter) = gpu() else { return };
    let a = texture();
    let (b, m) = moved(&a, 0.3, (2.3, -1.7));
    let truth = |p: [f32; 2]| {
        let t = apply(&m, (p[0] as f64, p[1] as f64));
        [t.0 as f32, t.1 as f32]
    };
    let mut gpu = GpuKlt::new(&adapter, W as u32, H as u32, W as u32, 2000).unwrap();

    gpu.submit(a.data_bytes().unwrap(), std::iter::empty())
        .unwrap();
    gpu.finish().unwrap();
    let mut corners = Vector::<Point2f>::new();
    imgproc::good_features_to_track(
        &a,
        &mut corners,
        1500,
        0.001,
        0.0,
        &core::no_array(),
        7,
        false,
        0.04,
    )
    .unwrap();
    let points: Vec<[f32; 2]> = corners.iter().map(|p| [p.x, p.y]).collect();
    gpu.submit(b.data_bytes().unwrap(), points.iter().copied())
        .unwrap();
    gpu.finish().unwrap();
    let on_gpu = checked(&points, &gpu.fwd, &gpu.bwd);
    let on_cpu = cpu_klt(&a, &b, &points).unwrap();
    let both: Vec<([f32; 2], [f32; 2], [f32; 2])> = points
        .iter()
        .zip(on_gpu.iter().zip(&on_cpu))
        .filter_map(|(p, (g, c))| Some((truth(*p), (*g)?, (*c)?)))
        .collect();
    let kept = (
        on_gpu.iter().filter(|p| p.is_some()).count(),
        on_cpu.iter().filter(|p| p.is_some()).count(),
    );
    assert!(
        both.len() * 100 >= kept.0.max(kept.1) * 99,
        "kept by both {} of {kept:?}",
        both.len()
    );
    let dist = |a: [f32; 2], b: [f32; 2]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt();
    let apart: Vec<f32> = both.iter().map(|(_, g, c)| dist(*g, *c)).collect();
    let off: Vec<f32> = both.iter().map(|(t, g, _)| dist(*t, *g)).collect();
    eprintln!(
        "kept {kept:?}, by both {}, apart median {} px, max {}; off the true motion median {} px",
        both.len(),
        median(apart.clone()),
        apart.iter().fold(0.0f32, |a, b| a.max(*b)),
        median(off.clone())
    );
    assert!(
        median(apart.clone()) < 0.001 && apart.iter().all(|d| *d < 0.05),
        "GPU vs OpenCV: median {} px, max {}",
        median(apart.clone()),
        apart.iter().fold(0.0f32, |a, b| a.max(*b))
    );
    assert!(
        median(off.clone()) < 0.02,
        "GPU vs the true motion: median {} px",
        median(off)
    );
}

/// The whole tracker over a few frames, on the GPU and on the CPU: as many tracks, as close to the true motion
#[test]
fn tracker_on_gpu_as_on_cpu() {
    if gpu().is_none() {
        return;
    }
    let first = texture();
    let frames: Vec<(Mat, [f64; 6])> = (0..6)
        .map(|i| moved(&first, 0.15 * i as f64, (1.1 * i as f64, -0.7 * i as f64)))
        .collect();
    let run = |device: KltDevice| {
        let mut tracker = KltTracker::with_device(1500, 1.0, 960, device);
        let mut off = Vec::new();
        for (i, (frame, m)) in frames.iter().enumerate() {
            let image =
                GrayImage::from_raw(W as u32, H as u32, frame.data_bytes().unwrap().to_vec())
                    .unwrap();
            let mut eig = Mat::default();
            imgproc::corner_min_eigen_val(frame, &mut eig, 7, 3, core::BORDER_DEFAULT).unwrap();
            for o in tracker.track(&image).unwrap().0 {
                if matches!(device, KltDevice::Gpu(_)) {
                    let reference = *eig
                        .at_2d::<f32>(o.b[1].round() as i32, o.b[0].round() as i32)
                        .unwrap();
                    assert!(
                        (o.texture - reference).abs() <= 2e-4 * reference.abs().max(1e-6),
                        "texture {} vs {reference}",
                        o.texture
                    );
                }
                let truth = apply(
                    m,
                    apply(&invert(&frames[i - 1].1), (o.a[0] as f64, o.a[1] as f64)),
                );
                off.push((truth.0 - o.b[0] as f64).hypot(truth.1 - o.b[1] as f64) as f32);
            }
        }
        (off, tracker.uses_gpu())
    };
    let (cpu, gpu) = (run(KltDevice::Cpu), run(KltDevice::Gpu(None)));
    assert!(gpu.1, "GPU unexpectedly fell back");
    assert!(
        cpu.0.len().abs_diff(gpu.0.len()) * 50 < cpu.0.len(),
        "observations: CPU {}, GPU {}",
        cpu.0.len(),
        gpu.0.len()
    );
    eprintln!(
        "observations: CPU {}, GPU {}; {}",
        cpu.0.len(),
        gpu.0.len(),
        gpu.1
    );
    let (c, g) = (median(cpu.0), median(gpu.0));
    eprintln!("off the true motion: median {c} px on the CPU, {g} on the GPU");
    assert!(
        g < 0.02 && (g - c).abs() < 0.002,
        "off the true motion: median {c} px on the CPU, {g} on the GPU"
    );
}

fn cpu_klt(prev: &Mat, cur: &Mat, points: &[[f32; 2]]) -> opencv::Result<Vec<Option<[f32; 2]>>> {
    let pa: Vector<Point2f> = points.iter().map(|p| Point2f::new(p[0], p[1])).collect();
    let mut pb = Vector::<Point2f>::new();
    let mut pa2 = Vector::<Point2f>::new();
    let mut st = Vector::<u8>::new();
    let mut st2 = Vector::<u8>::new();
    let mut err = Vector::<f32>::new();
    let criteria = TermCriteria::new(3 /* COUNT | EPS */, KLT_ITERATIONS, KLT_EPS)?;
    let window = Size::new(WINDOW, WINDOW);
    video::calc_optical_flow_pyr_lk(
        prev,
        cur,
        &pa,
        &mut pb,
        &mut st,
        &mut err,
        window,
        KLT_LEVELS,
        criteria,
        0,
        KLT_MIN_EIG,
    )?;
    video::calc_optical_flow_pyr_lk(
        cur,
        prev,
        &pb,
        &mut pa2,
        &mut st2,
        &mut err,
        window,
        KLT_LEVELS,
        criteria,
        0,
        KLT_MIN_EIG,
    )?;
    (0..pa.len())
        .map(|i| {
            let (a, b, a2) = (pa.get(i)?, pb.get(i)?, pa2.get(i)?);
            let fb = ((a2.x - a.x).powi(2) + (a2.y - a.y).powi(2)).sqrt();
            Ok((st.get(i)? == 1 && st2.get(i)? == 1 && fb < FB_MAX).then_some([b.x, b.y]))
        })
        .collect()
}

fn checked(points: &[[f32; 2]], fwd: &[[f32; 4]], bwd: &[[f32; 4]]) -> Vec<Option<[f32; 2]>> {
    points
        .iter()
        .zip(fwd.iter().zip(bwd))
        .map(|(a, (f, b))| {
            let fb = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            (f[2] > 0.5 && b[2] > 0.5 && fb < 0.1).then_some([f[0], f[1]])
        })
        .collect()
}
