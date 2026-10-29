// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

use super::ffmpeg_processor::VideoInfo;
use super::ffmpeg_video::RateControl;
use super::mdk_processor::*;
use super::*;
use ffmpeg_next::{Dictionary, frame};
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{Arc, atomic::AtomicBool},
};

pub enum Processor<'a> {
    Ffmpeg(FfmpegProcessor<'a>),
    Mdk(MDKProcessor),
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    AppleAnalysis(super::apple_analysis_decoder::AppleAnalysisDecoder),
}
pub struct VideoProcessor<'a> {
    inner: Processor<'a>,
}

fn correct_raw_source_size(url: &str, info: &mut VideoInfo) {
    let filename = gyroflow_core::filesystem::get_filename(url).to_ascii_lowercase();
    // Nikon R3D/NEV can report the decoder's reduced image as the source size.
    // Even a reduced 4K/8K frame can exceed the old small-image threshold.
    // Use valid MP4 source dimensions; native RED keeps the decoder's result.
    if filename.ends_with(".r3d") || filename.ends_with(".nev") {
        if let Some((w, h)) = crate::util::peek_container_size_from_url(url) {
            info.width = w;
            info.height = h;
        }
    }
}

impl<'a> VideoProcessor<'a> {
    pub fn for_optical_analysis(url: &str, gpu: bool, options: Option<Dictionary>, image_sequence: bool) -> Result<Self, FFmpegError> {
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if gpu && !image_sequence && std::env::var("GYROFLOW_OPTICAL_ASYNC_VT").as_deref() != Ok("0") {
            let filename = gyroflow_core::filesystem::get_filename(url).to_ascii_lowercase();
            if (filename.ends_with(".mp4") || filename.ends_with(".mov"))
                && Self::get_video_info(url).is_ok_and(|info| matches!(info.codec_id, ffmpeg_next::codec::Id::H264 | ffmpeg_next::codec::Id::HEVC))
            {
                return Ok(Self { inner: Processor::AppleAnalysis(super::apple_analysis_decoder::AppleAnalysisDecoder::new(url)?) });
            }
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        let _ = image_sequence;
        Self::from_file(url, gpu, 0, options)
    }

    pub fn from_file(
        url: &str,
        gpu_decoding: bool,
        gpu_decoder_index: usize,
        decoder_options: Option<Dictionary>,
    ) -> Result<Self, FFmpegError> {
        let filename = gyroflow_core::filesystem::get_filename(url);
        if filename.to_lowercase().ends_with(".braw")
            || filename.to_lowercase().ends_with(".r3d")
            || filename.to_lowercase().ends_with(".nev")
            || (crate::crm::available() && filename.to_ascii_lowercase().ends_with(".crm"))
        {
            Ok(Self {
                inner: Processor::Mdk(MDKProcessor::from_file(url, decoder_options, gpu_decoding)),
            })
        } else {
            Ok(Self {
                inner: Processor::Ffmpeg(FfmpegProcessor::from_file(
                    url,
                    gpu_decoding,
                    gpu_decoder_index,
                    decoder_options,
                )?),
            })
        }
    }

    pub fn get_video_info(
        url: &str,
    ) -> Result<crate::rendering::ffmpeg_processor::VideoInfo, ffmpeg_next::Error> {
        let filename = gyroflow_core::filesystem::get_filename(url);
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        if filename.to_ascii_lowercase().ends_with(".crm") {
            return crate::crm::video_info(url);
        }
        if filename.to_lowercase().ends_with(".braw")
            || filename.to_lowercase().ends_with(".r3d")
            || filename.to_lowercase().ends_with(".nev")
        {
            let mut mdk = MDKProcessor::from_file(url, None, false);

            let (tx, rx) = futures_intrusive::channel::shared::oneshot_channel();

            let info = Rc::new(RefCell::new(
                crate::rendering::ffmpeg_processor::VideoInfo::default(),
            ));

            let info2 = info.clone();
            mdk.mdk.startProcessing(
                0,
                0,
                0,
                false,
                &mdk.custom_decoder,
                vec![],
                move |frame_num,
                      _,
                      _,
                      _,
                      org_width,
                      org_height,
                      fps,
                      duration_ms,
                      frame_count,
                      data| {
                    if fps > 0.0 && org_width > 0 {
                        let mut info2 = info2.borrow_mut();
                        info2.duration_ms = duration_ms;
                        info2.frame_count = frame_count as usize;
                        info2.fps = fps;
                        info2.width = org_width;
                        info2.height = org_height;
                    }
                    if frame_num < 0 || data.is_empty() {
                        let _ = tx.send(frame_num != -2);
                        return true;
                    }
                    false
                },
            );
            let succeeded = pollster::block_on(rx.receive()).unwrap_or(false);
            info.borrow_mut().rotation = mdk.mdk.getRotation();

            // Nikon ZR .r3d is MP4 with NR3D codec; MDK's R3D plugin returns 0
            // for rotation. Peek the tkhd.matrix via telemetry-parser as a
            // fallback so the batch render path mirrors the QML preview.
            if filename.to_lowercase().ends_with(".r3d") && info.borrow().rotation == 0 {
                let peeked = crate::util::peek_container_rotation_from_url(url);
                if peeked != 0 {
                    info.borrow_mut().rotation = peeked;
                }
            }

            correct_raw_source_size(url, &mut info.borrow_mut());

            std::thread::sleep(std::time::Duration::from_millis(100));
            mdk.mdk.stopProcessing(0);

            if !succeeded {
                log::error!(target: "video.codec", "MDK video info prepare failed: {url}");
                return Err(ffmpeg_next::Error::InvalidData);
            }
            let info = info.borrow().clone();
            Ok(info)
        } else {
            FfmpegProcessor::get_video_info(url)
        }
    }

    pub fn on_frame<F>(&mut self, cb: F)
    where
        F: FnMut(
                i64,
                &mut frame::Video,
                Option<&mut frame::Video>,
                &mut ffmpeg_video_converter::Converter,
                &mut RateControl,
            ) -> Result<(), FFmpegError>
            + 'static,
    {
        match &mut self.inner {
            Processor::Ffmpeg(x) => x.on_frame(cb),
            Processor::Mdk(x) => x.on_frame(cb),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Processor::AppleAnalysis(x) => x.callback = Some(Box::new(cb)),
        }
    }
    /// Select source-frame indices before FFmpeg's hardware download. Callbacks repeat the same
    /// timestamp check for MDK, whose native processing API already supplies CPU frames.
    pub fn set_decode_frame_step(&mut self, every_nth: usize, source_fps: f64) {
        let every_nth = every_nth.max(1);
        if let Processor::Ffmpeg(proc) = &mut self.inner {
            proc.video.decode_frame_step = Some(ffmpeg_video::DecodeFrameStep::at_source_fps(every_nth, source_fps));
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        if let Processor::AppleAnalysis(proc) = &mut self.inner {
            proc.sampling = (every_nth, source_fps);
        }
    }

    pub fn start_decoder_only(
        &mut self,
        ranges: Vec<(f64, f64)>,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<(), FFmpegError> {
        match &mut self.inner {
            Processor::Ffmpeg(x) => x.start_decoder_only(ranges, cancel_flag),
            Processor::Mdk(x) => x.start_decoder_only(ranges, cancel_flag),
            #[cfg(any(target_os = "macos", target_os = "ios"))]
            Processor::AppleAnalysis(x) => x.start(ranges, cancel_flag).map_err(|error| {
                if matches!(error, FFmpegError::GPUDecodingFailed) { FFmpegError::AsyncDecodingFailed } else { error }
            }),
        }
    }

    pub fn set_strict_decode_errors(&mut self, enabled: bool) {
        if let Processor::Ffmpeg(proc) = &mut self.inner {
            proc.video.strict_decode_errors = enabled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Minimal MP4 track headers exercise the actual file and metadata readers.
    // No encoded Nikon frames or SDK are needed to test the size fallback.
    fn container(w: u32, h: u32) -> Vec<u8> {
        fn atom(name: &[u8; 4], payload: Vec<u8>) -> Vec<u8> {
            [((payload.len() + 8) as u32).to_be_bytes().as_slice(), name, &payload].concat()
        }
        let mut mvhd = vec![0; 100];
        mvhd[12..16].copy_from_slice(&60000u32.to_be_bytes());
        mvhd[16..20].copy_from_slice(&1001u32.to_be_bytes());
        let mut tkhd = vec![0; 84];
        tkhd[3] = 3;
        tkhd[12..16].copy_from_slice(&1u32.to_be_bytes());
        tkhd[20..24].copy_from_slice(&1001u32.to_be_bytes());
        for offset in [40, 56] {
            tkhd[offset..offset + 4].copy_from_slice(&65536u32.to_be_bytes());
        }
        tkhd[72..76].copy_from_slice(&0x40000000u32.to_be_bytes());
        tkhd[76..80].copy_from_slice(&(w << 16).to_be_bytes());
        tkhd[80..84].copy_from_slice(&(h << 16).to_be_bytes());
        let mut mdhd = vec![0; 24];
        mdhd[12..16].copy_from_slice(&60000u32.to_be_bytes());
        mdhd[16..20].copy_from_slice(&1001u32.to_be_bytes());
        let mut hdlr = vec![0; 25];
        hdlr[8..12].copy_from_slice(b"vide");
        let mdia = atom(b"mdia", [atom(b"mdhd", mdhd), atom(b"hdlr", hdlr)].concat());
        let trak = atom(b"trak", [atom(b"tkhd", tkhd), mdia].concat());
        let moov = atom(b"moov", [atom(b"mvhd", mvhd), trak].concat());
        [atom(b"ftyp", b"isom\0\0\0\0isom".to_vec()), moov].concat()
    }

    fn probe(dir: &std::path::Path, name: &str, bytes: &[u8], size: (u32, u32)) -> VideoInfo {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let url = gyroflow_core::filesystem::path_to_url(path.to_str().unwrap());
        let mut info = VideoInfo {
            width: size.0, height: size.1, fps: 59.94, duration_ms: 7307.0,
            frame_count: 438, rotation: 90, ..Default::default()
        };
        correct_raw_source_size(&url, &mut info);
        assert_eq!((info.fps, info.duration_ms, info.frame_count, info.rotation), (59.94, 7307.0, 438, 90));
        info
    }

    #[test]
    fn raw_size_fallback_reads_nev_and_r3d_container_dimensions() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["clip.NEV", "clip.nev", "clip.R3D", "clip.r3d"] {
            for decoded_size in [(378, 212), (1512, 850), (3024, 1701), (6048, 3402)] {
                let info = probe(dir.path(), name, &container(6048, 3402), decoded_size);
                assert_eq!((info.width, info.height), (6048, 3402), "{name} {decoded_size:?}");
            }
        }
    }

    #[test]
    fn raw_size_fallback_uses_container_dimensions_and_preserves_other_formats() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["clip.NEV", "clip.R3D"] {
            let info = probe(dir.path(), name, &container(6060, 3410), (6048, 3402));
            assert_eq!((info.width, info.height), (6060, 3410));
        }
        for name in ["clip.BRAW", "clip.MP4", "clip.CRM"] {
            let info = probe(dir.path(), name, &container(6048, 3402), (378, 212));
            assert_eq!((info.width, info.height), (378, 212));
        }
    }

    #[test]
    fn raw_size_fallback_preserves_size_when_container_is_unusable() {
        let dir = tempfile::tempdir().unwrap();
        for bytes in [b"RED2".to_vec(), container(0, 3402), container(6048, 0)] {
            for name in ["clip.NEV", "clip.R3D"] {
                let info = probe(dir.path(), name, &bytes, (378, 212));
                assert_eq!((info.width, info.height), (378, 212));
            }
        }
    }
}
