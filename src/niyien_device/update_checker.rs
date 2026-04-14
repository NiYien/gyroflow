// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::{
    fmt, fs,
    io::Read,
    path::{Path, PathBuf},
};

use serde_json::Value;

pub type Result<T> = std::result::Result<T, UpdateError>;

#[derive(Debug)]
pub enum UpdateError {
    Http(String),
    Io(std::io::Error),
    InvalidJson(serde_json::Error),
    MissingField(&'static str),
    InvalidVersion(String),
    Md5Mismatch { expected: String, actual: String },
}

impl fmt::Display for UpdateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(message) => f.write_str(message),
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::InvalidJson(err) => write!(f, "invalid update json: {err}"),
            Self::MissingField(field) => write!(f, "missing update field `{field}`"),
            Self::InvalidVersion(version) => write!(f, "invalid semver version `{version}`"),
            Self::Md5Mismatch { expected, actual } => {
                write!(
                    f,
                    "firmware md5 mismatch, expected {expected}, got {actual}"
                )
            }
        }
    }
}

impl std::error::Error for UpdateError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwareUpdateInfo {
    pub version: String,
    pub filename: String,
    pub md5: String,
    pub changelog_en: String,
    pub changelog_zh: String,
}

pub fn check_update(current_version: &str) -> Result<Option<FirmwareUpdateInfo>> {
    let body = ureq::get(
        gyroflow_core::distribution::config()
            .endpoints
            .firmware_manifest
            .as_str(),
    )
    .call()
    .map_err(|err| UpdateError::Http(err.to_string()))?
    .into_body()
    .read_to_string()
    .map_err(|err| UpdateError::Http(err.to_string()))?;

    check_update_from_body(current_version, &body)
}

pub fn download_firmware(info: &FirmwareUpdateInfo) -> Result<Vec<u8>> {
    let cache_dir = firmware_cache_dir()?;
    download_firmware_with_cache(info, &cache_dir, || {
        let url = firmware_download_url(&info.filename)?;
        let response = ureq::get(url.as_str())
            .call()
            .map_err(|err| UpdateError::Http(err.to_string()))?;
        let mut reader = response.into_body().into_reader();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map_err(UpdateError::Io)?;
        Ok(bytes)
    })
}

fn check_update_from_body(current_version: &str, body: &str) -> Result<Option<FirmwareUpdateInfo>> {
    let root: Value = serde_json::from_str(body).map_err(UpdateError::InvalidJson)?;
    let entry = root
        .get("firmware/A1")
        .ok_or(UpdateError::MissingField("firmware/A1"))?;

    let version = entry
        .get("version")
        .and_then(Value::as_str)
        .ok_or(UpdateError::MissingField("firmware/A1.version"))?;
    let latest = parse_semver(version)?;
    let current = parse_semver(current_version)?;

    if latest <= current {
        return Ok(None);
    }

    Ok(Some(FirmwareUpdateInfo {
        version: version.to_owned(),
        filename: entry
            .get("file")
            .and_then(Value::as_str)
            .ok_or(UpdateError::MissingField("firmware/A1.file"))?
            .to_owned(),
        md5: normalize_md5(
            entry
                .get("md5")
                .and_then(Value::as_str)
                .ok_or(UpdateError::MissingField("firmware/A1.md5"))?,
        ),
        changelog_en: entry
            .get("info_en")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        changelog_zh: entry
            .get("info_zh")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
    }))
}

fn download_firmware_with_cache<F>(
    info: &FirmwareUpdateInfo,
    cache_dir: &Path,
    fetcher: F,
) -> Result<Vec<u8>>
where
    F: FnOnce() -> Result<Vec<u8>>,
{
    fs::create_dir_all(cache_dir).map_err(UpdateError::Io)?;

    let cache_path = cache_dir.join(&info.filename);
    if let Ok(cached_bytes) = fs::read(&cache_path) {
        let cached_md5 = md5_hex(&cached_bytes);
        if cached_md5 == normalize_md5(&info.md5) {
            return Ok(cached_bytes);
        }
    }

    let bytes = fetcher()?;
    let actual_md5 = md5_hex(&bytes);
    let expected_md5 = normalize_md5(&info.md5);
    if actual_md5 != expected_md5 {
        return Err(UpdateError::Md5Mismatch {
            expected: expected_md5,
            actual: actual_md5,
        });
    }

    fs::write(&cache_path, &bytes).map_err(UpdateError::Io)?;
    Ok(bytes)
}

fn firmware_cache_dir() -> Result<PathBuf> {
    let path = crate::core::settings::data_dir()
        .join("niyien")
        .join("firmware");
    fs::create_dir_all(&path).map_err(UpdateError::Io)?;
    Ok(path)
}

fn firmware_download_url(filename: &str) -> Result<url::Url> {
    let base = url::Url::parse(
        gyroflow_core::distribution::config()
            .endpoints
            .firmware_base
            .as_str(),
    )
    .map_err(|err| UpdateError::Http(format!("invalid firmware base url: {err}")))?;
    base.join(filename)
        .map_err(|err| UpdateError::Http(format!("invalid firmware filename `{filename}`: {err}")))
}

fn parse_semver(version: &str) -> Result<semver::Version> {
    semver::Version::parse(
        version
            .trim()
            .trim_start_matches(|ch| ch == 'V' || ch == 'v'),
    )
    .map_err(|_| UpdateError::InvalidVersion(version.to_owned()))
}

fn normalize_md5(md5: &str) -> String {
    md5.trim().to_ascii_lowercase()
}

fn md5_hex(bytes: &[u8]) -> String {
    let digest = md5_digest(bytes);
    let mut output = String::with_capacity(32);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn md5_digest(bytes: &[u8]) -> [u8; 16] {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613,
        0xfd469501, 0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193,
        0xa679438e, 0x49b40821, 0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d,
        0x02441453, 0xd8a1e681, 0xe7d3fbc8, 0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a, 0xfffa3942, 0x8771f681, 0x6d9d6122,
        0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70, 0x289b7ec6, 0xeaa127fa,
        0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665, 0xf4292244,
        0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb,
        0xeb86d391,
    ];

    let bit_len = (bytes.len() as u64) * 8;
    let mut message = bytes.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0 = 0x67452301u32;
    let mut b0 = 0xefcdab89u32;
    let mut c0 = 0x98badcfeu32;
    let mut d0 = 0x10325476u32;

    for chunk in message.chunks_exact(64) {
        let mut words = [0u32; 16];
        for (i, word) in words.iter_mut().enumerate() {
            let offset = i * 4;
            *word = u32::from_le_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }

        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = if i < 16 {
                ((b & c) | ((!b) & d), i)
            } else if i < 32 {
                ((d & b) | ((!d) & c), (5 * i + 1) % 16)
            } else if i < 48 {
                (b ^ c ^ d, (3 * i + 5) % 16)
            } else {
                (c ^ (b | !d), (7 * i) % 16)
            };

            let temp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(K[i])
                    .wrapping_add(words[g])
                    .rotate_left(S[i]),
            );
            a = temp;
        }

        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut digest = [0u8; 16];
    digest[0..4].copy_from_slice(&a0.to_le_bytes());
    digest[4..8].copy_from_slice(&b0.to_le_bytes());
    digest[8..12].copy_from_slice(&c0.to_le_bytes());
    digest[12..16].copy_from_slice(&d0.to_le_bytes());
    digest
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_newer_update() {
        let body = r#"{
            "firmware/A1": {
                "file": "A1_V1.4.0.firmware",
                "md5": "098f6bcd4621d373cade4e832627b4f6",
                "version": "V1.4.0",
                "info_en": "English notes",
                "info_zh": "中文说明"
            }
        }"#;

        assert_eq!(
            check_update_from_body("V1.3.9", body).unwrap(),
            Some(FirmwareUpdateInfo {
                version: "V1.4.0".into(),
                filename: "A1_V1.4.0.firmware".into(),
                md5: "098f6bcd4621d373cade4e832627b4f6".into(),
                changelog_en: "English notes".into(),
                changelog_zh: "中文说明".into(),
            })
        );
    }

    #[test]
    fn returns_none_when_current_version_is_latest() {
        let body = r#"{
            "firmware/A1": {
                "file": "A1_V1.4.0.firmware",
                "md5": "098f6bcd4621d373cade4e832627b4f6",
                "version": "V1.4.0",
                "info_en": "",
                "info_zh": ""
            }
        }"#;

        assert_eq!(check_update_from_body("1.4.0", body).unwrap(), None);
    }

    #[test]
    fn rejects_invalid_versions() {
        let body = r#"{
            "firmware/A1": {
                "file": "A1_bad.firmware",
                "md5": "098f6bcd4621d373cade4e832627b4f6",
                "version": "latest",
                "info_en": "",
                "info_zh": ""
            }
        }"#;

        assert!(check_update_from_body("V1.3.0", body).is_err());
    }

    #[test]
    fn reuses_matching_cached_firmware() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"test".to_vec();
        let cache_path = temp.path().join("A1_V1.4.0.firmware");
        fs::write(&cache_path, &bytes).unwrap();
        let info = FirmwareUpdateInfo {
            version: "V1.4.0".into(),
            filename: "A1_V1.4.0.firmware".into(),
            md5: md5_hex(&bytes),
            changelog_en: String::new(),
            changelog_zh: String::new(),
        };

        let downloaded = download_firmware_with_cache(&info, temp.path(), || {
            panic!("fetcher should not be called when cache matches");
        })
        .unwrap();

        assert_eq!(downloaded, bytes);
    }

    #[test]
    fn downloads_and_caches_firmware() {
        let temp = tempfile::tempdir().unwrap();
        let bytes = b"firmware".to_vec();
        let info = FirmwareUpdateInfo {
            version: "V1.4.0".into(),
            filename: "A1_V1.4.0.firmware".into(),
            md5: md5_hex(&bytes),
            changelog_en: String::new(),
            changelog_zh: String::new(),
        };

        let downloaded =
            download_firmware_with_cache(&info, temp.path(), || Ok(bytes.clone())).unwrap();

        assert_eq!(downloaded, bytes);
        assert_eq!(
            fs::read(temp.path().join("A1_V1.4.0.firmware")).unwrap(),
            downloaded
        );
    }

    #[test]
    fn rejects_md5_mismatch() {
        let temp = tempfile::tempdir().unwrap();
        let info = FirmwareUpdateInfo {
            version: "V1.4.0".into(),
            filename: "A1_V1.4.0.firmware".into(),
            md5: "00000000000000000000000000000000".into(),
            changelog_en: String::new(),
            changelog_zh: String::new(),
        };

        let err = download_firmware_with_cache(&info, temp.path(), || Ok(b"firmware".to_vec()))
            .unwrap_err();

        assert!(matches!(err, UpdateError::Md5Mismatch { .. }));
    }

    #[test]
    fn computes_md5_hex() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"test"), "098f6bcd4621d373cade4e832627b4f6");
    }

    #[test]
    fn encodes_firmware_filename_in_url() {
        let url = firmware_download_url("NiYien_Senseflow A1_V1.2.8.firmware").unwrap();
        assert_eq!(
            url.as_str(),
            "https://www.niyien.com/Update/firmware/A1/NiYien_Senseflow%20A1_V1.2.8.firmware"
        );
    }
}
