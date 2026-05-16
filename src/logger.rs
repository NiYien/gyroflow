// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Custom log dispatcher: stderr + gyroflow.log (Debug+) +
// gyroflow-incidents.log (Warn+, append-only) + in-memory ring buffer (used
// by the panic hook to dump session log into the crash zip). Replaces the
// previous simplelog::CombinedLogger setup.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Local};
use log::{Level, LevelFilter, Log, Metadata, Record};
use parking_lot::Mutex;

use crate::log_context::LogContext;

// 13 third-party noise targets that should be limited to Warn+ in the file
// log. Top-level Debug stays unaffected for app code.
const NOISE_TARGETS: &[&str] = &[
    "mp4parse",
    "wgpu",
    "naga",
    "akaze",
    "ureq",
    "rustls",
    "mdk",
];

const RING_BUFFER_BYTES: usize = 8 * 1024 * 1024;
const SESSION_LOG_NAME: &str = "gyroflow.log";
const INCIDENTS_LOG_NAME: &str = "gyroflow-incidents.log";
const LOG_DIR_NAME: &str = "logs";
const ROTATION_KEEP: usize = 4;

static SESSION_ID: OnceLock<String> = OnceLock::new();
static RING_BUFFER: OnceLock<Mutex<VecDeque<u8>>> = OnceLock::new();
static LOG_DIR: OnceLock<PathBuf> = OnceLock::new();

/// Stable 8-character session id for this process (set during init).
pub fn session_id() -> &'static str {
    SESSION_ID.get().map(|s| s.as_str()).unwrap_or("?")
}

/// Snapshot of the in-memory ring buffer (used by the panic hook for
/// crash dumps). Cheap clone since the buffer is bounded.
pub fn ring_buffer_snapshot() -> Vec<u8> {
    if let Some(rb) = RING_BUFFER.get() {
        let g = rb.lock();
        g.iter().copied().collect()
    } else {
        Vec::new()
    }
}

/// Directory used for log files (resolved during init from data_dir).
pub fn log_dir() -> Option<&'static Path> {
    LOG_DIR.get().map(|p| p.as_path())
}

struct GyroflowLogger {
    max_level:    LevelFilter,
    session_file: Mutex<Option<File>>,
    incidents:    Mutex<Option<File>>,
    use_stderr:   bool,
}

impl GyroflowLogger {
    fn matches_noise_target(target: &str) -> bool {
        // Match either exact target ("wgpu") or dotted/double-colon prefix
        // ("wgpu::backend::vulkan", "wgpu_core::device").
        for n in NOISE_TARGETS {
            if target == *n
                || target.starts_with(&format!("{n}::"))
                || target.starts_with(&format!("{n}.")) // dot-namespaced bucket
                || target.starts_with(&format!("{n}_"))  // crate name with `_`
            {
                return true;
            }
        }
        false
    }

    fn is_suppressed_noise_record(target: &str, level: Level, body: &str) -> bool {
        if level == Level::Warn
            && Self::matches_noise_target(target)
            && (target == "mp4parse"
                || target.starts_with("mp4parse::")
                || target.starts_with("mp4parse.")
                || target.starts_with("mp4parse_"))
            && body.contains("InvalidData(HdlrPredefinedNonzero)")
        {
            return true;
        }

        level == Level::Error
            && target == "telemetry_parser::cooke::bin"
            && body.starts_with("Unknown Cooke data: Length: 71 (0x47) bytes")
    }
}

impl Log for GyroflowLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= self.max_level
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let target = record.target();
        let lvl = record.level();
        let body = record.args().to_string();
        if Self::is_suppressed_noise_record(target, lvl, &body) {
            return;
        }
        // Noise filter: third-party crates limited to Warn+.
        if Self::matches_noise_target(target) && lvl > Level::Warn {
            return;
        }
        let line = format_line(record);
        let bytes = line.as_bytes();

        if self.use_stderr {
            // Best-effort: ignore write errors on stderr (closed pipes etc.)
            let _ = std::io::stderr().write_all(bytes);
        }

        if let Some(f) = self.session_file.lock().as_mut() {
            let _ = f.write_all(bytes);
        }

        if lvl <= Level::Warn {
            if let Some(f) = self.incidents.lock().as_mut() {
                let _ = f.write_all(bytes);
            }
        }

        if let Some(rb) = RING_BUFFER.get() {
            let mut g = rb.lock();
            if bytes.len() >= RING_BUFFER_BYTES {
                g.clear();
                g.extend(bytes.iter().copied().skip(bytes.len() - RING_BUFFER_BYTES));
            } else {
                let overflow = (g.len() + bytes.len()).saturating_sub(RING_BUFFER_BYTES);
                if overflow > 0 {
                    g.drain(..overflow);
                }
                g.extend(bytes.iter().copied());
            }
        }
    }

    fn flush(&self) {
        if let Some(f) = self.session_file.lock().as_mut() { let _ = f.flush(); }
        if let Some(f) = self.incidents.lock().as_mut() { let _ = f.flush(); }
    }
}

fn format_line(record: &Record) -> String {
    let now: DateTime<Local> = Local::now();
    let ts = now.format("%Y-%m-%d %H:%M:%S%.3f");
    let lvl_str = match record.level() {
        Level::Error => "ERROR",
        Level::Warn  => "WARN ",
        Level::Info  => "INFO ",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    };
    let target = record.target();
    let body = record.args();

    if record.level() <= Level::Warn {
        // Append context only on warn/error for diagnostic self-containment.
        let ctx = LogContext::snapshot();
        let mut suffix = String::new();
        if !ctx.session_id.is_empty() {
            suffix.push_str(&format!("[sid={} ", ctx.session_id));
        } else {
            suffix.push_str("[sid=? ");
        }
        if let Some(v) = &ctx.video_path {
            suffix.push_str(&format!("vid={v} "));
        }
        if let Some(o) = &ctx.op {
            suffix.push_str(&format!("op={o}"));
        }
        // Trim trailing space and close.
        if suffix.ends_with(' ') { suffix.pop(); }
        suffix.push(']');
        format!("[{ts}] [{lvl_str}] [{target}] {suffix} {body}\n")
    } else {
        format!("[{ts}] [{lvl_str}] [{target}] {body}\n")
    }
}

/// Rotate `.log.3 -> .log.4`, ..., `.log -> .log.1`. Returns Err if the
/// most-recent rename failed (caller should record an Error in the new file).
fn rotate(dir: &Path) -> std::io::Result<()> {
    let main = dir.join(SESSION_LOG_NAME);
    // Drop the oldest if it exists.
    let oldest = dir.join(format!("{SESSION_LOG_NAME}.{ROTATION_KEEP}"));
    if oldest.exists() {
        let _ = std::fs::remove_file(&oldest);
    }
    // Shift .3 -> .4, .2 -> .3, .1 -> .2
    for i in (1..ROTATION_KEEP).rev() {
        let src = dir.join(format!("{SESSION_LOG_NAME}.{i}"));
        let dst = dir.join(format!("{SESSION_LOG_NAME}.{}", i + 1));
        if src.exists() {
            let _ = std::fs::rename(&src, &dst);
        }
    }
    if main.exists() {
        std::fs::rename(&main, dir.join(format!("{SESSION_LOG_NAME}.1")))?;
    }
    Ok(())
}

fn short_session_id() -> String {
    let id = uuid::Uuid::new_v4().simple().to_string();
    id[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logger_suppresses_only_known_mp4parse_hdlr_predefined_noise() {
        assert!(GyroflowLogger::is_suppressed_noise_record(
            "mp4parse",
            Level::Warn,
            "InvalidData(HdlrPredefinedNonzero)",
        ));
        assert!(GyroflowLogger::is_suppressed_noise_record(
            "mp4parse::read",
            Level::Warn,
            "metadata parse returned InvalidData(HdlrPredefinedNonzero)",
        ));

        assert!(!GyroflowLogger::is_suppressed_noise_record(
            "mp4parse",
            Level::Warn,
            "InvalidData(UnexpectedEof)",
        ));
        assert!(!GyroflowLogger::is_suppressed_noise_record(
            "gyroflow",
            Level::Warn,
            "InvalidData(HdlrPredefinedNonzero)",
        ));
        assert!(!GyroflowLogger::is_suppressed_noise_record(
            "mp4parse",
            Level::Error,
            "InvalidData(HdlrPredefinedNonzero)",
        ));
    }

    #[test]
    fn logger_suppresses_known_cooke_unknown_data_noise() {
        assert!(GyroflowLogger::is_suppressed_noise_record(
            "telemetry_parser::cooke::bin",
            Level::Error,
            "Unknown Cooke data: Length: 71 (0x47) bytes",
        ));

        assert!(!GyroflowLogger::is_suppressed_noise_record(
            "telemetry_parser::cooke::bin",
            Level::Error,
            "Unknown Cooke data: Length: 72 (0x48) bytes",
        ));
        assert!(!GyroflowLogger::is_suppressed_noise_record(
            "telemetry_parser::cooke::bin",
            Level::Error,
            "Failed to parse YAML: bad input",
        ));
        assert!(!GyroflowLogger::is_suppressed_noise_record(
            "gyroflow_core::gyro_source",
            Level::Error,
            "Unknown Cooke data: Length: 71 (0x47) bytes",
        ));
    }
}

/// Initialize the logging system. Idempotent (no-op on second call).
/// Layout: `<data_dir>/logs/{gyroflow.log, gyroflow.log.1..4, gyroflow-incidents.log, crashes/}`
pub fn init() {
    if SESSION_ID.get().is_some() {
        return; // already initialized
    }
    // 1. Resolve log dir under data_dir; ensure crashes/ subdir exists.
    let data_dir = gyroflow_core::settings::data_dir();
    let dir = data_dir.join(LOG_DIR_NAME);
    let crashes = dir.join("crashes");
    let _ = std::fs::create_dir_all(&crashes);
    let _ = LOG_DIR.set(dir.clone());

    // 1a. Sweep orphan crash sidecars (`.repeats`/`.dismissed`/`.uploaded`
    // whose sibling `<base>.zip` no longer exists). Silent + best-effort.
    sweep_orphan_crash_sidecars(&crashes);

    // 2. Generate session id and seed LogContext.
    let sid = short_session_id();
    let _ = SESSION_ID.set(sid.clone());
    LogContext::set_session_id(sid.clone());

    // 3. Initialize ring buffer storage.
    let _ = RING_BUFFER.set(Mutex::new(VecDeque::with_capacity(RING_BUFFER_BYTES)));

    // 4. Rotate then open new session log + incidents log.
    let rotation_err = rotate(&dir).err();
    let session_file = OpenOptions::new()
        .create(true).truncate(true).write(true)
        .open(dir.join(SESSION_LOG_NAME))
        .ok();
    let incidents = OpenOptions::new()
        .create(true).append(true)
        .open(dir.join(INCIDENTS_LOG_NAME))
        .ok();

    let logger = Box::new(GyroflowLogger {
        max_level:    LevelFilter::Debug,
        session_file: Mutex::new(session_file),
        incidents:    Mutex::new(incidents),
        use_stderr:   !cfg!(target_os = "android"),
    });
    if let Err(e) = log::set_boxed_logger(logger) {
        // Already set (e.g. test). Not fatal.
        eprintln!("logger::init: set_boxed_logger failed: {e}");
    }
    log::set_max_level(LevelFilter::Debug);

    // 5. First line: announce session start. If rotation failed, also flag it.
    log::info!(target: "app", "Gyroflow session start: sid={sid} version={}", env!("CARGO_PKG_VERSION"));
    if let Some(e) = rotation_err {
        log::error!(target: "app", "Log rotation failed: {e}");
    }
}

// Sweep stale `.repeats`/`.dismissed`/`.uploaded` sidecars whose
// sibling zip has been manually deleted by the user. Silent on errors —
// failure here is purely cosmetic and must not block startup.
fn sweep_orphan_crash_sidecars(crashes: &Path) {
    let entries = match std::fs::read_dir(crashes) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = match path.extension().and_then(|s| s.to_str()) {
            Some(e) => e,
            None    => continue,
        };
        if !matches!(ext, "repeats" | "dismissed" | "uploaded") {
            continue;
        }
        let zip = path.with_extension("zip");
        if !zip.exists() {
            if let Err(e) = std::fs::remove_file(&path) {
                log::debug!(target: "app", "sweep_orphan_crash_sidecars: remove {path:?} failed: {e}");
            }
        }
    }
}
