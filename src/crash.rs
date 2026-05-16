// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Rust-panic crash dump pipeline. Complementary to the breakpad OS-level
// crash handler in `util::install_crash_handler` — this captures `panic!`
// (assertion failures, unwrap, etc.) into a self-contained zip with the
// session log, backtrace, and contextual metadata. Phase 4 picks up the zip
// for user-triggered upload.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::panic::PanicHookInfo;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::log_context::LogContext;
use crate::logger;

static PREVIOUS_HOOK: OnceLock<Box<dyn Fn(&PanicHookInfo) + Sync + Send + 'static>> = OnceLock::new();

// --- dedup state ----------------------------------------------------------
//
// Process-internal LRU map keyed on the panic site fingerprint. The hook
// merges any panic that matches an existing entry within 60 seconds: instead
// of writing a new zip, it increments a sibling `<base>.repeats` text file.
// Sweeps stale entries on every invocation; evicts the oldest when capacity
// is exceeded.

const DEDUP_WINDOW_SECS: u64 = 60;
const DEDUP_CAPACITY: usize = 64;

type DedupKey = u64;

struct DedupEntry {
    last_ts:  Instant,
    zip_path: PathBuf,
}

struct DedupLru {
    entries: HashMap<DedupKey, DedupEntry>,
}

impl DedupLru {
    fn new() -> Self { Self { entries: HashMap::new() } }
}

static DEDUP_STATE: OnceLock<Mutex<DedupLru>> = OnceLock::new();

fn dedup_state() -> &'static Mutex<DedupLru> {
    DEDUP_STATE.get_or_init(|| Mutex::new(DedupLru::new()))
}

/// Install the panic hook. Idempotent; chains on top of the existing default
/// hook so terminal backtraces remain visible to `cargo run` users.
pub fn register_panic_hook() {
    if PREVIOUS_HOOK.get().is_some() {
        return;
    }
    let prev = std::panic::take_hook();
    let _ = PREVIOUS_HOOK.set(prev);

    std::panic::set_hook(Box::new(|info| {
        // The hook itself must never panic. Wrap dump dispatch in catch_unwind
        // so a defect in the dedup path cannot recurse into the panic hook.
        let dump_res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            write_dump(info)
        }));
        match dump_res {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("crash: failed to write panic dump: {e}"),
            Err(_)     => eprintln!("crash: panic hook itself panicked, suppressed"),
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

    let payload_str = panic_payload(info);
    let location_tuple = info
        .location()
        .map(|l| (l.file().to_string(), l.line(), l.column()))
        .unwrap_or_else(|| ("<unknown>".to_string(), 0, 0));
    let dedup_key = compute_dedup_key(&location_tuple.0, location_tuple.1, location_tuple.2, &payload_str);

    // Sweep + lookup under the LRU mutex; decide merge vs new-zip path.
    let merge_target: Option<PathBuf> = {
        let mut state = match dedup_state().lock() {
            Ok(g)  => g,
            Err(p) => p.into_inner(),
        };
        let now = Instant::now();
        let window = std::time::Duration::from_secs(DEDUP_WINDOW_SECS);
        state.entries.retain(|_, e| now.duration_since(e.last_ts) <= window);
        if let Some(entry) = state.entries.get_mut(&dedup_key) {
            entry.last_ts = now;
            // Still bumping ts; need the path for sidecar update.
            Some(entry.zip_path.clone())
        } else {
            None
        }
    };

    if let Some(zip_path) = merge_target {
        // Merge path: try to update the sibling .repeats sidecar atomically.
        // On any failure, fall back to writing a new zip so no diagnostic
        // information is lost.
        if zip_path.exists() && update_repeats_sidecar(&zip_path).is_ok() {
            return Ok(zip_path);
        }
        // Fallback (sidecar write failed or sibling zip vanished): drop the
        // stale entry and proceed to new-zip path so the count restarts.
        if let Ok(mut state) = dedup_state().lock() {
            state.entries.remove(&dedup_key);
        }
    }

    // New-zip path.
    let now_dt = chrono::Local::now();
    let ts = now_dt.format("%Y%m%dT%H%M%S").to_string();
    let sid = logger::session_id();
    let file_name = format!("{ts}-{sid}.zip");
    let final_path = dir.join(&file_name);
    let tmp_path = dir.join(format!(".{file_name}.tmp"));

    let zip_bytes = build_zip_bytes(info, &now_dt)?;

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

    // Record / refresh the LRU entry. Evict the oldest if over capacity.
    if let Ok(mut state) = dedup_state().lock() {
        if state.entries.len() >= DEDUP_CAPACITY {
            if let Some(oldest_key) = state
                .entries
                .iter()
                .min_by_key(|(_, e)| e.last_ts)
                .map(|(k, _)| *k)
            {
                state.entries.remove(&oldest_key);
            }
        }
        state.entries.insert(dedup_key, DedupEntry {
            last_ts:  Instant::now(),
            zip_path: final_path.clone(),
        });
    }

    Ok(final_path)
}

fn build_zip_bytes(info: &PanicHookInfo<'_>, now: &chrono::DateTime<chrono::Local>) -> std::io::Result<Vec<u8>> {
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
        let meta = build_meta_json(now);
        zw.start_file("meta.json", opts)?;
        zw.write_all(meta.as_bytes())?;

        zw.finish()?;
    }
    Ok(zip_bytes)
}

fn panic_payload(info: &PanicHookInfo<'_>) -> String {
    info
        .payload()
        .downcast_ref::<&str>()
        .copied()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".to_string())
}

fn compute_dedup_key(file: &str, line: u32, col: u32, payload: &str) -> DedupKey {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    file.hash(&mut hasher);
    line.hash(&mut hasher);
    col.hash(&mut hasher);
    payload.hash(&mut hasher);
    hasher.finish()
}

fn update_repeats_sidecar(zip_path: &Path) -> std::io::Result<u64> {
    let sidecar = zip_path.with_extension("repeats");
    let tmp_path = {
        let mut p = sidecar.clone();
        p.set_extension("repeats.tmp");
        p
    };

    let prev: u64 = match std::fs::read_to_string(&sidecar) {
        Ok(s)  => s.trim().parse::<u64>().unwrap_or(1),
        Err(_) => 1,
    };
    let next = prev.saturating_add(1);
    let body = next.to_string();

    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(body.as_bytes())?;
        f.sync_all().ok();
    }
    if std::fs::rename(&tmp_path, &sidecar).is_err() {
        std::fs::write(&sidecar, body.as_bytes())?;
        let _ = std::fs::remove_file(&tmp_path);
    }
    Ok(next)
}

fn format_panic(info: &PanicHookInfo<'_>) -> String {
    let payload = panic_payload(info);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedup_key_depends_on_all_fields() {
        let a = compute_dedup_key("foo.rs", 10, 5, "msg");
        let b = compute_dedup_key("foo.rs", 10, 5, "msg");
        let c = compute_dedup_key("foo.rs", 10, 6, "msg"); // col differs
        let d = compute_dedup_key("foo.rs", 11, 5, "msg"); // line differs
        let e = compute_dedup_key("bar.rs", 10, 5, "msg"); // file differs
        let f = compute_dedup_key("foo.rs", 10, 5, "msg2"); // payload differs
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_ne!(a, e);
        assert_ne!(a, f);
    }

    #[test]
    fn repeats_sidecar_increments_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("20260516T180000-abcd.zip");
        std::fs::File::create(&zip).unwrap().write_all(b"PK").unwrap();
        // First merge: no prior sidecar → 1 + 1 = 2.
        let n1 = update_repeats_sidecar(&zip).unwrap();
        assert_eq!(n1, 2);
        let sidecar = zip.with_extension("repeats");
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "2");
        // Second merge: 2 + 1 = 3.
        let n2 = update_repeats_sidecar(&zip).unwrap();
        assert_eq!(n2, 3);
        assert_eq!(std::fs::read_to_string(&sidecar).unwrap(), "3");
    }

    #[test]
    fn repeats_sidecar_recovers_from_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("20260516T180000-defg.zip");
        std::fs::File::create(&zip).unwrap().write_all(b"PK").unwrap();
        std::fs::write(zip.with_extension("repeats"), b"not a number").unwrap();
        // Garbage parses as 1 → next = 2.
        let n = update_repeats_sidecar(&zip).unwrap();
        assert_eq!(n, 2);
    }
}
