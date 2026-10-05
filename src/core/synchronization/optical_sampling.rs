// SPDX-License-Identifier: GPL-3.0-or-later

//! Integer frame sampling shared by optical stabilization and synchronization.

pub const TARGET_FPS: f64 = 25.0;

/// Keep real source frames at a fixed integer interval, including fractional-rate footage.
/// The rate is in motion-data time, so slow-motion playback does not change the sampling density.
pub fn frame_step(scaled_fps: f64) -> usize {
    if !scaled_fps.is_finite() || scaled_fps <= 0.0 { return 1; }
    (scaled_fps / TARGET_FPS).round().max(1.0) as usize
}

/// Anchor sampling to source frame indices, not callback counts, so overlapping decode ranges agree.
pub fn keep_frame(timestamp_us: i64, fps: f64, every_nth: usize) -> bool {
    let index = super::optical_motion::tracks::frame_index(timestamp_us, fps);
    index % every_nth.max(1) == 0
}

/// Preserve the original high-pass duration as closely as integer sampling allows.
/// A quadratic fit needs at least five samples to leave a useful residual.
pub fn high_pass_lengths(every_nth: usize) -> (usize, usize) {
    let step = every_nth.max(1) as f64;
    let minimum = ((15.0 / step).round() as usize).max(5) | 1;
    let maximum = ((61.0 / step).round() as usize).max(minimum) | 1;
    (minimum, maximum)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_sampling_targets_25_fps() {
        for (fps, step) in [(10.0, 1), (23.976, 1), (25.0, 1), (29.97, 1), (50.0, 2),
            (59.94, 2), (59.97, 2), (60000.0 / 1001.0, 2), (100.0, 4), (119.88, 5), (120.0, 5), (239.76, 10), (240.0, 10)] {
            assert_eq!(frame_step(fps), step, "fps={fps}");
        }
        for fps in [0.0, -1.0, f64::NAN, f64::INFINITY] { assert_eq!(frame_step(fps), 1); }
    }

    #[test]
    fn sampling_uses_the_same_phase_after_seeks_and_time_scaling() {
        let fps = 24000.0 / 1001.0;
        let scale = 5.0;
        let step = frame_step(fps * scale);
        assert_eq!(step, 5);
        for index in [0, 1, 5, 99, 100, 101, 105, 99, 100, 105] {
            let file_us = (index as f64 * 1e6 / fps).round() as i64;
            assert_eq!(keep_frame(file_us, fps, step), index % step == 0);
            assert_eq!(keep_frame((file_us as f64 / scale).round() as i64, fps * scale, step), index % step == 0);
        }
    }

    #[test]
    fn fractional_rates_keep_source_frame_phase_over_six_hours() {
        for fps in [23.976, 24000.0 / 1001.0, 29.97, 30000.0 / 1001.0,
            59.94, 59.97, 60000.0 / 1001.0, 119.88, 120000.0 / 1001.0, 239.76, 240000.0 / 1001.0] {
            let step = frame_step(fps);
            // Generate timestamps directly from the source rate, including far into a long clip.
            // Rounding the source rate to an integer would eventually select different frames.
            for seconds in [0.0, 1.0, 60.0, 3600.0, 21600.0] {
                let first = (seconds * fps).round() as usize;
                let selected: Vec<_> = (first..first + step * 8).filter(|&index| {
                    let timestamp_us = (index as f64 * 1e6 / fps).round() as i64;
                    let keep = keep_frame(timestamp_us, fps, step);
                    assert_eq!(keep, index % step == 0, "fps={fps}, frame={index}");
                    keep
                }).collect();
                assert_eq!(selected.len(), 8, "fps={fps}, seconds={seconds}");
                assert!(selected.windows(2).all(|frames| frames[1] - frames[0] == step));
            }
        }
    }

    #[test]
    fn high_pass_keeps_time_span_and_enough_samples_for_quadratic_fit() {
        assert_eq!(high_pass_lengths(1), (15, 61));
        assert_eq!(high_pass_lengths(2), (9, 31));
        assert_eq!(high_pass_lengths(5), (5, 13));
        assert_eq!(high_pass_lengths(10), (5, 7));
        assert_eq!(high_pass_lengths(100), (5, 5));
    }
}
