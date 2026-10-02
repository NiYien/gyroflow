// SPDX-License-Identifier: GPL-3.0-or-later

//! Track observation / frame-pair / window data types and the frame-continuity
//! state machine used while feeding decoded frames.

use nalgebra::Vector3;

/// One tracked point seen in two consecutive frames (px at tracking resolution).
#[derive(Clone, Copy, Debug)]
pub struct Observation { pub id: u32, pub a: [f32; 2], pub b: [f32; 2] }

/// Scaled video time of a frame; the per-frame offset is NOT added.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawFrame { pub index: usize, pub ts_ms: f64 }

pub struct RawPair { pub a: RawFrame, pub b: RawFrame, pub obs: Vec<Observation> }

#[derive(Default)]
pub struct RawWindow { pub pairs: Vec<RawPair>, pub track_size: (u32, u32), pub frames: usize, pub restarts: usize, pub dropped_pairs: usize }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStep { First, Continuous, Gap, Restart }

pub fn classify_step(last_index: Option<usize>, index: usize, every_nth: usize) -> FrameStep {
    match last_index {
        None => FrameStep::First,
        Some(last) if index == last + every_nth => FrameStep::Continuous,
        Some(last) if index <= last => FrameStep::Restart,
        Some(_) => FrameStep::Gap,
    }
}

/// Frame index from scaled timestamp (us) and scaled fps.
pub fn frame_index(ts_us: i64, scaled_fps: f64) -> usize {
    crate::frame_at_timestamp(ts_us as f64 / 1000.0, scaled_fps).max(0) as usize
}

pub struct WindowBuilder {
    pub raw: RawWindow,
    last: Option<RawFrame>,
    every_nth: usize,
}

impl WindowBuilder {
    pub fn new(every_nth: usize) -> Self {
        Self { raw: RawWindow::default(), last: None, every_nth: every_nth.max(1) }
    }

    /// Restart clears `raw.pairs` (adds their count to `dropped_pairs`, bumps `restarts`). Caller resets the tracker on Gap and Restart.
    pub fn begin_frame(&mut self, index: usize) -> FrameStep {
        let step = classify_step(self.last.map(|f| f.index), index, self.every_nth);
        if step == FrameStep::Restart {
            self.raw.dropped_pairs += self.raw.pairs.len();
            self.raw.pairs.clear();
            self.raw.restarts += 1;
        }
        step
    }

    /// Pushes a pair only for Continuous with non-empty `obs`; always records `frame` as the last one.
    pub fn end_frame(&mut self, frame: RawFrame, step: FrameStep, obs: Vec<Observation>) {
        self.raw.frames += 1;
        if step == FrameStep::Continuous && !obs.is_empty() {
            if let Some(a) = self.last {
                self.raw.pairs.push(RawPair { a, b: frame, obs });
            }
        }
        self.last = Some(frame);
    }
}

/// mid = ts + per-frame offset; readout is signed.
#[derive(Clone, Copy, Debug)]
pub struct FrameTiming { pub index: usize, pub ts_ms: f64, pub mid_ms: f64, pub readout_ms: f64 }

/// `seq` is the pair's index in `WindowTracks::pairs`; ids are never reused across tracker resets,
/// so "same id and adjacent seq" means continuous.
pub struct PairData {
    pub seq: usize, pub a: FrameTiming, pub b: FrameTiming, pub ids: Vec<u32>,
    pub va: Vec<Vector3<f64>>, pub vb: Vec<Vector3<f64>>, pub fa: Vec<f32>, pub fb: Vec<f32>,
}

pub struct WindowTracks { pub pairs: Vec<PairData>, pub focal_px: f64 }

pub fn row_time_ms(f: &FrameTiming, pos: f32) -> f64 {
    f.mid_ms + f.readout_ms * (pos as f64 - 0.5)
}

/// x/width when `horizontal`, else y/height.
pub fn readout_pos(pt: [f32; 2], size: (u32, u32), horizontal: bool) -> f32 {
    if horizontal { pt[0] / size.0 as f32 } else { pt[1] / size.1 as f32 }
}

/// Size a frame is tracked at: frames wider than `track_width` are downscaled to that width, the height in proportion
/// and rounded to an even number; narrower frames keep their size.
pub fn tracking_size(size: (u32, u32), track_width: u32) -> (u32, u32) {
    let (w, h) = size;
    if w <= track_width { return size; }
    let th = (h as f64 * track_width as f64 / w as f64 / 2.0).round() as u32 * 2;
    (track_width, th.max(2))
}

/// Ids for the corners one replenish added, given in cell order (`counts[c]` corners of grid cell `c`, the corners of
/// a cell next to each other): handed out round-robin over the cells, starting at `first_id` (the first corner of
/// every cell, then the second of every cell, ...). Ties between equally long track segments are broken by id
/// (`cost::select_tracks`), and this way the lowest ids are spread over the frame instead of filling one cell.
pub fn round_robin_ids(counts: &[usize], first_id: u32) -> Vec<u32> {
    let mut start = Vec::with_capacity(counts.len());
    let mut total = 0;
    for &n in counts {
        start.push(total);
        total += n;
    }
    let mut ids = vec![0u32; total];
    let mut next = first_id;
    for k in 0..counts.iter().copied().max().unwrap_or(0) {
        for (cell, &n) in counts.iter().enumerate() {
            if k < n {
                ids[start[cell] + k] = next;
                next = next.wrapping_add(1);
            }
        }
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn classify_step_stride_1_and_2() {
        assert_eq!(classify_step(None, 7, 1), FrameStep::First);
        assert_eq!(classify_step(Some(7), 8, 1), FrameStep::Continuous);
        assert_eq!(classify_step(Some(7), 9, 1), FrameStep::Gap);
        assert_eq!(classify_step(Some(8), 10, 2), FrameStep::Continuous);
        assert_eq!(classify_step(Some(8), 9, 2), FrameStep::Gap);
        assert_eq!(classify_step(Some(8), 8, 2), FrameStep::Restart);
        assert_eq!(classify_step(Some(8), 2, 2), FrameStep::Restart);
    }
    #[test] fn restart_drops_pairs_without_duplicates() {
        let obs = || vec![Observation { id: 1, a: [0.0; 2], b: [1.0; 2] }];
        let mut w = WindowBuilder::new(1);
        for i in 10..14 { let s = w.begin_frame(i); w.end_frame(RawFrame { index: i, ts_ms: i as f64 }, s, obs()); }
        assert_eq!(w.raw.pairs.len(), 3);
        for i in 10..13 { let s = w.begin_frame(i); w.end_frame(RawFrame { index: i, ts_ms: i as f64 }, s, obs()); }  // software retry
        assert_eq!((w.raw.restarts, w.raw.dropped_pairs, w.raw.pairs.len()), (1, 3, 2));
        let idx: Vec<usize> = w.raw.pairs.iter().map(|p| p.a.index).collect();
        assert_eq!(idx, vec![10, 11]);
    }
    #[test] fn gap_keeps_existing_pairs() {
        let obs = || vec![Observation { id: 1, a: [0.0; 2], b: [1.0; 2] }];
        let mut w = WindowBuilder::new(1);
        for i in [10, 11, 15, 16] { let s = w.begin_frame(i); w.end_frame(RawFrame { index: i, ts_ms: i as f64 }, s, obs()); }
        assert_eq!(w.raw.pairs.len(), 2);   // 10-11 and 15-16, nothing across the gap
    }
    #[test] fn frame_index_uses_scaled_time() {
        // 120 fps clip conformed to 30 fps: the caller passes scaled µs and the scaled fps
        assert_eq!(frame_index(1_000_000, 30.0), 30);
        assert_eq!(frame_index(-5, 30.0), 0);
    }
    #[test] fn row_time_handles_signed_readout() {
        let f = FrameTiming { index: 0, ts_ms: 100.0, mid_ms: 100.0, readout_ms: -10.0 };
        assert_eq!(row_time_ms(&f, 0.0), 105.0);
        assert_eq!(row_time_ms(&f, 1.0), 95.0);
    }
    #[test] fn readout_pos_uses_x_when_horizontal() {
        assert_eq!(readout_pos([240.0, 54.0], (960, 540), true), 0.25);
        assert_eq!(readout_pos([240.0, 54.0], (960, 540), false), 0.1);
    }
    #[test] fn tracking_size_downscales_wide_frames_to_an_even_height() {
        assert_eq!(tracking_size((1920, 1080), 960), (960, 540));
        assert_eq!(tracking_size((3840, 1606), 960), (960, 402));   // 401.5
        assert_eq!(tracking_size((3840, 1600), 960), (960, 400));
        assert_eq!(tracking_size((4096, 2160), 960), (960, 506));   // 506.25
        assert_eq!(tracking_size((960, 540), 960), (960, 540));
        assert_eq!(tracking_size((640, 361), 960), (640, 361));     // not downscaled: left as is
    }
    #[test] fn round_robin_ids_interleave_the_cells() {
        // Cell order: two corners of cell 0, none of cell 1, one of cell 2, three of cell 3
        assert_eq!(round_robin_ids(&[2, 0, 1, 3], 10), vec![10, 13, 11, 12, 14, 15]);
        assert_eq!(round_robin_ids(&[], 7), Vec::<u32>::new());
        assert_eq!(round_robin_ids(&[0, 0], 7), Vec::<u32>::new());
    }
    #[test] fn round_robin_ids_spread_the_lowest_ids_over_the_grid() {
        // A full replenish of the 6x4 grid: 62 corners per cell
        let counts = [62usize; 24];
        let ids = round_robin_ids(&counts, 1000);
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (1000..1000 + 62 * 24).collect::<Vec<u32>>());   // unique, allocated from first_id
        let cell_of = |k: usize| k / 62;
        let mut cells: Vec<usize> = (0..ids.len()).filter(|&k| ids[k] < 1024).map(cell_of).collect();
        cells.sort_unstable();
        cells.dedup();
        assert_eq!(cells.len(), 24);   // the first 24 ids: one in every cell
    }
}
