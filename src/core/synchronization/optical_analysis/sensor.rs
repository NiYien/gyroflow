// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw two-endpoint measurements for in-camera stabilization reconstruction.

use std::sync::Arc;
use nalgebra::{Matrix2, Matrix2x3, Matrix3, UnitQuaternion, Vector2, Vector3};
use crate::stabilization::{SensorEndpoint, SensorProjection};
use super::{MIN_BAND_POINTS, SIGMA_FLOOR_PX};

pub(crate) struct SensorPoint {
    pub(crate) a: SensorEndpoint,
    pub(crate) b: SensorEndpoint,
    pub(crate) ta_us: f64,
    pub(crate) tb_us: f64,
    pub(crate) gyro_ab: UnitQuaternion<f64>,
    pub(crate) band: u8,
}

pub(crate) struct SensorPair {
    pub(crate) seq: usize,
    pub(crate) frame_a: Arc<SensorProjection>,
    pub(crate) frame_b: Arc<SensorProjection>,
    pub(crate) duration_us: f64,
    pub(crate) points: Vec<SensorPoint>,
}

pub(crate) struct SensorBand {
    pub(crate) pair: usize,
    pub(crate) band: u8,
    pub(crate) ta_us: f64,
    pub(crate) tb_us: f64,
    /// Finite relative SE(2) initialization, not an exact difference of absolute sensor parameters.
    pub(crate) correction: Vector3<f64>,
    pub(crate) info: Matrix3<f64>,
    /// Fixed across all cutoff candidates; measured in target tracking pixels.
    pub(crate) cauchy_scale_px: f64,
}

pub(crate) fn predicted_sensor(pair: &SensorPair, point: &SensorPoint, source: Vector2<f64>) -> Option<Vector2<f64>> {
    pair.frame_b.ray_to_sensor(point.gyro_ab * pair.frame_a.sensor_to_ray(source)?)
}

pub(crate) fn residual(pair: &SensorPair, point: &SensorPoint, sa: Vector3<f64>, sb: Vector3<f64>) -> Option<Vector2<f64>> {
    let predicted = predicted_sensor(pair, point, pair.frame_a.add_correction(&point.a, sa))?;
    let target = pair.frame_b.add_correction(&point.b, sb);
    pair.frame_b.sensor_to_ray(target)?;
    let error = (target - predicted).component_mul(&pair.frame_b.full_to_track);
    error.iter().all(|v| v.is_finite()).then_some(error)
}

pub(crate) fn residual_and_jacobians(pair: &SensorPair, point: &SensorPoint, sa: Vector3<f64>, sb: Vector3<f64>) -> Option<(Vector2<f64>, Matrix2x3<f64>, Matrix2x3<f64>)> {
    let error = residual(pair, point, sa, sb)?;
    let source = pair.frame_a.add_correction(&point.a, sa);
    let mut derivative = Matrix2::zeros();
    for axis in 0..2 {
        let step = 0.25 / pair.frame_a.full_to_track[axis];
        let mut d = Vector2::zeros(); d[axis] = step;
        let column = (predicted_sensor(pair, point, source + d)? - predicted_sensor(pair, point, source - d)?) / (2.0 * step);
        derivative.set_column(axis, &column);
    }
    let scale = Matrix2::from_diagonal(&pair.frame_b.full_to_track);
    let ja = -scale * derivative * pair.frame_a.correction_jacobian(&point.a, sa);
    let jb = scale * pair.frame_b.correction_jacobian(&point.b, sb);
    (ja.iter().chain(jb.iter()).all(|v| v.is_finite())).then_some((error, ja, jb))
}

pub(crate) fn fit_band_shift(pair: &SensorPair, indices: &[usize]) -> Option<SensorBand> {
    fit_band_shift_with_weights(pair, indices).map(|v| v.0)
}

/// The collection path uses these same weights to average video times before converting their offsets.
pub(super) fn fit_band_shift_with_weights(pair: &SensorPair, indices: &[usize]) -> Option<(SensorBand, Vec<f64>)> {
    if indices.len() < MIN_BAND_POINTS { return None; }
    let scale = Matrix2::from_diagonal(&pair.frame_b.full_to_track);
    let predicted: Vec<_> = indices.iter().map(|&i| {
        let point = pair.points.get(i)?;
        predicted_sensor(pair, point, pair.frame_a.add_correction(&point.a, Vector3::zeros()))
    }).collect::<Option<_>>()?;
    let mut weights = vec![1.0; indices.len()];
    let mut correction = Vector3::zeros();
    let mut errors = vec![0.0; indices.len()];
    let mut h = Matrix3::zeros();
    let mut cauchy_scale_px = 2.5 * SIGMA_FLOOR_PX;
    for _ in 0..5 {
        h.fill(0.0);
        let mut gradient = Vector3::zeros();
        for (k, &i) in indices.iter().enumerate() {
            let point = &pair.points[i].b;
            let e = scale * (pair.frame_b.add_correction(point, correction) - predicted[k]);
            let j = scale * pair.frame_b.correction_jacobian(point, correction);
            h += j.transpose() * j * weights[k];
            gradient += j.transpose() * e * weights[k];
        }
        correction -= h.try_inverse()? * gradient;
        if !correction.iter().all(|v| v.is_finite()) { return None; }
        for (k, &i) in indices.iter().enumerate() {
            errors[k] = (scale * (pair.frame_b.add_correction(&pair.points[i].b, correction) - predicted[k])).norm();
        }
        let mut sorted = errors.clone(); sorted.sort_by(f64::total_cmp);
        cauchy_scale_px = 2.5 * (1.4826 * sorted[sorted.len() / 2]).max(SIGMA_FLOOR_PX);
        for k in 0..indices.len() { weights[k] = 1.0 / (1.0 + (errors[k] / cauchy_scale_px).powi(2)); }
    }
    let sw: f64 = weights.iter().sum();
    if sw < MIN_BAND_POINTS as f64 * 0.5 { return None; }
    let variance = errors.iter().zip(&weights).map(|(e, w)| w * e * e).sum::<f64>() / sw / 2.0;
    let floor = SIGMA_FLOOR_PX / (pair.frame_b.focal_full.x * pair.frame_b.full_to_track.x).max(1.0);
    let covariance = h.try_inverse()? * variance + Matrix3::identity() * floor * floor;
    let info = covariance.try_inverse()?;
    if !info.iter().all(|v| v.is_finite()) { return None; }
    let ta_us = indices.iter().zip(&weights).map(|(&i, w)| pair.points[i].ta_us * w).sum::<f64>() / sw;
    let tb_us = indices.iter().zip(&weights).map(|(&i, w)| pair.points[i].tb_us * w).sum::<f64>() / sw;
    Some((SensorBand { pair: pair.seq, band: pair.points[indices[0]].band, ta_us, tb_us, correction, info, cauchy_scale_px }, weights))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn pinhole() -> Arc<SensorProjection> {
        Arc::new(SensorProjection::test_pinhole(500.0, 500.0, Vector2::repeat(1.0)))
    }

    /// Independently project world points and undo the sensor transform; no production residual is used.
    pub(crate) fn physical_pair(seq: usize, ta_us: f64, tb_us: f64, qa: UnitQuaternion<f64>, qb: UnitQuaternion<f64>, sa: Vector3<f64>, sb: Vector3<f64>) -> SensorPair {
        let projection = pinhole();
        let observe = |q: UnitQuaternion<f64>, s: Vector3<f64>, world: Vector3<f64>| {
            let p = q.inverse() * world;
            let body = Vector2::new(500.0 * p.x / -p.z, -500.0 * p.y / -p.z);
            let z = nalgebra::Rotation2::new(-s.z) * (body - 500.0 * Vector2::new(s.x, s.y)) + Vector2::new(480.0, 270.0);
            SensorEndpoint { sensor_full: [z.x as f32, z.y as f32], known_translation_full: [0.0; 2] }
        };
        let points = (0..240).map(|i| {
            let x = (i % 20) as f64 * 40.0 + 100.0;
            let y = (i / 20) as f64 * 40.0 + 50.0;
            let world = Vector3::new((x - 480.0) / 100.0, (270.0 - y) / 100.0, -5.0);
            SensorPoint { a: observe(qa, sa, world), b: observe(qb, sb, world), ta_us, tb_us, gyro_ab: qb.inverse() * qa, band: (i / 40) as u8 }
        }).collect();
        SensorPair { seq, frame_a: projection.clone(), frame_b: projection, duration_us: tb_us - ta_us, points }
    }

    #[test]
    fn finite_sensor_fit_matches_exact_relative_transform() {
        for step in [Vector3::new(0.004, 0.0, 0.0), Vector3::new(0.0, 0.004, 0.0), Vector3::new(0.0, 0.0, 0.004), Vector3::new(0.004, -0.002, 0.005)] {
            let sa = Vector3::new(0.01, -0.006, 3.0f64.to_radians());
            let pair = physical_pair(0, 0.0, 33333.0, UnitQuaternion::identity(), UnitQuaternion::identity(), sa, sa + step);
            let fit = fit_band_shift(&pair, &(0..pair.points.len()).collect::<Vec<_>>()).unwrap();
            let t = nalgebra::Rotation2::new(-sa.z) * Vector2::new(step.x, step.y);
            let expected = Vector3::new(t.x, t.y, step.z);
            for axis in 0..3 { assert!((fit.correction[axis] - expected[axis]).abs() <= 0.02 * expected[axis].abs() + 1e-6, "axis={axis} got={} expected={}", fit.correction[axis], expected[axis]); }
        }
    }

    #[test]
    fn double_ended_sensor_geometry_closes_with_nonzero_body_and_source() {
        let sa = Vector3::new(0.01, -0.006, 3.0f64.to_radians());
        let sb = sa + Vector3::new(2.0 / 500.0, -1.0 / 500.0, 0.005);
        let pair = physical_pair(0, 0.0, 33333.0, UnitQuaternion::from_euler_angles(0.04, -0.02, 0.03), UnitQuaternion::from_euler_angles(0.05, -0.025, 0.04), sa, sb);
        for point in &pair.points { assert!(residual(&pair, point, sa, sb).unwrap().norm() < 0.1); }
    }

    #[test]
    fn sensor_jacobians_match_independent_parameter_differences() {
        let sa = Vector3::new(0.01, -0.006, 0.04);
        let sb = Vector3::new(0.014, -0.008, 0.045);
        let pair = physical_pair(0, 0.0, 33333.0, UnitQuaternion::identity(), UnitQuaternion::from_euler_angles(0.03, -0.02, 0.01), sa, sb);
        for p in pair.points.iter().step_by(19) {
            let (_, ja, jb) = residual_and_jacobians(&pair, p, sa, sb).unwrap();
            for axis in 0..3 {
                let mut d = Vector3::zeros(); d[axis] = 1e-3;
                let a = (residual(&pair, p, sa + d, sb).unwrap() - residual(&pair, p, sa - d, sb).unwrap()) / 0.002;
                let b = (residual(&pair, p, sa, sb + d).unwrap() - residual(&pair, p, sa, sb - d).unwrap()) / 0.002;
                assert!((ja.column(axis) - a).norm() < 0.05);
                assert!((jb.column(axis) - b).norm() < 0.05);
            }
        }
    }
}
