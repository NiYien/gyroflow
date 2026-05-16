// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Startup hook: scan for unuploaded, non-dismissed crash dumps and notify
// the controller so it can show the crash-mode FeedbackDialog. The dismissed
// filter is what stops the panic-loop scenario from re-prompting on every
// launch — manual menu entry uses the unfiltered list instead so the user
// can still reverse the decision.

use std::path::PathBuf;

/// Sorted (oldest-first) list of unuploaded **and non-dismissed** crash zips.
/// Caller (controller) is expected to emit `crashCheckpointFound(count)` to
/// QML if the list is non-empty, and to feed these paths into
/// `PackageInputs::crash_zips` when the user submits.
pub fn scan() -> Vec<PathBuf> {
    super::pending_crash_zips_excluding_dismissed()
}

/// Convenience overload taking a custom directory (for tests / non-default
/// data_dir layouts).
pub fn scan_in(dir: &std::path::Path) -> Vec<PathBuf> {
    super::pending_crash_zips_excluding_dismissed_in(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn touch(p: &std::path::Path) {
        std::fs::File::create(p).unwrap().write_all(b"x").unwrap();
    }

    #[test]
    fn empty_dir_no_crash() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(scan_in(tmp.path()).is_empty());
    }

    #[test]
    fn returns_sorted_pending() {
        let tmp = tempfile::tempdir().unwrap();
        for name in ["20260502T100000-aaaa1111.zip", "20260502T120000-bbbb2222.zip"] {
            std::fs::File::create(tmp.path().join(name))
                .unwrap()
                .write_all(b"x")
                .unwrap();
        }
        let pending = scan_in(tmp.path());
        assert_eq!(pending.len(), 2);
        assert!(pending[0].file_name().unwrap().to_str().unwrap().starts_with("20260502T100000"));
    }

    #[test]
    fn dismissed_zip_excluded_from_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        touch(&dir.join("20260516T100000-aaaa.zip"));
        touch(&dir.join("20260516T110000-bbbb.zip"));
        touch(&dir.join("20260516T110000-bbbb.dismissed"));

        let pending = scan_in(dir);
        assert_eq!(pending.len(), 1);
        assert!(
            pending[0]
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("20260516T100000")
        );
    }

    #[test]
    fn dismissed_and_uploaded_both_present_means_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        touch(&dir.join("20260516T120000-cccc.zip"));
        touch(&dir.join("20260516T120000-cccc.dismissed"));
        touch(&dir.join("20260516T120000-cccc.uploaded"));
        assert!(scan_in(dir).is_empty());
    }

    #[test]
    fn no_dismissed_markers_identical_to_old_behavior() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        for name in ["20260502T100000-aaaa1111.zip", "20260502T120000-bbbb2222.zip"] {
            touch(&dir.join(name));
        }
        // Manual entry (unfiltered) vs. auto-prompt (filtered) match when no
        // dismissed markers exist.
        assert_eq!(scan_in(dir), super::super::pending_crash_zips_in(dir));
    }
}
