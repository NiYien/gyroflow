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
// SESSION and DiagSession fields are read/written in Tasks 2-7; allow dead_code for skeleton.
#[allow(dead_code)]
static SESSION: Mutex<Option<DiagSession>> = Mutex::new(None);

#[inline]
pub fn is_enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("GYROFLOW_SMOOTH_DIAG")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    })
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
    // Filled in Task 3.
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

#[allow(clippy::needless_return)]
pub fn init_session() {
    if !is_enabled() {
        return;
    }
    // Task 2: create per-session output directory and initialize SESSION.
}

#[allow(clippy::needless_return)]
#[allow(private_interfaces)]
pub fn record_session(
    _ts_ms: &[f64],
    _q_raw: &[(f64, f64, f64, f64)],
    _q_smooth: &[(f64, f64, f64, f64)],
    _fovs_baseline_and_final: &[(f64, f64)],
    _smoothing_meta: &SmoothingMeta,
    _video_meta: &VideoMeta,
) {
    if !is_enabled() {
        return;
    }
    // Tasks 3+4: populate FrameRecord fields and write dump.csv.
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
}
