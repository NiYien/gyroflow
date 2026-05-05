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
use std::fs::File;
use std::io::{BufWriter, Write};
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

const CSV_HEADER: &str = "frame_idx,ts_ms,\
q_raw_w,q_raw_x,q_raw_y,q_raw_z,\
q_smooth_w,q_smooth_x,q_smooth_y,q_smooth_z,\
delta_pitch_deg,delta_yaw_deg,delta_roll_deg,delta_total_deg,\
vel_pitch_deg_s,vel_yaw_deg_s,vel_roll_deg_s,\
accel_pitch_deg_s2,accel_yaw_deg_s2,accel_roll_deg_s2,\
jerk_pitch_deg_s3,jerk_yaw_deg_s3,jerk_roll_deg_s3,\
fov_baseline,fov_final";

/// Format an f64 with `digits` decimal places; NaN serializes as empty string (pandas-friendly).
fn fmt_f64(v: f64, digits: usize) -> String {
    if v.is_nan() {
        String::new()
    } else {
        format!("{:.*}", digits, v)
    }
}

/// Write all buffered frames to `<out_dir>/dump.csv`.
fn write_csv(s: &DiagSession) -> std::io::Result<()> {
    let path = s.out_dir.join("dump.csv");
    let mut w = BufWriter::new(File::create(&path)?);
    writeln!(w, "{}", CSV_HEADER)?;
    for f in &s.frames {
        writeln!(
            w,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            f.frame_idx,
            fmt_f64(f.ts_ms, 4),
            fmt_f64(f.q_raw_w, 8), fmt_f64(f.q_raw_x, 8), fmt_f64(f.q_raw_y, 8), fmt_f64(f.q_raw_z, 8),
            fmt_f64(f.q_smooth_w, 8), fmt_f64(f.q_smooth_x, 8), fmt_f64(f.q_smooth_y, 8), fmt_f64(f.q_smooth_z, 8),
            fmt_f64(f.delta_pitch_deg, 6), fmt_f64(f.delta_yaw_deg, 6), fmt_f64(f.delta_roll_deg, 6), fmt_f64(f.delta_total_deg, 6),
            fmt_f64(f.vel_pitch_deg_s, 4), fmt_f64(f.vel_yaw_deg_s, 4), fmt_f64(f.vel_roll_deg_s, 4),
            fmt_f64(f.accel_pitch_deg_s2, 4), fmt_f64(f.accel_yaw_deg_s2, 4), fmt_f64(f.accel_roll_deg_s2, 4),
            fmt_f64(f.jerk_pitch_deg_s3, 4), fmt_f64(f.jerk_yaw_deg_s3, 4), fmt_f64(f.jerk_roll_deg_s3, 4),
            fmt_f64(f.fov_baseline, 6), fmt_f64(f.fov_final, 6),
        )?;
    }
    Ok(())
}

/// Return the current time as an RFC 3339 / ISO 8601 string.
/// Uses the `time` crate for the local UTC offset (already a dep with
/// `local-offset` feature); formats manually since the `formatting` feature
/// is not enabled in this crate's Cargo.toml.
/// Falls back to a plain UTC string if the offset cannot be determined.
fn current_timestamp_iso() -> String {
    // Derive seconds since Unix epoch via std (always available).
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    // Try to read the local UTC offset via the `time` crate so the timestamp
    // is expressed in local time rather than UTC.
    let offset_secs: i32 = time::OffsetDateTime::now_local()
        .map(|dt| dt.offset().whole_seconds())
        .unwrap_or(0);

    // Apply offset to get local wall-clock seconds.
    let local_secs = unix_secs + offset_secs as i64;

    // Decompose into calendar fields (proleptic Gregorian).
    // Days since 1970-01-01.
    let days = local_secs.div_euclid(86400) as i32;
    let secs_of_day = local_secs.rem_euclid(86400) as u32;

    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;

    // Algorithm from https://howardhinnant.github.io/date_algorithms.html
    // (civil_from_days) — public domain.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    // Format the UTC offset part (+HH:MM or Z for UTC).
    let offset_part = if offset_secs == 0 {
        "Z".to_string()
    } else {
        let sign = if offset_secs >= 0 { '+' } else { '-' };
        let abs = offset_secs.unsigned_abs();
        format!("{}{:02}:{:02}", sign, abs / 3600, (abs % 3600) / 60)
    };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{}",
        y, m, d, hour, minute, second, offset_part
    )
}

/// Write `<out_dir>/meta.json` with video / smoothing / zooming context.
fn write_meta_json(s: &DiagSession) -> std::io::Result<()> {
    let path = s.out_dir.join("meta.json");
    let ts_iso = current_timestamp_iso();
    let sid = crate::log_context::LogContext::snapshot().session_id;
    let v = serde_json::json!({
        "session_id": sid,
        "timestamp_iso": ts_iso,
        "video": {
            "path": s.video_meta.path_basename,
            "duration_ms": s.video_meta.duration_ms,
            "frame_count": s.video_meta.frame_count,
            "fps": s.video_meta.fps,
            "width": s.video_meta.width,
            "height": s.video_meta.height,
        },
        "gyro_source": {
            "sample_rate_hz": s.video_meta.gyro_sample_rate_hz,
        },
        "smoothing": {
            "method": s.smoothing_meta.method,
            "method_id": s.smoothing_meta.method_id,
            "params": s.smoothing_meta.params_json,
        },
        "zooming": {
            "adaptive_zoom_window": s.smoothing_meta.adaptive_zoom_window,
            "method": s.smoothing_meta.zoom_method,
            "max_zoom": s.smoothing_meta.max_zoom_pct,
            "max_zoom_iterations": s.smoothing_meta.max_zoom_iterations,
        },
    });
    let body = serde_json::to_string_pretty(&v)?;
    std::fs::write(&path, body)?;
    Ok(())
}

pub fn flush_and_close() {
    if !is_enabled() {
        return;
    }
    let session = match SESSION.lock().take() {
        Some(s) => s,
        None => return,
    };
    if let Err(e) = write_csv(&session) {
        log::warn!(target: "app", "[SmoothDiag] write_csv error: {}", e);
    }
    if let Err(e) = write_meta_json(&session) {
        log::warn!(target: "app", "[SmoothDiag] write_meta_json error: {}", e);
    }
    log::info!(
        target: "app",
        "[SmoothDiag] session closed: {} frames -> {}",
        session.frames.len(),
        session.out_dir.display()
    );
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
    #[serial_test::serial]
    fn flush_writes_dump_csv() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("smooth_test");
        std::fs::create_dir_all(&dir).unwrap();

        force_enabled_for_test(true);
        *SESSION.lock() = Some(DiagSession {
            out_dir: dir.clone(),
            frames: vec![FrameRecord {
                frame_idx: 0,
                ts_ms: 1.5,
                q_raw_w: 1.0, q_raw_x: 0.0, q_raw_y: 0.0, q_raw_z: 0.0,
                q_smooth_w: 1.0, q_smooth_x: 0.0, q_smooth_y: 0.0, q_smooth_z: 0.0,
                delta_pitch_deg: 0.1, delta_yaw_deg: 0.2, delta_roll_deg: 0.3, delta_total_deg: 0.37,
                vel_pitch_deg_s: 1.0, vel_yaw_deg_s: 2.0, vel_roll_deg_s: 3.0,
                accel_pitch_deg_s2: f64::NAN, accel_yaw_deg_s2: f64::NAN, accel_roll_deg_s2: f64::NAN,
                jerk_pitch_deg_s3: f64::NAN, jerk_yaw_deg_s3: f64::NAN, jerk_roll_deg_s3: f64::NAN,
                fov_baseline: 0.85, fov_final: 0.90,
            }],
            smoothing_meta: SmoothingMeta::default(),
            video_meta: VideoMeta::default(),
        });

        flush_and_close();

        let csv_path = dir.join("dump.csv");
        assert!(csv_path.exists(), "dump.csv must be written");
        let body = std::fs::read_to_string(&csv_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 2, "header + 1 row");

        let header = lines[0];
        assert!(header.starts_with("frame_idx,ts_ms,q_raw_w"), "header begins with frame_idx,ts_ms,q_raw_w; got: {header}");
        assert!(header.ends_with("fov_baseline,fov_final"), "header ends with fov columns");
        assert_eq!(header.split(',').count(), 25, "25 columns expected");

        let row = lines[1];
        let cells: Vec<&str> = row.split(',').collect();
        assert_eq!(cells.len(), 25);
        assert_eq!(cells[0], "0");                      // frame_idx
        assert!(cells[1].starts_with("1.5"));           // ts_ms
        assert_eq!(cells[17], "");                      // accel_pitch is NaN -> empty (column index 17)
        assert!(cells[24].starts_with("0.9"));          // fov_final

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

    #[test]
    #[serial_test::serial]
    fn flush_writes_meta_json() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("smooth_meta");
        std::fs::create_dir_all(&dir).unwrap();

        force_enabled_for_test(true);
        *SESSION.lock() = Some(DiagSession {
            out_dir: dir.clone(),
            frames: vec![],
            smoothing_meta: SmoothingMeta {
                method: "Default".into(),
                method_id: 1,
                params_json: serde_json::json!([{"name": "smoothness", "value": 0.5}]),
                adaptive_zoom_window: 4.0,
                zoom_method: "EnvelopeFollower".into(),
                max_zoom_pct: 130.0,
                max_zoom_iterations: 5,
            },
            video_meta: VideoMeta {
                path_basename: "test.mp4".into(),
                duration_ms: 60000.0,
                frame_count: 1800,
                fps: 30.0,
                width: 3840,
                height: 2160,
                gyro_sample_rate_hz: 200.0,
            },
        });
        flush_and_close();

        let meta_path = dir.join("meta.json");
        assert!(meta_path.exists());
        let body = std::fs::read_to_string(&meta_path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["video"]["path"], "test.mp4");
        assert_eq!(v["video"]["fps"], 30.0);
        assert_eq!(v["smoothing"]["method"], "Default");
        assert_eq!(v["smoothing"]["method_id"], 1);
        assert_eq!(v["zooming"]["max_zoom"], 130.0);
        assert_eq!(v["zooming"]["max_zoom_iterations"], 5);
        // timestamp_iso must exist and be a non-empty string
        assert!(v["timestamp_iso"].as_str().map(|s| !s.is_empty()).unwrap_or(false));

        force_enabled_for_test(false);
    }
}
