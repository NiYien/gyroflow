// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::collections::HashMap;
use std::error;
use std::sync::Arc;
use std::sync::atomic::Ordering::Relaxed;

use ffmpeg_next::{
    Dictionary, Rational, Stream, codec, encoder, ffi, format, frame, media, rescale,
    rescale::Rescale,
};

#[cfg(target_os = "android")]
use super::ffmpeg_android::*;
use super::ffmpeg_audio::*;
use super::ffmpeg_video::*;
use super::*;
use gyroflow_core::filesystem::{self, FfmpegPathWrapper, FilesystemError};

#[derive(Debug, Default)]
pub struct FrameTimestamps {
    pub first: Option<i64>,
    pub last_video: Option<i64>,
    pub last_audio: Option<i64>,
    pub add_audio: i64,
    pub add_video: i64,
    pub last_duration_video: i64,
    pub last_duration_audio: i64,
}

pub struct FfmpegProcessor<'a> {
    pub gpu_decoding: bool,
    pub gpu_device: Option<String>,
    pub video_codec: Option<String>,

    pub audio_codec: codec::Id,

    input_context: format::context::Input,

    pub video: VideoTranscoder<'a>,

    pub ranges_ms: Vec<(Option<f64>, Option<f64>)>,

    pub decoder_fps: f64,

    pub preserve_other_tracks: bool,

    #[cfg(target_os = "android")]
    pub android_handles: Option<AndroidHWHandles>,

    ost_time_bases: Vec<Rational>,

    frame_ts: FrameTimestamps,

    _file: FfmpegPathWrapper,
}

#[derive(PartialEq)]
pub enum Status {
    Continue,
    Finish,
}

#[derive(Debug)]
pub enum FFmpegError {
    EncoderNotFound,
    DecoderNotFound,
    NoSupportedFormats,
    NoOutputContext,
    EncoderConverterEmpty,
    ConverterEmpty,
    FrameEmpty,
    NoGPUDecodingDevice,
    NoHWTransferFormats,
    FromHWTransferError(i32),
    ToHWTransferError(i32),
    CannotCreateGPUDecoding,
    NoFramesContext,
    GPUDecodingFailed,
    AsyncDecodingFailed,
    ToHWBufferError(i32),
    PixelFormatNotSupported((format::Pixel, Vec<format::Pixel>, Option<format::Pixel>)),
    /// The device rejected the encoder itself, not merely a pixel format: the
    /// selected hardware H.264 encoder cannot encode the source's bit depth (no
    /// GPU family can encode H.264 above 8-bit). Carries the encoder name and
    /// the rejected pixel format for diagnostics. Distinct from
    /// `PixelFormatNotSupported` because the fix is switching the output codec,
    /// not the pixel format — the render layer heals it silently instead of
    /// raising the format-choice dialog.
    EncoderCodecUnsupported((String, format::Pixel)),
    UnknownPixelFormat(format::Pixel),
    InternalError(ffmpeg_next::Error),
    CannotOpenInputFile((String, FilesystemError)),
    CannotOpenOutputFile((String, FilesystemError)),
}

impl std::fmt::Display for FFmpegError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            FFmpegError::AsyncDecodingFailed => write!(f, "Asynchronous hardware decoding failed"),
            FFmpegError::EncoderNotFound => write!(f, "Encoder not found"),
            FFmpegError::DecoderNotFound => write!(f, "Decoder not found"),
            FFmpegError::NoSupportedFormats => write!(f, "No supported formats"),
            FFmpegError::NoOutputContext => write!(f, "No output context"),
            FFmpegError::EncoderConverterEmpty => write!(f, "Encoder converter is null"),
            FFmpegError::ConverterEmpty => write!(f, "Converter is null"),
            FFmpegError::FrameEmpty => write!(f, "Frame is null"),
            FFmpegError::NoHWTransferFormats => write!(f, "No hardware transfer formats"),
            FFmpegError::FromHWTransferError(i) => write!(
                f,
                "Error transferring frame from the GPU: {:?}",
                ffmpeg_next::Error::Other { errno: *i }
            ),
            FFmpegError::ToHWTransferError(i) => write!(
                f,
                "Error transferring frame to the GPU: {:?}",
                ffmpeg_next::Error::Other { errno: *i }
            ),
            FFmpegError::ToHWBufferError(i) => write!(
                f,
                "Error getting HW transfer buffer to the GPU: {:?}",
                ffmpeg_next::Error::Other { errno: *i }
            ),
            FFmpegError::NoFramesContext => write!(f, "Empty hw frames context"),
            FFmpegError::GPUDecodingFailed => write!(f, "GPU decoding failed, please try again."),
            FFmpegError::CannotCreateGPUDecoding => {
                write!(f, "Unable to create HW devices context")
            }
            FFmpegError::NoGPUDecodingDevice => {
                write!(f, "Unable to create any HW decoding context")
            }
            FFmpegError::UnknownPixelFormat(v) => write!(f, "Unknown pixel format: {:?}", v),
            FFmpegError::PixelFormatNotSupported(v) => write!(
                f,
                "Pixel format {:?} is not supported. Supported ones: {:?}. Optimal choice: {:?}",
                v.0, v.1, v.2
            ),
            FFmpegError::EncoderCodecUnsupported((name, fmt)) => write!(
                f,
                "Encoder {name} cannot encode pixel format {fmt:?} on this device"
            ),
            FFmpegError::InternalError(e) => write!(f, "ffmpeg error: {:?}", e),
            FFmpegError::CannotOpenInputFile((url, e)) => {
                write!(f, "Cannot open input file {url}: {e:?}")
            }
            FFmpegError::CannotOpenOutputFile((url, e)) => {
                write!(f, "Cannot open output file {url}: {e:?}")
            }
        }
    }
}
impl error::Error for FFmpegError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match *self {
            FFmpegError::InternalError(ref e) => Some(e),
            _ => None,
        }
    }
}
impl From<ffmpeg_next::Error> for FFmpegError {
    fn from(err: ffmpeg_next::Error) -> FFmpegError {
        FFmpegError::InternalError(err)
    }
}

#[derive(Debug, Clone)]
pub struct VideoInfo {
    pub duration_ms: f64,
    pub frame_count: usize,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub bitrate: f64, // in Mbps
    pub rotation: i32,
    pub created_at: Option<i64>,
    pub codec_id: codec::Id,
    pub pix_fmt: ffmpeg_next::format::Pixel,
    pub profile: i32,
}

impl Default for VideoInfo {
    fn default() -> Self {
        Self {
            duration_ms: 0.0,
            frame_count: 0,
            fps: 0.0,
            width: 0,
            height: 0,
            bitrate: 0.0,
            rotation: 0,
            created_at: None,
            codec_id: codec::Id::None,
            pix_fmt: ffmpeg_next::format::Pixel::None,
            profile: -99, // FF_PROFILE_UNKNOWN
        }
    }
}

pub(super) fn decoder_error(gpu_decoding: bool, stage: &str, error: ffmpeg_next::Error) -> FFmpegError {
    ::log::warn!(target: "video.codec", "decoder failed stage={stage} hardware={gpu_decoding} error={error:?}");
    if gpu_decoding && error != Error::Eof
        && error != (Error::Other { errno: ffmpeg_next::util::error::EAGAIN })
    {
        FFmpegError::GPUDecodingFailed
    } else {
        error.into()
    }
}

fn frame_processing_error(gpu_decoding: bool, error: FFmpegError) -> FFmpegError {
    // A download failure belongs to decoding. Upload and encoder failures do not.
    // Never use the shared FFmpeg log to classify errors from parallel jobs.
    if gpu_decoding && matches!(error, FFmpegError::FromHWTransferError(_)) {
        ::log::warn!(target: "video.codec", "hardware frame download failed: {error:?}");
        FFmpegError::GPUDecodingFailed
    } else {
        error
    }
}

pub(super) fn next_decoder_attempt(index: i32, rendered_frames: usize, error: &FFmpegError) -> Option<i32> {
    if index < 0 {
        return None;
    }
    if matches!(error, FFmpegError::GPUDecodingFailed) {
        return Some(-1);
    }
    if rendered_frames == 0 {
        match index {
            0..=3 => return Some(index + 1),
            4 => return Some(-1),
            _ => {}
        }
    }
    None
}

impl<'a> FfmpegProcessor<'a> {
    pub fn from_file(
        url: &str,
        mut gpu_decoding: bool,
        gpu_decoder_index: usize,
        mut decoder_options: Option<Dictionary>,
    ) -> Result<Self, FFmpegError> {
        let mut file = FfmpegPathWrapper::new(url, false)
            .map_err(|e| FFmpegError::CannotOpenInputFile((url.to_string(), e)))?;

        ffmpeg_next::init()?;
        crate::rendering::init_log();

        let hwaccel_device = decoder_options
            .as_ref()
            .and_then(|x| x.get("hwaccel_device").map(|x| x.to_string()));
        if file.path.starts_with("fd:") {
            match &mut decoder_options {
                Some(dict) => {
                    dict.set("fd", &file.path[3..]);
                    file.path = "fd:".into();
                }
                None => {
                    let mut dict = Dictionary::new();
                    dict.set("fd", &file.path[3..]);
                    file.path = "fd:".into();
                    decoder_options = Some(dict);
                }
            }
        }

        let mut input_context = decoder_options.map_or_else(
            || format::input(&file.path),
            |dict| format::input_with_dictionary(&file.path, dict),
        )?;

        // format::context::input::dump(&input_context, 0, Some(file.path));

        let best_video_stream = unsafe {
            let mut decoder: *const ffi::AVCodec = std::ptr::null();
            let index = ffi::av_find_best_stream(
                input_context.as_mut_ptr(),
                media::Type::Video.into(),
                -1i32,
                -1i32,
                &mut decoder,
                0,
            );
            if index >= 0 && !decoder.is_null() {
                if gpu_decoding && cfg!(target_os = "android") {
                    let decoder_name = match (*decoder).id {
                        ffi::AVCodecID::AV_CODEC_ID_H264 => Some("h264_mediacodec"),
                        ffi::AVCodecID::AV_CODEC_ID_HEVC => Some("hevc_mediacodec"),
                        ffi::AVCodecID::AV_CODEC_ID_VP8 => Some("vp8_mediacodec"),
                        ffi::AVCodecID::AV_CODEC_ID_VP9 => Some("vp9_mediacodec"),
                        ffi::AVCodecID::AV_CODEC_ID_AV1 => Some("av1_mediacodec"),
                        _ => None,
                    };
                    if let Some(name) = decoder_name {
                        let name = std::ffi::CString::new(name).unwrap();
                        let mc_ptr = ffi::avcodec_find_decoder_by_name(name.as_ptr());
                        if !mc_ptr.is_null() {
                            decoder = mc_ptr;
                        }
                    }
                }
                Ok((Stream::wrap(&input_context, index as usize), decoder))
            } else {
                Err(Error::StreamNotFound)
            }
        };

        let strm = best_video_stream?;
        let stream = strm.0;
        let decoder = strm.1;

        let decoder_fps = stream.rate().into();

        let mut decoder_ctx =
            unsafe { codec::context::Context::wrap(ffi::avcodec_alloc_context3(decoder), None) };
        unsafe {
            if ffi::avcodec_parameters_to_context(
                decoder_ctx.as_mut_ptr(),
                stream.parameters().as_ptr(),
            ) < 0
            {
                ::log::error!("avcodec_parameters_to_context failed");
                return Err(FFmpegError::DecoderNotFound);
            }
        }
        decoder_ctx.set_threading(ffmpeg_next::threading::Config {
            kind: ffmpeg_next::threading::Type::Frame,
            count: 5,
        });

        let codec = decoder_ctx.codec().ok_or(FFmpegError::DecoderNotFound)?;

        // ProRes has no real hardware video decoder outside Apple Silicon, which
        // has a dedicated ProRes decode block reachable via VideoToolbox. On every
        // other platform the only "HW" path is software decode plus a Vulkan
        // upload/scale/transfer that is fragile and can lose the device
        // (VK_ERROR_DEVICE_LOST) on some GPUs, yielding zero decoded frames. Force
        // software decode for ProRes everywhere except macOS on aarch64.
        if gpu_decoding
            && matches!(unsafe { (*decoder).id }, ffi::AVCodecID::AV_CODEC_ID_PRORES)
            && !cfg!(all(target_os = "macos", target_arch = "aarch64"))
        {
            ::log::info!(
                "ProRes: forcing software decode (hardware decode only supported on Apple Silicon)"
            );
            gpu_decoding = false;
        }

        let mut hw_backend = String::new();
        if gpu_decoding {
            let hw = ffmpeg_hw::init_device_for_decoding(
                gpu_decoder_index,
                unsafe { codec.as_ptr() },
                &mut decoder_ctx,
                hwaccel_device.as_deref(),
            )?;
            log::debug!(
                "Selected HW backend {:?} ({}) with format {:?}",
                hw.1,
                hw.2,
                hw.3
            );
            hw_backend = hw.2;
        }
        gpu_decoding = !hw_backend.is_empty();

        Ok(Self {
            _file: file,
            gpu_decoding,
            gpu_device: if !gpu_decoding {
                None
            } else {
                Some(hw_backend)
            },
            video_codec: None,

            audio_codec: codec::Id::AAC,

            ost_time_bases: Vec::new(),

            frame_ts: Default::default(),

            ranges_ms: Vec::new(),

            preserve_other_tracks: false,

            decoder_fps,

            #[cfg(target_os = "android")]
            android_handles: None, //if gpu_decoding { AndroidHWHandles::init_with_context(&mut decoder_ctx).ok() } else { None },

            video: VideoTranscoder {
                gpu_encoding: true,
                gpu_decoding,
                input_index: stream.index(),
                encoder_params: EncoderParams {
                    options: Dictionary::new(),
                    ..EncoderParams::default()
                },
                decoder: Some(decoder_ctx.decoder().open_as(codec)
                    .map_err(|e| decoder_error(gpu_decoding, "open", e))?.video()?),
                ..VideoTranscoder::default()
            },

            input_context,
        })
    }

    pub fn render(
        &mut self,
        output_folder: &str,
        output_filename: &str,
        output_size: (u32, u32),
        bitrate: Option<f64>,
        cancel_flag: Arc<AtomicBool>,
        pause_flag: Arc<AtomicBool>,
    ) -> Result<(), FFmpegError> {
        // Logging context for the encode pipeline.
        use super::export_timing::{Stage, WallTimer, measure};
        let timing = self.video.timing.clone();
        let _log_ctx = gyroflow_core::log_context::LogContext::enter(
            gyroflow_core::log_context::LogContextUpdate::default().op("encode"),
        );
        let output_url = filesystem::get_file_url(output_folder, output_filename, true);
        let mut file = FfmpegPathWrapper::new(&output_url, true)
            .map_err(|e| FFmpegError::CannotOpenOutputFile((output_url.to_string(), e)))?;

        let mut stream_mapping: Vec<isize> = vec![0; self.input_context.nb_streams() as _];
        let mut ist_time_bases = vec![Rational(0, 0); self.input_context.nb_streams() as _];
        self.ost_time_bases
            .resize(self.input_context.nb_streams() as _, Rational(0, 0));
        let mut atranscoders = HashMap::new();
        let mut output_index = 0usize;

        let mut start_ms = None;
        let mut end_ms = None;

        if let Some(first_range) = self.ranges_ms.first() {
            if let Some(start) = first_range.0 {
                start_ms = Some(start);
                let position = (start as i64).rescale((1, 1000), rescale::TIME_BASE);
                self.input_context.seek(position, ..position)?;
            }
            if let Some(end) = first_range.1 {
                end_ms = Some(end);
            }
            self.ranges_ms.remove(0);
        }

        let output_filename = output_filename
            .strip_suffix(".tmp")
            .unwrap_or(output_filename);

        let mut output_options = Dictionary::new();
        let mut output_format = if let Some(pos) = output_filename.rfind('.') {
            &output_filename[pos + 1..]
        } else {
            "mp4"
        }
        .to_ascii_lowercase();
        if file.path.starts_with("fd:") {
            output_options.set("fd", &file.path[3..]);
            file.path = "fd:".into();
        }
        if output_format == "mkv" {
            output_format = String::from("matroska");
        }

        let mut octx = if output_format == "exr" || output_format == "png" {
            format::output_with(&file.path, output_options)
        } else {
            format::output_as_with(&file.path, &output_format, output_options)
        }?;

        // Copy metadata
        let mut metadata = self.input_context.metadata().to_owned();
        for (k, v) in self.video.encoder_params.metadata.iter() {
            metadata.set(k, v);
        }

        for (i, stream) in self.input_context.streams().enumerate() {
            // Copy timecode from stream if global metadata doesn't have it
            if let Some(timecode) = stream.metadata().get("timecode") {
                if metadata.get("timecode").is_none() {
                    metadata.set("timecode", timecode);
                }
            }

            let medium = stream.parameters().medium();
            if medium != media::Type::Audio
                && medium != media::Type::Video
                && (!self.preserve_other_tracks || medium != media::Type::Data)
            {
                stream_mapping[i] = -1;
                continue;
            }
            // Limit to first video stream
            if medium == media::Type::Video && self.video.output_index.is_some() {
                stream_mapping[i] = -1;
                continue;
            }
            stream_mapping[i] = output_index as isize;
            ist_time_bases[i] = stream.time_base();

            if medium == media::Type::Video {
                self.video.input_index = i;
                self.video.output_index = Some(output_index);

                let codec =
                    encoder::find_by_name(self.video_codec.as_ref().ok_or(Error::EncoderNotFound)?)
                        .ok_or(Error::EncoderNotFound)?;
                unsafe {
                    if !codec.as_ptr().is_null() {
                        self.video.codec_supported_formats =
                            super::ffmpeg_hw::pix_formats_to_vec((*codec.as_ptr()).pix_fmts);
                        log::debug!("Codec formats: {:?}", self.video.codec_supported_formats);
                    }
                }
                let mut out_stream = octx.add_stream(codec)?;
                self.video.encoder_params.codec = Some(codec);

                self.video.encoder_params.frame_rate = Some(stream.avg_frame_rate());
                self.video.encoder_params.time_base = Some(stream.rate().invert());

                out_stream.set_rate(stream.rate());
                out_stream.set_time_base(stream.time_base());
                out_stream.set_avg_frame_rate(stream.avg_frame_rate());

                output_index += 1;
            } else if medium == media::Type::Audio && self.audio_codec != codec::Id::None {
                if self.preserve_other_tracks
                /*stream.codec().id() == self.audio_codec*/
                {
                    // Direct stream copy
                    let mut ost = octx.add_stream(encoder::find(codec::Id::None))?;
                    ost.set_parameters(stream.parameters());
                    // We need to set codec_tag to 0 lest we run into incompatible codec tag issues when muxing into a different container format.
                    unsafe {
                        (*ost.parameters().as_mut_ptr()).codec_tag = 0;
                    }
                } else {
                    // Transcode audio
                    atranscoders.insert(
                        i,
                        AudioTranscoder::new(
                            self.audio_codec,
                            &stream,
                            &mut octx,
                            output_index as _,
                        ).map(|mut audio| { audio.timing = timing.clone(); audio })?,
                    );
                }
                output_index += 1;
            } else if self.preserve_other_tracks && medium == media::Type::Data {
                // Direct stream copy
                let mut ost = octx.add_stream(encoder::find(codec::Id::None))?;
                ost.set_parameters(stream.parameters());
                ost.set_avg_frame_rate(stream.avg_frame_rate());
                output_index += 1;
            }
        }
        let mut updated_creation_time = None;
        if let Some(start_ms) = start_ms {
            if start_ms > 0.0 {
                for (k, v) in metadata.iter() {
                    if k == "creation_time" {
                        if let Ok(v) = chrono::DateTime::parse_from_rfc3339(v) {
                            if let Some(v) = v.checked_add_signed(
                                chrono::TimeDelta::try_milliseconds(start_ms.round() as i64)
                                    .unwrap(),
                            ) {
                                updated_creation_time = Some(v.to_rfc3339());
                            }
                        }
                        break;
                    }
                }
            }
        }
        if let Some(updated_creation_time) = updated_creation_time {
            metadata.set("creation_time", &updated_creation_time);
        }
        log::debug!("Output metadata: {:?}", &metadata);
        octx.set_metadata(metadata);
        // Header will be written after video encoder is initalized, in ffmpeg_video.rs:init_encoder

        let mut video_inited = false;
        // let mut copied_stream_first_pts = None;
        // let mut copied_stream_first_dts = None;

        let process_stream = |atranscoders: &mut HashMap<usize, AudioTranscoder>,
                              octx: &mut format::context::Output,
                              stream: Stream,
                              mut packet: ffmpeg_next::Packet,
                              start_ms: Option<f64>,
                              end_ms: Option<f64>,
                              ist_index: usize,
                              ost_index: isize,
                              ost_time_base: Rational,
                              frame_ts: &mut FrameTimestamps|
         -> Result<bool, Error> {
            match atranscoders.get_mut(&ist_index) {
                Some(atranscoder) => {
                    packet.rescale_ts(stream.time_base(), atranscoder.decoder.time_base());
                    measure(&timing, Stage::Decode, || atranscoder.decoder.send_packet(&packet))?;
                    let status = atranscoder.receive_and_process_decoded_frames(
                        octx,
                        ost_time_base,
                        start_ms,
                        end_ms,
                        frame_ts,
                    )?;
                    if status == Status::Finish {
                        return Ok(true);
                    }
                }
                None => {
                    // Direct stream copy
                    // TODO: Wrong pts, shifted by length of packet, would need to synchronize with first video frame pts
                    // if copied_stream_first_pts.is_none() {
                    //     copied_stream_first_pts = packet.pts();
                    //     copied_stream_first_dts = packet.dts();
                    // }

                    packet.rescale_ts(ist_time_bases[ist_index], ost_time_base);
                    packet.set_position(-1);
                    packet.set_stream(ost_index as _);
                    // packet.set_pts(packet.pts().map(|x| x - copied_stream_first_pts.unwrap_or_default()));
                    // packet.set_dts(packet.dts().map(|x| x - copied_stream_first_dts.unwrap_or_default()));
                    measure(&timing, Stage::Mux, || packet.write_interleaved(octx))?;
                }
            }
            Ok(false)
        };

        let _wall_timer = WallTimer::new(&timing);
        loop {
            let mut pending_packets: Vec<(Stream, ffmpeg_next::Packet, usize, isize)> = Vec::new();

            let mut encoding_video = true;
            let mut encoding_audio = self.audio_codec != codec::Id::None;

            let mut packets = self.input_context.packets();
            for (stream, mut packet) in std::iter::from_fn(|| measure(&timing, Stage::Demux, || packets.next())) {
                let ist_index = stream.index();
                let ost_index = stream_mapping[ist_index];
                if ost_index < 0 {
                    continue;
                }

                if ist_index == self.video.input_index {
                    if encoding_video {
                        {
                            let decoder =
                                self.video.decoder.as_mut().ok_or(Error::DecoderNotFound)?;
                            packet.rescale_ts(stream.time_base(), (1, 1000000)); // rescale to microseconds
                            measure(&timing, Stage::Decode, || decoder.send_packet(&packet))
                                .map_err(|e| decoder_error(self.gpu_decoding, "send_packet", e))?;
                        }

                        match self.video.receive_and_process_video_frames(
                            output_size,
                            bitrate,
                            Some(&mut octx),
                            &mut self.ost_time_bases,
                            start_ms,
                            end_ms,
                            &mut self.frame_ts,
                        ) {
                            Ok(encoding_status) => {
                                if self.video.encoder.is_some() {
                                    video_inited = true;
                                    if !pending_packets.is_empty() {
                                        for (stream, packet, ist_index, ost_index) in
                                            pending_packets.drain(..)
                                        {
                                            let ost_time_base =
                                                self.ost_time_bases[ost_index as usize];
                                            process_stream(
                                                &mut atranscoders,
                                                &mut octx,
                                                stream,
                                                packet,
                                                start_ms,
                                                end_ms,
                                                ist_index,
                                                ost_index,
                                                ost_time_base,
                                                &mut self.frame_ts,
                                            )?;
                                        }
                                    }
                                }
                                if encoding_status == Status::Finish {
                                    encoding_video = false;
                                }
                                while pause_flag.load(Relaxed) {
                                    std::thread::sleep(std::time::Duration::from_millis(100));
                                }
                            }
                            Err(e) => {
                                return Err(frame_processing_error(self.gpu_decoding, e));
                            }
                        }
                    }
                } else if self.audio_codec != codec::Id::None || self.preserve_other_tracks {
                    if encoding_audio {
                        if !video_inited {
                            pending_packets.push((stream, packet, ist_index, ost_index));
                            continue;
                        }
                        let ost_time_base = self.ost_time_bases[ost_index as usize];
                        if process_stream(
                            &mut atranscoders,
                            &mut octx,
                            stream,
                            packet,
                            start_ms,
                            end_ms,
                            ist_index,
                            ost_index,
                            ost_time_base,
                            &mut self.frame_ts,
                        )? {
                            encoding_audio = false;
                        }
                    }
                }
                if (!encoding_video && !encoding_audio) || cancel_flag.load(Relaxed) {
                    break;
                }
            }
            if !self.ranges_ms.is_empty() && !cancel_flag.load(Relaxed) {
                let next_range = self.ranges_ms.remove(0);
                if let Some(start) = next_range.0 {
                    start_ms = Some(start);
                    let position = (start as i64).rescale((1, 1000), rescale::TIME_BASE);
                    self.input_context.seek(position, ..position)?;
                    self.frame_ts.add_video = self.frame_ts.last_video.unwrap_or_default()
                        + self.frame_ts.last_duration_video;
                    self.frame_ts.add_audio = self.frame_ts.last_audio.unwrap_or_default()
                        + self.frame_ts.last_duration_audio;
                    self.frame_ts.first = None;
                }
                end_ms = next_range.1;
                continue;
            } else {
                break;
            }
        }

        // Flush encoders and decoders.
        {
            let ost_time_base = self.ost_time_bases[self.video.output_index.unwrap_or_default()];
            let decoder = self.video
                .decoder
                .as_mut()
                .ok_or(Error::DecoderNotFound)?;
            measure(&timing, Stage::Decode, || decoder.send_eof())
                .map_err(|e| decoder_error(self.gpu_decoding, "send_eof", e))?;
            // self.video.decoder.as_mut().ok_or(Error::DecoderNotFound)?.flush();
            self.video.receive_and_process_video_frames(
                output_size,
                bitrate,
                Some(&mut octx),
                &mut self.ost_time_bases,
                start_ms,
                end_ms,
                &mut self.frame_ts,
            ).map_err(|e| frame_processing_error(self.gpu_decoding, e))?;
            let encoder = self.video
                .encoder
                .as_mut()
                .ok_or(Error::EncoderNotFound)?;
            measure(&timing, Stage::Encode, || encoder.send_eof())?;
            if let Err(e) = self
                .video
                .receive_and_process_encoded_packets(&mut octx, ost_time_base)
            {
                log::error!("Failed to flush last packet: {e:?}");
            }
        }
        if self.audio_codec != codec::Id::None {
            for (ost_index, transcoder) in atranscoders.iter_mut() {
                let ost_time_base = self.ost_time_bases[*ost_index];
                transcoder.flush(
                    &mut octx,
                    ost_time_base,
                    start_ms,
                    end_ms,
                    &mut self.frame_ts,
                )?;
            }
        }

        measure(&timing, Stage::Mux, || octx.write_trailer())?;

        Ok(())
    }

    pub fn start_decoder_only(
        &mut self,
        mut ranges: Vec<(f64, f64)>,
        cancel_flag: Arc<AtomicBool>,
    ) -> Result<(), FFmpegError> {
        let mut start_ms = None;
        let mut end_ms = None;

        if let Some(first_range) = ranges.first() {
            start_ms = Some(first_range.0);
            end_ms = Some(first_range.1);
            let position = (first_range.0 as i64).rescale((1, 1000), rescale::TIME_BASE);
            self.input_context.seek(position, ..position)?;
            ranges.remove(0);
        }

        self.video.decode_only = true;

        for (i, stream) in self.input_context.streams().enumerate() {
            if stream.parameters().medium() == media::Type::Video {
                self.video.input_index = i;

                // TODO this doesn't work for some reason
                // let c_name = CString::new("resize").unwrap();
                // let c_val = CString::new("1280x720").unwrap();
                // unsafe { ffi::av_opt_set((*codec.as_mut_ptr()).priv_data, c_name.as_ptr(), c_val.as_ptr(), 1); }

                self.video.encoder_params.frame_rate =
                    self.video.decoder.as_ref().unwrap().frame_rate();
                self.video.encoder_params.time_base = Some(stream.rate().invert());
                break;
            }
        }

        let mut any_encoded = false;
        loop {
            for (stream, mut packet) in self.input_context.packets() {
                let ist_index = stream.index();

                if ist_index == self.video.input_index {
                    let decoder = self.video.decoder.as_mut().ok_or(Error::DecoderNotFound)?;
                    packet.rescale_ts(stream.time_base(), (1, 1000000)); // rescale to microseconds

                    if let Err(err) = decoder.send_packet(&packet) {
                        ::log::error!("Decoder error {:?}", err);
                        if self.gpu_decoding || !any_encoded || self.video.strict_decode_errors {
                            return Err(decoder_error(self.gpu_decoding, "send_packet", err));
                        }
                    }
                    match self.video.receive_and_process_video_frames(
                        (0, 0),
                        None,
                        None,
                        &mut self.ost_time_bases,
                        start_ms,
                        end_ms,
                        &mut self.frame_ts,
                    ) {
                        Ok(encoding_status) => {
                            any_encoded = true;
                            if encoding_status == Status::Finish || cancel_flag.load(Relaxed) {
                                break;
                            }
                        }
                        Err(e) => {
                            ::log::error!("Encoder error {:?}", e);
                            let e = frame_processing_error(self.gpu_decoding, e);
                            if matches!(e, FFmpegError::GPUDecodingFailed) || !any_encoded || self.video.strict_decode_errors {
                                return Err(e);
                            }
                        }
                    }
                }
            }
            if !ranges.is_empty() && !cancel_flag.load(Relaxed) {
                let next_range = ranges.remove(0);
                let position = (next_range.0 as i64).rescale((1, 1000), rescale::TIME_BASE);
                self.input_context.seek(position, ..position)?;
                // Both bounds must follow the range: a stale start_ms from the
                // first range drops every frame of a range that starts earlier.
                start_ms = Some(next_range.0);
                end_ms = Some(next_range.1);
                continue;
            } else {
                break;
            }
        }

        // Flush decoder.
        self.video
            .decoder
            .as_mut()
            .ok_or(Error::DecoderNotFound)?
            .send_eof()
            .map_err(|e| decoder_error(self.gpu_decoding, "send_eof", e))?;
        self.video.receive_and_process_video_frames(
            (0, 0),
            None,
            None,
            &mut self.ost_time_bases,
            start_ms,
            end_ms,
            &mut self.frame_ts,
        ).map_err(|e| frame_processing_error(self.gpu_decoding, e))?;

        if let Some(step) = &self.video.decode_frame_step {
            ::log::debug!(target: "sync", "[optical] decode sampling: decoded={} retained={} skipped_before_transfer={}",
                step.decoded, step.retained, step.decoded - step.retained);
        }
        Ok(())
    }

    pub fn on_frame<F>(&mut self, cb: F)
    where
        F: FnMut(
                i64,
                &mut frame::Video,
                Option<&mut frame::Video>,
                &mut ffmpeg_video_converter::Converter,
                &mut ffmpeg_video::RateControl,
            ) -> Result<(), FFmpegError>
            + 'a,
    {
        self.video.on_frame_callback = Some(Box::new(cb));
    }
    pub fn on_encoder_initialized<F>(&mut self, cb: F)
    where
        F: FnMut(&encoder::video::Video) -> Result<(), FFmpegError> + 'a,
    {
        self.video.on_encoder_initialized = Some(Box::new(cb));
    }

    pub fn get_video_info(url: &str) -> Result<VideoInfo, ffmpeg_next::Error> {
        let mut file =
            FfmpegPathWrapper::new(url, false).map_err(|_| ffmpeg_next::Error::ProtocolNotFound)?;
        let mut dict = Dictionary::new();
        if file.path.starts_with("fd:") {
            dict.set("fd", &file.path[3..]);
            file.path = "fd:".into();
        }

        let context = format::input_with_dictionary(&file.path, dict)?;
        let created_at = context
            .metadata()
            .get("creation_time")
            .and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok())
            .map(|x| x.timestamp_millis());
        if let Some(stream) = context.streams().best(media::Type::Video) {
            let codec = codec::context::Context::from_parameters(stream.parameters())?;
            if let Ok(video) = codec.decoder().video() {
                let mut bitrate = video.bit_rate();
                if bitrate == 0 {
                    bitrate = context.bit_rate() as usize;
                }

                let mut frames = stream.frames() as usize;
                if frames == 0 {
                    frames = (stream.duration() as f64
                        * f64::from(stream.time_base())
                        * f64::from(stream.rate())) as usize;
                }

                let rotation = {
                    let mut theta = 0.0;
                    if let Some(rotate_tag) = stream.metadata().get("rotate") {
                        if let Ok(num) = rotate_tag.parse::<f64>() {
                            theta = num;
                        }
                    }
                    if theta == 0.0 {
                        for side_data in stream.side_data() {
                            if side_data.kind() == codec::packet::side_data::Type::DisplayMatrix {
                                let display_matrix = side_data.data();
                                if display_matrix.len() == 9 * 4 {
                                    theta = -unsafe {
                                        ffi::av_display_rotation_get(
                                            display_matrix.as_ptr() as *const i32
                                        )
                                    };
                                }
                            }
                        }
                    }

                    theta -= 360.0 * (theta / 360.0 + 0.9 / 360.0).floor();
                    theta as i32
                };

                // Read codec signature directly from stream parameters via FFI
                // so the values reflect the source stream rather than any decoder
                // post-init state. AVCodecParameters fields: codec_id (AVCodecID),
                // format (AVPixelFormat as c_int), profile (c_int).
                let (codec_id, pix_fmt, profile) = unsafe {
                    let params_ptr = stream.parameters().as_ptr();
                    let codec_id = codec::Id::from((*params_ptr).codec_id);
                    let format_raw: i32 = (*params_ptr).format;
                    let pix_fmt = ffmpeg_next::format::Pixel::from(
                        std::mem::transmute::<i32, ffi::AVPixelFormat>(format_raw),
                    );
                    let profile: i32 = (*params_ptr).profile;
                    (codec_id, pix_fmt, profile)
                };

                return Ok(VideoInfo {
                    duration_ms: stream.duration() as f64 * f64::from(stream.time_base()) * 1000.0,
                    frame_count: frames,
                    fps: f64::from(stream.rate()), // or avg_frame_rate?
                    width: video.width(),
                    height: video.height(),
                    bitrate: bitrate as f64 / 1024.0 / 1024.0,
                    rotation,
                    created_at,
                    codec_id,
                    pix_fmt,
                    profile,
                });
            }
        }
        Err(ffmpeg_next::Error::StreamNotFound)
    }
}

#[cfg(test)]
mod decode_fallback_tests {
    use super::*;

    #[test]
    fn decode_fallback_stops_after_software_and_keeps_encoder_errors() {
        assert_eq!(next_decoder_attempt(0, 12, &FFmpegError::GPUDecodingFailed), Some(-1));
        assert_eq!(next_decoder_attempt(-1, 0, &FFmpegError::GPUDecodingFailed), None);
        for stage in ["open", "send_packet", "receive_frame", "send_eof"] {
            assert!(matches!(decoder_error(true, stage, Error::Unknown), FFmpegError::GPUDecodingFailed));
            assert!(matches!(decoder_error(false, stage, Error::Unknown), FFmpegError::InternalError(Error::Unknown)));
        }
        for error in [Error::Eof, Error::Other { errno: ffmpeg_next::util::error::EAGAIN }] {
            assert!(matches!(decoder_error(true, "send_packet", error), FFmpegError::InternalError(_)));
        }
        let encoder_error = FFmpegError::ToHWTransferError(-1);
        assert!(matches!(frame_processing_error(true, encoder_error), FFmpegError::ToHWTransferError(-1)));
        assert!(matches!(frame_processing_error(true, FFmpegError::FromHWTransferError(-1)), FFmpegError::GPUDecodingFailed));
        assert!(matches!(frame_processing_error(false, FFmpegError::InternalError(Error::InvalidData)), FFmpegError::InternalError(Error::InvalidData)));
        let mut index = 0;
        let mut attempts = vec![index];
        while let Some(next) = next_decoder_attempt(index, 0, &FFmpegError::DecoderNotFound) {
            index = next;
            attempts.push(index);
            assert!(attempts.len() <= 6);
        }
        assert_eq!(attempts, [0, 1, 2, 3, 4, -1]);
    }

    fn fixture(dir: &std::path::Path) -> String {
        let path = dir.join("source.y4m");
        let mut data = b"YUV4MPEG2 W32 H24 F25:1 Ip A1:1 C420\n".to_vec();
        for index in 0..8u8 {
            data.extend_from_slice(b"FRAME\n");
            data.extend(vec![16 + index; 32 * 24]);
            data.extend(vec![128; 32 * 24 / 2]);
        }
        std::fs::write(&path, data).unwrap();
        filesystem::path_to_url(path.to_str().unwrap())
    }

    #[test]
    fn decode_fallback_recreates_pipeline_and_replaces_partial_output() {
        let dir = tempfile::tempdir().unwrap();
        let input = fixture(dir.path());
        let folder = filesystem::path_to_url(dir.path().to_str().unwrap());
        let captured = Arc::new(());
        let mut index = 0;
        let mut attempts = 0;
        loop {
            attempts += 1;
            assert!(attempts <= 2);
            // No callback or native codec from the previous attempt may survive here.
            assert_eq!(Arc::strong_count(&captured), 1);
            let (result, frames) = {
                let mut proc = FfmpegProcessor::from_file(&input, false, 0, None).unwrap();
                proc.video_codec = Some("ffv1".into());
                proc.video.gpu_encoding = false;
                proc.video.processing_order = ProcessingOrder::PostConversion;
                proc.audio_codec = codec::Id::None;
                let frames = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let observed = frames.clone();
                let captured = captured.clone();
                proc.on_frame(move |_, input, output, _, _| {
                    let _keep_until_pipeline_drop = &captured;
                    let frame = observed.fetch_add(1, Relaxed);
                    // Exercise the real export cleanup after several frames were encoded.
                    if index == 0 && frame == 3 {
                        return Err(FFmpegError::GPUDecodingFailed);
                    }
                    *output.unwrap() = input.clone();
                    Ok(())
                });
                let result = proc.render(&folder, "result.nut.tmp", (32, 24), None,
                    Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
                (result, frames.load(Relaxed))
            };
            match result {
                Err(ref error) => index = next_decoder_attempt(index, frames, error).unwrap(),
                Ok(()) => break,
            }
        }
        assert_eq!(attempts, 2);
        assert_eq!(Arc::strong_count(&captured), 1);
        let output = filesystem::get_file_url(&folder, "result.nut.tmp", false);
        let mut decoded = Vec::new();
        {
            let mut proc = FfmpegProcessor::from_file(&output, false, 0, None).unwrap();
            proc.video.strict_decode_errors = true;
            proc.on_frame(|ts, frame, _, _, _| {
                decoded.push((ts, frame.data(0)[0]));
                Ok(())
            });
            proc.start_decoder_only(Vec::new(), Arc::new(AtomicBool::new(false))).unwrap();
        }
        assert_eq!(decoded, (0..8).map(|i| (i * 40_000, 16 + i as u8)).collect::<Vec<_>>());
    }

    #[test]
    fn decode_fallback_software_failure_after_frames_is_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let input = fixture(dir.path());
        let folder = filesystem::path_to_url(dir.path().to_str().unwrap());
        let mut proc = FfmpegProcessor::from_file(&input, false, 0, None).unwrap();
        proc.video_codec = Some("ffv1".into());
        proc.video.gpu_encoding = false;
        proc.video.processing_order = ProcessingOrder::PostConversion;
        proc.audio_codec = codec::Id::None;
        let mut frames = 0;
        proc.on_frame(move |_, input, output, _, _| {
            frames += 1;
            if frames == 4 { return Err(Error::InvalidData.into()); }
            *output.unwrap() = input.clone();
            Ok(())
        });
        let error = proc.render(&folder, "failed.nut", (32, 24), None,
            Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false))).unwrap_err();
        assert!(matches!(error, FFmpegError::InternalError(Error::InvalidData)));
        assert_eq!(next_decoder_attempt(-1, 3, &error), None);
    }

    #[test]
    fn rendered_sample_aspect_ratio_override_preserves_pixels_and_timestamps() {
        let dir = tempfile::tempdir().unwrap();
        let input = fixture(dir.path());
        let path = dir.path().join("source.y4m");
        let data = std::fs::read(&path).unwrap();
        let header_end = data.iter().position(|&x| x == b'\n').unwrap();
        let mut anamorphic = b"YUV4MPEG2 W32 H24 F25:1 Ip A133:100 C420\n".to_vec();
        anamorphic.extend_from_slice(&data[header_end + 1..]);
        std::fs::write(path, anamorphic).unwrap();
        let folder = filesystem::path_to_url(dir.path().to_str().unwrap());

        for order in [ProcessingOrder::PreConversion, ProcessingOrder::PostConversion] {
            for aspect in [None, Some(Rational(1, 1))] {
                let expected = aspect.unwrap_or(Rational(133, 100));
                {
                    let mut proc = FfmpegProcessor::from_file(&input, false, 0, None).unwrap();
                    proc.video_codec = Some("libx264".into());
                    proc.video.gpu_encoding = false;
                    proc.video.processing_order = if order == ProcessingOrder::PreConversion {
                        ProcessingOrder::PreConversion
                    } else { ProcessingOrder::PostConversion };
                    proc.video.encoder_params.sample_aspect_ratio = aspect;
                    proc.video.encoder_params.options.set("crf", "0");
                    proc.video.encoder_params.options.set("preset", "ultrafast");
                    proc.audio_codec = codec::Id::None;
                    proc.on_frame(|_, input, output, _, _| {
                        // The raw Y4M decoder omits frame SAR; model the tagged camera frame.
                        unsafe { (*input.as_mut_ptr()).sample_aspect_ratio = Rational(133, 100).into(); }
                        *output.unwrap() = input.clone();
                        Ok(())
                    });
                    proc.render(&folder, "aspect.mov", (32, 24), None,
                        Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false))).unwrap();
                }
                let output = filesystem::get_file_url(&folder, "aspect.mov", false);
                let mut frames = Vec::new();
                {
                    let mut proc = FfmpegProcessor::from_file(&output, false, 0, None).unwrap();
                    let stream = proc.input_context.streams().best(media::Type::Video).unwrap();
                    let stored_aspect = unsafe { Rational::from((*stream.parameters().as_ptr()).sample_aspect_ratio) };
                    assert_eq!(stored_aspect, expected);
                    proc.on_frame(|ts, frame, _, _, _| {
                        assert_eq!((frame.width(), frame.height()), (32, 24));
                        frames.push((ts, frame.data(0)[0]));
                        Ok(())
                    });
                    proc.start_decoder_only(Vec::new(), Arc::new(AtomicBool::new(false))).unwrap();
                }
                assert_eq!(frames, (0..8).map(|i| (i * 40_000, 16 + i as u8)).collect::<Vec<_>>());
            }
        }
    }
}

/* unsafe extern "C" fn get_hw_format(ctx: *mut ffi::AVCodecContext, pix_fmts: *const ffi::AVPixelFormat) -> ffi::AVPixelFormat {
    let mut i = 0;
    loop {
        let p = *pix_fmts.offset(i);
        if p == ffi::AVPixelFormat::AV_PIX_FMT_NONE {
            break;
        }
        if p == hw_format {
            return p;
        }
        i += 1;
    }

    ::log::error!("Failed to get HW surface format.");
    ffi::AVPixelFormat::AV_PIX_FMT_NONE
} */
