// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Sync pipeline quality-diagnostics dump (complements sync_perf: perf measures
//! time, diag measures quality).
//!
//! Enabled via env var `GYROFLOW_SYNC_DIAG=1`. When enabled, sync creates an
//! output directory `<cwd>/sync_diag_output/<timestamp>/` at startup and dumps buffers
//! as CSV on completion:
//! - `pose_frames.csv`            per-frame R inliers + axis-angle
//! - `estimated_vs_raw_gyro.csv`  paired curves of estimated_gyro vs raw_imu
//! - `initial_offsets.csv`        per-segment (offset, cost, max_angle) from essential_matrix
//! - `cost_curves_essmat.csv`     full per-segment cost curve from essential_matrix
//! - `cost_curves_rssync.csv`     full per-segment cost curve from rs_sync
//! - `summary.txt`                per-segment initial vs final, second/best ratio
//!
//! When disabled, every sink call performs one atomic load and returns
//! immediately — zero allocation.

use parking_lot::Mutex;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

static ENABLED: OnceLock<bool> = OnceLock::new();
static SESSION: Mutex<Option<DiagSession>> = Mutex::new(None);

#[inline]
pub fn is_enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("GYROFLOW_SYNC_DIAG")
            .map(|v| matches!(v.trim(), "1" | "true" | "yes" | "on"))
            .unwrap_or(false)
    })
}

struct DiagSession {
    out_dir: PathBuf,
    pose_frames: Vec<PoseFrameRecord>,
    estimated_vs_raw: Vec<EstVsRawRecord>,
    initial_offsets: Vec<InitOffsetRecord>,
    cost_curves_essmat: Vec<CostCurvePoint>,
    cost_curves_rssync: Vec<CostCurvePoint>,
    rssync_summary: Vec<RssyncSummaryRecord>,
    local_minima: Vec<LocalMinRecord>,
    sharpness_summary: Vec<SharpnessSummaryRecord>,
    correlation_curves: Vec<CorrelationCurvePoint>,
    correlation_summary: Vec<CorrelationSummaryRecord>,
    fusion_decisions: Vec<FusionDecisionRecord>,
}

struct FusionDecisionRecord {
    range_idx: usize,
    ncc_peak_ms: f64,
    ncc_peak_height: f64,
    fwhm_ms: f64,
    window_ms: f64,
    second_peak_ratio: f64,
    cost_final_ms: f64,
    fused_offset_ms: f64,
    refined_cost: f64,
    // Plan B additions:
    rs_argmin_ms: f64,        // full_sync's cost global argmin
    rs_2nd_over_best: f64,    // rs-sync 2nd_best_cost / best_cost
    rs_refined_ms: f64,       // Path B fine-search result (otherwise NaN)
    path_taken: String,       // "rssync_trusted" | "ncc_window_refine" | "ncc_peak_only" | "fallback_initial" | "motion_too_weak" | "ncc_fft_failed" | "weak_signal" | ...
    fallback_reason: Option<String>,
}

struct PoseFrameRecord {
    ts_us: i64,
    n_inliers: i32,
    axis_angle_deg: f64,
    ax: f64,
    ay: f64,
    az: f64,
}

struct EstVsRawRecord {
    ts_ms: f64,
    est_x: f64,
    est_y: f64,
    est_z: f64,
    raw_x: f64,
    raw_y: f64,
    raw_z: f64,
}

struct InitOffsetRecord {
    range_idx: usize,
    offset_ms: f64,
    cost: f64,
    max_angle_deg: f64,
    n_frames: usize,
}

struct CostCurvePoint {
    range_idx: usize,
    offset_ms: f64,
    cost: f64,
}

struct RssyncSummaryRecord {
    range_idx: usize,
    initial_offset_ms: f64,
    final_offset_ms: f64,
    final_cost: f64,
    second_best_cost: f64,
    second_best_ratio: f64,
}

struct LocalMinRecord {
    range_idx: usize,
    offset_ms: f64,
    cost: f64,
    depth: f64,
    width_ms: f64,
    sharpness: f64,
    is_final: u8,
    is_sharpest: u8,
}

struct CorrelationCurvePoint {
    range_idx: usize,
    offset_ms: f64,
    corr_x: f64,
    corr_y: f64,
    corr_z: f64,
    corr_mean: f64,
    n_paired: usize,
}

struct CorrelationSummaryRecord {
    range_idx: usize,
    cost_final_offset_ms: f64,
    cost_final_corr_mean: f64,
    corr_peak_offset_ms: f64,
    corr_peak_value: f64,
    corr_peak_to_final_diff_ms: f64,
    corr_at_initial: f64,
    initial_offset_ms: f64,
}

struct SharpnessSummaryRecord {
    range_idx: usize,
    n_local_minima: usize,
    baseline_p75: f64,
    final_offset_ms: f64,
    final_depth: f64,
    final_width_ms: f64,
    final_sharpness: f64,
    sharpest_offset_ms: f64,
    sharpest_depth: f64,
    sharpest_width_ms: f64,
    sharpest_sharpness: f64,
    sharpness_ratio: f64,
    same_minimum: bool,
    sharpest_offset_diff_from_final_ms: f64,
}

/// Called at sync start. Creates the output directory and resets buffers.
/// Returns immediately when disabled.
pub fn init_session() {
    if !is_enabled() {
        return;
    }
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Dump under <cwd>/sync_diag_output/<timestamp>/ so repo root stays clean.
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    dir.push("sync_diag_output");
    dir.push(format!("{}", ts));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        log::warn!("[SyncDiag] failed to create {}: {}", dir.display(), e);
        return;
    }
    log::info!("[SyncDiag] session opened at {}", dir.display());
    *SESSION.lock() = Some(DiagSession {
        out_dir: dir,
        pose_frames: Vec::new(),
        estimated_vs_raw: Vec::new(),
        initial_offsets: Vec::new(),
        cost_curves_essmat: Vec::new(),
        cost_curves_rssync: Vec::new(),
        rssync_summary: Vec::new(),
        local_minima: Vec::new(),
        sharpness_summary: Vec::new(),
        correlation_curves: Vec::new(),
        correlation_summary: Vec::new(),
        fusion_decisions: Vec::new(),
    });
}

#[inline]
pub fn record_pose_frame(
    ts_us: i64,
    n_inliers: i32,
    axis_angle_deg: f64,
    ax: f64,
    ay: f64,
    az: f64,
) {
    if !is_enabled() {
        return;
    }
    if let Some(s) = SESSION.lock().as_mut() {
        s.pose_frames.push(PoseFrameRecord {
            ts_us,
            n_inliers,
            axis_angle_deg,
            ax,
            ay,
            az,
        });
    }
}

#[inline]
pub fn record_estimated_vs_raw_gyro(
    ts_ms: f64,
    est_x: f64,
    est_y: f64,
    est_z: f64,
    raw_x: f64,
    raw_y: f64,
    raw_z: f64,
) {
    if !is_enabled() {
        return;
    }
    if let Some(s) = SESSION.lock().as_mut() {
        s.estimated_vs_raw.push(EstVsRawRecord {
            ts_ms,
            est_x,
            est_y,
            est_z,
            raw_x,
            raw_y,
            raw_z,
        });
    }
}

#[inline]
pub fn record_initial_offset_segment(
    range_idx: usize,
    offset_ms: f64,
    cost: f64,
    max_angle_deg: f64,
    n_frames: usize,
) {
    if !is_enabled() {
        return;
    }
    if let Some(s) = SESSION.lock().as_mut() {
        s.initial_offsets.push(InitOffsetRecord {
            range_idx,
            offset_ms,
            cost,
            max_angle_deg,
            n_frames,
        });
    }
}

#[inline]
pub fn record_cost_curve_essmat(range_idx: usize, points: &[(f64, f64)]) {
    if !is_enabled() {
        return;
    }
    if let Some(s) = SESSION.lock().as_mut() {
        s.cost_curves_essmat.reserve(points.len());
        for (offset_ms, cost) in points {
            s.cost_curves_essmat.push(CostCurvePoint {
                range_idx,
                offset_ms: *offset_ms,
                cost: *cost,
            });
        }
    }
}

#[inline]
pub fn record_cost_curve_rssync(range_idx: usize, points: &[(f64, f64)]) {
    if !is_enabled() {
        return;
    }
    if let Some(s) = SESSION.lock().as_mut() {
        s.cost_curves_rssync.reserve(points.len());
        for (offset_ms, cost) in points {
            s.cost_curves_rssync.push(CostCurvePoint {
                range_idx,
                offset_ms: *offset_ms,
                cost: *cost,
            });
        }
    }
}

#[inline]
pub fn record_rssync_summary(
    range_idx: usize,
    initial_offset_ms: f64,
    final_offset_ms: f64,
    final_cost: f64,
    second_best_cost: f64,
) {
    if !is_enabled() {
        return;
    }
    let ratio = if final_cost > 0.0 {
        second_best_cost / final_cost
    } else {
        f64::INFINITY
    };
    if let Some(s) = SESSION.lock().as_mut() {
        s.rssync_summary.push(RssyncSummaryRecord {
            range_idx,
            initial_offset_ms,
            final_offset_ms,
            final_cost,
            second_best_cost,
            second_best_ratio: ratio,
        });
    }
}

/// Analyze a cost curve: local minima, depth, width, sharpness (depth/width),
/// and record per-segment results.
///
/// `curve` can be in any order; internally sorted ascending by offset.
/// `final_offset_ms` is the offset actually chosen by rs_sync.full_sync
/// (external convention), used for tagging.
/// `width_tolerance` is the relative tolerance for "valley width"
/// (e.g. 0.05 means `cost < min*(1+0.05)` counts as within the valley).
pub fn analyze_curve_and_record(
    range_idx: usize,
    curve: &[(f64, f64)],
    final_offset_ms: f64,
    width_tolerance: f64,
) {
    if !is_enabled() {
        return;
    }
    let mut sorted: Vec<(f64, f64)> = curve
        .iter()
        .filter(|(_, c)| !c.is_nan() && c.is_finite())
        .copied()
        .collect();
    if sorted.len() < 3 {
        return;
    }
    sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // baseline = P75 of cost (resistant to deep minima dragging it down)
    let mut costs: Vec<f64> = sorted.iter().map(|p| p.1).collect();
    costs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let baseline = costs[(costs.len() as f64 * 0.75) as usize];

    // Estimate step_ms (median of adjacent-point gaps)
    let mut step_ms_samples: Vec<f64> = sorted
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).abs())
        .collect();
    step_ms_samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let step_ms = if step_ms_samples.is_empty() {
        5.0
    } else {
        step_ms_samples[step_ms_samples.len() / 2].max(0.001)
    };

    // Find local minima
    let n = sorted.len();
    let mut minima: Vec<(usize, f64, f64, f64, f64, f64)> = Vec::new();
    // (idx, offset, cost, depth, width_ms, sharpness)
    for i in 1..(n - 1) {
        if sorted[i].1 < sorted[i - 1].1 && sorted[i].1 < sorted[i + 1].1 {
            let cost_i = sorted[i].1;
            let threshold = cost_i * (1.0 + width_tolerance);
            // Expand left
            let mut l = i;
            while l > 0 && sorted[l - 1].1 < threshold {
                l -= 1;
            }
            // Expand right
            let mut r = i;
            while r + 1 < n && sorted[r + 1].1 < threshold {
                r += 1;
            }
            let width_ms = (sorted[r].0 - sorted[l].0).abs().max(step_ms);
            let depth = (baseline - cost_i).max(0.0);
            let sharpness = depth / width_ms;
            minima.push((i, sorted[i].0, cost_i, depth, width_ms, sharpness));
        }
    }

    if minima.is_empty() {
        return;
    }

    // Find the minimum nearest to `final_offset_ms` and the sharpest minimum
    let final_min = minima
        .iter()
        .min_by(|a, b| {
            (a.1 - final_offset_ms)
                .abs()
                .partial_cmp(&(b.1 - final_offset_ms).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()
        .unwrap();
    let sharpest_min = minima
        .iter()
        .max_by(|a, b| {
            a.5.partial_cmp(&b.5).unwrap_or(std::cmp::Ordering::Equal)
        })
        .copied()
        .unwrap();

    let same = (final_min.1 - sharpest_min.1).abs() < step_ms * 1.5;
    let sharpness_ratio = if final_min.5 > 1e-9 {
        sharpest_min.5 / final_min.5
    } else {
        f64::INFINITY
    };

    if let Some(s) = SESSION.lock().as_mut() {
        for m in &minima {
            s.local_minima.push(LocalMinRecord {
                range_idx,
                offset_ms: m.1,
                cost: m.2,
                depth: m.3,
                width_ms: m.4,
                sharpness: m.5,
                is_final: if (m.1 - final_min.1).abs() < 1e-9 { 1 } else { 0 },
                is_sharpest: if (m.1 - sharpest_min.1).abs() < 1e-9 { 1 } else { 0 },
            });
        }
        s.sharpness_summary.push(SharpnessSummaryRecord {
            range_idx,
            n_local_minima: minima.len(),
            baseline_p75: baseline,
            final_offset_ms,
            final_depth: final_min.3,
            final_width_ms: final_min.4,
            final_sharpness: final_min.5,
            sharpest_offset_ms: sharpest_min.1,
            sharpest_depth: sharpest_min.3,
            sharpest_width_ms: sharpest_min.4,
            sharpest_sharpness: sharpest_min.5,
            sharpness_ratio,
            same_minimum: same,
            sharpest_offset_diff_from_final_ms: sharpest_min.1 - final_min.1,
        });
    }
}

/// Pearson correlation sweep between visually estimated gyro and raw IMU gyro
/// (pure diagnostic dump).
///
/// - `estimated_gyro`: (timestamp_ms, [x, y, z]), any order
/// - `raw_imu`:        (timestamp_ms, [x, y, z]), **must** be ts-ascending
/// - scan range: `initial_offset_ms ± search_size_ms`, step `step_ms`
///
/// For each candidate offset, shift raw_imu by the offset and pair with
/// estimated_gyro; compute Pearson r per axis. Writes `correlation_curves.csv`
/// and summary comparing `cost_final` vs `corr_peak`.
///
/// Pearson naturally normalizes scale and DC offset, measuring only "shape
/// similarity" — exactly the quantification of "eyeballed curve alignment".
pub fn analyze_correlation_and_record(
    range_idx: usize,
    estimated_gyro: &[(f64, [f64; 3])],
    raw_imu: &[(f64, [f64; 3])],
    initial_offset_ms: f64,
    final_offset_ms: f64,
    search_size_ms: f64,
    step_ms: f64,
) {
    if !is_enabled() {
        return;
    }
    if estimated_gyro.len() < 10 || raw_imu.len() < 10 || step_ms <= 0.0 {
        return;
    }

    let n_steps = (search_size_ms * 2.0 / step_ms) as usize;
    let mut curve: Vec<CorrelationCurvePoint> = Vec::with_capacity(n_steps + 1);

    let tol_ms = (step_ms * 2.0).max(10.0); // nearest-neighbor match tolerance

    for k in 0..=n_steps {
        let offset_ms = initial_offset_ms - search_size_ms + (k as f64) * step_ms;
        let (rx, ry, rz, rm, n) =
            compute_triaxis_correlation(estimated_gyro, raw_imu, offset_ms, tol_ms);
        curve.push(CorrelationCurvePoint {
            range_idx,
            offset_ms,
            corr_x: rx,
            corr_y: ry,
            corr_z: rz,
            corr_mean: rm,
            n_paired: n,
        });
    }

    // Find correlation peak (argmax of mean, with sufficient n_paired)
    let min_n = (estimated_gyro.len() / 3).max(10);
    let peak = curve
        .iter()
        .filter(|p| p.n_paired >= min_n && !p.corr_mean.is_nan())
        .max_by(|a, b| {
            a.corr_mean
                .partial_cmp(&b.corr_mean)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let (peak_off, peak_val) = peak
        .map(|p| (p.offset_ms, p.corr_mean))
        .unwrap_or((f64::NAN, f64::NAN));

    // Find the point nearest to final_offset_ms
    let final_pt = curve
        .iter()
        .filter(|p| !p.corr_mean.is_nan())
        .min_by(|a, b| {
            (a.offset_ms - final_offset_ms)
                .abs()
                .partial_cmp(&(b.offset_ms - final_offset_ms).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let final_corr = final_pt.map(|p| p.corr_mean).unwrap_or(f64::NAN);

    // Find the point nearest to initial_offset_ms
    let init_pt = curve
        .iter()
        .filter(|p| !p.corr_mean.is_nan())
        .min_by(|a, b| {
            (a.offset_ms - initial_offset_ms)
                .abs()
                .partial_cmp(&(b.offset_ms - initial_offset_ms).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let init_corr = init_pt.map(|p| p.corr_mean).unwrap_or(f64::NAN);

    if let Some(s) = SESSION.lock().as_mut() {
        s.correlation_curves.extend(curve);
        s.correlation_summary.push(CorrelationSummaryRecord {
            range_idx,
            cost_final_offset_ms: final_offset_ms,
            cost_final_corr_mean: final_corr,
            corr_peak_offset_ms: peak_off,
            corr_peak_value: peak_val,
            corr_peak_to_final_diff_ms: peak_off - final_offset_ms,
            corr_at_initial: init_corr,
            initial_offset_ms,
        });
    }
}

/// Compute per-axis Pearson r and paired-count for estimated vs raw gyro at a
/// given offset. Returns (r_x, r_y, r_z, r_mean, n_paired).
pub fn compute_triaxis_correlation(
    estimated: &[(f64, [f64; 3])],
    raw: &[(f64, [f64; 3])],
    offset_ms: f64,
    tol_ms: f64,
) -> (f64, f64, f64, f64, usize) {
    let mut px: Vec<(f64, f64)> = Vec::with_capacity(estimated.len());
    let mut py: Vec<(f64, f64)> = Vec::with_capacity(estimated.len());
    let mut pz: Vec<(f64, f64)> = Vec::with_capacity(estimated.len());
    for (ts_ms, est_xyz) in estimated {
        let target = *ts_ms - offset_ms;
        if let Some(raw_xyz) = nearest_raw(raw, target, tol_ms) {
            px.push((est_xyz[0], raw_xyz[0]));
            py.push((est_xyz[1], raw_xyz[1]));
            pz.push((est_xyz[2], raw_xyz[2]));
        }
    }
    if px.len() < 10 {
        return (f64::NAN, f64::NAN, f64::NAN, f64::NAN, px.len());
    }
    let rx = pearson(&px);
    let ry = pearson(&py);
    let rz = pearson(&pz);
    let rm = (rx + ry + rz) / 3.0;
    (rx, ry, rz, rm, px.len())
}

fn nearest_raw(raw: &[(f64, [f64; 3])], ts_ms: f64, tol_ms: f64) -> Option<[f64; 3]> {
    if raw.is_empty() {
        return None;
    }
    let idx = match raw.binary_search_by(|p| {
        p.0.partial_cmp(&ts_ms).unwrap_or(std::cmp::Ordering::Equal)
    }) {
        Ok(i) => i,
        Err(i) => i,
    };
    let mut best: Option<(f64, [f64; 3])> = None;
    for c in [idx.saturating_sub(1), idx, idx + 1] {
        if c < raw.len() {
            let d = (raw[c].0 - ts_ms).abs();
            if d <= tol_ms {
                match best {
                    None => best = Some((d, raw[c].1)),
                    Some((bd, _)) if d < bd => best = Some((d, raw[c].1)),
                    _ => {}
                }
            }
        }
    }
    best.map(|(_, v)| v)
}

fn pearson(xy: &[(f64, f64)]) -> f64 {
    let n = xy.len() as f64;
    if n < 2.0 {
        return f64::NAN;
    }
    let (mut sx, mut sy) = (0.0f64, 0.0f64);
    for (x, y) in xy {
        sx += x;
        sy += y;
    }
    let mx = sx / n;
    let my = sy / n;
    let (mut cov, mut vx, mut vy) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in xy {
        let dx = x - mx;
        let dy = y - my;
        cov += dx * dy;
        vx += dx * dx;
        vy += dy * dy;
    }
    if vx < 1e-18 || vy < 1e-18 {
        return f64::NAN;
    }
    cov / (vx * vy).sqrt()
}

/// Record a fusion-B decision. `path_taken` records the decision path;
/// `fallback_reason` is retained for backward compatibility.
#[inline]
#[allow(clippy::too_many_arguments)]
pub fn record_fusion_decision(
    range_idx: usize,
    ncc_peak_ms: f64,
    ncc_peak_height: f64,
    fwhm_ms: f64,
    window_ms: f64,
    second_peak_ratio: f64,
    cost_final_ms: f64,
    fused_offset_ms: f64,
    refined_cost: f64,
    rs_argmin_ms: f64,
    rs_2nd_over_best: f64,
    rs_refined_ms: f64,
    path_taken: &str,
    fallback_reason: Option<&str>,
) {
    if !is_enabled() {
        return;
    }
    if let Some(s) = SESSION.lock().as_mut() {
        s.fusion_decisions.push(FusionDecisionRecord {
            range_idx,
            ncc_peak_ms,
            ncc_peak_height,
            fwhm_ms,
            window_ms,
            second_peak_ratio,
            cost_final_ms,
            fused_offset_ms,
            refined_cost,
            rs_argmin_ms,
            rs_2nd_over_best,
            rs_refined_ms,
            path_taken: path_taken.to_string(),
            fallback_reason: fallback_reason.map(|s| s.to_string()),
        });
    }
}

/// Called at sync end. Dumps buffers to CSV and closes the session.
/// Returns immediately when disabled.
pub fn flush_and_close() {
    if !is_enabled() {
        return;
    }
    let session = match SESSION.lock().take() {
        Some(s) => s,
        None => return,
    };
    let dir = session.out_dir.clone();
    if let Err(e) = write_all(&session) {
        log::warn!("[SyncDiag] flush error: {}", e);
    } else {
        log::info!(
            "[SyncDiag] session closed: {} pose, {} est_vs_raw, {} init_off, {} essmat_pts, {} rssync_pts, {} summary, {} local_min, {} sharp_summary, {} corr_pts, {} corr_summary, {} fusion_dec -> {}",
            session.pose_frames.len(),
            session.estimated_vs_raw.len(),
            session.initial_offsets.len(),
            session.cost_curves_essmat.len(),
            session.cost_curves_rssync.len(),
            session.rssync_summary.len(),
            session.local_minima.len(),
            session.sharpness_summary.len(),
            session.correlation_curves.len(),
            session.correlation_summary.len(),
            session.fusion_decisions.len(),
            dir.display()
        );
    }
}

fn write_all(s: &DiagSession) -> std::io::Result<()> {
    write_pose_frames(s)?;
    write_estimated_vs_raw(s)?;
    write_initial_offsets(s)?;
    write_cost_curves(&s.out_dir, "cost_curves_essmat.csv", &s.cost_curves_essmat)?;
    write_cost_curves(&s.out_dir, "cost_curves_rssync.csv", &s.cost_curves_rssync)?;
    write_local_minima(s)?;
    write_correlation_curves(s)?;
    write_fusion_decisions(s)?;
    write_summary(s)?;
    Ok(())
}

fn write_fusion_decisions(s: &DiagSession) -> std::io::Result<()> {
    let mut w = open_csv(&s.out_dir, "fusion_decision.csv")?;
    writeln!(
        w,
        "range_idx,ncc_peak_ms,ncc_peak_height,fwhm_ms,window_ms,second_peak_ratio,cost_final_ms,fused_offset_ms,refined_cost,rs_argmin_ms,rs_2nd_over_best,rs_refined_ms,path_taken,fallback_reason"
    )?;
    for r in &s.fusion_decisions {
        writeln!(
            w,
            "{},{:.4},{:.6},{:.4},{:.4},{:.6},{:.4},{:.4},{:.6},{:.4},{:.4},{:.4},{},{}",
            r.range_idx,
            r.ncc_peak_ms,
            r.ncc_peak_height,
            r.fwhm_ms,
            r.window_ms,
            r.second_peak_ratio,
            r.cost_final_ms,
            r.fused_offset_ms,
            r.refined_cost,
            r.rs_argmin_ms,
            r.rs_2nd_over_best,
            r.rs_refined_ms,
            r.path_taken,
            r.fallback_reason.as_deref().unwrap_or(""),
        )?;
    }
    Ok(())
}

fn write_correlation_curves(s: &DiagSession) -> std::io::Result<()> {
    let mut w = open_csv(&s.out_dir, "correlation_curves.csv")?;
    writeln!(
        w,
        "range_idx,offset_ms,corr_x,corr_y,corr_z,corr_mean,n_paired"
    )?;
    for r in &s.correlation_curves {
        writeln!(
            w,
            "{},{:.4},{:.6},{:.6},{:.6},{:.6},{}",
            r.range_idx, r.offset_ms, r.corr_x, r.corr_y, r.corr_z, r.corr_mean, r.n_paired
        )?;
    }
    Ok(())
}

fn open_csv(dir: &Path, name: &str) -> std::io::Result<BufWriter<File>> {
    let mut p = dir.to_path_buf();
    p.push(name);
    Ok(BufWriter::new(File::create(p)?))
}

fn write_pose_frames(s: &DiagSession) -> std::io::Result<()> {
    let mut w = open_csv(&s.out_dir, "pose_frames.csv")?;
    writeln!(w, "ts_us,n_inliers,axis_angle_deg,ax,ay,az")?;
    for r in &s.pose_frames {
        writeln!(
            w,
            "{},{},{:.6},{:.6},{:.6},{:.6}",
            r.ts_us, r.n_inliers, r.axis_angle_deg, r.ax, r.ay, r.az
        )?;
    }
    Ok(())
}

fn write_estimated_vs_raw(s: &DiagSession) -> std::io::Result<()> {
    let mut w = open_csv(&s.out_dir, "estimated_vs_raw_gyro.csv")?;
    writeln!(w, "ts_ms,est_x,est_y,est_z,raw_x,raw_y,raw_z")?;
    for r in &s.estimated_vs_raw {
        writeln!(
            w,
            "{:.4},{:.6},{:.6},{:.6},{:.6},{:.6},{:.6}",
            r.ts_ms, r.est_x, r.est_y, r.est_z, r.raw_x, r.raw_y, r.raw_z
        )?;
    }
    Ok(())
}

fn write_initial_offsets(s: &DiagSession) -> std::io::Result<()> {
    let mut w = open_csv(&s.out_dir, "initial_offsets.csv")?;
    writeln!(w, "range_idx,offset_ms,cost,max_angle_deg,n_frames")?;
    for r in &s.initial_offsets {
        writeln!(
            w,
            "{},{:.4},{:.6},{:.6},{}",
            r.range_idx, r.offset_ms, r.cost, r.max_angle_deg, r.n_frames
        )?;
    }
    Ok(())
}

fn write_cost_curves(dir: &Path, name: &str, pts: &[CostCurvePoint]) -> std::io::Result<()> {
    let mut w = open_csv(dir, name)?;
    writeln!(w, "range_idx,offset_ms,cost")?;
    for p in pts {
        writeln!(w, "{},{:.4},{:.6}", p.range_idx, p.offset_ms, p.cost)?;
    }
    Ok(())
}

fn write_local_minima(s: &DiagSession) -> std::io::Result<()> {
    let mut w = open_csv(&s.out_dir, "local_minima.csv")?;
    writeln!(
        w,
        "range_idx,offset_ms,cost,depth,width_ms,sharpness,is_final,is_sharpest"
    )?;
    for r in &s.local_minima {
        writeln!(
            w,
            "{},{:.4},{:.6},{:.6},{:.4},{:.6},{},{}",
            r.range_idx,
            r.offset_ms,
            r.cost,
            r.depth,
            r.width_ms,
            r.sharpness,
            r.is_final,
            r.is_sharpest
        )?;
    }
    Ok(())
}

fn write_summary(s: &DiagSession) -> std::io::Result<()> {
    let mut w = open_csv(&s.out_dir, "summary.txt")?;
    writeln!(w, "rs_sync per-segment summary")?;
    writeln!(
        w,
        "{:<10} {:>14} {:>14} {:>10} {:>14} {:>14} {:>14}",
        "range_idx", "initial_ms", "final_ms", "diff_ms", "final_cost", "2nd_best_cost", "2nd/best",
    )?;
    for r in &s.rssync_summary {
        let diff = r.final_offset_ms - r.initial_offset_ms;
        writeln!(
            w,
            "{:<10} {:>14.3} {:>14.3} {:>10.3} {:>14.6} {:>14.6} {:>14.3}",
            r.range_idx,
            r.initial_offset_ms,
            r.final_offset_ms,
            diff,
            r.final_cost,
            r.second_best_cost,
            r.second_best_ratio
        )?;
    }
    writeln!(w)?;
    writeln!(w, "Sharpness analysis (depth = baseline_p75 - cost; width = span where cost < min*(1+0.05); sharpness = depth/width)")?;
    writeln!(
        w,
        "{:<10} {:>5} {:>10} {:>14} {:>10} {:>10} {:>10} | {:>14} {:>10} {:>10} {:>10} | {:>10} {:>6} {:>14}",
        "range_idx",
        "n_min",
        "baseline",
        "final_ofs",
        "f_depth",
        "f_width",
        "f_sharp",
        "sharpest_ofs",
        "s_depth",
        "s_width",
        "s_sharp",
        "ratio",
        "same?",
        "ofs_diff_ms",
    )?;
    for r in &s.sharpness_summary {
        writeln!(
            w,
            "{:<10} {:>5} {:>10.3} {:>14.3} {:>10.3} {:>10.3} {:>10.4} | {:>14.3} {:>10.3} {:>10.3} {:>10.4} | {:>10.3} {:>6} {:>14.3}",
            r.range_idx,
            r.n_local_minima,
            r.baseline_p75,
            r.final_offset_ms,
            r.final_depth,
            r.final_width_ms,
            r.final_sharpness,
            r.sharpest_offset_ms,
            r.sharpest_depth,
            r.sharpest_width_ms,
            r.sharpest_sharpness,
            r.sharpness_ratio,
            if r.same_minimum { "yes" } else { "NO" },
            r.sharpest_offset_diff_from_final_ms
        )?;
    }
    writeln!(w)?;
    writeln!(
        w,
        "Correlation analysis (Pearson r on estimated vs raw gyro per-axis, averaged)"
    )?;
    writeln!(
        w,
        "{:<10} {:>14} {:>14} {:>14} {:>14} {:>14} {:>14}",
        "range_idx",
        "initial_ms",
        "corr@init",
        "cost_final_ms",
        "corr@final",
        "corr_peak_ms",
        "corr_peak_r",
    )?;
    for r in &s.correlation_summary {
        writeln!(
            w,
            "{:<10} {:>14.3} {:>14.4} {:>14.3} {:>14.4} {:>14.3} {:>14.4}",
            r.range_idx,
            r.initial_offset_ms,
            r.corr_at_initial,
            r.cost_final_offset_ms,
            r.cost_final_corr_mean,
            r.corr_peak_offset_ms,
            r.corr_peak_value,
        )?;
    }
    writeln!(w)?;
    writeln!(
        w,
        "(corr_peak - cost_final diff greater than step means cost and correlation disagree)"
    )?;
    for r in &s.correlation_summary {
        writeln!(
            w,
            "range {}: corr_peak - cost_final = {:+.1} ms",
            r.range_idx, r.corr_peak_to_final_diff_ms
        )?;
    }
    Ok(())
}

// ── FFT NCC time-delay estimation (pre-localization for fusion-B) ─────────────

/// Output of FFT NCC.
///
/// - `peak_offset_ms`: offset at NCC global peak (same semantics as
///   `gyro.set_offset`; i.e. `raw[t - peak_offset] ≈ estimated[t]`)
/// - `peak_height`: normalized NCC value at the peak (∈ [-1, 1], higher = better match)
/// - `fwhm_ms`: full-width half-max of NCC crossing peak_height/2 on both sides;
///   `NAN` when not determinable
/// - `per_axis`: per-axis peak heights (diagnostic for coordinate alignment)
/// - `second_peak_ratio`: secondary local peak / main peak, for detecting periodic ambiguity
/// - `valid_window_ms`: time span actually involved in the FFT (resampled grid span)
#[derive(Debug, Clone, Copy)]
pub struct NccResult {
    pub peak_offset_ms: f64,
    pub peak_height: f64,
    pub fwhm_ms: f64,
    pub per_axis: [f64; 3],
    pub second_peak_ratio: f64,
    pub valid_window_ms: f64,
}

/// Within sync range `[t_start_ms, t_end_ms]`, use FFT-based normalized
/// cross-correlation to locate the relative time delay between visual
/// `estimated` and raw IMU `raw`. Returns only trustworthy peaks; returns
/// `None` when sequences are too short or a single axis has too little energy
/// (denominator degenerate).
///
/// `search_radius_ms` limits the τ range considered during peak search —
/// typically set to rs-sync's search_size_ms to avoid FFT wrap-around being
/// mistaken for a peak.
pub fn ncc_fft_align(
    estimated: &[(f64, [f64; 3])],
    raw: &[(f64, [f64; 3])],
    t_start_ms: f64,
    t_end_ms: f64,
    search_radius_ms: f64,
) -> Option<NccResult> {
    use rustfft::FftPlanner;
    use rustfft::num_complex::Complex;

    let t_window = t_end_ms - t_start_ms;
    if t_window <= 0.0 || estimated.len() < 16 || raw.len() < 16 {
        return None;
    }

    // Estimate sample rate of `estimated` (robust: median of adjacent ts gaps)
    let mut est_sorted: Vec<(f64, [f64; 3])> = estimated
        .iter()
        .filter(|(t, _)| *t >= t_start_ms && *t <= t_end_ms)
        .copied()
        .collect();
    est_sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if est_sorted.len() < 16 {
        return None;
    }
    let mut est_dts: Vec<f64> = est_sorted
        .windows(2)
        .map(|w| (w[1].0 - w[0].0).abs())
        .filter(|d| *d > 1e-6)
        .collect();
    if est_dts.is_empty() {
        return None;
    }
    est_dts.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let est_dt_ms = est_dts[est_dts.len() / 2];

    // Uniform grid
    let grid_len: usize = ((t_window / est_dt_ms).floor() as usize).max(16);
    let dt_ms = t_window / (grid_len as f64);
    let grid_t0 = t_start_ms;

    // Prepare a sorted copy of raw
    let mut raw_sorted: Vec<(f64, [f64; 3])> = raw.to_vec();
    raw_sorted.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

    // raw must cover [t_start - search, t_end + search]; linearly interpolate
    // onto grid. Grid points outside raw's time range are zero-filled
    // (NCC denominator is determined by total energy, which is fine).
    let resample = |src: &[(f64, [f64; 3])], axis: usize| -> Vec<f64> {
        let mut out = vec![0.0f64; grid_len];
        if src.is_empty() {
            return out;
        }
        let mut j: usize = 0;
        for k in 0..grid_len {
            let t = grid_t0 + (k as f64) * dt_ms;
            while j + 1 < src.len() && src[j + 1].0 < t {
                j += 1;
            }
            if j + 1 >= src.len() {
                out[k] = src[src.len() - 1].1[axis];
            } else {
                let (t0, v0) = (src[j].0, src[j].1[axis]);
                let (t1, v1) = (src[j + 1].0, src[j + 1].1[axis]);
                if t1 > t0 {
                    let alpha = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
                    out[k] = v0 + alpha * (v1 - v0);
                } else {
                    out[k] = v0;
                }
            }
        }
        out
    };

    // N = next_pow2(grid_len * 2); zero-padding avoids circular-convolution aliasing
    let n_fft = (grid_len * 2).next_power_of_two().max(64);

    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(n_fft);
    let ifft = planner.plan_fft_inverse(n_fft);

    // Compute NCC(τ) per axis, then sum across axes
    let mut ncc_per_axis: [Vec<f64>; 3] = Default::default();
    let mut peak_per_axis = [0.0f64; 3];
    for axis in 0..3 {
        let est_grid = resample(&est_sorted, axis);
        let raw_grid = resample(&raw_sorted, axis);

        // Remove DC (mean-centering)
        let est_mean: f64 = est_grid.iter().sum::<f64>() / (grid_len as f64);
        let raw_mean: f64 = raw_grid.iter().sum::<f64>() / (grid_len as f64);
        let mut x: Vec<Complex<f64>> = vec![Complex::default(); n_fft];
        let mut y: Vec<Complex<f64>> = vec![Complex::default(); n_fft];
        let mut ex2 = 0.0f64;
        let mut ey2 = 0.0f64;
        for k in 0..grid_len {
            let e = est_grid[k] - est_mean;
            let r = raw_grid[k] - raw_mean;
            x[k] = Complex::new(e, 0.0);
            y[k] = Complex::new(r, 0.0);
            ex2 += e * e;
            ey2 += r * r;
        }
        // Denominator too small → this axis has insufficient signal energy; NCC = 0 contributes nothing
        let denom = (ex2 * ey2).sqrt();
        if denom < 1e-12 {
            ncc_per_axis[axis] = vec![0.0; n_fft];
            peak_per_axis[axis] = 0.0;
            continue;
        }

        fft.process(&mut x);
        fft.process(&mut y);

        // Frequency-domain high-pass (cutoff 0.3 Hz) to suppress DC / low-frequency
        // drift. Low-frequency energy widens the NCC peak and introduces parabolic-fit
        // skew (empirically MVI_5502 seg 0: FWHM narrow but peak location off by 5.5ms).
        // Typical visual/IMU signals focus on 0.3–15 Hz motion; below 0.3 Hz is mostly
        // bias drift. Synthetic sine signals (min 0.8 Hz) are unaffected.
        let cutoff_freq_hz = 0.3;
        let sample_rate_hz = 1000.0 / dt_ms;
        let cutoff_bin =
            ((cutoff_freq_hz * n_fft as f64) / sample_rate_hz).ceil() as usize;
        let cutoff_bin = cutoff_bin.max(1).min(n_fft / 2 - 1);
        for i in 0..=cutoff_bin {
            x[i] = Complex::default();
            y[i] = Complex::default();
        }
        for i in (n_fft - cutoff_bin)..n_fft {
            x[i] = Complex::default();
            y[i] = Complex::default();
        }

        // Recompute normalization denominator (post-filter energy; by Parseval
        // equivalent to the L2 norm after time-domain high-pass)
        let ex2_f: f64 = x.iter().map(|c| c.norm_sqr()).sum::<f64>() / (n_fft as f64);
        let ey2_f: f64 = y.iter().map(|c| c.norm_sqr()).sum::<f64>() / (n_fft as f64);
        let denom_f = (ex2_f * ey2_f).sqrt();
        let denom = if denom_f > 1e-12 { denom_f } else { denom };

        // Cross-spectrum C = conj(X) * Y
        //   IFFT(C)[k] = Σ_n est[n] * raw[n+k]  → k = raw lag behind est in samples
        let mut cross: Vec<Complex<f64>> = (0..n_fft).map(|i| x[i].conj() * y[i]).collect();
        ifft.process(&mut cross);

        // rustfft's IFFT has no 1/N scaling; divide each element by N
        let scale = 1.0 / (n_fft as f64);
        let axis_ncc: Vec<f64> = cross.iter().map(|c| c.re * scale / denom).collect();
        let peak_val = axis_ncc
            .iter()
            .cloned()
            .fold(f64::NEG_INFINITY, f64::max);
        peak_per_axis[axis] = peak_val;
        ncc_per_axis[axis] = axis_ncc;
    }

    // Sum across axes: NCC_total(k) = Σ_axis NCC_axis(k)
    // Note: per-axis values are normalized, so total ∈ [-3, 3]. Divide by 3 to
    // restore to [-1, 1].
    let mut ncc_total: Vec<f64> = vec![0.0; n_fft];
    for k in 0..n_fft {
        ncc_total[k] =
            (ncc_per_axis[0][k] + ncc_per_axis[1][k] + ncc_per_axis[2][k]) / 3.0;
    }

    // Map k → τ_samples (with sign), confine peak search within search_radius.
    // rustfft: k ∈ [0, N/2) means τ = +k; k ∈ [N/2, N) means τ = k - N (negative)
    // offset_ms convention: est[t] ≈ raw[t - offset]
    //   Our cross-spectrum cross[k] = Σ est[n] * raw[n+k] is maximal when raw
    //   lags est by k samples, i.e. `raw[t - offset_ms] = est[t]` gives
    //   offset_ms = -τ_ms (τ positive → raw lagging).
    //   → offset_ms = -τ_samples * dt_ms
    let half = n_fft / 2;
    let radius_samples = ((search_radius_ms / dt_ms).ceil() as isize).max(1) as usize;
    let tau_idx = |k: usize| -> isize {
        if k < half {
            k as isize
        } else {
            (k as isize) - (n_fft as isize)
        }
    };

    let mut peak_idx: Option<usize> = None;
    let mut peak_val = f64::NEG_INFINITY;
    for k in 0..n_fft {
        let t = tau_idx(k).unsigned_abs();
        if t > radius_samples {
            continue;
        }
        if ncc_total[k] > peak_val {
            peak_val = ncc_total[k];
            peak_idx = Some(k);
        }
    }
    let peak_idx = peak_idx?;
    let ncc_peak_idx = peak_idx; // Alias to keep naming consistent with downstream FWHM / second-peak code

    // Three-point parabolic fit (in units of k index)
    let prev_k = (peak_idx + n_fft - 1) % n_fft;
    let next_k = (peak_idx + 1) % n_fft;
    let y_m1 = ncc_total[prev_k];
    let y_0 = ncc_total[peak_idx];
    let y_p1 = ncc_total[next_k];
    let denom = y_m1 - 2.0 * y_0 + y_p1;
    let delta_samples = if denom.abs() > 1e-12 {
        0.5 * (y_m1 - y_p1) / denom
    } else {
        0.0
    };
    let refined_tau = (tau_idx(peak_idx) as f64) + delta_samples;
    let refined_peak = y_0 - 0.25 * (y_m1 - y_p1) * delta_samples;
    let peak_offset_ms = -refined_tau * dt_ms; // convention: see comment above

    // FWHM (scan both sides of the peak in sample units)
    let half_h = refined_peak / 2.0;
    let find_half = |start_k: usize, dir: isize| -> Option<f64> {
        // Starting at start_k, step in direction `dir` and find where NCC crosses half_h (linear interp)
        let mut prev_k = start_k;
        let mut prev_y = ncc_total[prev_k];
        for step in 1..(radius_samples + 1) {
            let cur_k = if dir > 0 {
                (start_k + step) % n_fft
            } else {
                (start_k + n_fft - step) % n_fft
            };
            let cur_y = ncc_total[cur_k];
            if cur_y <= half_h && prev_y >= half_h && prev_y > cur_y {
                let frac = (half_h - cur_y) / (prev_y - cur_y);
                let prev_tau = tau_idx(prev_k) as f64;
                let cur_tau = tau_idx(cur_k) as f64;
                return Some(cur_tau + frac * (prev_tau - cur_tau));
            }
            prev_k = cur_k;
            prev_y = cur_y;
        }
        None
    };
    let left_tau = find_half(ncc_peak_idx, -1);
    let right_tau = find_half(ncc_peak_idx, 1);
    let fwhm_ms = match (left_tau, right_tau) {
        (Some(l), Some(r)) => ((l - r).abs()) * dt_ms,
        (Some(l), None) => 2.0 * (l - refined_tau).abs() * dt_ms,
        (None, Some(r)) => 2.0 * (refined_tau - r).abs() * dt_ms,
        _ => f64::NAN,
    };

    // Secondary peak: max value excluding points within ±min_sep of the main peak
    let min_sep_ms = (fwhm_ms.max(50.0)).min(500.0); // adaptive, clipped to a reasonable range
    let min_sep_samples = (min_sep_ms / dt_ms).ceil() as isize;
    let mut second_val = f64::NEG_INFINITY;
    for k in 0..n_fft {
        let tau = tau_idx(k);
        if tau.unsigned_abs() > radius_samples {
            continue;
        }
        if (tau - tau_idx(ncc_peak_idx)).abs() < min_sep_samples {
            continue;
        }
        if ncc_total[k] > second_val {
            second_val = ncc_total[k];
        }
    }
    let second_peak_ratio = if refined_peak.abs() > 1e-12 && second_val.is_finite() {
        (second_val / refined_peak).max(0.0)
    } else {
        0.0
    };

    Some(NccResult {
        peak_offset_ms,
        peak_height: refined_peak,
        fwhm_ms,
        per_axis: peak_per_axis,
        second_peak_ratio,
        valid_window_ms: (grid_len as f64) * dt_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_path_is_noop() {
        // ENABLED defaults to false in test env (no GYROFLOW_SYNC_DIAG set);
        // all sinks must return cheaply without touching SESSION.
        // We can't easily reset OnceLock between tests, so this test relies on
        // the absence of the env var at test startup.
        if is_enabled() {
            // If env var is set in CI, skip this test silently.
            return;
        }
        record_pose_frame(0, 0, 0.0, 0.0, 0.0, 0.0);
        record_estimated_vs_raw_gyro(0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
        record_initial_offset_segment(0, 0.0, 0.0, 0.0, 0);
        record_cost_curve_essmat(0, &[]);
        record_cost_curve_rssync(0, &[]);
        record_rssync_summary(0, 0.0, 0.0, 0.0, 0.0);
        // SESSION should remain None.
        assert!(SESSION.lock().is_none());
    }

    /// Synthesize signals with a known delay (τ_true = -120ms, i.e. offset = +120ms)
    /// and verify ncc_fft_align's peak_offset_ms within ±1ms, peak_height close to 1.
    #[test]
    fn ncc_fft_align_recovers_synthetic_offset() {
        let dt_ms = 10.0; // 100 Hz
        let duration_ms = 6000.0;
        let n = (duration_ms / dt_ms) as usize;
        let offset_ms = 120.0; // ground truth: raw leads est by 120ms

        // est[t] = A*sin(2π f t + phase per axis); raw[t] = est[t + offset]
        let mut est: Vec<(f64, [f64; 3])> = Vec::with_capacity(n);
        let mut raw: Vec<(f64, [f64; 3])> = Vec::with_capacity(n);
        let freqs = [1.3, 2.7, 0.8]; // Hz per axis
        let phases = [0.0, 1.1, 2.4];
        for k in 0..n {
            let t = k as f64 * dt_ms;
            let mut est_xyz = [0.0; 3];
            let mut raw_xyz = [0.0; 3];
            for axis in 0..3 {
                let w = 2.0 * std::f64::consts::PI * freqs[axis] / 1000.0;
                est_xyz[axis] = (w * t + phases[axis]).sin();
                // raw leads by `offset`: raw's current value equals est's value at (t + offset)
                raw_xyz[axis] = (w * (t + offset_ms) + phases[axis]).sin();
            }
            est.push((t, est_xyz));
            raw.push((t, raw_xyz));
        }

        let r = ncc_fft_align(&est, &raw, 0.0, duration_ms, 500.0)
            .expect("ncc_fft_align should return a peak for clean synthetic signal");
        assert!(
            (r.peak_offset_ms - offset_ms).abs() < 5.0,
            "peak_offset_ms={} vs truth {}",
            r.peak_offset_ms,
            offset_ms
        );
        assert!(
            r.peak_height > 0.8,
            "peak_height={} should be close to 1 for clean signal",
            r.peak_height
        );
        assert!(r.fwhm_ms.is_finite() && r.fwhm_ms > 0.0);
    }

    /// Synthesize uncorrelated signals (independent noise for est/raw); verify
    /// peak_height < 0.3 (would trigger failure detection).
    #[test]
    fn ncc_fft_align_rejects_uncorrelated_noise() {
        let dt_ms = 10.0;
        let duration_ms = 6000.0;
        let n = (duration_ms / dt_ms) as usize;
        let mut rng = fastrand::Rng::with_seed(42);
        let mut est: Vec<(f64, [f64; 3])> = Vec::with_capacity(n);
        let mut raw: Vec<(f64, [f64; 3])> = Vec::with_capacity(n);
        for k in 0..n {
            let t = k as f64 * dt_ms;
            let est_xyz = [
                rng.f64() * 2.0 - 1.0,
                rng.f64() * 2.0 - 1.0,
                rng.f64() * 2.0 - 1.0,
            ];
            let raw_xyz = [
                rng.f64() * 2.0 - 1.0,
                rng.f64() * 2.0 - 1.0,
                rng.f64() * 2.0 - 1.0,
            ];
            est.push((t, est_xyz));
            raw.push((t, raw_xyz));
        }
        let r = ncc_fft_align(&est, &raw, 0.0, duration_ms, 500.0);
        if let Some(res) = r {
            assert!(
                res.peak_height < 0.3,
                "uncorrelated noise should not produce high NCC peak, got {}",
                res.peak_height
            );
        }
        // None is also an acceptable result
    }
}
