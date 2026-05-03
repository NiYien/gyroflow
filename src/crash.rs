// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Rust-panic crash dump pipeline. Complementary to the breakpad OS-level
// crash handler in `util::install_crash_handler` — this captures `panic!`
// (assertion failures, unwrap, etc.) into a self-contained zip with the
// session log, backtrace, and contextual metadata. Phase 4 picks up the zip
// for user-triggered upload.

use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::PathBuf;
use std::sync::OnceLock;

use crate::log_context::LogContext;
use crate::logger;

static PREVIOUS_HOOK: OnceLock<Box<dyn Fn(&PanicHookInfo) + Sync + Send + 'static>> = OnceLock::new();

/// Install the panic hook. Idempotent; chains on top of the existing default
/// hook so terminal backtraces remain visible to `cargo run` users.
pub fn register_panic_hook() {
    if PREVIOUS_HOOK.get().is_some() {
        return;
    }
    let prev = std::panic::take_hook();
    let _ = PREVIOUS_HOOK.set(prev);

    std::panic::set_hook(Box::new(|info| {
        // Best-effort dump first, then delegate to the previous hook so the
        // terminal still shows the original panic backtrace.
        if let Err(e) = write_dump(info) {
            eprintln!("crash: failed to write panic dump: {e}");
        }
        if let Some(prev) = PREVIOUS_HOOK.get() {
            prev(info);
        }
    }));
}

fn write_dump(info: &PanicHookInfo<'_>) -> std::io::Result<PathBuf> {
    let dir = logger::log_dir()
        .map(|p| p.join("crashes"))
        .ok_or_else(|| std::io::Error::other("logger::log_dir() not initialized"))?;
    std::fs::create_dir_all(&dir)?;

    let now = chrono::Local::now();
    let ts = now.format("%Y%m%dT%H%M%S").to_string();
    let sid = logger::session_id();
    let file_name = format!("{ts}-{sid}.zip");
    let final_path = dir.join(&file_name);
    let tmp_path = dir.join(format!(".{file_name}.tmp"));

    // Build zip in memory.
    let mut zip_bytes: Vec<u8> = Vec::with_capacity(256 * 1024);
    {
        let cursor = std::io::Cursor::new(&mut zip_bytes);
        let mut zw = zip::ZipWriter::new(cursor);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(6));

        // 1. session.log — snapshot of the in-memory ring buffer.
        let log_bytes = logger::ring_buffer_snapshot();
        zw.start_file("session.log", opts)?;
        zw.write_all(&log_bytes)?;

        // 2. panic.txt — message + backtrace + location.
        let panic_text = format_panic(info);
        zw.start_file("panic.txt", opts)?;
        zw.write_all(panic_text.as_bytes())?;

        // 3. meta.json — session id, version, os, ts, current TLS context.
        let meta = build_meta_json(&now);
        zw.start_file("meta.json", opts)?;
        zw.write_all(meta.as_bytes())?;

        zw.finish()?;
    }

    // Atomic write: tmp + rename. On Windows, rename overwrites if the dest
    // doesn't exist; we generated a fresh per-second name so collision is
    // unlikely. If it does happen, fall back to non-atomic write.
    if let Ok(mut f) = std::fs::File::create(&tmp_path) {
        f.write_all(&zip_bytes)?;
        f.sync_all().ok();
        drop(f);
        if std::fs::rename(&tmp_path, &final_path).is_err() {
            std::fs::write(&final_path, &zip_bytes)?;
            let _ = std::fs::remove_file(&tmp_path);
        }
    } else {
        std::fs::write(&final_path, &zip_bytes)?;
    }
    Ok(final_path)
}

fn format_panic(info: &PanicHookInfo<'_>) -> String {
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| info.payload().downcast_ref::<String>().map(|s| s.as_str()))
        .unwrap_or("<non-string panic payload>");
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "<unknown>".to_string());
    let bt = std::backtrace::Backtrace::force_capture();
    format!("panic: {payload}\nlocation: {location}\nbacktrace:\n{bt}\n")
}

fn build_meta_json(now: &chrono::DateTime<chrono::Local>) -> String {
    let ctx = LogContext::snapshot();
    let video_path = ctx
        .video_path
        .as_deref()
        .map(|s| serde_json::Value::String(s.into()))
        .unwrap_or(serde_json::Value::Null);
    let op = ctx
        .op
        .as_deref()
        .map(|s| serde_json::Value::String(s.into()))
        .unwrap_or(serde_json::Value::Null);
    let value = serde_json::json!({
        "session_id": ctx.session_id,
        "app_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "ts": now.to_rfc3339(),
        "video_path": video_path,
        "op": op,
    });
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
}

/// If `GYROFLOW_CRASH_TEST=1` is set, panic immediately so the dump path can
/// be exercised without trying to repro a real bug.
pub fn maybe_trigger_test_panic() {
    if std::env::var("GYROFLOW_CRASH_TEST").as_deref() == Ok("1") {
        panic!("GYROFLOW_CRASH_TEST=1: deliberate panic for crash dump validation");
    }
}
