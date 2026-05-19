// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

// Batch video-gyro matching algorithm module.
// Automatically matches multiple video files to their corresponding gyroscope data files
// based on creation timestamps and duration analysis.
//
// Multi-session capable (batch-gyro-match-multi-session):
//   1. find_calibration_videos / find_calibration_gyros -> Vec<Vec<usize>> (one cluster per shooting block).
//   2. pair_sessions: pair V/G clusters greedily by anchor time (median(created_at)).
//   3. compute_session_offset: per-session offset/delay/reliable flag.
//   4. assign_videos_by_coverage: video belongs to the session whose [v_start, v_end] covers it.
//   5. assign_fallback: borrow nearest reliable session within +/- 24h.

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
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum MatchStatus {
    Matched,
    /// Video borrowed offset from a neighbouring session within +/- 24h
    /// because no session covers it / its own session is unreliable.
    MatchedFallback,
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
    // Per-clip sync initial offset (= -front_comp), so the sync search window
    // is centered on the pre-allocated buffer point rather than 0.
    pub init_offset_ms: Option<f64>,
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

/// Find clusters of consecutive short videos suitable for calibration.
/// Short videos: duration < 10s + pre_recording, must have created_at.
/// Consecutive: adjacent creation times <= 90s apart, group size >= 2.
/// Returns one Vec<usize> per cluster (multi-session aware).
fn find_calibration_videos(videos: &[VideoMatchInfo]) -> Vec<Vec<usize>> {
    let mut candidates: Vec<(usize, i64)> = videos
        .iter()
        .enumerate()
        .filter(|(_, v)| v.duration_ms < 10_000.0 + v.pre_recording_ms && v.created_at_ms.is_some())
        .map(|(i, v)| (i, v.created_at_ms.unwrap()))
        .collect();

    candidates.sort_by_key(|&(_, t)| t);

    find_consecutive_groups(&candidates)
}

/// Find clusters of consecutive short gyro files suitable for calibration.
/// Short gyros: duration < 12s.
/// Consecutive: adjacent creation times <= 90s apart, group size >= 2.
fn find_calibration_gyros(gyros: &[GyroMatchInfo]) -> Vec<Vec<usize>> {
    let mut candidates: Vec<(usize, i64)> = gyros
        .iter()
        .enumerate()
        .filter(|(_, g)| g.duration_ms < 12_000.0)
        .map(|(i, g)| (i, g.created_at_ms))
        .collect();

    candidates.sort_by_key(|&(_, t)| t);

    find_consecutive_groups(&candidates)
}

/// Generic helper: given sorted (index, timestamp) pairs, find all consecutive
/// groups (gap <= 90s, group size >= 2). Returns one Vec<usize> per group.
fn find_consecutive_groups(sorted_candidates: &[(usize, i64)]) -> Vec<Vec<usize>> {
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

    groups
}

/// Trait to extract created_at_ms from a video or gyro entry; lets
/// cluster_anchor share a single implementation.
trait HasCreatedAt {
    fn created_at(&self) -> Option<i64>;
}

impl HasCreatedAt for VideoMatchInfo {
    fn created_at(&self) -> Option<i64> {
        self.created_at_ms
    }
}

impl HasCreatedAt for GyroMatchInfo {
    fn created_at(&self) -> Option<i64> {
        Some(self.created_at_ms)
    }
}

/// Median created_at_ms over the cluster. For even-length inputs returns the
/// lower median (e.g. for [1000, 1500, 2000, 2500] returns 1500). Items
/// without `created_at_ms` are skipped. Panics only when the cluster has zero
/// valid timestamps - callers must ensure clusters are non-empty (always true
/// for the calibration pipeline, which builds clusters from items that
/// already have timestamps).
fn cluster_anchor<T: HasCreatedAt>(cluster: &[usize], items: &[T]) -> i64 {
    let mut ts: Vec<i64> = cluster
        .iter()
        .filter_map(|&i| items.get(i).and_then(|x| x.created_at()))
        .collect();
    ts.sort();
    // Lower median: index (n - 1) / 2.
    ts[(ts.len() - 1) / 2]
}

// --- T5: compute_global_offset ---

// Maximum allowed difference between gyro duration and video duration (seconds).
const SYNC_DURATION_OFFSET_MAX: f64 = 1.5;
// Maximum allowed difference between two offsets from adjacent pairs (ms).
const SYNC_CREATE_OFFSET_MAX: i64 = 3000;
// Maximum gap between adjacent calibration gyro creation times (ms).
const ADJACENT_GYRO_GAP_MAX: i64 = 60_000;

// --- Multi-session constants ---

// Anchor time difference upper bound when pairing a V cluster with a G cluster.
// 30 minutes gives ~5x slack over typical user workflow (start cal IMU, take
// a series of cal shots over a few minutes), while staying well below the
// spec's 18h ceiling so "different shooting day" V clusters still reject.
// Spec.md §session pairing originally wrote 18h - that is the absolute upper
// bound; the 30-min practical bound prevents orphan content V clusters that
// happen to fall within the same day from polluting the cal offset estimate.
// A V cluster outside this window orphans and its videos rely on the +/- 24h
// fallback to borrow the nearest reliable session's offset.
const SESSION_PAIR_ANCHOR_GAP_MAX_MS: i64 = 30 * 60_000;
// Per-video coverage tolerance (matches legacy behaviour).
const COVERAGE_TOLERANCE_MS: i64 = 1000;
// When two sessions both cover the same video and the depth difference is
// smaller than this, the video is pushed to the fallback path instead of
// arbitrarily picking one.
const COVERAGE_DEPTH_AMBIGUITY_MS: i64 = 100;
// Fallback search window: video may borrow a neighbouring session within +/- 36h.
// 36h covers "previous-day / next-day / one-and-a-half-day" common scenarios
// while still rejecting two-day-plus borrowing (which risks larger clock
// drift than is reasonable to bridge from a single cal).
const FALLBACK_MAX_GAP_MS: i64 = 36 * 3_600_000;

/// Internal result from legacy offset computation (used by manual_pairs path).
struct OffsetResult {
    offset: i64,
    delay: i64,
    calibration_video_indices: Vec<usize>,
    #[allow(dead_code)]
    calibration_gyro_indices: Vec<usize>,
}

/// Per-session bookkeeping. Each session represents one paired (V cluster, G cluster).
/// `cal_video_indices` is the subset of `v_cluster` whose (v, g) pair landed
/// in the winning offset bucket - these are the *verified* cal videos. The
/// is_cal status check uses this narrower set so content clips that
/// accidentally fell into the V cluster (e.g. < 10s but not actual cal) are
/// kept as Matched rather than Skipped by the render queue.
struct Session {
    v_cluster: Vec<usize>,
    cal_video_indices: Vec<usize>,
    g_cluster: Vec<usize>,
    anchor_ms: i64,
    offset: i64,
    delay: i64,
    reliable: bool,
}

/// Compute offset/delay/spread for a single session.
///
/// Equivalent to the legacy compute_global_offset but scoped to one
/// (v_cluster, g_cluster) pair. Returns `Some((offset, delay, spread_ms))` when
/// at least one candidate pair passes the duration / adjacency filters, or
/// `None` when no candidate survived.
fn compute_session_offset(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    v_cluster: &[usize],
    g_cluster: &[usize],
) -> Option<(i64, i64, i64, Vec<usize>)> {
    if v_cluster.len() < 2 || g_cluster.len() < 2 {
        return None;
    }

    // Candidate offsets: (offset, delay, video_pair, gyro_pair)
    let mut candidates: Vec<(i64, i64, [usize; 2], [usize; 2])> = Vec::new();

    for vi in 0..v_cluster.len() - 1 {
        let v0 = &videos[v_cluster[vi]];
        let v1 = &videos[v_cluster[vi + 1]];

        let v0_created = v0.created_at_ms?;
        let v1_created = v1.created_at_ms?;

        let v0_dur_s = v0.duration_ms / 1000.0;
        let v1_dur_s = v1.duration_ms / 1000.0;
        let pre0_s = v0.pre_recording_ms / 1000.0;
        let pre1_s = v1.pre_recording_ms / 1000.0;

        for gi in 0..g_cluster.len() - 1 {
            let g0 = &gyros[g_cluster[gi]];
            let g1 = &gyros[g_cluster[gi + 1]];

            let gyro_gap = (g1.created_at_ms - g0.created_at_ms).abs();
            if gyro_gap > ADJACENT_GYRO_GAP_MAX {
                continue;
            }

            let g0_dur_s = g0.duration_ms / 1000.0;
            let g1_dur_s = g1.duration_ms / 1000.0;

            let dur_diff0 = g0_dur_s - 0.5 + pre0_s - v0_dur_s;
            let dur_diff1 = g1_dur_s - 0.5 + pre1_s - v1_dur_s;

            if dur_diff0.abs() > SYNC_DURATION_OFFSET_MAX {
                continue;
            }
            if dur_diff1.abs() > SYNC_DURATION_OFFSET_MAX {
                continue;
            }
            if (dur_diff0 - dur_diff1).abs() > SYNC_DURATION_OFFSET_MAX {
                continue;
            }

            let offset0 = g0.created_at_ms - v0_created;
            let offset1 = g1.created_at_ms - v1_created;

            if (offset0 - offset1).abs() > SYNC_CREATE_OFFSET_MAX {
                continue;
            }

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
            log::info!(
                "[batch_match_diag] candidate vi_pair=[{},{}] gi_pair=[{},{}] offset0={}ms offset1={}ms avg={}ms delay={}ms dur_diff=[{:.3},{:.3}] total_diff=[{:.3},{:.3}] v_paths=['{}','{}'] g_paths=['{}','{}']",
                v_cluster[vi],
                v_cluster[vi + 1],
                g_cluster[gi],
                g_cluster[gi + 1],
                offset0,
                offset1,
                avg_offset,
                delay,
                dur_diff0,
                dur_diff1,
                total_diff0,
                total_diff1,
                v0.path,
                v1.path,
                g0.path,
                g1.path
            );
            candidates.push((
                avg_offset,
                delay,
                [v_cluster[vi], v_cluster[vi + 1]],
                [g_cluster[gi], g_cluster[gi + 1]],
            ));
        }
    }

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by_key(|c| c.0);

    // RANSAC-style mode finder with video-coverage tie-break.
    //
    // For each candidate we treat its offset as a hypothesis and count how
    // many other candidates fall within SYNC_CREATE_OFFSET_MAX (= inlier
    // count, robust to proxy duplicates and cross-pair noise). The candidate
    // with the highest inlier count wins.
    //
    // When multiple candidates tie on inlier count (e.g. two equally-sized
    // offset clusters from a multi-modal distribution like "morning cal +
    // evening cal with different drifts") we tie-break by **geometric video
    // coverage**: which hypothesis offset, when applied across all gyros,
    // covers the most videos. This favours the cluster that explains more
    // of the data instead of arbitrarily picking the lower offset.
    let n = candidates.len();
    let inlier_counts: Vec<usize> = candidates
        .iter()
        .map(|c| {
            candidates
                .iter()
                .filter(|other| (other.0 - c.0).abs() <= SYNC_CREATE_OFFSET_MAX)
                .count()
        })
        .collect();
    let max_count = *inlier_counts.iter().max().unwrap_or(&1);
    let mode_indices: Vec<usize> = inlier_counts
        .iter()
        .enumerate()
        .filter(|&(_, &c)| c == max_count)
        .map(|(i, _)| i)
        .collect();

    // Closure: count how many input videos this offset would cover.
    let coverage = |test_offset: i64| -> usize {
        let mut covered = 0usize;
        for v in videos.iter() {
            if let Some(v_created) = v.created_at_ms {
                for g in gyros.iter() {
                    let v_start = g.created_at_ms - test_offset;
                    let v_end = v_start + (g.duration_ms as i64);
                    if v_created >= v_start - COVERAGE_TOLERANCE_MS
                        && v_created <= v_end + COVERAGE_TOLERANCE_MS
                    {
                        covered += 1;
                        break;
                    }
                }
            }
        }
        covered
    };

    // Pick the mode. Tie-break by video coverage; stable on equal coverage
    // by preferring the earlier-listed mode (deterministic).
    let chosen_idx = if mode_indices.len() == 1 {
        mode_indices[0]
    } else {
        let mut best_idx = mode_indices[0];
        let mut best_coverage = coverage(candidates[best_idx].0);
        for &i in mode_indices.iter().skip(1) {
            let cov = coverage(candidates[i].0);
            log::info!(
                "[batch_match_diag] tie_break candidate_offset={} inlier_count={} coverage={}",
                candidates[i].0,
                max_count,
                cov
            );
            if cov > best_coverage {
                best_coverage = cov;
                best_idx = i;
            }
        }
        best_idx
    };

    // The inlier set: every candidate within SYNC_CREATE_OFFSET_MAX of the
    // chosen hypothesis. Median of inliers becomes the final offset.
    let chosen_center = candidates[chosen_idx].0;
    let inliers: Vec<&(i64, i64, [usize; 2], [usize; 2])> = candidates
        .iter()
        .filter(|c| (c.0 - chosen_center).abs() <= SYNC_CREATE_OFFSET_MAX)
        .collect();
    let mut inlier_offsets: Vec<i64> = inliers.iter().map(|c| c.0).collect();
    inlier_offsets.sort();
    let median_offset = inlier_offsets[inlier_offsets.len() / 2];

    // Delay: majority of inliers.
    let delay_500_count = inliers.iter().filter(|c| c.1 == 500).count();
    let delay = if delay_500_count * 2 > inliers.len() {
        500
    } else {
        0
    };

    // Spread = max - min within the inlier set (used for the reliable flag).
    let spread = inlier_offsets.last().copied().unwrap_or(median_offset)
        - inlier_offsets.first().copied().unwrap_or(median_offset);

    // Track the video indices that participated in the inlier set. These are
    // the "verified" cal videos; any video in v_cluster outside this set is
    // just an incidentally-short clip and stays Matched.
    let mut cal_videos: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for c in &inliers {
        cal_videos.insert(c.2[0]);
        cal_videos.insert(c.2[1]);
    }
    let cal_video_indices: Vec<usize> = cal_videos.into_iter().collect();

    log::info!(
        "[batch_match_diag] selected global_offset={}ms delay={}ms inlier_count={}/{} ties={} spread_ms={} cal_videos={:?} all_candidates={:?}",
        median_offset,
        delay,
        inliers.len(),
        n,
        mode_indices.len(),
        spread,
        cal_video_indices,
        candidates.iter().map(|c| c.0).collect::<Vec<_>>()
    );

    Some((median_offset, delay, spread, cal_video_indices))
}

/// Pair V/G clusters into sessions. Each V cluster forms ONE candidate
/// session with its nearest G cluster, IF the anchor gap is within
/// SESSION_PAIR_ANCHOR_GAP_MAX_MS (10 min - tight enough to require the V/G
/// pair to be from the same calibration moment) AND the duration check
/// passes. Multiple sessions may share the same G cluster (real users do
/// several cal moments in a day, each with their own short videos but only
/// one IMU recording).
///
/// V clusters that don't pair stay as orphans - their videos fall through to
/// the fallback path and borrow the nearest reliable session's offset.
fn pair_sessions(
    v_clusters: Vec<Vec<usize>>,
    g_clusters: Vec<Vec<usize>>,
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
) -> Vec<Session> {
    if v_clusters.is_empty() || g_clusters.is_empty() {
        return Vec::new();
    }

    let v_with_anchor: Vec<(Vec<usize>, i64)> = v_clusters
        .into_iter()
        .map(|c| {
            let a = cluster_anchor(&c, videos);
            (c, a)
        })
        .collect();
    let g_with_anchor: Vec<(Vec<usize>, i64)> = g_clusters
        .into_iter()
        .map(|c| {
            let a = cluster_anchor(&c, gyros);
            (c, a)
        })
        .collect();

    let mut sessions: Vec<Session> = Vec::new();

    for (v_cluster, v_anchor) in &v_with_anchor {
        // Find the nearest G cluster.
        let nearest = (0..g_with_anchor.len())
            .min_by_key(|&gi| (g_with_anchor[gi].1 - v_anchor).abs());
        let gi = match nearest {
            Some(gi) => gi,
            None => continue,
        };
        let (g_cluster, g_anchor) = &g_with_anchor[gi];
        let gap = (g_anchor - v_anchor).abs();

        if gap > SESSION_PAIR_ANCHOR_GAP_MAX_MS {
            log::info!(
                "[batch_match_diag] session_rejected v_anchor={} nearest_g_anchor={} gap_ms={} reason=anchor_gap_too_large",
                v_anchor,
                g_anchor,
                gap
            );
            continue;
        }

        // Duration cross-check: at least one (v, g) pair within the candidate
        // clusters must satisfy |g_dur - 0.5 + pre - v_dur| <= 1.5.
        let duration_ok = v_cluster.iter().any(|&vi| {
            g_cluster.iter().any(|&gj| {
                let v = &videos[vi];
                let g = &gyros[gj];
                let v_dur_s = v.duration_ms / 1000.0;
                let pre_s = v.pre_recording_ms / 1000.0;
                let g_dur_s = g.duration_ms / 1000.0;
                (g_dur_s - 0.5 + pre_s - v_dur_s).abs() <= SYNC_DURATION_OFFSET_MAX
            })
        });

        if !duration_ok {
            log::info!(
                "[batch_match_diag] session_rejected v_anchor={} g_anchor={} gap_ms={} reason=duration_mismatch",
                v_anchor,
                g_anchor,
                gap
            );
            continue;
        }

        log::info!(
            "[batch_match_diag] session_paired v_anchor={} g_anchor={} gap_ms={} v_size={} g_size={}",
            v_anchor,
            g_anchor,
            gap,
            v_cluster.len(),
            g_cluster.len()
        );
        sessions.push(Session {
            v_cluster: v_cluster.clone(),
            cal_video_indices: Vec::new(), // populated by compute_session_offset
            g_cluster: g_cluster.clone(),
            // Anchor = V anchor: each session's "centre" is where the cal
            // videos were taken. Used for assign_gyro_ownership (gyros snap
            // to nearest session by recording time) and fallback (videos
            // borrow from temporally nearest session).
            anchor_ms: *v_anchor,
            offset: 0,
            delay: 0,
            reliable: false,
        });
    }

    for s in &sessions {
        log::info!(
            "[batch_match_diag] session_built anchor={} v_size={} g_size={}",
            s.anchor_ms,
            s.v_cluster.len(),
            s.g_cluster.len()
        );
    }

    sessions
}

/// Compute (gyro_start_ms, gyro_end_ms, front_comp, calib_anchor_ms) for a
/// single matched (video, gyro, session) triple. Mirrors the legacy
/// assign_gyro_to_videos formula but takes the calibration anchor from a
/// caller-supplied list (the session's V cluster) instead of a global list.
fn compute_clip_window(
    v: &VideoMatchInfo,
    g: &GyroMatchInfo,
    v_created: i64,
    video_offset: i64,
    session_calib_indices: &[usize],
    videos: &[VideoMatchInfo],
) -> (f64, f64, f64, i64) {
    let video_start = g.created_at_ms - video_offset;
    let video_end = video_start + (g.duration_ms as i64);

    // Drift anchor: nearest calibration video strictly inside this gyro segment.
    let calib_anchor_ms = session_calib_indices
        .iter()
        .filter_map(|&ci| videos.get(ci).and_then(|cv| cv.created_at_ms))
        .filter(|&t| t >= video_start && t <= video_end)
        .min_by_key(|&t| (t - v_created).abs())
        .unwrap_or(video_start);
    let time_diff_from_calib = (v_created - calib_anchor_ms).abs() as f64;
    let drift_comp =
        (time_diff_from_calib * MAX_DAILY_DRIFT_MS / MS_PER_DAY).min(MAX_DAILY_DRIFT_MS);
    let front_comp = COMP_TIME_MS + drift_comp;
    let back_comp = COMP_TIME_MS + drift_comp;

    let gyro_start_ms = (v_created - video_start) as f64 - front_comp;
    let gyro_end_ms = gyro_start_ms + v.duration_ms + front_comp + back_comp;
    (gyro_start_ms, gyro_end_ms, front_comp, calib_anchor_ms)
}

/// For every gyro, assign it to the reliable session whose anchor is closest.
/// This partitions the gyro pool so each session's coverage check only sees
/// gyros that physically belong to its shooting day - even when sessions are
/// exactly one day apart (where a symmetric +/- 24h window would let day-1
/// gyros leak into a day-2 session check).
fn assign_gyro_ownership(gyros: &[GyroMatchInfo], sessions: &[Session]) -> Vec<Vec<usize>> {
    let mut owned: Vec<Vec<usize>> = vec![Vec::new(); sessions.len()];
    for (gi, g) in gyros.iter().enumerate() {
        let nearest = (0..sessions.len())
            .filter(|&sid| sessions[sid].reliable)
            .min_by_key(|&sid| (g.created_at_ms - sessions[sid].anchor_ms).abs());
        if let Some(sid) = nearest {
            owned[sid].push(gi);
        }
    }
    owned
}

/// Phase 4: assign each video to a session based on coverage. Returns the per-
/// video result list (status placeholders for not-yet-resolved videos) plus
/// the indices that still need fallback handling.
fn assign_videos_by_coverage(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    sessions: &[Session],
    owned_gyros: &[Vec<usize>],
) -> (Vec<MatchResult>, Vec<usize>) {
    let mut results: Vec<MatchResult> = Vec::with_capacity(videos.len());
    let mut pending: Vec<usize> = Vec::new();

    for (vi, v) in videos.iter().enumerate() {
        let v_created = match v.created_at_ms {
            Some(t) => t,
            None => {
                results.push(MatchResult {
                    video_index: vi,
                    job_id: None,
                    gyro_index: None,
                    status: MatchStatus::NoCreationTime,
                    global_offset_ms: None,
                    gyro_start_ms: None,
                    gyro_end_ms: None,
                    init_offset_ms: None,
                });
                continue;
            }
        };

        // Find every reliable session that covers this video; record
        // (session_id, gyro_id, depth). Coverage is restricted to gyros that
        // this session OWNS (nearest-anchor partitioning), so two sessions on
        // adjacent days don't both claim the same wall-clock coordinate.
        let mut hits: Vec<(usize, usize, i64)> = Vec::new();
        for (sid, s) in sessions.iter().enumerate() {
            if !s.reliable {
                continue;
            }
            let video_offset = s.offset - s.delay;
            for &gi in &owned_gyros[sid] {
                let g = &gyros[gi];
                let video_start = g.created_at_ms - video_offset;
                let video_end = video_start + (g.duration_ms as i64);
                if v_created >= video_start - COVERAGE_TOLERANCE_MS
                    && v_created <= video_end + COVERAGE_TOLERANCE_MS
                {
                    let depth =
                        (v_created - video_start).min(video_end - v_created);
                    hits.push((sid, gi, depth));
                }
            }
        }

        // Reduce hits down to per-session best (the deepest gyro hit in each session).
        hits.sort_by(|a, b| a.0.cmp(&b.0).then(b.2.cmp(&a.2)));
        let mut per_session_best: Vec<(usize, usize, i64)> = Vec::new();
        for h in hits {
            if per_session_best.last().map(|p| p.0) != Some(h.0) {
                per_session_best.push(h);
            }
        }

        if per_session_best.is_empty() {
            // Placeholder; fallback fills in later.
            results.push(MatchResult {
                video_index: vi,
                job_id: None,
                gyro_index: None,
                status: MatchStatus::Unmatched,
                global_offset_ms: None,
                gyro_start_ms: None,
                gyro_end_ms: None,
                init_offset_ms: None,
            });
            pending.push(vi);
            continue;
        }

        let (sid, gi, top_depth, ambiguous) = if per_session_best.len() == 1 {
            let h = per_session_best[0];
            (h.0, h.1, h.2, false)
        } else {
            // Pick the deepest.
            per_session_best.sort_by(|a, b| b.2.cmp(&a.2));
            let top = per_session_best[0];
            let second = per_session_best[1];
            let ambiguous = (top.2 - second.2).abs() < COVERAGE_DEPTH_AMBIGUITY_MS;
            (top.0, top.1, top.2, ambiguous)
        };

        log::info!(
            "[batch_match_diag] assign_coverage vi={} hits={} top_session={} top_gyro={} top_depth={}ms ambiguous={}",
            vi,
            per_session_best.len(),
            sid,
            gi,
            top_depth,
            ambiguous
        );

        if ambiguous {
            results.push(MatchResult {
                video_index: vi,
                job_id: None,
                gyro_index: None,
                status: MatchStatus::Unmatched,
                global_offset_ms: None,
                gyro_start_ms: None,
                gyro_end_ms: None,
                init_offset_ms: None,
            });
            pending.push(vi);
            continue;
        }

        let s = &sessions[sid];
        let video_offset = s.offset - s.delay;
        let g = &gyros[gi];
        let (gyro_start_ms, gyro_end_ms, front_comp, calib_anchor_ms) =
            compute_clip_window(v, g, v_created, video_offset, &s.cal_video_indices, videos);

        // A video is treated as a calibration pair only if it actually
        // contributed to the winning offset bucket (i.e. appeared in a
        // (v, g) pair whose offset landed in the chosen cluster). Videos in
        // the V cluster purely by duration heuristic (< 10s) but with no
        // matching cal gyro pair stay Matched so the render queue keeps
        // them in the output instead of Skipping them as calibration.
        let is_cal = s.cal_video_indices.contains(&vi);
        let status = if is_cal {
            MatchStatus::CalibrationPair
        } else {
            MatchStatus::Matched
        };

        log::info!(
            "[batch_match_diag] assign video_index={} gyro_index={} session={} status={} session_offset={}ms delay={}ms video_created={} gyro_created={} calib_anchor={} raw_range=[{:.1},{:.1}] duration={:.1}ms front={:.1}ms v_path='{}' g_path='{}'",
            vi,
            gi,
            sid,
            match status {
                MatchStatus::Matched => "matched",
                MatchStatus::CalibrationPair => "calibration",
                _ => "?",
            },
            s.offset,
            s.delay,
            v_created,
            g.created_at_ms,
            calib_anchor_ms,
            gyro_start_ms,
            gyro_end_ms,
            v.duration_ms,
            front_comp,
            v.path,
            g.path
        );

        results.push(MatchResult {
            video_index: vi,
            job_id: None,
            gyro_index: Some(gi),
            status,
            global_offset_ms: Some(s.offset),
            gyro_start_ms: Some(gyro_start_ms),
            gyro_end_ms: Some(gyro_end_ms),
            init_offset_ms: Some(-front_comp),
        });
    }

    (results, pending)
}

/// Phase 5: for every pending video, try to borrow the nearest reliable
/// session's offset (within +/- 24h of v.created_at). Hits become
/// MatchedFallback; misses remain Unmatched.
fn assign_fallback(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    sessions: &[Session],
    owned_gyros: &[Vec<usize>],
    pending: &[usize],
    results: &mut [MatchResult],
) {
    let reliable: Vec<usize> = sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| s.reliable)
        .map(|(i, _)| i)
        .collect();

    for &vi in pending {
        let v = &videos[vi];
        let v_created = match v.created_at_ms {
            Some(t) => t,
            None => continue, // already NoCreationTime
        };

        // Find the reliable session whose anchor is closest to v.
        let nearest = reliable
            .iter()
            .map(|&sid| {
                let gap = (sessions[sid].anchor_ms - v_created).abs();
                (sid, gap)
            })
            .min_by_key(|&(_, gap)| gap);

        let (sid, gap) = match nearest {
            Some(pair) => pair,
            None => continue, // no reliable session -> stays Unmatched
        };

        if gap > FALLBACK_MAX_GAP_MS {
            log::info!(
                "[batch_match_diag] fallback_skipped vi={} nearest_session={} gap_ms={} reason=over_24h",
                vi,
                sid,
                gap
            );
            continue;
        }

        let s = &sessions[sid];
        let video_offset = s.offset - s.delay;

        // Find the gyro inside the borrowed session whose [v_start, v_end]
        // covers (or is closest to) the video. Restrict to gyros owned by the
        // borrowed session (nearest-anchor partition) so we don't pick a gyro
        // from another day.
        let mut best_gyro: Option<(usize, i64)> = None; // (gyro_index, abs_distance_outside_window)
        for &gi in &owned_gyros[sid] {
            let g = &gyros[gi];
            let video_start = g.created_at_ms - video_offset;
            let video_end = video_start + (g.duration_ms as i64);
            let inside = v_created >= video_start - COVERAGE_TOLERANCE_MS
                && v_created <= video_end + COVERAGE_TOLERANCE_MS;
            let dist = if v_created < video_start {
                (video_start - v_created).abs()
            } else if v_created > video_end {
                (v_created - video_end).abs()
            } else {
                0
            };
            let cur_best_dist = best_gyro.map(|p| p.1).unwrap_or(i64::MAX);
            if (inside && cur_best_dist > 0) || dist < cur_best_dist {
                best_gyro = Some((gi, dist));
            }
        }

        let gi = match best_gyro {
            Some((g, _)) => g,
            None => continue,
        };
        let g = &gyros[gi];
        let (gyro_start_ms, gyro_end_ms, front_comp, calib_anchor_ms) =
            compute_clip_window(v, g, v_created, video_offset, &s.cal_video_indices, videos);

        log::info!(
            "[batch_match_diag] fallback_used vi={} borrow_session={} gap_ms={} gyro_index={} calib_anchor={} raw_range=[{:.1},{:.1}]",
            vi,
            sid,
            gap,
            gi,
            calib_anchor_ms,
            gyro_start_ms,
            gyro_end_ms
        );

        results[vi] = MatchResult {
            video_index: vi,
            job_id: None,
            gyro_index: Some(gi),
            status: MatchStatus::MatchedFallback,
            global_offset_ms: Some(s.offset),
            gyro_start_ms: Some(gyro_start_ms),
            gyro_end_ms: Some(gyro_end_ms),
            init_offset_ms: Some(-front_comp),
        };
    }
}

// --- T6: assign_gyro_to_videos (LEGACY path, kept for manual_pairs) ---

// Compensation time margin (ms). Base buffer added to both ends of every clip's
// gyro window. Sized to absorb typical external-IMU/camera clock offsets so the
// sync search has a consistent margin on both sides.
const COMP_TIME_MS: f64 = 1500.0;
// Maximum per-day drift compensation (ms).
const MAX_DAILY_DRIFT_MS: f64 = 1000.0;
// Milliseconds in a day.
const MS_PER_DAY: f64 = 86_400_000.0;

/// Legacy single-session assigner kept for the manual_pairs path. The auto
/// path now flows through assign_videos_by_coverage + assign_fallback.
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
                        init_offset_ms: None,
                    };
                }
            };

            let is_cal = calibration_video_indices.contains(&vi);

            for (gi, g) in gyros.iter().enumerate() {
                let video_start = g.created_at_ms - video_offset;
                let video_end = video_start + (g.duration_ms as i64);

                if v_created >= video_start - 1000 && v_created <= video_end + 1000 {
                    let calib_anchor_ms = calibration_video_indices
                        .iter()
                        .filter_map(|&ci| videos.get(ci).and_then(|cv| cv.created_at_ms))
                        .filter(|&t| t >= video_start && t <= video_end)
                        .min_by_key(|&t| (t - v_created).abs())
                        .unwrap_or(video_start);
                    let time_diff_from_calib = (v_created - calib_anchor_ms).abs() as f64;
                    let drift_comp = (time_diff_from_calib * MAX_DAILY_DRIFT_MS / MS_PER_DAY)
                        .min(MAX_DAILY_DRIFT_MS);
                    let front_comp = COMP_TIME_MS + drift_comp;
                    let back_comp = COMP_TIME_MS + drift_comp;
                    let legacy_video_start = g.created_at_ms - global_offset - delay;
                    let legacy_video_end = legacy_video_start + (g.duration_ms as i64);
                    let legacy_front_comp = (500.0 + drift_comp).min(1500.0);
                    let legacy_back_comp = 2000.0;

                    let gyro_start_ms = (v_created - video_start) as f64 - front_comp;
                    let gyro_end_ms = gyro_start_ms + v.duration_ms + front_comp + back_comp;
                    log::info!(
                        "[batch_match_diag] assign video_index={} gyro_index={} status={} global_offset={}ms delay={}ms video_created={} gyro_created={} current_video_start={} current_video_end={} legacy_video_start={} legacy_video_end={} calib_anchor={} time_from_anchor={:.1}ms drift={:.1}ms front={:.1}ms back={:.1}ms legacy_front={:.1}ms legacy_back={:.1}ms raw_range=[{:.1},{:.1}] duration={:.1}ms pre_recording={:.1}ms v_path='{}' g_path='{}'",
                        vi,
                        gi,
                        if is_cal { "calibration" } else { "matched" },
                        global_offset,
                        delay,
                        v_created,
                        g.created_at_ms,
                        video_start,
                        video_end,
                        legacy_video_start,
                        legacy_video_end,
                        calib_anchor_ms,
                        time_diff_from_calib,
                        drift_comp,
                        front_comp,
                        back_comp,
                        legacy_front_comp,
                        legacy_back_comp,
                        gyro_start_ms,
                        gyro_end_ms,
                        v.duration_ms,
                        v.pre_recording_ms,
                        v.path,
                        g.path
                    );

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
                        init_offset_ms: Some(-front_comp),
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
                init_offset_ms: None,
            }
        })
        .collect()
}

// --- T7: Manual calibration pair support ---

/// Compute offset from manually specified calibration pairs (legacy single-session).
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

    let cal_video_indices: Vec<usize> = manual_pairs.iter().map(|p| p.video_index).collect();
    let cal_gyro_indices: Vec<usize> = manual_pairs.iter().map(|p| p.gyro_index).collect();

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
/// If `manual_pairs` is provided (non-empty), uses manual calibration pairs and
/// goes through the legacy single-session path. Otherwise, runs the
/// multi-session pipeline: cluster -> pair -> compute_session_offset ->
/// coverage assign -> +/- 24h fallback.
pub fn batch_match(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    manual_pairs: Option<&[ManualCalibrationPair]>,
) -> BatchMatchResult {
    if let Some(pairs) = manual_pairs
        && !pairs.is_empty()
    {
        return match compute_from_manual_pairs(videos, gyros, pairs) {
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
            Err(e) => unmatched_results(videos, e),
        };
    }
    auto_match(videos, gyros)
}

/// Build an "everything unmatched" result for failure cases.
fn unmatched_results(videos: &[VideoMatchInfo], error: MatchError) -> BatchMatchResult {
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
            init_offset_ms: None,
        })
        .collect();
    BatchMatchResult {
        results,
        global_offset_ms: None,
        error: Some(error),
    }
}

/// Multi-session automatic calibration pipeline.
fn auto_match(videos: &[VideoMatchInfo], gyros: &[GyroMatchInfo]) -> BatchMatchResult {
    let v_clusters = find_calibration_videos(videos);
    let g_clusters = find_calibration_gyros(gyros);

    for (i, c) in v_clusters.iter().enumerate() {
        let anchor = cluster_anchor(c, videos);
        log::info!(
            "[batch_match_diag] cluster_detected kind=video idx={} size={} anchor={} indices={:?}",
            i,
            c.len(),
            anchor,
            c
        );
    }
    for (i, c) in g_clusters.iter().enumerate() {
        let anchor = cluster_anchor(c, gyros);
        log::info!(
            "[batch_match_diag] cluster_detected kind=gyro idx={} size={} anchor={} indices={:?}",
            i,
            c.len(),
            anchor,
            c
        );
    }

    if v_clusters.is_empty() || g_clusters.is_empty() {
        return unmatched_results(videos, MatchError::NoCalibrationPairsFound);
    }

    let mut sessions = pair_sessions(v_clusters, g_clusters, videos, gyros);

    if sessions.is_empty() {
        return unmatched_results(videos, MatchError::NoCalibrationPairsFound);
    }

    for s in sessions.iter_mut() {
        match compute_session_offset(videos, gyros, &s.v_cluster, &s.g_cluster) {
            Some((off, dly, spread, cal_videos)) => {
                s.offset = off;
                s.delay = dly;
                s.cal_video_indices = cal_videos;
                s.reliable = spread <= SYNC_CREATE_OFFSET_MAX;
                log::info!(
                    "[batch_match_diag] session_offset anchor={} offset={}ms delay={}ms spread={}ms reliable={}",
                    s.anchor_ms,
                    s.offset,
                    s.delay,
                    spread,
                    s.reliable
                );
            }
            None => {
                s.reliable = false;
                log::info!(
                    "[batch_match_diag] session_offset_failed anchor={} reason=no_candidate_pair",
                    s.anchor_ms
                );
            }
        }
    }

    let reliable_count = sessions.iter().filter(|s| s.reliable).count();
    if reliable_count == 0 {
        return unmatched_results(videos, MatchError::NoCalibrationPairsFound);
    }

    let owned_gyros = assign_gyro_ownership(gyros, &sessions);
    let (mut results, pending) =
        assign_videos_by_coverage(videos, gyros, &sessions, &owned_gyros);
    assign_fallback(videos, gyros, &sessions, &owned_gyros, &pending, &mut results);

    let global_offset_ms = if reliable_count == 1 {
        sessions
            .iter()
            .find(|s| s.reliable)
            .map(|s| s.offset)
    } else {
        None
    };

    BatchMatchResult {
        results,
        global_offset_ms,
        error: None,
    }
}

// =============================================================================
// Unit tests
// =============================================================================
#[cfg(test)]
mod tests {
    use super::*;

    fn v(idx: usize, dur: f64, created: Option<i64>) -> VideoMatchInfo {
        VideoMatchInfo {
            path: format!("v{}", idx),
            duration_ms: dur,
            created_at_ms: created,
            pre_recording_ms: 0.0,
        }
    }

    fn g(idx: usize, dur: f64, created: i64) -> GyroMatchInfo {
        GyroMatchInfo {
            path: format!("g{}", idx),
            duration_ms: dur,
            created_at_ms: created,
        }
    }

    // --- Phase 1 tests ---

    #[test]
    fn cluster_detection_single_day() {
        // 5 short videos, all consecutive within 30s
        let videos: Vec<VideoMatchInfo> = (0..5)
            .map(|i| v(i, 5_000.0, Some(1_000 + i as i64 * 30_000)))
            .collect();
        let clusters = find_calibration_videos(&videos);
        assert_eq!(clusters.len(), 1);
        assert_eq!(clusters[0], vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn cluster_detection_multi_day() {
        // day1: 3 videos within 30s; day2: 4 videos within 30s; gap >> 90s
        let mut videos: Vec<VideoMatchInfo> = Vec::new();
        for i in 0..3 {
            videos.push(v(i, 5_000.0, Some(1_000 + i as i64 * 30_000)));
        }
        for i in 0..4 {
            videos.push(v(
                3 + i,
                5_000.0,
                Some(1_000 + 86_400_000 + i as i64 * 30_000),
            ));
        }
        let clusters = find_calibration_videos(&videos);
        assert_eq!(clusters.len(), 2);
        assert_eq!(clusters[0], vec![0, 1, 2]);
        assert_eq!(clusters[1], vec![3, 4, 5, 6]);
    }

    #[test]
    fn cluster_anchor_median_odd() {
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(1_500)),
            v(2, 5_000.0, Some(2_000)),
        ];
        let cluster = vec![0, 1, 2];
        assert_eq!(cluster_anchor(&cluster, &videos), 1_500);
    }

    #[test]
    fn cluster_anchor_median_even() {
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(1_500)),
            v(2, 5_000.0, Some(2_000)),
            v(3, 5_000.0, Some(2_500)),
        ];
        let cluster = vec![0, 1, 2, 3];
        // Lower median: sorted = [1000, 1500, 2000, 2500], index (4-1)/2 = 1 -> 1500.
        assert_eq!(cluster_anchor(&cluster, &videos), 1_500);
    }

    // --- Phase 2 tests ---

    #[test]
    fn pair_same_day() {
        // V cluster anchor = 1000, G cluster anchor = 60000 (60s later), durations match.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 60_000),
            g(1, 5_500.0, 90_000),
        ];
        let sessions = pair_sessions(vec![vec![0, 1]], vec![vec![0, 1]], &videos, &gyros);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].v_cluster, vec![0, 1]);
        assert_eq!(sessions[0].g_cluster, vec![0, 1]);
    }

    #[test]
    fn pair_accepts_29min_rejects_31min_boundary() {
        // SESSION_PAIR_ANCHOR_GAP_MAX_MS = 30 minutes. Verify the boundary.
        let videos = vec![
            v(0, 5_000.0, Some(0)),
            v(1, 5_000.0, Some(30_000)),
        ];
        // 29-min gap: should pair.
        let gyros_29 = vec![
            g(0, 5_500.0, 29 * 60_000),
            g(1, 5_500.0, 29 * 60_000 + 30_000),
        ];
        let sessions_29 =
            pair_sessions(vec![vec![0, 1]], vec![vec![0, 1]], &videos, &gyros_29);
        assert_eq!(sessions_29.len(), 1, "29-min anchor gap must pair within 30-min threshold");

        // 31-min gap: should reject.
        let gyros_31 = vec![
            g(0, 5_500.0, 31 * 60_000),
            g(1, 5_500.0, 31 * 60_000 + 30_000),
        ];
        let sessions_31 =
            pair_sessions(vec![vec![0, 1]], vec![vec![0, 1]], &videos, &gyros_31);
        assert!(sessions_31.is_empty(), "31-min anchor gap must reject (above 30-min threshold)");
    }

    #[test]
    fn ransac_tie_break_uses_video_coverage_when_inlier_counts_tie() {
        // Construct a case where two distinct offset clusters tie on inlier
        // count (1 vs 1) but differ on geometric video coverage. The cluster
        // whose offset places more videos inside the gyro time windows wins.
        //
        // Setup:
        //   Cal videos v[0]/v[1] at created_at = 0 and 30000ms (V cluster).
        //   Cal gyros at:
        //     g[0]=200, g[1]=30200       -> offset cluster A = 200
        //     g[2]=1_000_000, g[3]=1_030_000 -> offset cluster B = 1_000_000
        //   (gaps inside G cluster are within ADJACENT_GYRO_GAP_MAX 60s so
        //    (g0,g1) and (g2,g3) both make valid adjacent G pairs.)
        //
        // BUT: g[1]->g[2] gap is 970 sec > 60s so (g1,g2) cross-pair is
        //   filtered, and find_consecutive_groups SPLITS them into two cal
        //   clusters since gap > 90s. To force them into a single G cluster
        //   for this isolated unit test we call compute_session_offset
        //   directly with the explicit g_cluster indices.
        //
        // We pad in many videos at low timestamps so offset 200 covers them
        // (gyro at 200, dur ~5s -> matches v[0] etc.). Offset 1_000_000
        // covers fewer.
        let mut videos = vec![v(0, 5_000.0, Some(0)), v(1, 5_000.0, Some(30_000))];
        // Pad 10 short videos near offset-200's window so coverage favours
        // cluster A. With test_offset=200 these all map inside g[0]/g[1].
        for i in 0..10 {
            videos.push(v(2 + i, 1_000.0, Some(i as i64 * 2_500)));
        }
        let gyros = vec![
            g(0, 5_500.0, 200),
            g(1, 5_500.0, 30_200),
            g(2, 5_500.0, 1_000_000),
            g(3, 5_500.0, 1_030_000),
        ];

        // The G cluster sort ADJACENT_GYRO_GAP_MAX (60s) filter inside
        // compute_session_offset will skip the (g1, g2) cross-pair, so we
        // get two distinct candidates: 200 and 1_000_000.
        let (offset, _delay, _spread, _cal_v) =
            compute_session_offset(&videos, &gyros, &[0, 1], &[0, 1, 2, 3])
                .expect("should produce candidates");
        // Both candidates have inlier count = 1; tie-break by coverage picks
        // the one that covers more videos -> the smaller offset (=200)
        // since the padded videos are near zero.
        assert_eq!(
            offset, 200,
            "coverage tie-break must pick offset whose video window matches more videos"
        );
    }

    #[test]
    fn pair_reject_22h_gap() {
        // V anchor = 1000, G anchor = 1000 + 22h = 79_201_000. Gap = 22h > 18h.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 1_000 + 22 * 3_600_000),
            g(1, 5_500.0, 31_000 + 22 * 3_600_000),
        ];
        let sessions = pair_sessions(vec![vec![0, 1]], vec![vec![0, 1]], &videos, &gyros);
        assert!(sessions.is_empty());
    }

    #[test]
    fn pair_three_v_two_g() {
        // 3 V clusters at day1 / day2 / day3, 2 G clusters at day1 / day2.
        // Expect 2 sessions; day3 V cluster left orphan.
        let day = 24 * 3_600_000;
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            v(2, 5_000.0, Some(1_000 + day)),
            v(3, 5_000.0, Some(31_000 + day)),
            v(4, 5_000.0, Some(1_000 + 2 * day)),
            v(5, 5_000.0, Some(31_000 + 2 * day)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 1_000),
            g(1, 5_500.0, 31_000),
            g(2, 5_500.0, 1_000 + day),
            g(3, 5_500.0, 31_000 + day),
        ];
        let sessions = pair_sessions(
            vec![vec![0, 1], vec![2, 3], vec![4, 5]],
            vec![vec![0, 1], vec![2, 3]],
            &videos,
            &gyros,
        );
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].v_cluster, vec![0, 1]);
        assert_eq!(sessions[1].v_cluster, vec![2, 3]);
    }

    #[test]
    fn pair_reject_duration_mismatch() {
        // V anchor and G anchor are 60s apart (within 10-min threshold), so
        // anchor check passes. V dur 9s vs G dur 2s -> |2 - 0.5 + 0 - 9| =
        // 7.5 >> 1.5 -> duration check rejects.
        let videos = vec![
            v(0, 9_000.0, Some(18 * 3_600_000)),
            v(1, 9_000.0, Some(18 * 3_600_000 + 30_000)),
        ];
        let gyros = vec![
            g(0, 2_000.0, 18 * 3_600_000 + 60_000),
            g(1, 2_000.0, 18 * 3_600_000 + 90_000),
        ];
        let sessions = pair_sessions(vec![vec![0, 1]], vec![vec![0, 1]], &videos, &gyros);
        assert!(sessions.is_empty());
    }

    // --- Phase 3 tests ---

    #[test]
    fn session_offset_reliable() {
        // Two cal pairs with offsets 1000 and 1200 (spread 200ms < 3s -> reliable).
        // V0 1000 / V1 31000; G0 2000 / G1 32200.
        // offset0 = 2000-1000 = 1000; offset1 = 32200-31000 = 1200; spread = 200.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_200),
        ];
        let result = compute_session_offset(&videos, &gyros, &[0, 1], &[0, 1]);
        let (offset, _delay, spread, _cal_videos) = result.expect("should succeed");
        assert_eq!(spread, 0); // Single avg per (vi,gi) pair -> single offset in candidates.
        // offset = (1000+1200)/2 = 1100
        assert_eq!(offset, 1100);
    }

    #[test]
    fn session_offset_picks_one_of_two_clusters() {
        // Two distinct candidate offset clusters; bucket-mode picks the
        // larger one (or first if same size). Renamed from
        // session_offset_unreliable since bucket-mode no longer flags
        // multi-cluster inputs as unreliable - it picks the majority cluster
        // and trusts its median.
        // that survive filters but with differing offsets. Easiest: V0 paired
        // with G0/G1 (offset ~1000), V0/V1 plus G2/G3 with offset ~6000
        // shifted by 5s. But pair iteration is (V_i, V_{i+1}) x (G_i, G_{i+1}).
        // To get TWO candidate offsets we need 3 V's and 3 G's where the
        // adjacent-pair offsets differ. Use V0/V1/V2 with stable 1000ms offset
        // but G0/G1 = 1000ms, G1/G2 = 6000ms (jump).
        // BUT: cross-pair offset0/offset1 consistency check (3s) will filter
        // them out at the per-pair level. Use a single cluster where each
        // ADJACENT (vi pair, gi pair) survives but candidates as a whole have
        // spread > 3s. That requires offset jumps inside one (vi, vi+1) /
        // (gi, gi+1) pair to be <3s but DIFFERENT pairs to land at different
        // offsets. Simulate: 3 V's and 3 G's, two non-overlapping V-pair x
        // G-pair combos with stable inner offsets but different inter-cluster
        // offsets.
        // V0=1000, V1=31000, V2=61000 (adjacent gap 30s, < 90s threshold)
        // G0=2000 (off=+1000), G1=32000 (off=+1000), G2=67000 (off=+6000)
        // Pair (V0,V1) x (G0,G1) -> avg 1000.
        // Pair (V1,V2) x (G1,G2) -> offset1=32000-31000=1000, offset2=67000-61000=6000.
        //   abs diff = 5000 > 3000 -> filtered out.
        // Pair (V0,V1) x (G1,G2) -> offset1=32000-1000=31000, offset2=67000-31000=36000.
        //   adj gyro gap = 35s, OK. dur diff fine. offset diff 5000 > 3000 -> filtered.
        // No way to get spread > 3s without each pair already being filtered.
        //
        // The spread metric is over surviving candidates. The natural way to get
        // multi-modal candidates is e.g., two stable offset clusters paired
        // across the V cluster: V0/V1 with G0/G1 (+1000) and V0/V1 with G2/G3
        // (+5000). Requires gi+1 - gi <= 60s. Use 4 G's: G0=2000, G1=32000,
        // G2=2000+5000=7000, G3=32000+5000=37000. Gyro gap G2-G3 = 30s OK.
        // But pair (G0,G1) has gap 30s OK. Pair (G2,G3) has gap 30s OK.
        // Pair (V0,V1)x(G0,G1) offset0=1000 off1=1000 avg=1000 spread=0 pass.
        // Pair (V0,V1)x(G2,G3) offset0=7000-1000=6000 off1=37000-31000=6000 avg=6000 pass.
        // Pair (V0,V1)x(G1,G2): G1=32000, G2=7000 -> reverse. gyro gap |7000-32000|=25000>60000? no, 25s OK.
        //   off0=32000-1000=31000 off1=7000-31000=-24000 diff 55000 > 3000 -> filter.
        // Pair (V0,V1)x(G0,G3): G0=2000, G3=37000 gap 35s OK. off0=1000, off1=37000-31000=6000, diff 5000 > 3000 filter.
        //
        // So we have two surviving candidates at offset 1000 and 6000. Spread = 5000 > 3000 -> unreliable.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 5_500.0, 7_000),
            g(3, 5_500.0, 37_000),
        ];
        let (offset, _delay, spread, _cal_videos) =
            compute_session_offset(&videos, &gyros, &[0, 1], &[0, 1, 2, 3])
                .expect("should pick a winner from one of the two clusters");
        // Bucket-mode picks one of the two single-member clusters.
        assert!(offset == 1000 || offset == 6000, "got offset={}", offset);
        // Spread within the chosen bucket is small (single member -> 0).
        assert!(spread <= SYNC_CREATE_OFFSET_MAX, "spread={}", spread);
    }

    // --- Phase 4 tests ---

    fn make_session(offset: i64, anchor: i64, v_cluster: Vec<usize>, g_cluster: Vec<usize>) -> Session {
        Session {
            cal_video_indices: v_cluster.clone(),
            v_cluster,
            g_cluster,
            anchor_ms: anchor,
            offset,
            delay: 0,
            reliable: true,
        }
    }

    #[test]
    fn assign_single_session_hit() {
        // One session, one gyro covering one normal video.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),   // cal
            v(1, 5_000.0, Some(31_000)),  // cal
            v(2, 6_000.0, Some(5_000)),   // regular video
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 20_000.0, 2_000),
        ];
        let sessions = vec![make_session(1_000, 1_000, vec![0, 1], vec![0, 1])];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let (results, pending) = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned);
        assert!(pending.is_empty(), "no pending expected, got {:?}", pending);
        let r2 = &results[2];
        assert_eq!(r2.status, MatchStatus::Matched);
        assert!(r2.gyro_index.is_some());
    }

    #[test]
    fn assign_two_sessions_deep_wins() {
        // Two sessions on different days. Probe video sits deep inside session
        // A's long gyro; session B's gyros are on day 2 and the 24h restriction
        // prevents cross-session false coverage.
        let day = 86_400_000i64;
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            v(2, 5_000.0, Some(1_000 + day)),
            v(3, 5_000.0, Some(31_000 + day)),
            // Probe video at day 1, 5s -> session A's long gyro covers it deeply.
            v(4, 1_000.0, Some(5_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 10_000.0, 1_000), // session A long gyro covering v4
            g(3, 5_500.0, 6_000 + day),
            g(4, 5_500.0, 36_000 + day),
        ];
        let sessions = vec![
            make_session(1_000, 1_000, vec![0, 1], vec![0, 1, 2]),
            // Session B with a small (5000ms) offset like a real clock drift,
            // anchor on day 2 - nearest-anchor partition keeps day 1 gyros in A.
            make_session(5_000, 1_000 + day, vec![2, 3], vec![3, 4]),
        ];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let (results, pending) = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned);
        assert!(pending.is_empty(), "no pending expected, got {:?}", pending);
        // v4 should be Matched (regular) via session A.
        let r4 = &results[4];
        assert_eq!(r4.status, MatchStatus::Matched);
        assert_eq!(r4.global_offset_ms, Some(1_000));
    }

    #[test]
    fn assign_two_sessions_ambiguous_fallback() {
        // Two sessions whose gyro windows independently cover v at near-equal
        // depth -> pushed to fallback path. Anchors are far apart so the
        // nearest-anchor partition splits the gyros cleanly between the two
        // sessions; both sessions still produce a hit on the same video.
        // session 0 offset=0 -> g0 v_start=4900, end=5100, depth=100.
        // session 1 offset=2000 -> g1 v_start=4950, end=5050, depth=50.
        // depth diff = 50 < COVERAGE_DEPTH_AMBIGUITY_MS (100) -> ambiguous.
        let videos = vec![v(0, 1_000.0, Some(5_000))];
        let gyros = vec![
            g(0, 200.0, 4_900),
            g(1, 100.0, 6_950),
        ];
        let sessions = vec![
            make_session(0, 3_000, vec![], vec![0]),
            make_session(2_000, 7_000, vec![], vec![1]),
        ];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let (_results, pending) = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned);
        assert_eq!(pending, vec![0]);
    }

    // --- Phase 5 tests ---

    #[test]
    fn matchstatus_matched_fallback_serde() {
        let s = serde_json::to_string(&MatchStatus::MatchedFallback).unwrap();
        assert_eq!(s, "\"MatchedFallback\"");
    }

    #[test]
    fn fallback_borrow_neighbor_day() {
        // Session A covers day1; video on day2 has no covering gyro on its own
        // day; the only reliable session is day1, anchor < 24h away.
        let videos = vec![v(0, 5_000.0, Some(20 * 3_600_000))]; // 20h after day1 anchor
        let gyros = vec![g(0, 1_000.0, 1_000)]; // tiny gyro, doesn't cover v
        let sessions = vec![make_session(1_000, 0, vec![], vec![0])];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let mut results = vec![MatchResult {
            video_index: 0,
            job_id: None,
            gyro_index: None,
            status: MatchStatus::Unmatched,
            global_offset_ms: None,
            gyro_start_ms: None,
            gyro_end_ms: None,
            init_offset_ms: None,
        }];
        assign_fallback(&videos, &gyros, &sessions, &owned, &[0], &mut results);
        assert_eq!(results[0].status, MatchStatus::MatchedFallback);
        assert_eq!(results[0].global_offset_ms, Some(1_000));
    }

    #[test]
    fn fallback_too_far_unmatched() {
        // Session anchor 0; video at 40h away -> > 36h -> Unmatched stays.
        let videos = vec![v(0, 5_000.0, Some(40 * 3_600_000))];
        let gyros = vec![g(0, 1_000.0, 1_000)];
        let sessions = vec![make_session(1_000, 0, vec![], vec![0])];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let mut results = vec![MatchResult {
            video_index: 0,
            job_id: None,
            gyro_index: None,
            status: MatchStatus::Unmatched,
            global_offset_ms: None,
            gyro_start_ms: None,
            gyro_end_ms: None,
            init_offset_ms: None,
        }];
        assign_fallback(&videos, &gyros, &sessions, &owned, &[0], &mut results);
        assert_eq!(results[0].status, MatchStatus::Unmatched);
    }

    #[test]
    fn fallback_unreliable_session_internal_video() {
        // Two sessions: A reliable but far, B unreliable and close to v.
        // v ends up pending (Phase 4 won't pick B because B is unreliable; A
        // doesn't cover v in its gyros), then fallback picks A.
        let videos = vec![v(0, 5_000.0, Some(20 * 3_600_000))];
        let gyros = vec![g(0, 1_000.0, 1_000), g(1, 1_000.0, 18 * 3_600_000)];
        let sessions = vec![
            Session {
                v_cluster: vec![],
                cal_video_indices: vec![],
                g_cluster: vec![1],
                anchor_ms: 18 * 3_600_000,
                offset: 1_000,
                delay: 0,
                reliable: false, // B unreliable
            },
            Session {
                v_cluster: vec![],
                cal_video_indices: vec![],
                g_cluster: vec![0],
                anchor_ms: 0,
                offset: 2_000,
                delay: 0,
                reliable: true, // A reliable, anchor 20h from v
            },
        ];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let mut results = vec![MatchResult {
            video_index: 0,
            job_id: None,
            gyro_index: None,
            status: MatchStatus::Unmatched,
            global_offset_ms: None,
            gyro_start_ms: None,
            gyro_end_ms: None,
            init_offset_ms: None,
        }];
        assign_fallback(&videos, &gyros, &sessions, &owned, &[0], &mut results);
        assert_eq!(results[0].status, MatchStatus::MatchedFallback);
        // borrowed offset is A's = 2000
        assert_eq!(results[0].global_offset_ms, Some(2_000));
    }

    // --- Phase 6 tests ---

    #[test]
    fn batch_match_single_day_equivalence() {
        // Single-day calibration + one normal video. Probe video and long
        // gyro must be longer than the cal thresholds (10s for video, 12s for
        // gyro) so they are NOT classified as calibration candidates.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            v(2, 60_000.0, Some(5_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 60_000.0, 2_000),
        ];
        let result = batch_match(&videos, &gyros, None);
        assert!(result.global_offset_ms.is_some());
        assert_eq!(result.results.len(), 3);
        // v0/v1 are calibration, v2 should be Matched.
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[2].status, MatchStatus::Matched);
    }

    #[test]
    fn batch_match_multi_day_independent_offsets() {
        let day = 86_400_000i64;
        let videos = vec![
            // Day 1 cal pair + one normal video
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            v(2, 60_000.0, Some(5_000)),
            // Day 2 cal pair + one normal video
            v(3, 5_000.0, Some(1_000 + day)),
            v(4, 5_000.0, Some(31_000 + day)),
            v(5, 60_000.0, Some(5_000 + day)),
        ];
        let gyros = vec![
            // Day 1 with offset = 1000ms
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 60_000.0, 2_000),
            // Day 2 with offset = 5000ms (different drift from day 1)
            g(3, 5_500.0, 6_000 + day),
            g(4, 5_500.0, 36_000 + day),
            g(5, 60_000.0, 6_000 + day),
        ];
        let result = batch_match(&videos, &gyros, None);
        // Two reliable sessions -> top-level global_offset_ms is None.
        assert!(
            result.global_offset_ms.is_none(),
            "expected None for multi-session, got {:?}",
            result.global_offset_ms
        );
        // v2 borrows day1 offset 1000, v5 borrows day2 offset 5000.
        assert_eq!(result.results[2].global_offset_ms, Some(1_000));
        assert_eq!(result.results[5].global_offset_ms, Some(5_000));
        // Both probe videos should be Matched (each lands in its own session's coverage).
        assert_eq!(result.results[2].status, MatchStatus::Matched);
        assert_eq!(result.results[5].status, MatchStatus::Matched);
    }

    #[test]
    fn batch_match_no_calibration_at_all_unmatched() {
        // Single video, no calibration clips.
        let videos = vec![v(0, 60_000.0, Some(5_000))];
        let gyros = vec![g(0, 60_000.0, 5_000)];
        let result = batch_match(&videos, &gyros, None);
        assert!(result.global_offset_ms.is_none());
        assert_eq!(result.results[0].status, MatchStatus::Unmatched);
    }

    #[test]
    fn day2_v4_cluster_picks_correct_offset_with_proxy_duplicates() {
        // Reproduces user's day-2 input from the log: 10 cal videos (5 unique
        // + 5 proxy dups at same created_at) and 4 cal gyros (2 unique + 2
        // proxy dups). The candidate offsets observed from the log:
        //   -197500, -192500 (delay=500), -197000, -201500, -196500
        // Bucket-mode (3s window) clusters them as:
        //   {-201500} | {-197500, -197000, -196500} | {-192500}
        // The 3-member middle cluster wins, median = -197000.
        //
        // Regression: before bucket-mode, coverage-tie-break could pick
        // -192500 (the lone delay=500 outlier), and spread metric tagged the
        // session as unreliable for the user's data.
        let g3_dur = 2301.0;
        let g4_dur = 4028.0;
        let g3_t = 1763702244000_i64;
        let g4_t = 1763702249000_i64;
        let videos = vec![
            v(0, 2202.2, Some(1763702441500)),
            v(1, 2202.2, Some(1763702441500)),
            v(2, 3153.2, Some(1763702445500)),
            v(3, 3153.2, Some(1763702445500)),
            v(4, 7057.1, Some(1763702495500)),
            v(5, 4804.8, Some(1763702516500)),
            v(6, 8508.5, Some(1763702542500)),
            v(7, 5855.9, Some(1763702571500)),
            v(8, 5155.2, Some(1763702613500)),
            v(9, 8008.0, Some(1763702629500)),
        ];
        let gyros = vec![
            g(0, g3_dur, g3_t),
            g(1, g4_dur, g4_t),
            g(2, g3_dur, g3_t),
            g(3, g4_dur, g4_t),
            g(4, 1_800_000.0, 1763702254000),
        ];

        let result = batch_match(&videos, &gyros, None);
        let offset = result
            .global_offset_ms
            .expect("single session should be reliable with bucket-mode");
        // Expect the majority cluster's median: -197000 (within tolerance).
        assert!(
            (-198000..=-196000).contains(&offset),
            "expected day-2 offset around -197000, got {}",
            offset
        );
        // Make sure we did NOT pick the lone -192500 outlier.
        assert_ne!(offset, -192500, "bucket-mode must avoid the delay=500 cross-pair outlier");
    }

    #[test]
    fn day1_v0_cluster_offset_is_minus_194500() {
        // User's day-1 input: 12 cal videos (DSC_1295..DSC_1306), 2 cal gyros
        // (12:52:44 and 12:52:48) producing a single candidate at -194500ms.
        let videos = vec![
            v(0, 951.0, Some(1763528158500)),
            v(1, 1901.9, Some(1763528162500)),
            v(2, 6606.6, Some(1763528216500)),
            v(3, 8358.4, Some(1763528261500)),
            v(4, 6356.4, Some(1763528293500)),
            v(5, 4554.6, Some(1763528354500)),
            v(6, 7757.8, Some(1763528373500)),
            v(7, 8908.9, Some(1763528430500)),
            v(8, 6706.7, Some(1763528506500)),
            v(9, 4704.7, Some(1763528544500)),
            v(10, 6256.2, Some(1763528561500)),
            v(11, 8208.2, Some(1763528578500)),
        ];
        let gyros = vec![
            g(0, 1726.0, 1763527964000),
            g(1, 2302.0, 1763527968000),
            g(2, 1_800_000.0, 1763527973000),
        ];
        let result = batch_match(&videos, &gyros, None);
        let offset = result
            .global_offset_ms
            .expect("day-1 session should be reliable");
        assert_eq!(offset, -194500, "day-1 offset must be -194500ms");
    }

    #[test]
    fn content_clip_inside_cal_v_cluster_is_not_calibration_pair() {
        // Reproduces user's day-2 V4 cluster behaviour:
        //   - 2 real cal videos (DSC_1392 2.2s, DSC_1393 3.15s) - SHOULD be CalibrationPair.
        //   - 6 content clips (DSC_1394 ~7s, ...) that happen to be < 10s and
        //     within 90s of the cal videos, so find_calibration_videos puts
        //     them in the same V cluster.
        // Cal gyros are 2.3s and 4s. Content clips at 5-8s do NOT match.
        // Without the per-video duration check, all 8 would be marked
        // CalibrationPair -> render_queue would Skip them as calibration,
        // dropping the user's content from the render queue.
        let g_t = 1763702244000_i64;
        let g_dur_short = 2301.0;
        let g_dur_long = 4028.0;
        let videos = vec![
            // Real cal videos
            v(0, 2202.2, Some(1763702441500)),
            v(1, 3153.2, Some(1763702445500)),
            // Content clips (in same cluster by gap < 90s but durations > cal)
            v(2, 7057.1, Some(1763702495500)),
            v(3, 4804.8, Some(1763702516500)),
            v(4, 8508.5, Some(1763702542500)),
            v(5, 5855.9, Some(1763702571500)),
        ];
        let gyros = vec![
            g(0, g_dur_short, g_t),
            g(1, g_dur_long, g_t + 5_000),
            g(2, 1_800_000.0, g_t + 10_000), // long IMU
        ];
        let result = batch_match(&videos, &gyros, None);
        // Real cal videos: CalibrationPair
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair,
            "v0 (2.2s) duration matches g[0] (2.3s) -> CalibrationPair");
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair,
            "v1 (3.15s) duration matches g[1] (4s) within 1.5s -> CalibrationPair");
        // Content clips: Matched (NOT CalibrationPair - would be Skipped)
        for vi in 2..=5 {
            assert_eq!(
                result.results[vi].status,
                MatchStatus::Matched,
                "v{} (content clip) must be Matched not CalibrationPair (would be Skipped by render_queue)",
                vi
            );
        }
    }

    #[test]
    fn one_day_missing_cal_falls_back_within_36h_unmatched_beyond() {
        // Scenario: day 1 has full cal (2 short cal videos + 2 cal gyros +
        // long IMU). Subsequent blocks have NO cal videos and NO cal gyros -
        // only long content videos + a long IMU.
        //
        // Algorithm should:
        //   - Form 1 reliable session (day 1)
        //   - Content videos WITHIN 36h of day-1 session anchor:
        //     borrow day-1 offset -> MatchedFallback
        //   - Content videos BEYOND 36h: Unmatched (per spec
        //     "宁可 Unmatched 也不错配")
        let hour: i64 = 3_600_000;
        let day1_anchor = 0_i64;
        let day1_offset: i64 = -180_000;

        let videos = vec![
            // Day-1 cal pair (matches cal gyros)
            v(0, 1_500.0, Some(day1_anchor)),
            v(1, 2_000.0, Some(day1_anchor + 30_000)),
            // Day-1 content
            v(2, 60_000.0, Some(day1_anchor + 5 * 60_000)),
            // Content 23h after day-1 (within 36h)
            v(3, 60_000.0, Some(day1_anchor + 23 * hour)),
            v(4, 30_000.0, Some(day1_anchor + 23 * hour + 60_000)),
            // Content 35h after day-1 (within 36h, just below boundary)
            v(5, 60_000.0, Some(day1_anchor + 35 * hour)),
            // Content 37h after day-1 (beyond 36h, Unmatched)
            v(6, 60_000.0, Some(day1_anchor + 37 * hour)),
            v(7, 30_000.0, Some(day1_anchor + 37 * hour + 60_000)),
        ];
        let gyros = vec![
            g(0, 1_500.0, day1_anchor + day1_offset),
            g(1, 2_000.0, day1_anchor + day1_offset + 30_000),
            g(2, 1_800_000.0, day1_anchor + day1_offset + 10_000),
            // Long IMUs for the post-cal content blocks (no cal in them).
            g(3, 5_400_000.0, day1_anchor + 23 * hour - 60_000),
            g(4, 5_400_000.0, day1_anchor + 35 * hour - 60_000),
            g(5, 5_400_000.0, day1_anchor + 37 * hour - 60_000),
        ];

        let result = batch_match(&videos, &gyros, None);

        // 1 reliable session (day-1).
        assert_eq!(
            result.global_offset_ms,
            Some(day1_offset),
            "single reliable session should report its offset as global"
        );

        // Day-1 cal pair.
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair);
        // Day-1 content matched via day-1 long IMU.
        assert_eq!(result.results[2].status, MatchStatus::Matched);

        // 23h and 35h content: MatchedFallback (borrows day-1 offset).
        for vi in [3usize, 4, 5] {
            assert_eq!(
                result.results[vi].status,
                MatchStatus::MatchedFallback,
                "v{} (within 36h) expected MatchedFallback, got {:?}",
                vi, result.results[vi].status
            );
            assert_eq!(
                result.results[vi].global_offset_ms,
                Some(day1_offset),
                "v{} should borrow day-1 offset",
                vi
            );
        }

        // 37h content: Unmatched (beyond 36h).
        for vi in [6usize, 7] {
            assert_eq!(
                result.results[vi].status,
                MatchStatus::Unmatched,
                "v{} (37h from day-1 anchor) expected Unmatched (beyond 36h fallback), got {:?}",
                vi, result.results[vi].status
            );
            assert_eq!(
                result.results[vi].global_offset_ms,
                None,
                "Unmatched videos must not have an offset"
            );
        }
    }

    #[test]
    fn user_real_data_full_replay_83_videos_10_gyros() {
        // Replays exactly what the user loaded (extracted from
        // gyroflow.log.1 17:41 run): 83 videos across 3 shooting blocks
        // (day1 1119, day2 afternoon 1121, day2 night 1121_night) plus 10
        // gyros (with 1121 / 1121_night proxy duplicates).
        //
        // Validates:
        //   - 2 reliable sessions form (day1 + day2)
        //   - day1 offset = -194500ms, day2 offset = -197000ms (different!)
        //   - cal_video_indices identifies the *actual* cal videos:
        //       Day1: vi=0,1 (DSC_1295/_1296 only) - 2.2s match
        //       Day2: vi=31..34 (DSC_1392+proxy / DSC_1393+proxy)
        //   - Content clips (vi 2..30, 35..72, 73..82) are Matched (NOT
        //     CalibrationPair) so render queue keeps them
        //   - Day2 night videos use day2 offset (not day1, not Unmatched)
        let videos = vec![
            // V0: day1 cal (12 short clips, DSC_1295..1306)
            v(0, 951.0,    Some(1763528158500)),
            v(1, 1901.9,   Some(1763528162500)),
            v(2, 6606.6,   Some(1763528216500)),
            v(3, 8358.4,   Some(1763528261500)),
            v(4, 6356.4,   Some(1763528293500)),
            v(5, 4554.6,   Some(1763528354500)),
            v(6, 7757.8,   Some(1763528373500)),
            v(7, 8908.9,   Some(1763528430500)),
            v(8, 6706.7,   Some(1763528506500)),
            v(9, 4704.7,   Some(1763528544500)),
            v(10, 6256.3,  Some(1763528561500)),
            v(11, 8208.2,  Some(1763528578500)),
            // Day1 content (DSC_1307..1325)
            v(12, 13263.3, Some(1763528592500)),
            v(13, 5705.7,  Some(1763528673500)),
            v(14, 5855.9,  Some(1763528801500)),
            v(15, 10310.3, Some(1763528847500)),
            v(16, 7057.1,  Some(1763528894500)),
            v(17, 5355.4,  Some(1763528913500)),
            v(18, 6106.1,  Some(1763529073500)),
            v(19, 4554.6,  Some(1763529090500)),
            v(20, 4354.4,  Some(1763529176500)),
            v(21, 5255.2,  Some(1763529195500)),
            v(22, 5205.2,  Some(1763529222500)),
            v(23, 5555.6,  Some(1763529234500)),
            v(24, 5655.7,  Some(1763529293500)),
            v(25, 7307.3,  Some(1763529325500)),
            v(26, 5055.1,  Some(1763529396500)),
            v(27, 9409.4,  Some(1763529493500)),
            v(28, 9109.1,  Some(1763529517500)),
            v(29, 5305.3,  Some(1763529577500)),
            v(30, 4604.6,  Some(1763529679500)),
            // Day2 V4 cluster (DSC_1392/_1393 + proxy duplicates from 1121_night)
            v(31, 2202.2,  Some(1763702441500)),
            v(32, 2202.2,  Some(1763702441500)),
            v(33, 3153.2,  Some(1763702445500)),
            v(34, 3153.2,  Some(1763702445500)),
            // Day2 afternoon content (DSC_1394..1431, no proxy dups)
            v(35, 7057.1,  Some(1763702495500)),
            v(36, 4804.8,  Some(1763702516500)),
            v(37, 8508.5,  Some(1763702542500)),
            v(38, 5855.9,  Some(1763702571500)),
            v(39, 5155.1,  Some(1763702613500)),
            v(40, 8008.0,  Some(1763702629500)),
            v(41, 5355.4,  Some(1763703065500)),
            v(42, 3553.6,  Some(1763703106500)),
            v(43, 8308.3,  Some(1763703127500)),
            v(44, 4504.5,  Some(1763703189500)),
            v(45, 12012.0, Some(1763703213500)),
            v(46, 6556.6,  Some(1763703312500)),
            v(47, 4904.9,  Some(1763703340500)),
            v(48, 1651.7,  Some(1763703362500)),
            v(49, 5305.3,  Some(1763703368500)),
            v(50, 4754.8,  Some(1763703417500)),
            v(51, 4954.9,  Some(1763703431500)),
            v(52, 4604.6,  Some(1763703525500)),
            v(53, 8958.9,  Some(1763703544500)),
            v(54, 6156.2,  Some(1763703563500)),
            v(55, 6856.9,  Some(1763703582500)),
            v(56, 4654.7,  Some(1763703653500)),
            v(57, 5105.1,  Some(1763703667500)),
            v(58, 7958.0,  Some(1763703682500)),
            v(59, 6606.6,  Some(1763703695500)),
            v(60, 5105.1,  Some(1763703727500)),
            v(61, 3553.6,  Some(1763703774500)),
            v(62, 8358.4,  Some(1763703792500)),
            v(63, 4854.8,  Some(1763703841500)),
            v(64, 4704.7,  Some(1763703851500)),
            v(65, 7307.3,  Some(1763703924500)),
            v(66, 3303.3,  Some(1763703937500)),
            v(67, 5705.7,  Some(1763703948500)),
            v(68, 5155.1,  Some(1763703962500)),
            v(69, 5455.4,  Some(1763703979500)),
            v(70, 3203.2,  Some(1763703989500)),
            v(71, 7757.8,  Some(1763703999500)),
            v(72, 5305.3,  Some(1763704014500)),
            // Day2 night (DSC_1432..1441, 1121_night folder)
            v(73, 2002.0,  Some(1763727602500)),
            v(74, 6906.9,  Some(1763727668500)),
            v(75, 7307.3,  Some(1763727691500)),
            v(76, 4704.7,  Some(1763727809500)),
            v(77, 6356.4,  Some(1763727836500)),
            v(78, 5655.7,  Some(1763727867500)),
            v(79, 4404.4,  Some(1763727895500)),
            v(80, 5205.2,  Some(1763727910500)),
            v(81, 5055.1,  Some(1763727967500)),
            v(82, 6706.7,  Some(1763728059500)),
        ];
        // 10 gyros (raw indices match log.1 17:41 mapping):
        let gyros = vec![
            // 0: day2 cal 13:17:24 (1121)
            g(0, 2301.0, 1763702244000),
            // 1: day2 cal 13:17:29 (1121)
            g(1, 4028.0, 1763702249000),
            // 2: day2 long IMU 13:17:34 (1121) - covers afternoon content
            g(2, 2_400_000.0, 1763702254000),
            // 3: day2 night IMU 19:59:09 - covers night clips
            g(3, 2_400_000.0, 1763726349000),
            // 4: day2 cal 13:17:24 (1121_night dup of #0)
            g(4, 2301.0, 1763702244000),
            // 5: day2 cal 13:17:29 (1121_night dup of #1)
            g(5, 4028.0, 1763702249000),
            // 6: day2 long IMU 13:17:34 (1121_night dup of #2)
            g(6, 2_400_000.0, 1763702254000),
            // 7: day1 cal 12:52:44 (1119)
            g(7, 1726.0, 1763527964000),
            // 8: day1 cal 12:52:48 (1119)
            g(8, 2302.0, 1763527968000),
            // 9: day1 long IMU 12:52:53 (1119) - covers day1 content
            g(9, 2_400_000.0, 1763527973000),
        ];

        let result = batch_match(&videos, &gyros, None);

        // Two reliable sessions -> multi-session, no single global offset.
        assert!(
            result.global_offset_ms.is_none(),
            "expected multi-session result, got global_offset={:?}",
            result.global_offset_ms
        );
        assert_eq!(result.results.len(), 83);

        // Day 1: vi 0..30 must use offset -194500.
        for vi in 0..=30 {
            assert_eq!(
                result.results[vi].global_offset_ms,
                Some(-194500),
                "vi={} (day1) expected offset -194500, got {:?}",
                vi,
                result.results[vi].global_offset_ms
            );
        }
        // Day 2: vi 31..82 must use offset -197000.
        for vi in 31..=82 {
            assert_eq!(
                result.results[vi].global_offset_ms,
                Some(-197000),
                "vi={} (day2) expected offset -197000, got {:?}",
                vi,
                result.results[vi].global_offset_ms
            );
        }

        // CalibrationPair status: only the videos whose (v, g) pair landed
        // in the inlier set should be marked. These are the *actual* cal
        // videos identified by the algorithm. Everything else stays Matched
        // so the render queue does NOT Skip them.
        let cal_vi: Vec<usize> = (0..videos.len())
            .filter(|&vi| result.results[vi].status == MatchStatus::CalibrationPair)
            .collect();

        // Print for user visibility - the test will show this in failure
        // message via the assertion below.
        let expected_cal: Vec<usize> = vec![0, 1, 31, 32, 33, 34];
        assert_eq!(
            cal_vi, expected_cal,
            "cal videos mismatch.\n  expected (real cal pairs only): {:?}\n  got: {:?}",
            expected_cal, cal_vi
        );

        // Sanity: every video should have a gyro assigned (no Unmatched).
        let unmatched: Vec<usize> = (0..videos.len())
            .filter(|&vi| {
                matches!(
                    result.results[vi].status,
                    MatchStatus::Unmatched | MatchStatus::NoCreationTime
                )
            })
            .collect();
        assert!(
            unmatched.is_empty(),
            "no video should be Unmatched, got {:?}",
            unmatched
        );
    }

    #[test]
    fn three_session_combined_uses_per_day_offsets() {
        // Day 1 (V0+G0) + Day 2 afternoon (V4+G1) + Day 2 night (V8 only, no
        // own cal gyro). When combined into a single batch_match call, day-1
        // videos must get -194500 and day-2 (incl. night) videos must get the
        // day-2 offset (around -197000), NOT day-1 leaking across days.
        let day1_g0_t = 1763527964000_i64;
        let day1_g1_t = 1763527968000_i64;
        let day1_imu_t = 1763527973000_i64;
        let day2_g0_t = 1763702244000_i64;
        let day2_g1_t = 1763702249000_i64;
        let day2_imu_t = 1763702254000_i64;
        let day2_night_imu_t = 1763726349000_i64;
        // Day-1 V0 cal videos (subset).
        let mut videos = vec![
            v(0, 951.0, Some(1763528158500)),
            v(1, 1901.9, Some(1763528162500)),
            v(2, 6606.6, Some(1763528216500)),
            // Day-1 long content
            v(3, 13_263.2, Some(1763528592500)),
            // Day-2 V4 cal videos with proxy dups
            v(4, 2202.2, Some(1763702441500)),
            v(5, 2202.2, Some(1763702441500)),
            v(6, 3153.2, Some(1763702445500)),
            v(7, 3153.2, Some(1763702445500)),
            v(8, 7057.1, Some(1763702495500)),
            v(9, 4804.8, Some(1763702516500)),
            // Day-2 long content
            v(10, 12_012.0, Some(1763703213500)),
        ];
        // Day-2 night videos (V8 cluster, no own cal gyro).
        videos.push(v(11, 2002.0, Some(1763727602500)));
        videos.push(v(12, 6906.9, Some(1763727668500)));
        videos.push(v(13, 7307.3, Some(1763727691500)));

        let gyros = vec![
            // Day 1
            g(0, 1726.0, day1_g0_t),
            g(1, 2302.0, day1_g1_t),
            g(2, 1_800_000.0, day1_imu_t),
            // Day 2 cal (with dups)
            g(3, 2301.0, day2_g0_t),
            g(4, 4028.0, day2_g1_t),
            g(5, 2301.0, day2_g0_t),
            g(6, 4028.0, day2_g1_t),
            // Day 2 long IMU
            g(7, 1_800_000.0, day2_imu_t),
            // Day 2 night IMU
            g(8, 1_800_000.0, day2_night_imu_t),
        ];

        let result = batch_match(&videos, &gyros, None);
        assert!(
            result.global_offset_ms.is_none(),
            "multi-session run should not have a single global offset"
        );

        // Day-1 cal videos use -194500.
        for vi in 0..=2 {
            assert_eq!(
                result.results[vi].global_offset_ms,
                Some(-194500),
                "v{} (day-1 cal) must use day-1 offset",
                vi
            );
        }
        // Day-1 long content also matched to day-1 long IMU with -194500.
        assert_eq!(
            result.results[3].global_offset_ms,
            Some(-194500),
            "v3 (day-1 content) must use day-1 offset"
        );

        // Day-2 cal videos use ~-197000 (bucket-mode majority cluster).
        let day2_offset = result.results[4]
            .global_offset_ms
            .expect("v4 should have offset");
        assert!(
            (-198000..=-196000).contains(&day2_offset),
            "v4 (day-2 cal) offset {} not in expected day-2 range",
            day2_offset
        );
        // CRITICAL: must differ from day-1 (the whole point of multi-session).
        assert_ne!(day2_offset, -194500, "day-2 must use its OWN offset, not day-1");

        // All day-2 (afternoon + night) videos use the same day-2 offset.
        for vi in 4..=13 {
            assert_eq!(
                result.results[vi].global_offset_ms,
                Some(day2_offset),
                "v{} must use day-2 offset {}",
                vi,
                day2_offset
            );
        }
    }

    #[test]
    fn batch_match_real_user_pattern_two_days_no_cross_day_offset() {
        // Regression test for the reported issue where day-2 videos were
        // assigned day-1 offset. Mimics the user's data layout:
        //   - Day 1: 1 short cal gyro cluster (G0) + 1 long IMU, several V
        //     clusters (V0 = true cal near G0; V1 = short non-cal videos
        //     15 min later, too far from G0 to pair).
        //   - Day 2: similar - G1 + V4 (cal) + V8 (night videos 7h after V4).
        //
        // Expectations:
        //   * Two reliable sessions form (day-1 G0+V0 and day-2 G1+V4).
        //   * V1 (15 min from G0, beyond 10-min threshold) gets orphaned but
        //     is still covered by the day-1 long IMU -> Matched with day-1 offset.
        //   * V8 night videos (7h from G1) gets orphaned but is covered by
        //     the day-2 night IMU -> Matched with DAY-2 offset, NOT day-1.
        let day = 86_400_000i64;
        let videos = vec![
            // V0: 3 day-1 cal videos around offset 0 (within cluster gap 90s)
            v(0, 951.0, Some(0)),
            v(1, 1901.9, Some(4_000)),
            v(2, 6606.6, Some(58_000)),
            // Day-1 long-form content (not in any V cluster, dur > 10s)
            v(3, 13_263.2, Some(400_000)),
            // V1: 2 day-1 short non-cal videos 15 min after cal (15*60_000)
            v(4, 7057.1, Some(15 * 60_000)),
            v(5, 5355.4, Some(15 * 60_000 + 19_000)),
            // V4: 3 day-2 cal videos
            v(6, 2202.2, Some(day)),
            v(7, 3153.2, Some(day + 4_000)),
            v(8, 7057.1, Some(day + 54_000)),
            // V8: 3 day-2 night videos 7h after V4 (orphan from G1)
            v(9, 2002.0, Some(day + 7 * 3_600_000)),
            v(10, 6906.9, Some(day + 7 * 3_600_000 + 66_000)),
            v(11, 7307.3, Some(day + 7 * 3_600_000 + 89_000)),
        ];
        let gyros = vec![
            // G0: 2 day-1 cal gyros (3 minutes before V0)
            g(0, 951.0, -180_000),
            g(1, 1901.9, -176_000),
            // Day-1 long IMU
            g(2, 1_800_000.0, -175_000),
            // G1: 2 day-2 cal gyros
            g(3, 2202.2, day - 200_000),
            g(4, 3153.2, day - 196_000),
            // Day-2 long IMU
            g(5, 1_800_000.0, day - 195_000),
            // Day-2 night IMU covering V8
            g(6, 1_800_000.0, day + 7 * 3_600_000 - 200_000),
        ];

        let result = batch_match(&videos, &gyros, None);

        // Two reliable sessions -> top-level global_offset_ms is None.
        assert!(
            result.global_offset_ms.is_none(),
            "expected None for multi-session, got {:?}",
            result.global_offset_ms
        );

        // V0 cal videos: only v[0]/v[1] match cal gyro durations (0.951s, 1.9s).
        // v[2] (6.6s) is in V cluster by duration but does NOT match cal gyro
        // duration, so it should be Matched (not CalibrationPair) - this
        // prevents it from being Skipped by the render queue.
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[2].status, MatchStatus::Matched);
        let day1_offset = result.results[0]
            .global_offset_ms
            .expect("v0 should have offset");

        // Day-1 long content (vi=3) matched via day-1 long IMU
        assert_eq!(
            result.results[3].global_offset_ms,
            Some(day1_offset),
            "v3 (day1 long content) must use day-1 offset"
        );

        // V1 non-cal (vi=4,5) - day-1 short content, covered by day-1 long IMU
        assert_eq!(
            result.results[4].global_offset_ms,
            Some(day1_offset),
            "v4 (day1 short, orphan V cluster) must use day-1 offset"
        );
        assert_eq!(
            result.results[5].global_offset_ms,
            Some(day1_offset),
            "v5 must use day-1 offset"
        );

        // V4 cal pair on day 2
        assert_eq!(result.results[6].status, MatchStatus::CalibrationPair);
        let day2_offset = result.results[6]
            .global_offset_ms
            .expect("v6 should have offset");

        // The whole point: day-1 and day-2 offsets MUST differ for this test
        // to catch the regression.
        assert_ne!(
            day1_offset, day2_offset,
            "day1 and day2 offsets must differ (both = {})",
            day1_offset
        );

        // V8 night videos (vi=9,10,11) MUST use day-2 offset, not day-1.
        for vi in 9..=11 {
            assert_eq!(
                result.results[vi].global_offset_ms,
                Some(day2_offset),
                "v{} (day2 night, orphan V cluster) MUST use day-2 offset, NOT day-1 (regression)",
                vi
            );
        }
    }

    #[test]
    fn batch_match_manual_pairs_unchanged() {
        // Manual pair path must behave exactly like the legacy single-session.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            v(2, 3_000.0, Some(5_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 10_000.0, 2_000),
        ];
        let pairs = vec![
            ManualCalibrationPair {
                job_id: 0,
                video_index: 0,
                gyro_index: 0,
            },
            ManualCalibrationPair {
                job_id: 1,
                video_index: 1,
                gyro_index: 1,
            },
        ];
        let result = batch_match(&videos, &gyros, Some(&pairs));
        assert!(result.global_offset_ms.is_some());
        // The manual path uses assign_gyro_to_videos -> v0/v1 should be CalibrationPair, v2 Matched.
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[2].status, MatchStatus::Matched);
    }
}
