// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2023 Adrian <adrian.eddy at gmail>

// NiYien lens-data integration:
// On every cargo invocation, build.rs resolves the latest niyien-lens-data
// release tag via the GitHub `releases/latest` redirect, compares it against
// `resources/.lens_data_pin`, and refreshes `resources/camera_db/` and
// `resources/lens_presets/` when the tag changes. Network failures fall back
// to the cached pin so offline builds still work after the first online build.
// The effective tag is injected into the binary as `BUILTIN_LENS_DATA_TAG`.

use std::time::Duration;

const NIYIEN_LENS_DATA_REPO: &str = "NiYien/niyien-lens-data";

#[derive(Debug, Clone, Copy)]
enum TagSource {
    EnvOverride,
    GithubLatest,
    PinFallback,
}

fn main() {
    let project_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();

    // Force build.rs to rerun on every cargo invocation. The sentinel path is
    // never created on disk; cargo treats a missing rerun-if-changed path as
    // permanently stale, so the latest-tag check below executes every build.
    println!("cargo:rerun-if-changed=resources/.lens_data_pin_check_sentinel");
    println!("cargo:rerun-if-env-changed=NIYIEN_LENS_DATA_TAG");

    // Synthetic cfg `neuflow_burn_enabled` gates all Burn-backed NeuFlow code
    // to Win+Mac targets even when the cargo feature is on. See change
    // burn-platform-and-ai-sync-toggle for rationale.
    println!("cargo::rustc-check-cfg=cfg(neuflow_burn_enabled)");
    let neuflow_burn_supported = neuflow_burn_target_supported();

    // 1. gyroflow upstream lens profiles database (unchanged behaviour).
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

    // 2. NiYien lens-data snapshot (camera_db + lens_presets).
    download_niyien_lens_snapshot(&project_dir);

    // 3. Generate the built-in preset lookup table from the snapshot on disk.
    generate_builtin_preset_table(&project_dir);

    // 4. Regenerate NeuFlow v2 model bindings from bundled ONNX (only when
    //    feature `neuflow-burn` is enabled AND target supports burn; pulled in
    //    as build-dep regardless but ModelGen is skipped to keep default-build
    //    cost down). On Linux/iOS/Android the feature is a no-op and we don't
    //    spend time regenerating the model.
    let feature_on = std::env::var_os("CARGO_FEATURE_NEUFLOW_BURN").is_some();
    if feature_on && neuflow_burn_supported {
        println!("cargo:rustc-cfg=neuflow_burn_enabled");
        generate_neuflow_burn_model(&project_dir);
    } else if feature_on {
        let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
        println!(
            "cargo:warning=NeuFlow Burn disabled on target={target_os} (feature gated to win+mac)"
        );
    }
}

fn neuflow_burn_target_supported() -> bool {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    target_os == "windows" || target_os == "macos"
}

fn generate_neuflow_burn_model(project_dir: &str) {
    use burn_onnx::ModelGen;

    let onnx_path = format!("{project_dir}/../../resources/neuflow_v2_iter5_768x432.onnx");
    if !std::path::Path::new(&onnx_path).exists() {
        panic!(
            "neuflow-burn feature enabled but ONNX source missing at {onnx_path}. \
             Expected resources/neuflow_v2_iter5_768x432.onnx to be present."
        );
    }
    println!("cargo:rerun-if-changed={onnx_path}");

    ModelGen::new()
        .input(&onnx_path)
        .out_dir("burn_onnx/")
        .run_from_script();

    // Wrap 3 unguarded matmuls in F32 cast blocks. burn-onnx leaves these as
    // raw F16 matmul, which accumulates a 128-dim (or 1296-dim) dot product
    // in F16. Combined with cubecl autotune's kernel switching, the ULP-level
    // variance bleeds into rs-sync and produces bimodal sync offsets. Adding
    // F32 cast guards here is the targeted fix vs. disabling autotune globally.
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    post_process_neuflow_model_for_f32_matmul_casts(&out_dir);

    // Copy the regenerated .bpk into the source tree so mod.rs::find_weight_file()
    // can still locate it from a release/install layout (no env!("OUT_DIR") at runtime).
    // The .rs glue stays in $OUT_DIR and is pulled in via include!().
    let src_bpk = format!("{out_dir}/burn_onnx/neuflow_v2_iter5_768x432.bpk");
    let dst_bpk =
        format!("{project_dir}/neuflow_burn/generated_iter5/neuflow_v2_iter5_768x432.bpk");
    if let Err(e) = std::fs::copy(&src_bpk, &dst_bpk) {
        panic!("Failed to copy regenerated burnpack {src_bpk} -> {dst_bpk}: {e}");
    }
}

fn post_process_neuflow_model_for_f32_matmul_casts(out_dir: &str) {
    let path = format!("{out_dir}/burn_onnx/neuflow_v2_iter5_768x432.rs");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            println!("cargo:warning=neuflow post-process: cannot read {path}: {e}");
            return;
        }
    };

    // Exact-match replacements. Pattern not found → log + skip (don't fail
    // build), since burn-onnx output shape could shift across regenerations.
    let replacements: &[(&str, &str)] = &[
        (
            "let matmul18_out1 = softmax3_out1.matmul(constant86_out1);",
            "let matmul18_out1 = { let _dt = softmax3_out1.dtype(); \
             softmax3_out1.cast(burn::tensor::DType::F32) \
             .matmul(constant86_out1.cast(burn::tensor::DType::F32)).cast(_dt) };",
        ),
        (
            "let matmul19_out1 = transpose7_out1.matmul(reshape7_out1);",
            "let matmul19_out1 = { let _dt = transpose7_out1.dtype(); \
             transpose7_out1.cast(burn::tensor::DType::F32) \
             .matmul(reshape7_out1.cast(burn::tensor::DType::F32)).cast(_dt) };",
        ),
        (
            "let matmul20_out1 = transpose10_out1.matmul(reshape12_out1);",
            "let matmul20_out1 = { let _dt = transpose10_out1.dtype(); \
             transpose10_out1.cast(burn::tensor::DType::F32) \
             .matmul(reshape12_out1.cast(burn::tensor::DType::F32)).cast(_dt) };",
        ),
    ];

    let mut new_content = content;
    let mut applied = 0;
    for (from, to) in replacements {
        if new_content.contains(from) {
            new_content = new_content.replace(from, to);
            applied += 1;
        } else {
            let preview: String = from.chars().take(60).collect();
            println!(
                "cargo:warning=neuflow post-process: pattern not found, skipping: {preview}..."
            );
        }
    }

    if applied > 0 {
        if let Err(e) = std::fs::write(&path, &new_content) {
            panic!("Failed to write post-processed {path}: {e}");
        }
        println!(
            "cargo:warning=neuflow post-process: applied {applied}/{} F32-matmul cast wrappers",
            replacements.len()
        );
    }
}

fn generate_builtin_preset_table(project_dir: &str) {
    use std::fmt::Write as _;

    let presets_dir = format!("{project_dir}/../../resources/lens_presets");
    let presets_path = std::path::Path::new(&presets_dir);
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let out_path = format!("{out_dir}/builtin_lens_preset_files.rs");

    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    let read = match std::fs::read_dir(presets_path) {
        Ok(read) => read,
        Err(e) => panic!(
            "Failed to read {presets_dir}: {e}. The niyien-lens-data snapshot should have \
             materialized this directory in step 2."
        ),
    };
    for entry in read {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let file_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => continue,
        };
        if file_name == "index.json" {
            continue;
        }
        entries.push((file_name, path));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // Always rerun when the index changes (file appearing/disappearing on disk).
    println!(
        "cargo:rerun-if-changed={}",
        presets_path.join("index.json").display()
    );

    let mut code = String::from("&[\n");
    for (name, path) in &entries {
        println!("cargo:rerun-if-changed={}", path.display());
        // Raw string literal; CARGO_MANIFEST_DIR cannot contain a `"` character or cargo
        // itself would already be broken.
        let _ = writeln!(code, "    (\"{name}\", include_str!(r\"{}\")),", path.display());
    }
    code.push_str("]\n");

    if let Err(e) = std::fs::write(&out_path, code) {
        panic!("Failed to write {out_path}: {e}");
    }
}

fn download_niyien_lens_snapshot(project_dir: &str) {
    let extract_base = format!("{project_dir}/../../resources");
    let pin_path = format!("{extract_base}/.lens_data_pin");

    let (effective_tag, source) = match resolve_effective_tag(&pin_path) {
        Ok(v) => v,
        Err(e) => panic!(
            "Failed to determine niyien-lens-data tag for build.\n\
             Cause: {e}\n\
             First-time builds require network access to query \
             https://github.com/{NIYIEN_LENS_DATA_REPO}/releases/latest.\n\
             Workaround: set env NIYIEN_LENS_DATA_TAG=<tag> (e.g. \
             NIYIEN_LENS_DATA_TAG=data-v20260429.1) to pin a known release."
        ),
    };

    println!(
        "cargo:warning=niyien-lens-data effective tag = {effective_tag} (source: {source:?})"
    );

    let cached_tag = read_pin_file(&pin_path);
    let lens_presets_dir = format!("{extract_base}/lens_presets");
    let camera_db_dir = format!("{extract_base}/camera_db");
    let dirs_complete = std::path::Path::new(&lens_presets_dir).exists()
        && std::path::Path::new(&camera_db_dir).exists();

    let needs_refresh = !dirs_complete || cached_tag.as_deref() != Some(effective_tag.as_str());

    if needs_refresh {
        // Wipe both directories so a partial leftover from a previous tag
        // cannot be silently mixed with the new tag's contents.
        let _ = std::fs::remove_dir_all(&lens_presets_dir);
        let _ = std::fs::remove_dir_all(&camera_db_dir);

        for (subdir, tarball) in [
            ("lens_presets", "lens_presets.tar.gz"),
            ("camera_db", "camera_db.tar.gz"),
        ] {
            let target = format!("{extract_base}/{subdir}");
            let url = format!(
                "https://github.com/{NIYIEN_LENS_DATA_REPO}/releases/download/{effective_tag}/{tarball}"
            );
            println!("cargo:warning=downloading {url}");

            let response = match ureq::get(&url).call() {
                Ok(r) => r,
                Err(e) => panic!(
                    "Failed to download niyien-lens-data snapshot {url}: {e:?}.\n\
                     Hint: verify that the tag '{effective_tag}' exists in \
                     https://github.com/{NIYIEN_LENS_DATA_REPO}/releases; \
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

        if let Err(e) = write_pin_file(&pin_path, &effective_tag) {
            panic!("Failed to write pin file {pin_path}: {e}");
        }
    }

    // Inject the effective tag into the binary so OTA / About / startup logs
    // can read it via env!("BUILTIN_LENS_DATA_TAG") at runtime.
    println!("cargo:rustc-env=BUILTIN_LENS_DATA_TAG={effective_tag}");
}

fn resolve_effective_tag(pin_path: &str) -> Result<(String, TagSource), String> {
    // 1. Explicit env override wins outright.
    if let Ok(tag) = std::env::var("NIYIEN_LENS_DATA_TAG") {
        let trimmed = tag.trim();
        if !trimmed.is_empty() {
            return Ok((trimmed.to_owned(), TagSource::EnvOverride));
        }
    }

    // 2. Ask GitHub what the current latest tag is.
    match fetch_latest_tag_via_redirect() {
        Ok(tag) => return Ok((tag, TagSource::GithubLatest)),
        Err(e) => {
            println!("cargo:warning=niyien-lens-data: latest lookup failed ({e}); falling back to .lens_data_pin");
        }
    }

    // 3. Offline fallback: read the previously written pin.
    if let Some(tag) = read_pin_file(pin_path) {
        return Ok((tag, TagSource::PinFallback));
    }

    Err(format!(
        "no env override, GitHub latest unreachable, and no cached tag in {pin_path}"
    ))
}

fn fetch_latest_tag_via_redirect() -> Result<String, String> {
    let url = format!("https://github.com/{NIYIEN_LENS_DATA_REPO}/releases/latest");

    // ureq 3.x: max_redirects=0 returns the 3xx response without following it,
    // and max_redirects_will_error=false ensures it isn't surfaced as Err.
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .max_redirects_will_error(false)
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();

    let response = agent
        .get(&url)
        .call()
        .map_err(|e| format!("HTTP request failed: {e}"))?;

    let status = response.status().as_u16();
    if !(300..400).contains(&status) {
        return Err(format!(
            "expected 3xx redirect from {url}, got {status}"
        ));
    }

    let location = response
        .headers()
        .get("location")
        .ok_or_else(|| "redirect response missing Location header".to_owned())?
        .to_str()
        .map_err(|e| format!("Location header is not valid UTF-8: {e}"))?
        .trim()
        .to_owned();

    // Expected shape: ".../releases/tag/<TAG>". Take the last path segment.
    let tag = location
        .rsplit('/')
        .find(|seg| !seg.is_empty())
        .ok_or_else(|| format!("malformed Location header: {location}"))?
        .to_owned();

    if !location.contains("/releases/tag/") {
        return Err(format!(
            "unexpected redirect target {location} (no /releases/tag/ segment)"
        ));
    }

    Ok(tag)
}

fn read_pin_file(pin_path: &str) -> Option<String> {
    let raw = std::fs::read_to_string(pin_path).ok()?;
    let trimmed = raw.lines().next().unwrap_or("").trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

fn write_pin_file(pin_path: &str, tag: &str) -> std::io::Result<()> {
    if let Some(parent) = std::path::Path::new(pin_path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(pin_path, format!("{tag}\n"))
}
