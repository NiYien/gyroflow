// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::RefCell;

#[derive(Clone, Copy, Debug, PartialEq)]
struct EncoderError {
    reason: &'static str,
    codec: &'static str,
}

thread_local! {
    // Concurrent queue jobs must not read each other's FFmpeg errors.
    static LAST_ENCODER_ERROR: RefCell<Option<EncoderError>> = const { RefCell::new(None) };
}

pub(super) fn clear() {
    let _ = LAST_ENCODER_ERROR.try_with(|error| {
        if let Ok(mut error) = error.try_borrow_mut() {
            *error = None;
        }
    });
}

fn parse_amf_error(line: &str) -> Option<EncoderError> {
    let (prefix, message) = line.trim().split_once(']')?;
    let codec = match prefix.strip_prefix('[')?.split_whitespace().next()? {
        "h264_amf" => "H.264/AVC",
        "hevc_amf" => "H.265/HEVC",
        "av1_amf" => "AV1",
        _ => return None,
    };
    let is_component_creation = message.trim_start().starts_with("CreateComponent(");
    if !is_component_creation
        && !message.trim_start().starts_with("encoder->Init()")
        && !message.trim_start().starts_with("SubmitInput()")
    {
        return None;
    }
    let code = message.split_once("failed with error ")?.1.trim();
    let code = code.trim_end_matches('\0').trim().parse::<u32>().ok()?;
    let reason = match code {
        10 if is_component_creation => "codec",
        30 => "codec",
        29 => "resolution",
        31 => "pixel_format",
        _ => return None,
    };
    Some(EncoderError { reason, codec })
}

pub(super) fn record(line: &str) {
    if let Some(diagnostic) = parse_amf_error(line) {
        let _ = LAST_ENCODER_ERROR.try_with(|error| {
            if let Ok(mut error) = error.try_borrow_mut() {
                *error = Some(diagnostic);
            }
        });
    }
}

pub(crate) fn take_message_marker(use_gpu: bool, width: usize, height: usize) -> Option<String> {
    let error = LAST_ENCODER_ERROR
        .try_with(|error| error.try_borrow_mut().ok()?.take())
        .ok()??;
    if !use_gpu {
        return None;
    }
    Some(format!(
        "gpu_encoder_failed:{};{};{};{}",
        error.reason, error.codec, width, height
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feedback_errors_keep_the_codec_and_requested_resolution() {
        clear();
        record("[hevc_amf @ 00000202969538C0] CreateComponent(AMFVideoEncoderHW_HEVC) failed with error 10\n");
        assert_eq!(
            take_message_marker(true, 1920, 1080).as_deref(),
            Some("gpu_encoder_failed:codec;H.265/HEVC;1920;1080")
        );
        record("[h264_amf @ 0000017C1107A500] SubmitInput() failed with error 29\n");
        assert_eq!(
            take_message_marker(true, 3840, 2160).as_deref(),
            Some("gpu_encoder_failed:resolution;H.264/AVC;3840;2160")
        );
    }

    #[test]
    fn unknown_and_unrelated_errors_do_not_claim_a_gpu_limit() {
        for line in [
            "[CUDA @ 1234] Cannot load nvcuda.dll",
            "[h264 @ 1234] SubmitInput() failed with error 29",
            "[libx264 @ 1234] SubmitInput() failed with error 29",
            "[hevc_amf @ 1234] CreateComponent(AMFVideoEncoderHW_HEVC) failed with error 100",
            "[hevc_amf @ 1234] encoder->Init() failed with error 10",
            "[h264_amf @ 1234] An unrelated operation failed with error 29",
        ] {
            clear();
            record(line);
            assert_eq!(take_message_marker(true, 3840, 2160), None, "{line}");
        }
    }

    #[test]
    fn attempts_and_software_encoding_do_not_reuse_old_errors() {
        clear();
        record("[hevc_amf @ 1234] CreateComponent(AMFVideoEncoderHW_HEVC) failed with error 10");
        clear();
        assert_eq!(take_message_marker(true, 3840, 2160), None);
        record("[h264_amf @ 1234] encoder->Init() failed with error 31");
        assert_eq!(take_message_marker(false, 3840, 2160), None);
        assert_eq!(take_message_marker(true, 3840, 2160), None);
    }

    #[test]
    fn parallel_jobs_keep_their_own_cause() {
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let other_barrier = barrier.clone();
        clear();
        record("[hevc_amf @ 1234] CreateComponent(AMFVideoEncoderHW_HEVC) failed with error 10");
        let other = std::thread::spawn(move || {
            clear();
            record("[h264_amf @ 5678] SubmitInput() failed with error 29");
            other_barrier.wait();
            take_message_marker(true, 3840, 2160).unwrap()
        });
        barrier.wait();
        assert_eq!(
            take_message_marker(true, 1920, 1080).as_deref(),
            Some("gpu_encoder_failed:codec;H.265/HEVC;1920;1080")
        );
        assert_eq!(
            other.join().unwrap(),
            "gpu_encoder_failed:resolution;H.264/AVC;3840;2160"
        );
    }
}
