// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Feedback / crash report module. Phase 4 entry. Public surface:
//
// * `pending_crash_zips()` — startup scan for unuploaded crash dumps
//   (Phase 1 API, kept stable)
// * `PackageOptions` / `Packager` — compose a feedback zip from local
//   artifacts (Phase 4 §2)
// * `Meta::collect()` — system info snapshot for the manifest (Phase 4 §3)
// * `Uploader::submit()` — three-step POST/PUT/POST flow with retry
//   (Phase 4 §4); branches on `upload.kind` between r2_presigned_put
//   and pan123_multipart per docs/feedback-schema.md §6
// * `crash_pickup::scan_and_notify` — startup hook for crash dialog
//   (Phase 4 §5)

use std::path::PathBuf;

pub mod crash_pickup;
pub mod meta;
pub mod packager;
pub mod uploader;

pub use packager::{PackageInputs, PackageOptions};
pub use uploader::{FeedbackJobState, JobOutcome, SubmitArgs};

// --- shared constants -----------------------------------------------------

pub const NIYIEN_FEEDBACK_BASE: &str = "https://www.niyien.com/api";
pub const MAX_PACKAGE_SIZE_BYTES: u64 = 50_000_000;
pub const RETRY_ATTEMPTS: u32 = 3;
pub const BACKOFF_SECS: [u64; 3] = [1, 2, 4];

// --- pending_crash_zips (Phase 1 API) -------------------------------------

/// Scan `<data_dir>/logs/crashes/` for `*.zip` files lacking a sibling
/// `<base>.uploaded` marker. Returns full paths sorted by filename
/// (timestamp-prefixed → chronological).
pub fn pending_crash_zips() -> Vec<PathBuf> {
    let dir = match crate::logger::log_dir() {
        Some(p) => p.join("crashes"),
        None    => return Vec::new(),
    };
    pending_crash_zips_in(&dir)
}

/// Same as `pending_crash_zips` but takes the directory explicitly. Useful
/// for unit tests and for callers that don't go through `logger::init`.
pub fn pending_crash_zips_in(dir: &std::path::Path) -> Vec<PathBuf> {
    pending_crash_zips_in_filtered(dir, false)
}

/// Variant that additionally drops any zip with a sibling `<base>.dismissed`
/// marker. Used by the startup auto-prompt so a user who chose Cancel doesn't
/// get re-prompted on next launch. The unfiltered `pending_crash_zips()` is
/// still used by manual menu entry so the user can reverse the decision.
pub fn pending_crash_zips_excluding_dismissed() -> Vec<PathBuf> {
    let dir = match crate::logger::log_dir() {
        Some(p) => p.join("crashes"),
        None    => return Vec::new(),
    };
    pending_crash_zips_in_filtered(&dir, true)
}

pub fn pending_crash_zips_excluding_dismissed_in(dir: &std::path::Path) -> Vec<PathBuf> {
    pending_crash_zips_in_filtered(dir, true)
}

fn pending_crash_zips_in_filtered(dir: &std::path::Path, exclude_dismissed: bool) -> Vec<PathBuf> {
    let entries = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return Vec::new(),
    };
    let mut zips = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("zip") {
            continue;
        }
        let mut marker = path.clone();
        marker.set_extension("uploaded");
        if marker.exists() {
            continue;
        }
        if exclude_dismissed {
            let mut dismissed = path.clone();
            dismissed.set_extension("dismissed");
            if dismissed.exists() {
                continue;
            }
        }
        zips.push(path);
    }
    zips.sort();
    zips
}

/// Pending feedback (failed uploads) live alongside the logs/ dir under a
/// sibling `feedback/pending/` directory. The uploader writes both the zip
/// and a JSON descriptor here on retry exhaustion.
pub fn pending_feedback_dir() -> Option<PathBuf> {
    let logs = crate::logger::log_dir()?;
    Some(logs.parent()?.join("feedback").join("pending"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn touch(p: &std::path::Path) {
        std::fs::File::create(p).unwrap().write_all(b"x").unwrap();
    }

    #[test]
    fn pending_excludes_uploaded() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let z1 = dir.join("20260502T100000-aaaa1111.zip");
        let z2 = dir.join("20260502T120000-bbbb2222.zip");
        let z3 = dir.join("20260502T130000-cccc3333.zip");
        touch(&z1);
        touch(&z2);
        touch(&z3);
        touch(&dir.join("20260502T120000-bbbb2222.uploaded"));

        let pending = pending_crash_zips_in(dir);
        assert_eq!(pending, vec![z1, z3]);
    }

    #[test]
    fn missing_dir_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("does-not-exist");
        assert!(pending_crash_zips_in(&nonexistent).is_empty());
    }

    #[test]
    fn ignores_non_zip_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        touch(&dir.join("foo.txt"));
        touch(&dir.join("bar.zip"));
        let pending = pending_crash_zips_in(dir);
        assert_eq!(pending.len(), 1);
        assert!(pending[0].ends_with("bar.zip"));
    }

    #[test]
    fn excluding_dismissed_drops_dismissed_only() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let z1 = dir.join("20260516T100000-aaaa.zip");
        let z2 = dir.join("20260516T110000-bbbb.zip");
        let z3 = dir.join("20260516T120000-cccc.zip");
        touch(&z1);
        touch(&z2);
        touch(&z3);
        // z2 dismissed; z3 uploaded; only z1 remains pending under the strict filter.
        touch(&dir.join("20260516T110000-bbbb.dismissed"));
        touch(&dir.join("20260516T120000-cccc.uploaded"));

        let manual = pending_crash_zips_in(dir);
        // Manual entry still sees the dismissed zip but not the uploaded one.
        assert_eq!(manual, vec![z1.clone(), z2.clone()]);

        let auto = pending_crash_zips_excluding_dismissed_in(dir);
        assert_eq!(auto, vec![z1]);
    }

    #[test]
    fn dismissed_plus_uploaded_uploaded_wins() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let z = dir.join("20260516T130000-dddd.zip");
        touch(&z);
        touch(&dir.join("20260516T130000-dddd.dismissed"));
        touch(&dir.join("20260516T130000-dddd.uploaded"));
        // Uploaded > dismissed in both functions: not pending in either.
        assert!(pending_crash_zips_in(dir).is_empty());
        assert!(pending_crash_zips_excluding_dismissed_in(dir).is_empty());
    }

    #[test]
    fn no_dismissed_markers_means_both_filters_agree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let z1 = dir.join("20260516T140000-eeee.zip");
        let z2 = dir.join("20260516T150000-ffff.zip");
        touch(&z1);
        touch(&z2);
        assert_eq!(
            pending_crash_zips_in(dir),
            pending_crash_zips_excluding_dismissed_in(dir),
        );
    }
}
