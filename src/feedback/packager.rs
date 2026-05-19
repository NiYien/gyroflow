// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Packager: assemble a feedback zip in memory from a set of local artifact
// paths plus user-supplied summary/email. Returns the zip bytes + sha256 hex
// for the uploader. Decoupled from filesystem discovery — the controller
// supplies a `PackageInputs` struct with concrete paths.
//
// Compression uses zip Deflated level 9 (instead of zstd as proposed in
// design §D8): saves a heavy zstd dep, ratio difference for log text is
// well under the ±20% size estimate tolerance the design accepts.

use std::collections::BTreeMap;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;

use super::meta::Meta;

// Tail-cap for OFX / Adobe plugin logs (design §D1). Files at or below the
// cap are embedded whole; files above it contribute only their last cap-bytes
// (advanced forward past the first `\n` so the first line is well-formed).
const PLUGIN_LOG_TAIL_CAP: u64 = 5 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PackageInputs {
    pub current_log:   Option<PathBuf>,
    pub history_logs:  Vec<PathBuf>,    // .log.1 .. .log.4 in order
    pub incidents_log: Option<PathBuf>,
    pub openfx_log:    Option<PathBuf>, // <data_dir>/gyroflow-openfx.log
    pub adobe_log:     Option<PathBuf>, // <data_dir>/gyroflow-adobe.log
    pub project_file:  Option<PathBuf>, // current .gyroflow snapshot
    pub lens_file:     Option<PathBuf>, // lens.json from data_dir
    pub queue_file:    Option<PathBuf>, // render_queue.json
    pub settings_file: Option<PathBuf>, // settings.json
    pub crash_zips:    Vec<PathBuf>,    // unuploaded crash dumps
}

impl Default for PackageInputs {
    fn default() -> Self {
        Self {
            current_log:   None,
            history_logs:  Vec::new(),
            incidents_log: None,
            openfx_log:    None,
            adobe_log:     None,
            project_file:  None,
            lens_file:     None,
            queue_file:    None,
            settings_file: None,
            crash_zips:    Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PackageOptions {
    pub include_current_log:    bool, // mandatory — UI does not allow off
    pub include_history_logs:   bool,
    pub include_incidents:      bool,
    pub include_project:        bool,
    pub include_video_meta:     bool, // phase 4 stub (no probe yet)
    pub include_lens:           bool,
    pub include_queue_settings: bool,
    pub include_system_info:    bool, // mandatory — UI does not allow off
    pub include_crashes:        bool,
}

impl Default for PackageOptions {
    fn default() -> Self {
        Self {
            include_current_log:    true,
            include_history_logs:   true,
            include_incidents:      true,
            include_project:        true,
            include_video_meta:     true,
            include_lens:           true,
            include_queue_settings: true,
            include_system_info:    true,
            include_crashes:        true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PackerError {
    #[error("zip error: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("package exceeds {} MiB limit", super::MAX_PACKAGE_SIZE_BYTES / 1024 / 1024)]
    TooLarge,
}

/// Estimate the *uncompressed* sum of selected input files. Real zip is
/// typically 20-40% smaller for text-heavy payloads. UI displays this as a
/// upper bound; design §"Risks" accepts ±20% error.
pub fn estimate_size(inputs: &PackageInputs, options: &PackageOptions) -> u64 {
    let mut total: u64 = 0;
    for (cond, path) in iter_paths(inputs, options) {
        if !cond { continue; }
        if let Ok(meta) = std::fs::metadata(path) {
            total = total.saturating_add(meta.len());
        }
    }
    // Plugin logs are always counted (no PackageOptions toggle) and capped to
    // PLUGIN_LOG_TAIL_CAP each so a 70 MiB OFX log does not inflate the UI's
    // pre-submit size hint to scary numbers (design §D7).
    if let Some(p) = &inputs.openfx_log {
        total = total.saturating_add(capped_metadata_len(p, PLUGIN_LOG_TAIL_CAP));
    }
    if let Some(p) = &inputs.adobe_log {
        total = total.saturating_add(capped_metadata_len(p, PLUGIN_LOG_TAIL_CAP));
    }
    // manifest + per-zip overhead approximation
    total.saturating_add(2_048)
}

/// Returns `min(metadata.len(), cap)`, or 0 when metadata cannot be read
/// (e.g., the file was deleted between discovery and packaging).
fn capped_metadata_len(path: &Path, cap: u64) -> u64 {
    match std::fs::metadata(path) {
        Ok(m) => m.len().min(cap),
        Err(_) => 0,
    }
}

/// Read up to `PLUGIN_LOG_TAIL_CAP` bytes from the end of the file. Files at
/// or below the cap are returned whole. When the file exceeds the cap, the
/// returned bytes are advanced past the first `\n` so the first line is
/// well-formed; if no `\n` is present, the slice is returned unchanged.
fn read_plugin_log_tail(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let cap = PLUGIN_LOG_TAIL_CAP;
    let mut buf = Vec::new();
    if len <= cap {
        f.read_to_end(&mut buf)?;
        return Ok(buf);
    }
    f.seek(SeekFrom::End(-(cap as i64)))?;
    f.read_to_end(&mut buf)?;
    if let Some(idx) = buf.iter().position(|&b| b == b'\n') {
        buf.drain(..=idx);
    }
    Ok(buf)
}

fn iter_paths<'a>(inputs: &'a PackageInputs, options: &'a PackageOptions) -> Vec<(bool, &'a Path)> {
    let mut v: Vec<(bool, &Path)> = Vec::new();
    if let Some(p) = &inputs.current_log {
        v.push((options.include_current_log, p.as_path()));
    }
    if options.include_history_logs {
        for p in &inputs.history_logs { v.push((true, p.as_path())); }
    }
    if let Some(p) = &inputs.incidents_log {
        v.push((options.include_incidents, p.as_path()));
    }
    if let Some(p) = &inputs.project_file {
        v.push((options.include_project, p.as_path()));
    }
    if let Some(p) = &inputs.lens_file {
        v.push((options.include_lens, p.as_path()));
    }
    if let Some(p) = &inputs.queue_file {
        v.push((options.include_queue_settings, p.as_path()));
    }
    if let Some(p) = &inputs.settings_file {
        v.push((options.include_queue_settings, p.as_path()));
    }
    if options.include_crashes {
        for p in &inputs.crash_zips { v.push((true, p.as_path())); }
    }
    v
}

#[derive(serde::Serialize)]
struct ManifestJson<'a> {
    app_version:     &'a str,
    os:              &'a str,
    gpu:             &'a str,
    cpu:             &'a str,
    memory_total:    u64,
    display_scale:   Option<f64>,
    summary:         &'a str,
    email:           &'a str,
    ts:              String,
    files:           Vec<String>,
    sha256_per_file: BTreeMap<String, String>,
    // `<zip_entry_name> -> N` when the local `<base>.repeats` sidecar
    // existed at packaging time. Omitted entirely when no sidecar is
    // present (consumers MUST treat absence as count = 1, never default
    // a literal — see feedback-submission spec).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    crash_repeats:   BTreeMap<String, u64>,
}

/// Read `<zip>.repeats` and parse the integer, returning `None` when the
/// sidecar is missing or unparseable. Used at packaging time to attach the
/// repeats count to the manifest.
fn read_repeats_sidecar(zip_path: &Path) -> Option<u64> {
    let sidecar = zip_path.with_extension("repeats");
    let raw = std::fs::read_to_string(&sidecar).ok()?;
    raw.trim().parse::<u64>().ok()
}

/// Pack the selected inputs into an in-memory zip. Returns `(bytes, sha256_hex)`.
/// Errors: PackerError::TooLarge if final zip exceeds 50 MB.
pub fn pack(
    inputs: &PackageInputs,
    options: &PackageOptions,
    summary: &str,
    email: &str,
    meta: &Meta,
) -> Result<(Vec<u8>, String), PackerError> {
    // Collect (zip_path, source_path) pairs — silently drop entries whose
    // source is missing (e.g., user has no project loaded) so that toggling
    // a checkbox on with a missing file is a no-op rather than an error.
    let mut entries: Vec<(String, PathBuf)> = Vec::new();

    if options.include_current_log {
        if let Some(p) = &inputs.current_log {
            entries.push(("logs/current-session.log".into(), p.clone()));
        }
    }
    if options.include_history_logs {
        for (i, p) in inputs.history_logs.iter().enumerate() {
            entries.push((format!("logs/session-{}.log", i + 1), p.clone()));
        }
    }
    if options.include_incidents {
        if let Some(p) = &inputs.incidents_log {
            entries.push(("logs/incidents.log".into(), p.clone()));
        }
    }
    if options.include_project {
        if let Some(p) = &inputs.project_file {
            entries.push(("project/current.gyroflow".into(), p.clone()));
        }
    }
    if options.include_lens {
        if let Some(p) = &inputs.lens_file {
            entries.push(("project/lens.json".into(), p.clone()));
        }
    }
    if options.include_queue_settings {
        if let Some(p) = &inputs.queue_file {
            entries.push(("render-queue.json".into(), p.clone()));
        }
        if let Some(p) = &inputs.settings_file {
            entries.push(("settings.json".into(), p.clone()));
        }
    }
    if options.include_crashes {
        for p in &inputs.crash_zips {
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("crash.zip");
            entries.push((format!("crashes/{name}"), p.clone()));
        }
    }
    // Plugin logs (OFX / Adobe). Always included when present — no
    // matching PackageOptions toggle today; stable `-tail` zip names
    // regardless of whether truncation actually fired (design §D3, §D6).
    if let Some(p) = &inputs.openfx_log {
        entries.push(("logs/openfx-tail.log".into(), p.clone()));
    }
    if let Some(p) = &inputs.adobe_log {
        entries.push(("logs/adobe-tail.log".into(), p.clone()));
    }

    // Build zip in memory.
    let mut buf: Vec<u8> = Vec::with_capacity(512 * 1024);
    let mut sha256_per_file: BTreeMap<String, String> = BTreeMap::new();
    let mut file_list: Vec<String> = Vec::new();
    let mut crash_repeats: BTreeMap<String, u64> = BTreeMap::new();

    // Pre-scan crash zips for `.repeats` sidecars so the manifest can
    // surface "this bug actually triggered N times" without requiring the
    // receiver to inspect raw filesystem state.
    if options.include_crashes {
        for p in &inputs.crash_zips {
            if let Some(n) = read_repeats_sidecar(p) {
                let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("crash.zip");
                crash_repeats.insert(format!("crashes/{name}"), n);
            }
        }
    }
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zw = zip::ZipWriter::new(cursor);
        let opts = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .compression_level(Some(9));

        // Stream each entry. Files that fail to open are skipped silently
        // (consistent with the "missing source = no-op" rule above).
        for (zip_name, src) in &entries {
            // Plugin-log entries get the tail-cap reader; everything else
            // reads the whole file.
            let bytes_result = if zip_name == "logs/openfx-tail.log"
                || zip_name == "logs/adobe-tail.log"
            {
                read_plugin_log_tail(src)
            } else {
                std::fs::read(src)
            };
            let bytes = match bytes_result {
                Ok(b) => b,
                Err(_) => continue,
            };
            zw.start_file(zip_name, opts)?;
            zw.write_all(&bytes)?;
            sha256_per_file.insert(zip_name.clone(), hex_sha256(&bytes));
            file_list.push(zip_name.clone());
        }

        // manifest.json last — it references the file list above.
        let manifest = ManifestJson {
            app_version:     &meta.app_version,
            os:              &meta.os,
            gpu:             &meta.gpu,
            cpu:             &meta.cpu,
            memory_total:    meta.memory_total,
            display_scale:   meta.display_scale,
            summary,
            email,
            ts:              chrono::Utc::now().to_rfc3339(),
            files:           file_list,
            sha256_per_file,
            crash_repeats,
        };
        let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
        zw.start_file("manifest.json", opts)?;
        zw.write_all(&manifest_bytes)?;

        zw.finish()?;
    }

    if buf.len() as u64 > super::MAX_PACKAGE_SIZE_BYTES {
        return Err(PackerError::TooLarge);
    }
    let sha = hex_sha256(&buf);
    Ok((buf, sha))
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::File::create(&p).unwrap().write_all(content).unwrap();
        p
    }

    fn dummy_meta() -> Meta {
        Meta {
            app_version:   "1.6.3-test".into(),
            os:            "TestOS".into(),
            gpu:           "TestGPU".into(),
            cpu:           "TestCPU".into(),
            memory_total:  16 * 1024 * 1024 * 1024,
            display_scale: Some(1.0),
        }
    }

    #[test]
    fn pack_logs_only() {
        let tmp = tempfile::tempdir().unwrap();
        let cur_log = make_file(tmp.path(), "gyroflow.log", b"hello session");
        let inputs = PackageInputs { current_log: Some(cur_log), ..Default::default() };
        let mut opts = PackageOptions::default();
        opts.include_history_logs = false;
        opts.include_incidents = false;
        opts.include_project = false;
        opts.include_lens = false;
        opts.include_queue_settings = false;
        opts.include_crashes = false;
        let (bytes, sha) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        assert!(!bytes.is_empty());
        assert_eq!(sha.len(), 64);
        // verify roundtrip
        let zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<_> = zr.file_names().map(|s| s.to_string()).collect();
        assert!(names.contains(&"logs/current-session.log".to_string()));
        assert!(names.contains(&"manifest.json".to_string()));
    }

    #[test]
    fn missing_source_is_no_op() {
        // current_log path doesn't exist; pack should still succeed with
        // just manifest.json
        let tmp = tempfile::tempdir().unwrap();
        let inputs = PackageInputs {
            current_log: Some(tmp.path().join("nope.log")),
            ..Default::default()
        };
        let opts = PackageOptions {
            include_current_log:    true,
            include_history_logs:   false,
            include_incidents:      false,
            include_project:        false,
            include_video_meta:     false,
            include_lens:           false,
            include_queue_settings: false,
            include_system_info:    false,
            include_crashes:        false,
        };
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        let zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<_> = zr.file_names().map(|s| s.to_string()).collect();
        assert_eq!(names, vec!["manifest.json"]);
    }

    #[test]
    fn estimate_size_sums_existing_files() {
        let tmp = tempfile::tempdir().unwrap();
        let l1 = make_file(tmp.path(), "a.log", &vec![b'x'; 1024]);
        let l2 = make_file(tmp.path(), "b.log", &vec![b'y'; 2048]);
        let inputs = PackageInputs {
            current_log: Some(l1),
            history_logs: vec![l2],
            ..Default::default()
        };
        let opts = PackageOptions::default();
        let est = estimate_size(&inputs, &opts);
        // 1024 + 2048 + manifest overhead
        assert!(est >= 3072);
        assert!(est < 8192);
    }

    #[test]
    fn manifest_includes_repeats_when_sidecar_present() {
        let tmp = tempfile::tempdir().unwrap();
        let crash_zip = make_file(tmp.path(), "20260516T174801-30a5d1f9.zip", b"PK\x03\x04stub");
        // Write the sibling .repeats sidecar with content "5".
        std::fs::write(tmp.path().join("20260516T174801-30a5d1f9.repeats"), b"5").unwrap();

        let inputs = PackageInputs { crash_zips: vec![crash_zip], ..Default::default() };
        let mut opts = PackageOptions::default();
        opts.include_current_log = false;
        opts.include_history_logs = false;
        opts.include_incidents = false;
        opts.include_project = false;
        opts.include_lens = false;
        opts.include_queue_settings = false;
        opts.include_system_info = false;
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();

        // Extract manifest.json and verify the crash_repeats field.
        let mut zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut manifest_str = String::new();
        {
            let mut f = zr.by_name("manifest.json").unwrap();
            std::io::Read::read_to_string(&mut f, &mut manifest_str).unwrap();
        }
        let v: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
        let repeats = v.get("crash_repeats").expect("crash_repeats field present");
        assert_eq!(repeats["crashes/20260516T174801-30a5d1f9.zip"], 5);
    }

    #[test]
    fn manifest_omits_repeats_when_no_sidecar() {
        let tmp = tempfile::tempdir().unwrap();
        let crash_zip = make_file(tmp.path(), "20260516T180000-abcd.zip", b"PK\x03\x04stub");
        // Intentionally NO .repeats sidecar.
        let inputs = PackageInputs { crash_zips: vec![crash_zip], ..Default::default() };
        let opts = PackageOptions {
            include_current_log:    false,
            include_history_logs:   false,
            include_incidents:      false,
            include_project:        false,
            include_video_meta:     false,
            include_lens:           false,
            include_queue_settings: false,
            include_system_info:    false,
            include_crashes:        true,
        };
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        let mut zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut manifest_str = String::new();
        {
            let mut f = zr.by_name("manifest.json").unwrap();
            std::io::Read::read_to_string(&mut f, &mut manifest_str).unwrap();
        }
        let v: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
        // Empty map skipped by `skip_serializing_if`: field must be absent.
        assert!(v.get("crash_repeats").is_none(),
                "crash_repeats must be omitted when no sidecar — found: {:?}",
                v.get("crash_repeats"));
    }

    #[test]
    fn manifest_omits_repeats_on_parse_failure() {
        let tmp = tempfile::tempdir().unwrap();
        let crash_zip = make_file(tmp.path(), "20260516T190000-efgh.zip", b"PK\x03\x04");
        std::fs::write(tmp.path().join("20260516T190000-efgh.repeats"), b"not a number").unwrap();

        let inputs = PackageInputs { crash_zips: vec![crash_zip], ..Default::default() };
        let opts = PackageOptions {
            include_current_log:    false,
            include_history_logs:   false,
            include_incidents:      false,
            include_project:        false,
            include_video_meta:     false,
            include_lens:           false,
            include_queue_settings: false,
            include_system_info:    false,
            include_crashes:        true,
        };
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        let mut zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut manifest_str = String::new();
        {
            let mut f = zr.by_name("manifest.json").unwrap();
            std::io::Read::read_to_string(&mut f, &mut manifest_str).unwrap();
        }
        let v: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
        assert!(v.get("crash_repeats").is_none());
    }

    #[test]
    fn openfx_log_under_cap_embedded_whole() {
        let tmp = tempfile::tempdir().unwrap();
        let content = vec![b'X'; 1024];
        let p = make_file(tmp.path(), "gyroflow-openfx.log", &content);
        let inputs = PackageInputs { openfx_log: Some(p), ..Default::default() };
        let opts = PackageOptions::default();
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        let mut zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut entry = zr.by_name("logs/openfx-tail.log").expect("entry present");
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
        assert_eq!(buf, content);
    }

    #[test]
    fn openfx_log_over_cap_tail_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("gyroflow-openfx.log");
        {
            let mut f = std::fs::File::create(&p).unwrap();
            // Head: a partial line without a trailing newline. Far enough
            // from the tail window that it cannot affect the result.
            f.write_all(b"PARTIAL_HEAD_FRAGMENT").unwrap();
            // Body: many "AAAAAAA\n" lines (8 bytes each), total > 6 MiB so
            // the source comfortably exceeds the 5 MiB cap.
            let line = b"AAAAAAA\n";
            let target = 6 * 1024 * 1024;
            let mut written: usize = 0;
            while written < target {
                f.write_all(line).unwrap();
                written += line.len();
            }
        }
        let inputs = PackageInputs { openfx_log: Some(p), ..Default::default() };
        let opts = PackageOptions::default();
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        let mut zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let mut entry = zr.by_name("logs/openfx-tail.log").expect("entry present");
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut buf).unwrap();
        assert!((buf.len() as u64) <= PLUGIN_LOG_TAIL_CAP,
            "entry exceeded cap: {} bytes", buf.len());
        // After advancing past the first '\n' in the last 5 MiB, the entry
        // must start at a line boundary — i.e., with 'A' (the first byte of
        // "AAAAAAA\n").
        assert_eq!(buf.first().copied(), Some(b'A'),
            "first byte should be the start of a line, got {:?}", buf.first());
    }

    #[test]
    fn plugin_log_absent_silently_omitted() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = PackageInputs {
            openfx_log: Some(tmp.path().join("missing-openfx.log")),
            adobe_log:  Some(tmp.path().join("missing-adobe.log")),
            ..Default::default()
        };
        let opts = PackageOptions::default();
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        let mut zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<_> = zr.file_names().map(|s| s.to_string()).collect();
        assert!(!names.iter().any(|n| n == "logs/openfx-tail.log"));
        assert!(!names.iter().any(|n| n == "logs/adobe-tail.log"));
        let mut manifest_str = String::new();
        {
            let mut f = zr.by_name("manifest.json").unwrap();
            std::io::Read::read_to_string(&mut f, &mut manifest_str).unwrap();
        }
        let v: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
        let files = v.get("files").and_then(|x| x.as_array()).unwrap();
        assert!(!files.iter().any(|x| x.as_str() == Some("logs/openfx-tail.log")));
        assert!(!files.iter().any(|x| x.as_str() == Some("logs/adobe-tail.log")));
        let sha = v.get("sha256_per_file").and_then(|x| x.as_object()).unwrap();
        assert!(!sha.contains_key("logs/openfx-tail.log"));
        assert!(!sha.contains_key("logs/adobe-tail.log"));
    }

    #[test]
    fn estimate_size_caps_plugin_logs() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("gyroflow-openfx.log");
        std::fs::write(&p, vec![b'x'; 7 * 1024 * 1024]).unwrap();
        let inputs = PackageInputs { openfx_log: Some(p), ..Default::default() };
        let opts = PackageOptions::default();
        let est = estimate_size(&inputs, &opts);
        assert!(est <= 5 * 1024 * 1024 + 2_048,
            "estimate must be capped: got {} bytes", est);
    }

    #[test]
    fn adobe_log_packed_alongside_openfx() {
        let tmp = tempfile::tempdir().unwrap();
        let ofx = make_file(tmp.path(), "gyroflow-openfx.log",
            b"OFX line one\nOFX line two\n");
        let adb = make_file(tmp.path(), "gyroflow-adobe.log",
            b"Adobe line one\nAdobe line two\n");
        let inputs = PackageInputs {
            openfx_log: Some(ofx),
            adobe_log:  Some(adb),
            ..Default::default()
        };
        let opts = PackageOptions::default();
        let (bytes, _) = pack(&inputs, &opts, "", "", &dummy_meta()).unwrap();
        let mut zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<_> = zr.file_names().map(|s| s.to_string()).collect();
        assert!(names.contains(&"logs/openfx-tail.log".to_string()));
        assert!(names.contains(&"logs/adobe-tail.log".to_string()));
        let mut manifest_str = String::new();
        {
            let mut f = zr.by_name("manifest.json").unwrap();
            std::io::Read::read_to_string(&mut f, &mut manifest_str).unwrap();
        }
        let v: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();
        let sha = v.get("sha256_per_file").and_then(|x| x.as_object()).unwrap();
        let ofx_sha = sha.get("logs/openfx-tail.log")
            .and_then(|x| x.as_str()).expect("ofx sha present");
        let adb_sha = sha.get("logs/adobe-tail.log")
            .and_then(|x| x.as_str()).expect("adobe sha present");
        assert_eq!(ofx_sha.len(), 64);
        assert_eq!(adb_sha.len(), 64);
        assert_ne!(ofx_sha, adb_sha, "distinct content must hash differently");
    }

    #[test]
    fn full_roundtrip_with_all_kinds() {
        let tmp = tempfile::tempdir().unwrap();
        let inputs = PackageInputs {
            current_log:   Some(make_file(tmp.path(), "cur.log", b"current")),
            history_logs:  vec![make_file(tmp.path(), "h1.log", b"hist1")],
            incidents_log: Some(make_file(tmp.path(), "inc.log", b"warn line")),
            openfx_log:    None,
            adobe_log:     None,
            project_file:  Some(make_file(tmp.path(), "p.gyroflow", b"{}")),
            lens_file:     Some(make_file(tmp.path(), "l.json", b"[]")),
            queue_file:    Some(make_file(tmp.path(), "q.json", b"[]")),
            settings_file: Some(make_file(tmp.path(), "s.json", b"{}")),
            crash_zips:    vec![make_file(tmp.path(), "c.zip", b"PK\x03\x04stub")],
        };
        let (bytes, sha) = pack(&inputs, &PackageOptions::default(),
            "test summary", "user@example.com", &dummy_meta()).unwrap();
        assert_eq!(sha.len(), 64);
        let zr = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names: Vec<_> = zr.file_names().map(|s| s.to_string()).collect();
        for expected in [
            "logs/current-session.log",
            "logs/session-1.log",
            "logs/incidents.log",
            "project/current.gyroflow",
            "project/lens.json",
            "render-queue.json",
            "settings.json",
            "crashes/c.zip",
            "manifest.json",
        ] {
            assert!(names.contains(&expected.to_string()), "missing: {expected}");
        }
    }
}
