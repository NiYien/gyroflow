// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize)]
pub struct DistributionConfig {
    pub brand: BrandConfig,
    pub release: ReleaseConfig,
    pub endpoints: EndpointsConfig,
    pub data: DataConfig,
    pub sources: SourceMap,
    pub routing: RoutingConfig,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BrandConfig {
    pub display_name: String,
    pub artifact_prefix: String,
    pub organization_name: String,
    pub application_name: String,
    pub organization_domain: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ReleaseConfig {
    pub github_owner: String,
    pub github_repo: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct EndpointsConfig {
    pub manifest_api: String,
    pub telemetry_api: String,
    pub firmware_manifest: String,
    pub firmware_base: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DataConfig {
    pub lens: DataPackageConfig,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DataPackageConfig {
    pub source_dir: String,
    pub asset_name: String,
    pub install_dir: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RoutingConfig {
    pub cn_countries: Vec<String>,
}

pub type SourceMap = HashMap<String, SourceConfig>;

#[derive(Clone, Debug, Deserialize)]
pub struct SourceConfig {
    pub base: String,
}

pub fn config() -> &'static DistributionConfig {
    static CONFIG: std::sync::OnceLock<DistributionConfig> = std::sync::OnceLock::new();
    CONFIG.get_or_init(load_config)
}

fn load_config() -> DistributionConfig {
    let from_env = std::env::var("GYROFLOW_DISTRIBUTION_CONFIG")
        .ok()
        .and_then(|path| std::fs::read_to_string(path).ok());
    let raw = from_env.unwrap_or_else(|| {
        include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../distribution/niyien.toml"
        ))
        .to_owned()
    });
    toml::from_str(&raw).unwrap_or_else(|err| {
        panic!("Failed to parse distribution/niyien.toml: {err}");
    })
}

pub fn package_config(name: &str) -> Option<&'static DataPackageConfig> {
    match name {
        "lens" => Some(&config().data.lens),
        _ => None,
    }
}

pub fn manifest_api() -> &'static str {
    config().endpoints.manifest_api.as_str()
}

pub fn telemetry_api() -> &'static str {
    config().endpoints.telemetry_api.as_str()
}

pub fn package_install_root(name: &str) -> Option<PathBuf> {
    package_config(name).map(|pkg| crate::settings::data_dir().join(&pkg.install_dir))
}

pub fn package_versions_root(name: &str) -> Option<PathBuf> {
    package_install_root(name).map(|root| root.join("versions"))
}

pub fn installed_package_version(name: &str) -> u64 {
    crate::settings::get_u64(&format!("distribution.package_version.{name}"), 0)
}

pub fn set_installed_package_version(name: &str, version: u64) {
    crate::settings::set(
        &format!("distribution.package_version.{name}"),
        serde_json::Value::from(version),
    );
}

pub fn current_package_dir(name: &str) -> Option<PathBuf> {
    let root = package_versions_root(name)?;
    let version = installed_package_version(name);
    if version == 0 {
        return None;
    }
    let path = root.join(version.to_string());
    path.is_dir().then_some(path)
}

pub fn bundled_package_dir(name: &str) -> Option<PathBuf> {
    let pkg = package_config(name)?;
    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let candidates = [
        exe_dir.join(&pkg.install_dir),
        exe_dir.join("resources").join(&pkg.install_dir),
        exe_dir.join("../Resources").join(&pkg.install_dir),
    ];
    candidates.into_iter().find(|path| dir_has_payload(path))
}

pub fn development_package_dir(name: &str) -> Option<PathBuf> {
    let pkg = package_config(name)?;
    let rel = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../")
        .join(&pkg.source_dir);
    dir_has_payload(&rel).then_some(rel)
}

pub fn resolve_package_dir(name: &str) -> Option<PathBuf> {
    current_package_dir(name)
        .or_else(|| bundled_package_dir(name))
        .or_else(|| development_package_dir(name))
}

pub fn resolve_package_subdir(name: &str, subdir: &str) -> Option<PathBuf> {
    let path = resolve_package_dir(name)?.join(subdir);
    path.is_dir().then_some(path)
}

fn dir_has_payload(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    dir_has_payload_recursive(path)
}

fn dir_has_payload_recursive(path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(path) else {
        return false;
    };
    for entry in entries.flatten() {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            if dir_has_payload_recursive(&entry_path) {
                return true;
            }
            continue;
        }
        if entry.file_name().to_string_lossy().to_ascii_lowercase() != "readme.md" {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::DistributionConfig;

    #[test]
    fn niyien_brand_display_name_drives_window_title_format() {
        let raw_config = include_str!("../../distribution/niyien.toml");
        let config: DistributionConfig = toml::from_str(raw_config).unwrap();
        let display_name = config.brand.display_name.as_str();

        assert_eq!(display_name, "Gyroflow(NiYien)");

        let main_window_qml = include_str!("../ui/main_window.qml");
        assert!(main_window_qml.contains(r#"title: brandDisplayName + " " + version;"#));
        assert!(!main_window_qml.contains(r#"title: "Gyroflow "#));

        assert_eq!(
            format!("{display_name} {}", "1.6.3"),
            "Gyroflow(NiYien) 1.6.3"
        );
        assert_eq!(
            format!("{display_name} {}", "1.6.3(ni42)"),
            "Gyroflow(NiYien) 1.6.3(ni42)"
        );
    }

    #[test]
    fn niyien_active_platform_identifiers_use_niyien_namespace() {
        let mac_plist = include_str!("../../_deployment/mac/Gyroflow.app/Contents/Info.plist");
        assert!(mac_plist.contains("<string>com.niyien.gyroflow</string>"));
        assert!(mac_plist.contains("<key>CFBundleDisplayName</key>                 <string>Gyroflow(NiYien)</string>"));
        assert!(mac_plist.contains("<key>CFBundleName</key>                        <string>Gyroflow(NiYien)</string>"));
        assert!(mac_plist.contains("com.niyien.gyroflow.project"));
        assert!(!mac_plist.contains("xyz.gyroflow"));

        let android_manifest = include_str!("../../_deployment/android/AndroidManifest.xml");
        assert!(android_manifest.contains(r#"package="com.niyien.gyroflow""#));
        assert!(android_manifest.contains(r#"android:name="com.niyien.gyroflow.MainActivity""#));
        assert!(android_manifest.contains(r#"android:label="Gyroflow(NiYien)""#));
        assert!(!android_manifest.contains("xyz.gyroflow"));

        let main_activity = include_str!(
            "../../_deployment/android/src/com/niyien/gyroflow/MainActivity.java"
        );
        assert!(main_activity.contains("package com.niyien.gyroflow;"));

        let util_rs = include_str!("../util.rs");
        assert!(util_rs.contains("Java_com_niyien_gyroflow_MainActivity_urlReceived"));
        assert!(!util_rs.contains("Java_xyz_gyroflow_MainActivity_urlReceived"));

        let android_just = include_str!("../../_scripts/android.just");
        assert!(
            android_just.contains("com.niyien.gyroflow/com.niyien.gyroflow.MainActivity")
        );

        let app_qml = include_str!("../ui/App.qml");
        assert!(
            app_qml.contains("https://play.google.com/store/apps/details?id=com.niyien.gyroflow")
        );
        assert!(app_qml.contains("After the DMG opens, drag Gyroflow(NiYien).app to the Applications folder."));
        assert!(!app_qml.contains("id=xyz.gyroflow"));

        let macos_just = include_str!("../../_scripts/macos.just");
        assert!(macos_just.contains(r#"BundleIdentifier              := "com.niyien.gyroflow""#));
        assert!(macos_just.contains(r#"AppBundleName                 := "Gyroflow(NiYien).app""#));
        assert!(macos_just.contains("_deployment/_binaries/mac/{{AppBundleName}}"));
        assert!(!macos_just.contains("_deployment/_binaries/mac/Gyroflow.app"));
    }
}
