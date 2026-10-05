// SPDX-License-Identifier: GPL-3.0-or-later
// Physical equations adapted from the existing distortion models by Adrian Eddy and Vladimir Pinchuk.
//! Continuous versions of the physical lens equations for the sensor optimizer only.
//! Coefficients are the renderer's existing f32 kernel values promoted to f64.

use super::{DistortionModel, DistortionModels, KernelParams};
use nalgebra::{Matrix2, Vector2};

fn finite(p: Vector2<f64>) -> Option<Vector2<f64>> {
    p.iter().all(|v| v.is_finite()).then_some(p)
}

fn polynomial(coeffs: &[f64], x: f64) -> f64 {
    coeffs.iter().rev().fold(0.0, |v, c| v * x + c)
}

fn roots_in_interval(coeffs: &[f64], end: f64) -> Vec<f64> {
    let degree = coeffs.iter().rposition(|v| *v != 0.0).unwrap_or(0);
    if degree == 0 {
        return vec![];
    }
    if degree == 1 {
        let root = -coeffs[0] / coeffs[1];
        return if root > 0.0 && root < end {
            vec![root]
        } else {
            vec![]
        };
    }
    let derivative: Vec<_> = (1..=degree).map(|i| coeffs[i] * i as f64).collect();
    let mut boundaries = vec![0.0];
    boundaries.extend(roots_in_interval(&derivative, end));
    boundaries.push(end);
    let mut roots = Vec::new();
    for interval in boundaries.windows(2) {
        let (mut lo, mut hi) = (interval[0], interval[1]);
        let left = polynomial(coeffs, lo);
        let right = polynomial(coeffs, hi);
        if lo > 0.0 && left == 0.0 {
            roots.push(lo);
        }
        if left.signum() == right.signum() || left == 0.0 || right == 0.0 {
            continue;
        }
        for _ in 0..64 {
            let mid = (lo + hi) / 2.0;
            if mid == lo || mid == hi {
                break;
            }
            let value = polynomial(coeffs, mid);
            if value == 0.0 {
                lo = mid;
                hi = mid;
                break;
            }
            if value.signum() == left.signum() {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        roots.push((lo + hi) / 2.0);
    }
    roots
}

// Check every extremum, not just the endpoint: a polynomial can rise again beyond its first fold.
fn positive_through(coeffs: &[f64], end: f64) -> bool {
    if !end.is_finite() || coeffs.iter().any(|v| !v.is_finite()) {
        return false;
    }
    if polynomial(coeffs, 0.0) <= 0.0 || polynomial(coeffs, end) <= 0.0 {
        return false;
    }
    // A coefficient bound proves the common small-distortion case without finding any roots.
    let mut lower = coeffs[0];
    let mut power = 1.0;
    for &coefficient in &coeffs[1..] {
        power *= end;
        lower += coefficient.min(0.0) * power;
    }
    if lower > 0.0 && lower.is_finite() {
        return true;
    }
    let derivative: Vec<_> = (1..coeffs.len()).map(|i| coeffs[i] * i as f64).collect();
    roots_in_interval(&derivative, end)
        .into_iter()
        .all(|x| polynomial(coeffs, x) > 0.0)
}

fn poly_add(a: &[f64], b: &[f64], scale: f64) -> Vec<f64> {
    let mut out = vec![0.0; a.len().max(b.len())];
    for (o, v) in out.iter_mut().zip(a) {
        *o += v;
    }
    for (o, v) in out.iter_mut().zip(b) {
        *o += v * scale;
    }
    out
}
fn poly_mul(a: &[f64], b: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; a.len() + b.len() - 1];
    for (i, x) in a.iter().enumerate() {
        for (j, y) in b.iter().enumerate() {
            out[i + j] += x * y;
        }
    }
    out
}

fn standard_branch(k: &[f64; 24], p: Vector2<f64>) -> bool {
    if k[..12].iter().all(|v| *v == 0.0) {
        return true;
    }
    let r2 = p.norm_squared();
    let r4 = r2 * r2;
    let r6 = r4 * r2;
    // Along lambda*p, multiply each Jacobian entry by denominator squared; its determinant is polynomial.
    let n = [1.0, 0.0, k[0] * r2, 0.0, k[1] * r4, 0.0, k[4] * r6];
    let d = [1.0, 0.0, k[5] * r2, 0.0, k[6] * r4, 0.0, k[7] * r6];
    if !positive_through(&n, 1.0) || !positive_through(&d, 1.0) {
        return false;
    }
    let nd = [k[0], 0.0, 2.0 * k[1] * r2, 0.0, 3.0 * k[4] * r4];
    let dd = [k[5], 0.0, 2.0 * k[6] * r2, 0.0, 3.0 * k[7] * r4];
    let radial = poly_add(&poly_mul(&nd, &d), &poly_mul(&n, &dd), -1.0);
    let base = poly_mul(&n, &d);
    let den2 = poly_mul(&d, &d);
    let (x, y) = (p.x, p.y);
    let tangential = [
        [
            0.0,
            2.0 * k[2] * y + 6.0 * k[3] * x + 2.0 * k[8] * x,
            0.0,
            4.0 * k[9] * x * r2,
        ],
        [
            0.0,
            2.0 * k[2] * x + 2.0 * k[3] * y + 2.0 * k[8] * y,
            0.0,
            4.0 * k[9] * y * r2,
        ],
        [
            0.0,
            2.0 * k[2] * x + 2.0 * k[3] * y + 2.0 * k[10] * x,
            0.0,
            4.0 * k[11] * x * r2,
        ],
        [
            0.0,
            6.0 * k[2] * y + 2.0 * k[3] * x + 2.0 * k[10] * y,
            0.0,
            4.0 * k[11] * y * r2,
        ],
    ];
    let entries: Vec<_> = (0..4)
        .map(|i| {
            let scale = 2.0 * p[i / 2] * p[i % 2];
            let r = poly_mul(&[0.0, 0.0, scale], &radial);
            let r = if i == 0 || i == 3 {
                poly_add(&r, &base, 1.0)
            } else {
                r
            };
            poly_add(&r, &poly_mul(&tangential[i], &den2), 1.0)
        })
        .collect();
    positive_through(
        &poly_add(
            &poly_mul(&entries[0], &entries[3]),
            &poly_mul(&entries[1], &entries[2]),
            -1.0,
        ),
        1.0,
    )
}

fn radial(p: Vector2<f64>, radius: f64, derivative: f64) -> Option<(Vector2<f64>, Matrix2<f64>)> {
    let r = p.norm();
    if !radius.is_finite() || !derivative.is_finite() || derivative <= 0.0 || radius < 0.0 {
        return None;
    }
    if r == 0.0 {
        return Some((p, Matrix2::identity() * derivative));
    }
    let scale = radius / r;
    Some((
        p * scale,
        Matrix2::identity() * scale + p * p.transpose() * ((derivative - scale) / (r * r)),
    ))
}

fn sony_segments(k: &[f64; 24]) -> usize {
    if (1.0..=10.0).contains(&k[0]) && k[1] > 0.0 && k[2] == 0.0 {
        k[0] as usize
    } else {
        0
    }
}

fn sony_segment(k: &[f64; 24], i: usize) -> (f64, f64, f64, f64) {
    let h = k[1];
    let (y, next, c, next_c) = (
        k[2 + i],
        k[3 + i],
        if i == 0 { 0.0 } else { k[13 + i] },
        k[14 + i],
    );
    (
        y,
        (next - y) / h - h * (next_c + 2.0 * c) / 3.0,
        c,
        (next_c - c) / (3.0 * h),
    )
}

// Preserve the renderer's published constants as well as its recorded coefficients.
const SONY_THETA0: f64 = super::sony::THETA0 as f32 as f64;
const SONY_TMAX: f64 = 1.5533f32 as f64;

fn sony_spline_angle(k: &[f64; 24], n: usize, r: f64, domain_start: f64) -> Option<(f64, f64)> {
    if !r.is_finite() || r < domain_start {
        return None;
    }
    let h = k[1];
    for i in 0..n {
        let lo = (domain_start - i as f64 * h).max(0.0);
        let hi = (r - i as f64 * h).min(h);
        if hi < lo {
            continue;
        }
        let (_, b, c, d) = sony_segment(k, i);
        let slope = |x: f64| b + (2.0 * c + 3.0 * d * x) * x;
        if slope(lo) <= 1e-9 || slope(hi) <= 1e-9 {
            return None;
        }
        if d != 0.0 {
            let vertex = -c / (3.0 * d);
            if vertex > lo && vertex < hi && slope(vertex) <= 1e-9 {
                return None;
            }
        }
    }
    let i = ((r / h).floor() as usize).min(n - 1);
    let (y, b, c, d) = sony_segment(k, i);
    let dx = (r - i as f64 * h).clamp(0.0, h);
    let slope = b + (2.0 * c + 3.0 * d * dx) * dx;
    let theta = y + (b + (c + d * dx) * dx) * dx + slope * (r - i as f64 * h - dx);
    (theta.is_finite() && slope.is_finite() && slope > 1e-9 && theta >= 0.0)
        .then_some((theta, slope))
}

// This is the recorded outer spline, with no linear-region override.
fn sony_spline_radius(k: &[f64; 24], n: usize, theta: f64) -> Option<(f64, f64)> {
    let h = k[1];
    let mut i = 0;
    while i + 1 < n && theta >= k[3 + i] {
        i += 1;
    }
    let (y, b, c, d) = sony_segment(k, i);
    let next = k[3 + i];
    if theta >= next {
        let slope = b + (2.0 * c + 3.0 * d * h) * h;
        if slope <= 1e-9 {
            return None;
        }
        return Some((n as f64 * h + (theta - next) / slope, 1.0 / slope));
    }
    let (mut lo, mut hi) = (0.0, h);
    let mut dx = h * (theta - y) / (next - y).max(1e-12);
    for _ in 0..60 {
        let value = y + (b + (c + d * dx) * dx) * dx - theta;
        let slope = b + (2.0 * c + 3.0 * d * dx) * dx;
        if !slope.is_finite() || slope <= 1e-9 {
            return None;
        }
        if value.abs() <= 1e-14 {
            return Some((i as f64 * h + dx, 1.0 / slope));
        }
        if value > 0.0 {
            hi = dx;
        } else {
            lo = dx;
        }
        let next_dx = dx - value / slope;
        dx = if next_dx > lo && next_dx < hi {
            next_dx
        } else {
            (lo + hi) / 2.0
        };
    }
    None
}

fn sony_angle(k: &[f64; 24], n: usize, r: f64) -> Option<(f64, f64)> {
    if k[13] > 0.0 && r < k[13] {
        let slope = SONY_THETA0 / k[13];
        return Some((r * slope, slope));
    }
    sony_spline_angle(k, n, r, k[13].max(0.0))
}
fn sony_radius(k: &[f64; 24], n: usize, theta: f64) -> Option<(f64, f64)> {
    if k[13] > 0.0 && theta < SONY_THETA0 {
        let slope = k[13] / SONY_THETA0;
        return Some((theta * slope, slope));
    }
    sony_spline_radius(k, n, theta)
}
fn sony_ray(theta: f64) -> (f64, f64) {
    let tt = SONY_TMAX.tan();
    if theta < SONY_TMAX {
        let r = theta.tan();
        (r, 1.0 + r * r)
    } else {
        (tt + (theta - SONY_TMAX) * (1.0 + tt * tt), 1.0 + tt * tt)
    }
}
fn sony_theta(r: f64) -> (f64, f64) {
    let tt = SONY_TMAX.tan();
    if r < tt {
        (r.atan(), 1.0 / (1.0 + r * r))
    } else {
        (
            SONY_TMAX + (r - tt) / (1.0 + tt * tt),
            1.0 / (1.0 + tt * tt),
        )
    }
}
fn compatible(a: Vector2<f64>, b: Vector2<f64>, params: &KernelParams) -> bool {
    let focal = Vector2::new(params.f[0] as f64, params.f[1] as f64);
    let error = (a - b).component_mul(&focal).norm();
    error.is_finite() && error <= 0.05
}
fn sony_forward(p: Vector2<f64>, params: &KernelParams) -> Option<(Vector2<f64>, Matrix2<f64>)> {
    let k = params.k.map(f64::from);
    let n = sony_segments(&k);
    if n == 0 {
        return Some((p, Matrix2::identity()));
    }
    let r = p.norm();
    let (theta, derivative) = sony_theta(r);
    let (original, slope) = sony_radius(&k, n, theta)?;
    let radius = if k[13] > 0.0 && theta >= SONY_THETA0 {
        // Translate only the outer radius curve to meet the original center gain at the recorded join.
        let join = sony_spline_radius(&k, n, SONY_THETA0)?.0;
        sony_spline_angle(&k, n, original, join)?;
        original + (k[13] - join)
    } else {
        original
    };
    let output = radial(p, radius, slope * derivative)?;
    if r > 0.0 {
        let reference = p * (original / r);
        let original_back = sony_ray(sony_angle(&k, n, original)?.0).0;
        if !compatible(p, p * (original_back / r), params)
            || !compatible(output.0, reference, params)
        {
            return None;
        }
    }
    Some(output)
}
fn sony_inverse(target: Vector2<f64>, params: &KernelParams) -> Option<Vector2<f64>> {
    let k = params.k.map(f64::from);
    let n = sony_segments(&k);
    if n == 0 {
        return Some(target);
    }
    let r = target.norm();
    if r == 0.0 {
        return Some(target);
    }
    let original_theta = sony_angle(&k, n, r)?.0;
    let original_radius = sony_ray(original_theta).0;
    let original_back = sony_radius(&k, n, original_theta)?.0;
    if !compatible(target, target * (original_back / r), params) {
        return None;
    }
    let theta = if k[13] > 0.0 && r >= k[13] {
        let join = sony_spline_radius(&k, n, SONY_THETA0)?.0;
        let delta = k[13] - join;
        // Both spline knots and the end extrapolation move with the same radius offset.
        sony_spline_angle(&k, n, r - delta, join)?.0
    } else {
        original_theta
    };
    let output = target * (sony_ray(theta).0 / r);
    if !compatible(output, target * (original_radius / r), params) {
        return None;
    }
    finite(output)
}

fn forward_with_jacobian(
    model: &DistortionModel,
    p: Vector2<f64>,
    params: &KernelParams,
) -> Option<(Vector2<f64>, Matrix2<f64>)> {
    use DistortionModels as M;
    finite(p)?;
    let k = params.k.map(f64::from);
    let r2 = p.norm_squared();
    let r = r2.sqrt();
    let (point, j) = match &model.inner {
        M::OpenCVFisheye(_) if k[..4].iter().all(|v| *v == 0.0) => (p, Matrix2::identity()),
        M::OpenCVFisheye(_) => {
            let theta = r.atan();
            let t2 = theta * theta;
            let scale = 1.0 + t2 * (k[0] + t2 * (k[1] + t2 * (k[2] + t2 * k[3])));
            if !positive_through(&[1.0, 3.0 * k[0], 5.0 * k[1], 7.0 * k[2], 9.0 * k[3]], t2) {
                return None;
            }
            let derivative =
                1.0 + t2 * (3.0 * k[0] + t2 * (5.0 * k[1] + t2 * (7.0 * k[2] + t2 * 9.0 * k[3])));
            radial(p, theta * scale, derivative / (1.0 + r2))?
        }
        M::Poly3(_) | M::Poly5(_) | M::PtLens(_) => {
            let (scale, derivative) = match &model.inner {
                M::Poly3(_) => (1.0 + k[0] * r2, 1.0 + 3.0 * k[0] * r2),
                M::Poly5(_) => (
                    1.0 + r2 * (k[0] + k[1] * r2),
                    1.0 + r2 * (3.0 * k[0] + 5.0 * k[1] * r2),
                ),
                _ => (
                    1.0 + r * (k[2] + r * (k[1] + r * k[0])),
                    1.0 + r * (2.0 * k[2] + r * (3.0 * k[1] + r * 4.0 * k[0])),
                ),
            };
            let coefficients = match &model.inner {
                M::Poly3(_) => vec![1.0, 0.0, 3.0 * k[0]],
                M::Poly5(_) => vec![1.0, 0.0, 3.0 * k[0], 0.0, 5.0 * k[1]],
                _ => vec![1.0, 2.0 * k[2], 3.0 * k[1], 4.0 * k[0]],
            };
            if !positive_through(&coefficients, r) {
                return None;
            }
            radial(p, r * scale, derivative)?
        }
        M::Sony(_) => sony_forward(p, params)?,
        M::OpenCVStandard(_) | M::Insta360(_) => {
            let (v, prefix) = if matches!(&model.inner, M::Insta360(_)) {
                let length = (1.0 + r2).sqrt();
                let den = 1.0 + k[5] * length;
                if den <= 0.0 {
                    return None;
                }
                (
                    p / den,
                    Matrix2::identity() / den - p * p.transpose() * (k[5] / (length * den * den)),
                )
            } else {
                (p, Matrix2::identity())
            };
            let mut branch_k = k;
            if matches!(&model.inner, M::Insta360(_)) {
                branch_k = [0.0; 24];
                branch_k[0] = k[0];
                branch_k[1] = k[1];
                branch_k[4] = k[2];
                branch_k[2] = k[3];
                branch_k[3] = k[4];
            }
            if !standard_branch(&branch_k, v) {
                return None;
            }
            let (x, y) = (v.x, v.y);
            let r2 = v.norm_squared();
            let r4 = r2 * r2;
            let r6 = r4 * r2;
            let (numerator, denominator, nd, dd, p1, p2, s1, s2, s3, s4) =
                if matches!(&model.inner, M::OpenCVStandard(_)) {
                    (
                        1.0 + k[0] * r2 + k[1] * r4 + k[4] * r6,
                        1.0 + k[5] * r2 + k[6] * r4 + k[7] * r6,
                        k[0] + 2.0 * k[1] * r2 + 3.0 * k[4] * r4,
                        k[5] + 2.0 * k[6] * r2 + 3.0 * k[7] * r4,
                        k[2],
                        k[3],
                        k[8],
                        k[9],
                        k[10],
                        k[11],
                    )
                } else {
                    (
                        1.0 + k[0] * r2 + k[1] * r4 + k[2] * r6,
                        1.0,
                        k[0] + 2.0 * k[1] * r2 + 3.0 * k[2] * r4,
                        0.0,
                        k[3],
                        k[4],
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                    )
                };
            let scale = numerator / denominator;
            if !scale.is_finite() || scale <= 0.0 {
                return None;
            }
            let derivative = (nd * denominator - numerator * dd) / (denominator * denominator);
            let output = Vector2::new(
                x * scale + 2.0 * p1 * x * y + p2 * (r2 + 2.0 * x * x) + s1 * r2 + s2 * r4,
                y * scale + p1 * (r2 + 2.0 * y * y) + 2.0 * p2 * x * y + s3 * r2 + s4 * r4,
            );
            let j = Matrix2::new(
                scale
                    + 2.0 * x * x * derivative
                    + 2.0 * p1 * y
                    + 6.0 * p2 * x
                    + 2.0 * x * (s1 + 2.0 * s2 * r2),
                2.0 * x * y * derivative
                    + 2.0 * p1 * x
                    + 2.0 * p2 * y
                    + 2.0 * y * (s1 + 2.0 * s2 * r2),
                2.0 * x * y * derivative
                    + 2.0 * p1 * x
                    + 2.0 * p2 * y
                    + 2.0 * x * (s3 + 2.0 * s4 * r2),
                scale
                    + 2.0 * y * y * derivative
                    + 6.0 * p1 * y
                    + 2.0 * p2 * x
                    + 2.0 * y * (s3 + 2.0 * s4 * r2),
            );
            (output, j * prefix)
        }
        _ => return None,
    };
    if !j.iter().all(|v| v.is_finite()) || j.determinant() <= 0.0 {
        return None;
    }
    Some((finite(point)?, j))
}

pub(crate) fn forward(
    model: &DistortionModel,
    p: Vector2<f64>,
    params: &KernelParams,
) -> Option<Vector2<f64>> {
    forward_with_jacobian(model, p, params).map(|v| v.0)
}

pub(crate) fn inverse(
    model: &DistortionModel,
    target: Vector2<f64>,
    params: &KernelParams,
) -> Option<Vector2<f64>> {
    finite(target)?;
    if matches!(&model.inner, DistortionModels::Sony(_)) {
        return sony_inverse(target, params);
    }
    if (matches!(&model.inner, DistortionModels::OpenCVStandard(_))
        && params.k[..12].iter().all(|v| *v == 0.0))
        || (matches!(&model.inner, DistortionModels::OpenCVFisheye(_))
            && params.k[..4].iter().all(|v| *v == 0.0))
    {
        return Some(target);
    }
    // Start on the optical axis and remain on its connected forward branch; never clip a failed inverse.
    let mut p = Vector2::zeros();
    for _ in 0..60 {
        let (value, j) = forward_with_jacobian(model, p, params)?;
        let error = value - target;
        if error.norm() <= 1e-13 {
            return finite(p);
        }
        let step = j.try_inverse()? * error;
        let mut accepted = None;
        for power in 0..20 {
            let candidate = p - step * 0.5f64.powi(power);
            if let Some((next, _)) = forward_with_jacobian(model, candidate, params) {
                if (next - target).norm_squared() < error.norm_squared() {
                    accepted = Some(candidate);
                    break;
                }
            }
        }
        p = accepted?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kernel(name: &str) -> KernelParams {
        let mut p = KernelParams::default();
        p.f = [500.0; 2];
        match name {
            "opencv_standard" => {
                p.k[..12].copy_from_slice(&[
                    -0.08, 0.012, 0.001, -0.0007, 0.002, 0.003, -0.001, 0.0002, 0.0003, -0.0001,
                    0.0002, 0.0001,
                ]);
            }
            "opencv_fisheye" => {
                p.k[..4].copy_from_slice(&[0.03, -0.004, 0.0005, 0.0001]);
            }
            "poly3" => {
                p.k[0] = 0.04;
            }
            "poly5" => {
                p.k[0] = -0.04;
                p.k[1] = 0.012;
            }
            "ptlens" => {
                p.k[..3].copy_from_slice(&[0.005, -0.01, 0.02]);
            }
            "insta360" => {
                p.k[..6].copy_from_slice(&[0.01, -0.002, 0.0003, 0.001, -0.002, 0.5]);
            }
            "sony" => {
                let angles: Vec<_> = (1..=10).map(|i| (i as f64 * 0.1).atan()).collect();
                let values = super::super::sony::Sony::coefficients_from_lens_curve(&angles, 0.1);
                for (out, v) in p.k.iter_mut().zip(values) {
                    *out = v as f32;
                }
            }
            _ => unreachable!(),
        }
        p
    }

    #[test]
    fn sensor_continuous_optics_match_seven_legacy_physical_models() {
        for name in [
            "opencv_standard",
            "opencv_fisheye",
            "poly3",
            "poly5",
            "ptlens",
            "insta360",
            "sony",
        ] {
            let model = DistortionModel::from_name(name);
            for focal in [500.0, 15000.0] {
                let mut params = kernel(name);
                params.f = [focal as f32; 2];
                let mut points = vec![
                    Vector2::new(0.001, -0.002),
                    Vector2::new(0.15, 0.22),
                    Vector2::new(-0.4, 0.3),
                    Vector2::new(0.65, -0.3),
                    Vector2::new(1.2, 0.5),
                ];
                for x in [20.0, 180.0, 340.0, 480.0, 620.0, 780.0, 940.0] {
                    for y in [20.0, 145.0, 270.0, 395.0, 520.0] {
                        points.push(Vector2::new((x - 480.0) / focal, (y - 270.0) / focal));
                    }
                }
                let mut valid = 0;
                let mut rejected = 0;
                let mut maximum: f64 = 0.0;
                for p in points {
                    let d = forward(&model, p, &params).unwrap();
                    let u = inverse(&model, d, &params).unwrap();
                    assert!(
                        (u - p).norm() < 1e-11,
                        "{name} precise inverse {}",
                        (u - p).norm()
                    );
                    let old_d = model.distort_point(p.x as f32, p.y as f32, 1.0, &params);
                    let old_u = model.undistort_point((d.x as f32, d.y as f32), &params);
                    let old_back = model.undistort_point(old_d, &params);
                    // Reproduce both old SensorProjection acceptance checks before comparing its values.
                    let back_error = old_back.map_or(f64::INFINITY, |v| {
                        (Vector2::new(v.0 as f64, v.1 as f64) - p).norm() * focal
                    });
                    let inverse_error = old_u.map_or(f64::INFINITY, |v| {
                        let f = model.distort_point(v.0, v.1, 1.0, &params);
                        (Vector2::new(f.0 as f64, f.1 as f64) - d).norm() * focal
                    });
                    let fd = (d - Vector2::new(old_d.0 as f64, old_d.1 as f64)).norm() * focal;
                    let ud = old_u.map_or(f64::INFINITY, |v| {
                        (u - Vector2::new(v.0 as f64, v.1 as f64)).norm() * focal
                    });
                    if back_error <= 0.05 && inverse_error <= 0.05 {
                        valid += 1;
                        maximum = maximum.max(fd).max(ud);
                        assert!(
                            fd <= 0.05 && ud <= 0.05,
                            "{name} f={focal} p={p:?} old-valid forward={fd} inverse={ud}"
                        );
                    } else {
                        rejected += 1;
                        println!("legacy invalid model={name} f={focal} p={p:?} ray_closure_px={back_error} sensor_closure_px={inverse_error} new_vs_old_forward_px={fd} new_vs_old_inverse_px={ud}");
                    }
                    let (_, j) = forward_with_jacobian(&model, p, &params).unwrap();
                    for axis in 0..2 {
                        let mut delta = Vector2::zeros();
                        delta[axis] = 1e-6;
                        let piece = |v: Vector2<f64>| {
                            let theta = sony_theta(v.norm()).0;
                            if params.k[13] > 0.0 && theta < SONY_THETA0 {
                                return 0;
                            }
                            let n = sony_segments(&params.k.map(f64::from));
                            let mut i = 0;
                            while i + 1 < n && theta >= params.k[3 + i] as f64 {
                                i += 1;
                            }
                            i + 1
                        };
                        let numeric = if name == "sony" && piece(p - delta) != piece(p + delta) {
                            // A C0 knot has no two-sided derivative; use a second-order stencil in this segment.
                            let sign = if piece(p + delta * 2.0) == piece(p) {
                                1.0
                            } else {
                                -1.0
                            };
                            assert_eq!(piece(p + delta * (2.0 * sign)), piece(p));
                            (-forward(&model, p, &params).unwrap() * 3.0
                                + forward(&model, p + delta * sign, &params).unwrap() * 4.0
                                - forward(&model, p + delta * (2.0 * sign), &params).unwrap())
                                / (2e-6 * sign)
                        } else {
                            (forward(&model, p + delta, &params).unwrap()
                                - forward(&model, p - delta, &params).unwrap())
                                / 2e-6
                        };
                        assert!(
                            (numeric - j.column(axis)).norm() < 1e-7,
                            "{name} axis={axis} p={p:?} difference={} numeric={numeric:?} analytic={:?}",(numeric-j.column(axis)).norm(),j.column(axis)
                        );
                    }
                }
                println!("legacy compatibility model={name} f={focal} old_valid={valid} old_invalid={rejected} max_valid_difference_px={maximum} actual_image_grid=35");
                assert_eq!(valid + rejected, 40);
                assert!(valid > 0);
            }
        }
    }

    #[test]
    fn sensor_continuous_optics_have_no_float32_microstep_plateau() {
        for name in [
            "opencv_standard",
            "opencv_fisheye",
            "poly3",
            "poly5",
            "ptlens",
            "insta360",
            "sony",
        ] {
            let model = DistortionModel::from_name(name);
            let params = kernel(name);
            let p = Vector2::new(0.617234, -0.31123);
            let d = Vector2::new(1e-10, 0.0);
            let (_, j) = forward_with_jacobian(&model, p, &params).unwrap();
            let movement =
                forward(&model, p + d, &params).unwrap() - forward(&model, p, &params).unwrap();
            assert!(movement.norm() > 1e-12, "{name} lost the microstep");
            assert!((movement - j * d).norm() < 1e-14, "{name}");
            let recovered =
                inverse(&model, forward(&model, p + d, &params).unwrap(), &params).unwrap();
            assert!((recovered - p - d).norm() < 1e-12, "{name}");
        }
    }

    #[test]
    fn sensor_continuous_optics_reject_fold_and_fisheye_unreachable_domain() {
        let model = DistortionModel::from_name("opencv_standard");
        let mut p = KernelParams::default();
        p.k[0] = -0.08;
        assert!(forward(&model, Vector2::new(3.0, 0.0), &p).is_none());
        let model = DistortionModel::from_name("opencv_fisheye");
        p.k[0] = 0.01;
        for r in [2.0, 4.0] {
            assert!(inverse(&model, Vector2::new(r, 0.0), &p).is_none());
        }
        let model = DistortionModel::from_name("sony");
        let values = super::super::sony::Sony::coefficients_from_lens_curve(
            &[0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.78, 0.83, 0.82],
            0.1,
        );
        for (out, v) in p.k.iter_mut().zip(values) {
            *out = v as f32;
        }
        assert!(inverse(&model, Vector2::new(1.2, 0.0), &p).is_none());
    }
    #[test]
    fn sensor_continuous_optics_reject_a_second_rising_branch_and_poles() {
        let mut p = KernelParams::default();
        p.k[0] = -2.0;
        p.k[1] = 1.0;
        assert!(forward(
            &DistortionModel::from_name("poly5"),
            Vector2::new(2.0, 0.0),
            &p
        )
        .is_none());
        assert!(forward(
            &DistortionModel::from_name("opencv_fisheye"),
            Vector2::new(1.2f64.tan(), 0.0),
            &p
        )
        .is_none());
        p.k = [0.0; 24];
        p.k[0] = -2.0;
        p.k[5] = -1.0;
        assert!(forward(
            &DistortionModel::from_name("opencv_standard"),
            Vector2::new(2.0, 0.0),
            &p
        )
        .is_none());
    }

    #[test]
    fn sensor_sony_precision_at_every_piecewise_join() {
        let model = DistortionModel::from_name("sony");
        for h in [0.1, 0.001] {
            let angles: Vec<_> = (1..=10).map(|i| (i as f64 * h).atan()).collect();
            let coeffs = super::super::sony::Sony::coefficients_from_lens_curve(&angles, h);
            let mut params = KernelParams::default();
            params.f = [15000.0; 2];
            for (dst, v) in params.k.iter_mut().zip(coeffs) {
                *dst = v as f32;
            }
            let k = params.k.map(f64::from);
            let n = sony_segments(&k);
            let join = sony_spline_radius(&k, n, SONY_THETA0).unwrap().0;
            let delta = k[13] - join;
            assert_eq!(
                inverse(&model, Vector2::zeros(), &params).unwrap(),
                Vector2::zeros()
            );
            assert_eq!(
                forward(&model, Vector2::zeros(), &params).unwrap(),
                Vector2::zeros()
            );
            let tmax_radius = sony_spline_radius(&k, n, SONY_TMAX).unwrap().0 + delta;
            let mut joins: Vec<_> = (1..=10).map(|i| i as f64 * k[1] + delta).collect();
            joins.extend([k[13], tmax_radius, 12.0 * k[1] + delta]);
            let mut valid = 0;
            let mut invalid = 0;
            for radius in joins {
                let eps = 1e-10;
                let left = inverse(&model, Vector2::new(radius - eps, 0.0), &params);
                let right = inverse(&model, Vector2::new(radius + eps, 0.0), &params);
                let further = inverse(&model, Vector2::new(radius + 3.0 * eps, 0.0), &params);
                if let (Some(left), Some(right), Some(further)) = (left, right, further) {
                    let jump = ((right - left) - (further - right)).norm() * 15000.0;
                    println!("Sony continuous h={h} join_radius={radius:.16} delta={delta} remaining_jump_px_at_f15000={jump:.12e}");
                    assert!(
                        jump <= 1e-5,
                        "Sony C0 join h={h} radius={radius} jump={jump}"
                    );
                    for ray in [left, right] {
                        let sensor = forward(&model, ray, &params).unwrap();
                        let back = inverse(&model, sensor, &params).unwrap();
                        assert!((back - ray).norm() < 1e-11);
                    }
                    valid += 1;
                } else {
                    assert!(left.is_none() && right.is_none() && further.is_none());
                    println!("Sony join outside compatibility domain h={h} radius={radius} delta={delta}");
                    invalid += 1;
                }
            }
            assert!(valid > 0);
            assert!(
                invalid > 0,
                "TMAX must report the compatibility limit in this fixture"
            );
        }
    }

    #[test]
    fn sensor_sony_outer_inverse_undoes_the_same_radius_shift() {
        let model = DistortionModel::from_name("sony");
        let mut params = kernel("sony");
        params.f = [15000.0; 2];
        let k = params.k.map(f64::from);
        let n = sony_segments(&k);
        let delta = k[13] - sony_spline_radius(&k, n, SONY_THETA0).unwrap().0;
        let theta = 0.2f64;
        let ray = Vector2::new(theta.tan(), 0.0);
        let expected = sony_spline_radius(&k, n, theta).unwrap().0 + delta;
        let sensor = forward(&model, ray, &params).unwrap();
        assert!((sensor.x - expected).abs() < 1e-13);
        assert!((inverse(&model, sensor, &params).unwrap() - ray).norm() < 1e-12);
        let wrong = sony_ray(sony_angle(&k, n, sensor.x).unwrap().0).0;
        assert!(
            (wrong - ray.x).abs() > 1e-10,
            "an unshifted inverse would hide this regression"
        );
    }

    #[test]
    fn sensor_sony_rejects_an_incompatible_outer_shift_but_keeps_the_center() {
        let model = DistortionModel::from_name("sony");
        let mut params = kernel("sony");
        params.f = [15000.0; 2];
        params.k[13] += 0.001;
        let center = Vector2::new((SONY_THETA0 * 0.5).tan(), 0.0);
        let mapped = forward(&model, center, &params).unwrap();
        assert!((inverse(&model, mapped, &params).unwrap() - center).norm() < 1e-12);
        assert!(forward(&model, Vector2::new(0.2, 0.0), &params).is_none());
        assert!(inverse(&model, Vector2::new(0.2, 0.0), &params).is_none());
    }
}
