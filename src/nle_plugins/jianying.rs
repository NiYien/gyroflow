// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "macos"))]
pub(super) const ASSET_NAME: &str = "GyroflowNiyien-Jianying-windows.zip";
#[cfg(target_os = "macos")]
pub(super) const ASSET_NAME: &str = "GyroflowNiyien-Jianying-macos.zip";
pub(super) const BUNDLE_NAME: &str = "GyroflowJianyingBatch11.ofx.bundle";
const PLUGIN_ID: &str = "xyz.niyien.gyroflow.jianying.batch11";
#[cfg(not(target_os = "macos"))]
const WRAPPER: &str = "Contents/Win64/GyroflowJianyingBatch11.ofx";
#[cfg(target_os = "macos")]
const WRAPPER: &str = "Contents/MacOS/GyroflowJianyingBatch11.ofx";
#[cfg(not(target_os = "macos"))]
const CORE: &str = "Contents/Win64/gyroflow_ofx.dll";
#[cfg(target_os = "macos")]
const CORE: &str = "Contents/MacOS/libgyroflow_ofx.dylib";
#[cfg(not(target_os = "macos"))]
const MANIFEST: &str = "Contents/package-manifest.json";
#[cfg(target_os = "macos")]
const MANIFEST: &str = "Contents/Resources/package-manifest.json";
#[cfg(not(target_os = "macos"))]
const VERSION_FILE: &str = "Contents/version.txt";
#[cfg(target_os = "macos")]
const VERSION_FILE: &str = "Contents/Resources/version.txt";

pub(super) fn platform_supported() -> bool {
    cfg!(target_os = "windows") || cfg!(all(target_os = "macos", target_arch = "aarch64"))
}

pub(super) struct Host {
    pub version: String,
    root: PathBuf,
}

fn local_root() -> io::Result<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is unavailable"))
}

pub(super) fn host() -> io::Result<Host> {
    #[cfg(target_os = "macos")]
    { return macos::host(); }
    #[cfg(not(target_os = "macos"))]
    find_host(&local_root()?.join("JianyingPro"))
}

fn find_host(root: &Path) -> io::Result<Host> {
    let mut versions = Vec::new();
    if let Ok(entries) = std::fs::read_dir(root.join("Apps")) {
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(version) = parse_numeric_dotted_version(&name) {
                if entry.path().join("JianyingPro.exe").is_file() {
                    versions.push((version, name));
                }
            }
        }
    }
    versions.sort_by(|a, b| b.0.cmp(&a.0));
    let (_, version) = versions.into_iter().next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Jianying is not installed in LOCALAPPDATA/JianyingPro/Apps",
        )
    })?;
    Ok(Host {
        version,
        root: root.to_owned(),
    })
}

fn installed_bundle() -> io::Result<PathBuf> {
    #[cfg(target_os = "macos")]
    { return Ok(macos::user_root()?.join("User Data/Cache/OFX").join(BUNDLE_NAME)); }
    #[cfg(not(target_os = "macos"))]
    Ok(local_root()?
        .join("JianyingPro/User Data/Cache/OFX")
        .join(BUNDLE_NAME))
}

#[derive(Deserialize)]
struct PackageManifest {
    schema_version: u32,
    plugin_id: String,
    version: String,
    files: Vec<PackageFile>,
    #[serde(default)]
    platform: Option<String>,
    #[serde(default)]
    architecture: Option<String>,
    #[serde(default)]
    candidate: Option<bool>,
    #[serde(default)]
    fragment_sampling_policy: Option<String>,
}

#[derive(Deserialize)]
struct PackageFile {
    path: String,
    sha256: String,
}

fn validate_bundle(bundle: &Path) -> io::Result<String> {
    let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidData, message);
    let manifest: PackageManifest = serde_json::from_slice(&std::fs::read(
        bundle.join(MANIFEST),
    )?)
    .map_err(|error| invalid(&format!("Invalid Jianying package manifest: {error}")))?;
    if manifest.schema_version != 1
        || manifest.plugin_id != PLUGIN_ID
        || manifest.version.trim().is_empty()
        || std::fs::read_to_string(bundle.join(VERSION_FILE))?.trim() != manifest.version
    {
        return Err(invalid(
            "Jianying package identity or version does not match",
        ));
    }
    #[cfg(target_os = "macos")]
    if manifest.platform.as_deref() != Some("macos")
        || manifest.architecture.as_deref() != Some("arm64")
        || manifest.candidate != Some(false)
        || manifest.fragment_sampling_policy.as_deref() != Some("core-default")
    {
        return Err(invalid("Jianying macOS requires a formal arm64 package using core-default sampling"));
    }
    let mut listed = std::collections::HashSet::new();
    for file in manifest.files {
        let relative = Path::new(&file.path);
        if file.path.contains('\\')
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            || file.path.is_empty()
            || !listed.insert(file.path.clone())
        {
            return Err(invalid("Invalid or duplicate Jianying package file path"));
        }
        let path = bundle.join(relative);
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink()
            || format!("{:x}", Sha256::digest(std::fs::read(&path)?)) != file.sha256
        {
            return Err(invalid(&format!(
                "Jianying package checksum mismatch: {}",
                file.path
            )));
        }
    }
    if !bundle.join(WRAPPER).is_file() || !listed.contains(CORE) {
        return Err(invalid(
            "Jianying package is missing its wrapper or shared OFX core",
        ));
    }
    #[cfg(not(target_os = "macos"))]
    if !listed.contains(WRAPPER) {
        return Err(invalid("Jianying package does not cover its wrapper"));
    }
    Ok(manifest.version)
}

fn validate_trusted_bundle(bundle: &Path) -> io::Result<String> {
    let version = validate_bundle(bundle)?;
    #[cfg(target_os = "macos")]
    macos::verify_trust(bundle)?;
    Ok(version)
}

pub(super) fn installed_version() -> io::Result<String> {
    let bundle = installed_bundle()?;
    #[cfg(target_os = "macos")]
    if let Some(version) = macos::local_candidate_version(&bundle) {
        return Ok(version);
    }
    if bundle.join(MANIFEST).is_file() {
        return match validate_trusted_bundle(&bundle) {
            Ok(version) => Ok(version),
            Err(error) => {
                log::warn!(target: "plugin", "[nle jianying] installed package needs repair: {error}");
                Ok(String::new())
            }
        };
    }
    // Retain the accepted experimental identity so existing drafts survive upgrades.
    Ok(
        if bundle.join(WRAPPER).is_file()
            && bundle
                .join("Contents/Win64/gyroflow-test7-core.dll")
                .is_file()
        {
            "1.11".to_owned()
        } else {
            String::new()
        },
    )
}

pub(super) fn verify_delivery(
    content: Vec<u8>,
    nightly: bool,
    sha256: &str,
    size: u64,
) -> io::Result<Vec<u8>> {
    let content = if nightly {
        let mut outer = zip::ZipArchive::new(Cursor::new(content))?;
        let mut inner = Vec::new();
        std::io::Read::read_to_end(&mut outer.by_name(ASSET_NAME)?, &mut inner)?;
        inner
    } else {
        content
    };
    if content.len() as u64 != size || format!("{:x}", Sha256::digest(&content)) != sha256 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Jianying download size or SHA-256 does not match the selected release",
        ));
    }
    Ok(content)
}

#[cfg(not(target_os = "macos"))]
fn ensure_host_closed() -> io::Result<()> {
    let output = Command::new("powershell.exe").args([
        "-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
        "$ErrorActionPreference='Stop'; try { $cim=@(Get-CimInstance Win32_Process -Filter \"Name='JianyingPro.exe'\" -ErrorAction Stop); $native=@(Get-Process -Name JianyingPro -ErrorAction SilentlyContinue); if ($cim.Count -gt 0 -or $native.Count -gt 0) { exit 23 }; exit 0 } catch { Write-Error $_; exit 1 }",
    ]).output()?;
    if output.status.code() == Some(23) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "PLUGIN_COPY_BLOCKED:jianying",
        ));
    }
    if !output.status.success() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Unable to check whether Jianying is closed: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        ));
    }
    Ok(())
}

fn replace_bundle(staged: &Path, destination: &Path, backup: &Path) -> io::Result<String> {
    replace_bundle_with(staged, destination, backup, validate_bundle)
}

fn replace_bundle_with(
    staged: &Path, destination: &Path, backup: &Path,
    verify: impl FnOnce(&Path) -> io::Result<String>,
) -> io::Result<String> {
    let had_previous = destination.exists();
    if had_previous {
        std::fs::rename(destination, backup)?;
    }
    let result = std::fs::rename(staged, destination).and_then(|()| verify(destination));
    if let Err(install_error) = &result {
        if destination.exists() {
            std::fs::rename(destination, staged).map_err(|error| {
                io::Error::new(error.kind(), format!(
                    "Jianying installation failed ({install_error}); unable to remove the failed package ({error}); complete previous package: {}",
                    backup.display()
                ))
            })?;
        }
        if had_previous {
            std::fs::rename(backup, destination).map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!(
                        "Jianying installation failed; restore the complete backup at {}: {error}",
                        backup.display()
                    ),
                )
            })?;
        }
    }
    result
}

pub(super) fn install_archive(content: Vec<u8>) -> io::Result<String> {
    let host = host()?;
    #[cfg(target_os = "macos")]
    use macos::ensure_host_closed;
    ensure_host_closed()?;
    let cache = host.root.join("User Data/Cache/OFX");
    std::fs::create_dir_all(&cache)?;
    let stage = tempfile::Builder::new()
        .prefix(".gyroflow-jianying-")
        .tempdir_in(&cache)?;
    let mut archive = zip::ZipArchive::new(Cursor::new(content))?;
    #[cfg(target_os = "macos")]
    macos::validate_archive(&mut archive)?;
    archive.extract(stage.path())?;
    let source = stage.path().join(BUNDLE_NAME);
    validate_trusted_bundle(&source)?;
    #[cfg(not(target_os = "macos"))]
    let backup_root = local_root()?.join("Gyroflow/Plugin Backups/jianying");
    #[cfg(target_os = "macos")]
    let backup_root = macos::backup_root()?;
    std::fs::create_dir_all(&backup_root)?;
    let backup_dir = tempfile::Builder::new()
        .prefix("previous-")
        .tempdir_in(&backup_root)?
        .keep();
    let backup = backup_dir.join(BUNDLE_NAME);
    ensure_host_closed()?;
    let version = replace_bundle_with(&source, &cache.join(BUNDLE_NAME), &backup, validate_trusted_bundle)?;
    log::info!(target: "plugin", "[nle jianying] installed version={version} host={} backup={}", host.version, backup.display());
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn package(root: &Path, version: &str) -> PathBuf {
        let bundle = root.join(BUNDLE_NAME);
        std::fs::create_dir_all(bundle.join(WRAPPER).parent().unwrap()).unwrap();
        std::fs::create_dir_all(bundle.join(MANIFEST).parent().unwrap()).unwrap();
        let mut files = Vec::new();
        for (path, contents) in [
            (WRAPPER, "wrapper"),
            (CORE, "core"),
            (VERSION_FILE, version),
        ] {
            std::fs::write(bundle.join(path), contents).unwrap();
            files.push(serde_json::json!({"path":path,"sha256":format!("{:x}",Sha256::digest(contents.as_bytes()))}));
        }
        std::fs::write(
            bundle.join(MANIFEST),
            serde_json::to_vec(&serde_json::json!({
                "schema_version":1,"plugin_id":PLUGIN_ID,"version":version,"files":files,
                "platform":"macos","architecture":"arm64","candidate":false,
                "fragment_sampling_policy":"core-default"
            }))
            .unwrap(),
        )
        .unwrap();
        bundle
    }

    #[test]
    fn detects_numeric_host_versions_in_install_root_only() {
        let root = tempfile::tempdir().unwrap();
        for version in ["11.8.0.14711", "11.10.0.1", "other"] {
            let app = root.path().join("Apps").join(version);
            std::fs::create_dir_all(&app).unwrap();
            std::fs::write(app.join("JianyingPro.exe"), []).unwrap();
        }
        assert_eq!(find_host(root.path()).unwrap().version, "11.10.0.1");
    }

    #[test]
    fn validates_files_and_rejects_tampered_core() {
        let root = tempfile::tempdir().unwrap();
        let bundle = package(root.path(), "2.1.2-dev");
        assert_eq!(validate_bundle(&bundle).unwrap(), "2.1.2-dev");
        std::fs::write(bundle.join(CORE), b"tampered").unwrap();
        assert!(validate_bundle(&bundle).is_err());
    }

    #[test]
    fn upgrade_keeps_complete_previous_bundle() {
        let root = tempfile::tempdir().unwrap();
        let source_root = root.path().join("stage");
        let source = package(&source_root, "2.1.3");
        let destination = package(root.path(), "2.1.2");
        std::fs::write(destination.join("user-extra"), b"preserve").unwrap();
        let backup = root.path().join("backup");
        assert_eq!(
            replace_bundle(&source, &destination, &backup).unwrap(),
            "2.1.3"
        );
        assert_eq!(
            std::fs::read(backup.join("user-extra")).unwrap(),
            b"preserve"
        );
        assert_eq!(validate_bundle(&backup).unwrap(), "2.1.2");
        assert_eq!(validate_bundle(&destination).unwrap(), "2.1.3");
    }

    #[test]
    fn failed_verification_restores_previous_bundle() {
        let root = tempfile::tempdir().unwrap();
        let source = package(&root.path().join("stage"), "2.1.3");
        std::fs::write(source.join(CORE), b"broken").unwrap();
        let destination = package(root.path(), "2.1.2");
        let backup = root.path().join("backup");
        assert!(replace_bundle(&source, &destination, &backup).is_err());
        assert_eq!(validate_bundle(&destination).unwrap(), "2.1.2");
        assert!(!backup.exists());
    }

    #[test]
    fn delivery_requires_selected_package_hash_and_size() {
        let content = b"release zip".to_vec();
        let hash = format!("{:x}", Sha256::digest(&content));
        assert!(verify_delivery(content.clone(), false, &hash, content.len() as u64).is_ok());
        assert!(verify_delivery(content.clone(), false, &hash, 1).is_err());
        assert!(verify_delivery(content, false, &"0".repeat(64), 11).is_err());
    }

    #[test]
    fn nightly_delivery_checks_the_named_inner_release_zip() {
        let content = b"selected release zip".to_vec();
        let hash = format!("{:x}", Sha256::digest(&content));
        let wrap = |name: &str| {
            let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
            archive.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut archive, &content).unwrap();
            archive.finish().unwrap().into_inner()
        };
        let wrapper = wrap(ASSET_NAME);
        assert_eq!(verify_delivery(wrapper.clone(), true, &hash, content.len() as u64).unwrap(), content);
        assert!(verify_delivery(wrap("wrong-package.zip"), true, &hash, content.len() as u64).is_err());
        assert!(verify_delivery(wrapper.clone(), true, &format!("{:x}", Sha256::digest(&wrapper)), content.len() as u64).is_err());
        assert!(verify_delivery(wrapper, true, &hash, 1).is_err());
    }

    #[test]
    #[ignore = "Requires GYROFLOW_TEST_JIANYING_PACKAGE pointing to an actual distribution ZIP"]
    fn actual_distribution_package_matches_its_manifest() {
        let path = std::env::var_os("GYROFLOW_TEST_JIANYING_PACKAGE").expect("GYROFLOW_TEST_JIANYING_PACKAGE is required");
        let archive = std::fs::File::open(path).unwrap();
        let temporary = tempfile::tempdir().unwrap();
        zip::ZipArchive::new(archive).unwrap().extract(temporary.path()).unwrap();
        let version = validate_bundle(&temporary.path().join(BUNDLE_NAME)).unwrap();
        println!("Validated actual Jianying distribution package: version={version}");
    }
}
