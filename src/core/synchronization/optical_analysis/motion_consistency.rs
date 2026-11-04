// SPDX-License-Identifier: GPL-3.0-or-later
//! Local motion checks on the existing tracked points, without an image model or another tracking pass.

use super::{
    Derived, MIN_BAND_POINTS, Pair,
    translation::{PairPoint, TranslationSolver, TranslationSolverConfig},
};
use crate::stabilization::ComputeParams;
use nalgebra::{Matrix3, Vector2, Vector3};
use opencv::{
    core::{Mat, Point2f, Vector},
    prelude::*,
};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

const ROBUST_PX: f64 = super::odometry::ROBUST_PX;

struct Point {
    id: u32,
    row_ms: f64,
    xy: Vector2<f64>,
    rotation: Matrix3<f64>,
    reference: bool,
}

struct CameraFrame {
    seq: usize,
    index: usize,
    time_ms: f64,
    segment: u32,
    center: Vector3<f64>,
    trusted: bool,
    points: Vec<Point>,
}

struct Observation {
    frame: usize,
    id: u32,
    xy: Vector2<f64>,
    center: Vector3<f64>,
    rotation: Matrix3<f64>,
    ray: Vector3<f64>,
    reference: bool,
}

#[derive(Clone, Copy)]
struct Score {
    frame: usize,
    segment: u32,
    window: [usize; 4],
    original: f64,
    guarded: Option<f64>,
    reference: bool,
}

fn quantile(values: &mut [f64], fraction: f64) -> f64 {
    if values.is_empty() {
        return f64::INFINITY;
    }
    let x = (values.len() - 1) as f64 * fraction;
    let low = x.floor() as usize;
    let high = x.ceil() as usize;
    let (lower, pivot, _) = values.select_nth_unstable_by(high, f64::total_cmp);
    let upper = *pivot;
    if upper.is_nan() {
        return f64::INFINITY;
    }
    if low == high || !upper.is_finite() {
        return upper;
    }
    let lower = lower.iter().copied().max_by(f64::total_cmp).unwrap();
    if lower.is_nan() {
        return f64::INFINITY;
    }
    lower + (upper - lower) * (x - low as f64)
}

fn error(point: &Observation, direction: Vector3<f64>, focal: f64) -> f64 {
    let v = point.rotation.transpose() * direction;
    if v.z >= -1e-10 {
        return f64::INFINITY;
    }
    (Vector2::new(-v.x / v.z, v.y / v.z) - point.xy).norm() * focal
}

fn fit(train: &[Observation]) -> (Vector3<f64>, Option<Vector3<f64>>) {
    let mut lhs = Matrix3::zeros();
    let mut rhs = Vector3::zeros();
    let mut direction = Vector3::zeros();
    for p in train {
        let projection = Matrix3::identity() - p.ray * p.ray.transpose();
        lhs += projection;
        rhs += projection * p.center;
        direction += p.ray;
    }
    (direction.normalize(), lhs.lu().solve(&rhs))
}

fn predict(train: &[Observation], test: &[Observation], focal: f64) -> f64 {
    let (direction, point) = fit(train);
    let mut values: Vec<_> = test.iter().map(|p| error(p, direction, focal)).collect();
    let mut best = quantile(&mut values, 0.5);
    if let Some(point) = point.filter(|point| {
        train
            .iter()
            .chain(test)
            .all(|p| (p.rotation.transpose() * (*point - p.center)).z < -1e-10)
    }) {
        let mut values: Vec<_> = test
            .iter()
            .map(|p| error(p, point - p.center, focal))
            .collect();
        best = best.min(quantile(&mut values, 0.5));
    }
    best
}

fn joint_static_score(a: &[Observation], b: &[Observation], focal: f64) -> f64 {
    let mut lhs = Matrix3::zeros();
    let mut rhs = Vector3::zeros();
    let mut direction = Vector3::zeros();
    for p in a.iter().chain(b) {
        let projection = Matrix3::identity() - p.ray * p.ray.transpose();
        lhs += projection;
        rhs += projection * p.center;
        direction += p.ray;
    }
    direction = direction.normalize();
    let infinity = |part: &[Observation]| {
        quantile(
            &mut part
                .iter()
                .map(|p| error(p, direction, focal))
                .collect::<Vec<_>>(),
            0.5,
        )
    };
    let mut best = infinity(a).min(infinity(b));
    if let Some(mut point) = lhs.lu().solve(&rhs) {
        for iteration in 0..=3 {
            if !a
                .iter()
                .chain(b)
                .all(|p| (p.rotation.transpose() * (point - p.center)).z < -1e-10)
            {
                break;
            }
            let score = |part: &[Observation]| {
                quantile(
                    &mut part
                        .iter()
                        .map(|p| error(p, point - p.center, focal))
                        .collect::<Vec<_>>(),
                    0.5,
                )
            };
            best = best.min(score(a).min(score(b)));
            if iteration == 3 {
                break;
            }
            let mut normal = Matrix3::zeros();
            let mut gradient = Vector3::zeros();
            for p in a.iter().chain(b) {
                let r = p.rotation.transpose();
                let v = r * (point - p.center);
                let e = (Vector2::new(-v.x / v.z, v.y / v.z) - p.xy) * focal;
                let weight = 1.0 / (1.0 + e.norm_squared() / ROBUST_PX.powi(2));
                let jx = (-r.row(0).transpose() * v.z + r.row(2).transpose() * v.x)
                    * (focal / v.z.powi(2));
                let jy = (r.row(1).transpose() * v.z - r.row(2).transpose() * v.y)
                    * (focal / v.z.powi(2));
                normal += (jx * jx.transpose() + jy * jy.transpose()) * weight;
                gradient += (jx * e.x + jy * e.y) * weight;
            }
            let Some(step) = normal.lu().solve(&gradient) else {
                break;
            };
            point -= step;
        }
    }
    best
}

#[derive(Default)]
struct Support {
    ids: HashSet<u32>,
    before: Vec<f64>,
    after: Vec<f64>,
    before_ids: usize,
    after_ids: usize,
    before_frames: HashSet<usize>,
    after_frames: HashSet<usize>,
}

impl Support {
    fn add(&mut self, id: u32, window: &[Score], center: usize, guarded: bool) {
        self.ids.insert(id);
        self.before_ids += usize::from(window.iter().any(|p| p.frame <= center));
        self.after_ids += usize::from(window.iter().any(|p| p.frame >= center));
        for p in window {
            let value = if guarded {
                p.guarded.unwrap()
            } else {
                p.original
            };
            if p.frame <= center {
                self.before.push(value);
                self.before_frames.insert(p.frame);
            }
            if p.frame >= center {
                self.after.push(value);
                self.after_frames.insert(p.frame);
            }
        }
    }
    fn accepted(mut self, cut: f64) -> HashSet<u32> {
        if self.before.len() < MIN_BAND_POINTS
            || self.after.len() < MIN_BAND_POINTS
            || self.before_ids < 5
            || self.after_ids < 5
            || self.before_frames.len() < 2
            || self.after_frames.len() < 2
            || quantile(&mut self.before, 0.5) <= cut
            || quantile(&mut self.after, 0.5) <= cut
        {
            HashSet::new()
        } else {
            self.ids
        }
    }
}

fn supported(window: &[Score], center: usize, cut: f64, guarded: bool) -> bool {
    if window.len() < 3 {
        return false;
    }
    let a = window.partition_point(|p| p.frame <= center);
    let b = window.partition_point(|p| p.frame < center);
    let (first, last) = if a < 2 || window.len() - b < 2 {
        window.split_at(window.len() / 2)
    } else {
        (&window[..a], &window[b..])
    };
    if first.len() < 2 || last.len() < 2 {
        return false;
    }
    let value = |p: &Score| {
        if guarded {
            p.guarded.unwrap()
        } else {
            p.original
        }
    };
    quantile(&mut first.iter().map(value).collect::<Vec<_>>(), 0.5) > cut
        && quantile(&mut last.iter().map(value).collect::<Vec<_>>(), 0.5) > cut
}

pub(super) struct MotionConsistency {
    solver: TranslationSolver,
    next_seq: usize,
    last_index: Option<usize>,
    background: HashSet<u32>,
    previous: HashSet<u32>,
    segment: u32,
    center: Vector3<f64>,
    correction: Matrix3<f64>,
    frames: VecDeque<CameraFrame>,
}

impl MotionConsistency {
    pub fn new() -> Self {
        Self {
            solver: TranslationSolver::new(TranslationSolverConfig::resolved()),
            next_seq: 0,
            last_index: None,
            background: HashSet::new(),
            previous: HashSet::new(),
            segment: 0,
            center: Vector3::zeros(),
            correction: Matrix3::identity(),
            frames: VecDeque::new(),
        }
    }

    pub fn filter(
        &mut self,
        params: &ComputeParams,
        pairs: &VecDeque<Pair>,
        derived: &[Derived],
        focal: f64,
        from_seq: usize,
        cancel: &dyn Fn() -> bool,
    ) -> Option<HashSet<(usize, u32)>> {
        if cancel() {
            return None;
        }
        let started = std::time::Instant::now();
        let mut by_pair: HashMap<usize, Vec<&Derived>> = HashMap::new();
        for point in derived {
            if point.seq >= self.next_seq {
                by_pair.entry(point.seq).or_default().push(point);
            }
        }
        let gyro = params.gyro.read();
        for pair in pairs.iter().filter(|p| p.seq >= self.next_seq) {
            if cancel() {
                return None;
            }
            if self.last_index.is_some_and(|i| i != pair.a.index) {
                self.solver.reset();
                self.background.clear();
                self.previous.clear();
                self.segment += 1;
                self.center = Vector3::zeros();
                self.correction = Matrix3::identity();
            }
            let points = by_pair.remove(&pair.seq).unwrap_or_default();
            let common: Vec<_> = points
                .iter()
                .filter(|p| self.previous.contains(&p.id))
                .copied()
                .collect();
            let mut train: Vec<_> = common
                .iter()
                .filter(|p| self.background.contains(&p.id))
                .copied()
                .collect();
            if train.len() < MIN_BAND_POINTS {
                train = common.clone();
            }
            let mut rejected = HashSet::new();
            if train.len() >= MIN_BAND_POINTS {
                let plane = |p: Vector3<f64>| Vector2::new(-p.x / p.z * focal, p.y / p.z * focal);
                let as_float = |p: Vector2<f64>| Point2f::new(p.x as f32, p.y as f32);
                let before =
                    Vector::<Point2f>::from_iter(train.iter().map(|p| as_float(plane(p.p))));
                let after =
                    Vector::<Point2f>::from_iter(train.iter().map(|p| as_float(plane(p.p + p.r))));
                let mut inliers = Mat::default();
                let _ = opencv::core::set_rng_seed(0);
                if let Ok(model) = opencv::calib3d::estimate_affine_partial_2d(
                    &before,
                    &after,
                    &mut inliers,
                    opencv::calib3d::RANSAC,
                    ROBUST_PX,
                    2000,
                    0.99,
                    10,
                ) {
                    if let (Ok(values), Ok(inside)) =
                        (model.data_typed::<f64>(), inliers.data_typed::<u8>())
                    {
                        let count = inside.iter().filter(|x| **x != 0).count();
                        if values.len() == 6 && count >= MIN_BAND_POINTS && count * 2 >= train.len()
                        {
                            self.background.clear();
                            for p in common {
                                let a = plane(p.p);
                                let b = plane(p.p + p.r);
                                let dx = values[0] * a.x + values[1] * a.y + values[2] - b.x;
                                let dy = values[3] * a.x + values[4] * a.y + values[5] - b.y;
                                if dx.hypot(dy) <= ROBUST_PX {
                                    self.background.insert(p.id);
                                } else {
                                    rejected.insert(p.id);
                                }
                            }
                        }
                    }
                }
            }
            let seed: Vec<_> = points
                .iter()
                .filter(|p| !rejected.contains(&p.id))
                .map(|p| PairPoint {
                    id: p.id,
                    band: p.band,
                    p: p.p,
                    r: p.r,
                    texture: p.texture,
                })
                .collect();
            let result = self.solver.step(&seed, 1.0 / focal, pair.b.mid_ms / 1000.0);
            if result.new_segment {
                self.segment += 1;
                self.center = Vector3::zeros();
                self.correction = Matrix3::identity();
            }
            let trusted = result.confidence > 0.0;
            if trusted {
                let body = gyro
                    .org_quat_at_timestamp(pair.b.mid_ms)
                    .to_rotation_matrix()
                    .into_inner();
                self.center += self.correction * body * result.c_segment;
                self.correction = self.correction
                    * body
                    * self.solver.last_rotation().transpose()
                    * body.transpose();
            }
            let observations = if trusted {
                points
                    .iter()
                    .map(|p| {
                        let ray = p.p + p.r;
                        Point {
                            id: p.id,
                            row_ms: p.tb_ms,
                            xy: Vector2::new(-ray.x / ray.z, ray.y / ray.z),
                            rotation: self.correction
                                * gyro
                                    .org_quat_at_timestamp(p.tb_ms)
                                    .to_rotation_matrix()
                                    .into_inner(),
                            reference: !rejected.contains(&p.id),
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            self.frames.push_back(CameraFrame {
                seq: pair.seq,
                index: pair.b.index,
                time_ms: pair.b.mid_ms,
                segment: self.segment,
                center: self.center,
                trusted,
                points: observations,
            });
            self.previous = points.iter().map(|p| p.id).collect();
            self.last_index = Some(pair.b.index);
        }
        drop(gyro);
        let seed_time = started.elapsed();
        if let Some(pair) = pairs.back() {
            self.next_seq = pair.seq + 1;
        }
        let radius = (params.scaled_fps * 0.25).round_ties_even().max(1.0) as usize;
        let mut centers: BTreeMap<u32, Vec<&CameraFrame>> = BTreeMap::new();
        for frame in self.frames.iter().filter(|f| f.trusted) {
            centers.entry(frame.segment).or_default().push(frame);
        }
        let mut tracks: BTreeMap<(u32, u32), Vec<Observation>> = BTreeMap::new();
        for frame in self.frames.iter().filter(|f| f.trusted) {
            if cancel() {
                return None;
            }
            let poses = &centers[&frame.segment];
            for p in &frame.points {
                let right = poses.partition_point(|pose| pose.time_ms <= p.row_ms);
                let center = if right == 0 {
                    poses[0].center
                } else if right == poses.len() {
                    poses[right - 1].center
                } else {
                    let a = poses[right - 1];
                    let b = poses[right];
                    let t = (p.row_ms - a.time_ms) / (b.time_ms - a.time_ms);
                    a.center * (1.0 - t) + b.center * t
                };
                let ray = (p.rotation * Vector3::new(p.xy.x, -p.xy.y, -1.0)).normalize();
                tracks
                    .entry((frame.segment, p.id))
                    .or_default()
                    .push(Observation {
                        frame: frame.index,
                        id: p.id,
                        xy: p.xy,
                        center,
                        rotation: p.rotation,
                        ray,
                        reference: p.reference,
                    });
            }
        }
        let mut scores: BTreeMap<u32, Vec<Score>> = BTreeMap::new();
        for (&(segment, _), track) in &tracks {
            if cancel() {
                return None;
            }
            if track.len() < 4 {
                continue;
            }
            for p in track {
                let mut left = track.partition_point(|q| q.frame < p.frame.saturating_sub(radius));
                let mut a_end = track.partition_point(|q| q.frame < p.frame);
                let mut b_start = track.partition_point(|q| q.frame <= p.frame);
                let mut right = track.partition_point(|q| q.frame <= p.frame + radius);
                if a_end - left < 2 || right - b_start < 2 {
                    let begin = p
                        .frame
                        .saturating_sub(radius)
                        .min(track.last().unwrap().frame.saturating_sub(2 * radius))
                        .max(track[0].frame);
                    left = track.partition_point(|q| q.frame < begin);
                    right = track.partition_point(|q| q.frame <= begin + 2 * radius);
                    a_end = left + (right - left) / 2;
                    b_start = a_end;
                }
                if a_end - left < 2 || right - b_start < 2 {
                    continue;
                }
                let a = &track[left..a_end];
                let b = &track[b_start..right];
                let original = predict(a, b, focal).min(predict(b, a, focal));
                scores.entry(p.id).or_default().push(Score {
                    frame: p.frame,
                    segment,
                    window: [left, a_end, b_start, right],
                    original,
                    guarded: None,
                    reference: p.reference,
                });
            }
        }
        for track in scores.values_mut() {
            track.sort_by_key(|p| p.frame);
        }
        let geometry_time = started.elapsed() - seed_time;
        let mut rejected = HashSet::new();
        if let (Some(first), Some(last)) = (self.frames.front(), self.frames.back()) {
            for frame in self.frames.iter().filter(|f| f.seq >= from_seq) {
                if cancel() {
                    return None;
                }
                let begin = frame
                    .index
                    .saturating_sub(radius)
                    .min(last.index.saturating_sub(2 * radius))
                    .max(first.index);
                let end = begin + 2 * radius;
                let center = begin + radius;
                let mut reference: Vec<_> = scores
                    .values()
                    .flatten()
                    .filter(|p| p.reference && p.frame >= begin && p.frame <= end)
                    .map(|p| p.original)
                    .collect();
                let cut = quantile(&mut reference, 0.95).max(ROBUST_PX);
                if !cut.is_finite() {
                    continue;
                }
                let mut original = Support::default();
                for (&id, track) in &scores {
                    let left = track.partition_point(|p| p.frame < begin);
                    let right = track.partition_point(|p| p.frame <= end);
                    let window = &track[left..right];
                    if supported(window, center, cut, false) {
                        original.add(id, window, center, false);
                    }
                }
                let original = original.accepted(cut);
                if original.is_empty() {
                    continue;
                }
                let mut guarded = Support::default();
                // A joint static explanation can only retain points. Fit it lazily for already-supported rejections.
                for id in original {
                    let track = scores.get_mut(&id).unwrap();
                    let left = track.partition_point(|p| p.frame < begin);
                    let right = track.partition_point(|p| p.frame <= end);
                    for score in &mut track[left..right] {
                        if score.guarded.is_none() {
                            let observations = &tracks[&(score.segment, id)];
                            let [a, b, c, d] = score.window;
                            score.guarded = Some(score.original.min(joint_static_score(
                                &observations[a..b],
                                &observations[c..d],
                                focal,
                            )));
                        }
                    }
                    let window = &track[left..right];
                    if supported(window, center, cut, true) {
                        guarded.add(id, window, center, true);
                    }
                }
                rejected.extend(
                    guarded
                        .accepted(cut)
                        .into_iter()
                        .map(|id| (frame.index, id)),
                );
            }
        }
        // Only the recent raw observations are needed by the next analysis batch.
        if let Some(last) = self.frames.back() {
            let cutoff = last.index.saturating_sub(4 * radius);
            while self.frames.front().is_some_and(|f| f.index < cutoff) {
                self.frames.pop_front();
            }
        }
        log::info!(target:"sync","Optical motion consistency: rejected={} cached_frames={} elapsed_ms={:.1} seed_ms={:.1} geometry_ms={:.1} selection_ms={:.1}",
            rejected.len(),self.frames.len(),started.elapsed().as_secs_f64()*1000.0,seed_time.as_secs_f64()*1000.0,geometry_time.as_secs_f64()*1000.0,
            (started.elapsed()-seed_time-geometry_time).as_secs_f64()*1000.0);
        Some(rejected)
    }
}

#[cfg(test)]
fn run_component_checks() {
    for mode in ["static", "stop-start", "occlusion"] {
        let manager = crate::StabilizationManager::default();
        manager.init_from_video_data(4000.0, 30.0, 120, (1920, 1080));
        manager.gyro.write().quaternions = (0..=4000)
            .map(|ms| (ms * 1000, crate::Quat64::identity()))
            .collect();
        let params = ComputeParams::from_manager(&manager);
        let observe = |frame: usize| -> HashMap<u32, Vector3<f64>> {
            let center = Vector3::new(frame as f64 * 0.002, 0.0, 0.0);
            let mut points = HashMap::new();
            for id in 0..320u32 {
                let world = Vector3::new(
                    (id % 16) as f64 * 0.15 - 1.125,
                    (id / 16) as f64 * 0.07 - 0.665,
                    -(2.5 + (id % 3) as f64 * 1.5),
                );
                points.insert(id, (world - center).normalize());
            }
            if mode != "static" && !(mode == "occlusion" && (45..60).contains(&frame)) {
                let end = if mode == "occlusion" { 90 } else { 60 };
                let shift = frame.saturating_sub(30).min(end - 30) as f64 * 0.015;
                for id in 400..500u32 {
                    let world = Vector3::new(
                        (id % 10) as f64 * 0.012 + 0.15,
                        ((id - 400) / 10) as f64 * 0.012 - 0.45 + shift,
                        -2.5,
                    );
                    let track = if mode == "occlusion" && frame >= 60 {
                        id + 1000
                    } else {
                        id
                    };
                    points.insert(track, (world - center).normalize());
                }
            }
            points
        };
        let frame = |index| super::Frame {
            index,
            timestamp_ms: index as f64 * 1000.0 / 30.0,
            start_ms: index as f64 * 1000.0 / 30.0,
            per_px_ms: 0.0,
            mid_ms: index as f64 * 1000.0 / 30.0,
        };
        let mut filter = MotionConsistency::new();
        let mut previous = observe(0);
        let mut pairs = VecDeque::new();
        let mut derived = Vec::new();
        let mut rejected = HashSet::new();
        let mut start = 0;
        for index in 1..120 {
            let current = observe(index);
            let mut ids: Vec<_> = current.keys().copied().collect();
            ids.sort_unstable();
            for id in ids {
                if let Some(a) = previous.get(&id) {
                    derived.push(Derived {
                        id,
                        seq: index - 1,
                        band: 0,
                        p: *a,
                        r: current[&id] - a,
                        ta_ms: (index - 1) as f64 * 1000.0 / 30.0,
                        tb_ms: index as f64 * 1000.0 / 30.0,
                        texture: 1.0,
                    });
                }
            }
            pairs.push_back(Pair {
                seq: index - 1,
                a: frame(index - 1),
                b: frame(index),
                obs: Vec::new(),
            });
            previous = current;
            if index % 40 == 0 || index == 119 {
                rejected.extend(
                    filter
                        .filter(&params, &pairs, &derived, 500.0, start, &|| false)
                        .unwrap(),
                );
                assert!(
                    filter.frames.len() <= 33,
                    "rolling cache grew beyond its time window"
                );
                pairs.clear();
                derived.clear();
                start = index;
            }
        }
        assert!(
            rejected.iter().all(|(_, id)| *id >= 400),
            "{mode}: rejected a known static point"
        );
        if mode == "static" {
            assert!(rejected.is_empty());
        }
        if mode == "stop-start" {
            assert!(
                rejected.iter().all(|(f, _)| *f >= 20 && *f <= 80),
                "rejected a stopped interval"
            );
            assert!(
                (50..56).all(|f| (400..500).all(|id| rejected.contains(&(f, id)))),
                "missed supported moving tracks"
            );
        }
        if mode == "occlusion" {
            assert!(
                (70..76).all(|f| (1400..1500).all(|id| rejected.contains(&(f, id)))),
                "new identities did not acquire motion evidence"
            );
            assert!(
                rejected.iter().all(|(f, _)| *f >= 20 && *f <= 110),
                "motion leaked into stopped interval"
            );
        }
        assert!(
            filter
                .filter(&params, &VecDeque::new(), &[], 500.0, 120, &|| true)
                .is_none()
        );
        println!(
            "motion consistency {mode}: passed ({} rejected pairs)",
            rejected.len()
        );
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn static_depth_motion_intervals_and_bounded_cache() {
        super::run_component_checks();
    }
}
