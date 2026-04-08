// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::time::{Duration, Instant};

pub const FRAME_HEADER_START: u8 = 0xAA;
pub const FRAME_HEADER_END: u8 = 0x55;
pub const PROTOCOL_VERSION: u8 = 1;
pub const LEGACY_PROTOCOL_VERSION: u8 = 0;
pub const DEFAULT_MAX_PAYLOAD_LEN: usize = 256;
pub const DEFAULT_PARTIAL_FRAME_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub cmd: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
enum ParseState {
    SeekingStart,
    SeekingEnd,
    ReadingVersion,
    ReadingCmd,
    ReadingLen {
        cmd: u8,
    },
    ReadingData {
        cmd: u8,
        expected_len: usize,
        data: Vec<u8>,
    },
    ReadingChecksum {
        cmd: u8,
        data: Vec<u8>,
    },
}

#[derive(Clone, Debug)]
pub struct FrameParser {
    state: ParseState,
    frame_bytes: Vec<u8>,
    max_payload_len: usize,
    partial_frame_timeout: Duration,
    last_activity: Option<Instant>,
}

impl Default for FrameParser {
    fn default() -> Self {
        Self::new()
    }
}

impl FrameParser {
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_PAYLOAD_LEN, DEFAULT_PARTIAL_FRAME_TIMEOUT)
    }

    pub fn with_limits(max_payload_len: usize, partial_frame_timeout: Duration) -> Self {
        Self {
            state: ParseState::SeekingStart,
            frame_bytes: Vec::with_capacity(6),
            max_payload_len,
            partial_frame_timeout,
            last_activity: None,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<Frame> {
        self.feed_at(bytes, Instant::now())
    }

    pub fn feed_at(&mut self, bytes: &[u8], now: Instant) -> Vec<Frame> {
        self.clear_if_timed_out_at(now);

        let mut frames = Vec::new();
        for &byte in bytes {
            self.process_byte(byte, now, &mut frames);
        }
        frames
    }

    pub fn clear_if_timed_out(&mut self) -> bool {
        self.clear_if_timed_out_at(Instant::now())
    }

    pub fn clear_if_timed_out_at(&mut self, now: Instant) -> bool {
        if let Some(last_activity) = self.last_activity {
            if now.saturating_duration_since(last_activity) >= self.partial_frame_timeout {
                self.reset();
                return true;
            }
        }
        false
    }

    fn process_byte(&mut self, byte: u8, now: Instant, frames: &mut Vec<Frame>) {
        let state = std::mem::replace(&mut self.state, ParseState::SeekingStart);
        match state {
            ParseState::SeekingStart => {
                if byte == FRAME_HEADER_START {
                    self.start_frame(now);
                    self.state = ParseState::SeekingEnd;
                }
            }
            ParseState::SeekingEnd => {
                if byte == FRAME_HEADER_END {
                    self.push_frame_byte(byte, now);
                    self.state = ParseState::ReadingVersion;
                } else if byte == FRAME_HEADER_START {
                    self.start_frame(now);
                    self.state = ParseState::SeekingEnd;
                } else {
                    self.reset();
                }
            }
            ParseState::ReadingVersion => {
                if byte == PROTOCOL_VERSION || byte == LEGACY_PROTOCOL_VERSION {
                    self.push_frame_byte(byte, now);
                    self.state = ParseState::ReadingCmd;
                } else {
                    self.reject_and_resync(byte, now);
                }
            }
            ParseState::ReadingCmd => {
                self.push_frame_byte(byte, now);
                self.state = ParseState::ReadingLen { cmd: byte };
            }
            ParseState::ReadingLen { cmd } => {
                if (byte as usize) <= self.max_payload_len {
                    self.push_frame_byte(byte, now);
                    if byte == 0 {
                        self.state = ParseState::ReadingChecksum {
                            cmd,
                            data: Vec::new(),
                        };
                    } else {
                        self.state = ParseState::ReadingData {
                            cmd,
                            expected_len: byte as usize,
                            data: Vec::with_capacity(byte as usize),
                        };
                    }
                } else {
                    self.reject_and_resync(byte, now);
                }
            }
            ParseState::ReadingData {
                cmd,
                expected_len,
                mut data,
            } => {
                self.push_frame_byte(byte, now);
                data.push(byte);
                if data.len() == expected_len {
                    self.state = ParseState::ReadingChecksum { cmd, data };
                } else {
                    self.state = ParseState::ReadingData {
                        cmd,
                        expected_len,
                        data,
                    };
                }
            }
            ParseState::ReadingChecksum { cmd, data } => {
                let expected = checksum(&self.frame_bytes);
                if byte == expected {
                    frames.push(Frame { cmd, data });
                    self.reset();
                } else {
                    self.reject_and_resync(byte, now);
                }
            }
        }
    }

    fn start_frame(&mut self, now: Instant) {
        self.frame_bytes.clear();
        self.frame_bytes.push(FRAME_HEADER_START);
        self.last_activity = Some(now);
    }

    fn push_frame_byte(&mut self, byte: u8, now: Instant) {
        self.frame_bytes.push(byte);
        self.last_activity = Some(now);
    }

    fn reject_and_resync(&mut self, byte: u8, now: Instant) {
        self.reset();
        if byte == FRAME_HEADER_START {
            self.start_frame(now);
            self.state = ParseState::SeekingEnd;
        }
    }

    fn reset(&mut self) {
        self.state = ParseState::SeekingStart;
        self.frame_bytes.clear();
        self.last_activity = None;
    }
}

pub fn encode(cmd: u8, data: &[u8]) -> Vec<u8> {
    assert!(
        data.len() <= u8::MAX as usize,
        "payload length {} exceeds protocol limit",
        data.len()
    );

    let mut frame = Vec::with_capacity(6 + data.len());
    frame.push(FRAME_HEADER_START);
    frame.push(FRAME_HEADER_END);
    frame.push(PROTOCOL_VERSION);
    frame.push(cmd);
    frame.push(data.len() as u8);
    frame.extend_from_slice(data);
    frame.push(checksum(&frame));
    frame
}

fn checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |acc, byte| acc.wrapping_add(*byte))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_frame() -> Frame {
        Frame {
            cmd: 0x42,
            data: vec![0x10, 0x20, 0x30, 0x40],
        }
    }

    #[test]
    fn encodes_and_decodes_round_trip() {
        let frame = sample_frame();
        let encoded = encode(frame.cmd, &frame.data);
        let mut parser = FrameParser::new();

        assert_eq!(parser.feed_at(&encoded, Instant::now()), vec![frame]);
    }

    #[test]
    fn parses_byte_by_byte() {
        let frame = sample_frame();
        let encoded = encode(frame.cmd, &frame.data);
        let mut parser = FrameParser::new();
        let now = Instant::now();
        let mut decoded = Vec::new();

        for byte in encoded {
            decoded.extend(parser.feed_at(&[byte], now));
        }

        assert_eq!(decoded, vec![frame]);
    }

    #[test]
    fn parses_random_chunks() {
        let frame = sample_frame();
        let encoded = encode(frame.cmd, &frame.data);
        let mut parser = FrameParser::new();
        let mut rng = fastrand::Rng::with_seed(7);
        let now = Instant::now();
        let mut decoded = Vec::new();
        let mut offset = 0;

        while offset < encoded.len() {
            let chunk_len = rng.usize(1..=3).min(encoded.len() - offset);
            decoded.extend(parser.feed_at(&encoded[offset..offset + chunk_len], now));
            offset += chunk_len;
        }

        assert_eq!(decoded, vec![frame]);
    }

    #[test]
    fn drops_checksum_errors() {
        let mut encoded = encode(0x33, &[1, 2, 3]);
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;

        let mut parser = FrameParser::new();
        assert!(parser.feed_at(&encoded, Instant::now()).is_empty());
    }

    #[test]
    fn parses_multiple_frames() {
        let first = encode(0x11, &[1, 2]);
        let second = encode(0x22, &[3, 4, 5]);
        let mut parser = FrameParser::new();
        let mut input = Vec::new();
        input.extend_from_slice(&first);
        input.extend_from_slice(&second);

        assert_eq!(
            parser.feed_at(&input, Instant::now()),
            vec![
                Frame {
                    cmd: 0x11,
                    data: vec![1, 2],
                },
                Frame {
                    cmd: 0x22,
                    data: vec![3, 4, 5],
                },
            ]
        );
    }

    #[test]
    fn resyncs_after_garbage_bytes() {
        let valid = encode(0x55, &[9, 8, 7]);
        let mut input = vec![0x00, 0x99, 0xAA, 0x01, 0x02, 0xAA];
        input.extend_from_slice(&valid[1..]);
        let mut parser = FrameParser::new();

        assert_eq!(
            parser.feed_at(&input, Instant::now()),
            vec![Frame {
                cmd: 0x55,
                data: vec![9, 8, 7],
            }]
        );
    }

    #[test]
    fn rejects_len_over_limit() {
        let encoded = encode(0x66, &[1, 2, 3, 4, 5]);
        let mut parser = FrameParser::with_limits(4, Duration::from_secs(1));

        assert!(parser.feed_at(&encoded, Instant::now()).is_empty());
    }

    #[test]
    fn drops_invalid_version() {
        let mut encoded = encode(0x77, &[1, 2, 3]);
        encoded[2] = 2;

        let mut parser = FrameParser::new();
        assert!(parser.feed_at(&encoded, Instant::now()).is_empty());
    }

    #[test]
    fn accepts_legacy_zero_version_response() {
        let mut encoded = encode(0x79, &[4, 5, 6]);
        encoded[2] = LEGACY_PROTOCOL_VERSION;
        let checksum_index = encoded.len() - 1;
        let checksum = encoded[..checksum_index]
            .iter()
            .fold(0u8, |acc, byte| acc.wrapping_add(*byte));
        encoded[checksum_index] = checksum;

        let mut parser = FrameParser::new();
        assert_eq!(
            parser.feed_at(&encoded, Instant::now()),
            vec![Frame {
                cmd: 0x79,
                data: vec![4, 5, 6],
            }]
        );
    }

    #[test]
    fn clears_partial_frame_after_timeout() {
        let encoded = encode(0x88, &[1, 2, 3, 4]);
        let timeout = Duration::from_millis(10);
        let mut parser = FrameParser::with_limits(DEFAULT_MAX_PAYLOAD_LEN, timeout);
        let start = Instant::now();

        assert!(parser.feed_at(&encoded[..3], start).is_empty());
        assert!(parser.clear_if_timed_out_at(start + timeout + Duration::from_millis(1)));
        assert_eq!(
            parser.feed_at(&encoded, start + timeout + Duration::from_millis(2)),
            vec![Frame {
                cmd: 0x88,
                data: vec![1, 2, 3, 4],
            }]
        );
    }
}
