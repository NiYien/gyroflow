// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use app_dirs2::{AppDataType, AppInfo};
use parking_lot::RwLock;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering::SeqCst},
};

#[cfg(test)]
fn test_settings_file_override() -> &'static std::sync::Mutex<Option<PathBuf>> {
    static PATH: std::sync::OnceLock<std::sync::Mutex<Option<PathBuf>>> =
        std::sync::OnceLock::new();
    PATH.get_or_init(|| std::sync::Mutex::new(None))
}

/// Project fields that carry a value backed by a persisted UI setting, paired
/// with that setting's key.
///
/// A `.gyroflow` records whatever the app's global stabilization settings were
/// when it was written. Importing one therefore writes those globals back — a
/// path that bypasses the settings layer entirely, which is how a project
/// holding `"method": "Fixed camera"` kept restoring the black-preview state
/// even once the stored `smoothingMethod` was no longer honoured.
///
/// The keys here must all be denied by the app's settings policy; a test in the
/// main crate asserts exactly that, so gating a field whose key is actually
/// user-facing fails the build rather than silently dropping the user's choice.
pub const GATED_PROJECT_STABILIZATION_FIELDS: &[(&str, &str)] = &[
    ("method", "smoothingMethod"),
    ("adaptive_zoom_window", "adaptiveZoom"),
    ("adaptive_zoom_method", "zoomingMethod"),
    ("max_zoom", "maxZoom"),
    // Upstream's key really is spelled this way in the project format.
    ("max_zoom_terations", "maxZoomIterations"),
    ("lens_correction_amount", "correctionAmount"),
    ("use_gravity_vectors", "useGravityVectors"),
    ("horizon_lock_integration_method", "hlIntegrationMethod"),
    ("video_speed_affects_smoothing", "videoSpeedAffectsSmoothing"),
    ("video_speed_affects_zooming", "videoSpeedAffectsZooming"),
    (
        "video_speed_affects_zooming_limit",
        "videoSpeedAffectsZoomingLimit",
    ),
];

static PROJECT_IMPORT_GATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether project import should ignore the fields above.
///
/// Defaults to **off** and is switched on only by the desktop app. The NLE
/// plugins and the CLI share this crate and must keep honouring a project's
/// stabilization settings verbatim — that is the entire point of handing them a
/// project — so the gate can never be something they opt out of by accident.
pub fn project_import_gate() -> bool {
    PROJECT_IMPORT_GATE.load(SeqCst)
}

pub fn set_project_import_gate(enabled: bool) {
    PROJECT_IMPORT_GATE.store(enabled, SeqCst);
}

pub fn data_dir() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

    PATH.get_or_init(|| {
        if let Ok(custom_dir) = std::env::var("GYROFLOW_DATA_DIR") {
            if !custom_dir.trim().is_empty() {
                let path = PathBuf::from(custom_dir);
                let _ = std::fs::create_dir_all(&path);
                let _ = std::fs::create_dir_all(path.join("lens_profiles"));
                return path;
            }
        }

        let brand = &crate::distribution::config().brand;
        let mut path = app_dirs2::get_app_dir(
            AppDataType::UserData,
            &AppInfo {
                name: &brand.application_name,
                author: &brand.organization_name,
            },
            "",
        )
        .unwrap();
        if path.file_name().unwrap() == path.parent().unwrap().file_name().unwrap() {
            path = path.parent().unwrap().to_path_buf();
        }

        #[cfg(target_os = "windows")]
        unsafe {
            use std::os::windows::ffi::OsStringExt;
            use windows::Win32::UI::Shell::*;
            let mut len = 0;
            let _ =
                windows::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName(&mut len, None);
            if len > 0 {
                // It's a Microsoft Store package
                if let Ok(raw_path) =
                    SHGetKnownFolderPath(&FOLDERID_Profile, KNOWN_FOLDER_FLAG::default(), None)
                {
                    let s = std::ffi::OsString::from_wide(raw_path.as_wide());
                    path = PathBuf::from(s);
                    path.push("AppData");
                    path.push("Local");
                    path.push(&brand.application_name);
                    windows::Win32::System::Com::CoTaskMemFree(Some(raw_path.as_ptr() as *mut _));
                }
            }
        }

        #[cfg(target_os = "macos")]
        unsafe {
            use std::ffi::{CStr, OsString};
            use std::mem::MaybeUninit;
            use std::os::unix::ffi::OsStringExt;
            let init_size = match libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) {
                -1 => 1024,
                n => n as usize,
            };
            let mut buf = Vec::with_capacity(init_size);
            let mut pwd: MaybeUninit<libc::passwd> = MaybeUninit::uninit();
            let mut pwdp = std::ptr::null_mut();
            match libc::getpwuid_r(
                libc::geteuid(),
                pwd.as_mut_ptr(),
                buf.as_mut_ptr(),
                buf.capacity(),
                &mut pwdp,
            ) {
                0 if !pwdp.is_null() => {
                    let pwd = pwd.assume_init();
                    let bytes = CStr::from_ptr(pwd.pw_dir).to_bytes().to_vec();
                    let pw_dir = OsString::from_vec(bytes);
                    path = PathBuf::from(pw_dir);
                    path.push("Library");
                    path.push("Application Support");
                    path.push(&brand.application_name);
                }
                _ => {}
            }
        }
        let _ = std::fs::create_dir_all(&path);
        if let Err(e) = std::fs::create_dir_all(&path.join("lens_profiles")) {
            ::log::error!(
                "Failed to create lens profiles directory at {:?}: {e:?}",
                path.join("lens_profiles")
            );
        }
        path
    })
    .clone()
}

pub fn get_all() -> HashMap<String, serde_json::Value> {
    map().read().clone()
}

pub fn get(key: &str, default: serde_json::Value) -> serde_json::Value {
    map().read().get(key).unwrap_or(&default).clone()
}

pub fn set(key: &str, value: serde_json::Value) {
    map().write().insert(key.to_string(), value);
    spawn_store_thread();
}

pub fn contains(key: &str) -> bool {
    map().read().contains_key(key)
}

pub fn clear() {
    map().write().clear();
    store();
}
pub fn flush() {
    store();
}

#[cfg(test)]
pub fn with_test_settings_file<R>(file: PathBuf, f: impl FnOnce() -> R) -> R {
    let previous_map = map().read().clone();
    *map().write() = HashMap::new();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    *test_settings_file_override().lock().unwrap() = Some(file);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    std::thread::sleep(std::time::Duration::from_millis(1300));

    *test_settings_file_override().lock().unwrap() = None;
    *map().write() = previous_map;

    match result {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

pub fn try_get(key: &str) -> Option<serde_json::Value> {
    map().read().get(key).map(Clone::clone)
}
pub fn get_u64(key: &str, default: u64) -> u64 {
    map()
        .read()
        .get(key)
        .and_then(|x| x.as_u64())
        .unwrap_or(default)
}
pub fn get_f64(key: &str, default: f64) -> f64 {
    map()
        .read()
        .get(key)
        .and_then(|x| x.as_f64())
        .unwrap_or(default)
}
pub fn get_bool(key: &str, default: bool) -> bool {
    map()
        .read()
        .get(key)
        .and_then(|x| x.as_bool())
        .unwrap_or(default)
}
pub fn get_str(key: &str, default: &str) -> String {
    map()
        .read()
        .get(key)
        .and_then(|x| x.as_str())
        .map(|x| x.to_owned())
        .unwrap_or_else(|| default.to_owned())
}

fn map() -> Arc<RwLock<HashMap<String, serde_json::Value>>> {
    static MAP: std::sync::OnceLock<Arc<RwLock<HashMap<String, serde_json::Value>>>> =
        std::sync::OnceLock::new();
    MAP.get_or_init(|| {
        let mut map = HashMap::new();
        let file = data_dir().join("settings.json");
        log::info!("Settings file path: {}", file.display());

        if let Ok(v) = serde_json::from_str::<HashMap<String, serde_json::Value>>(
            &std::fs::read_to_string(file).unwrap_or_default(),
        ) {
            map = v;
        }

        Arc::new(RwLock::new(map))
    })
    .clone()
}

fn timestamp() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize
}
fn spawn_store_thread() {
    static STORE_TIMEOUT: AtomicUsize = AtomicUsize::new(0);

    let is_thread_running = STORE_TIMEOUT.load(SeqCst) != 0;
    STORE_TIMEOUT.store(timestamp() + 1, SeqCst); // 1 second

    if is_thread_running {
        return;
    }
    std::thread::spawn(|| {
        while STORE_TIMEOUT.load(SeqCst) > timestamp() {
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        store();
        STORE_TIMEOUT.store(0, SeqCst);
    });
}

fn store() {
    let file = settings_file();
    let map = map().read().clone();
    let json = serde_json::to_string_pretty(&map).unwrap();
    if let Err(e) = std::fs::write(&file, json) {
        log::error!("Failed to write the settings file {file:?}: {e:?}");
    } else {
        log::info!("Settings saved to {file:?}");
    }
}

fn settings_file() -> PathBuf {
    #[cfg(test)]
    if let Some(file) = test_settings_file_override().lock().unwrap().clone() {
        return file;
    }

    data_dir().join("settings.json")
}

/// Guard against re-introducing any read of the upstream Gyroflow data directory.
///
/// A first-launch migration used to copy the upstream `settings.json` wholesale,
/// which shipped dirty keys into this app twice: portrait `preserved*` output
/// settings, and a smoothing method of "Fixed camera" that collapsed the adaptive
/// zoom to a black frame. That migration is gone, but nothing stopped an upstream
/// merge from bringing it back, and a commented-out app-dir lookup sat in
/// `external_sdk` as a one-uncomment-away landmine. Hence: scan every `.rs` file in
/// the repository, comments included.
///
/// Note the forbidden identifiers are deliberately not spelled out anywhere in this
/// file — including in prose. The scan covers its own source, so naming them here
/// would make the guard fail on itself.
#[cfg(test)]
mod upstream_data_dir_guard {
    use std::path::{Path, PathBuf};

    /// The forbidden literals are assembled from fragments on purpose. This file is
    /// itself part of the scan, so spelling them out verbatim would make the guard
    /// trip over its own source and force an exclusion list — and an exclusion list
    /// would have to exclude `settings.rs`, the very file that needs guarding most.
    const BRAND: &str = concat!("Gyro", "flow");

    fn repo_root() -> PathBuf {
        // gyroflow-core's manifest lives at <repo>/src/core.
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
    }

    /// Whitespace is stripped before matching so a needle written as one contiguous
    /// string matches regardless of how the offending code is formatted or wrapped.
    fn pack(source: &str) -> String {
        source.chars().filter(|c| !c.is_whitespace()).collect()
    }

    fn forbidden_needles() -> Vec<(String, &'static str)> {
        vec![
            (
                format!("name:\"{BRAND}\",author:\"{BRAND}\""),
                "AppInfo pointing at the upstream data directory",
            ),
            (
                format!("author:\"{BRAND}\",name:\"{BRAND}\""),
                "AppInfo pointing at the upstream data directory",
            ),
            (
                format!("\"{BRAND}\",\"{BRAND}\")"),
                "ProjectDirs/app-dir call ending in the upstream qualifier pair",
            ),
            (
                concat!("legacy_data", "_dir").to_string(),
                "legacy data directory resolver",
            ),
            (
                concat!("migrate_from", "_legacy_dir").to_string(),
                "legacy data directory migration",
            ),
        ]
    }

    /// Pure detection over already-packed source. Split out so the guard itself is
    /// testable with synthetic input instead of relying on the repository contents.
    fn violations_in(packed: &str) -> Vec<&'static str> {
        forbidden_needles()
            .into_iter()
            .filter(|(needle, _)| packed.contains(needle.as_str()))
            .map(|(_, reason)| reason)
            .collect()
    }

    fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                // Build artifacts contain vendored upstream sources; never scan them.
                if path.file_name().map(|n| n == "target").unwrap_or(false) {
                    continue;
                }
                collect_rs_files(&path, out);
            } else if path.extension().map(|e| e == "rs").unwrap_or(false) {
                out.push(path);
            }
        }
    }

    #[test]
    fn no_source_file_reads_the_upstream_data_dir() {
        let src_root = repo_root().join("src");
        let mut files = Vec::new();
        collect_rs_files(&src_root, &mut files);
        assert!(
            files.len() > 10,
            "guard found only {} .rs files under {src_root:?} — the scan root is wrong",
            files.len()
        );

        let mut offenders = Vec::new();
        for file in files {
            let Ok(source) = std::fs::read_to_string(&file) else {
                continue;
            };
            for reason in violations_in(&pack(&source)) {
                let shown = file
                    .strip_prefix(&src_root)
                    .unwrap_or(&file)
                    .display()
                    .to_string();
                offenders.push(format!("  src/{shown}: {reason}"));
            }
        }

        assert!(
            offenders.is_empty(),
            "source reads the upstream data directory:\n{}\n\n\
             Reading anything out of the upstream app directory is forbidden — it \
             imported dirty settings into this app twice. If an upstream merge \
             brought this back, drop it rather than adapting it.",
            offenders.join("\n")
        );
    }

    #[test]
    fn guard_detects_every_forbidden_form() {
        let samples = [
            format!("&AppInfo {{ name: \"{BRAND}\", author: \"{BRAND}\" }},"),
            format!("&AppInfo {{ author: \"{BRAND}\", name: \"{BRAND}\" }},"),
            format!("ProjectDirs::from(\"xyz\", \"{BRAND}\", \"{BRAND}\")"),
            concat!("fn legacy_data", "_dir() -> PathBuf {").to_string(),
            concat!("migrate_from", "_legacy_dir(&path);").to_string(),
        ];
        for sample in samples {
            assert!(
                !violations_in(&pack(&sample)).is_empty(),
                "guard failed to flag: {sample}"
            );
        }
    }

    #[test]
    fn guard_flags_commented_out_forms_too() {
        // This is the shape that actually sat in the tree: a disabled app-dir
        // lookup, one uncomment away from restoring the cross-app settings leak.
        // Stripping whitespace before matching makes comment markers irrelevant,
        // but the guarantee is worth pinning rather than inferring.
        let line_comment = format!("// if let Some(d) = ProjectDirs::from(\"xyz\", \"{BRAND}\", \"{BRAND}\") {{");
        let block_comment = format!(
            "/*\n    let info = &AppInfo {{\n        name: \"{BRAND}\",\n        author: \"{BRAND}\",\n    }};\n*/"
        );
        let doc_comment = concat!("/// See migrate_from", "_legacy_dir for the old behaviour.").to_string();
        for sample in [line_comment, block_comment, doc_comment] {
            assert!(
                !violations_in(&pack(&sample)).is_empty(),
                "guard failed to flag a commented-out form: {sample}"
            );
        }
    }

    #[test]
    fn guard_allows_the_intentional_brand_mentions() {
        // Every one of these exists in the repository today and is deliberate:
        // none of them is a filesystem path into the upstream app directory.
        let samples = [
            format!("keep_awake::inhibit_system(\"{BRAND}\", \"Rendering video\");"),
            format!("cstr::cstr!(\"{BRAND}\"), 1, 0, cstr::cstr!(\"TimelineGyroChart\"),"),
            format!("QIcon::setThemeName(QStringLiteral(\"{BRAND}\"));"),
            format!("doc = \"{BRAND}\""),
            // NiYien's own predecessor tool, read-only, intentionally still read.
            "fn legacy_tool_telemetry_ini() -> Option<std::path::PathBuf> {".to_string(),
            "let path = legacy_tool_anon_id()?;".to_string(),
            // The supported brand-driven resolution must keep working.
            "&AppInfo { name: &brand.application_name, author: &brand.organization_name },"
                .to_string(),
        ];
        for sample in samples {
            assert!(
                violations_in(&pack(&sample)).is_empty(),
                "guard false-positived on an intentional mention: {sample}"
            );
        }
    }
}
