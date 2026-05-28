// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

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
}
pub struct VideoProcessor<'a> {
    inner: Processor<'a>,
}

impl<'a> VideoProcessor<'a> {
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
                    if frame_num == -1 || data.is_empty() {
                        let _ = tx.send(());
                        return true;
                    }
                    false
                },
            );
            pollster::block_on(rx.receive());
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

            // Defensive size bypass for Nikon ZR .r3d when MDK regresses to
            // reporting the proxy stream dimensions (e.g. 249x140 instead of
            // native 3984x2240). All RED cameras and Nikon ZR ship at >= 2K
            // (width >= 1920, area >= 2 MP), so the threshold below is a safe
            // camera-class lower bound: any .r3d under 1000 wide or under
            // 1 megapixel is unambiguously a sub-native MDK read. Real
            // RED2-container R3D files (Komodo, V-RAPTOR) fail telemetry-parser
            // parse_mp4 → peek returns None → MDK value preserved, zero
            // regression for them.
            if filename.to_lowercase().ends_with(".r3d") {
                let (mw, mh) = {
                    let info_b = info.borrow();
                    (info_b.width, info_b.height)
                };
                if mw < 1000 || (mw as u64) * (mh as u64) < 1_000_000 {
                    if let Some((w, h)) = crate::util::peek_container_size_from_url(url) {
                        if w > 0 && h > 0 {
                            let mut info_mut = info.borrow_mut();
                            info_mut.width = w;
                            info_mut.height = h;
                        }
                    }
                }
            }

            std::thread::sleep(std::time::Duration::from_millis(100));
            mdk.mdk.stopProcessing(0);

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
        }
    }
}
