// SPDX-License-Identifier: GPL-3.0-or-later

//! Frame timings and unit bearings of one window's raw tracks (spec §5.2). The points go back to full resolution, are
//! undistorted with the frame's lens data (sensor shift and mesh undone as the render does) and become directions in
//! the quaternions' frame. Nothing here depends on the offset: it runs once per window.

use nalgebra::{ Matrix3, Vector3 };
use rayon::prelude::*;

use crate::stabilization::{ undistort_points, ComputeParams, FrameTransform };
use super::tracks::{ readout_pos, FrameTiming, PairData, RawFrame, RawWindow, WindowTracks };

/// Undistorted coordinates at or below this are the undistortion's mark for a point it could not invert
const INVALID_BELOW: f32 = -500_000.0;

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
    if pts.is_empty() { return Vec::new(); }
    let sx = params.width as f32 / track_size.0.max(1) as f32;
    let sy = params.height as f32 / track_size.1.max(1) as f32;
    let full: Vec<(f32, f32)> = pts.iter().map(|p| (p.0 * sx, p.1 * sy)).collect();
    let (camera_matrix, dist, _p, _rotations, shifts, mesh, _fov, _r_limit) =
        FrameTransform::at_timestamp_for_points(params, &full, frame.ts_ms, Some(frame.index), false);
    let shifts = shifts.map(|s| if s.len() == 1 { vec![s[0]; full.len()] } else { s });
    undistort_points(&full, camera_matrix, &dist, Matrix3::identity(), None, None, params, 1.0, 1.0, frame.ts_ms, shifts, mesh, 0.0)
        .into_iter()
        .map(|p| {
            let valid = p.0.is_finite() && p.1.is_finite() && p.0 > INVALID_BELOW && p.1 > INVALID_BELOW;
            valid.then(|| Vector3::new(p.0 as f64, -p.1 as f64, -1.0).normalize())
        })
        .collect()
}
