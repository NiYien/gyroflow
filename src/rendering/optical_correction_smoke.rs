// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Instant;

use gyroflow_core::StabilizationManager;
use gyroflow_core::stabilization::FrameTransform;
use gyroflow_core::synchronization::optical_analysis::{BlendConfig, OpticalBaseMode, measurement_params};
use nalgebra::{Quaternion, UnitQuaternion, Vector3};
use serde_json::Value;

struct TruthMetrics {
    pairs: usize,
    skipped_gaps: usize,
    skipped_held: usize,
    hp_xy_px: f64,
    hp_xy_max_px: f64,
    hp_roll_deg: f64,
    drift_deg_s: f64,
    path_deg: f64,
    truth_path_deg: f64,
}

fn score_against_truth(
    keys_us: &[i64], held: &[bool], estimate: impl Fn(i64) -> UnitQuaternion<f64>,
    truth: impl Fn(f64) -> UnitQuaternion<f64>, fps: f64, focal_px: f64,
) -> Option<TruthMetrics> {
    if keys_us.len() < 2 || held.len() != keys_us.len() - 1 || !fps.is_finite() || fps <= 0.0
        || !focal_px.is_finite() || focal_px <= 0.0 || keys_us.windows(2).any(|ts| ts[1] <= ts[0]) {
        return None;
    }
    let w = (0.25 * fps).round().max(1.0) as usize;
    let mut trajectory = Vec::new();
    let mut accumulated = Vector3::<f64>::zeros();
    let mut skipped_gaps = 0;
    let mut skipped_held = 0;
    let mut path = 0.0;
    let mut truth_path = 0.0;
    for (k, ts) in keys_us.windows(2).enumerate() {
        if (ts[1] - ts[0]) as f64 > 1.5e6 / fps {
            skipped_gaps += 1;
            continue;
        }
        if held[k] {
            skipped_held += 1;
            continue;
        }
        let m_est = estimate(ts[1]).inverse() * estimate(ts[0]);
        let m_truth = truth(ts[1] as f64 / 1000.0).inverse() * truth(ts[0] as f64 / 1000.0);
        let error = (m_truth * m_est.inverse()).scaled_axis();
        if !error.iter().all(|x| x.is_finite()) {
            return None;
        }
        accumulated += error;
        trajectory.push(accumulated);
        path += m_est.angle();
        truth_path += m_truth.angle();
    }
    let pairs = trajectory.len();
    if pairs < w.saturating_mul(2).saturating_add(1) {
        return None;
    }
    let mut prefix = Vec::with_capacity(pairs + 1);
    prefix.push(Vector3::<f64>::zeros());
    for p in &trajectory {
        prefix.push(prefix.last().unwrap() + p);
    }
    let mut xy_sum = 0.0;
    let mut xy_max: f64 = 0.0;
    let mut roll_sum = 0.0;
    for i in w..pairs - w {
        let mean = (prefix[i + w + 1] - prefix[i - w]) / (2 * w + 1) as f64;
        let delta = trajectory[i] - mean;
        let xy_sq = delta.x * delta.x + delta.y * delta.y;
        xy_sum += xy_sq;
        xy_max = xy_max.max(xy_sq);
        roll_sum += delta.z * delta.z;
    }
    let count = (pairs - 2 * w) as f64;
    let duration_s = (keys_us.last()? - keys_us.first()?) as f64 / 1e6;
    Some(TruthMetrics {
        pairs, skipped_gaps, skipped_held,
        hp_xy_px: (xy_sum / count).sqrt() * focal_px,
        hp_xy_max_px: xy_max.sqrt() * focal_px,
        hp_roll_deg: (roll_sum / count).sqrt().to_degrees(),
        drift_deg_s: accumulated.norm().to_degrees() / duration_s,
        path_deg: path.to_degrees(), truth_path_deg: truth_path.to_degrees(),
    })
}

pub fn run(paths: &str) -> i32 {
    let paths = parse_paths(paths);
    if paths.is_empty() {
        eprintln!("No .gyroflow project paths supplied");
        return 2;
    }
    let out_dir = Path::new("target/optical_correction_smoke");
    if let Err(e) = std::fs::create_dir_all(out_dir) {
        eprintln!("Cannot create {}: {e}", out_dir.display());
        return 1;
    }
    let ignore = std::env::var("GYROFLOW_OPTICAL_CORRECTION_SMOKE_IGNORE").as_deref() == Ok("1");
    let score_truth = ignore && std::env::var("GYROFLOW_OPTICAL_CORRECTION_SMOKE_TRUTH").as_deref() == Ok("1");
    let mut summary = String::from("# Optical correction smoke\n\n| Project | frames | measured_frames | rms_deg | from_video | applied | elapsed_ms | Result |\n|---|---:|---:|---:|---|---|---:|---|\n");
    let mut truth_summary = String::from("\n## Pure-optical vs gyro\n\n| Project | trajectory | mode | correction_forced | stabilization_verdict | sync_points | pairs | gaps | held | hp_xy_px | hp_xy_max_px | hp_roll_deg | drift_deg_s | path_deg | truth_path_deg |\n|---|---|---|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|\n");
    let mut exit_code = 0;
    for path in paths {
        let stab = StabilizationManager::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut elapsed_ms = None;
        let mut correction_forced = false;
        let mut truth_gyro = None;
        let mut sync_points = None;
        let mut stabilization_verdict = String::from("unknown");
        let mut scores = [None, None];
        let result = (|| -> Result<(), String> {
            if !Path::new(&path).extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("gyroflow")) {
                return Err("Only .gyroflow projects are accepted".into());
            }
            log::info!(target: "sync", "[optical-smoke] importing {path} ignore_file_motion={ignore}");
            stab.lens_profile_db.write().load_all();
            let url = gyroflow_core::filesystem::path_to_url(&path);
            stab.import_gyroflow_file(&url, true, |_| (), cancel.clone(), false).map_err(|e| format!("import: {e:?}"))?;
            let reconstruction_enabled = stab.optical_ui.read().stab_enabled;
            if reconstruction_enabled {
                stab.set_optical_correction_enabled(true);
                correction_forced = true;
                log::info!(target: "sync", "[optical-smoke] reconstruction was ticked from metadata; switched to the optical correction");
            }
            if stab.gyro.read().quaternions.is_empty() {
                stab.recompute_gyro();
            }
            if score_truth {
                let ignored = stab.gyro.read().ignores_file_motion();
                if ignored {
                    stab.set_ignore_file_motion(false);
                }
                let mut gyro = stab.gyro.read().clone();
                let correction = gyro.optical_correction.take();
                let reconstruction = gyro.optical_stab.take();
                let translation = gyro.optical_translation.take();
                if correction.is_some() || reconstruction.is_some() || translation.is_some() {
                    gyro.integrate();
                }
                sync_points = Some(gyro.get_offsets().len());
                stabilization_verdict = gyro.file_metadata.read().additional_data.get("stabilization_verdict")
                    .and_then(Value::as_str).unwrap_or("unknown").to_owned();
                if gyro.has_motion() && gyro.quaternions.len() >= 2 {
                    truth_gyro = Some(gyro);
                }
            }
            if ignore {
                stab.set_ignore_file_motion(true);
            }
            let started = Instant::now();
            let analyzed = super::analyze_optically(&stab, cancel, None, |_, _, _| ());
            if analyzed.is_ok() {
                stab.recompute_blocking();
            }
            elapsed_ms = Some(started.elapsed().as_millis());
            if analyzed.is_ok() && score_truth {
                let final_gyro = stab.gyro.read().clone();
                if let (Some(truth), Some(correction)) = (&truth_gyro, &final_gyro.optical_correction) {
                    let base = &correction.video_base;
                    let keys_us: Vec<_> = base.iter().map(|(us, _)| *us).collect();
                    let held: Vec<_> = base.windows(2).map(|q| q[0].1.map(f32::to_bits) == q[1].1.map(f32::to_bits)).collect();
                    let fps = stab.params.read().get_scaled_fps();
                    let params = measurement_params(&stab);
                    let k = FrameTransform::get_lens_data_at_timestamp(&params, 0.0, false).0;
                    let focal_px = k[(0, 0)] * 960.0 / params.width as f64;
                    scores[0] = score_against_truth(&keys_us, &held, |us| {
                        let q = base[base.binary_search_by_key(&us, |(ts, _)| *ts).unwrap()].1;
                        UnitQuaternion::from_quaternion(Quaternion::new(q[0] as f64, q[1] as f64, q[2] as f64, q[3] as f64))
                    }, |ms| truth.org_quat_at_timestamp(ms), fps, focal_px);
                    scores[1] = score_against_truth(&keys_us, &held, |us| final_gyro.org_quat_at_timestamp(us as f64 / 1000.0),
                        |ms| truth.org_quat_at_timestamp(ms), fps, focal_px);
                }
            }
            analyzed
        })();
        let info = stab.optical_correction_info();
        let applied = stab.gyro.read().optical_correction_applied;
        let complete_info = info["frames"].as_u64().is_some()
            && info["measured_frames"].as_u64().is_some()
            && info["rms_deg"].as_f64().is_some_and(f64::is_finite)
            && info["from_video"].as_bool().is_some();
        let status = match result {
            Err(e) => e,
            Ok(()) if !complete_info => "Optical correction info is incomplete".into(),
            Ok(()) if !applied => "Optical correction was not applied".into(),
            Ok(()) => "ok".into(),
        };
        let passed = status == "ok";
        let _ = writeln!(summary, "| {} | {} | {} | {} | {} | {applied} | {} | {} |",
            table_text(&path), info_cell(&info, "frames"), info_cell(&info, "measured_frames"),
            info_cell(&info, "rms_deg"), info_cell(&info, "from_video"),
            elapsed_ms.map_or_else(|| "unavailable".into(), |ms| ms.to_string()), table_text(&status));
        log::info!(target: "sync", "[optical-smoke] {path}: info={info} applied={applied} elapsed_ms={elapsed_ms:?} result={status}");
        if score_truth {
            let mode = OpticalBaseMode::resolved();
            let mode = if mode == OpticalBaseMode::Auto {
                let config = BlendConfig::resolved();
                format!("auto ({}, {}, {})", config.ratio_full, config.ratio_none, config.leverage)
            } else { mode.as_str().to_owned() };
            for (trajectory, metrics) in ["base", "base+correction"].into_iter().zip(scores) {
                let cells = metrics.map_or_else(|| std::iter::repeat_n("unavailable", 9).collect::<Vec<_>>().join(" | "), |m| {
                    format!("{} | {} | {} | {:.6} | {:.6} | {:.6} | {:.6} | {:.6} | {:.6}",
                        m.pairs, m.skipped_gaps, m.skipped_held, m.hp_xy_px, m.hp_xy_max_px, m.hp_roll_deg, m.drift_deg_s, m.path_deg, m.truth_path_deg)
                });
                let _ = writeln!(truth_summary, "| {} | {trajectory} | {} | {} | {} | {} | {cells} |",
                    table_text(&path), table_text(&mode), if correction_forced { "yes" } else { "no" }, table_text(&stabilization_verdict),
                    sync_points.map_or_else(|| "unavailable".into(), |n| n.to_string()));
            }
        }
        if !passed {
            exit_code = 1;
            break;
        }
    }
    if score_truth {
        summary.push_str(&truth_summary);
    }
    if let Err(e) = std::fs::write(out_dir.join("summary.txt"), summary) {
        eprintln!("Cannot write smoke summary: {e}");
        return 1;
    }
    exit_code
}

fn parse_paths(s: &str) -> Vec<String> {
    s.split(';').map(str::trim).filter(|path| !path.is_empty()).map(str::to_owned).collect()
}

fn info_cell(info: &Value, key: &str) -> String {
    info.get(key).filter(|value| !value.is_null()).map_or_else(|| "unavailable".into(), Value::to_string)
}

fn table_text(s: &str) -> String {
    s.replace('|', "\\|").replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use nalgebra::{Quaternion, UnitQuaternion};

    const FPS: f64 = 60.0;

    fn keys(count: usize) -> Vec<i64> {
        (0..count).map(|i| (i as f64 * 1e6 / FPS).round() as i64).collect()
    }

    fn truth(ms: f64) -> UnitQuaternion<f64> {
        let t = ms / 1000.0;
        let phase = std::f64::consts::TAU * t;
        UnitQuaternion::from_axis_angle(&nalgebra::Vector3::y_axis(), (0.3 * phase.sin()).to_radians())
            * UnitQuaternion::from_axis_angle(&nalgebra::Vector3::x_axis(), (0.2 * (0.7 * phase).sin()).to_radians())
    }

    fn through_f32(q: UnitQuaternion<f64>) -> UnitQuaternion<f64> {
        let q = q.quaternion();
        UnitQuaternion::new_normalize(Quaternion::new(q.w as f32 as f64, q.i as f32 as f64, q.j as f32 as f64, q.k as f32 as f64))
    }

    fn score(estimate: impl Fn(i64) -> UnitQuaternion<f64>) -> TruthMetrics {
        score_against_truth(&keys(300), &vec![false; 299], estimate, truth, FPS, 1000.0).unwrap()
    }

    #[test]
    fn identical_paths_have_no_error() {
        let m = score(|us| through_f32(truth(us as f64 / 1000.0)));
        assert!(m.hp_xy_px < 0.002, "{}", m.hp_xy_px);
        assert!(m.hp_roll_deg < 1e-4, "{}", m.hp_roll_deg);
        assert!(m.drift_deg_s < 1e-3, "{}", m.drift_deg_s);
        assert_eq!(m.pairs, 299);
        assert_eq!(m.skipped_gaps, 0);
    }

    #[test]
    fn steady_drift_is_drift_not_jitter() {
        let m = score(|us| {
            let t = us as f64 / 1e6;
            through_f32(truth(t * 1000.0) * UnitQuaternion::from_axis_angle(&nalgebra::Vector3::y_axis(), (0.6 * t).to_radians()))
        });
        assert!((m.drift_deg_s / 0.6 - 1.0).abs() < 0.01, "{}", m.drift_deg_s);
        assert!(m.hp_xy_px < 0.02, "{}", m.hp_xy_px);
    }

    #[test]
    fn oscillation_shows_as_jitter() {
        let m = score(|us| {
            let t = us as f64 / 1e6;
            through_f32(truth(t * 1000.0) * UnitQuaternion::from_axis_angle(&nalgebra::Vector3::y_axis(), (0.05 * (std::f64::consts::TAU * 3.0 * t).sin()).to_radians()))
        });
        assert!((m.hp_xy_px / 0.7435 - 1.0).abs() < 0.15, "{}", m.hp_xy_px);
        assert!(m.hp_roll_deg < 0.005, "{}", m.hp_roll_deg);
    }

    #[test]
    fn roll_is_reported_apart() {
        let m = score(|us| {
            let t = us as f64 / 1e6;
            through_f32(truth(t * 1000.0) * UnitQuaternion::from_axis_angle(&nalgebra::Vector3::z_axis(), (0.05 * (std::f64::consts::TAU * 3.0 * t).sin()).to_radians()))
        });
        assert!((m.hp_roll_deg / 0.0426 - 1.0).abs() < 0.15, "{}", m.hp_roll_deg);
        assert!(m.hp_xy_px < 0.02, "{}", m.hp_xy_px);
    }

    #[test]
    fn gaps_are_skipped() {
        let mut keys = keys(300);
        keys.remove(150);
        let m = score_against_truth(&keys, &vec![false; keys.len() - 1], |us| through_f32(truth(us as f64 / 1000.0)), truth, FPS, 1000.0).unwrap();
        assert_eq!(m.skipped_gaps, 1);
        assert_eq!(m.pairs, 297);
        assert!(m.hp_xy_px < 0.002, "{}", m.hp_xy_px);
    }

    #[test]
    fn held_pairs_are_skipped() {
        let keys = keys(300);
        let mut held = vec![false; 299];
        held[150] = true;
        let shift = truth(keys[150] as f64 / 1000.0) * truth(keys[151] as f64 / 1000.0).inverse();
        let m = score_against_truth(&keys, &held, |us| {
            let q = truth(us as f64 / 1000.0);
            through_f32(if us <= keys[150] { q } else { shift * q })
        }, truth, FPS, 1000.0).unwrap();
        assert_eq!(m.skipped_held, 1);
        assert_eq!(m.skipped_gaps, 0);
        assert_eq!(m.pairs, 298);
        assert!(m.hp_xy_px < 0.002, "{}", m.hp_xy_px);
    }

    #[test]
    fn too_short_is_unavailable() {
        assert!(score_against_truth(&keys(20), &vec![false; 19], |us| through_f32(truth(us as f64 / 1000.0)), truth, FPS, 1000.0).is_none());
    }

    #[test]
    fn parse_paths_trims_and_drops_empty_entries() {
        assert_eq!(parse_paths("a.gyroflow; b.gyroflow ;;"), vec!["a.gyroflow", "b.gyroflow"]);
    }

    #[test]
    fn empty_paths_return_usage_error() {
        assert_eq!(run(""), 2);
    }
}
