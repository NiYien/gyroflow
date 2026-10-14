// SPDX-License-Identifier: GPL-3.0-or-later
use std::{env, path::Path, process::Command};

pub fn build(target_os: &str) {
    if !matches!(target_os, "windows" | "macos") {
        return;
    }
    println!("cargo:rerun-if-changed=_scripts/install_crm_deps.py");
    println!("cargo:rerun-if-changed=_scripts/build_crm.rs");
    println!("cargo:rerun-if-changed=src/rendering/crm_decoder.cpp");
    let root = Path::new("ext/crm");
    if !root.join("LibRaw-0.22.2/libraw/libraw.h").exists()
        || !root.join("include/abi/mdk/VideoDecoder.h").exists()
        || !root.join("mdk-abi-sdk-0.39.zip.verified").exists()
    {
        let python = env::var("PYTHON").unwrap_or_else(|_| {
            if target_os == "windows" {
                "python"
            } else {
                "python3"
            }
            .into()
        });
        let status = Command::new(python)
            .arg("_scripts/install_crm_deps.py")
            .status()
            .expect("Run python _scripts/install_crm_deps.py to install the CRM build inputs");
        assert!(
            status.success(),
            "CRM source dependency installation failed"
        );
    }
    let abi_headers = root.join("include/abi");
    let abi_headers = if target_os == "windows" {
        // MSVC 14.43 cannot use std::ceil in a constexpr function. Keep the
        // pinned SDK intact and adapt only the headers used by this build.
        let headers = Path::new(&env::var("OUT_DIR").unwrap()).join("mdk-abi");
        for entry in walkdir::WalkDir::new(&abi_headers) {
            let entry = entry.unwrap();
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry.path().strip_prefix(&abi_headers).unwrap();
            let destination = headers.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            if relative == Path::new("mdk/AudioFormat.h") {
                let original = std::fs::read_to_string(entry.path()).unwrap();
                let signature = "constexpr int framesForDuration(double duration) const";
                assert_eq!(
                    original.matches(signature).count(),
                    1,
                    "Review the MDK AudioFormat compatibility fix after an SDK upgrade"
                );
                std::fs::write(
                    destination,
                    original.replacen(signature, "int framesForDuration(double duration) const", 1),
                ).unwrap();
            } else {
                std::fs::copy(entry.path(), destination).unwrap();
            }
        }
        headers
    } else {
        abi_headers
    };
    let source = root.join("LibRaw-0.22.2");
    let mut raw = cc::Build::new();
    raw.cpp(true)
        .std("c++17")
        .warnings(false)
        .include(&source)
        .define("LIBRAW_NODLL", None)
        .define("LIBRAW_BUILDLIB", None)
        .define("LIBRAW_MAX_CR3_RAW_FILE_SIZE", "1099511627776LL");
    let openmp =
        target_os == "windows" && env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("x86_64");
    if target_os == "windows" {
        raw.flag("/utf-8");
    }
    if openmp {
        raw.flag("/openmp");
    }
    let mut files = walkdir::WalkDir::new(source.join("src"))
        .into_iter()
        .filter_map(Result::ok)
        .map(|entry| entry.into_path())
        .filter(|p| p.extension().is_some_and(|x| x == "cpp"))
        // LibRaw ships alternative placeholders for builds without postprocessing.
        .filter(|p| {
            !p.file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with("_ph.cpp")
        })
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        println!("cargo:rerun-if-changed={}", file.display());
        raw.file(file);
    }
    raw.compile("crm_libraw");

    let mut decoder = cc::Build::new();
    decoder
        .cpp(true)
        .warnings(false)
        .include(&abi_headers)
        .include(&source)
        .define("LIBRAW_NODLL", None)
        .file("src/rendering/crm_decoder.cpp");
    if target_os == "windows" {
        decoder.flag("/utf-8").flag("/std:c++latest").flag("/GR-");
        if openmp {
            decoder.flag("/openmp");
        }
    } else {
        // Xcode 15.4 accepts C++23 under its draft name.
        decoder.flag("-std=c++2b").flag("-fno-rtti");
    }
    decoder.compile("crm_decoder");
}
