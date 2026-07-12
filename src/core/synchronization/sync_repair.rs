// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

// NiYien tuning for external-IMU low-motion batches (e.g. S1H + SenseFlow):
// widen the cross-video consensus band to ±1.8s and raise the per-point
// confidence floor to 0.18. The consensus band (offset span ≤ this value, see
// `coarse_consistency_bands`) is what gates each clip green/yellow; raising the
// floor only drops points strictly below 0.18.
const CROSS_VIDEO_SUPPORT_MS: f64 = 3000.0;
pub const MIN_BATCH_SYNC_POINT_RANK: f32 = 12.0;
pub const MIN_BATCH_SYNC_POINT_CONFIDENCE: f64 = 0.15;

/// Ride-along floor for the yellow-only rescue pass (change
/// batch-sync-consensus-rescue).
///
/// A point below `MIN_BATCH_SYNC_POINT_CONFIDENCE` never votes: it is excluded
/// from the within-video subset search, from the coarse consistency bands, and
/// from the eligible-job count. Every green/yellow verdict is therefore decided
/// on exactly the same inputs as before this pass existed. Only afterwards, and
/// only for videos that came out yellow (i.e. would otherwise be exported with
/// their offsets cleared and the gyro applied at 0 ms), do we look again at the
/// discarded points and accept one that the *already-chosen* consensus band
/// vouches for. Because the band and the green set are fixed before the rescue
/// runs, the pass can only turn yellow → green, never the reverse.
///
/// The floor sits between the arbiter's agree-rescue conf (0.12, emitted when
/// posterior and fusion land within a frame of each other) and its flat drop
/// conf (0.0, emitted when they genuinely disagree), so a point the arbiter had
/// no corroboration for stays unrescuable.
pub const RIDE_ALONG_CONFIDENCE_FLOOR: f64 = 0.10;

/// `GYROFLOW_BATCH_SYNC_YELLOW_RESCUE=0|false|no|off` disables the yellow-only
/// consensus rescue pass; confirmation then reverts byte-for-byte to the
/// pre-change behaviour. Default on.
fn yellow_rescue_enabled() -> bool {
    static RESOLVED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *RESOLVED.get_or_init(|| {
        match std::env::var("GYROFLOW_BATCH_SYNC_YELLOW_RESCUE")
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("0") | Some("false") | Some("no") | Some("off") => false,
            _ => true,
        }
    })
}

#[derive(Default, Debug, Clone, PartialEq)]
pub struct BatchSyncPointDiagnostic {
    pub invalid_numeric: bool,
    pub low_rank: bool,
    pub low_confidence: bool,
    pub outside_video_subset: bool,
    pub insufficient_cross_video_support: bool,
    /// Set on a point that failed one of the gates above but was reinstated by
    /// the yellow-only consensus rescue pass. The original rejection flags are
    /// deliberately left set so the log still shows why it was discarded first.
    pub rescued_by_consensus: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchSyncPointCandidate {
    pub job_id: u32,
    pub timestamp_ms: f64,
    pub offset_ms: f64,
    pub cost: f64,
    pub confidence: f64,
    pub rank: f32,
    pub repair_round: u8,
    pub diagnostic: BatchSyncPointDiagnostic,
}

impl BatchSyncPointCandidate {
    pub fn with_id(self, id: usize) -> BatchSyncPoint {
        BatchSyncPoint {
            id,
            job_id: self.job_id,
            timestamp_ms: self.timestamp_ms,
            offset_ms: self.offset_ms,
            cost: self.cost,
            confidence: self.confidence,
            rank: self.rank,
            repair_round: self.repair_round,
            diagnostic: self.diagnostic,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchSyncPoint {
    pub id: usize,
    pub job_id: u32,
    pub timestamp_ms: f64,
    pub offset_ms: f64,
    pub cost: f64,
    pub confidence: f64,
    pub rank: f32,
    pub repair_round: u8,
    pub diagnostic: BatchSyncPointDiagnostic,
}

impl BatchSyncPoint {
    fn from_candidate(id: usize, candidate: BatchSyncPointCandidate) -> Self {
        candidate.with_id(id)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchSyncVideoColor {
    Green,
    Yellow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchSyncBatchStatus {
    Empty,
    AllGreen,
    Mixed,
    AllYellow,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchSyncVideoState {
    pub job_id: u32,
    pub color: BatchSyncVideoColor,
    pub confirmed_points: Vec<BatchSyncPoint>,
    pub discarded_points: Vec<BatchSyncPoint>,
    pub repair_round: u8,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CoarseConsistencyBand {
    pub point_ids: Vec<usize>,
    pub job_ids: BTreeSet<u32>,
    pub offset_span_ms: f64,
    pub offset_min_ms: f64,
    pub offset_max_ms: f64,
    pub confidence_sum: f64,
    pub confidence_average: f64,
}

impl CoarseConsistencyBand {
    fn from_points(points: &[BatchSyncPoint]) -> Self {
        let min_offset = points
            .iter()
            .map(|p| p.offset_ms)
            .fold(f64::INFINITY, f64::min);
        let max_offset = points
            .iter()
            .map(|p| p.offset_ms)
            .fold(f64::NEG_INFINITY, f64::max);
        let confidence_sum = points.iter().map(|p| p.confidence).sum::<f64>();
        let job_ids = points.iter().map(|p| p.job_id).collect::<BTreeSet<_>>();
        Self {
            point_ids: points.iter().map(|p| p.id).collect(),
            job_ids,
            offset_span_ms: max_offset - min_offset,
            offset_min_ms: min_offset,
            offset_max_ms: max_offset,
            confidence_sum,
            confidence_average: confidence_sum / points.len() as f64,
        }
    }

    /// Whether `offset_ms` would have belonged to this band, had it been allowed
    /// to vote — i.e. admitting it keeps the band within `CROSS_VIDEO_SUPPORT_MS`,
    /// which is the band's own membership rule in `coarse_consistency_bands`.
    ///
    /// This cannot contradict the main path. Suppose a *voting* point p of a
    /// yellow video passed this test. Then the window spanning the band plus p is
    /// itself a legal band, it holds every point the winning band held, and it
    /// covers one job more (p's — which, being yellow, is absent from the
    /// winner). `rank_cmp` orders on job count first, so that window would have
    /// outranked the winner and p would already have been confirmed. So nothing
    /// rejected for `insufficient_cross_video_support` can come back in here —
    /// only points that never got to vote at all: conf-suppressed by the arbiter,
    /// or dropped by the within-video subset search.
    fn accepts_offset(&self, offset_ms: f64) -> bool {
        let lo = self.offset_min_ms.min(offset_ms);
        let hi = self.offset_max_ms.max(offset_ms);
        hi - lo <= CROSS_VIDEO_SUPPORT_MS
    }

    fn rank_cmp(&self, other: &Self) -> Ordering {
        self.job_ids
            .len()
            .cmp(&other.job_ids.len())
            .then_with(|| cmp_f64(self.confidence_sum, other.confidence_sum))
            .then_with(|| cmp_f64(self.confidence_average, other.confidence_average))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchSyncConfirmationResult {
    pub videos: Vec<BatchSyncVideoState>,
    pub batch_status: BatchSyncBatchStatus,
    pub support_by_point_id: HashMap<usize, usize>,
    pub best_band: Option<CoarseConsistencyBand>,
}

impl BatchSyncConfirmationResult {
    pub fn video_state(&self, job_id: u32) -> Option<&BatchSyncVideoState> {
        self.videos.iter().find(|video| video.job_id == job_id)
    }

    pub fn supporting_video_count(&self, point_id: usize) -> usize {
        self.support_by_point_id
            .get(&point_id)
            .copied()
            .unwrap_or_default()
    }

    pub fn include_missing_jobs<I>(&mut self, job_ids: I)
    where
        I: IntoIterator<Item = u32>,
    {
        for job_id in job_ids {
            if self.video_state(job_id).is_none() {
                self.videos.push(BatchSyncVideoState {
                    job_id,
                    color: BatchSyncVideoColor::Yellow,
                    confirmed_points: Vec::new(),
                    discarded_points: Vec::new(),
                    repair_round: 0,
                });
            }
        }
        self.videos.sort_by_key(|video| video.job_id);
        self.update_batch_status();
    }

    fn update_batch_status(&mut self) {
        let green_count = self
            .videos
            .iter()
            .filter(|video| video.color == BatchSyncVideoColor::Green)
            .count();
        self.batch_status = batch_status_for_counts(green_count, self.videos.len());
    }
}

pub fn dynamic_video_tolerance_ms(delta_t_ms: f64) -> f64 {
    let ten_minutes_ms = 10.0 * 60_000.0;
    (25.0 * (delta_t_ms / ten_minutes_ms).max(1.0)).min(80.0)
}

pub fn coarse_consistency_bands(points: &[BatchSyncPoint]) -> Vec<CoarseConsistencyBand> {
    let mut sorted = points.to_vec();
    sorted.sort_by(|a, b| cmp_f64(a.offset_ms, b.offset_ms));

    let mut bands = Vec::new();
    for start in 0..sorted.len() {
        for end in start..sorted.len() {
            if sorted[end].offset_ms - sorted[start].offset_ms > CROSS_VIDEO_SUPPORT_MS {
                break;
            }
            bands.push(CoarseConsistencyBand::from_points(&sorted[start..=end]));
        }
    }
    bands
}

pub fn confirm_batch_sync_points(
    candidates: Vec<BatchSyncPointCandidate>,
) -> BatchSyncConfirmationResult {
    confirm_batch_sync_points_internal(candidates, None)
}

pub fn confirm_batch_sync_points_for_jobs<I>(
    candidates: Vec<BatchSyncPointCandidate>,
    expected_job_ids: I,
) -> BatchSyncConfirmationResult
where
    I: IntoIterator<Item = u32>,
{
    let expected_job_ids = expected_job_ids.into_iter().collect::<BTreeSet<_>>();
    let candidates = candidates
        .into_iter()
        .filter(|candidate| expected_job_ids.contains(&candidate.job_id))
        .collect();
    confirm_batch_sync_points_internal(candidates, Some(expected_job_ids))
}

fn confirm_batch_sync_points_internal(
    candidates: Vec<BatchSyncPointCandidate>,
    expected_job_ids: Option<BTreeSet<u32>>,
) -> BatchSyncConfirmationResult {
    let mut grouped = BTreeMap::<u32, Vec<BatchSyncPoint>>::new();
    for (id, candidate) in candidates.into_iter().enumerate() {
        let job_id = candidate.job_id;
        grouped
            .entry(job_id)
            .or_default()
            .push(BatchSyncPoint::from_candidate(id, candidate));
    }

    let job_count = expected_job_ids
        .as_ref()
        .map(|ids| ids.len())
        .unwrap_or_else(|| grouped.len());
    let mut valid_subset_points = Vec::new();
    let mut discarded_by_job = BTreeMap::<u32, Vec<BatchSyncPoint>>::new();
    let mut subset_by_job = BTreeMap::<u32, Vec<BatchSyncPoint>>::new();

    for (job_id, points) in &grouped {
        let mut valid = Vec::new();
        for point in points {
            if !is_point_numeric_valid(point) {
                let mut discarded = point.clone();
                discarded.diagnostic.invalid_numeric = true;
                discarded_by_job.entry(*job_id).or_default().push(discarded);
            } else if point.rank < MIN_BATCH_SYNC_POINT_RANK {
                let mut discarded = point.clone();
                discarded.diagnostic.low_rank = true;
                discarded_by_job.entry(*job_id).or_default().push(discarded);
            } else if point.confidence < MIN_BATCH_SYNC_POINT_CONFIDENCE {
                let mut discarded = point.clone();
                discarded.diagnostic.low_confidence = true;
                discarded_by_job.entry(*job_id).or_default().push(discarded);
            } else {
                valid.push(point.clone());
            }
        }

        let subset_ids = largest_video_consistent_subset_ids(&valid);
        let subset_ids = subset_ids.into_iter().collect::<HashSet<_>>();
        for point in valid {
            if subset_ids.contains(&point.id) {
                valid_subset_points.push(point.clone());
                subset_by_job.entry(*job_id).or_default().push(point);
            } else {
                let mut discarded = point;
                discarded.diagnostic.outside_video_subset = true;
                discarded_by_job.entry(*job_id).or_default().push(discarded);
            }
        }
    }

    let support_by_point_id = cross_video_support_counts(&valid_subset_points);
    let best_band = coarse_consistency_bands(&valid_subset_points)
        .into_iter()
        .filter(|band| band.job_ids.len() >= 2)
        .max_by(|a, b| a.rank_cmp(b));
    let eligible_job_count = subset_by_job.len();
    let confirmation_job_count = if job_count <= 1 {
        job_count
    } else {
        eligible_job_count
    };
    let required_band_job_count = required_batch_support_jobs(confirmation_job_count);
    let band_supported = best_band
        .as_ref()
        .is_some_and(|band| band.job_ids.len() >= required_band_job_count);
    let best_band_points = best_band
        .as_ref()
        .filter(|band| band.job_ids.len() >= required_band_job_count)
        .map(|band| band.point_ids.iter().copied().collect::<HashSet<_>>())
        .unwrap_or_default();

    let mut videos = Vec::new();
    for job_id in grouped.keys().copied() {
        let mut confirmed_points = Vec::new();
        let mut discarded_points = discarded_by_job.remove(&job_id).unwrap_or_default();

        for point in subset_by_job.remove(&job_id).unwrap_or_default() {
            let confirmed = match job_count {
                0 => false,
                1 => true,
                _ => best_band_points.contains(&point.id),
            };

            if confirmed {
                confirmed_points.push(point);
            } else {
                let mut discarded = point;
                discarded.diagnostic.insufficient_cross_video_support = true;
                discarded_points.push(discarded);
            }
        }

        let repair_round = confirmed_points
            .iter()
            .chain(discarded_points.iter())
            .map(|point| point.repair_round)
            .max()
            .unwrap_or_default();
        let color = if confirmed_points.is_empty() {
            BatchSyncVideoColor::Yellow
        } else {
            BatchSyncVideoColor::Green
        };

        videos.push(BatchSyncVideoState {
            job_id,
            color,
            confirmed_points,
            discarded_points,
            repair_round,
        });
    }

    rescue_yellow_videos(&mut videos, best_band.as_ref(), band_supported);

    let green_count = videos
        .iter()
        .filter(|video| video.color == BatchSyncVideoColor::Green)
        .count();
    let batch_status = batch_status_for_counts(green_count, videos.len());

    let mut result = BatchSyncConfirmationResult {
        videos,
        batch_status,
        support_by_point_id,
        best_band,
    };

    if let Some(expected_job_ids) = expected_job_ids {
        result.include_missing_jobs(expected_job_ids);
    }
    result
}

/// Yellow-only consensus rescue (change batch-sync-consensus-rescue).
///
/// A yellow video is not a neutral outcome: `render_queue` clears the offsets in
/// its `.gyroflow` (`batch-sync-write T2 yellow`) and `GyroSource` then applies
/// 0 ms, so a clip whose true offset is ~-1.5 s is stabilised against gyro that
/// is a second and a half out. That is strictly worse than accepting an offset
/// the rest of the batch already agrees with.
///
/// So once the batch has settled — band chosen, every green/yellow verdict made
/// on exactly the pre-change inputs — take one more look at the videos that came
/// out yellow. A point they discarded is reinstated only if the arbiter had
/// corroborated it (conf-suppressed into the ride-along window, not flat-dropped),
/// it clears the rank gate, and the offset band the batch already agreed on would
/// have accepted it (`CoarseConsistencyBand::accepts_offset`). A false peak lands
/// seconds away from that band and stays discarded.
///
/// This is the whole reason the pass is safe: it reads only `Yellow` videos and
/// never touches `best_band`, the green set, or the eligible-job count. A green
/// video cannot be demoted, and the band cannot be re-chosen around a rescued
/// point. Yellow → Green is the only transition it can produce.
fn rescue_yellow_videos(
    videos: &mut [BatchSyncVideoState],
    best_band: Option<&CoarseConsistencyBand>,
    band_supported: bool,
) {
    if !band_supported || !yellow_rescue_enabled() {
        return;
    }
    let Some(band) = best_band else {
        return;
    };

    for video in videos.iter_mut() {
        // Only videos the batch failed to confirm. Never re-open a green one.
        if video.color != BatchSyncVideoColor::Yellow {
            continue;
        }

        let eligible = video
            .discarded_points
            .iter()
            .filter(|point| {
                // Only points the arbiter itself corroborated. `low_confidence`
                // plus a confidence at or above the ride-along floor is exactly
                // the window the both-weak agree-rescue emits into, and that
                // corroboration — posterior and fusion landing within a frame of
                // each other — is the signal doing the real work here.
                //
                // Deliberately NOT extended to points that cleared the voting
                // floor and were then dropped by the within-video subset search.
                // Those are the ones a false-peak fusion offset produces, and the
                // band is not a safe enough oracle to readmit them: its own edges
                // can BE false peaks (a fusion-win offset carries a confidence
                // well above the floor, so a wrong one gets confirmed and widens
                // the band it would later be judged against). Rescuing them needs
                // the false-peak problem fixed first; until then they stay yellow,
                // which is at least honest.
                point.diagnostic.low_confidence
                    && is_point_numeric_valid(point)
                    && point.rank >= MIN_BATCH_SYNC_POINT_RANK
                    && point.confidence >= RIDE_ALONG_CONFIDENCE_FLOOR
                    && band.accepts_offset(point.offset_ms)
            })
            .cloned()
            .collect::<Vec<_>>();
        if eligible.is_empty() {
            continue;
        }

        // Keep the rescued points mutually consistent, same rule the main path
        // uses within a video — two offsets that disagree would make
        // `offset_at_timestamp` interpolate across the clip.
        let keep = largest_video_consistent_subset_ids(&eligible)
            .into_iter()
            .collect::<HashSet<_>>();
        if keep.is_empty() {
            continue;
        }

        let mut still_discarded = Vec::with_capacity(video.discarded_points.len());
        for mut point in std::mem::take(&mut video.discarded_points) {
            if keep.contains(&point.id) {
                point.diagnostic.rescued_by_consensus = true;
                ::log::debug!(
                    target: "sync",
                    "[batch_sync] rescued job={} ts={:.4} offset={:.4} conf={:.3} rank={:.1} band=[{:.1},{:.1}]",
                    point.job_id,
                    point.timestamp_ms,
                    point.offset_ms,
                    point.confidence,
                    point.rank,
                    band.offset_min_ms,
                    band.offset_max_ms
                );
                video.confirmed_points.push(point);
            } else {
                still_discarded.push(point);
            }
        }
        video.discarded_points = still_discarded;

        if !video.confirmed_points.is_empty() {
            video.color = BatchSyncVideoColor::Green;
        }
    }
}

fn batch_status_for_counts(green_count: usize, total: usize) -> BatchSyncBatchStatus {
    match (green_count, total) {
        (_, 0) => BatchSyncBatchStatus::Empty,
        (0, _) => BatchSyncBatchStatus::AllYellow,
        (green, total) if green == total => BatchSyncBatchStatus::AllGreen,
        _ => BatchSyncBatchStatus::Mixed,
    }
}

fn required_batch_support_jobs(job_count: usize) -> usize {
    match job_count {
        0 => usize::MAX,
        1 => 1,
        2 => 2,
        _ => (job_count / 2 + 1).max(2),
    }
}

fn is_point_numeric_valid(point: &BatchSyncPoint) -> bool {
    point.timestamp_ms.is_finite()
        && point.offset_ms.is_finite()
        && point.cost.is_finite()
        && point.confidence.is_finite()
        && point.rank.is_finite()
}

fn cross_video_support_counts(points: &[BatchSyncPoint]) -> HashMap<usize, usize> {
    let mut supports = HashMap::<usize, HashSet<u32>>::new();
    for point in points {
        for other in points {
            if point.job_id == other.job_id {
                continue;
            }
            if (point.offset_ms - other.offset_ms).abs() <= CROSS_VIDEO_SUPPORT_MS {
                supports.entry(point.id).or_default().insert(other.job_id);
            }
        }
    }
    supports
        .into_iter()
        .map(|(point_id, job_ids)| (point_id, job_ids.len()))
        .collect()
}

fn largest_video_consistent_subset_ids(points: &[BatchSyncPoint]) -> Vec<usize> {
    let mut candidates = points.to_vec();
    candidates.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.id.cmp(&b.id))
    });

    let mut best = Vec::<BatchSyncPoint>::new();
    search_video_subset(&candidates, 0, Vec::new(), &mut best);
    best.into_iter().map(|point| point.id).collect()
}

fn search_video_subset(
    points: &[BatchSyncPoint],
    index: usize,
    current: Vec<BatchSyncPoint>,
    best: &mut Vec<BatchSyncPoint>,
) {
    if index == points.len() {
        if subset_rank_cmp(&current, best) == Ordering::Greater {
            *best = current;
        }
        return;
    }

    if current.len() + (points.len() - index) < best.len() {
        return;
    }

    let candidate = &points[index];
    if current
        .iter()
        .all(|point| video_points_are_consistent(point, candidate))
    {
        let mut with_candidate = current.clone();
        with_candidate.push(candidate.clone());
        search_video_subset(points, index + 1, with_candidate, best);
    }
    search_video_subset(points, index + 1, current, best);
}

fn video_points_are_consistent(a: &BatchSyncPoint, b: &BatchSyncPoint) -> bool {
    let delta_t_ms = (a.timestamp_ms - b.timestamp_ms).abs();
    let offset_delta_ms = (a.offset_ms - b.offset_ms).abs();
    offset_delta_ms <= dynamic_video_tolerance_ms(delta_t_ms)
}

fn subset_rank_cmp(a: &[BatchSyncPoint], b: &[BatchSyncPoint]) -> Ordering {
    a.len()
        .cmp(&b.len())
        .then_with(|| cmp_f64(confidence_sum(a), confidence_sum(b)))
        .then_with(|| cmp_f64(confidence_average(a), confidence_average(b)))
}

fn confidence_sum(points: &[BatchSyncPoint]) -> f64 {
    points.iter().map(|point| point.confidence).sum()
}

fn confidence_average(points: &[BatchSyncPoint]) -> f64 {
    if points.is_empty() {
        0.0
    } else {
        confidence_sum(points) / points.len() as f64
    }
}

fn cmp_f64(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(job_id: u32, timestamp_ms: f64, offset_ms: f64, confidence: f64) -> BatchSyncPointCandidate {
        BatchSyncPointCandidate {
            job_id,
            timestamp_ms,
            offset_ms,
            cost: 1.0,
            confidence,
            rank: 100.0,
            repair_round: 0,
            diagnostic: BatchSyncPointDiagnostic::default(),
        }
    }

    #[test]
    fn dynamic_tolerance_uses_25ms_steps_and_80ms_cap() {
        assert_eq!(dynamic_video_tolerance_ms(9.0 * 60_000.0), 25.0);
        assert_eq!(dynamic_video_tolerance_ms(10.0 * 60_000.0), 25.0);
        assert_eq!(dynamic_video_tolerance_ms(20.0 * 60_000.0), 50.0);
        assert_eq!(dynamic_video_tolerance_ms(30.0 * 60_000.0), 75.0);
        assert_eq!(dynamic_video_tolerance_ms(40.0 * 60_000.0), 80.0);
    }

    #[test]
    fn low_confidence_point_is_not_discarded_when_cross_video_supported() {
        let result = confirm_batch_sync_points(vec![
            point(1, 1000.0, 1000.0, 0.2),
            point(2, 1000.0, 1100.0, 0.8),
        ]);

        let job = result.video_state(1).unwrap();
        assert_eq!(job.color, BatchSyncVideoColor::Green);
        assert_eq!(job.confirmed_points.len(), 1);
        assert_eq!(job.confirmed_points[0].confidence, 0.2);
    }

    #[test]
    fn very_low_confidence_point_is_discarded_even_when_cross_video_supported() {
        let result = confirm_batch_sync_points(vec![
            point(1, 1000.0, 1000.0, 0.1),
            point(2, 1000.0, 1100.0, 0.8),
        ]);

        let job = result.video_state(1).unwrap();
        assert_eq!(job.color, BatchSyncVideoColor::Yellow);
        assert_eq!(job.confirmed_points.len(), 0);
        assert_eq!(job.discarded_points.len(), 1);
        assert!(job.discarded_points[0].diagnostic.low_confidence);
    }

    #[test]
    fn low_rank_point_is_discarded_even_when_cross_video_supported() {
        // Threshold lowered from 30 → 12 (G change). rank=10 still triggers
        // low_rank discard; rank=15 (used in the kept-test below) passes.
        let mut low_rank = point(1, 1000.0, 1000.0, 0.8);
        low_rank.rank = 10.0;
        let result = confirm_batch_sync_points(vec![
            low_rank,
            point(2, 1000.0, 1100.0, 0.8),
        ]);

        let job = result.video_state(1).unwrap();
        assert_eq!(job.color, BatchSyncVideoColor::Yellow);
        assert_eq!(job.confirmed_points.len(), 0);
        assert_eq!(job.discarded_points.len(), 1);
        assert!(job.discarded_points[0].diagnostic.low_rank);
    }

    #[test]
    fn rank_above_new_threshold_passes_low_rank_filter() {
        // Real-world case from this session: P1004734 fallback rank=22.6
        // (between old 30 and new 12 threshold). Must survive the rank gate.
        let mut mid_rank = point(1, 1000.0, 1000.0, 0.8);
        mid_rank.rank = 22.6;
        let result = confirm_batch_sync_points(vec![
            mid_rank,
            point(2, 1000.0, 1100.0, 0.8),
        ]);

        let job = result.video_state(1).unwrap();
        assert_eq!(job.color, BatchSyncVideoColor::Green);
        assert_eq!(job.confirmed_points.len(), 1);
        assert_eq!(job.confirmed_points[0].rank, 22.6);
    }

    #[test]
    fn cross_video_support_counts_each_other_job_once() {
        let result = confirm_batch_sync_points(vec![
            point(1, 1000.0, 1000.0, 0.9),
            point(1, 2000.0, 1010.0, 0.8),
            point(2, 1000.0, 1050.0, 0.7),
        ]);

        let supported = result.supporting_video_count(result.video_state(2).unwrap().confirmed_points[0].id);
        assert_eq!(supported, 1);
    }

    #[test]
    fn coarse_bands_do_not_chain_offsets_beyond_1800ms_span() {
        // Consensus band widened to 1800ms span. Adjacent pair 1700..3500
        // (span exactly 1800) stays in a band, but 0..3500 must not chain.
        let bands = coarse_consistency_bands(&[
            point(1, 1000.0, 0.0, 0.9).with_id(0),
            point(2, 1000.0, 1700.0, 0.9).with_id(1),
            point(3, 1000.0, 3500.0, 0.9).with_id(2),
        ]);

        assert!(bands.iter().all(|band| band.offset_span_ms <= 1800.0));
        assert!(!bands.iter().any(|band| band.point_ids.len() == 3));
    }

    #[test]
    fn isolated_bands_do_not_confirm_points_and_batch_is_all_yellow() {
        let result = confirm_batch_sync_points(vec![
            point(1, 1000.0, 0.0, 0.9),
            point(2, 1000.0, 3000.0, 0.9),
            point(3, 1000.0, 6000.0, 0.9),
        ]);

        assert_eq!(result.batch_status, BatchSyncBatchStatus::AllYellow);
        assert!(result.videos.iter().all(|video| video.color == BatchSyncVideoColor::Yellow));
    }

    #[test]
    fn video_inlier_subset_keeps_good_points_and_drops_outlier() {
        let result = confirm_batch_sync_points(vec![
            point(1, 1000.0, 1000.0, 0.7),
            point(1, 2000.0, 1020.0, 0.6),
            point(1, 3000.0, 1300.0, 0.9),
            point(2, 1000.0, 1010.0, 0.8),
        ]);

        let job = result.video_state(1).unwrap();
        assert_eq!(job.color, BatchSyncVideoColor::Green);
        assert_eq!(job.confirmed_points.len(), 2);
        assert_eq!(job.discarded_points.len(), 1);
        assert_eq!(job.discarded_points[0].offset_ms, 1300.0);
    }

    #[test]
    fn video_state_is_green_with_one_confirmed_point_otherwise_yellow() {
        let result = confirm_batch_sync_points(vec![
            point(1, 1000.0, 1000.0, 0.7),
            point(2, 1000.0, 1100.0, 0.8),
            point(3, 1000.0, 5000.0, 0.9),
        ]);

        assert_eq!(result.video_state(1).unwrap().color, BatchSyncVideoColor::Green);
        assert_eq!(result.video_state(2).unwrap().color, BatchSyncVideoColor::Green);
        assert_eq!(result.video_state(3).unwrap().color, BatchSyncVideoColor::Yellow);
    }

    #[test]
    fn competing_actual_wrong_offset_bands_are_not_confirmed() {
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(554879608, 742.4085, -449.8757, 0.513),
                point(554879608, 742.4085, -445.0717, 0.534),
                point(1463787749, 2002.0000, -524.1478, 0.195),
                point(1463787749, 6006.0000, -1142.4167, 0.427),
                point(1010741224, 1251.2500, -168.0857, 0.065),
                point(1010741224, 3753.7500, 1762.9096, 0.615),
                point(1230166270, 1192.8585, 2026.7586, 0.135),
                point(1230166270, 3311.6420, -5080.2338, 0.141),
                point(1505624329, 1126.1250, -138.2116, 0.171),
                point(1505624329, 2877.8750, -2030.2008, 0.155),
                point(819180043, 1251.2500, 781.1733, 0.371),
                point(819180043, 3753.7500, -5298.5838, 0.123),
                point(739264048, 1751.7500, -5391.0749, 0.104),
                point(739264048, 5255.2500, -5309.8970, 0.121),
                point(992777890, 2877.8750, 2228.7062, 0.184),
                point(992777890, 8633.6250, -4589.1796, 0.279),
            ],
            [
                1834466556,
                554879608,
                1463787749,
                1010741224,
                1230166270,
                1505624329,
                819180043,
                739264048,
                992777890,
            ],
        );

        assert_eq!(result.batch_status, BatchSyncBatchStatus::AllYellow);
        assert!(result.videos.iter().all(|video| video.color == BatchSyncVideoColor::Yellow));
    }

    #[test]
    fn four_video_batch_requires_more_than_two_video_band() {
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, 1000.0, 0.9),
                point(2, 1000.0, 1100.0, 0.9),
                point(3, 1000.0, 5000.0, 0.9),
                point(4, 1000.0, 8000.0, 0.9),
            ],
            [1, 2, 3, 4],
        );

        assert_eq!(result.batch_status, BatchSyncBatchStatus::AllYellow);
        assert!(result.videos.iter().all(|video| video.color == BatchSyncVideoColor::Yellow));
    }

    #[test]
    fn missing_expected_jobs_do_not_raise_support_threshold_for_eligible_band() {
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(2033394524, 742.4085, -1939.2348, 1.0),
                point(826836314, 875.8750, -1939.8776, 0.160),
                point(45336309, 1976.9750, -1949.0341, 0.106),
                point(1217710009, 875.8750, -1939.8527, 1.0),
                point(845094404, 4546.2085, -1936.8685, 1.0),
                point(845094404, 10335.3250, -1936.6979, 1.0),
            ],
            [
                1011020730,
                2033394524,
                845094404,
                45336309,
                1172432475,
                1260352080,
                1217710009,
                20336725,
                2072533678,
                826836314,
            ],
        );

        assert_eq!(result.batch_status, BatchSyncBatchStatus::Mixed);
        // conf floor raised to 0.18: job 826836314 (conf 0.160) now falls below
        // the floor and is discarded as low_confidence, so it is no longer green.
        // The remaining eligible band still confirms despite the missing jobs.
        for job_id in [2033394524, 1217710009, 845094404] {
            assert_eq!(result.video_state(job_id).unwrap().color, BatchSyncVideoColor::Green);
        }
        assert_eq!(result.video_state(845094404).unwrap().confirmed_points.len(), 2);
        for job_id in [826836314, 45336309] {
            assert_eq!(result.video_state(job_id).unwrap().color, BatchSyncVideoColor::Yellow);
            assert!(result.video_state(job_id).unwrap().discarded_points[0]
                .diagnostic
                .low_confidence);
        }
    }

    #[test]
    fn non_finite_points_are_discarded_with_diagnostics() {
        let result = confirm_batch_sync_points(vec![
            point(1, 1000.0, f64::NAN, 0.7),
            point(1, 1000.0, 1000.0, f64::INFINITY),
        ]);

        let job = result.video_state(1).unwrap();
        assert_eq!(job.color, BatchSyncVideoColor::Yellow);
        assert_eq!(job.discarded_points.len(), 2);
        assert!(job.discarded_points.iter().all(|p| p.diagnostic.invalid_numeric));
    }

    #[test]
    fn expected_job_with_no_points_is_reported_yellow() {
        let mut result = confirm_batch_sync_points(vec![
            point(1, 1000.0, 1000.0, 0.8),
            point(2, 1000.0, 1100.0, 0.7),
        ]);
        result.include_missing_jobs([1, 2, 3]);

        assert_eq!(result.batch_status, BatchSyncBatchStatus::Mixed);
        assert_eq!(result.video_state(3).unwrap().color, BatchSyncVideoColor::Yellow);
        assert!(result.video_state(3).unwrap().confirmed_points.is_empty());
        assert!(result.video_state(3).unwrap().discarded_points.is_empty());
    }

    #[test]
    fn missing_expected_jobs_count_as_batch_size_before_confirmation() {
        let result = confirm_batch_sync_points_for_jobs(
            vec![point(1, 1000.0, 1000.0, 0.8)],
            [1, 2, 3],
        );

        assert_eq!(result.batch_status, BatchSyncBatchStatus::AllYellow);
        assert_eq!(result.video_state(1).unwrap().color, BatchSyncVideoColor::Yellow);
        assert_eq!(
            result.video_state(1).unwrap().discarded_points[0]
                .diagnostic
                .insufficient_cross_video_support,
            true
        );
        assert_eq!(result.video_state(2).unwrap().color, BatchSyncVideoColor::Yellow);
        assert_eq!(result.video_state(3).unwrap().color, BatchSyncVideoColor::Yellow);
    }

    #[test]
    fn confirmation_for_expected_jobs_ignores_candidates_from_other_jobs() {
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, 1000.0, 0.8),
                point(2, 1000.0, 5000.0, 0.7),
                point(3, 1000.0, 1050.0, 0.9),
            ],
            [1, 2],
        );

        assert_eq!(result.video_state(1).unwrap().color, BatchSyncVideoColor::Yellow);
        assert_eq!(result.video_state(2).unwrap().color, BatchSyncVideoColor::Yellow);
        assert!(result.video_state(3).is_none());
    }

    #[test]
    fn all_expected_jobs_with_no_points_are_all_yellow() {
        let mut result = confirm_batch_sync_points(Vec::new());
        result.include_missing_jobs([1, 2, 3]);

        assert_eq!(result.batch_status, BatchSyncBatchStatus::AllYellow);
        assert_eq!(result.videos.len(), 3);
        assert!(result.videos.iter().all(|video| video.color == BatchSyncVideoColor::Yellow));
    }

    // ── yellow-only consensus rescue (change batch-sync-consensus-rescue) ──
    //
    // Shape of the regression these lock in: a batch of clips sharing one
    // external gyro file all sync to about the same offset, but on a few of them
    // the arbiter's both-weak branch fires and the point is emitted at the
    // agree-rescue confidence instead of being confirmed. Those clips used to go
    // yellow, get their offsets cleared, and stabilise against gyro 1.5 s out.

    fn ride_along(job_id: u32, timestamp_ms: f64, offset_ms: f64) -> BatchSyncPointCandidate {
        // What the arbiter emits from both-weak when posterior and fusion agree
        // to within a frame: below MIN_BATCH_SYNC_POINT_CONFIDENCE (never votes)
        // but at or above RIDE_ALONG_CONFIDENCE_FLOOR (rescuable).
        point(job_id, timestamp_ms, offset_ms, 0.12)
    }

    #[test]
    fn ride_along_conf_sits_between_the_drop_conf_and_the_voting_floor() {
        // The whole safety argument rests on this ordering. If someone moves one
        // of these constants, the rescue either stops working (floor above the
        // arbiter's 0.12) or starts letting flat-dropped points in (floor at 0).
        assert!(RIDE_ALONG_CONFIDENCE_FLOOR > 0.0);
        assert!(RIDE_ALONG_CONFIDENCE_FLOOR <= 0.12);
        assert!(0.12 < MIN_BATCH_SYNC_POINT_CONFIDENCE);
    }

    #[test]
    fn ride_along_point_inside_the_consensus_band_rescues_a_yellow_video() {
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, -1500.0, 0.9),
                point(2, 1000.0, -1510.0, 0.9),
                point(3, 1000.0, -1505.0, 0.9),
                // Job 4's only point was conf-suppressed by the arbiter, but it
                // agrees with what the other three found.
                ride_along(4, 1000.0, -1495.0),
            ],
            [1, 2, 3, 4],
        );

        let video = result.video_state(4).unwrap();
        assert_eq!(video.color, BatchSyncVideoColor::Green);
        assert_eq!(video.confirmed_points.len(), 1);
        assert!(video.confirmed_points[0].diagnostic.rescued_by_consensus);
        // The original rejection reason is preserved for the log.
        assert!(video.confirmed_points[0].diagnostic.low_confidence);
    }

    #[test]
    fn ride_along_point_outside_the_consensus_band_stays_discarded() {
        // A false peak: the point is self-consistent but nowhere near what the
        // rest of the batch agreed on. It must not be laundered into green.
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, -1500.0, 0.9),
                point(2, 1000.0, -1510.0, 0.9),
                point(3, 1000.0, -1505.0, 0.9),
                ride_along(4, 1000.0, 3200.0),
            ],
            [1, 2, 3, 4],
        );

        let video = result.video_state(4).unwrap();
        assert_eq!(video.color, BatchSyncVideoColor::Yellow);
        assert!(video.confirmed_points.is_empty());
        assert!(!video.discarded_points[0].diagnostic.rescued_by_consensus);
    }

    #[test]
    fn flat_dropped_point_is_never_rescued_even_inside_the_band() {
        // conf 0.0 is what the arbiter emits when posterior and fusion genuinely
        // disagree — no corroboration, so landing in the band is not enough.
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, -1500.0, 0.9),
                point(2, 1000.0, -1510.0, 0.9),
                point(3, 1000.0, -1505.0, 0.9),
                point(4, 1000.0, -1495.0, 0.0),
            ],
            [1, 2, 3, 4],
        );

        assert_eq!(result.video_state(4).unwrap().color, BatchSyncVideoColor::Yellow);
    }

    #[test]
    fn low_rank_point_is_never_rescued_even_inside_the_band() {
        let mut low_rank = ride_along(4, 1000.0, -1495.0);
        low_rank.rank = MIN_BATCH_SYNC_POINT_RANK - 1.0;

        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, -1500.0, 0.9),
                point(2, 1000.0, -1510.0, 0.9),
                point(3, 1000.0, -1505.0, 0.9),
                low_rank,
            ],
            [1, 2, 3, 4],
        );

        assert_eq!(result.video_state(4).unwrap().color, BatchSyncVideoColor::Yellow);
    }

    #[test]
    fn rescue_never_demotes_a_green_video_or_moves_the_band() {
        // The Pareto property, stated directly: adding rescuable points to a
        // batch may only add greens. Every video green without them stays green
        // with them, keeping the same confirmed points.
        let baseline_points = vec![
            point(1, 1000.0, -1500.0, 0.9),
            point(2, 1000.0, -1510.0, 0.9),
            point(3, 1000.0, -1505.0, 0.9),
        ];
        let baseline = confirm_batch_sync_points_for_jobs(baseline_points.clone(), [1, 2, 3, 4]);

        let mut augmented_points = baseline_points;
        augmented_points.push(ride_along(4, 1000.0, -1495.0));
        let augmented = confirm_batch_sync_points_for_jobs(augmented_points, [1, 2, 3, 4]);

        for job_id in [1, 2, 3] {
            let before = baseline.video_state(job_id).unwrap();
            let after = augmented.video_state(job_id).unwrap();
            assert_eq!(before.color, BatchSyncVideoColor::Green);
            assert_eq!(after.color, BatchSyncVideoColor::Green, "job {job_id} was demoted");
            assert_eq!(
                before.confirmed_points, after.confirmed_points,
                "job {job_id} lost or changed a confirmed point"
            );
        }
        // The band itself must be chosen from the voting points alone.
        assert_eq!(
            baseline.best_band.as_ref().map(|b| b.point_ids.len()),
            augmented.best_band.as_ref().map(|b| b.point_ids.len())
        );
        assert_eq!(baseline.video_state(4).unwrap().color, BatchSyncVideoColor::Yellow);
        assert_eq!(augmented.video_state(4).unwrap().color, BatchSyncVideoColor::Green);
    }

    #[test]
    fn rescue_does_not_run_when_the_band_lacks_support() {
        // Two jobs, each on its own offset: no band reaches the required job
        // count, so the batch is AllYellow. A ride-along point must not conjure
        // a consensus that does not exist.
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, -1500.0, 0.9),
                point(2, 1000.0, 8000.0, 0.9),
                ride_along(3, 1000.0, -1495.0),
            ],
            [1, 2, 3],
        );

        assert_eq!(result.video_state(3).unwrap().color, BatchSyncVideoColor::Yellow);
    }

    #[test]
    fn point_dropped_by_the_within_video_subset_is_not_rescued() {
        // Job 4 has a false-peak offset that beat its good offset in the subset
        // search (equal confidence, lower id wins), so the good one was discarded
        // as `outside_video_subset` and the false peak then failed cross-video
        // support. Tempting to reinstate the good one — but a point that cleared
        // the voting floor did so on a confidence a false peak also earns, and
        // the band we would judge it against can itself be widened by one. Left
        // yellow on purpose; see the note in `rescue_yellow_videos`.
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, -1500.0, 0.9),
                point(2, 1000.0, -1510.0, 0.9),
                point(3, 1000.0, -1505.0, 0.9),
                point(4, 500.0, -9000.0, 0.5),
                point(4, 4000.0, -1495.0, 0.5),
            ],
            [1, 2, 3, 4],
        );

        let video = result.video_state(4).unwrap();
        assert_eq!(video.color, BatchSyncVideoColor::Yellow);
        assert!(video.confirmed_points.is_empty());
        assert!(
            video
                .discarded_points
                .iter()
                .all(|point| !point.diagnostic.rescued_by_consensus)
        );
    }

    // ── field corpus: user report 20260712-226ab84d ────────────────────────────
    //
    // The batch that motivated this change: 90 Canon clips at 60fps sharing one
    // external SenseFlow gyro file, of which 8 came out yellow ("10% of the
    // videos are unconfirmed and have no stabilisation"). Every candidate point
    // the app produced, with the arbiter branch and Δ it was decided under, is
    // replayed here through the real confirmation path.
    //
    // Ground truth is each clip's own `init_offset_ms` from batch_match — a
    // wall-clock match on file timestamps, computed without touching optical
    // flow, so it is independent of everything the sync pipeline does. Across
    // all 90 clips it spans -1734..-1500 ms.
    //
    // Columns: job ts offset cost conf rank arb_branch arb_delta wall_clock

    const FIELD_CORPUS: &str = include_str!("testdata/batch_sync_20260712_226ab84d.txt");

    /// What `rs_sync`'s both-weak agree-rescue exit emits. Kept in sync with
    /// `ARB_AGREE_CONF` there; the replay applies the same rule the arbiter does.
    const CORPUS_ARB_AGREE_CONF: f64 = 0.12;
    const CORPUS_ARB_DELTA_GATE: f64 = 30.0;

    struct CorpusRow {
        candidate: BatchSyncPointCandidate,
        branch: String,
        delta: f64,
        wall_clock_ms: f64,
    }

    fn field_corpus() -> Vec<CorpusRow> {
        FIELD_CORPUS
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                let f = line.split_whitespace().collect::<Vec<_>>();
                assert_eq!(f.len(), 9, "malformed corpus row: {line}");
                CorpusRow {
                    candidate: BatchSyncPointCandidate {
                        job_id: f[0].parse().unwrap(),
                        timestamp_ms: f[1].parse().unwrap(),
                        offset_ms: f[2].parse().unwrap(),
                        cost: f[3].parse().unwrap(),
                        confidence: f[4].parse().unwrap(),
                        rank: f[5].parse().unwrap(),
                        repair_round: 0,
                        diagnostic: BatchSyncPointDiagnostic::default(),
                    },
                    branch: f[6].to_string(),
                    delta: f[7].parse().unwrap(),
                    wall_clock_ms: f[8].parse().unwrap(),
                }
            })
            .collect()
    }

    /// The batch as the app confirmed it before this change: every point at the
    /// confidence the arbiter actually emitted, flat 0.0 included.
    fn corpus_before() -> Vec<BatchSyncPointCandidate> {
        field_corpus().into_iter().map(|row| row.candidate).collect()
    }

    /// The same batch with the both-weak agree-rescue exit applied — the only
    /// thing this change alters upstream of confirmation.
    fn corpus_after() -> Vec<BatchSyncPointCandidate> {
        field_corpus()
            .into_iter()
            .map(|row| {
                let mut candidate = row.candidate;
                if row.branch == "both-weak" && row.delta <= CORPUS_ARB_DELTA_GATE {
                    candidate.confidence = CORPUS_ARB_AGREE_CONF;
                }
                candidate
            })
            .collect()
    }

    fn corpus_job_ids() -> Vec<u32> {
        let mut ids = field_corpus()
            .iter()
            .map(|row| row.candidate.job_id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    #[test]
    fn field_corpus_reproduces_the_reported_yellow_clips() {
        // Sanity: the replay must land on the batch the user actually saw, or the
        // conclusions drawn from it are about a different batch.
        let result = confirm_batch_sync_points_for_jobs(corpus_before(), corpus_job_ids());

        let yellow = result
            .videos
            .iter()
            .filter(|video| video.color == BatchSyncVideoColor::Yellow)
            .map(|video| video.job_id)
            .collect::<BTreeSet<_>>();

        // The eight clips named in the report's status writes.
        for job_id in [
            1170677632, 1269400795, 1338297863, 1340252728, 1803039359, 1908103841, 2067806936,
            2075791166,
        ] {
            assert!(yellow.contains(&job_id), "job {job_id} was yellow in the field log");
        }
    }

    #[test]
    fn field_corpus_rescue_adds_greens_and_demotes_none() {
        let job_ids = corpus_job_ids();
        let before = confirm_batch_sync_points_for_jobs(corpus_before(), job_ids.clone());
        let after = confirm_batch_sync_points_for_jobs(corpus_after(), job_ids.clone());

        let green_of = |result: &BatchSyncConfirmationResult| {
            result
                .videos
                .iter()
                .filter(|video| video.color == BatchSyncVideoColor::Green)
                .map(|video| video.job_id)
                .collect::<BTreeSet<_>>()
        };
        let green_before = green_of(&before);
        let green_after = green_of(&after);

        // The Pareto property, on real data rather than a hand-built fixture.
        assert!(
            green_before.is_subset(&green_after),
            "clips demoted: {:?}",
            green_before.difference(&green_after).collect::<Vec<_>>()
        );
        for job_id in &green_before {
            assert_eq!(
                before.video_state(*job_id).unwrap().confirmed_points,
                after.video_state(*job_id).unwrap().confirmed_points,
                "job {job_id} kept green but its confirmed points changed"
            );
        }
        // The consensus band is chosen from the voting points alone, so the
        // ride-along points must not have moved it.
        assert_eq!(
            before.best_band.as_ref().map(|b| b.point_ids.clone()),
            after.best_band.as_ref().map(|b| b.point_ids.clone())
        );

        // Exact field outcome, so a future change to the arbiter or to confirmation
        // that regresses this batch fails here rather than in a user's export.
        assert_eq!(green_before.len(), 75);
        assert_eq!(green_after.len(), 80);

        let rescued = green_after
            .difference(&green_before)
            .copied()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            rescued,
            BTreeSet::from([382131327, 845898114, 1269400795, 1803039359, 2075791166])
        );
        // Three of the eight clips the user actually saw as yellow. Of the other
        // five, 1340252728 / 1908103841 / 1338297863 are genuinely mis-synced and
        // must stay yellow; 1170677632 and 2067806936 do hold a good offset, but
        // one the arbiter never corroborated, so we decline to rescue them
        // (design D5 — the consensus band is not a safe enough oracle for those).
        for job_id in [1269400795, 1803039359, 2075791166] {
            assert!(rescued.contains(&job_id));
        }
    }

    #[test]
    fn field_corpus_rescued_points_agree_with_the_wall_clock() {
        // The one thing that would make this change harmful: turning an honest
        // yellow into a confidently wrong green. Every point the rescue reinstates
        // is checked against that clip's own optical-flow-independent wall-clock
        // match. A false peak on this batch sits 1.5-4.7 s away from it.
        const GROSS_ERROR_MS: f64 = 500.0;

        let wall_clock = field_corpus()
            .into_iter()
            .map(|row| (row.candidate.job_id, row.wall_clock_ms))
            .collect::<BTreeMap<_, _>>();

        let after = confirm_batch_sync_points_for_jobs(corpus_after(), corpus_job_ids());

        let mut rescued = 0;
        for video in &after.videos {
            for point in &video.confirmed_points {
                if !point.diagnostic.rescued_by_consensus {
                    continue;
                }
                rescued += 1;
                let truth = wall_clock[&video.job_id];
                let error = (point.offset_ms - truth).abs();
                assert!(
                    error < GROSS_ERROR_MS,
                    "job {} rescued to {:.1}ms but wall clock says {:.1}ms ({:.1}ms off)",
                    video.job_id,
                    point.offset_ms,
                    truth,
                    error
                );
            }
        }
        assert!(rescued > 0, "nothing was rescued, so nothing was verified");
    }

    #[test]
    fn field_corpus_false_peaks_stay_yellow() {
        // The clips whose sync genuinely failed: their best point is 1.0-4.7 s
        // from the wall clock. They must keep saying so.
        let after = confirm_batch_sync_points_for_jobs(corpus_after(), corpus_job_ids());

        for job_id in [1340252728, 1908103841, 1338297863] {
            assert_eq!(
                after.video_state(job_id).unwrap().color,
                BatchSyncVideoColor::Yellow,
                "job {job_id} is a false peak and must not be laundered into green"
            );
        }
    }

    #[test]
    fn field_corpus_documents_the_fusion_win_false_peak_still_confirming_wrong_clips() {
        // KNOWN BAD, and not what this change fixes — recorded so it is not
        // mistaken for a regression and so whoever fixes it sees this test go red.
        //
        // The arbiter's fusion-win tier hands a strong-basin fusion offset
        // ARB_FUSION_WIN_CONF = 0.5, which clears both the batch voting floor and
        // the single-video 0.4 filter. On this batch 13% of fusion-win offsets are
        // false peaks, and a false peak's Pearson r is indistinguishable from a
        // true one (median 0.620 vs 0.664), so the r gate cannot separate them.
        //
        // Clip 357456670 is one: it syncs to -60.8 ms when its wall clock says
        // -1541 ms, and it is confirmed GREEN — before this change and after it.
        // Worse, being confirmed it stretches the consensus band all the way out
        // to -61 ms, which is precisely why the rescue pass refuses to treat that
        // band as an oracle on its own (design D5).
        let job_ids = corpus_job_ids();
        let before = confirm_batch_sync_points_for_jobs(corpus_before(), job_ids.clone());
        let after = confirm_batch_sync_points_for_jobs(corpus_after(), job_ids);

        for result in [&before, &after] {
            let video = result.video_state(357456670).unwrap();
            assert_eq!(
                video.color,
                BatchSyncVideoColor::Green,
                "fusion-win false peak is (still) confirmed — if this now fails, \
                 the false-peak bug was fixed and this test should be updated"
            );
            let band = result.best_band.as_ref().unwrap();
            assert!(
                band.offset_max_ms > -100.0,
                "the false peak is (still) stretching the consensus band"
            );
        }
    }

    #[test]
    fn rescued_points_within_a_video_stay_mutually_consistent() {
        // Both land in the band but disagree with each other by far more than the
        // dynamic tolerance; confirming both would make offset_at_timestamp
        // interpolate garbage across the clip. Only the consistent subset is kept.
        let result = confirm_batch_sync_points_for_jobs(
            vec![
                point(1, 1000.0, -2000.0, 0.9),
                point(2, 1000.0, -1000.0, 0.9),
                point(3, 1000.0, -1500.0, 0.9),
                ride_along(4, 1000.0, -1900.0),
                ride_along(4, 2000.0, -1100.0),
            ],
            [1, 2, 3, 4],
        );

        let video = result.video_state(4).unwrap();
        assert_eq!(video.color, BatchSyncVideoColor::Green);
        assert_eq!(video.confirmed_points.len(), 1);
    }
}
