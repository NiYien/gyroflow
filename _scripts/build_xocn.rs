// SPDX-License-Identifier: GPL-3.0-or-later
use std::{env, path::Path, process::Command};

pub fn build(target_os: &str) {
    for file in [
        "_scripts/build_xocn.rs",
        "_scripts/install_crm_deps.py",
        "src/rendering/xocn_codec.cpp",
        "src/rendering/xocn_codec.h",
        "src/rendering/xocn_linear_curve.h",
        "src/rendering/xocn_reader.cpp",
        "src/rendering/xocn_mdk_compat.h",
        "vendor/xocn/libraw_llvc3.h",
    ] {
        println!("cargo:rerun-if-changed={file}");
    }
    let headers = Path::new("ext/crm/include/abi");
    if !headers.join("mdk/FrameReader.h").exists() {
        let python = env::var("PYTHON").unwrap_or_else(|_| {
            if target_os == "windows" {
                "python"
            } else {
                "python3"
            }
            .into()
        });
        let status = Command::new(python)
            .args(["_scripts/install_crm_deps.py", "--abi-only"])
            .status()
            .expect("Install the pinned MDK ABI headers for X-OCN");
        assert!(status.success(), "X-OCN ABI headers installation failed");
    }
    let adapted = Path::new(&env::var("OUT_DIR").unwrap()).join("xocn-mdk-abi");
    for entry in walkdir::WalkDir::new(headers) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(headers).unwrap();
        let destination = adapted.join(relative);
        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        let mut text = std::fs::read_to_string(entry.path()).unwrap();
        // Adapt inline standard-library helpers without changing the pinned ABI.
        text = text
            .replace("#include <bit>", "")
            .replace("std::to_underlying(", "xocn_mdk_compat::underlying(")
            .replace("std::bit_ceil(", "xocn_mdk_compat::bit_ceil(")
            .replace(
                "std::endian::native == std::endian::big",
                "xocn_mdk_compat::native_big_endian",
            )
            .replace(
                "protocols().contains(url.substr(0, p))",
                "protocols().count(url.substr(0, p)) != 0",
            )
            .replace(
                "constexpr int framesForDuration(double duration) const",
                "int framesForDuration(double duration) const",
            );
        if relative == Path::new("mdk/global.h") {
            text = format!("#include \"xocn_mdk_compat.h\"\n{text}");
        }
        if relative == Path::new("mdk/AudioFormat.h") {
            // bitset's constexpr operations were added after C++17.
            text = text.replace("constexpr ", "");
        }
        std::fs::write(destination, text).unwrap();
    }
    std::fs::copy(
        "src/rendering/xocn_mdk_compat.h",
        adapted.join("mdk/xocn_mdk_compat.h"),
    )
    .unwrap();
    let mut native = cc::Build::new();
    native
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .include(&adapted)
        .file("src/rendering/xocn_codec.cpp")
        .file("src/rendering/xocn_reader.cpp");
    if target_os == "windows" {
        native.flag("/utf-8").flag("/EHsc");
    }
    if target_os == "linux" {
        // The Linux MDK plugin ABI uses libc++; keep this choice local to X-OCN.
        native
            .compiler("clang++")
            .flag("-stdlib=libc++")
            .cpp_link_stdlib("c++");
    }
    native.compile("xocn_decoder");
}
