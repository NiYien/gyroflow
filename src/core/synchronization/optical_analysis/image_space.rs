// SPDX-License-Identifier: GPL-3.0-or-later

//! Fit translation and uniform expansion of a relative depth layer, before removing parallax.

use nalgebra::{Matrix3, Vector2, Vector3};
use super::translation::{PairPoint, PairTranslation};

#[derive(Clone, Debug, Default)]
pub(super) struct LayerMotion {
    pub motion: [f32; 2],
    pub scale_rate: f32,
    pub far_beta: f32,
    /// Relative inverse depth of the layer the automatic parameters hold steady, from this pair alone
    pub auto_beta: f32,
    /// The scores behind `auto_beta`, for the analysis to pool over neighbouring pairs before it chooses
    pub scores: Option<LayerScores>,
    pub weight: f32,
}

/// Half width, in log inverse depth, of what counts as one layer for the automatic reference (about +-28% in depth).
const AUTO_LAYER_LOG: f64 = 0.25;

/// The reference depths scored, in log inverse depth relative to the pair's median: from 1/55 to 6.9 times it.
const AUTO_GRID_START: f64 = -4.0;
const AUTO_GRID_STEP: f64 = AUTO_LAYER_LOG / 4.0;
const AUTO_GRID_LEN: usize = 96;

/// Beyond this many standard deviations a depth is taken as surely in front of or behind a layer edge.
const AUTO_SURE_SD: f64 = 6.0;

/// Standard normal cumulative distribution (Abramowitz and Stegun 7.1.26, absolute error below 1.5e-7).
fn normal_cdf(x: f64) -> f64 {
    let z = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + 0.3275911 * z);
    let erf = 1.0 - ((((1.061405429 * t - 1.453152027) * t + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * (-z * z).exp();
    if x >= 0.0 { 0.5 * (1.0 + erf) } else { 0.5 * (1.0 - erf) }
}

fn grid_depth(index: usize) -> f64 { AUTO_GRID_START + index as f64 * AUTO_GRID_STEP }

/// The automatic reference's scores at each depth of the grid, normalized by the total weight, and the depths the
/// measured content spans. Pairs close in time add up: the layer is then chosen from what a stretch of video shows,
/// instead of flipping between two layers whose scores are close and averaging the flips into a depth that is neither.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct LayerScores {
    /// Each piece scores 2 on the layer, 1 in front of it and 0 behind it; stored minus the constant 1
    score: Vec<f32>,
    /// Weight of the content on the layer, and the same weighted by where it is expected to lie (log depth)
    mass: Vec<f32>,
    position: Vec<f32>,
    low: usize,
    high: usize,
}

impl LayerScores {
    /// `depths` holds (inverse depth relative to the median, weight, standard deviation of its log). A depth the
    /// parallax has not pinned down yet (near the direction of travel, or early in a segment) counts with the
    /// probability of each outcome, so it spreads over the layers instead of making up a far one.
    pub(super) fn new(depths: &[(f64, f64, f64)]) -> Option<Self> {
        let values: Vec<_> = depths.iter()
            .filter(|(rho, weight, sd)| rho.is_finite() && *rho > 0.0 && weight.is_finite() && *weight > 0.0 && !sd.is_nan())
            .map(|(rho, weight, sd)| (rho.ln(), *weight, sd.max(1e-9))).collect();
        let total: f64 = values.iter().map(|v| v.1).sum();
        if values.is_empty() || total <= 0.0 { return None; }
        // Search the depths the well measured content spans; a wildly uncertain one must not stretch the search.
        let mut by_depth: Vec<_> = values.iter().map(|v| (v.0, v.1 / (1.0 + (v.2 / AUTO_LAYER_LOG).powi(2)))).collect();
        by_depth.sort_by(|a, b| a.0.total_cmp(&b.0));
        let measured: f64 = by_depth.iter().map(|v| v.1).sum();
        let quantile = |fraction: f64| {
            let mut sum = 0.0;
            by_depth.iter().find(|v| { sum += v.1; sum >= fraction * measured }).map_or(by_depth[by_depth.len() - 1].0, |v| v.0)
        };
        let index = |x: f64| ((x - AUTO_GRID_START) / AUTO_GRID_STEP).clamp(0.0, (AUTO_GRID_LEN - 1) as f64);
        let low = index(quantile(0.01) - AUTO_LAYER_LOG).floor() as usize;
        let high = index(quantile(0.99) + AUTO_LAYER_LOG).ceil() as usize;
        let (mut score, mut mass, mut position) = (vec![0.0f64; AUTO_GRID_LEN], vec![0.0f64; AUTO_GRID_LEN], vec![0.0f64; AUTO_GRID_LEN]);
        // From its last uncertain grid depth on, a depth lies surely behind the layer: added once as a step
        let mut behind_from = vec![0.0f64; AUTO_GRID_LEN + 1];
        let pdf = |x: f64| (-0.5 * x * x).exp() / (2.0 * std::f64::consts::PI).sqrt();
        for &(value, weight, sd) in &values {
            let first = index(value - AUTO_LAYER_LOG - AUTO_SURE_SD * sd).floor() as usize;
            let past = (((value + AUTO_LAYER_LOG + AUTO_SURE_SD * sd - AUTO_GRID_START) / AUTO_GRID_STEP).ceil().max(0.0) as usize).min(AUTO_GRID_LEN);
            for g in first..past {
                let b = grid_depth(g);
                let (lower, upper) = ((b - AUTO_LAYER_LOG - value) / sd, (b + AUTO_LAYER_LOG - value) / sd);
                let behind = normal_cdf(lower);
                let same = normal_cdf(upper) - behind;
                score[g] += weight * (same - behind);
                if same > 1e-12 {
                    let expected = (value + sd * (pdf(lower) - pdf(upper)) / same).clamp(b - AUTO_LAYER_LOG, b + AUTO_LAYER_LOG);
                    mass[g] += weight * same;
                    position[g] += weight * same * expected;
                }
            }
            behind_from[past] += weight;
        }
        let mut behind = 0.0;
        for g in 0..AUTO_GRID_LEN {
            behind += behind_from[g];
            score[g] -= behind;
        }
        let normalized = |v: Vec<f64>| v.into_iter().map(|x| (x / total) as f32).collect();
        Some(Self { score: normalized(score), mass: normalized(mass), position: normalized(position), low, high: high.max(low) })
    }

    /// Adds `other` weighted by `k`, over both ranges.
    pub(super) fn add(&mut self, other: &Self, k: f64) {
        if self.score.is_empty() {
            *self = Self { score: vec![0.0; AUTO_GRID_LEN], mass: vec![0.0; AUTO_GRID_LEN], position: vec![0.0; AUTO_GRID_LEN], low: other.low, high: other.high };
        }
        for g in 0..AUTO_GRID_LEN {
            self.score[g] += (k * other.score[g] as f64) as f32;
            self.mass[g] += (k * other.mass[g] as f64) as f32;
            self.position[g] += (k * other.position[g] as f64) as f32;
        }
        self.low = self.low.min(other.low);
        self.high = self.high.max(other.high);
    }

    /// The best scoring depth, the farther one on a tie, refined to where its content lies on average. Relative to
    /// the median inverse depth; zero without scores.
    pub(super) fn choose(&self) -> f64 {
        if self.score.is_empty() { return 0.0; }
        let mut best = self.low;
        for g in self.low..=self.high.min(AUTO_GRID_LEN - 1) {
            if self.score[g] > self.score[best] + 1e-7 { best = g; }
        }
        (if self.mass[best] > 0.0 { (self.position[best] / self.mass[best]) as f64 } else { grid_depth(best) }).exp()
    }
}

/// Inverse depth, relative to the median, of the layer a viewer reads as standing still. Each visible piece scores 2
/// when it lies on the layer and is held still, 1 when it lies in front of it and slides as parallax, and 0 when it
/// lies behind it and moves. Open scenes pick the far layer; a near layer wins once it covers most of the picture.
fn auto_reference(depths: &[(f64, f64, f64)]) -> f64 {
    LayerScores::new(depths).map_or(0.0, |scores| scores.choose())
}

/// How much of a point's motion a viewer can see: its texture against the picture's typical tracked texture. Flat or
/// dark areas still get corners but barely show that they move. Unknown texture counts fully.
fn visibility(texture: f32, typical: f64) -> f64 {
    let e = texture as f64;
    if !e.is_finite() || e < 0.0 || !(typical > 0.0) { 1.0 } else { e / (e + typical) }
}

fn quantile(values: &[(f64, f64)], fraction: f64) -> f64 {
    let mut values = values.to_vec();
    values.sort_by(|a, b| a.0.total_cmp(&b.0));
    let target = values.iter().map(|v| v.1).sum::<f64>() * fraction;
    let mut sum = 0.0;
    for &(value, weight) in &values {
        sum += weight;
        if sum >= target { return value; }
    }
    values.last().map_or(0.0, |v| v.0)
}

fn plane(ray: Vector3<f64>) -> Option<Vector2<f64>> {
    (ray.iter().all(|v| v.is_finite()) && ray.z < -1e-9)
        .then(|| Vector2::new(-ray.x / ray.z, ray.y / ray.z))
}

/// `projection` maps upright source camera rays to the analysis output, including its crop.
pub(super) fn fit(points: &[PairPoint], pair: &PairTranslation, projection: &Matrix3<f64>, size: (usize, usize)) -> LayerMotion {
    if pair.new_segment || !pair.confidence.is_finite() || pair.confidence <= 0.0 || size.0 == 0 || size.1 == 0 {
        return LayerMotion::default();
    }
    let mut cells = [0usize; 16 * 9];
    // Standard deviation of each kept point's log inverse depth and its texture, for the automatic reference
    let mut log_sd = Vec::new();
    let mut texture = Vec::new();
    let data: Vec<_> = points.iter().filter_map(|point| {
        if !pair.carried_depth.contains(&point.id) { return None; }
        let rho = *pair.inv_depth.get(&point.id)?;
        if !rho.is_finite() || rho <= 0.0 { return None; }
        let x = plane(point.p)?;
        let delta = plane(point.p + point.r)? - x;
        let output = projection * Vector3::new(point.p.x, -point.p.y, -point.p.z);
        if !output.iter().all(|v| v.is_finite()) || output.z <= 1e-9 { return None; }
        let (u, v) = (output.x / output.z / size.0 as f64, output.y / output.z / size.1 as f64);
        if !(0.0..1.0).contains(&u) || !(0.0..1.0).contains(&v) { return None; }
        let cell = (u * 16.0) as usize + 16 * (v * 9.0) as usize;
        cells[cell] += 1;
        log_sd.push(pair.inv_depth_var.get(&point.id).map_or(f64::INFINITY, |var| var.max(0.0).sqrt() / rho));
        texture.push(point.texture);
        Some((x, delta, rho, cell))
    }).collect();
    if data.len() < 12 { return LayerMotion::default(); }
    let depths: Vec<_> = data.iter().map(|d| (d.2, 1.0 / cells[d.3] as f64)).collect();
    let median = quantile(&depths, 0.5);
    if median <= 0.0 { return LayerMotion::default(); }
    let mut robust = vec![[1.0; 2]; data.len()];
    let mut solution = Vector3::zeros();
    for _ in 0..6 {
        let mut h = Matrix3::zeros();
        let mut g = Vector3::zeros();
        for (i, (x, delta, rho, cell)) in data.iter().enumerate() {
            let beta = rho / median;
            for axis in 0..2 {
                let mut row = Vector3::zeros();
                row[axis] = beta;
                row.z = beta * x[axis];
                let weight = robust[i][axis] / cells[*cell] as f64;
                h += row * row.transpose() * weight;
                g += row * (delta[axis] * weight);
            }
        }
        let Some(cholesky) = h.cholesky() else { return LayerMotion::default(); };
        solution = cholesky.solve(&g);
        if !solution.iter().all(|v| v.is_finite()) { return LayerMotion::default(); }
        let residuals: Vec<_> = data.iter().flat_map(|(x, delta, rho, cell)| {
            let residual = delta - (solution.xy() + x * solution.z) * (rho / median);
            [0, 1].map(|axis| (residual[axis], 1.0 / cells[*cell] as f64))
        }).collect();
        let centre = quantile(&residuals, 0.5);
        let deviations: Vec<_> = residuals.iter().map(|(v, w)| ((v - centre).abs(), *w)).collect();
        let scale = (2.5 * 1.4826 * quantile(&deviations, 0.5)).max(1e-9);
        for (i, weights) in robust.iter_mut().enumerate() {
            for axis in 0..2 { weights[axis] = 1.0 / (1.0 + (residuals[2 * i + axis].0 / scale).powi(2)); }
        }
    }
    let mut textures: Vec<_> = texture.iter().map(|e| *e as f64).filter(|e| e.is_finite() && *e >= 0.0).collect();
    textures.sort_by(f64::total_cmp);
    let typical = textures.get(textures.len() / 2).copied().unwrap_or(0.0);
    let auto_depths: Vec<_> = depths.iter().zip(&log_sd).zip(&texture)
        .map(|((d, sd), e)| (d.0 / median, d.1 * visibility(*e, typical), *sd)).collect();
    let scores = LayerScores::new(&auto_depths);
    LayerMotion { motion: [solution.x as f32, solution.y as f32], scale_rate: solution.z as f32,
        far_beta: (quantile(&depths, 0.2) / median) as f32,
        auto_beta: scores.as_ref().map_or(0.0, |scores| scores.choose()) as f32, scores,
        weight: pair.confidence.clamp(0.0, 1.0) as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    fn scene(motion: Vector3<f64>, dense_near: bool) -> (Vec<PairPoint>, PairTranslation, Matrix3<f64>) {
        let mut points = Vec::new();
        let mut depths = HashMap::new();
        for y in 0..9 {
            for x in 0..16 {
                let rho = if x < 8 { 0.5 } else { 2.0 };
                let repeats = if dense_near && x >= 8 { 5 } else { 1 };
                for j in 0..repeats {
                    let xy = Vector2::new(((x as f64 + 0.4 + j as f64 * 0.03) * 50.0 - 400.0) / 500.0,
                        ((y as f64 + 0.5) * 50.0 - 225.0) / 500.0);
                    let moved = xy + (motion.xy() + xy * motion.z) * (rho / 0.5);
                    let p = Vector3::new(xy.x, -xy.y, -1.0).normalize();
                    let b = Vector3::new(moved.x, -moved.y, -1.0).normalize();
                    let id = points.len() as u32;
                    points.push(PairPoint { id, band: 0, p, r: b - p, texture: f32::NAN });
                    depths.insert(id, rho);
                }
            }
        }
        let pair = PairTranslation { c: Vector3::zeros(), c_segment: Vector3::zeros(),
            carried_depth: depths.keys().copied().collect::<HashSet<_>>(), inv_depth_var: depths.keys().map(|id| (*id, 1e-6)).collect(), inv_depth: depths,
            ref_inv_depth: 0.0, confidence: 0.75, track_age_s: 1.0, new_segment: false, pred: Some(0.1) };
        (points, pair, Matrix3::new(500.0, 0.0, 400.0, 0.0, 500.0, 225.0, 0.0, 0.0, 1.0))
    }

    #[test]
    fn translation_image_space_lateral_motion() {
        let truth = Vector3::new(0.002, -0.001, 0.0);
        let (mut points, pair, projection) = scene(truth, false);
        // Sparse mismatches exercise the robust fit without dominating any cell.
        for point in points.iter_mut().step_by(19) { point.r.x += 0.01; }
        let got = fit(&points, &pair, &projection, (800, 450));
        assert!((Vector2::from(got.motion.map(f64::from)) - truth.xy()).norm() / truth.xy().norm() < 0.03, "{got:?}");
        assert!(got.scale_rate.abs() < 1e-5, "{got:?}");
        assert_eq!(got.weight, 0.75);
    }

    #[test]
    fn translation_image_space_forward_expansion() {
        let (points, pair, projection) = scene(Vector3::new(0.0, 0.0, 0.003), false);
        let got = fit(&points, &pair, &projection, (800, 450));
        assert!((got.scale_rate as f64 / 0.003 - 1.0).abs() < 0.03, "{got:?}");
        assert!(got.motion.iter().all(|v| v.abs() < 1e-7));
    }

    #[test]
    fn translation_image_space_equal_area_depth_and_visibility() {
        let (mut points, mut pair, projection) = scene(Vector3::new(0.002, 0.0, 0.0), true);
        let got = fit(&points, &pair, &projection, (800, 450));
        assert!((got.far_beta - 1.0).abs() < 0.05, "{got:?}");
        let count_depths: Vec<_> = pair.inv_depth.values().map(|rho| (*rho / 0.5, 1.0)).collect();
        assert!(quantile(&count_depths, 0.2) - got.far_beta as f64 > 2.0);
        for point in &mut points { point.p = Vector3::new(20.0, 0.0, -1.0).normalize(); }
        assert_eq!(fit(&points, &pair, &projection, (800, 450)).weight, 0.0);
        let (points, _, _) = scene(Vector3::zeros(), false);
        assert_eq!(fit(&points[..11], &pair, &projection, (800, 450)).weight, 0.0);
        pair.new_segment = true;
        assert_eq!(fit(&points, &pair, &projection, (800, 450)).weight, 0.0);
        pair.new_segment = false;
        pair.confidence = 0.0;
        assert_eq!(fit(&points, &pair, &projection, (800, 450)).weight, 0.0);
    }

    /// Visible area split by column: the left `far_columns` of 16 at inverse depth 0.5, the rest at 2.0.
    fn split_depths(far_columns: usize) -> Vec<(f64, f64, f64)> {
        (0..16 * 9).map(|cell| (if cell % 16 < far_columns { 0.5 } else { 2.0 }, 1.0, 0.0)).collect()
    }

    #[test]
    fn translation_auto_reference_holds_the_far_layer_until_a_near_one_dominates() {
        // Equal areas, and a near layer on 60% of the picture: the far layer stays the reference
        assert!((auto_reference(&split_depths(8)) - 0.5).abs() < 0.2, "{}", auto_reference(&split_depths(8)));
        assert!((auto_reference(&split_depths(7)) - 0.5).abs() < 0.2);
        // A near layer on 75% of the picture takes over
        assert!((auto_reference(&split_depths(4)) - 2.0).abs() < 0.6, "{}", auto_reference(&split_depths(4)));
        // A continuous spread of depths (a ground plane) keeps the far end
        let spread: Vec<_> = (0..100).map(|i| ((0.01 * i as f64 * 3.0_f64.ln()).exp() * 0.4, 1.0, 0.0)).collect();
        assert!(auto_reference(&spread) < 0.4 * 1.35, "{}", auto_reference(&spread));
        assert_eq!(auto_reference(&[]), 0.0);
        assert_eq!(auto_reference(&[(f64::NAN, 1.0, 0.0), (-1.0, 1.0, 0.0), (1.0, 0.0, 0.0), (1.0, 1.0, f64::NAN)]), 0.0);
    }

    #[test]
    fn translation_auto_reference_counts_what_shows_motion() {
        assert_eq!(visibility(f32::NAN, 2.0), 1.0);
        assert_eq!(visibility(1.0, 0.0), 1.0);
        assert!((visibility(2.0, 2.0) - 0.5).abs() < 1e-12 && visibility(20.0, 2.0) > 0.9 && visibility(0.1, 2.0) < 0.05);
        // A flat dark wall on 70% of the picture, textured far content on the rest
        let mut depths: Vec<_> = (0..70).map(|_| (2.0, 1.0 * visibility(0.05, 1.0), 0.05)).collect();
        depths.extend((0..30).map(|_| (0.5, 1.0 * visibility(4.0, 1.0), 0.05)));
        assert!((auto_reference(&depths) - 0.5).abs() < 0.1, "{}", auto_reference(&depths));
        // Textured like the rest, the same wall dominates
        for d in &mut depths[..70] { d.1 = visibility(4.0, 1.0); }
        assert!((auto_reference(&depths) - 2.0).abs() < 0.3, "{}", auto_reference(&depths));
    }

    #[test]
    fn translation_auto_scores_add_up_over_pairs() {
        let near = |share: usize| {
            let mut depths: Vec<_> = (0..share).map(|_| (2.0, 1.0, 0.05)).collect();
            depths.extend((share..100).map(|_| (0.5, 1.0, 0.05)));
            LayerScores::new(&depths).unwrap()
        };
        // Near on 68% of the picture wins, on 64% it doesn't; together the far layer does
        let (a, b) = (near(68), near(64));
        assert!((a.choose() - 2.0).abs() < 0.3 && (b.choose() - 0.5).abs() < 0.1);
        let mut pooled = LayerScores::default();
        pooled.add(&a, 1.0);
        pooled.add(&b, 1.0);
        assert!((pooled.choose() - 0.5).abs() < 0.1, "{}", pooled.choose());
        assert_eq!(LayerScores::default().choose(), 0.0);
        assert!(LayerScores::new(&[]).is_none());
    }

    #[test]
    fn translation_auto_reference_does_not_take_unmeasured_depths_for_a_far_layer() {
        // A third of the picture still near its starting guess of almost no inverse depth, the rest measured
        let mut depths: Vec<_> = (0..48).map(|_| (0.02, 1.0, 3.0)).collect();
        depths.extend((0..48).map(|_| (0.5, 1.0, 0.05)));
        depths.extend((0..48).map(|_| (2.0, 1.0, 0.05)));
        assert!((auto_reference(&depths) - 0.5).abs() < 0.1, "{}", auto_reference(&depths));
        // Measured just as well, the same third is a real far layer
        for d in &mut depths[..48] { d.2 = 0.05; }
        assert!(auto_reference(&depths) < 0.03, "{}", auto_reference(&depths));
    }

    #[test]
    fn translation_auto_reference_reaches_the_samples_relative_to_the_median() {
        let (points, pair, projection) = scene(Vector3::new(0.002, 0.0, 0.0), false);
        let got = fit(&points, &pair, &projection, (800, 450));
        // Equal-area far and near halves: both references are the far layer, in units of the median depth
        assert!(got.auto_beta > 0.0 && (got.auto_beta - got.far_beta).abs() < 0.05, "{got:?}");
    }
}
