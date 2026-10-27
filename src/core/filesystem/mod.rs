// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2023 Adrian <adrian.eddy at gmail>

#[cfg(target_os = "android")]
pub mod android;
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod apple;

use itertools::Itertools;
use parking_lot::RwLock;
use std::collections::HashSet;
use std::fs::*;
use std::io::{Read, Write};
use std::path::*;

pub type Result<T> = std::result::Result<T, FilesystemError>;

// Filesystem assumptions:
// 1. All file access should be based on URLs. On Desktop it will be file:///, but on mobile it can be arbitrary
// 2. URL can be arbitrary and doesn't have to contain any names
// 3. We can have access to a folder via URL, but we can't assume it's a path
// 4. We can create new files in a folder and get an URL to that file, and that URL can be arbitrary
// 5. We can have access to file data only, without any info about it's location, folder, parent or other files in the same folder

// Testing:
// - Dragging video file
// - Dragging .gyroflow file
// - Dragging .gyroflow preset
// - Opening video file
// - Opening .gyroflow file
// - Opening .gyroflow preset
// - Dragging lens profile
// - Opening lens profile
// - Dragging gyro data
// - Opening gyro data
// - Changing output path
// - Selecting output folder
// - Rendering to existing file
// - Adding file to queue
// - Adding multiple files to queue
// - Exporting lens profile
// - Dragging preset to render queue
// - Rendering with project file
// - Saving project file
// - Opening video with project file next to it
// - Merging mp4
// - Detecting mp4 sequence
// - Rendering r3d with conversion
// - Using CLI

lazy_static::lazy_static! {
    static ref ALLOWED_FOLDERS: RwLock<HashSet<String>> = RwLock::new(HashSet::new());
}

#[derive(thiserror::Error, Debug)]
pub enum FilesystemError {
    #[error("Invalid url {0:?}")]
    InvalidUrl((String, url::ParseError)),
    #[error("Not a file url {0}")]
    NotAFile(String),
    #[error("Not a folder url {0}")]
    NotAFolder(String),
    #[error("Path doesn't have a parent {0}")]
    NoParent(String),
    #[error("Invalid path {0}")]
    InvalidPath(String),
    #[error("Invalid file descriptor {0}")]
    InvalidFD(i32),
    #[error("Unknown error")]
    Unknown,
    #[error("IO error: {0:?}")]
    IOError(#[from] std::io::Error),
    #[error("String error: {0:?}")]
    Utf8Error(#[from] std::str::Utf8Error),
    #[cfg(target_os = "android")]
    #[error("Java exception: {0:?}")]
    JavaException(#[from] jni::errors::Error),
}
#[macro_export]
macro_rules! function_name {
    () => {{
        fn f() {}
        fn type_name_of<T>(_: T) -> &'static str {
            std::any::type_name::<T>()
        }
        type_name_of(f)
            .rsplit("::")
            .find(|&part| part != "f" && part != "{{closure}}")
            .expect("Short function name")
    }};
}
#[macro_export]
macro_rules! dbg_call {
    ($($param:ident )* $(-> $ret:expr)*) => {{
        let mut dbg_str = format!("{}", function_name!());
        $(
            dbg_str.push_str(&format!(" | {}: {}", stringify!($param), $param));
        )*
        $(
            dbg_str.push_str(" -> ");
            dbg_str.push_str(&format!("{:?}", $ret));
        )*
        log::trace!("{}", dbg_str);
    }};
}
macro_rules! result {
    ($result:expr $(, $param:ident)*) => {{
        let ret = $result;
        dbg_call!($($param )* -> ret);
        match ret {
            Ok(x) => x,
            Err(e) => { log::error!("{e:?}"); Default::default() }
        }
    }};
}
mod sequence;
pub use sequence::*;
#[cfg(test)]
mod output_tests;

pub fn start_accessing_url(_url: &str, _is_folder: bool) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    apple::start_accessing_url(_url, _is_folder);
}
pub fn stop_accessing_url(_url: &str, _is_folder: bool) {
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    apple::stop_accessing_url(_url, _is_folder);
}

pub struct FileWrapper {
    pub size: usize,
    pub url: String,

    file: Option<std::fs::File>,

    #[cfg(target_os = "android")]
    pub android_handle: android::AndroidFileHandle,
}
impl FileWrapper {
    pub fn get_file(&mut self) -> &mut File {
        self.file.as_mut().unwrap()
    }
}
impl Read for FileWrapper {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::Other))?
            .read(buf)
    }
}
impl std::io::Seek for FileWrapper {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.file
            .as_mut()
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::Other))?
            .seek(pos)
    }
}
impl Drop for FileWrapper {
    fn drop(&mut self) {
        log::debug!("FileWrapper::drop {}", self.url);
        #[cfg(target_os = "android")]
        if let Some(f) = self.file.take() {
            // Discard the std::fs::File to prevent from closing the file descriptor.
            // It will be closed in Drop for AndroidFileHandle.
            std::mem::forget(f);
        }
        stop_accessing_url(&self.url, false);
    }
}
pub struct FfmpegPathWrapper {
    pub org_url: String,
    pub path: String,
    #[cfg(target_os = "android")]
    _file: FileWrapper,
}
impl FfmpegPathWrapper {
    pub fn new(url: &str, _write: bool) -> Result<Self> {
        log::debug!("FfmpegPathWrapper::new {url}, write: {_write}");
        #[cfg(target_os = "android")]
        {
            // On android we have to use raw file descriptor, because ffmpeg can't use the content:// urls
            let file = FileWrapper::open_android(&url, if _write { "wt" } else { "r" })?;
            Ok(Self {
                org_url: url.to_owned(),
                path: format!("fd:{}", file.android_handle.fd),
                _file: file,
            })
        }
        #[cfg(not(target_os = "android"))]
        {
            start_accessing_url(url, false);
            let mut path = url.to_owned();
            if path.starts_with("file://") {
                path = url_to_path(&path);
            }
            Ok(Self {
                org_url: url.to_owned(),
                path: path,
            })
        }
    }
}

#[cfg(not(target_os = "android"))]
impl Drop for FfmpegPathWrapper {
    fn drop(&mut self) {
        log::debug!("FfmpegPathWrapper::drop {}", self.org_url);
        stop_accessing_url(&self.org_url, false);
    }
}

fn url_to_pathbuf(mut url: &str) -> Result<PathBuf> {
    Ok(if url.contains("://") {
        // It's an url
        url::Url::parse(url)
            .map_err(|e| FilesystemError::InvalidUrl((url.into(), e)))?
            .to_file_path()
            .map_err(|_| FilesystemError::NotAFile(url.into()))?
    } else {
        if url.starts_with("//?/") {
            url = &url[4..];
        } // Windows extended path
        Path::new(url).to_path_buf()
    })
}

pub fn get_filename(url: &str) -> String {
    fn inner(url: &str) -> Result<String> {
        if url.is_empty() {
            return Ok(String::new());
        }
        if !url.contains("://") && !url.contains('/') && !url.contains('\\') {
            return Ok(url.to_owned());
        }

        #[cfg(target_os = "android")]
        if url.starts_with("content://") {
            if android::is_dir_url(url) {
                return Ok(String::new());
            } // no filename
            return Ok(android::get_url_info(url)
                .map(|x| x.filename.unwrap_or_default())
                .unwrap_or_default());
        } else if !url.starts_with("file://") {
            log::error!("Unknown android url scheme: {url}");
        }

        match url_to_pathbuf(url) {
            Ok(pathbuf) => {
                let s = pathbuf.to_str().unwrap();
                if pathbuf.is_dir() && !s.ends_with(".RDC") && !s.ends_with(".RDM") {
                    return Ok(String::new());
                }
                Ok(pathbuf
                    .file_name()
                    .ok_or(FilesystemError::NotAFile(url.into()))?
                    .to_string_lossy()
                    .to_string())
            }
            // Cross-OS URL fallback: Url::to_file_path rejects e.g.
            // file:///Users/... on Windows (no drive letter), which would
            // otherwise drop the filename and break the same-folder fallback
            // in StabilizationManager::get_new_videofile_url.
            Err(_) => Ok(filename_from_url_string(url)),
        }
    }
    result!(inner(url), url)
}

fn filename_from_url_string(url: &str) -> String {
    let mut s = url;
    if let Some(pos) = s.find("://") {
        let after_scheme = &s[pos + 3..];
        match after_scheme.find('/') {
            Some(slash) => s = &after_scheme[slash..],
            None => return String::new(),
        }
    }
    if let Some(q) = s.find(|c| c == '?' || c == '#') {
        s = &s[..q];
    }
    if s.ends_with('/') || s.ends_with('\\') {
        return String::new();
    }
    let segment = match s.rfind(|c| c == '/' || c == '\\') {
        Some(i) => &s[i + 1..],
        None => s,
    };
    match urlencoding::decode(segment) {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => segment.to_string(),
    }
}
pub fn get_folder(url: &str) -> String {
    fn inner(url: &str) -> Result<String> {
        if url.is_empty() {
            return Ok(String::new());
        }
        if url.ends_with("/") {
            // it's already a directory url
            return Ok(url.to_string());
        }

        #[cfg(target_os = "android")]
        if url.starts_with("content://") {
            if android::is_dir_url(url) {
                // it's already a directory url
                return Ok(url.to_string());
            }
            // Try to find the folder in the allowed list
            if let Some(doc_pos) = url.find("/document/") {
                let tree_part = if url.contains("/tree/") {
                    url[..doc_pos + 1].to_string()
                } else if let Some(pos2) = url.rfind("%2F") {
                    url[..pos2].replace("/document/", "/tree/")
                } else {
                    url.to_string()
                };
                let lock = ALLOWED_FOLDERS.read();
                for x in lock.iter() {
                    if tree_part.trim_end_matches('/') == x.trim_end_matches('/') {
                        return Ok(x.clone());
                    }
                }
            }

            log::warn!(
                "Cannot get directory path on android, url: {url}, info: {:?}",
                android::get_url_info(url)
            );
            return Ok(String::new());
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if is_sandboxed() {
            if let Some(pos) = url.rfind('/') {
                let folder = &url[0..pos + 1];
                if ALLOWED_FOLDERS
                    .read()
                    .contains(&normalize_url(folder, true))
                {
                    return Ok(folder.to_string());
                }
            }
            return Ok(String::new());
        }

        let pathbuf = url_to_pathbuf(url)?;
        if pathbuf.is_dir() {
            return Ok(path_to_url(&pathbuf.to_string_lossy()));
        }
        Ok(path_to_url(
            &pathbuf
                .parent()
                .ok_or(FilesystemError::NoParent(url.into()))?
                .to_string_lossy(),
        ))
    }
    let mut ret = inner(url);
    if let Ok(ref mut ret) = ret {
        if !ret.is_empty() && !ret.ends_with('/') {
            ret.push('/');
        }
    }
    result!(ret, url)
}

pub fn exists(url: &str) -> bool {
    fn inner(url: &str) -> Result<bool> {
        if url.is_empty() {
            return Ok(false);
        }

        #[cfg(target_os = "android")]
        if url.starts_with("content://") {
            return android::get_url_info(url)
                .map(|x| x.filename.is_some() && !x.filename.unwrap().is_empty());
        }

        start_accessing_url(url, false);
        let exists = url_to_pathbuf(url).map(|x| x.exists());
        stop_accessing_url(url, false);
        exists
    }
    result!(inner(url), url)
}
// True iff the URL points to an existing directory. Used by QML DropArea
// hover/drop logic to distinguish folders from files without relying on a
// "filename contains a dot" heuristic (which breaks for folders like
// `Footage.2024` or `Clips.v1`).
pub fn is_dir(url: &str) -> bool {
    fn inner(url: &str) -> Result<bool> {
        if url.is_empty() {
            return Ok(false);
        }

        #[cfg(target_os = "android")]
        if url.starts_with("content://") {
            return Ok(android::is_dir_url(url));
        }

        start_accessing_url(url, true);
        let is_dir = url_to_pathbuf(url).map(|x| x.is_dir());
        stop_accessing_url(url, true);
        is_dir
    }
    result!(inner(url), url)
}
// True iff the URL is a bare SAF tree URI (as returned by ACTION_OPEN_DOCUMENT_TREE
// pickers), i.e. it has a /tree/ segment but no /document/ segment. Such URIs
// cannot be queried via ContentResolver directly - they must be converted to a
// document URI first (DocumentsContract.buildDocumentUriUsingTree).
pub fn is_bare_content_tree_url(url: &str) -> bool {
    url.starts_with("content://") && url.contains("/tree/") && !url.contains("/document/")
}
// Restores the fully percent-encoded form a SAF URI must be in before it reaches
// ContentResolver.
//
// SAF matches URI permission grants by exact `Uri.toString()` equality, so a
// content:// URL has to arrive in byte-identical form to what the picker handed
// out. QML's `url` type decodes on the way through: a picked path comes back with
// spaces and non-ASCII characters raw while %3A / %2F stay encoded. That mixed
// form no longer matches the grant, the resulting SecurityException is swallowed
// by the JNI wrapper, and the only visible symptom is an empty filename and a
// "video could not be read" error. The pre-existing space handling covered the
// ASCII half of this; non-ASCII paths (any CJK folder name) hit the same wall.
//
// Idempotent: an already-encoded URL contains no spaces and is pure ASCII, so it
// is returned untouched. Non-content URLs keep the historical space-only
// behaviour so local file paths are unaffected.
pub fn reencode_url_for_android(url: &str) -> std::borrow::Cow<'_, str> {
    let encode_non_ascii = url.starts_with("content://");
    if !url.contains(' ') && !(encode_non_ascii && !url.is_ascii()) {
        return std::borrow::Cow::Borrowed(url);
    }
    let mut out = String::with_capacity(url.len() + 8);
    for ch in url.chars() {
        if ch == ' ' {
            out.push_str("%20");
        } else if ch.is_ascii() || !encode_non_ascii {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for b in ch.encode_utf8(&mut buf).as_bytes() {
                // Uppercase hex to match what Android's Uri encoder emits.
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    std::borrow::Cow::Owned(out)
}
pub fn exists_in_folder(folder_url: &str, filename: &str) -> bool {
    fn inner(folder_url: &str, filename: &str) -> bool {
        if folder_url.is_empty() || filename.is_empty() {
            return false;
        }

        #[cfg(target_os = "android")]
        if folder_url.starts_with("content://") && android::is_dir_url(folder_url) {
            if let Ok(files) = android::list_files(folder_url) {
                let cmp = Some(filename.to_owned());
                for x in files {
                    if x.filename == cmp {
                        return true;
                    }
                }
                return false;
            }
        }
        exists(&get_file_url(folder_url, filename, false))
    }

    let ret = inner(folder_url, filename);
    dbg_call!(folder_url filename -> ret);
    ret
}
pub fn list_folder(folder_url: &str) -> Vec<(String, String)> {
    fn inner(folder_url: &str) -> Vec<(String, String)> {
        let mut ret = Vec::new();

        #[cfg(target_os = "android")]
        if folder_url.starts_with("content://") && android::is_dir_url(folder_url) {
            if let Ok(files) = android::list_files(folder_url) {
                for x in files {
                    if let Some(url) = x.url {
                        if let Some(filename) = x.filename {
                            ret.push((filename, url));
                        }
                    }
                }
                return ret;
            }
        }
        if let Ok(entries) = std::fs::read_dir(url_to_path(folder_url)) {
            for entry in entries {
                if let Ok(entry) = entry {
                    if entry
                        .metadata()
                        .as_ref()
                        .map(|x| x.is_file() || x.is_dir())
                        .unwrap_or_default()
                    {
                        ret.push((
                            entry.file_name().to_string_lossy().to_string(),
                            path_to_url(entry.path().to_string_lossy().as_ref()),
                        ));
                    }
                }
            }
        }
        ret
    }

    let ret = inner(folder_url);
    dbg_call!(folder_url -> ret);
    ret
}
pub fn get_mime(filename: &str) -> &'static str {
    if filename.is_empty() || !filename.contains('.') {
        return "application/octet-stream";
    }
    let pos = filename.rfind('.').unwrap();
    let ext = filename[pos + 1..].to_ascii_lowercase();
    match ext.as_str() {
        "gyroflow" => "application/octet-stream",
        "json" => "application/json",
        "gcsv" => "application/octet-stream",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        "png" => "image/png",
        "exr" => "image/x-exr",
        _ => "application/octet-stream",
    }
}
pub fn get_file_url(folder_url: &str, filename: &str, can_create: bool) -> String {
    fn inner(folder_url: &str, filename: &str, _can_create: bool) -> Result<String> {
        if folder_url.is_empty() {
            return Ok(String::new());
        }

        #[cfg(target_os = "android")]
        if folder_url.starts_with("content://") && android::is_dir_url(folder_url) {
            if let Ok(files) = android::list_files(folder_url) {
                let cmp = Some(filename.to_owned());
                for x in files {
                    if x.filename == cmp {
                        if let Some(url) = x.url {
                            return Ok(url);
                        }
                    }
                }
                if _can_create {
                    match android::create_file(folder_url, filename, get_mime(filename)) {
                        Ok(new_url) => return Ok(new_url),
                        Err(e) => {
                            log::error!("android::create_file failed: {e:?}");
                        }
                    }
                }
                return Ok(String::new());
            }
        }

        let filename_escaped = urlencoding::encode(filename);
        if folder_url.ends_with('/') || folder_url.ends_with('\\') {
            Ok(format!("{folder_url}{filename_escaped}"))
        } else {
            Ok(format!("{folder_url}/{filename_escaped}"))
        }
    }
    result!(
        inner(folder_url, filename, can_create),
        folder_url,
        filename,
        can_create
    )
}
pub fn read(url: &str) -> Result<Vec<u8>> {
    dbg_call!(url);
    start_accessing_url(url, false);
    let buf = {
        let mut f = open_file(&url, false, false)?;
        let mut buf = Vec::with_capacity(f.size);
        f.get_file().read_to_end(&mut buf)?;
        buf
    };
    stop_accessing_url(url, false);
    Ok(buf)
}
pub fn write(url: &str, data: &[u8]) -> Result<()> {
    dbg_call!(url);
    start_accessing_url(url, false);
    {
        let mut f = open_file(&url, true, true)?;
        f.get_file().write(data)?;
    }
    stop_accessing_url(url, false);
    Ok(())
}
pub fn read_to_string(url: &str) -> Result<String> {
    dbg_call!(url);
    let data = read(url)?;
    Ok(std::str::from_utf8(&data)?.to_owned())
}
pub fn filename_with_extension(filename: &str, ext: &str) -> String {
    let new_filename = if let Some(pos) = filename.rfind('.') {
        format!("{}.{ext}", &filename[0..pos])
    } else {
        filename.to_string()
    };
    dbg_call!(filename ext -> new_filename);
    new_filename
}
pub fn filename_with_suffix(filename: &str, suffix: &str) -> String {
    let new_filename = if let Some(pos) = filename.rfind('.') {
        format!("{}{suffix}{}", &filename[0..pos], &filename[pos..])
    } else {
        filename.to_string()
    };
    dbg_call!(filename suffix -> new_filename);
    new_filename
}

pub fn remove_file(url: &str) -> Result<()> {
    dbg_call!(url);
    #[cfg(target_os = "android")]
    {
        if url.starts_with("file://") {
            Ok(std::fs::remove_file(url_to_pathbuf(url)?)?)
        } else {
            android::remove_file(url).map(|_| ())
        }
    }
    #[cfg(not(target_os = "android"))]
    {
        let path = url_to_path(url);
        Ok(std::fs::remove_file(path)?)
    }
}

pub fn can_open_file(url: &str) -> bool {
    dbg_call!(url);
    if !exists(url) {
        return false;
    }
    let x = open_file(url, false, false).is_ok();
    x
}

pub fn is_ios_photo_import_url(url: &str) -> bool {
    url.contains("/Library/Caches/") && url.contains("/ios-photo-imports/")
}

pub fn can_create_file(folder: &str, filename: &str) -> bool {
    if folder.is_empty() || filename.is_empty() {
        return false;
    }
    fn inner(folder: &str, filename: &str) -> bool {
        // Direct Android paths are checked by the OS permission and the write probe below.
        let direct_android = cfg!(target_os = "android") && folder.starts_with("file://");
        if is_sandboxed() && !direct_android && folder.contains("://") {
            let lock = ALLOWED_FOLDERS.read();
            if !lock.contains(&normalize_url(folder, true)) {
                return false; // Access not allowed
            }
        }

        let already_exists = exists(&get_file_url(folder, filename, false));

        let url = get_file_url(folder, filename, true);
        if !url.is_empty() {
            if open_file(&url, true, false).is_ok() {
                if !already_exists {
                    let _ = remove_file(&url);
                }
                return true;
            }
        }
        false
    }
    let ret = inner(folder, filename);
    dbg_call!(folder filename -> ret);
    ret
}

pub fn open_file(url: &str, writing: bool, truncate: bool) -> Result<FileWrapper> {
    dbg_call!(url writing);
    start_accessing_url(url, false);

    #[cfg(target_os = "android")]
    {
        return FileWrapper::open_android(
            url,
            if writing && truncate {
                "wt"
            } else if writing {
                "w"
            } else {
                "r"
            },
        );
    }
    #[cfg(not(target_os = "android"))]
    {
        let path = url_to_path(url);
        let file = if (writing && truncate) || (writing && !exists(url)) {
            File::create(path)?
        } else if writing {
            OpenOptions::new().read(true).write(true).open(path)?
        } else {
            File::open(path)?
        };
        let size = file.metadata()?.len() as usize;
        Ok(FileWrapper {
            file: Some(file),
            size,
            url: url.to_owned(),
        })
    }
}

/// What identifies a file whatever URL or path reaches it: another spelling of the path, a letter case variant on a
/// case-insensitive volume, a symbolic or a hard link, another document URL of it on Android. The device and inode numbers
/// on Unix (Android and Apple too: a `content://` file is read through its descriptor), the volume serial number and file ID
/// on Windows, and the canonical path on a filesystem that gives no IDs
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FileId(FileIdRepr);
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FileIdRepr {
    /// Where the ID comes from, the device or volume, and the inode or file ID
    Id(u8, u64, u128),
    Path(PathBuf),
}
impl FileWrapper {
    /// The identity of the open file
    pub fn id(&self) -> Result<FileId> {
        let file = self.file.as_ref().ok_or(FilesystemError::Unknown)?;
        Ok(FileId(match system_file_id(file)? {
            Some(id) => id,
            None => FileIdRepr::Path(std::fs::canonicalize(url_to_pathbuf(&self.url)?)?)
        }))
    }
}
#[cfg(unix)]
fn system_file_id(file: &File) -> Result<Option<FileIdRepr>> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata()?;
    Ok(Some(FileIdRepr::Id(0, m.dev(), m.ino() as u128)))
}
/// The volume serial number and the 128-bit file ID of `FILE_ID_INFO` (ReFS's IDs don't fit 64 bits), or where a filesystem
/// doesn't give those (exFAT, FAT), the 64-bit file index of `BY_HANDLE_FILE_INFORMATION`. One volume always gives the same
/// kind, and the kind is part of the ID, so the two are never compared. A file ID of 0 isn't one: some network filesystems
/// give 0 for every file
#[cfg(windows)]
fn system_file_id(file: &File) -> Result<Option<FileIdRepr>> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::{ Foundation::HANDLE, Storage::FileSystem::* };
    let handle = HANDLE(file.as_raw_handle());
    let mut info = FILE_ID_INFO::default();
    // SAFETY: the handle is the open file's, and `info` is a FILE_ID_INFO of the size given
    if unsafe { GetFileInformationByHandleEx(handle, FileIdInfo, &mut info as *mut FILE_ID_INFO as *mut _, std::mem::size_of::<FILE_ID_INFO>() as u32) }.is_ok() {
        let id = u128::from_le_bytes(info.FileId.Identifier);
        return Ok((id != 0).then_some(FileIdRepr::Id(1, info.VolumeSerialNumber, id)));
    }
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is the open file's, and `info` is a BY_HANDLE_FILE_INFORMATION
    unsafe { GetFileInformationByHandle(handle, &mut info) }.map_err(std::io::Error::from)?;
    let id = (info.nFileIndexHigh as u128) << 32 | info.nFileIndexLow as u128;
    Ok((id != 0).then_some(FileIdRepr::Id(2, info.dwVolumeSerialNumber as u64, id)))
}
#[cfg(not(any(unix, windows)))]
fn system_file_id(_file: &File) -> Result<Option<FileIdRepr>> {
    Ok(None)
}
/// The identity of the file at `url`, `None` when there's no file there
pub fn file_id(url: &str) -> Result<Option<FileId>> {
    if url.is_empty() || !exists(url) { return Ok(None); }
    Ok(Some(open_file(url, false, false)?.id()?))
}
/// The identity of the file or folder at `path`, through the symbolic links there, and how many names it has (hard links;
/// `None` where that can't be told). A file with one name is reached only by it, and by the symbolic links to it
pub fn path_id(path: &Path) -> Result<(FileId, Option<u64>)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path)?;
        Ok((FileId(FileIdRepr::Id(0, m.dev(), m.ino() as u128)), Some(m.nlink())))
    }
    #[cfg(windows)]
    {
        use std::os::windows::{ fs::OpenOptionsExt, io::AsRawHandle };
        use windows::Win32::{ Foundation::HANDLE, Storage::FileSystem::* };
        // Its attributes only, which no program's sharing keeps from, and so a folder too
        let file = OpenOptions::new().access_mode(FILE_READ_ATTRIBUTES.0).custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0).open(path)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: the handle is the open file's, and `info` is a BY_HANDLE_FILE_INFORMATION
        let links = unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut info) }.ok().map(|_| info.nNumberOfLinks as u64);
        Ok((FileId(match system_file_id(&file)? {
            Some(id) => id,
            None => FileIdRepr::Path(std::fs::canonicalize(path)?)
        }), links))
    }
    #[cfg(not(any(unix, windows)))]
    {
        Ok((FileId(FileIdRepr::Path(std::fs::canonicalize(path)?)), None))
    }
}
/// The file `path` reaches: the one at the end of the symbolic links there (as [`output_target`] finds it), or `path` itself
pub fn resolve_links(path: &Path) -> std::io::Result<PathBuf> {
    if is_link(path) { linked_file(path) } else { Ok(path.to_owned()) }
}
/// Whether the URLs `a` and `b` reach the same file, by its [`FileId`]: `false` when there's no file at either
pub fn is_same_file(a: &str, b: &str) -> Result<bool> {
    let ret = match (file_id(a)?, file_id(b)?) {
        (Some(a), Some(b)) => a == b,
        _ => false
    };
    dbg_call!(a b -> ret);
    Ok(ret)
}

/// How an output replaces a file that's already where it goes
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overwrite {
    /// The complete output is renamed onto it, which replaces it at once (desktop)
    Rename,
    /// It's written into in place: an image sequence's frames, and outputs on Android and iOS
    InPlace,
}

/// What keeps an output from being written where it goes, found by [`output_target`] before anything is written, or by
/// [`publish`]. The first field is the output, as it's shown
#[derive(thiserror::Error, Debug)]
pub enum OutputError {
    #[error("The output {0} is a folder. Choose another name for the output.")]
    IsAFolder(String),
    #[error("The output {0} isn't a file name. Choose another name for the output.")]
    NotAFileName(String),
    #[error("The output {0} can't be created in {1}: {2}")]
    NoFolder(String, String, std::io::Error),
    #[error("The output {0} is open in another program, which keeps it from being overwritten. Close it there, or choose another name or folder for the output.")]
    Busy(String),
    #[error("The output {0} can't be written to: {1}")]
    CantWrite(String, std::io::Error),
    #[error("The output {0} can't be replaced: {1}")]
    CantReplace(String, std::io::Error),
    #[error("Failed to check the output {0}: {1}")]
    Check(String, std::io::Error),
    #[error("The complete output couldn't be given the name {0}: {1}")]
    Publish(String, std::io::Error),
    #[error("Failed to create the output {0}: {1}")]
    Create(String, std::io::Error),
    #[error("Failed to write the output {0}: {1}")]
    Write(String, std::io::Error),
    /// Why the complete output couldn't get its name, and the file it's kept in
    #[error("{}", kept_message(.0, .1))]
    Kept(Box<OutputError>, PathBuf),
}
fn kept_message(e: &OutputError, kept: &Path) -> String {
    let e = e.to_string();
    format!("{e}{} The complete output is kept as {}.", if e.ends_with('.') { "" } else { "." }, kept.display())
}

/// The file that an output at `url` gets written to (a path, so not Android's documents, which have no links either),
/// checked before anything is written, so that writing it can't fail for what's already there. Nothing is created or
/// changed:
/// - a symbolic link there is followed, as creating the file follows it: the file at the end of its links is the one
///   replaced, or the one created where they point, and the links stay. A loop of links is refused, and so is a link to what
///   isn't a file name, or on Windows to a name that a program's path names otherwise than the link does;
/// - a new file needs a name (no separator or `.` at the end, on Windows no `:`) and a folder that exists;
/// - an existing file is replaced only where it can be written to, and with [`Overwrite::Rename`] only where the rename can
///   replace it: on Windows a program that has it open without letting it be deleted keeps it from that (FFmpeg opens
///   files so, and with it a player)
pub fn output_target(url: &str, overwrite: Overwrite) -> std::result::Result<PathBuf, OutputError> {
    start_accessing_url(url, false);
    let ret = url_to_pathbuf(url)
        .map_err(|e| OutputError::Check(display_url(url), std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())))
        .and_then(|path| path_target(&path, overwrite, &|| display_url(url)));
    stop_accessing_url(url, false);
    dbg_call!(url -> ret);
    ret
}

/// [`output_target`] of a `path`, shown as `display()`
pub fn path_target(path: &Path, overwrite: Overwrite, display: &dyn Fn() -> String) -> std::result::Result<PathBuf, OutputError> {
    use std::io::ErrorKind;
    if not_a_file_name(path) {
        return Err(OutputError::NotAFileName(display()));
    }
    // Through the symbolic links there, which the system follows as it does to create the file, where it lets them be
    // followed: it reports a loop itself
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => Err(OutputError::IsAFolder(display())),
        Ok(_) => {
            // The file replaced: the one at the end of the symbolic links there, which keep pointing to it
            let target = if is_link(path) { linked_file(path).map_err(|e| OutputError::Check(display(), e))? } else { path.to_owned() };
            file_target(&target, overwrite, display)
        },
        Err(e) if e.kind() == ErrorKind::NotFound => {
            // Nothing there, or a symbolic link to where nothing is yet: the file is created where it points
            let target = if is_link(path) { links_end(path).map_err(|e| OutputError::Check(display(), e))? } else { path.to_owned() };
            let shown = || if target == path { display() } else { format!("{}, a symbolic link to {},", display(), target.display()) };
            if not_a_file_name(&target) {
                return Err(OutputError::NotAFileName(shown()));
            }
            let dir = target.parent().filter(|x| !x.as_os_str().is_empty()).unwrap_or(Path::new("."));
            match std::fs::metadata(dir) {
                Ok(m) if m.is_dir() => Ok(target),
                Ok(_) => Err(OutputError::NoFolder(shown(), dir.display().to_string(), std::io::Error::from(ErrorKind::NotADirectory))),
                Err(e) => Err(OutputError::NoFolder(shown(), dir.display().to_string(), e))
            }
        },
        Err(e) => Err(OutputError::Check(display(), e))
    }
}

/// [`path_target`] of the file `path`, where it's known to be there and to be no symbolic link nor folder (as a listing of
/// its folder tells): it's replaced only where it can be written to, and with [`Overwrite::Rename`] only where the rename can
/// replace it
pub fn file_target(path: &Path, overwrite: Overwrite, display: &dyn Fn() -> String) -> std::result::Result<PathBuf, OutputError> {
    if let Err(e) = OpenOptions::new().write(true).open(path) {
        return Err(if is_busy(&e) { OutputError::Busy(display()) } else { OutputError::CantWrite(display(), e) });
    }
    #[cfg(windows)]
    if overwrite == Overwrite::Rename && let Err(e) = can_be_replaced(path) {
        return Err(if is_busy(&e) { OutputError::Busy(display()) } else { OutputError::CantReplace(display(), e) });
    }
    let _ = overwrite;
    Ok(path.to_owned())
}

/// A complete output's file while it's written: a new file in the folder of the file that gets the output's name (`target`, as
/// [`output_target`] found it for the output's `url`), which gets that name once it's complete and on disk
/// ([`TempOutput::publish`]). So an output that fails never leaves a partial file under that name, nor replaces the file that
/// was there. It's removed when dropped before that, and kept, which the error says, when only the rename fails.
///
/// Existing temporary names may be inputs or completed outputs kept after a failed rename, so they are never truncated.
/// Creating a new name atomically also keeps concurrent renders separate.
pub struct TempOutput {
    /// Keeps the file open until publishing or cleanup.
    file: File,
    path: PathBuf,
    target: PathBuf,
    url: String,
    done: bool,
}
impl TempOutput {
    /// Tries another name whenever one already exists.
    pub fn create(url: &str, target: PathBuf, name: impl Fn(&std::ffi::OsStr, u32) -> std::ffi::OsString) -> std::result::Result<Self, OutputError> {
        let dir = target.parent().filter(|x| !x.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_owned();
        let error = |e: std::io::Error| OutputError::Create(display_url(url), std::io::Error::new(e.kind(), format!("creating a temporary file in {}: {e}", dir.display())));
        let file_name = target.file_name().unwrap_or_default();
        for n in 0..1000u32 {
            let path = dir.join(name(file_name, n));
            let file = match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                // A name taken otherwise, as by a folder on Windows
                Err(e) => if std::fs::symlink_metadata(&path).is_ok() { continue; } else { return Err(error(e)); }
            };
            dbg_call!(url -> path);
            return Ok(Self { file, path, target, url: url.to_owned(), done: false });
        }
        Err(error(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "every temporary name is in use")))
    }
    pub fn path(&self) -> &Path { &self.path }
    pub fn file(&mut self) -> &mut File { &mut self.file }
    /// Once it's on disk, it gets the output's name, which replaces the file there at once. A program that opened that file
    /// since is told apart. Kept where only that rename fails
    pub fn publish(mut self) -> std::result::Result<(), OutputError> {
        self.file.sync_all().map_err(|e| OutputError::Write(display_url(&self.url), e))?;
        self.done = true;
        publish(&self.path, &self.target, &self.url).map_err(|e| OutputError::Kept(Box::new(e), self.path.clone()))
    }
}
impl Drop for TempOutput {
    fn drop(&mut self) {
        // Remove only this render's partial file.
        if !self.done && let Err(e) = std::fs::remove_file(&self.path) {
            log::error!("Failed to remove the partial output {}: {e:?}", self.path.display());
        }
    }
}


/// Gives the complete output `temp` (on disk) the name of `target`, which [`output_target`] found for `url`: a rename, which
/// replaces what's there at once. A program that opened the file there since is told apart
fn publish(temp: &Path, target: &Path, url: &str) -> std::result::Result<(), OutputError> {
    start_accessing_url(url, false);
    let ret = std::fs::rename(temp, target).map_err(|e| {
        #[cfg(windows)]
        if can_be_replaced(target).is_err_and(|e| is_busy(&e)) {
            return OutputError::Busy(display_url(url));
        }
        OutputError::Publish(display_url(url), e)
    });
    stop_accessing_url(url, false);
    dbg_call!(url -> ret);
    ret
}

/// Whether `path` is a symbolic link (on Windows also a junction)
fn is_link(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
}

/// Whether `path` names no file: it ends with a separator, its last part is `.` or `..`, or on Windows its name holds a `:`.
/// Refused alike on every system: on Unix creating the file and `rename` both refuse those, Windows refuses a separator at the
/// end too, and there a `:` names a stream of a file, which a rename can't give the output
fn not_a_file_name(path: &Path) -> bool {
    let text = path.as_os_str().as_encoded_bytes();
    let separator = |x: &u8| std::path::is_separator(*x as char);
    // `file_name` skips a `.` at the end
    let dot = text.strip_suffix(b".").is_some_and(|x| x.last().is_none_or(separator));
    text.last().is_some_and(separator) || dot || path.file_name().is_none_or(|x| cfg!(windows) && x.as_encoded_bytes().contains(&b':'))
}

/// Where the symbolic links from `path` on lead: each followed to what it points to, a relative one from its folder, up to
/// the first path that isn't a link, which needn't exist. A relative one is joined as it is, so the system resolves the path
/// that results as it would have resolved the link: a `..` in it physically on Unix, by the letters of the path on Windows.
/// Windows takes those letters from the path the link was reached by, in which a directory symbolic link has been replaced
/// by its target but a junction hasn't: a `..` that would go up to another folder for one than for the other isn't followed.
/// Each must point to a file name
fn links_end(path: &Path) -> std::io::Result<PathBuf> {
    let folder = |p: &Path| p.parent().filter(|x| !x.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_owned();
    let mut end = path.to_owned();
    // As many links as Windows follows in a path, more than Linux (40) and macOS (32), so a chain the system followed is never
    // cut short; and then what the last one leads to
    for _ in 0..=63 {
        if !is_link(&end) {
            return Ok(end);
        }
        let to = std::fs::read_link(&end)?;
        if not_a_file_name(&to) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("the symbolic link {} points to {}, which isn't a file name", end.display(), to.display())));
        }
        // Windows follows a link by the letters of `to`, while a program's path loses the dots and spaces at the end of its
        // parts (`GetFullPathNameW`) and reads a `:` as a stream: through `final.mp4.` creating the file makes `final.mp4.`,
        // which the rename would publish as `final.mp4`, leaving the link pointing to nothing
        let renamed = |x: &[u8]| x.ends_with(b".") || x.ends_with(b" ") || x.contains(&b':');
        if cfg!(windows) && to.components().any(|x| matches!(x, Component::Normal(x) if renamed(x.as_encoded_bytes()))) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("the symbolic link {} points to {}, a name that Windows follows as it's written but programs can't use: a part of it ends with a dot or a space, or holds a ':'", end.display(), to.display())));
        }
        let next = end.parent().unwrap_or(Path::new("")).join(&to);
        let relative = matches!(to.components().next(), Some(Component::Normal(_) | Component::CurDir | Component::ParentDir));
        if cfg!(windows) && relative && to.components().any(|x| x == Component::ParentDir) {
            // The folder it goes into, and the one `..` going up from where the link's folder really is leads to
            let refused = |why: String| std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("the symbolic link {} points to {}, up from its folder, {why}", end.display(), to.display()));
            match (std::fs::canonicalize(folder(&next)), std::fs::canonicalize(folder(&end)).map(|dir| std::fs::canonicalize(folder(&dir.join(&to))))) {
                (Ok(a), Ok(Ok(b))) if a == b => {},
                // Neither is there: it leads into a folder that doesn't exist, which the check then says
                (Err(a), Ok(Err(b))) if a.kind() == std::io::ErrorKind::NotFound && b.kind() == std::io::ErrorKind::NotFound => {},
                (Err(e), Ok(Err(_))) | (_, Err(e)) => return Err(refused(format!("and where that folder really is can't be told: {e}"))),
                _ => return Err(refused("which is reached through another link: which folder Windows takes it to can't be told".to_owned()))
            }
        }
        end = next;
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "it leads through more than 63 symbolic links, or a loop of them"))
}

/// The file the symbolic link `path` leads to: its canonical path, or where its volume has none (on Windows one without a
/// DOS name), the end of its links when that is the same file as the one opened through the link
fn linked_file(path: &Path) -> std::io::Result<PathBuf> {
    let e = match std::fs::canonicalize(path) {
        Ok(target) => return Ok(target),
        Err(e) => e
    };
    let end = links_end(path)?;
    let id = |p: &Path| File::open(p).ok().and_then(|f| system_file_id(&f).ok().flatten());
    match (id(path), id(&end)) {
        (Some(a), Some(b)) if a == b => Ok(end),
        _ => Err(std::io::Error::new(e.kind(), format!("finding the file its symbolic links lead to: {e}")))
    }
}

/// Whether `e` is a sharing violation: a program has the file open without sharing what was asked for
fn is_busy(e: &std::io::Error) -> bool {
    #[cfg(windows)]
    { e.raw_os_error() == Some(windows::Win32::Foundation::ERROR_SHARING_VIOLATION.0 as i32) }
    #[cfg(not(windows))]
    { let _ = e; false }
}

/// Whether the file `path` can be replaced by a rename: opening it to delete it fails as the rename would, where a program
/// has it open without letting it be deleted (a sharing violation: the C runtime's `open`, and so FFmpeg, opens files so) or
/// it can't be deleted. A read-only file can be opened so, and it's refused for its writing
#[cfg(windows)]
fn can_be_replaced(path: &Path) -> std::io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows::Win32::Storage::FileSystem::*;
    OpenOptions::new().access_mode(DELETE.0).share_mode((FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE).0).open(path).map(drop)
}

pub fn path_to_url(path: &str) -> String {
    fn inner(mut path: &str) -> Result<String> {
        if path.is_empty() {
            return Ok(String::new());
        }
        if path.contains("://") {
            return Ok(path.to_owned());
        } // Already an url
        if path.starts_with("//?/") {
            path = &path[4..];
        } // Windows extended path
        let mut ret = url::Url::from_file_path(&path)
            .map_err(|_| FilesystemError::NotAFile(path.into()))?
            .to_string();
        if (path.ends_with('/') || path.ends_with('\\')) && !ret.ends_with('/') {
            ret.push('/');
        }
        Ok(ret)
    }
    result!(inner(path), path)
}

pub fn url_to_path(url: &str) -> String {
    result!(url_to_path_for_platform(url, cfg!(target_os = "android")), url)
}
fn url_to_path_for_platform(url: &str, android: bool) -> Result<String> {
    if url.is_empty() {
        return Ok(String::new());
    }
    // Only SAF URLs lack a local path. Direct folder imports need the full path.
    if android && url.starts_with("content://") {
        return Ok(get_filename(url));
    }
    Ok(url_to_pathbuf(url)?.to_string_lossy().to_string())
}
#[cfg(any(target_os = "android", test))]
fn android_content_display_path(url: &str) -> Option<String> {
    if !url.starts_with("content://") {
        return None;
    }
    // Folder grants contain only a tree ID; child URLs also contain a document ID.
    let (_, id) = url.rsplit_once("/document/").or_else(|| url.rsplit_once("/tree/"))?;
    // The children endpoint is outside the encoded document ID.
    let id = id.split('/').next()?;
    let decoded = urlencoding::decode(id).ok()?;
    let (_, path) = decoded.split_once(':')?;
    Some(path.trim_start_matches('/').to_owned())
}

pub fn display_url(url: &str) -> String {
    dbg_call!(url);

    #[cfg(target_os = "android")]
    if let Some(path) = android_content_display_path(url) {
        return path;
    }

    let path = url_to_path(url);
    if cfg!(any(target_os = "macos", target_os = "ios")) {
        if path.contains("/File Provider Storage/") {
            return path
                .split("/File Provider Storage/")
                .last()
                .unwrap()
                .to_owned();
        } else if path.contains("/com~apple~CloudDocs/") {
            return format!(
                "iCloud/{}",
                path.split("/com~apple~CloudDocs/")
                    .last()
                    .unwrap()
                    .to_owned()
            );
        } else if path.contains("/Data/Application/") {
            return path
                .split("/Data/Application/")
                .last()
                .unwrap()
                .split('/')
                .skip(1)
                .join("/")
                .to_owned();
        } else if path.contains("/com.apple.filesystems.userfsd/") {
            return path
                .split("/com.apple.filesystems.userfsd/")
                .last()
                .unwrap()
                .split('/')
                .skip(1)
                .join("/")
                .to_owned();
        }
    }
    path
}
pub fn display_folder_filename(folder: &str, filename: &str) -> String {
    fn inner(folder: &str, filename: &str) -> String {
        if folder.is_empty() && !filename.is_empty() {
            return filename.to_string();
        }

        let url = get_file_url(folder, filename, false);
        if cfg!(target_os = "android") {
            if url.contains("/document/") && url.contains("%3A") {
                return display_url(&url);
            } else if folder.contains("/tree/") {
                let parts = folder.split("/tree/").last().unwrap();
                if let Ok(decoded) = urlencoding::decode(parts) {
                    if let Some(pos) = decoded.find(':') {
                        return format!("{}/{filename}", &decoded[pos + 1..].trim_end_matches('/'));
                    }
                }
            }
            return filename.to_owned();
        }
        display_url(&url)
    }
    let ret = inner(folder, filename);
    dbg_call!(folder filename -> ret);
    ret
}

pub fn normalize_url(url: &str, is_folder: bool) -> String {
    if !url.contains("://") {
        return String::new();
    }
    if let Ok(url) = url::Url::parse(url) {
        let mut url = url.to_string();
        if is_folder && !url.ends_with('/') {
            url.push('/');
        }
        url
    } else {
        String::new()
    }
}

pub fn folder_access_granted(folder_url: &str) {
    if folder_url.is_empty() || !folder_url.contains("://") {
        return;
    }

    let folder_url = normalize_url(folder_url, true);
    dbg_call!(folder_url);
    start_accessing_url(&folder_url, true); // This will not have an equivalent `stop_accessing_url` because we don't know when the access ends
    let already_allowed = ALLOWED_FOLDERS.read().contains(&folder_url);

    if !already_allowed {
        let mut lock = ALLOWED_FOLDERS.write();
        lock.insert(folder_url);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        filename_from_url_string, get_filename, is_bare_content_tree_url,
        reencode_url_for_android, path_to_url, url_to_path_for_platform,
    };

    #[test]
    fn ios_photo_import_url_matches_only_the_picker_cache_component() {
        assert!(super::is_ios_photo_import_url(
            "file:///private/var/mobile/Containers/Data/Application/ABC/Library/Caches/NiYien/GyroflowNiYien/ios-photo-imports/session/item/IMG_0001.mov"
        ));
        assert!(!super::is_ios_photo_import_url(
            "file:///private/var/mobile/Containers/Data/Application/ABC/Library/Caches/NiYien/GyroflowNiYien/ios-photo-imports-backup/IMG_0001.mov"
        ));
        assert!(!super::is_ios_photo_import_url(
            "file:///private/var/mobile/Containers/Shared/AppGroup/DCIM/IMG_0001.mov"
        ));
    }

    // The picker hands out a fully encoded URI; QML's `url` type decodes the
    // non-ASCII half on the way through while leaving %3A / %2F alone. SAF
    // compares permission grants by exact string, so the decoded form must be
    // restored before it reaches ContentResolver.
    #[test]
    fn android_url_reencodes_non_ascii_in_content_urls() {
        let decoded = "content://com.android.externalstorage.documents/document/primary%3A工作%2FTest_Video%2FDSC_1402.MP4";
        let encoded = "content://com.android.externalstorage.documents/document/primary%3A%E5%B7%A5%E4%BD%9C%2FTest_Video%2FDSC_1402.MP4";
        assert_eq!(reencode_url_for_android(decoded), encoded);
    }

    #[test]
    fn android_url_reencode_is_idempotent() {
        let encoded = "content://com.android.externalstorage.documents/document/primary%3A%E5%B7%A5%E4%BD%9C%2Fclip.mp4";
        assert_eq!(reencode_url_for_android(encoded), encoded);
        // Already-encoded input must not allocate a new string.
        assert!(matches!(
            reencode_url_for_android(encoded),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn android_url_reencodes_spaces_everywhere() {
        // Pre-existing behaviour, kept for every scheme.
        assert_eq!(
            reencode_url_for_android("content://authority/document/my clip.mp4"),
            "content://authority/document/my%20clip.mp4"
        );
        assert_eq!(
            reencode_url_for_android("file:///storage/my clip.mp4"),
            "file:///storage/my%20clip.mp4"
        );
    }

    #[test]
    fn android_url_leaves_non_content_non_ascii_alone() {
        // Local file paths never went through a SAF grant, so re-encoding them
        // would be a behaviour change with no upside.
        let f = "file:///storage/emulated/0/工作/clip.mp4";
        assert_eq!(reencode_url_for_android(f), f);
    }

    #[test]
    fn bare_tree_url_detected() {
        assert!(is_bare_content_tree_url(
            "content://com.android.externalstorage.documents/tree/primary%3ADCIM%2FCamera"
        ));
    }

    #[test]
    fn document_uri_from_tree_is_not_bare() {
        assert!(!is_bare_content_tree_url(
            "content://com.android.externalstorage.documents/tree/primary%3ADCIM/document/primary%3ADCIM%2Fclip.mp4"
        ));
    }

    #[test]
    fn plain_content_uri_is_not_tree() {
        assert!(!is_bare_content_tree_url(
            "content://com.android.providers.media.documents/document/video%3A1234"
        ));
    }

    #[test]
    fn file_url_is_not_tree() {
        assert!(!is_bare_content_tree_url("file:///storage/emulated/0/tree/x"));
    }

    #[test]
    fn helper_mac_url() {
        assert_eq!(
            filename_from_url_string("file:///Users/foo/Videos/clip.mp4"),
            "clip.mp4"
        );
    }

    #[test]
    fn helper_win_url() {
        assert_eq!(
            filename_from_url_string("file:///C:/Users/Foo/Videos/clip.mp4"),
            "clip.mp4"
        );
    }

    #[test]
    fn helper_unicode_percent() {
        assert_eq!(
            filename_from_url_string("file:///Users/foo/Videos/%E6%B5%8B%E8%AF%95.mp4"),
            "测试.mp4"
        );
    }

    #[test]
    fn helper_space_percent() {
        assert_eq!(
            filename_from_url_string("file:///Users/foo/My%20Videos/clip%20final.mp4"),
            "clip final.mp4"
        );
    }

    #[test]
    fn helper_folder_url_returns_empty() {
        assert_eq!(filename_from_url_string("file:///Users/foo/Videos/"), "");
    }

    #[test]
    fn helper_image_sequence_token() {
        assert_eq!(
            filename_from_url_string("file:///Users/foo/Videos/clip_%2504d.png"),
            "clip_%04d.png"
        );
    }

    #[test]
    fn helper_strips_query_and_fragment() {
        assert_eq!(
            filename_from_url_string("file:///Users/foo/Videos/clip.mp4?token=abc"),
            "clip.mp4"
        );
        assert_eq!(
            filename_from_url_string("file:///Users/foo/Videos/clip.mp4#frag"),
            "clip.mp4"
        );
    }

    #[test]
    fn get_filename_bare_fast_path() {
        assert_eq!(get_filename("clip.mp4"), "clip.mp4");
    }

    #[test]
    fn android_content_display_path_handles_folder_grants_and_children() {
        assert_eq!(super::android_content_display_path("content://provider/tree/primary%3ADownload/document/primary%3ADownload%2FA/children/"), Some("Download/A".into()));
        assert_eq!(super::android_content_display_path("content://provider/tree/primary%3ADownload/document/primary%3ADownload%2Fchildren/children"), Some("Download/children".into()));
        assert_eq!(super::android_content_display_path("content://com.android.externalstorage.documents/tree/primary%3ADownload%2FGyroflow-UI-Test-20260910"), Some("Download/Gyroflow-UI-Test-20260910".into()));
        assert_eq!(super::android_content_display_path("content://provider/tree/primary%3ADownload/document/primary%3ADownload%2F%E4%B8%AD%E6%96%87%20space"), Some("Download/中文 space".into()));
        assert_eq!(super::android_content_display_path("content://provider/tree/primary%3aDownload%2fA"), Some("Download/A".into()));
        assert_eq!(super::android_content_display_path("content://provider/document/opaque-id"), None);
        assert_eq!(super::android_content_display_path("file:///storage/emulated/0/tree/primary%3ADownload"), None);
    }

    #[test]
    fn android_local_file_urls_keep_full_path() {
        let folder = std::env::temp_dir().join("gyroflow path roundtrip").join("中文素材");
        for path in [folder.clone(), folder.join("clip 01.mp4")] {
            let expected = path.to_string_lossy().to_string();
            let url = path_to_url(&expected);
            assert_eq!(url_to_path_for_platform(&url, true).unwrap(), expected);
            assert_eq!(url_to_path_for_platform(&expected, true).unwrap(), expected);
        }
        assert_eq!(url_to_path_for_platform("", true).unwrap(), "");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn get_filename_cross_os_uses_fallback_on_windows() {
        // mac-style URL: Url::to_file_path() fails on Windows -> degraded path runs
        assert_eq!(
            get_filename("file:///Users/foo/Videos/clip.mp4"),
            "clip.mp4"
        );
        assert_eq!(
            get_filename("file:///Users/foo/Videos/%E6%B5%8B%E8%AF%95.mp4"),
            "测试.mp4"
        );
    }
}

pub fn restore_allowed_folders(list: &[String]) {
    for x in list {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            let (url, is_stale) = apple::resolve_bookmark(&x, None);
            if !url.is_empty() && url.contains("://") && !is_stale {
                folder_access_granted(&url);
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            folder_access_granted(x);
        }
    }
}

pub fn get_allowed_folder_urls() -> Vec<String> {
    let mut urls: Vec<String> = ALLOWED_FOLDERS.read().iter().cloned().collect();
    urls.sort();
    urls
}

pub fn get_allowed_folders() -> Vec<String> {
    let mut ret = Vec::new();
    let folders = ALLOWED_FOLDERS.read().clone();
    for x in folders.into_iter() {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            let bookmark = apple::create_bookmark(&x, true, None);
            if !bookmark.is_empty() {
                ret.push(bookmark);
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            ret.push(x);
        }
    }
    ret
}

pub fn is_sandboxed() -> bool {
    if cfg!(any(target_os = "android", target_os = "ios")) {
        return true;
    }
    if cfg!(target_os = "macos") && std::env::var("APP_SANDBOX_CONTAINER_ID").is_ok() {
        return true;
    }
    false
}

// Diagnostic override: when GYROFLOW_FORCE_ACCESS_DENIED is set, check_access()
// returns "denied" for any URL. Lets the permission-denied UI (dialog, Settings
// deep-link, the wired surfaces) be tested without reproducing a real macOS TCC
// deny, which is fiddly (user-intent grant + relaunch + Full Disk Access state).
fn force_access_denied() -> bool {
    use std::sync::OnceLock;
    static FORCE: OnceLock<bool> = OnceLock::new();
    *FORCE.get_or_init(|| {
        let on = matches!(
            std::env::var("GYROFLOW_FORCE_ACCESS_DENIED").ok().as_deref(),
            Some("1") | Some("true") | Some("yes") | Some("on")
        );
        if on {
            log::warn!(target: "video.load", "force_access_denied resolved=true source=env");
        }
        on
    })
}

// Probe access to a file/folder URL, distinguishing a permission denial (EPERM /
// PermissionDenied) from a missing or merely-unsupported target. Returns one of
// "ok" | "denied" | "not_found" | "other". This is the signal the UI uses to tell
// a macOS TCC block apart from a corrupt/unsupported file (MDK reports both
// identically as videoWidth=0). On non-macOS it simply reflects real fs errors.
pub fn check_access(url: &str) -> &'static str {
    if force_access_denied() {
        return "denied";
    }
    if url.is_empty() {
        return "not_found";
    }
    // content:// (Android SAF) is governed by the picker grant, not POSIX errno;
    // this affordance is macOS-targeted, so treat it as accessible there.
    #[cfg(target_os = "android")]
    if url.starts_with("content://") {
        return "ok";
    }

    start_accessing_url(url, false);
    let path = url_to_path(url);
    // A denied directory fails is_dir() (metadata is itself EPERM) and falls through
    // to File::open, which also returns PermissionDenied — so both map to "denied".
    let probe = if std::path::Path::new(&path).is_dir() {
        std::fs::read_dir(&path).map(|_| ())
    } else {
        std::fs::File::open(&path).map(|_| ())
    };
    stop_accessing_url(url, false);
    match probe {
        Ok(_) => "ok",
        Err(e) => match e.kind() {
            std::io::ErrorKind::PermissionDenied => {
                log::warn!(target: "video.load", "check_access denied url={url} kind={}", protected_folder_kind(url));
                "denied"
            }
            std::io::ErrorKind::NotFound => "not_found",
            _ => "other",
        },
    }
}

// Classify a URL's location into a macOS TCC-protected-folder kind, for messaging
// and Settings deep-linking: "downloads" | "documents" | "desktop" | "removable" |
// "network" | "other". Path-only (no fs access), so it is safe to call after a deny.
pub fn protected_folder_kind(url: &str) -> &'static str {
    let path = url_to_path(url);
    if path.is_empty() {
        return "other";
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let under = |sub: &str| !home.is_empty() && path.starts_with(&format!("{home}/{sub}"));
    if under("Downloads") {
        "downloads"
    } else if under("Documents") {
        "documents"
    } else if under("Desktop") {
        "desktop"
    } else if path.starts_with("/Volumes/") {
        // External and network mounts both live under /Volumes; we can't reliably
        // tell them apart by path, so report the broader "removable" bucket which
        // the UI maps to the Full Disk Access pane.
        "removable"
    } else {
        "other"
    }
}
