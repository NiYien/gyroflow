// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2023 Adrian <adrian.eddy at gmail>

/// Default tag pinned for the NiYien lens-data repository. Override at build
/// time with `NIYIEN_LENS_DATA_TAG=<tag>` to pick up an unreleased snapshot.
/// Bump this constant whenever a new `data-v*` release ships with content
/// that the NiYien gyroflow fork should embed as the compile-time fallback.
const NIYIEN_LENS_DATA_REPO: &str = "NiYien/niyien-lens-data";
const NIYIEN_LENS_DATA_DEFAULT_TAG: &str = "data-v20260421.1";

fn main() {
    let project_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // 1. gyroflow 原生 lens profiles 数据库（upstream 流程保留不动）
    let db_path = format!("{project_dir}/../../resources/camera_presets/profiles.cbor.gz");
    if !std::path::Path::new(&db_path).exists() {
        std::fs::create_dir_all(&format!("{project_dir}/../../resources/camera_presets")).unwrap();
        if let Ok(mut body) = ureq::get(
            "https://github.com/gyroflow/lens_profiles/releases/latest/download/profiles.cbor.gz",
        )
        .call()
        .map(|x| x.into_body().into_reader())
        {
            match std::fs::File::create(&db_path) {
                Ok(mut file) => {
                    std::io::copy(&mut body, &mut file).unwrap();
                }
                Err(e) => {
                    panic!("Failed to create {db_path}: {e:?}");
                }
            }
        }
    }

    // 2. NiYien lens-data 快照（camera_db + lens_presets）
    //    仅当目标目录不存在时下载；删除目录即可强制刷新。
    download_niyien_lens_snapshot(&project_dir);
}

fn download_niyien_lens_snapshot(project_dir: &str) {
    let tag = std::env::var("NIYIEN_LENS_DATA_TAG")
        .unwrap_or_else(|_| NIYIEN_LENS_DATA_DEFAULT_TAG.to_owned());
    println!("cargo:rerun-if-env-changed=NIYIEN_LENS_DATA_TAG");

    let extract_base = format!("{project_dir}/../../resources");

    for (subdir, tarball) in [
        ("lens_presets", "lens_presets.tar.gz"),
        ("camera_db", "camera_db.tar.gz"),
    ] {
        let target = format!("{extract_base}/{subdir}");
        if std::path::Path::new(&target).exists() {
            continue;
        }

        let url = format!(
            "https://github.com/{NIYIEN_LENS_DATA_REPO}/releases/download/{tag}/{tarball}"
        );
        println!("cargo:warning=downloading {url}");

        let response = match ureq::get(&url).call() {
            Ok(r) => r,
            Err(e) => panic!(
                "Failed to download niyien-lens-data snapshot {url}: {e:?}.\n\
                 Hint: verify that the tag '{tag}' exists in https://github.com/{NIYIEN_LENS_DATA_REPO}/releases; \
                 set env NIYIEN_LENS_DATA_TAG to a different tag if needed."
            ),
        };
        let mut reader = response.into_body().into_reader();
        let decoder = flate2::read::GzDecoder::new(&mut reader);
        let mut archive = tar::Archive::new(decoder);
        if let Err(e) = archive.unpack(&extract_base) {
            panic!("Failed to extract {tarball} into {extract_base}: {e:?}");
        }

        if !std::path::Path::new(&target).exists() {
            panic!(
                "After extracting {tarball}, expected directory {target} does not exist. \
                 The tarball may be malformed (expected entries prefixed with '{subdir}/')."
            );
        }
    }
}
