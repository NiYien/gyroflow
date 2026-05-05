// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Smoothing pipeline quality-diagnostics dump.
//!
//! Enabled via env var `GYROFLOW_SMOOTH_DIAG=1`. When enabled,
//! `recompute_blocking` opens a session under `<data_dir>/diag/smooth_<ts>_<sid>/`
//! and writes:
//! - `dump.csv`     per-frame q_raw/q_smooth/delta/derivatives/fov columns
//! - `meta.json`    video + smoothing + zooming params
//! - `plot.py`      self-contained Python analyzer (run `python plot.py`)
//!
//! When disabled, every entry-point performs one atomic load and returns
//! immediately — zero allocation.

use parking_lot::Mutex;
use std::path::PathBuf;
use std::sync::OnceLock;

static ENABLED: OnceLock<bool> = OnceLock::new();
/// In tests, -1 means "use env", 0 means forced-false, 1 means forced-true.
#[cfg(test)]
static TEST_ENABLED_OVERRIDE: std::sync::atomic::AtomicI8 =
    std::sync::atomic::AtomicI8::new(-1);

// SESSION and DiagSession fields are read/written in Tasks 2-7; allow dead_code for skeleton.
#[allow(dead_code)]
static SESSION: Mutex<Option<DiagSession>> = Mutex::new(None);

#[inline]
pub fn is_enabled() -> bool {
    #[cfg(test)]
    {
        let o = TEST_ENABLED_OVERRIDE.load(std::sync::atomic::Ordering::SeqCst);
        if o >= 0 {
            return o == 1;
        }
    }
    *ENABLED.get_or_init(|| {
        std::env::var("GYROFLOW_SMOOTH_DIAG")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    })
}

/// Test-only helper to override the enabled flag without relying on OnceLock state.
#[cfg(test)]
pub(crate) fn force_enabled_for_test(on: bool) {
    TEST_ENABLED_OVERRIDE.store(if on { 1 } else { 0 }, std::sync::atomic::Ordering::SeqCst);
}

#[allow(dead_code)]
struct DiagSession {
    pub out_dir: PathBuf,
    pub frames: Vec<FrameRecord>,
    pub smoothing_meta: SmoothingMeta,
    pub video_meta: VideoMeta,
}

#[derive(Default, Clone, Debug)]
pub(crate) struct FrameRecord {
    pub frame_idx: usize,
    pub ts_ms: f64,
    pub q_raw_w: f64, pub q_raw_x: f64, pub q_raw_y: f64, pub q_raw_z: f64,
    pub q_smooth_w: f64, pub q_smooth_x: f64, pub q_smooth_y: f64, pub q_smooth_z: f64,
    pub delta_pitch_deg: f64,
    pub delta_yaw_deg: f64,
    pub delta_roll_deg: f64,
    pub delta_total_deg: f64,
    pub vel_pitch_deg_s: f64,
    pub vel_yaw_deg_s: f64,
    pub vel_roll_deg_s: f64,
    pub accel_pitch_deg_s2: f64,
    pub accel_yaw_deg_s2: f64,
    pub accel_roll_deg_s2: f64,
    pub jerk_pitch_deg_s3: f64,
    pub jerk_yaw_deg_s3: f64,
    pub jerk_roll_deg_s3: f64,
    pub fov_baseline: f64,
    pub fov_final: f64,
}

#[derive(Default, Clone, Debug)]
pub(crate) struct SmoothingMeta {
    pub method: String,
    pub method_id: usize,
    pub params_json: serde_json::Value,
    pub adaptive_zoom_window: f64,
    pub zoom_method: String,
    pub max_zoom_pct: f64,
    pub max_zoom_iterations: usize,
}

#[derive(Default, Clone, Debug)]
pub(crate) struct VideoMeta {
    pub path_basename: String,
    pub duration_ms: f64,
    pub frame_count: usize,
    pub fps: f64,
    pub width: usize,
    pub height: usize,
    pub gyro_sample_rate_hz: f64,
}

pub fn init_session() {
    if !is_enabled() {
        return;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let sid = {
        let snap = crate::log_context::LogContext::snapshot();
        if snap.session_id.is_empty() {
            "nosid".to_string()
        } else {
            // Sanitize session_id for use as a filesystem path segment.
            snap.session_id
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
                .collect()
        }
    };
    let mut dir = crate::settings::data_dir();
    dir.push("diag");
    dir.push(format!("smooth_{}_{}", ts, sid));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!(target: "app", "[SmoothDiag] failed to create {}: {}", dir.display(), e);
        return;
    }
    log::info!(target: "app", "[SmoothDiag] session opened at {}", dir.display());
    let mut session_guard = SESSION.lock();
    if session_guard.is_some() {
        log::warn!(target: "app", "[SmoothDiag] init_session called while a session is already open; replacing it (previous data discarded)");
    }
    *session_guard = Some(DiagSession {
        out_dir: dir,
        frames: Vec::new(),
        smoothing_meta: SmoothingMeta::default(),
        video_meta: VideoMeta::default(),
    });
}

#[allow(private_interfaces)]
pub fn record_session(
    ts_ms: &[f64],
    q_raw: &[(f64, f64, f64, f64)],
    q_smooth: &[(f64, f64, f64, f64)],
    fovs: &[(f64, f64)], // (baseline, final)
    smoothing_meta: &SmoothingMeta,
    video_meta: &VideoMeta,
) {
    if !is_enabled() {
        return;
    }
    let n = ts_ms.len();
    // Silent no-op for empty input (e.g. called before any frames are ready).
    if n == 0 {
        return;
    }
    // Warn and bail on mismatched slice lengths — indicates a programming error.
    if q_raw.len() != n || q_smooth.len() != n || fovs.len() != n {
        log::warn!(
            target: "app",
            "[SmoothDiag] record_session length mismatch: ts={}, q_raw={}, q_smooth={}, fovs={}",
            n, q_raw.len(), q_smooth.len(), fovs.len()
        );
        return;
    }

    use nalgebra::{Quaternion, UnitQuaternion};
    const RAD2DEG: f64 = 180.0 / std::f64::consts::PI;

    // 1) Convert q_raw to UnitQuaternion for incremental-rotation derivatives.
    let uq_raw: Vec<UnitQuaternion<f64>> = q_raw
        .iter()
        .map(|(w, x, y, z)| {
            UnitQuaternion::from_quaternion(Quaternion::new(*w, *x, *y, *z))
        })
        .collect();

    // 2) Finite differences using central differences; endpoints stay NaN.
    //    Angular velocity at frame i is derived from the incremental rotation
    //    q_raw[i-1]^{-1} * q_raw[i+1], whose Euler angles are small (bounded by
    //    2 * omega_max * dt) and therefore free of the asin fold that plagues
    //    absolute-angle differences when total rotation exceeds 90 deg.
    let mut vel = vec![(f64::NAN, f64::NAN, f64::NAN); n];
    let mut accel = vec![(f64::NAN, f64::NAN, f64::NAN); n];
    let mut jerk = vec![(f64::NAN, f64::NAN, f64::NAN); n];
    // Compute average dt from first and last timestamps.
    let dt_s = if n >= 2 {
        ((ts_ms[n - 1] - ts_ms[0]) / 1000.0) / (n as f64 - 1.0)
    } else {
        1.0 / 30.0
    };
    // First-order velocity: incremental rotation q[i-1]^{-1} * q[i+1] over 2*dt.
    if n >= 3 {
        for i in 1..n - 1 {
            let dq = uq_raw[i - 1].inverse() * uq_raw[i + 1];
            let (p, y_, r) = dq.euler_angles();
            vel[i] = (
                p * RAD2DEG / (2.0 * dt_s),
                y_ * RAD2DEG / (2.0 * dt_s),
                r * RAD2DEG / (2.0 * dt_s),
            );
        }
    }
    // Second-order acceleration via central differences of velocity.
    if n >= 5 {
        for i in 2..n - 2 {
            let (p0, y0, r0) = vel[i - 1];
            let (p1, y1, r1) = vel[i + 1];
            accel[i] = (
                (p1 - p0) / (2.0 * dt_s),
                (y1 - y0) / (2.0 * dt_s),
                (r1 - r0) / (2.0 * dt_s),
            );
        }
    }
    // Third-order jerk via central differences of acceleration.
    if n >= 7 {
        for i in 3..n - 3 {
            let (p0, y0, r0) = accel[i - 1];
            let (p1, y1, r1) = accel[i + 1];
            jerk[i] = (
                (p1 - p0) / (2.0 * dt_s),
                (y1 - y0) / (2.0 * dt_s),
                (r1 - r0) / (2.0 * dt_s),
            );
        }
    }

    // 3) Per-frame delta-angle = q_raw^{-1} * q_smooth, decomposed to Euler.
    let mut frames = Vec::with_capacity(n);
    for i in 0..n {
        let qr = UnitQuaternion::from_quaternion(Quaternion::new(
            q_raw[i].0, q_raw[i].1, q_raw[i].2, q_raw[i].3,
        ));
        let qs = UnitQuaternion::from_quaternion(Quaternion::new(
            q_smooth[i].0, q_smooth[i].1, q_smooth[i].2, q_smooth[i].3,
        ));
        let delta = qr.inverse() * qs;
        let (dp, dy, dr) = delta.euler_angles();
        let total = delta.angle() * RAD2DEG;
        frames.push(FrameRecord {
            frame_idx: i,
            ts_ms: ts_ms[i],
            q_raw_w: q_raw[i].0, q_raw_x: q_raw[i].1, q_raw_y: q_raw[i].2, q_raw_z: q_raw[i].3,
            q_smooth_w: q_smooth[i].0, q_smooth_x: q_smooth[i].1, q_smooth_y: q_smooth[i].2, q_smooth_z: q_smooth[i].3,
            delta_pitch_deg: dp * RAD2DEG,
            delta_yaw_deg: dy * RAD2DEG,
            delta_roll_deg: dr * RAD2DEG,
            delta_total_deg: total,
            vel_pitch_deg_s: vel[i].0,
            vel_yaw_deg_s: vel[i].1,
            vel_roll_deg_s: vel[i].2,
            accel_pitch_deg_s2: accel[i].0,
            accel_yaw_deg_s2: accel[i].1,
            accel_roll_deg_s2: accel[i].2,
            jerk_pitch_deg_s3: jerk[i].0,
            jerk_yaw_deg_s3: jerk[i].1,
            jerk_roll_deg_s3: jerk[i].2,
            fov_baseline: fovs[i].0,
            fov_final: fovs[i].1,
        });
    }

    if let Some(s) = SESSION.lock().as_mut() {
        s.frames = frames;
        s.smoothing_meta = smoothing_meta.clone();
        s.video_meta = video_meta.clone();
    }
}

#[allow(clippy::needless_return)]
pub fn flush_and_close() {
    if !is_enabled() {
        return;
    }
    // Tasks 5/6/7: write meta.json, embed plot.py, close session.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_path_is_noop() {
        // ENABLED defaults to false in test env (env var not set).
        // record_session and flush_and_close must return without touching SESSION.
        assert!(!is_enabled());
        // None of the calls below should panic or allocate a session.
        record_session(&[], &[], &[], &[], &SmoothingMeta::default(), &VideoMeta::default());
        flush_and_close();
        assert!(SESSION.lock().is_none());
    }

    #[test]
    #[serial_test::serial]
    fn record_session_constant_velocity_ramp() {
        // 60 frames at 30 fps, perfectly linear yaw of 100 deg/s. All derivatives
        // beyond velocity should be ~0; delta_total should equal 0 because we feed
        // q_smooth == q_raw (smoothing is irrelevant in this fixture).
        let n = 60usize;
        let dt = 1.0 / 30.0;
        let mut ts_ms = Vec::with_capacity(n);
        let mut q_raw = Vec::with_capacity(n);
        let mut q_smooth = Vec::with_capacity(n);
        let mut fovs = Vec::with_capacity(n);
        for i in 0..n {
            let t = i as f64 * dt;
            ts_ms.push(t * 1000.0);
            let yaw_rad = 100.0_f64.to_radians() * t;
            // axis-angle around y -> quaternion
            let half = yaw_rad * 0.5;
            let q = (half.cos(), 0.0, half.sin(), 0.0);
            q_raw.push(q);
            q_smooth.push(q); // identical: zero deviation
            fovs.push((1.0_f64, 1.0_f64));
        }

        force_enabled_for_test(true);
        *SESSION.lock() = Some(DiagSession {
            out_dir: std::path::PathBuf::from("."),
            frames: Vec::new(),
            smoothing_meta: SmoothingMeta::default(),
            video_meta: VideoMeta::default(),
        });

        record_session(&ts_ms, &q_raw, &q_smooth, &fovs, &SmoothingMeta::default(), &VideoMeta::default());

        let s = SESSION.lock();
        let frames = &s.as_ref().unwrap().frames;
        assert_eq!(frames.len(), n);

        // First and last few frames have NaN derivatives (3rd-diff needs i±3 neighbors).
        // Pick a middle frame.
        let mid = &frames[n / 2];
        assert!(mid.delta_total_deg.abs() < 1e-9, "delta_total ~0 (q_smooth == q_raw)");
        assert!((mid.vel_yaw_deg_s - 100.0).abs() < 1e-3, "yaw vel ~100 deg/s, got {}", mid.vel_yaw_deg_s);
        assert!(mid.accel_yaw_deg_s2.abs() < 1e-3, "accel ~0");
        assert!(mid.jerk_yaw_deg_s3.abs() < 1e-3, "jerk ~0");

        // cleanup: drop s before re-locking SESSION (parking_lot is non-reentrant)
        drop(s);
        *SESSION.lock() = None;
        force_enabled_for_test(false);
    }

    #[test]
    #[serial_test::serial]
    fn record_session_nonzero_delta_when_smooth_lags() {
        use nalgebra::UnitQuaternion;
        let qr = UnitQuaternion::from_euler_angles(0.0, 10f64.to_radians(), 0.0);
        let qs = UnitQuaternion::from_euler_angles(0.0, 8f64.to_radians(), 0.0);

        let ts = vec![0.0];
        let q_raw_v = vec![(qr.w, qr.i, qr.j, qr.k)];
        let q_smooth_v = vec![(qs.w, qs.i, qs.j, qs.k)];
        let fovs = vec![(1.0_f64, 1.0_f64)];

        force_enabled_for_test(true);
        *SESSION.lock() = Some(DiagSession {
            out_dir: std::path::PathBuf::from("."),
            frames: Vec::new(),
            smoothing_meta: SmoothingMeta::default(),
            video_meta: VideoMeta::default(),
        });

        record_session(&ts, &q_raw_v, &q_smooth_v, &fovs, &SmoothingMeta::default(), &VideoMeta::default());

        let s = SESSION.lock();
        let f0 = &s.as_ref().unwrap().frames[0];
        assert!((f0.delta_yaw_deg.abs() - 2.0).abs() < 1e-3, "delta_yaw ≈ 2°, got {}", f0.delta_yaw_deg);
        assert!((f0.delta_total_deg - 2.0).abs() < 1e-3);

        // cleanup: drop s before re-locking SESSION (parking_lot is non-reentrant)
        drop(s);
        *SESSION.lock() = None;
        force_enabled_for_test(false);
    }

    #[test]
    #[serial_test::serial] // env var manipulation, must serialize
    fn init_session_creates_directory_when_enabled() {
        // Override data_dir via the GYROFLOW_DATA_DIR env (settings.rs:17 honors it).
        let tmp = tempfile::tempdir().expect("tempdir");
        // Safety: tests run single-threaded thanks to serial_test.
        unsafe {
            std::env::set_var("GYROFLOW_DATA_DIR", tmp.path());
            std::env::set_var("GYROFLOW_SMOOTH_DIAG", "1");
        }
        // Force ENABLED to true via a test-only setter (since OnceLock can't be reset).
        force_enabled_for_test(true);

        // Preflight: settings::data_dir() caches via OnceLock, so if a prior test in
        // this binary already called it, our GYROFLOW_DATA_DIR override is silently
        // ignored. Detect this and skip rather than emit a misleading failure.
        let preflight = crate::settings::data_dir();
        if !preflight.starts_with(tmp.path()) {
            eprintln!(
                "[skip] settings::data_dir() already cached to {}, skipping test (cannot honor GYROFLOW_DATA_DIR after first call)",
                preflight.display()
            );
            *SESSION.lock() = None;
            force_enabled_for_test(false);
            unsafe {
                std::env::remove_var("GYROFLOW_SMOOTH_DIAG");
                std::env::remove_var("GYROFLOW_DATA_DIR");
            }
            return;
        }

        init_session();

        let s = SESSION.lock();
        let dir = s.as_ref().expect("session opened").out_dir.clone();
        drop(s);
        assert!(dir.exists(), "out_dir should exist: {}", dir.display());
        assert!(dir.starts_with(tmp.path()), "out_dir under data_dir");
        assert!(
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("smooth_"))
                .unwrap_or(false),
            "dir name starts with smooth_"
        );

        // cleanup
        *SESSION.lock() = None;
        force_enabled_for_test(false);
        unsafe {
            std::env::remove_var("GYROFLOW_SMOOTH_DIAG");
            std::env::remove_var("GYROFLOW_DATA_DIR");
        }
    }
}
