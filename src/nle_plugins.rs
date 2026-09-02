// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
const LINUX_OPENFX_INSTALL_ROOT: &str = "/usr/OFX/Plugins/";
const LINUX_PLUGIN_MANUAL_INSTALL_REQUIRED: &str = "PLUGIN_MANUAL_INSTALL_REQUIRED:";
const FINALCUT_ASSET_NAME: &str = "GyroflowNiyien-FinalCut-macos.zip";
const FINALCUT_ARTIFACT_NAME: &str = "GyroflowNiyien-FinalCut-macos";
const FINALCUT_APP_PATH: &str = "/Applications/GyroflowNiYien Final Cut.app";
const FINALCUT_APP_NAME: &str = "GyroflowNiYien Final Cut.app";
const FINALCUT_XPC_NAME: &str = "GyroflowNiYienFinalCutEffect.pluginkit";
const FINALCUT_APP_BUNDLE_ID: &str = "com.niyien.gyroflow.finalcut";
const FINALCUT_XPC_BUNDLE_ID: &str = "com.niyien.gyroflow.finalcut.effect";
const FINALCUT_EFFECT_UUID: &str = "ABAD71F5-23F5-46F6-AB08-C11603168AA4";
const FINALCUT_TEMPLATE_MARKER: &str = ".gyroflow-install.json";
const FINALCUT_TEMPLATE_NAME: &str = "Gyroflow NiYien.moef";
const FINAL_CUT_HOST_BUNDLE_IDS: [&str; 2] = ["com.apple.FinalCutApp", "com.apple.FinalCut"];
const FINAL_CUT_HOST_FALLBACK_PATHS: [&str; 2] = [
    "/Applications/Final Cut Pro Creator Studio.app",
    "/Applications/Final Cut Pro.app",
];

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PluginPlatform {
    Windows,
    Macos,
    Linux,
}

fn current_plugin_platform() -> PluginPlatform {
    #[cfg(target_os = "windows")]
    {
        PluginPlatform::Windows
    }
    #[cfg(target_os = "macos")]
    {
        PluginPlatform::Macos
    }
    #[cfg(target_os = "linux")]
    {
        PluginPlatform::Linux
    }
}

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
    state: String,
    repair_required: bool,
    detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum FinalCutInstallState {
    NotInstalled,
    AppInstalledTemplateMissing,
    BrokenOrUntrusted,
    Installed,
    UpdateAvailable,
}

#[derive(Debug, Clone)]
struct FinalCutDetection {
    state: FinalCutInstallState,
    version: String,
    detail: String,
}

pub fn get_path(typ: &str) -> &'static str {
    get_path_for_platform(typ, current_plugin_platform())
}

fn plugin_available_on_platform(typ: &str, platform: PluginPlatform) -> bool {
    match platform {
        PluginPlatform::Linux => typ == "openfx",
        PluginPlatform::Windows => typ == "openfx" || typ == "adobe",
        PluginPlatform::Macos => typ == "openfx" || typ == "adobe" || typ == "finalcut",
    }
}

fn get_path_for_platform(typ: &str, platform: PluginPlatform) -> &'static str {
    match (typ, platform) {
        ("openfx", PluginPlatform::Windows) => {
            "C:/Program Files/Common Files/OFX/Plugins/GyroflowNiyien.ofx.bundle"
        }
        ("adobe", PluginPlatform::Windows) => {
            "C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien-Adobe-windows.aex"
        }
        ("openfx", PluginPlatform::Macos) => "/Library/OFX/Plugins/GyroflowNiyien.ofx.bundle",
        ("adobe", PluginPlatform::Macos) => {
            "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien.plugin"
        }
        ("finalcut", PluginPlatform::Macos) => FINALCUT_APP_PATH,
        ("openfx", PluginPlatform::Linux) => "/usr/OFX/Plugins/GyroflowNiyien.ofx.bundle",
        _ => "",
    }
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

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let appdata = std::env::var_os("APPDATA").map(PathBuf::from);
    resolve_scripts_dir_for_platform(
        current_plugin_platform(),
        home.as_deref(),
        appdata.as_deref(),
    )
}

fn resolve_scripts_dir_for_platform(
    platform: PluginPlatform,
    home: Option<&Path>,
    appdata: Option<&Path>,
) -> io::Result<PathBuf> {
    match platform {
        PluginPlatform::Windows => {
            let appdata = appdata.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "APPDATA is unavailable for Resolve script installation",
                )
            })?;
            Ok(appdata
                .join("Blackmagic Design")
                .join("DaVinci Resolve")
                .join("Support")
                .join("Fusion")
                .join("Scripts")
                .join("Utility"))
        }
        PluginPlatform::Macos => {
            let home = home.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "HOME is unavailable for Resolve script installation",
                )
            })?;
            Ok(home
                .join("Library")
                .join("Application Support")
                .join("Blackmagic Design")
                .join("DaVinci Resolve")
                .join("Fusion")
                .join("Scripts")
                .join("Utility"))
        }
        PluginPlatform::Linux => {
            let home = home.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "HOME is unavailable for Resolve script installation",
                )
            })?;
            Ok(home
                .join(".local")
                .join("share")
                .join("DaVinciResolve")
                .join("Fusion")
                .join("Scripts")
                .join("Utility"))
        }
    }
}

fn install_resolve_scripts(extracted_root: &Path) -> io::Result<PathBuf> {
    let destination = resolve_scripts_dir()?;
    copy_resolve_scripts_to(extracted_root, &destination)?;
    Ok(destination)
}

fn ensure_install_directory(destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)
}

fn linux_openfx_binary(bundle: &Path) -> PathBuf {
    bundle
        .join("Contents")
        .join("Linux-x86-64")
        .join("GyroflowNiyien.ofx")
}

fn detect_linux_openfx_bundle(bundle: &Path) -> io::Result<String> {
    let version_file = bundle.join("Contents").join("version.txt");
    if !linux_openfx_binary(bundle).is_file() || !version_file.is_file() {
        return Ok(String::new());
    }
    Ok(std::fs::read_to_string(version_file)?.trim().to_owned())
}

fn validate_linux_openfx_source(extracted_root: &Path) -> io::Result<PathBuf> {
    let canonical_root = extracted_root.canonicalize()?;
    let source = canonical_root
        .join("GyroflowNiyien.ofx.bundle")
        .canonicalize()?;
    if !source.starts_with(&canonical_root) || detect_linux_openfx_bundle(&source)?.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Linux OpenFX package is missing its x86_64 binary or version file",
        ));
    }
    Ok(source)
}

fn copy_directory_contents(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory_contents(&source_path, &destination_path)?;
        } else {
            std::fs::copy(source_path, destination_path)?;
        }
    }
    Ok(())
}

fn copy_linux_openfx_bundle_direct(source: &Path, install_root: &Path) -> io::Result<()> {
    let destination = install_root.join("GyroflowNiyien.ofx.bundle");
    copy_directory_contents(source, &destination)
}

fn linux_manual_install_error(source: &Path, detail: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "{LINUX_PLUGIN_MANUAL_INSTALL_REQUIRED}{}|{LINUX_OPENFX_INSTALL_ROOT}|{detail}",
            source.display()
        ),
    )
}

fn run_linux_privileged_copy_with<F>(source: &Path, mut run: F) -> io::Result<()>
where
    F: FnMut(&str, &[std::ffi::OsString]) -> io::Result<bool>,
{
    let args = vec![
        std::ffi::OsString::from("/bin/cp"),
        std::ffi::OsString::from("-a"),
        std::ffi::OsString::from("--"),
        source.as_os_str().to_owned(),
        std::ffi::OsString::from(LINUX_OPENFX_INSTALL_ROOT),
    ];
    match run("pkexec", &args) {
        Ok(true) => Ok(()),
        Ok(false) => Err(linux_manual_install_error(
            source,
            "pkexec returned a failure status",
        )),
        Err(error) => Err(linux_manual_install_error(source, &error.to_string())),
    }
}

fn run_linux_privileged_copy(source: &Path) -> io::Result<()> {
    run_linux_privileged_copy_with(source, |program, args| {
        Command::new(program)
            .args(args)
            .status()
            .map(|status| status.success())
    })
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
    } else if cfg!(target_os = "linux") {
        if typ != "openfx" || extract_path != LINUX_OPENFX_INSTALL_ROOT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Linux supports only the fixed system OpenFX destination",
            ));
        }
        let canonical_source = validate_linux_openfx_source(Path::new(tempdir))?;
        match copy_linux_openfx_bundle_direct(&canonical_source, Path::new(extract_path)) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                ::log::warn!(
                    "[nle copy_files] Linux direct copy denied; requesting PolicyKit authorization"
                );
                run_linux_privileged_copy(&canonical_source)?;
                true
            }
            Err(error) => return Err(error),
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
        // on macOS osascript shows an admin auth dialog. Linux handles its
        // constrained PolicyKit retry in the platform branch above.
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
fn nightly_artifact_name_for_plugin_on_platform(
    typ: &str,
    platform: PluginPlatform,
) -> Option<&'static str> {
    match (typ, platform) {
        ("openfx", PluginPlatform::Windows) => Some("GyroflowNiyien-OpenFX-windows"),
        ("openfx", PluginPlatform::Macos) => Some("GyroflowNiyien-OpenFX-macos-zip"),
        ("openfx", PluginPlatform::Linux) => Some("GyroflowNiyien-OpenFX-linux"),
        ("adobe", PluginPlatform::Windows) => Some("GyroflowNiyien-Adobe-windows"),
        ("adobe", PluginPlatform::Macos) => Some("GyroflowNiyien-Adobe-macos-zip"),
        ("finalcut", PluginPlatform::Macos) => Some(FINALCUT_ARTIFACT_NAME),
        _ => None,
    }
}

fn nightly_artifact_name_for_plugin(typ: &str) -> Option<&'static str> {
    nightly_artifact_name_for_plugin_on_platform(typ, current_plugin_platform())
}

fn plugin_package_for_platform(
    typ: &str,
    platform: PluginPlatform,
) -> Option<(&'static str, &'static str)> {
    match (typ, platform) {
        ("openfx", PluginPlatform::Windows) => Some((
            "GyroflowNiyien-OpenFX-windows.zip",
            "C:/Program Files/Common Files/OFX/Plugins/",
        )),
        ("openfx", PluginPlatform::Macos) => {
            Some(("GyroflowNiyien-OpenFX-macos.zip", "/Library/OFX/Plugins/"))
        }
        ("openfx", PluginPlatform::Linux) => {
            Some(("GyroflowNiyien-OpenFX-linux.zip", LINUX_OPENFX_INSTALL_ROOT))
        }
        ("adobe", PluginPlatform::Windows) => Some((
            "GyroflowNiyien-Adobe-windows.aex",
            "C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/",
        )),
        ("adobe", PluginPlatform::Macos) => Some((
            "GyroflowNiyien-Adobe-macos.zip",
            "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/",
        )),
        ("finalcut", PluginPlatform::Macos) => Some((FINALCUT_ASSET_NAME, "/Applications/")),
        _ => None,
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinalCutTemplateMarker {
    schema_version: u32,
    template_version: String,
    effect_bundle_identifier: String,
    #[serde(rename = "effectUUID")]
    effect_uuid: String,
    #[serde(rename = "templateSHA256")]
    template_sha256: String,
}

fn plist_string(path: &Path, key: &str) -> io::Result<String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/plutil")
            .args(["-extract", key, "raw", "-o", "-"])
            .arg(path)
            .output()?;
        if !output.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{} is missing {key}: {}",
                    path.display(),
                    String::from_utf8_lossy(&output.stderr).trim()
                ),
            ));
        }
        let value = String::from_utf8(output.stdout)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let value = value.trim();
        if value.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} has an empty {key}", path.display()),
            ));
        }
        Ok(value.to_owned())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let contents = std::fs::read_to_string(path)?;
        let expression = regex::Regex::new(&format!(
            r#"<key>{}</key>\s*<string>([^<]+)</string>"#,
            regex::escape(key)
        ))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        expression
            .captures(&contents)
            .and_then(|captures| captures.get(1))
            .map(|value| value.as_str().to_owned())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} is missing {key}", path.display()),
                )
            })
    }
}

fn finalcut_xpc_path(app: &Path) -> PathBuf {
    app.join("Contents").join("PlugIns").join(FINALCUT_XPC_NAME)
}

fn finalcut_bundled_template_path(app: &Path) -> PathBuf {
    app.join("Contents")
        .join("Resources")
        .join("Motion Templates")
        .join("Effects.localized")
        .join("NiYien")
        .join("Gyroflow")
}

fn validate_finalcut_app_structure(app: &Path) -> io::Result<String> {
    if !app.is_dir() || app.file_name().and_then(|name| name.to_str()) != Some(FINALCUT_APP_NAME) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Final Cut package must contain {FINALCUT_APP_NAME}"),
        ));
    }
    let app_info = app.join("Contents").join("Info.plist");
    let xpc = finalcut_xpc_path(app);
    let xpc_info = xpc.join("Contents").join("Info.plist");
    let app_identifier = plist_string(&app_info, "CFBundleIdentifier")?;
    let xpc_identifier = plist_string(&xpc_info, "CFBundleIdentifier")?;
    let app_version = plist_string(&app_info, "CFBundleShortVersionString")?;
    let xpc_version = plist_string(&xpc_info, "CFBundleShortVersionString")?;
    if app_identifier != FINALCUT_APP_BUNDLE_ID || xpc_identifier != FINALCUT_XPC_BUNDLE_ID {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut App or XPC bundle identifier mismatch",
        ));
    }
    if app_version.is_empty() || app_version != xpc_version {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut App and XPC versions do not match",
        ));
    }
    if !app
        .join("Contents")
        .join("MacOS")
        .join("GyroflowNiYien Final Cut")
        .is_file()
        || !xpc
            .join("Contents")
            .join("MacOS")
            .join("GyroflowNiYienFinalCutEffect")
            .is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut App or XPC executable is missing",
        ));
    }
    let bundled_template = finalcut_bundled_template_path(app);
    let moef = bundled_template.join(FINALCUT_TEMPLATE_NAME);
    if !moef.is_file()
        || !bundled_template.join("large.png").is_file()
        || !bundled_template.join("small.png").is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut App is missing its complete Motion template resources",
        ));
    }
    let template = std::fs::read_to_string(moef)?;
    if !template.contains(FINALCUT_EFFECT_UUID)
        || template.contains("Gyroflow Toolbox")
        || template.contains("Project Path")
        || template.contains("Bookmark")
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut Motion template identity or persistence contract is invalid",
        ));
    }
    if app
        .join("Contents")
        .join("PlugIns")
        .read_dir()?
        .filter_map(Result::ok)
        .any(|entry| entry.path().extension().and_then(|value| value.to_str()) == Some("appex"))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut package must not contain a Workflow extension",
        ));
    }
    Ok(app_version)
}

fn validate_finalcut_extracted_root(root: &Path) -> io::Result<PathBuf> {
    let entries: Vec<_> = std::fs::read_dir(root)?.collect::<Result<_, _>>()?;
    if entries.len() != 1 || entries[0].file_name() != FINALCUT_APP_NAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Final Cut zip must contain exactly one top-level {FINALCUT_APP_NAME}"),
        ));
    }
    let app = entries[0].path();
    validate_finalcut_app_structure(&app)?;
    Ok(app)
}

fn validate_finalcut_trust_with<F>(app: &Path, mut run: F) -> io::Result<()>
where
    F: FnMut(&str, &[std::ffi::OsString]) -> io::Result<bool>,
{
    let app_arg = app.as_os_str().to_owned();
    let codesign_args = vec![
        "--verify".into(),
        "--deep".into(),
        "--strict".into(),
        "--verbose=2".into(),
        app_arg.clone(),
    ];
    if !run("/usr/bin/codesign", &codesign_args)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Final Cut App code signature verification failed",
        ));
    }
    let spctl_args = vec![
        "--assess".into(),
        "--type".into(),
        "execute".into(),
        "--verbose=4".into(),
        app_arg,
    ];
    if !run("/usr/sbin/spctl", &spctl_args)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Final Cut App Gatekeeper assessment failed",
        ));
    }
    Ok(())
}

fn validate_finalcut_trust(app: &Path) -> io::Result<()> {
    validate_finalcut_trust_with(app, |program, arguments| {
        Command::new(program)
            .args(arguments)
            .status()
            .map(|status| status.success())
    })
}

fn finalcut_template_install_path(home: &Path) -> PathBuf {
    home.join("Movies")
        .join("Motion Templates.localized")
        .join("Effects.localized")
        .join("NiYien")
        .join("Gyroflow")
}

fn validate_installed_finalcut_template(template: &Path, app_version: &str) -> io::Result<()> {
    if !template.join(FINALCUT_TEMPLATE_NAME).is_file()
        || !template.join("large.png").is_file()
        || !template.join("small.png").is_file()
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut installed template resources are incomplete",
        ));
    }
    let marker: FinalCutTemplateMarker =
        serde_json::from_slice(&std::fs::read(template.join(FINALCUT_TEMPLATE_MARKER))?)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    if marker.schema_version != 1
        || marker.template_version != app_version
        || marker.effect_bundle_identifier != FINALCUT_XPC_BUNDLE_ID
        || marker.effect_uuid != FINALCUT_EFFECT_UUID
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut template installation marker does not match the App",
        ));
    }
    let template_bytes = std::fs::read(template.join(FINALCUT_TEMPLATE_NAME))?;
    let actual_hash = format!("{:x}", Sha256::digest(template_bytes));
    if actual_hash != marker.template_sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Final Cut installed template hash mismatch",
        ));
    }
    Ok(())
}

fn detect_finalcut_at<F>(
    app: &Path,
    template: &Path,
    latest_version: &str,
    mut trust: F,
) -> FinalCutDetection
where
    F: FnMut(&Path) -> io::Result<()>,
{
    if !app.exists() {
        return FinalCutDetection {
            state: FinalCutInstallState::NotInstalled,
            version: String::new(),
            detail: "Final Cut integration App is not installed".to_owned(),
        };
    }
    let version = match validate_finalcut_app_structure(app) {
        Ok(version) => version,
        Err(error) => {
            return FinalCutDetection {
                state: FinalCutInstallState::BrokenOrUntrusted,
                version: String::new(),
                detail: error.to_string(),
            };
        }
    };
    if let Err(error) = trust(app) {
        return FinalCutDetection {
            state: FinalCutInstallState::BrokenOrUntrusted,
            version,
            detail: error.to_string(),
        };
    }
    if let Err(error) = validate_installed_finalcut_template(template, &version) {
        return FinalCutDetection {
            state: FinalCutInstallState::AppInstalledTemplateMissing,
            version,
            detail: error.to_string(),
        };
    }
    let update_available = compare_plugin_versions(latest_version, &version) == Ordering::Greater;
    FinalCutDetection {
        state: if update_available {
            FinalCutInstallState::UpdateAvailable
        } else {
            FinalCutInstallState::Installed
        },
        version,
        detail: if update_available {
            "A newer Final Cut integration is available".to_owned()
        } else {
            "Final Cut integration is installed".to_owned()
        },
    }
}

fn detect_finalcut(latest_version: &str) -> io::Result<FinalCutDetection> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unavailable"))?;
    Ok(detect_finalcut_at(
        Path::new(FINALCUT_APP_PATH),
        &finalcut_template_install_path(&home),
        latest_version,
        validate_finalcut_trust,
    ))
}

fn finalcut_download_url(plugins_base: &str) -> String {
    let normalized = plugins_base.trim().trim_end_matches('/');
    let base = if normalized.is_empty() {
        DEFAULT_RELEASE_PLUGINS_BASE
    } else {
        normalized
    };
    if base.contains("nightly.link") {
        format!("{base}/{FINALCUT_ARTIFACT_NAME}.zip")
    } else {
        format!("{base}/{FINALCUT_ASSET_NAME}")
    }
}

fn unwrap_finalcut_delivery(content: Vec<u8>) -> io::Result<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(&content)).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Final Cut download is not a zip: {error}"),
        )
    })?;
    if archive.len() == 1 && archive.name_for_index(0) == Some(FINALCUT_ASSET_NAME) {
        let mut inner = Vec::new();
        use std::io::Read;
        archive.by_index(0)?.read_to_end(&mut inner)?;
        zip::ZipArchive::new(Cursor::new(&inner)).map_err(|error| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Final Cut artifact contains an invalid inner zip: {error}"),
            )
        })?;
        return Ok(inner);
    }
    drop(archive);
    Ok(content)
}

fn extract_finalcut_archive_with<F>(
    archive: &Path,
    destination: &Path,
    mut run: F,
) -> io::Result<()>
where
    F: FnMut(&str, &[std::ffi::OsString]) -> io::Result<bool>,
{
    let arguments = vec![
        "-x".into(),
        "-k".into(),
        archive.as_os_str().to_owned(),
        destination.as_os_str().to_owned(),
    ];
    if run("/usr/bin/ditto", &arguments)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "ditto failed to extract the Final Cut package",
        ))
    }
}

fn extract_finalcut_archive(archive: &Path, destination: &Path) -> io::Result<()> {
    extract_finalcut_archive_with(archive, destination, |program, arguments| {
        Command::new(program)
            .args(arguments)
            .status()
            .map(|status| status.success())
    })
}

fn finalcut_privileged_install_script() -> &'static str {
    r#"on run argv
set sourcePath to item 1 of argv
set destinationPath to item 2 of argv
set transactionID to item 3 of argv
set stagingPath to destinationPath & ".staging-" & transactionID
set backupPath to destinationPath & ".backup-" & transactionID
set commandText to "/bin/rm -rf " & quoted form of stagingPath & " " & quoted form of backupPath & "; " & ¬
    "/usr/bin/ditto " & quoted form of sourcePath & " " & quoted form of stagingPath & "; " & ¬
    "if [ -e " & quoted form of destinationPath & " ]; then /bin/mv " & quoted form of destinationPath & " " & quoted form of backupPath & "; fi; " & ¬
    "if /bin/mv " & quoted form of stagingPath & " " & quoted form of destinationPath & "; then /bin/rm -rf " & quoted form of backupPath & "; " & ¬
    "else status=$?; if [ -e " & quoted form of backupPath & " ]; then /bin/mv " & quoted form of backupPath & " " & quoted form of destinationPath & "; fi; exit $status; fi"
do shell script commandText with administrator privileges
end run"#
}

fn install_finalcut_app_privileged(source: &Path, destination: &Path) -> io::Result<()> {
    let transaction_id = uuid::Uuid::new_v4().simple().to_string();
    let status = Command::new("/usr/bin/osascript")
        .args([
            "-e".as_ref(),
            finalcut_privileged_install_script().as_ref(),
            source.as_os_str(),
            destination.as_os_str(),
            transaction_id.as_ref(),
        ])
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "FINALCUT_APP_INSTALL_BLOCKED: close Final Cut Pro, Motion, and the Gyroflow Final Cut App, then retry",
        ))
    }
}

fn register_finalcut_xpc_with<F>(app: &Path, mut run: F) -> io::Result<()>
where
    F: FnMut(&Path, &[std::ffi::OsString]) -> io::Result<bool>,
{
    let xpc = finalcut_xpc_path(app);
    if !xpc.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "FINALCUT_PLUGIN_REGISTRATION_FAILED: installed FxPlug XPC is missing",
        ));
    }
    let arguments = [std::ffi::OsString::from("-a"), xpc.as_os_str().to_owned()];
    if run(Path::new("/usr/bin/pluginkit"), &arguments)? {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            "FINALCUT_PLUGIN_REGISTRATION_FAILED: PlugInKit rejected the installed FxPlug XPC",
        ))
    }
}

fn register_finalcut_xpc(app: &Path) -> io::Result<()> {
    register_finalcut_xpc_with(app, |program, arguments| {
        Command::new(program)
            .args(arguments)
            .status()
            .map(|status| status.success())
    })
}

fn run_finalcut_template_installer(app: &Path) -> io::Result<()> {
    let executable = app
        .join("Contents")
        .join("MacOS")
        .join("GyroflowNiYien Final Cut");
    let status = Command::new(&executable)
        .arg("--install-template-and-quit")
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "FINALCUT_TEMPLATE_INSTALL_FAILED:{}",
                status.code().unwrap_or(-1)
            ),
        ))
    }
}

fn finalize_finalcut_install_with<T, R>(
    app: &Path,
    mut install_template: T,
    mut register_xpc: R,
) -> io::Result<()>
where
    T: FnMut(&Path) -> io::Result<()>,
    R: FnMut(&Path) -> io::Result<()>,
{
    install_template(app)?;
    register_xpc(app)
}

fn install_finalcut(plugins_base: String) -> io::Result<String> {
    let download_url = finalcut_download_url(&plugins_base);
    crate::network::prewarm_url(&download_url);
    let mut response = crate::network::call_with_plugin_retry("plugin:finalcut", || {
        crate::network::get(&download_url).call()
    })
    .map_err(|error| {
        io::Error::new(
            io::ErrorKind::Other,
            format!("Failed to download {download_url}: {error}"),
        )
    })?
    .into_body()
    .into_reader();
    use std::io::Read;
    let mut downloaded = Vec::new();
    response.read_to_end(&mut downloaded)?;
    let delivery = unwrap_finalcut_delivery(downloaded)?;

    let temporary = tempfile::tempdir()?;
    let archive = temporary.path().join(FINALCUT_ASSET_NAME);
    let extracted = temporary.path().join("extracted");
    std::fs::create_dir(&extracted)?;
    std::fs::write(&archive, delivery)?;
    extract_finalcut_archive(&archive, &extracted)?;
    let source_app = validate_finalcut_extracted_root(&extracted)?;
    validate_finalcut_trust(&source_app)?;
    install_finalcut_app_privileged(&source_app, Path::new(FINALCUT_APP_PATH))?;

    let installed_app = Path::new(FINALCUT_APP_PATH);
    let latest = latest_plugin_info();
    if let Err(error) = finalize_finalcut_install_with(
        installed_app,
        run_finalcut_template_installer,
        register_finalcut_xpc,
    ) {
        let detection = detect_finalcut(&latest.version)?;
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "{error}; Final Cut integration state after post-commit setup failure: {:?}: {}",
                detection.state, detection.detail
            ),
        ));
    }
    let detection = detect_finalcut(&latest.version)?;
    if !matches!(
        detection.state,
        FinalCutInstallState::Installed | FinalCutInstallState::UpdateAvailable
    ) {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            format!("FINALCUT_INSTALL_VERIFICATION_FAILED:{}", detection.detail),
        ));
    }
    remember_installed_plugin("finalcut", &detection.version, &latest);
    Ok(detection.version)
}

#[cfg(test)]
fn replace_finalcut_app_for_test(
    source: &Path,
    destination: &Path,
    fail_after_backup: bool,
) -> io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(".finalcut-staging-test");
    let backup = parent.join(".finalcut-backup-test");
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    if backup.exists() {
        std::fs::remove_dir_all(&backup)?;
    }
    copy_directory_contents(source, &staging)?;
    let mut moved_existing = false;
    if destination.exists() {
        std::fs::rename(destination, &backup)?;
        moved_existing = true;
    }
    if fail_after_backup {
        std::fs::remove_dir_all(&staging)?;
        if moved_existing {
            std::fs::rename(&backup, destination)?;
        }
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "injected Final Cut install failure",
        ));
    }
    if let Err(error) = std::fs::rename(&staging, destination) {
        if moved_existing {
            let _ = std::fs::rename(&backup, destination);
        }
        return Err(error);
    }
    if moved_existing {
        std::fs::remove_dir_all(backup)?;
    }
    Ok(())
}

pub fn install(typ: &str, plugins_base: String) -> io::Result<String> {
    let platform = current_plugin_platform();
    if !plugin_available_on_platform(typ, platform) {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Plugin type {typ} is unavailable on this platform"),
        ));
    }
    if typ == "finalcut" {
        return install_finalcut(plugins_base);
    }
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
    let (filename, extract_path) = plugin_package_for_platform(typ, platform).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            format!("Plugin type {typ} is unavailable on this platform"),
        )
    })?;
    // For nightly-style bases (artifact-mode plugin publish), URLs follow
    //   {base}{artifact_name}.zip
    // where {artifact_name} is the V4 short name from the plugin workflow
    // (no file extension; macos-zip variant has the `-zip` suffix). The
    // wrapper served by nightly.link contains the deliverable file we know
    // by `filename`; the existing zip-branch in this function unwraps one
    // layer, so the only change needed is the URL construction.
    let download_url = if is_nightly_base {
        let artifact_name = nightly_artifact_name_for_plugin(typ).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                format!("Plugin type {typ} has no artifact on this platform"),
            )
        })?;
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

fn nle_detection_paths_for_platform(
    typ: &str,
    platform: PluginPlatform,
    username: &str,
) -> Vec<PathBuf> {
    use chrono::{Datelike, Utc};

    match (typ, platform) {
        ("openfx", PluginPlatform::Windows) => vec![
            PathBuf::from(format!(
                "C:/Users/{}/AppData/Roaming/Blackmagic Design/DaVinci Resolve",
                username
            )),
            PathBuf::from("C:/Program Files/Common Files/OFX/Plugins"),
            PathBuf::from("C:/Program Files/VEGAS"),
        ],
        ("openfx", PluginPlatform::Macos) => vec![
            PathBuf::from("/Applications/DaVinci Resolve/"),
            PathBuf::from("/Applications/DaVinci Resolve.app/"),
            PathBuf::from("/Applications/DaVinci Resolve Studio/"),
            PathBuf::from("/Applications/DaVinci Resolve Studio.app/"),
            PathBuf::from("/Library/OFX/Plugins"),
        ],
        ("openfx", PluginPlatform::Linux) => vec![
            PathBuf::from("/opt/resolve"),
            PathBuf::from("/usr/OFX/Plugins"),
        ],
        ("adobe", PluginPlatform::Windows) => vec![PathBuf::from(
            "C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/",
        )],
        ("adobe", PluginPlatform::Macos) => {
            let mut paths = Vec::new();
            for year in 2019..(Utc::now().year() + 1) {
                paths.push(PathBuf::from(format!(
                    "/Applications/Adobe Premiere Pro {year}/"
                )));
                paths.push(PathBuf::from(format!(
                    "/Applications/Adobe After Effects {year}/"
                )));
                paths.push(PathBuf::from(format!(
                    "/Applications/Adobe Premiere Pro {year}.app/"
                )));
                paths.push(PathBuf::from(format!(
                    "/Applications/Adobe After Effects {year}.app/"
                )));
            }
            paths
        }
        _ => Vec::new(),
    }
}

pub fn is_nle_installed(typ: &str) -> bool {
    nle_detection_paths_for_platform(
        typ,
        current_plugin_platform(),
        &whoami::username().unwrap_or_default(),
    )
    .iter()
    .any(|path| path.exists())
}

fn final_cut_host_bundle_identifier(path: &Path) -> Option<String> {
    plist_string(
        &path.join("Contents").join("Info.plist"),
        "CFBundleIdentifier",
    )
    .ok()
}

fn valid_final_cut_host_bundle(path: &Path) -> bool {
    path.is_dir()
        && final_cut_host_bundle_identifier(path)
            .is_some_and(|identifier| FINAL_CUT_HOST_BUNDLE_IDS.contains(&identifier.as_str()))
}

fn final_cut_host_detected_from_paths(
    workspace_paths: &[PathBuf],
    fallback_paths: &[PathBuf],
) -> bool {
    workspace_paths
        .iter()
        .chain(fallback_paths)
        .any(|path| valid_final_cut_host_bundle(path))
}

#[cfg(target_os = "macos")]
fn final_cut_workspace_application_paths() -> Vec<PathBuf> {
    use objc2::rc::autoreleasepool;
    use objc2_app_kit::NSWorkspace;
    use objc2_foundation::NSString;

    autoreleasepool(|_| {
        let workspace = NSWorkspace::sharedWorkspace();
        FINAL_CUT_HOST_BUNDLE_IDS
            .iter()
            .filter_map(|identifier| {
                let identifier = NSString::from_str(identifier);
                workspace
                    .URLForApplicationWithBundleIdentifier(&identifier)
                    .and_then(|url| url.path())
                    .map(|path| PathBuf::from(path.to_string()))
            })
            .collect()
    })
}

pub fn is_final_cut_host_installed() -> bool {
    #[cfg(target_os = "macos")]
    {
        let workspace_paths = final_cut_workspace_application_paths();
        let fallback_paths: Vec<_> = FINAL_CUT_HOST_FALLBACK_PATHS
            .iter()
            .map(PathBuf::from)
            .collect();
        final_cut_host_detected_from_paths(&workspace_paths, &fallback_paths)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

pub fn latest_version() -> Option<String> {
    let info = latest_plugin_info();
    (!info.version.is_empty()).then_some(info.version)
}

pub fn status_json(typ: &str) -> io::Result<String> {
    let latest = latest_plugin_info();
    if typ == "finalcut" && current_plugin_platform() == PluginPlatform::Macos {
        let detection = detect_finalcut(&latest.version)?;
        let installed = load_installed_plugin_info(typ, &detection.version);
        let source_changed = source_changed(&installed, &latest);
        let update_available = detection.state == FinalCutInstallState::UpdateAvailable
            || (detection.state == FinalCutInstallState::Installed && source_changed);
        let payload = PluginStatus {
            typ: typ.to_owned(),
            installed_version: detection.version,
            installed_source_ref: installed.source_ref,
            installed_source_base: installed.source_base,
            latest_version: latest.version.clone(),
            latest_source_ref: latest.source_ref.clone(),
            latest_source_tag: latest.source_tag.clone(),
            latest_source_base: latest.source_base.clone(),
            latest_source_mode: latest.source_mode.clone(),
            latest_label: latest_display_label(&latest),
            source_changed,
            update_available,
            is_latest: detection.state == FinalCutInstallState::Installed && !source_changed,
            state: if update_available {
                "update_available".to_owned()
            } else {
                serde_json::to_value(detection.state)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| "broken_or_untrusted".to_owned())
            },
            repair_required: matches!(
                detection.state,
                FinalCutInstallState::AppInstalledTemplateMissing
                    | FinalCutInstallState::BrokenOrUntrusted
            ),
            detail: detection.detail,
        };
        return serde_json::to_string(&payload)
            .map_err(|error| io::Error::new(io::ErrorKind::Other, error));
    }
    let installed_version = detect(typ)?;
    let installed = load_installed_plugin_info(typ, &installed_version);
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
        state: if installed_version.is_empty() {
            "not_installed".to_owned()
        } else if update_available {
            "update_available".to_owned()
        } else {
            "installed".to_owned()
        },
        repair_required: false,
        detail: String::new(),
    };
    serde_json::to_string(&payload).map_err(|err| io::Error::new(io::ErrorKind::Other, err))
}

pub fn detect(typ: &str) -> io::Result<String> {
    if !plugin_available_on_platform(typ, current_plugin_platform()) {
        return Ok(String::new());
    }
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
        if typ == "finalcut" {
            return Ok(detect_finalcut("")?.version);
        }
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
    #[cfg(target_os = "linux")]
    {
        if typ == "openfx" && !path.is_empty() {
            let version = detect_linux_openfx_bundle(Path::new(path))?;
            ::log::info!("[nle detect] linux bundle={path:?} version={version:?}");
            Ok(version)
        } else {
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

    fn write_test_plist(path: &Path, identifier: &str, version: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            path,
            format!(
                "<?xml version=\"1.0\"?><plist><dict>\
                 <key>CFBundleIdentifier</key><string>{identifier}</string>\
                 <key>CFBundleShortVersionString</key><string>{version}</string>\
                 </dict></plist>"
            ),
        )
        .unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn plist_reader_accepts_binary_host_info_plists() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("Info.plist");
        write_test_plist(&path, "com.apple.FinalCutApp", "12.3");
        assert!(
            Command::new("/usr/bin/plutil")
                .args(["-convert", "binary1"])
                .arg(&path)
                .status()
                .unwrap()
                .success()
        );

        assert_eq!(
            plist_string(&path, "CFBundleIdentifier").unwrap(),
            "com.apple.FinalCutApp"
        );
        assert_eq!(
            plist_string(&path, "CFBundleShortVersionString").unwrap(),
            "12.3"
        );
    }

    fn write_finalcut_test_app(root: &Path, version: &str) -> PathBuf {
        let app = root.join(FINALCUT_APP_NAME);
        write_test_plist(
            &app.join("Contents").join("Info.plist"),
            FINALCUT_APP_BUNDLE_ID,
            version,
        );
        let xpc = finalcut_xpc_path(&app);
        write_test_plist(
            &xpc.join("Contents").join("Info.plist"),
            FINALCUT_XPC_BUNDLE_ID,
            version,
        );
        let app_binary = app
            .join("Contents")
            .join("MacOS")
            .join("GyroflowNiYien Final Cut");
        let xpc_binary = xpc
            .join("Contents")
            .join("MacOS")
            .join("GyroflowNiYienFinalCutEffect");
        std::fs::create_dir_all(app_binary.parent().unwrap()).unwrap();
        std::fs::create_dir_all(xpc_binary.parent().unwrap()).unwrap();
        std::fs::write(app_binary, b"app").unwrap();
        std::fs::write(xpc_binary, b"xpc").unwrap();
        let template = finalcut_bundled_template_path(&app);
        std::fs::create_dir_all(&template).unwrap();
        std::fs::write(
            template.join(FINALCUT_TEMPLATE_NAME),
            format!(
                "<filter pluginUUID=\"{FINALCUT_EFFECT_UUID}\">\
                 <parameter name=\"Project Payload\"/>\
                 <parameter name=\"Timing Payload\"/></filter>"
            ),
        )
        .unwrap();
        std::fs::write(template.join("large.png"), b"large").unwrap();
        std::fs::write(template.join("small.png"), b"small").unwrap();
        app
    }

    fn write_installed_finalcut_template(root: &Path, version: &str) -> PathBuf {
        let template = root.join("installed-template");
        std::fs::create_dir_all(&template).unwrap();
        let moef = format!("<filter pluginUUID=\"{FINALCUT_EFFECT_UUID}\"/>");
        std::fs::write(template.join(FINALCUT_TEMPLATE_NAME), moef.as_bytes()).unwrap();
        std::fs::write(template.join("large.png"), b"large").unwrap();
        std::fs::write(template.join("small.png"), b"small").unwrap();
        let marker = serde_json::json!({
            "schemaVersion": 1,
            "templateVersion": version,
            "effectBundleIdentifier": FINALCUT_XPC_BUNDLE_ID,
            "effectUUID": FINALCUT_EFFECT_UUID,
            "templateSHA256": format!("{:x}", Sha256::digest(moef.as_bytes())),
        });
        std::fs::write(
            template.join(FINALCUT_TEMPLATE_MARKER),
            serde_json::to_vec(&marker).unwrap(),
        )
        .unwrap();
        template
    }

    #[test]
    fn finalcut_is_macos_only_with_fixed_asset_and_install_path() {
        assert!(plugin_available_on_platform(
            "finalcut",
            PluginPlatform::Macos
        ));
        assert!(!plugin_available_on_platform(
            "finalcut",
            PluginPlatform::Windows
        ));
        assert!(!plugin_available_on_platform(
            "finalcut",
            PluginPlatform::Linux
        ));
        assert_eq!(
            get_path_for_platform("finalcut", PluginPlatform::Macos),
            FINALCUT_APP_PATH
        );
        assert_eq!(
            plugin_package_for_platform("finalcut", PluginPlatform::Macos),
            Some((FINALCUT_ASSET_NAME, "/Applications/"))
        );
        assert_eq!(
            nightly_artifact_name_for_plugin_on_platform("finalcut", PluginPlatform::Macos),
            Some(FINALCUT_ARTIFACT_NAME)
        );
    }

    #[test]
    fn finalcut_addition_preserves_existing_plugin_paths_assets_and_artifacts() {
        let cases = [
            (
                "openfx",
                PluginPlatform::Windows,
                "C:/Program Files/Common Files/OFX/Plugins/GyroflowNiyien.ofx.bundle",
                (
                    "GyroflowNiyien-OpenFX-windows.zip",
                    "C:/Program Files/Common Files/OFX/Plugins/",
                ),
                "GyroflowNiyien-OpenFX-windows",
            ),
            (
                "adobe",
                PluginPlatform::Windows,
                "C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien-Adobe-windows.aex",
                (
                    "GyroflowNiyien-Adobe-windows.aex",
                    "C:/Program Files/Adobe/Common/Plug-ins/7.0/MediaCore/",
                ),
                "GyroflowNiyien-Adobe-windows",
            ),
            (
                "openfx",
                PluginPlatform::Macos,
                "/Library/OFX/Plugins/GyroflowNiyien.ofx.bundle",
                ("GyroflowNiyien-OpenFX-macos.zip", "/Library/OFX/Plugins/"),
                "GyroflowNiyien-OpenFX-macos-zip",
            ),
            (
                "adobe",
                PluginPlatform::Macos,
                "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/GyroflowNiyien.plugin",
                (
                    "GyroflowNiyien-Adobe-macos.zip",
                    "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore/",
                ),
                "GyroflowNiyien-Adobe-macos-zip",
            ),
            (
                "openfx",
                PluginPlatform::Linux,
                "/usr/OFX/Plugins/GyroflowNiyien.ofx.bundle",
                ("GyroflowNiyien-OpenFX-linux.zip", "/usr/OFX/Plugins/"),
                "GyroflowNiyien-OpenFX-linux",
            ),
        ];

        for (typ, platform, path, package, artifact) in cases {
            assert!(plugin_available_on_platform(typ, platform));
            assert_eq!(get_path_for_platform(typ, platform), path);
            assert_eq!(plugin_package_for_platform(typ, platform), Some(package));
            assert_eq!(
                nightly_artifact_name_for_plugin_on_platform(typ, platform),
                Some(artifact)
            );
        }
        assert!(!plugin_available_on_platform(
            "adobe",
            PluginPlatform::Linux
        ));
        assert!(plugin_package_for_platform("adobe", PluginPlatform::Linux).is_none());
    }

    #[test]
    fn finalcut_detection_distinguishes_all_five_states() {
        let root = tempfile::tempdir().unwrap();
        let app = root.path().join(FINALCUT_APP_NAME);
        let template = root.path().join("template");
        let missing = detect_finalcut_at(&app, &template, "2.1.2", |_| Ok(()));
        assert_eq!(missing.state, FinalCutInstallState::NotInstalled);

        let app = write_finalcut_test_app(root.path(), "2.1.2");
        let template_missing = detect_finalcut_at(&app, &template, "2.1.2", |_| Ok(()));
        assert_eq!(
            template_missing.state,
            FinalCutInstallState::AppInstalledTemplateMissing
        );

        let template = write_installed_finalcut_template(root.path(), "2.1.2");
        let installed = detect_finalcut_at(&app, &template, "2.1.2", |_| Ok(()));
        assert_eq!(installed.state, FinalCutInstallState::Installed);
        std::fs::remove_file(template.join("small.png")).unwrap();
        let preview_missing = detect_finalcut_at(&app, &template, "2.1.2", |_| Ok(()));
        assert_eq!(
            preview_missing.state,
            FinalCutInstallState::AppInstalledTemplateMissing
        );
        std::fs::write(template.join("small.png"), b"small").unwrap();
        let update = detect_finalcut_at(&app, &template, "2.1.3", |_| Ok(()));
        assert_eq!(update.state, FinalCutInstallState::UpdateAvailable);
        let untrusted = detect_finalcut_at(&app, &template, "2.1.2", |_| {
            Err(io::Error::new(io::ErrorKind::PermissionDenied, "untrusted"))
        });
        assert_eq!(untrusted.state, FinalCutInstallState::BrokenOrUntrusted);
    }

    #[test]
    fn finalcut_structure_rejects_wrong_identity_and_extra_archive_entries() {
        let root = tempfile::tempdir().unwrap();
        let app = write_finalcut_test_app(root.path(), "2.1.2");
        write_test_plist(
            &app.join("Contents").join("Info.plist"),
            "com.example.fake",
            "2.1.2",
        );
        assert_eq!(
            validate_finalcut_app_structure(&app).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );

        let root = tempfile::tempdir().unwrap();
        write_finalcut_test_app(root.path(), "2.1.2");
        std::fs::write(root.path().join("unexpected.txt"), b"unexpected").unwrap();
        assert_eq!(
            validate_finalcut_extracted_root(root.path())
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn finalcut_trust_uses_codesign_before_gatekeeper_without_shell() {
        let app = Path::new("/tmp/GyroflowNiYien Final Cut.app");
        let mut calls = Vec::new();
        validate_finalcut_trust_with(app, |program, arguments| {
            calls.push((program.to_owned(), arguments.to_vec()));
            Ok(true)
        })
        .unwrap();

        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].0, "/usr/bin/codesign");
        assert_eq!(calls[1].0, "/usr/sbin/spctl");
        assert!(calls[0].1.iter().any(|argument| argument == "--deep"));
        assert!(calls[0].1.iter().any(|argument| argument == "--strict"));
        assert!(calls[1].1.iter().any(|argument| argument == "execute"));
    }

    #[test]
    fn finalcut_extraction_uses_ditto_with_fixed_arguments() {
        let archive = Path::new("/tmp/package.zip");
        let destination = Path::new("/tmp/extracted");
        let mut calls = Vec::new();
        extract_finalcut_archive_with(archive, destination, |program, arguments| {
            calls.push((program.to_owned(), arguments.to_vec()));
            Ok(true)
        })
        .unwrap();
        assert_eq!(
            calls,
            vec![(
                "/usr/bin/ditto".to_owned(),
                vec![
                    "-x".into(),
                    "-k".into(),
                    archive.as_os_str().to_owned(),
                    destination.as_os_str().to_owned(),
                ],
            )]
        );
    }

    #[test]
    fn finalcut_registration_targets_the_installed_xpc() {
        let root = tempfile::tempdir().unwrap();
        let app = write_finalcut_test_app(root.path(), "2.1.2");
        let expected_xpc = finalcut_xpc_path(&app);

        register_finalcut_xpc_with(&app, |program, arguments| {
            Ok(program == Path::new("/usr/bin/pluginkit")
                && arguments
                    == [
                        std::ffi::OsString::from("-a"),
                        expected_xpc.as_os_str().to_owned(),
                    ])
        })
        .unwrap();
    }

    #[test]
    fn finalcut_post_commit_installs_template_before_registering_xpc() {
        let root = tempfile::tempdir().unwrap();
        let app = write_finalcut_test_app(root.path(), "2.1.2");
        let calls = std::cell::RefCell::new(Vec::new());

        finalize_finalcut_install_with(
            &app,
            |_| {
                calls.borrow_mut().push("template");
                Ok(())
            },
            |_| {
                calls.borrow_mut().push("registration");
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(*calls.borrow(), ["template", "registration"]);
    }

    #[test]
    fn finalcut_staged_replacement_rolls_back_and_preserves_old_app() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("source");
        let source = write_finalcut_test_app(&source_root, "2.1.3");
        let destination_root = root.path().join("Applications");
        let destination = write_finalcut_test_app(&destination_root, "2.1.2");

        let error = replace_finalcut_app_for_test(&source, &destination, true).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(
            plist_string(
                &destination.join("Contents").join("Info.plist"),
                "CFBundleShortVersionString"
            )
            .unwrap(),
            "2.1.2"
        );

        replace_finalcut_app_for_test(&source, &destination, false).unwrap();
        assert_eq!(
            plist_string(
                &destination.join("Contents").join("Info.plist"),
                "CFBundleShortVersionString"
            )
            .unwrap(),
            "2.1.3"
        );
    }

    #[test]
    fn finalcut_release_and_artifact_urls_share_fixed_filename_contract() {
        assert_eq!(
            finalcut_download_url(""),
            format!("{DEFAULT_RELEASE_PLUGINS_BASE}/{FINALCUT_ASSET_NAME}")
        );
        assert_eq!(
            finalcut_download_url("https://mirror.example/plugins/"),
            format!("https://mirror.example/plugins/{FINALCUT_ASSET_NAME}")
        );
        assert_eq!(
            finalcut_download_url("https://nightly.link/run/"),
            format!("https://nightly.link/run/{FINALCUT_ARTIFACT_NAME}.zip")
        );
    }

    fn test_zip_with_file(name: &str, contents: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let cursor = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(cursor);
        archive
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        archive.write_all(contents).unwrap();
        archive.finish().unwrap().into_inner()
    }

    #[test]
    fn finalcut_artifact_wrapper_unwraps_exactly_one_fixed_delivery_zip() {
        let delivery =
            test_zip_with_file("GyroflowNiYien Final Cut.app/Contents/Info.plist", b"plist");
        let wrapper = test_zip_with_file(FINALCUT_ASSET_NAME, &delivery);

        assert_eq!(unwrap_finalcut_delivery(wrapper).unwrap(), delivery);

        let direct = test_zip_with_file(
            "GyroflowNiYien Final Cut.app/Contents/Info.plist",
            b"direct",
        );
        assert_eq!(unwrap_finalcut_delivery(direct.clone()).unwrap(), direct);
    }

    #[test]
    fn finalcut_failed_signature_check_stops_before_gatekeeper() {
        let mut programs = Vec::new();
        let error = validate_finalcut_trust_with(Path::new("/tmp/App.app"), |program, _| {
            programs.push(program.to_owned());
            Ok(false)
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(programs, vec!["/usr/bin/codesign"]);
    }

    fn write_host_app(path: &Path, identifier: &str) {
        write_test_plist(
            &path.join("Contents").join("Info.plist"),
            identifier,
            "12.3",
        );
    }

    #[test]
    fn final_cut_host_detection_accepts_both_ids_and_rejects_same_name_impostor() {
        let root = tempfile::tempdir().unwrap();
        let creator_studio = root.path().join("Final Cut Pro Creator Studio.app");
        let legacy = root.path().join("Final Cut Pro.app");
        let impostor = root.path().join("impostor").join("Final Cut Pro.app");
        write_host_app(&creator_studio, "com.apple.FinalCutApp");
        write_host_app(&legacy, "com.apple.FinalCut");
        write_host_app(&impostor, "com.example.fake-final-cut");

        assert!(final_cut_host_detected_from_paths(
            std::slice::from_ref(&creator_studio),
            &[]
        ));
        assert!(final_cut_host_detected_from_paths(
            &[],
            std::slice::from_ref(&legacy)
        ));
        assert!(!final_cut_host_detected_from_paths(
            &[],
            std::slice::from_ref(&impostor)
        ));
    }

    fn write_linux_openfx_bundle(root: &Path, version: &str) -> PathBuf {
        let bundle = root.join("GyroflowNiyien.ofx.bundle");
        let binary = bundle
            .join("Contents")
            .join("Linux-x86-64")
            .join("GyroflowNiyien.ofx");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::write(binary, b"linux openfx").unwrap();
        std::fs::write(bundle.join("Contents").join("version.txt"), version).unwrap();
        bundle
    }

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

    #[test]
    fn linux_platform_exposes_only_resolve_openfx_contract() {
        assert!(plugin_available_on_platform(
            "openfx",
            PluginPlatform::Linux
        ));
        assert!(!plugin_available_on_platform(
            "adobe",
            PluginPlatform::Linux
        ));
        assert_eq!(
            get_path_for_platform("openfx", PluginPlatform::Linux),
            "/usr/OFX/Plugins/GyroflowNiyien.ofx.bundle"
        );
        assert_eq!(get_path_for_platform("adobe", PluginPlatform::Linux), "");

        let package = plugin_package_for_platform("openfx", PluginPlatform::Linux).unwrap();
        assert_eq!(package.0, "GyroflowNiyien-OpenFX-linux.zip");
        assert_eq!(package.1, "/usr/OFX/Plugins/");
        assert_eq!(
            nightly_artifact_name_for_plugin_on_platform("openfx", PluginPlatform::Linux),
            Some("GyroflowNiyien-OpenFX-linux")
        );
        assert!(plugin_package_for_platform("adobe", PluginPlatform::Linux).is_none());
        assert_eq!(
            nightly_artifact_name_for_plugin_on_platform("adobe", PluginPlatform::Linux),
            None
        );
    }

    #[test]
    fn linux_resolve_detection_uses_standard_host_and_ofx_paths() {
        assert_eq!(
            nle_detection_paths_for_platform("openfx", PluginPlatform::Linux, "tester"),
            vec![
                PathBuf::from("/opt/resolve"),
                PathBuf::from("/usr/OFX/Plugins")
            ]
        );
        assert!(
            nle_detection_paths_for_platform("adobe", PluginPlatform::Linux, "tester").is_empty()
        );
    }

    #[test]
    fn linux_resolve_scripts_use_user_local_data_directory() {
        let home = Path::new("/home/tester");
        assert_eq!(
            resolve_scripts_dir_for_platform(PluginPlatform::Linux, Some(home), None).unwrap(),
            home.join(".local")
                .join("share")
                .join("DaVinciResolve")
                .join("Fusion")
                .join("Scripts")
                .join("Utility")
        );
    }

    #[test]
    fn linux_detect_requires_binary_and_explicit_version_file() {
        let root = tempfile::tempdir().unwrap();
        let bundle = write_linux_openfx_bundle(root.path(), " 2.1.2.34\n");
        assert_eq!(detect_linux_openfx_bundle(&bundle).unwrap(), "2.1.2.34");

        std::fs::remove_file(bundle.join("Contents").join("version.txt")).unwrap();
        assert_eq!(detect_linux_openfx_bundle(&bundle).unwrap(), "");
        std::fs::write(bundle.join("Contents").join("version.txt"), "2.1.2.34\n").unwrap();
        std::fs::remove_file(
            bundle
                .join("Contents")
                .join("Linux-x86-64")
                .join("GyroflowNiyien.ofx"),
        )
        .unwrap();
        assert_eq!(detect_linux_openfx_bundle(&bundle).unwrap(), "");
    }

    #[test]
    fn linux_openfx_source_is_canonical_and_complete_before_copy() {
        let root = tempfile::tempdir().unwrap();
        let bundle = write_linux_openfx_bundle(root.path(), "2.1.2.34\n");

        let validated = validate_linux_openfx_source(root.path()).unwrap();

        assert!(validated.is_absolute());
        assert_eq!(validated, bundle.canonicalize().unwrap());
    }

    #[test]
    fn linux_openfx_direct_copy_installs_complete_bundle() {
        let source_root = tempfile::tempdir().unwrap();
        let destination_root = tempfile::tempdir().unwrap();
        let source = write_linux_openfx_bundle(source_root.path(), "2.1.2.34\n")
            .canonicalize()
            .unwrap();

        copy_linux_openfx_bundle_direct(&source, destination_root.path()).unwrap();

        let installed = destination_root.path().join("GyroflowNiyien.ofx.bundle");
        assert_eq!(detect_linux_openfx_bundle(&installed).unwrap(), "2.1.2.34");
    }

    #[test]
    fn linux_privileged_copy_uses_shell_free_fixed_pkexec_arguments() {
        let root = tempfile::tempdir().unwrap();
        let source = write_linux_openfx_bundle(root.path(), "2.1.2.34\n")
            .canonicalize()
            .unwrap();
        let mut calls = Vec::new();

        run_linux_privileged_copy_with(&source, |program, args| {
            calls.push((program.to_owned(), args.to_vec()));
            Ok(true)
        })
        .unwrap();

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "pkexec");
        assert_eq!(
            calls[0].1,
            vec![
                std::ffi::OsString::from("/bin/cp"),
                std::ffi::OsString::from("-a"),
                std::ffi::OsString::from("--"),
                source.into_os_string(),
                std::ffi::OsString::from("/usr/OFX/Plugins/"),
            ]
        );
        assert!(!calls[0].1.iter().any(|arg| arg == "sh" || arg == "-c"));
    }

    #[test]
    fn linux_privileged_copy_failure_returns_manual_fallback_with_fixed_destination() {
        let root = tempfile::tempdir().unwrap();
        let source = write_linux_openfx_bundle(root.path(), "2.1.2.34\n")
            .canonicalize()
            .unwrap();

        let error = run_linux_privileged_copy_with(&source, |_program, _args| {
            Err(io::Error::new(io::ErrorKind::NotFound, "pkexec missing"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(error
            .to_string()
            .starts_with(LINUX_PLUGIN_MANUAL_INSTALL_REQUIRED));
        assert!(error
            .to_string()
            .contains(&source.to_string_lossy().into_owned()));
        assert!(error.to_string().contains("/usr/OFX/Plugins/"));
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
