// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

const CROSS_VIDEO_SUPPORT_MS: f64 = 1500.0;

#[derive(Default, Debug, Clone, PartialEq)]
pub struct BatchSyncPointDiagnostic {
    pub invalid_numeric: bool,
    pub outside_video_subset: bool,
    pub insufficient_cross_video_support: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BatchSyncPointCandidate {
    pub job_id: u32,
    pub timestamp_ms: f64,
    pub offset_ms: f64,
    pub cost: f64,
    pub confidence: f64,
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
            confidence_sum,
            confidence_average: confidence_sum / points.len() as f64,
        }
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
            if is_point_numeric_valid(point) {
                valid.push(point.clone());
            } else {
                let mut discarded = point.clone();
                discarded.diagnostic.invalid_numeric = true;
                discarded_by_job.entry(*job_id).or_default().push(discarded);
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
    let best_band_points = best_band
        .as_ref()
        .map(|band| band.point_ids.iter().copied().collect::<HashSet<_>>())
        .unwrap_or_default();

    let mut videos = Vec::new();
    for job_id in grouped.keys().copied() {
        let mut confirmed_points = Vec::new();
        let mut discarded_points = discarded_by_job.remove(&job_id).unwrap_or_default();

        for point in subset_by_job.remove(&job_id).unwrap_or_default() {
            let support_count = support_by_point_id
                .get(&point.id)
                .copied()
                .unwrap_or_default();
            let confirmed = match job_count {
                0 => false,
                1 => true,
                2 => support_count >= 1,
                _ => support_count >= 2 || best_band_points.contains(&point.id),
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

fn batch_status_for_counts(green_count: usize, total: usize) -> BatchSyncBatchStatus {
    match (green_count, total) {
        (_, 0) => BatchSyncBatchStatus::Empty,
        (0, _) => BatchSyncBatchStatus::AllYellow,
        (green, total) if green == total => BatchSyncBatchStatus::AllGreen,
        _ => BatchSyncBatchStatus::Mixed,
    }
}

fn is_point_numeric_valid(point: &BatchSyncPoint) -> bool {
    point.timestamp_ms.is_finite()
        && point.offset_ms.is_finite()
        && point.cost.is_finite()
        && point.confidence.is_finite()
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
    fn coarse_bands_do_not_chain_offsets_beyond_1500ms_span() {
        let bands = coarse_consistency_bands(&[
            point(1, 1000.0, 0.0, 0.9).with_id(0),
            point(2, 1000.0, 1400.0, 0.9).with_id(1),
            point(3, 1000.0, 2800.0, 0.9).with_id(2),
        ]);

        assert!(bands.iter().all(|band| band.offset_span_ms <= 1500.0));
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
}
