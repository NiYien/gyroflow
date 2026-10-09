// SPDX-License-Identifier: GPL-3.0-or-later
//! Desktop Canon CRM preview/synchronization decoder. Video export is disabled separately.

pub const fn available() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
}

pub fn decoder_for_url(url: &str) -> Option<String> {
    if !available()
        || !crate::core::filesystem::get_filename(url)
            .to_ascii_lowercase()
            .ends_with(".crm")
    {
        return None;
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    {
        unsafe extern "C" {
            fn niyien_register_crm_decoder();
            fn niyien_crm_set_logger(callback: unsafe extern "C" fn(*const std::ffi::c_char));
        }
        unsafe extern "C" fn native_log(message: *const std::ffi::c_char) {
            if message.is_null() {
                return;
            }
            let message = unsafe { std::ffi::CStr::from_ptr(message) }.to_string_lossy();
            ::log::info!(target: "video.load", "{message}");
            #[cfg(test)]
            eprintln!("{message}");
        }
        static REGISTER: std::sync::Once = std::sync::Once::new();
        REGISTER.call_once(|| unsafe {
            niyien_crm_set_logger(native_log);
            niyien_register_crm_decoder();
        });
    }
    let path = crate::core::filesystem::url_to_path(url);
    if path.is_empty() {
        return None;
    }
    // Hex keeps colons, Unicode, and decoder option separators out of the MDK grammar.
    use std::fmt::Write;
    let mut encoded = String::with_capacity(path.len() * 2);
    for byte in path.as_bytes() {
        let _ = write!(encoded, "{byte:02x}");
    }
    Some(format!("CRM:source_hex={encoded}"))
}

pub fn configure_player(player: &mut qml_video_rs::video_item::MDKVideoItem, url: &str) {
    let current = crate::util::qurl_to_encoded(player.url.clone());
    let decoder = decoder_for_url(url);
    let is_crm = decoder.is_some();
    if is_crm
        || crate::core::filesystem::get_filename(&current)
            .to_ascii_lowercase()
            .ends_with(".crm")
    {
        let buffer = if is_crm {
            // Canon stores audio in chunks ahead of video; less than one chunk
            // can stall a paused prepare before the first video packet arrives.
            "0+1000"
        } else if url.starts_with("http://") || url.starts_with("https://") {
            "-1"
        } else {
            "0+4000"
        };
        player.setDefaultProperty("buffer".into(), buffer.into());
        // Restore the player's own defaults, including explicit decoder overrides.
        let decoders = decoder.map(qmetaobject::QString::from)
            .unwrap_or_else(|| player.defaultVideoDecoders());
        player.setDefaultProperty(
            "video.decoders".into(),
            decoders,
        );
    }
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
mod native {
    use crate::rendering::{FFmpegError, ffmpeg_processor::VideoInfo};
    use std::{
        ffi::{CString, c_char, c_void},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };
    #[repr(C)]
    pub struct Info {
        duration: f64,
        fps: f64,
        bitrate: f64,
        frames: u64,
        width: u32,
        height: u32,
        rotation: i32,
        created: [u8; 64],
    }
    #[repr(C)]
    pub struct Range {
        from: f64,
        to: f64,
    }
    type FrameCallback = unsafe extern "C" fn(*mut c_void, f64, u32, u32, u32, *const u8) -> bool;
    unsafe extern "C" {
        fn niyien_crm_read(
            path: *const c_char,
            decoder: *const c_char,
            info: *mut Info,
            ranges: *const Range,
            count: usize,
            frame: Option<FrameCallback>,
            cancelled: Option<unsafe extern "C" fn(*mut c_void) -> bool>,
            user: *mut c_void,
        ) -> i32;
    }
    struct Access(String);
    impl Drop for Access {
        fn drop(&mut self) {
            crate::core::filesystem::stop_accessing_url(&self.0, false);
        }
    }
    fn prepare(url: &str) -> Result<(CString, CString, Access), FFmpegError> {
        let decoder = super::decoder_for_url(url).ok_or(FFmpegError::DecoderNotFound)?;
        let path = CString::new(crate::core::filesystem::url_to_path(url))
            .map_err(|_| FFmpegError::DecoderNotFound)?;
        crate::core::filesystem::start_accessing_url(url, false);
        Ok((path, CString::new(decoder).unwrap(), Access(url.into())))
    }
    pub fn video_info(url: &str) -> Result<VideoInfo, ffmpeg_next::Error> {
        let (path, decoder, _access) =
            prepare(url).map_err(|_| ffmpeg_next::Error::DecoderNotFound)?;
        let mut info: Info = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            niyien_crm_read(
                path.as_ptr(),
                decoder.as_ptr(),
                &mut info,
                std::ptr::null(),
                0,
                None,
                None,
                std::ptr::null_mut(),
            )
        };
        if rc != 0 || info.width == 0 || info.fps <= 0.0 {
            return Err(ffmpeg_next::Error::InvalidData);
        }
        let created = std::str::from_utf8(&info.created)
            .unwrap_or("")
            .trim_end_matches('\0');
        Ok(VideoInfo {
            width: info.width,
            height: info.height,
            frame_count: info.frames as usize,
            duration_ms: info.duration,
            fps: info.fps,
            bitrate: info.bitrate,
            rotation: info.rotation,
            created_at: chrono::DateTime::parse_from_rfc3339(created)
                .ok()
                .map(|d| d.timestamp_millis()),
            ..VideoInfo::default()
        })
    }
    pub fn process<F>(
        url: &str,
        ranges: Vec<(f64, f64)>,
        cancel: Arc<AtomicBool>,
        callback: F,
    ) -> Result<(), FFmpegError>
    where
        F: FnMut(i64, &mut ffmpeg_next::frame::Video) -> Result<(), FFmpegError>,
    {
        struct Context<F> {
            callback: F,
            cancel: Arc<AtomicBool>,
            frame: Option<ffmpeg_next::frame::Video>,
            error: Option<FFmpegError>,
        }
        unsafe extern "C" fn is_cancelled<F>(user: *mut c_void) -> bool {
            unsafe { &*(user as *const Context<F>) }
                .cancel
                .load(Ordering::Relaxed)
        }
        unsafe extern "C" fn receive<F>(
            user: *mut c_void,
            ms: f64,
            width: u32,
            height: u32,
            stride: u32,
            data: *const u8,
        ) -> bool
        where
            F: FnMut(i64, &mut ffmpeg_next::frame::Video) -> Result<(), FFmpegError>,
        {
            let ctx = unsafe { &mut *(user as *mut Context<F>) };
            if ctx.cancel.load(Ordering::Relaxed) {
                return false;
            }
            if data.is_null()
                || width == 0
                || height == 0
                || width > 1920
                || height > 1080
                || stride < width * 4
            {
                ctx.error = Some(FFmpegError::FrameEmpty);
                return false;
            }
            if ctx
                .frame
                .as_ref()
                .is_none_or(|f| f.width() != width || f.height() != height)
            {
                ctx.frame = Some(ffmpeg_next::frame::Video::new(
                    ffmpeg_next::format::Pixel::RGBA,
                    width,
                    height,
                ));
            }
            let frame = ctx.frame.as_mut().unwrap();
            let dest_stride = frame.stride(0);
            let input =
                unsafe { std::slice::from_raw_parts(data, stride as usize * height as usize) };
            for row in 0..height as usize {
                frame.data_mut(0)[row * dest_stride..row * dest_stride + width as usize * 4]
                    .copy_from_slice(
                        &input[row * stride as usize..row * stride as usize + width as usize * 4],
                    );
            }
            // Never unwind through the C++ callback boundary.
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                (ctx.callback)((ms * 1000.0).round() as i64, frame)
            })) {
                Ok(Ok(())) => true,
                Ok(Err(error)) => {
                    ctx.error = Some(error);
                    false
                }
                Err(_) => {
                    ctx.error = Some(FFmpegError::FrameEmpty);
                    false
                }
            }
        }
        let (path, decoder, _access) = prepare(url)?;
        let ranges: Vec<_> = ranges
            .into_iter()
            .map(|(from, to)| Range { from, to })
            .collect();
        let mut context = Context {
            callback,
            cancel,
            frame: None,
            error: None,
        };
        let rc = unsafe {
            niyien_crm_read(
                path.as_ptr(),
                decoder.as_ptr(),
                std::ptr::null_mut(),
                ranges.as_ptr(),
                ranges.len(),
                Some(receive::<F>),
                Some(is_cancelled::<F>),
                &mut context as *mut _ as *mut c_void,
            )
        };
        if let Some(error) = context.error {
            return Err(error);
        }
        if rc != 0 {
            return Err(FFmpegError::DecoderNotFound);
        }
        Ok(())
    }
}
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use native::{process, video_info};

#[cfg(all(test, any(target_os = "windows", target_os = "macos")))]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    #[test]
    fn crm_decoder_options_encode_paths_without_option_separators() {
        let url = crate::core::filesystem::path_to_url("C:/clips/with space/相机.CRM");
        let decoder = decoder_for_url(&url).unwrap();
        assert!(decoder.starts_with("CRM:source_hex="));
        assert!(
            decoder
                .trim_start_matches("CRM:source_hex=")
                .bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        assert!(decoder_for_url("file:///C:/clips/ordinary.mp4").is_none());
    }
    #[test]
    #[ignore = "Requires a real R3 CRM in GYROFLOW_CRM_FIXTURE"]
    fn crm_fixture_metadata_ranges_and_cancel() {
        let path = std::env::var("GYROFLOW_CRM_FIXTURE").expect("CRM fixture path");
        let url = crate::core::filesystem::path_to_url(&path);
        let info = video_info(&url).expect("CRM container metadata");
        assert_eq!(
            (info.width, info.height, info.frame_count),
            (6000, 3164, 224)
        );
        assert!((info.fps - 60000.0 / 1001.0).abs() < 0.001);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut timestamps = Vec::new();
        process(
            &url,
            vec![(0.0, 80.0), (1800.0, 1900.0), (400.0, 450.0)],
            cancel.clone(),
            |ts, frame| {
                assert_eq!((frame.width(), frame.height()), (1920, 1012));
                assert!(frame.data(0).chunks_exact(4).take(100).all(|p| p[3] == 255));
                timestamps.push(ts);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(timestamps.len(), 14);
        assert_eq!(timestamps[0], 0);
        assert!(timestamps.iter().all(|t| (0..=80000).contains(t)
            || (1800000..=1900000).contains(t)
            || (400000..=450000).contains(t)));
        cancel.store(true, Ordering::Relaxed);
        process(&url, vec![], cancel.clone(), |_, _| {
            panic!("cancelled read must not emit frames")
        })
        .unwrap();
        for _ in 0..8 {
            cancel.store(false, Ordering::Relaxed);
            let mut count = 0;
            process(&url, vec![], cancel.clone(), |_, _| {
                count += 1;
                cancel.store(true, Ordering::Relaxed);
                Ok(())
            })
            .unwrap();
            assert_eq!(count, 1);
        }
        cancel.store(false, Ordering::Relaxed);
        let mut tail = Vec::new();
        process(&url, vec![(3670.0, 4000.0)], cancel, |ts, _| {
            tail.push(ts);
            Ok(())
        })
        .unwrap();
        assert_eq!(tail.len(), 4);
    }
}
