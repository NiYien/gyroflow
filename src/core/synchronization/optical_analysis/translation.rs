// SPDX-License-Identifier: GPL-3.0-or-later

//! Camera translation and persistent depths after the measured gyro rotation has been removed.

use std::{collections::HashMap, sync::OnceLock};
use nalgebra::{Matrix3, Matrix3x6, Matrix6, Rotation3, Vector3, Vector6};
use super::{MIN_BAND_POINTS, odometry::{NOISE_PX, ROBUST_PX, ITERATIONS, DEPTH_NOISE_REL}};

/// One tracked point of one frame pair, against the quaternions.
pub struct PairPoint {
    pub id: u32,
    pub band: u8,
    pub p: Vector3<f64>,
    pub r: Vector3<f64>,
}

pub struct PairTranslation {
    /// Camera movement in the second frame's quaternion coordinates, in the pair's scale.
    pub c: Vector3<f64>,
    /// The same movement in the segment's fixed scale.
    pub c_segment: Vector3<f64>,
    /// Inverse depths used by this pair's model, before propagation and normalization.
    pub inv_depth: HashMap<u32, f64>,
    /// Median inverse depth of the inliers, in the segment's scale.
    pub ref_inv_depth: f64,
    pub confidence: f64,
    /// Median age of the inliers' tracks, seconds.
    pub track_age_s: f64,
    pub new_segment: bool,
    /// Smoothed historical prediction cost relative to a rotation-only fit.
    pub pred: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TranslationSolverConfig {
    /// Prior on camera x/y rotation (pitch/yaw), in tracked pixels.
    pub yaw_pitch_prior_px: f64,
    /// Prior on optical-axis rotation (roll), in tracked pixels.
    pub roll_prior_px: f64,
    pub pred_max: f64,
}

impl TranslationSolverConfig {
    pub const DEFAULT: Self = Self { yaw_pitch_prior_px: 0.05, roll_prior_px: 0.5, pred_max: 0.8 };

    pub fn resolved() -> Self {
        static RESOLVED: OnceLock<TranslationSolverConfig> = OnceLock::new();
        *RESOLVED.get_or_init(|| {
            let mut config = Self::DEFAULT;
            let raw = std::env::var("GYROFLOW_TRANSLATION_ROT_PRIOR_PX").ok();
            if let Some(raw) = raw.as_deref() {
                match raw.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x > 0.0) {
                    Some(value) => config.yaw_pitch_prior_px = value,
                    None => log::warn!(target: "lifecycle", "GYROFLOW_TRANSLATION_ROT_PRIOR_PX={} invalid, falling back to {}", raw, config.yaw_pitch_prior_px),
                }
            }
            log::info!(target: "lifecycle", "translation_rot_prior_px_config resolved={} source={}",
                config.yaw_pitch_prior_px, if raw.is_some() { "env" } else { "default" });
            let raw = std::env::var("GYROFLOW_TRANSLATION_ROLL_PRIOR_PX").ok();
            if let Some(raw) = raw.as_deref() {
                match raw.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x > 0.0) {
                    Some(value) => config.roll_prior_px = value,
                    None => log::warn!(target: "lifecycle", "GYROFLOW_TRANSLATION_ROLL_PRIOR_PX={} invalid, falling back to {}", raw, config.roll_prior_px),
                }
            }
            log::info!(target: "lifecycle", "translation_roll_prior_px_config resolved={} source={}",
                config.roll_prior_px, if raw.is_some() { "env" } else { "default" });
            let raw = std::env::var("GYROFLOW_TRANSLATION_PRED_MAX").ok();
            if let Some(raw) = raw.as_deref() {
                match raw.trim().parse::<f64>().ok().filter(|x| x.is_finite() && *x > 0.0 && *x <= 10.0) {
                    Some(value) => config.pred_max = value,
                    None => log::warn!(target: "lifecycle", "GYROFLOW_TRANSLATION_PRED_MAX={} invalid, falling back to {}", raw, config.pred_max),
                }
            }
            log::info!(target: "lifecycle", "translation_pred_max_config resolved={} source={}",
                config.pred_max, if raw.is_some() { "env" } else { "default" });
            config
        })
    }
}

pub struct TranslationSolver {
    config: TranslationSolverConfig,
    depth: HashMap<u32, (f64, f64)>,
    velocity: Vector3<f64>,
    lambda: f64,
    first_seen: HashMap<u32, f64>,
    pred: Option<f64>,
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

impl TranslationSolver {
    pub fn new(config: TranslationSolverConfig) -> Self {
        Self { config, depth: HashMap::new(), velocity: Vector3::zeros(),
            lambda: 1.0, first_seen: HashMap::new(), pred: None }
    }

    fn reset(&mut self) { *self = Self::new(self.config); }

    fn failed(&mut self) -> PairTranslation {
        self.reset();
        PairTranslation { c: Vector3::zeros(), c_segment: Vector3::zeros(), inv_depth: HashMap::new(),
            ref_inv_depth: 0.0, confidence: 0.0, track_age_s: 0.0, new_segment: true, pred: None }
    }

    // Fit a free rotation without priors on the same known tracks.
    fn rotation_cost(a: &[Vector3<f64>], b: &[Vector3<f64>], known: &[(usize, f64)],
                     weight: f64, robust: f64) -> Option<f64> {
        let mut m = Matrix3::identity();
        for _ in 0..ITERATIONS {
            let mut h = Matrix3::zeros();
            let mut g = Vector3::zeros();
            for &(i, _) in known {
                let ma = m * a[i];
                let r = b[i].cross(&ma);
                let j = -b[i].cross_matrix() * ma.cross_matrix();
                let w = weight / (1.0 + (r.norm() / robust).powi(2));
                h += j.transpose() * j * w;
                g += j.transpose() * r * w;
            }
            let dx = -h.cholesky()?.solve(&g);
            m = Rotation3::from_scaled_axis(dx).into_inner() * m;
        }
        Some(known.iter().map(|&(i, _)| {
            let r = b[i].cross(&(m * a[i]));
            (r.norm_squared() / robust.powi(2)).ln_1p()
        }).sum())
    }

    // Fit motion without priors while keeping the carried depths fixed.
    fn structure_cost(a: &[Vector3<f64>], b: &[Vector3<f64>], known: &[(usize, f64)],
                      velocity: Vector3<f64>, weight: f64, robust: f64) -> Option<f64> {
        let (mut m, mut v) = (Matrix3::identity(), velocity);
        for _ in 0..ITERATIONS {
            let mut h = Matrix6::zeros();
            let mut g = Vector6::zeros();
            for &(i, rho) in known {
                let ma = m * a[i];
                let bx = b[i].cross_matrix();
                let r = b[i].cross(&(ma - rho * v));
                let mut j = Matrix3x6::zeros();
                j.fixed_view_mut::<3, 3>(0, 0).copy_from(&(-bx * ma.cross_matrix()));
                j.fixed_view_mut::<3, 3>(0, 3).copy_from(&(-bx * rho));
                let w = weight / (1.0 + (r.norm() / robust).powi(2));
                h += j.transpose() * j * w;
                g += j.transpose() * r * w;
            }
            let dx = -h.cholesky()?.solve(&g);
            m = Rotation3::from_scaled_axis(dx.fixed_rows::<3>(0).into_owned()).into_inner() * m;
            v += dx.fixed_rows::<3>(3);
        }
        Some(known.iter().map(|&(i, rho)| {
            let r = b[i].cross(&(m * a[i] - rho * v));
            (r.norm_squared() / robust.powi(2)).ln_1p()
        }).sum())
    }

    /// `px` is the tracked pixel's angular size; `tb_s` is the second frame's time, seconds.
    pub fn step(&mut self, pts: &[PairPoint], px: f64, tb_s: f64) -> PairTranslation {
        let n = pts.len();
        if n < MIN_BAND_POINTS { return self.failed(); }
        let a: Vec<_> = pts.iter().map(|p| p.p).collect();
        let b: Vec<_> = pts.iter().map(|p| (p.p + p.r).normalize()).collect();
        let weight = 1.0 / (NOISE_PX * px).powi(2);
        let robust = ROBUST_PX * px;
        let known = pts.iter().filter(|p| self.depth.contains_key(&p.id)).count();
        let new_segment = known < MIN_BAND_POINTS;
        let (rho0, var, v0) = if new_segment {
            self.reset();
            Self::start(&a, &b, Matrix3::identity(), robust)
        } else {
            let (rho0, var) = pts.iter().map(|p| self.depth.get(&p.id).copied().unwrap_or((1.0, 4.0))).unzip();
            (rho0, var, self.velocity)
        };
        // Compare both models on exactly the known tracks before the current joint solve.
        if !new_segment {
            let known: Vec<_> = pts.iter().enumerate().filter_map(|(i, point)|
                self.depth.get(&point.id).map(|&(rho, _)| (i, rho))).collect();
            let Some(c_pred) = Self::structure_cost(&a, &b, &known, self.velocity, weight, robust) else { return self.failed(); };
            let Some(c_rot) = Self::rotation_cost(&a, &b, &known, weight, robust) else { return self.failed(); };
            let ratio = c_pred / c_rot.max(1e-300);
            self.pred = Some(self.pred.map_or(ratio, |previous| 0.2 * ratio + 0.8 * previous));
        }
        let prediction_rejected = self.pred.is_some_and(|pred| pred > self.config.pred_max);
        let mut rho = rho0.clone();
        let (mut m, mut v) = (Matrix3::identity(), v0);
        let mut h = Matrix6::zeros();
        let mut hxr = vec![Vector6::zeros(); n];
        let mut hrr = vec![0.0; n];
        let mut gr = vec![0.0; n];
        for _ in 0..ITERATIONS {
            let mut hxx = Matrix6::zeros();
            let mut gx = Vector6::zeros();
            for i in 0..n {
                let ma = m * a[i];
                let bx = b[i].cross_matrix();
                let r = b[i].cross(&(ma - v * rho[i]));
                let w = weight / (1.0 + (r.norm() / robust).powi(2));
                let mut jx = Matrix3x6::zeros();
                jx.fixed_view_mut::<3, 3>(0, 0).copy_from(&(-bx * ma.cross_matrix()));
                jx.fixed_view_mut::<3, 3>(0, 3).copy_from(&(-bx * rho[i]));
                let jr = -b[i].cross(&v);
                let jxt = jx.transpose();
                hxx += jxt * jx * w;
                gx += jxt * r * w;
                hxr[i] = jxt * jr * w;
                hrr[i] = w * jr.norm_squared() + 1.0 / var[i];
                gr[i] = w * jr.dot(&r) + (rho[i] - rho0[i]) / var[i];
            }
            let omega = Rotation3::from_matrix_unchecked(m).scaled_axis();
            for k in 0..3 {
                let prior_px = if k < 2 { self.config.yaw_pitch_prior_px } else { self.config.roll_prior_px };
                let rot_weight = 1.0 / (prior_px * px).powi(2);
                hxx[(k, k)] += rot_weight;
                gx[k] += omega[k] * rot_weight;
            }
            // Eliminate each point's depth before solving the six motion components.
            h = hxx;
            let mut g = gx;
            for i in 0..n {
                h -= hxr[i] * hxr[i].transpose() / hrr[i];
                g -= hxr[i] * (gr[i] / hrr[i]);
            }
            let Some(chol) = h.cholesky() else { return self.failed(); };
            let dx = -chol.solve(&g);
            m = Rotation3::from_scaled_axis(dx.fixed_rows::<3>(0).into_owned()).into_inner() * m;
            v += dx.fixed_rows::<3>(3);
            for i in 0..n { rho[i] = (rho[i] - (gr[i] + hxr[i].dot(&dx)) / hrr[i]).max(0.0); }
        }
        let Some(cov) = h.try_inverse() else { return self.failed(); };

        // Capture pair-scale outputs before advancing depths and normalizing the filter state.
        let c = m.transpose() * v;
        let inliers: Vec<usize> = (0..n).filter(|&i| b[i].cross(&(m * a[i] - v * rho[i])).norm() <= 3.0 * robust).collect();
        let ref_rho = median(inliers.iter().map(|&i| rho[i]).collect());
        let sigma_c = cov[(3, 3)].max(cov[(4, 4)]).max(cov[(5, 5)]).sqrt();
        let lin = |x: f64, x0: f64, x1: f64| ((x - x0) / (x1 - x0)).clamp(0.0, 1.0);
        let posterior = if c.norm() * ref_rho / px < 0.3 { 1.0 } else { lin(sigma_c / c.norm(), 1.0, 0.25) };
        let confidence = lin(inliers.len() as f64, 60.0, 200.0)
            .min(lin(inliers.len() as f64 / n as f64, 0.3, 0.6)).min(posterior);
        let confidence = if new_segment || prediction_rejected { 0.0 } else { confidence };
        self.first_seen = pts.iter().map(|p| (p.id, self.first_seen.get(&p.id).copied().unwrap_or(tb_s))).collect();
        let output = PairTranslation {
            c, c_segment: c * self.lambda,
            inv_depth: pts.iter().zip(&rho).map(|(p, r)| (p.id, *r)).collect(),
            ref_inv_depth: ref_rho / self.lambda, confidence,
            track_age_s: median(inliers.iter().map(|&i| tb_s - self.first_seen[&pts[i].id]).collect()),
            new_segment, pred: self.pred,
        };
        if prediction_rejected {
            self.reset();
            return output;
        }
        let mut depth: Vec<(f64, f64)> = (0..n).map(|i| {
            let r = rho[i] / (a[i] - c * rho[i]).norm().max(1e-6);
            (r, 1.0 / hrr[i] + (DEPTH_NOISE_REL * r).powi(2))
        }).collect();
        let s = median(depth.iter().map(|d| d.0).collect());
        let s = if s > 1e-9 { s } else { 1.0 };
        for d in depth.iter_mut() { d.0 /= s; d.1 /= s * s; }
        self.depth = pts.iter().map(|p| p.id).zip(depth).collect();
        self.velocity = v * s;
        self.lambda /= s;
        output
    }

    /// Where a first frame pair starts from: the direction of the translation is the one in all the planes that each
    /// point's two bearings span, less the rotation `m0`; the depths follow from it. Started from equal depths and no
    /// velocity instead, the first pair over flat ground can settle on the other way a plane's motion can be explained,
    /// with a rotation pixels off, which the pairs after it then keep
    fn start(a: &[Vector3<f64>], b: &[Vector3<f64>], m0: Matrix3<f64>, robust: f64) -> (Vec<f64>, Vec<f64>, Vector3<f64>) {
        let n = a.len();
        let ma: Vec<Vector3<f64>> = a.iter().map(|a| m0 * a).collect();
        let mut planes = Matrix3::zeros();
        for (ma, b) in ma.iter().zip(b) {
            let normal = ma.cross(b);
            planes += normal * normal.transpose() / (1.0 + (normal.norm() / (5.0 * robust)).powi(2));
        }
        let eigen = planes.symmetric_eigen();
        let mut dir: Vector3<f64> = eigen.eigenvectors.column(eigen.eigenvalues.imin()).into_owned();
        // b × (m0·a) = ρ·(b × dir) for each point
        let mut rho: Vec<f64> = (0..n).map(|i| {
            let bd = b[i].cross(&dir);
            b[i].cross(&ma[i]).dot(&bd) / bd.norm_squared().max(1e-12)
        }).collect();
        let mut sorted = rho.clone();
        sorted.sort_by(|x, y| x.total_cmp(y));
        let mut median = sorted.get(n / 2).copied().unwrap_or(1.0);
        if median < 0.0 {
            dir = -dir;
            median = -median;
            rho.iter_mut().for_each(|r| *r = -*r);
        }
        let s = median.max(1e-9);
        let rho: Vec<f64> = rho.into_iter().map(|r| (r / s).max(0.0)).collect();
        (rho, vec![4.0; n], dir * s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nalgebra::{Rotation3, Vector3};

    const SEED: u64 = 0x7452_414e_534c_4154;
    const SEEDS: [u64; 6] = [
        SEED,
        SEED.wrapping_add(0x9E37_79B9_7F4A_7C15),
        SEED.wrapping_add(2_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        SEED.wrapping_add(3_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        SEED.wrapping_add(4_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
        SEED.wrapping_add(5_u64.wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    ];
    struct ScenePair {
        points: Vec<PairPoint>,
        ref_true: f64,
        layer_ratio_true: f64,
    }
    const PX: f64 = 1.0 / 500.0;
    /// Equal thirds put the median inverse depth in the middle layer.
    const LAYERS: [f64; 3] = [2.0, 4.0, 8.0];

    struct Random(u64);
    impl Random {
        fn uniform(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 + 0.5) / ((1_u64 << 53) as f64)
        }
        fn normal(&mut self) -> f64 {
            (-2.0 * self.uniform().ln()).sqrt() * (std::f64::consts::TAU * self.uniform()).cos()
        }
        fn axis(&mut self) -> Vector3<f64> {
            Vector3::new(self.normal(), self.normal(), self.normal()).normalize()
        }
    }

    // Project persistent world points independently of the solver's residual and depth equations.
    fn scene(n_tracks: usize, depths: &[f64], moving_frac: f64, c_true: impl Fn(usize) -> Vector3<f64>,
             gyro_err_px: f64, noise_px: f64, pairs: usize, seed: u64) -> Vec<ScenePair> {
        scene_with_gyro(n_tracks, depths, moving_frac, c_true, |_| gyro_err_px, noise_px, pairs, seed)
    }

    fn scene_with_gyro(n_tracks: usize, depths: &[f64], moving_frac: f64, c_true: impl Fn(usize) -> Vector3<f64>,
                       gyro_err_px: impl Fn(usize) -> f64, noise_px: f64, pairs: usize, seed: u64) -> Vec<ScenePair> {
        let mut rng = Random(seed);
        let columns = ((n_tracks as f64 * 960.0 / 540.0).sqrt().ceil() as usize).max(1);
        let rows = n_tracks.div_ceil(columns);
        let mut world: Vec<Vector3<f64>> = (0..n_tracks).map(|id| {
            let u = (id % columns) as f64 + rng.uniform();
            let v = (id / columns) as f64 + rng.uniform();
            let z = depths[id % depths.len()];
            Vector3::new((u / columns as f64 * 960.0 - 480.0) * z / 500.0,
                         -(v / rows as f64 * 540.0 - 270.0) * z / 500.0, -z)
        }).collect();
        let observe = |world: &[Vector3<f64>], camera: Vector3<f64>, rng: &mut Random| {
            world.iter().map(|point| {
                let point = point - camera;
                let u = 480.0 + 500.0 * point.x / (-point.z) + noise_px * rng.normal();
                let v = 270.0 - 500.0 * point.y / (-point.z) + noise_px * rng.normal();
                Vector3::new((u - 480.0) / 500.0, -(v - 270.0) / 500.0, -1.0).normalize()
            }).collect::<Vec<_>>()
        };
        let mut camera = Vector3::zeros();
        let mut previous = observe(&world, camera, &mut rng);
        let moving_count = (n_tracks as f64 * moving_frac).round() as usize;
        (0..pairs).map(|pair| {
            // Truth uses the first camera center and excludes independently moving points.
            let radial: Vec<_> = world.iter().enumerate().skip(moving_count)
                .map(|(id, point)| (id, 1.0 / (point - camera).norm())).collect();
            let ref_true = median(radial.iter().map(|(_, rho)| *rho).collect());
            let near = median(radial.iter().filter(|(id, _)| id % depths.len() == 0).map(|(_, rho)| *rho).collect());
            let far = median(radial.iter().filter(|(id, _)| id % depths.len() == depths.len() - 1).map(|(_, rho)| *rho).collect());
            let layer_ratio_true = near / far;
            camera += c_true(pair);
            for point in world.iter_mut().take(moving_count) {
                let angle = std::f64::consts::TAU * rng.uniform();
                let step = 2.0 * (camera.z - point.z) / 500.0;
                point.x += step * angle.cos();
                point.y += step * angle.sin();
            }
            let current = observe(&world, camera, &mut rng);
            let gyro_error = Rotation3::from_scaled_axis(rng.axis() * gyro_err_px(pair) * PX);
            let points = previous.iter().zip(&current).enumerate().map(|(id, (p, b))| {
                let row = 500.0 * p.y / p.z + 270.0;
                PairPoint { id: id as u32, band: ((row / 540.0 * 6.0).floor() as i32).clamp(0, 5) as u8,
                            p: *p, r: gyro_error * b - p }
            }).collect();
            previous = current;
            ScenePair { points, ref_true, layer_ratio_true }
        }).collect()
    }

    fn true_move(_: usize) -> Vector3<f64> { Vector3::new(0.01, 0.003, 0.001) }

    fn median(mut values: Vec<f64>) -> f64 {
        if values.is_empty() || values.iter().any(|v| !v.is_finite()) { return f64::NAN; }
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    }

    fn solve(points: &[ScenePair], config: TranslationSolverConfig) -> Vec<PairTranslation> {
        let mut solver = TranslationSolver::new(config);
        points.iter().enumerate().map(|(i, pair)| solver.step(&pair.points, PX, (i + 1) as f64 / 30.0)).collect()
    }

    fn valid(pair: &PairTranslation) -> bool {
        !pair.inv_depth.is_empty() && pair.inv_depth.values().all(|v| v.is_finite())
            && pair.c.iter().chain(pair.c_segment.iter()).all(|v| v.is_finite())
            && pair.ref_inv_depth.is_finite() && pair.confidence.is_finite()
            && pair.track_age_s.is_finite() && pair.pred.is_none_or(f64::is_finite)
    }

    fn reference_px(pair: &PairTranslation) -> f64 {
        pair.c.norm() * median(pair.inv_depth.values().copied().collect()) / PX
    }

    #[derive(Debug)]
    struct Metric {
        test: usize,
        name: &'static str,
        value: f64,
        pass: bool,
    }

    #[derive(Debug)]
    struct Evaluation {
        metrics: Vec<Metric>,
        observations: Vec<WalkingGyroObservation>,
        direction_diagnostics: Vec<(&'static str, f64)>,
    }

    #[derive(Debug)]
    struct WalkingGyroObservation {
        gyro_px: f64,
        coverage: f64,
        rms_ratio: f64,
        mean_confidence: f64,
    }

    impl Evaluation {
        fn add(&mut self, test: usize, name: &'static str, value: f64, pass: bool) {
            self.metrics.push(Metric { test, name, value, pass: pass && value.is_finite() });
        }
        fn all_pass(&self) -> bool { self.metrics.iter().all(|metric| metric.pass) }
        fn test_pass(&self, test: usize) -> bool {
            self.metrics.iter().filter(|metric| metric.test == test).all(|metric| metric.pass)
        }
        fn validity(&mut self, test: usize, pairs: &[PairTranslation]) {
            let invalid = pairs.iter().filter(|pair| !valid(pair)).count();
            self.add(test, "invalid_pairs", invalid as f64, invalid == 0);
        }
        fn safety(&mut self, test: usize, pairs: &[PairTranslation], start: usize) {
            self.validity(test, pairs);
            let mut worst: f64 = 0.0;
            let mut worst_pair = start;
            let mut failures = 0;
            for (i, pair) in pairs.iter().enumerate().skip(start) {
                let motion = reference_px(pair);
                if !valid(pair) || !(pair.confidence == 0.0 || motion <= 0.3) { failures += 1; }
                if pair.confidence != 0.0 && (!motion.is_finite() || motion > worst) {
                    worst = if motion.is_finite() { motion } else { f64::INFINITY };
                    worst_pair = i;
                }
            }
            self.add(test, "unsafe_pairs", failures as f64, failures == 0);
            self.add(test, "worst_claimed_motion_px", worst, worst <= 0.3);
            self.add(test, "worst_pair", worst_pair as f64, true);
        }
    }

    // Tests and the parameter grid share every scene, metric, and acceptance condition.
    fn evaluate(config: TranslationSolverConfig, seed: u64) -> Evaluation {
        let mut out = Evaluation { metrics: Vec::new(), observations: Vec::new(), direction_diagnostics: Vec::new() };
        let points = scene(300, &[2.0, 8.0], 0.1, true_move, 0.0, 0.1, 10, seed);
        let pairs = solve(&points, config);
        out.validity(0, &pairs);
        let last = &pairs[9];
        let pair_angles: Vec<_> = pairs[5..10].iter().enumerate()
            .map(|(i, pair)| pair.c_segment.angle(&true_move(i + 5)).to_degrees()).collect();
        let sum: Vector3<f64> = pairs[5..10].iter().map(|pair| pair.c_segment).sum();
        let truth = true_move(0);
        let sum_angle_deg = sum.angle(&truth).to_degrees();
        let mut azimuth_error_deg = (sum.y.atan2(sum.x) - truth.y.atan2(truth.x)).to_degrees();
        while azimuth_error_deg <= -180.0 { azimuth_error_deg += 360.0; }
        while azimuth_error_deg > 180.0 { azimuth_error_deg -= 360.0; }
        let direction_azimuth_deg = azimuth_error_deg.abs();
        // Direction diagnostics never participate in acceptance or candidate selection.
        for (name, value) in ["pair_angle_deg_5", "pair_angle_deg_6", "pair_angle_deg_7", "pair_angle_deg_8", "pair_angle_deg_9"]
            .into_iter().zip(pair_angles.iter().copied()) {
            out.direction_diagnostics.push((name, value));
        }
        out.direction_diagnostics.push(("median_pair_angle_deg", median(pair_angles)));
        out.direction_diagnostics.push(("sum_angle_deg", sum_angle_deg));
        out.direction_diagnostics.push(("sum_azimuth_error_deg", (sum.y.atan2(sum.x) - truth.y.atan2(truth.x)).to_degrees()));
        out.direction_diagnostics.push(("sum_elevation_error_deg",
            (sum.z.atan2(sum.x.hypot(sum.y)) - truth.z.atan2(truth.x.hypot(truth.y))).to_degrees()));
        let near = median(last.inv_depth.iter().filter(|(id, _)| **id % 2 == 0).map(|(_, rho)| *rho).collect());
        let far = median(last.inv_depth.iter().filter(|(id, _)| **id % 2 == 1).map(|(_, rho)| *rho).collect());
        let ratio = near / far;
        let ratio_true = points[9].layer_ratio_true;
        let ratio_error = (ratio / ratio_true - 1.0).abs();
        out.add(0, "direction_azimuth_deg", direction_azimuth_deg, direction_azimuth_deg <= 2.0);
        out.add(0, "depth_ratio", ratio, true);
        out.add(0, "depth_ratio_true", ratio_true, true);
        out.add(0, "depth_ratio_error", ratio_error, ratio_error <= 0.05);
        out.add(0, "last_confidence", last.confidence, last.confidence > 0.0);

        let pairs = solve(&scene(300, &LAYERS, 0.0, |_| Vector3::zeros(), 0.0, 0.1, 10, seed), config);
        out.safety(1, &pairs, 0);
        let pairs = solve(&scene(300, &LAYERS, 0.0, |_| Vector3::zeros(), 0.5, 0.1, 10, seed), config);
        out.safety(2, &pairs, 0);

        let pairs = solve(&scene(20, &[2.0, 8.0], 0.0, true_move, 0.0, 0.1, 1, seed), config);
        let pair = &pairs[0];
        out.add(3, "few_confidence", pair.confidence, pair.confidence == 0.0);
        out.add(3, "few_c_norm", pair.c.norm(), pair.c == Vector3::zeros());
        out.add(3, "few_c_segment_norm", pair.c_segment.norm(), pair.c_segment == Vector3::zeros());
        out.add(3, "few_depth_count", pair.inv_depth.len() as f64, pair.inv_depth.is_empty());

        let points = scene(300, &LAYERS, 0.0, true_move, 0.0, 0.1, 30, seed);
        let pairs = solve(&points, config);
        out.validity(4, &pairs);
        let mut relative_error_sq: f64 = 0.0;
        let mut max_error: f64 = 0.0;
        let mut error_pair = 2;
        let mut min_confidence: f64 = f64::INFINITY;
        let mut max_age_error: f64 = 0.0;
        let mut segment_errors = 0;
        for (i, pair) in pairs.iter().enumerate() {
            if pair.new_segment != (i == 0) { segment_errors += 1; }
            let age_error = (pair.track_age_s - i as f64 / 30.0).abs();
            max_age_error = if age_error.is_finite() { max_age_error.max(age_error) } else { f64::INFINITY };
            if i >= 2 {
                let truth = 500.0 * true_move(i).norm() * points[i].ref_true;
                let measured = 500.0 * pair.ref_inv_depth * pair.c_segment.norm();
                let error = (measured - truth).abs() / truth;
                relative_error_sq += error.powi(2);
                if !error.is_finite() || error > max_error {
                    max_error = if error.is_finite() { error } else { f64::INFINITY };
                    error_pair = i;
                }
                min_confidence = if pair.confidence.is_finite() { min_confidence.min(pair.confidence) } else { f64::NEG_INFINITY };
            }
        }
        let relative_error_rms = (relative_error_sq / 28.0).sqrt();
        out.add(4, "reference_rms_relative_error", relative_error_rms, relative_error_rms <= 0.03);
        out.add(4, "reference_max_relative_error", max_error, max_error <= 0.05);
        out.add(4, "reference_worst_pair", error_pair as f64, true);
        out.add(4, "reference_min_confidence", min_confidence, min_confidence > 0.0);
        out.add(4, "reference_segment_errors", segment_errors as f64, segment_errors == 0);
        out.add(4, "reference_max_age_error_s", max_age_error, max_age_error < 1e-12);

        let motion = |k: usize| Vector3::new(0.016 * (std::f64::consts::TAU * 2.0 * k as f64 / 30.0).sin(), 0.0, 0.0);
        let points = scene(300, &LAYERS, 0.0, motion, 0.0, 0.1, 60, seed);
        let pairs = solve(&points, config);
        out.validity(5, &pairs);
        let mut count = 0;
        let (mut error_sq, mut truth_sq) = (0.0, 0.0);
        for (i, pair) in pairs.iter().enumerate().skip(2) {
            let truth = 500.0 * motion(i).x * points[i].ref_true;
            let measured = 500.0 * pair.ref_inv_depth * pair.c_segment.x;
            if pair.confidence > 0.0 {
                count += 1;
                error_sq += (measured - truth).powi(2);
                truth_sq += truth.powi(2);
            }
        }
        let coverage = count as f64 / 58.0;
        let error_rms = (error_sq / count as f64).sqrt();
        let truth_rms = (truth_sq / count as f64).sqrt();
        let rms_ratio = error_rms / truth_rms;
        out.add(5, "walking_coverage", coverage, coverage >= 0.8);
        out.add(5, "walking_count", count as f64, count > 0);
        out.add(5, "walking_error_rms", error_rms, true);
        out.add(5, "walking_truth_rms", truth_rms, truth_rms > 0.0);
        out.add(5, "walking_rms_ratio", rms_ratio, rms_ratio <= 0.05);

        let pairs = solve(&scene(300, &LAYERS, 0.0, true_move, 0.0, 0.1, 10, seed), config);
        out.validity(6, &pairs);
        let starts: Vec<_> = pairs.iter().filter(|pair| pair.new_segment).collect();
        let confidence = starts.iter().map(|pair| pair.confidence.abs()).fold(0.0_f64, f64::max);
        let pred_count = starts.iter().filter(|pair| pair.pred.is_some()).count();
        out.add(6, "segment_start_count", starts.len() as f64, !starts.is_empty());
        out.add(6, "segment_start_max_confidence", confidence, confidence == 0.0);
        out.add(6, "segment_start_pred_count", pred_count as f64, pred_count == 0);

        let points = scene_with_gyro(300, &LAYERS, 0.0,
            |k| if k < 10 { true_move(k) } else { Vector3::zeros() },
            |k| if k < 10 { 0.0 } else { 0.5 }, 0.1, 20, seed);
        let pairs = solve(&points, config);
        let rejected = (10..18).find(|&i| pairs[i].confidence == 0.0
            && pairs[i].pred.is_some_and(|pred| pred.is_finite() && pred > config.pred_max));
        out.add(7, "fake_rejected_pair", rejected.map_or(-1.0, |i| i as f64), rejected.is_some());
        out.add(7, "fake_rejection_pred", rejected.and_then(|i| pairs[i].pred).unwrap_or(f64::NAN), rejected.is_some());
        out.safety(7, &pairs, rejected.map_or(10, |i| i + 1));

        // These observations never participate in acceptance or candidate selection.
        for gyro_px in [0.2, 0.5] {
            let points = scene(300, &LAYERS, 0.0, motion, gyro_px, 0.1, 60, seed);
            let pairs = solve(&points, config);
            let mut count = 0;
            let (mut error_sq, mut truth_sq, mut confidence_sum) = (0.0, 0.0, 0.0);
            for (i, pair) in pairs.iter().enumerate().skip(2) {
                confidence_sum += pair.confidence;
                if pair.confidence > 0.0 {
                    count += 1;
                    let truth = 500.0 * motion(i).x * points[i].ref_true;
                    let measured = 500.0 * pair.ref_inv_depth * pair.c_segment.x;
                    error_sq += (measured - truth).powi(2);
                    truth_sq += truth.powi(2);
                }
            }
            out.observations.push(WalkingGyroObservation { gyro_px, coverage: count as f64 / 58.0,
                rms_ratio: (error_sq / truth_sq).sqrt(), mean_confidence: confidence_sum / 58.0 });
        }
        out
    }

    fn check(test: usize) {
        let mut failures = Vec::new();
        // Cache complete evaluations only; all eight tests still inspect all six seeds.
        static DEFAULT_EVALUATIONS: OnceLock<Vec<Evaluation>> = OnceLock::new();
        let evaluations = DEFAULT_EVALUATIONS.get_or_init(||
            SEEDS.iter().map(|&seed| evaluate(TranslationSolverConfig::DEFAULT, seed)).collect());
        for (seed, evaluation) in SEEDS.into_iter().zip(evaluations) {
            let metrics: Vec<_> = evaluation.metrics.iter().filter(|metric| metric.test == test).collect();
            eprintln!("test={test} seed={seed:#018x} metrics={metrics:?}");
            if !evaluation.test_pass(test) { failures.push(format!("seed={seed:#018x} metrics={metrics:?}")); }
        }
        assert!(failures.is_empty(), "test={test}\n{}", failures.join("\n"));
    }

    #[test]
    fn recovers_direction_and_depth_ratios() { check(0); }
    #[test]
    fn zero_translation_claims_nothing() { check(1); }
    #[test]
    fn gyro_error_is_not_taken_for_translation() { check(2); }
    #[test]
    fn too_few_points_have_no_confidence() { check(3); }
    #[test]
    fn reference_layer_motion_holds_while_tracks_continue() { check(4); }
    #[test]
    fn walking_translation_is_recovered() { check(5); }
    #[test]
    fn segment_starts_claim_nothing() { check(6); }
    #[test]
    fn prediction_check_rejects_a_learned_fake_structure() { check(7); }

    #[test]
    #[ignore]
    fn translation_parameter_grid() -> std::io::Result<()> {
        use std::io::Write;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
            .join("target/translation-execution/task-7-r5-grid.csv");
        let mut file = std::io::BufWriter::new(std::fs::File::create_new(&path)?);
        let mut header = false;
        for yaw_pitch_prior_px in [0.05, 0.1] {
            for roll_prior_px in [0.5, 0.05] {
                let config = TranslationSolverConfig { yaw_pitch_prior_px, roll_prior_px, pred_max: 0.8 };
                let mut all_pass = true;
                let mut worst_coverage = [f64::INFINITY; 2];
                let mut worst_rms_ratio = [0.0_f64; 2];
                for seed in SEEDS {
                    let evaluation = evaluate(config, seed);
                    if !header {
                        write!(file, "yaw_pitch_prior_px,roll_prior_px,pred_max,seed")?;
                        for metric in &evaluation.metrics {
                            write!(file, ",t{}_{}_value,t{}_{}_pass", metric.test, metric.name, metric.test, metric.name)?;
                        }
                        for observation in &evaluation.observations {
                            write!(file, ",walking_gyro_{}_coverage,walking_gyro_{}_rms_ratio,walking_gyro_{}_mean_confidence",
                                observation.gyro_px, observation.gyro_px, observation.gyro_px)?;
                        }
                        for (name, _) in &evaluation.direction_diagnostics { write!(file, ",{name}")?; }
                        writeln!(file, ",all_pass")?;
                        header = true;
                    }
                    write!(file, "{yaw_pitch_prior_px},{roll_prior_px},{},{seed:#018x}", config.pred_max)?;
                    for metric in &evaluation.metrics { write!(file, ",{},{}", metric.value, metric.pass)?; }
                    for (i, observation) in evaluation.observations.iter().enumerate() {
                        write!(file, ",{},{},{}", observation.coverage, observation.rms_ratio, observation.mean_confidence)?;
                        worst_coverage[i] = worst_coverage[i].min(observation.coverage);
                        worst_rms_ratio[i] = if observation.rms_ratio.is_finite() {
                            worst_rms_ratio[i].max(observation.rms_ratio)
                        } else { f64::INFINITY };
                    }
                    for (_, value) in &evaluation.direction_diagnostics { write!(file, ",{value}")?; }
                    writeln!(file, ",{}", evaluation.all_pass())?;
                    file.flush()?;
                    all_pass &= evaluation.all_pass();
                }
                eprintln!("grid config={config:?} all_pass={all_pass} walking_gyro_px=[0.2,0.5] worst_coverage={worst_coverage:?} worst_rms_ratio={worst_rms_ratio:?}");
            }
        }
        Ok(())
    }
}
