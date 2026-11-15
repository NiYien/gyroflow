// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use ::windows::{
    Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, REG_ROUTINE_FLAGS, RRF_RT_REG_EXPAND_SZ,
        RRF_RT_REG_SZ, RRF_SUBKEY_WOW6432KEY, RRF_SUBKEY_WOW6464KEY, RegGetValueW,
    },
    core::HSTRING,
};
use cpp::cpp;
use qttypes::QString;

cpp! {{
    #include <QSettings>
    #include <QDirIterator>
    #include <QFileInfo>
    #include <QStandardPaths>
}}

const HOST_PATH_SETTING: &str = "nleJianyingHostPath";

fn registry_string(hive: HKEY, key: &str, name: &str, view: REG_ROUTINE_FLAGS) -> Option<String> {
    let key = HSTRING::from(key);
    let name = HSTRING::from(name);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | view;
    let mut bytes = 0;
    unsafe {
        let status = RegGetValueW(hive, &key, &name, flags, None, None, Some(&mut bytes));
        if status.0 != 0 {
            log::debug!(target: "plugin", "[nle jianying] registry query failed hive={hive:?} view={view:?} key={key} value={name}: {}", io::Error::from_raw_os_error(status.0 as i32));
            return None;
        }
        let mut value = vec![0u16; bytes as usize / 2];
        let status = RegGetValueW(
            hive,
            &key,
            &name,
            flags,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut bytes),
        );
        if status.0 != 0 {
            log::debug!(target: "plugin", "[nle jianying] registry read failed hive={hive:?} view={view:?} key={key} value={name}: {}", io::Error::from_raw_os_error(status.0 as i32));
            return None;
        }
        let length = value.iter().position(|&ch| ch == 0).unwrap_or(value.len());
        match String::from_utf16(&value[..length]) {
            Ok(value) => {
                log::debug!(target: "plugin", "[nle jianying] registry value hive={hive:?} view={view:?} key={key} name={name} path={value:?}");
                Some(value)
            }
            Err(error) => {
                log::debug!(target: "plugin", "[nle jianying] registry invalid UTF-16 key={key} value={name}: {error}");
                None
            }
        }
    }
}

fn registered_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for hive in [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE] {
        for view in [RRF_SUBKEY_WOW6464KEY, RRF_SUBKEY_WOW6432KEY] {
            for (key, name) in [
                (
                    r"Software\Microsoft\Windows\CurrentVersion\App Paths\JianyingPro.exe",
                    "",
                ),
                (
                    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\JianyingPro",
                    "InstallLocation",
                ),
                (
                    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\JianyingPro",
                    "UninstallString",
                ),
                (
                    r"Software\Microsoft\Windows\CurrentVersion\Uninstall\JianyingPro",
                    "DisplayIcon",
                ),
            ] {
                if let Some(value) = registry_string(hive, key, name, view) {
                    match registered_directory(&value) {
                        Some(directory) if !directories.contains(&directory) => directories.push(directory),
                        Some(_) => {},
                        None => log::debug!(target: "plugin", "[nle jianying] registry path rejected key={key} value={name} path={value:?}"),
                    }
                }
            }
        }
    }
    directories
}

fn registered_directory(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    let path = if let Some(quoted) = value.strip_prefix('"') {
        quoted.split_once('"')?.0
    } else if let Some(end) = value.to_ascii_lowercase().find(".exe") {
        &value[..end + 4]
    } else {
        value
    };
    let path = Path::new(path);
    if !path.is_absolute() {
        return None;
    }
    let directory = if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        path.parent()?
    } else {
        path
    };
    Some(directory.to_owned())
}

fn find_candidates(directories: &[PathBuf], user_root: &Path, source: &str, checked: &mut Vec<PathBuf>) -> Option<Host> {
    for directory in directories {
        // The registration or shortcut can name the executable, its directory, or the install root.
        for root in directory.ancestors().take(3).filter(|root| root.parent().is_some()) {
            if checked.iter().any(|path| path == root) { continue; }
            checked.push(root.to_owned());
            match find_host(root) {
                Ok(mut host) => {
                    log::info!(target: "plugin", "[nle jianying] host selected source={source} candidate={root:?} version={:?} user_root={user_root:?}", host.version);
                    // User data stays under GenericDataLocation, independently of the binaries.
                    host.root = user_root.to_owned();
                    return Some(host);
                }
                Err(error) => log::debug!(target: "plugin", "[nle jianying] candidate rejected source={source} path={root:?} kind={:?}: {error}", error.kind()),
            }
        }
    }
    None
}

fn running_directories() -> Vec<PathBuf> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut system = System::new();
    system.refresh_processes_specifics(ProcessesToUpdate::All, true,
        ProcessRefreshKind::new().with_exe(UpdateKind::OnlyIfNotSet));
    let mut directories = Vec::new();
    for (pid, process) in system.processes() {
        if process.name().eq_ignore_ascii_case("JianyingPro.exe") {
            match process.exe() {
                Some(path) => {
                    log::debug!(target: "plugin", "[nle jianying] running host pid={pid} path={path:?}");
                    directories.push(path.to_owned());
                }
                None => log::debug!(target: "plugin", "[nle jianying] running host path unavailable pid={pid}"),
            }
        }
    }
    log::debug!(target: "plugin", "[nle jianying] running host candidates={}", directories.len());
    directories
}

fn shortcut_directories(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    for root in roots {
        let folder = QString::from(root.to_string_lossy().as_ref());
        let recursive = root.ends_with("Programs");
        let shortcuts = cpp!(unsafe [folder as "QString", recursive as "bool"] -> QString as "QString" {
            QStringList paths;
            QDirIterator it(folder, {QStringLiteral("*剪映*.lnk"), QStringLiteral("*Jianying*.lnk")}, QDir::Files,
                recursive ? QDirIterator::Subdirectories : QDirIterator::NoIteratorFlags);
            while (it.hasNext()) {
                const QFileInfo link(it.next());
                paths.append(link.absoluteFilePath() + QLatin1Char('\t') + link.symLinkTarget());
            }
            return paths.join(QLatin1Char('\n'));
        }).to_string();
        log::debug!(target: "plugin", "[nle jianying] shortcuts inspected directory={root:?}");
        for line in shortcuts.lines() {
            if let Some((shortcut, target)) = line.split_once('\t') {
                let target = PathBuf::from(target);
                if target.is_absolute() && target.file_name().is_some_and(|name| name.eq_ignore_ascii_case("JianyingPro.exe")) {
                    log::debug!(target: "plugin", "[nle jianying] shortcut path={shortcut:?} target={target:?}");
                    directories.push(target);
                } else {
                    log::debug!(target: "plugin", "[nle jianying] shortcut target rejected path={shortcut:?} target={target:?}");
                }
            }
        }
    }
    directories
}

pub(super) fn set_host_location(path: &str) -> io::Result<String> {
    let selected = Path::new(path);
    if !selected.is_absolute() {
        log::warn!(target: "plugin", "[nle jianying] manual location rejected path={selected:?}: path is not absolute");
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "Jianying installation path must be absolute"));
    }
    match find_host(selected) {
        Ok(_) => {
            gyroflow_core::settings::set(HOST_PATH_SETTING, path.into());
            log::info!(target: "plugin", "[nle jianying] manual location saved path={selected:?}");
            Ok(String::new())
        }
        Err(error) => {
            log::warn!(target: "plugin", "[nle jianying] manual location rejected path={selected:?}: {error}");
            Err(error)
        }
    }
}

pub(super) fn host() -> io::Result<Host> {
    let user_root = local_root()?.join("JianyingPro");
    let mut checked = Vec::new();
    let saved = gyroflow_core::settings::get_str(HOST_PATH_SETTING, "");
    if !saved.is_empty() {
        let saved = PathBuf::from(saved);
        if saved.is_absolute() {
            if let Some(host) = find_candidates(&[saved], &user_root, "manual", &mut checked) { return Ok(host); }
        } else {
            log::debug!(target: "plugin", "[nle jianying] saved location rejected: path is not absolute path={saved:?}");
        }
    }
    if let Some(host) = find_candidates(&registered_directories(), &user_root, "registry", &mut checked) { return Ok(host); }
    if let Some(host) = find_candidates(&running_directories(), &user_root, "process", &mut checked) { return Ok(host); }
    let locations = cpp!(unsafe [] -> QString as "QString" {
        QStringList paths = QStandardPaths::standardLocations(QStandardPaths::ApplicationsLocation);
        paths.append(QStandardPaths::standardLocations(QStandardPaths::DesktopLocation));
        return paths.join(QLatin1Char('\n'));
    }).to_string();
    let shortcuts: Vec<PathBuf> = locations.lines().map(PathBuf::from).collect();
    if let Some(host) = find_candidates(&shortcut_directories(&shortcuts), &user_root, "shortcut", &mut checked) { return Ok(host); }
    let mut defaults = vec![user_root.clone()];
    for variable in ["ProgramFiles", "ProgramFiles(x86)"] {
        if let Some(path) = std::env::var_os(variable) { defaults.push(PathBuf::from(path).join("JianyingPro")); }
    }
    if let Some(host) = find_candidates(&defaults, &user_root, "default", &mut checked) { return Ok(host); }
    log::warn!(target: "plugin", "[nle jianying] host detection failed checked={} user_root={user_root:?}; no usable manual, registry, process, shortcut or default path", checked.len());
    Err(io::Error::new(io::ErrorKind::NotFound, "Jianying installation was not found; select its installation folder"))
}

pub(super) fn plugin_cache(root: &Path) -> io::Result<PathBuf> {
    let settings_file = root.join("User Data/Config/globalSetting");
    match std::fs::metadata(&settings_file) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(root.join("User Data/Cache/OFX"));
        }
        Err(error) => return Err(error),
    }
    let file = QString::from(settings_file.to_string_lossy().as_ref());
    let mut status = 0i32;
    let status_ptr = &mut status as *mut i32;
    let configured =
        cpp!(unsafe [file as "QString", status_ptr as "int *"] -> QString as "QString" {
            QSettings settings(file, QSettings::IniFormat);
            const auto path = settings.value(QStringLiteral("currentCachePath")).toString();
            *status_ptr = int(settings.status());
            return path;
        })
        .to_string();
    if status != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unable to read Jianying cache settings",
        ));
    }
    if configured.is_empty() {
        return Ok(root.join("User Data/Cache/OFX"));
    }
    let cache = PathBuf::from(configured);
    if !cache.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Jianying cache path must be absolute",
        ));
    }
    Ok(cache.join("OFX"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_paths_accept_custom_drives_and_installer_forms() {
        for (value, expected) in [
            (r"D:\视频工具\剪映", r"D:\视频工具\剪映"),
            (r"D:\视频工具\剪映\Apps", r"D:\视频工具\剪映\Apps"),
            (
                r#""D:\视频工具\剪映\Apps\uninst.exe" /S"#,
                r"D:\视频工具\剪映\Apps",
            ),
            (
                r"D:\视频工具\剪映\Apps\uninst.exe,0",
                r"D:\视频工具\剪映\Apps",
            ),
            (
                r#""D:\视频工具\剪映\Apps\11.8.0.14711\JianyingPro.exe""#,
                r"D:\视频工具\剪映\Apps\11.8.0.14711",
            ),
        ] {
            assert_eq!(
                registered_directory(value),
                Some(PathBuf::from(expected)),
                "{value}"
            );
        }
        assert_eq!(
            registered_directory(r"E:\Custom Apps\剪映\Apps\JianyingPro.EXE"),
            Some(PathBuf::from(r"E:\Custom Apps\剪映\Apps"))
        );
        assert!(registered_directory("relative/Apps/JianyingPro.exe").is_none());
    }

    #[test]
    #[ignore = "Requires an installed Jianying host with Windows installation registration"]
    fn actual_installation_registration_resolves_host() {
        let directories = registered_directories();
        let empty = tempfile::tempdir().unwrap();
        let registered = find_candidates(&directories, empty.path(), "registry", &mut Vec::new()).unwrap();
        let detected = host().unwrap();
        assert_eq!(registered.version, detected.version);
        let cache = plugin_cache(&detected.root).unwrap();
        println!(
            "{}",
            serde_json::json!({"registered_directories": directories, "host_version": detected.version, "user_root": detected.root, "plugin_cache": cache})
        );
    }

    #[test]
    fn registered_install_uses_separate_user_data_and_skips_stale_entries() {
        let temp = tempfile::tempdir().unwrap();
        let custom = temp.path().join("custom installation/Apps");
        let user = temp.path().join("user data");
        let app = custom.join("Apps/11.8.0.14711");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("JianyingPro.exe"), []).unwrap();
        assert!(find_host(&user).is_err());
        for directory in [&custom, &custom.join("Apps"), &app] {
            let host =
                find_candidates(&[temp.path().join("removed"), directory.to_owned()], &user, "registry", &mut Vec::new())
                    .unwrap();
            assert_eq!(host.version, "11.8.0.14711");
            assert_eq!(host.root, user);
            assert_eq!(
                plugin_cache(&host.root).unwrap(),
                user.join("User Data/Cache/OFX")
            );
        }
    }

    #[test]
    fn default_install_remains_a_fallback() {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("Apps/11.8.0.14711");
        std::fs::create_dir_all(&app).unwrap();
        std::fs::write(app.join("JianyingPro.exe"), []).unwrap();
        assert_eq!(
            find_candidates(&[temp.path().to_owned()], temp.path(), "default", &mut Vec::new()).unwrap().version,
            "11.8.0.14711"
        );
    }

    #[test]
    fn cache_setting_supports_qt_escaping_and_fresh_directories() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("User Data/Config");
        std::fs::create_dir_all(&config).unwrap();
        for (setting, expected) in [
            (r"D:\\剪映缓存\\Cache", r"D:\剪映缓存\Cache\OFX"),
            (r#""E:\\剪映 缓存,新\\Cache""#, r"E:\剪映 缓存,新\Cache\OFX"),
            ("D:/剪映缓存/Cache", "D:/剪映缓存/Cache/OFX"),
        ] {
            std::fs::write(
                config.join("globalSetting"),
                format!("[General]\ncurrentCachePath={setting}\n"),
            )
            .unwrap();
            assert_eq!(plugin_cache(temp.path()).unwrap(), PathBuf::from(expected));
        }
        std::fs::write(
            config.join("globalSetting"),
            "[General]\notherSetting=true\n",
        )
        .unwrap();
        assert_eq!(
            plugin_cache(temp.path()).unwrap(),
            temp.path().join("User Data/Cache/OFX")
        );
        std::fs::write(
            config.join("globalSetting"),
            "[General]\ncurrentCachePath=relative/cache\n",
        )
        .unwrap();
        assert!(plugin_cache(temp.path()).is_err());
    }

    #[test]
    fn shortcut_discovery_resolves_real_windows_links_without_scanning_desktop_subdirectories() {
        let temp = tempfile::tempdir().unwrap();
        let installation = temp.path().join("D drive/剪映 安装/11.8.0.14711");
        std::fs::create_dir_all(&installation).unwrap();
        let executable = installation.join("JianyingPro.exe");
        std::fs::write(&executable, []).unwrap();
        let programs = temp.path().join("Programs");
        let desktop = temp.path().join("Desktop");
        for link in [programs.join("剪映/剪映专业版.lnk"), desktop.join("project/Jianying.lnk")] {
            std::fs::create_dir_all(link.parent().unwrap()).unwrap();
            let output = Command::new("powershell.exe")
                .args(["-NoProfile", "-NonInteractive", "-Command",
                    "$link = (New-Object -ComObject WScript.Shell).CreateShortcut($env:GYROFLOW_TEST_SHORTCUT); $link.TargetPath = $env:GYROFLOW_TEST_EXECUTABLE; $link.Save()"])
                .env("GYROFLOW_TEST_SHORTCUT", &link).env("GYROFLOW_TEST_EXECUTABLE", &executable)
                .output().unwrap();
            assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        }
        let paths = shortcut_directories(&[programs, desktop]);
        assert_eq!(paths, vec![executable]);
        let user = temp.path().join("user");
        let detected = find_candidates(&paths, &user, "shortcut", &mut Vec::new()).unwrap();
        assert_eq!(detected.version, "11.8.0.14711");
        assert_eq!(detected.root, user);
    }
}
