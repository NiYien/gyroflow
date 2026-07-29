// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

//! Canon built-in gyro time-offset classification.
//!
//! Canon bodies that record a CNDM gyro track keep that gyro as their motion
//! source (`FileMetadata::keep_video_gyro`). Whether such a video still needs an
//! auto-sync pass depends on the body: some are frame-aligned, some lead their
//! own video by exactly one frame, and the rest have simply never been measured.
//!
//! The classification lives in camera_db's `canon.json` under the top-level
//! `builtin_gyro_offset` field and is the **single source of truth** — there is
//! deliberately no built-in fallback table. A missing / unreadable / malformed
//! table degrades to an empty table, which makes every Canon body `Unknown`, and
//! `Unknown` bodies run a normal auto-sync. That is the safe direction: an
//! unclassified body costs a few seconds of sync instead of silently shipping an
//! uncorrected offset.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Time offset of a Canon body's built-in gyro relative to its own frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanonGyroOffset {
    /// The gyro leads its own video by one frame. Skip auto-sync and apply a
    /// fixed `-(1000/fps)` ms offset instead.
    OneFrame,
    /// The gyro is aligned with the frames. Skip auto-sync, apply no offset.
    NoOffset,
    /// Never classified. Run a normal auto-sync and adopt whatever it computes.
    Unknown,
}

impl CanonGyroOffset {
    /// Stable lowercase label for log lines.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneFrame => "one_frame",
            Self::NoOffset => "none",
            Self::Unknown => "unknown",
        }
    }

    /// Whether a body with this classification skips auto-sync entirely.
    pub fn skips_autosync(self) -> bool {
        !matches!(self, Self::Unknown)
    }
}

/// Canonical model name -> classification. `Default` is an empty table, which
/// classifies every body as `Unknown`.
pub type OffsetTable = HashMap<String, CanonGyroOffset>;

/// Strip the brand prefix off a `detected_source` string and return the
/// camera_db canonical model name to look up.
///
/// `detected_source` exists in two spellings in this codebase —
/// `"Canon R5 Mark II"` (`stabilization_params.rs`) and
/// `"Canon EOS R5 Mark II"` (`render_queue.rs`) — so the optional `EOS ` segment
/// after the brand has to be tolerated. camera_db keys carry neither prefix.
///
/// Returns `None` for anything that is not a Canon source, or when nothing is
/// left after stripping (e.g. a bare `"Canon "` from an EXIF-less file).
pub fn model_key(detected_source: &str) -> Option<&str> {
    let rest = detected_source.strip_prefix("Canon ")?;
    let rest = rest.strip_prefix("EOS ").unwrap_or(rest);
    let rest = rest.trim();
    (!rest.is_empty()).then_some(rest)
}

/// Classify a `detected_source` against an already-loaded table.
///
/// Pure: no IO, no globals, no logging — the unit tests drive it directly.
///
/// The comparison is a **full equality** match against the table key, never a
/// substring test. camera_db lists `R50` and `R50 V` as two different bodies
/// with different readout rows (`-13.5/-27` vs `15.7/31.9`); a `contains` match
/// would let one silently inherit the other's classification. Do not "simplify"
/// this back into `contains`.
pub fn classify(detected_source: &str, table: &OffsetTable) -> CanonGyroOffset {
    match model_key(detected_source) {
        Some(key) => table.get(key).copied().unwrap_or(CanonGyroOffset::Unknown),
        None => CanonGyroOffset::Unknown,
    }
}

/// Classify against the table loaded from the active camera_db.
///
/// Deliberately silent: this is called per job from several gates (including the
/// per-frame-ish sync-frame estimator), so the log lines belong at the decision
/// points in `render_queue`, not here. Table loading itself logs once per
/// camera_db path.
pub fn classify_detected_source(detected_source: &str) -> CanonGyroOffset {
    classify(detected_source, &offset_table())
}

/// Parse the `builtin_gyro_offset` field out of a `canon.json` document.
///
/// Any shape problem (not an object, missing field, non-string or misspelled
/// value) drops the offending entry rather than failing the whole load — a typo
/// in one row must not silently reclassify every other body.
pub fn parse_table(canon_json: &str) -> OffsetTable {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(canon_json) else {
        ::log::warn!(target: "lens", "[canon_arbitration] canon.json is not valid JSON; builtin_gyro_offset table unavailable");
        return OffsetTable::default();
    };
    let Some(map) = root.get("builtin_gyro_offset").and_then(|v| v.as_object()) else {
        ::log::warn!(target: "lens", "[canon_arbitration] canon.json has no builtin_gyro_offset object; all Canon bodies will be treated as unknown (lens package too old?)");
        return OffsetTable::default();
    };

    let mut table = OffsetTable::default();
    for (model, value) in map {
        let class = match value.as_str() {
            Some("one_frame") => CanonGyroOffset::OneFrame,
            Some("none") => CanonGyroOffset::NoOffset,
            other => {
                ::log::warn!(
                    target: "lens",
                    "[canon_arbitration] builtin_gyro_offset['{model}'] has unrecognized value {other:?}; entry ignored"
                );
                continue;
            }
        };
        table.insert(model.clone(), class);
    }
    table
}

struct CachedTable {
    /// camera_db directory this table was parsed from. `""` when no camera_db
    /// could be located, so that case is cached too instead of retrying per job.
    camera_db_dir: String,
    table: Arc<OffsetTable>,
}

static CACHE: Mutex<Option<CachedTable>> = Mutex::new(None);

/// The classification table for the currently active camera_db.
///
/// Cached on the camera_db directory path, so switching the lens hot-update
/// package (`AppData\...\lens\versions\<N>\camera_db`) re-reads the table
/// without a process restart.
pub fn offset_table() -> Arc<OffsetTable> {
    let dir = crate::gyro_source::get_camera_db_path();
    let key = dir.clone().unwrap_or_default();

    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(cached) = cache.as_ref().filter(|c| c.camera_db_dir == key) {
        return Arc::clone(&cached.table);
    }

    let table = Arc::new(load_table(dir.as_deref()));
    *cache = Some(CachedTable {
        camera_db_dir: key,
        table: Arc::clone(&table),
    });
    table
}

fn load_table(camera_db_dir: Option<&str>) -> OffsetTable {
    let Some(dir) = camera_db_dir else {
        ::log::warn!(target: "lens", "[canon_arbitration] camera_db path not found; builtin_gyro_offset table empty, all Canon bodies treated as unknown");
        return OffsetTable::default();
    };
    let path = std::path::Path::new(dir).join("canon.json");
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            ::log::warn!(target: "lens", "[canon_arbitration] canon.json unreadable at {} ({e}); builtin_gyro_offset table empty", path.display());
            return OffsetTable::default();
        }
    };
    let table = parse_table(&content);
    ::log::info!(
        target: "lens",
        "[canon_arbitration] builtin_gyro_offset table loaded from {} entries={}",
        path.display(),
        table.len()
    );
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(entries: &[(&str, CanonGyroOffset)]) -> OffsetTable {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect()
    }

    fn full_table() -> OffsetTable {
        table(&[
            ("R5 Mark II", CanonGyroOffset::OneFrame),
            ("C50", CanonGyroOffset::NoOffset),
            ("C80", CanonGyroOffset::NoOffset),
            ("C400", CanonGyroOffset::NoOffset),
            ("R6 Mark III", CanonGyroOffset::NoOffset),
        ])
    }

    // 5.1
    #[test]
    fn classify_hits_one_frame_and_none() {
        let t = full_table();
        assert_eq!(
            classify("Canon R5 Mark II", &t),
            CanonGyroOffset::OneFrame
        );
        assert_eq!(classify("Canon C50", &t), CanonGyroOffset::NoOffset);
    }

    // 5.2
    #[test]
    fn classify_tolerates_optional_eos_segment() {
        let t = full_table();
        assert_eq!(
            classify("Canon EOS R5 Mark II", &t),
            classify("Canon R5 Mark II", &t)
        );
        assert_eq!(
            classify("Canon EOS R5 Mark II", &t),
            CanonGyroOffset::OneFrame
        );
        assert_eq!(classify("Canon EOS C50", &t), CanonGyroOffset::NoOffset);
    }

    // 5.3 — R50 and R50 V are two different bodies; neither may inherit the
    // other's classification via substring matching.
    #[test]
    fn classify_never_confuses_r50_with_r50_v() {
        let only_r50_v = table(&[("R50 V", CanonGyroOffset::NoOffset)]);
        assert_eq!(
            classify("Canon R50", &only_r50_v),
            CanonGyroOffset::Unknown
        );
        assert_eq!(
            classify("Canon R50 V", &only_r50_v),
            CanonGyroOffset::NoOffset
        );

        let only_r50 = table(&[("R50", CanonGyroOffset::NoOffset)]);
        assert_eq!(
            classify("Canon R50 V", &only_r50),
            CanonGyroOffset::Unknown
        );
        assert_eq!(classify("Canon R50", &only_r50), CanonGyroOffset::NoOffset);
    }

    // 5.4
    #[test]
    fn classify_unlisted_model_is_unknown() {
        let t = full_table();
        assert_eq!(classify("Canon R50 V", &t), CanonGyroOffset::Unknown);
        assert_eq!(
            classify("Canon EOS R50 V", &t),
            CanonGyroOffset::Unknown
        );
        assert!(!classify("Canon R50 V", &t).skips_autosync());
    }

    // 5.5
    #[test]
    fn classify_with_empty_table_is_always_unknown() {
        let t = OffsetTable::default();
        for src in [
            "Canon R5 Mark II",
            "Canon EOS R5 Mark II",
            "Canon C50",
            "Canon R50 V",
        ] {
            assert_eq!(classify(src, &t), CanonGyroOffset::Unknown, "{src}");
        }
    }

    // 5.6
    #[test]
    fn parse_table_falls_back_to_empty_on_bad_input() {
        // Malformed JSON.
        assert!(parse_table("{ not json").is_empty());
        // Valid JSON without the field.
        assert!(parse_table(r#"{"version":1,"models":{"C50":{"sw":35.9}}}"#).is_empty());
        // Field present but not an object.
        assert!(parse_table(r#"{"builtin_gyro_offset":"one_frame"}"#).is_empty());
        // Empty document.
        assert!(parse_table("").is_empty());
    }

    #[test]
    fn parse_table_drops_only_the_misspelled_entries() {
        let t = parse_table(
            r#"{"builtin_gyro_offset":{
                "R5 Mark II":"one_frame",
                "C50":"None",
                "C80":"no_offset",
                "C400":1,
                "R6 Mark III":"none"
            }}"#,
        );
        assert_eq!(t.len(), 2);
        assert_eq!(
            classify("Canon R5 Mark II", &t),
            CanonGyroOffset::OneFrame
        );
        assert_eq!(
            classify("Canon R6 Mark III", &t),
            CanonGyroOffset::NoOffset
        );
        // Misspelled values must degrade to unknown, not to a wrong class.
        assert_eq!(classify("Canon C50", &t), CanonGyroOffset::Unknown);
        assert_eq!(classify("Canon C80", &t), CanonGyroOffset::Unknown);
        assert_eq!(classify("Canon C400", &t), CanonGyroOffset::Unknown);
    }

    #[test]
    fn parse_table_reads_the_shipped_shape() {
        let t = parse_table(
            r#"{"builtin_gyro_offset":{
                "R5 Mark II":"one_frame",
                "C50":"none",
                "C80":"none",
                "C400":"none",
                "R6 Mark III":"none"
            }}"#,
        );
        assert_eq!(t, full_table());
    }

    // 5.7 — non-Canon sources never reach the table.
    #[test]
    fn classify_non_canon_brands_are_unknown() {
        let t = full_table();
        for src in [
            "Sony ILCE-7SM3",
            "RED KOMODO",
            "Nikon Z 8",
            "Canon", // brand with no trailing space
            "Canon ", // EXIF-less file: brand only
            "Canon EOS ",
            "",
        ] {
            assert_eq!(classify(src, &t), CanonGyroOffset::Unknown, "{src}");
        }
        assert!(model_key("Sony ILCE-7SM3").is_none());
        assert!(model_key("Canon ").is_none());
    }

    #[test]
    fn model_key_strips_brand_and_optional_eos() {
        assert_eq!(model_key("Canon R5 Mark II"), Some("R5 Mark II"));
        assert_eq!(model_key("Canon EOS R5 Mark II"), Some("R5 Mark II"));
        assert_eq!(model_key("Canon EOS C500 Mark II"), Some("C500 Mark II"));
        assert_eq!(model_key("Canon R50 V"), Some("R50 V"));
    }

    #[test]
    fn skips_autosync_only_for_classified_bodies() {
        assert!(CanonGyroOffset::OneFrame.skips_autosync());
        assert!(CanonGyroOffset::NoOffset.skips_autosync());
        assert!(!CanonGyroOffset::Unknown.skips_autosync());
    }
}
