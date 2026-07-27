// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

//! Local clock consensus for batch auto-sync (change batch-sync-dynamic-local-offset).
//!
//! Across multi-day batches the camera and an external IMU logger drift
//! continuously (measured 2521 ms/day on feedback 20260724-da416882). A single
//! deep-search anchor therefore cannot describe the whole batch: reliable sync
//! results form per-day clusters that do not fit one 3000 ms consistency band,
//! and slices cut far from the anchor eventually miss the real data entirely.
//!
//! This module is the pure model behind the fix:
//! - normalization between slice-relative sync offsets and the wall-clock
//!   clock-shift domain (`wall_clock_offset_ms` / `slice_offset_ms`),
//! - per-video evidence built from already-gated sync points (one vote per
//!   video, merged as the median of its largest consistent subset),
//! - local time windows (`LOCAL_WINDOW_MS` full width, pairwise membership),
//! - one best band per window (reusing `sync_repair`'s band enumeration and
//!   ranking so old and new paths cannot fork thresholds),
//! - a physical drift-rate gate guarding every new confirmed value,
//! - `BatchClockState`: the in-memory, non-persisted state the render queue
//!   holds for one batch run.
//!
//! Nothing here touches gyro data or the render queue directly; the runtime
//! integration lives in `render_queue.rs`.

use std::collections::BTreeSet;
use std::ops::Range;

use super::sync_repair::{
    coarse_consistency_bands, BatchSyncPoint, CoarseConsistencyBand, CROSS_VIDEO_SUPPORT_MS,
};

/// Full width of a local consensus window, in hours: two pieces of evidence
/// belong to the same window iff `|t_i - t_j| <= LOCAL_WINDOW_H` (this is NOT
/// a ±radius). Calibrated on feedback 20260724-da416882: the sparse tail pair
/// (HZY_4210/HZY_4221) needs the previous group 1.87h away, so 1.5h fails;
/// same-day clusters span at most 7.1h of capture across 3 days of footage.
pub const LOCAL_WINDOW_H: f64 = 6.0;
pub const LOCAL_WINDOW_MS: f64 = LOCAL_WINDOW_H * 3_600_000.0;

/// Videos required to confirm a local offset. The adaptive floor for small
/// batches is 2 (`required_support_videos`), never 1 — a single video must not
/// confirm itself (fusion-win false peaks carry conf 0.5 and would propagate).
pub const MIN_SUPPORT_VIDEOS: usize = 3;

/// Full offset span a local band may cover. Shares the constant with the
/// cross-video confirmation path so the two can never disagree.
pub const LOCAL_BAND_SPAN_MS: f64 = CROSS_VIDEO_SUPPORT_MS;

/// Physical budget for how fast a confirmed clock shift may move: drift
/// measured on the motivating batch was 2521 ms/day. That figure came from a
/// relay chain recorded before the Decision-13 sign fix (relay hops amplified
/// baseline errors ×2), so the true rate may be up to half that — the Z8
/// live batch measured ~1507 ms/day post-fix. Either way the cap keeps
/// 2.4-4x headroom; kept unchanged.
pub const DRIFT_RATE_CAP_MS_PER_DAY: f64 = 6000.0;

/// Constant tolerance absorbing second-level file-timestamp quantisation
/// (two clips 16s apart in the same gyro file measured 564.6 ms apart).
pub const DRIFT_TOL_MS: f64 = 1500.0;

const DAY_MS: f64 = 86_400_000.0;

// ─── normalization (slice ↔ wall-clock domain) ─────────────────────────────

/// Convert a slice-relative sync offset into the wall-clock clock-shift domain
/// (same domain as `learned_clock_shift_ms` / session offset: gyro clock −
/// video clock).
///
/// `assumed_shift_ms` is the session clock shift the gyro slice was cut with;
/// `init_offset_ms` is the offset a sync would report if that assumption were
/// exactly right (batch_match's per-clip `init_offset_ms`). A sync landing
/// exactly on `init_offset_ms` therefore normalizes to `assumed_shift_ms`
/// itself.
///
/// The residual enters NEGATED: gyroflow offsets map video time to gyro time
/// as `gyro_pos = video_t − offset`, so when the true shift sits Δ beyond the
/// assumption (content Δ later in the gyro file than the slice predicted) the
/// measured sync offset *decreases* by Δ. `sync − init = assumed − true`,
/// hence `true = assumed − (sync − init)`. Empirically pinned by the
/// 2026-07-27 borrowed-baseline live fixtures below (design Decision 13); the
/// pre-fix `assumed + (sync − init)` doubled any baseline error and amplified
/// it ×2 per relay hop.
pub fn wall_clock_offset_ms(sync_offset_ms: f64, init_offset_ms: f64, assumed_shift_ms: f64) -> f64 {
    assumed_shift_ms - (sync_offset_ms - init_offset_ms)
}

/// Inverse of [`wall_clock_offset_ms`]: project a wall-clock clock shift back
/// into the slice-relative domain of a slice cut with `assumed_shift_ms`.
pub fn slice_offset_ms(wall_clock_offset_ms: f64, init_offset_ms: f64, assumed_shift_ms: f64) -> f64 {
    init_offset_ms - (wall_clock_offset_ms - assumed_shift_ms)
}

// ─── evidence ──────────────────────────────────────────────────────────────

/// One video's vote in the local clock consensus. Built only from sync results
/// that fully finished and passed the existing quality gates; ungraded and
/// unweighted — every video counts exactly once.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchClockEvidence {
    pub job_id: u32,
    /// Video capture time, epoch ms.
    pub video_created_at_ms: f64,
    /// Identity of the gyro source the sync ran against.
    pub gyro_id: u64,
    /// IMU session the video was matched into. Consensus never crosses it.
    pub session_id: u64,
    /// Normalized clock shift (gyro − video) — NOT the slice-relative offset.
    pub wall_clock_offset_ms: f64,
    /// `|sync_offset − initial_offset|` of the merged representative, kept so
    /// logs can audit whether the coarse-offset prior systematically pins the
    /// result (it should not; see design "粗 offset 的双重身份").
    pub prior_displacement_ms: f64,
    /// Quality summary (mean confidence of the merged subset).
    pub confidence: f64,
    /// Consensus generation the producing task was started under.
    pub generation: u64,
}

fn median_f64(sorted: &[f64]) -> f64 {
    let n = sorted.len();
    if n % 2 == 1 {
        sorted[n / 2]
    } else {
        (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0
    }
}

/// Merge one video's already-gated sync points into a single vote.
///
/// The merge operator is fixed by the spec: the median of the video's largest
/// self-consistent subset (same subset search the confirmation path uses, so
/// a false peak that lost the within-video vote cannot sneak into consensus).
/// Returns `None` when `points` is empty or no subset survives.
pub fn video_evidence(
    job_id: u32,
    video_created_at_ms: f64,
    gyro_id: u64,
    session_id: u64,
    points: &[BatchSyncPoint],
    init_offset_ms: f64,
    assumed_shift_ms: f64,
    generation: u64,
) -> Option<BatchClockEvidence> {
    let subset_ids = super::sync_repair::largest_video_consistent_subset_ids(points);
    if subset_ids.is_empty() {
        return None;
    }
    let subset_ids: BTreeSet<usize> = subset_ids.into_iter().collect();
    let subset: Vec<&BatchSyncPoint> = points.iter().filter(|p| subset_ids.contains(&p.id)).collect();

    let mut offsets: Vec<f64> = subset.iter().map(|p| p.offset_ms).collect();
    offsets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_slice_offset = median_f64(&offsets);
    let confidence = subset.iter().map(|p| p.confidence).sum::<f64>() / subset.len() as f64;

    Some(BatchClockEvidence {
        job_id,
        video_created_at_ms,
        gyro_id,
        session_id,
        wall_clock_offset_ms: wall_clock_offset_ms(median_slice_offset, init_offset_ms, assumed_shift_ms),
        prior_displacement_ms: (median_slice_offset - init_offset_ms).abs(),
        confidence,
        generation,
    })
}

// ─── local windows ─────────────────────────────────────────────────────────

/// Group evidence (must be sorted by `video_created_at_ms`) into maximal local
/// windows. Membership is pairwise: a window is a maximal run whose extreme
/// members are at most `window_ms` apart (full width, not a radius). Windows
/// may overlap; runs contained in a previous run are not emitted.
pub fn local_windows_with_width(sorted_times_ms: &[f64], window_ms: f64) -> Vec<Range<usize>> {
    let n = sorted_times_ms.len();
    let mut windows = Vec::new();
    let mut prev_end = 0usize;
    let mut end = 0usize;
    for start in 0..n {
        if end < start {
            end = start;
        }
        while end + 1 < n && sorted_times_ms[end + 1] - sorted_times_ms[start] <= window_ms {
            end += 1;
        }
        // Only maximal runs: skip if fully contained in the previous window.
        if windows.is_empty() || end + 1 > prev_end {
            windows.push(start..end + 1);
            prev_end = end + 1;
        }
    }
    windows
}

pub fn local_windows(sorted_times_ms: &[f64]) -> Vec<Range<usize>> {
    local_windows_with_width(sorted_times_ms, LOCAL_WINDOW_MS)
}

// ─── support threshold ─────────────────────────────────────────────────────

/// Videos a band must contain to confirm, adapted to small batches:
/// `clamp(qualified, 2, MIN_SUPPORT_VIDEOS)`. The floor is 2 — with a single
/// qualified video the threshold stays 2 and nothing can ever confirm.
pub fn required_support_videos(batch_qualified_videos: usize) -> usize {
    batch_qualified_videos.clamp(2, MIN_SUPPORT_VIDEOS)
}

// ─── drift-rate gate ───────────────────────────────────────────────────────

/// Budget for how far a candidate confirmed value may sit from a reference
/// confirmed value `delta_t_ms` of capture time away.
pub fn drift_budget_ms(delta_t_ms: f64) -> f64 {
    DRIFT_RATE_CAP_MS_PER_DAY * (delta_t_ms.abs() / DAY_MS) + DRIFT_TOL_MS
}

#[derive(Debug, Clone, PartialEq)]
pub struct DriftGateRejection {
    pub ref_created_at_ms: f64,
    pub ref_offset_ms: f64,
    pub budget_ms: f64,
    pub actual_ms: f64,
}

/// The structural defence replacing the old majority rule: a new confirmed
/// value must be physically reachable from the nearest existing one. Evidence
/// consistency is deliberately NOT an input — three neighbouring clips agreeing
/// on the same false peak still fail this gate.
pub fn drift_gate(
    candidate_offset_ms: f64,
    candidate_created_at_ms: f64,
    reference: Option<&ConfirmedLocalOffset>,
) -> Result<(), DriftGateRejection> {
    let Some(r) = reference else {
        // No reference: nothing to gate against. Normal flow never gets here
        // because an Anchor/Session initial value is registered up front.
        return Ok(());
    };
    let budget = drift_budget_ms(candidate_created_at_ms - r.created_at_ms);
    let actual = (candidate_offset_ms - r.wall_clock_offset_ms).abs();
    if actual <= budget {
        Ok(())
    } else {
        Err(DriftGateRejection {
            ref_created_at_ms: r.created_at_ms,
            ref_offset_ms: r.wall_clock_offset_ms,
            budget_ms: budget,
            actual_ms: actual,
        })
    }
}

// ─── confirmed values and state ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmedOffsetSource {
    /// Registered from a deep-search anchor; reliability vouched by the deep
    /// search itself, exempt from the support-video threshold.
    Anchor,
    /// Registered from a reliable wall-clock `Session.offset` when the batch
    /// had no deep search.
    Session,
    /// Confirmed by local window consensus.
    Local,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmedLocalOffset {
    pub source: ConfirmedOffsetSource,
    pub session_id: u64,
    /// Capture-time position of this confirmed value, epoch ms.
    pub created_at_ms: f64,
    /// Wall-clock clock shift (gyro − video).
    pub wall_clock_offset_ms: f64,
    pub support_videos: usize,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConsensusRejection {
    pub session_id: u64,
    pub created_at_ms: f64,
    pub wall_clock_offset_ms: f64,
    pub support_job_ids: BTreeSet<u32>,
    pub band_span_ms: f64,
    pub rejection: DriftGateRejection,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConsensusOutcome {
    /// Local values confirmed by this recompute (full set, not a delta).
    pub confirmed_local: Vec<ConfirmedLocalOffset>,
    /// Candidates that met the support threshold but failed the drift gate.
    pub rejected: Vec<ConsensusRejection>,
    pub generation: u64,
}

/// Expected drift accumulated over a relay gap of `delta_t_ms`, using the
/// calibrated cap rate (conservative: warns earlier than the measured rate).
pub fn relay_gap_expected_drift_ms(delta_t_ms: f64) -> f64 {
    DRIFT_RATE_CAP_MS_PER_DAY * (delta_t_ms.abs() / DAY_MS)
}

/// Whether the gap between a video and its nearest confirmed value is wide
/// enough to warrant a `relay_gap` warning: expected drift above 60% of
/// `search_size` (the primary convergence criterion `|truth − init| <=
/// search_size`).
pub fn relay_gap_warning(delta_t_ms: f64, search_size_ms: f64) -> bool {
    relay_gap_expected_drift_ms(delta_t_ms) > 0.6 * search_size_ms
}

/// In-memory local clock state for one batch auto-sync run. Held by the render
/// queue, cleared with the batch (never persisted to settings, project files,
/// sidecars or caches).
#[derive(Debug, Clone, Default)]
pub struct BatchClockState {
    /// One evidence entry per job (latest submission wins — repair rounds
    /// resubmit the same job).
    evidence: Vec<BatchClockEvidence>,
    /// Initial values registered from deep-search anchors / session offsets.
    initial: Vec<ConfirmedLocalOffset>,
    /// Values confirmed by local consensus, recomputed from evidence.
    local: Vec<ConfirmedLocalOffset>,
    generation: u64,
}

impl BatchClockState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_empty(&self) -> bool {
        self.evidence.is_empty() && self.initial.is_empty()
    }

    pub fn evidence_count(&self) -> usize {
        self.evidence.len()
    }

    /// Register an initial confirmed value (deep-search anchor or reliable
    /// session offset). Exempt from the support threshold and the drift gate;
    /// it is what the first ordinary candidates are gated against.
    pub fn register_initial(
        &mut self,
        source: ConfirmedOffsetSource,
        session_id: u64,
        created_at_ms: f64,
        wall_clock_offset_ms: f64,
    ) {
        debug_assert!(source != ConfirmedOffsetSource::Local);
        self.generation += 1;
        self.initial.push(ConfirmedLocalOffset {
            source,
            session_id,
            created_at_ms,
            wall_clock_offset_ms,
            support_videos: 0,
            generation: self.generation,
        });
    }

    /// Submit one video's evidence and recompute the consensus. Returns the
    /// outcome for logging. Resubmitting the same job replaces its previous
    /// evidence (one vote per video).
    pub fn submit_evidence(&mut self, evidence: BatchClockEvidence) -> ConsensusOutcome {
        self.evidence.retain(|e| e.job_id != evidence.job_id);
        self.evidence.push(evidence);
        self.recompute_consensus()
    }

    /// Remove one job's evidence (job deleted from the queue).
    pub fn remove_job(&mut self, job_id: u32) {
        let before = self.evidence.len();
        self.evidence.retain(|e| e.job_id != job_id);
        if self.evidence.len() != before {
            self.recompute_consensus();
        }
    }

    /// Drop every locally-confirmed value and all evidence but keep initial
    /// registrations (used when a re-run invalidates ordinary confirmations).
    pub fn invalidate_local(&mut self) {
        self.evidence.clear();
        self.local.clear();
        self.generation += 1;
    }

    /// Drop initial registrations of one source (e.g. Session values are
    /// re-derived after every batch match while Anchor values live with the
    /// learned clock shift).
    pub fn unregister_initial_source(&mut self, source: ConfirmedOffsetSource) {
        let before = self.initial.len();
        self.initial.retain(|c| c.source != source);
        if self.initial.len() != before {
            self.generation += 1;
        }
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn confirmed(&self) -> impl Iterator<Item = &ConfirmedLocalOffset> {
        self.initial.iter().chain(self.local.iter())
    }

    /// Nearest confirmed value by capture time within the same session.
    /// Ties break on more support videos, then newer generation. Initial
    /// values compete on distance like everyone else (no extra priority).
    pub fn nearest_confirmed(&self, created_at_ms: f64, session_id: u64) -> Option<&ConfirmedLocalOffset> {
        self.confirmed()
            .filter(|c| c.session_id == session_id)
            .min_by(|a, b| {
                let da = (a.created_at_ms - created_at_ms).abs();
                let db = (b.created_at_ms - created_at_ms).abs();
                da.partial_cmp(&db)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.support_videos.cmp(&a.support_videos))
                    .then_with(|| b.generation.cmp(&a.generation))
            })
    }

    /// Rebuild the locally-confirmed set from the full evidence pool.
    ///
    /// Deterministic greedy chain per session: candidates (one best band per
    /// local window meeting the adaptive threshold) are accepted nearest-first
    /// relative to the already-confirmed set, so the drift gate always compares
    /// against the closest post — which is exactly the relay semantics.
    fn recompute_consensus(&mut self) -> ConsensusOutcome {
        let mut accepted_all = Vec::new();
        let mut rejected_all = Vec::new();

        let sessions: BTreeSet<u64> = self.evidence.iter().map(|e| e.session_id).collect();
        for session_id in sessions {
            let mut ev: Vec<&BatchClockEvidence> =
                self.evidence.iter().filter(|e| e.session_id == session_id).collect();
            ev.sort_by(|a, b| {
                a.video_created_at_ms
                    .partial_cmp(&b.video_created_at_ms)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.job_id.cmp(&b.job_id))
            });
            let required = required_support_videos(ev.len());

            // Pre-bucket by local window, then find each window's best band.
            // Overlapping windows can elect the same winning band; identical
            // support sets are deduplicated (same set → same median → same
            // candidate).
            let times: Vec<f64> = ev.iter().map(|e| e.video_created_at_ms).collect();
            let mut candidates = Vec::new();
            let mut seen_support: BTreeSet<Vec<u32>> = BTreeSet::new();
            for window in local_windows(&times) {
                let members = &ev[window];
                if members.len() < required {
                    continue;
                }
                // Reuse the confirmation path's band enumeration and ranking
                // (one synthetic point per video, id = index into `members`).
                let points: Vec<BatchSyncPoint> = members
                    .iter()
                    .enumerate()
                    .map(|(i, e)| BatchSyncPoint {
                        id: i,
                        job_id: e.job_id,
                        timestamp_ms: e.video_created_at_ms,
                        offset_ms: e.wall_clock_offset_ms,
                        cost: 0.0,
                        confidence: e.confidence,
                        rank: 100.0,
                        ..Default::default()
                    })
                    .collect();
                let best: Option<CoarseConsistencyBand> = coarse_consistency_bands(&points)
                    .into_iter()
                    .filter(|band| band.job_ids.len() >= required)
                    .max_by(|a, b| a.rank_cmp(b));
                let Some(band) = best else { continue };
                let support_key: Vec<u32> = band.job_ids.iter().copied().collect();
                if !seen_support.insert(support_key) {
                    continue;
                }

                let mut offsets: Vec<f64> = band
                    .point_ids
                    .iter()
                    .map(|&i| members[i].wall_clock_offset_ms)
                    .collect();
                offsets.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mut times_in_band: Vec<f64> = band
                    .point_ids
                    .iter()
                    .map(|&i| members[i].video_created_at_ms)
                    .collect();
                times_in_band.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

                candidates.push((
                    median_f64(&times_in_band),
                    median_f64(&offsets),
                    band.job_ids.clone(),
                    band.offset_span_ms,
                ));
            }

            // Greedy nearest-first chain against initial + already-accepted.
            let refs: Vec<ConfirmedLocalOffset> = self
                .initial
                .iter()
                .filter(|c| c.session_id == session_id)
                .cloned()
                .collect();
            let initial_ref_count = refs.len();
            let mut accepted: Vec<ConfirmedLocalOffset> = Vec::new();
            while !candidates.is_empty() {
                let pick = if refs.is_empty() && accepted.is_empty() {
                    // No reference at all (abnormal flow): seed with the most
                    // supported candidate, ungated per spec.
                    candidates
                        .iter()
                        .enumerate()
                        .max_by(|(_, a), (_, b)| {
                            a.2.len()
                                .cmp(&b.2.len())
                                .then_with(|| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal))
                        })
                        .map(|(i, _)| i)
                        .unwrap()
                } else {
                    // Nearest candidate to any existing post.
                    candidates
                        .iter()
                        .enumerate()
                        .min_by(|(_, a), (_, b)| {
                            let da = nearest_dist(a.0, refs.iter().chain(accepted.iter()));
                            let db = nearest_dist(b.0, refs.iter().chain(accepted.iter()));
                            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
                        })
                        .map(|(i, _)| i)
                        .unwrap()
                };
                let (center_ms, offset_ms, job_ids, span_ms) = candidates.swap_remove(pick);
                let reference = nearest_of(center_ms, refs.iter().chain(accepted.iter()));
                match drift_gate(offset_ms, center_ms, reference) {
                    Ok(()) => accepted.push(ConfirmedLocalOffset {
                        source: ConfirmedOffsetSource::Local,
                        session_id,
                        created_at_ms: center_ms,
                        wall_clock_offset_ms: offset_ms,
                        support_videos: job_ids.len(),
                        generation: self.generation + 1,
                    }),
                    Err(rejection) => rejected_all.push(ConsensusRejection {
                        session_id,
                        created_at_ms: center_ms,
                        wall_clock_offset_ms: offset_ms,
                        support_job_ids: job_ids,
                        band_span_ms: span_ms,
                        rejection,
                    }),
                }
            }
            let _ = initial_ref_count;
            accepted_all.extend(accepted);
        }

        let changed = accepted_all != self.local;
        self.local = accepted_all.clone();
        if changed {
            self.generation += 1;
            for c in self.local.iter_mut() {
                c.generation = self.generation;
            }
        }
        ConsensusOutcome {
            confirmed_local: self.local.clone(),
            rejected: rejected_all,
            generation: self.generation,
        }
    }
}

fn nearest_dist<'a>(t: f64, posts: impl Iterator<Item = &'a ConfirmedLocalOffset>) -> f64 {
    posts
        .map(|c| (c.created_at_ms - t).abs())
        .fold(f64::INFINITY, f64::min)
}

fn nearest_of<'a>(
    t: f64,
    posts: impl Iterator<Item = &'a ConfirmedLocalOffset>,
) -> Option<&'a ConfirmedLocalOffset> {
    posts.min_by(|a, b| {
        let da = (a.created_at_ms - t).abs();
        let db = (b.created_at_ms - t).abs();
        da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: f64 = 3_600_000.0;

    fn pt(id: usize, job_id: u32, ts: f64, offset: f64, conf: f64) -> BatchSyncPoint {
        BatchSyncPoint {
            id,
            job_id,
            timestamp_ms: ts,
            offset_ms: offset,
            cost: 1.0,
            confidence: conf,
            rank: 100.0,
            ..Default::default()
        }
    }

    fn ev(job_id: u32, t_ms: f64, offset_ms: f64) -> BatchClockEvidence {
        ev_conf(job_id, t_ms, offset_ms, 0.9)
    }

    fn ev_conf(job_id: u32, t_ms: f64, offset_ms: f64, confidence: f64) -> BatchClockEvidence {
        BatchClockEvidence {
            job_id,
            video_created_at_ms: t_ms,
            gyro_id: 0,
            session_id: 0,
            wall_clock_offset_ms: offset_ms,
            prior_displacement_ms: 0.0,
            confidence,
            generation: 0,
        }
    }

    // ── 1.1 normalization ──────────────────────────────────────────────────

    #[test]
    fn sync_landing_on_init_normalizes_to_the_assumed_shift() {
        // The anchor case: a sync confirming the wall-clock assumption exactly
        // reports offset == init, and its normalized value is the session
        // shift itself.
        assert_eq!(wall_clock_offset_ms(-1505.6, -1505.6, -6265.0), -6265.0);
    }

    #[test]
    fn positive_residual_moves_the_wall_clock_value_negatively() {
        // Sign convention (design Decision 13): `gyro_pos = video_t − offset`,
        // so a sync offset above init means the true shift sits *below* the
        // assumption: `true = assumed − (sync − init)`.
        assert_eq!(wall_clock_offset_ms(-1400.0, -1500.0, -6265.0), -6365.0);
        assert_eq!(wall_clock_offset_ms(-1600.0, -1500.0, -6265.0), -6165.0);
    }

    #[test]
    fn normalization_round_trips_both_directions() {
        let (sync, init, shift) = (-1234.5, -1500.0, -6265.0);
        let wall = wall_clock_offset_ms(sync, init, shift);
        assert!((slice_offset_ms(wall, init, shift) - sync).abs() < 1e-9);

        let wall2 = -3810.7;
        let slice = slice_offset_ms(wall2, init, shift);
        assert!((wall_clock_offset_ms(slice, init, shift) - wall2).abs() < 1e-9);
    }

    #[test]
    fn reprojecting_into_a_different_slice_preserves_the_wall_clock_value() {
        // A confirmed wall-clock value handed to a differently-cut slice must
        // predict a slice offset that normalizes back to the same value.
        let wall = -6265.0;
        let slice_a = slice_offset_ms(wall, -1500.0, -6265.0);
        let slice_b = slice_offset_ms(wall, -1567.4, -3700.0);
        assert!((wall_clock_offset_ms(slice_a, -1500.0, -6265.0) - wall).abs() < 1e-9);
        assert!((wall_clock_offset_ms(slice_b, -1567.4, -3700.0) - wall).abs() < 1e-9);
    }

    // ── 6.1 borrowed-baseline discrimination (sid 36f327ae live fixture) ──
    //
    // Round-trip tests cannot catch a global sign error: flipping both
    // normalization functions keeps them inverses of each other. The only
    // discriminating scenario is a slice cut with a *borrowed* baseline far
    // from the video's true clock shift, checked against an independent
    // deep-search truth. Numbers below are measured values from the
    // 2026-07-27 live session (sid 36f327ae, Z8 + SenseFlow, two runs of the
    // same 12-video batch anchored on different days).

    #[test]
    fn borrowed_baseline_normalizes_to_the_deep_search_truth() {
        // DSC_1428, run 2: sliced with the 11-19 anchor baseline -193850 ms
        // (borrowed, ~3s off), sync residual measured +3046.0 ms. Its own
        // deep-search truth (frame-verified sign convention) is -196890 ms.
        let init = -1500.0;
        let wall = wall_clock_offset_ms(init + 3046.0, init, -193850.0);
        assert!(
            (wall - -196890.0).abs() <= 10.0,
            "borrowed-baseline evidence must land on the deep-search truth, got {wall}"
        );
    }

    #[test]
    fn two_baselines_for_the_same_video_agree_on_one_truth() {
        // DSC_1430 measured under both anchors: run 1 baseline -196890 ms with
        // residual -247.1 ms, run 2 baseline -193850 ms with residual
        // +2792.6 ms. An honest normalization is baseline-independent: both
        // measurements must recover the same wall-clock shift (~-196643 ms).
        let init = -1500.0;
        let run1 = wall_clock_offset_ms(init - 247.1, init, -196890.0);
        let run2 = wall_clock_offset_ms(init + 2792.6, init, -193850.0);
        assert!(
            (run1 - run2).abs() <= 3.0,
            "same video, two baselines: {run1} vs {run2} must agree"
        );
        assert!((run1 - -196642.9).abs() <= 1.0, "got {run1}");
    }

    // ── 1.3 per-video merge ────────────────────────────────────────────────

    #[test]
    fn video_merges_to_the_median_of_its_largest_consistent_subset() {
        // Three consistent points and one far outlier: the outlier loses the
        // subset vote and the median is taken over the three.
        let points = vec![
            pt(0, 1, 1000.0, -1500.0, 0.9),
            pt(1, 1, 2000.0, -1510.0, 0.8),
            pt(2, 1, 3000.0, -1490.0, 0.7),
            pt(3, 1, 4000.0, 2000.0, 0.9),
        ];
        let e = video_evidence(1, 0.0, 0, 0, &points, -1500.0, -6265.0, 0).unwrap();
        // median(-1510, -1500, -1490) = -1500 → residual 0 → assumed shift.
        assert_eq!(e.wall_clock_offset_ms, -6265.0);
        assert_eq!(e.prior_displacement_ms, 0.0);
    }

    #[test]
    fn even_subset_takes_the_middle_average() {
        let points = vec![
            pt(0, 1, 1000.0, -1500.0, 0.9),
            pt(1, 1, 2000.0, -1510.0, 0.9),
        ];
        let e = video_evidence(1, 0.0, 0, 0, &points, -1500.0, 0.0, 0).unwrap();
        // Median residual is -5.0; under Decision 13 the residual enters
        // negated, so the wall value is +5.0.
        assert_eq!(e.wall_clock_offset_ms, 5.0);
        assert_eq!(e.prior_displacement_ms, 5.0);
    }

    #[test]
    fn one_video_is_one_vote_regardless_of_point_count() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        // Same job submitting twice must not double its weight.
        state.submit_evidence(ev(1, 1.0 * H, 10.0));
        state.submit_evidence(ev(1, 1.0 * H, 12.0));
        assert_eq!(state.evidence_count(), 1);
    }

    #[test]
    fn empty_points_produce_no_evidence() {
        assert!(video_evidence(1, 0.0, 0, 0, &[], -1500.0, 0.0, 0).is_none());
    }

    // ── 1.4 windows ────────────────────────────────────────────────────────

    #[test]
    fn window_membership_is_pairwise_full_width() {
        // T, T+5h, T+7h: T and T+7h must not share a window; the middle joins
        // both. A ±6h radius reading would wrongly merge all three.
        let times = [0.0, 5.0 * H, 7.0 * H];
        let windows = local_windows(&times);
        assert_eq!(windows, vec![0..2, 1..3]);
    }

    #[test]
    fn exactly_six_hours_apart_is_still_one_window() {
        let times = [0.0, 6.0 * H];
        assert_eq!(local_windows(&times), vec![0..2]);
    }

    #[test]
    fn beyond_six_hours_splits_windows() {
        let times = [0.0, 6.0 * H + 1.0];
        assert_eq!(local_windows(&times), vec![0..1, 1..2]);
    }

    #[test]
    fn contained_runs_are_not_emitted_twice() {
        let times = [0.0, 1.0 * H, 2.0 * H];
        assert_eq!(local_windows(&times), vec![0..3]);
    }

    #[test]
    fn multi_day_clusters_form_separate_windows() {
        // Three same-day clusters ~20h apart (the motivating batch shape).
        let times = [0.0, 1.6 * H, 22.0 * H, 23.0 * H, 41.0 * H, 42.0 * H];
        assert_eq!(local_windows(&times), vec![0..2, 2..4, 4..6]);
    }

    // ── 1.5 one best band per window ───────────────────────────────────────

    #[test]
    fn each_window_takes_its_single_best_band() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        // Six videos in one window forming two separate qualifying bands
        // (4000 is out of band reach of -10: span 4010 > 3000). The higher
        // confidence sum wins; the losing band is NOT adopted even though it
        // meets the threshold on its own.
        state.submit_evidence(ev_conf(1, 0.1 * H, -10.0, 0.9));
        state.submit_evidence(ev_conf(2, 0.2 * H, 0.0, 0.9));
        state.submit_evidence(ev_conf(3, 0.3 * H, 10.0, 0.9));
        state.submit_evidence(ev_conf(4, 0.4 * H, 4000.0, 0.5));
        state.submit_evidence(ev_conf(5, 0.5 * H, 4010.0, 0.5));
        let out = state.submit_evidence(ev_conf(6, 0.6 * H, 4020.0, 0.5));

        assert_eq!(out.confirmed_local.len(), 1);
        let c = &out.confirmed_local[0];
        assert_eq!(c.wall_clock_offset_ms, 0.0);
        assert_eq!(c.support_videos, 3);
        assert_eq!(c.source, ConfirmedOffsetSource::Local);
    }

    #[test]
    fn bands_do_not_chain_beyond_the_span_inside_a_window() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        // 0 / 2000 / 4000: total span 4000 > 3000 must not become one band;
        // best band has 2 videos < required 3 → nothing confirms.
        state.submit_evidence(ev(1, 0.1 * H, 0.0));
        state.submit_evidence(ev(2, 0.2 * H, 2000.0));
        let out = state.submit_evidence(ev(3, 0.3 * H, 4000.0));
        assert!(out.confirmed_local.is_empty());
    }

    // ── 1.6 drift gate ─────────────────────────────────────────────────────

    #[test]
    fn over_budget_candidate_is_rejected_and_recorded() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        // 2h away, offset 8000ms: budget = 6000×(2/24)+1500 = 2000 < 8000.
        state.submit_evidence(ev(1, 2.0 * H, 8000.0));
        state.submit_evidence(ev(2, 2.1 * H, 8010.0));
        let out = state.submit_evidence(ev(3, 2.2 * H, 7990.0));

        assert!(out.confirmed_local.is_empty());
        assert_eq!(out.rejected.len(), 1);
        let r = &out.rejected[0];
        assert_eq!(r.rejection.ref_offset_ms, 0.0);
        assert!((r.rejection.budget_ms - 2025.0).abs() < 1.0); // 6000×(2.1/24)+1500
        assert!((r.rejection.actual_ms - 8000.0).abs() < 1.0);
    }

    #[test]
    fn within_budget_true_drift_is_accepted() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        // 22h away, 2900ms: budget = 6000×(22/24)+1500 = 7000 ≥ 2900.
        state.submit_evidence(ev(1, 22.0 * H, 2900.0));
        state.submit_evidence(ev(2, 22.1 * H, 2905.0));
        let out = state.submit_evidence(ev(3, 22.2 * H, 2895.0));

        assert_eq!(out.confirmed_local.len(), 1);
        assert_eq!(out.rejected.len(), 0);
        assert_eq!(out.confirmed_local[0].wall_clock_offset_ms, 2900.0);
    }

    #[test]
    fn consistency_does_not_bypass_the_drift_gate() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        // Ten videos agreeing to within 20ms — systematically consistent false
        // peaks — still rejected: the gate reads the physical budget only.
        let mut out = ConsensusOutcome::default();
        for i in 0..10u32 {
            out = state.submit_evidence(ev(i + 1, 1.0 * H + f64::from(i) * 60_000.0, 9000.0 + f64::from(i)));
        }
        assert!(out.confirmed_local.is_empty());
        assert!(!out.rejected.is_empty());
    }

    #[test]
    fn no_reference_means_no_gate() {
        assert!(drift_gate(123_456.0, 0.0, None).is_ok());

        // State-level: no initial registration (abnormal flow) — the first
        // candidate seeds the chain ungated.
        let mut state = BatchClockState::new();
        state.submit_evidence(ev(1, 0.1 * H, 50_000.0));
        state.submit_evidence(ev(2, 0.2 * H, 50_010.0));
        let out = state.submit_evidence(ev(3, 0.3 * H, 49_990.0));
        assert_eq!(out.confirmed_local.len(), 1);
        assert_eq!(out.confirmed_local[0].wall_clock_offset_ms, 50_000.0);
    }

    #[test]
    fn relay_chain_accepts_stepwise_drift_a_direct_jump_would_fail() {
        // Clusters every 7h drifting 3000ms per step, ending at +12000 after
        // 28h. Direct from the anchor the far cluster is out of budget
        // (6000×28/24+1500 = 8500 < 12000), but each 7h step fits its own
        // budget (6000×7/24+1500 = 3250 ≥ 3000), so the greedy nearest-first
        // chain confirms all the way out.
        let mut chained = BatchClockState::new();
        chained.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        for (i, (t_h, off)) in [(7.0, 3000.0), (14.0, 6000.0), (21.0, 9000.0), (28.0, 12000.0)]
            .into_iter()
            .enumerate()
        {
            let base = (i as u32) * 10;
            chained.submit_evidence(ev(base + 1, t_h * H, off));
            chained.submit_evidence(ev(base + 2, t_h * H + 60_000.0, off + 5.0));
            chained.submit_evidence(ev(base + 3, t_h * H + 120_000.0, off - 5.0));
        }
        let far = chained.nearest_confirmed(28.0 * H, 0).unwrap();
        assert_eq!(far.source, ConfirmedOffsetSource::Local);
        assert!((far.wall_clock_offset_ms - 12000.0).abs() <= 5.0);
        assert!(chained.nearest_confirmed(0.0, 0).unwrap().source == ConfirmedOffsetSource::Anchor);

        // The same far cluster without the intermediate steps is rejected:
        // physically unreachable from the anchor in one jump.
        let mut direct = BatchClockState::new();
        direct.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        direct.submit_evidence(ev(1, 28.0 * H, 12000.0));
        direct.submit_evidence(ev(2, 28.0 * H + 60_000.0, 12005.0));
        let out = direct.submit_evidence(ev(3, 28.0 * H + 120_000.0, 11995.0));
        assert!(out.confirmed_local.is_empty());
        assert_eq!(out.rejected.len(), 1);
        // Candidate center = median time = 28h + 1min → budget ≈ 8504ms.
        let budget = out.rejected[0].rejection.budget_ms;
        assert!(budget > 8500.0 && budget < 8510.0, "budget {budget}");
    }

    // ── 1.7 threshold ──────────────────────────────────────────────────────

    #[test]
    fn threshold_clamps_between_two_and_three() {
        assert_eq!(required_support_videos(0), 2);
        assert_eq!(required_support_videos(1), 2);
        assert_eq!(required_support_videos(2), 2);
        assert_eq!(required_support_videos(3), 3);
        assert_eq!(required_support_videos(65), 3);
    }

    #[test]
    fn a_single_video_never_confirms_anything() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        let out = state.submit_evidence(ev(1, 0.1 * H, 100.0));
        assert!(out.confirmed_local.is_empty());
        assert!(out.rejected.is_empty());
    }

    #[test]
    fn a_two_video_batch_confirms_with_two() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        state.submit_evidence(ev(1, 0.1 * H, 100.0));
        let out = state.submit_evidence(ev(2, 0.2 * H, 110.0));
        assert_eq!(out.confirmed_local.len(), 1);
        assert_eq!(out.confirmed_local[0].support_videos, 2);
    }

    #[test]
    fn two_videos_in_a_window_stay_pending_when_the_batch_has_three() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        state.submit_evidence(ev(1, 0.1 * H, 100.0));
        state.submit_evidence(ev(2, 0.2 * H, 110.0));
        // Third qualified video far away raises the batch threshold to 3; the
        // two-video window no longer confirms.
        let out = state.submit_evidence(ev(3, 30.0 * H, 5000.0));
        assert!(out.confirmed_local.is_empty());
    }

    // ── 1.8 window-width calibration lower bound ───────────────────────────

    #[test]
    fn sparse_tail_needs_the_six_hour_window() {
        // Feedback 20260724: HZY_4182 at 07:03, HZY_4210 at 08:56, HZY_4221 at
        // 09:02 — the tail pair is 1.87h from its predecessor. At 1.5h width
        // the tail two cannot reach threshold 3; at 6h all three group.
        let times = [0.0, 1.87 * H, 1.97 * H];

        let narrow = local_windows_with_width(&times, 1.5 * H);
        assert!(narrow.iter().all(|w| w.len() < 3), "1.5h window must not group all three");

        let wide = local_windows_with_width(&times, LOCAL_WINDOW_MS);
        assert!(wide.iter().any(|w| w.len() == 3), "6h window must group all three");
    }

    // ── nearest_confirmed selection ────────────────────────────────────────

    #[test]
    fn nearest_confirmed_prefers_distance_then_support_then_generation() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        // Cluster ~20.5h out drifting +1000ms — inside the drift budget
        // (6000×20.5/24+1500 ≈ 6625 ≥ 1005).
        for (t, off) in [(20.0, 1000.0), (21.0, 1010.0), (20.5, 1005.0)] {
            state.submit_evidence(ev((t * 10.0) as u32, t * H, off));
        }
        // Video at 19h: the local post (median 20.5h) is nearer than the anchor.
        let near = state.nearest_confirmed(19.0 * H, 0).unwrap();
        assert_eq!(near.source, ConfirmedOffsetSource::Local);
        assert_eq!(near.wall_clock_offset_ms, 1005.0);
        // Video at 5h: the anchor is nearer — initial values compete on
        // distance like everyone else.
        let anchor = state.nearest_confirmed(5.0 * H, 0).unwrap();
        assert_eq!(anchor.source, ConfirmedOffsetSource::Anchor);
    }

    #[test]
    fn confirmed_values_do_not_cross_sessions() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 7, 0.0, -6265.0);
        assert!(state.nearest_confirmed(0.0, 8).is_none());
        assert!(state.nearest_confirmed(0.0, 7).is_some());
    }

    // ── relay gap warning ──────────────────────────────────────────────────

    #[test]
    fn relay_gap_warns_past_sixty_percent_of_search_size() {
        // Cap rate 6000/day: 12h gap → 3000ms expected = exactly 60% of a
        // 5000ms search_size → not yet warning; 13h → warning.
        assert!(!relay_gap_warning(12.0 * H, 5000.0));
        assert!(relay_gap_warning(13.0 * H, 5000.0));
    }

    // ── relay propagation (tasks 4.10 / 4.11) ──────────────────────────────
    //
    // The convergence criterion is the primary one from the design:
    // `|truth − initial_offset| <= search_size` (half width, batch default
    // 5000ms). Slice overlap is tighter for short clips but scales the same
    // way, so these synthetic runs use the primary criterion.

    const DAY: f64 = 86_400_000.0;
    const DRIFT_PER_DAY: f64 = 2521.0; // measured on feedback 20260724
    const SEARCH_MS: f64 = 5000.0;

    /// Simulate a chronological relay run: every clip takes its initial
    /// offset from the nearest confirmed value, syncs iff the truth is within
    /// `search_size`, and successful syncs feed evidence back. Returns
    /// (clips, static_anchor_failures, relay_failures).
    fn simulate_relay(clip_times_h: &[f64]) -> (usize, usize, usize) {
        let truth = |t_ms: f64| t_ms / DAY * DRIFT_PER_DAY;
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        let (mut static_fail, mut relay_fail) = (0usize, 0usize);
        for (i, t_h) in clip_times_h.iter().enumerate() {
            let t = t_h * H;
            let tv = truth(t);
            if tv.abs() > SEARCH_MS {
                static_fail += 1;
            }
            let init = state.nearest_confirmed(t, 0).unwrap().wall_clock_offset_ms;
            if (tv - init).abs() > SEARCH_MS {
                relay_fail += 1;
            } else {
                state.submit_evidence(ev(i as u32 + 1, t, tv));
            }
        }
        (clip_times_h.len(), static_fail, relay_fail)
    }

    #[test]
    fn relay_covers_a_five_day_batch_a_static_anchor_cannot() {
        // days=5, anchor on the FIRST clip (the worst-case position from the
        // design: static coverage ends ~1.98 days out at 2521 ms/day). 20
        // clips per day, 10 min apart.
        let mut times = Vec::new();
        for day in 0..5 {
            for k in 0..20 {
                times.push(day as f64 * 24.0 + k as f64 / 6.0);
            }
        }
        let (n, static_fail, relay_fail) = simulate_relay(&times);
        assert_eq!(n, 100);
        assert!(
            static_fail >= 50,
            "the static anchor must lose the far days (got {static_fail}/100 failures)"
        );
        assert_eq!(
            relay_fail, 0,
            "stepwise relay must keep every clip within search_size"
        );
    }

    #[test]
    fn relay_frontier_degrades_with_period_gap() {
        // Task 4.11: three period spacings. PRECONDITION the relay depends
        // on: the drift accumulated across ONE period gap must stay below
        // search_size (5000ms ⇔ ~47.6h at 2521 ms/day — measured from the
        // PREVIOUS period's evidence centre, so a nominal 48h spacing still
        // lands ~40ms inside the boundary in this deterministic sim; the
        // design's noisy 48h → 73/80 estimate sits exactly on that edge).
        // 36h gaps are safely inside; 50h/60h are beyond it, the first clips
        // of the next period cannot sync, the period yields no evidence, and
        // the chain stays broken from there on.
        let mk = |gap_h: f64| -> Vec<f64> {
            let mut times = Vec::new();
            for period in 0..4 {
                for k in 0..10 {
                    times.push(period as f64 * gap_h + k as f64 / 6.0);
                }
            }
            times
        };

        let (_, _, fail_36) = simulate_relay(&mk(36.0));
        assert_eq!(fail_36, 0, "36h gaps stay within the relay's reach");

        let (n_48, _, fail_48) = simulate_relay(&mk(48.0));
        assert_eq!((n_48, fail_48), (40, 0), "48h sits just inside the boundary");

        let (_, _, fail_50) = simulate_relay(&mk(50.0));
        assert!(fail_50 > 0, "50h gap drift (~5170ms effective) exceeds search_size");

        let (_, _, fail_60) = simulate_relay(&mk(60.0));
        assert!(fail_60 >= fail_50, "wider gaps can only fail more");

        // The relay_gap warning fires long before the cliff: expected drift
        // over a 48h gap is far beyond 60% of search_size.
        assert!(relay_gap_warning(48.0 * H, SEARCH_MS));
        assert!(!relay_gap_warning(1.0 * H, SEARCH_MS));
    }

    // ── lifecycle ──────────────────────────────────────────────────────────

    #[test]
    fn invalidate_local_keeps_initials_and_drops_the_rest() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, -6265.0);
        state.submit_evidence(ev(1, 0.1 * H, 100.0));
        state.submit_evidence(ev(2, 0.2 * H, 110.0));
        state.invalidate_local();
        assert_eq!(state.evidence_count(), 0);
        assert_eq!(state.confirmed().count(), 1);
        assert_eq!(state.confirmed().next().unwrap().source, ConfirmedOffsetSource::Anchor);
    }

    #[test]
    fn removing_a_job_recomputes_consensus() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        state.submit_evidence(ev(1, 0.1 * H, 100.0));
        state.submit_evidence(ev(2, 0.2 * H, 110.0));
        state.submit_evidence(ev(3, 0.3 * H, 105.0));
        assert_eq!(state.confirmed().filter(|c| c.source == ConfirmedOffsetSource::Local).count(), 1);
        state.remove_job(1);
        state.remove_job(2);
        // Only one video left (< threshold 2… actually clamp(1,2,3)=2 > 1).
        assert_eq!(state.confirmed().filter(|c| c.source == ConfirmedOffsetSource::Local).count(), 0);
    }

    #[test]
    fn clear_resets_everything() {
        let mut state = BatchClockState::new();
        state.register_initial(ConfirmedOffsetSource::Anchor, 0, 0.0, 0.0);
        state.submit_evidence(ev(1, 0.1 * H, 100.0));
        state.clear();
        assert!(state.is_empty());
        assert_eq!(state.confirmed().count(), 0);
    }
}
