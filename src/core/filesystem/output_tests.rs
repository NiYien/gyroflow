// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn directory() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/upstream-bugfix-port/fs-tests");
    let path = root.join(format!("{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    create_dir_all(&path).unwrap();
    std::fs::canonicalize(path).unwrap()
}

fn url(path: &Path) -> String { path_to_url(path.to_str().unwrap()) }
fn temporary(name: &std::ffi::OsStr, n: u32) -> std::ffi::OsString {
    format!("{}.{}.tmp", name.to_string_lossy(), n).into()
}

#[test]
fn output_identity_refuses_hard_links_and_sequence_frames() {
    let dir = directory();
    let input = dir.join("frame_0100.png");
    let output = dir.join("result_00001.png");
    std::fs::write(&input, b"original pixels").unwrap();
    hard_link(&input, &output).unwrap();
    assert!(is_same_file(&url(&input), &url(&output)).unwrap());
    let cancel = AtomicBool::new(false);
    assert!(output_overwrites_input(&url(&dir), "result_%05d.png", Some(1), &url(&dir.join("frame_%04d.png")), 100..=104, &cancel).is_some());
    assert!(output_overwrites_input(&url(&dir), "different.mp4", None, &url(&input), 0..=1, &cancel).is_none());
    assert_eq!(std::fs::read(&input).unwrap(), b"original pixels");
}

#[test]
fn failed_output_keeps_the_existing_file_and_removes_its_partial_file() {
    let dir = directory();
    let target = dir.join("output.mp4");
    std::fs::write(&target, b"previous output").unwrap();
    let path;
    {
        let mut temp = TempOutput::create(&url(&target), output_target(&url(&target), Overwrite::Rename).unwrap(), temporary).unwrap();
        path = temp.path().to_owned();
        temp.file().write_all(b"unfinished").unwrap();
    }
    assert!(!path.exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"previous output");
}

#[test]
fn concurrent_outputs_do_not_share_temporary_files() {
    let dir = directory();
    let target = dir.join("output.mp4");
    let mut first = TempOutput::create(&url(&target), target.clone(), temporary).unwrap();
    let mut second = TempOutput::create(&url(&target), target.clone(), temporary).unwrap();
    assert_ne!(first.path(), second.path());
    first.file().write_all(b"first completed").unwrap();
    second.file().write_all(b"second incomplete").unwrap();
    first.publish().unwrap();
    drop(second);
    assert_eq!(std::fs::read(&target).unwrap(), b"first completed");
}

#[test]
fn an_existing_temporary_name_is_never_truncated() {
    let dir = directory();
    let target = dir.join("output.mp4");
    let existing = dir.join("output.mp4.0.tmp");
    std::fs::write(&existing, b"input or previously completed video").unwrap();
    let temp = TempOutput::create(&url(&target), target.clone(), temporary).unwrap();
    assert_ne!(temp.path(), existing);
    drop(temp);
    assert_eq!(std::fs::read(&existing).unwrap(), b"input or previously completed video");
}

#[test]
fn rename_failure_reports_where_the_complete_file_is_kept() {
    let dir = directory();
    let target = dir.join("output.mp4");
    let mut temp = TempOutput::create(&url(&target), target.clone(), temporary).unwrap();
    let kept = temp.path().to_owned();
    temp.file().write_all(b"complete video").unwrap();
    create_dir(&target).unwrap();
    let error = temp.publish().unwrap_err();
    assert!(matches!(error, OutputError::Kept(..)));
    assert!(error.to_string().contains(kept.to_str().unwrap()));
    assert_eq!(std::fs::read(&kept).unwrap(), b"complete video");
    assert!(target.is_dir());
}

#[test]
fn invalid_output_names_and_frame_patterns_are_rejected() {
    let dir = directory();
    assert!(matches!(output_target(&url(&dir), Overwrite::Rename), Err(OutputError::IsAFolder(_))));
    assert!(FramePath::parse("image_%q.png").is_none());
    assert!(FramePath::parse("image_%%.png").is_none());
    assert!(FramePath::parse("image_%99999d.png").unwrap().too_long());
    assert_eq!(FramePath::parse("image_%%_%04d.png").unwrap().frame(12), "image_%_0012.png");
}

#[cfg(windows)]
#[test]
fn an_open_output_is_rejected_before_rendering() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::FILE_SHARE_READ;
    let dir = directory();
    let target = dir.join("output.mp4");
    std::fs::write(&target, b"existing output").unwrap();
    let _opened = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ.0).open(&target).unwrap();
    assert!(matches!(output_target(&url(&target), Overwrite::Rename), Err(OutputError::Busy(_))));
    assert_eq!(std::fs::read(&target).unwrap(), b"existing output");
}
