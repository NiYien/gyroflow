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
        .include(root.join("include/abi"))
        .include(&source)
        .define("LIBRAW_NODLL", None)
        .file("src/rendering/crm_decoder.cpp");
    if target_os == "windows" {
        decoder.flag("/utf-8").flag("/std:c++latest").flag("/GR-");
        if openmp {
            decoder.flag("/openmp");
        }
    } else {
        decoder.flag("-std=c++23").flag("-fno-rtti");
    }
    decoder.compile("crm_decoder");
}
