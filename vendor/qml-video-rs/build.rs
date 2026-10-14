use std::collections::HashMap;
use std::env;
use std::fs::File;
use std::process::Command;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=MDK_SDK");
    println!("cargo:rerun-if-env-changed=SDK_BASE");
    println!("cargo:rerun-if-changed=../../_scripts/install_mdk_sdk.py");
    println!("cargo:rerun-if-changed=../../_scripts/mdk_sdk_manifest.json");
    println!("cargo:rerun-if-changed=../../_scripts/build_r3d_plugin.py");
    println!("cargo:rerun-if-changed=../mdk-r3d");
    let qt_include_path = env::var("DEP_QT_INCLUDE_PATH").unwrap();
    let qt_library_path = env::var("DEP_QT_LIBRARY_PATH").unwrap();
    let qt_version      = env::var("DEP_QT_VERSION").unwrap();

    println!("cargo:rerun-if-changed=src/cpp/MDKPlayer.cpp");
    println!("cargo:rerun-if-changed=src/cpp/MDKPlayer.h");
    println!("cargo:rerun-if-changed=src/cpp/VideoTextureNode.cpp");
    println!("cargo:rerun-if-changed=src/cpp/VideoTextureNode.h");

    let mut config = cpp_build::Config::new();

    for f in std::env::var("DEP_QT_COMPILE_FLAGS").unwrap().split_terminator(";") {
        config.flag(f);
    }

    let mut public_include = |name| {
        if cfg!(target_os = "macos") {
            config.include(format!("{}/{}.framework/Headers/", qt_library_path, name));
        }
        config.include(format!("{}/{}", qt_include_path, name));
    };
    public_include("QtCore");
    public_include("QtGui");
    public_include("QtQuick");
    public_include("QtQml");

    let mut private_include = |name| {
        if cfg!(target_os = "macos") {
            config.include(format!("{}/{}.framework/Headers/{}",       qt_library_path, name, qt_version));
            config.include(format!("{}/{}.framework/Headers/{}/{}",    qt_library_path, name, qt_version, name));
        }
        config.include(format!("{}/{}/{}",    qt_include_path, name, qt_version))
              .include(format!("{}/{}/{}/{}", qt_include_path, name, qt_version, name));
    };
    private_include("QtCore");
    private_include("QtGui");
    private_include("QtQuick");
    private_include("QtQml");

    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let arch_win = if target_arch == "aarch64" { "arm64" } else { "x64" };
    let arch_lnx = if target_arch == "aarch64" { "arm64" } else { "amd64" };

    // Desktop and iOS use the verified BRAW 6 snapshot installed below.
    // Android keeps its existing SDK because it has no BRAW runtime.
    let sdk: HashMap<&str, (String, String, &str, &str)> = vec![
        ("windows", (String::new(), format!("lib/{arch_win}/"), "mdk.lib", "include/")),
        ("linux", (String::new(), format!("lib/{arch_lnx}/"), "libmdk.so", "include/")),
        ("macos", (String::new(), "lib/mdk.framework/".to_owned(), "mdk", "include/")),
        ("android",  ("https://github.com/NiYien/qml-video-rs/releases/download/mdk-503d1ed/mdk-sdk-android-arm64-v8a-MinSizeRel.zip".to_string(), format!("lib/arm64-v8a/"),  "libmdk.so",  "include/")),
        ("ios", (String::new(), "lib/mdk.framework/".to_owned(), "mdk", "include/")),
    ].into_iter().collect();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let entry = &sdk[target_os.as_str()];

    let sdk_result = if target_os == "android" {
        download_and_extract(&entry.0, &format!("{}/{}", entry.1, entry.2))
    } else {
        let python = env::var("PYTHON").unwrap_or_else(|_| if cfg!(windows) { "python" } else { "python3" }.into());
        let output = Command::new(python)
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../_scripts/install_mdk_sdk.py"))
            .arg(&target_os).output().expect("Cannot run the pinned MDK installer");
        if !output.status.success() {
            panic!("{}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(String::from_utf8(output.stdout).expect("MDK SDK path is not UTF-8").trim().to_owned())
    };
    if let Ok(path) = sdk_result {
        println!("cargo:sdk_path={path}");
        if target_os == "macos" || target_os == "ios" {
            println!("cargo:rustc-link-lib=framework=mdk");
            config.flag_if_supported("-fobjc-arc");
            config.flag("-x").flag("objective-c++");
            let _ = Command::new("mkdir").args(&[&format!("{}/../../../../Frameworks", env::var("OUT_DIR").unwrap())]).status();
            if std::path::Path::new(&format!("{path}/lib/mdk.xcframework/ios-arm64/mdk.framework")).exists() {
                println!("cargo:rustc-link-search=framework={path}lib/mdk.xcframework/ios-arm64/");
                let _ = Command::new("mkdir").args(&["-p", &format!("{}/{}mdk", path, entry.3)]).status();
                let _ = Command::new("cp").args(&["-af", &format!("{path}lib/mdk.xcframework/ios-arm64/mdk.framework/Headers/"), &format!("{}/{}mdk", path, entry.3)]).status();
                assert!(Command::new("cp").args(&["-af", &format!("{path}/lib/mdk.xcframework/ios-arm64/mdk.framework"), &format!("{}/../../../../Frameworks/", env::var("OUT_DIR").unwrap())]).status().unwrap().success(), "Cannot copy the pinned MDK framework");
            } else {
                println!("cargo:rustc-link-search=framework={path}lib/");
                assert!(Command::new("cp").args(&["-af", &format!("{path}/lib/mdk.framework"), &format!("{}/../../../../Frameworks/", env::var("OUT_DIR").unwrap())]).status().unwrap().success(), "Cannot copy the pinned MDK framework");
            }
        } else {
            println!("cargo:rustc-link-search={}/{}", path, entry.1);
            println!("cargo:rustc-link-lib=mdk");
        }
        if target_os == "macos" {
            let python = env::var("PYTHON").unwrap_or_else(|_| "python3".into());
            let status = Command::new(python)
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../_scripts/build_r3d_plugin.py"))
                .arg("--mdk-sdk").arg(&path)
                .arg("--output").arg(format!("{}/../../../../Frameworks/mdk.framework/Versions/A/libmdk-r3d.dylib", env::var("OUT_DIR").unwrap()))
                .status().expect("Cannot build the macOS R3D plugin");
            assert!(status.success(), "macOS R3D plugin build failed");
        }
        if target_os == "windows" {
            println!("cargo:rustc-link-lib=dxguid");
            std::fs::copy(format!("{}/bin/{arch_win}/mdk.dll", path), format!("{}/../../../mdk.dll", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/bin/{arch_win}/ffmpeg-9.dll", path), format!("{}/../../../ffmpeg-9.dll", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/bin/{arch_win}/mdk-braw.dll", path), format!("{}/../../../mdk-braw.dll", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/bin/{arch_win}/mdk-r3d.dll", path), format!("{}/../../../mdk-r3d.dll", env::var("OUT_DIR").unwrap())).unwrap();

            let _ = std::fs::copy(format!("{}/bin/{arch_win}/mdk.pdb", path), format!("{}/../../../mdk.pdb", env::var("OUT_DIR").unwrap())).unwrap();
            let _ = std::fs::copy(format!("{}/bin/{arch_win}/mdk-braw.pdb", path), format!("{}/../../../mdk-braw.pdb", env::var("OUT_DIR").unwrap()));
            let _ = std::fs::copy(format!("{}/bin/{arch_win}/mdk-r3d.pdb", path), format!("{}/../../../mdk-r3d.pdb", env::var("OUT_DIR").unwrap()));
        }
        if target_os == "android" {
            std::fs::copy(format!("{}/lib/arm64-v8a/libmdk.so", path), format!("{}/../../../libmdk.so", env::var("OUT_DIR").unwrap())).unwrap();
            // std::fs::copy(format!("{}/lib/arm64-v8a/libmdk.so.dsym", path), format!("{}/../../../libmdk.so.dsym", env::var("OUT_DIR").unwrap())).unwrap();
            let _ = std::fs::copy(format!("{}/lib/arm64-v8a/libffmpeg.so", path), format!("{}/../../../libffmpeg.so", env::var("OUT_DIR").unwrap()));
            // std::fs::copy(format!("{}/lib/arm64-v8a/libqtav-mediacodec.so", path), format!("{}/../../../libqtav-mediacodec.so", env::var("OUT_DIR").unwrap())).unwrap();
        }
        if target_os == "linux" {
            std::fs::copy(format!("{}/lib/{arch_lnx}/libc++.so.1", path), format!("{}/../../../libc++.so.1", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/lib/{arch_lnx}/libffmpeg.so.9", path), format!("{}/../../../libffmpeg.so.9", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/lib/{arch_lnx}/libmdk.so.0", path), format!("{}/../../../libmdk.so.0", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/lib/{arch_lnx}/libmdk-braw.so", path), format!("{}/../../../libmdk-braw.so", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/lib/{arch_lnx}/libmdk-r3d.so", path), format!("{}/../../../libmdk-r3d.so", env::var("OUT_DIR").unwrap())).unwrap();
        }
        config.include(format!("{}/{}", path, entry.3));
    } else {
        panic!("Unable to download or extract mdk-sdk. Please make sure you have 7z in PATH or download mdk manually from https://sourceforge.net/projects/mdk-sdk/ and extract to {}", env::var("OUT_DIR").unwrap());
    }

    let vulkan_sdk = env::var("VULKAN_SDK");
    if let Ok(sdk) = vulkan_sdk {
        if !sdk.is_empty() {
            config.include(format!("{}/Include", sdk));
            config.include(format!("{}/include", sdk));
        }
    }

    config
        .include(&qt_include_path)
        .build("src/lib.rs");

}

fn download_and_extract(url: &str, check: &str) -> Result<String, std::io::Error> {
    if let Ok(path) = env::var("MDK_SDK") {
        if Path::new(&format!("{}/{}", path, check)).exists() {
            return Ok(path);
        }
    }
    let out_dir = env::var("OUT_DIR").unwrap();

    if !Path::new(&format!("{}/mdk-sdk/{}", out_dir, check)).exists() && !Path::new(&format!("{}/mdk-sdk/{}", out_dir, check.replace("mdk.framework", "mdk.xcframework"))).exists() {
        // Detect outer archive extension. `.zip` is the nightly.link wrapper
        // (zip-of-7z or zip-of-tar.xz); `.tar.xz` and `.7z` are the original
        // wang-bin formats served directly by SourceForge.
        let download_ext = if url.ends_with(".zip") { ".zip" }
                          else if url.contains(".tar.xz") { ".tar.xz" }
                          else { ".7z" };
        {
            let mut reader = ureq::get(url).call().map(|x| x.into_body().into_reader()).map_err(|_| std::io::ErrorKind::Other)?;
            let mut file = File::create(format!("{}/mdk-sdk{}", out_dir, download_ext))?;
            std::io::copy(&mut reader, &mut file)?;
        }
        // If the outer is a nightly.link zip, peel it to expose the inner
        // wang-bin archive, then continue through the existing .7z / .tar.xz
        // extraction logic. The inner archive's filename follows wang-bin's
        // `mdk-sdk-*` convention.
        let ext = if download_ext == ".zip" {
            Command::new("7z").current_dir(&out_dir).args(&["x", "-y", "mdk-sdk.zip"]).status()?;
            std::fs::remove_file(format!("{}/mdk-sdk.zip", out_dir))?;
            let inner = std::fs::read_dir(&out_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .find(|n| n.starts_with("mdk-sdk-") && (n.ends_with(".7z") || n.ends_with(".tar.xz")))
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "Inner archive (mdk-sdk-*.7z or mdk-sdk-*.tar.xz) not found after unzipping outer nightly.link wrapper"))?;
            let inner_ext = if inner.ends_with(".tar.xz") { ".tar.xz" } else { ".7z" };
            std::fs::rename(format!("{}/{}", out_dir, inner), format!("{}/mdk-sdk{}", out_dir, inner_ext))?;
            inner_ext
        } else {
            download_ext
        };
        Command::new("7z").current_dir(&out_dir).args(&["x", "-y", &format!("mdk-sdk{}", ext)]).status()?;
        std::fs::remove_file(format!("{}/mdk-sdk{}", out_dir, ext))?;
        if ext == ".tar.xz" {
            let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
            if target_os == "macos" || target_os == "ios" || target_os == "linux" {
                Command::new("tar").current_dir(&out_dir).args(&["-xf", "mdk-sdk.tar"]).status()?;
            } else {
                Command::new("7z").current_dir(&out_dir).args(&["x", "-y", "mdk-sdk.tar"]).status()?;
            }
            std::fs::remove_file(format!("{}/mdk-sdk.tar", out_dir))?;
        }
    }

    Ok(format!("{}/mdk-sdk/", out_dir))
}
