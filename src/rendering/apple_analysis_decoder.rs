// SPDX-License-Identifier: GPL-3.0-or-later
//! Apple optical-analysis input: asynchronous VT decoding, original sampling and scaler.
use super::{FFmpegError, ffmpeg_video::RateControl, ffmpeg_video_converter::Converter};
use ffmpeg_next::{ffi, format::Pixel, frame};
use ffmpeg_next::rescale::Rescale;
use std::{ffi::{CString, c_char, c_void}, sync::{Arc, atomic::{AtomicBool, Ordering::Relaxed}}};

type Callback = dyn FnMut(i64, &mut frame::Video, Option<&mut frame::Video>, &mut Converter, &mut RateControl) -> Result<(), FFmpegError>;
#[repr(C)]
struct Range { from: f64, to: f64 }
unsafe extern "C" {
    fn niyien_apple_analysis_read(path: *const c_char, ranges: *const Range, count: usize,
        select: unsafe extern "C" fn(*mut c_void, i64) -> i32,
        receive: unsafe extern "C" fn(*mut c_void, i64, u32, u32, *const c_char, *const *const u8, *const i32) -> bool,
        cancelled: unsafe extern "C" fn(*mut c_void) -> bool, user: *mut c_void) -> i32;
}

pub struct AppleAnalysisDecoder {
    url: String,
    time_base: ffmpeg_next::Rational,
    retain_code: i32,
    size: (u32, u32),
    pub sampling: (usize, f64),
    pub callback: Option<Box<Callback>>,
}
impl AppleAnalysisDecoder {
    pub fn new(url: &str) -> Result<Self, FFmpegError> {
        let path = gyroflow_core::filesystem::FfmpegPathWrapper::new(url, false)
            .map_err(|_| FFmpegError::AsyncDecodingFailed)?;
        let input = ffmpeg_next::format::input(&path.path)?;
        let stream = input.streams().find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
            .ok_or(FFmpegError::AsyncDecodingFailed)?;
        let parameters = stream.parameters();
        let size = unsafe { ((*parameters.as_ptr()).width, (*parameters.as_ptr()).height) };
        if size.0 <= 0 || size.1 <= 0 { return Err(FFmpegError::AsyncDecodingFailed); }
        #[cfg(target_os = "ios")]
        let retain_code = unsafe {
            let pixel = ffi::av_pix_fmt_desc_get(std::mem::transmute::<i32, ffi::AVPixelFormat>((*parameters.as_ptr()).format));
            if pixel.is_null() { return Err(FFmpegError::AsyncDecodingFailed); }
            match (*pixel).comp[0].depth {
                8 => 1,
                10 => 2,
                _ => return Err(FFmpegError::AsyncDecodingFailed),
            }
        };
        #[cfg(not(target_os = "ios"))]
        let retain_code = 1;
        Ok(Self { url: url.into(), time_base: stream.time_base(), retain_code, size: (size.0 as u32, size.1 as u32), sampling: (1, 0.0), callback: None })
    }
    pub fn start(&mut self, ranges: Vec<(f64, f64)>, cancel: Arc<AtomicBool>) -> Result<(), FFmpegError> {
        let path = CString::new(gyroflow_core::filesystem::url_to_path(&self.url)).map_err(|_| FFmpegError::GPUDecodingFailed)?;
        struct Access<'a>(&'a str);
        impl Drop for Access<'_> { fn drop(&mut self) { gyroflow_core::filesystem::stop_accessing_url(self.0, false); } }
        gyroflow_core::filesystem::start_accessing_url(&self.url, false);
        let _access = Access(&self.url);
        let ranges: Vec<_> = ranges.into_iter().map(|(from, to)| Range { from, to }).collect();
        let mut context = Context { callback: self.callback.take(), cancel, sampling: self.sampling, time_base: self.time_base, retain_code: self.retain_code, size: self.size,
            converter: Converter::default(), frame: None, error: None, decoded: 0, retained: 0 };
        ::log::info!(target: "video.codec", "Optical analysis decoder: asynchronous VideoToolbox (MDK)");
        let result = unsafe { niyien_apple_analysis_read(path.as_ptr(), ranges.as_ptr(), ranges.len(),
            select, receive, cancelled, &mut context as *mut _ as *mut c_void) };
        ::log::debug!(target: "sync", "[optical] async VT sampling: decoded={} retained={}", context.decoded, context.retained);
        if let Some(error) = context.error { return Err(error); }
        if result != 0 {
            ::log::warn!(target: "video.codec", "Asynchronous VT analysis decode failed: code={result}");
            return Err(FFmpegError::GPUDecodingFailed);
        }
        Ok(())
    }
}
struct Context {
    callback: Option<Box<Callback>>,
    cancel: Arc<AtomicBool>,
    sampling: (usize, f64),
    time_base: ffmpeg_next::Rational,
    retain_code: i32,
    size: (u32, u32),
    converter: Converter,
    frame: Option<frame::Video>,
    error: Option<FFmpegError>,
    decoded: usize,
    retained: usize,
}
unsafe extern "C" fn cancelled(user: *mut c_void) -> bool {
    unsafe { &*(user as *const Context) }.cancel.load(Relaxed)
}
unsafe extern "C" fn select(user: *mut c_void, us: i64) -> i32 {
    let ctx = unsafe { &mut *(user as *mut Context) };
    if ctx.cancel.load(Relaxed) { return -1; }
    ctx.decoded += 1;
    let us = normalize_timestamp(us, ctx.time_base);
    if gyroflow_core::synchronization::optical_sampling::keep_frame(us, ctx.sampling.1, ctx.sampling.0) { ctx.retain_code } else { 0 }
}
fn normalize_timestamp(us: i64, time_base: ffmpeg_next::Rational) -> i64 {
    // MDK truncates fractional microseconds. Recover the container's timestamp
    // tick before applying the same microsecond rounding as packet.rescale_ts.
    us.rescale((1, 1_000_000), time_base).rescale(time_base, (1, 1_000_000))
}
unsafe extern "C" fn receive(user: *mut c_void, us: i64, width: u32, height: u32,
    format: *const c_char, planes: *const *const u8, strides: *const i32) -> bool {
    let ctx = unsafe { &mut *(user as *mut Context) };
    let us = normalize_timestamp(us, ctx.time_base);
    // C++ keeps every source plane alive until this callback returns. Copy into
    // refcounted FFmpeg buffers before the threaded scaler consumes them.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), FFmpegError> {
        if format.is_null() || width == 0 || height == 0 || width > 32768 || height > 32768 {
            return Err(FFmpegError::GPUDecodingFailed);
        }
        // VT can expose coded dimensions (e.g. 1088 rows for a 1080-row video).
        // Exclude alignment padding before scaling, as FFmpeg's decoder does.
        let (visible_width, visible_height) = ctx.size;
        if width < visible_width || height < visible_height
            || width - visible_width >= 64 || height - visible_height >= 64 {
            return Err(FFmpegError::GPUDecodingFailed);
        }
        let (width, height) = (visible_width, visible_height);
        let pixel = Pixel::from(unsafe { ffi::av_get_pix_fmt(format) });
        if pixel == Pixel::None { return Err(FFmpegError::GPUDecodingFailed); }
        if ctx.frame.as_ref().is_none_or(|f| f.width() != width || f.height() != height || f.format() != pixel) {
            ctx.frame = Some(frame::Video::new(pixel, width, height));
        }
        let frame = ctx.frame.as_mut().unwrap();
        if frame.planes() > 4 { return Err(FFmpegError::GPUDecodingFailed); }
        for plane in 0..frame.planes() {
            let source = unsafe { *planes.add(plane) };
            let source_stride = unsafe { *strides.add(plane) };
            let row_bytes = unsafe { ffi::av_image_get_linesize(pixel.into(), width as i32, plane as i32) };
            if source.is_null() || row_bytes <= 0 || source_stride < row_bytes { return Err(FFmpegError::GPUDecodingFailed); }
            let rows = frame.plane_height(plane) as usize;
            let dest_stride = frame.stride(plane);
            for row in 0..rows {
                let src = unsafe { std::slice::from_raw_parts(source.add(row * source_stride as usize), row_bytes as usize) };
                frame.data_mut(plane)[row * dest_stride..row * dest_stride + row_bytes as usize].copy_from_slice(src);
            }
        }
        ctx.retained += 1;
        if let Some(callback) = &mut ctx.callback { callback(us, frame, None, &mut ctx.converter, &mut RateControl::default())?; }
        Ok(())
    }));
    match result {
        Ok(Ok(())) => !ctx.cancel.load(Relaxed),
        Ok(Err(error)) => { ctx.error = Some(error); false },
        Err(_) => { ctx.error = Some(FFmpegError::GPUDecodingFailed); false },
    }
}
