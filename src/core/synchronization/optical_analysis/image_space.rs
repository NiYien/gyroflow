// SPDX-License-Identifier: GPL-3.0-or-later

//! Fit translation and uniform expansion of a relative depth layer, before removing parallax.

use nalgebra::{DMatrix, DVector, Matrix3, Vector2, Vector3};
use super::translation::{PairPoint, PairTranslation};
use crate::gyro_source::PairDepthGrid;
use crate::gyro_source::optical_translation::depth_warp::{CELLS, COLS, ROWS, EMPTY_VISIBILITY};

#[derive(Clone, Debug, Default)]
pub(super) struct LayerMotion {
    pub motion: [f32; 2],
    pub scale_rate: f32,
    pub far_beta: f32,
    /// Numerator and denominator of the dominant layer's depth gain, for the analysis to pool over neighbouring pairs:
    /// the weighted median of the content's parallax projected on the unit-depth layer's motion, and its squared norm
    pub dominant: [f32; 2],
    /// Depth grid over the source image for the depth warp, when asked for
    pub depth: Option<PairDepthGrid>,
    pub weight: f32,
}

/// Scatter of log inverse depth within one grid cell (relief inside it)
const GRID_SCATTER: f64 = 0.15;
/// Scale of the robust smoothness between neighbouring cells, in log inverse depth: steps much larger than this
/// (occlusion edges) are kept instead of being smoothed out
const GRID_EDGE_SCALE: f64 = 0.3;
/// Floor of the prior's spread around the pair's median depth
const GRID_SPREAD_FLOOR: f64 = 0.1;
const GRID_IRLS: usize = 4;

/// Grid cell coordinates of a point of the normalized image plane, clamped to the cell centres' range
fn grid_position(x: &Vector2<f64>, half_extent: [f64; 2]) -> (f64, f64) {
    ((x.x + half_extent[0]) / (2.0 * half_extent[0]) * COLS as f64 - 0.5, (x.y + half_extent[1]) / (2.0 * half_extent[1]) * ROWS as f64 - 0.5)
}

/// Log inverse depth (relative to the pair's median) on a COLS x ROWS grid over the source image, and how reliable
/// each cell is: the points' depths with their own uncertainty, a robust smoothness between neighbouring cells, and a prior
/// at the median layer that a cell without points falls back to. `points` holds each point's plane position, log
/// inverse depth, variance of that log and visibility.
pub(super) fn depth_grid(points: &[(Vector2<f64>, f64, f64, f64)], half_extent: [f64; 2]) -> Option<PairDepthGrid> {
    if points.len() < 12 || !half_extent.iter().all(|h| h.is_finite() && *h > 0.0) { return None; }
    let mut logs: Vec<f64> = points.iter().map(|p| p.1).collect();
    logs.sort_by(f64::total_cmp);
    let centre = logs[logs.len() / 2];
    let mut deviations: Vec<f64> = logs.iter().map(|z| (z - centre).abs()).collect();
    deviations.sort_by(f64::total_cmp);
    let spread = (1.4826 * deviations[deviations.len() / 2]).max(GRID_SPREAD_FLOOR);
    let mut data = DMatrix::<f64>::identity(CELLS, CELLS) / (spread * spread);
    let mut g = DVector::<f64>::zeros(CELLS);
    let (mut visibility, mut counts) = (vec![0.0f64; CELLS], vec![0usize; CELLS]);
    // Precision each cell gets from its own measurements, not through its neighbours
    let mut direct = vec![0.0f64; CELLS];
    for (x, z, variance, vis) in points {
        let (gx, gy) = grid_position(x, half_extent);
        let (gx, gy) = (gx.clamp(0.0, (COLS - 1) as f64), gy.clamp(0.0, (ROWS - 1) as f64));
        let (x0, y0) = ((gx.floor() as usize).min(COLS - 2), (gy.floor() as usize).min(ROWS - 2));
        let (fx, fy) = (gx - x0 as f64, gy - y0 as f64);
        let basis = [(x0 + y0 * COLS, (1.0 - fx) * (1.0 - fy)), (x0 + 1 + y0 * COLS, fx * (1.0 - fy)),
            (x0 + (y0 + 1) * COLS, (1.0 - fx) * fy), (x0 + 1 + (y0 + 1) * COLS, fx * fy)];
        let w = 1.0 / (variance.max(0.0) + GRID_SCATTER * GRID_SCATTER);
        for &(a, wa) in &basis {
            g[a] += w * wa * z;
            direct[a] += w * wa;
            for &(b, wb) in &basis { data[(a, b)] += w * wa * wb; }
        }
        let nearest = gx.round() as usize + gy.round() as usize * COLS;
        visibility[nearest] += vis;
        counts[nearest] += 1;
    }
    let edges: Vec<(usize, usize)> = (0..CELLS).flat_map(|c| [(c % COLS + 1 < COLS).then_some((c, c + 1)), (c / COLS + 1 < ROWS).then_some((c, c + COLS))])
        .flatten().collect();
    let mut edge_weights = vec![1.0; edges.len()];
    let mut z = DVector::<f64>::zeros(CELLS);
    for _ in 0..GRID_IRLS {
        let mut h = data.clone();
        for (&(i, j), w) in edges.iter().zip(&edge_weights) {
            let w = w / (GRID_EDGE_SCALE * GRID_EDGE_SCALE);
            h[(i, i)] += w; h[(j, j)] += w; h[(i, j)] -= w; h[(j, i)] -= w;
        }
        z = h.clone().cholesky()?.solve(&g);
        for (&(i, j), w) in edges.iter().zip(edge_weights.iter_mut()) {
            let r = (z[i] - z[j]) / GRID_EDGE_SCALE;
            *w = 1.0 / (1.0 + r * r);
        }
    }
    if !z.iter().all(|v| v.is_finite()) { return None; }
    // Reliability: the share of the prior's variance the cell's own measurements remove. The smoothness fills a cell
    // without points from its neighbours, but on a 2D grid it would also make far, unmeasured cells look certain.
    let prior_precision = 1.0 / (spread * spread);
    Some(PairDepthGrid {
        z: z.iter().map(|v| *v as f32).collect(),
        r: (0..CELLS).map(|c| (direct[c] / (direct[c] + prior_precision)) as f32).collect(),
        vis: (0..CELLS).map(|c| if counts[c] > 0 { (visibility[c] / counts[c] as f64) as f32 } else { EMPTY_VISIBILITY }).collect(),
    })
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
#[cfg(test)]
pub(super) fn fit(points: &[PairPoint], pair: &PairTranslation, projection: &Matrix3<f64>, size: (usize, usize)) -> LayerMotion {
    fit_with_depth(points, pair, projection, size, None)
}

/// `fit`, plus the depth grid over a source image of normalized half size `grid` when given.
pub(super) fn fit_with_depth(points: &[PairPoint], pair: &PairTranslation, projection: &Matrix3<f64>, size: (usize, usize), grid: Option<[f64; 2]>) -> LayerMotion {
    if pair.new_segment || !pair.confidence.is_finite() || pair.confidence <= 0.0 || size.0 == 0 || size.1 == 0 {
        return LayerMotion::default();
    }
    let mut cells = [0usize; 16 * 9];
    // Texture of each kept point, for the dominant layer, and the variance of its log inverse depth for the grid
    let mut texture = Vec::new();
    let mut log_variance = Vec::new();
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
        texture.push(point.texture);
        log_variance.push(pair.inv_depth_var.get(&point.id).map_or(1e6, |var| (var.max(0.0) / (rho * rho)).min(1e6)));
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
    // Dominant layer: the content that most of the visible, textured picture follows. Content at depth beta moves by
    // beta times the unit layer's motion, so the weighted median parallax projected on that motion gives its depth.
    // A median rather than a mean, so that a minority far in front or behind does not pull it.
    let weights: Vec<_> = data.iter().zip(&texture).map(|(d, e)| visibility(*e, typical) / cells[d.3] as f64).collect();
    let median_of = |axis: usize| quantile(&data.iter().zip(&weights).map(|(d, w)| (d.1[axis], *w)).collect::<Vec<_>>(), 0.5);
    let parallax = Vector2::new(median_of(0), median_of(1));
    let unit = solution.xy();
    let depth = grid.and_then(|half_extent| {
        let entries: Vec<_> = data.iter().zip(&texture).zip(&log_variance)
            .map(|(((x, _, rho, _), e), variance)| (*x, (rho / median).ln(), *variance, visibility(*e, typical))).collect();
        depth_grid(&entries, half_extent)
    });
    LayerMotion { motion: [solution.x as f32, solution.y as f32], scale_rate: solution.z as f32,
        far_beta: (quantile(&depths, 0.2) / median) as f32,
        dominant: [parallax.dot(&unit) as f32, unit.norm_squared() as f32],
        depth,
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

    /// One point per cell, the right `near_columns` of 16 at inverse depth 2.0 with texture `near_texture`, the rest
    /// at 0.5 with texture 1.0 (or every cell at `single` when given), the camera moving sideways.
    fn layered(near_columns: usize, near_texture: f32, single: Option<f64>) -> (Vec<PairPoint>, PairTranslation, Matrix3<f64>) {
        let (mut points, mut pair, projection) = scene(Vector3::zeros(), false);
        let motion = Vector2::new(0.002, 0.001);
        for point in &mut points {
            let x = point.p.x / -point.p.z;
            let y = -point.p.y / -point.p.z;
            let column = ((x * 500.0 + 400.0) / 50.0) as usize;
            let near = column >= 16 - near_columns;
            let rho = single.unwrap_or(if near { 2.0 } else { 0.5 });
            let moved = Vector2::new(x, y) + motion * rho;
            point.r = Vector3::new(moved.x, -moved.y, -1.0).normalize() - point.p;
            point.texture = if near && single.is_none() { near_texture } else { 1.0 };
            pair.inv_depth.insert(point.id, rho);
        }
        (points, pair, projection)
    }

    /// The dominant layer's inverse depth in the scene's own units, from a single pair (far_beta is 0.5 there)
    fn dominant_depth(points: &[PairPoint], pair: &PairTranslation, projection: &Matrix3<f64>) -> f64 {
        let got = fit(points, pair, projection, (800, 450));
        assert!(got.dominant[1] > 0.0, "{got:?}");
        (got.dominant[0] / got.dominant[1]) as f64 / got.far_beta as f64 * 0.5
    }

    #[test]
    fn translation_dominant_layer_of_a_single_depth() {
        let (points, pair, projection) = layered(0, 1.0, Some(1.3));
        let got = fit(&points, &pair, &projection, (800, 450));
        let ratio = (got.dominant[0] / got.dominant[1]) as f64;
        assert!((ratio - 1.0).abs() < 0.02, "{got:?}");
    }

    #[test]
    fn translation_dominant_layer_follows_what_covers_most_of_the_picture() {
        // Far on about 70% of the picture: a near minority must not pull it
        let (points, pair, projection) = layered(5, 1.0, None);
        let far = dominant_depth(&points, &pair, &projection);
        assert!((far - 0.5).abs() / 0.5 < 0.05, "{far}");
        // Near on about 70%: the subject that fills the frame wins
        let (points, pair, projection) = layered(11, 1.0, None);
        let near = dominant_depth(&points, &pair, &projection);
        assert!((near - 2.0).abs() / 2.0 < 0.15, "{near}");
    }

    #[test]
    fn translation_dominant_layer_counts_what_shows_motion() {
        assert_eq!(visibility(f32::NAN, 2.0), 1.0);
        assert_eq!(visibility(1.0, 0.0), 1.0);
        assert!((visibility(2.0, 2.0) - 0.5).abs() < 1e-12 && visibility(20.0, 2.0) > 0.9 && visibility(0.1, 2.0) < 0.05);
        // A flat dark wall on half the picture, near; textured far content on the rest
        let (points, pair, projection) = layered(8, 0.05, None);
        let depth = dominant_depth(&points, &pair, &projection);
        assert!((depth - 0.5).abs() / 0.5 < 0.15, "{depth}");
    }

    #[test]
    fn translation_dominant_layer_is_empty_without_a_fit() {
        let (points, mut pair, projection) = layered(5, 1.0, None);
        pair.confidence = 0.0;
        assert_eq!(fit(&points, &pair, &projection, (800, 450)).dominant, [0.0, 0.0]);
    }

    /// Four points per grid cell over a 1.6 x 0.9 plane, at log inverse depth `z(column)`, for the columns `columns`
    fn grid_points(z: impl Fn(usize) -> f64, columns: std::ops::Range<usize>) -> Vec<(Vector2<f64>, f64, f64, f64)> {
        let mut points = Vec::new();
        for row in 0..ROWS {
            for column in columns.clone() {
                for (dx, dy) in [(0.25, 0.25), (0.75, 0.25), (0.25, 0.75), (0.75, 0.75)] {
                    let x = Vector2::new(-0.8 + (column as f64 + dx) * 1.6 / COLS as f64, -0.45 + (row as f64 + dy) * 0.9 / ROWS as f64);
                    points.push((x, z(column), 1e-4, 1.0));
                }
            }
        }
        points
    }

    #[test]
    fn translation_depth_grid_follows_layers_and_keeps_their_edge() {
        let flat = depth_grid(&grid_points(|_| 0.0, 0..COLS), [0.8, 0.45]).unwrap();
        assert!(flat.z.iter().all(|z| z.abs() < 0.05), "{:?}", flat.z);
        // An occlusion edge: inverse depth four times larger on the right half
        let step = depth_grid(&grid_points(|c| if c < COLS / 2 { -0.69 } else { 0.69 }, 0..COLS), [0.8, 0.45]).unwrap();
        for c in 0..CELLS {
            let column = c % COLS;
            if column <= COLS / 2 - 2 { assert!((step.z[c] + 0.69).abs() < 0.1, "{column}: {}", step.z[c]); }
            if column >= COLS / 2 + 1 { assert!((step.z[c] - 0.69).abs() < 0.1, "{column}: {}", step.z[c]); }
            assert!(step.r[c] > 0.9, "{}", step.r[c]);
        }
        // Nothing measured on the right: those cells fall back to the prior and are known to be unknown
        let half = depth_grid(&grid_points(|c| if c < 3 { -0.5 } else { 0.5 }, 0..6), [0.8, 0.45]).unwrap();
        for c in (0..CELLS).filter(|c| c % COLS >= 12) {
            assert!(half.r[c] < 0.2, "{}", half.r[c]);
            assert!((half.vis[c] - EMPTY_VISIBILITY).abs() < 1e-6);
        }
        assert!(depth_grid(&grid_points(|_| 0.0, 0..1)[..11], [0.8, 0.45]).is_none());
    }
}
