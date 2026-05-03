// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Startup hook: scan for unuploaded crash dumps and notify the controller
// so it can show the crash-mode FeedbackDialog.

use std::path::PathBuf;

/// Sorted (oldest-first) list of unuploaded crash zips. Caller (controller)
/// is expected to emit `crashCheckpointFound(count)` to QML if the list is
/// non-empty, and to feed these paths into `PackageInputs::crash_zips` when
/// the user submits.
pub fn scan() -> Vec<PathBuf> {
    super::pending_crash_zips()
}

/// Convenience overload taking a custom directory (for tests / non-default
/// data_dir layouts).
pub fn scan_in(dir: &std::path::Path) -> Vec<PathBuf> {
    super::pending_crash_zips_in(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
}
