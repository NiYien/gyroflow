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

/// `frames`: frames recorded (duplicates not counted); `duplicates`: frames skipped as `FrameStep::Duplicate`
#[derive(Default)]
pub struct RawWindow { pub pairs: Vec<RawPair>, pub track_size: (u32, u32), pub frames: usize, pub restarts: usize, pub dropped_pairs: usize, pub duplicates: usize }

/// How a frame continues a window, by its frame index against the window's frames since its last restart
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameStep {
    /// The window's first frame
    First,
    /// The next frame after the last one (`last + every_nth`)
    Continuous,
    /// Later than that: the tracker starts over, the pairs so far stay
    Gap,
    /// At or before the first frame since the last restart: a decode retry feeds the window again from its start,
    /// so the pairs so far are dropped and the window starts over with this frame
    Restart,
    /// After the first and at or before the last frame: a frame seen already, skipped. The decoder delivers the
    /// overlap of two windows twice (it seeks back to the next range's start after each range), and a variable
    /// frame rate clip can map two frames to the same index.
    Duplicate,
}

/// `span`: (first, last) frame index since the window's last restart, None before its first frame. `every_nth` is
/// clamped to at least 1.
pub fn classify_step(span: Option<(usize, usize)>, index: usize, every_nth: usize) -> FrameStep {
    match span {
        None => FrameStep::First,
        Some((_, last)) if index == last + every_nth.max(1) => FrameStep::Continuous,
        Some((first, _)) if index <= first => FrameStep::Restart,
        Some((_, last)) if index <= last => FrameStep::Duplicate,
        Some(_) => FrameStep::Gap,
    }
}

/// Frame index from scaled timestamp (us) and scaled fps.
pub fn frame_index(ts_us: i64, scaled_fps: f64) -> usize {
    crate::frame_at_timestamp(ts_us as f64 / 1000.0, scaled_fps).max(0) as usize
}

pub struct WindowBuilder {
    pub raw: RawWindow,
    /// Frame index of the window's first frame since its last restart
    first: Option<usize>,
    last: Option<RawFrame>,
    every_nth: usize,
}

impl WindowBuilder {
    pub fn new(every_nth: usize) -> Self {
        Self { raw: RawWindow::default(), first: None, last: None, every_nth: every_nth.max(1) }
    }

    /// Classifies the frame (see `classify_step`). Restart clears `raw.pairs` (adds their count to `dropped_pairs`,
    /// bumps `restarts`); First and Restart make the frame the window's first. Duplicate only bumps `duplicates`: the
    /// caller skips the frame, neither tracking it nor calling `end_frame`. Caller resets the tracker on Gap and Restart.
    pub fn begin_frame(&mut self, index: usize) -> FrameStep {
        let span = self.first.zip(self.last.map(|f| f.index));
        let step = classify_step(span, index, self.every_nth);
        match step {
            FrameStep::First => self.first = Some(index),
            FrameStep::Restart => {
                self.raw.dropped_pairs += self.raw.pairs.len();
                self.raw.pairs.clear();
                self.raw.restarts += 1;
                self.first = Some(index);
            }
            FrameStep::Duplicate => self.raw.duplicates += 1,
            FrameStep::Continuous | FrameStep::Gap => {}
        }
        step
    }

    /// Pushes a pair only for Continuous with non-empty `obs`; records `frame` as the last one for every step but
    /// Duplicate, which leaves the window untouched.
    pub fn end_frame(&mut self, frame: RawFrame, step: FrameStep, obs: Vec<Observation>) {
        if step == FrameStep::Duplicate { return; }
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
        use FrameStep::*;
        assert_eq!(classify_step(None, 7, 1), First);
        assert_eq!(classify_step(Some((7, 7)), 8, 1), Continuous);
        assert_eq!(classify_step(Some((7, 7)), 9, 1), Gap);
        assert_eq!(classify_step(Some((2, 8)), 10, 2), Continuous);
        assert_eq!(classify_step(Some((2, 8)), 9, 2), Gap);
        // At or before the first frame since the last restart: a decode retry
        assert_eq!(classify_step(Some((8, 8)), 8, 2), Restart);
        assert_eq!(classify_step(Some((4, 8)), 4, 2), Restart);
        assert_eq!(classify_step(Some((4, 8)), 2, 2), Restart);
        // After the first, at or before the last: seen already
        assert_eq!(classify_step(Some((4, 8)), 5, 2), Duplicate);
        assert_eq!(classify_step(Some((4, 8)), 8, 2), Duplicate);
        // A stride of 0 counts as 1
        assert_eq!(classify_step(Some((7, 7)), 8, 0), Continuous);
        assert_eq!(classify_step(Some((7, 7)), 7, 0), Restart);
        assert_eq!(classify_step(Some((7, 9)), 8, 0), Duplicate);
    }
    fn obs() -> Vec<Observation> { vec![Observation { id: 1, a: [0.0; 2], b: [1.0; 2] }] }
    /// One frame as the tracking thread handles it: a Duplicate is skipped, everything else is recorded
    fn feed(w: &mut WindowBuilder, index: usize, ts_ms: f64) -> FrameStep {
        let s = w.begin_frame(index);
        if s != FrameStep::Duplicate { w.end_frame(RawFrame { index, ts_ms }, s, obs()); }
        s
    }
    /// (a, b) frame indices of every pair
    fn pair_indices(w: &WindowBuilder) -> Vec<(usize, usize)> { w.raw.pairs.iter().map(|p| (p.a.index, p.b.index)).collect() }
    fn consecutive(from: usize, to: usize) -> Vec<(usize, usize)> { (from..to).map(|i| (i, i + 1)).collect() }
    #[test] fn restart_drops_pairs_without_duplicates() {
        let mut w = WindowBuilder::new(1);
        for i in 10..14 { feed(&mut w, i, i as f64); }
        assert_eq!(w.raw.pairs.len(), 3);
        for i in 10..13 { feed(&mut w, i, i as f64); }  // software retry
        assert_eq!((w.raw.restarts, w.raw.dropped_pairs, w.raw.pairs.len(), w.raw.duplicates), (1, 3, 2, 0));
        let idx: Vec<usize> = w.raw.pairs.iter().map(|p| p.a.index).collect();
        assert_eq!(idx, vec![10, 11]);
    }
    /// Windows A = frames 10..=30 and B = 20..=40 in the decoder's order: A's range (its overlap with B goes to both),
    /// then B's range, which delivers the overlap 20..=30 again
    fn decode_overlapping(a: &mut WindowBuilder, b: &mut WindowBuilder) {
        for i in 10..=30 {
            feed(a, i, i as f64);
            if i >= 20 { feed(b, i, i as f64); }
        }
        for i in 20..=40 {
            if i <= 30 { assert_eq!(feed(a, i, i as f64), FrameStep::Duplicate, "frame {i}"); }
            feed(b, i, i as f64);
        }
    }
    #[test] fn overlap_delivered_again_is_skipped_not_restarted() {
        let (mut a, mut b) = (WindowBuilder::new(1), WindowBuilder::new(1));
        decode_overlapping(&mut a, &mut b);
        // A keeps every pair of its range, once
        assert_eq!(pair_indices(&a), consecutive(10, 30));
        assert_eq!((a.raw.restarts, a.raw.dropped_pairs, a.raw.duplicates, a.raw.frames), (0, 0, 11, 21));
        // B restarts at its own first frame and ends with its whole range
        assert_eq!(pair_indices(&b), consecutive(20, 40));
        assert_eq!((b.raw.restarts, b.raw.dropped_pairs, b.raw.duplicates), (1, 10, 0));
    }
    #[test] fn decode_retry_of_overlapping_windows_starts_both_over() {
        let (mut a, mut b) = (WindowBuilder::new(1), WindowBuilder::new(1));
        decode_overlapping(&mut a, &mut b);
        // A software retry on the same process feeds both ranges again from the start
        decode_overlapping(&mut a, &mut b);
        assert_eq!(pair_indices(&a), consecutive(10, 30));
        assert_eq!((a.raw.restarts, a.raw.dropped_pairs), (1, 20));
        assert_eq!(pair_indices(&b), consecutive(20, 40));
        assert_eq!((b.raw.restarts, b.raw.dropped_pairs), (3, 10 + 20 + 10));
    }
    #[test] fn repeated_frame_index_is_a_duplicate() {
        // A variable frame rate clip maps two frames to index 12; the second is skipped and the pairs stay
        let mut w = WindowBuilder::new(1);
        let steps: Vec<FrameStep> = [(10, 0.0), (11, 1.0), (12, 2.0), (12, 2.4), (13, 3.0), (14, 4.0)].iter().map(|&(i, t)| feed(&mut w, i, t)).collect();
        use FrameStep::*;
        assert_eq!(steps, vec![First, Continuous, Continuous, Duplicate, Continuous, Continuous]);
        assert_eq!(pair_indices(&w), consecutive(10, 14));
        assert_eq!((w.raw.restarts, w.raw.duplicates, w.raw.frames), (0, 1, 5));
        assert_eq!(w.raw.pairs[2].a.ts_ms, 2.0);   // the pair 12-13 starts at the frame recorded first
    }
    #[test] fn overlap_with_the_other_stride_phase_keeps_the_pairs() {
        // Every 2nd frame: B's range delivers the overlap at the odd frames A's range skipped. Those are duplicates
        // for both windows; B then resumes after a gap.
        let (mut a, mut b) = (WindowBuilder::new(2), WindowBuilder::new(2));
        for i in (10..=30).step_by(2) {
            feed(&mut a, i, i as f64);
            if i >= 20 { feed(&mut b, i, i as f64); }
        }
        for i in (21..=41).step_by(2) {
            if i <= 30 { assert_eq!(feed(&mut a, i, i as f64), FrameStep::Duplicate); }
            let s = feed(&mut b, i, i as f64);
            assert_eq!(s, if i <= 29 { FrameStep::Duplicate } else if i == 31 { FrameStep::Gap } else { FrameStep::Continuous }, "frame {i}");
        }
        assert_eq!(pair_indices(&a), (10..30).step_by(2).map(|i| (i, i + 2)).collect::<Vec<_>>());
        let b_pairs: Vec<(usize, usize)> = (20..30).step_by(2).chain((31..41).step_by(2)).map(|i| (i, i + 2)).collect();
        assert_eq!((pair_indices(&b), b.raw.restarts), (b_pairs, 0));
    }
    #[test] fn gap_keeps_existing_pairs() {
        let mut w = WindowBuilder::new(1);
        for i in [10, 11, 15, 16] { feed(&mut w, i, i as f64); }
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
