// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Feedback / crash report module. Phase 1 only exposes the pending-crash
// detection API so other code (Phase 4 client UI / startup hook) can decide
// whether to surface a "previous crash" dialog. Packaging, uploading, and
// retry live in Phase 4.

use std::path::PathBuf;

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
        zips.push(path);
    }
    zips.sort();
    zips
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
}
