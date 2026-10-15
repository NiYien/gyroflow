// SPDX-License-Identifier: GPL-3.0-or-later
use gyroflow_core::filesystem;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct OutputFile {
    pub url: String,
    pub gallery_id: String,
    pub copied_url: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Delivery {
    pub epoch: u64,
    pub state: String,
    pub error: String,
    pub files: Vec<OutputFile>,
}

pub fn staging_folder(job_id: u32) -> String {
    static SESSION: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let session = SESSION.get_or_init(|| format!("{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()));
    filesystem::path_to_url(&gyroflow_core::settings::data_dir()
        .join("mobile-exports").join(session).join(job_id.to_string()).to_string_lossy())
}

pub fn rendered_filename(filename: &str, separate: bool, ranges: usize, index: usize) -> String {
    let mut result = filename.to_owned();
    if separate && ranges > 1 {
        if let Some(dot) = result.rfind('.') { result.insert_str(dot, &format!("-{:03}", index + 1)); }
    }
    result
}

impl Delivery {
    pub fn persist_in(&self, folder: &str) {
        let path = std::path::PathBuf::from(filesystem::url_to_path(folder));
        if !path.starts_with(gyroflow_core::settings::data_dir().join("mobile-exports")) { return; }
        if let Ok(data) = serde_json::to_vec(self) {
            if let Err(error) = std::fs::write(path.join("delivery.json"), data) {
                log::warn!("Unable to record mobile export delivery: {error}");
            }
        }
    }

    fn persist(&self) {
        if let Some(file) = self.files.first() { self.persist_in(&filesystem::get_folder(&file.url)); }
    }

    pub fn save_gallery(&mut self, mut save: impl FnMut(&str) -> Result<String, String>) {
        self.error.clear();
        for index in 0..self.files.len() {
            if !self.files[index].gallery_id.is_empty() { continue; }
            match save(&filesystem::url_to_path(&self.files[index].url)) {
                Ok(id) => self.files[index].gallery_id = id,
                Err(error) => { self.state = "failed".into(); self.error = error; self.persist(); return; }
            }
            self.persist();
        }
        self.state = if self.files.is_empty() { "failed" } else { "saved" }.into();
        self.persist();
    }

    pub fn copy_to_folder(&mut self, folder: &str) {
        self.error.clear();
        for file in &mut self.files {
            if !file.copied_url.is_empty() && filesystem::get_folder(&file.copied_url).trim_end_matches('/') == folder.trim_end_matches('/') { continue; }
            let result = (|| -> Result<String, String> {
                let original = filesystem::get_filename(&file.url);
                let mut name = original.clone();
                for suffix in 1.. {
                    if !filesystem::exists_in_folder(folder, &name) { break; }
                    name = filesystem::filename_with_suffix(&original, &format!("_{suffix}"));
                }
                let url = filesystem::get_file_url(folder, &name, true);
                if url.is_empty() { return Err("save_failed".into()); }
                let copied = (|| {
                    let mut source = filesystem::open_file(&file.url, false, false)?;
                    let mut target = filesystem::open_file(&url, true, true)?;
                    std::io::copy(source.get_file(), target.get_file()).map_err(filesystem::FilesystemError::from)?;
                    target.get_file().sync_all().map_err(filesystem::FilesystemError::from)
                })();
                if let Err(error) = copied {
                    let _ = filesystem::remove_file(&url);
                    return Err(error.to_string());
                }
                Ok(url)
            })();
            match result {
                Ok(url) => file.copied_url = url,
                Err(error) => { self.state = "failed".into(); self.error = error; return; }
            }
        }
        self.state = "folder".into();
        self.persist();
    }

    pub fn output_url(&self) -> Option<&str> {
        self.files.first().map(|file| if self.state == "folder" { file.copied_url.as_str() } else { file.url.as_str() })
    }
}

/// Remove only completed video copies from previous sessions; projects and failed exports stay.
pub fn cleanup_completed() {
    cleanup_completed_in(&gyroflow_core::settings::data_dir().join("mobile-exports"));
}

fn cleanup_completed_in(root: &std::path::Path) {
    let directories = |path: &std::path::Path| -> Vec<std::path::PathBuf> {
        std::fs::read_dir(path).into_iter().flatten().filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path()).collect()
    };
    for session in directories(root) {
        for folder in directories(&session) {
            let receipt = folder.join("delivery.json");
            let Some(delivery) = std::fs::read(&receipt).ok().and_then(|data| serde_json::from_slice::<Delivery>(&data).ok()) else { continue; };
            if !matches!(delivery.state.as_str(), "saved" | "folder") { continue; }
            let Ok(parent) = folder.canonicalize() else { continue; };
            for file in delivery.files {
                let path = std::path::PathBuf::from(filesystem::url_to_path(&file.url));
                if path.canonicalize().ok().and_then(|path| path.parent().map(|p| p == parent)).unwrap_or(false)
                    && path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("mp4") || extension.eq_ignore_ascii_case("mov") || extension.eq_ignore_ascii_case("m4v")) {
                    let _ = std::fs::remove_file(path);
                }
            }
            let _ = std::fs::remove_file(receipt);
            let _ = std::fs::remove_dir(folder);
        }
        let _ = std::fs::remove_dir(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gallery_retry_does_not_publish_successful_files_twice() {
        let mut delivery = Delivery { files: vec![
            OutputFile { url: "file:///one.mp4".into(), ..Default::default() },
            OutputFile { url: "file:///two.mp4".into(), ..Default::default() },
        ], ..Default::default() };
        let mut calls = 0;
        delivery.save_gallery(|_| { calls += 1; if calls == 1 { Ok("asset-one".into()) } else { Err("denied".into()) } });
        assert_eq!(delivery.state, "failed");
        assert_eq!(delivery.files[0].gallery_id, "asset-one");
        calls = 0;
        delivery.save_gallery(|_| { calls += 1; Ok("asset-two".into()) });
        assert_eq!(calls, 1);
        assert_eq!(delivery.state, "saved");
        delivery.save_gallery(|_| panic!("already saved"));
    }

    #[test]
    fn split_export_names_match_rendered_outputs() {
        assert_eq!(rendered_filename("clip.mp4", true, 2, 1), "clip-002.mp4");
        assert_eq!(rendered_filename("clip.mp4", true, 1, 0), "clip.mp4");
        assert_eq!(rendered_filename("clip.mp4", false, 2, 1), "clip.mp4");
    }

    #[test]
    fn cleanup_preserves_projects_failed_exports_and_files_outside_the_session() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside.mp4");
        std::fs::write(&outside, b"outside").unwrap();
        for state in ["saved", "failed"] {
            let folder = root.path().join("session").join(state);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("clip.mp4"), b"video").unwrap();
            std::fs::write(folder.join("clip.gyroflow"), b"project").unwrap();
            let delivery = Delivery { state: state.into(), files: vec![OutputFile {
                url: filesystem::path_to_url(&folder.join("clip.mp4").to_string_lossy()), ..Default::default()
            }, OutputFile { url: filesystem::path_to_url(&outside.to_string_lossy()), ..Default::default() }], ..Default::default() };
            std::fs::write(folder.join("delivery.json"), serde_json::to_vec(&delivery).unwrap()).unwrap();
        }
        cleanup_completed_in(root.path());
        assert!(outside.exists());
        assert!(!root.path().join("session/saved/clip.mp4").exists());
        assert!(root.path().join("session/saved/clip.gyroflow").exists());
        assert!(root.path().join("session/failed/clip.mp4").exists());
    }

    #[test]
    fn folder_fallback_preserves_existing_files_and_source() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let path = source.path().join("clip.mp4");
        std::fs::write(&path, b"new video").unwrap();
        std::fs::write(target.path().join("clip.mp4"), b"existing").unwrap();
        let mut delivery = Delivery { files: vec![OutputFile {
            url: filesystem::path_to_url(&path.to_string_lossy()), ..Default::default()
        }], ..Default::default() };
        delivery.copy_to_folder(&filesystem::path_to_url(&target.path().to_string_lossy()));
        assert_eq!(delivery.state, "folder", "{}", delivery.error);
        assert_eq!(std::fs::read(target.path().join("clip.mp4")).unwrap(), b"existing");
        assert_eq!(std::fs::read(target.path().join("clip_1.mp4")).unwrap(), b"new video");
        assert!(path.exists());
    }
}
