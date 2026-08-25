// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use semver::Version;
use serde::Serialize;
use std::cmp::Ordering;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::process::Command;
use zip_extensions::zip_archive_extensions::ZipArchiveExtensions;

const DEFAULT_RELEASE_PLUGINS_BASE: &str =
    "https://github.com/NiYien/gyroflow-plugins/releases/latest/download";

const RESOLVE_SIDECAR_FILES: [&str; 3] = [
    "Gyroflow NiYien Auto Cut Current Clip.lua",
    "Gyroflow NiYien Auto Cut Current Track.lua",
    "gyroflow_autocut_common.inc",
];
const LEGACY_RESOLVE_ENTRY: &str = "Gyroflow NiYien Auto Cut.lua";

#[derive(Debug, Clone, Default, Serialize)]
struct LatestPluginInfo {
    version: String,
    source_ref: String,
    source_tag: String,
    source_base: String,
    source_mode: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct InstalledPluginInfo {
    version: String,
    source_ref: String,
    source_base: String,
}

#[derive(Debug, Clone, Default, Serialize)]
struct PluginStatus {
    typ: String,
    installed_version: String,
    installed_source_ref: String,
    installed_source_base: String,
    latest_version: String,
    latest_source_ref: String,
    latest_source_tag: String,
    latest_source_base: String,
    latest_source_mode: String,
    latest_label: String,
    source_changed: bool,
    update_available: bool,
    is_latest: bool,
}

pub fn get_path(typ: &str) -> &'static str {
    if cfg!(target_os = "windows") {
        if typ == "openfx" {
            return "C:/Program Files/Common Files/OFX/Plugins/GyroflowNiyien.ofx.bundle";
        } else if typ == "adobe" {
            return "C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien-Adobe-windows.aex";
        }
    } else if cfg!(target_os = "macos") {
        if typ == "openfx" {
            return "/Library/OFX/Plugins/GyroflowNiyien.ofx.bundle";
        } else if typ == "adobe" {
            return "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien.plugin";
        }
    }
    ""
}

#[cfg(target_os = "windows")]
fn query_file_version(path: &str) -> Option<String> {
    use windows::{
        Win32::Storage::FileSystem::{
            GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
        },
        core::HSTRING,
    };
    unsafe {
        let hpath = HSTRING::from(path);
        let size = GetFileVersionInfoSizeW(&hpath, None) as usize;
        if size == 0 {
            return None;
        }
        let mut buffer: Vec<u16> = vec![0; size];
        GetFileVersionInfoW(&hpath, None, buffer.len() as u32, buffer.as_mut_ptr() as _)
            .expect("get file version info failed.");
        let pblock = buffer.as_ptr() as _;
        let lang_id = {
            let mut buffer = std::ptr::null_mut();
            let mut len = 0;
            if VerQueryValueW(
                pblock,
                &HSTRING::from("\\VarFileInfo\\Translation"),
                &mut buffer as _,
                &mut len,
            )
            .as_bool()
            {
                let ret = *(buffer as *mut i32);
                ((ret & 0xffff) << 16) + (ret >> 16)
            } else {
                0x040904E4
            }
        };

        unsafe fn file_version_item(
            pblock: *const std::ffi::c_void,
            lang_id: i32,
            version_detail: &str,
        ) -> Option<String> {
            unsafe {
                let mut buffer = std::ptr::null_mut();
                let mut len = 0;
                let ok = VerQueryValueW(
                    pblock,
                    &HSTRING::from(format!(
                        "\\\\StringFileInfo\\\\{lang_id:08x}\\\\{version_detail}"
                    )),
                    &mut buffer,
                    &mut len,
                );
                if ok == false || len == 0 {
                    return None;
                }
                let raw = std::slice::from_raw_parts(buffer.cast(), len as usize);
                match raw.iter().position(|&c| c == 0) {
                    Some(null_pos) => Some(String::from_utf16_lossy(&raw[..null_pos])),
                    None => Some(String::from_utf16_lossy(raw)),
                }
            }
        }

        let v = file_version_item(pblock, lang_id, "ProductVersion")?;
        if v.split('.').count() == 4 && v.ends_with(".0") {
            return Some(v.strip_suffix(".0").unwrap().to_owned());
        }
        Some(v)
    }
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn query_file_version_from_plist(path: &str) -> Option<String> {
    let file = std::fs::read_to_string(path).ok()?;
    let re =
        regex::Regex::new(r#"<key>CFBundleShortVersionString</key>\s*<string>([^<]+)</string>"#)
            .unwrap();
    let cap = re.captures(&file)?;
    let mut v = cap.get(1)?.as_str();
    if v.split('.').count() == 4 && v.ends_with(".0") {
        v = v.strip_suffix(".0").unwrap();
    }
    Some(v.to_owned())
}

fn resolve_sidecar_sources(extracted_root: &Path) -> io::Result<Vec<PathBuf>> {
    let source_dir = extracted_root.join("ResolveScripts");
    let mut sources = Vec::with_capacity(RESOLVE_SIDECAR_FILES.len());
    for name in RESOLVE_SIDECAR_FILES {
        let source = source_dir.join(name);
        if !source.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("OpenFX package is missing ResolveScripts/{name}"),
            ));
        }
        sources.push(source);
    }
    Ok(sources)
}

fn copy_resolve_scripts_to(extracted_root: &Path, destination: &Path) -> io::Result<()> {
    let sources = resolve_sidecar_sources(extracted_root)?;
    std::fs::create_dir_all(destination)?;
    for source in sources {
        let name = source.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Resolve sidecar has no file name",
            )
        })?;
        std::fs::copy(&source, destination.join(name))?;
    }
    match std::fs::remove_file(destination.join(LEGACY_RESOLVE_ENTRY)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(())
}

fn resolve_scripts_dir() -> io::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var_os("APPDATA").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "APPDATA is unavailable for Resolve script installation",
            )
        })?;
        return Ok(PathBuf::from(appdata)
            .join("Blackmagic Design")
            .join("DaVinci Resolve")
            .join("Support")
            .join("Fusion")
            .join("Scripts")
            .join("Utility"));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "HOME is unavailable for Resolve script installation",
            )
        })?;
        return Ok(PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("Blackmagic Design")
            .join("DaVinci Resolve")
            .join("Fusion")
            .join("Scripts")
            .join("Utility"));
    }
    #[allow(unreachable_code)]
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "Resolve scripts are supported only on Windows and macOS",
    ))
}

fn install_resolve_scripts(extracted_root: &Path) -> io::Result<PathBuf> {
    let destination = resolve_scripts_dir()?;
    copy_resolve_scripts_to(extracted_root, &destination)?;
    Ok(destination)
}

fn ensure_install_directory(destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)
}

fn windows_elevated_copy_script(
    extract_path: &str,
    source_arg: &str,
    destination_arg: &str,
) -> String {
    let powershell_literal = |value: &str| format!("'{}'", value.replace('\'', "''"));
    format!(
        "$ErrorActionPreference = 'Stop'; New-Item -ItemType Directory -Force -Path {} | Out-Null; & xcopy.exe {} {} /Y /E /H /I; exit $LASTEXITCODE",
        powershell_literal(extract_path),
        powershell_literal(source_arg),
        powershell_literal(destination_arg)
    )
}

fn copy_files(tempdir: &str, extract_path: &str, typ: &str) -> io::Result<()> {
    ::log::info!(
        "[nle copy_files] start typ={typ:?} tempdir={tempdir:?} extract_path={extract_path:?} extract_path_exists={}",
        Path::new(extract_path).exists()
    );
    let source = if typ == "openfx" {
        Path::new(tempdir).join("GyroflowNiyien.ofx.bundle")
    } else {
        PathBuf::from(tempdir)
    };
    if !source.exists() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Plugin package is missing {}", source.display()),
        ));
    }
    let destination = if typ == "openfx" {
        Path::new(extract_path).join("GyroflowNiyien.ofx.bundle")
    } else {
        PathBuf::from(extract_path)
    };
    let source_arg = source.to_string_lossy().into_owned();
    let destination_arg = destination.to_string_lossy().into_owned();
    let macos_copy_source = if typ == "openfx" {
        source_arg.clone()
    } else {
        format!("{tempdir}/")
    };

    let output = if cfg!(target_os = "windows") {
        let install_directory_ready = match ensure_install_directory(Path::new(extract_path)) {
            Ok(()) => {
                ::log::info!(
                    target: "plugin",
                    "[nle copy_files] install directory ready path={extract_path:?}"
                );
                true
            }
            Err(error) => {
                ::log::warn!(
                    target: "plugin",
                    "[nle copy_files] direct install directory creation failed path={extract_path:?}: {error}; escalating"
                );
                false
            }
        };
        if !install_directory_ready {
            false
        } else {
            let xcopy_out = Command::new("xcopy")
                .args([&source_arg, &destination_arg, "/Y", "/E", "/H", "/I"])
                .output()?;
            let stdout = String::from_utf8_lossy(&xcopy_out.stdout);
            let stderr = String::from_utf8_lossy(&xcopy_out.stderr);
            ::log::info!(
                "[nle copy_files] xcopy(direct) status={:?} success={} stdout={:?} stderr={:?}",
                xcopy_out.status.code(),
                xcopy_out.status.success(),
                stdout.trim(),
                stderr.trim()
            );
            xcopy_out.status.success()
        }
    } else if cfg!(target_os = "macos") {
        if gyroflow_core::filesystem::is_sandboxed() {
            let macosname = match typ {
                "openfx" => "GyroflowNiyien.ofx.bundle",
                "adobe" => "GyroflowNiyien.plugin",
                _ => unreachable!(),
            };
            let src = Path::new(tempdir).join(macosname);
            let target = Path::new(extract_path).join(macosname);
            gyroflow_core::filesystem::start_accessing_url(extract_path, true);
            match std::fs::create_dir_all(&target) {
                Ok(_) => log::info!("Folder created at {target:?}"),
                Err(e) => log::error!("Failed to create folder at {target:?}: {e:?}"),
            }
            let result = fs_extra::copy_items(
                &[src.as_path()],
                &extract_path,
                &fs_extra::dir::CopyOptions::new()
                    .overwrite(true)
                    .copy_inside(true),
            );
            gyroflow_core::filesystem::stop_accessing_url(extract_path, true);
            match result {
                Ok(_) => log::info!("Folder copied from {src:?} to {extract_path:?}"),
                Err(e) => {
                    fn to_io(e: &fs_extra::error::ErrorKind) -> std::io::ErrorKind {
                        match e {
                            fs_extra::error::ErrorKind::NotFound => std::io::ErrorKind::NotFound,
                            fs_extra::error::ErrorKind::PermissionDenied => {
                                std::io::ErrorKind::PermissionDenied
                            }
                            fs_extra::error::ErrorKind::AlreadyExists => {
                                std::io::ErrorKind::AlreadyExists
                            }
                            fs_extra::error::ErrorKind::Interrupted => {
                                std::io::ErrorKind::Interrupted
                            }
                            fs_extra::error::ErrorKind::Other => std::io::ErrorKind::Other,
                            fs_extra::error::ErrorKind::Io(ioe) => ioe.kind(),
                            _ => std::io::ErrorKind::Other,
                        }
                    }
                    return Err(io::Error::new(
                        to_io(&e.kind),
                        format!("Failed to copy files from {src:?} to {extract_path:?}: {e:?}"),
                    ));
                }
            }
            true
        } else {
            Command::new("osascript").args(&["-e", &format!("do shell script \"mkdir -p \\\"{extract_path}\\\" ; cp -Rpf \\\"{macos_copy_source}\\\" \\\"{extract_path}\\\"\"")]).output()?.status.success()
        }
    } else {
        return Err(io::Error::new(io::ErrorKind::Other, "Unsupported OS"));
    };
    // let stderr = String::from_utf8_lossy(&output.stderr);

    if output {
        ::log::info!("[nle copy_files] direct copy succeeded, no UAC needed");
        Ok(())
    } else {
        ::log::warn!(
            "[nle copy_files] direct copy failed, escalating to UAC/sudo retry (typ={typ:?})"
        );
        // Retry with elevated privileges. On Windows this triggers a UAC prompt;
        // on macOS osascript shows an admin auth dialog.
        let status = if cfg!(target_os = "windows") {
            let script = windows_elevated_copy_script(
                extract_path,
                source_arg.as_str(),
                destination_arg.as_str(),
            );
            runas::Command::new("powershell.exe")
                .args(&[
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    script.as_str(),
                ])
                .status()
        } else if cfg!(target_os = "macos") {
            Command::new("osascript").args(&["-e", &format!("do shell script \"install -m 0755 -o $USER -d \\\"{extract_path}\\\" ; cp -Rpf \\\"{macos_copy_source}\\\" \\\"{extract_path}\\\"\" with administrator privileges")]).status()
        } else {
            return Err(io::Error::new(io::ErrorKind::Other, "Unsupported OS"));
        }?;

        ::log::info!(
            "[nle copy_files] elevated retry returned status={:?} success={}",
            status.code(),
            status.success()
        );
        if status.success() {
            Ok(())
        } else {
            // Sentinel consumed by NlePlugins.qml to show a "close your video
            // editor and try again" message instead of the raw Debug error.
            // Keep ErrorKind::PermissionDenied — install() relies on it to
            // preserve the tempdir on failure.
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("PLUGIN_COPY_BLOCKED:{typ}"),
            ))
        }
    }
}

// Map a plugin type + current platform to the V4 short artifact name used by
// the plugin workflow on workflow_dispatch / nightly publishes. The names live
// in `NiYien/gyroflow-plugins/.github/workflows/release.yml` and must stay in
// sync with `_scripts/publish_pan123_release.py PLUGIN_ASSET_NAMES` (filename
// side) + `control_center.config.json::publish_defaults.plugins_artifact_name`
// (CSV of artifact names). On macOS the `-zip` suffix selects the .zip variant
// over the .dmg artifact.
fn nightly_artifact_name_for_plugin(typ: &str) -> &'static str {
    match (typ, cfg!(target_os = "windows")) {
        ("openfx", true) => "GyroflowNiyien-OpenFX-windows",
        ("openfx", false) => "GyroflowNiyien-OpenFX-macos-zip",
        ("adobe", true) => "GyroflowNiyien-Adobe-windows",
        ("adobe", false) => "GyroflowNiyien-Adobe-macos-zip",
        _ => unreachable!(),
    }
}

pub fn install(typ: &str, plugins_base: String) -> io::Result<String> {
    // Single base for all plugin downloads — manifest.plugins_base when present,
    // GitHub releases as offline fallback. Filenames are fixed and match
    // _scripts/publish_pan123_release.py PLUGIN_ASSET_NAMES (release naming,
    // shared across CI / tag-release pipelines — there is no separate nightly
    // naming on the server).
    let normalized_custom_base = plugins_base.trim().trim_end_matches('/').to_owned();
    let base = if normalized_custom_base.is_empty() {
        format!("{DEFAULT_RELEASE_PLUGINS_BASE}/")
    } else {
        format!("{normalized_custom_base}/")
    };
    let is_nightly_base = base.contains("nightly.link");
    let (filename, extract_path) = match typ {
        "openfx" => {
            if cfg!(target_os = "windows") {
                (
                    "GyroflowNiyien-OpenFX-windows.zip",
                    "C:/Program Files/Common Files/OFX/Plugins/",
                )
            } else {
                (
                    "GyroflowNiyien-OpenFX-macos.zip",
                    "/Library/OFX/Plugins/",
                )
            }
        }
        "adobe" => {
            if cfg!(target_os = "windows") {
                (
                    "GyroflowNiyien-Adobe-windows.aex",
                    "C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/",
                )
            } else {
                (
                    "GyroflowNiyien-Adobe-macos.zip",
                    "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/",
                )
            }
        }
        _ => unreachable!(),
    };
    // For nightly-style bases (artifact-mode plugin publish), URLs follow
    //   {base}{artifact_name}.zip
    // where {artifact_name} is the V4 short name from the plugin workflow
    // (no file extension; macos-zip variant has the `-zip` suffix). The
    // wrapper served by nightly.link contains the deliverable file we know
    // by `filename`; the existing zip-branch in this function unwraps one
    // layer, so the only change needed is the URL construction.
    let download_url = if is_nightly_base {
        let artifact_name = nightly_artifact_name_for_plugin(typ);
        format!("{base}{artifact_name}.zip")
    } else {
        format!("{base}{filename}")
    };
    ::log::info!(
        "[nle install] start typ={typ:?} plugins_base={plugins_base:?} effective_base={base:?} download_url={download_url:?} extract_path={extract_path:?}"
    );

    // Best-effort cold-edge prewarm before the (retried) full download. Mirrors
    // a manual `curl -r 0-0` to trigger the 123 direct-link CDN's cold origin
    // fetch so the full GET is more likely to hit a warm edge. No-op when
    // disabled via env; never affects the install result (all errors swallowed).
    crate::network::prewarm_url(&download_url);

    // Surface network / HTTP errors instead of swallowing them. The previous
    // `if let Ok(...)` skipped the entire download block on any ureq failure,
    // leaving detect() to return Ok("") and the UI showing no feedback.
    //
    // Transient failures (5xx / timeouts / connection hiccups) are retried with
    // the dedicated plugin retry profile: CN plugin zips stream from the 123
    // direct-link CDN, whose cold origin fetch sporadically returns a transient
    // 504. A single attempt turned that blip into a hard, user-visible install
    // failure (app/lens/sdk downloads already retry via call_with_retry).
    let retry_label = format!("plugin:{typ}");
    let mut reader = match crate::network::call_with_plugin_retry(&retry_label, || {
        crate::network::get(&download_url).call()
    }) {
        Ok(resp) => {
            ::log::info!(
                "[nle install] HTTP ok status={} content_len_hdr={:?}",
                resp.status(),
                resp.headers()
                    .get("content-length")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_owned())
            );
            resp.into_body().into_reader()
        }
        Err(e) => {
            ::log::error!("[nle install] Failed to download plugin from {download_url}: {e}");
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("Failed to download {download_url}: {e}"),
            ));
        }
    };
    use std::io::Read;
    let mut content = Vec::new();
    reader.read_to_end(&mut content)?;
    ::log::info!(
        "[nle install] body read_to_end bytes={} sniff={:02x?}",
        content.len(),
        &content[..content.len().min(16)]
    );

    let tempdir = tempfile::tempdir()?;
    ::log::info!("[nle install] tempdir created at {:?}", tempdir.path());
    let take_zip_path = download_url.ends_with(".zip");
    ::log::info!(
        "[nle install] branch={}",
        if take_zip_path { "zip" } else { "raw_file" }
    );
    if take_zip_path {
        let mut archive = match zip::ZipArchive::new(Cursor::new(content)) {
            Ok(a) => a,
            Err(e) => {
                ::log::error!("[nle install] zip open failed: {e}");
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("zip open: {e}"),
                ));
            }
        };
        ::log::info!(
            "[nle install] zip outer entries={} first={:?}",
            archive.len(),
            archive.name_for_index(0).map(|s| s.to_owned())
        );
        let mut inner = Vec::new();

        if archive
            .name_for_index(0)
            .map(|x| x.ends_with(".zip"))
            .unwrap_or_default()
        {
            ::log::info!("[nle install] outer zip wraps an inner .zip — unwrapping one layer");
            archive.extract_file_to_memory(0, &mut inner)?;
            let mut archive2 = zip::ZipArchive::new(Cursor::new(inner))?;
            ::log::info!(
                "[nle install] zip inner entries={} first={:?}",
                archive2.len(),
                archive2.name_for_index(0).map(|s| s.to_owned())
            );
            archive2.extract(tempdir.path())?;
        } else {
            archive.extract(tempdir.path())?;
        }
        match std::fs::read_dir(tempdir.path()) {
            Ok(rd) => {
                let names: Vec<String> = rd
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                ::log::info!("[nle install] tempdir contents after extract: {names:?}");
            }
            Err(e) => ::log::warn!("[nle install] read_dir tempdir failed: {e}"),
        }
        if typ == "openfx" {
            resolve_sidecar_sources(tempdir.path())?;
        }
        let result = copy_files(tempdir.path().to_str().unwrap(), &extract_path, typ);
        if let Err(e) = result {
            ::log::error!("[nle install] copy_files (zip branch) returned Err: {e:?}");
            if e.kind() == std::io::ErrorKind::PermissionDenied {
                // Don't delete tempdir if permission was denied
                let _tmpdir = tempdir.keep();
            }
            return Err(e);
        }
        ::log::info!("[nle install] copy_files (zip branch) OK");
        if typ == "openfx" {
            let scripts_dir = install_resolve_scripts(tempdir.path())?;
            ::log::info!(
                "[nle install] Resolve Utility scripts installed at {:?}; restart Resolve to refresh the Scripts menu",
                scripts_dir
            );
        }
    } else {
        let tempfile = tempdir
            .path()
            .join(download_url.split('/').rev().next().unwrap());
        std::fs::write(&tempfile, &content)?;
        ::log::info!(
            "[nle install] wrote raw file to {:?} ({} bytes)",
            tempfile,
            content.len()
        );
        match copy_files(tempdir.path().to_str().unwrap(), &extract_path, typ) {
            Ok(()) => ::log::info!("[nle install] copy_files (raw branch) OK"),
            Err(e) => {
                ::log::error!("[nle install] copy_files (raw branch) returned Err: {e:?}");
                return Err(e);
            }
        }
    }
    let detected = detect(typ)?;
    ::log::info!(
        "[nle install] detect after install: typ={typ:?} -> version={detected:?} extract_path_exists={}",
        Path::new(extract_path).exists()
    );
    remember_installed_plugin(typ, &detected, &latest_plugin_info());
    ::log::info!("[nle install] done typ={typ:?} returning Ok({detected:?})");
    Ok(detected)
}

pub fn is_nle_installed(typ: &str) -> bool {
    use chrono::{Datelike, Utc};

    match typ {
        "openfx" => {
            if cfg!(target_os = "windows") {
                Path::new(&format!(
                    "C:/Users/{}/AppData/Roaming/Blackmagic Design/DaVinci Resolve",
                    whoami::username().unwrap_or_default()
                ))
                .exists()
                    || Path::new("C:/Program Files/Common Files/OFX/Plugins").exists()
                    || Path::new("C:/Program Files/VEGAS").exists()
            } else {
                Path::new("/Applications/DaVinci Resolve/").exists()
                    || Path::new("/Applications/DaVinci Resolve.app/").exists()
                    || Path::new("/Applications/DaVinci Resolve Studio/").exists()
                    || Path::new("/Applications/DaVinci Resolve Studio.app/").exists()
                    || Path::new("/Library/OFX/Plugins").exists()
            }
        }
        "adobe" => {
            if cfg!(target_os = "windows") {
                Path::new("C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/").exists()
            } else {
                (2019..(Utc::now().year() + 1)).any(|y| {
                    Path::new(&format!("/Applications/Adobe Premiere Pro {y}/")).exists()
                        || Path::new(&format!("/Applications/Adobe After Effects {y}/")).exists()
                        || Path::new(&format!("/Applications/Adobe Premiere Pro {y}.app/")).exists()
                        || Path::new(&format!("/Applications/Adobe After Effects {y}.app/"))
                            .exists()
                })
            }
        }
        _ => unreachable!(),
    }
}

pub fn latest_version() -> Option<String> {
    let info = latest_plugin_info();
    (!info.version.is_empty()).then_some(info.version)
}

pub fn status_json(typ: &str) -> io::Result<String> {
    let installed_version = detect(typ)?;
    let installed = load_installed_plugin_info(typ, &installed_version);
    let latest = latest_plugin_info();
    let source_changed = source_changed(&installed, &latest);
    let version_cmp = compare_plugin_versions(&latest.version, &installed.version);
    let update_available = if installed.version.is_empty() {
        true
    } else if source_changed {
        true
    } else {
        version_cmp == Ordering::Greater
    };
    let latest_label = latest_display_label(&latest);
    let payload = PluginStatus {
        typ: typ.to_owned(),
        installed_version: installed.version,
        installed_source_ref: installed.source_ref,
        installed_source_base: installed.source_base,
        latest_version: latest.version,
        latest_source_ref: latest.source_ref,
        latest_source_tag: latest.source_tag,
        latest_source_base: latest.source_base,
        latest_source_mode: latest.source_mode,
        latest_label,
        source_changed,
        update_available,
        is_latest: !update_available && !installed_version.is_empty(),
    };
    serde_json::to_string(&payload).map_err(|err| io::Error::new(io::ErrorKind::Other, err))
}

pub fn detect(typ: &str) -> io::Result<String> {
    let path = get_path(typ);
    ::log::info!(
        "[nle detect] typ={typ:?} get_path={path:?} exists={}",
        if path.is_empty() {
            false
        } else {
            Path::new(path).exists()
        }
    );
    #[cfg(target_os = "windows")]
    {
        if !path.is_empty() && Path::new(path).exists() {
            let probe_path = if typ == "openfx" {
                format!("{path}/Contents/Win64/GyroflowNiyien.ofx")
            } else {
                path.to_owned()
            };
            let probe_exists = Path::new(&probe_path).exists();
            let version = query_file_version(&probe_path);
            ::log::info!(
                "[nle detect] windows probe_path={probe_path:?} probe_exists={probe_exists} query_file_version={version:?}"
            );
            Ok(version.unwrap_or_default())
        } else {
            ::log::info!("[nle detect] windows: path missing or empty, returning empty version");
            Ok(String::new())
        }
    }
    #[cfg(target_os = "macos")]
    {
        if Path::new(path).exists() {
            let plist_path = format!("{path}/Contents/Info.plist");
            let version = query_file_version_from_plist(&plist_path);
            ::log::info!("[nle detect] macos plist_path={plist_path:?} query_result={version:?}");
            Ok(version.unwrap_or_default())
        } else {
            ::log::info!("[nle detect] macos: path missing, returning empty version");
            Ok(String::new())
        }
    }
}

fn latest_plugin_info() -> LatestPluginInfo {
    let source_base = crate::distribution::plugin_source_base()
        .trim()
        .trim_end_matches('/')
        .to_owned();
    if !source_base.is_empty() {
        let source_ref = crate::distribution::plugin_source_ref();
        let source_tag = crate::distribution::plugin_source_tag();
        let source_mode = crate::distribution::plugin_source_mode();
        return LatestPluginInfo {
            version: latest_version_token(&source_ref, &source_tag, &source_base),
            source_ref,
            source_tag,
            source_base,
            source_mode,
        };
    }

    // Manifest doesn't carry source metadata — fall back to GitHub releases as the
    // single source of truth. The historical nightly / actions-runs branch was
    // removed because the deploy side (publish_pan123_release.py) now ships one
    // fixed release naming for both CI runs and tag releases, so the client
    // doesn't need a parallel nightly path.
    let body = match crate::network::get(
        "https://api.github.com/repos/NiYien/gyroflow-plugins/releases",
    )
    .call()
    .ok()
    .and_then(|response| response.into_body().read_to_string().ok())
    {
        Some(body) => body,
        None => return LatestPluginInfo::default(),
    };
    let releases: Vec<serde_json::Value> = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(_) => return LatestPluginInfo::default(),
    };
    for obj in releases {
        let Some(obj) = obj.as_object() else { continue };
        if obj.get("draft").and_then(|x| x.as_bool()) != Some(false)
            || obj.get("prerelease").and_then(|x| x.as_bool()) != Some(false)
        {
            continue;
        }
        let Some(tag_name) = obj.get("tag_name").and_then(|x| x.as_str()) else {
            continue;
        };
        let source_ref = tag_name.trim().to_owned();
        return LatestPluginInfo {
            version: source_ref.trim_start_matches('v').to_owned(),
            source_ref: source_ref.clone(),
            source_tag: source_ref,
            source_base: DEFAULT_RELEASE_PLUGINS_BASE.to_owned(),
            source_mode: "release".to_owned(),
        };
    }
    LatestPluginInfo::default()
}

fn latest_version_token(source_ref: &str, source_tag: &str, source_base: &str) -> String {
    let trimmed_ref = source_ref.trim();
    if !trimmed_ref.is_empty() {
        return trimmed_ref.to_owned();
    }
    let trimmed_tag = source_tag.trim();
    if !trimmed_tag.is_empty() {
        return trimmed_tag.to_owned();
    }
    if !source_base.trim().is_empty() {
        return "manifest".to_owned();
    }
    String::new()
}

fn latest_display_label(info: &LatestPluginInfo) -> String {
    if !info.source_tag.trim().is_empty() {
        return info.source_tag.trim().to_owned();
    }
    if !info.source_ref.trim().is_empty() {
        return info.source_ref.trim().to_owned();
    }
    info.version.trim().to_owned()
}

fn load_installed_plugin_info(typ: &str, installed_version: &str) -> InstalledPluginInfo {
    InstalledPluginInfo {
        version: installed_version.trim().to_owned(),
        source_ref: gyroflow_core::settings::get_str(&installed_source_ref_key(typ), ""),
        source_base: gyroflow_core::settings::get_str(&installed_source_base_key(typ), ""),
    }
}

fn remember_installed_plugin(typ: &str, installed_version: &str, latest: &LatestPluginInfo) {
    gyroflow_core::settings::set(
        &installed_source_ref_key(typ),
        latest.source_ref.trim().to_owned().into(),
    );
    gyroflow_core::settings::set(
        &installed_source_base_key(typ),
        latest.source_base.trim().to_owned().into(),
    );
    gyroflow_core::settings::set(
        &installed_version_key(typ),
        installed_version.trim().to_owned().into(),
    );
}

fn installed_source_ref_key(typ: &str) -> String {
    format!("nlePluginInstalledSourceRef_{typ}")
}

fn installed_source_base_key(typ: &str) -> String {
    format!("nlePluginInstalledSourceBase_{typ}")
}

fn installed_version_key(typ: &str) -> String {
    format!("nlePluginInstalledVersion_{typ}")
}

fn normalize_source_base(value: &str) -> String {
    value.trim().trim_end_matches('/').to_owned()
}

fn source_changed(installed: &InstalledPluginInfo, latest: &LatestPluginInfo) -> bool {
    let latest_ref = latest.source_ref.trim();
    if !latest_ref.is_empty() {
        let installed_ref = installed.source_ref.trim();
        return installed_ref.is_empty() || installed_ref != latest_ref;
    }

    let latest_base = normalize_source_base(&latest.source_base);
    if latest_base.is_empty() {
        return false;
    }
    let installed_base = normalize_source_base(&installed.source_base);
    installed_base != latest_base
}

fn compare_plugin_versions(latest: &str, installed: &str) -> Ordering {
    let latest = latest.trim();
    let installed = installed.trim();
    if latest.is_empty() || installed.is_empty() {
        return Ordering::Equal;
    }
    if latest.eq_ignore_ascii_case(installed) {
        return Ordering::Equal;
    }
    if let (Ok(latest), Ok(installed)) = (latest.parse::<u64>(), installed.parse::<u64>()) {
        return latest.cmp(&installed);
    }
    if let (Some(latest), Some(installed)) = (
        parse_numeric_dotted_version(latest),
        parse_numeric_dotted_version(installed),
    ) {
        return latest.cmp(&installed);
    }

    let latest_semver = parse_semver(latest);
    let installed_semver = parse_semver(installed);
    match (latest_semver, installed_semver) {
        (Some(latest), Some(installed)) => latest.cmp(&installed),
        _ => Ordering::Equal,
    }
}

fn parse_numeric_dotted_version(value: &str) -> Option<Vec<u64>> {
    let trimmed = value.trim().trim_start_matches('v');
    if trimmed.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    for part in trimmed.split('.') {
        if part.is_empty() || !part.chars().all(|ch| ch.is_ascii_digit()) {
            return None;
        }
        parts.push(part.parse::<u64>().ok()?);
    }
    while parts.len() > 1 && matches!(parts.last(), Some(&0)) {
        parts.pop();
    }
    Some(parts)
}

fn parse_semver(value: &str) -> Option<Version> {
    let trimmed = value.trim().trim_start_matches('v');
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(version) = Version::parse(trimmed) {
        return Some(version);
    }
    if trimmed.matches('.').count() == 3 && trimmed.ends_with(".0") {
        return Version::parse(trimmed.trim_end_matches(".0")).ok();
    }
    Version::parse(&format!("{trimmed}.0")).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_sidecar_file(root: &Path, name: &str, contents: &str) {
        let source = root.join("ResolveScripts");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join(name), contents).unwrap();
    }

    #[test]
    fn install_directory_is_created_when_missing() {
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("Common Files").join("OFX").join("Plugins");
        assert!(!destination.exists());

        ensure_install_directory(&destination).unwrap();

        assert!(destination.is_dir());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_elevated_copy_script_creates_destination_and_copies_payload() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source payload");
        let source_file = source.join("Contents").join("Win64").join("plugin.bin");
        std::fs::create_dir_all(source_file.parent().unwrap()).unwrap();
        std::fs::write(&source_file, b"plugin payload").unwrap();

        let install_root = root
            .path()
            .join("Program Files")
            .join("Common Files")
            .join("OFX")
            .join("Plugins");
        let destination = install_root.join("GyroflowNiyien.ofx.bundle");
        let script = windows_elevated_copy_script(
            install_root.to_str().unwrap(),
            source.to_str().unwrap(),
            destination.to_str().unwrap(),
        );

        let status = Command::new(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                script.as_str(),
            ])
            .status()
            .unwrap();

        assert!(status.success());
        assert_eq!(
            std::fs::read(
                destination
                    .join("Contents")
                    .join("Win64")
                    .join("plugin.bin")
            )
            .unwrap(),
            b"plugin payload"
        );
    }

    #[test]
    fn openfx_install_success_is_silent_in_qml() {
        let qml = include_str!("ui/menu/NlePlugins.qml");
        let handler_start = qml
            .find("function onNle_plugins_result(command: string, result: string)")
            .expect("NLE plugin result handler exists");
        let handler_remaining = &qml[handler_start..];
        let handler_end = handler_remaining
            .find("\n    Row {")
            .expect("NLE plugin result handler ends before status rows");
        let handler = &handler_remaining[..handler_end];

        assert!(
            !handler.contains("Modal.Info"),
            "successful plugin installation must not show a confirmation modal"
        );
        assert!(
            !qml.contains("pendingInstallType"),
            "success-modal-only state must be removed with the modal"
        );
    }

    #[test]
    fn resolve_sidecar_copy_requires_all_files() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        write_sidecar_file(
            source.path(),
            "Gyroflow NiYien Auto Cut Current Clip.lua",
            "clip",
        );
        write_sidecar_file(
            source.path(),
            "Gyroflow NiYien Auto Cut Current Track.lua",
            "track",
        );

        let err = copy_resolve_scripts_to(source.path(), destination.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!destination
            .path()
            .join("gyroflow_autocut_common.inc")
            .exists());
    }

    #[test]
    fn resolve_sidecar_copy_installs_exact_contract() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        for (name, contents) in [
            ("Gyroflow NiYien Auto Cut Current Clip.lua", "clip"),
            ("Gyroflow NiYien Auto Cut Current Track.lua", "track"),
            ("gyroflow_autocut_common.inc", "common"),
        ] {
            write_sidecar_file(source.path(), name, contents);
        }
        std::fs::write(
            source.path().join("ResolveScripts").join("unexpected.lua"),
            "unexpected",
        )
        .unwrap();

        copy_resolve_scripts_to(source.path(), destination.path()).unwrap();

        assert_eq!(
            std::fs::read_to_string(
                destination
                    .path()
                    .join("Gyroflow NiYien Auto Cut Current Clip.lua")
            )
            .unwrap(),
            "clip"
        );
        assert_eq!(
            std::fs::read_to_string(destination.path().join("gyroflow_autocut_common.inc"))
                .unwrap(),
            "common"
        );
        assert!(!destination.path().join("unexpected.lua").exists());
    }

    #[test]
    fn resolve_sidecar_copy_removes_only_legacy_entry() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        for (name, contents) in [
            ("Gyroflow NiYien Auto Cut Current Clip.lua", "clip"),
            ("Gyroflow NiYien Auto Cut Current Track.lua", "track"),
            ("gyroflow_autocut_common.inc", "common"),
        ] {
            write_sidecar_file(source.path(), name, contents);
        }
        std::fs::write(
            destination.path().join("Gyroflow NiYien Auto Cut.lua"),
            "legacy",
        )
        .unwrap();
        std::fs::write(destination.path().join("Other Utility.lua"), "unrelated").unwrap();

        copy_resolve_scripts_to(source.path(), destination.path()).unwrap();

        assert!(!destination
            .path()
            .join("Gyroflow NiYien Auto Cut.lua")
            .exists());
        assert!(destination.path().join("Other Utility.lua").exists());
    }

    #[test]
    fn compare_semver_versions() {
        assert_eq!(compare_plugin_versions("1.6.3", "1.6.2"), Ordering::Greater);
        assert_eq!(
            compare_plugin_versions("v1.6.3", "1.6.3.0"),
            Ordering::Equal
        );
        assert_eq!(compare_plugin_versions("1.6.2", "1.6.3"), Ordering::Less);
    }

    #[test]
    fn compare_numeric_run_versions() {
        assert_eq!(compare_plugin_versions("248", "247"), Ordering::Greater);
        assert_eq!(compare_plugin_versions("247", "247"), Ordering::Equal);
    }

    #[test]
    fn compare_four_part_versions() {
        assert_eq!(
            compare_plugin_versions("2.1.1.107", "2.1.1.106"),
            Ordering::Greater
        );
        assert_eq!(compare_plugin_versions("2.1.1.0", "2.1.1"), Ordering::Equal);
    }

    #[test]
    fn source_change_forces_update() {
        let installed = InstalledPluginInfo {
            version: "9.9.9".to_owned(),
            source_ref: String::new(),
            source_base: String::new(),
        };
        let latest = LatestPluginInfo {
            version: "1.0.0".to_owned(),
            source_ref: "v1.0.0".to_owned(),
            source_tag: "v1.0.0".to_owned(),
            source_base: "https://github.com/NiYien/gyroflow-plugins/releases/latest/download"
                .to_owned(),
            source_mode: "release".to_owned(),
        };
        assert!(source_changed(&installed, &latest));
    }

    #[test]
    fn same_source_and_ref_is_not_source_change() {
        let installed = InstalledPluginInfo {
            version: "1.0.0".to_owned(),
            source_ref: "v1.0.0".to_owned(),
            source_base: "https://github.com/NiYien/gyroflow-plugins/releases/latest/download/"
                .to_owned(),
        };
        let latest = LatestPluginInfo {
            version: "1.0.0".to_owned(),
            source_ref: "v1.0.0".to_owned(),
            source_tag: "v1.0.0".to_owned(),
            source_base: "https://github.com/NiYien/gyroflow-plugins/releases/latest/download"
                .to_owned(),
            source_mode: "release".to_owned(),
        };
        assert!(!source_changed(&installed, &latest));
    }

    #[test]
    fn same_source_ref_ignores_mirror_base_change() {
        let installed = InstalledPluginInfo {
            version: "2.1.2.14".to_owned(),
            source_ref: "actions-run-25153325566".to_owned(),
            source_base: "https://www.niyien.com/api/download/content/plugin-49595a258ec8"
                .to_owned(),
        };
        let latest = LatestPluginInfo {
            version: "actions-run-25153325566".to_owned(),
            source_ref: "actions-run-25153325566".to_owned(),
            source_tag: "GyroflowNiyien-OpenFX-macos".to_owned(),
            source_base: "https://nightly.link/NiYien/gyroflow-plugins/actions/runs/25153325566"
                .to_owned(),
            source_mode: "artifact".to_owned(),
        };
        assert!(!source_changed(&installed, &latest));
    }

    #[test]
    fn different_source_ref_forces_update_across_mirrors() {
        let installed = InstalledPluginInfo {
            version: "2.1.2.14".to_owned(),
            source_ref: "actions-run-25116018536".to_owned(),
            source_base: "https://www.niyien.com/api/download/content/plugin-old".to_owned(),
        };
        let latest = LatestPluginInfo {
            version: "actions-run-25153325566".to_owned(),
            source_ref: "actions-run-25153325566".to_owned(),
            source_tag: "GyroflowNiyien-OpenFX-macos".to_owned(),
            source_base: "https://nightly.link/NiYien/gyroflow-plugins/actions/runs/25153325566"
                .to_owned(),
            source_mode: "artifact".to_owned(),
        };
        assert!(source_changed(&installed, &latest));
    }

    #[test]
    fn source_base_change_is_fallback_when_latest_ref_is_empty() {
        let installed = InstalledPluginInfo {
            version: "1.0.0".to_owned(),
            source_ref: String::new(),
            source_base: "https://mirror-a.example/plugins".to_owned(),
        };
        let latest = LatestPluginInfo {
            version: "manifest".to_owned(),
            source_ref: String::new(),
            source_tag: String::new(),
            source_base: "https://mirror-b.example/plugins".to_owned(),
            source_mode: "release".to_owned(),
        };
        assert!(source_changed(&installed, &latest));
    }
}
