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
    // Multi-language release notes keyed by language code (zh/en/ja/...).
    // Optional in older manifests; clients fall back to `changelog` when
    // this map is empty (release-notes-i18n).
    #[serde(default)]
    pub changelogs: BTreeMap<String, String>,
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
    #[serde(default)]
    pub archive_url: String,
    #[serde(default)]
    pub archive_sha256: String,
    #[serde(default)]
    pub archive_size: u64,
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
    // True when the aggregated release notes dropped older skipped
    // versions to stay within the display cap; the update dialog shows
    // a "full history" link in that case (changelog-history page).
    pub changelog_truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, Default)]
pub struct ManualAppVersion {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub changelog: String,
    // See AppRelease::changelogs.
    #[serde(default)]
    pub changelogs: BTreeMap<String, String>,
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
    identity_origin: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    identity_age_days: Option<u64>,
    #[serde(skip_serializing_if = "str::is_empty")]
    language: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    os: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    camera_brand: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    camera_model: &'a str,
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
    // Cameras already reported by this process, keyed by (brand, model).
    // Per-process is enough: the dashboard counts unique users per day, so a
    // repeat report within one session carries no new information.
    static ref REPORTED_CAMERAS: Mutex<std::collections::HashSet<(String, String)>> =
        Mutex::new(std::collections::HashSet::new());

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
        // Cold-edge prewarm before the (retried) download: lens/sdk content also
        // streams from the 123 direct-link CDN, same cold-origin 504 risk as the
        // plugin path. Best-effort, gated by NIYIEN_DOWNLOAD_PREWARM.
        crate::network::prewarm_url(&release.url);
        let response = crate::network::call_with_retry(package_name, || {
            configure_geo_request(crate::network::get(&release.url)).call()
        })
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

/// How this installation's anonymous id came to be. Reported with every event
/// so migrated users can be told apart from genuinely new ones.
const IDENTITY_ORIGIN_GENERATED: &str = "generated";
const IDENTITY_ORIGIN_ADOPTED: &str = "adopted_from_tool";

fn telemetry_anon_id() -> String {
    let existing = gyroflow_core::settings::get_str("telemetryAnonId", "");
    if !existing.trim().is_empty() {
        return existing;
    }

    // First run on this machine. Before minting a fresh identity, adopt the one
    // NiYien Tool already uses if it is installed here: both products report the
    // same product_id, so an adopted id lets the server recognize a migrating
    // user as returning rather than counting them as a new customer.
    //
    // Only ever done at first generation — an install that already has an id
    // keeps it, since re-pointing would sever its history and report one person
    // under two identities across the switchover.
    if let Some(adopted) = legacy_tool_anon_id() {
        ::log::info!(
            target: "update",
            "telemetry identity adopted from NiYien Tool"
        );
        gyroflow_core::settings::set("telemetryAnonId", adopted.clone().into());
        gyroflow_core::settings::set(
            "telemetryIdentityOrigin",
            IDENTITY_ORIGIN_ADOPTED.into(),
        );
        return adopted;
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
    gyroflow_core::settings::set(
        "telemetryIdentityOrigin",
        IDENTITY_ORIGIN_GENERATED.into(),
    );
    gyroflow_core::settings::set(
        "telemetryAnonIdCreatedAt",
        (now_ms as u64 / 1000).into(),
    );
    generated
}

/// Origin of the current identity. Installations that predate this recording
/// report `generated` — they are not backdated, but they also predate adoption,
/// so `generated` is accurate for them.
fn telemetry_identity_origin() -> String {
    let stored = gyroflow_core::settings::get_str("telemetryIdentityOrigin", "");
    if stored.trim().is_empty() {
        IDENTITY_ORIGIN_GENERATED.to_owned()
    } else {
        stored
    }
}

/// Age of this installation's identity in whole days, or `None` when the id
/// predates creation-time recording. Never inferred as "created now" — that
/// would make every established install look brand new on release day.
fn telemetry_identity_age_days() -> Option<u64> {
    let created_at = gyroflow_core::settings::get_u64("telemetryAnonIdCreatedAt", 0);
    if created_at == 0 {
        return None;
    }

    let now_s = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    Some(now_s.saturating_sub(created_at) / 86400)
}

/// OS description, matching the shape already used by the feedback payload
/// (`src/feedback/meta.rs`). Cached — probing the OS on every event is wasteful
/// and the answer cannot change while the process runs.
fn os_description() -> String {
    static CACHED: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CACHED
        .get_or_init(|| {
            let info = os_info::get();
            format!("{} {} ({})", info.os_type(), info.version(), info.bitness())
        })
        .clone()
}

/// NiYien Tool's anonymous id, if that product is installed on this machine.
///
/// Read-only: the Tool's file is never written, modified, or deleted. Path per
/// NiYien_Tool `mainwindow.cpp` (`get_local_path() + "/NiYien_Tool/telemetry.ini"`).
/// Returns `None` when absent, unreadable, or malformed, so the caller falls
/// through to generating a fresh id.
fn legacy_tool_anon_id() -> Option<String> {
    let path = legacy_tool_telemetry_ini()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    parse_legacy_tool_anon_id(&contents)
}

fn legacy_tool_telemetry_ini() -> Option<std::path::PathBuf> {
    #[cfg(target_os = "windows")]
    {
        let local = std::env::var_os("LOCALAPPDATA")?;
        Some(
            std::path::PathBuf::from(local)
                .join("NiYien_Tool")
                .join("telemetry.ini"),
        )
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        Some(
            std::path::PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("NiYien_Tool")
                .join("telemetry.ini"),
        )
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// Minimal INI lookup for the Tool's `[telemetry] anon_id=...` entry. Written by
/// QSettings, so the file is plain `key=value` under a section header.
fn parse_legacy_tool_anon_id(contents: &str) -> Option<String> {
    let mut in_telemetry_section = false;

    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }

        if line.starts_with('[') && line.ends_with(']') {
            in_telemetry_section = line[1..line.len() - 1].trim().eq_ignore_ascii_case("telemetry");
            continue;
        }

        if !in_telemetry_section {
            continue;
        }

        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("anon_id") {
                let value = value.trim().trim_matches('"').trim();
                if !value.is_empty() && value.len() <= 128 {
                    return Some(value.to_owned());
                }
                return None;
            }
        }
    }

    None
}

/// Report that this installation encountered a camera, mirroring NiYien Tool's
/// `open` event so the two products' camera and language breakdowns describe the
/// same thing and can be read side by side.
///
/// Deduplicated per process by `(brand, model)`. The dashboard counts unique
/// users per day, never `open` event volume, so re-reporting the same camera
/// within a session would add nothing — which is also why this needs no
/// persisted queue or cooldown.
pub fn report_camera_open_event(brand: &str, model: &str) {
    let brand = brand.trim();
    let model = model.trim();
    if brand.is_empty() && model.is_empty() {
        return;
    }

    {
        let mut seen = REPORTED_CAMERAS.lock();
        if !seen.insert((brand.to_owned(), model.to_owned())) {
            return;
        }
    }

    ::log::debug!(target: "update", "telemetry open event: brand={brand} model={model}");
    report_event_internal("open", "", "", "", "", 0, 0, "", brand, model);
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
    report_event_internal(
        event,
        artifact_type,
        artifact_version,
        selected_source,
        status,
        duration_ms,
        bytes,
        error_code,
        "",
        "",
    );
}

#[allow(clippy::too_many_arguments)]
fn report_event_internal(
    event: &str,
    artifact_type: &str,
    artifact_version: &str,
    selected_source: &str,
    status: &str,
    duration_ms: u128,
    bytes: u64,
    error_code: &str,
    camera_brand: &str,
    camera_model: &str,
) {
    let endpoint = gyroflow_core::distribution::telemetry_api().to_owned();
    if endpoint.is_empty() {
        return;
    }
    let anon_id = telemetry_anon_id();
    let identity_origin = telemetry_identity_origin();
    let language = crate::util::system_locale_name();
    let os = os_description();

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
        identity_origin: &identity_origin,
        identity_age_days: telemetry_identity_age_days(),
        language: &language,
        os: &os,
        camera_brand,
        camera_model,
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

/// Pick the release-notes string that matches the given UI locale, with
/// a fallback chain. Used by both the auto-update path and manual
/// version listings (release-notes-i18n).
///
/// Fallback order:
///   1. `changelogs[base(locale)]` — e.g. `zh_CN` -> `zh`, `pt_BR` -> `pt`
///   2. `changelogs["en"]`
///   3. `changelogs["zh"]`
///   4. First entry of `changelogs` (sorted by key — BTreeMap is ordered)
///   5. Legacy `changelog` string
pub fn pick_changelog(
    legacy: &str,
    changelogs: &BTreeMap<String, String>,
    locale: &str,
) -> String {
    if !changelogs.is_empty() {
        // Try the base language code of the current locale ("zh_CN" -> "zh").
        let base = base_lang_code(locale);
        if !base.is_empty() {
            if let Some(text) = changelogs.get(base) {
                return text.clone();
            }
        }
        for fallback in ["en", "zh"] {
            if let Some(text) = changelogs.get(fallback) {
                return text.clone();
            }
        }
        if let Some((_, text)) = changelogs.iter().next() {
            return text.clone();
        }
    }
    legacy.to_owned()
}

/// Extract the base ISO 639-1 language code from a locale string.
/// Splits on `_` or `-` and lowercases the first chunk. Returns empty
/// string if the input is empty or the chunk has fewer than 2 chars.
fn base_lang_code(locale: &str) -> &str {
    let trimmed = locale.trim();
    if trimmed.is_empty() {
        return "";
    }
    let end = trimmed
        .find(|c: char| c == '_' || c == '-')
        .unwrap_or(trimmed.len());
    let base = &trimmed[..end];
    if base.len() >= 2 { base } else { "" }
}

/// Cap on how many skipped versions the update dialog aggregates.
pub const AGGREGATED_CHANGELOG_MAX_VERSIONS: usize = 5;

/// Locale-resolved release notes for an update candidate targeting
/// `target_version`, aggregated across every version the user skipped:
/// entries of `manual_versions` newer than the running build and not
/// newer than `target_version`, newest first. Each entry resolves its
/// text through `pick_changelog`; entries that resolve to empty text
/// don't count against the display cap. With two or more entries each
/// section gets a bold version heading (the dialog renders Markdown);
/// a single entry stays plain so the current look doesn't change.
/// Returns the joined text plus whether older entries were dropped by
/// the cap.
///
/// Old or malformed manifests may not carry the target version inside
/// `manual_versions`; its section is then synthesized from the
/// candidate's own `changelog`/`changelogs` fields so the dialog never
/// shows only older versions' notes (and a plain non-aggregated
/// manifest degrades to exactly the pre-aggregation behavior).
pub fn resolve_update_changelog(
    manual_versions: &[ManualAppVersion],
    target_version: &str,
    fallback_legacy: &str,
    fallback_changelogs: &BTreeMap<String, String>,
    locale: &str,
) -> (String, bool) {
    let mut entries: Vec<(&str, String)> = manual_versions
        .iter()
        .filter(|v| {
            app_version_is_newer_than_current(&v.version)
                && compare_app_versions(&v.version, target_version) != Ordering::Greater
        })
        .filter_map(|v| {
            let text = pick_changelog(&v.changelog, &v.changelogs, locale);
            let text = text.trim().to_owned();
            (!text.is_empty()).then(|| (v.version.trim(), text))
        })
        .collect();
    if !entries.iter().any(|(v, _)| app_versions_equivalent(v, target_version)) {
        let text = pick_changelog(fallback_legacy, fallback_changelogs, locale)
            .trim()
            .to_owned();
        if !text.is_empty() {
            entries.push((target_version.trim(), text));
        }
    }
    entries.sort_by(|a, b| compare_app_versions(b.0, a.0));
    let truncated = entries.len() > AGGREGATED_CHANGELOG_MAX_VERSIONS;
    entries.truncate(AGGREGATED_CHANGELOG_MAX_VERSIONS);
    if entries.len() <= 1 {
        return (entries.pop().map(|(_, text)| text).unwrap_or_default(), truncated);
    }
    let text = entries
        .iter()
        .map(|(version, text)| format!("**v{}**\n\n{}", version.trim_start_matches('v'), text))
        .collect::<Vec<_>>()
        .join("\n\n");
    (text, truncated)
}

pub fn app_update_candidates(
    manifest: &Manifest,
    locale: &str,
) -> Vec<AppUpdateCandidate> {
    let mut candidates = Vec::new();
    if has_app_update(manifest) {
        let (changelog, changelog_truncated) = resolve_update_changelog(
            &manifest.app.manual_versions,
            &manifest.app.version,
            &manifest.app.changelog,
            &manifest.app.changelogs,
            locale,
        );
        candidates.push(AppUpdateCandidate {
            channel: "auto".to_owned(),
            version: manifest.app.version.trim().to_owned(),
            changelog,
            changelog_truncated,
        });
    }
    if let Some(manual) = latest_manual_app_update(manifest)
        .filter(|manual| !app_versions_equivalent(&manual.version, &manifest.app.version))
    {
        let (changelog, changelog_truncated) = resolve_update_changelog(
            &manifest.app.manual_versions,
            &manual.version,
            &manual.changelog,
            &manual.changelogs,
            locale,
        );
        candidates.push(AppUpdateCandidate {
            channel: "manual".to_owned(),
            version: manual.version.trim().to_owned(),
            changelog,
            changelog_truncated,
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

// NiYien custom version ordering. Rejects SemVer's "release > pre-release"
// rule because for niyien builds the bare base (e.g. "1.6.3") is the first
// release of that base and any "<base>-<schema>.<N>" build is a later one.
//
// Cross-base: pure (major, minor, patch) numeric comparison. Suffix never
// influences cross-base ordering, so 1.6.4 > 1.6.3-ni.999.
//
// Same base: bare base < any suffixed build. Within the same suffix schema
// (ni / dev), the trailing integer is compared numerically so ni.28 > ni.27.
// Across schemas, "ni" outranks "dev" so a CI build always beats a local
// dev build at the same base.
#[derive(Debug, PartialEq, Eq)]
struct NiyienVersion {
    base: (u64, u64, u64),
    suffix: Option<NiyienSuffix>,
}

#[derive(Debug, PartialEq, Eq)]
struct NiyienSuffix {
    schema: String,
    sequence: Option<u64>,
    raw: String,
}

fn parse_app_version(version: &str) -> Option<NiyienVersion> {
    let trimmed = version.trim().trim_start_matches('v');
    if trimmed.is_empty() {
        return None;
    }
    let (base_str, suffix_str) = match trimmed.split_once('-') {
        Some((b, s)) => (b, Some(s)),
        None => (trimmed, None),
    };
    let mut parts = base_str.split('.');
    let major = parts.next()?.parse::<u64>().ok()?;
    let minor = parts.next()?.parse::<u64>().ok()?;
    let patch = parts.next()?.parse::<u64>().ok()?;
    if parts.next().is_some() {
        return None;
    }
    let suffix = suffix_str.map(|raw| {
        let (schema, seq) = match raw.split_once('.') {
            Some((k, n)) => (k.to_owned(), n.parse::<u64>().ok()),
            None => (raw.to_owned(), None),
        };
        NiyienSuffix {
            schema,
            sequence: seq,
            raw: raw.to_owned(),
        }
    });
    Some(NiyienVersion {
        base: (major, minor, patch),
        suffix,
    })
}

fn schema_priority(schema: &str) -> u8 {
    // Higher number = newer at the same base. Extend when adding schemas.
    match schema {
        "ni" => 2,
        "dev" => 1,
        _ => 0,
    }
}

fn cmp_niyien(a: &NiyienVersion, b: &NiyienVersion) -> Ordering {
    a.base.cmp(&b.base).then_with(|| match (&a.suffix, &b.suffix) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Less,
        (Some(_), None) => Ordering::Greater,
        (Some(x), Some(y)) => schema_priority(&x.schema)
            .cmp(&schema_priority(&y.schema))
            .then_with(|| match (x.sequence, y.sequence) {
                (Some(xn), Some(yn)) => xn.cmp(&yn),
                _ => x.raw.cmp(&y.raw),
            }),
    })
}

impl Ord for NiyienVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_niyien(self, other)
    }
}

impl PartialOrd for NiyienVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
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
                default_app_update_kind(platform).to_owned()
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
            kind: default_app_update_kind(platform).to_owned(),
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

    if selection.platform == "linux" {
        prepare_linux_appimage(&path)?;
    }

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

// 8-hex-char URL fingerprint used to key partial download files to their
// source URL, so a manifest URL change (e.g. a new actions run) never resumes
// into a stale partial from the previous URL.
fn url_hash8(url: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(url.as_bytes());
    hex_digest(&hasher.finalize()[..4])
}

fn resumable_temp_path(final_path: &Path, url: &str, kind: &str) -> PathBuf {
    final_path.with_extension(format!("{}.{kind}", url_hash8(url)))
}

// Remove leftover partials for `final_path` written for other URLs (including
// the legacy unsuffixed temp names), keeping only `keep`. Best-effort.
fn clean_stale_partials(final_path: &Path, keep: &Path, kind: &str) {
    let Some(parent) = final_path.parent() else {
        return;
    };
    let Some(stem) = final_path.file_stem().and_then(|s| s.to_str()) else {
        return;
    };
    let keep_name = keep
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned();
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name != keep_name
            && name.starts_with(&format!("{stem}."))
            && name.ends_with(kind)
            && std::fs::remove_file(entry.path()).is_ok()
        {
            log::info!(target: "update", "removed stale update partial {name}");
        }
    }
}

// Body-resume streaming (design D4). One call covers the whole body phase: it
// re-issues ranged GETs after mid-body failures, appending to `temp_path`,
// until the stream reaches EOF at the known total or the shared retry budget
// (`update_retry_profile`) is exhausted. Header-phase failures inside each
// issue still go through `call_with_retry` with its own budget; both budgets
// are small so the worst-case product stays bounded.
fn stream_update_body_resumable<F>(
    label: &str,
    url: &str,
    temp_path: &Path,
    expected_total: u64,
    progress: &mut F,
    progress_status: &str,
) -> Result<(), String>
where
    F: FnMut(u64, u64, &str),
{
    let (retries, base) = crate::network::update_retry_profile();
    let mut body_tries = 0u32;
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let offset = std::fs::metadata(temp_path).map(|m| m.len()).unwrap_or(0);
        let ranged = offset > 0;
        let response = crate::network::call_with_retry(label, || {
            let mut request = configure_geo_request(crate::network::get(url));
            if ranged {
                request = request.header("Range", format!("bytes={offset}-"));
            }
            request.call()
        });
        let response = match response {
            Ok(response) => response,
            // 416: our partial is at or past the remote length (content changed
            // under the same URL, or local corruption) — drop it and restart.
            Err(ureq::Error::StatusCode(416)) if ranged => {
                let _ = std::fs::remove_file(temp_path);
                if body_tries >= retries {
                    return Err(format!("download {label} failed: HTTP 416 on resume"));
                }
                log::warn!(
                    target: "update",
                    "download {label} resume from {offset} got HTTP 416, restarting from zero"
                );
                body_tries += 1;
                continue;
            }
            Err(err) => return Err(format!("download {label} failed: {err}")),
        };
        // 206 = the server honored the range and we append; a 200 means the
        // range was ignored, so the file restarts from scratch.
        let append = ranged && response.status() == 206;
        let base_offset = if append { offset } else { 0 };
        let content_length = response
            .headers()
            .get("content-length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
        let content_range_total = response
            .headers()
            .get("content-range")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.rsplit('/').next()?.trim().parse::<u64>().ok());
        let known_total = content_range_total
            .or(content_length.map(|len| base_offset + len))
            .unwrap_or(expected_total);

        let mut reader = response.into_body().into_reader();
        let mut output = if append {
            std::fs::OpenOptions::new().append(true).open(temp_path)
        } else {
            std::fs::File::create(temp_path)
        }
        .map_err(|err| format!("create update temp file failed: {err}"))?;
        let mut written = base_offset;
        let body_error = loop {
            match reader.read(&mut buffer) {
                Ok(0) => break None,
                Ok(read) => {
                    // Disk write failures are not transient network errors —
                    // surface them immediately instead of burning the budget.
                    output
                        .write_all(&buffer[..read])
                        .map_err(|err| format!("write update download failed: {err}"))?;
                    written += read as u64;
                    progress(written, known_total.max(written), progress_status);
                }
                Err(err) => break Some(err.to_string()),
            }
        };
        if body_error.is_none() {
            output
                .flush()
                .map_err(|err| format!("flush update download failed: {err}"))?;
        }
        drop(output);
        // A clean EOF short of the known total is a truncated stream (server
        // closed early) — resume it like a mid-body failure instead of handing
        // a short file to checksum verification.
        let failure = body_error.or_else(|| {
            (known_total > 0 && written < known_total)
                .then(|| format!("stream ended early at {written}/{known_total} bytes"))
        });
        match failure {
            None => return Ok(()),
            Some(err) => {
                if body_tries >= retries {
                    return Err(format!(
                        "read {label} body failed after {} attempts: {err}",
                        body_tries + 1
                    ));
                }
                let delay = crate::network::backoff_delay(base, body_tries);
                log::warn!(
                    target: "update",
                    "download {label} body interrupted at {written}/{} (attempt {}/{}): {err}; resuming in {delay:?}",
                    known_total,
                    body_tries + 1,
                    retries + 1
                );
                body_tries += 1;
                std::thread::sleep(delay);
            }
        }
    }
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

    // Cold-edge prewarm before the (retried) app-update download — same 123 CDN
    // cold-origin 504 as the plugin path. Best-effort, gated by env.
    crate::network::prewarm_url(url);

    if crate::network::body_resume_enabled() {
        let temp_path = resumable_temp_path(&path, url, "download");
        clean_stale_partials(&path, &temp_path, "download");
        stream_update_body_resumable(
            label,
            url,
            &temp_path,
            expected_size,
            progress,
            progress_status,
        )?;
        // Hash the completed file from disk so verification covers the resumed
        // prefix as well as the freshly streamed bytes.
        let (actual_sha256, _) = sha256_file_hex(&temp_path)
            .map_err(|err| format!("read {label} download failed: {err}"))?;
        if let Err(err) = verify_sha256_hex(label, &actual_sha256, expected_sha256) {
            // A complete-but-wrong file must not be resumed into next time.
            let _ = std::fs::remove_file(&temp_path);
            return Err(err);
        }
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|err| format!("replace cached update file failed: {err}"))?;
        }
        std::fs::rename(&temp_path, &path)
            .map_err(|err| format!("activate update download failed: {err}"))?;
        return Ok(path);
    }

    // Legacy single-shot body path (NIYIEN_UPDATE_BODY_RESUME=0): kept
    // byte-identical to the pre-resume behavior as the rollback path.
    let response = crate::network::call_with_retry(label, || {
        configure_geo_request(crate::network::get(url)).call()
    })
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
    // Cold-edge prewarm before the (retried) nightly-wrapper download.
    crate::network::prewarm_url(url);

    if crate::network::body_resume_enabled() {
        // The wrapper size is not in the manifest (sha/size describe the inner
        // file), so short-read detection relies on per-attempt headers.
        let wrapper_path = resumable_temp_path(&path, url, "nightly-wrapper.zip");
        clean_stale_partials(&path, &wrapper_path, "nightly-wrapper.zip");
        stream_update_body_resumable(
            &format!("{label} (nightly wrapper)"),
            url,
            &wrapper_path,
            0,
            progress,
            progress_status,
        )?;
        let mut buffer = [0_u8; 128 * 1024];
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
        return Ok(path);
    }

    // Legacy single-shot body path (NIYIEN_UPDATE_BODY_RESUME=0): kept
    // byte-identical to the pre-resume behavior as the rollback path.
    let response = crate::network::call_with_retry(label, || {
        configure_geo_request(crate::network::get(url)).call()
    })
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

/// Sentinel error surfaced to QML when Android blocks the install because the
/// per-app "install unknown apps" grant is missing. QML shows a guidance
/// dialog instead of the generic failure message (same sentinel pattern as
/// the plugin-copy-blocked message in nle_plugins).
pub const INSTALL_PERMISSION_REQUIRED_ERROR: &str = "install-permission-required";

pub fn open_downloaded_update(prepared: &PreparedAppUpdate) -> Result<(), String> {
    if prepared.selection.platform == "macos" {
        return open_macos_update(&prepared.path);
    }
    if prepared.selection.platform == "windows" {
        return launch_windows_update(prepared);
    }
    if prepared.selection.platform == "android" {
        return open_android_update(&prepared.path);
    }
    if prepared.selection.platform == "linux" {
        return open_linux_update_directory(&prepared.path);
    }
    Err(format!(
        "app update handoff is not supported on {}",
        prepared.selection.platform
    ))
}

fn open_android_update(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "android")]
    {
        crate::util::android_install_apk(&path.to_string_lossy())
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = path;
        Err("Android update handoff is only available on Android".to_owned())
    }
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

fn default_app_update_kind(platform: &str) -> &'static str {
    if platform == "windows" {
        "web_installer_zip"
    } else if platform == "linux" {
        "appimage"
    } else {
        "dmg"
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
    } else if platform == "linux" {
        "gyroflow-niyien-linux64.AppImage"
    } else {
        "gyroflow-niyien-mac-universal.dmg"
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn linux_appimage_mode(mode: u32) -> u32 {
    mode | 0o100
}

#[cfg(unix)]
fn prepare_linux_appimage(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("read downloaded Linux AppImage metadata failed: {err}"))?;
    let current_mode = metadata.permissions().mode();
    let updated_mode = linux_appimage_mode(current_mode);
    if updated_mode == current_mode {
        return Ok(());
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(updated_mode))
        .map_err(|err| format!("mark downloaded Linux AppImage executable failed: {err}"))
}

#[cfg(not(unix))]
fn prepare_linux_appimage(path: &Path) -> Result<(), String> {
    let _ = path;
    Err("Linux AppImage preparation is only available on Unix".to_owned())
}

pub fn app_update_handoff_should_quit(platform: &str) -> bool {
    normalize_app_update_platform(platform) != "linux"
}

fn open_linux_update_directory(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        open_linux_update_directory_with(path, |program, args| {
            std::process::Command::new(program)
                .args(args.iter().copied())
                .status()
                .map(|status| status.success())
                .map_err(|err| err.to_string())
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        Err("Linux update folder handoff is only available on Linux".to_owned())
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn open_linux_update_directory_with<F>(path: &Path, mut run: F) -> Result<(), String>
where
    F: FnMut(&str, &[&std::ffi::OsStr]) -> Result<bool, String>,
{
    let absolute_path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|err| format!("resolve downloaded Linux AppImage path failed: {err}"))?
            .join(path)
    };
    let directory = absolute_path.parent().ok_or_else(|| {
        format!(
            "downloaded Linux AppImage has no containing directory: {}",
            absolute_path.display()
        )
    })?;

    let xdg_error = match run("xdg-open", &[directory.as_os_str()]) {
        Ok(true) => return Ok(()),
        Ok(false) => "command returned a failure status".to_owned(),
        Err(err) => err,
    };
    let gio_error = match run(
        "gio",
        &[std::ffi::OsStr::new("open"), directory.as_os_str()],
    ) {
        Ok(true) => return Ok(()),
        Ok(false) => "command returned a failure status".to_owned(),
        Err(err) => err,
    };

    Err(format!(
        "Unable to open the folder containing Linux AppImage {}. xdg-open failed: {}; gio open failed: {}",
        absolute_path.display(),
        xdg_error,
        gio_error
    ))
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

pub fn fetch_app_update_candidates(
    force: bool,
    locale: &str,
) -> Result<Vec<AppUpdateCandidate>, String> {
    match fetch_manifest(force) {
        Ok(manifest) => Ok(app_update_candidates(&manifest, locale)),
        Err(first_err) if force => cached_manifest()
            .map(|manifest| app_update_candidates(&manifest, locale))
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
    fn linux_manifest_defaults_to_appimage_and_ignores_archive_for_updates() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9.9.9",
                    "url": "https://example.test/gyroflow-niyien-linux64.AppImage",
                    "packages": {
                        "linux": {
                            "package_url": "https://example.test/gyroflow-niyien-linux64.AppImage",
                            "package_sha256": "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                            "package_size": 78,
                            "archive_url": "https://example.test/gyroflow-niyien-linux64.tar.gz",
                            "archive_sha256": "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee",
                            "archive_size": 90
                        }
                    }
                }
            }"#,
        )
        .unwrap();

        let package = manifest.app.packages.get("linux").unwrap();
        assert_eq!(
            package.archive_url,
            "https://example.test/gyroflow-niyien-linux64.tar.gz"
        );
        assert_eq!(package.archive_sha256, "e".repeat(64));
        assert_eq!(package.archive_size, 90);

        let selected = app_update_package_for_platform(&manifest, "linux").unwrap();
        assert_eq!(selected.kind, "appimage");
        assert_eq!(
            selected.download_url,
            "https://example.test/gyroflow-niyien-linux64.AppImage"
        );
        assert_eq!(selected.download_sha256, "d".repeat(64));
        assert_eq!(selected.download_size, 78);
    }

    #[test]
    fn linux_update_uses_appimage_filename_for_direct_and_wrapped_urls() {
        assert_eq!(
            default_app_update_filename("linux"),
            "gyroflow-niyien-linux64.AppImage"
        );
        let wrapped = AppUpdateSelection {
            platform: "linux".to_owned(),
            download_url: "https://nightly.link/NiYien/gyroflow/actions/runs/123/gyroflow-niyien-linux-appimage.zip".to_owned(),
            ..Default::default()
        };
        assert_eq!(
            app_update_filename(&wrapped),
            "gyroflow-niyien-linux64.AppImage"
        );
    }

    #[test]
    fn linux_appimage_mode_adds_only_owner_execute_permission() {
        assert_eq!(linux_appimage_mode(0o640), 0o740);
        assert_eq!(linux_appimage_mode(0o755), 0o755);
    }

    #[cfg(unix)]
    #[test]
    fn prepare_linux_appimage_preserves_mode_except_owner_execute() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "gyroflow-linux-appimage-mode-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, b"appimage").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();

        prepare_linux_appimage(&path).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o740
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn linux_update_opener_prefers_xdg_open_for_the_containing_directory() {
        let appimage = std::env::temp_dir()
            .join("gyroflow-linux-opener-xdg")
            .join("gyroflow-niyien-linux64.AppImage");
        let expected_dir = appimage.parent().unwrap().to_path_buf();
        let mut calls = Vec::new();

        open_linux_update_directory_with(&appimage, |program, args: &[&std::ffi::OsStr]| {
            calls.push((
                program.to_owned(),
                args.iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            ));
            Ok(true)
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![(
                "xdg-open".to_owned(),
                vec![expected_dir.to_string_lossy().into_owned()]
            )]
        );
    }

    #[test]
    fn linux_update_opener_falls_back_to_gio_open() {
        let appimage = std::env::temp_dir()
            .join("gyroflow-linux-opener-gio")
            .join("gyroflow-niyien-linux64.AppImage");
        let expected_dir = appimage.parent().unwrap().to_string_lossy().into_owned();
        let mut calls = Vec::new();

        open_linux_update_directory_with(&appimage, |program, args: &[&std::ffi::OsStr]| {
            calls.push((
                program.to_owned(),
                args.iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect::<Vec<_>>(),
            ));
            Ok(program == "gio")
        })
        .unwrap();

        assert_eq!(
            calls,
            vec![
                ("xdg-open".to_owned(), vec![expected_dir.clone()]),
                ("gio".to_owned(), vec!["open".to_owned(), expected_dir]),
            ]
        );
    }

    #[test]
    fn linux_update_opener_reports_absolute_appimage_path_when_all_openers_fail() {
        let appimage = std::env::temp_dir()
            .join("gyroflow-linux-opener-fail")
            .join("gyroflow-niyien-linux64.AppImage");

        let error =
            open_linux_update_directory_with(&appimage, |_program, _args: &[&std::ffi::OsStr]| {
                Ok(false)
            })
            .unwrap_err();

        assert!(appimage.is_absolute());
        assert!(error.contains(&appimage.to_string_lossy().into_owned()));
        assert!(error.contains("xdg-open"));
        assert!(error.contains("gio open"));
    }

    #[test]
    fn linux_update_handoff_never_requests_application_quit() {
        assert!(!app_update_handoff_should_quit("linux"));
        assert!(app_update_handoff_should_quit("windows"));
        assert!(app_update_handoff_should_quit("macos"));
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

    // Minimal HTTP/1.1 stub for body-resume tests. Serves one connection per
    // plan entry in order, always answering `Connection: close` so ureq never
    // reuses a socket across plans. The first request is always the best-effort
    // prewarm probe (`Range: bytes=0-0`), so plans start with `Prewarm`.
    enum StubPlan {
        Prewarm,
        // 200 with the full Content-Length declared but only the first `1` (n)
        // bytes of the body sent before a clean close — a truncated stream.
        TruncatedAt(Vec<u8>, usize),
        // Honors `Range: bytes=N-` with a 206; serves 200 full without Range.
        Ranged(Vec<u8>),
        // Ignores any Range header and always serves the full body with 200.
        FullIgnoringRange(Vec<u8>),
    }

    fn spawn_http_stub(plans: Vec<StubPlan>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for plan in plans {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buf = [0_u8; 8192];
                let mut req = Vec::new();
                loop {
                    match stream.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            req.extend_from_slice(&buf[..n]);
                            if req.windows(4).any(|w| w == b"\r\n\r\n") {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
                let req_text = String::from_utf8_lossy(&req).into_owned();
                let range_start = req_text.lines().find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("range: bytes=")
                        .and_then(|v| v.split('-').next()?.trim().parse::<usize>().ok())
                });
                seen.push(req_text);
                let mut response = Vec::new();
                match &plan {
                    StubPlan::Prewarm => {
                        response.extend_from_slice(
                            b"HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-0/1\r\nContent-Length: 1\r\nConnection: close\r\n\r\nX",
                        );
                    }
                    StubPlan::TruncatedAt(content, cut) => {
                        response.extend_from_slice(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                content.len()
                            )
                            .as_bytes(),
                        );
                        response.extend_from_slice(&content[..*cut]);
                    }
                    StubPlan::Ranged(content) => match range_start {
                        Some(start) if start < content.len() => {
                            let body = &content[start..];
                            response.extend_from_slice(
                                format!(
                                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {}-{}/{}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    start,
                                    content.len() - 1,
                                    content.len(),
                                    body.len()
                                )
                                .as_bytes(),
                            );
                            response.extend_from_slice(body);
                        }
                        _ => {
                            response.extend_from_slice(
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    content.len()
                                )
                                .as_bytes(),
                            );
                            response.extend_from_slice(content);
                        }
                    },
                    StubPlan::FullIgnoringRange(content) => {
                        response.extend_from_slice(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                content.len()
                            )
                            .as_bytes(),
                        );
                        response.extend_from_slice(content);
                    }
                }
                let _ = stream.write_all(&response);
            }
            seen
        });
        (format!("http://127.0.0.1:{port}"), handle)
    }

    fn resume_test_target(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gyroflow-resume-test-{tag}-{}-{}.bin",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn resume_test_content() -> Vec<u8> {
        (0_u32..300_000).map(|i| (i % 251) as u8).collect()
    }

    #[test]
    fn download_resumes_with_range_after_mid_body_truncation() {
        let content = resume_test_content();
        let cut = 150_000_usize;
        let (base, stub) = spawn_http_stub(vec![
            StubPlan::Prewarm,
            StubPlan::TruncatedAt(content.clone(), cut),
            StubPlan::Ranged(content.clone()),
        ]);
        let path = resume_test_target("range");
        let result = download_or_reuse_update_file(
            "app update",
            &format!("{base}/resume-range.bin"),
            &sha256_hex_for_test(&content),
            content.len() as u64,
            path.clone(),
            &mut |_, _, _| {},
            "downloading",
        )
        .unwrap();
        assert_eq!(fs::read(&result).unwrap(), content);
        let requests = stub.join().unwrap();
        assert!(
            requests[2]
                .to_ascii_lowercase()
                .contains(&format!("range: bytes={cut}-")),
            "resume request should carry the partial offset, got: {}",
            requests[2]
        );
        let _ = fs::remove_file(&result);
    }

    #[test]
    fn download_restarts_from_zero_when_server_ignores_range() {
        let content = resume_test_content();
        let (base, stub) = spawn_http_stub(vec![
            StubPlan::Prewarm,
            StubPlan::TruncatedAt(content.clone(), 100_000),
            StubPlan::FullIgnoringRange(content.clone()),
        ]);
        let path = resume_test_target("ignored");
        let result = download_or_reuse_update_file(
            "app update",
            &format!("{base}/resume-ignored.bin"),
            &sha256_hex_for_test(&content),
            content.len() as u64,
            path.clone(),
            &mut |_, _, _| {},
            "downloading",
        )
        .unwrap();
        assert_eq!(fs::read(&result).unwrap(), content);
        let requests = stub.join().unwrap();
        assert!(requests[2].to_ascii_lowercase().contains("range: bytes=100000-"));
        let _ = fs::remove_file(&result);
    }

    #[test]
    fn download_cleans_stale_partials_from_other_urls() {
        let content = resume_test_content();
        let (base, stub) = spawn_http_stub(vec![
            StubPlan::Prewarm,
            StubPlan::Ranged(content.clone()),
        ]);
        let path = resume_test_target("stale");
        // Partials left by a previous URL (hash-suffixed) and by the legacy
        // unsuffixed temp name must be swept, not resumed into.
        let stale_other_url = path.with_extension("deadbeef.download");
        let stale_legacy = path.with_extension("download");
        fs::write(&stale_other_url, b"stale-bytes").unwrap();
        fs::write(&stale_legacy, b"stale-bytes").unwrap();
        let result = download_or_reuse_update_file(
            "app update",
            &format!("{base}/resume-stale.bin"),
            &sha256_hex_for_test(&content),
            content.len() as u64,
            path.clone(),
            &mut |_, _, _| {},
            "downloading",
        )
        .unwrap();
        assert_eq!(fs::read(&result).unwrap(), content);
        assert!(!stale_other_url.exists(), "old-URL partial must be swept");
        assert!(!stale_legacy.exists(), "legacy temp partial must be swept");
        let requests = stub.join().unwrap();
        assert!(
            !requests[1].to_ascii_lowercase().contains("range:"),
            "fresh download must not resume from a stale partial"
        );
        let _ = fs::remove_file(&result);
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
        // Cross-base: numeric (major, minor, patch) only; suffix never wins across base.
        assert!(app_version_is_newer_than("1.6.4", "1.6.3"));
        assert!(app_version_is_newer_than("v1.6.4", "1.6.3"));
        assert!(app_version_is_newer_than("1.6.4-ni.1", "1.6.3-ni.999"));
        assert!(!app_version_is_newer_than("1.6.3-ni.999", "1.6.4"));

        // Same base: bare base is the FIRST release of that base, any suffix is later.
        assert!(app_version_is_newer_than("1.6.3-ni.1", "1.6.3"));
        assert!(!app_version_is_newer_than("1.6.3", "1.6.3-ni.1"));

        // Same base + same schema: numeric on the trailing sequence.
        assert!(app_version_is_newer_than("1.6.3-ni.28", "1.6.3-ni.27"));
        assert!(!app_version_is_newer_than("1.6.3-ni.27", "1.6.3-ni.28"));

        // Same base + cross schema: ni > dev.
        assert!(app_version_is_newer_than("1.6.3-ni.1", "1.6.3-dev.42"));
        assert!(!app_version_is_newer_than("1.6.3-dev.42", "1.6.3-ni.1"));

        // Equal / older / unparseable.
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
                        { "version": "9999.9.7-ni.10", "changelog": "older test" },
                        { "version": "9999.9.9-ni.1", "changelog": "latest test" },
                        { "version": "9999.9.8", "changelog": "older stable" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let manual = latest_manual_app_update(&manifest).unwrap();
        assert_eq!(manual.version, "9999.9.9-ni.1");
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
                        { "version": "9999.9.7-ni.10", "changelog": "older test" },
                        { "version": "9999.9.9-ni.1", "changelog": "latest test" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest, "");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].channel, "auto");
        assert_eq!(candidates[0].version, "9999.9.8");
        // Aggregated: the target version (synthesized from the top-level
        // fields, absent from manual_versions here) plus the skipped one.
        assert_eq!(
            candidates[0].changelog,
            "**v9999.9.8**\n\nstable update\n\n**v9999.9.7-ni.10**\n\nolder test"
        );
        assert!(!candidates[0].changelog_truncated);
        assert_eq!(candidates[1].channel, "manual");
        assert_eq!(candidates[1].version, "9999.9.9-ni.1");
        assert_eq!(
            candidates[1].changelog,
            "**v9999.9.9-ni.1**\n\nlatest test\n\n**v9999.9.7-ni.10**\n\nolder test"
        );
        assert!(!candidates[1].changelog_truncated);
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

        let candidates = app_update_candidates(&manifest, "");
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

        assert!(app_update_candidates(&manifest, "").is_empty());
    }

    // ---- release-notes-i18n: pick_changelog fallback chain ----

    #[test]
    fn pick_changelog_uses_base_lang_match() {
        let mut map = BTreeMap::new();
        map.insert("zh".to_owned(), "中文".to_owned());
        map.insert("en".to_owned(), "English".to_owned());
        map.insert("ja".to_owned(), "日本語".to_owned());
        assert_eq!(pick_changelog("legacy", &map, "zh_CN"), "中文");
        assert_eq!(pick_changelog("legacy", &map, "ja_JP"), "日本語");
        assert_eq!(pick_changelog("legacy", &map, "en_US"), "English");
    }

    #[test]
    fn pick_changelog_base_lang_handles_dash_separator() {
        let mut map = BTreeMap::new();
        map.insert("pt".to_owned(), "Português".to_owned());
        // BCP-47 "pt-BR" should land on "pt" just like POSIX "pt_BR".
        assert_eq!(pick_changelog("legacy", &map, "pt-BR"), "Português");
    }

    #[test]
    fn pick_changelog_falls_back_to_en_then_zh_then_first() {
        // cs locale -> no cs key, fallback to en.
        let mut map = BTreeMap::new();
        map.insert("zh".to_owned(), "中文".to_owned());
        map.insert("en".to_owned(), "English".to_owned());
        map.insert("ja".to_owned(), "日本語".to_owned());
        assert_eq!(pick_changelog("legacy", &map, "cs"), "English");

        // No en, but zh exists -> fall back to Chinese.
        let mut map_no_en = BTreeMap::new();
        map_no_en.insert("zh".to_owned(), "中文".to_owned());
        map_no_en.insert("ja".to_owned(), "日本語".to_owned());
        assert_eq!(pick_changelog("legacy", &map_no_en, "fr_FR"), "中文");

        // No en, no zh -> first entry by BTreeMap key order.
        let mut map_only_obscure = BTreeMap::new();
        map_only_obscure.insert("ja".to_owned(), "日本語".to_owned());
        map_only_obscure.insert("ko".to_owned(), "한국어".to_owned());
        assert_eq!(
            pick_changelog("legacy", &map_only_obscure, "de"),
            "日本語"
        );
    }

    #[test]
    fn pick_changelog_falls_back_to_legacy_when_map_empty() {
        let empty: BTreeMap<String, String> = BTreeMap::new();
        assert_eq!(pick_changelog("legacy text", &empty, "zh_CN"), "legacy text");
        assert_eq!(pick_changelog("legacy text", &empty, ""), "legacy text");
    }

    #[test]
    fn pick_changelog_handles_empty_locale_with_map() {
        // Empty locale skips the base-lang step but en/zh fallbacks still apply.
        let mut map = BTreeMap::new();
        map.insert("zh".to_owned(), "中文".to_owned());
        map.insert("en".to_owned(), "English".to_owned());
        assert_eq!(pick_changelog("legacy", &map, ""), "English");
    }

    #[test]
    fn pick_changelog_single_chinese_for_english_locale() {
        // Spec scenario: en client falls back to zh when only zh present.
        let mut map = BTreeMap::new();
        map.insert("zh".to_owned(), "中文".to_owned());
        assert_eq!(pick_changelog("legacy", &map, "en_US"), "中文");
    }

    #[test]
    fn pick_changelog_full_9_language_map_resolves_correctly() {
        let mut map = BTreeMap::new();
        for code in ["zh", "en", "ja", "ko", "de", "fr", "es", "ru", "pt"] {
            map.insert(code.to_owned(), format!("text-{code}"));
        }
        for (locale, expected) in [
            ("zh_CN", "text-zh"),
            ("zh_TW", "text-zh"),
            ("en_US", "text-en"),
            ("en_GB", "text-en"),
            ("ja_JP", "text-ja"),
            ("ko_KR", "text-ko"),
            ("de_DE", "text-de"),
            ("fr_FR", "text-fr"),
            ("es_ES", "text-es"),
            ("es_MX", "text-es"),
            ("ru_RU", "text-ru"),
            ("pt_PT", "text-pt"),
            ("pt_BR", "text-pt"),
            ("cs", "text-en"),
            ("da", "text-en"),
            ("fi", "text-en"),
            ("nb", "text-en"),
        ] {
            assert_eq!(
                pick_changelog("legacy", &map, locale),
                expected,
                "locale={locale}"
            );
        }
    }

    #[test]
    fn app_update_candidates_picks_locale_aware_changelog() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.8",
                    "changelog": "legacy stable",
                    "changelogs": {
                        "zh": "稳定版更新",
                        "en": "Stable update",
                        "ja": "安定版アップデート"
                    },
                    "manual_versions": [
                        {
                            "version": "9999.9.9-ni.1",
                            "changelog": "legacy manual",
                            "changelogs": {
                                "zh": "测试版",
                                "en": "Manual test",
                                "ja": "テスト版"
                            }
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        let zh = app_update_candidates(&manifest, "zh_CN");
        assert_eq!(zh.len(), 2);
        assert_eq!(zh[0].changelog, "稳定版更新");
        assert_eq!(zh[1].changelog, "测试版");

        let ja = app_update_candidates(&manifest, "ja_JP");
        assert_eq!(ja[0].changelog, "安定版アップデート");
        assert_eq!(ja[1].changelog, "テスト版");

        let de = app_update_candidates(&manifest, "de_DE");
        // No de in map -> fallback to en.
        assert_eq!(de[0].changelog, "Stable update");
        assert_eq!(de[1].changelog, "Manual test");
    }

    #[test]
    fn app_update_candidates_legacy_manifest_without_changelogs() {
        // Manifest pre-i18n: only `changelog` string, no `changelogs` field.
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.8",
                    "changelog": "legacy single",
                    "manual_versions": [
                        { "version": "9999.9.9-ni.1", "changelog": "legacy manual" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest, "ja_JP");
        assert_eq!(candidates[0].changelog, "legacy single");
        assert_eq!(candidates[1].changelog, "legacy manual");
    }

    // ---- changelog-history-page-and-cumulative-notes: aggregation ----

    #[test]
    fn aggregated_changelog_spans_skipped_versions_with_headings() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.8",
                    "changelog": "top-level stable",
                    "manual_versions": [
                        { "version": "9999.9.6", "changelog": "notes six" },
                        { "version": "9999.9.8", "changelog": "notes eight" },
                        { "version": "9999.9.7", "changelog": "notes seven" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest, "");
        assert_eq!(candidates[0].channel, "auto");
        // Descending by version, each section headed by its version; the
        // target's own entry from manual_versions wins over the top-level
        // fallback (no synthesis when the entry exists).
        assert_eq!(
            candidates[0].changelog,
            "**v9999.9.8**\n\nnotes eight\n\n**v9999.9.7**\n\nnotes seven\n\n**v9999.9.6**\n\nnotes six"
        );
        assert!(!candidates[0].changelog_truncated);
    }

    #[test]
    fn aggregated_changelog_single_version_has_no_heading() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.8",
                    "changelog": "top-level stable",
                    "manual_versions": [
                        { "version": "9999.9.8", "changelog": "only entry" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest, "");
        assert_eq!(candidates[0].changelog, "only entry");
        assert!(!candidates[0].changelog_truncated);
    }

    #[test]
    fn aggregated_changelog_caps_at_five_and_sets_truncated() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.7",
                    "manual_versions": [
                        { "version": "9999.9.1", "changelog": "one" },
                        { "version": "9999.9.2", "changelog": "two" },
                        { "version": "9999.9.3", "changelog": "three" },
                        { "version": "9999.9.4", "changelog": "four" },
                        { "version": "9999.9.5", "changelog": "five" },
                        { "version": "9999.9.6", "changelog": "six" },
                        { "version": "9999.9.7", "changelog": "seven" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest, "");
        let text = &candidates[0].changelog;
        assert!(candidates[0].changelog_truncated);
        // Newest five survive, oldest two are dropped by the cap.
        for kept in ["**v9999.9.7**", "**v9999.9.6**", "**v9999.9.5**", "**v9999.9.4**", "**v9999.9.3**"] {
            assert!(text.contains(kept), "missing {kept} in {text}");
        }
        assert!(!text.contains("**v9999.9.2**"));
        assert!(!text.contains("**v9999.9.1**"));
    }

    #[test]
    fn aggregated_changelog_skips_empty_entries_without_consuming_cap() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.6",
                    "manual_versions": [
                        { "version": "9999.9.1", "changelog": "one" },
                        { "version": "9999.9.2", "changelog": "  " },
                        { "version": "9999.9.3", "changelog": "three" },
                        { "version": "9999.9.4", "changelog": "four" },
                        { "version": "9999.9.5", "changelog": "five" },
                        { "version": "9999.9.6", "changelog": "six" }
                    ]
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest, "");
        let text = &candidates[0].changelog;
        // The blank 9999.9.2 doesn't consume a slot: five non-empty
        // sections remain and nothing was truncated.
        assert!(!candidates[0].changelog_truncated);
        for kept in ["**v9999.9.6**", "**v9999.9.5**", "**v9999.9.4**", "**v9999.9.3**", "**v9999.9.1**"] {
            assert!(text.contains(kept), "missing {kept} in {text}");
        }
        assert!(!text.contains("**v9999.9.2**"));
    }

    #[test]
    fn aggregated_changelog_falls_back_when_manual_versions_empty() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.8",
                    "changelog": "top-level only"
                }
            }"#,
        )
        .unwrap();

        let candidates = app_update_candidates(&manifest, "");
        assert_eq!(candidates[0].changelog, "top-level only");
        assert!(!candidates[0].changelog_truncated);
    }

    #[test]
    fn aggregated_changelog_resolves_locale_per_version() {
        let manifest: Manifest = serde_json::from_str(
            r#"{
                "app": {
                    "version": "9999.9.8",
                    "manual_versions": [
                        {
                            "version": "9999.9.7",
                            "changelog": "legacy seven",
                            "changelogs": { "zh": "七中文", "en": "seven en" }
                        },
                        {
                            "version": "9999.9.8",
                            "changelog": "legacy eight",
                            "changelogs": { "ja": "八日本語", "en": "eight en" }
                        }
                    ]
                }
            }"#,
        )
        .unwrap();

        // ja_JP: the 9999.9.8 entry has ja, the 9999.9.7 one falls back to en.
        let candidates = app_update_candidates(&manifest, "ja_JP");
        assert_eq!(
            candidates[0].changelog,
            "**v9999.9.8**\n\n八日本語\n\n**v9999.9.7**\n\nseven en"
        );
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
                            "version": "9.9.9-ni.1",
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

#[cfg(test)]
mod release_automation_tests {
    use std::{fs, path::PathBuf, process::Command};

    fn repo_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
    }

    fn read_repo_file(path: &str) -> String {
        fs::read_to_string(repo_root().join(path)).unwrap()
    }

    fn run_script(program: &str, script: &str) {
        let eval_arg = if program == "node" { "-e" } else { "-c" };
        let output = Command::new(program)
            .arg(eval_arg)
            .arg(script)
            .current_dir(repo_root())
            .output()
            .unwrap_or_else(|err| panic!("failed to run {program}: {err}"));
        assert!(
            output.status.success(),
            "{program} script failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn release_automation_workflow_publishes_android_apk() {
        let workflow = read_repo_file(".github/workflows/release.yml");

        assert!(workflow.contains("{ os: windows-2022,  type: android }"));
        assert!(workflow.contains("matrix.targets.type == 'android'"));
        assert!(workflow.contains("name: Upload Android package (release)"));
        assert!(workflow.contains("uses: actions/upload-artifact@v7"));
        assert!(workflow.contains("name: gyroflow-niyien.apk"));
        assert!(workflow.contains("path: _deployment/_binaries/gyroflow-niyien.apk"));
        assert!(workflow.contains("archive: false"));
        assert!(workflow.contains("name: Upload Android package (nightly)"));
        assert!(workflow.contains("uses: actions/upload-artifact@v4"));
        assert!(workflow.contains("name: gyroflow-niyien-android"));
        assert!(workflow.contains("./release_artifacts/gyroflow-niyien.apk"));
        assert!(!workflow.contains("linux/android"));
    }

    #[test]
    fn release_automation_android_deploy_restores_sources_and_requires_apk() {
        let script = read_repo_file("_scripts/android.just");

        assert!(script.contains("function Restore-TemporarySourceEdits"));
        assert!(script.contains("try {"));
        assert!(script.contains("finally {"));
        assert!(script.contains("function Assert-NativeCommandSucceeded"));
        assert!(script.contains("src\\ui\\components\\Modal.qml"));
        assert!(script.contains("Cargo.toml"));
        assert!(script.contains("_deployment\\android\\AndroidManifest.xml"));
        assert!(script.contains("[System.IO.File]::ReadAllText($CargoTomlPath) -ne $OriginalCargoToml"));
        assert!(script.contains("[System.IO.File]::ReadAllText($ModalQmlPath) -ne $OriginalModalQml"));
        assert!(
            script.contains("[System.IO.File]::ReadAllText($AndroidManifestPath) -ne $OriginalAndroidManifest")
        );
        assert!(script.contains("function Require-AndroidReleaseSigning"));
        assert!(script.contains("function Set-CargoApkDebugSigningEnvironment"));
        assert!(script.contains("KEY_STORE_PATH"));
        assert!(script.contains("KEY_STORE_ALIAS"));
        assert!(script.contains("KEY_STORE_PASS"));
        assert!(script.contains("$HasReleaseSigningEnv = $Env:KEY_STORE_PATH -and $Env:KEY_STORE_ALIAS -and $Env:KEY_STORE_PASS"));
        assert!(script.contains("cargo apk only needs a signing key for the intermediate build."));
        assert!(script.contains("CARGO_APK_${ProfileEnv}_KEYSTORE"));
        assert!(script.contains(".android\\debug.keystore"));
        assert!(script.contains("GITHUB_REF_TYPE"));
        assert!(script.contains("BUILD_PROFILE"));
        assert!(script.contains("$CargoApkProfile = $Env:BUILD_PROFILE"));
        assert!(script.contains("cargo apk build --profile $CargoApkProfile"));
        assert!(script.contains("networkTimeout=120000"));
        assert!(script.contains("Assert-NativeCommandSucceeded \"cargo apk build\""));
        assert!(script.contains("Assert-NativeCommandSucceeded \"androiddeployqt apk\""));
        assert!(script.contains("Expected APK was not produced"));
        assert!(!script.contains("{{ArtifactPrefix}}.apk\" -Force -ErrorAction SilentlyContinue"));
    }

    #[test]
    fn release_automation_publish_script_derives_android_asset_and_wrapper() {
        run_script(
            "python",
            r#"
from pathlib import Path
from _scripts import publish_pan123_release as publish

workflow = Path(".github/workflows/release.yml").read_text(encoding="utf-8")
required = publish.derive_required_app_asset_names(workflow_text=workflow)
assert "gyroflow-niyien.apk" in required, required
assert publish.pan123_remote_name_for("gyroflow-niyien.apk") == "gyroflow-niyien-android.zip"
"#,
        );
    }

    #[test]
    fn release_automation_summary_records_android_package_metadata() {
        run_script(
            "python",
            r#"
import hashlib
from pathlib import Path
from _scripts import publish_pan123_release as publish

apk_content_source = Path("openspec/changes/distribution-restore-linux-android-ci/proposal.md")
payload = apk_content_source.read_bytes()
packages = publish.build_app_packages_metadata({"gyroflow-niyien.apk": apk_content_source})

android = packages["android"]
assert android["kind"] == "apk", android
assert android["package_filename"] == "gyroflow-niyien-android.zip", android
assert android["package_sha256"] == hashlib.sha256(payload).hexdigest(), android
assert android["package_size"] == len(payload), android
"#,
        );
    }

    #[test]
    fn release_automation_control_center_inventory_uses_android_remote_wrapper() {
        run_script(
            "python",
            r#"
from distribution.control_center.backend import api

assert "gyroflow-niyien-android.zip" in api.EXPECTED_APP_ASSETS, api.EXPECTED_APP_ASSETS
assert "gyroflow-niyien.apk" not in api.EXPECTED_APP_ASSETS, api.EXPECTED_APP_ASSETS
"#,
        );
    }

    #[test]
    fn release_automation_manifest_release_urls_ignore_pan123_wrapper_filenames() {
        run_script(
            "node",
            r#"
const { buildPlatformPackage } = require('./api/_distribution');

const req = { headers: {}, socket: {} };
const source = { region: 'global', base: 'https://github.com/NiYien/gyroflow/releases/download' };
const entry = {
  tag: 'v9.9.9',
  packages: {
    windows: {
      kind: 'web_installer_zip',
      installer_filename: 'gyroflow-niyien-win-setup.zip',
      package_filename: 'gyroflow-niyien-windows64.zip'
    },
    android: {
      kind: 'apk',
      package_filename: 'gyroflow-niyien-android.zip'
    }
  }
};

const windows = buildPlatformPackage(req, entry, source, 'windows');
if (!windows.installer_url.endsWith('/v9.9.9/gyroflow-niyien-windows64-setup.exe')) {
  throw new Error(`windows installer_url=${windows.installer_url}`);
}
if (!windows.package_url.endsWith('/v9.9.9/gyroflow-niyien-windows64.zip')) {
  throw new Error(`windows package_url=${windows.package_url}`);
}

const android = buildPlatformPackage(req, entry, source, 'android');
if (android.installer_url) {
  throw new Error(`android installer_url=${android.installer_url}`);
}
if (!android.package_url.endsWith('/v9.9.9/gyroflow-niyien.apk')) {
  throw new Error(`android package_url=${android.package_url}`);
}
if (android.package_url.endsWith('/gyroflow-niyien-android.zip')) {
  throw new Error(`android release URL used Pan123 wrapper: ${android.package_url}`);
}
"#,
        );
    }

    #[test]
    fn release_automation_manifest_android_is_package_only() {
        run_script(
            "node",
            r#"
const handler = require('./api/manifest');

process.env.NIYIEN_RELEASE_POLICY_JSON = JSON.stringify({
  auto_version: '9.9.9',
  versions: [{
    version: '9.9.9',
    tag: 'v9.9.9',
    channels: ['auto', 'manual'],
    packages: {
      android: {
        kind: 'apk',
        package_filename: 'gyroflow-niyien-android.zip',
        package_sha256: 'a'.repeat(64),
        package_size: 123
      }
    }
  }]
});
process.env.NIYIEN_LENS_DISABLED = '1';
process.env.NIYIEN_PLUGINS_DISABLED = '1';
process.env.NIYIEN_SDK_DISABLED = '1';

const req = {
  query: { country: 'CN', platform: 'android' },
  headers: { host: 'www.niyien.com', 'x-forwarded-proto': 'https' },
  socket: {}
};

function callManifest(country) {
  const request = { ...req, query: { country, platform: 'android' } };
  const response = {
    setHeader() {},
    status() { return this; },
    json(payload) { this.payload = payload; }
  };
  return handler(request, response).then(() => response.payload);
}

Promise.all([callManifest('CN'), callManifest('US')]).then(([cn, global]) => {
  const android = cn.app.packages.android;
  if (android.kind !== 'apk') throw new Error(`kind=${android.kind}`);
  if ('installer_url' in android && android.installer_url) {
    throw new Error(`installer_url=${android.installer_url}`);
  }
  if (cn.app.url !== android.package_url) {
    throw new Error(`url=${cn.app.url}, package_url=${android.package_url}`);
  }
  if (!android.package_url.includes('/gyroflow-niyien-android.zip')) {
    throw new Error(`package_url=${android.package_url}`);
  }
  const globalAndroid = global.app.packages.android;
  if (global.app.url !== globalAndroid.package_url) {
    throw new Error(`global url=${global.app.url}, package_url=${globalAndroid.package_url}`);
  }
  if (!globalAndroid.package_url.endsWith('/gyroflow-niyien.apk')) {
    throw new Error(`global package_url=${globalAndroid.package_url}`);
  }
  if (globalAndroid.package_url.endsWith('/gyroflow-niyien-android.zip')) {
    throw new Error(`global package_url uses wrapper: ${globalAndroid.package_url}`);
  }
}).catch((err) => {
  console.error(err);
  process.exit(1);
});
"#,
        );
    }
}
