// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Vladimir Pinchuk (https://github.com/VladimirP1)

use crate::gyro_source::GyroSource;
use itertools::izip;
use nalgebra::{ComplexField, Vector3};
use rustfft::{FftPlanner, num_complex::Complex};
use std::f32::consts::PI;
use std::iter::zip;
pub struct OptimSync {
    sample_rate: f64,
    gyro: [Vec<f64>; 3],
}

fn blackman(width: usize) -> Vec<f32> {
    let a0 = 7938.0 / 18608.0;
    let a1 = 9240.0 / 18608.0;
    let a2 = 1430.0 / 18608.0;
    let mut samples = vec![0.0; width];
    let size = (width - 1) as f32;
    for i in 0..width {
        let n = i as f32;
        let v = a0 - a1 * (2.0 * PI * n / size).cos() + a2 * (4.0 * PI * n / size).cos();
        samples[i] = v;
    }
    samples
}

impl OptimSync {
    pub fn new(gyro: &GyroSource) -> Option<OptimSync> {
        let file_metadata = gyro.file_metadata.read();
        let raw_imu = gyro.raw_imu(&file_metadata);

        let duration_ms = raw_imu.last()?.timestamp_ms - raw_imu.first()?.timestamp_ms;
        let samples_total = raw_imu.iter().filter(|x| x.gyro.is_some()).count();
        let avg_sr = samples_total as f64 / duration_ms * 1000.0;

        let interp_gyro = |ts| {
            let i_r = raw_imu
                .partition_point(|sample| sample.timestamp_ms < ts)
                .min(raw_imu.len() - 1);
            let i_l = i_r.max(1) - 1;

            let left = &raw_imu[i_l];
            let right = &raw_imu[i_r];
            if i_l == i_r {
                return Vector3::from_column_slice(&left.gyro.unwrap_or_default());
            }
            (Vector3::from_column_slice(&left.gyro.unwrap_or_default()) * (right.timestamp_ms - ts)
                + Vector3::from_column_slice(&right.gyro.unwrap_or_default())
                    * (ts - left.timestamp_ms))
                / (right.timestamp_ms - left.timestamp_ms)
        };

        let mut gyr = [Vec::<f64>::new(), Vec::<f64>::new(), Vec::<f64>::new()];
        for i in 0..((duration_ms * avg_sr / 1000.0) as usize) {
            let s = interp_gyro(i as f64 * 1000.0 / avg_sr);
            for j in 0..3 {
                gyr[j].push(s[j]);
            }
        }

        Some(OptimSync {
            sample_rate: avg_sr,
            gyro: gyr,
        })
    }

    pub fn run(
        &mut self,
        target_sync_points: usize,
        trim_ranges_s: Vec<(f64, f64)>,
    ) -> (Vec<f64>, Vec<f32>, f64, f64) {
        let gyro_c32: Vec<Vec<Complex<f32>>> = self
            .gyro
            .iter()
            .map(|v| v.iter().map(|&x| Complex::from_real(x as f32)).collect())
            .collect();

        let step_size_samples = 16;
        let nms_radius = ((self.sample_rate / 16.0 / 2.0) * 8.0) as usize; // sync points no closer than 8 seconds

        let fft_size = self.sample_rate.round() as usize;
        let rank_window_center_offset_ms = fft_size as f64 / 2.0 / self.sample_rate * 1000.0;
        let scale = (1.0 / fft_size as f32).sqrt() / fft_size as f32 * 256.0;
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(fft_size);

        let win = blackman(fft_size);

        let ffts: Vec<Vec<_>> = gyro_c32
            .iter()
            .map(|gyro_c32_chan| {
                gyro_c32_chan
                    .windows(fft_size)
                    .step_by(step_size_samples)
                    .map(|chunk| {
                        let mut cm: Vec<_> = zip(chunk, &win).map(|(x, y)| x * y).collect();
                        fft.process(&mut cm);
                        zip(cm.iter(), cm.iter().rev())
                            .take(fft_size / 2)
                            .map(|(a, b)| a + b)
                            .map(|x| x.norm() * scale)
                            .collect::<Vec<_>>()
                    })
                    .collect()
            })
            .collect();

        let map_to_bin = |freq: f64| {
            (fft_size as f64 / self.sample_rate * freq)
                .round()
                .max(0.0)
                .min((fft_size / 2 - 1) as f64) as usize
        };

        let band_energy = |axis: &Vec<Vec<f32>>, begin, end| {
            let f: Vec<_> = axis
                .iter()
                .map(|bins| bins[map_to_bin(begin)..map_to_bin(end)].iter().sum::<f32>())
                .collect();
            f
        };

        fn sum_vec_f32(a: &[f32], b: &[f32]) -> Vec<f32> {
            zip(a, b).map(|(a, b)| a + b).collect()
        }
        let merged_ffts: Vec<_> = izip!(&ffts[0], &ffts[1], &ffts[2])
            .map(|(x, y, z)| sum_vec_f32(&sum_vec_f32(x, y), z))
            .collect();

        let lf = band_energy(&merged_ffts, 0.0, 2.0);
        let mf = band_energy(&merged_ffts, 2.0, 30.0);
        let hf = band_energy(&merged_ffts, 30.0, 2000.0);

        // For low-motion videos (MF max < 50.0), include LF in rank calculation
        // since meaningful signal is concentrated below 2Hz
        let mf_max = mf.iter().cloned().fold(0.0_f32, f32::max);
        let low_motion = mf_max < 50.0;

        // Raw frequency-band energy per window. Preserved untouched so
        // `sync_data.rank` keeps the real signal magnitude even at positions
        // that the selection pipeline below considers "not worth picking".
        // Downstream consumers (sync_repair / candidate rank lookup in
        // render_queue) can then read the actual energy instead of the
        // post-zeroing 0 sentinel.
        let rank_raw: Vec<f32> = izip!(&lf, &mf, &hf)
            .map(|(lf, mf, hf)| {
                if low_motion {
                    // Low-motion video: combine LF+MF as signal source
                    (lf + mf) / (1.0 + nlfunc(*hf, 450.0) * 0.003)
                } else {
                    // Normal video: prefer mid freqs, penalize low and high freqs
                    mf / (1.0 + nlfunc(*hf, 450.0) * 0.003) / (1.0 + nlfunc(*lf, 650.0) * 0.003)
                }
            })
            .collect();

        // Working copy used only for selection (NMS + per-segment max).
        // Threshold/trim/endclip zero this copy so the selection routine
        // below skips low-energy or out-of-trim windows.
        let mut rank: Vec<f32> = rank_raw.clone();

        // Diagnostics: snapshot rank distribution before any zeroing pass.
        let rank_initial_max = rank.iter().cloned().fold(0.0_f32, f32::max);
        let rank_initial_nonzero = rank.iter().filter(|&&x| x > 0.0).count();

        let ratio = step_size_samples as f64 / self.sample_rate;
        for i in 0..rank.len() {
            // Window center time, aligned with output ts and downstream fract filter.
            let time = i as f64 * ratio + rank_window_center_offset_ms / 1000.0;
            if rank[i] < super::sync_repair::MIN_BATCH_SYNC_POINT_RANK
                || !trim_ranges_s.iter().any(|x| time >= x.0 && time <= x.1)
            {
                rank[i] = 0.0;
            }
        }
        // Diagnostics: count surviving ranks after threshold + trim filter.
        let rank_after_threshold_trim_nonzero = rank.iter().filter(|&&x| x > 0.0).count();
        // If the time exceeds 8 seconds, clear the data for the first 2 seconds and last 2 seconds,
        // as most of the distortion occurs from button presses.
        let total_duration = rank.len() as f64 * ratio;
        let endclip_applied = total_duration > 12.0;
        if endclip_applied {
            for i in 0..rank.len() {
                let time = i as f64 * ratio;
                if time < 2.0 || time >= (total_duration - 2.0) {
                    rank[i] = 0.0;
                }
            }
        }
        // Diagnostics: count surviving ranks after head/tail clip pass.
        let rank_after_endclip_nonzero = rank.iter().filter(|&&x| x > 0.0).count();

        let mut rank_nms = rank.clone();
        for i in 0..rank.len() {
            for j in
                (i as i64 - nms_radius as i64).max(0) as usize..(i + nms_radius).min(rank.len() - 1)
            {
                if rank[j] < rank[i] {
                    rank_nms[j] = 0.0;
                }
            }
        }

        // Divide rank_nms evenly according to target_sync_points, and choose the point from each interval to sync_points.
        let segment_size = (rank_nms.len() + target_sync_points - 1) / target_sync_points;
        let selected_sync_points: Vec<f64> = (0..target_sync_points)
            .filter_map(|i| {
                let start = i * segment_size;
                let end = std::cmp::min(start + segment_size, rank_nms.len());

                // Find the maximum value within the current interval along with its index.
                let max_pair = rank_nms
                    .get(start..end)?
                    .iter()
                    .enumerate()
                    .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                max_pair.and_then(|(relative_idx, &value)| {
                    if value < 0.1 {
                        return None;
                    } else {
                        let absolute_idx = start + relative_idx;
                        Some(
                            absolute_idx as f64 * step_size_samples as f64 / self.sample_rate
                                * 1000.0
                                + rank_window_center_offset_ms,
                        )
                    }
                })
            })
            .collect();

        // Diagnostics: emit one summary line per OptimSync run so the batch sync
        // path's "no rank-qualified" cases (selected.is_empty()) are traceable
        // back to which zeroing stage swept the rank array clean.
        let rank_nms_max = rank_nms.iter().cloned().fold(0.0_f32, f32::max);
        let rank_nms_nonzero = rank_nms.iter().filter(|&&x| x > 0.0).count();
        log::info!(
            "[optimsync] dur={:.2}s sr={:.1} samples={} mf_max={:.2} low_motion={} target_pts={} \
             trim_ranges={} endclip_applied={} | rank: init_max={:.2} init_nz={}/{} -> \
             after_threshold30+trim={} -> after_endclip={} -> nms_nz={} (nms_max={:.2}) | selected={}/{}",
            total_duration,
            self.sample_rate,
            rank.len(),
            mf_max,
            low_motion,
            target_sync_points,
            trim_ranges_s.len(),
            endclip_applied,
            rank_initial_max,
            rank_initial_nonzero,
            rank.len(),
            rank_after_threshold_trim_nonzero,
            rank_after_endclip_nonzero,
            rank_nms_nonzero,
            rank_nms_max,
            selected_sync_points.len(),
            target_sync_points,
        );

        // use inline_python::python;
        // python! {
        //     import matplotlib.pyplot as plt
        //     import os

        //     plt.plot('lf, label = "lf", alpha = .3)
        //     plt.plot('mf, label = "mf", alpha = .3)
        //     plt.plot('hf, label = "hf", alpha = .3)

        //     plt.plot('rank, label = "rank")
        //     plt.plot('rank_nms, label = "rank_nms")

        //     plt.legend()
        //     plt.tight_layout()
        //     fig = plt.gcf()
        //     fig.set_size_inches(10, 5)
        //     plt.show()
        // }
        // Return rank_raw (untouched frequency-band energy) so downstream
        // sync_data.rank reflects real signal magnitude, not the post-zeroing
        // sentinel used during selection.
        (selected_sync_points, rank_raw, ratio, rank_window_center_offset_ms)
    }
}

pub fn nlfunc(arg: f32, trip_point: f32) -> f32 {
    if arg < trip_point {
        0.0
    } else {
        arg - trip_point
    }
}
