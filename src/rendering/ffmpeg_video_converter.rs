// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use crate::rendering::FFmpegError;
use ffmpeg_next::{ffi, format, frame, software};

#[derive(Default)]
pub struct Converter {
    pub convert_to: Option<software::scaling::Context>,
    pub convert_from: Option<software::scaling::Context>,
    pub sw_frame_converted: Option<frame::Video>,
    pub sw_frame_converted_out: Option<frame::Video>,
    /// `scale_threaded`'s scaler and output
    pub threaded: Option<(ThreadedScaler, frame::Video)>,
}

/// A scaler set up like `software::scaling::Context::get`, that splits each frame over threads: a context initialized
/// with `threads` scales slices of the output rows side by side in `sws_scale_frame`. Each row is computed as without
/// threads, so the output is the same, bit for bit
pub struct ThreadedScaler {
    ptr: *mut ffi::SwsContext,
}

impl ThreadedScaler {
    pub fn get(src_format: format::Pixel, src_w: u32, src_h: u32, dst_format: format::Pixel, dst_w: u32, dst_h: u32,
               flags: software::scaling::Flags, threads: i32) -> Result<Self, FFmpegError> {
        unsafe {
            let ptr = ffi::sws_alloc_context();
            if ptr.is_null() { return Err(ffmpeg_next::Error::InvalidData.into()); }
            let scaler = Self { ptr };
            // What `sws_getContext` sets, and the threads
            (*ptr).flags = flags.bits() as _;
            (*ptr).src_w = src_w as _;
            (*ptr).src_h = src_h as _;
            (*ptr).src_format = ffi::AVPixelFormat::from(src_format) as _;
            (*ptr).dst_w = dst_w as _;
            (*ptr).dst_h = dst_h as _;
            (*ptr).dst_format = ffi::AVPixelFormat::from(dst_format) as _;
            (*ptr).threads = threads;
            let ret = ffi::sws_init_context(ptr, std::ptr::null_mut(), std::ptr::null_mut());
            if ret < 0 { return Err(ffmpeg_next::Error::from(ret).into()); }
            Ok(scaler)
        }
    }

    pub fn run(&mut self, input: &frame::Video, output: &mut frame::Video) -> Result<(), FFmpegError> {
        let ret = unsafe { ffi::sws_scale_frame(self.ptr, output.as_mut_ptr(), input.as_ptr()) };
        if ret < 0 { return Err(ffmpeg_next::Error::from(ret).into()); }
        Ok(())
    }
}

impl Drop for ThreadedScaler {
    fn drop(&mut self) {
        unsafe { ffi::sws_freeContext(self.ptr); }
    }
}
impl<'a> Converter {
    pub fn convert_pixel_format<F>(
        &mut self,
        frame: &mut frame::Video,
        out_frame: &mut frame::Video,
        format: format::Pixel,
        interpolation: software::scaling::flag::Flags,
        mut cb: F,
    ) -> Result<(), FFmpegError>
    where
        F: FnMut(&mut frame::Video, &mut frame::Video) + 'a,
    {
        if frame.format() != format {
            if self.sw_frame_converted.is_none() {
                self.sw_frame_converted =
                    Some(frame::Video::new(format, frame.width(), frame.height()));
                //self.convert_from = Some(software::converter((frame.width(), frame.height()), frame.format(), format)?);
                self.convert_from = Some(software::scaling::Context::get(
                    frame.format(), // input
                    frame.width(),
                    frame.height(),
                    format, // output
                    frame.width(),
                    frame.height(),
                    interpolation,
                )?);
            }

            if self.sw_frame_converted_out.is_none() {
                self.sw_frame_converted_out = Some(frame::Video::new(
                    format,
                    out_frame.width(),
                    out_frame.height(),
                ));
                //self.convert_to = Some(software::converter((out_frame.width(), out_frame.height()), format, out_frame.format())?);
                self.convert_to = Some(software::scaling::Context::get(
                    format, // input
                    out_frame.width(),
                    out_frame.height(),
                    out_frame.format(), // output
                    out_frame.width(),
                    out_frame.height(),
                    interpolation,
                )?);
            }

            let sw_frame_converted = self
                .sw_frame_converted
                .as_mut()
                .ok_or(FFmpegError::FrameEmpty)?;
            let sw_frame_converted_out = self
                .sw_frame_converted_out
                .as_mut()
                .ok_or(FFmpegError::FrameEmpty)?;
            let convert_from = self
                .convert_from
                .as_mut()
                .ok_or(FFmpegError::ConverterEmpty)?;
            let convert_to = self
                .convert_to
                .as_mut()
                .ok_or(FFmpegError::ConverterEmpty)?;

            convert_from.run(frame, sw_frame_converted)?;

            cb(sw_frame_converted, sw_frame_converted_out);

            convert_to.run(sw_frame_converted_out, out_frame)?;
        } else {
            cb(frame, out_frame);
        }
        Ok(())
    }

    // Scale is only used for autosync
    pub fn scale(
        &mut self,
        frame: &mut frame::Video,
        format: format::Pixel,
        width: u32,
        height: u32,
    ) -> Result<frame::Video, FFmpegError> {
        if frame.width() != width || frame.height() != height || frame.format() != format {
            if self.sw_frame_converted.is_none() {
                self.sw_frame_converted = Some(frame::Video::new(format, width, height));
                self.convert_to = Some(software::scaling::Context::get(
                    frame.format(),
                    frame.width(),
                    frame.height(),
                    format,
                    width,
                    height,
                    software::scaling::Flags::BILINEAR,
                )?);
            }

            let sw_frame_converted = self
                .sw_frame_converted
                .as_mut()
                .ok_or(FFmpegError::FrameEmpty)?;
            let convert_to = self
                .convert_to
                .as_mut()
                .ok_or(FFmpegError::ConverterEmpty)?;

            convert_to.run(frame, sw_frame_converted)?;

            Ok(unsafe { frame::Video::wrap(ffi::av_frame_clone(sw_frame_converted.as_ptr())) })
        } else {
            Ok(unsafe { frame::Video::wrap(ffi::av_frame_clone(frame.as_ptr())) })
        }
    }

    /// `scale`, with each frame split over `threads` threads: the same pixels, see `ThreadedScaler`
    pub fn scale_threaded(
        &mut self,
        frame: &mut frame::Video,
        format: format::Pixel,
        width: u32,
        height: u32,
        threads: i32,
    ) -> Result<frame::Video, FFmpegError> {
        if frame.width() != width || frame.height() != height || frame.format() != format {
            if self.threaded.is_none() {
                let scaler = ThreadedScaler::get(frame.format(), frame.width(), frame.height(), format, width, height,
                    software::scaling::Flags::BILINEAR, threads)?;
                self.threaded = Some((scaler, frame::Video::new(format, width, height)));
            }
            let (scaler, converted) = self.threaded.as_mut().ok_or(FFmpegError::ConverterEmpty)?;
            scaler.run(frame, converted)?;
            Ok(unsafe { frame::Video::wrap(ffi::av_frame_clone(converted.as_ptr())) })
        } else {
            Ok(unsafe { frame::Video::wrap(ffi::av_frame_clone(frame.as_ptr())) })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn random_frame(pixel: format::Pixel, w: u32, h: u32, mut seed: u64) -> frame::Video {
        let mut frame = frame::Video::new(pixel, w, h);
        for plane in 0..frame.planes() {
            for byte in frame.data_mut(plane).iter_mut() {
                seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
                *byte = seed as u8;
            }
        }
        frame
    }

    #[test]
    fn threaded_scaling_matches_the_single_threaded_scaler() {
        use format::Pixel::*;
        for (pixel, w, h) in [(P010LE, 3840, 2160), (YUV420P, 1920, 1080), (NV12, 1920, 1080), (YUV422P10LE, 3840, 2160), (YUV420P10LE, 1280, 720), (GRAY8, 1920, 1080)] {
            let (tw, th) = (960, (((h as f64 * 960.0 / w as f64) / 2.0).round() * 2.0) as u32);
            for threads in [2, crate::rendering::ANALYSIS_SCALE_THREADS, 8] {
                let (mut single, mut threaded) = (Converter::default(), Converter::default());
                // Several frames through the same converters: their state carries over
                for seed in 1..4 {
                    let mut input = random_frame(pixel, w, h, (seed as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
                    let a = single.scale(&mut input, GRAY8, tw, th).unwrap();
                    let b = threaded.scale_threaded(&mut input, GRAY8, tw, th, threads).unwrap();
                    assert_eq!((a.width(), a.height(), a.format()), (b.width(), b.height(), b.format()));
                    for y in 0..th as usize {
                        let row = |f: &frame::Video| f.data(0)[y * f.stride(0)..][..tw as usize].to_vec();
                        assert_eq!(row(&a), row(&b), "{pixel:?} {w}x{h} threads {threads} frame {seed} row {y}");
                    }
                }
            }
        }
    }
}
