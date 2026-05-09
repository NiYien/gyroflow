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
