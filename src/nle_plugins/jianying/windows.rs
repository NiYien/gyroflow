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
}}

fn registry_string(hive: HKEY, key: &str, name: &str, view: REG_ROUTINE_FLAGS) -> Option<String> {
    let key = HSTRING::from(key);
    let name = HSTRING::from(name);
    let flags = RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | view;
    let mut bytes = 0;
    unsafe {
        RegGetValueW(hive, &key, &name, flags, None, None, Some(&mut bytes))
            .ok()
            .ok()?;
        let mut value = vec![0u16; bytes as usize / 2];
        RegGetValueW(
            hive,
            &key,
            &name,
            flags,
            None,
            Some(value.as_mut_ptr().cast()),
            Some(&mut bytes),
        )
        .ok()
        .ok()?;
        let length = value.iter().position(|&ch| ch == 0).unwrap_or(value.len());
        String::from_utf16(&value[..length]).ok()
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
                if let Some(directory) = registry_string(hive, key, name, view)
                    .and_then(|value| registered_directory(&value))
                {
                    if !directories.contains(&directory) {
                        directories.push(directory);
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

fn find_registered_host(directories: &[PathBuf], user_root: &Path) -> io::Result<Host> {
    // Registrations may point to the install root, Apps, or a versioned executable.
    let roots = directories
        .iter()
        .flat_map(|directory| directory.ancestors().take(3));
    for root in roots.chain(std::iter::once(user_root)) {
        if let Ok(mut host) = find_host(root) {
            // Jianying's user data stays under GenericDataLocation, independently of its binaries.
            host.root = user_root.to_owned();
            return Ok(host);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Jianying installation was not found",
    ))
}

pub(super) fn host() -> io::Result<Host> {
    find_registered_host(
        &registered_directories(),
        &local_root()?.join("JianyingPro"),
    )
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
        let registered = find_registered_host(&directories, empty.path()).unwrap();
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
                find_registered_host(&[temp.path().join("removed"), directory.to_owned()], &user)
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
            find_registered_host(&[], temp.path()).unwrap().version,
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
}
