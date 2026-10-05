// SPDX-License-Identifier: GPL-3.0-or-later

use nalgebra::{Matrix3, Vector3};

// A small fraction of the available displacement, rather than an objective-only tolerance.
const CONVERGENCE_BUDGET_FRACTION: f64 = 2e-4;

#[derive(Clone, Copy, Debug)]
pub(super) struct Geometry {
    pub world_to_camera: Matrix3<f64>,
    pub focal_ratio: f64,
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct SolveStats {
    pub iterations: usize,
    pub stages: usize,
    pub line_searches: usize,
    pub maximum_fraction: f64,
    pub requested_fraction: f64,
    pub correction_fraction: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum SolveError { InvalidInput, Singular, NoDescent, NotConverged, Cancelled }

#[derive(Clone, Copy, Debug)]
pub(super) struct SolveFailure {
    pub reason: SolveError,
    pub stats: SolveStats,
}

// Symmetric block-pentadiagonal storage, with the two bands below the diagonal.
struct Bands {
    diagonal: Vec<Matrix3<f64>>,
    first: Vec<Matrix3<f64>>,
    second: Vec<Matrix3<f64>>,
}

impl Bands {
    fn zeros(n: usize) -> Self {
        Self { diagonal: vec![Matrix3::zeros(); n], first: vec![Matrix3::zeros(); n], second: vec![Matrix3::zeros(); n] }
    }

    fn solve(&mut self, rhs: &[Vector3<f64>], fixed: &[bool]) -> Option<Vec<Vector3<f64>>> {
        let n = rhs.len();
        // Eliminate fixed-zero variables exactly, including their off-diagonal blocks.
        for i in 0..n {
            if fixed[i] { self.diagonal[i] = Matrix3::identity(); }
            if i > 0 && (fixed[i] || fixed[i - 1]) { self.first[i] = Matrix3::zeros(); }
            if i > 1 && (fixed[i] || fixed[i - 2]) { self.second[i] = Matrix3::zeros(); }
        }
        let mut inverses = Vec::<Matrix3<f64>>::with_capacity(n);
        let mut value = vec![Vector3::zeros(); n];
        for i in 0..n {
            if i > 1 { self.second[i] *= inverses[i - 2].transpose(); }
            if i > 0 {
                if i > 1 {
                    let product = self.second[i] * self.first[i - 1].transpose();
                    self.first[i] -= product;
                }
                self.first[i] *= inverses[i - 1].transpose();
            }
            let diagonal = self.diagonal[i] - self.first[i] * self.first[i].transpose() - self.second[i] * self.second[i].transpose();
            let inverse = diagonal.cholesky()?.l().try_inverse()?;
            let mut b = if fixed[i] { Vector3::zeros() } else { rhs[i] };
            if i > 0 { b -= self.first[i] * value[i - 1]; }
            if i > 1 { b -= self.second[i] * value[i - 2]; }
            value[i] = inverse * b;
            inverses.push(inverse);
        }
        for i in (0..n).rev() {
            let mut b = value[i];
            if i + 1 < n { b -= self.first[i + 1].transpose() * value[i + 1]; }
            if i + 2 < n { b -= self.second[i + 2].transpose() * value[i + 2]; }
            value[i] = inverses[i].transpose() * b;
        }
        value.iter().all(|v| v.iter().all(|x| x.is_finite())).then_some(value)
    }
}

struct Penalty {
    weight: Vec<f64>,
    curvature: Vec<([f64; 3], f64)>,
}

impl Penalty {
    fn new(times: &[f64], sigma: f64) -> Option<Self> {
        let n = times.len();
        let intervals: Vec<_> = times.windows(2).map(|t| t[1] - t[0]).collect();
        if n < 3 || intervals.iter().any(|dt| !dt.is_finite() || *dt <= 0.0) { return None; }
        let mean_dt = (times[n - 1] - times[0]) / (n - 1) as f64;
        let mut weight = vec![0.0; n];
        weight[0] = intervals[0] / (2.0 * mean_dt);
        weight[n - 1] = intervals[n - 2] / (2.0 * mean_dt);
        let lambda = sigma.powi(4) / (4.0 * std::f64::consts::LN_2.powi(2));
        let mut curvature = Vec::with_capacity(n - 2);
        for i in 1..n - 1 {
            let (a, b) = (intervals[i - 1], intervals[i]);
            weight[i] = (a + b) / (2.0 * mean_dt);
            let scale = 2.0 / (a + b);
            curvature.push(([scale / a, -scale * (1.0 / a + 1.0 / b), scale / b], lambda * weight[i]));
        }
        Some(Self { weight, curvature })
    }

    fn energy(&self, e: &[Vector3<f64>]) -> f64 {
        let mut value: f64 = e.iter().zip(&self.weight).map(|(e, w)| w * e.norm_squared()).sum();
        for (i, (d, w)) in self.curvature.iter().enumerate() {
            value += w * (e[i] * d[0] + e[i + 1] * d[1] + e[i + 2] * d[2]).norm_squared();
        }
        value
    }

    fn derivatives(&self, e: &[Vector3<f64>]) -> (Vec<Vector3<f64>>, Bands) {
        let mut gradient: Vec<_> = e.iter().zip(&self.weight).map(|(e, w)| e * (2.0 * w)).collect();
        let mut h = Bands::zeros(e.len());
        for (i, w) in self.weight.iter().enumerate() { h.diagonal[i] = Matrix3::identity() * (2.0 * w); }
        for (i, (d, w)) in self.curvature.iter().enumerate() {
            let second = e[i] * d[0] + e[i + 1] * d[1] + e[i + 2] * d[2];
            for j in 0..3 {
                gradient[i + j] += second * (2.0 * w * d[j]);
                h.diagonal[i + j] += Matrix3::identity() * (2.0 * w * d[j] * d[j]);
            }
            h.first[i + 1] += Matrix3::identity() * (2.0 * w * d[0] * d[1]);
            h.first[i + 2] += Matrix3::identity() * (2.0 * w * d[1] * d[2]);
            h.second[i + 2] += Matrix3::identity() * (2.0 * w * d[0] * d[2]);
        }
        (gradient, h)
    }
}

fn barrier(
    request: &[Vector3<f64>], e: &[Vector3<f64>], maps: &[Matrix3<f64>], along_axis: bool,
    mu: f64, mut derivatives: Option<(&mut [Vector3<f64>], &mut Bands)>,
) -> Option<f64> {
    let mut energy = 0.0;
    for i in 0..e.len() {
        let a = &maps[i];
        let x = a * (request[i] - e[i]);
        let gap = 1.0 - x.x * x.x - x.y * x.y;
        if !gap.is_finite() || gap <= 0.0 { return None; }
        energy -= mu * gap.ln();
        if let Some((gradient, h)) = &mut derivatives {
            let ax = a.row(0).transpose();
            let ay = a.row(1).transpose();
            let v = ax * x.x + ay * x.y;
            gradient[i] -= v * (2.0 * mu / gap);
            h.diagonal[i] += (ax * ax.transpose() + ay * ay.transpose()) * (2.0 * mu / gap)
                + v * v.transpose() * (4.0 * mu / (gap * gap));
        }
        if along_axis {
            let gap = 1.0 - x.z * x.z;
            if !gap.is_finite() || gap <= 0.0 { return None; }
            energy -= mu * gap.ln();
            if let Some((gradient, h)) = &mut derivatives {
                let az = a.row(2).transpose();
                gradient[i] -= az * (2.0 * mu * x.z / gap);
                h.diagonal[i] += az * az.transpose() * (2.0 * mu * (1.0 + x.z * x.z) / (gap * gap));
            }
        }
    }
    Some(energy)
}

/// Keep the requested world-space compensation, subtracting only a smooth budget correction.
pub(super) fn constrain(
    times: &[f64], request: &[Vector3<f64>], geometry: &[Geometry], fixed: &[bool],
    sigma: f64, budget: f64, along_axis: bool, cancelled: &dyn Fn() -> bool,
) -> Result<(Vec<Vector3<f64>>, SolveStats), SolveFailure> {
    let mut stats = SolveStats::default();
    match constrain_inner(times, request, geometry, fixed, sigma, budget, along_axis, cancelled, &mut stats) {
        Ok(curve) => Ok((curve, stats)),
        Err(reason) => Err(SolveFailure { reason, stats }),
    }
}

fn constrain_inner(
    times: &[f64], request: &[Vector3<f64>], geometry: &[Geometry], fixed: &[bool],
    sigma: f64, budget: f64, along_axis: bool, cancelled: &dyn Fn() -> bool, stats: &mut SolveStats,
) -> Result<Vec<Vector3<f64>>, SolveError> {
    let n = times.len();
    if n != request.len() || n != geometry.len() || n != fixed.len() || !sigma.is_finite() || sigma <= 0.0
        || !budget.is_finite() || budget <= 0.0 { return Err(SolveError::InvalidInput); }
    if cancelled() { return Err(SolveError::Cancelled); }
    if n < 3 { return Ok(request.to_vec()); }
    let focal = geometry.iter().map(|g| g.focal_ratio).fold(0.0f64, f64::max);
    if focal <= 0.0 || !focal.is_finite() || geometry.iter().any(|g| g.focal_ratio <= 0.0 || !g.focal_ratio.is_finite()
        || !g.world_to_camera.iter().all(|v| v.is_finite())) || request.iter().any(|v| !v.iter().all(|x| x.is_finite())) {
        return Err(SolveError::InvalidInput);
    }
    // Normalize to the budget so convergence does not depend on the clip's arbitrary position scale.
    let scale = budget / focal;
    let target: Vec<_> = request.iter().map(|v| v / scale).collect();
    let maps: Vec<_> = geometry.iter().map(|g| {
        let mut a = g.world_to_camera;
        a.row_mut(0).scale_mut(g.focal_ratio / focal);
        a.row_mut(1).scale_mut(g.focal_ratio / focal);
        a.row_mut(2).scale_mut(1.0 / focal);
        a
    }).collect();
    let maximum = |values: &[Vector3<f64>]| values.iter().zip(&maps).map(|(v, a)| {
        let x = a * v;
        if along_axis { x.xy().norm().max(x.z.abs()) } else { x.xy().norm() }
    }).fold(0.0f64, f64::max);
    stats.maximum_fraction = maximum(&target);
    stats.requested_fraction = stats.maximum_fraction;
    if stats.maximum_fraction <= 1.0 { return Ok(request.to_vec()); }
    let penalty = Penalty::new(times, sigma).ok_or(SolveError::InvalidInput)?;
    let mut e = vec![Vector3::zeros(); n];
    for i in 0..n {
        let mut camera = geometry[i].world_to_camera * target[i];
        let radius = 0.9 * focal / geometry[i].focal_ratio;
        let norm = camera.xy().norm();
        if norm > radius { camera.x *= radius / norm; camera.y *= radius / norm; }
        if along_axis { camera.z = camera.z.clamp(-0.9 * focal, 0.9 * focal); }
        e[i] = target[i] - geometry[i].world_to_camera.transpose() * camera;
        if fixed[i] { e[i] = Vector3::zeros(); }
    }
    // Start the barrier at the objective's scale; a tiny initial barrier pins the first
    // Newton steps against individual constraints before the long correction can bend.
    let mut mu = (penalty.energy(&e) / n as f64).max(1.0);
    for stage in 0..18 {
        stats.stages = stage + 1;
        let previous = e.clone();
        let mut converged = false;
        for _ in 0..60 {
            if cancelled() { return Err(SolveError::Cancelled); }
            let (mut gradient, mut h) = penalty.derivatives(&e);
            let energy = penalty.energy(&e) + barrier(&target, &e, &maps, along_axis, mu, Some((&mut gradient, &mut h))).ok_or(SolveError::InvalidInput)?;
            for i in 0..n { if fixed[i] { gradient[i] = Vector3::zeros(); } }
            let rhs: Vec<_> = gradient.iter().map(|v| -v).collect();
            let step = h.solve(&rhs, fixed).ok_or(SolveError::Singular)?;
            stats.iterations += 1;
            let decrement: f64 = -gradient.iter().zip(&step).map(|(g, p)| g.dot(p)).sum::<f64>();
            if decrement.is_finite() && decrement >= 0.0 && decrement <= 1e-10 * (1.0 + energy.abs())
                && maximum(&step) <= CONVERGENCE_BUDGET_FRACTION {
                converged = true;
                break;
            }
            if !decrement.is_finite() || decrement <= 0.0 { return Err(SolveError::NoDescent); }
            let mut alpha = 1.0;
            let mut accepted = false;
            for _ in 0..40 {
                if cancelled() { return Err(SolveError::Cancelled); }
                stats.line_searches += 1;
                let candidate: Vec<_> = e.iter().zip(&step).map(|(v, p)| v + p * alpha).collect();
                if let Some(b) = barrier(&target, &candidate, &maps, along_axis, mu, None) {
                    let next = penalty.energy(&candidate) + b;
                    if next <= energy - 0.01 * alpha * decrement {
                        e = candidate;
                        accepted = true;
                        break;
                    }
                }
                alpha *= 0.5;
            }
            if !accepted { return Err(SolveError::NoDescent); }
        }
        if !converged { return Err(SolveError::NotConverged); }
        let change: Vec<_> = e.iter().zip(&previous).map(|(a, b)| a - b).collect();
        if mu <= 1e-7 && maximum(&change) <= CONVERGENCE_BUDGET_FRACTION {
            let output: Vec<_> = target.iter().zip(&e).map(|(c, e)| c - e).collect();
            stats.maximum_fraction = maximum(&output);
            stats.correction_fraction = maximum(&e);
            if stats.maximum_fraction > 1.0 + 1e-10 { return Err(SolveError::NotConverged); }
            return Ok(output.into_iter().map(|v| v * scale).collect());
        }
        mu *= 0.1;
    }
    Err(SolveError::NotConverged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_solver_matches_a_dense_system_with_exact_fixed_nodes() {
        let n = 11;
        let times: Vec<_> = (0..n).map(|i| i as f64 * 0.031 + (i % 2) as f64 * 0.002).collect();
        let penalty = Penalty::new(&times, 0.2).unwrap();
        let (_, mut bands) = penalty.derivatives(&vec![Vector3::zeros(); n]);
        let mut dense = nalgebra::DMatrix::<f64>::zeros(n * 3, n * 3);
        let fixed: Vec<_> = (0..n).map(|i| i == 0 || i == 5 || i == n - 1).collect();
        for i in 0..n { for j in i.saturating_sub(2)..=i {
            let block = if i == j { bands.diagonal[i] } else if i == j + 1 { bands.first[i] } else { bands.second[i] };
            for a in 0..3 { for b in 0..3 { dense[(3*i+a, 3*j+b)] = block[(a,b)]; dense[(3*j+b,3*i+a)] = block[(a,b)]; } }
        } }
        let rhs: Vec<_> = (0..n).map(|i| Vector3::new(i as f64, 2.0, -3.0)).collect();
        let mut expected_rhs = nalgebra::DVector::from_iterator(3*n, rhs.iter().flat_map(|v| v.iter().copied()));
        for i in 0..n { if fixed[i] { for k in 0..3 { let row = 3*i+k; dense.row_mut(row).fill(0.0); dense.column_mut(row).fill(0.0); dense[(row,row)] = 1.0; expected_rhs[row] = 0.0; } } }
        let expected = dense.cholesky().unwrap().solve(&expected_rhs);
        let actual = bands.solve(&rhs, &fixed).unwrap();
        for i in 0..n { for k in 0..3 { assert!((actual[i][k] - expected[3*i+k]).abs() < 1e-10); } }
    }

    #[test]
    fn feasible_request_is_unchanged_and_cancellation_is_reported() {
        let times = [0.0, 0.03, 0.07, 0.1];
        let request = [Vector3::zeros(), Vector3::new(0.001, -0.002, 0.003), Vector3::zeros(), Vector3::zeros()];
        let geometry = [Geometry { world_to_camera: Matrix3::identity(), focal_ratio: 2.0 }; 4];
        let fixed = [true, false, false, true];
        assert_eq!(constrain(&times, &request, &geometry, &fixed, 0.5, 0.02, true, &|| false).unwrap().0, request);
        assert_eq!(constrain(&times, &request, &geometry, &fixed, 0.5, 0.02, true, &|| true).unwrap_err().reason, SolveError::Cancelled);
    }

    #[test]
    fn smooth_budget_correction_respects_xy_z_and_endpoints() {
        let n = 181;
        let times: Vec<_> = (0..n).map(|i| i as f64 / 30.0).collect();
        let request: Vec<_> = times.iter().map(|t| Vector3::new(0.04 * (std::f64::consts::PI*t/6.0).sin(), 0.0, 0.03 * (std::f64::consts::PI*t/6.0).sin())).collect();
        let geometry = vec![Geometry { world_to_camera: Matrix3::identity(), focal_ratio: 2.0 }; n];
        let fixed: Vec<_> = (0..n).map(|i| i == 0 || i == n - 1).collect();
        let (output, stats) = constrain(&times, &request, &geometry, &fixed, 0.5, 0.02, true, &|| false).unwrap();
        assert!(stats.iterations > 0);
        assert!(output.iter().all(|x| x.xy().norm()*2.0 <= 0.02 && x.z.abs() <= 0.02));
        assert_eq!(output[0], request[0]);
        assert_eq!(output[n-1], request[n-1]);
        let correction: Vec<_> = request.iter().zip(&output).map(|(a,b)| a-b).collect();
        let maximum_acceleration = correction.windows(3).map(|v| (v[0]-2.0*v[1]+v[2]).norm()).fold(0.0f64,f64::max);
        assert!(maximum_acceleration < 0.001);
    }

    #[test]
    fn budget_follows_dynamic_focal_length_and_world_frame_rotation() {
        let n=121;
        let times:Vec<_>=(0..n).map(|i|i as f64/30.0+(i%2) as f64*0.002).collect();
        let request:Vec<_>=times.iter().map(|t|Vector3::new(0.025*(std::f64::consts::PI*t/times[n-1]).sin(),0.003*(t*4.0).sin(),0.01)).collect();
        let mut request=request;
        request[0]=Vector3::zeros();request[n-1]=Vector3::zeros();
        let geometry:Vec<_>=times.iter().map(|t|Geometry{world_to_camera:nalgebra::Rotation3::from_euler_angles(0.0,t*0.03,0.0).into_inner(),focal_ratio:1.0+t/2.0}).collect();
        let fixed:Vec<_>=(0..n).map(|i|i==0||i==n-1).collect();
        let (a,_)=constrain(&times,&request,&geometry,&fixed,0.3,0.02,true,&||false).unwrap();
        for (point,g) in a.iter().zip(&geometry) {let p=g.world_to_camera*point;assert!(p.xy().norm()*g.focal_ratio<=0.02+1e-12&&p.z.abs()<=0.02+1e-12);}
        let rotation=nalgebra::Rotation3::from_euler_angles(0.3,-0.4,0.5).into_inner();
        let rotated:Vec<_>=request.iter().map(|p|rotation*p).collect();
        let transformed:Vec<_>=geometry.iter().map(|g|Geometry{world_to_camera:g.world_to_camera*rotation.transpose(),..*g}).collect();
        let (b,_)=constrain(&times,&rotated,&transformed,&fixed,0.3,0.02,true,&||false).unwrap();
        for (a,b) in a.iter().zip(b) {assert!((rotation*a-b).norm()<1e-6);}
    }
}
