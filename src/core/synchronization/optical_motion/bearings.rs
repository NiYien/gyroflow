// SPDX-License-Identifier: GPL-3.0-or-later

//! Frame timings and unit bearings of one window's raw tracks (spec §5.2). The points go back to full resolution, are
//! undistorted with the frame's lens data (sensor shift and mesh undone as the render does) and become directions in
//! the quaternions' frame. Nothing here depends on the offset: it runs once per window.

use nalgebra::Vector3;
use rayon::prelude::*;

use crate::stabilization::{ undistort_points_to_plane, ComputeParams, FrameTransform };
use super::tracks::{ readout_pos, FrameTiming, PairData, RawFrame, RawWindow, WindowTracks };

/// Frame timings and unit bearings for every pair of the window. `params` must be the measurement params (keyframes cleared, lens_correction_amount 1.0, framebuffer_inverted false).
///
/// Every raw pair gives one pair, with `seq` its index, even when none of its observations has a valid bearing in
/// both frames: dropping a pair would make the pairs around it look adjacent, and the high-pass takes a track's
/// observations in adjacent pairs as one continuous run.
pub fn build_window_tracks(raw: &RawWindow, params: &ComputeParams, horizontal: bool, window_mid_ms: f64) -> WindowTracks {
    let focal_px = FrameTransform::get_lens_data_at_timestamp(params, window_mid_ms, false).0[(0, 0)];
    // All timings under one lock of the metadata, released before the bearings, which take it themselves
    let timings: Vec<(FrameTiming, FrameTiming)> = {
        let gyro = params.gyro.read();
        let md = gyro.file_metadata.read();
        let timing = |f: RawFrame| FrameTiming {
            index: f.index,
            ts_ms: f.ts_ms,
            mid_ms: f.ts_ms + md.per_frame_time_offsets.get(f.index).copied().unwrap_or(0.0),
            readout_ms: FrameTransform::get_frame_readout_time(params, false, f.ts_ms, &md),
        };
        raw.pairs.iter().map(|p| (timing(p.a), timing(p.b))).collect()
    };
    let pairs = raw.pairs.par_iter().zip(timings).enumerate().map(|(seq, (rp, (a, b)))| {
        let pts_a: Vec<(f32, f32)> = rp.obs.iter().map(|o| (o.a[0], o.a[1])).collect();
        let pts_b: Vec<(f32, f32)> = rp.obs.iter().map(|o| (o.b[0], o.b[1])).collect();
        let ba = bearings(params, raw.track_size, &pts_a, &a);
        let bb = bearings(params, raw.track_size, &pts_b, &b);
        let n = rp.obs.len();
        let mut pd = PairData {
            seq, a, b,
            ids: Vec::with_capacity(n), va: Vec::with_capacity(n), vb: Vec::with_capacity(n), fa: Vec::with_capacity(n), fb: Vec::with_capacity(n),
        };
        for ((o, va), vb) in rp.obs.iter().zip(ba).zip(bb) {
            let (Some(va), Some(vb)) = (va, vb) else { continue };
            pd.ids.push(o.id);
            pd.va.push(va);
            pd.vb.push(vb);
            pd.fa.push(readout_pos(o.a, raw.track_size, horizontal));
            pd.fb.push(readout_pos(o.b, raw.track_size, horizontal));
        }
        pd
    }).collect();
    WindowTracks { pairs, focal_px }
}

/// Unit bearings of points tracked at `track_size`, in the quaternions' frame: the axis flips the renderer applies
/// (`F·R·F`, F = diag(1, -1, -1)). None for a point the undistortion could not invert
fn bearings(params: &ComputeParams, track_size: (u32, u32), pts: &[(f32, f32)], frame: &FrameTiming) -> Vec<Option<Vector3<f64>>> {
    undistort_points_to_plane(pts, frame.ts_ms, frame.index, params, track_size)
        .into_iter()
        .map(|p| p.map(|(x, y)| Vector3::new(x as f64, -y as f64, -1.0).normalize()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::tracks::{ Observation, RawPair };
    use crate::StabilizationManager;
    use crate::lens_profile::Dimensions;

    // These constants pin the bearings before extracting undistort_points_to_plane and depend on the
    // toolchain's float library. To refresh them for another toolchain, set them to 0 and copy the values
    // reported by `just test-core optical_motion::bearings`.
    const GOLDEN_POINTS: usize = 45;
    const GOLDEN_FOCAL_BITS: u64 = 4_652_552_666_608_566_272;
    const GOLDEN_HASH: u64 = 9_618_775_943_658_929_199;

    #[test]
    fn bearings_bits_unchanged() {
        let stab = StabilizationManager::default();
        { let mut p = stab.params.write(); p.size = (1920, 1080); p.fps = 30.0; p.frame_count = 300; p.duration_ms = 10_000.0; }
        {
            // A fixed lens with distortion, using the same fields as the frame transform tests.
            let mut lens = stab.lens.write();
            lens.calib_dimension = Dimensions { w: 1920, h: 1080 };
            lens.orig_dimension = lens.calib_dimension.clone();
            lens.fisheye_params.camera_matrix = vec![[1100.0, 0.0, 960.0], [0.0, 1100.0, 540.0], [0.0, 0.0, 1.0]];
            lens.fisheye_params.distortion_coeffs = vec![0.05, -0.02, 0.01, -0.005];
        }
        let mut params = ComputeParams::from_manager(&stab);
        params.keyframes.clear();
        params.lens_correction_amount = 1.0;
        params.framebuffer_inverted = false;
        // A 9x5 grid at the tracking size, moved by a few pixels in the second frame.
        let obs: Vec<Observation> = (0..45u32).map(|i| {
            let (x, y) = (60.0 + 105.0 * (i % 9) as f32, 50.0 + 110.0 * (i / 9) as f32);
            Observation { id: i, a: [x, y], b: [x + 3.5, y - 2.25], texture: f32::NAN }
        }).collect();
        let raw = RawWindow {
            pairs: vec![RawPair { a: RawFrame { index: 30, ts_ms: 1000.0 }, b: RawFrame { index: 31, ts_ms: 1000.0 + 1000.0 / 30.0 }, obs }],
            track_size: (960, 540),
            ..Default::default()
        };
        let w = build_window_tracks(&raw, &params, false, 1016.0);
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for pd in &w.pairs {
            for v in pd.va.iter().chain(&pd.vb) {
                for c in [v.x, v.y, v.z] { h = (h ^ c.to_bits()).wrapping_mul(0x0000_0100_0000_01b3); }
            }
        }
        assert!(w.pairs[0].ids.len() >= 40, "{} valid points", w.pairs[0].ids.len());
        assert_eq!((w.pairs[0].ids.len(), w.focal_px.to_bits(), h), (GOLDEN_POINTS, GOLDEN_FOCAL_BITS, GOLDEN_HASH));
    }
}
