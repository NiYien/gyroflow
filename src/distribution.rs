// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cmp::Ordering;
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
    pub package_path: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct AppUpdateCandidate {
    pub channel: String,
    pub version: String,
    pub changelog: String,
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
    #[serde(default)]
    pub packages: BTreeMap<String, AppPackageRelease>,
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

const LOCAL_COUNTRY_CACHE_TTL_MS: u64 = 60 * 60 * 1000;
const LOCAL_COUNTRY_FAILURE_TTL_MS: u64 = 5 * 60 * 1000;
const LOCAL_COUNTRY_CHECKED_AT_KEY: &str = "distributionCountryCheckedAt";
const LOCAL_COUNTRY_FAILED_AT_KEY: &str = "distributionCountryLookupFailedAt";

lazy_static::lazy_static! {
    static ref MANIFEST_CACHE: RwLock<Option<CachedManifest>> = RwLock::new(None);
    // Single-flight lock: at startup multiple modules concurrently call
    // fetch_manifest before any thread has populated the cache, which
    // used to fan out into 4-5 parallel HTTP fetches. This Mutex
    // serializes the actual fetch path; threads waiting on it then hit
    // the freshly-populated cache via the second cache check below.
    static ref FETCH_LOCK: Mutex<()> = Mutex::new(());
}

fn cached_manifest() -> Option<Manifest> {
    MANIFEST_CACHE.read().as_ref().map(|entry| entry.manifest.clone())
}

fn manifest_request_url(country_hint: Option<&str>) -> Result<url::Url, String> {
    let mut url = url::Url::parse(gyroflow_core::distribution::manifest_api())
        .map_err(|err| format!("invalid manifest url: {err}"))?;
    let country = country_hint.and_then(normalize_cached_country_header);
    {
        let mut pairs = url.query_pairs_mut();
        pairs
            .append_pair("platform", platform_name())
            .append_pair("arch", std::env::consts::ARCH)
            .append_pair("app_version", env!("CARGO_PKG_VERSION"));
        if let Some(country) = country.as_deref() {
            pairs.append_pair("country", country);
        }
    }
    Ok(url)
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

    let local_country = local_country_hint();
    let url = manifest_request_url(local_country.as_deref())?;

    let started = Instant::now();
    let body = configure_geo_request(crate::network::get(url.as_str()))
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
        "Distribution geo context: country={}, region={}, city={}, country_source={}, selected_source={}, proxy=disabled, http_proxy={}, https_proxy={}, all_proxy={}",
        manifest.country,
        manifest.region,
        manifest.city,
        manifest.country_source,
        manifest.selected_source,
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
        let response = configure_geo_request(crate::network::get(&release.url))
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
        if let Err(err) = configure_geo_request(crate::network::post(&endpoint))
            .header("Content-Type", "application/json")
            .send(body.as_str())
        {
            log::debug!("Telemetry submit failed: {}", err);
        }
    });
}

fn configure_geo_request<T>(request: ureq::RequestBuilder<T>) -> ureq::RequestBuilder<T> {
    let mut request = crate::network::configure(request);
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

fn local_country_hint() -> Option<String> {
    let cached_country = cached_local_country();
    let now_ms = now_millis();
    if cached_country.is_some()
        && timestamp_is_fresh(
            now_ms,
            gyroflow_core::settings::get_u64(LOCAL_COUNTRY_CHECKED_AT_KEY, 0),
            LOCAL_COUNTRY_CACHE_TTL_MS,
        )
    {
        return cached_country;
    }
    if timestamp_is_fresh(
        now_ms,
        gyroflow_core::settings::get_u64(LOCAL_COUNTRY_FAILED_AT_KEY, 0),
        LOCAL_COUNTRY_FAILURE_TTL_MS,
    ) {
        return cached_country;
    }

    let ipinfo_country = lookup_ipinfo_country();
    if ipinfo_country.is_none() {
        gyroflow_core::settings::set(LOCAL_COUNTRY_FAILED_AT_KEY, now_ms.into());
    }
    select_local_country_hint(ipinfo_country.as_deref(), cached_country.as_deref())
}

fn select_local_country_hint(
    ipinfo_country: Option<&str>,
    cached_country: Option<&str>,
) -> Option<String> {
    ipinfo_country
        .and_then(normalize_cached_country_header)
        .or_else(|| cached_country.and_then(normalize_cached_country_header))
}

fn cached_local_country() -> Option<String> {
    let value = gyroflow_core::settings::get_str("distributionCountry", "");
    normalize_cached_country_header(&value)
}

fn lookup_ipinfo_country() -> Option<String> {
    let body = crate::network::get("https://ipinfo.io/json")
        .config()
        .timeout_global(Some(Duration::from_secs(3)))
        .build()
        .call()
        .ok()?
        .into_body()
        .read_to_string()
        .ok()?;
    let country = country_from_ipinfo_body(&body)?;
    gyroflow_core::settings::set("distributionCountry", country.clone().into());
    gyroflow_core::settings::set(LOCAL_COUNTRY_CHECKED_AT_KEY, now_millis().into());
    gyroflow_core::settings::set(LOCAL_COUNTRY_FAILED_AT_KEY, 0.into());
    Some(country)
}

fn country_from_ipinfo_body(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    value
        .get("country")
        .and_then(|value| value.as_str())
        .and_then(normalize_cached_country_header)
}

fn timestamp_is_fresh(now_ms: u64, checked_at_ms: u64, ttl_ms: u64) -> bool {
    checked_at_ms > 0 && now_ms.saturating_sub(checked_at_ms) < ttl_ms
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn normalize_cached_country_header(value: &str) -> Option<String> {
    let value = value.trim();
    if value.len() == 2 && value.chars().all(|c| c.is_ascii_alphabetic()) {
        Some(value.to_ascii_uppercase())
    } else {
        None
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
    app_version_is_newer_than_current(&manifest.app.version)
}

pub fn app_update_candidates(manifest: &Manifest) -> Vec<AppUpdateCandidate> {
    let mut candidates = Vec::new();
    if has_app_update(manifest) {
        candidates.push(AppUpdateCandidate {
            channel: "auto".to_owned(),
            version: manifest.app.version.trim().to_owned(),
            changelog: manifest.app.changelog.clone(),
        });
    }
    if let Some(manual) = latest_manual_app_update(manifest)
        .filter(|manual| !app_versions_equivalent(&manual.version, &manifest.app.version))
    {
        candidates.push(AppUpdateCandidate {
            channel: "manual".to_owned(),
            version: manual.version.trim().to_owned(),
            changelog: manual.changelog.clone(),
        });
    }
    candidates
}

pub fn latest_manual_app_update(manifest: &Manifest) -> Option<&ManualAppVersion> {
    manifest
        .app
        .manual_versions
        .iter()
        .filter(|version| app_version_is_newer_than_current(&version.version))
        .max_by(|a, b| compare_app_versions(&a.version, &b.version))
}

pub fn app_version_is_newer_than_current(version: &str) -> bool {
    let latest = version.trim();
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
    app_version_is_newer_than(latest, current_canonical)
}

fn app_version_is_newer_than(version: &str, current: &str) -> bool {
    let version = version.trim();
    let current = current.trim();
    if version.is_empty() || version == current {
        return false;
    }
    match (parse_app_version(version), parse_app_version(current)) {
        (Some(version), Some(current)) => version > current,
        _ => false,
    }
}

fn app_versions_equivalent(a: &str, b: &str) -> bool {
    let a = a.trim();
    let b = b.trim();
    if a == b {
        return true;
    }
    match (parse_app_version(a), parse_app_version(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

fn compare_app_versions(a: &str, b: &str) -> Ordering {
    match (parse_app_version(a), parse_app_version(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => a.trim().cmp(b.trim()),
    }
}

fn parse_app_version(version: &str) -> Option<semver::Version> {
    semver::Version::parse(version.trim().trim_start_matches('v')).ok()
}

pub fn app_update_package_for_platform(
    manifest: &Manifest,
    platform: &str,
) -> Option<AppUpdateSelection> {
    let platform = normalize_app_update_platform(platform);
    app_update_selection_from_package(
        &manifest.app.version,
        platform,
        manifest.app.url.trim(),
        manifest.app.packages.get(platform),
    )
}

pub fn app_update_package_for_requested_version(
    manifest: &Manifest,
    requested_version: Option<&str>,
    platform: &str,
) -> Option<AppUpdateSelection> {
    match requested_version.map(str::trim).filter(|version| !version.is_empty()) {
        Some(version) if version != manifest.app.version.trim() => {
            manual_app_update_package_for_platform(manifest, version, platform)
        }
        _ => app_update_package_for_platform(manifest, platform),
    }
}

pub fn manual_app_update_package_for_platform(
    manifest: &Manifest,
    version: &str,
    platform: &str,
) -> Option<AppUpdateSelection> {
    let version = version.trim();
    if version.is_empty() {
        return current_platform_app_update_package(manifest);
    }
    let platform = normalize_app_update_platform(platform);
    let manual = manifest
        .app
        .manual_versions
        .iter()
        .find(|item| item.version.trim() == version)?;
    app_update_selection_from_package(
        &manual.version,
        platform,
        manual.url.trim(),
        manual.packages.get(platform),
    )
}

fn app_update_selection_from_package(
    version: &str,
    platform: &'static str,
    fallback_url: &str,
    package: Option<&AppPackageRelease>,
) -> Option<AppUpdateSelection> {
    let selection = match (platform, package) {
        ("windows", Some(package)) => AppUpdateSelection {
            version: version.to_owned(),
            platform: platform.to_owned(),
            kind: if package.kind.trim().is_empty() {
                "web_installer_zip".to_owned()
            } else {
                package.kind.trim().to_owned()
            },
            download_url: first_non_empty(package.installer_url.trim(), fallback_url).to_owned(),
            download_sha256: package.installer_sha256.trim().to_owned(),
            download_size: package.installer_size,
            package_url: package.package_url.trim().to_owned(),
            package_sha256: package.package_sha256.trim().to_owned(),
            package_size: package.package_size,
        },
        (_, Some(package)) => AppUpdateSelection {
            version: version.to_owned(),
            platform: platform.to_owned(),
            kind: if package.kind.trim().is_empty() {
                "dmg".to_owned()
            } else {
                package.kind.trim().to_owned()
            },
            download_url: first_non_empty(package.package_url.trim(), fallback_url).to_owned(),
            download_sha256: package.package_sha256.trim().to_owned(),
            download_size: package.package_size,
            package_url: package.package_url.trim().to_owned(),
            package_sha256: package.package_sha256.trim().to_owned(),
            package_size: package.package_size,
        },
        _ if !fallback_url.is_empty() => AppUpdateSelection {
            version: version.to_owned(),
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

fn first_non_empty<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.trim().is_empty() {
        fallback.trim()
    } else {
        value.trim()
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
    let cache_dir = app_update_cache_dir()?;
    std::fs::create_dir_all(&cache_dir)
        .map_err(|err| format!("create update cache dir failed: {err}"))?;
    let path = download_or_reuse_update_file(
        "app update",
        &selection.download_url,
        &selection.download_sha256,
        selection.download_size,
        cache_dir.join(app_update_filename(selection)),
        &mut progress,
        "downloading",
    )?;
    let package_path =
        if selection.platform == "windows" && !selection.package_url.trim().is_empty() {
            Some(download_or_reuse_update_file(
                "app update package",
                &selection.package_url,
                &selection.package_sha256,
                selection.package_size,
                cache_dir.join(app_update_filename_from_url(
                    &selection.package_url,
                    default_windows_package_filename(),
                )),
                &mut progress,
                "downloading_package",
            )?)
        } else {
            None
        };

    let ready_size = package_path
        .as_deref()
        .or(Some(path.as_path()))
        .and_then(|path| path.metadata().ok())
        .map(|metadata| metadata.len())
        .unwrap_or(selection.download_size);
    progress(ready_size, ready_size, "ready");
    Ok(PreparedAppUpdate {
        selection: selection.clone(),
        path,
        package_path,
    })
}

fn download_or_reuse_update_file<F>(
    label: &str,
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    path: PathBuf,
    progress: &mut F,
    progress_status: &str,
) -> Result<PathBuf, String>
where
    F: FnMut(u64, u64, &str),
{
    if let Some(cached_size) = cached_update_file_size_if_valid(label, &path, expected_sha256)? {
        let total = if expected_size > 0 {
            expected_size
        } else {
            cached_size
        };
        progress(cached_size, total, "cached");
        return Ok(path);
    }

    if is_wrapper_url(url) {
        return download_nightly_wrapped_update_file(
            label,
            url,
            expected_sha256,
            expected_size,
            path,
            progress,
            progress_status,
        );
    }

    let response = configure_geo_request(crate::network::get(url))
        .call()
        .map_err(|err| format!("download {label} failed: {err}"))?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(expected_size);

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
        progress(downloaded, total, progress_status);
    }
    output
        .flush()
        .map_err(|err| format!("flush update download failed: {err}"))?;
    drop(output);

    verify_sha256_hex(
        label,
        &hex_digest(hasher.finalize().as_slice()),
        expected_sha256,
    )?;
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|err| format!("replace cached update file failed: {err}"))?;
    }
    std::fs::rename(&temp_path, &path)
        .map_err(|err| format!("activate update download failed: {err}"))?;
    Ok(path)
}

// Wrapper-zip aware URL classifier. Returns true when the URL is expected
// to deliver a one-level zip wrapper around a single raw deliverable. Two
// shapes are recognized:
//   * nightly.link host: any URL there is a V4 short artifact wrapper
//     (preserves the original is_nightly_link_url behavior).
//   * CN release path: 123 disk auto-renames .exe/.apk uploads with a .bak
//     suffix, so the publish pipeline ships these wrapped under the
//     nightly-style short artifact name (see APP_FILE_TO_ARTIFACT_NAME in
//     `_scripts/publish_pan123_release.py`). The portable Windows zip
//     basename `gyroflow-niyien-windows64.zip` is intentionally absent from
//     this list so it stays on the plain-download path.
fn is_wrapper_url(url: &str) -> bool {
    let parsed = match url::Url::parse(url) {
        Ok(u) => u,
        Err(_) => return false,
    };
    if parsed
        .host_str()
        .map(|h| h.eq_ignore_ascii_case("nightly.link"))
        .unwrap_or(false)
    {
        return true;
    }
    let basename = parsed
        .path_segments()
        .and_then(|mut segments| segments.next_back())
        .unwrap_or("");
    matches!(
        basename,
        "gyroflow-niyien-win-setup.zip" | "gyroflow-niyien-android.zip"
    )
}

// nightly.link serves GitHub Actions artifacts as a one-level zip wrapper that
// contains exactly one file (per the nightly upload steps in
// `.github/workflows/release.yml`, which use `actions/upload-artifact@v4` with
// short artifact names + a single `path:` file). Download the wrapper to a
// temp file, extract the inner deliverable while computing SHA256 on the raw
// inner bytes, and rename to the target cache path.
fn download_nightly_wrapped_update_file<F>(
    label: &str,
    url: &str,
    expected_sha256: &str,
    expected_size: u64,
    path: PathBuf,
    progress: &mut F,
    progress_status: &str,
) -> Result<PathBuf, String>
where
    F: FnMut(u64, u64, &str),
{
    let response = configure_geo_request(crate::network::get(url))
        .call()
        .map_err(|err| format!("download {label} (nightly wrapper) failed: {err}"))?;
    let wrapper_total = response
        .headers()
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);

    let wrapper_path = path.with_extension("nightly-wrapper.zip");
    if wrapper_path.exists() {
        let _ = std::fs::remove_file(&wrapper_path);
    }
    let mut reader = response.into_body().into_reader();
    let mut wrapper_file = std::fs::File::create(&wrapper_path)
        .map_err(|err| format!("create nightly wrapper temp file failed: {err}"))?;
    let mut buffer = [0_u8; 128 * 1024];
    let mut wrapper_downloaded = 0_u64;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|err| format!("read nightly wrapper failed: {err}"))?;
        if read == 0 {
            break;
        }
        wrapper_file
            .write_all(&buffer[..read])
            .map_err(|err| format!("write nightly wrapper failed: {err}"))?;
        wrapper_downloaded += read as u64;
        let total = wrapper_total.max(wrapper_downloaded);
        progress(wrapper_downloaded, total, progress_status);
    }
    wrapper_file
        .flush()
        .map_err(|err| format!("flush nightly wrapper failed: {err}"))?;
    drop(wrapper_file);

    let extract_result = extract_nightly_inner_file(
        label,
        &wrapper_path,
        &path,
        expected_sha256,
        expected_size,
        &mut buffer,
        progress,
        progress_status,
    );
    let _ = std::fs::remove_file(&wrapper_path);
    extract_result?;
    Ok(path)
}

fn extract_nightly_inner_file<F>(
    label: &str,
    wrapper_path: &Path,
    target_path: &Path,
    expected_sha256: &str,
    expected_size: u64,
    buffer: &mut [u8],
    progress: &mut F,
    progress_status: &str,
) -> Result<(), String>
where
    F: FnMut(u64, u64, &str),
{
    let wrapper_handle = std::fs::File::open(wrapper_path)
        .map_err(|err| format!("open nightly wrapper failed: {err}"))?;
    let mut archive = zip::ZipArchive::new(wrapper_handle)
        .map_err(|err| format!("read nightly wrapper as zip ({label}) failed: {err}"))?;
    if archive.is_empty() {
        return Err(format!("nightly wrapper for {label} is empty"));
    }

    let target_basename = target_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_owned();
    let mut entry_index = 0_usize;
    for i in 0..archive.len() {
        if let Ok(entry) = archive.by_index(i) {
            if entry.is_file() {
                let name = entry.name();
                if name == target_basename || name.ends_with(&format!("/{target_basename}")) {
                    entry_index = i;
                    break;
                }
                entry_index = i;
            }
        }
    }
    let mut inner = archive
        .by_index(entry_index)
        .map_err(|err| format!("open nightly wrapper inner #{entry_index} ({label}): {err}"))?;
    let inner_size = inner.size();
    let total = if expected_size > 0 {
        expected_size
    } else {
        inner_size
    };

    let temp_path = target_path.with_extension("download");
    let mut output = std::fs::File::create(&temp_path)
        .map_err(|err| format!("create update temp file failed: {err}"))?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    loop {
        let read = inner
            .read(buffer)
            .map_err(|err| format!("read nightly inner failed: {err}"))?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|err| format!("write update download failed: {err}"))?;
        hasher.update(&buffer[..read]);
        downloaded += read as u64;
        progress(downloaded, total, progress_status);
    }
    output
        .flush()
        .map_err(|err| format!("flush update download failed: {err}"))?;
    drop(output);
    drop(inner);
    drop(archive);

    verify_sha256_hex(
        label,
        &hex_digest(hasher.finalize().as_slice()),
        expected_sha256,
    )?;
    if target_path.exists() {
        std::fs::remove_file(target_path)
            .map_err(|err| format!("replace cached update file failed: {err}"))?;
    }
    std::fs::rename(&temp_path, target_path)
        .map_err(|err| format!("activate update download failed: {err}"))?;
    Ok(())
}

fn cached_update_file_size_if_valid(
    label: &str,
    path: &Path,
    expected_sha256: &str,
) -> Result<Option<u64>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let (actual_sha256, size) =
        sha256_file_hex(path).map_err(|err| format!("read cached {label} failed: {err}"))?;
    if expected_sha256.trim().is_empty()
        || actual_sha256.eq_ignore_ascii_case(expected_sha256.trim())
    {
        Ok(Some(size))
    } else {
        log::warn!(
            "cached {label} sha256 mismatch, ignoring {}",
            path.display()
        );
        Ok(None)
    }
}

fn sha256_file_hex(path: &Path) -> Result<(String, u64), std::io::Error> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        size += read as u64;
    }
    Ok((hex_digest(hasher.finalize().as_slice()), size))
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
    package_file: Option<&Path>,
) -> Vec<String> {
    let mut args = vec![
        "/UPDATE=1".to_owned(),
        format!("/DIR={}", install_dir.display()),
        format!("/PACKAGESHA256={}", selection.package_sha256),
        format!("/PACKAGESIZE={}", selection.package_size),
        "/LAUNCH=1".to_owned(),
    ];
    if !selection.package_url.trim().is_empty() {
        args.push(format!("/PACKAGEURL={}", selection.package_url));
    }
    if let Some(package_file) = package_file {
        args.push(format!("/PACKAGEFILE={}", package_file.display()));
    }
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
    app_update_filename_from_url(
        &selection.download_url,
        default_app_update_filename(&selection.platform),
    )
}

fn app_update_filename_from_url(url: &str, fallback_filename: &str) -> String {
    // For wrapper zips (nightly.link or CN release short-name zip) the URL
    // basename is the V4 short artifact name (e.g.
    // `gyroflow-niyien-win-setup.zip`), which is *not* the extension of the
    // raw deliverable inside. Use the platform-specific fallback so the
    // cached file keeps the right extension (.exe / .apk / .dmg) for
    // launching the installer or mounting the dmg.
    if is_wrapper_url(url) {
        return fallback_filename.to_owned();
    }
    url::Url::parse(url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.next_back().map(|value| value.to_owned()))
        })
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| fallback_filename.to_owned())
}

fn default_app_update_filename(platform: &str) -> &'static str {
    if platform == "windows" {
        "gyroflow-niyien-windows64-setup.exe"
    } else if platform == "android" {
        "gyroflow-niyien.apk"
    } else {
        "gyroflow-niyien-mac-universal.dmg"
    }
}

fn default_windows_package_filename() -> &'static str {
    "gyroflow-niyien-windows64.zip"
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
        prepared.package_path.as_deref(),
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
        CloseHandle, DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE,
    };
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT,
        GetCurrentProcess, InitializeProcThreadAttributeList, PROC_THREAD_ATTRIBUTE_HANDLE_LIST,
        PROCESS_INFORMATION, STARTUPINFOEXW, UpdateProcThreadAttribute,
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
                prepared.package_path.as_deref(),
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
        Err(first_err) if force => cached_manifest()
            .map(|manifest| manifest.app.manual_versions)
            .ok_or(first_err),
        Err(err) => Err(err),
    }
}

pub fn fetch_app_update_candidates(force: bool) -> Result<Vec<AppUpdateCandidate>, String> {
    match fetch_manifest(force) {
        Ok(manifest) => Ok(app_update_candidates(&manifest)),
        Err(first_err) if force => cached_manifest()
            .map(|manifest| app_update_candidates(&manifest))
            .ok_or(first_err),
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
    use std::fs;

    fn sha256_hex_for_test(content: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content);
        hex_digest(hasher.finalize().as_slice())
    }

    #[test]
    fn cached_country_header_normalizes_two_letter_country_codes() {
        assert_eq!(normalize_cached_country_header(" cn "), Some("CN".to_owned()));
        assert_eq!(normalize_cached_country_header("us"), Some("US".to_owned()));
        assert_eq!(normalize_cached_country_header("USA"), None);
        assert_eq!(normalize_cached_country_header("1N"), None);
        assert_eq!(normalize_cached_country_header(""), None);
    }

    #[test]
    fn manifest_url_includes_normalized_local_country() {
        let url = manifest_request_url(Some(" cn ")).unwrap();
        let pairs: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("country"), Some(&"CN".to_owned()));
    }

    #[test]
    fn ipinfo_body_country_parses_only_two_letter_codes() {
        assert_eq!(
            country_from_ipinfo_body(r#"{"ip":"203.0.113.1","country":"cn"}"#),
            Some("CN".to_owned())
        );
        assert_eq!(
            country_from_ipinfo_body(r#"{"ip":"203.0.113.1","country":"USA"}"#),
            None
        );
        assert_eq!(country_from_ipinfo_body("not json"), None);
    }

    #[test]
    fn local_country_hint_prefers_fresh_ipinfo_over_cached_country() {
        assert_eq!(
            select_local_country_hint(Some("CN"), Some("US")),
            Some("CN".to_owned())
        );
        assert_eq!(
            select_local_country_hint(None, Some("us")),
            Some("US".to_owned())
        );
    }

    #[test]
    fn local_country_timestamp_freshness_uses_ttl_window() {
        assert!(timestamp_is_fresh(10_000, 9_000, 2_000));
        assert!(!timestamp_is_fresh(10_000, 7_000, 2_000));
        assert!(!timestamp_is_fresh(10_000, 0, 2_000));
    }

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
    fn download_app_update_reuses_cached_file_when_sha256_matches() {
        let content = b"cached update payload";
        let filename = format!(
            "gyroflow-app-update-cache-test-{}-{}.bin",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let selection = AppUpdateSelection {
            platform: "macos".to_owned(),
            download_url: format!("http://127.0.0.1:9/{filename}"),
            download_sha256: sha256_hex_for_test(content),
            download_size: content.len() as u64,
            ..Default::default()
        };
        let cache_dir = app_update_cache_dir().unwrap();
        fs::create_dir_all(&cache_dir).unwrap();
        let cached_path = cache_dir.join(app_update_filename(&selection));
        fs::write(&cached_path, content).unwrap();

        let mut progress_events = Vec::new();
        let prepared = download_app_update(&selection, |downloaded, total, status| {
            progress_events.push((downloaded, total, status.to_owned()));
        })
        .unwrap();

        assert_eq!(prepared.path, cached_path);
        assert_eq!(fs::read(&prepared.path).unwrap(), content);
        assert!(progress_events.iter().any(|(downloaded, total, status)| {
            *downloaded == content.len() as u64
                && *total == content.len() as u64
                && status == "ready"
        }));
        let _ = fs::remove_file(prepared.path);
    }

    #[test]
    fn download_app_update_reuses_cached_windows_package_file() {
        let setup_content = b"cached setup payload";
        let package_content = b"cached windows package payload";
        let id = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let selection = AppUpdateSelection {
            platform: "windows".to_owned(),
            download_url: format!("http://127.0.0.1:9/gyroflow-cache-test-{id}-setup.exe"),
            download_sha256: sha256_hex_for_test(setup_content),
            download_size: setup_content.len() as u64,
            package_url: format!("http://127.0.0.1:9/gyroflow-cache-test-{id}-windows.zip"),
            package_sha256: sha256_hex_for_test(package_content),
            package_size: package_content.len() as u64,
            ..Default::default()
        };
        let cache_dir = app_update_cache_dir().unwrap();
        fs::create_dir_all(&cache_dir).unwrap();
        let setup_path = cache_dir.join(app_update_filename_from_url(
            &selection.download_url,
            default_app_update_filename(&selection.platform),
        ));
        let package_path = cache_dir.join(app_update_filename_from_url(
            &selection.package_url,
            default_windows_package_filename(),
        ));
        fs::write(&setup_path, setup_content).unwrap();
        fs::write(&package_path, package_content).unwrap();

        let prepared = download_app_update(&selection, |_, _, _| {}).unwrap();

        assert_eq!(prepared.path, setup_path);
        assert_eq!(
            prepared.package_path.as_deref(),
            Some(package_path.as_path())
        );
        assert_eq!(fs::read(&prepared.path).unwrap(), setup_content);
        assert_eq!(
            fs::read(prepared.package_path.as_ref().unwrap()).unwrap(),
            package_content
        );
        let _ = fs::remove_file(prepared.path);
        if let Some(package_path) = prepared.package_path {
            let _ = fs::remove_file(package_path);
        }
    }

    #[test]
    fn manual_windows_version_selects_its_own_setup_and_zip_package() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9.9.9",
                    "manual_versions": [
                        {
                            "version": "9.9.8-beta",
                            "url": "https://example.test/run-42/setup.exe",
                            "packages": {
                                "windows": {
                                    "kind": "web_installer_zip",
                                    "installer_url": "https://example.test/run-42/setup.exe",
                                    "installer_sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                                    "installer_size": 78,
                                    "package_url": "https://example.test/run-42/windows.zip",
                                    "package_sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                                    "package_size": 90
                                }
                            }
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let selected =
            manual_app_update_package_for_platform(&manifest, "9.9.8-beta", "windows").unwrap();
        assert_eq!(selected.version, "9.9.8-beta");
        assert_eq!(selected.kind, "web_installer_zip");
        assert_eq!(
            selected.download_url,
            "https://example.test/run-42/setup.exe"
        );
        assert_eq!(selected.download_sha256, "d".repeat(64));
        assert_eq!(selected.download_size, 78);
        assert_eq!(
            selected.package_url,
            "https://example.test/run-42/windows.zip"
        );
        assert_eq!(selected.package_sha256, "e".repeat(64));
        assert_eq!(selected.package_size, 90);
    }

    #[test]
    fn app_version_compare_requires_candidate_to_be_newer() {
        assert!(app_version_is_newer_than("1.6.4", "1.6.3"));
        assert!(app_version_is_newer_than("v1.6.4", "1.6.3"));
        assert!(app_version_is_newer_than("1.6.4-0.ni.7", "1.6.3"));
        assert!(!app_version_is_newer_than("1.6.3-0.ni.7", "1.6.3"));
        assert!(!app_version_is_newer_than("1.6.3", "1.6.3"));
        assert!(!app_version_is_newer_than("1.6.2", "1.6.3"));
        assert!(!app_version_is_newer_than("not-a-version", "1.6.3"));
    }

    #[test]
    fn latest_manual_app_update_returns_only_newest_newer_version() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "manual_versions": [
                        { "version": "0.0.1", "changelog": "old" },
                        { "version": "9999.9.7-0.ni.10", "changelog": "older test" },
                        { "version": "9999.9.9-0.ni.1", "changelog": "latest test" },
                        { "version": "9999.9.8", "changelog": "older stable" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let manual = latest_manual_app_update(&manifest).unwrap();
        assert_eq!(manual.version, "9999.9.9-0.ni.1");
        assert_eq!(manual.changelog, "latest test");
    }

    #[test]
    fn app_update_candidates_include_auto_and_manual_channels_separately() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.8",
                    "changelog": "stable update",
                    "manual_versions": [
                        { "version": "9999.9.7-0.ni.10", "changelog": "older test" },
                        { "version": "9999.9.9-0.ni.1", "changelog": "latest test" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].channel, "auto");
        assert_eq!(candidates[0].version, "9999.9.8");
        assert_eq!(candidates[0].changelog, "stable update");
        assert_eq!(candidates[1].channel, "manual");
        assert_eq!(candidates[1].version, "9999.9.9-0.ni.1");
        assert_eq!(candidates[1].changelog, "latest test");
    }

    #[test]
    fn app_update_candidates_hide_manual_when_same_as_auto() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.9",
                    "changelog": "stable update",
                    "manual_versions": [
                        { "version": "v9999.9.9", "changelog": "same manual" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].channel, "auto");
        assert_eq!(candidates[0].version, "9999.9.9");
    }

    #[test]
    fn app_update_candidates_empty_when_every_channel_is_current_or_older() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "not-a-version",
                    "manual_versions": [
                        { "version": "0.0.2", "changelog": "old test" },
                        { "version": "also-not-a-version", "changelog": "bad test" }
                    ]
                }
            }"#,
        )
        .unwrap();

        assert!(app_update_candidates(&manifest).is_empty());
    }

    #[test]
    fn requested_auto_version_selects_auto_package() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9.9.9",
                    "url": "https://example.test/stable.exe",
                    "manual_versions": [
                        {
                            "version": "9.9.9-0.ni.1",
                            "url": "https://example.test/test.exe"
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let selected =
            app_update_package_for_requested_version(&manifest, Some("9.9.9"), "windows")
                .unwrap();
        assert_eq!(selected.version, "9.9.9");
        assert_eq!(selected.download_url, "https://example.test/stable.exe");
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
            Some(std::path::Path::new("C:/cache/windows.zip")),
        );

        assert!(args.iter().any(|arg| arg == "/UPDATE=1"));
        assert!(args.iter().any(|arg| arg == "/LAUNCH=1"));
        assert!(args.iter().any(|arg| arg == "/WAITHANDLE=1234"));
        assert!(args.iter().any(|arg| arg == "/WAITPID=42"));
        assert!(args.iter().any(|arg| arg == "/WAITSTART=01db000000000000"));
        assert!(args.iter().any(|arg| arg == "/DIR=C:/Gyroflow"));
        assert!(
            args.iter()
                .any(|arg| arg == "/PACKAGEURL=https://example.test/windows.zip")
        );
        assert!(
            args.iter()
                .any(|arg| arg == "/PACKAGEFILE=C:/cache/windows.zip")
        );
        assert!(
            args.iter()
                .any(|arg| arg == &format!("/PACKAGESHA256={}", "b".repeat(64)))
        );
        assert!(args.iter().any(|arg| arg == "/PACKAGESIZE=34"));
    }

    #[test]
    fn is_wrapper_url_recognizes_nightly_and_cn_short_names() {
        // nightly.link host: any URL is a wrapper
        assert!(is_wrapper_url(
            "https://nightly.link/NiYien/gyroflow/actions/runs/123/gyroflow-niyien-win-setup.zip"
        ));
        // CN release short-name wrappers (123 disk avoids .bak suffix)
        assert!(is_wrapper_url(
            "https://download.niyien.com/api/download/app/v1.6.3/gyroflow-niyien-win-setup.zip"
        ));
        assert!(is_wrapper_url(
            "https://download.niyien.com/api/download/app/v1.6.3/gyroflow-niyien-android.zip"
        ));
        // Portable Windows zip is NOT a wrapper, even on download.niyien.com
        assert!(!is_wrapper_url(
            "https://download.niyien.com/api/download/app/v1.6.3/gyroflow-niyien-windows64.zip"
        ));
        // Plain GitHub Release .exe stays on the regular download path
        assert!(!is_wrapper_url(
            "https://github.com/NiYien/gyroflow/releases/download/v1.6.3/gyroflow-niyien-windows64-setup.exe"
        ));
        // Mac dmg is never a wrapper
        assert!(!is_wrapper_url(
            "https://download.niyien.com/api/download/app/v1.6.3/gyroflow-niyien-mac-universal.dmg"
        ));
        // Garbage URL returns false instead of panicking
        assert!(!is_wrapper_url("not a url"));
    }

    #[test]
    fn app_update_filename_falls_back_to_platform_default_for_cn_wrapper() {
        // CN release wrapper URL: cache filename should be the raw inner
        // .exe so launch_windows_update_impl can spawn it after extraction.
        let selection = AppUpdateSelection {
            platform: "windows".to_owned(),
            download_url:
                "https://download.niyien.com/api/download/app/v1.6.3/gyroflow-niyien-win-setup.zip"
                    .to_owned(),
            ..Default::default()
        };
        assert_eq!(
            app_update_filename(&selection),
            "gyroflow-niyien-windows64-setup.exe"
        );

        // Android wrapper falls back to the apk default
        let android = AppUpdateSelection {
            platform: "android".to_owned(),
            download_url:
                "https://download.niyien.com/api/download/app/v1.6.3/gyroflow-niyien-android.zip"
                    .to_owned(),
            ..Default::default()
        };
        assert_eq!(app_update_filename(&android), "gyroflow-niyien.apk");
    }
}
