// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

// Batch video-gyro matching algorithm module.
// Automatically matches multiple video files to their corresponding gyroscope data files
// based on creation timestamps and duration analysis.

// --- T1: Data structures ---

/// Input: metadata for a video file to be matched.
pub struct VideoMatchInfo {
    pub path: String,
    pub duration_ms: f64,
    pub created_at_ms: Option<i64>,
    pub pre_recording_ms: f64,
}

/// Input: metadata for a gyro data file to be matched.
pub struct GyroMatchInfo {
    pub path: String,
    pub duration_ms: f64,
    pub created_at_ms: i64,
}

/// Input: manually specified calibration pair (job_id + gyro index).
/// 使用 job_id 而非 video_index，避免 remove/sort 后队列位置变化导致 pair 断裂。
/// 调用 batch_match 前需将 job_id 转换为当前队列中的 video_index。
pub struct ManualCalibrationPair {
    pub job_id: u32,
    pub video_index: usize,
    pub gyro_index: usize,
}

/// Status of a match result.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchStatus {
    Matched,
    CalibrationPair,
    Unmatched,
    NoCreationTime,
}

/// Result for a single video's match outcome.
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub video_index: usize,
    pub job_id: Option<u32>, // [queue-lifecycle T4] 用于在 remove 后按 job_id 查找
    pub gyro_index: Option<usize>,
    pub status: MatchStatus,
    pub global_offset_ms: Option<i64>,
    pub gyro_start_ms: Option<f64>,
    pub gyro_end_ms: Option<f64>,
}

/// Result of the entire batch matching operation.
pub struct BatchMatchResult {
    pub results: Vec<MatchResult>,
    pub global_offset_ms: Option<i64>,
    pub error: Option<MatchError>,
}

/// Errors that can occur during matching.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchError {
    NoCalibrationPairsFound,
    InsufficientCoverage,
}

// --- T3 & T4: Calibration detection ---

// Threshold for consecutive short clip detection (ms).
const CONSECUTIVE_GAP_THRESHOLD_MS: i64 = 90_000;
// Minimum number of consecutive short clips to form a calibration group.
const MIN_CONSECUTIVE_COUNT: usize = 2;

/// Find indices of consecutive short videos suitable for calibration.
/// Short videos: duration < 10s + pre_recording, must have created_at.
/// Consecutive: adjacent creation times <= 90s apart, group size >= 2.
fn find_calibration_videos(videos: &[VideoMatchInfo]) -> Vec<usize> {
    // Collect (original_index, created_at_ms) for short videos with timestamps
    let mut candidates: Vec<(usize, i64)> = videos
        .iter()
        .enumerate()
        .filter(|(_, v)| v.duration_ms < 10_000.0 + v.pre_recording_ms && v.created_at_ms.is_some())
        .map(|(i, v)| (i, v.created_at_ms.unwrap()))
        .collect();

    // Sort by created_at
    candidates.sort_by_key(|&(_, t)| t);

    find_consecutive_groups(&candidates)
}

/// Find indices of consecutive short gyro files suitable for calibration.
/// Short gyros: duration < 12s.
/// Consecutive: adjacent creation times <= 90s apart, group size >= 2.
fn find_calibration_gyros(gyros: &[GyroMatchInfo]) -> Vec<usize> {
    let mut candidates: Vec<(usize, i64)> = gyros
        .iter()
        .enumerate()
        .filter(|(_, g)| g.duration_ms < 12_000.0)
        .map(|(i, g)| (i, g.created_at_ms))
        .collect();

    candidates.sort_by_key(|&(_, t)| t);

    find_consecutive_groups(&candidates)
}

/// Generic helper: given sorted (index, timestamp) pairs, find all indices
/// belonging to consecutive groups (gap <= 90s, group size >= 2).
fn find_consecutive_groups(sorted_candidates: &[(usize, i64)]) -> Vec<usize> {
    if sorted_candidates.is_empty() {
        return Vec::new();
    }

    let mut groups: Vec<Vec<usize>> = Vec::new();
    let mut current_group = vec![sorted_candidates[0].0];

    for window in sorted_candidates.windows(2) {
        let gap = window[1].1 - window[0].1;
        if gap <= CONSECUTIVE_GAP_THRESHOLD_MS {
            current_group.push(window[1].0);
        } else {
            if current_group.len() >= MIN_CONSECUTIVE_COUNT {
                groups.push(std::mem::take(&mut current_group));
            } else {
                current_group.clear();
            }
            current_group.push(window[1].0);
        }
    }
    if current_group.len() >= MIN_CONSECUTIVE_COUNT {
        groups.push(current_group);
    }

    // Flatten all qualifying groups into a single list
    groups.into_iter().flatten().collect()
}

// --- T5: compute_global_offset ---

// Maximum allowed difference between gyro duration and video duration (seconds).
const SYNC_DURATION_OFFSET_MAX: f64 = 1.5;
// Maximum allowed difference between two offsets from adjacent pairs (ms).
const SYNC_CREATE_OFFSET_MAX: i64 = 3000;
// Maximum gap between adjacent calibration gyro creation times (ms).
const ADJACENT_GYRO_GAP_MAX: i64 = 60_000;

/// Internal result from offset computation.
struct OffsetResult {
    offset: i64,
    delay: i64,
    calibration_video_indices: Vec<usize>,
    calibration_gyro_indices: Vec<usize>,
}

/// Compute the global time offset between video and gyro timelines.
///
/// Uses pairs of adjacent calibration videos and adjacent calibration gyros.
/// For each pair combination, checks duration and offset consistency.
/// Returns the median offset with best coverage.
fn compute_global_offset(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    cal_video_indices: &[usize],
    cal_gyro_indices: &[usize],
) -> Result<OffsetResult, MatchError> {
    if cal_video_indices.len() < 2 || cal_gyro_indices.len() < 2 {
        return Err(MatchError::NoCalibrationPairsFound);
    }

    // Candidate offsets: (offset, delay, video_pair, gyro_pair)
    let mut candidates: Vec<(i64, i64, [usize; 2], [usize; 2])> = Vec::new();

    // Compare adjacent video pairs with adjacent gyro pairs
    for vi in 0..cal_video_indices.len() - 1 {
        let v0 = &videos[cal_video_indices[vi]];
        let v1 = &videos[cal_video_indices[vi + 1]];

        let v0_created = match v0.created_at_ms {
            Some(t) => t,
            None => continue,
        };
        let v1_created = match v1.created_at_ms {
            Some(t) => t,
            None => continue,
        };

        let v0_dur_s = v0.duration_ms / 1000.0;
        let v1_dur_s = v1.duration_ms / 1000.0;
        let pre0_s = v0.pre_recording_ms / 1000.0;
        let pre1_s = v1.pre_recording_ms / 1000.0;

        for gi in 0..cal_gyro_indices.len() - 1 {
            let g0 = &gyros[cal_gyro_indices[gi]];
            let g1 = &gyros[cal_gyro_indices[gi + 1]];

            // Check adjacent gyro gap
            let gyro_gap = (g1.created_at_ms - g0.created_at_ms).abs();
            if gyro_gap > ADJACENT_GYRO_GAP_MAX {
                continue;
            }

            let g0_dur_s = g0.duration_ms / 1000.0;
            let g1_dur_s = g1.duration_ms / 1000.0;

            // Duration match: |gyro_duration - 0.5 + pre_recording - video_duration| <= 1.5
            let dur_diff0 = g0_dur_s - 0.5 + pre0_s - v0_dur_s;
            let dur_diff1 = g1_dur_s - 0.5 + pre1_s - v1_dur_s;

            if dur_diff0.abs() > SYNC_DURATION_OFFSET_MAX {
                continue;
            }
            if dur_diff1.abs() > SYNC_DURATION_OFFSET_MAX {
                continue;
            }

            // The two duration diffs should be close
            if (dur_diff0 - dur_diff1).abs() > SYNC_DURATION_OFFSET_MAX {
                continue;
            }

            // Offset consistency
            let offset0 = g0.created_at_ms - v0_created;
            let offset1 = g1.created_at_ms - v1_created;

            if (offset0 - offset1).abs() > SYNC_CREATE_OFFSET_MAX {
                continue;
            }

            // Delay detection: when gyro recording is significantly longer than video
            let total_diff0 = g0_dur_s + pre0_s - v0_dur_s;
            let total_diff1 = g1_dur_s + pre1_s - v1_dur_s;
            let delay = if total_diff0 > 0.8
                && total_diff1 > 0.8
                && (total_diff0 > 1.3 || total_diff1 > 1.3)
            {
                500i64
            } else {
                0i64
            };

            let avg_offset = (offset0 + offset1) / 2;
            candidates.push((
                avg_offset,
                delay,
                [cal_video_indices[vi], cal_video_indices[vi + 1]],
                [cal_gyro_indices[gi], cal_gyro_indices[gi + 1]],
            ));
        }
    }

    if candidates.is_empty() {
        return Err(MatchError::NoCalibrationPairsFound);
    }

    // Group candidates by similar offset and pick the group with best coverage
    // First, sort by offset for median selection
    candidates.sort_by_key(|c| c.0);

    // Try each candidate offset and check coverage across all gyros
    let mut best: Option<(i64, i64, usize, Vec<usize>, Vec<usize>)> = None;

    // Collect unique offsets to try
    let unique_offsets: Vec<(i64, i64)> = candidates.iter().map(|c| (c.0, c.1)).collect();

    for &(test_offset, test_delay) in &unique_offsets {
        let video_offset = test_offset - test_delay;
        let mut covered = 0usize;

        for v in videos.iter() {
            if let Some(v_created) = v.created_at_ms {
                for g in gyros.iter() {
                    // The gyro at g.created_at_ms corresponds to video time (g.created_at - offset + delay)
                    let video_start = g.created_at_ms - video_offset;
                    let video_end = video_start + (g.duration_ms as i64);

                    if v_created >= video_start - 1000 && v_created <= video_end + 1000 {
                        covered += 1;
                        break;
                    }
                }
            }
        }

        let dominated = match &best {
            Some((_, _, best_cov, _, _)) => covered > *best_cov,
            None => true,
        };

        if dominated && covered >= 2 {
            // Collect the calibration indices used
            let cal_v: Vec<usize> = candidates
                .iter()
                .filter(|c| (c.0 - test_offset).abs() <= SYNC_CREATE_OFFSET_MAX)
                .flat_map(|c| c.2.iter().copied())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            let cal_g: Vec<usize> = candidates
                .iter()
                .filter(|c| (c.0 - test_offset).abs() <= SYNC_CREATE_OFFSET_MAX)
                .flat_map(|c| c.3.iter().copied())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();

            best = Some((test_offset, test_delay, covered, cal_v, cal_g));
        }
    }

    match best {
        Some((offset, delay, _coverage, cal_v, cal_g)) => {
            // Use median of matching offsets for robustness
            let mut matching_offsets: Vec<i64> = candidates
                .iter()
                .filter(|c| (c.0 - offset).abs() <= SYNC_CREATE_OFFSET_MAX)
                .map(|c| c.0)
                .collect();
            matching_offsets.sort();
            let median_offset = matching_offsets[matching_offsets.len() / 2];

            Ok(OffsetResult {
                offset: median_offset,
                delay,
                calibration_video_indices: cal_v,
                calibration_gyro_indices: cal_g,
            })
        }
        None => Err(MatchError::InsufficientCoverage),
    }
}

// --- T6: assign_gyro_to_videos ---

// Compensation time margin (ms).
const COMP_TIME_MS: f64 = 500.0;
// Maximum per-day drift compensation (ms).
const MAX_DAILY_DRIFT_MS: f64 = 1000.0;
// Milliseconds in a day.
const MS_PER_DAY: f64 = 86_400_000.0;

/// Assign each video to its corresponding gyro segment based on the global offset.
///
/// For each video with a creation timestamp, finds the gyro file whose time range
/// covers the video, then computes the exact start/end within that gyro's timeline.
fn assign_gyro_to_videos(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    global_offset: i64,
    delay: i64,
    calibration_video_indices: &[usize],
) -> Vec<MatchResult> {
    let video_offset = global_offset - delay;

    videos
        .iter()
        .enumerate()
        .map(|(vi, v)| {
            let v_created = match v.created_at_ms {
                Some(t) => t,
                None => {
                    return MatchResult {
                        video_index: vi,
                        job_id: None,
                        gyro_index: None,
                        status: MatchStatus::NoCreationTime,
                        global_offset_ms: Some(global_offset),
                        gyro_start_ms: None,
                        gyro_end_ms: None,
                    };
                }
            };

            // Check if this is a calibration video
            let is_cal = calibration_video_indices.contains(&vi);

            // Try to find a matching gyro
            for (gi, g) in gyros.iter().enumerate() {
                // The gyro at g.created_at_ms corresponds to video time (g.created_at - offset + delay)
                let video_start = g.created_at_ms - video_offset;
                let video_end = video_start + (g.duration_ms as i64);

                if v_created >= video_start - 1000 && v_created <= video_end + 1000 {
                    // Compute time drift compensation
                    let time_diff_from_start = (v_created - video_start).abs() as f64;
                    let drift_comp = (time_diff_from_start * MAX_DAILY_DRIFT_MS / MS_PER_DAY)
                        .min(MAX_DAILY_DRIFT_MS);
                    let front_comp = COMP_TIME_MS + drift_comp;
                    let back_comp = COMP_TIME_MS + drift_comp;

                    // Position within the gyro's own timeline
                    let gyro_start_ms = (v_created - video_start) as f64 - front_comp;
                    let gyro_end_ms = gyro_start_ms + v.duration_ms + front_comp + back_comp;

                    let status = if is_cal {
                        MatchStatus::CalibrationPair
                    } else {
                        MatchStatus::Matched
                    };

                    return MatchResult {
                        video_index: vi,
                        job_id: None,
                        gyro_index: Some(gi),
                        status,
                        global_offset_ms: Some(global_offset),
                        gyro_start_ms: Some(gyro_start_ms),
                        gyro_end_ms: Some(gyro_end_ms),
                    };
                }
            }

            MatchResult {
                video_index: vi,
                job_id: None,
                gyro_index: None,
                status: MatchStatus::Unmatched,
                global_offset_ms: Some(global_offset),
                gyro_start_ms: None,
                gyro_end_ms: None,
            }
        })
        .collect()
}

// --- T7: Manual calibration pair support ---

/// Compute offset from manually specified calibration pairs,
/// then assign gyro segments to all videos.
fn compute_from_manual_pairs(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    manual_pairs: &[ManualCalibrationPair],
) -> Result<OffsetResult, MatchError> {
    if manual_pairs.is_empty() {
        return Err(MatchError::NoCalibrationPairsFound);
    }

    if manual_pairs.len() == 1 {
        let v = &videos[manual_pairs[0].video_index];
        let g = &gyros[manual_pairs[0].gyro_index];
        let v_created = v.created_at_ms.ok_or(MatchError::NoCalibrationPairsFound)?;
        let offset = g.created_at_ms - v_created;
        return Ok(OffsetResult {
            offset,
            delay: 0,
            calibration_video_indices: vec![manual_pairs[0].video_index],
            calibration_gyro_indices: vec![manual_pairs[0].gyro_index],
        });
    }

    // Extract video and gyro indices from manual pairs
    let cal_video_indices: Vec<usize> = manual_pairs.iter().map(|p| p.video_index).collect();
    let cal_gyro_indices: Vec<usize> = manual_pairs.iter().map(|p| p.gyro_index).collect();

    // Compute offsets from each adjacent pair
    let mut offsets: Vec<i64> = Vec::new();
    let mut delays: Vec<i64> = Vec::new();

    for i in 0..manual_pairs.len() - 1 {
        let v0 = &videos[manual_pairs[i].video_index];
        let v1 = &videos[manual_pairs[i + 1].video_index];
        let g0 = &gyros[manual_pairs[i].gyro_index];
        let g1 = &gyros[manual_pairs[i + 1].gyro_index];

        let v0_created = v0
            .created_at_ms
            .ok_or(MatchError::NoCalibrationPairsFound)?;
        let v1_created = v1
            .created_at_ms
            .ok_or(MatchError::NoCalibrationPairsFound)?;

        let offset0 = g0.created_at_ms - v0_created;
        let offset1 = g1.created_at_ms - v1_created;
        let avg = (offset0 + offset1) / 2;
        offsets.push(avg);

        // Delay detection
        let pre0_s = v0.pre_recording_ms / 1000.0;
        let pre1_s = v1.pre_recording_ms / 1000.0;
        let diff0 = g0.duration_ms / 1000.0 + pre0_s - v0.duration_ms / 1000.0;
        let diff1 = g1.duration_ms / 1000.0 + pre1_s - v1.duration_ms / 1000.0;
        let delay = if diff0 > 0.8 && diff1 > 0.8 && (diff0 > 1.3 || diff1 > 1.3) {
            500
        } else {
            0
        };
        delays.push(delay);
    }

    offsets.sort();
    let median_offset = offsets[offsets.len() / 2];
    // Use the most common delay
    let delay = if delays.iter().filter(|&&d| d == 500).count() > delays.len() / 2 {
        500
    } else {
        0
    };

    Ok(OffsetResult {
        offset: median_offset,
        delay,
        calibration_video_indices: cal_video_indices,
        calibration_gyro_indices: cal_gyro_indices,
    })
}

// --- T8: Top-level API ---

/// Batch match videos to gyro files.
///
/// If `manual_pairs` is provided (non-empty), uses manual calibration pairs.
/// Otherwise, automatically detects calibration videos/gyros, computes offset, and assigns.
pub fn batch_match(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    manual_pairs: Option<&[ManualCalibrationPair]>,
) -> BatchMatchResult {
    // Choose manual or automatic path
    let offset_result = if let Some(pairs) = manual_pairs {
        if !pairs.is_empty() {
            compute_from_manual_pairs(videos, gyros, pairs)
        } else {
            auto_match(videos, gyros)
        }
    } else {
        auto_match(videos, gyros)
    };

    match offset_result {
        Ok(or) => {
            let results = assign_gyro_to_videos(
                videos,
                gyros,
                or.offset,
                or.delay,
                &or.calibration_video_indices,
            );
            BatchMatchResult {
                results,
                global_offset_ms: Some(or.offset),
                error: None,
            }
        }
        Err(e) => {
            // Return all videos as unmatched
            let results = videos
                .iter()
                .enumerate()
                .map(|(i, v)| MatchResult {
                    video_index: i,
                    job_id: None,
                    gyro_index: None,
                    status: if v.created_at_ms.is_some() {
                        MatchStatus::Unmatched
                    } else {
                        MatchStatus::NoCreationTime
                    },
                    global_offset_ms: None,
                    gyro_start_ms: None,
                    gyro_end_ms: None,
                })
                .collect();
            BatchMatchResult {
                results,
                global_offset_ms: None,
                error: Some(e),
            }
        }
    }
}

/// Automatic calibration detection and offset computation.
fn auto_match(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
) -> Result<OffsetResult, MatchError> {
    let cal_videos = find_calibration_videos(videos);
    let cal_gyros = find_calibration_gyros(gyros);

    if cal_videos.len() < 2 || cal_gyros.len() < 2 {
        return Err(MatchError::NoCalibrationPairsFound);
    }

    compute_global_offset(videos, gyros, &cal_videos, &cal_gyros)
}
