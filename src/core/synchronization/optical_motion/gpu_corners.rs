// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

//! GPU corner responses with the local tracker's masks, per-cell thresholds and deterministic point order.
use super::tracks::round_robin_ids;
pub(super) const COLS: usize = 4;
pub(super) const ROWS: usize = 6;
const QUALITY: f64 = 0.002;
const NOISE_MULTIPLE: f64 = 8.0;
const GLARE_LEVEL: u8 = 249;
const GLARE_RADII: f64 = 2.5;
const GLARE_MIN_AREA: f64 = 30.0;

pub(super) struct GpuCorners {
    pub size: (usize, usize),
    pub global: Vec<f32>,
    pub local: Vec<f32>,
    pub global_candidates: Vec<u64>,
    pub local_candidates: Vec<u64>,
    pub glare: Vec<(i32, i32, i32)>,
}
impl GpuCorners {
    pub fn new(w: usize, h: usize) -> Self {
        Self {
            size: (w, h),
            global: vec![0.0; w * h],
            local: vec![0.0; w * h],
            global_candidates: Vec::new(),
            local_candidates: Vec::new(),
            glare: Vec::new(),
        }
    }
    pub fn find_glare(&mut self, pixels: &[u8], stride: usize) {
        self.glare = glare(pixels, stride, self.size.0 as i32, self.size.1 as i32);
    }
    pub fn texture_at(&self, p: [f32; 2]) -> f32 {
        let (x, y) = (p[0].round() as i32, p[1].round() as i32);
        if x < 0 || y < 0 || x >= self.size.0 as i32 || y >= self.size.1 as i32 {
            f32::NAN
        } else {
            self.global[y as usize * self.size.0 + x as usize]
        }
    }
    pub fn select(
        &self,
        occupied: &[[f32; 2]],
        max_points: usize,
        ratio: f64,
        first_id: u32,
    ) -> Vec<(u32, [f32; 2])> {
        if occupied.len() as f64 >= ratio * max_points as f64 {
            return Vec::new();
        }
        let (w, h) = self.size;
        let distance = (w.max(h) as f64 / 120.0).max(4.0);
        let mut mask = vec![true; w * h];
        for p in occupied {
            clear_circle(&mut mask, w, h, p[0] as i32, p[1] as i32, distance as i32);
        }
        for &(x, y, r) in &self.glare {
            clear_circle(&mut mask, w, h, x, y, r);
        }
        let maximum = |eig: &[f32], mask: &[bool], rect: (usize, usize, usize, usize)| {
            let (x0, y0, x1, y1) = rect;
            let mut lanes = [0.0f32; 8];
            for y in y0..y1 {
                let start = y * w + x0;
                let values = &eig[start..y * w + x1];
                let valid = &mask[start..y * w + x1];
                let end = values.len() / 8 * 8;
                for (values, valid) in values[..end].chunks_exact(8).zip(valid[..end].chunks_exact(8)) {
                    for k in 0..8 {
                        // Responses are nonnegative; masked pixels contribute zero without a branch per pixel.
                        let value = f32::from_bits(values[k].to_bits() & (valid[k] as u32).wrapping_neg());
                        lanes[k] = lanes[k].max(value);
                    }
                }
                for (&value, &valid) in values[end..].iter().zip(&valid[end..]) {
                    if valid { lanes[0] = lanes[0].max(value); }
                }
            }
            lanes.into_iter().fold(0.0f32, f32::max) as f64
        };
        let strongest = maximum(&self.global, &mask, (0, 0, w, h));
        if strongest <= 0.0 {
            return Vec::new();
        }
        let mut noise: Vec<f32> = (0..h)
            .step_by(8)
            .flat_map(|y| (0..w).step_by(8).map(move |x| self.global[y * w + x]))
            .filter(|v| *v > 0.0)
            .collect();
        if noise.is_empty() {
            return Vec::new();
        }
        let nth = noise.len() / 10;
        let floor =
            NOISE_MULTIPLE * (*noise.select_nth_unstable_by(nth, |a, b| a.total_cmp(b)).1 as f64);
        let threshold = QUALITY * strongest;
        let mut candidates = vec![Vec::new(); COLS * ROWS];
        for &k in &self.local_candidates {
            let i = k as u32 as usize;
            let (x, y) = (i % w, i / w);
            let c = (((x + 1) * COLS - 1) / w).min(COLS - 1);
            let r = (((y + 1) * ROWS - 1) / h).min(ROWS - 1);
            candidates[r * COLS + c].push(k);
        }
        let share = max_points / (COLS * ROWS);
        let mut points = Vec::new();
        let mut counts = vec![0; COLS * ROWS];
        for r in 0..ROWS {
            for c in 0..COLS {
                let rect = (
                    w * c / COLS,
                    h * r / ROWS,
                    w * (c + 1) / COLS,
                    h * (r + 1) / ROWS,
                );
                let (x0, y0, x1, y1) = rect;
                let has = occupied
                    .iter()
                    .filter(|p| {
                        p[0] as usize >= x0
                            && (p[0] as usize) < x1
                            && p[1] as usize >= y0
                            && (p[1] as usize) < y1
                    })
                    .count();
                if has >= share || x1 <= x0 || y1 <= y0 {
                    continue;
                }
                let cell_strongest = maximum(&self.global, &mask, rect);
                let cell_threshold = threshold.min(floor.max(QUALITY * cell_strongest));
                if cell_threshold <= 0.0 || cell_strongest < cell_threshold {
                    continue;
                }
                let local_max = maximum(&self.local, &mask, rect);
                let cutoff = (local_max * (cell_threshold / cell_strongest)) as f32;
                let selected = select_candidates(
                    &candidates[r * COLS + c],
                    &mask,
                    w,
                    rect,
                    cutoff,
                    distance,
                    share - has,
                );
                counts[r * COLS + c] = selected.len();
                // OpenCV selects the whole cell before its circles affect the next cell.
                for p in selected {
                    clear_circle(&mut mask, w, h, p[0] as i32, p[1] as i32, distance as i32);
                    points.push(p);
                }
            }
        }
        let mut ids = round_robin_ids(&counts, first_id);
        if occupied.len() + points.len() < max_points {
            let cutoff = (maximum(&self.global, &mask, (0, 0, w, h)) * QUALITY) as f32;
            let selected = select_candidates(
                &self.global_candidates,
                &mask,
                w,
                (0, 0, w, h),
                cutoff,
                distance,
                max_points - occupied.len() - points.len(),
            );
            for p in selected {
                ids.push(first_id.wrapping_add(points.len() as u32));
                points.push(p);
            }
        }
        ids.into_iter().zip(points).collect()
    }
}

/// Candidates are ordered by strength, then descending pixel index, as OpenCV orders its pointers.
fn select_candidates(
    candidates: &[u64],
    mask: &[bool],
    stride: usize,
    rect: (usize, usize, usize, usize),
    cutoff: f32,
    distance: f64,
    wanted: usize,
) -> Vec<[f32; 2]> {
    if wanted == 0 {
        return Vec::new();
    }
    let (x0, y0, x1, y1) = rect;
    let cell = (distance.round_ties_even() as usize).max(1);
    let (gw, gh) = ((x1 - x0).div_ceil(cell), (y1 - y0).div_ceil(cell));
    let mut grid = vec![Vec::<[f32; 2]>::new(); gw * gh];
    let mut out = Vec::new();
    for &k in candidates {
        if f32::from_bits((k >> 32) as u32) <= cutoff {
            break;
        }
        let i = k as u32 as usize;
        if !mask[i] {
            continue;
        }
        let p = [(i % stride) as f32, (i / stride) as f32];
        let (cx, cy) = ((i % stride - x0) / cell, (i / stride - y0) / cell);
        let near = (cy.saturating_sub(1)..=(cy + 1).min(gh - 1)).any(|y| {
            (cx.saturating_sub(1)..=(cx + 1).min(gw - 1)).any(|x| {
                grid[y * gw + x].iter().any(|q| {
                    (((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)) as f64) < distance * distance
                })
            })
        });
        if near {
            continue;
        }
        grid[cy * gw + cx].push(p);
        out.push(p);
        if out.len() == wanted {
            break;
        }
    }
    out
}

/// Filled LINE_8 integer circles, including their boundary, matching the existing OpenCV masks.
fn clear_circle(mask: &mut [bool], w: usize, h: usize, cx: i32, cy: i32, radius: i32) {
    let mut line = |y: i32, left: i32, right: i32| {
        if y < 0 || y >= h as i32 {
            return;
        }
        let (left, right) = (left.max(0), right.min(w as i32 - 1));
        if left <= right {
            mask[y as usize * w + left as usize..=y as usize * w + right as usize].fill(false);
        }
    };
    let (mut dx, mut dy, mut err, mut plus, mut minus) = (radius, 0, 0, 1, 2 * radius - 1);
    while dx >= dy {
        line(cy + dy, cx - dx, cx + dx);
        line(cy - dy, cx - dx, cx + dx);
        line(cy + dx, cx - dy, cx + dy);
        line(cy - dx, cx - dy, cx + dy);
        dy += 1;
        err += plus;
        plus += 2;
        if err > 0 {
            err -= minus;
            dx -= 1;
            minus -= 2;
        }
    }
}

/// Overflow fallback for a response plateau; ordinary frames use the GPU candidate list.
pub(super) fn local_maxima(values: &[f32], w: usize, h: usize, cells: bool) -> Vec<u64> {
    let mut out = Vec::new();
    for y in 1..h.saturating_sub(1) {
        for x in 1..w.saturating_sub(1) {
            if cells {
                let c = ((x + 1) * COLS - 1) / w;
                let r = ((y + 1) * ROWS - 1) / h;
                if x == w * c / COLS
                    || x + 1 == w * (c + 1) / COLS
                    || y == h * r / ROWS
                    || y + 1 == h * (r + 1) / ROWS
                {
                    continue;
                }
            }
            let i = y * w + x;
            let v = values[i];
            if v > 0.0
                && (y - 1..=y + 1).all(|yy| (x - 1..=x + 1).all(|xx| values[yy * w + xx] <= v))
            {
                out.push((v.to_bits() as u64) << 32 | i as u64);
            }
        }
    }
    out.sort_unstable_by(|a, b| b.cmp(a));
    out
}

fn glare(pixels: &[u8], stride: usize, w: i32, h: i32) -> Vec<(i32, i32, i32)> {
    let (wu, hu) = (w as usize, h as usize);
    let (wf, hf) = (w as f64, h as f64);
    let min_area = GLARE_MIN_AREA * (wf / 960.0).powi(2);
    let row = |y: usize| &pixels[y * stride..y * stride + wu];
    let counts: Vec<u32> = (0..hu)
        .map(|y| row(y).iter().map(|&p| (p > GLARE_LEVEL) as u32).sum())
        .collect();
    if (counts.iter().sum::<u32>() as f64) < min_area {
        return Vec::new();
    }

    // The runs (y, from x, to x past the run), each with the blob it's in: the run it's joined to, first in the frame
    let mut runs: Vec<(u32, u32, u32)> = Vec::new();
    let mut parent: Vec<u32> = Vec::new();
    fn root(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            parent[i as usize] = parent[parent[i as usize] as usize];
            i = parent[i as usize];
        }
        i
    }
    let mut above = 0..0;
    for y in 0..hu {
        let start = runs.len();
        if counts[y] > 0 {
            let r = row(y);
            let mut x = 0;
            while let Some(skip) = r[x..].iter().position(|&p| p > GLARE_LEVEL) {
                let x0 = x + skip;
                x = x0
                    + r[x0..]
                        .iter()
                        .position(|&p| p <= GLARE_LEVEL)
                        .unwrap_or(wu - x0);
                let i = runs.len() as u32;
                runs.push((y as u32, x0 as u32, x as u32));
                parent.push(i);
                // 8-connected to the runs above that come within a pixel of it
                for j in above.clone() {
                    let (_, a0, a1) = runs[j];
                    if a0 <= x as u32 && x0 as u32 <= a1 {
                        let (ri, rj) = (root(&mut parent, i), root(&mut parent, j as u32));
                        if ri != rj {
                            parent[ri.max(rj) as usize] = ri.min(rj);
                        }
                    }
                }
                if x >= wu {
                    break;
                }
            }
        }
        above = start..runs.len();
    }
    // Each blob's area and bounding box (the right and the bottom edge past it)
    let mut blobs = vec![(0u64, u32::MAX, u32::MAX, 0u32, 0u32); runs.len()];
    for i in 0..runs.len() {
        let (y, x0, x1) = runs[i];
        let b = &mut blobs[root(&mut parent, i as u32) as usize];
        *b = (
            b.0 + (x1 - x0) as u64,
            b.1.min(x0),
            b.2.min(y),
            b.3.max(x1),
            b.4.max(y + 1),
        );
    }
    blobs
        .into_iter()
        .filter(|b| b.0 > 0)
        .filter_map(|(area, bx, by, x_end, y_end)| {
            let (area, bx, by, bw, bh) = (
                area as f64,
                bx as f64,
                by as f64,
                (x_end - bx) as f64,
                (y_end - by) as f64,
            );
            // The frame's edge may cut it (a half disc is twice as wide as tall): its size is the larger side then
            let cut = bx <= 0.0 || by <= 0.0 || bx + bw >= wf || by + bh >= hf;
            let compact =
                area >= 0.5 * bw * bh && bw.max(bh) <= if cut { 3.0 } else { 2.0 } * bw.min(bh);
            let radius = bw.max(bh) / 2.0;
            (area >= min_area && compact && radius <= wf / 12.0).then(|| {
                (
                    (bx + bw / 2.0) as i32,
                    (by + bh / 2.0) as i32,
                    (GLARE_RADII * radius) as i32,
                )
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opencv::{
        core::{CV_8UC1, Mat, Point, Scalar},
        imgproc,
        prelude::*,
    };
    #[test]
    fn filled_circles_match_opencv() {
        for (w, h) in [(64, 48), (853, 480)] {
            for radius in [0, 1, 2, 4, 7, 8, 13, 31] {
                for (x, y) in [(0, 0), (w / 2, h / 2), (w - 1, h - 1), (-2, 7)] {
                    let mut mask = vec![true; (w * h) as usize];
                    clear_circle(&mut mask, w as usize, h as usize, x, y, radius);
                    let mut expected =
                        Mat::new_rows_cols_with_default(h, w, CV_8UC1, Scalar::all(255.0)).unwrap();
                    imgproc::circle(
                        &mut expected,
                        Point::new(x, y),
                        radius,
                        Scalar::all(0.0),
                        -1,
                        imgproc::LINE_8,
                        0,
                    )
                    .unwrap();
                    assert!(
                        mask.iter()
                            .zip(expected.data_bytes().unwrap())
                            .all(|(a, b)| *a == (*b != 0)),
                        "{w}x{h} circle {x},{y}/{radius}"
                    );
                }
            }
        }
    }
}
