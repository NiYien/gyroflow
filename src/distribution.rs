// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
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
