// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

// Process-global blocklist of codec signatures known to fail GPU decoding
// in the current sync session. Cleared at the start of every Auto-sync
// invocation (single or batch) to give GPU a fresh chance after restarts.

use crate::rendering::ffmpeg_processor::VideoInfo;
use parking_lot::Mutex;
use std::collections::HashSet;
use std::sync::OnceLock;

// codec::Id and format::Pixel don't impl Hash, so we store the underlying
// FFmpeg integer codes (which are stable across libavcodec versions) and
// reconstruct the typed values for diagnostics via Debug.
#[derive(Hash, Eq, PartialEq, Clone, Copy)]
pub struct CodecSignature {
    pub codec_id_raw: i32,
    pub pix_fmt_raw: i32,
    pub profile: i32,
}

impl std::fmt::Debug for CodecSignature {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Render with typed names where ffmpeg-next can map the integer back.
        let codec_id = ffmpeg_next::codec::Id::from(unsafe {
            std::mem::transmute::<i32, ffmpeg_next::ffi::AVCodecID>(self.codec_id_raw)
        });
        let pix_fmt = ffmpeg_next::format::Pixel::from(unsafe {
            std::mem::transmute::<i32, ffmpeg_next::ffi::AVPixelFormat>(self.pix_fmt_raw)
        });
        f.debug_struct("CodecSignature")
            .field("codec", &codec_id)
            .field("pix_fmt", &pix_fmt)
            .field("profile", &self.profile)
            .finish()
    }
}

impl From<&VideoInfo> for CodecSignature {
    fn from(info: &VideoInfo) -> Self {
        Self {
            codec_id_raw: unsafe {
                std::mem::transmute::<ffmpeg_next::ffi::AVCodecID, i32>(info.codec_id.into())
            },
            pix_fmt_raw: unsafe {
                std::mem::transmute::<ffmpeg_next::ffi::AVPixelFormat, i32>(info.pix_fmt.into())
            },
            profile: info.profile,
        }
    }
}

static BLOCKLIST: OnceLock<Mutex<HashSet<CodecSignature>>> = OnceLock::new();

fn store() -> &'static Mutex<HashSet<CodecSignature>> {
    BLOCKLIST.get_or_init(|| Mutex::new(HashSet::new()))
}

pub fn is_blocklisted(sig: &CodecSignature) -> bool {
    store().lock().contains(sig)
}

pub fn record_failure(sig: CodecSignature) {
    store().lock().insert(sig);
}

pub fn clear() {
    store().lock().clear();
}
