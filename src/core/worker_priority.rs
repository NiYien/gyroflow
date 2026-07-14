// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien
//
// Lower the OS priority of rayon worker threads so the UI thread keeps
// getting time slices even when sync/render pegs every core.
//
// Default level is BelowNormal; the env var GYROFLOW_WORKER_PRIORITY
// (normal | below_normal | lowest) overrides it. WorkerPriority::Normal
// is a no-op — no priority API call is made — so users can always
// restore today's behavior by setting the env var to `normal`.

use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerPriority {
    Normal,
    BelowNormal,
    Lowest,
}

impl WorkerPriority {
    fn as_str(self) -> &'static str {
        match self {
            WorkerPriority::Normal => "Normal",
            WorkerPriority::BelowNormal => "BelowNormal",
            WorkerPriority::Lowest => "Lowest",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Resolved {
    level: WorkerPriority,
    from_env: bool,
}

static RESOLVED: OnceLock<Resolved> = OnceLock::new();
static LOGGED: OnceLock<()> = OnceLock::new();

fn parse_env(raw: &str) -> Option<WorkerPriority> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "normal" => Some(WorkerPriority::Normal),
        "below_normal" | "below-normal" | "belownormal" => Some(WorkerPriority::BelowNormal),
        "lowest" => Some(WorkerPriority::Lowest),
        _ => None,
    }
}

fn resolve_inner() -> Resolved {
    match std::env::var("GYROFLOW_WORKER_PRIORITY") {
        Ok(raw) if !raw.is_empty() => match parse_env(&raw) {
            Some(level) => Resolved { level, from_env: true },
            None => {
                log::warn!(
                    target: "lifecycle",
                    "GYROFLOW_WORKER_PRIORITY={} invalid, falling back to below_normal",
                    raw
                );
                Resolved { level: WorkerPriority::BelowNormal, from_env: true }
            }
        },
        _ => Resolved { level: WorkerPriority::BelowNormal, from_env: false },
    }
}

pub fn resolve() -> WorkerPriority {
    let resolved = *RESOLVED.get_or_init(resolve_inner);
    if LOGGED.set(()).is_ok() {
        log::info!(
            target: "lifecycle",
            "worker_priority resolved={} source={}",
            resolved.level.as_str(),
            if resolved.from_env { "env" } else { "default" }
        );
    }
    resolved.level
}

/// Apply the resolved worker priority to the current OS thread.
/// Returns silently on `Normal` (no API call). Logs and continues on failure.
pub fn apply_to_current_thread() {
    let level = resolve();
    if level == WorkerPriority::Normal {
        return;
    }

    if let Err(err) = set_priority_native(level) {
        log::warn!(
            target: "lifecycle",
            "set_current_thread_priority({}) failed: {}",
            level.as_str(),
            err
        );
    }
}

#[cfg(windows)]
fn set_priority_native(level: WorkerPriority) -> Result<(), String> {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL, THREAD_PRIORITY_LOWEST,
    };
    let prio = match level {
        WorkerPriority::Normal => return Ok(()),
        WorkerPriority::BelowNormal => THREAD_PRIORITY_BELOW_NORMAL,
        WorkerPriority::Lowest => THREAD_PRIORITY_LOWEST,
    };
    unsafe {
        SetThreadPriority(GetCurrentThread(), prio)
            .map_err(|e| format!("SetThreadPriority: {:?}", e))
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn set_priority_native(level: WorkerPriority) -> Result<(), String> {
    // macOS/iOS: pthread_set_qos_class_self_np maps cleanly to
    // BELOW_NORMAL (Utility) and LOWEST (Background). Apple-specific API.
    use std::os::raw::c_int;
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: c_int) -> c_int;
    }
    // Values from <sys/qos.h>:
    //   QOS_CLASS_UTILITY    = 0x11
    //   QOS_CLASS_BACKGROUND = 0x09
    let qos: u32 = match level {
        WorkerPriority::Normal => return Ok(()),
        WorkerPriority::BelowNormal => 0x11,
        WorkerPriority::Lowest => 0x09,
    };
    let rc = unsafe { pthread_set_qos_class_self_np(qos, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(format!("pthread_set_qos_class_self_np rc={}", rc))
    }
}

#[cfg(all(unix, not(any(target_os = "macos", target_os = "ios"))))]
fn set_priority_native(level: WorkerPriority) -> Result<(), String> {
    // Linux / Android: setpriority(PRIO_PROCESS, tid, nice) operates on a
    // single kernel task on Linux (where threads are tasks), targeting this
    // worker only. nice >0 lowers priority.
    let nice: i32 = match level {
        WorkerPriority::Normal => return Ok(()),
        WorkerPriority::BelowNormal => 5,
        WorkerPriority::Lowest => 15,
    };
    let tid = unsafe { libc::syscall(libc::SYS_gettid) } as u32;
    // setpriority returns -1 on error AND on a legitimate priority of -1, so
    // disambiguate via errno per POSIX.
    unsafe { *libc_errno() = 0 };
    let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, nice) };
    let errno = unsafe { *libc_errno() };
    if rc == 0 && errno == 0 {
        Ok(())
    } else {
        Err(format!("setpriority rc={} errno={}", rc, errno))
    }
}

#[cfg(target_os = "android")]
unsafe fn libc_errno() -> *mut i32 {
    // Bionic exports __errno, not __errno_location. Referencing the latter
    // leaves an unresolvable symbol in the cdylib and dlopen fails on device.
    unsafe { libc::__errno() }
}

#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "ios", target_os = "android"))
))]
unsafe fn libc_errno() -> *mut i32 {
    // glibc and musl both export __errno_location.
    unsafe { libc::__errno_location() }
}

#[cfg(not(any(windows, unix)))]
fn set_priority_native(_level: WorkerPriority) -> Result<(), String> {
    Err("unsupported platform".to_string())
}
