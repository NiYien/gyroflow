// SPDX-License-Identifier: GPL-3.0-or-later

//! The rotation the picture shows beyond the motion data, the in-camera stabilization reconstruction and the dominant
//! layer of the translation analysis, for files with motion data (openspec change fused-residual-rotation).
//!
//! Each tracked point of a frame pair, ray `p` where the motion data put it in the second frame and residual `r`
//! against where it was, sees `r ≈ (R_bᵀ·(δ(t_a) - δ(t_b))) × p` with δ a world-frame rotation error at each point's
//! own readout times. What the measurement constrains is the sum of two processes on a 5 ms grid: δ, the bounded error
//! of the motion data (an Ornstein-Uhlenbeck process per world axis), and η, the slow smooth error of the picture model
//! (parallax left over, scale or lens errors; a second-difference prior). They separate by their spectra; only δ is
//! applied. How strong δ's prior is comes from the tracks themselves: the estimate from one half of them has to
//! reduce the motion the other half shows in the current render, or none is taken (strength zero).
//!
//! The numerics follow the offline prototype validated in target/unified-path-lab (a3/a4_estimate.py); the fixture
//! `fixtures/residual_rotation_fixture.bin` holds that reference (`fixtures/make_residual_rotation_fixture.py`)

use nalgebra::{ Matrix3, Vector3 };
use super::solver::BandSym;
#[cfg(test)]
use nalgebra::UnitQuaternion;

/// Grid spacing of δ and η, milliseconds
pub const GRID_MS: f64 = 5.0;
/// Floor of a point's per-pair noise, tracked pixels
pub const NOISE_FLOOR_PX: f64 = 0.3;
/// Cauchy scale of the robust reweighting, in units of the noise
const ROBUST_K: f64 = 2.5;
/// Step variance of η's second difference, rad² per grid step
const Q_ETA: f64 = 1e-12;
/// The strength grid: δ's standard deviation (degrees) and correlation time (seconds)
pub const SIGMAS_DEG: [f64; 8] = [0.0, 0.005, 0.01, 0.02, 0.05, 0.1, 0.2, 0.5];
pub const TAUS_S: [f64; 4] = [0.03, 0.1, 0.3, 1.0];
/// Bands the held-out motion is scored in, Hz (the upper edge at most 0.45 of the frame rate)
const BANDS_HZ: [(f64, f64); 3] = [(0.4, 3.0), (1.0, 3.0), (3.0, 10.0)];
/// Roll is priced at the half diagonal of a 960×540 picture
const ROLL_LEVER_PX: f64 = 550.7267925205461; // hypot(480, 270)
/// Equal-area cells per frame pair
pub const CELLS: (usize, usize) = (16, 9);

/// One tracked point of a frame pair, measured with the reconstruction that applies
#[derive(Clone, Copy, Debug)]
pub struct ResidualPoint {
    /// Index of its frame pair; points of a pair are consecutive
    pub pair: u32,
    /// Which half of the tracks (track id parity)
    pub half: u8,
    /// Its equal-area cell in the first frame, row-major over `CELLS`
    pub cell: u16,
    /// Readout times in the two frames, video milliseconds
    pub ta_ms: f64,
    pub tb_ms: f64,
    /// Where the motion data put it in the second frame (unit ray, quaternion frame), and how far off that was
    pub p: Vector3<f64>,
    pub r: Vector3<f64>,
    /// Camera to world in the second frame, at `tb_ms`
    pub rb: Matrix3<f64>,
}

/// A dominant-layer sample of the translation analysis: what the layer moved in the image (already times its gain)
#[derive(Clone, Copy, Debug)]
pub struct DominantSample {
    pub time_ms: f64,
    pub segment: u32,
    pub motion: [f64; 2],
    pub camera_to_world: Matrix3<f64>,
}

/// The cumulative world-frame rotation-equivalent of the dominant layer (pitch = -m_y, yaw = -m_x), held across a
/// segment break, at `times_ms` (clamped at the ends)
pub fn dominant_chain(samples: &[DominantSample], times_ms: &[f64]) -> Vec<Vector3<f64>> {
    if samples.is_empty() { return vec![Vector3::zeros(); times_ms.len()]; }
    let mut chain = vec![Vector3::zeros(); samples.len()];
    for i in 1..samples.len() {
        chain[i] = chain[i - 1];
        if samples[i].segment == samples[i - 1].segment {
            let om = Vector3::new(-samples[i].motion[1], -samples[i].motion[0], 0.0);
            chain[i] -= samples[i].camera_to_world * om;
        }
    }
    let t: Vec<f64> = samples.iter().map(|s| s.time_ms).collect();
    times_ms.iter().map(|&x| Vector3::from_fn(|c, _| interp(x, &t, |i| chain[i][c]))).collect()
}

/// `np.interp`: linear inside, the end values outside. `xs` increasing
pub fn interp(x: f64, xs: &[f64], ys: impl Fn(usize) -> f64) -> f64 {
    let n = xs.len();
    if n == 0 { return 0.0; }
    if x <= xs[0] { return ys(0); }
    if x >= xs[n - 1] { return ys(n - 1); }
    let i = xs.partition_point(|v| *v <= x).clamp(1, n - 1);
    let (x0, x1) = (xs[i - 1], xs[i]);
    if x1 <= x0 { return ys(i); }
    let f = (x - x0) / (x1 - x0);
    ys(i - 1) + (ys(i) - ys(i - 1)) * f
}

/// The measurement on the grid: points, their interpolation terms, residuals after the dominant layer, weights
pub struct ResidualProblem {
    pub t0_ms: f64,
    pub nodes: usize,
    pub points: Vec<ResidualPoint>,
    terms: Vec<([usize; 4], [f64; 4])>,
    /// Residuals the estimate explains: `r` minus the dominant layer's part
    pub res: Vec<Vector3<f64>>,
    /// Equal-area weight of each point, mean one
    pub area: Vec<f64>,
    /// Noise floor in radians
    pub floor: f64,
    /// Band width of the assembled system
    pub u: usize,
    /// Projector `R_b (I - ppᵀ) R_bᵀ` per point
    pw: Vec<Matrix3<f64>>,
    /// First point of each pair, and the end
    bounds: Vec<usize>,
}

impl ResidualProblem {
    /// `points` sorted by pair; `focal_px` in tracked pixels
    pub fn new(points: Vec<ResidualPoint>, focal_px: f64) -> Option<Self> {
        if points.is_empty() { return None; }
        let h = GRID_MS;
        let t0_ms = points.iter().map(|p| p.ta_ms).fold(f64::INFINITY, f64::min) - 2.0 * h;
        let tmax = points.iter().map(|p| p.tb_ms).fold(f64::NEG_INFINITY, f64::max);
        let nodes = ((tmax - t0_ms) / h).ceil() as usize + 3;
        let terms: Vec<([usize; 4], [f64; 4])> = points.iter().map(|p| {
            let (ua, ub) = ((p.ta_ms - t0_ms) / h, (p.tb_ms - t0_ms) / h);
            let (ja, jb) = (ua.floor(), ub.floor());
            let (fa, fb) = (ua - ja, ub - jb);
            let (ja, jb) = (ja as usize, jb as usize);
            ([ja, ja + 1, jb, jb + 1], [1.0 - fa, fa, -(1.0 - fb), -fb])
        }).collect();
        let span = terms.iter().map(|(n, _)| n.iter().max().unwrap() - n.iter().min().unwrap()).max().unwrap_or(0) + 1;
        let u = 6 * span.max(3) + 5;
        let mut bounds = vec![0];
        for i in 1..points.len() { if points[i].pair != points[i - 1].pair { bounds.push(i); } }
        bounds.push(points.len());
        // Equal-area weights: each point 1 / (points of its cell in its pair)
        let mut area = vec![0.0; points.len()];
        for k in 0..bounds.len() - 1 {
            let mut count = std::collections::HashMap::<u16, usize>::new();
            for p in &points[bounds[k]..bounds[k + 1]] { *count.entry(p.cell).or_default() += 1; }
            for i in bounds[k]..bounds[k + 1] { area[i] = 1.0 / count[&points[i].cell] as f64; }
        }
        let mean = area.iter().sum::<f64>() / area.len() as f64;
        for a in &mut area { *a /= mean; }
        let pw = points.iter().map(|p| {
            let proj = Matrix3::identity() - p.p * p.p.transpose();
            p.rb * proj * p.rb.transpose()
        }).collect();
        let res = points.iter().map(|p| p.r).collect();
        Some(Self { t0_ms, nodes, points, terms, res, area, floor: NOISE_FLOOR_PX / focal_px, u, pw, bounds })
    }

    pub fn grid_ms(&self, k: usize) -> f64 { self.t0_ms + k as f64 * GRID_MS }

    /// Takes the dominant layer's motion out: `d_dom` its cumulative rotation-equivalent at the grid nodes
    pub fn subtract_dominant(&mut self, d_dom: &[Vector3<f64>]) {
        for (i, p) in self.points.iter().enumerate() {
            let (n, c) = self.terms[i];
            let delta: Vector3<f64> = (0..4).map(|a| d_dom[n[a]] * c[a]).sum();
            self.res[i] -= (p.rb.transpose() * delta).cross(&p.p);
        }
    }

    /// Weights before any reweighting: equal area over the noise floor
    pub fn initial_weights(&self) -> Vec<f64> {
        self.area.iter().map(|a| a / (self.floor * self.floor)).collect()
    }

    /// Information on [δ, η] (6 per node, node-major) of the points `use_point` selects, and its right-hand side
    pub(super) fn assemble(&self, w: &[f64], use_point: impl Fn(&ResidualPoint) -> bool) -> (BandSym, Vec<f64>) {
        let n = 6 * self.nodes;
        let mut ab = BandSym::new(n, self.u);
        let mut rhs = vec![0.0; n];
        for (i, p) in self.points.iter().enumerate() {
            if !use_point(p) || w[i] == 0.0 { continue; }
            let (nodes, coef) = self.terms[i];
            let gw = p.rb * p.p.cross(&self.res[i]) * w[i];
            let wp = self.pw[i] * w[i];
            for a in 0..4 {
                for s in [0, 3] {
                    for c in 0..3 { rhs[6 * nodes[a] + s + c] += coef[a] * gw[c]; }
                }
                for b in 0..4 {
                    let (na, nb) = (nodes[a], nodes[b]);
                    if na > nb { continue; }
                    let cc = coef[a] * coef[b];
                    for si in [0, 3] {
                        for sj in [0, 3] {
                            for ii in 0..3 {
                                for jj in 0..3 {
                                    let (row, col) = (6 * na + si + ii, 6 * nb + sj + jj);
                                    if row <= col { ab.add(col, row, cc * wp[(ii, jj)]); }
                                }
                            }
                        }
                    }
                }
            }
        }
        (ab, rhs)
    }

    /// The prior added to an assembled system: δ an Ornstein-Uhlenbeck process (σ rad, τ s), η a second difference
    /// random walk with a weak anchor on its first two nodes
    fn add_prior(&self, ab: &mut BandSym, sigma: f64, tau_s: f64) {
        let g = self.nodes;
        let phi = (-GRID_MS / 1000.0 / tau_s).exp();
        let qd = sigma * sigma * (1.0 - phi * phi);
        for k in 0..g {
            let dd = if k == 0 || k == g - 1 { 1.0 / qd } else { (1.0 + phi * phi) / qd };
            for ax in 0..3 {
                ab.add(6 * k + ax, 6 * k + ax, dd);
                if k + 1 < g { ab.add(6 * (k + 1) + ax, 6 * k + ax, -phi / qd); }
            }
        }
        // D2ᵀD2 by its three diagonals
        let (mut g0, mut g1) = (vec![0.0; g], vec![0.0; g.saturating_sub(1)]);
        for r in 0..g.saturating_sub(2) {
            g0[r] += 1.0; g0[r + 1] += 4.0; g0[r + 2] += 1.0;
            g1[r] += -2.0; g1[r + 1] += -2.0;
        }
        for v in g0.iter_mut().take(2) { *v += 1e-6; }
        for k in 0..g {
            for ax in 0..3 {
                let i = 6 * k + 3 + ax;
                ab.add(i, i, g0[k] / Q_ETA);
                if k + 1 < g { ab.add(6 * (k + 1) + 3 + ax, i, g1[k] / Q_ETA); }
                if k + 2 < g { ab.add(6 * (k + 2) + 3 + ax, i, 1.0 / Q_ETA); }
            }
        }
    }

    /// δ and η per node for a prior strength σ > 0. None when the system is singular
    fn solve(&self, assembled: &(BandSym, Vec<f64>), sigma: f64, tau_s: f64) -> Option<(Vec<Vector3<f64>>, Vec<Vector3<f64>>)> {
        let mut ab = assembled.0.clone();
        self.add_prior(&mut ab, sigma, tau_s);
        let x = ab.solve(assembled.1.clone())?;
        let d = (0..self.nodes).map(|k| Vector3::new(x[6 * k], x[6 * k + 1], x[6 * k + 2])).collect();
        let eta = (0..self.nodes).map(|k| Vector3::new(x[6 * k + 3], x[6 * k + 4], x[6 * k + 5])).collect();
        Some((d, eta))
    }

    /// δ per node for a prior strength, zero for σ = 0. None when the system is singular
    fn solve_delta(&self, assembled: &(BandSym, Vec<f64>), sigma: f64, tau_s: f64) -> Option<Vec<Vector3<f64>>> {
        if sigma <= 0.0 { return Some(vec![Vector3::zeros(); self.nodes]); }
        Some(self.solve(assembled, sigma, tau_s)?.0)
    }

    /// The final estimate from every point: solved, robustly reweighted once (Cauchy, per pair scale) from what δ + η
    /// leave of each point, solved again. Zero for σ = 0
    pub fn estimate(&self, sigma: f64, tau_s: f64) -> Option<Vec<Vector3<f64>>> {
        if sigma <= 0.0 { return Some(vec![Vector3::zeros(); self.nodes]); }
        let mut w = self.initial_weights();
        let (d, eta) = self.solve(&self.assemble(&w, |_| true), sigma, tau_s)?;
        let total: Vec<Vector3<f64>> = d.iter().zip(&eta).map(|(a, b)| a + b).collect();
        let en: Vec<f64> = self.points.iter().enumerate().map(|(i, p)| {
            let (n, c) = self.terms[i];
            let delta: Vector3<f64> = (0..4).map(|a| total[n[a]] * c[a]).sum();
            (self.res[i] - (p.rb.transpose() * delta).cross(&p.p)).norm()
        }).collect();
        for k in 0..self.bounds.len() - 1 {
            let s = self.bounds[k]..self.bounds[k + 1];
            let sigma_k = (1.4826 * median(&en[s.clone()])).max(self.floor);
            for i in s {
                w[i] = self.area[i] / (sigma_k * sigma_k * (1.0 + (en[i] / (ROBUST_K * sigma_k)).powi(2)));
            }
        }
        Some(self.solve(&self.assemble(&w, |_| true), sigma, tau_s)?.0)
    }
}

/// `np.median`
pub fn median(x: &[f64]) -> f64 {
    if x.is_empty() { return 0.0; }
    let mut v = x.to_vec();
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n % 2 == 1 { v[n / 2] } else { 0.5 * (v[n / 2 - 1] + v[n / 2]) }
}

/// The motion one half of the tracks shows in the current render: cumulative pitch, yaw (the picture centre over the
/// output focal length) and roll, radians, at the times of a uniformly sampled series
pub struct HeldOutMotion {
    pub times_ms: Vec<f64>,
    pub out: Vec<[f64; 3]>,
    pub fps: f64,
}

/// Per band (0.4-3, 1-3, 3-10 Hz) and axis, the RMS of what is left of `held` once δ's local representation is taken
/// out, in pixels (`focal_960` for pitch and yaw, the half diagonal for roll). `orientation` is camera to world at
/// each grid node. None when the series is too short to filter
pub fn held_out_left(d: &[Vector3<f64>], orientation: &[Matrix3<f64>], grid_t0_ms: f64, held: &HeldOutMotion, focal_960: f64) -> Option<[[f64; 3]; 3]> {
    let n = held.times_ms.len();
    if n < 2 || d.is_empty() { return None; }
    let grid_t: Vec<f64> = (0..d.len()).map(|k| grid_t0_ms + k as f64 * GRID_MS).collect();
    let local: Vec<Vector3<f64>> = d.iter().zip(orientation).map(|(dk, r)| r.transpose() * (dk - d[0])).collect();
    let scale = [focal_960, focal_960, ROLL_LEVER_PX];
    let t_first = held.times_ms[0];
    let t_last = held.times_ms[n - 1];
    let keep: Vec<bool> = held.times_ms.iter().map(|t| *t > t_first + 1000.0 && *t < t_last - 1000.0).collect();
    if !keep.iter().any(|k| *k) { return None; }
    let mut out = [[0.0; 3]; 3];
    for c in 0..3 {
        let left: Vec<f64> = (0..n).map(|i| {
            let dl = interp(held.times_ms[i], &grid_t, |k| local[k][c]);
            (held.out[i][c] - dl) * scale[c]
        }).collect();
        for (b, (lo, hi)) in BANDS_HZ.iter().enumerate() {
            let filtered = bandpass(&left, *lo, hi.min(0.45 * held.fps), held.fps)?;
            let (mut s, mut m) = (0.0, 0);
            for (v, k) in filtered.iter().zip(&keep) { if *k { s += v * v; m += 1; } }
            out[b][c] = (s / m as f64).sqrt();
        }
    }
    Some(out)
}

/// What the strength selection found
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub sigma_deg: f64,
    pub tau_s: f64,
    /// (σ degrees, τ s, mean log ratio to no correction over both directions)
    pub scores: Vec<(f64, f64, f64)>,
}

/// The strength grid in the order it's scored: zero, then every σ > 0 for each τ
pub fn strength_grid() -> Vec<(f64, f64)> {
    let mut grid = vec![(0.0, TAUS_S[0])];
    for tau in TAUS_S { for s in &SIGMAS_DEG[1..] { grid.push((*s, tau)); } }
    grid
}

/// Selects δ's prior: half A's estimate scored on half B's held-out motion and the other way round, averaged; the
/// smallest mean log ratio to no correction wins, ties to the smaller σ; nothing better than zero selects zero
pub fn select_strength(problem: &ResidualProblem, held: [&HeldOutMotion; 2], orientation: &[Matrix3<f64>], focal_960: f64,
    cancelled: &dyn Fn() -> bool) -> Option<Selection> {
    let w = problem.initial_weights();
    let assembled = [problem.assemble(&w, |p| p.half == 0), problem.assemble(&w, |p| p.half == 1)];
    let zero = vec![Vector3::zeros(); problem.nodes];
    let reference = [held_out_left(&zero, orientation, problem.t0_ms, held[0], focal_960)?,
                     held_out_left(&zero, orientation, problem.t0_ms, held[1], focal_960)?];
    // A half none of whose pairs could be fitted shows no motion to take a ratio to
    if reference.iter().flatten().flatten().any(|v| !(*v > 0.0)) { return None; }
    let mut scores = Vec::new();
    for (sig_deg, tau) in strength_grid() {
        if cancelled() { return None; }
        let mut total = 0.0;
        // (estimated from, scored on)
        for (est, on) in [(1usize, 0usize), (0, 1)] {
            let d = problem.solve_delta(&assembled[est], sig_deg.to_radians(), tau)?;
            let left = held_out_left(&d, orientation, problem.t0_ms, held[on], focal_960)?;
            let mut s = 0.0;
            for b in 0..3 { for c in 0..3 { s += (left[b][c] / reference[on][b][c]).ln(); } }
            total += s / 9.0;
        }
        scores.push((sig_deg, tau, total / 2.0));
    }
    let best = scores.iter().copied().fold(None::<(f64, f64, f64)>, |best, s| match best {
        Some(b) if (b.2, b.0, b.1) <= (s.2, s.0, s.1) => Some(b),
        _ => Some(s),
    })?;
    Some(Selection { sigma_deg: best.0, tau_s: best.1, scores })
}

/// The registered similarity fit of matched points `a` → `b` about `center` (Cauchy, six passes), each point counting
/// `prior` times: rotation (rad), translation x and y
pub fn fit_similarity(a: &[(f64, f64)], b: &[(f64, f64)], center: (f64, f64), prior: &[f64]) -> [f64; 3] {
    use num::complex::Complex64 as C;
    let z: Vec<C> = a.iter().map(|p| C::new(p.0 - center.0, p.1 - center.1)).collect();
    let q: Vec<C> = b.iter().map(|p| C::new(p.0 - center.0, p.1 - center.1)).collect();
    let mut w = prior.to_vec();
    let (mut gain, mut off) = (C::new(1.0, 0.0), C::new(0.0, 0.0));
    for _ in 0..6 {
        let sw: f64 = w.iter().sum();
        let za = z.iter().zip(&w).map(|(z, w)| z * w).sum::<C>() / sw;
        let qb = q.iter().zip(&w).map(|(q, w)| q * w).sum::<C>() / sw;
        let num: C = z.iter().zip(&q).zip(&w).map(|((z, q), w)| (z - za).conj() * (q - qb) * w).sum();
        let den: f64 = z.iter().zip(&w).map(|(z, w)| (z - za).norm_sqr() * w).sum();
        gain = num / den;
        off = qb - gain * za;
        let res: Vec<f64> = z.iter().zip(&q).map(|(z, q)| (q - (gain * z + off)).norm()).collect();
        let sc = (1.4826 * median(&res)).max(0.3);
        for ((w, r), p) in w.iter_mut().zip(&res).zip(prior) { *w = p / (1.0 + (r / sc).powi(2)); }
    }
    [gain.arg(), off.re, off.im]
}

/// `cum` of the lab scripts: the path of per-pair rates, NaN rates replaced by linear interpolation of the valid ones
/// (end values outside), one longer than `rates`
pub fn cumulative(rates: &[f64]) -> Vec<f64> {
    let valid: Vec<usize> = (0..rates.len()).filter(|&i| rates[i].is_finite()).collect();
    let mut out = vec![0.0; rates.len() + 1];
    if valid.is_empty() { return out; }
    let xs: Vec<f64> = valid.iter().map(|&i| i as f64).collect();
    for i in 0..rates.len() {
        let v = if rates[i].is_finite() { rates[i] } else { interp(i as f64, &xs, |k| rates[valid[k]]) };
        out[i + 1] = out[i] + v;
    }
    out
}

// ---- Butterworth band pass, zero phase: scipy.signal butter(3, [lo, hi], 'bandpass', output='sos') + sosfiltfilt ----

type Sos = Vec<[f64; 6]>;

fn butter_bandpass(lo: f64, hi: f64, fs: f64) -> Option<Sos> {
    use num::complex::Complex64 as C;
    const N: usize = 3;
    if !(lo > 0.0 && hi > lo && hi < fs / 2.0) { return None; }
    // Analog prototype, pre-warped edges (fs = 2 after normalising by Nyquist)
    let proto: Vec<C> = [-2.0f64, 0.0, 2.0].iter().map(|m| -(C::new(0.0, std::f64::consts::PI * m / (2.0 * N as f64))).exp()).collect();
    let warp = |f: f64| 4.0 * (std::f64::consts::PI * (f / (fs / 2.0)) / 2.0).tan();
    let (w1, w2) = (warp(lo), warp(hi));
    let bw = w2 - w1;
    let wo = (w1 * w2).sqrt();
    let mut poles = Vec::with_capacity(2 * N);
    for p in &proto {
        let pl = p * bw / 2.0;
        let root = (pl * pl - wo * wo).sqrt();
        poles.push(pl + root);
        poles.push(pl - root);
    }
    let k_bp = bw.powi(N as i32);
    // Bilinear: N zeros at +1 (the band pass zeros at s = 0) and N at -1
    let pz: Vec<C> = poles.iter().map(|p| (4.0 + p) / (4.0 - p)).collect();
    let den: C = poles.iter().map(|p| 4.0 - p).product();
    let k = k_bp * (C::new(4.0f64.powi(N as i32), 0.0) / den).re;
    // Sections of two poles each - a conjugate pair, or two real ones (a wide band's real prototype pole splits into
    // two real poles) - with one zero at +1 and one at -1. Any pairing gives the same cascade
    let real_tol = |p: &C| p.im.abs() <= 1e-12 * p.norm().max(1.0);
    let mut den: Vec<[f64; 2]> = pz.iter().filter(|p| !real_tol(p) && p.im > 0.0).map(|p| [-2.0 * p.re, p.norm_sqr()]).collect();
    let mut reals: Vec<f64> = pz.iter().filter(|p| real_tol(p)).map(|p| p.re).collect();
    reals.sort_by(|a, b| a.total_cmp(b));
    for r in reals.chunks(2) {
        if r.len() != 2 { return None; }
        den.push([-(r[0] + r[1]), r[0] * r[1]]);
    }
    if den.len() != N { return None; }
    Some(den.iter().enumerate().map(|(i, a)| {
        let g = if i == 0 { k } else { 1.0 };
        [g, 0.0, -g, 1.0, a[0], a[1]]
    }).collect())
}

fn sosfilt(sos: &Sos, x: &[f64], zi: &[[f64; 2]]) -> Vec<f64> {
    let mut z: Vec<[f64; 2]> = zi.to_vec();
    x.iter().map(|&v| {
        let mut x = v;
        for (s, z) in sos.iter().zip(z.iter_mut()) {
            let y = s[0] * x + z[0];
            z[0] = s[1] * x - s[4] * y + z[1];
            z[1] = s[2] * x - s[5] * y;
            x = y;
        }
        x
    }).collect()
}

fn sosfilt_zi(sos: &Sos) -> Vec<[f64; 2]> {
    let mut scale = 1.0;
    sos.iter().map(|s| {
        let (b0, b1, b2, a1, a2) = (s[0], s[1], s[2], s[4], s[5]);
        let z1 = (b1 + b2 - b0 * (a1 + a2)) / (1.0 + a1 + a2);
        let z2 = b2 - a2 * b0 - a2 * z1;
        let zi = [scale * z1, scale * z2];
        scale *= (b0 + b1 + b2) / (1.0 + a1 + a2);
        zi
    }).collect()
}

/// The registered band filter: third-order Butterworth band pass, zero phase, odd extension as scipy pads
pub fn bandpass(x: &[f64], lo: f64, hi: f64, fs: f64) -> Option<Vec<f64>> {
    let sos = butter_bandpass(lo, hi, fs)?;
    let pad = 3 * (2 * sos.len() + 1);
    let n = x.len();
    if n <= pad { return None; }
    let mut ext = Vec::with_capacity(n + 2 * pad);
    for i in (1..=pad).rev() { ext.push(2.0 * x[0] - x[i]); }
    ext.extend_from_slice(x);
    for i in 1..=pad { ext.push(2.0 * x[n - 1] - x[n - 1 - i]); }
    let zi = sosfilt_zi(&sos);
    let scaled = |v: f64| zi.iter().map(|z| [z[0] * v, z[1] * v]).collect::<Vec<_>>();
    let y = sosfilt(&sos, &ext, &scaled(ext[0]));
    let mut rev: Vec<f64> = y.into_iter().rev().collect();
    rev = sosfilt(&sos, &rev, &scaled(rev[0]));
    rev.reverse();
    Some(rev[pad..pad + n].to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        points: Vec<ResidualPoint>,
        samples: Vec<DominantSample>,
        grid_t: Vec<f64>,
        orientation: Vec<Matrix3<f64>>,
        tf: Vec<f64>,
        out: [Vec<[f64; 3]>; 2],
        out_noise: [Vec<[f64; 3]>; 2],
        d_dom: Vec<Vector3<f64>>,
        res: Vec<Vector3<f64>>,
        area: Vec<f64>,
        selected: [f64; 5],
        table: Vec<f64>,
        table_noise: Vec<f64>,
        d: Vec<Vector3<f64>>,
        band_in: Vec<[f64; 3]>,
        band_out: Vec<[f64; 9]>,
        t0: f64,
        focal: f64,
        fps: f64,
        floor: f64,
    }

    fn fixture() -> Fixture {
        let bytes = include_bytes!("fixtures/residual_rotation_fixture.bin");
        assert_eq!(&bytes[..8], b"RESROT01");
        let u32at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap()) as usize;
        let (np, g, frames, nt, nb) = (u32at(8), u32at(12), u32at(16), u32at(20), u32at(24));
        let f64at = |o: usize| f64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
        let (t0, focal, fps, floor) = (f64at(28), f64at(36), f64at(44), f64at(52));
        let mut off = 60;
        let mut take = |n: usize| -> Vec<f64> { let v = (0..n).map(|i| f64at(off + 8 * i)).collect(); off += 8 * n; v };
        let p = take(np * 15);
        let points = p.chunks(15).map(|c| {
            let q = UnitQuaternion::from_quaternion(nalgebra::Quaternion::new(c[11], c[12], c[13], c[14]));
            ResidualPoint { pair: c[0] as u32, half: c[1] as u8, cell: c[2] as u16, ta_ms: c[3], tb_ms: c[4],
                p: Vector3::new(c[5], c[6], c[7]), r: Vector3::new(c[8], c[9], c[10]), rb: *q.to_rotation_matrix().matrix() }
        }).collect();
        let s = take(frames * 9);
        let samples = s.chunks(9).map(|c| {
            let q = UnitQuaternion::from_quaternion(nalgebra::Quaternion::new(c[5], c[6], c[7], c[8]));
            DominantSample { time_ms: c[0], segment: c[1] as u32, motion: [c[2] * c[4], c[3] * c[4]], camera_to_world: *q.to_rotation_matrix().matrix() }
        }).collect();
        let gr = take(g * 10);
        let grid_t = gr.chunks(10).map(|c| c[0]).collect();
        let orientation = gr.chunks(10).map(|c| Matrix3::from_row_slice(&c[1..10])).collect();
        let tf = take(frames - 1);
        let mut series = |n: usize| -> Vec<[f64; 3]> { take(n * 3).chunks(3).map(|c| [c[0], c[1], c[2]]).collect() };
        let out = [series(frames - 1), series(frames - 1)];
        let out_noise = [series(frames - 1), series(frames - 1)];
        let vec3 = |v: Vec<f64>| v.chunks(3).map(|c| Vector3::new(c[0], c[1], c[2])).collect::<Vec<_>>();
        let d_dom = vec3(take(g * 3));
        let res = vec3(take(np * 3));
        let area = take(np);
        let sel = take(5);
        let table = take(nt);
        let table_noise = take(nt);
        let d = vec3(take(g * 3));
        let band_in = take(nb * 3).chunks(3).map(|c| [c[0], c[1], c[2]]).collect();
        let band_out = take(nb * 9).chunks(9).map(|c| c.try_into().unwrap()).collect();
        Fixture { points, samples, grid_t, orientation, tf, out, out_noise, d_dom, res, area,
            selected: sel.try_into().unwrap(), table, table_noise, d, band_in, band_out, t0, focal, fps, floor }
    }

    fn rel(a: f64, b: f64) -> f64 { (a - b).abs() / b.abs().max(1e-300) }

    #[test]
    fn preprocessing_matches_the_reference() {
        let f = fixture();
        let mut problem = ResidualProblem::new(f.points.clone(), f.focal).unwrap();
        assert!((problem.t0_ms - f.t0).abs() < 1e-9);
        assert_eq!(problem.nodes, f.grid_t.len());
        assert!((problem.floor - f.floor).abs() < 1e-15);
        assert_eq!(problem.u as f64, f.selected[4]);
        let d_dom = dominant_chain(&f.samples, &f.grid_t);
        for (a, b) in d_dom.iter().zip(&f.d_dom) { assert!((a - b).norm() <= 1e-12 + 1e-9 * b.norm(), "{a} {b}"); }
        problem.subtract_dominant(&d_dom);
        for (a, b) in problem.res.iter().zip(&f.res) { assert!((a - b).norm() <= 1e-9 * b.norm().max(1e-12), "{a} {b}"); }
        for (a, b) in problem.area.iter().zip(&f.area) { assert!(rel(*a, *b) < 1e-9); }
    }

    #[test]
    fn band_filter_matches_scipy() {
        let f = fixture();
        for (bi, (lo, hi)) in BANDS_HZ.iter().enumerate() {
            for c in 0..3 {
                let x: Vec<f64> = f.band_in.iter().map(|v| v[c]).collect();
                let y = bandpass(&x, *lo, hi.min(0.45 * f.fps), f.fps).unwrap();
                let scale = f.band_out.iter().map(|v| v[3 * bi + c].abs()).fold(0.0, f64::max);
                for (a, row) in y.iter().zip(&f.band_out) { assert!((a - row[3 * bi + c]).abs() < 1e-9 * scale, "band {bi} axis {c}"); }
            }
        }
    }

    fn held(f: &Fixture, out: &[Vec<[f64; 3]>; 2]) -> [HeldOutMotion; 2] {
        [0, 1].map(|h| HeldOutMotion { times_ms: f.tf.clone(), out: out[h].clone(), fps: f.fps })
    }

    #[test]
    fn selection_matches_the_reference_and_falls_back_to_zero() {
        let f = fixture();
        let mut problem = ResidualProblem::new(f.points.clone(), f.focal).unwrap();
        problem.subtract_dominant(&dominant_chain(&f.samples, &f.grid_t));
        for (out, table, sel) in [(&f.out, &f.table, (f.selected[0], f.selected[1])), (&f.out_noise, &f.table_noise, (f.selected[2], f.selected[3]))] {
            let h = held(&f, out);
            let s = select_strength(&problem, [&h[0], &h[1]], &f.orientation, f.focal, &|| false).unwrap();
            for ((_, _, a), b) in s.scores.iter().zip(table) { assert!((a - b).abs() < 1e-7, "{a} {b}"); }
            assert!((s.sigma_deg - sel.0).abs() < 1e-12 && (s.tau_s - sel.1).abs() < 1e-12, "{s:?} vs {sel:?}");
        }
        assert!(f.selected[0] > 0.0, "the visible error selects a correction");
        assert_eq!(f.selected[2], 0.0, "noise alone selects none");
    }

    #[test]
    fn final_estimate_matches_the_reference() {
        let f = fixture();
        let mut problem = ResidualProblem::new(f.points.clone(), f.focal).unwrap();
        problem.subtract_dominant(&dominant_chain(&f.samples, &f.grid_t));
        let t = std::time::Instant::now();
        let d = problem.estimate(f.selected[0].to_radians(), f.selected[1]).unwrap();
        let max = d.iter().zip(&f.d).map(|(a, b)| (a - b).norm()).fold(0.0, f64::max);
        assert!(max < 1e-6, "max difference {max} rad");
        eprintln!("final estimate: {} nodes, {:.3} s", d.len(), t.elapsed().as_secs_f64());
    }

    #[test]
    fn zero_strength_gives_no_correction() {
        let f = fixture();
        let problem = ResidualProblem::new(f.points, f.focal).unwrap();
        let d = problem.estimate(0.0, TAUS_S[0]).unwrap();
        assert!(d.iter().all(|v| *v == Vector3::zeros()));
    }

    #[test]
    fn similarity_fit_recovers_a_rotation_and_shift() {
        let a: Vec<(f64, f64)> = (0..50).map(|i| (100.0 + 13.0 * (i % 10) as f64 * 7.0, 60.0 + 41.0 * (i / 10) as f64 * 2.0)).collect();
        let (ang, tx, ty) = (0.01f64, 3.0, -2.0);
        let c = (480.0, 270.0);
        let b: Vec<(f64, f64)> = a.iter().map(|p| {
            let (x, y) = (p.0 - c.0, p.1 - c.1);
            (c.0 + x * ang.cos() - y * ang.sin() + tx, c.1 + x * ang.sin() + y * ang.cos() + ty)
        }).collect();
        let s = fit_similarity(&a, &b, c, &vec![1.0; a.len()]);
        assert!((s[0] - ang).abs() < 1e-9 && (s[1] - tx).abs() < 1e-9 && (s[2] - ty).abs() < 1e-9, "{s:?}");
    }

    #[test]
    fn similarity_fit_follows_the_points_that_count_more() {
        // Two interleaved layers shifted apart: the fit goes with the one whose points count three times
        let a: Vec<(f64, f64)> = (0..60).map(|i| (100.0 + 70.0 * (i % 12) as f64, 80.0 + 80.0 * (i / 12) as f64)).collect();
        let b: Vec<(f64, f64)> = a.iter().enumerate().map(|(i, p)| (p.0 + if i % 2 == 0 { 2.0 } else { -2.0 }, p.1)).collect();
        let prior = |even: f64| (0..a.len()).map(|i| if i % 2 == 0 { even } else { 1.0 }).collect::<Vec<_>>();
        let (more, fewer) = (fit_similarity(&a, &b, (480.0, 270.0), &prior(3.0))[1], fit_similarity(&a, &b, (480.0, 270.0), &prior(1.0 / 3.0))[1]);
        assert!(more > 1.0 && fewer < -1.0, "{more} {fewer}");
        let equal = fit_similarity(&a, &b, (480.0, 270.0), &prior(1.0))[1];
        assert!(fewer + 1.0 < equal && equal + 1.0 < more, "{equal}");
    }

    #[test]
    fn cumulative_interpolates_missing_rates() {
        assert_eq!(cumulative(&[1.0, f64::NAN, 3.0]), vec![0.0, 1.0, 3.0, 6.0]);
        assert_eq!(cumulative(&[f64::NAN, 2.0]), vec![0.0, 2.0, 4.0]);
    }
}
