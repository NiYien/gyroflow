use std::collections::HashMap;
use std::env;
use std::process::Command;
use std::path::Path;

fn main() {
    println!("cargo:rerun-if-env-changed=MDK_SDK");
    println!("cargo:rerun-if-env-changed=SDK_BASE");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
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

    // Each platform installs its pinned runtime and validates the libraries it uses.
    let sdk: HashMap<&str, (String, String, &str, &str)> = vec![
        ("windows", (String::new(), format!("lib/{arch_win}/"), "mdk.lib", "include/")),
        ("linux", (String::new(), format!("lib/{arch_lnx}/"), "libmdk.so", "include/")),
        ("macos", (String::new(), "lib/mdk.framework/".to_owned(), "mdk", "include/")),
        ("android", (String::new(), format!("lib/arm64-v8a/"), "libmdk.so", "include/")),
        ("ios", (String::new(), "lib/mdk.framework/".to_owned(), "mdk", "include/")),
    ].into_iter().collect();

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let entry = &sdk[target_os.as_str()];

    let sdk_result: Result<String, std::io::Error> = {
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
            let ffmpeg = if target_arch == "x86_64" || target_arch == "aarch64" {
                // Preview and export must both keep the verified HEVC tile decoder.
                let sdk = env::var("FFMPEG_DIR").expect("FFMPEG_DIR must point to the verified FFmpeg 9 SDK");
                let marker = Path::new(&sdk).join(".gyroflow-hevc-tile-patch.json");
                assert!(marker.is_file(), "The Windows FFmpeg SDK is missing its HEVC tile verification");
                println!("cargo:rerun-if-changed={}", marker.display());
                format!("{sdk}/bin/ffmpeg-9.dll")
            } else {
                format!("{path}/bin/{arch_win}/ffmpeg-9.dll")
            };
            std::fs::copy(ffmpeg, format!("{}/../../../ffmpeg-9.dll", env::var("OUT_DIR").unwrap())).unwrap();
            std::fs::copy(format!("{}/bin/{arch_win}/mdk-braw.dll", path), format!("{}/../../../mdk-braw.dll", env::var("OUT_DIR").unwrap())).unwrap();
            // Use the same seek/audio fixes as macOS instead of the bundled upstream plugin.
            let compiler = cc::Build::new().cpp(true).get_compiler();
            let python = env::var("PYTHON").unwrap_or_else(|_| "python".into());
            let status = Command::new(python)
                .envs(compiler.env().iter().cloned())
                .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../_scripts/build_r3d_plugin.py"))
                .arg("--mdk-sdk").arg(&path)
                .arg("--output").arg(format!("{}/../../../mdk-r3d.dll", env::var("OUT_DIR").unwrap()))
                .arg("--compiler").arg(compiler.path())
                .arg("--arch").arg(arch_win)
                .status().expect("Cannot build the Windows R3D plugin");
            assert!(status.success(), "Windows R3D plugin build failed");

            let _ = std::fs::copy(format!("{}/bin/{arch_win}/mdk.pdb", path), format!("{}/../../../mdk.pdb", env::var("OUT_DIR").unwrap())).unwrap();
            let _ = std::fs::copy(format!("{}/bin/{arch_win}/mdk-braw.pdb", path), format!("{}/../../../mdk-braw.pdb", env::var("OUT_DIR").unwrap()));
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
