// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::{
    fmt,
    io::{Cursor, Read},
    time::{Duration, Instant},
};

use serde_json::Value;

use super::{
    commands::{
        MSG_CMD_OTA_BEGIN, MSG_CMD_OTA_INFO, MSG_CMD_OTA_REBOOT, MSG_CMD_OTA_TRANS,
        MSG_CMD_OTA_VERIFY, MSG_CMD_OTA_VERSION, OtaAck, Response, VersionInfo, parse_response,
    },
    protocol::{Frame, encode},
};

const OTA_CHUNK_SIZE: usize = 128;
const A1_DEVICE_PRODUCT_ID: u8 = 0xA1;

pub type Result<T> = std::result::Result<T, OtaError>;

#[derive(Debug)]
pub enum OtaError {
    Archive(std::io::Error),
    InvalidJson(serde_json::Error),
    MissingHeader,
    MissingBinary,
    InvalidHeader(&'static str),
    Validation(String),
}

impl fmt::Display for OtaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Archive(err) => write!(f, "failed to read firmware archive: {err}"),
            Self::InvalidJson(err) => write!(f, "failed to parse firmware header json: {err}"),
            Self::MissingHeader => write!(f, "firmware package is missing bin_header json"),
            Self::MissingBinary => write!(f, "firmware package is missing AES bin payload"),
            Self::InvalidHeader(field) => write!(f, "firmware header field `{field}` is invalid"),
            Self::Validation(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for OtaError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FirmwarePackage {
    pub company_name: String,
    pub product_name: String,
    pub version: String,
    pub magic_num: u32,
    pub crc: u32,
    pub bin_data: Vec<u8>,
    pub changelog_en: String,
    pub changelog_zh: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtaState {
    Idle,
    Version,
    VersionWait,
    Begin,
    BeginWait,
    BinInfo,
    BinInfoWait,
    Trans,
    TransWait,
    Verify,
    VerifyWait,
    Reboot,
    WaitingReconnect,
    Success,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OtaAction {
    Send(Vec<u8>),
    WaitingReconnect,
    Complete(VersionInfo),
    Failed(String),
    Noop,
}

#[derive(Clone, Debug)]
pub struct OtaManager {
    state: OtaState,
    firmware: FirmwarePackage,
    chunk_index: u32,
    total_chunks: u32,
    state_entered_at: Instant,
    last_error: Option<String>,
}

#[derive(Clone, Debug)]
struct FirmwareHeader {
    company_name: String,
    product_name: String,
    version: String,
    magic_num: u32,
    crc: u32,
}

impl OtaManager {
    pub fn new(firmware: FirmwarePackage) -> Self {
        let now = Instant::now();
        let total_chunks = firmware.bin_data.len().div_ceil(OTA_CHUNK_SIZE) as u32;
        Self {
            state: OtaState::Idle,
            firmware,
            chunk_index: 0,
            total_chunks,
            state_entered_at: now,
            last_error: None,
        }
    }

    pub fn state(&self) -> OtaState {
        self.state
    }

    pub fn error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn validate_firmware(&self, device_product_id: u8) -> Result<()> {
        if self.firmware.bin_data.is_empty() {
            return Err(OtaError::Validation(
                "firmware binary payload is empty".to_owned(),
            ));
        }

        match device_product_id {
            A1_DEVICE_PRODUCT_ID => {
                if !normalize_product_name(&self.firmware.product_name).contains("A1") {
                    return Err(OtaError::Validation(format!(
                        "firmware product `{}` does not match device product_id 0x{device_product_id:02X}",
                        self.firmware.product_name
                    )));
                }
                if self.firmware.magic_num == 0 {
                    return Err(OtaError::Validation(
                        "firmware magic_num must be non-zero for A1 updates".to_owned(),
                    ));
                }
                Ok(())
            }
            _ => Err(OtaError::Validation(format!(
                "unsupported device product_id 0x{device_product_id:02X} for firmware validation"
            ))),
        }
    }

    pub fn start(&mut self) -> Vec<u8> {
        self.start_at(Instant::now())
    }

    pub fn start_at(&mut self, now: Instant) -> Vec<u8> {
        // Logging context: each public OTA entry point (start/on_frame/
        // on_timeout) installs its own short-lived RAII scope. Storing the
        // guard on self would break Clone/Debug derives and the RAII
        // invariant under struct moves.
        let _log_ctx = gyroflow_core::log_context::LogContext::enter(
            gyroflow_core::log_context::LogContextUpdate::default().op("ota"),
        );
        self.chunk_index = 0;
        self.last_error = None;
        self.send_step(OtaState::Version, now)
    }

    pub fn on_frame(&mut self, frame: &Frame) -> OtaAction {
        self.on_frame_at(frame, Instant::now())
    }

    pub fn on_frame_at(&mut self, frame: &Frame, now: Instant) -> OtaAction {
        match (self.state, parse_response(frame)) {
            (OtaState::VersionWait, Some(Response::Version(_)))
                if frame.cmd == MSG_CMD_OTA_VERSION =>
            {
                OtaAction::Send(self.send_step(OtaState::Begin, now))
            }
            (OtaState::BeginWait, Some(Response::OtaAck(ack)))
                if frame.cmd == MSG_CMD_OTA_BEGIN =>
            {
                self.handle_simple_ack(ack, OtaState::BinInfo, now)
            }
            (OtaState::BinInfoWait, Some(Response::OtaAck(ack)))
                if frame.cmd == MSG_CMD_OTA_INFO =>
            {
                self.handle_simple_ack(ack, OtaState::Trans, now)
            }
            (OtaState::TransWait, Some(Response::OtaAck(ack)))
                if frame.cmd == MSG_CMD_OTA_TRANS =>
            {
                self.handle_trans_ack(ack, now)
            }
            (OtaState::VerifyWait, Some(Response::OtaAck(ack)))
                if frame.cmd == MSG_CMD_OTA_VERIFY =>
            {
                if ack.result == 0 {
                    OtaAction::Send(self.send_step(OtaState::Reboot, now))
                } else {
                    self.fail_at(
                        format!("OTA verify failed with result code {}", ack.result),
                        now,
                    )
                }
            }
            (OtaState::WaitingReconnect, Some(Response::Version(_)))
                if frame.cmd == MSG_CMD_OTA_VERSION =>
            {
                OtaAction::WaitingReconnect
            }
            _ => OtaAction::Noop,
        }
    }

    pub fn on_timeout(&mut self) -> OtaAction {
        self.on_timeout_at(Instant::now())
    }

    pub fn on_timeout_at(&mut self, now: Instant) -> OtaAction {
        if let Some(timeout) = timeout_for_state(self.state) {
            if now.saturating_duration_since(self.state_entered_at) >= timeout {
                return self.fail_at(format!("OTA {} timed out", self.state.label()), now);
            }
            if self.state == OtaState::WaitingReconnect {
                return OtaAction::WaitingReconnect;
            }
        }
        OtaAction::Noop
    }

    pub fn on_device_reconnected(&mut self, version: &VersionInfo) -> OtaAction {
        self.on_device_reconnected_at(version, Instant::now())
    }

    pub fn on_device_reconnected_at(&mut self, version: &VersionInfo, now: Instant) -> OtaAction {
        if self.state != OtaState::WaitingReconnect {
            return OtaAction::Noop;
        }

        if normalize_version(&version.soft_version) == normalize_version(&self.firmware.version) {
            self.enter_state(OtaState::Success, now);
            return OtaAction::Complete(version.clone());
        }

        self.fail_at(
            format!(
                "device reconnected with unexpected version `{}` (expected `{}`)",
                version.soft_version, self.firmware.version
            ),
            now,
        )
    }

    pub fn progress(&self) -> f64 {
        if self.total_chunks == 0 {
            return 0.0;
        }
        self.chunk_index as f64 / self.total_chunks as f64
    }

    fn handle_simple_ack(&mut self, ack: OtaAck, next_state: OtaState, now: Instant) -> OtaAction {
        if ack.result != 0 {
            return self.fail_at(
                format!(
                    "OTA {} failed with result code {}",
                    self.state.label(),
                    ack.result
                ),
                now,
            );
        }

        OtaAction::Send(self.send_step(next_state, now))
    }

    fn handle_trans_ack(&mut self, ack: OtaAck, now: Instant) -> OtaAction {
        if ack.result != 0 {
            return self.fail_at(
                format!("OTA transfer failed with result code {}", ack.result),
                now,
            );
        }

        let Some(ack_index) = ack.ack_index else {
            return self.fail_at("OTA transfer ACK missing chunk index".to_owned(), now);
        };

        if ack_index + 1 == self.chunk_index {
            return OtaAction::Noop;
        }

        if ack_index != self.chunk_index {
            return self.fail_at(
                format!(
                    "unexpected OTA transfer ACK index {ack_index}, expected {}",
                    self.chunk_index
                ),
                now,
            );
        }

        self.chunk_index += 1;
        if self.chunk_index >= self.total_chunks {
            OtaAction::Send(self.send_step(OtaState::Verify, now))
        } else {
            OtaAction::Send(self.send_step(OtaState::Trans, now))
        }
    }

    fn send_step(&mut self, state: OtaState, now: Instant) -> Vec<u8> {
        self.enter_state(state, now);
        match state {
            OtaState::Version => {
                self.enter_state(OtaState::VersionWait, now);
                encode(MSG_CMD_OTA_VERSION, &[])
            }
            OtaState::Begin => {
                self.enter_state(OtaState::BeginWait, now);
                encode(MSG_CMD_OTA_BEGIN, &[])
            }
            OtaState::BinInfo => {
                self.enter_state(OtaState::BinInfoWait, now);
                let mut payload = Vec::with_capacity(8);
                payload.extend_from_slice(&(self.firmware.bin_data.len() as u32).to_le_bytes());
                payload.extend_from_slice(&self.firmware.magic_num.to_le_bytes());
                encode(MSG_CMD_OTA_INFO, &payload)
            }
            OtaState::Trans => {
                self.enter_state(OtaState::TransWait, now);
                let start = self.chunk_index as usize * OTA_CHUNK_SIZE;
                let end = (start + OTA_CHUNK_SIZE).min(self.firmware.bin_data.len());
                let chunk = &self.firmware.bin_data[start..end];

                let mut payload = Vec::with_capacity(4 + chunk.len());
                payload.extend_from_slice(&self.chunk_index.to_le_bytes());
                payload.extend_from_slice(chunk);
                encode(MSG_CMD_OTA_TRANS, &payload)
            }
            OtaState::Verify => {
                self.enter_state(OtaState::VerifyWait, now);
                encode(MSG_CMD_OTA_VERIFY, &self.firmware.crc.to_le_bytes())
            }
            OtaState::Reboot => {
                self.enter_state(OtaState::WaitingReconnect, now);
                encode(MSG_CMD_OTA_REBOOT, &[])
            }
            _ => Vec::new(),
        }
    }

    fn enter_state(&mut self, state: OtaState, now: Instant) {
        self.state = state;
        self.state_entered_at = now;
    }

    fn fail_at(&mut self, message: String, now: Instant) -> OtaAction {
        self.last_error = Some(message.clone());
        self.enter_state(OtaState::Failed, now);
        OtaAction::Failed(message)
    }
}

pub fn load_firmware(data: &[u8]) -> Result<FirmwarePackage> {
    let mut archive = tar::Archive::new(Cursor::new(data));
    let mut changelog_en = String::new();
    let mut changelog_zh = String::new();
    let mut header_json = None;
    let mut bin_data = None;

    let entries = archive.entries().map_err(OtaError::Archive)?;
    for entry in entries {
        let mut entry = entry.map_err(OtaError::Archive)?;
        let path = entry
            .path()
            .map_err(OtaError::Archive)?
            .to_string_lossy()
            .into_owned();

        let mut contents = Vec::new();
        entry
            .read_to_end(&mut contents)
            .map_err(OtaError::Archive)?;

        if path.contains("change_en.log") {
            changelog_en = String::from_utf8_lossy(&contents).to_string();
        } else if path.contains("change_zh.log") {
            changelog_zh = String::from_utf8_lossy(&contents).to_string();
        } else if path.contains("bin_header_") && path.ends_with(".json") {
            header_json = Some(contents);
        } else if path.contains("AES_") && path.ends_with(".bin") {
            bin_data = Some(contents);
        }
    }

    let header_json = header_json.ok_or(OtaError::MissingHeader)?;
    let bin_data = bin_data.ok_or(OtaError::MissingBinary)?;
    let header = parse_firmware_header(&header_json)?;

    if bin_data.is_empty() {
        return Err(OtaError::Validation(
            "firmware binary payload is empty".to_owned(),
        ));
    }

    Ok(FirmwarePackage {
        company_name: header.company_name,
        product_name: header.product_name,
        version: header.version,
        magic_num: header.magic_num,
        crc: header.crc,
        bin_data,
        changelog_en,
        changelog_zh,
    })
}

fn parse_firmware_header(bytes: &[u8]) -> Result<FirmwareHeader> {
    let root: Value = serde_json::from_slice(bytes).map_err(OtaError::InvalidJson)?;

    Ok(FirmwareHeader {
        company_name: required_string(
            &root,
            &[&["bin_descript", "company_name"], &["company_name"]],
            "company_name",
        )?,
        product_name: required_string(
            &root,
            &[&["bin_descript", "product_name"], &["product_name"]],
            "product_name",
        )?,
        version: required_string(
            &root,
            &[
                &["bin_descript", "version"],
                &["version"],
                &["moudle_descript", "version"],
            ],
            "version",
        )?,
        magic_num: required_u32(&root, &[&["magic_num"]], "magic_num")?,
        crc: required_u32(
            &root,
            &[&["moudle_descript", "Crc"], &["Crc"], &["crc"]],
            "Crc",
        )?,
    })
}

fn required_string(root: &Value, paths: &[&[&str]], field: &'static str) -> Result<String> {
    let value = first_value(root, paths).ok_or(OtaError::InvalidHeader(field))?;
    if let Some(string) = value.as_str() {
        return Ok(string.to_owned());
    }
    if let Some(number) = value.as_u64() {
        return Ok(number.to_string());
    }
    if let Some(number) = value.as_i64() {
        return Ok(number.to_string());
    }
    Err(OtaError::InvalidHeader(field))
}

fn required_u32(root: &Value, paths: &[&[&str]], field: &'static str) -> Result<u32> {
    let value = first_value(root, paths).ok_or(OtaError::InvalidHeader(field))?;
    if let Some(number) = value.as_u64() {
        return u32::try_from(number).map_err(|_| OtaError::InvalidHeader(field));
    }
    if let Some(number) = value.as_i64() {
        return u32::try_from(number).map_err(|_| OtaError::InvalidHeader(field));
    }
    if let Some(string) = value.as_str() {
        return string
            .parse::<u32>()
            .map_err(|_| OtaError::InvalidHeader(field));
    }
    Err(OtaError::InvalidHeader(field))
}

fn first_value<'a>(root: &'a Value, paths: &[&[&str]]) -> Option<&'a Value> {
    for path in paths {
        if let Some(value) = nested_value(root, path) {
            return Some(value);
        }
    }
    None
}

fn nested_value<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn normalize_product_name(name: &str) -> String {
    name.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_uppercase()
}

fn normalize_version(version: &str) -> String {
    version
        .trim()
        .trim_start_matches(|ch| ch == 'V' || ch == 'v')
        .to_ascii_lowercase()
}

fn timeout_for_state(state: OtaState) -> Option<Duration> {
    match state {
        OtaState::VersionWait => Some(Duration::from_secs(1)),
        OtaState::BeginWait => Some(Duration::from_secs(1)),
        OtaState::BinInfoWait => Some(Duration::from_secs(10)),
        OtaState::TransWait => Some(Duration::from_secs(3)),
        OtaState::VerifyWait => Some(Duration::from_secs(3)),
        OtaState::WaitingReconnect => Some(Duration::from_secs(30)),
        _ => None,
    }
}

impl OtaState {
    fn label(self) -> &'static str {
        match self {
            OtaState::Idle => "idle",
            OtaState::Version => "version",
            OtaState::VersionWait => "version wait",
            OtaState::Begin => "begin",
            OtaState::BeginWait => "begin wait",
            OtaState::BinInfo => "bin info",
            OtaState::BinInfoWait => "bin info wait",
            OtaState::Trans => "transfer",
            OtaState::TransWait => "transfer wait",
            OtaState::Verify => "verify",
            OtaState::VerifyWait => "verify wait",
            OtaState::Reboot => "reboot",
            OtaState::WaitingReconnect => "waiting reconnect",
            OtaState::Success => "success",
            OtaState::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::io::Cursor;

    fn sample_firmware_package() -> FirmwarePackage {
        FirmwarePackage {
            company_name: "NiYien".into(),
            product_name: "A1".into(),
            version: "V1.4.0".into(),
            magic_num: 0x1234ABCD,
            crc: 0x89ABCDEF,
            bin_data: (0u8..200).collect(),
            changelog_en: "English changelog".into(),
            changelog_zh: "中文更新日志".into(),
        }
    }

    fn sample_firmware_archive() -> Vec<u8> {
        let header_json = r#"{
            "bin_descript": {
                "company_name": "NiYien",
                "company_simple": "NY",
                "product_name": "A1",
                "product_simple": "A1",
                "type": 1,
                "bin_num": 1,
                "version": "V1.4.0"
            },
            "moudle_descript": {
                "moudle_name": "IMU",
                "moudle_name_second": "A1",
                "version": "V1.4.0",
                "Crc": 2309737967
            },
            "magic_num": 305441741
        }"#;

        let mut builder = tar::Builder::new(Vec::new());
        append_tar_entry(&mut builder, "change_en.log", b"English changelog");
        append_tar_entry(&mut builder, "change_zh.log", "中文更新日志".as_bytes());
        append_tar_entry(&mut builder, "bin_header_A1.json", header_json.as_bytes());
        append_tar_entry(&mut builder, "AES_A1.bin", &(0u8..200).collect::<Vec<_>>());
        builder.finish().unwrap();
        builder.into_inner().unwrap()
    }

    fn append_tar_entry(builder: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8]) {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, path, Cursor::new(data))
            .unwrap();
    }

    fn version_frame(cmd: u8, version: &str) -> Frame {
        let mut data = vec![A1_DEVICE_PRODUCT_ID];
        data.extend_from_slice(version.as_bytes());
        data.push(0);
        data.extend_from_slice(b"HW1");
        data.push(0);
        data.extend_from_slice(b"SN0000000001");
        Frame { cmd, data }
    }

    fn ack_frame(cmd: u8, result: u8) -> Frame {
        Frame {
            cmd,
            data: vec![result],
        }
    }

    fn trans_ack_frame(index: u32) -> Frame {
        let mut data = vec![0];
        data.extend_from_slice(&index.to_le_bytes());
        Frame {
            cmd: MSG_CMD_OTA_TRANS,
            data,
        }
    }

    fn version_info(version: &str) -> VersionInfo {
        VersionInfo {
            product_id: A1_DEVICE_PRODUCT_ID,
            soft_version: version.into(),
            hard_version: "HW1".into(),
            serial_number: *b"SN0000000001",
        }
    }

    #[test]
    fn loads_firmware_package() {
        let package = load_firmware(&sample_firmware_archive()).unwrap();

        assert_eq!(package.company_name, "NiYien");
        assert_eq!(package.product_name, "A1");
        assert_eq!(package.version, "V1.4.0");
        assert_eq!(package.magic_num, 0x1234ABCD);
        assert_eq!(package.crc, 0x89ABCDEF);
        assert_eq!(package.bin_data.len(), 200);
        assert_eq!(package.changelog_en, "English changelog");
        assert_eq!(package.changelog_zh, "中文更新日志");
    }

    #[test]
    fn rejects_corrupt_tar() {
        assert!(load_firmware(b"not a tar archive").is_err());
    }

    #[test]
    fn rejects_mismatched_firmware() {
        let mut package = sample_firmware_package();
        package.product_name = "B2".into();
        let manager = OtaManager::new(package);

        assert!(manager.validate_firmware(A1_DEVICE_PRODUCT_ID).is_err());
    }

    #[test]
    fn rejects_unknown_product_id() {
        let manager = OtaManager::new(sample_firmware_package());

        assert!(manager.validate_firmware(0x55).is_err());
    }

    #[test]
    fn drives_full_ota_flow() {
        let mut manager = OtaManager::new(sample_firmware_package());
        let start = Instant::now();

        assert_eq!(manager.start_at(start), encode(MSG_CMD_OTA_VERSION, &[]));
        assert_eq!(manager.state(), OtaState::VersionWait);

        assert_eq!(
            manager.on_frame_at(&version_frame(MSG_CMD_OTA_VERSION, "V1.3.0"), start),
            OtaAction::Send(encode(MSG_CMD_OTA_BEGIN, &[]))
        );
        assert_eq!(manager.state(), OtaState::BeginWait);

        let mut info_payload = Vec::new();
        info_payload.extend_from_slice(&(200u32).to_le_bytes());
        info_payload.extend_from_slice(&0x1234ABCDu32.to_le_bytes());
        assert_eq!(
            manager.on_frame_at(&ack_frame(MSG_CMD_OTA_BEGIN, 0), start),
            OtaAction::Send(encode(MSG_CMD_OTA_INFO, &info_payload))
        );
        assert_eq!(manager.state(), OtaState::BinInfoWait);

        let mut first_chunk_payload = Vec::new();
        first_chunk_payload.extend_from_slice(&0u32.to_le_bytes());
        first_chunk_payload.extend_from_slice(&(0u8..128).collect::<Vec<_>>());
        assert_eq!(
            manager.on_frame_at(&ack_frame(MSG_CMD_OTA_INFO, 0), start),
            OtaAction::Send(encode(MSG_CMD_OTA_TRANS, &first_chunk_payload))
        );
        assert_eq!(manager.state(), OtaState::TransWait);
        assert_eq!(manager.progress(), 0.0);

        let mut second_chunk_payload = Vec::new();
        second_chunk_payload.extend_from_slice(&1u32.to_le_bytes());
        second_chunk_payload.extend_from_slice(&(128u8..200).collect::<Vec<_>>());
        assert_eq!(
            manager.on_frame_at(&trans_ack_frame(0), start),
            OtaAction::Send(encode(MSG_CMD_OTA_TRANS, &second_chunk_payload))
        );
        assert_eq!(manager.progress(), 0.5);

        assert_eq!(
            manager.on_frame_at(&trans_ack_frame(1), start),
            OtaAction::Send(encode(MSG_CMD_OTA_VERIFY, &0x89ABCDEFu32.to_le_bytes()))
        );
        assert_eq!(manager.progress(), 1.0);
        assert_eq!(manager.state(), OtaState::VerifyWait);

        assert_eq!(
            manager.on_frame_at(&ack_frame(MSG_CMD_OTA_VERIFY, 0), start),
            OtaAction::Send(encode(MSG_CMD_OTA_REBOOT, &[]))
        );
        assert_eq!(manager.state(), OtaState::WaitingReconnect);

        assert_eq!(
            manager
                .on_device_reconnected_at(&version_info("V1.4.0"), start + Duration::from_secs(1),),
            OtaAction::Complete(version_info("V1.4.0"))
        );
        assert_eq!(manager.state(), OtaState::Success);
    }

    #[test]
    fn handles_error_ack() {
        let mut manager = OtaManager::new(sample_firmware_package());
        let start = Instant::now();

        manager.start_at(start);
        manager.on_frame_at(&version_frame(MSG_CMD_OTA_VERSION, "V1.3.0"), start);
        assert_eq!(
            manager.on_frame_at(&ack_frame(MSG_CMD_OTA_BEGIN, 3), start),
            OtaAction::Failed("OTA begin wait failed with result code 3".into())
        );
        assert_eq!(manager.state(), OtaState::Failed);
    }

    #[test]
    fn ignores_duplicate_ack_and_rejects_out_of_order_ack() {
        let mut manager = OtaManager::new(sample_firmware_package());
        let start = Instant::now();

        manager.start_at(start);
        manager.on_frame_at(&version_frame(MSG_CMD_OTA_VERSION, "V1.3.0"), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_BEGIN, 0), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_INFO, 0), start);
        manager.on_frame_at(&trans_ack_frame(0), start);

        assert_eq!(
            manager.on_frame_at(&trans_ack_frame(0), start),
            OtaAction::Noop
        );
        assert_eq!(manager.state(), OtaState::TransWait);

        assert_eq!(
            manager.on_frame_at(
                &Frame {
                    cmd: MSG_CMD_OTA_TRANS,
                    data: vec![0, 9, 0, 0, 0],
                },
                start,
            ),
            OtaAction::Failed("unexpected OTA transfer ACK index 9, expected 1".into())
        );
        assert_eq!(manager.state(), OtaState::Failed);
    }

    #[test]
    fn exposes_state_timeouts() {
        assert_eq!(
            timeout_for_state(OtaState::VersionWait),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            timeout_for_state(OtaState::BeginWait),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            timeout_for_state(OtaState::BinInfoWait),
            Some(Duration::from_secs(10))
        );
        assert_eq!(
            timeout_for_state(OtaState::TransWait),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            timeout_for_state(OtaState::VerifyWait),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            timeout_for_state(OtaState::WaitingReconnect),
            Some(Duration::from_secs(30))
        );
        assert_eq!(timeout_for_state(OtaState::Idle), None);
    }

    #[test]
    fn times_out_while_waiting_for_reconnect() {
        let mut manager = OtaManager::new(sample_firmware_package());
        let start = Instant::now();

        manager.start_at(start);
        manager.on_frame_at(&version_frame(MSG_CMD_OTA_VERSION, "V1.3.0"), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_BEGIN, 0), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_INFO, 0), start);
        manager.on_frame_at(&trans_ack_frame(0), start);
        manager.on_frame_at(&trans_ack_frame(1), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_VERIFY, 0), start);

        assert_eq!(
            manager.on_timeout_at(start + Duration::from_secs(1)),
            OtaAction::WaitingReconnect
        );
        assert_eq!(
            manager.on_timeout_at(start + Duration::from_secs(31)),
            OtaAction::Failed("OTA waiting reconnect timed out".into())
        );
        assert_eq!(manager.state(), OtaState::Failed);
    }

    #[test]
    fn fails_if_reconnected_version_does_not_match_firmware() {
        let mut manager = OtaManager::new(sample_firmware_package());
        let start = Instant::now();

        manager.start_at(start);
        manager.on_frame_at(&version_frame(MSG_CMD_OTA_VERSION, "V1.3.0"), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_BEGIN, 0), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_INFO, 0), start);
        manager.on_frame_at(&trans_ack_frame(0), start);
        manager.on_frame_at(&trans_ack_frame(1), start);
        manager.on_frame_at(&ack_frame(MSG_CMD_OTA_VERIFY, 0), start);

        assert_eq!(
            manager
                .on_device_reconnected_at(&version_info("V1.3.0"), start + Duration::from_secs(1)),
            OtaAction::Failed(
                "device reconnected with unexpected version `V1.3.0` (expected `V1.4.0`)".into()
            )
        );
        assert_eq!(manager.state(), OtaState::Failed);
    }
}
