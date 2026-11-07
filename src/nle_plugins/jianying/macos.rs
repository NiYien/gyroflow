// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;

const HOST_ID: &str = "com.lemon.lvpro";
const HOST_LIBRARY_SHA256: &str = "8db71da2f613404b3c6ad5edd91d33af4ad877b54aa52dec3730d6cf3922d503";

fn home() -> io::Result<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "HOME is unavailable"))
}

pub(super) fn user_root() -> io::Result<PathBuf> {
    Ok(home()?.join("Movies/JianyingPro"))
}

pub(super) fn backup_root() -> io::Result<PathBuf> {
    Ok(home()?.join("Library/Application Support/GyroflowNiYien/Plugin Backups/jianying"))
}

pub(super) fn local_candidate_version(bundle: &Path) -> Option<String> {
    // Version discovery does not promote a local candidate into a formal installable package.
    let manifest: PackageManifest = serde_json::from_slice(&std::fs::read(bundle.join(MANIFEST)).ok()?).ok()?;
    if manifest.schema_version != 1 || manifest.plugin_id != PLUGIN_ID
        || manifest.platform.as_deref() != Some("macos") || manifest.architecture.as_deref() != Some("arm64")
        || manifest.candidate != Some(true) || manifest.version.trim().is_empty()
        || !bundle.join(WRAPPER).is_file() || !bundle.join("Contents/MacOS/libgyroflow_ofx.dylib").is_file()
        || std::fs::read_to_string(bundle.join(VERSION_FILE)).ok()?.trim() != manifest.version
        || plist_string(&bundle.join("Contents/Info.plist"), "CFBundleIdentifier").ok()? != PLUGIN_ID
        || plist_string(&bundle.join("Contents/Info.plist"), "NiYienPackageVersion").ok()? != manifest.version {
        return None;
    }
    Some(manifest.version)
}

fn inspect_host(app: &Path, root: PathBuf, expected_library: &str) -> io::Result<Host> {
    let info = app.join("Contents/Info.plist");
    if plist_string(&info, "CFBundleIdentifier")? != HOST_ID {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "Unexpected Jianying application identity"));
    }
    let version = plist_string(&info, "CFBundleShortVersionString")?;
    if parse_numeric_dotted_version(&version).is_none()
        || format!("{:x}", Sha256::digest(std::fs::read(app.join("Contents/Frameworks/libcccreator.dylib"))?))
            != expected_library
    {
        return Err(io::Error::new(io::ErrorKind::Unsupported,
            "This Jianying build has not been verified for the macOS plugin"));
    }
    Ok(Host { version, root })
}

pub(super) fn host() -> io::Result<Host> {
    if !platform_supported() {
        return Err(io::Error::new(io::ErrorKind::Unsupported,
            "Jianying macOS requires native Apple Silicon"));
    }
    let candidates = [PathBuf::from("/Applications/VideoFusion-macOS.app"),
        home()?.join("Applications/VideoFusion-macOS.app")];
    let app = candidates.iter().find(|p| p.is_dir()).ok_or_else(||
        io::Error::new(io::ErrorKind::NotFound, "Jianying is not installed"))?;
    inspect_host(app, user_root()?, HOST_LIBRARY_SHA256)
}

fn editor_process(line: &str) -> bool {
    line.contains("/VideoFusion-macOS.app/")
        && !line.ends_with("/VideoFusion-macOSTrayHelper.app/Contents/MacOS/VideoFusion-macOSTrayHelper")
}

pub(super) fn ensure_host_closed() -> io::Result<()> {
    let output = Command::new("/bin/ps").args(["-axww", "-o", "pid=,comm="]).output()?;
    let rows = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || rows.lines().count() < 20 {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied,
            "Unable to verify whether Jianying is closed"));
    }
    for line in rows.lines() {
        if editor_process(line) {
            return Err(io::Error::new(io::ErrorKind::PermissionDenied, "PLUGIN_COPY_BLOCKED:jianying"));
        }
        if line.contains("/VideoFusion-macOSTrayHelper.app/") {
            let pid = line.split_whitespace().next().unwrap_or_default();
            let opened = Command::new("/usr/sbin/lsof").args(["-nP", "-p", pid, "-Fn"]).output()?;
            if !opened.status.success() || String::from_utf8_lossy(&opened.stdout).lines()
                .any(|s| s.contains("/OFX/") || s.contains(".ofx") || s.contains("libcccreator"))
            {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, "PLUGIN_COPY_BLOCKED:jianying"));
            }
        }
    }
    Ok(())
}

pub(super) fn verify_trust(bundle: &Path) -> io::Result<()> {
    let invalid = || io::Error::new(io::ErrorKind::InvalidData,
        "Jianying macOS package must have a valid NiYien Developer ID signature");
    let info = bundle.join("Contents/Info.plist");
    if plist_string(&info, "CFBundleIdentifier")? != PLUGIN_ID
        || plist_string(&info, "CFBundleExecutable")? != "GyroflowJianyingBatch11.ofx" {
        return Err(invalid());
    }
    let status = Command::new("/usr/bin/codesign")
        .args(["--verify", "--deep", "--strict", "--all-architectures"]).arg(bundle).status()?;
    if !status.success() { return Err(invalid()); }
    for path in [bundle.to_path_buf(), bundle.join(CORE)] {
        let requirement = format!(
            "=anchor apple generic and certificate leaf[subject.OU] = \"{FINALCUT_TEAM_ID}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists");
        if !Command::new("/usr/bin/codesign")
            .args(["--verify", "--strict", "--all-architectures", "-R", &requirement])
            .arg(&path).status()?.success() {
            return Err(invalid());
        }
        let details = Command::new("/usr/bin/codesign").args(["-d", "--verbose=4"]).arg(&path).output()?;
        let text = String::from_utf8_lossy(&details.stderr);
        if !details.status.success()
            || !text.lines().any(|line| line == format!("TeamIdentifier={FINALCUT_TEAM_ID}"))
            || !text.lines().any(|line| line.starts_with("Authority=Developer ID Application:"))
        {
            return Err(invalid());
        }
    }
    for path in [bundle.join(WRAPPER), bundle.join(CORE)] {
        if !Command::new("/usr/bin/lipo").arg(path).args(["-verify_arch", "arm64"]).status()?.success() {
            return Err(invalid());
        }
    }
    Ok(())
}

pub(super) fn validate_archive(archive: &mut zip::ZipArchive<Cursor<Vec<u8>>>) -> io::Result<()> {
    let mut names = std::collections::HashSet::new();
    let mut bytes = 0u64;
    for i in 0..archive.len() {
        let file = archive.by_index(i)?;
        let name = file.name();
        bytes = bytes.checked_add(file.size()).ok_or_else(||
            io::Error::new(io::ErrorKind::InvalidData, "Jianying archive size overflow"))?;
        if !names.insert(name.to_owned()) || name.contains('\\') || file.enclosed_name().is_none()
            || name.split('/').any(|s| s.is_empty() || s == "." || s == "..")
            || !(name.starts_with(&format!("{BUNDLE_NAME}/")) || name == "delivery-manifest.json")
            || file.unix_mode().map(|mode| mode & 0o170000 == 0o120000).unwrap_or(false)
            || bytes > 512 * 1024 * 1024
        {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid Jianying macOS archive"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_gate_blocks_editors_but_allows_only_idle_tray() {
        assert!(editor_process("123 /Applications/VideoFusion-macOS.app/Contents/MacOS/VideoFusion-macOS"));
        assert!(editor_process("124 /Applications/VideoFusion-macOS.app/Contents/Frameworks/VideoFusion-macOS.app/Contents/MacOS/VideoFusion-macOS"));
        assert!(!editor_process("125 /Applications/VideoFusion-macOS.app/Contents/Frameworks/VideoFusion-macOSTrayHelper.app/Contents/MacOS/VideoFusion-macOSTrayHelper"));
        assert!(!editor_process("126 /Applications/Other.app/Contents/MacOS/Other"));
    }

    #[test]
    fn host_identity_checks_library_content_not_version_alone() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("VideoFusion-macOS.app");
        std::fs::create_dir_all(app.join("Contents/Frameworks")).unwrap();
        std::fs::write(app.join("Contents/Info.plist"), br#"<?xml version="1.0"?><plist><dict>
          <key>CFBundleIdentifier</key><string>com.lemon.lvpro</string>
          <key>CFBundleShortVersionString</key><string>11.7.0</string></dict></plist>"#).unwrap();
        std::fs::write(app.join("Contents/Frameworks/libcccreator.dylib"), b"owned fixture").unwrap();
        let expected = format!("{:x}", Sha256::digest(b"owned fixture"));
        assert_eq!(inspect_host(&app, temp.path().to_owned(), &expected).unwrap().version, "11.7.0");
        assert!(inspect_host(&app, temp.path().to_owned(), HOST_LIBRARY_SHA256).is_err());
    }

    #[test]
    fn archive_gate_rejects_escape_and_ambiguous_paths() {
        for name in ["../escape", "/absolute", "other/file", "delivery-manifest.json/../escape",
                     "GyroflowJianyingBatch11.ofx.bundle//Contents/core"] {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            writer.start_file(name, zip::write::SimpleFileOptions::default()).unwrap();
            std::io::Write::write_all(&mut writer, b"x").unwrap();
            let data = writer.finish().unwrap().into_inner();
            assert!(validate_archive(&mut zip::ZipArchive::new(Cursor::new(data)).unwrap()).is_err());
        }
    }

    #[test]
    fn local_candidate_version_is_discovered_without_accepting_it_for_installation() {
        let temporary = tempfile::tempdir().unwrap();
        let bundle = temporary.path();
        std::fs::create_dir_all(bundle.join("Contents/Resources")).unwrap();
        std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        std::fs::write(bundle.join(WRAPPER), b"owned wrapper").unwrap();
        std::fs::write(bundle.join("Contents/MacOS/libgyroflow_ofx.dylib"), b"owned core").unwrap();
        std::fs::write(bundle.join(VERSION_FILE), "2.1.2-macos-candidate.14\n").unwrap();
        std::fs::write(bundle.join(MANIFEST), serde_json::to_vec(&serde_json::json!({
            "schema_version":1, "plugin_id":PLUGIN_ID, "version":"2.1.2-macos-candidate.14",
            "platform":"macos", "architecture":"arm64", "candidate":true, "files":[]
        })).unwrap()).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), format!(
            "<?xml version=\"1.0\"?><plist><dict><key>CFBundleIdentifier</key><string>{PLUGIN_ID}</string>\
             <key>NiYienPackageVersion</key><string>2.1.2-macos-candidate.14</string></dict></plist>")).unwrap();
        assert_eq!(local_candidate_version(bundle).as_deref(), Some("2.1.2-macos-candidate.14"));
        assert!(validate_bundle(&bundle).is_err());
    }
}
