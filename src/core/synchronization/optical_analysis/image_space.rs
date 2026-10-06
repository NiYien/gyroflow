// SPDX-License-Identifier: GPL-3.0-or-later

//! Fit translation and uniform expansion of a relative depth layer, before removing parallax.

use nalgebra::{Matrix3, Vector2, Vector3};
use super::translation::{PairPoint, PairTranslation};

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct LayerMotion {
    pub motion: [f32; 2],
    pub scale_rate: f32,
    pub far_beta: f32,
    pub weight: f32,
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
    LayerMotion { motion: [solution.x as f32, solution.y as f32], scale_rate: solution.z as f32,
        far_beta: (quantile(&depths, 0.2) / median) as f32, weight: pair.confidence.clamp(0.0, 1.0) as f32 }
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
                    points.push(PairPoint { id, band: 0, p, r: b - p });
                    depths.insert(id, rho);
                }
            }
        }
        let pair = PairTranslation { c: Vector3::zeros(), c_segment: Vector3::zeros(),
            carried_depth: depths.keys().copied().collect::<HashSet<_>>(), inv_depth: depths,
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
}
