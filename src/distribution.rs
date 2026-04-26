// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct Manifest {
    #[serde(default)]
    pub app: AppRelease,
    #[serde(default)]
    pub lens: DataPackageRelease,
    #[serde(default)]
    pub sdk_base: String,
    #[serde(default)]
    pub plugins_base: String,
    #[serde(default)]
    pub plugins_source_mode: String,
    #[serde(default)]
    pub plugins_source_ref: String,
    #[serde(default)]
    pub plugins_source_tag: String,
    #[serde(default)]
    pub country: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub country_source: String,
    #[serde(default)]
    pub selected_source: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct AppRelease {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub changelog: String,
    #[serde(default)]
    pub manual_versions: Vec<ManualAppVersion>,
    #[serde(default)]
    pub packages: BTreeMap<String, AppPackageRelease>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct AppPackageRelease {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub installer_url: String,
    #[serde(default)]
    pub installer_sha256: String,
    #[serde(default)]
    pub installer_size: u64,
    #[serde(default)]
    pub package_url: String,
    #[serde(default)]
    pub package_sha256: String,
    #[serde(default)]
    pub package_size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct AppUpdateSelection {
    pub version: String,
    pub platform: String,
    pub kind: String,
    pub download_url: String,
    pub download_sha256: String,
    pub download_size: u64,
    pub package_url: String,
    pub package_sha256: String,
    pub package_size: u64,
}

#[derive(Clone, Debug, Default)]
pub struct PreparedAppUpdate {
    pub selection: AppUpdateSelection,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct ManualAppVersion {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub changelog: String,
    #[serde(default)]
    pub recommended: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct DataPackageRelease {
    #[serde(default)]
    pub version: u64,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
struct DataBundle {
    #[serde(rename = "__version")]
    version: u64,
    #[serde(rename = "__package")]
    package: String,
    files: BTreeMap<String, Vec<u8>>,
}

#[derive(Clone)]
struct CachedManifest {
    fetched_at: Instant,
    manifest: Manifest,
}

#[derive(Clone, Debug, Serialize)]
struct TelemetryEvent<'a> {
    anon_id: &'a str,
    source_app_id: &'a str,
    product_id: &'a str,
    event: &'a str,
    app_version: &'a str,
    platform: &'a str,
    arch: &'a str,
    artifact_type: &'a str,
    artifact_version: &'a str,
    selected_source: &'a str,
    status: &'a str,
    duration_ms: u128,
    bytes: u64,
    error_code: &'a str,
}

pub struct DataSyncResult {
    pub package: &'static str,
    pub updated: bool,
}

lazy_static::lazy_static! {
    static ref MANIFEST_CACHE: RwLock<Option<CachedManifest>> = RwLock::new(None);
    // Single-flight lock: at startup multiple modules concurrently call
    // fetch_manifest before any thread has populated the cache, which
    // used to fan out into 4-5 parallel HTTP fetches. This Mutex
    // serializes the actual fetch path; threads waiting on it then hit
    // the freshly-populated cache via the second cache check below.
    static ref FETCH_LOCK: Mutex<()> = Mutex::new(());
}

pub fn fetch_manifest(force: bool) -> Result<Manifest, String> {
    const TTL: Duration = Duration::from_secs(300);
    if !force {
        if let Some(entry) = MANIFEST_CACHE.read().clone() {
            if entry.fetched_at.elapsed() < TTL {
                return Ok(entry.manifest);
            }
        }
    }

    // Serialize the fetch path. Re-check cache after acquiring the lock
    // — if another thread fetched while we were waiting, just reuse it.
    let _fetch_guard = FETCH_LOCK.lock();
    if !force {
        if let Some(entry) = MANIFEST_CACHE.read().clone() {
            if entry.fetched_at.elapsed() < TTL {
                return Ok(entry.manifest);
            }
        }
    }

    let mut url = url::Url::parse(gyroflow_core::distribution::manifest_api())
        .map_err(|err| format!("invalid manifest url: {err}"))?;
    url.query_pairs_mut()
        .append_pair("platform", platform_name())
        .append_pair("arch", std::env::consts::ARCH)
        .append_pair("app_version", env!("CARGO_PKG_VERSION"));

    let started = Instant::now();
    let body = configure_geo_request(ureq::get(url.as_str()))
        .call()
        .map_err(|err| format!("fetch manifest failed: {err}"))?
        .into_body()
        .read_to_string()
        .map_err(|err| format!("read manifest failed: {err}"))?;
    let manifest: Manifest =
        serde_json::from_str(&body).map_err(|err| format!("parse manifest failed: {err}"))?;
    log::info!("Distribution manifest URL: {}", url);
    match serde_json::to_string_pretty(&manifest) {
        Ok(pretty) => log::info!("Distribution manifest payload:\n{}", pretty),
        Err(err) => log::warn!("Serialize manifest for logging failed: {}", err),
    }
    log::info!(
        "Distribution geo context: country={}, region={}, city={}, country_source={}, selected_source={}, disable_proxy={}, http_proxy={}, https_proxy={}, all_proxy={}",
        manifest.country,
        manifest.region,
        manifest.city,
        manifest.country_source,
        manifest.selected_source,
        disable_proxy_enabled(),
        env_value_for_log("HTTP_PROXY"),
        env_value_for_log("HTTPS_PROXY"),
        env_value_for_log("ALL_PROXY"),
    );

    apply_manifest_sources(&manifest);
    let source_label = manifest_source_label(&manifest);
    report_download_event(
        "manifest_fetch",
        "manifest",
        manifest.app.version.as_str(),
        &source_label,
        "success",
        started.elapsed().as_millis(),
        body.len() as u64,
        "",
    );

    *MANIFEST_CACHE.write() = Some(CachedManifest {
        fetched_at: Instant::now(),
        manifest: manifest.clone(),
    });
    Ok(manifest)
}

pub fn sync_data_packages(manifest: &Manifest) -> Result<Vec<DataSyncResult>, String> {
    let mut results = Vec::new();
    results.push(sync_package("lens", &manifest.lens)?);
    Ok(results)
}

fn sync_package(
    package_name: &'static str,
    release: &DataPackageRelease,
) -> Result<DataSyncResult, String> {
    if release.version == 0 || release.url.is_empty() {
        return Ok(DataSyncResult {
            package: package_name,
            updated: false,
        });
    }

    let installed = gyroflow_core::distribution::installed_package_version(package_name);
    let package_dir = gyroflow_core::distribution::current_package_dir(package_name);
    if installed >= release.version && package_dir.is_some() {
        return Ok(DataSyncResult {
            package: package_name,
            updated: false,
        });
    }

    let started = Instant::now();
    let result = (|| -> Result<usize, String> {
        let response = configure_geo_request(ureq::get(&release.url))
            .call()
            .map_err(|err| format!("download {package_name} failed: {err}"))?;
        let mut reader = response.into_body().into_reader();
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .map_err(|err| format!("read {package_name} failed: {err}"))?;

        verify_sha256(package_name, &bytes, &release.sha256)?;
        install_package(package_name, &bytes, release.version)?;
        Ok(bytes.len())
    })();

    match result {
        Ok(size) => {
            report_download_event(
                "download_result",
                package_name,
                &release.version.to_string(),
                &release.url,
                "success",
                started.elapsed().as_millis(),
                size as u64,
                "",
            );
            Ok(DataSyncResult {
                package: package_name,
                updated: true,
            })
        }
        Err(err) => {
            report_download_event(
                "download_result",
                package_name,
                &release.version.to_string(),
                &release.url,
                "fail",
                started.elapsed().as_millis(),
                0,
                &err,
            );
            Err(err)
        }
    }
}

fn verify_sha256(package_name: &str, bytes: &[u8], expected: &str) -> Result<(), String> {
    if expected.trim().is_empty() {
        return Ok(());
    }
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let actual = hasher.finalize();
    let actual_hex = actual
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if actual_hex.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(format!(
            "{package_name} sha256 mismatch, expected {}, got {}",
            expected, actual_hex
        ))
    }
}

fn install_package(package_name: &str, bytes: &[u8], expected_version: u64) -> Result<(), String> {
    let versions_root = gyroflow_core::distribution::package_versions_root(package_name)
        .ok_or_else(|| format!("unknown package {package_name}"))?;
    let target_dir = versions_root.join(expected_version.to_string());
    if target_dir.is_dir() {
        gyroflow_core::distribution::set_installed_package_version(package_name, expected_version);
        return Ok(());
    }

    let bundle =
        decode_bundle(bytes).map_err(|err| format!("decode {package_name} failed: {err}"))?;
    if bundle.version != expected_version {
        log::warn!(
            "Distribution package version mismatch for {}: manifest={}, bundle={}",
            package_name,
            expected_version,
            bundle.version
        );
    }
    if bundle.package != package_name {
        log::warn!(
            "Distribution package name mismatch for {}: bundle={}",
            package_name,
            bundle.package
        );
    }

    if let Some(parent) = target_dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|err| format!("create {} root failed: {err}", package_name))?;
    }
    let staging = versions_root.join(format!("{}.tmp-{}", expected_version, std::process::id()));
    if staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    std::fs::create_dir_all(&staging)
        .map_err(|err| format!("create staging {} failed: {err}", package_name))?;

    for (relative_path, content) in bundle.files {
        let final_path = staging.join(&relative_path);
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!(
                    "create parent for {} failed ({}): {err}",
                    package_name, relative_path
                )
            })?;
        }
        std::fs::write(&final_path, content).map_err(|err| {
            format!(
                "write bundled file failed for {} ({}): {err}",
                package_name, relative_path
            )
        })?;
    }

    if target_dir.exists() {
        let _ = std::fs::remove_dir_all(&target_dir);
    }
    std::fs::rename(&staging, &target_dir)
        .map_err(|err| format!("activate {} failed: {err}", package_name))?;
    gyroflow_core::distribution::set_installed_package_version(package_name, expected_version);
    Ok(())
}

fn decode_bundle(bytes: &[u8]) -> Result<DataBundle, String> {
    let decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    ciborium::from_reader(decoder).map_err(|err| err.to_string())
}

fn telemetry_anon_id() -> String {
    let existing = gyroflow_core::settings::get_str("telemetryAnonId", "");
    if !existing.trim().is_empty() {
        return existing;
    }

    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let generated = format!(
        "gfniyien-{now_ms:012x}-{:016x}{:016x}",
        fastrand::u64(..),
        fastrand::u64(..)
    );
    gyroflow_core::settings::set("telemetryAnonId", generated.clone().into());
    generated
}

pub fn report_download_event(
    event: &str,
    artifact_type: &str,
    artifact_version: &str,
    selected_source: &str,
    status: &str,
    duration_ms: u128,
    bytes: u64,
    error_code: &str,
) {
    let endpoint = gyroflow_core::distribution::telemetry_api().to_owned();
    if endpoint.is_empty() {
        return;
    }
    let anon_id = telemetry_anon_id();

    let payload = TelemetryEvent {
        anon_id: &anon_id,
        source_app_id: "gyroflow_niyien",
        product_id: "gyroflow_niyien",
        event,
        app_version: env!("CARGO_PKG_VERSION"),
        platform: platform_name(),
        arch: std::env::consts::ARCH,
        artifact_type,
        artifact_version,
        selected_source,
        status,
        duration_ms,
        bytes,
        error_code,
    };
    let body = match serde_json::to_string(&payload) {
        Ok(body) => body,
        Err(err) => {
            log::warn!("Serialize telemetry payload failed: {}", err);
            return;
        }
    };

    crate::core::run_threaded(move || {
        if let Err(err) = configure_geo_request(ureq::post(&endpoint))
            .header("Content-Type", "application/json")
            .send(body.as_str())
        {
            log::debug!("Telemetry submit failed: {}", err);
        }
    });
}

fn configure_geo_request<T>(request: ureq::RequestBuilder<T>) -> ureq::RequestBuilder<T> {
    let mut request = request;
    if disable_proxy_enabled() {
        request = request.config().proxy(None).build();
    }
    if geo_debug_enabled() {
        request = request.header("x-telemetry-debug", "1");
    }
    if geo_bypass_cache_enabled() {
        request = request.header("x-geo-bypass-cache", "1");
    }
    request
}

fn geo_debug_enabled() -> bool {
    env_flag("NIYIEN_GEO_DEBUG") || env_flag("NIYIEN_TELEMETRY_DEBUG_GEO")
}

fn geo_bypass_cache_enabled() -> bool {
    env_flag("NIYIEN_GEO_BYPASS_CACHE")
}

fn disable_proxy_enabled() -> bool {
    env_flag("NIYIEN_DISABLE_PROXY")
}

fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn env_value_for_log(name: &str) -> &'static str {
    if std::env::var_os(name).is_some() {
        "set"
    } else {
        "empty"
    }
}

fn apply_manifest_sources(manifest: &Manifest) {
    if !manifest.sdk_base.is_empty() {
        gyroflow_core::settings::set("sdkBase", manifest.sdk_base.clone().into());
    }
    if !manifest.plugins_base.is_empty() {
        gyroflow_core::settings::set("pluginsBase", manifest.plugins_base.clone().into());
    }
    gyroflow_core::settings::set(
        "pluginsSourceMode",
        manifest.plugins_source_mode.trim().to_owned().into(),
    );
    gyroflow_core::settings::set(
        "pluginsSourceRef",
        manifest.plugins_source_ref.trim().to_owned().into(),
    );
    gyroflow_core::settings::set(
        "pluginsSourceTag",
        manifest.plugins_source_tag.trim().to_owned().into(),
    );
    if !manifest.country.is_empty() {
        gyroflow_core::settings::set("distributionCountry", manifest.country.clone().into());
    }
    if !manifest.region.is_empty() {
        gyroflow_core::settings::set("distributionRegion", manifest.region.clone().into());
    }
}

fn manifest_source_label(manifest: &Manifest) -> String {
    if !manifest.region.is_empty() {
        manifest.region.clone()
    } else if !manifest.country.is_empty() {
        manifest.country.clone()
    } else {
        "manifest".to_owned()
    }
}

pub fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "android") {
        "android"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        std::env::consts::OS
    }
}

pub fn has_app_update(manifest: &Manifest) -> bool {
    let latest = manifest.app.version.trim();
    if latest.is_empty() {
        return false;
    }
    let current_canonical = crate::util::get_canonical_version().trim();
    if latest == current_canonical
        || latest == env!("CARGO_PKG_VERSION")
        || latest == crate::util::get_version()
    {
        return false;
    }
    match (
        semver::Version::parse(latest.trim_start_matches('v')),
        semver::Version::parse(current_canonical.trim_start_matches('v')),
    ) {
        (Ok(latest), Ok(current)) => latest > current,
        _ => true,
    }
}

pub fn app_update_package_for_platform(
    manifest: &Manifest,
    platform: &str,
) -> Option<AppUpdateSelection> {
    let platform = normalize_app_update_platform(platform);
    let package = manifest.app.packages.get(platform);
    let fallback_url = manifest.app.url.trim();

    let selection = match (platform, package) {
        ("windows", Some(package)) => AppUpdateSelection {
            version: manifest.app.version.clone(),
            platform: platform.to_owned(),
            kind: if package.kind.trim().is_empty() {
                "web_installer_zip".to_owned()
            } else {
                package.kind.trim().to_owned()
            },
            download_url: package.installer_url.trim().to_owned(),
            download_sha256: package.installer_sha256.trim().to_owned(),
            download_size: package.installer_size,
            package_url: package.package_url.trim().to_owned(),
            package_sha256: package.package_sha256.trim().to_owned(),
            package_size: package.package_size,
        },
        (_, Some(package)) => AppUpdateSelection {
            version: manifest.app.version.clone(),
            platform: platform.to_owned(),
            kind: if package.kind.trim().is_empty() {
                "dmg".to_owned()
            } else {
                package.kind.trim().to_owned()
            },
            download_url: package.package_url.trim().to_owned(),
            download_sha256: package.package_sha256.trim().to_owned(),
            download_size: package.package_size,
            package_url: package.package_url.trim().to_owned(),
            package_sha256: package.package_sha256.trim().to_owned(),
            package_size: package.package_size,
        },
        _ if !fallback_url.is_empty() => AppUpdateSelection {
            version: manifest.app.version.clone(),
            platform: platform.to_owned(),
            kind: if platform == "windows" {
                "web_installer_zip".to_owned()
            } else {
                "dmg".to_owned()
            },
            download_url: fallback_url.to_owned(),
            ..Default::default()
        },
        _ => return None,
    };

    if selection.download_url.trim().is_empty() {
        None
    } else {
        Some(selection)
    }
}

pub fn current_platform_app_update_package(manifest: &Manifest) -> Option<AppUpdateSelection> {
    app_update_package_for_platform(manifest, platform_name())
}

pub fn download_app_update<F>(
    selection: &AppUpdateSelection,
    mut progress: F,
) -> Result<PreparedAppUpdate, String>
where
    F: FnMut(u64, u64, &str),
{
    if selection.download_url.trim().is_empty() {
        return Err("update package url is empty".to_owned());
    }
    let response = configure_geo_request(ureq::get(selection.download_url.as_str()))
        .call()
        .map_err(|err| format!("download update failed: {err}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(selection.download_size);

    let cache_dir = app_update_cache_dir()?;
    std::fs::create_dir_all(&cache_dir)
        .map_err(|err| format!("create update cache dir failed: {err}"))?;
    let path = cache_dir.join(app_update_filename(selection));
    let temp_path = path.with_extension("download");
    let mut reader = response.into_body().into_reader();
    let mut output = std::fs::File::create(&temp_path)
        .map_err(|err| format!("create update temp file failed: {err}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("read update download failed: {err}"))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|err| format!("write update download failed: {err}"))?;
        hasher.update(&buffer[..read]);
        downloaded += read as u64;
        progress(downloaded, total, "downloading");
    }
    output
        .flush()
        .map_err(|err| format!("flush update download failed: {err}"))?;

    verify_sha256_hex(
        "app update",
        &hex_digest(hasher.finalize().as_slice()),
        &selection.download_sha256,
    )?;
    std::fs::rename(&temp_path, &path)
        .map_err(|err| format!("activate update download failed: {err}"))?;
    progress(downloaded, total, "ready");
    Ok(PreparedAppUpdate {
        selection: selection.clone(),
        path,
    })
}

pub fn open_downloaded_update(prepared: &PreparedAppUpdate) -> Result<(), String> {
    if prepared.selection.platform == "macos" {
        return open_macos_update(&prepared.path);
    }
    if prepared.selection.platform == "windows" {
        return launch_windows_update(prepared);
    }
    Err(format!(
        "app update handoff is not supported on {}",
        prepared.selection.platform
    ))
}

pub fn windows_setup_update_args(
    selection: &AppUpdateSelection,
    install_dir: &Path,
    wait_pid: Option<String>,
    wait_start: Option<String>,
    wait_handle: Option<String>,
) -> Vec<String> {
    let mut args = vec![
        "/UPDATE=1".to_owned(),
        format!("/DIR={}", install_dir.display()),
        format!("/PACKAGEURL={}", selection.package_url),
        format!("/PACKAGESHA256={}", selection.package_sha256),
        format!("/PACKAGESIZE={}", selection.package_size),
        "/LAUNCH=1".to_owned(),
    ];
    if let Some(handle) = wait_handle.filter(|value| !value.trim().is_empty()) {
        args.push(format!("/WAITHANDLE={}", handle));
    }
    if let (Some(pid), Some(start)) = (
        wait_pid.filter(|value| !value.trim().is_empty()),
        wait_start.filter(|value| !value.trim().is_empty()),
    ) {
        args.push(format!("/WAITPID={}", pid));
        args.push(format!("/WAITSTART={}", start));
    }
    args
}

fn normalize_app_update_platform(platform: &str) -> &'static str {
    match platform.trim().to_ascii_lowercase().as_str() {
        "macos" => "macos",
        "linux" => "linux",
        "android" => "android",
        _ => "windows",
    }
}

fn app_update_cache_dir() -> Result<PathBuf, String> {
    let mut dir = std::env::temp_dir();
    dir.push("gyroflow-niyien");
    dir.push("updates");
    Ok(dir)
}

fn app_update_filename(selection: &AppUpdateSelection) -> String {
    url::Url::parse(selection.download_url.as_str())
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back().map(|value| value.to_owned()))
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| {
            if selection.platform == "windows" {
                "gyroflow-niyien-windows64-setup.exe".to_owned()
            } else {
                "gyroflow-niyien-mac-universal.dmg".to_owned()
            }
        })
}

fn verify_sha256_hex(label: &str, actual: &str, expected: &str) -> Result<(), String> {
    if expected.trim().is_empty() {
        return Ok(());
    }
    if actual.eq_ignore_ascii_case(expected.trim()) {
        Ok(())
    } else {
        Err(format!(
            "{label} sha256 mismatch, expected {}, got {}",
            expected.trim(),
            actual
        ))
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn open_macos_update(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("open")
            .arg(path)
            .status()
            .map_err(|err| format!("open dmg failed: {err}"))?;
        if status.success() {
            return Ok(());
        }
        return Err(format!("open dmg failed with status {status}"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = path;
        Err("macOS update handoff is only available on macOS".to_owned())
    }
}

fn launch_windows_update(prepared: &PreparedAppUpdate) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        launch_windows_update_impl(prepared)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = prepared;
        Err("Windows update handoff is only available on Windows".to_owned())
    }
}

#[cfg(target_os = "windows")]
fn launch_windows_update_impl(prepared: &PreparedAppUpdate) -> Result<(), String> {
    let install_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        .ok_or_else(|| "cannot determine current install dir".to_owned())?;
    let wait_pid = Some(std::process::id().to_string());
    let wait_start = current_process_creation_time_hex().ok();
    if let Err(err) = launch_windows_setup_with_inherited_handle(
        prepared,
        &install_dir,
        wait_pid.clone(),
        wait_start.clone(),
    ) {
        log::warn!(
            "launch update setup with inherited handle failed, falling back to pid wait: {err}"
        );
    } else {
        return Ok(());
    }
    let args = windows_setup_update_args(
        &prepared.selection,
        &install_dir,
        wait_pid,
        wait_start,
        None,
    );
    std::process::Command::new(&prepared.path)
        .args(args)
        .spawn()
        .map_err(|err| format!("launch update setup failed: {err}"))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn launch_windows_setup_with_inherited_handle(
    prepared: &PreparedAppUpdate,
    install_dir: &Path,
    wait_pid: Option<String>,
    wait_start: Option<String>,
) -> Result<(), String> {
    use std::ffi::OsStr;
    use std::mem::{size_of, zeroed};
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess,
        InitializeProcThreadAttributeList, UpdateProcThreadAttribute, EXTENDED_STARTUPINFO_PRESENT,
        PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_HANDLE_LIST, STARTUPINFOEXW,
    };

    unsafe {
        let current_process = GetCurrentProcess();
        let mut inherited_handle: HANDLE = null_mut();
        if DuplicateHandle(
            current_process,
            current_process,
            current_process,
            &mut inherited_handle,
            0,
            1,
            DUPLICATE_SAME_ACCESS,
        ) == 0
        {
            return Err("DuplicateHandle failed".to_owned());
        }

        let result = (|| -> Result<(), String> {
            let mut attribute_size = 0_usize;
            InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut attribute_size);
            if attribute_size == 0 {
                return Err("InitializeProcThreadAttributeList size query failed".to_owned());
            }
            let mut attribute_storage = vec![0_u8; attribute_size];
            let attribute_list = attribute_storage.as_mut_ptr() as _;
            if InitializeProcThreadAttributeList(attribute_list, 1, 0, &mut attribute_size) == 0 {
                return Err("InitializeProcThreadAttributeList failed".to_owned());
            }

            let mut handle_list = [inherited_handle];
            if UpdateProcThreadAttribute(
                attribute_list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                handle_list.as_mut_ptr().cast(),
                size_of::<HANDLE>(),
                null_mut(),
                null(),
            ) == 0
            {
                DeleteProcThreadAttributeList(attribute_list);
                return Err(
                    "UpdateProcThreadAttribute(PROC_THREAD_ATTRIBUTE_HANDLE_LIST) failed"
                        .to_owned(),
                );
            }

            let args = windows_setup_update_args(
                &prepared.selection,
                install_dir,
                wait_pid,
                wait_start,
                Some((inherited_handle as usize).to_string()),
            );
            let command_line = windows_command_line(&prepared.path, &args);
            let mut command_line_w = wide_null(OsStr::new(&command_line));
            let application_w = wide_null(prepared.path.as_os_str());
            let mut startup_info: STARTUPINFOEXW = zeroed();
            startup_info.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup_info.lpAttributeList = attribute_list;
            let mut process_info: PROCESS_INFORMATION = zeroed();
            let created = CreateProcessW(
                application_w.as_ptr(),
                command_line_w.as_mut_ptr(),
                null(),
                null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT,
                null(),
                null(),
                &startup_info.StartupInfo,
                &mut process_info,
            );
            DeleteProcThreadAttributeList(attribute_list);
            if created == 0 {
                return Err("CreateProcessW failed".to_owned());
            }
            CloseHandle(process_info.hThread);
            CloseHandle(process_info.hProcess);
            Ok(())
        })();

        CloseHandle(inherited_handle);
        result
    }
}

#[cfg(target_os = "windows")]
fn windows_command_line(exe: &Path, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(quote_windows_arg(&exe.display().to_string()));
    parts.extend(args.iter().map(|arg| quote_windows_arg(arg)));
    parts.join(" ")
}

#[cfg(target_os = "windows")]
fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|ch| ch.is_whitespace() || ch == '"') {
        return arg.to_owned();
    }
    let mut quoted = String::from("\"");
    let mut backslashes = 0;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            _ => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(ch);
            }
        }
    }
    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

#[cfg(target_os = "windows")]
fn wide_null(value: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    value.encode_wide().chain(std::iter::once(0)).collect()
}

#[cfg(target_os = "windows")]
fn current_process_creation_time_hex() -> Result<String, String> {
    use std::mem::MaybeUninit;
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};

    unsafe {
        let process = GetCurrentProcess();
        let mut creation = MaybeUninit::<FILETIME>::zeroed();
        let mut exit = MaybeUninit::<FILETIME>::zeroed();
        let mut kernel = MaybeUninit::<FILETIME>::zeroed();
        let mut user = MaybeUninit::<FILETIME>::zeroed();
        if GetProcessTimes(
            process,
            creation.as_mut_ptr(),
            exit.as_mut_ptr(),
            kernel.as_mut_ptr(),
            user.as_mut_ptr(),
        ) == 0
        {
            return Err("GetProcessTimes failed".to_owned());
        }
        let creation = creation.assume_init();
        let value = ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64;
        Ok(format!("{value:016x}"))
    }
}

pub fn fetch_manual_versions(force: bool) -> Result<Vec<ManualAppVersion>, String> {
    match fetch_manifest(force) {
        Ok(manifest) => Ok(manifest.app.manual_versions),
        Err(first_err) if force => fetch_manifest(false)
            .map(|manifest| manifest.app.manual_versions)
            .map_err(|_| first_err),
        Err(err) => Err(err),
    }
}

pub fn download_source_base() -> String {
    match fetch_manifest(false) {
        Ok(manifest) if !manifest.sdk_base.is_empty() => manifest.sdk_base,
        Ok(_) | Err(_) => gyroflow_core::settings::get_str("sdkBase", ""),
    }
}

pub fn plugin_source_base() -> String {
    match fetch_manifest(false) {
        Ok(manifest) if !manifest.plugins_base.is_empty() => manifest.plugins_base,
        Ok(_) | Err(_) => gyroflow_core::settings::get_str("pluginsBase", ""),
    }
}

pub fn plugin_source_mode() -> String {
    match fetch_manifest(false) {
        Ok(manifest) if !manifest.plugins_source_mode.is_empty() => manifest.plugins_source_mode,
        Ok(_) | Err(_) => gyroflow_core::settings::get_str("pluginsSourceMode", ""),
    }
}

pub fn plugin_source_ref() -> String {
    match fetch_manifest(false) {
        Ok(manifest) if !manifest.plugins_source_ref.is_empty() => manifest.plugins_source_ref,
        Ok(_) | Err(_) => gyroflow_core::settings::get_str("pluginsSourceRef", ""),
    }
}

pub fn plugin_source_tag() -> String {
    match fetch_manifest(false) {
        Ok(manifest) if !manifest.plugins_source_tag.is_empty() => manifest.plugins_source_tag,
        Ok(_) | Err(_) => gyroflow_core::settings::get_str("pluginsSourceTag", ""),
    }
}

#[cfg(test)]
mod app_update_tests {
    use super::*;

    #[test]
    fn manifest_deserializes_windows_setup_and_zip_packages() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9.9.9",
                    "url": "https://example.test/setup.exe",
                    "packages": {
                        "windows": {
                            "kind": "web_installer_zip",
                            "installer_url": "https://example.test/setup.exe",
                            "installer_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                            "installer_size": 12,
                            "package_url": "https://example.test/windows.zip",
                            "package_sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                            "package_size": 34
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let selected = app_update_package_for_platform(&manifest, "windows").unwrap();
        assert_eq!(selected.kind, "web_installer_zip");
        assert_eq!(selected.download_url, "https://example.test/setup.exe");
        assert_eq!(
            selected.download_sha256,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        assert_eq!(selected.download_size, 12);
        assert_eq!(selected.package_url, "https://example.test/windows.zip");
        assert_eq!(
            selected.package_sha256,
            "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
        );
        assert_eq!(selected.package_size, 34);
    }

    #[test]
    fn manifest_deserializes_macos_dmg_package() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9.9.9",
                    "url": "https://example.test/gyroflow.dmg",
                    "packages": {
                        "macos": {
                            "kind": "dmg",
                            "package_url": "https://example.test/gyroflow.dmg",
                            "package_sha256": "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                            "package_size": 56
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let selected = app_update_package_for_platform(&manifest, "macos").unwrap();
        assert_eq!(selected.kind, "dmg");
        assert_eq!(selected.download_url, "https://example.test/gyroflow.dmg");
        assert_eq!(
            selected.download_sha256,
            "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"
        );
        assert_eq!(selected.download_size, 56);
    }

    #[test]
    fn windows_setup_args_include_wait_target_and_package_metadata() {
        let selected = AppUpdateSelection {
            version: "9.9.9".to_owned(),
            platform: "windows".to_owned(),
            kind: "web_installer_zip".to_owned(),
            download_url: "https://example.test/setup.exe".to_owned(),
            download_sha256: "a".repeat(64),
            download_size: 12,
            package_url: "https://example.test/windows.zip".to_owned(),
            package_sha256: "b".repeat(64),
            package_size: 34,
        };
        let args = windows_setup_update_args(
            &selected,
            std::path::Path::new("C:/Gyroflow"),
            Some("42".to_owned()),
            Some("01db000000000000".to_owned()),
            Some("1234".to_owned()),
        );

        assert!(args.iter().any(|arg| arg == "/UPDATE=1"));
        assert!(args.iter().any(|arg| arg == "/LAUNCH=1"));
        assert!(args.iter().any(|arg| arg == "/WAITHANDLE=1234"));
        assert!(args.iter().any(|arg| arg == "/WAITPID=42"));
        assert!(args.iter().any(|arg| arg == "/WAITSTART=01db000000000000"));
        assert!(args.iter().any(|arg| arg == "/DIR=C:/Gyroflow"));
        assert!(args
            .iter()
            .any(|arg| arg == "/PACKAGEURL=https://example.test/windows.zip"));
        assert!(args
            .iter()
            .any(|arg| arg == &format!("/PACKAGESHA256={}", "b".repeat(64))));
        assert!(args.iter().any(|arg| arg == "/PACKAGESIZE=34"));
    }
}
