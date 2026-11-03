// SPDX-License-Identifier: GPL-3.0-or-later
//! Portable Sony X-OCN H preview and analysis reader.

use std::ffi::{CString, c_char};

#[repr(C)]
#[derive(Default)]
struct Info {
    width: u32,
    height: u32,
    rate_num: u32,
    rate_den: u32,
    frames: u64,
    duration_ms: f64,
    bitrate: f64,
    pixel_aspect: f64,
}
unsafe extern "C" {
    fn niyien_register_xocn_reader();
    fn niyien_xocn_info(url: *const c_char, info: *mut Info) -> i32;
    fn niyien_xocn_read(
        source: *const c_char,
        ranges: *const Range,
        count: usize,
        receive: unsafe extern "C" fn(*mut std::ffi::c_void, f64, u32, u32, u32, *const u8) -> bool,
        cancelled: unsafe extern "C" fn(*mut std::ffi::c_void) -> bool,
        user: *mut std::ffi::c_void,
    ) -> i32;
}

#[repr(C)]
struct Range {
    from: f64,
    to: f64,
}

pub fn is_source(url: &str) -> bool {
    if !crate::core::filesystem::get_filename(url)
        .to_ascii_lowercase()
        .ends_with(".mxf")
    {
        return false;
    }
    let Ok(access) = crate::core::filesystem::FfmpegPathWrapper::new(url, false) else {
        return false;
    };
    let Ok(source) = CString::new(access.path.as_str()) else {
        return false;
    };
    let supported = unsafe { niyien_xocn_info(source.as_ptr(), std::ptr::null_mut()) == 0 };
    supported
}

pub fn decoder_for_url(url: &str) -> Option<String> {
    if !is_source(url) {
        return None;
    }
    static REGISTER: std::sync::Once = std::sync::Once::new();
    REGISTER.call_once(|| {
        crate::xocn_gpu::register();
        unsafe { niyien_register_xocn_reader() };
    });
    Some("XOCN".into())
}

pub fn video_info(
    url: &str,
) -> Result<crate::rendering::ffmpeg_processor::VideoInfo, ffmpeg_next::Error> {
    let access = crate::core::filesystem::FfmpegPathWrapper::new(url, false)
        .map_err(|_| ffmpeg_next::Error::InvalidData)?;
    let source = CString::new(access.path.as_str()).map_err(|_| ffmpeg_next::Error::InvalidData)?;
    let mut info = Info::default();
    let result = unsafe { niyien_xocn_info(source.as_ptr(), &mut info) };
    if result != 0 {
        return Err(ffmpeg_next::Error::InvalidData);
    }
    let fps = f64::from(info.rate_num) / f64::from(info.rate_den);
    Ok(crate::rendering::ffmpeg_processor::VideoInfo {
        width: info.width,
        height: info.height,
        fps,
        base_fps: fps,
        frame_count: info.frames as usize,
        duration_ms: info.duration_ms,
        bitrate: info.bitrate,
        pix_fmt: ffmpeg_next::format::Pixel::RGBA,
        ..Default::default()
    })
}

pub fn process<F>(
    url: &str,
    ranges: Vec<(f64, f64)>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
    callback: F,
) -> Result<(), crate::rendering::FFmpegError>
where
    F: FnMut(i64, &mut ffmpeg_next::frame::Video) -> Result<(), crate::rendering::FFmpegError>,
{
    use crate::rendering::FFmpegError;
    struct Context<F> {
        callback: F,
        cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
        error: Option<FFmpegError>,
    }
    unsafe extern "C" fn cancelled<F>(user: *mut std::ffi::c_void) -> bool {
        unsafe { &*(user as *const Context<F>) }
            .cancel
            .load(std::sync::atomic::Ordering::Relaxed)
    }
    unsafe extern "C" fn receive<F>(
        user: *mut std::ffi::c_void,
        milliseconds: f64,
        width: u32,
        height: u32,
        stride: u32,
        pixels: *const u8,
    ) -> bool
    where
        F: FnMut(i64, &mut ffmpeg_next::frame::Video) -> Result<(), FFmpegError>,
    {
        let context = unsafe { &mut *(user as *mut Context<F>) };
        let mut frame =
            ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::RGBA, width, height);
        let input =
            unsafe { std::slice::from_raw_parts(pixels, stride as usize * height as usize) };
        let output_stride = frame.stride(0);
        for y in 0..height as usize {
            frame.data_mut(0)[y * output_stride..y * output_stride + width as usize * 4]
                .copy_from_slice(
                    &input[y * stride as usize..y * stride as usize + width as usize * 4],
                );
        }
        match (context.callback)((milliseconds * 1000.0).round() as i64, &mut frame) {
            Ok(()) => !context.cancel.load(std::sync::atomic::Ordering::Relaxed),
            Err(error) => {
                context.error = Some(error);
                false
            }
        }
    }
    let access = crate::core::filesystem::FfmpegPathWrapper::new(url, false)
        .map_err(|error| FFmpegError::CannotOpenInputFile((url.to_owned(), error)))?;
    let source = CString::new(access.path.as_str()).map_err(|_| ffmpeg_next::Error::InvalidData)?;
    let ranges: Vec<Range> = ranges
        .into_iter()
        .map(|(from, to)| Range { from, to })
        .collect();
    let mut context = Context {
        callback,
        cancel,
        error: None,
    };
    crate::xocn_gpu::register();
    let result = unsafe {
        niyien_xocn_read(
            source.as_ptr(),
            ranges.as_ptr(),
            ranges.len(),
            receive::<F>,
            cancelled::<F>,
            &mut context as *mut _ as *mut std::ffi::c_void,
        )
    };
    if let Some(error) = context.error {
        return Err(error);
    }
    if result != 0 {
        return Err(ffmpeg_next::Error::InvalidData.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unrelated_extensions_keep_the_existing_readers() {
        for url in [
            "file:///clip.mp4",
            "file:///clip.mov",
            "file:///clip.crm",
            "file:///clip.braw",
            "file:///clip.r3d",
            "file:///clip.nev",
        ] {
            assert!(!is_source(url), "{url}");
        }
    }

    #[test]
    fn mxf_routing_requires_sony_raw_and_the_h_picture_magic() {
        fn element(key: &[u8], payload: &[u8], output: &mut Vec<u8>) {
            output.extend_from_slice(key);
            output.push(0x83);
            output.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
            output.extend_from_slice(payload);
        }
        let directory = tempfile::tempdir().unwrap();
        let descriptor_key = [6, 14, 43, 52, 2, 83, 1, 1, 13, 1, 1, 1, 1, 1, 39, 0];
        let picture_key = [6, 14, 43, 52, 1, 2, 1, 6, 14, 6, 13, 3, 25, 1, 69, 0];
        let mut descriptor = Vec::new();
        let coding = [6, 14, 43, 52, 4, 1, 1, 6, 14, 6, 4, 1, 2, 6, 9, 1];
        descriptor.extend_from_slice(&[0x32, 1, 0, 16]);
        descriptor.extend_from_slice(&coding);
        for (tag, value) in [
            (0x3202u16, 3344u32),
            (0x3203, 5016),
            (0x3208, 3328),
            (0x3209, 4992),
            (0x320a, 12),
            (0x320b, 8),
        ] {
            descriptor.extend_from_slice(&tag.to_be_bytes());
            descriptor.extend_from_slice(&[0, 4]);
            descriptor.extend_from_slice(&value.to_be_bytes());
        }
        descriptor.extend_from_slice(&[0x30, 1, 0, 8]);
        descriptor.extend_from_slice(&50u32.to_be_bytes());
        descriptor.extend_from_slice(&1u32.to_be_bytes());
        let mut picture = vec![0u8; 128];
        picture[..16]
            .copy_from_slice(&[83, 66, 82, 104, 16, 0, 0, 1, 19, 152, 6, 136, 61, 0, 110, 0]);
        for (name, sony, magic, expected) in [
            ("sony-h.mxf", true, b'h', true),
            ("ordinary.mxf", false, b'h', false),
            ("old-sony.mxf", true, b'g', false),
        ] {
            let mut fields = descriptor.clone();
            if !sony {
                fields[12] = 4;
            }
            picture[3] = magic;
            let mut bytes = Vec::new();
            element(&descriptor_key, &fields, &mut bytes);
            element(&picture_key, &picture, &mut bytes);
            let path = directory.path().join(name);
            std::fs::write(&path, bytes).unwrap();
            let url = crate::core::filesystem::path_to_url(&path.to_string_lossy());
            assert_eq!(is_source(&url), expected, "{name}");
        }
    }

    #[test]
    #[ignore = "Requires GYROFLOW_XOCN_TEST_FILE containing the original FX5 sample"]
    fn reads_real_fx5_source_info() {
        let path = std::env::var("GYROFLOW_XOCN_TEST_FILE").expect("FX5 sample");
        let url = crate::core::filesystem::path_to_url(&path);
        assert_eq!(decoder_for_url(&url).as_deref(), Some("XOCN"));
        let info = video_info(&url).unwrap();
        assert_eq!(
            (info.width, info.height, info.frame_count),
            (4992, 3328, 352)
        );
        assert_eq!((info.fps, info.duration_ms), (50.0, 7040.0));
    }

    #[test]
    #[ignore = "Requires GYROFLOW_XOCN_TEST_FILE containing the original FX5 sample"]
    fn real_fx5_ranges_and_cancellation_use_the_native_reader() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let path = std::env::var("GYROFLOW_XOCN_TEST_FILE").expect("FX5 sample");
        let url = crate::core::filesystem::path_to_url(&path);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut timestamps = Vec::new();
        process(
            &url,
            vec![(0.0, 0.0), (500.0, 540.0), (7000.0, 7020.0)],
            cancel.clone(),
            |time, frame| {
                assert_eq!((frame.width(), frame.height()), (1620, 1080));
                assert!(frame.data(0).iter().any(|value| *value != 0));
                timestamps.push(time);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(timestamps, [0, 500000, 520000, 540000, 7000000, 7020000]);
        cancel.store(true, Ordering::Relaxed);
        process(&url, vec![], cancel.clone(), |_, _| {
            panic!("cancelled source produced a frame")
        })
        .unwrap();
        cancel.store(false, Ordering::Relaxed);
        let after_first = cancel.clone();
        let mut count = 0;
        process(&url, vec![], cancel.clone(), |_, _| {
            count += 1;
            after_first.store(true, Ordering::Relaxed);
            Ok(())
        })
        .unwrap();
        assert_eq!(count, 1);
        cancel.store(false, Ordering::Relaxed);
        let mut restarted = Vec::new();
        process(&url, vec![(20.0, 40.0)], cancel, |time, _| {
            restarted.push(time);
            Ok(())
        })
        .unwrap();
        assert_eq!(restarted, [20000, 40000]);
    }
}
