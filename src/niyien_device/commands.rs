// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use super::protocol::{Frame, encode};

pub const MSG_CMD_VERSION: u8 = 0x02;
pub const MSG_CMD_TIME_SET: u8 = 0x03;
pub const MSG_CMD_TIME_GET: u8 = 0x04;

pub const MSG_CMD_OTA_BEGIN: u8 = 0xFE;
pub const MSG_CMD_OTA_INFO: u8 = 0xFD;
pub const MSG_CMD_OTA_TRANS: u8 = 0xFC;
pub const MSG_CMD_OTA_VERIFY: u8 = 0xFB;
pub const MSG_CMD_OTA_REBOOT: u8 = 0xFA;
pub const MSG_CMD_OTA_VERSION: u8 = 0xF9;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VersionInfo {
    pub product_id: u8,
    pub soft_version: String,
    pub hard_version: String,
    pub serial_number: [u8; 12],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimeSetResult {
    pub success: bool,
    pub code: u8,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtaAck {
    pub cmd: u8,
    pub result: u8,
    pub ack_index: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Version(VersionInfo),
    TimeGet(DeviceTime),
    TimeSetResult(TimeSetResult),
    OtaAck(OtaAck),
}

pub fn ask_version() -> Vec<u8> {
    encode(MSG_CMD_VERSION, &[])
}

pub fn ask_time() -> Vec<u8> {
    encode(MSG_CMD_TIME_GET, &[])
}

pub fn set_time(
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
    tz_offset_minutes: i16,
) -> Vec<u8> {
    let payload = [
        (year % 100) as u8,
        month,
        day,
        hour,
        minute,
        second,
        ((tz_offset_minutes >> 8) & 0xFF) as u8,
        (tz_offset_minutes & 0xFF) as u8,
    ];
    encode(MSG_CMD_TIME_SET, &payload)
}

pub fn parse_response(frame: &Frame) -> Option<Response> {
    match frame.cmd {
        MSG_CMD_VERSION => parse_version_payload(&frame.data, true).map(Response::Version),
        MSG_CMD_OTA_VERSION => parse_version_payload(&frame.data, false).map(Response::Version),
        MSG_CMD_TIME_GET => parse_time_payload(&frame.data).map(Response::TimeGet),
        MSG_CMD_TIME_SET => parse_time_set_payload(&frame.data).map(Response::TimeSetResult),
        MSG_CMD_OTA_BEGIN | MSG_CMD_OTA_INFO | MSG_CMD_OTA_TRANS | MSG_CMD_OTA_VERIFY => {
            parse_ota_ack(frame.cmd, &frame.data).map(Response::OtaAck)
        }
        _ => None,
    }
}

fn parse_version_payload(data: &[u8], require_serial: bool) -> Option<VersionInfo> {
    if data.len() < 3 {
        return None;
    }

    let soft_end = data[1..].iter().position(|byte| *byte == 0)? + 1;
    let hard_start = soft_end + 1;
    let hard_end = data[hard_start..].iter().position(|byte| *byte == 0)? + hard_start;
    let serial_start = hard_end + 1;

    let mut serial_number = [0u8; 12];
    if data.len() >= serial_start + 12 {
        serial_number.copy_from_slice(&data[serial_start..serial_start + 12]);
    } else if require_serial {
        return None;
    }

    Some(VersionInfo {
        product_id: data[0],
        soft_version: String::from_utf8_lossy(&data[1..soft_end]).to_string(),
        hard_version: String::from_utf8_lossy(&data[hard_start..hard_end]).to_string(),
        serial_number,
    })
}

fn parse_time_payload(data: &[u8]) -> Option<DeviceTime> {
    if data.len() < 6 {
        return None;
    }

    Some(DeviceTime {
        year: 2000 + data[0] as u16,
        month: data[1],
        day: data[2],
        hour: data[3],
        minute: data[4],
        second: data[5],
    })
}

fn parse_time_set_payload(data: &[u8]) -> Option<TimeSetResult> {
    let code = *data.first()?;
    Some(TimeSetResult {
        success: code == 0,
        code,
    })
}

fn parse_ota_ack(cmd: u8, data: &[u8]) -> Option<OtaAck> {
    let result = *data.first()?;
    let ack_index = if cmd == MSG_CMD_OTA_TRANS {
        match data.len() {
            3 => Some(u16::from_be_bytes([data[1], data[2]]) as u32),
            len if len >= 5 => Some(u32::from_le_bytes([data[1], data[2], data[3], data[4]])),
            _ => return None,
        }
    } else {
        None
    };

    Some(OtaAck {
        cmd,
        result,
        ack_index,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_version_query() {
        assert_eq!(ask_version(), vec![0xAA, 0x55, 0x01, 0x02, 0x00, 0x02]);
    }

    #[test]
    fn encodes_time_query() {
        assert_eq!(ask_time(), vec![0xAA, 0x55, 0x01, 0x04, 0x00, 0x04]);
    }

    #[test]
    fn encodes_set_time_request() {
        assert_eq!(
            set_time(2026, 4, 7, 13, 14, 15, -480),
            vec![
                0xAA, 0x55, 0x01, 0x03, 0x08, 0x1A, 0x04, 0x07, 0x0D, 0x0E, 0x0F, 0xFE, 0x20, 0x78
            ]
        );
    }

    #[test]
    fn parses_version_info() {
        let frame = Frame {
            cmd: MSG_CMD_VERSION,
            data: vec![
                0xA1, b'V', b'1', b'.', b'2', 0x00, b'H', b'W', b'9', 0x00, b'S', b'N', b'0', b'0',
                b'0', b'0', b'0', b'0', b'0', b'0', b'0', b'1',
            ],
        };

        assert_eq!(
            parse_response(&frame),
            Some(Response::Version(VersionInfo {
                product_id: 0xA1,
                soft_version: "V1.2".into(),
                hard_version: "HW9".into(),
                serial_number: *b"SN0000000001",
            }))
        );
    }

    #[test]
    fn parses_ota_version_without_serial() {
        let frame = Frame {
            cmd: MSG_CMD_OTA_VERSION,
            data: vec![
                0xA1, b'V', b'1', b'.', b'2', b'.', b'7', 0x00, b'V', b'1', b'.', b'2', b'.', b'3',
                0x00,
            ],
        };

        assert_eq!(
            parse_response(&frame),
            Some(Response::Version(VersionInfo {
                product_id: 0xA1,
                soft_version: "V1.2.7".into(),
                hard_version: "V1.2.3".into(),
                serial_number: [0u8; 12],
            }))
        );
    }

    #[test]
    fn parses_device_time() {
        let frame = Frame {
            cmd: MSG_CMD_TIME_GET,
            data: vec![26, 4, 7, 13, 14, 15],
        };

        assert_eq!(
            parse_response(&frame),
            Some(Response::TimeGet(DeviceTime {
                year: 2026,
                month: 4,
                day: 7,
                hour: 13,
                minute: 14,
                second: 15,
            }))
        );
    }

    #[test]
    fn parses_time_set_result() {
        let frame = Frame {
            cmd: MSG_CMD_TIME_SET,
            data: vec![0],
        };

        assert_eq!(
            parse_response(&frame),
            Some(Response::TimeSetResult(TimeSetResult {
                success: true,
                code: 0,
            }))
        );
    }

    #[test]
    fn returns_none_for_short_payload() {
        let version = Frame {
            cmd: MSG_CMD_VERSION,
            data: vec![0xA1, b'V', 0x00, b'H', 0x00],
        };
        let time = Frame {
            cmd: MSG_CMD_TIME_GET,
            data: vec![26, 4, 7, 13, 14],
        };

        assert_eq!(parse_response(&version), None);
        assert_eq!(parse_response(&time), None);
    }

    #[test]
    fn returns_none_without_nul_terminators() {
        let frame = Frame {
            cmd: MSG_CMD_VERSION,
            data: vec![
                0xA1, b'V', b'1', b'.', b'2', b'H', b'W', b'9', b'S', b'N', b'0', b'0', b'0', b'0',
                b'0', b'0', b'0', b'0', b'1',
            ],
        };

        assert_eq!(parse_response(&frame), None);
    }
}
