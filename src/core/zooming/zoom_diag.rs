// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Adaptive zoom diagnostics: per-run snapshots of every intermediate stage
//! that contributes to the final per-frame fov vector.
//!
//! Always emits a compact summary log (~10 lines, `target = "stab.zoom"`)
//! whenever `calculate_fovs` runs. When `GYROFLOW_ZOOM_DIAG=1`, also dumps a
//! full per-frame CSV to `<cwd>/zoom_diag_output/<session_ts>/run_NNN.csv`.
//!
//! Pipeline stages tracked:
//!   1. `raw_fov`             — fov_iterative output before any range fix
//!   2. `post_range_fix`      — fov_iterative after trim-outside frames are
//!                              replaced with max_fov (mod.rs:87 branch)
//!   3. `after_min_rolling`   — zoom_dynamic rolling min across the window
//!   4. `after_gaussian`      — zoom_dynamic gaussian smooth (or envelope)
//!   5. `final_fov`           — the value actually returned to the stabilizer
//!
//! `pad` captures `pad_edge` first/last values feeding the end boundaries so
//! we can tell whether end-frame zoom is being dragged down by real frames
//! inside the trim window or by edge padding.

use parking_lot::Mutex;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static ENABLED: OnceLock<bool> = OnceLock::new();
static SESSION_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
static RUN_COUNTER: AtomicU32 = AtomicU32::new(0);
static CURRENT: Mutex<Option<RunData>> = Mutex::new(None);

#[inline]
pub fn is_enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("GYROFLOW_ZOOM_DIAG")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    })
}

#[derive(Clone, Copy, Debug)]
pub enum MethodKind {
    GaussianFilter,
    EnvelopeFollower,
}

struct RunData {
    run_id: u32,
    timestamps: Vec<(usize, f64)>,
    trim_ranges: Vec<(f64, f64)>,
    adaptive_zoom_window: f64,
    method: MethodKind,
    raw_fov: Vec<f64>,
    post_range_fix: Vec<f64>,
    after_min_rolling: Vec<f64>,
    after_gaussian: Vec<f64>,
    final_fov: Vec<f64>,
    range_fix_max_fov: Option<f64>,
    range_fix_replaced: usize,
    pad_first: Option<f64>,
    pad_last: Option<f64>,
    pad_left: usize,
    pad_right: usize,
}

/// Start a new diag run. Always called from `calculate_fovs` (even when env
/// var is off) so the summary log is always emitted. Replaces any previous
/// in-flight run (shouldn't happen unless calculate_fovs is reentrant).
pub fn start_run(
    timestamps: &[(usize, f64)],
    trim_ranges: &[(f64, f64)],
    adaptive_zoom_window: f64,
    method: MethodKind,
) {
    let run_id = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    *CURRENT.lock() = Some(RunData {
        run_id,
        timestamps: timestamps.to_vec(),
        trim_ranges: trim_ranges.to_vec(),
        adaptive_zoom_window,
        method,
        raw_fov: Vec::new(),
        post_range_fix: Vec::new(),
        after_min_rolling: Vec::new(),
        after_gaussian: Vec::new(),
        final_fov: Vec::new(),
        range_fix_max_fov: None,
        range_fix_replaced: 0,
        pad_first: None,
        pad_last: None,
        pad_left: 0,
        pad_right: 0,
    });
}

#[inline]
pub fn record_raw_fov(v: &[f64]) {
    if let Some(r) = CURRENT.lock().as_mut() {
        r.raw_fov = v.to_vec();
    }
}

#[inline]
pub fn record_post_range_fix(v: &[f64], max_fov: f64, replaced: usize) {
    if let Some(r) = CURRENT.lock().as_mut() {
        r.post_range_fix = v.to_vec();
        r.range_fix_max_fov = Some(max_fov);
        r.range_fix_replaced = replaced;
    }
}

#[inline]
pub fn record_after_min_rolling(v: &[f64]) {
    if let Some(r) = CURRENT.lock().as_mut() {
        r.after_min_rolling = v.to_vec();
    }
}

#[inline]
pub fn record_after_gaussian(v: &[f64]) {
    if let Some(r) = CURRENT.lock().as_mut() {
        r.after_gaussian = v.to_vec();
    }
}

#[inline]
pub fn record_pad(first: f64, last: f64, left: usize, right: usize) {
    if let Some(r) = CURRENT.lock().as_mut() {
        r.pad_first = Some(first);
        r.pad_last = Some(last);
        r.pad_left = left;
        r.pad_right = right;
    }
}

#[inline]
pub fn record_final(v: &[f64]) {
    if let Some(r) = CURRENT.lock().as_mut() {
        r.final_fov = v.to_vec();
    }
}

/// End the current run: emit summary log, and write CSV if enabled.
pub fn finish_and_dump() {
    let Some(run) = CURRENT.lock().take() else { return };
    emit_summary_log(&run);
    if is_enabled() {
        if let Some(dir) = session_dir() {
            if let Err(e) = write_run_csv(&dir, &run) {
                log::warn!(target: "stab.zoom", "zoom_diag csv write failed: {}", e);
            }
        }
    }
}

fn session_dir() -> Option<PathBuf> {
    SESSION_DIR
        .get_or_init(|| {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            dir.push("zoom_diag_output");
            dir.push(format!("{}", ts));
            match std::fs::create_dir_all(&dir) {
                Ok(_) => {
                    log::info!(target: "stab.zoom", "zoom_diag session opened at {}", dir.display());
                    Some(dir)
                }
                Err(e) => {
                    log::warn!(target: "stab.zoom", "zoom_diag failed to create {}: {}", dir.display(), e);
                    None
                }
            }
        })
        .clone()
}

fn write_run_csv(dir: &std::path::Path, run: &RunData) -> std::io::Result<()> {
    let mut p = dir.to_path_buf();
    p.push(format!("run_{:03}.csv", run.run_id));
    let mut w = BufWriter::new(File::create(&p)?);
    writeln!(
        w,
        "frame_index,frame_seq,ts_ms,within_trim,raw_fov,post_range_fix,after_min_rolling,after_gaussian,final_fov"
    )?;
    let n = run.timestamps.len();
    let l = (n.saturating_sub(1)) as f64;
    let frame_in_trim = |i: usize| -> u8 {
        if run.trim_ranges.is_empty() {
            return 1;
        }
        let hit = run.trim_ranges.iter().any(|r| {
            i >= (l * r.0).floor() as usize && i <= (l * r.1).ceil() as usize
        });
        if hit { 1 } else { 0 }
    };
    let get = |v: &[f64], i: usize| -> String {
        v.get(i).map(|x| format!("{:.6}", x)).unwrap_or_default()
    };
    for i in 0..n {
        let (frame_seq, ts_ms) = run.timestamps[i];
        writeln!(
            w,
            "{},{},{:.4},{},{},{},{},{},{}",
            i,
            frame_seq,
            ts_ms,
            frame_in_trim(i),
            get(&run.raw_fov, i),
            get(&run.post_range_fix, i),
            get(&run.after_min_rolling, i),
            get(&run.after_gaussian, i),
            get(&run.final_fov, i),
        )?;
    }
    log::info!(target: "stab.zoom", "zoom_diag run {:03} -> {}", run.run_id, p.display());
    Ok(())
}

fn emit_summary_log(run: &RunData) {
    let n = run.timestamps.len();
    if n == 0 {
        log::info!(target: "stab.zoom", "run {:03}: empty timestamps", run.run_id);
        return;
    }
    let ts_min = run.timestamps.first().map(|t| t.1).unwrap_or(0.0);
    let ts_max = run.timestamps.last().map(|t| t.1).unwrap_or(0.0);
    let method = match run.method {
        MethodKind::GaussianFilter => "gaussian",
        MethodKind::EnvelopeFollower => "envelope",
    };
    log::info!(
        target: "stab.zoom",
        "run {:03}: n={} ts=[{:.1}..{:.1}]ms window={:.3}s method={} trim_ranges={:?}",
        run.run_id, n, ts_min, ts_max, run.adaptive_zoom_window, method, run.trim_ranges
    );
    if !run.trim_ranges.is_empty() {
        let l = (n.saturating_sub(1)) as f64;
        let frame_ranges: Vec<(usize, usize)> = run
            .trim_ranges
            .iter()
            .map(|r| ((l * r.0).floor() as usize, (l * r.1).ceil() as usize))
            .collect();
        log::info!(
            target: "stab.zoom",
            "run {:03}: trim frame_ranges={:?} replaced={} max_fov_substitute={:.4}",
            run.run_id,
            frame_ranges,
            run.range_fix_replaced,
            run.range_fix_max_fov.unwrap_or(f64::NAN),
        );
    }
    log_stage(run.run_id, "raw_fov         ", &run.raw_fov, &run.timestamps, true);
    log_stage(run.run_id, "post_range_fix  ", &run.post_range_fix, &run.timestamps, false);
    log_stage(run.run_id, "after_min_roll  ", &run.after_min_rolling, &run.timestamps, false);
    log_stage(run.run_id, "after_gaussian  ", &run.after_gaussian, &run.timestamps, false);
    log_stage(run.run_id, "final_fov       ", &run.final_fov, &run.timestamps, true);
    if run.pad_first.is_some() {
        log::info!(
            target: "stab.zoom",
            "run {:03}: pad first={:.4} last={:.4} left={} right={}",
            run.run_id,
            run.pad_first.unwrap_or(f64::NAN),
            run.pad_last.unwrap_or(f64::NAN),
            run.pad_left,
            run.pad_right,
        );
    }
    // Dump head/tail of raw_fov so we can eyeball end behavior even without CSV.
    if !run.raw_fov.is_empty() {
        let head_n = run.raw_fov.len().min(8);
        let tail_n = run.raw_fov.len().saturating_sub(head_n).min(8);
        let head: Vec<String> = run.raw_fov[..head_n].iter().map(|x| format!("{:.4}", x)).collect();
        let tail: Vec<String> = run.raw_fov[run.raw_fov.len() - tail_n..]
            .iter()
            .map(|x| format!("{:.4}", x))
            .collect();
        log::info!(target: "stab.zoom", "run {:03}: raw_fov head={:?} tail={:?}", run.run_id, head, tail);
    }
}

fn log_stage(
    run_id: u32,
    label: &str,
    values: &[f64],
    timestamps: &[(usize, f64)],
    show_mean_std: bool,
) {
    if values.is_empty() {
        log::info!(target: "stab.zoom", "run {:03}: {} (empty)", run_id, label);
        return;
    }
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut argmin = 0usize;
    let mut sum = 0.0f64;
    for (i, &v) in values.iter().enumerate() {
        if !v.is_finite() {
            continue;
        }
        if v < min {
            min = v;
            argmin = i;
        }
        if v > max {
            max = v;
        }
        sum += v;
    }
    let n = values.len() as f64;
    let mean = sum / n;
    let ts_at_argmin = timestamps
        .get(argmin)
        .map(|t| t.1)
        .unwrap_or(f64::NAN);
    if show_mean_std {
        let mut var = 0.0f64;
        for &v in values {
            if v.is_finite() {
                let d = v - mean;
                var += d * d;
            }
        }
        let std = (var / n).sqrt();
        log::info!(
            target: "stab.zoom",
            "run {:03}: {} min={:.4} @ i={} ts={:.1}ms max={:.4} mean={:.4} std={:.4}",
            run_id, label, min, argmin, ts_at_argmin, max, mean, std,
        );
    } else {
        log::info!(
            target: "stab.zoom",
            "run {:03}: {} min={:.4} @ i={} ts={:.1}ms max={:.4}",
            run_id, label, min, argmin, ts_at_argmin, max,
        );
    }
}
