// SPDX-License-Identifier: GPL-3.0-or-later

//! Depth warp, a second, per-region stage of translation stabilization. The rigid shift holds one reference layer
//! still; content at another depth keeps (beta - beta_ref) times the unit-depth layer's shake. This stage compensates
//! that on a coarse grid over the source image: each cell's depth is shrunk towards the reference by how well it is
//! known, and the step between neighbouring cells stays under a hard stretch limit.

use std::hash::Hash;
use std::sync::OnceLock;
use super::{TranslationSample, high_pass, time_difference_s};

pub const COLS: usize = 16;
pub const ROWS: usize = 9;
pub const CELLS: usize = COLS * ROWS;

/// Stored log inverse depth: steps of 1/32, `Z_MISSING` for a sample without a grid
const Z_STEP: f32 = 1.0 / 32.0;
const Z_MISSING: i8 = i8::MIN;

/// Largest step of the warp between neighbouring cells, as a fraction of their spacing
const STRETCH: f64 = 0.02;
/// Largest warp of a cell, as a fraction of the source's short side
const MAX_WARP: f64 = 0.03;
/// Visibility of a cell without any tracked point
pub const EMPTY_VISIBILITY: f32 = 0.05;
/// Time pooling of the grids at the end of the analysis (Gaussian sigma and radius)
const POOL_SIGMA_S: f64 = 0.25;
const POOL_RADIUS_S: f64 = 0.75;

/// The stretch limit; `GYROFLOW_DEPTH_WARP_STRETCH` in (0, 0.5] overrides it for comparisons.
pub fn stretch_limit() -> f64 {
    static VALUE: OnceLock<f64> = OnceLock::new();
    *VALUE.get_or_init(|| match std::env::var("GYROFLOW_DEPTH_WARP_STRETCH") {
        Ok(raw) if !raw.is_empty() => match raw.trim().parse::<f64>() {
            Ok(v) if v > 0.0 && v <= 0.5 => {
                log::info!(target: "lifecycle", "GYROFLOW_DEPTH_WARP_STRETCH={}", v);
                v
            }
            _ => {
                log::warn!(target: "lifecycle", "GYROFLOW_DEPTH_WARP_STRETCH={} invalid, falling back to {}", raw, STRETCH);
                STRETCH
            }
        },
        _ => STRETCH,
    })
}

/// One pair's grid: log inverse depth relative to the pair's median, how much of the prior's uncertainty the
/// measurements removed (`r` = 1 - posterior variance / prior variance: 0 without any information, 1 when known
/// exactly), and how visible each cell is.
#[derive(Clone, Debug, PartialEq)]
pub struct PairDepthGrid {
    pub z: Vec<f32>,
    pub r: Vec<f32>,
    pub vis: Vec<f32>,
}

/// The analysis' grids, one per translation sample, pooled over time and quantized.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DepthGrids {
    /// Half width and height of the grid in the source's normalized undistorted image plane
    pub half_extent: [f32; 2],
    pub timestamps_us: Vec<i64>,
    pub z: Vec<i8>,
    pub r: Vec<u8>,
    pub vis: Vec<u8>,
}

impl DepthGrids {
    /// Pools each sample's grid with those of the samples around it in its segment, weighted by time and the pair's
    /// confidence, and quantizes the result. `None` when no sample has a grid.
    pub fn pool(half_extent: [f64; 2], samples: &[TranslationSample], grids: &[Option<PairDepthGrid>]) -> Option<Self> {
        if !grids.iter().any(|g| g.is_some()) || !half_extent.iter().all(|h| h.is_finite() && *h > 0.0) { return None; }
        let mut out = Self { half_extent: half_extent.map(|h| h as f32), ..Default::default() };
        for i in 0..samples.len() {
            out.timestamps_us.push(samples[i].timestamp_us);
            let (mut zs, mut rs, mut ks, mut vis) = (vec![0.0f64; CELLS], vec![0.0f64; CELLS], 0.0f64, vec![0.0f64; CELLS]);
            for j in 0..samples.len() {
                let Some(grid) = grids.get(j).and_then(|g| g.as_ref()) else { continue };
                let dt = time_difference_s(samples[j].timestamp_us, samples[i].timestamp_us);
                if samples[j].segment != samples[i].segment || dt.abs() > POOL_RADIUS_S { continue; }
                let k = (-0.5 * (dt / POOL_SIGMA_S).powi(2)).exp() * samples[j].weight.clamp(0.0, 1.0) as f64;
                if !(k > 0.0) { continue; }
                ks += k;
                for c in 0..CELLS {
                    let r = (grid.r[c] as f64).clamp(0.0, 1.0);
                    zs[c] += k * r * grid.z[c] as f64;
                    rs[c] += k * r;
                    vis[c] += k * grid.vis[c] as f64;
                }
            }
            for c in 0..CELLS {
                if ks > 0.0 && rs[c] > 0.0 {
                    // The mean reliability, not the sum: neighbouring pairs share their tracks and are not independent
                    out.z.push(((zs[c] / rs[c]) as f32 / Z_STEP).round().clamp(-127.0, 127.0) as i8);
                    out.r.push(((rs[c] / ks) as f32 * 255.0).round().clamp(0.0, 255.0) as u8);
                    out.vis.push(((vis[c] / ks) as f32 * 255.0).round().clamp(0.0, 255.0) as u8);
                } else {
                    out.z.push(Z_MISSING);
                    out.r.push(0);
                    out.vis.push(0);
                }
            }
        }
        Some(out)
    }

    fn index_of(&self, timestamp_us: i64) -> Option<usize> {
        self.timestamps_us.binary_search(&timestamp_us).ok().filter(|i| (i + 1) * CELLS <= self.z.len().min(self.r.len()).min(self.vis.len()))
    }

    /// Log inverse depth relative to the pair's median, its reliability and the cell's visibility; `None` for a cell
    /// without data.
    pub fn cell(&self, timestamp_us: i64, c: usize) -> Option<(f64, f64, f64)> {
        let i = self.index_of(timestamp_us)? * CELLS + c;
        if self.z[i] == Z_MISSING { return None; }
        Some((self.z[i] as f64 * Z_STEP as f64, self.r[i] as f64 / 255.0, self.vis[i] as f64 / 255.0))
    }

    /// Centre of a cell in the normalized image plane
    pub fn centre(&self, c: usize) -> (f64, f64) {
        let (hx, hy) = (self.half_extent[0] as f64, self.half_extent[1] as f64);
        (-hx + ((c % COLS) as f64 + 0.5) * 2.0 * hx / COLS as f64, -hy + ((c / COLS) as f64 + 0.5) * 2.0 * hy / ROWS as f64)
    }

    pub fn hash_into(&self, hasher: &mut impl std::hash::Hasher) {
        for h in self.half_extent { h.to_bits().hash(hasher); }
        self.timestamps_us.hash(hasher);
        self.z.hash(hasher);
        self.r.hash(hasher);
        self.vis.hash(hasher);
    }
}

/// The warp at one moment, for lookups away from the translation result (and its lock)
#[derive(Clone, Debug)]
pub struct WarpCells {
    pub cells: Vec<[f32; 2]>,
    pub half_extent: [f32; 2],
}

impl WarpCells {
    /// Displacement at a point of the source's normalized undistorted image plane, in `shift_at`'s convention
    pub fn at(&self, x: f64, y: f64) -> nalgebra::Vector2<f64> {
        sample_cells(&self.cells, self.half_extent, x, y)
    }
}

/// The warp at one sample: the displacement of each cell, in the rigid shift's normalized units and convention.
#[derive(Clone, Debug)]
pub(super) struct WarpPoint {
    pub timestamp_us: i64,
    pub segment: u32,
    pub cells: Vec<[f32; 2]>,
}

/// The warp of one segment. `reference` holds the rigid stage's reference depth at each sample (relative to the
/// pair's median, like the grids), `sigma` its smoothing time. `None` when cancelled.
pub(super) fn segment_warp(segment: &[TranslationSample], times: &[f64], reference: &[f64], sigma: f64, along_axis: bool,
    depth: &DepthGrids, cancelled: &dyn Fn() -> bool) -> Option<Vec<WarpPoint>> {
    // The unit-depth layer's shake: its accumulated motion, high-passed like the rigid curve
    let mut position = nalgebra::Vector3::zeros();
    let positions: Vec<_> = segment.iter().enumerate().map(|(i, s)| {
        if i > 0 {
            let w = s.weight as f64;
            position += nalgebra::Vector3::new(w * s.layer_motion[0] as f64, w * s.layer_motion[1] as f64,
                if along_axis { w * s.layer_scale_rate as f64 } else { 0.0 });
        }
        position
    }).collect();
    let unit = high_pass(times, &positions, sigma, cancelled)?;
    let limit = stretch_limit();
    let spacing = (2.0 * depth.half_extent[0] as f64 / COLS as f64, 2.0 * depth.half_extent[1] as f64 / ROWS as f64);
    let centres: Vec<_> = (0..CELLS).map(|c| depth.centre(c)).collect();
    let mut out = Vec::with_capacity(segment.len());
    for (i, sample) in segment.iter().enumerate() {
        if cancelled() { return None; }
        let u = unit[i];
        let beta_ref = reference[i];
        let mut cells = vec![[0.0f32; 2]; CELLS];
        let data: Vec<_> = (0..CELLS).map(|c| depth.cell(sample.timestamp_us, c)).collect();
        if beta_ref > 0.0 && beta_ref.is_finite() && u.iter().all(|v| v.is_finite()) && data.iter().any(|d| d.is_some()) {
            let log_ref = beta_ref.ln();
            let vectors: Vec<_> = centres.iter().map(|(x, y)| nalgebra::Vector2::new(u.x + x * u.z, u.y + y * u.z)).collect();
            let mut target = vec![0.0; CELLS];
            let mut weight = vec![1e-3; CELLS];
            for c in 0..CELLS {
                // Shrunk towards the reference by how much the measurements tell: a cell nothing was learnt about
                // keeps the rigid shift. Its weight is the measurements' precision relative to the prior's, times
                // how visible the cell is, so the stretch goes where it shows least.
                let Some((z, r, vis)) = data[c] else { continue };
                target[c] = (log_ref + r * (z - log_ref)).exp() - beta_ref;
                weight[c] = vis.max(EMPTY_VISIBILITY as f64) * r / (1.0 - r + 1e-3);
            }
            let mean_weight = weight.iter().sum::<f64>() / CELLS as f64;
            for w in &mut weight { *w = (*w / mean_weight).max(1e-6); }
            let mut edges = Vec::with_capacity(2 * CELLS);
            for c in 0..CELLS {
                let (col, row) = (c % COLS, c / COLS);
                for (n, h) in [(col + 1 < COLS).then_some((c + 1, spacing.0)), (row + 1 < ROWS).then_some((c + COLS, spacing.1))].into_iter().flatten() {
                    let speed = vectors[c].norm().max(vectors[n].norm());
                    if speed > 1e-12 { edges.push((c, n, limit * h / speed)); }
                }
            }
            let cap = MAX_WARP / (sample.focal_length_over_short_side as f64).max(1e-9);
            let bounds: Vec<_> = vectors.iter().map(|v| if v.norm() > 1e-12 { cap / v.norm() } else { f64::INFINITY }).collect();
            let a = constrain(&target, &weight, &edges, &bounds);
            for c in 0..CELLS {
                let d = vectors[c] * a[c];
                cells[c] = [d.x as f32, d.y as f32];
            }
        }
        out.push(WarpPoint { timestamp_us: sample.timestamp_us, segment: sample.segment, cells });
    }
    Some(out)
}

/// The scalar field closest to `target` in the `weight`ed norm whose neighbours differ by at most each edge's bound
/// and whose cells stay within `bounds`: weighted Dykstra projections, then a uniform scale that makes the result
/// strictly feasible whatever the iterations left. A cell with a small weight takes most of each correction.
pub(super) fn constrain(target: &[f64], weight: &[f64], edges: &[(usize, usize, f64)], bounds: &[f64]) -> Vec<f64> {
    let n = target.len();
    let mut x = target.to_vec();
    let violation = |x: &[f64]| {
        let e = edges.iter().map(|&(i, j, k)| ((x[i] - x[j]).abs() - k).max(0.0)).fold(0.0, f64::max);
        let b = (0..n).map(|c| (x[c].abs() - bounds[c]).max(0.0)).fold(0.0, f64::max);
        e.max(b)
    };
    let scale = target.iter().map(|v| v.abs()).fold(0.0, f64::max).max(1e-12);
    if violation(&x) > 0.0 {
        let mut edge_increments = vec![[0.0f64; 2]; edges.len()];
        let mut bound_increments = vec![0.0f64; n];
        for _ in 0..300 {
            for (e, &(i, j, k)) in edges.iter().enumerate() {
                let (yi, yj) = (x[i] + edge_increments[e][0], x[j] + edge_increments[e][1]);
                let d = yi - yj;
                let excess = if d > k { d - k } else if d < -k { d + k } else { 0.0 };
                let (wi, wj) = (weight[i], weight[j]);
                let (ni, nj) = (yi - excess * wj / (wi + wj), yj + excess * wi / (wi + wj));
                edge_increments[e] = [yi - ni, yj - nj];
                x[i] = ni;
                x[j] = nj;
            }
            for c in 0..n {
                let y = x[c] + bound_increments[c];
                let projected = y.clamp(-bounds[c], bounds[c]);
                bound_increments[c] = y - projected;
                x[c] = projected;
            }
            if violation(&x) < 1e-9 * scale { break; }
        }
    }
    // Strictly feasible: shrinking every value shrinks every difference
    let mut s = 1.0f64;
    for &(i, j, k) in edges {
        let d = (x[i] - x[j]).abs();
        if d > k { s = s.min(k / d); }
    }
    for c in 0..n {
        if x[c].abs() > bounds[c] { s = s.min(bounds[c] / x[c].abs()); }
    }
    if s < 1.0 { for v in &mut x { *v *= s; } }
    x
}

/// Bilinear value of the cells at a point of the normalized image plane, clamped at the edges.
pub(super) fn sample_cells(cells: &[[f32; 2]], half_extent: [f32; 2], x: f64, y: f64) -> nalgebra::Vector2<f64> {
    let gx = ((x + half_extent[0] as f64) / (2.0 * half_extent[0] as f64) * COLS as f64 - 0.5).clamp(0.0, (COLS - 1) as f64);
    let gy = ((y + half_extent[1] as f64) / (2.0 * half_extent[1] as f64) * ROWS as f64 - 0.5).clamp(0.0, (ROWS - 1) as f64);
    let (x0, y0) = ((gx.floor() as usize).min(COLS - 2), (gy.floor() as usize).min(ROWS - 2));
    let (fx, fy) = (gx - x0 as f64, gy - y0 as f64);
    let at = |c: usize, r: usize| { let v = cells[r * COLS + c]; nalgebra::Vector2::new(v[0] as f64, v[1] as f64) };
    at(x0, y0) * (1.0 - fx) * (1.0 - fy) + at(x0 + 1, y0) * fx * (1.0 - fy) + at(x0, y0 + 1) * (1.0 - fx) * fy + at(x0 + 1, y0 + 1) * fx * fy
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(i: i64, weight: f32) -> TranslationSample {
        TranslationSample { timestamp_us: i * 40_000, weight, focal_length_over_short_side: 1.0,
            layer_motion: [if i % 10 < 5 { 0.002 } else { -0.002 }, 0.0], far_beta: 1.0, ..Default::default() }
    }

    /// Left half at log inverse depth `left`, right half at `right`, every cell with reliability `r`
    fn split_grid(left: f32, right: f32, r: f32, vis_right: f32) -> PairDepthGrid {
        PairDepthGrid {
            z: (0..CELLS).map(|c| if c % COLS < COLS / 2 { left } else { right }).collect(),
            r: vec![r; CELLS],
            vis: (0..CELLS).map(|c| if c % COLS < COLS / 2 { 1.0 } else { vis_right }).collect(),
        }
    }

    fn warp_of(grid: PairDepthGrid, beta_ref: f64) -> (Vec<WarpPoint>, DepthGrids) {
        let samples: Vec<_> = (0..60).map(|i| sample(i, 1.0)).collect();
        let grids = vec![Some(grid); samples.len()];
        let depth = DepthGrids::pool([0.8, 0.45], &samples, &grids).unwrap();
        let times: Vec<_> = samples.iter().map(|s| time_difference_s(s.timestamp_us, samples[0].timestamp_us)).collect();
        let warp = segment_warp(&samples, &times, &vec![beta_ref; samples.len()], 1.0, true, &depth, &|| false).unwrap();
        (warp, depth)
    }

    fn max_step(cells: &[[f32; 2]]) -> f64 {
        let mut worst = 0.0f64;
        for c in 0..CELLS {
            for n in [(c % COLS + 1 < COLS).then_some(c + 1), (c / COLS + 1 < ROWS).then_some(c + COLS)].into_iter().flatten() {
                worst = worst.max((cells[c][0] - cells[n][0]).abs() as f64).max((cells[c][1] - cells[n][1]).abs() as f64);
            }
        }
        worst
    }

    #[test]
    fn depth_warp_grids_round_trip_their_quantization() {
        let grid = PairDepthGrid { z: (0..CELLS).map(|c| -2.0 + c as f32 / 40.0).collect(), r: (0..CELLS).map(|c| c as f32 / CELLS as f32).collect(), vis: vec![0.4; CELLS] };
        let samples = vec![sample(0, 1.0)];
        let depth = DepthGrids::pool([0.8, 0.45], &samples, &[Some(grid.clone())]).unwrap();
        for c in 0..CELLS {
            let Some((z, r, vis)) = depth.cell(0, c) else { assert_eq!(c, 0, "only a cell without reliability has no data"); continue };
            assert!((z - grid.z[c] as f64).abs() <= 1.0 / 64.0 + 1e-9);
            assert!((r - grid.r[c] as f64).abs() <= 0.5 / 255.0 + 1e-6, "{r} {}", grid.r[c]);
            assert!((vis - 0.4).abs() < 0.005);
        }
        assert!(depth.z.len() + depth.r.len() + depth.vis.len() <= 450);
        assert!(DepthGrids::pool([0.8, 0.45], &samples, &[None]).is_none());
        let text = crate::util::compress_to_base91_cbor(&depth).unwrap();
        assert_eq!(crate::util::decompress_from_base91_cbor::<DepthGrids>(&text).unwrap(), depth);
    }

    #[test]
    fn depth_warp_is_zero_without_knowledge_or_depth_difference() {
        // Every depth unknown: the warp shrinks to the rigid shift
        let (warp, _) = warp_of(split_grid(0.0, 1.4, 0.0, 1.0), 1.0);
        assert!(warp.iter().all(|w| w.cells.iter().all(|c| c[0].abs() < 1e-6 && c[1].abs() < 1e-6)));
        // Every cell at the reference depth
        let (warp, _) = warp_of(split_grid(0.0, 0.0, 0.99, 1.0), 1.0);
        assert!(warp.iter().all(|w| w.cells.iter().all(|c| c[0] == 0.0 && c[1] == 0.0)));
    }

    #[test]
    fn depth_warp_compensates_the_parallax_of_another_layer_within_its_limits() {
        // Reference on the left (beta 1), the right four times nearer
        let (warp, depth) = warp_of(split_grid(0.0, 4f32.ln(), 0.99, 1.0), 1.0);
        let middle = &warp[30];
        let hx = 2.0 * depth.half_extent[0] as f64 / COLS as f64;
        // Left far from the step: no warp; right far from it: towards 3 times the unit motion
        assert!(middle.cells[0][0].abs() < 1e-6);
        assert!(middle.cells[COLS - 1][0].abs() > 0.0);
        for w in &warp {
            assert!(max_step(&w.cells) <= stretch_limit() * hx * 1.0 + 1e-6, "{}", max_step(&w.cells));
            assert!(w.cells.iter().all(|c| (c[0] as f64).hypot(c[1] as f64) <= MAX_WARP + 1e-6));
        }
    }

    #[test]
    fn depth_warp_puts_the_stretch_where_it_shows_least() {
        let target: Vec<_> = (0..CELLS).map(|c| if c % COLS < COLS / 2 { 0.0 } else { 3.0 }).collect();
        let edges: Vec<_> = (0..CELLS).filter(|c| c % COLS + 1 < COLS).map(|c| (c, c + 1, 0.1)).collect();
        let bounds = vec![f64::INFINITY; CELLS];
        // The near side barely visible: its cells move, the textured far side stays
        let weight: Vec<_> = (0..CELLS).map(|c| if c % COLS < COLS / 2 { 1.0 } else { 0.05 }).collect();
        let a = constrain(&target, &weight, &edges, &bounds);
        let deviation = |left: bool| (0..CELLS).filter(|c| (c % COLS < COLS / 2) == left).map(|c| (a[c] - target[c]).powi(2)).sum::<f64>();
        assert!(deviation(false) >= 4.0 * deviation(true), "{} {}", deviation(false), deviation(true));
        for &(i, j, k) in &edges { assert!((a[i] - a[j]).abs() <= k + 1e-9); }
        // A feasible target comes back unchanged
        let flat = vec![0.5; CELLS];
        assert_eq!(constrain(&flat, &weight, &edges, &bounds), flat);
    }

    #[test]
    fn depth_warp_samples_its_cells_bilinearly() {
        let cells: Vec<_> = (0..CELLS).map(|c| [(c % COLS) as f32, (c / COLS) as f32]).collect();
        let h = [0.8f32, 0.45f32];
        let centre = |c: usize| (-0.8 + ((c % COLS) as f64 + 0.5) * 1.6 / COLS as f64, -0.45 + ((c / COLS) as f64 + 0.5) * 0.9 / ROWS as f64);
        let (x, y) = centre(3 + 2 * COLS);
        let v = sample_cells(&cells, h, x, y);
        assert!((v.x - 3.0).abs() < 1e-5 && (v.y - 2.0).abs() < 1e-5, "{v:?}");
        let v = sample_cells(&cells, h, 5.0, -5.0);
        assert!((v.x - (COLS - 1) as f64).abs() < 1e-9 && v.y.abs() < 1e-9);
    }
}
