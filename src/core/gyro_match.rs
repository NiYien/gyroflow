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

/// Input: an accepted deep-match result acting as a session-offset anchor
/// (render-queue-deep-gyro-match 7.2/7.3).
pub struct DeepMatchAnchor {
    /// Index into the `gyros` array passed to `batch_match` (the caller maps
    /// its gyro-pool index to this filtered index).
    pub gyro_index: usize,
    /// Index into the `videos` array passed to `batch_match` (same order as the
    /// caller's `job_ids`). Used by `assign_videos_by_coverage` to pin the
    /// deep-matched clip to its own deep gyro/offset before any coverage
    /// competition.
    pub video_index: usize,
    /// Deep-match offset in milliseconds, gyroflow sync convention: the video
    /// content start sits at gyro file-relative `-offset_ms`.
    pub offset_ms: f64,
    /// created_at of the deep-matched video (camera clock). `None` degrades
    /// the anchor to self-only: it does not influence any other video.
    pub video_created_at_ms: Option<i64>,
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

// Multi-session pairing tolerance: how far apart two sessions' measured
// camera<->IMU offsets may be and still be considered "consistent" (i.e.
// caused by clock drift rather than by a mis-pairing of V and G clusters).
//
// 15s covers physical drift ~3.5s/day plus measurement noise (a month-long
// shoot accumulates ~30s of drift but each pair_sessions invocation operates
// on a single batch, where drift is bounded by hours). A mis-pairing (e.g. V
// from day-1 cal paired with G from day-2 cal) by contrast produces an
// offset that differs by hours/days, so 15s tightly rejects cross-day
// mis-pairs without ever rejecting legit clock-drift.
const MULTI_SESSION_OFFSET_TOLERANCE_MS: i64 = 15_000;
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
    // Per-calibration-video paired cal gyro: `(video_index, gyro_index)` taken
    // from the winning inlier candidates' `gi_pair`. Used by
    // assign_videos_by_coverage to short-circuit cal videos to CalibrationPair
    // with a valid clip window, bypassing the 70% coverage gate.
    cal_pairs: Vec<(usize, usize)>,
    g_cluster: Vec<usize>,
    anchor_ms: i64,
    offset: i64,
    delay: i64,
    reliable: bool,
}

/// How to derive a per-(vi_pair, gi_pair) candidate's offset from its two
/// per-pair raw offsets.
///
/// - `Avg`: legacy behaviour, `(offset0 + offset1) / 2`. Balances measurement
///   noise across two well-paired cal videos so each one passes the
///   downstream `clip_bounds_ok` 70% coverage gate. Required for the common
///   "tightly paired cal" case where both per-pair offsets are genuine but
///   the user pressed gyro/camera with a few hundred ms of latency jitter.
/// - `SinglePick`: NiYien Tool semantics (auto_sync.cpp:134-142), keep only
///   the half with smaller `|dur_diff|` (the better-matching cal pair).
///   Required when `vi_pair` spans two real cal sub-sessions with different
///   offsets - the avg lands between the two true offsets and creates a
///   "bridge" candidate that pulls both sub-sessions into the same inlier
///   halo, inflating spread above the reliable gate.
///
/// Two-pass strategy in `compute_session_offset`: try `Avg` first; if the
/// resulting inlier spread exceeds the reliable gate (signal that the avg
/// hit a bimodal cluster), retry with `SinglePick` and take whichever pass
/// produces the smaller spread.
#[derive(Copy, Clone, Debug)]
enum CandidateOffsetMode {
    Avg,
    SinglePick,
}

/// Raw per-(vi_pair, gi_pair) candidate. Keeps both per-pair offsets and
/// dur_diffs so we can re-derive the final candidate offset in either
/// `CandidateOffsetMode` without re-running the filter loop.
#[derive(Copy, Clone, Debug)]
struct CandidateRaw {
    offset0: i64,
    offset1: i64,
    dur_diff0: f64,
    dur_diff1: f64,
    delay: i64,
    vi_pair: [usize; 2],
    // The cal gyros this video pair matched against. Propagated into the
    // session's `cal_pairs` so assign_videos_by_coverage can short-circuit cal
    // videos to CalibrationPair using their paired cal gyro.
    gi_pair: [usize; 2],
}

impl CandidateRaw {
    fn resolved_offset(&self, mode: CandidateOffsetMode) -> i64 {
        match mode {
            CandidateOffsetMode::Avg => (self.offset0 + self.offset1) / 2,
            CandidateOffsetMode::SinglePick => {
                if self.dur_diff0.abs() <= self.dur_diff1.abs() {
                    self.offset0
                } else {
                    self.offset1
                }
            }
        }
    }
}

/// Resolved offset for a single (v_cluster, g_cluster) session. Replaces the
/// former 6-tuple return so the added `cal_pairs` field does not push the tuple
/// past readability. `inlier_count` / `coverage` describe the chosen offset's
/// support and are consumed by the multi-cluster Pass 2 selector to avoid
/// locking onto a degenerate single-inlier candidate.
struct SessionOffset {
    offset: i64,
    delay: i64,
    spread: i64,
    cal_video_indices: Vec<usize>,
    cal_pairs: Vec<(usize, usize)>,
    inlier_count: usize,
    coverage: usize,
}

/// Compute offset/delay/spread for a single session.
///
/// Equivalent to the legacy compute_global_offset but scoped to one
/// (v_cluster, g_cluster) pair. Returns `Some(SessionOffset)` when at least one
/// candidate pair passes the duration / adjacency filters, or `None` when no
/// candidate survived.
fn compute_session_offset(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    v_cluster: &[usize],
    g_cluster: &[usize],
) -> Option<SessionOffset> {
    if v_cluster.len() < 2 || g_cluster.len() < 2 {
        return None;
    }

    // Raw candidate pool: keeps both per-pair offsets so the RANSAC pass can
    // re-derive each candidate's offset under either Avg or SinglePick mode.
    let mut raw_candidates: Vec<CandidateRaw> = Vec::new();

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
            let sp_offset = if dur_diff0.abs() <= dur_diff1.abs() {
                offset0
            } else {
                offset1
            };
            log::info!(
                "[batch_match_diag] candidate vi_pair=[{},{}] gi_pair=[{},{}] offset0={}ms offset1={}ms avg={}ms single_pick={}ms delay={}ms dur_diff=[{:.3},{:.3}] total_diff=[{:.3},{:.3}] v_paths=['{}','{}'] g_paths=['{}','{}']",
                v_cluster[vi],
                v_cluster[vi + 1],
                g_cluster[gi],
                g_cluster[gi + 1],
                offset0,
                offset1,
                avg_offset,
                sp_offset,
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

            raw_candidates.push(CandidateRaw {
                offset0,
                offset1,
                dur_diff0,
                dur_diff1,
                delay,
                vi_pair: [v_cluster[vi], v_cluster[vi + 1]],
                gi_pair: [g_cluster[gi], g_cluster[gi + 1]],
            });
        }
    }

    if raw_candidates.is_empty() {
        return None;
    }

    // Pass 1 - `Avg`: legacy/balanced semantics. Right for "tightly paired
    // cal" cases where 2 well-paired cal videos differ by a few hundred ms
    // of user-induced latency jitter - the averaged offset lands at the
    // midpoint of the two true per-pair offsets so BOTH videos pass the
    // downstream `clip_bounds_ok` 70% coverage gate.
    let result_avg = select_session_from_candidates(
        &raw_candidates,
        videos,
        gyros,
        CandidateOffsetMode::Avg,
    );

    if let Some(avg) = &result_avg {
        if avg.spread <= SYNC_CREATE_OFFSET_MAX {
            return result_avg;
        }
    }

    // Pass 2 - `SinglePick` (NiYien Tool auto_sync.cpp:134-142). Triggered
    // when pass 1's RANSAC inlier spread blew past the reliable gate - the
    // bimodal signature: a `vi_pair` straddling two real cal sub-sessions
    // produced a "bridge" candidate at the arithmetic midpoint of the two
    // true offsets, pulling both sub-sessions into the same inlier halo
    // and inflating spread. Single-pick stores per-video truth offsets
    // (the half with smaller `|dur_diff|`), so the bridge collapses onto
    // whichever sub-session's offset is dominant in the candidate pool.
    //
    // Whichever pass produces the smaller spread wins. Pass 1 ties take
    // precedence (SinglePick must STRICTLY improve to be used) so tight-pair
    // cases where both passes coincide stay on the coverage-balanced Avg
    // result.
    let result_sp = select_session_from_candidates(
        &raw_candidates,
        videos,
        gyros,
        CandidateOffsetMode::SinglePick,
    );

    match (result_avg, result_sp) {
        (Some(a), Some(s)) if s.spread < a.spread => Some(s),
        (Some(a), _) => Some(a),
        (None, sp) => sp,
    }
}

/// RANSAC mode finder + median + spread, parameterized by how each
/// `CandidateRaw` resolves to a scalar offset. Shared between the
/// `Avg` and `SinglePick` passes in `compute_session_offset`.
///
/// For each candidate we treat its offset as a hypothesis and count how
/// many other candidates fall within SYNC_CREATE_OFFSET_MAX (= inlier
/// count, robust to proxy duplicates and cross-pair noise). Highest
/// inlier count wins; ties resolved by which offset, applied across all
/// gyros, covers the most videos geometrically.
fn select_session_from_candidates(
    raw: &[CandidateRaw],
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    mode: CandidateOffsetMode,
) -> Option<SessionOffset> {
    if raw.is_empty() {
        return None;
    }

    // Resolve each raw candidate's offset under this mode while keeping a
    // back-pointer to its raw record (for delay-majority counting and
    // cal-video collection).
    let mut candidates: Vec<(i64, &CandidateRaw)> = raw
        .iter()
        .map(|r| (r.resolved_offset(mode), r))
        .collect();
    candidates.sort_by_key(|c| c.0);

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

    let chosen_idx = if mode_indices.len() == 1 {
        mode_indices[0]
    } else {
        let mut best_idx = mode_indices[0];
        let mut best_coverage = coverage(candidates[best_idx].0);
        for &i in mode_indices.iter().skip(1) {
            let cov = coverage(candidates[i].0);
            log::info!(
                "[batch_match_diag] tie_break mode={:?} candidate_offset={} inlier_count={} coverage={}",
                mode,
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

    let chosen_center = candidates[chosen_idx].0;
    let inliers: Vec<&(i64, &CandidateRaw)> = candidates
        .iter()
        .filter(|c| (c.0 - chosen_center).abs() <= SYNC_CREATE_OFFSET_MAX)
        .collect();
    let mut inlier_offsets: Vec<i64> = inliers.iter().map(|c| c.0).collect();
    inlier_offsets.sort();
    let median_offset = inlier_offsets[inlier_offsets.len() / 2];

    let delay_500_count = inliers.iter().filter(|c| c.1.delay == 500).count();
    let delay = if delay_500_count * 2 > inliers.len() {
        500
    } else {
        0
    };

    let spread = inlier_offsets.last().copied().unwrap_or(median_offset)
        - inlier_offsets.first().copied().unwrap_or(median_offset);

    let mut cal_videos: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    for c in &inliers {
        cal_videos.insert(c.1.vi_pair[0]);
        cal_videos.insert(c.1.vi_pair[1]);
    }
    let cal_video_indices: Vec<usize> = cal_videos.into_iter().collect();

    // Per-cal-video paired gyro from the inliers' `gi_pair`. When the same cal
    // video appears in multiple inlier candidates, keep the gyro from the pair
    // with the smaller |dur_diff| (the better duration match). Drives the
    // CalibrationPair short-circuit in assign_videos_by_coverage.
    let mut cal_pair_map: std::collections::BTreeMap<usize, (usize, f64)> =
        std::collections::BTreeMap::new();
    for c in &inliers {
        let r = c.1;
        for (vi, gi, dd) in [
            (r.vi_pair[0], r.gi_pair[0], r.dur_diff0.abs()),
            (r.vi_pair[1], r.gi_pair[1], r.dur_diff1.abs()),
        ] {
            cal_pair_map
                .entry(vi)
                .and_modify(|e| {
                    if dd < e.1 {
                        *e = (gi, dd);
                    }
                })
                .or_insert((gi, dd));
        }
    }
    let cal_pairs: Vec<(usize, usize)> = cal_pair_map
        .into_iter()
        .map(|(vi, (gi, _))| (vi, gi))
        .collect();

    log::info!(
        "[batch_match_diag] selected mode={:?} global_offset={}ms delay={}ms inlier_count={}/{} ties={} spread_ms={} cal_videos={:?} all_candidates={:?}",
        mode,
        median_offset,
        delay,
        inliers.len(),
        n,
        mode_indices.len(),
        spread,
        cal_video_indices,
        candidates.iter().map(|c| c.0).collect::<Vec<_>>()
    );

    // Surface the chosen offset's support metrics so the multi-cluster Pass 2
    // selector can prefer geometric coverage / inlier count over a degenerate
    // single-inlier spread (which is trivially 0 and otherwise wins).
    let chosen_inlier_count = inliers.len();
    let chosen_coverage = coverage(chosen_center);

    Some(SessionOffset {
        offset: median_offset,
        delay,
        spread,
        cal_video_indices,
        cal_pairs,
        inlier_count: chosen_inlier_count,
        coverage: chosen_coverage,
    })
}

/// Mini-RANSAC anchor pool: collects measured offsets from locked-in sessions
/// and gates new candidates against the running median.
///
/// `is_inlier(candidate)` semantics:
///   - empty pool -> true (no reference; refuse to reject and rely on the
///     Layer-3 clip bounds gate as a downstream safety net);
///   - non-empty -> |candidate - median| <= MULTI_SESSION_OFFSET_TOLERANCE_MS.
///
/// Median uses `select_nth_unstable` (O(n)) rather than a full sort because
/// `pair_sessions` is on the batch_match hot path and the pool can grow with
/// V cluster count.
struct AnchorPool {
    offsets: Vec<i64>,
}

impl AnchorPool {
    fn new() -> Self {
        Self { offsets: Vec::new() }
    }

    fn push(&mut self, offset: i64) {
        self.offsets.push(offset);
    }

    fn median(&self) -> Option<i64> {
        if self.offsets.is_empty() {
            return None;
        }
        let mut v = self.offsets.clone();
        let mid = v.len() / 2;
        v.select_nth_unstable(mid);
        Some(v[mid])
    }

    fn is_inlier(&self, candidate: i64) -> bool {
        match self.median() {
            None => true,
            Some(m) => (candidate - m).abs() <= MULTI_SESSION_OFFSET_TOLERANCE_MS,
        }
    }
}

/// Pair V/G clusters into sessions.
///
/// Two-clock principle: V anchors live in camera clock, G anchors in IMU
/// clock; the absolute offset between the two clocks is *what calibration is
/// measuring*, so we MUST NOT pre-assume it is small.
///
/// - Single V + single G: pair unconditionally (Branch A).
/// - Multi V or multi G: enumerate all (V, G) candidates where
///   `duration_ok` + `compute_session_offset` succeed (Branch B). Each V
///   cluster is then assigned to exactly one of its candidates:
///     Pass 1: V clusters with a single candidate are auto-locked; their
///       measured offset seeds the anchor pool.
///     Pass 2: V clusters with multiple candidates pick the candidate
///       whose offset is closest to `median(anchor_pool)`, accepted only
///       if the gap is within `MULTI_SESSION_OFFSET_TOLERANCE_MS` (i.e.
///       consistent with clock drift, not a mis-pair across cal events).
///
/// Both branches rely on:
///   - `duration_ok` (|g_dur - 0.5 + pre - v_dur| <= 1.5s) to weed out
///     unrelated V/G pairs.
///   - `compute_session_offset`'s internal RANSAC + |offset0 - offset1|
///     <= 3s check to validate intra-session offset consistency.
///
/// V clusters with no surviving candidate (or whose only candidate is
/// inconsistent with the anchor pool) orphan; their videos fall through
/// to `assign_fallback` and borrow the nearest reliable session's offset.
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

    let duration_ok = |v_cluster: &[usize], g_cluster: &[usize]| -> bool {
        v_cluster.iter().any(|&vi| {
            g_cluster.iter().any(|&gj| {
                let v = &videos[vi];
                let g = &gyros[gj];
                let v_dur_s = v.duration_ms / 1000.0;
                let pre_s = v.pre_recording_ms / 1000.0;
                let g_dur_s = g.duration_ms / 1000.0;
                (g_dur_s - 0.5 + pre_s - v_dur_s).abs() <= SYNC_DURATION_OFFSET_MAX
            })
        })
    };

    let make_session = |v_anchor: i64, v_cluster: &[usize], g_cluster: &[usize]| Session {
        v_cluster: v_cluster.to_vec(),
        cal_video_indices: Vec::new(),
        cal_pairs: Vec::new(),
        g_cluster: g_cluster.to_vec(),
        // Anchor = V anchor: each session's "centre" is where the cal videos
        // were taken (camera clock). Used by assign_gyro_ownership (gyros snap
        // to nearest session by anchor distance - cross-frame, see TODO in
        // that function) and assign_fallback (intra-camera-clock distance).
        anchor_ms: v_anchor,
        offset: 0,
        delay: 0,
        reliable: false,
    };

    let mut sessions: Vec<Session> = Vec::new();

    // Branch A: single V + single G. Pair unconditionally - the absolute gap
    // IS the unknown camera<->IMU offset and may be arbitrarily large.
    if v_with_anchor.len() == 1 && g_with_anchor.len() == 1 {
        let (v_cluster, v_anchor) = &v_with_anchor[0];
        let (g_cluster, g_anchor) = &g_with_anchor[0];
        let gap = (g_anchor - v_anchor).abs();
        if !duration_ok(v_cluster, g_cluster) {
            log::info!(
                "[batch_match_diag] session_rejected_single v_anchor={} g_anchor={} gap_ms={} reason=duration_mismatch",
                v_anchor,
                g_anchor,
                gap
            );
            return sessions;
        }
        if gap > 30 * 60_000 {
            log::info!(
                "[batch_match_diag] anchor_gap_large gap_ms={} hint=cross_frame_offset_detected",
                gap
            );
        }
        log::info!(
            "[batch_match_diag] session_paired_single v_anchor={} g_anchor={} gap_ms={} v_size={} g_size={}",
            v_anchor,
            g_anchor,
            gap,
            v_cluster.len(),
            g_cluster.len()
        );
        sessions.push(make_session(*v_anchor, v_cluster, g_cluster));
        for s in &sessions {
            log::info!(
                "[batch_match_diag] session_built anchor={} v_size={} g_size={}",
                s.anchor_ms,
                s.v_cluster.len(),
                s.g_cluster.len()
            );
        }
        return sessions;
    }

    // Branch B: multi-cluster - enumerate every (V, G) pair where duration_ok
    // AND compute_session_offset both pass. Then resolve V <-> G assignment
    // via offset-consistency (sessions on the same camera/IMU pair should
    // produce nearly identical offsets, modulo drift).
    #[derive(Clone)]
    struct Cand {
        v_idx: usize,
        g_idx: usize,
        offset: i64,
        spread: i64,
        // Support metrics for the chosen offset, used by Pass 2 selection so a
        // degenerate single-inlier spread=0 cannot outrank a higher-coverage,
        // better-supported candidate (cross-day cal-cluster mis-lock repro).
        inlier_count: usize,
        coverage: usize,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for v_idx in 0..v_with_anchor.len() {
        for g_idx in 0..g_with_anchor.len() {
            let (v_cluster, v_anchor) = &v_with_anchor[v_idx];
            let (g_cluster, g_anchor) = &g_with_anchor[g_idx];
            if !duration_ok(v_cluster, g_cluster) {
                continue;
            }
            match compute_session_offset(videos, gyros, v_cluster, g_cluster) {
                Some(so) => {
                    log::info!(
                        "[batch_match_diag] candidate_session v_idx={} g_idx={} v_anchor={} g_anchor={} offset={} spread={} inlier={} coverage={}",
                        v_idx,
                        g_idx,
                        v_anchor,
                        g_anchor,
                        so.offset,
                        so.spread,
                        so.inlier_count,
                        so.coverage
                    );
                    cands.push(Cand {
                        v_idx,
                        g_idx,
                        offset: so.offset,
                        spread: so.spread,
                        inlier_count: so.inlier_count,
                        coverage: so.coverage,
                    });
                }
                None => {
                    log::info!(
                        "[batch_match_diag] candidate_rejected v_idx={} g_idx={} reason=no_offset_candidate",
                        v_idx,
                        g_idx
                    );
                }
            }
        }
    }

    // Group candidate indices by v_idx, BTreeMap for deterministic order.
    let mut per_v: std::collections::BTreeMap<usize, Vec<usize>> =
        std::collections::BTreeMap::new();
    for (ci, c) in cands.iter().enumerate() {
        per_v.entry(c.v_idx).or_default().push(ci);
    }

    // chosen: v_idx -> candidate index. anchor_pool: locked-in offsets used
    // as the running consistency reference for both Pass 2 (multi-candidate
    // RANSAC) and Pass 1 (single-candidate gated).
    let mut chosen: std::collections::BTreeMap<usize, usize> =
        std::collections::BTreeMap::new();
    let mut anchor_pool = AnchorPool::new();

    // Pass 1 (runs first): V clusters with exactly one candidate G cluster.
    // Single-candidate clusters carry no within-V ambiguity (only one G can
    // possibly match by duration), so their RANSAC-derived offsets are the
    // strongest seeds for the anchor pool. The FIRST single-candidate V
    // bootstraps the pool unconditionally; subsequent single-candidate V's
    // are gated by `is_inlier` against the running pool median. This is
    // the core defence against cross-day mis-pair scenarios where a day-2
    // V cluster's only G candidate is the wrong day's gyro (reproducer sid
    // 45b144da: day-1 V × day-1 G ≈ -194500, day-2 V × day-1 G ≈ -48h ->
    // outlier, orphan).
    //
    // Empty-pool degenerate behaviour: first cluster passes through
    // `is_inlier == true` (no reference) and seeds the pool. Downstream
    // Layer-3 clip bounds gate catches false-positives that slip through
    // the empty-pool start.
    for (v_idx, cis) in &per_v {
        if cis.len() != 1 {
            continue;
        }
        let ci = cis[0];
        let candidate_offset = cands[ci].offset;
        if anchor_pool.is_inlier(candidate_offset) {
            chosen.insert(*v_idx, ci);
            let median_str = match anchor_pool.median() {
                Some(m) => m.to_string(),
                None => "none".to_string(),
            };
            log::info!(
                "[batch_match_diag] session_locked_pass1 v_idx={} g_idx={} offset={} median={}",
                v_idx,
                cands[ci].g_idx,
                candidate_offset,
                median_str
            );
            anchor_pool.push(candidate_offset);
        } else {
            let median = anchor_pool.median().unwrap_or(0);
            let delta = (candidate_offset - median).abs();
            log::info!(
                "[batch_match_diag] session_rejected v_idx={} g_idx={} candidate_offset={} median={} delta_ms={} reason=anchor_pool_outlier",
                v_idx,
                cands[ci].g_idx,
                candidate_offset,
                median,
                delta
            );
        }
    }

    // Pass 2 (runs second): V clusters with multiple candidate G clusters.
    // The cluster ambiguity (which G is correct?) is resolved by the
    // anchor pool seeded from Pass 1. Selection rule:
    //   - Pool empty: pick smallest measurement spread (tie-break g_idx
    //     ascending). This bootstraps when no single-candidate V exists.
    //   - Pool non-empty: pick the candidate whose offset is closest to
    //     the running pool median. Then gate by is_inlier; if even the
    //     closest candidate is outside MULTI_SESSION_OFFSET_TOLERANCE_MS,
    //     orphan the whole V cluster.
    for (v_idx, cis) in &per_v {
        if cis.len() <= 1 {
            continue;
        }
        let pick = if anchor_pool.median().is_none() {
            // Empty-pool bootstrap: no running reference yet, so pick the
            // candidate with the strongest geometric support. Coverage (how
            // many videos the offset places inside a gyro window) and inlier
            // count come FIRST; spread is only a final tie-break. A
            // single-inlier candidate has a degenerate spread=0 that must NOT
            // outrank a higher-coverage, multi-inlier candidate. Repro
            // (feedback bf9c062f): cross-day cal cluster cov=3/inlier=1/
            // spread=0 vs contemporaneous cluster cov=33/inlier=2/spread=400
            // -> the latter is correct; the old `min_by_key(spread)` locked
            // the former (-4.79 day offset) and starved 73/75 clips of gyro.
            *cis.iter()
                .min_by_key(|&&ci| {
                    (
                        std::cmp::Reverse(cands[ci].coverage),
                        std::cmp::Reverse(cands[ci].inlier_count),
                        cands[ci].spread,
                        cands[ci].g_idx,
                    )
                })
                .unwrap()
        } else {
            let median = anchor_pool.median().unwrap();
            *cis.iter()
                .min_by_key(|&&ci| (cands[ci].offset - median).abs())
                .unwrap()
        };
        let pick_offset = cands[pick].offset;
        if anchor_pool.is_inlier(pick_offset) {
            chosen.insert(*v_idx, pick);
            let median_str = match anchor_pool.median() {
                Some(m) => m.to_string(),
                None => "none".to_string(),
            };
            log::info!(
                "[batch_match_diag] session_locked_pass2 v_idx={} g_idx={} offset={} spread={} coverage={} inlier={} median={} cand_count={}",
                v_idx,
                cands[pick].g_idx,
                pick_offset,
                cands[pick].spread,
                cands[pick].coverage,
                cands[pick].inlier_count,
                median_str,
                cis.len()
            );
            anchor_pool.push(pick_offset);
        } else {
            let median = anchor_pool.median().unwrap_or(0);
            let delta = (pick_offset - median).abs();
            log::info!(
                "[batch_match_diag] session_rejected v_idx={} best_offset={} median={} delta_ms={} cand_count={} reason=anchor_pool_outlier_pass2",
                v_idx,
                pick_offset,
                median,
                delta,
                cis.len()
            );
        }
    }

    // Build sessions in v_idx order (BTreeMap preserves ascending key order).
    for (&_v_idx, &ci) in &chosen {
        let c = &cands[ci];
        let (v_cluster, v_anchor) = &v_with_anchor[c.v_idx];
        let (g_cluster, _g_anchor) = &g_with_anchor[c.g_idx];
        sessions.push(make_session(*v_anchor, v_cluster, g_cluster));
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

// --- render-queue-deep-gyro-match 7.1: deep-match session anchor conversion ---

/// Convert an accepted deep-match offset into the session-offset domain used
/// by the batch matcher.
///
/// Sign conventions involved:
///   - `deep_offset_ms` is the gyroflow sync offset of the deep-matched video
///     against the WHOLE gyro file (same convention as the batch per-clip
///     `init_offset_ms = -front_comp`): the video content start (video t=0)
///     sits at gyro file-relative time `-deep_offset_ms`.
///   - A session offset lives in the wall-clock domain,
///     `offset = gyro.created_at_ms - video.created_at_ms` for a perfectly
///     timed pair (see `compute_session_offset` candidates), and
///     `compute_clip_window` projects `video_start = g.created_at - offset`
///     (with `delay = 0`).
///
/// Derivation: the content start that `compute_clip_window` assigns to the
/// deep-matched video is `v_created - (g.created_at - session_offset)`; deep
/// match measured it as `-deep_offset_ms`, hence
/// `session_offset = g.created_at + (-deep_offset_ms) - v_created`.
/// Validated against `compute_clip_window` by
/// `deep_anchor_session_offset_places_content_at_minus_deep_offset`.
///
/// `pub` (not `pub(crate)`): the render queue also uses this to learn the
/// pool-wide clock shift from an accepted deep match (the returned session
/// offset equals the gyro-clock-minus-video-clock error E), keeping the sign
/// convention in one place. `deep_match::predicted_gyro_position_ms` is the
/// algebraic inverse (round-trip tested there).
pub fn derive_session_offset_from_deep_match(
    gyro_created_at_ms: i64,
    video_created_at_ms: i64,
    deep_offset_ms: f64,
) -> i64 {
    gyro_created_at_ms + (-deep_offset_ms).round() as i64 - video_created_at_ms
}

/// Layer-3 gate: does the video-content portion of the clip window actually
/// land inside `[0, gyro_duration_ms]` enough to extract usable IMU samples?
///
/// `gyro_start_ms` / `gyro_end_ms` are `compute_clip_window`'s output (already
/// padded by `front_comp` / `back_comp`). The video content portion is
/// `[gyro_start_ms + front_comp, gyro_end_ms - back_comp]`. Required coverage
/// is `max(video_duration * COVERAGE_RATIO, video_duration -
/// COVERAGE_HEAD_TOL_MS)` (short videos rate-bounded, long videos
/// absolute-bounded - `max` satisfies both).
fn clip_bounds_ok(
    gyro_start_ms: f64,
    gyro_end_ms: f64,
    front_comp: f64,
    back_comp: f64,
    video_duration_ms: f64,
    gyro_duration_ms: f64,
) -> bool {
    let video_window_start = gyro_start_ms + front_comp;
    let video_window_end = gyro_end_ms - back_comp;
    let intersect_start = video_window_start.max(0.0);
    let intersect_end = video_window_end.min(gyro_duration_ms);
    let covered_ms = (intersect_end - intersect_start).max(0.0);

    let required = (video_duration_ms * COVERAGE_RATIO)
        .max(video_duration_ms - COVERAGE_HEAD_TOL_MS);
    covered_ms >= required
}

/// Compute coverage diagnostics (covered_ms, required_ms) using the same
/// formula as `clip_bounds_ok`. Used for log messages on the reject path.
fn clip_bounds_coverage(
    gyro_start_ms: f64,
    gyro_end_ms: f64,
    front_comp: f64,
    back_comp: f64,
    video_duration_ms: f64,
    gyro_duration_ms: f64,
) -> (f64, f64) {
    let video_window_start = gyro_start_ms + front_comp;
    let video_window_end = gyro_end_ms - back_comp;
    let intersect_start = video_window_start.max(0.0);
    let intersect_end = video_window_end.min(gyro_duration_ms);
    let covered_ms = (intersect_end - intersect_start).max(0.0);
    let required = (video_duration_ms * COVERAGE_RATIO)
        .max(video_duration_ms - COVERAGE_HEAD_TOL_MS);
    (covered_ms, required)
}

/// For every gyro, assign it to the reliable session whose anchor is closest.
/// This partitions the gyro pool so each session's coverage check only sees
/// gyros that physically belong to its shooting day - even when sessions are
/// exactly one day apart (where a symmetric +/- 24h window would let day-1
/// gyros leak into a day-2 session check).
///
/// TODO(cross-frame): `g.created_at_ms` is in IMU clock; `session.anchor_ms`
/// is in camera clock. Distance comparison only behaves correctly when the
/// camera<->IMU offsets across all sessions are similar (which is typical
/// when one camera + one IMU are used over a shoot). Multi-session shoots
/// where the user re-synced the IMU mid-way - producing very different
/// per-session offsets - can mis-snap long content IMUs to the wrong session.
/// Single-session (the common case) is not affected because `min_by_key`
/// trivially picks the lone reliable session. Future fix: partition by
/// explicit G cluster membership from `pair_sessions`, then apply each
/// session's measured offset before comparing.
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
    deep_anchors: &[DeepMatchAnchor],
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

        // Deep-anchor pin short-circuit (absolute highest priority, BEFORE the
        // calibration-video short-circuit and the coverage competition). A
        // deep-matched clip is content-level ground truth measured point-to-
        // point; it must NOT be re-assigned or overwritten by the wall-clock
        // statistical coverage path. If this video's index matches some
        // DeepMatchAnchor (which must carry a valid created_at so the window
        // can be projected — anchors without created_at degrade to self-only
        // and never pin), assign it directly to its own deep gyro and deep
        // offset, then `continue`, skipping the coverage competition and the
        // clip-bounds gate entirely. With an empty anchor slice (or no index
        // match) this branch is never taken and the assignment is byte-
        // identical to the pre-change behaviour.
        if let Some(a) = deep_anchors.iter().find(|a| {
            a.video_index == vi && a.video_created_at_ms.is_some() && a.gyro_index < gyros.len()
        }) {
            let g = &gyros[a.gyro_index];
            // Reproduce exactly what the coverage path would emit for this clip
            // via the deep-anchor session: project with the derived session
            // offset (delay = 0). This keeps init_offset_ms == -front_comp and
            // gyro_start_ms == -deep_offset_ms - front_comp, byte-identical to
            // the deep-anchor session's normal coverage output, but guaranteed
            // (no coverage competition / clip-bounds gate).
            let session_offset =
                derive_session_offset_from_deep_match(g.created_at_ms, v_created, a.offset_ms);
            let (gyro_start_ms, gyro_end_ms, front_comp, _calib_anchor_ms) =
                compute_clip_window(v, g, v_created, session_offset, &[], videos);
            log::info!(
                "[batch_match_diag] deep_pin vi={} gyro={} session_offset={}ms deep_offset_ms={:.1} init_offset_ms={:.1} range=[{:.1},{:.1}]",
                vi,
                a.gyro_index,
                session_offset,
                a.offset_ms,
                -front_comp,
                gyro_start_ms,
                gyro_end_ms
            );
            results.push(MatchResult {
                video_index: vi,
                job_id: None,
                gyro_index: Some(a.gyro_index),
                status: MatchStatus::Matched,
                global_offset_ms: Some(session_offset),
                gyro_start_ms: Some(gyro_start_ms),
                gyro_end_ms: Some(gyro_end_ms),
                init_offset_ms: Some(-front_comp),
            });
            continue;
        }

        // Calibration video short-circuit: a video identified as a session's
        // calibration video (recorded in `cal_pairs`, whose keys mirror
        // `cal_video_indices`) is tagged CalibrationPair using its paired cal
        // gyro, BEFORE the coverage-window hit check and the 70% clip bounds
        // gate. Its validity is established by compute_session_offset's
        // duration + offset consistency; the coverage gate only guards content
        // videos borrowing the wrong gyro and must not demote cal videos whose
        // (deliberately short) cal gyro physically covers < 70% of the clip.
        // Bypassing the hit check also handles cal videos whose averaged offset
        // projects them just outside the owned gyro window.
        if let Some((sid, gi)) = sessions.iter().enumerate().find_map(|(sid, s)| {
            if !s.reliable {
                return None;
            }
            s.cal_pairs
                .iter()
                .find(|(cv, _)| *cv == vi)
                .map(|&(_, gi)| (sid, gi))
        }) {
            let s = &sessions[sid];
            let video_offset = s.offset - s.delay;
            let g = &gyros[gi];
            let (gyro_start_ms, gyro_end_ms, front_comp, _calib_anchor_ms) = compute_clip_window(
                v,
                g,
                v_created,
                video_offset,
                &s.cal_video_indices,
                videos,
            );
            // Diagnostic-only coverage numbers (NOT used to gate). Lets the log
            // show how far below 70% the cal gyro covers without rejecting it.
            let (covered_ms, required_ms) = clip_bounds_coverage(
                gyro_start_ms,
                gyro_end_ms,
                front_comp,
                front_comp,
                v.duration_ms,
                g.duration_ms,
            );
            log::info!(
                "[batch_match_diag] cal_pair_exempt vi={} sid={} gi={} status=calibration covered_ms={:.1} required_ms={:.1}",
                vi,
                sid,
                gi,
                covered_ms,
                required_ms
            );
            results.push(MatchResult {
                video_index: vi,
                job_id: None,
                gyro_index: Some(gi),
                status: MatchStatus::CalibrationPair,
                global_offset_ms: Some(s.offset),
                gyro_start_ms: Some(gyro_start_ms),
                gyro_end_ms: Some(gyro_end_ms),
                init_offset_ms: Some(-front_comp),
            });
            continue;
        }

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

        // Layer-3 clip bounds gate. Even though the session's offset passed
        // anchor-pool consistency, the per-video clip window may still fall
        // outside [0, gyro_duration_ms] when the session was a false-positive
        // pair (e.g. anchor-pool seeded by mis-pair before the cross-day
        // outlier was discovered, or a legit session whose gyro just doesn't
        // cover this particular video). Reject -> push to pending so the
        // fallback path gets a chance to find a different gyro.
        let back_comp = front_comp; // compute_clip_window: front_comp == back_comp
        if !clip_bounds_ok(
            gyro_start_ms,
            gyro_end_ms,
            front_comp,
            back_comp,
            v.duration_ms,
            g.duration_ms,
        ) {
            let (covered_ms, required_ms) = clip_bounds_coverage(
                gyro_start_ms,
                gyro_end_ms,
                front_comp,
                back_comp,
                v.duration_ms,
                g.duration_ms,
            );
            log::info!(
                "[batch_match_diag] clip_bounds_reject vi={} sid={} gi={} video_dur={:.1} gyro_dur={:.1} covered_ms={:.1} required_ms={:.1} path=coverage reason=below_threshold",
                vi,
                sid,
                gi,
                v.duration_ms,
                g.duration_ms,
                covered_ms,
                required_ms
            );
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

/// Phase 5: for every pending video, borrow the nearest reliable session's
/// OFFSET SCALAR (within FALLBACK_MAX_GAP_MS of v.created_at) and then search
/// the FULL gyro pool for a gyro that, when projected via the borrowed offset,
/// physically covers this video. Successful matches become MatchedFallback;
/// videos with no covering gyro stay Unmatched.
///
/// Key semantic change vs. the legacy "borrow from owned_gyros[sid]" approach:
/// fallback now borrows offset only, never the gyro file. This eliminates the
/// "user didn't shoot mix.bin today" mis-pair where borrowing yesterday's
/// session's gyro projected today's video to yesterday's time range.
fn assign_fallback(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    sessions: &[Session],
    gyros_by_time: &[usize],
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
                "[batch_match_diag] fallback_skipped vi={} nearest_session={} gap_ms={} reason=over_36h",
                vi,
                sid,
                gap
            );
            continue;
        }

        let s = &sessions[sid];
        let video_offset = s.offset - s.delay;

        // Search the FULL gyro pool (not owned_gyros[sid]) for a gyro that
        // physically covers v_created using the borrowed offset. Anchor
        // around the nearest gyro by created_at (binary_search_by_key),
        // then scan ±5 neighbours and pick the one with min
        // |g.created_at - v.created_at| that actually covers v.
        let scan_start_idx = match gyros_by_time
            .binary_search_by_key(&v_created, |&i| gyros[i].created_at_ms)
        {
            Ok(idx) => idx,
            Err(idx) => idx,
        };
        let window = 5usize;
        let lo = scan_start_idx.saturating_sub(window);
        let hi = (scan_start_idx + window).min(gyros_by_time.len());

        let mut candidate: Option<(usize, i64)> = None; // (gyro_index, |g.created - v.created|)
        for &gi in &gyros_by_time[lo..hi] {
            let g = &gyros[gi];
            let video_start = g.created_at_ms - video_offset;
            let video_end = video_start + (g.duration_ms as i64);
            if v_created >= video_start - COVERAGE_TOLERANCE_MS
                && v_created <= video_end + COVERAGE_TOLERANCE_MS
            {
                let abs_dist = (g.created_at_ms - v_created).abs();
                if candidate.map(|c| abs_dist < c.1).unwrap_or(true) {
                    candidate = Some((gi, abs_dist));
                }
            }
        }

        let gi = match candidate {
            Some((g, _)) => g,
            None => {
                log::info!(
                    "[batch_match_diag] fallback_no_gyro vi={} borrowed_session={} reason=no_covering_gyro",
                    vi,
                    sid
                );
                continue;
            }
        };
        let g = &gyros[gi];
        let (gyro_start_ms, gyro_end_ms, front_comp, calib_anchor_ms) =
            compute_clip_window(v, g, v_created, video_offset, &s.cal_video_indices, videos);

        // Layer-3 clip bounds gate on the fallback exit. Even if the borrowed
        // offset projects v_created inside [video_start, video_end], the clip
        // window may still extend past the gyro's physical [0, duration_ms]
        // range. Reject and leave Unmatched.
        let back_comp = front_comp;
        if !clip_bounds_ok(
            gyro_start_ms,
            gyro_end_ms,
            front_comp,
            back_comp,
            v.duration_ms,
            g.duration_ms,
        ) {
            let (covered_ms, required_ms) = clip_bounds_coverage(
                gyro_start_ms,
                gyro_end_ms,
                front_comp,
                back_comp,
                v.duration_ms,
                g.duration_ms,
            );
            log::info!(
                "[batch_match_diag] fallback_clip_bounds_reject vi={} borrow_session={} gi={} video_dur={:.1} gyro_dur={:.1} covered_ms={:.1} required_ms={:.1} path=fallback reason=below_threshold",
                vi,
                sid,
                gi,
                v.duration_ms,
                g.duration_ms,
                covered_ms,
                required_ms
            );
            continue;
        }

        log::info!(
            "[batch_match_diag] fallback_used vi={} borrow_session={} gap_ms={} gyro_index={} gyro_created_at={} calib_anchor={} raw_range=[{:.1},{:.1}]",
            vi,
            sid,
            gap,
            gi,
            g.created_at_ms,
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

// Clip bounds gate thresholds. Applied at the assign_* exits to enforce that
// the computed `[gyro_start_ms, gyro_end_ms]` clip window actually has the
// video content portion physically covered by the gyro file. Mis-pairings
// (cross-day, cross-clock, fallback-borrowed offset that misses real gyro
// coverage) all manifest as a clip window that falls outside `[0,
// gyro_duration_ms]` and are rejected here as Unmatched regardless of which
// upstream branch produced them.
//
// COVERAGE_RATIO: minimum fraction of the video content window that must be
// physically covered by the gyro file.
// COVERAGE_HEAD_TOL_MS: absolute end-point loss tolerance (covers
// "camera-on-before-gyro" / "camera-off-after-gyro" legit cases).
// Required coverage = max(video_duration * COVERAGE_RATIO,
//                         video_duration - COVERAGE_HEAD_TOL_MS), so short
// clips are rate-bounded and long clips are absolute-bounded.
const COVERAGE_RATIO: f64 = 0.70;
const COVERAGE_HEAD_TOL_MS: f64 = 3000.0;

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

                    // Layer-3 clip bounds gate also on the manual_pairs path.
                    // Diagnostic role: if the user manually paired a v with a
                    // g whose physical durations and timestamps don't actually
                    // line up, slicing still produces an out-of-range clip
                    // window; tagging this as Unmatched + log message lets
                    // the user see the mis-pair instead of silently failing
                    // downstream.
                    if !clip_bounds_ok(
                        gyro_start_ms,
                        gyro_end_ms,
                        front_comp,
                        back_comp,
                        v.duration_ms,
                        g.duration_ms,
                    ) {
                        let (covered_ms, required_ms) = clip_bounds_coverage(
                            gyro_start_ms,
                            gyro_end_ms,
                            front_comp,
                            back_comp,
                            v.duration_ms,
                            g.duration_ms,
                        );
                        log::info!(
                            "[batch_match_diag] clip_bounds_reject vi={} gi={} video_dur={:.1} gyro_dur={:.1} covered_ms={:.1} required_ms={:.1} path=manual_pairs reason=below_threshold",
                            vi,
                            gi,
                            v.duration_ms,
                            g.duration_ms,
                            covered_ms,
                            required_ms
                        );
                        return MatchResult {
                            video_index: vi,
                            job_id: None,
                            gyro_index: None,
                            status: MatchStatus::Unmatched,
                            global_offset_ms: None,
                            gyro_start_ms: None,
                            gyro_end_ms: None,
                            init_offset_ms: None,
                        };
                    }

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
///
/// `deep_anchors` (render-queue-deep-gyro-match): deep-match-derived session
/// anchors consumed by the auto path; an empty slice is a strict no-op. The
/// legacy manual-pairs path ignores anchors (manual pairs already pin the
/// single-session offset explicitly).
pub fn batch_match(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    manual_pairs: Option<&[ManualCalibrationPair]>,
    deep_anchors: &[DeepMatchAnchor],
) -> BatchMatchResult {
    if let Some(pairs) = manual_pairs
        && !pairs.is_empty()
    {
        if !deep_anchors.is_empty() {
            log::info!(
                target: "sync",
                "[deep-match] {} anchor(s) ignored: manual calibration pairs take the legacy single-session path",
                deep_anchors.len()
            );
        }
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
    auto_match(videos, gyros, deep_anchors)
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

/// render-queue-deep-gyro-match 7.3: fold accepted deep-match anchors into the
/// session list. For each anchor (in caller-supplied order):
///   - locate its cluster: first the session whose G cluster contains the
///     anchored gyro (covers cal-gyro anchors and previously created anchor
///     sessions), otherwise a creation-time-derived reliable session whose
///     offset is consistent with the derived one (same camera<->IMU clock
///     pair, within MULTI_SESSION_OFFSET_TOLERANCE_MS);
///   - override that session's offset (millisecond-accurate anchor outranks
///     the +/-1.5s creation-time candidate; delay reset to 0 because the
///     anchor measures content alignment directly) and force it reliable,
///     bypassing compute_session_offset's two-calibration-pair requirement;
///   - or, with no matching session, append a synthetic reliable session so
///     gyro ownership / coverage / fallback treat the anchor exactly like a
///     calibration-derived session.
/// Anchors without video created_at degrade to self-only (logged, no effect
/// on the batch); an empty anchor slice leaves `sessions` untouched.
fn apply_deep_anchors(
    sessions: &mut Vec<Session>,
    anchors: &[DeepMatchAnchor],
    gyros: &[GyroMatchInfo],
) {
    for a in anchors {
        let Some(g) = gyros.get(a.gyro_index) else {
            log::warn!(
                target: "sync",
                "[deep-match] anchor skipped: gyro_index={} out of range ({} gyros)",
                a.gyro_index,
                gyros.len()
            );
            continue;
        };
        let Some(v_created) = a.video_created_at_ms else {
            log::info!(
                target: "sync",
                "[deep-match] anchor degraded to self-only: gyro_index={} (video has no created_at)",
                a.gyro_index
            );
            continue;
        };
        let derived =
            derive_session_offset_from_deep_match(g.created_at_ms, v_created, a.offset_ms);
        let sid = sessions
            .iter()
            .position(|s| s.g_cluster.contains(&a.gyro_index))
            .or_else(|| {
                // Restricted to creation-time-derived sessions (non-empty V
                // cluster) so a second anchor never hijacks another anchor's
                // synthetic session - each long gyro keeps its own anchor.
                sessions.iter().position(|s| {
                    !s.v_cluster.is_empty()
                        && s.reliable
                        && (s.offset - derived).abs() <= MULTI_SESSION_OFFSET_TOLERANCE_MS
                })
            });
        match sid {
            Some(i) => {
                let s = &mut sessions[i];
                log::info!(
                    target: "sync",
                    "[deep-match] anchor override: cluster={} session_offset={}ms (creation-time candidate was {}ms, reliable={}, delay={}ms)",
                    i,
                    derived,
                    s.offset,
                    s.reliable,
                    s.delay
                );
                s.offset = derived;
                s.delay = 0;
                s.reliable = true;
            }
            None => {
                log::info!(
                    target: "sync",
                    "[deep-match] anchor session created: gyro_index={} session_offset={}ms anchor_ms={}",
                    a.gyro_index,
                    derived,
                    v_created
                );
                sessions.push(Session {
                    v_cluster: Vec::new(),
                    cal_video_indices: Vec::new(),
                    cal_pairs: Vec::new(),
                    g_cluster: vec![a.gyro_index],
                    anchor_ms: v_created,
                    offset: derived,
                    delay: 0,
                    reliable: true,
                });
            }
        }
    }
}

/// Multi-session automatic calibration pipeline.
fn auto_match(
    videos: &[VideoMatchInfo],
    gyros: &[GyroMatchInfo],
    deep_anchors: &[DeepMatchAnchor],
) -> BatchMatchResult {
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

    // Deep anchors able to influence the batch (valid gyro index + video
    // created_at) can build sessions even without any calibration cluster,
    // so the two early-outs below only fire when no such anchor exists -
    // with an empty anchor slice this is byte-equivalent to the pre-anchor
    // flow.
    let has_usable_anchor = deep_anchors
        .iter()
        .any(|a| a.video_created_at_ms.is_some() && a.gyro_index < gyros.len());

    let mut sessions = if v_clusters.is_empty() || g_clusters.is_empty() {
        if !has_usable_anchor {
            return unmatched_results(videos, MatchError::NoCalibrationPairsFound);
        }
        Vec::new()
    } else {
        pair_sessions(v_clusters, g_clusters, videos, gyros)
    };

    if sessions.is_empty() && !has_usable_anchor {
        return unmatched_results(videos, MatchError::NoCalibrationPairsFound);
    }

    for s in sessions.iter_mut() {
        match compute_session_offset(videos, gyros, &s.v_cluster, &s.g_cluster) {
            Some(so) => {
                let spread = so.spread;
                s.offset = so.offset;
                s.delay = so.delay;
                s.cal_video_indices = so.cal_video_indices;
                s.cal_pairs = so.cal_pairs;
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

    apply_deep_anchors(&mut sessions, deep_anchors, gyros);

    let reliable_count = sessions.iter().filter(|s| s.reliable).count();
    if reliable_count == 0 {
        return unmatched_results(videos, MatchError::NoCalibrationPairsFound);
    }

    let owned_gyros = assign_gyro_ownership(gyros, &sessions);

    // Sort gyros by created_at once at the top of auto_match. assign_fallback
    // borrows the offset from the nearest session and then searches the FULL
    // gyro pool (not owned_gyros[sid]) for a gyro that physically covers the
    // video using that offset. O(log G) per fallback lookup via binary_search.
    let mut gyros_by_time: Vec<usize> = (0..gyros.len()).collect();
    gyros_by_time.sort_by_key(|&i| gyros[i].created_at_ms);

    let (mut results, pending) =
        assign_videos_by_coverage(videos, gyros, &sessions, &owned_gyros, deep_anchors);
    assign_fallback(
        videos,
        gyros,
        &sessions,
        &gyros_by_time,
        &pending,
        &mut results,
    );

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
    fn pair_single_v_single_g_8h_gap_pairs_unconditionally() {
        // Regression for the "camera vs IMU clock with timezone offset" case
        // (Canon CreationDateUtc treating camera local-wall-clock as UTC vs.
        // SenseFlow .mix.bin's correctly UTC-normalized timestamps). The
        // absolute V/G anchor gap IS the unknown camera<->IMU clock offset;
        // it must not be used as a pre-filter.
        //
        // V anchor = 0, G anchor = 8h 6min ahead (≈ user's real 29_178_000ms
        // gap). Durations match within 1.5s. Must form 1 session.
        let v_anchor_offset: i64 = 8 * 3_600_000 + 360_000; // 29_160_000 ms
        let videos = vec![
            v(0, 5_000.0, Some(0)),
            v(1, 5_000.0, Some(30_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, v_anchor_offset),
            g(1, 5_500.0, v_anchor_offset + 30_000),
        ];
        let sessions =
            pair_sessions(vec![vec![0, 1]], vec![vec![0, 1]], &videos, &gyros);
        assert_eq!(
            sessions.len(),
            1,
            "single V + single G must pair regardless of absolute anchor gap"
        );
        assert_eq!(sessions[0].anchor_ms, 0);
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
        let offset = compute_session_offset(&videos, &gyros, &[0, 1], &[0, 1, 2, 3])
            .expect("should produce candidates")
            .offset;
        // Both candidates have inlier count = 1; tie-break by coverage picks
        // the one that covers more videos -> the smaller offset (=200)
        // since the padded videos are near zero.
        assert_eq!(
            offset, 200,
            "coverage tie-break must pick offset whose video window matches more videos"
        );
    }

    #[test]
    fn pass2_empty_pool_prefers_coverage_over_degenerate_single_inlier_spread() {
        // Regression for feedback 20260601-bf9c062f ("3 天的视频无法一次性同步").
        //
        // One cal V cluster (day-30 morning clips C185/C186/C187) can pair with
        // TWO G clusters:
        //   - FAR  (2026-05-25 cal files): a cross-day clock-skew match. Only
        //     one inlier survives -> inlier=1, spread=0 (degenerate), and the
        //     offset geometrically covers few videos (coverage=3).
        //   - NEAR (2026-05-30 contemporaneous files): the correct ~6.8 min
        //     camera<->logger skew. Two inliers -> inlier=2, spread=400, and it
        //     covers far more videos.
        //
        // The old Pass-2 empty-pool rule `min_by_key(spread)` locked the FAR
        // cluster (spread=0 trivially wins) -> the -4.79 day global offset
        // starved 73/75 clips of gyro coverage (all `no_covering_gyro`). The fix
        // ranks coverage / inlier_count ahead of spread, so the NEAR cluster
        // (g_cluster [2,3,4]) is locked instead.
        //
        // All cal timestamps/durations are the real values from the feedback
        // log's `[batch_match_diag] candidate ...` lines.
        let mut videos = vec![
            v(0, 2102.1, Some(1_780_102_687_130)), // C185
            v(1, 1468.1, Some(1_780_102_691_190)), // C186
            v(2, 1434.8, Some(1_780_102_694_330)), // C187
        ];
        // FAR gyros (2026-05-25): durations reproduce the log dur_diff 0.85/0.909.
        let mut gyros = vec![
            g(0, 3452.0, 1_779_688_687_000), // 2026-05-25_13-58-07
            g(1, 2877.0, 1_779_688_693_000), // 2026-05-25_13-58-13
        ];
        // NEAR gyros (2026-05-30): g3/g4/g5 in the log.
        gyros.push(g(2, 2302.0, 1_780_102_280_000)); // 2026-05-30_08-51-20
        gyros.push(g(3, 1726.0, 1_780_102_285_000)); // 2026-05-30_08-51-25
        gyros.push(g(4, 1726.0, 1_780_102_288_000)); // 2026-05-30_08-51-28

        // Extra contemporaneous (video, gyro) pairs spread across the day. Under
        // the NEAR offset each gyro window covers its video, lifting NEAR
        // coverage above FAR's 3; under the FAR offset (~-4.14e8 ms) they shift
        // out of range and are never counted. Reproduces the log's
        // "coverage=33 vs 3" without needing all 75 clips.
        const NEAR_OFFSET: i64 = -406_260; // g_created - v_created
        for k in 0..6usize {
            let v_created = 1_780_103_500_000 + (k as i64) * 500_000;
            videos.push(v(3 + k, 2000.0, Some(v_created)));
            gyros.push(g(5 + k, 3000.0, v_created + NEAR_OFFSET));
        }

        let v_clusters = vec![vec![0usize, 1, 2]];
        let g_clusters = vec![vec![0usize, 1], vec![2usize, 3, 4]];

        let sessions = pair_sessions(v_clusters, g_clusters, &videos, &gyros);

        assert_eq!(sessions.len(), 1, "exactly one session should be locked");
        assert_eq!(
            sessions[0].g_cluster,
            vec![2usize, 3, 4],
            "Pass 2 must lock the contemporaneous NEAR gyro cluster (higher \
             coverage / more inliers), not the degenerate-spread cross-day FAR \
             cluster [0,1]"
        );
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
        // Branch A single-cluster pair runs duration cross-check. V dur 9s vs
        // G dur 2s -> |2 - 0.5 + 0 - 9| = 7.5 >> 1.5 -> reject (no session).
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

    #[test]
    fn pair_two_v_two_g_with_8h_timezone_shift() {
        // Multi-cluster equivalent of the timezone bug: 2 V clusters and 2 G
        // clusters; G anchors are uniformly shifted +8h vs. V anchors (Canon
        // local-as-UTC vs. SenseFlow true-UTC). Intervals on each side equal
        // 1h, so interval_match should pair V[0]<->G[0], V[1]<->G[1].
        let hour: i64 = 3_600_000;
        let videos = vec![
            v(0, 5_000.0, Some(0)),
            v(1, 5_000.0, Some(30_000)),
            v(2, 5_000.0, Some(hour)),
            v(3, 5_000.0, Some(hour + 30_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 8 * hour),
            g(1, 5_500.0, 8 * hour + 30_000),
            g(2, 5_500.0, 9 * hour),
            g(3, 5_500.0, 9 * hour + 30_000),
        ];
        let sessions = pair_sessions(
            vec![vec![0, 1], vec![2, 3]],
            vec![vec![0, 1], vec![2, 3]],
            &videos,
            &gyros,
        );
        assert_eq!(sessions.len(), 2, "two V + two G with matching intervals must form 2 sessions");
        assert_eq!(sessions[0].v_cluster, vec![0, 1]);
        assert_eq!(sessions[1].v_cluster, vec![2, 3]);
        assert_eq!(sessions[0].g_cluster, vec![0, 1]);
        assert_eq!(sessions[1].g_cluster, vec![2, 3]);
    }

    #[test]
    fn pair_two_v_two_g_interval_mismatch_returns_empty() {
        // V intervals 1h, G intervals 12h - no alignment matches within
        // tolerance. Combined with duration mismatch in Branch C fallback,
        // expect 0 sessions (rather than mis-pairing).
        let hour: i64 = 3_600_000;
        let videos = vec![
            v(0, 9_000.0, Some(0)),
            v(1, 9_000.0, Some(30_000)),
            v(2, 9_000.0, Some(hour)),
            v(3, 9_000.0, Some(hour + 30_000)),
        ];
        // Gyro durations completely mismatched (2s vs video 9s) so Branch C
        // also rejects on duration_ok.
        let gyros = vec![
            g(0, 2_000.0, 0),
            g(1, 2_000.0, 30_000),
            g(2, 2_000.0, 12 * hour),
            g(3, 2_000.0, 12 * hour + 30_000),
        ];
        let sessions = pair_sessions(
            vec![vec![0, 1], vec![2, 3]],
            vec![vec![0, 1], vec![2, 3]],
            &videos,
            &gyros,
        );
        assert!(
            sessions.is_empty(),
            "mismatched intervals + mismatched durations must not produce sessions"
        );
    }

    #[test]
    fn pair_three_v_two_g_k_offset_alignment() {
        // 3 V clusters, 2 G clusters, V intervals = [1h, 1h], G intervals = [1h].
        // alignments k=0 and k=1 both match 1 interval with same penalty (0).
        // tie-break by |k| smallest -> k=0 wins, pairing V[0]<->G[0], V[1]<->G[1].
        // V[2] orphans. duration_ok must still pass for actually formed sessions.
        let hour: i64 = 3_600_000;
        let videos = vec![
            v(0, 5_000.0, Some(0)),
            v(1, 5_000.0, Some(30_000)),
            v(2, 5_000.0, Some(hour)),
            v(3, 5_000.0, Some(hour + 30_000)),
            v(4, 5_000.0, Some(2 * hour)),
            v(5, 5_000.0, Some(2 * hour + 30_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 5 * hour),
            g(1, 5_500.0, 5 * hour + 30_000),
            g(2, 5_500.0, 6 * hour),
            g(3, 5_500.0, 6 * hour + 30_000),
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
        let so = result.expect("should succeed");
        let (offset, spread) = (so.offset, so.spread);
        assert_eq!(spread, 0); // Single avg per (vi,gi) pair -> single offset in candidates.
        // offset = (1000+1200)/2 = 1100
        assert_eq!(offset, 1100);
    }

    #[test]
    fn session_offset_bimodal_falls_back_to_single_pick() {
        // Regression: feedback 20260527-caccbf81 (P1004702..P1004706, all
        // Panasonic S5 + SenseFlow .bin). Five short videos and six gyros
        // cluster together but the per-video offsets split into two real
        // sub-sessions:
        //   group A (P1004702/03):   ~43094-43097ms
        //   group B (P1004704/05/06): ~43091.6-43091.9ms
        // (5+ second drift across the burst, plausibly from user pressing
        // gyro vs camera with different latency for two separate cal taps).
        //
        // Pass 1 (Avg) generated a "bridge" candidate at vi_pair=[40,41]
        // (avg = 43093193ms, midway between groups). RANSAC picked it as
        // the mode, pulled both groups into its 3000ms inlier halo, and
        // reported spread = 4124ms > SYNC_CREATE_OFFSET_MAX -> reliable
        // gate FAILS at the caller site (line ~1687).
        //
        // Pass 2 (SinglePick) stores each candidate's stronger half so the
        // bridge collapses onto v[41]/g[6]'s true offset (43091942ms),
        // joining v[42]/g[8] (43091822ms) as a tight inlier pair with
        // spread 120ms -> reliable gate PASSES, session ships and downstream
        // coverage/fallback handles the outlier P1004702/03 separately.
        let videos = vec![
            v(39, 3270.0, Some(1_775_253_706_761)), // P1004702
            v(40, 1400.0, Some(1_775_253_715_556)), // P1004703
            v(41, 5610.0, Some(1_775_253_725_058)), // P1004704
            v(42, 7470.0, Some(1_775_253_744_178)), // P1004705
            v(43, 6540.0, Some(1_775_253_755_388)), // P1004706
        ];
        let gyros = vec![
            g(4, 3450.0, 1_775_296_804_000), // 18:00:04 mix.bin
            g(5, 2300.0, 1_775_296_810_000), // 18:00:10
            g(6, 5760.0, 1_775_296_817_000), // 18:00:17
            g(7, 2300.0, 1_775_296_825_000), // 18:00:25 (delay=500 outlier)
            g(8, 7480.0, 1_775_296_836_000), // 18:00:36
            g(9, 6330.0, 1_775_296_847_000), // 18:00:47
        ];

        let so = compute_session_offset(
            &videos,
            &gyros,
            &[0, 1, 2, 3, 4],
            &[0, 1, 2, 3, 4, 5],
        )
        .expect("session should resolve via single-pick fallback");
        let (offset, spread) = (so.offset, so.spread);

        assert!(
            offset == 43_091_942 || offset == 43_091_822,
            "expected SinglePick to lock onto P1004704/05/06 cluster (~43091.9s), \
             got {} (Avg-bridge would land at 43093193)",
            offset
        );
        assert!(
            spread <= SYNC_CREATE_OFFSET_MAX,
            "spread {}ms must satisfy reliable gate; Avg's bridge candidate \
             used to push it to 4124ms",
            spread
        );
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
        let so = compute_session_offset(&videos, &gyros, &[0, 1], &[0, 1, 2, 3])
            .expect("should pick a winner from one of the two clusters");
        let (offset, spread) = (so.offset, so.spread);
        // Bucket-mode picks one of the two single-member clusters.
        assert!(offset == 1000 || offset == 6000, "got offset={}", offset);
        // Spread within the chosen bucket is small (single member -> 0).
        assert!(spread <= SYNC_CREATE_OFFSET_MAX, "spread={}", spread);
    }

    // --- Phase 4 tests ---

    fn make_session(offset: i64, anchor: i64, v_cluster: Vec<usize>, g_cluster: Vec<usize>) -> Session {
        Session {
            cal_video_indices: v_cluster.clone(),
            cal_pairs: Vec::new(),
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
        let (results, pending) = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, &[]);
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
        let (results, pending) = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, &[]);
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
        let (_results, pending) = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, &[]);
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
        // Session A covers day1 (cal pair); video on day2 has no own cal pair
        // but a long IMU file g1 covers it. Fallback borrows A's offset and
        // finds g1 in the FULL gyro pool (not owned by A) -> MatchedFallback.
        let v_t: i64 = 20 * 3_600_000;
        let videos = vec![v(0, 5_000.0, Some(v_t))]; // 20h after day1 anchor
        let gyros = vec![
            g(0, 1_000.0, 1_000), // day1 cal gyro (small)
            // day2 long IMU covering v at v_t. With borrowed offset=1000,
            // video_start = g1.created_at - 1000, so set g1.created_at = v_t+1000
            // and dur = 10000 to fully cover.
            g(1, 10_000.0, v_t + 1_000),
        ];
        let sessions = vec![make_session(1_000, 0, vec![], vec![0])];
        let mut gyros_by_time: Vec<usize> = (0..gyros.len()).collect();
        gyros_by_time.sort_by_key(|&i| gyros[i].created_at_ms);
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
        assign_fallback(&videos, &gyros, &sessions, &gyros_by_time, &[0], &mut results);
        assert_eq!(results[0].status, MatchStatus::MatchedFallback);
        assert_eq!(results[0].global_offset_ms, Some(1_000));
        // Should have picked g1 (the actually covering gyro), not g0.
        assert_eq!(results[0].gyro_index, Some(1));
    }

    #[test]
    fn fallback_too_far_unmatched() {
        // Session anchor 0; video at 40h away -> > 36h -> Unmatched stays.
        let videos = vec![v(0, 5_000.0, Some(40 * 3_600_000))];
        let gyros = vec![g(0, 1_000.0, 1_000)];
        let sessions = vec![make_session(1_000, 0, vec![], vec![0])];
        let mut gyros_by_time: Vec<usize> = (0..gyros.len()).collect();
        gyros_by_time.sort_by_key(|&i| gyros[i].created_at_ms);
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
        assign_fallback(&videos, &gyros, &sessions, &gyros_by_time, &[0], &mut results);
        assert_eq!(results[0].status, MatchStatus::Unmatched);
    }

    #[test]
    fn fallback_unreliable_session_internal_video() {
        // Two sessions: A reliable but far, B unreliable and close to v.
        // v ends up pending (Phase 4 won't pick B because B is unreliable; A
        // doesn't cover v with its OWN gyro g0), so fallback borrows A's offset
        // and searches the full gyro pool. With borrowed offset=2000 and g1 at
        // 20h+2000, v_t=20h falls inside g1's projected range -> MatchedFallback.
        let v_t: i64 = 20 * 3_600_000;
        let videos = vec![v(0, 5_000.0, Some(v_t))];
        let gyros = vec![
            g(0, 1_000.0, 1_000),
            g(1, 10_000.0, v_t + 2_000), // covers v_t when offset=2000 is applied
        ];
        let sessions = vec![
            Session {
                v_cluster: vec![],
                cal_video_indices: vec![],
                cal_pairs: vec![],
                g_cluster: vec![1],
                anchor_ms: 18 * 3_600_000,
                offset: 1_000,
                delay: 0,
                reliable: false, // B unreliable
            },
            Session {
                v_cluster: vec![],
                cal_video_indices: vec![],
                cal_pairs: vec![],
                g_cluster: vec![0],
                anchor_ms: 0,
                offset: 2_000,
                delay: 0,
                reliable: true, // A reliable, anchor 20h from v
            },
        ];
        let mut gyros_by_time: Vec<usize> = (0..gyros.len()).collect();
        gyros_by_time.sort_by_key(|&i| gyros[i].created_at_ms);
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
        assign_fallback(&videos, &gyros, &sessions, &gyros_by_time, &[0], &mut results);
        assert_eq!(results[0].status, MatchStatus::MatchedFallback);
        // borrowed offset is A's = 2000
        assert_eq!(results[0].global_offset_ms, Some(2_000));
        assert_eq!(results[0].gyro_index, Some(1));
    }

    // --- Phase 6 tests ---

    #[test]
    fn batch_match_single_day_equivalence() {
        // Single-day calibration + one normal video. Probe video and long
        // gyro must be longer than the cal thresholds (10s for video, 12s for
        // gyro) so they are NOT classified as calibration candidates.
        //
        // Long gyro (g2) duration is 70s to ensure the 60s content video v2
        // is fully covered including the front_comp+back_comp padding (Layer-3
        // clip bounds gate requires the video content portion to land inside
        // [0, gyro_duration_ms]).
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            v(2, 60_000.0, Some(5_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 70_000.0, 2_000),
        ];
        let result = batch_match(&videos, &gyros, None, &[]);
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
            // Day 1 with offset = 1000ms. Long IMU (g2) 70s to fully cover
            // 60s v2 including front+back padding (Layer-3 clip bounds gate).
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            g(2, 70_000.0, 2_000),
            // Day 2 with offset = 5000ms (different drift from day 1, within
            // anchor-pool 15s tolerance).
            g(3, 5_500.0, 6_000 + day),
            g(4, 5_500.0, 36_000 + day),
            g(5, 70_000.0, 6_000 + day),
        ];
        let result = batch_match(&videos, &gyros, None, &[]);
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
        let result = batch_match(&videos, &gyros, None, &[]);
        assert!(result.global_offset_ms.is_none());
        assert_eq!(result.results[0].status, MatchStatus::Unmatched);
    }

    #[test]
    fn calibration_videos_short_cal_gyro_below_70pct_still_calibration_pair() {
        // Regression guard (batch-match-calibration-coverage-gate): the 70%
        // clip_bounds gate must NOT demote calibration videos whose
        // (deliberately short) cal gyro covers < 70% of the clip, nor those
        // whose averaged offset projects them outside the gyro window. Both
        // sub-paths must surface as CalibrationPair with a gyro assigned.
        //
        // Two cal videos (3.0s) + two short cal gyros (2.3s). offset0=0,
        // offset1=2800 -> averaged session offset 1400 misaligns both:
        //   v0 created 10000 HITS g0 window [8600,10900] but partial coverage;
        //   v1 created 14000 MISSES every owned gyro window (the C528-type
        //   no_covering_gyro path) -> would have been Unmatched pre-fix.
        // A long content video v2 (15s, no covering gyro) confirms the
        // non-calibration path is untouched (still Unmatched, not exempted).
        let videos = vec![
            v(0, 3_000.0, Some(10_000)),
            v(1, 3_000.0, Some(14_000)),
            v(2, 15_000.0, Some(100_000)),
        ];
        let gyros = vec![
            g(0, 2_300.0, 10_000),
            g(1, 2_300.0, 16_800),
        ];
        let result = batch_match(&videos, &gyros, None, &[]);

        // Sub-path (a): cal video that hit its gyro but covers < 70%.
        assert_eq!(
            result.results[0].status,
            MatchStatus::CalibrationPair,
            "v0 should be CalibrationPair, got {:?}",
            result.results[0].status
        );
        assert!(
            result.results[0].gyro_index.is_some(),
            "v0 CalibrationPair must carry a paired cal gyro"
        );

        // Sub-path (b): cal video whose averaged offset misses every window.
        assert_eq!(
            result.results[1].status,
            MatchStatus::CalibrationPair,
            "v1 (window miss) should still be CalibrationPair, got {:?}",
            result.results[1].status
        );
        assert!(
            result.results[1].gyro_index.is_some(),
            "v1 CalibrationPair must carry a paired cal gyro"
        );

        // Content video path unchanged: no covering gyro -> Unmatched, NOT
        // short-circuited to CalibrationPair.
        assert_eq!(
            result.results[2].status,
            MatchStatus::Unmatched,
            "content video must stay on the gated path, got {:?}",
            result.results[2].status
        );
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

        let result = batch_match(&videos, &gyros, None, &[]);
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
        let result = batch_match(&videos, &gyros, None, &[]);
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
        let result = batch_match(&videos, &gyros, None, &[]);
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
    fn one_day_missing_cal_direct_coverage_within_36h_unmatched_beyond() {
        // Scenario: day 1 has full cal (2 short cal videos + 2 cal gyros +
        // long IMU). Subsequent shooting blocks have NO cal videos and NO
        // cal gyros - only long content videos + their own long IMUs.
        //
        // Algorithm post-clip-bounds-gate:
        //   - Form 1 reliable session (day 1).
        //   - Content videos at 23h/35h: their respective long IMUs are in
        //     the gyro pool, owned by session A (single-session). assign_*
        //     finds direct coverage -> Matched (NOT MatchedFallback - that
        //     path only triggers when assign_videos_by_coverage misses).
        //   - Content video at 37h: long IMU at 37h is also in pool/owned
        //     by A, direct coverage hits as long as offset projection holds.
        //     The 36h fallback gap check no longer applies here because the
        //     coverage path matched first.
        //   - To test "no covering gyro -> Unmatched" we omit g5 (37h IMU),
        //     leaving v6/v7 with no physical gyro coverage; fallback then
        //     runs, finds no covering gyro within ±5 window -> Unmatched.
        let hour: i64 = 3_600_000;
        let day1_anchor = 0_i64;
        let day1_offset: i64 = -180_000;

        let videos = vec![
            // Day-1 cal pair (matches cal gyros)
            v(0, 1_500.0, Some(day1_anchor)),
            v(1, 2_000.0, Some(day1_anchor + 30_000)),
            // Day-1 content
            v(2, 60_000.0, Some(day1_anchor + 5 * 60_000)),
            // Content 23h after day-1 (covered by g3)
            v(3, 60_000.0, Some(day1_anchor + 23 * hour)),
            v(4, 30_000.0, Some(day1_anchor + 23 * hour + 60_000)),
            // Content 35h after day-1 (covered by g4)
            v(5, 60_000.0, Some(day1_anchor + 35 * hour)),
            // Content 37h after day-1 - NO covering gyro -> Unmatched
            v(6, 60_000.0, Some(day1_anchor + 37 * hour)),
            v(7, 30_000.0, Some(day1_anchor + 37 * hour + 60_000)),
        ];
        let gyros = vec![
            g(0, 1_500.0, day1_anchor + day1_offset),
            g(1, 2_000.0, day1_anchor + day1_offset + 30_000),
            g(2, 1_800_000.0, day1_anchor + day1_offset + 10_000),
            // Long IMUs cover the 23h/35h content blocks. created_at is
            // shifted by day1_offset so that, when projected via session A's
            // offset (-180_000), v_created falls inside the gyro window.
            g(3, 5_400_000.0, day1_anchor + 23 * hour - 60_000 + day1_offset),
            g(4, 5_400_000.0, day1_anchor + 35 * hour - 60_000 + day1_offset),
            // g5 deliberately omitted: v6/v7 have no covering gyro at 37h.
        ];

        let result = batch_match(&videos, &gyros, None, &[]);

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

        // 23h/35h content: Matched via direct coverage (long IMUs in pool).
        for vi in [3usize, 4, 5] {
            assert_eq!(
                result.results[vi].status,
                MatchStatus::Matched,
                "v{} expected Matched (direct coverage), got {:?}",
                vi, result.results[vi].status
            );
            assert_eq!(
                result.results[vi].global_offset_ms,
                Some(day1_offset),
                "v{} should use day-1 offset",
                vi
            );
        }

        // 37h content: no covering gyro (g5 omitted) -> Unmatched.
        for vi in [6usize, 7] {
            assert_eq!(
                result.results[vi].status,
                MatchStatus::Unmatched,
                "v{} (no covering gyro at 37h) expected Unmatched, got {:?}",
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

        let result = batch_match(&videos, &gyros, None, &[]);

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

        let result = batch_match(&videos, &gyros, None, &[]);
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
        // Note: per anchor-pool RANSAC, day1/day2 offsets must agree within
        // MULTI_SESSION_OFFSET_TOLERANCE_MS (15s) to both lock as reliable
        // sessions. Real clock drift is ~3.5s/day, so 5s diff between
        // adjacent days is physically realistic. (The earlier fixture used
        // 20s diff which is implausible drift and is rejected as a
        // mis-pair by the anchor pool.)
        let gyros = vec![
            // G0: 2 day-1 cal gyros (3 minutes before V0). offset = -180_000.
            g(0, 951.0, -180_000),
            g(1, 1901.9, -176_000),
            // Day-1 long IMU
            g(2, 1_800_000.0, -175_000),
            // G1: 2 day-2 cal gyros. offset = -185_000 (5s drift from day1,
            // within the 15s anchor-pool tolerance).
            g(3, 2202.2, day - 185_000),
            g(4, 3153.2, day - 181_000),
            // Day-2 long IMU
            g(5, 1_800_000.0, day - 180_000),
            // Day-2 night IMU covering V8
            g(6, 1_800_000.0, day + 7 * 3_600_000 - 185_000),
        ];

        let result = batch_match(&videos, &gyros, None, &[]);

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
        // v2 dur=1500 chosen so the clip window (v_dur + front_comp + back_comp
        // = 1500 + 1500 + 1500 = 4500ms) fits inside the first covering gyro
        // g0 (5500ms), satisfying the Layer-3 clip bounds gate on this path.
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            v(2, 1_500.0, Some(5_000)),
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
        let result = batch_match(&videos, &gyros, Some(&pairs), &[]);
        assert!(result.global_offset_ms.is_some());
        // The manual path uses assign_gyro_to_videos -> v0/v1 should be CalibrationPair, v2 Matched.
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[2].status, MatchStatus::Matched);
    }

    // --- clip_bounds_ok tests ---

    #[test]
    fn clip_bounds_ok_full_coverage() {
        // 30s video, content window [3500, 32500] entirely inside [0, 120000].
        let ok = clip_bounds_ok(2000.0, 34000.0, 1500.0, 1500.0, 30000.0, 120000.0);
        assert!(ok, "video content fully covered must pass");
    }

    #[test]
    fn clip_bounds_ok_completely_out_of_bounds_rejects() {
        // Cross-day mis-pair: gyro_start_ms projected to -48h.
        let ok = clip_bounds_ok(
            -48.0 * 3_600_000.0,
            -48.0 * 3_600_000.0 + 33_000.0,
            1500.0,
            1500.0,
            30_000.0,
            600_000.0,
        );
        assert!(!ok, "fully out-of-bounds clip must reject");
    }

    #[test]
    fn clip_bounds_ok_short_video_at_70pct_boundary_passes() {
        // 10s video, content window [0, 10000], gyro 7000ms.
        // covered = 7000, required = max(7000, 7000) = 7000.
        let ok = clip_bounds_ok(-1500.0, 11500.0, 1500.0, 1500.0, 10_000.0, 7000.0);
        assert!(ok, "10s video with 70% coverage must pass at boundary");
    }

    #[test]
    fn clip_bounds_ok_short_video_below_70pct_rejects() {
        // 10s video, content window [0, 10000], gyro 6900ms (69%).
        let ok = clip_bounds_ok(-1500.0, 11500.0, 1500.0, 1500.0, 10_000.0, 6900.0);
        assert!(!ok, "10s video at 69% coverage must reject");
    }

    #[test]
    fn clip_bounds_ok_short_video_loses_3s_passes() {
        // 10s video losing 3s tail. content_window=[0,10000], gyro 7000.
        // covered=7000, required=max(7000, 7000)=7000 -> pass.
        let ok = clip_bounds_ok(-1500.0, 11500.0, 1500.0, 1500.0, 10_000.0, 7000.0);
        assert!(ok, "short video losing exactly 3s must pass");
    }

    #[test]
    fn clip_bounds_ok_short_video_loses_4s_rejects() {
        // 10s video losing 4s tail. covered=6000, required=max(7000, 6000)=7000 -> reject.
        let ok = clip_bounds_ok(-1500.0, 11500.0, 1500.0, 1500.0, 10_000.0, 6000.0);
        assert!(!ok, "short video losing 4s must reject");
    }

    #[test]
    fn clip_bounds_ok_long_video_loses_3s_passes() {
        // 300s video losing 3s tail. content_window=[0, 300000], gyro=297000.
        // covered=297000, required=max(210000, 297000)=297000 -> pass.
        let ok = clip_bounds_ok(-1500.0, 301500.0, 1500.0, 1500.0, 300_000.0, 297_000.0);
        assert!(ok, "long video losing exactly 3s must pass");
    }

    #[test]
    fn clip_bounds_ok_long_video_loses_4s_rejects() {
        // 300s video losing 4s tail. covered=296000, required=max(210000, 297000)=297000.
        let ok = clip_bounds_ok(-1500.0, 301500.0, 1500.0, 1500.0, 300_000.0, 296_000.0);
        assert!(!ok, "long video losing 4s must reject");
    }

    #[test]
    fn clip_bounds_ok_gyro_duration_zero_rejects() {
        // Corrupted / empty gyro file.
        let ok = clip_bounds_ok(-1500.0, 11500.0, 1500.0, 1500.0, 10_000.0, 0.0);
        assert!(!ok, "gyro_duration=0 must reject");
    }

    // --- AnchorPool tests ---

    #[test]
    fn anchor_pool_empty_is_inlier_returns_true() {
        let pool = AnchorPool::new();
        assert!(pool.is_inlier(0));
        assert!(pool.is_inlier(i64::MAX / 2));
    }

    #[test]
    fn anchor_pool_single_element_boundary() {
        let mut pool = AnchorPool::new();
        pool.push(100);
        // median = 100, tolerance = 15_000 ms.
        assert!(pool.is_inlier(100 - 15_000));
        assert!(pool.is_inlier(100 + 15_000));
        assert!(!pool.is_inlier(100 - 15_001));
        assert!(!pool.is_inlier(100 + 15_001));
    }

    #[test]
    fn anchor_pool_5_element_median() {
        let mut pool = AnchorPool::new();
        for off in [100, 200, 300, 5000, 800] {
            pool.push(off);
        }
        // Sorted: [100, 200, 300, 800, 5000]; median (index 2) = 300.
        assert_eq!(pool.median(), Some(300));
        assert!(pool.is_inlier(350));
        assert!(pool.is_inlier(300 + 15_000));
        assert!(!pool.is_inlier(300 + 15_001));
    }

    #[test]
    fn anchor_pool_outlier_push_does_not_corrupt_median() {
        let mut pool = AnchorPool::new();
        pool.push(100);
        pool.push(110);
        pool.push(120);
        // Pre-outlier median = 110.
        assert_eq!(pool.median(), Some(110));
        // Push outlier - median moves but stays anchored to bulk.
        pool.push(50_000_000);
        // Sorted: [100, 110, 120, 50_000_000]; median (index 2) = 120.
        assert_eq!(pool.median(), Some(120));
        // 120 + 15_000 = 15_120 still passes; 30_000 ~within 15s of 120 fails.
        assert!(pool.is_inlier(120 + 15_000));
        assert!(!pool.is_inlier(120 + 15_001));
    }

    // --- Layer 3 (clip_bounds_ok) integration tests ---

    // --- Section 6 fallback integration tests ---

    #[test]
    fn manual_pairs_mismatched_clip_bounds_unmatched() {
        // User specifies a manual cal pair where v and g durations + times
        // don't actually line up. The manual_pairs path runs slicing anyway
        // but Layer-3 clip bounds gate catches the mis-pair and flags it as
        // Unmatched (rather than silently emitting a broken clip window).
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),
            v(1, 5_000.0, Some(31_000)),
            // Wildly mis-timed video - user accidentally mapped it to g2.
            v(2, 60_000.0, Some(50_000_000)),
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_000),
            // Short gyro (10s) - cannot physically cover the 60s v2 with any
            // sensible offset; only present so the manual pair lookup finds
            // something in the coverage window.
            g(2, 10_000.0, 50_000_000),
        ];
        let pairs = vec![
            ManualCalibrationPair { job_id: 0, video_index: 0, gyro_index: 0 },
            ManualCalibrationPair { job_id: 1, video_index: 1, gyro_index: 1 },
        ];
        let result = batch_match(&videos, &gyros, Some(&pairs), &[]);
        // v0/v1 cal pair, fine.
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair);
        // v2: short gyro g2 doesn't cover 60s video content -> Layer 3 rejects.
        assert_eq!(
            result.results[2].status,
            MatchStatus::Unmatched,
            "manual_pairs path must reject when clip bounds out of range"
        );
        assert!(result.results[2].gyro_index.is_none());
    }

    #[test]
    fn fallback_borrow_offset_not_gyro_finds_day_of_video_imu() {
        // Day-1 cal session forms; day-2 content video has no day-2 cal pair
        // but a day-2 long IMU sits in the pool. Fallback should borrow day-1
        // offset and locate the day-2 IMU via the full-pool binary search.
        let day: i64 = 86_400_000;
        let day1_offset: i64 = -180_000;
        let videos = vec![
            // Day-1 cal pair
            v(0, 1_500.0, Some(0)),
            v(1, 2_000.0, Some(30_000)),
            // Day-2 content (no own cal pair, but covered by day-2 long IMU)
            v(2, 60_000.0, Some(day + 5 * 60_000)),
        ];
        let gyros = vec![
            // Day-1 cal pair
            g(0, 1_500.0, day1_offset),
            g(1, 2_000.0, day1_offset + 30_000),
            // Day-1 cal long IMU
            g(2, 1_800_000.0, day1_offset + 10_000),
            // Day-2 long IMU - offset-compensated so it covers v2.
            g(3, 5_400_000.0, day + 5 * 60_000 - 60_000 + day1_offset),
        ];
        let result = batch_match(&videos, &gyros, None, &[]);
        // 1 reliable session (day-1).
        assert_eq!(result.global_offset_ms, Some(day1_offset));
        // v2 day-2 content matched via day-2 IMU (g3) - either Matched (single
        // reliable session owns g3 via assign_gyro_ownership and coverage path
        // hits) or MatchedFallback (g3 not owned/not covered directly, fallback
        // path borrows day1 offset and searches full pool).
        let v2 = &result.results[2];
        assert!(
            matches!(v2.status, MatchStatus::Matched | MatchStatus::MatchedFallback),
            "v2 (day-2 content) expected Matched|MatchedFallback, got {:?}",
            v2.status
        );
        assert_eq!(v2.global_offset_ms, Some(day1_offset));
        assert_eq!(v2.gyro_index, Some(3), "v2 must use day-2 long IMU g3");
    }

    #[test]
    fn fallback_no_covering_gyro_stays_unmatched() {
        // Day-1 cal session forms; day-2 video has no covering gyro at all.
        // Fallback runs (within 36h fallback gap) but the full-pool search
        // returns no gyro that physically covers v with the borrowed offset.
        let day1_offset: i64 = -180_000;
        let videos = vec![
            v(0, 1_500.0, Some(0)),
            v(1, 2_000.0, Some(30_000)),
            // 12h content video - no day-2 IMU exists.
            v(2, 60_000.0, Some(12 * 3_600_000)),
        ];
        let gyros = vec![
            g(0, 1_500.0, day1_offset),
            g(1, 2_000.0, day1_offset + 30_000),
            // Day-1 long IMU - only 30 minutes long, doesn't reach 12h.
            g(2, 1_800_000.0, day1_offset + 10_000),
        ];
        let result = batch_match(&videos, &gyros, None, &[]);
        assert_eq!(result.global_offset_ms, Some(day1_offset));
        let v2 = &result.results[2];
        assert_eq!(
            v2.status,
            MatchStatus::Unmatched,
            "v2 (no covering gyro) must be Unmatched"
        );
        assert_eq!(v2.gyro_index, None);
    }

    #[test]
    fn assign_coverage_layer3_rejects_out_of_bounds_clip() {
        // Construct a session whose offset is internally consistent (passes
        // pair_sessions) but the assigned gyro physically cannot cover the
        // video content. Direct call to assign_videos_by_coverage with a
        // hand-built fixture so we control offset and gyro duration.
        let videos = vec![v(0, 30_000.0, Some(0))];
        // Short gyro at t=0, dur=5000. Video content [0, 30000] would need
        // gyro to cover most of it; only 5s available.
        let gyros = vec![g(0, 5_000.0, 0)];
        let sessions = vec![Session {
            v_cluster: vec![],
            cal_video_indices: vec![],
            cal_pairs: vec![],
            g_cluster: vec![0],
            anchor_ms: 0,
            offset: 0,
            delay: 0,
            reliable: true,
        }];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let (results, pending) = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, &[]);
        assert_eq!(results[0].status, MatchStatus::Unmatched);
        assert!(results[0].gyro_index.is_none());
        assert_eq!(pending, vec![0], "rejected video must be in pending for fallback");
    }

    // --- pair_sessions anchor-pool regression: cross-day mis-pair rejection ---

    #[test]
    fn pair_two_v_cluster_one_g_cluster_cross_day_outlier_rejected() {
        // Reproducer for user sid 45b144da: 2 V clusters + 1 G cluster, with
        // day2 V cluster anchor ~48h after G cluster. Both V clusters are
        // single-candidate (only one G exists). Pass 1 processes day1 first
        // (seeds the anchor pool with day1's plausible offset), then day2
        // (its offset differs by ~48h - rejected as outlier).
        let day: i64 = 86_400_000;
        let videos = vec![
            // Day1 V cluster (cal)
            v(0, 5_000.0, Some(0)),
            v(1, 5_000.0, Some(30_000)),
            // Day3 V cluster (cal) - mis-aligned by 2 days.
            v(2, 5_000.0, Some(2 * day)),
            v(3, 5_000.0, Some(2 * day + 30_000)),
        ];
        let gyros = vec![
            // Single G cluster on day1 (matches V0/V1 offsets).
            g(0, 5_500.0, 100),
            g(1, 5_500.0, 30_100),
        ];
        // 2 V clusters, both single-candidate against the lone G cluster.
        let sessions = pair_sessions(
            vec![vec![0, 1], vec![2, 3]],
            vec![vec![0, 1]],
            &videos,
            &gyros,
        );
        // Expectation: day1 V cluster locks; day2 V cluster orphans because
        // its offset (~+2*86400000 ms) is wildly inconsistent with day1's
        // (~+100 ms).
        assert_eq!(
            sessions.len(),
            1,
            "day2 V cluster must orphan via anchor-pool RANSAC rejection"
        );
        // The surviving session must be the day1 one (anchor close to 0).
        assert!(sessions[0].anchor_ms.abs() < day, "surviving session must be day1");
    }

    // --- User reproducer sid 20260520-e7834212 ---
    //
    // Real user data: 89 videos + 26 gyros across 2026-05-12..2026-05-16 (mix.bin
    // available for those days) plus 21 Canon content videos shot on 2026-05-17
    // with NO 2026-05-17 mix.bin file dragged in. Per spec: 5/17 videos must be
    // Unmatched because no day-of gyro exists, the older fallback ("borrow gyro
    // from owned set") would mis-pair them to a 5/16 long IMU and produce a clip
    // window 91M+ ms out of the gyro's [0, dur] range.

    #[test]
    fn user_repro_e7834212_canon_5_17_videos_stay_unmatched() {
        // Setup: 4 cal sessions (5/12, 5/13, 5/15, 5/16) + 21 Canon content
        // videos shot on 5/17 (no 5/17 gyro in pool). New code must keep all
        // 21 videos Unmatched (no covering gyro within ±5 binary-search window).
        //
        // V cluster anchors (from log line 14807-14812):
        //   V0=[6,7]   anchor=1778591766340  (5/12 anchor)
        //   V1=[12,13] anchor=1778654049300  (5/13 anchor)
        //   V3=[42,43] anchor=1778826450750  (5/15 anchor)
        //   V5=[53,54] anchor=1778897422640  (5/16 anchor)
        // Gyro cluster anchors:
        //   G0=[2,3]   anchor=1778591379000  -> session 0 offset=-387195
        //   G1=[4,5]   anchor=1778653661000  -> session 1 offset=-388370
        //   G2=[13,14] anchor=1778826060000  -> session 2 offset=-390700
        //   G3=[20,21] anchor=1778897030000  -> session 3 offset=-392290
        // 21 Canon 5/17 video timestamps (from log line 14956-14976 fallback_used):
        //   v_created = session_3.anchor + gap_ms

        // 8 cal videos forming 4 V clusters (2 each) - timestamps and
        // durations reverse-engineered from log lines 14827/14830/14833/14836
        // (candidate dur_diff + offset0/offset1) so cluster spacing exactly
        // matches gyro spacing (|delta_v - delta_g| ≤ SYNC_CREATE_OFFSET_MAX).
        let videos = vec![
            // V0 cal pair @ 5/12 (anchor=1778591766340, offsets -387340/-387050).
            v(0, 2570.0, Some(1778591766340)), // = g0_t + 387340
            v(1, 4471.0, Some(1778591772050)), // = g1_t + 387050
            // V1 cal pair @ 5/13 (anchor=1778654049300, offsets -388300/-388440).
            v(2, 2002.0, Some(1778654049300)), // = g2_t + 388300
            v(3, 2870.0, Some(1778654053440)), // = g3_t + 388440
            // V3 cal pair @ 5/15 (anchor=1778826450750, offsets -390750/-390650).
            v(4, 2102.0, Some(1778826450750)), // = g4_t + 390750
            v(5, 2202.0, Some(1778826455650)), // = g5_t + 390650
            // V5 cal pair @ 5/16 (anchor=1778897422640, offsets -392640/-391940).
            v(6, 2169.0, Some(1778897422640)), // = g6_t + 392640
            v(7, 2602.0, Some(1778897426940)), // = g7_t + 391940
        ];

        // 21 Canon 5/17 content videos (v_idx=8..28). Timestamps computed from
        // fallback_used gap_ms entries in the log relative to session 3 anchor.
        let mut videos = videos;
        let session3_anchor: i64 = 1778897422640;
        let canon_5_17: &[(i64, f64)] = &[
            // (gap_ms_from_session3_anchor, video_dur_ms_approx)
            (115579540, 16450.0), // vi=68
            (116689510, 9676.0),  // vi=69
            (116707030, 12546.0), // vi=70
            (116854680, 21722.0), // vi=71
            (117496120, 7808.0),  // vi=72
            (117908620, 23724.0), // vi=73
            (117980790, 30230.0), // vi=74
            (118036750, 26226.0), // vi=75
            (118093300, 64631.0), // vi=76 (1 min content)
            (118194270, 12913.0), // vi=77
            (118210620, 6206.0),  // vi=78
            (118561480, 23924.0), // vi=79
            (119265040, 16016.0), // vi=80
            (119288660, 27995.0), // vi=81
            (121041460, 14548.0), // vi=82
            (121120470, 14915.0), // vi=83
            (121153500, 72739.0), // vi=84 (1.2 min content)
            (121235720, 15449.0), // vi=85
            (121327060, 32633.0), // vi=86
            (122390050, 25492.0), // vi=87
            (122419740, 10177.0), // vi=88
        ];
        for (i, &(gap, dur)) in canon_5_17.iter().enumerate() {
            let v_t = session3_anchor + gap;
            videos.push(v(8 + i, dur, Some(v_t)));
        }

        // Gyro files. 4 cal pairs (2 gyros each = 8 total) form 4 G clusters.
        // Plus a 5/16 long IMU (mimicking the user's gyro_index=25 = 540s mix
        // that OLD code mistakenly matched the 5/17 videos against).
        // No 5/17 gyro is added: this is the whole point of the test.
        let gyros = vec![
            // G0 cal pair @ 5/12 (anchor=1778591379000).
            g(0, 3453.0, 1778591379000),
            g(1, 5179.0, 1778591385000),
            // G1 cal pair @ 5/13 (anchor=1778653661000).
            g(2, 2302.0, 1778653661000),
            g(3, 3453.0, 1778653665000),
            // G2 cal pair @ 5/15 (anchor=1778826060000).
            g(4, 2302.0, 1778826060000),
            g(5, 2877.0, 1778826065000),
            // G3 cal pair @ 5/16 (anchor=1778897030000).
            g(6, 2302.0, 1778897030000),
            g(7, 2877.0, 1778897035000),
            // 5/16 long IMU 17:11 = the gyro_index=25 from log
            // (created_at=1778921503000, dur=540324). This is what OLD code
            // would project the 5/17 videos onto (91M+ms out of range).
            g(8, 540324.0, 1778921503000),
        ];

        let result = batch_match(&videos, &gyros, None, &[]);

        // 4 reliable sessions expected -> global_offset is None (multi-session).
        assert!(
            result.global_offset_ms.is_none(),
            "expected multi-session run (4 sessions), got global_offset={:?}",
            result.global_offset_ms
        );

        // V cluster cal videos (v_idx 0..7) should all be CalibrationPair.
        for vi in 0..8 {
            let r = &result.results[vi];
            assert_eq!(
                r.status,
                MatchStatus::CalibrationPair,
                "cal v{} expected CalibrationPair, got {:?}",
                vi,
                r.status
            );
        }

        // The 21 Canon 5/17 videos (v_idx 8..28) MUST all be Unmatched.
        // OLD code would have MatchedFallback'd them to gyro_index=8 (the
        // 5/16 long IMU) producing a 91M+ms out-of-range clip window.
        // NEW code's strict coverage check rejects them.
        let mut unmatched_canon = 0;
        for vi in 8..29 {
            let r = &result.results[vi];
            assert_eq!(
                r.status,
                MatchStatus::Unmatched,
                "Canon 5/17 v{} (gap={}h from 5/16 cal) expected Unmatched, got {:?}",
                vi,
                (videos[vi].created_at_ms.unwrap() - session3_anchor) / 3_600_000,
                r.status
            );
            assert_eq!(r.gyro_index, None);
            assert_eq!(r.global_offset_ms, None);
            unmatched_canon += 1;
        }
        assert_eq!(unmatched_canon, 21, "all 21 Canon 5/17 videos must be Unmatched");

        // Buttons unblock when all videos have a deterministic status (not
        // pending in any way). Verify no MatchResult is left in some
        // half-Matched state (gyro_index Some but status Unmatched, or
        // gyro_index None but status Matched).
        for r in &result.results {
            match r.status {
                MatchStatus::Matched
                | MatchStatus::MatchedFallback
                | MatchStatus::CalibrationPair => {
                    assert!(r.gyro_index.is_some(),
                        "v{} matched-state must have gyro_index", r.video_index);
                    assert!(r.global_offset_ms.is_some(),
                        "v{} matched-state must have global_offset_ms", r.video_index);
                }
                MatchStatus::Unmatched | MatchStatus::NoCreationTime => {
                    assert!(r.gyro_index.is_none(),
                        "v{} unmatched must have no gyro_index", r.video_index);
                    assert!(r.global_offset_ms.is_none(),
                        "v{} unmatched must have no global_offset_ms", r.video_index);
                }
            }
        }
    }

    // --- render-queue-deep-gyro-match 7.1: anchor conversion sign convention ---

    #[test]
    fn deep_anchor_session_offset_places_content_at_minus_deep_offset() {
        // Field-test shape (2026-06-11): deep match accepted -344530.715ms,
        // i.e. the video content starts at gyro file-relative +344530.715ms.
        // Feeding the derived session offset through compute_clip_window must
        // place the video-content portion of the clip window (gyro_start_ms +
        // front_comp) back at exactly -deep_offset_ms (up to the i64 rounding
        // of the session-offset domain, < 0.5ms). No pre_recording or
        // COMP_TIME correction term is involved: COMP_TIME pads the window
        // symmetrically (cancelled by adding front_comp back) and
        // pre_recording only participates in duration filters.
        let cases: &[f64] = &[-344_530.715, 344_530.715, -12.0, 0.0];
        for &deep_offset_ms in cases {
            let g_created: i64 = 1_780_000_000_000;
            let v_created: i64 = 1_780_000_700_000; // arbitrary camera clock
            let video = v(0, 60_000.0, Some(v_created));
            let gyro = g(0, 7_200_000.0, g_created);

            let session_offset =
                derive_session_offset_from_deep_match(g_created, v_created, deep_offset_ms);

            // Anchored sessions use delay = 0, so video_offset == session_offset.
            let (gyro_start_ms, gyro_end_ms, front_comp, _calib_anchor) = compute_clip_window(
                &video,
                &gyro,
                v_created,
                session_offset,
                &[],
                std::slice::from_ref(&video),
            );
            let content_start_ms = gyro_start_ms + front_comp;
            assert!(
                (content_start_ms - (-deep_offset_ms)).abs() <= 0.5,
                "deep_offset={}ms: content start {}ms must equal -deep_offset {}ms (±0.5ms i64 rounding)",
                deep_offset_ms,
                content_start_ms,
                -deep_offset_ms
            );
            // Window stays a sane superset of the content range.
            assert!(gyro_start_ms < content_start_ms);
            assert!(gyro_end_ms > content_start_ms + video.duration_ms);
        }
    }

    // --- render-queue-deep-gyro-match 7.3: anchor integration ---

    #[test]
    fn deep_anchor_alone_builds_session_and_assigns_whole_batch() {
        // Spec scenario "One deep match anchors the whole day's batch": long
        // content videos + one long gyro file, no calibration clusters at all.
        // Without an anchor this is NoCalibrationPairsFound; with one anchored
        // job, every video covered by the gyro gets its segment via the
        // anchor-derived session offset.
        let deep_offset_ms = -344_530.7f64;
        let g_created: i64 = 1_000_000_000_000;
        let gyros = vec![g(0, 2_000_000.0, g_created)]; // 33min gyro, no cal cluster

        let v0_created: i64 = 1_000_000_500_000;
        let derived =
            derive_session_offset_from_deep_match(g_created, v0_created, deep_offset_ms);
        // Camera-clock time of the gyro file start under the derived offset.
        let video_start = g_created - derived;
        let videos = vec![
            v(0, 60_000.0, Some(v0_created)), // deep-matched job (content at gyro +344530.7ms)
            v(1, 60_000.0, Some(video_start + 100_000)), // same session, near gyro start
            v(2, 60_000.0, Some(video_start + 1_900_000)), // same session, near gyro end
        ];

        // Without an anchor: nothing matches.
        let bare = batch_match(&videos, &gyros, None, &[]);
        assert_eq!(bare.error, Some(MatchError::NoCalibrationPairsFound));

        let anchors = vec![DeepMatchAnchor {
            gyro_index: 0,
            video_index: 0,
            offset_ms: deep_offset_ms,
            video_created_at_ms: Some(v0_created),
        }];
        let result = batch_match(&videos, &gyros, None, &anchors);
        assert_eq!(result.error, None);
        assert_eq!(result.global_offset_ms, Some(derived));
        for r in &result.results {
            assert_eq!(
                r.status,
                MatchStatus::Matched,
                "v{} expected Matched via the anchor session, got {:?}",
                r.video_index,
                r.status
            );
            assert_eq!(r.gyro_index, Some(0));
            assert_eq!(r.global_offset_ms, Some(derived));
        }
        // The deep-matched job itself goes through normal assignment and its
        // content start must land back at gyro file-relative -deep_offset_ms.
        let r0 = &result.results[0];
        let content_start = r0.gyro_start_ms.unwrap() - r0.init_offset_ms.unwrap();
        assert!(
            (content_start - (-deep_offset_ms)).abs() <= 0.5,
            "deep job content start {}ms must equal -deep_offset {}ms",
            content_start,
            -deep_offset_ms
        );
    }

    #[test]
    fn deep_anchor_overrides_creation_time_session() {
        // Spec scenario "Deep anchor outranks creation-time candidates": a
        // calibration-pair session exists (offset 1100, +/-1.5s class), and a
        // deep anchor on a long gyro of the same clock pair measures 1500.
        // The anchor must override the session offset (no extra session).
        let videos = vec![
            v(0, 5_000.0, Some(1_000)),   // cal pair
            v(1, 5_000.0, Some(31_000)),  // cal pair
            v(2, 60_000.0, Some(248_500)), // deep-matched content video
        ];
        let gyros = vec![
            g(0, 5_500.0, 2_000),
            g(1, 5_500.0, 32_200),
            g(2, 600_000.0, 200_000), // long gyro, deep-matched against v2
        ];
        // True offset 1500 => gyro start at camera clock 198500, v2 content
        // at gyro file-relative 50000 => deep_offset = -50000.
        let deep_offset_ms = -50_000.0f64;
        let derived = derive_session_offset_from_deep_match(
            gyros[2].created_at_ms,
            videos[2].created_at_ms.unwrap(),
            deep_offset_ms,
        );
        assert_eq!(derived, 1_500);

        let anchors = vec![DeepMatchAnchor {
            gyro_index: 2,
            video_index: 2,
            offset_ms: deep_offset_ms,
            video_created_at_ms: videos[2].created_at_ms,
        }];
        let result = batch_match(&videos, &gyros, None, &anchors);
        assert_eq!(result.error, None);
        // Single session, overridden offset: global offset is the derived one.
        assert_eq!(
            result.global_offset_ms,
            Some(derived),
            "anchor must override the creation-time session offset (1100)"
        );
        assert_eq!(result.results[0].status, MatchStatus::CalibrationPair);
        assert_eq!(result.results[1].status, MatchStatus::CalibrationPair);
        let r2 = &result.results[2];
        assert_eq!(r2.status, MatchStatus::Matched);
        assert_eq!(r2.gyro_index, Some(2));
        assert_eq!(r2.global_offset_ms, Some(derived));
        let content_start = r2.gyro_start_ms.unwrap() - r2.init_offset_ms.unwrap();
        assert!(
            (content_start - 50_000.0).abs() <= 0.5,
            "v2 content start {}ms must equal 50000ms under the overridden offset",
            content_start
        );
    }

    #[test]
    fn deep_anchor_without_created_at_degrades_to_self_only() {
        // Spec scenario "Anchor without creation time degrades to self-only":
        // the batch result must be identical to a run without any anchor.
        let g_created: i64 = 1_000_000_000_000;
        let gyros = vec![g(0, 2_000_000.0, g_created)];
        let videos = vec![
            v(0, 60_000.0, Some(1_000_000_500_000)),
            v(1, 60_000.0, Some(1_000_000_600_000)),
        ];
        let anchors = vec![DeepMatchAnchor {
            gyro_index: 0,
            video_index: 0,
            offset_ms: -344_530.7,
            video_created_at_ms: None,
        }];
        let with_anchor = batch_match(&videos, &gyros, None, &anchors);
        let without = batch_match(&videos, &gyros, None, &[]);
        assert_eq!(with_anchor.error, without.error);
        assert_eq!(with_anchor.global_offset_ms, without.global_offset_ms);
        for (a, b) in with_anchor.results.iter().zip(without.results.iter()) {
            assert_eq!(a.status, b.status);
            assert_eq!(a.gyro_index, b.gyro_index);
            assert_eq!(a.global_offset_ms, b.global_offset_ms);
        }
    }

    // --- deep-match-decouple-from-auto-match: deep-pin short-circuit ---

    #[test]
    fn deep_pin_overrides_deeper_covering_session() {
        // Task 5.1: a deep-matched clip whose wall-clock time is ALSO covered
        // (more deeply) by another session's gyro must keep its own deep gyro
        // and deep offset (pinned), not the covering session's gyro/offset.
        // Built directly on assign_videos_by_coverage so the competing session
        // is fully deterministic.
        let g_deep_created: i64 = 1_000_000_000_000;
        let v_created: i64 = 1_000_000_500_000;
        let deep_offset_ms = -120_000.0f64; // content at gyro file-relative +120000ms

        // gyro 0: the deep gyro (long). gyro 1: a competing session's gyro that
        // also wall-clock-covers the clip, even more deeply.
        let gyros = vec![
            g(0, 2_000_000.0, g_deep_created),
            g(1, 2_000_000.0, v_created - 1_000_000), // centred on the clip => deep coverage
        ];
        let videos = vec![v(0, 60_000.0, Some(v_created))];

        // A reliable competing session owning gyro 1 with offset that places
        // the clip near the centre of gyro 1 (maximal coverage depth). Without
        // the pin this session would win the coverage competition.
        let competing_offset = gyros[1].created_at_ms - v_created; // video_start == v_created
        let sessions = vec![Session {
            v_cluster: vec![],
            cal_video_indices: vec![],
            cal_pairs: vec![],
            g_cluster: vec![1],
            anchor_ms: v_created,
            offset: competing_offset,
            delay: 0,
            reliable: true,
        }];
        let owned = assign_gyro_ownership(&gyros, &sessions);

        let derived =
            derive_session_offset_from_deep_match(g_deep_created, v_created, deep_offset_ms);
        let anchors = vec![DeepMatchAnchor {
            gyro_index: 0,
            video_index: 0,
            offset_ms: deep_offset_ms,
            video_created_at_ms: Some(v_created),
        }];

        let (results, pending) =
            assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, &anchors);
        assert!(pending.is_empty());
        let r0 = &results[0];
        // Pinned to the DEEP gyro (0), not the competing session's gyro (1).
        assert_eq!(r0.gyro_index, Some(0), "must keep the deep gyro, not the covering session's");
        assert_eq!(r0.status, MatchStatus::Matched);
        assert_eq!(r0.global_offset_ms, Some(derived));
        // Sign lock: content start (= gyro_start_ms - init_offset_ms) lands at
        // -deep_offset_ms, byte-aligned with finish_deep_match's convention.
        let content_start = r0.gyro_start_ms.unwrap() - r0.init_offset_ms.unwrap();
        assert!(
            (content_start - (-deep_offset_ms)).abs() <= 0.5,
            "content start {}ms must equal -deep_offset {}ms",
            content_start,
            -deep_offset_ms
        );
        // init_offset_ms == -front_comp == -(COMP_TIME_MS + drift_comp). drift_comp
        // is a small positive term (no cal video inside the projected window), so
        // init_offset is at or just below -COMP_TIME_MS, never above it.
        let init_off = r0.init_offset_ms.unwrap();
        assert!(
            init_off <= -COMP_TIME_MS + 0.5,
            "init_offset {}ms must be <= -COMP_TIME {}ms (= -front_comp)",
            init_off,
            -COMP_TIME_MS
        );
    }

    #[test]
    fn deep_pin_takes_precedence_over_calibration_pair() {
        // Task 5.2 (pin-before-cal): a deep-matched clip that ALSO sits in a
        // session's cal_video_indices must go through the deep pin, not the
        // CalibrationPair short-circuit.
        let g_deep_created: i64 = 2_000_000_000_000;
        let v_created: i64 = 2_000_000_300_000;
        let deep_offset_ms = -77_000.0f64;
        let gyros = vec![
            g(0, 2_000_000.0, g_deep_created), // deep gyro
            g(1, 5_500.0, v_created + 2_000),  // a "cal" gyro for the same clip
        ];
        let videos = vec![v(0, 5_000.0, Some(v_created))];
        // Session marks v0 as a calibration video paired with gyro 1.
        let sessions = vec![Session {
            v_cluster: vec![0],
            cal_video_indices: vec![0],
            cal_pairs: vec![(0usize, 1usize)],
            g_cluster: vec![1],
            anchor_ms: v_created,
            offset: 2_000,
            delay: 0,
            reliable: true,
        }];
        let owned = assign_gyro_ownership(&gyros, &sessions);
        let anchors = vec![DeepMatchAnchor {
            gyro_index: 0,
            video_index: 0,
            offset_ms: deep_offset_ms,
            video_created_at_ms: Some(v_created),
        }];

        let (results, _pending) =
            assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, &anchors);
        let r0 = &results[0];
        // Deep pin wins: gyro 0 + Matched, NOT CalibrationPair on gyro 1.
        assert_eq!(r0.gyro_index, Some(0));
        assert_eq!(r0.status, MatchStatus::Matched);
        assert_ne!(r0.status, MatchStatus::CalibrationPair);
    }

    #[test]
    fn deep_pin_per_day_isolation_via_batch_match() {
        // Task 5.2 (cross-day): two deep matches on clips from two different
        // days (clock-inconsistent). Each day's clip is pinned to its own deep
        // gyro/offset; the day-1 anchor is NOT borrowed by the day-2 clip.
        let day1_g: i64 = 1_000_000_000_000;
        let day2_g: i64 = day1_g + 3 * 86_400_000; // 3 days later

        let v1_created: i64 = day1_g + 500_000;
        let v2_created: i64 = day2_g + 700_000;
        let deep1 = -200_000.0f64;
        let deep2 = -350_000.0f64;

        let gyros = vec![
            g(0, 2_000_000.0, day1_g), // day-1 long gyro
            g(1, 2_000_000.0, day2_g), // day-2 long gyro
        ];
        let videos = vec![
            v(0, 60_000.0, Some(v1_created)), // deep-matched day-1 clip
            v(1, 60_000.0, Some(v2_created)), // deep-matched day-2 clip
        ];
        let anchors = vec![
            DeepMatchAnchor {
                gyro_index: 0,
                video_index: 0,
                offset_ms: deep1,
                video_created_at_ms: Some(v1_created),
            },
            DeepMatchAnchor {
                gyro_index: 1,
                video_index: 1,
                offset_ms: deep2,
                video_created_at_ms: Some(v2_created),
            },
        ];

        let result = batch_match(&videos, &gyros, None, &anchors);
        assert_eq!(result.error, None);
        // Day-1 clip pinned to gyro 0 at -deep1; day-2 clip pinned to gyro 1
        // at -deep2. No cross-day borrowing.
        let r0 = &result.results[0];
        let r1 = &result.results[1];
        assert_eq!(r0.gyro_index, Some(0));
        assert_eq!(r1.gyro_index, Some(1));
        let cs0 = r0.gyro_start_ms.unwrap() - r0.init_offset_ms.unwrap();
        let cs1 = r1.gyro_start_ms.unwrap() - r1.init_offset_ms.unwrap();
        assert!((cs0 - (-deep1)).abs() <= 0.5, "day-1 content start {}", cs0);
        assert!((cs1 - (-deep2)).abs() <= 0.5, "day-2 content start {}", cs1);
    }

    #[test]
    fn deep_pin_empty_or_unmatched_anchor_is_byte_equivalent() {
        // Task 5.3: with an empty anchor slice — or anchors that target a
        // different video_index — assign_videos_by_coverage produces results
        // byte-identical to the pre-change behaviour for the unpinned videos.
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

        let baseline = assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, &[]);

        // Anchor targets a non-existent video_index (99) => never pins => same.
        let unmatched_anchor = vec![DeepMatchAnchor {
            gyro_index: 2,
            video_index: 99,
            offset_ms: -50_000.0,
            video_created_at_ms: Some(5_000),
        }];
        // Anchor with no created_at on video_index 2 => degrades to self-only,
        // does NOT pin => same as baseline too.
        let no_created_anchor = vec![DeepMatchAnchor {
            gyro_index: 2,
            video_index: 2,
            offset_ms: -50_000.0,
            video_created_at_ms: None,
        }];

        for anchors in [unmatched_anchor.as_slice(), no_created_anchor.as_slice()] {
            let (results, pending) =
                assign_videos_by_coverage(&videos, &gyros, &sessions, &owned, anchors);
            assert_eq!(pending, baseline.1, "pending must match baseline");
            assert_eq!(results.len(), baseline.0.len());
            for (a, b) in results.iter().zip(baseline.0.iter()) {
                assert_eq!(a.video_index, b.video_index);
                assert_eq!(a.gyro_index, b.gyro_index);
                assert_eq!(a.status, b.status);
                assert_eq!(a.global_offset_ms, b.global_offset_ms);
                assert_eq!(a.gyro_start_ms, b.gyro_start_ms);
                assert_eq!(a.gyro_end_ms, b.gyro_end_ms);
                assert_eq!(a.init_offset_ms, b.init_offset_ms);
            }
        }
    }
}
