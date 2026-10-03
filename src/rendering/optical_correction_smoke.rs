// SPDX-License-Identifier: GPL-3.0-or-later

use std::fmt::Write as _;
use std::path::Path;
use std::sync::{Arc, atomic::AtomicBool};
use std::time::Instant;

use gyroflow_core::StabilizationManager;
use serde_json::Value;

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
    let mut summary = String::from("# Optical correction smoke\n\n| Project | frames | measured_frames | rms_deg | from_video | applied | elapsed_ms | Result |\n|---|---:|---:|---:|---|---|---:|---|\n");
    let mut exit_code = 0;
    for path in paths {
        let stab = StabilizationManager::default();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut elapsed_ms = None;
        let result = (|| -> Result<(), String> {
            if !Path::new(&path).extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("gyroflow")) {
                return Err("Only .gyroflow projects are accepted".into());
            }
            log::info!(target: "sync", "[optical-smoke] importing {path} ignore_file_motion={ignore}");
            stab.lens_profile_db.write().load_all();
            let url = gyroflow_core::filesystem::path_to_url(&path);
            stab.import_gyroflow_file(&url, true, |_| (), cancel.clone(), false).map_err(|e| format!("import: {e:?}"))?;
            if stab.gyro.read().quaternions.is_empty() {
                stab.recompute_gyro();
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
        if !passed {
            exit_code = 1;
            break;
        }
    }
    if let Err(e) = std::fs::write(out_dir.join("summary.md"), summary) {
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

    #[test]
    fn parse_paths_trims_and_drops_empty_entries() {
        assert_eq!(parse_paths("a.gyroflow; b.gyroflow ;;"), vec!["a.gyroflow", "b.gyroflow"]);
    }

    #[test]
    fn empty_paths_return_usage_error() {
        assert_eq!(run(""), 2);
    }
}
