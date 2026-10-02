// SPDX-License-Identifier: GPL-3.0-or-later

//! Optical motion sync method (offset_method = 3): KLT feature tracks plus a
//! rotation-residual cost, independent of the optical-flow based methods.
//!
//! `AutosyncProcess` drives it in two stages. While frames are decoded, an `OpticalSession` tracks every window on
//! its own tracking thread (spec §5.1). Once decoding is done, `finished_feeding_frames` turns the raw tracks into
//! bearings (`bearings::build_window_tracks`) and `solve_windows` searches each window's offset (spec §6), one result
//! row per window.

pub mod bearings;
pub mod config;
pub mod cost;
pub mod quat_table;
pub mod search;
#[cfg(feature = "use-opencv")] pub mod tracker;
pub mod tracks;
#[cfg(test)] pub mod testutil;

use std::panic::{ catch_unwind, AssertUnwindSafe };
use std::sync::Arc;
use std::sync::atomic::{ AtomicBool, AtomicU64, Ordering::Relaxed };
use std::sync::mpsc::{ sync_channel, Receiver, SyncSender, TrySendError };
use std::thread::JoinHandle;
use std::time::Instant;

use parking_lot::Mutex;

use crate::gyro_source::TimeQuat;
use crate::synchronization::{ sync_diag, GrayImage, SyncParams };
use self::config::OpticalConfig;
use self::cost::{ eval_coarse, eval_full, select_tracks, CostContext, SgCache };
use self::quat_table::QuatTable;
use self::search::{ failed, grid, run_search, search_intervals, FailReason, FullEval, SearchOutcome, SearchParams };
use self::tracks::{ frame_index, row_time_ms, FrameStep, Observation, RawFrame, RawWindow, WindowBuilder, WindowTracks };

/// Tracks kept alive per frame (upstream value)
pub const MAX_POINTS: usize = 1500;
/// Tracking threads of a session; window k is tracked by thread k % TRACKING_THREADS
const TRACKING_THREADS: usize = 2;
/// Frames a tracking thread's queue holds before `OpticalSession::feed` blocks the decoding thread
const QUEUE_FRAMES: usize = 16;
/// Reweighting rounds of the coarse evaluation
const COARSE_IRLS_ROUNDS: usize = 5;
/// The quaternion table reaches this far past the gyro times of the search intervals, ms: the near probes and the
/// output valley's refinement can go up to ~45 ms past an interval end
const TABLE_MARGIN_MS: f64 = 50.0;

/// Feature tracker fed the frames of one window in order; runs on a tracking thread
pub trait FrameTracker: Send {
    fn reset(&mut self);
    /// Observations between the previous frame and this one, with the size they are expressed in
    fn track(&mut self, img: &GrayImage) -> Result<(Vec<Observation>, (u32, u32)), String>;
}

/// None when built without `use-opencv`
pub fn default_tracker(cfg: &OpticalConfig) -> Option<Box<dyn FrameTracker>> {
    #[cfg(feature = "use-opencv")]
    { Some(Box::new(tracker::KltTracker::new(MAX_POINTS, cfg.replenish, cfg.track_width))) }
    #[cfg(not(feature = "use-opencv"))]
    { let _ = cfg; None }
}

/// One window's track store under construction and its tracker (None without `use-opencv`)
type WindowSlot = (WindowBuilder, Option<Box<dyn FrameTracker>>);

/// Calls the session's `on_frame_done` when dropped, so that every frame handed to `feed` reports done exactly once:
/// after it was tracked, skipped because of cancellation, or dropped (fed after `finish`, or left in the queue of a
/// tracking thread that died). `finished_feeding_frames` waits for that count.
struct FrameDone(Arc<dyn Fn() + Send + Sync>);

impl Drop for FrameDone {
    fn drop(&mut self) { (self.0)() }
}

/// A frame queued for a tracking thread. Fields drop in order: the frame is released before `done` reports it.
struct Job {
    window: usize,
    ts_us: i64,
    img: Arc<GrayImage>,
    _done: FrameDone,
}

#[derive(Default)]
struct Counters {
    track_ns: AtomicU64,
    decode_wait_ns: AtomicU64,
    /// Whether a tracking failure was logged already
    warned: AtomicBool,
}

/// Wall times of a session's tracking side, ms (logged in the `[optical] run:` line)
#[derive(Clone, Copy, Debug)]
pub struct SessionTimings {
    /// Time the tracking threads spent inside `FrameTracker::track`, summed over the threads (so it can exceed the
    /// elapsed time when both threads track at once)
    pub track_ms: f64,
    /// Time the decoding thread was blocked in `OpticalSession::feed` because the tracking thread's queue was full
    pub decode_wait_ms: f64,
}

/// Feature tracking of the windows while the frames are decoded (spec §5.1): TRACKING_THREADS threads, each with a
/// queue of QUEUE_FRAMES frames, and per window the raw track store and its own tracker.
pub struct OpticalSession {
    slots: Arc<Vec<Mutex<WindowSlot>>>,
    /// Queue of tracking thread k at index k; None once `finish` closed it or when the thread could not start
    senders: Mutex<Vec<Option<SyncSender<Job>>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    on_frame_done: Arc<dyn Fn() + Send + Sync>,
    counters: Arc<Counters>,
    has_tracker: bool,
}

impl OpticalSession {
    /// `on_frame_done` runs on the worker after each frame, also when cancelled. Window k is served by worker k % 2.
    ///
    /// Exactly once per frame given to `feed`: a frame `feed` cannot queue (fed after `finish`) reports done on the
    /// feeding thread. With a single window only one tracking thread is started.
    pub fn new(windows: usize, scaled_fps: f64, every_nth: usize, cancel: Arc<AtomicBool>,
               make_tracker: &dyn Fn() -> Option<Box<dyn FrameTracker>>, on_frame_done: Arc<dyn Fn() + Send + Sync>) -> Self {
        let slots: Vec<Mutex<WindowSlot>> = (0..windows).map(|_| Mutex::new((WindowBuilder::new(every_nth), make_tracker()))).collect();
        let has_tracker = !slots.is_empty() && slots.iter().all(|s| s.lock().1.is_some());
        let slots = Arc::new(slots);
        let counters = Arc::new(Counters::default());
        let mut senders = Vec::new();
        let mut workers = Vec::new();
        for k in 0..TRACKING_THREADS.min(windows) {
            let (tx, rx) = sync_channel::<Job>(QUEUE_FRAMES);
            let (slots, cancel, counters) = (slots.clone(), cancel.clone(), counters.clone());
            let spawned = std::thread::Builder::new()
                .name(format!("Optical track {}", k))
                .spawn(move || track_queue(rx, &slots, scaled_fps, &cancel, &counters));
            match spawned {
                Ok(handle) => {
                    senders.push(Some(tx));
                    workers.push(handle);
                }
                Err(e) => {
                    log::error!(target: "sync", "[optical] tracking thread {} could not start: {}", k, e);
                    senders.push(None);
                }
            }
        }
        Self { slots, senders: Mutex::new(senders), workers: Mutex::new(workers), on_frame_done, counters, has_tracker }
    }

    /// `ts_us`: scaled, per-frame offset NOT added. Blocks while the worker's queue is full.
    pub fn feed(&self, window: usize, ts_us: i64, img: Arc<GrayImage>) {
        // Created first: whichever way the frame leaves, it reports done
        let job = Job { window, ts_us, img, _done: FrameDone(self.on_frame_done.clone()) };
        if window >= self.slots.len() { return; }
        let Some(tx) = self.senders.lock().get(window % TRACKING_THREADS).cloned().flatten() else { return; };
        match tx.try_send(job) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(job)) => {
                let started = Instant::now();
                // An error means the thread is gone; the frame drops with it
                let _ = tx.send(job);
                self.counters.decode_wait_ns.fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
            }
        }
    }

    /// Closes the queues and joins the workers. A second call returns empty windows.
    pub fn finish(&self) -> Vec<RawWindow> {
        // Without senders a worker's loop ends once its queue is empty
        self.senders.lock().clear();
        let workers = std::mem::take(&mut *self.workers.lock());
        for worker in workers {
            if worker.join().is_err() {
                log::error!(target: "sync", "[optical] a tracking thread panicked");
            }
        }
        let raw: Vec<RawWindow> = self.slots.iter().map(|slot| std::mem::take(&mut slot.lock().0.raw)).collect();
        for (i, w) in raw.iter().enumerate() {
            log::debug!(target: "sync", "[optical] seg {}: tracked frames={} pairs={} restarts={} dropped_pairs={} duplicates={}",
                i, w.frames, w.pairs.len(), w.restarts, w.dropped_pairs, w.duplicates);
        }
        raw
    }

    pub fn has_tracker(&self) -> bool { self.has_tracker }

    pub fn timings(&self) -> SessionTimings {
        SessionTimings {
            track_ms: self.counters.track_ns.load(Relaxed) as f64 / 1e6,
            decode_wait_ms: self.counters.decode_wait_ns.load(Relaxed) as f64 / 1e6,
        }
    }
}

/// A tracking thread: its queue's frames in order until the queue is closed. Once cancelled the frames are only
/// reported done.
fn track_queue(rx: Receiver<Job>, slots: &[Mutex<WindowSlot>], scaled_fps: f64, cancel: &AtomicBool, counters: &Counters) {
    crate::worker_priority::apply_to_current_thread();
    for job in rx {
        if !cancel.load(Relaxed) {
            track_frame(&mut slots[job.window].lock(), job.window, job.ts_us, &job.img, scaled_fps, counters);
        }
        // `job` drops here, after the window's lock was released: on_frame_done
    }
}

/// One frame of a window: continuity check (`tracks::classify_step`), tracking, and the pair with the previous frame.
/// The tracker starts over after a gap or a decode retry (Restart). A frame the window has seen already (Duplicate:
/// the overlap with the next window delivered again, or a repeated frame index) is skipped: the tracker never sees
/// it and the window stays as it was. A tracking error leaves the frame without observations.
fn track_frame(slot: &mut WindowSlot, window: usize, ts_us: i64, img: &GrayImage, scaled_fps: f64, counters: &Counters) {
    let (builder, tracker) = slot;
    let index = frame_index(ts_us, scaled_fps);
    let pairs_before = builder.raw.pairs.len();
    let step = builder.begin_frame(index);
    if step == FrameStep::Duplicate { return; }
    if matches!(step, FrameStep::Gap | FrameStep::Restart) {
        if let Some(t) = tracker.as_mut() { t.reset(); }
    }
    if step == FrameStep::Restart {
        log::info!(target: "sync", "[optical] seg {}: restarted (frame index went back), dropped {} pairs", window, pairs_before);
    }
    let mut obs = Vec::new();
    if let Some(t) = tracker.as_mut() {
        let started = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| t.track(img)));
        counters.track_ns.fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
        let error = match result {
            Ok(Ok((o, size))) => {
                builder.raw.track_size = size;
                obs = o;
                None
            }
            Ok(Err(e)) => Some(e),
            Err(_) => {
                t.reset();
                Some("the tracker panicked".to_string())
            }
        };
        if let Some(e) = error {
            if !counters.warned.swap(true, Relaxed) {
                log::warn!(target: "sync", "[optical] seg {}: tracking failed, frame left without observations: {} (further failures are not logged)", window, e);
            }
        }
    }
    builder.end_frame(RawFrame { index, ts_ms: ts_us as f64 / 1000.0 }, step, obs);
}

/// Indices of the ranges that hold `ts_us`, ends included (the same window test as `AutosyncProcess::feed_frame`'s)
pub fn windows_containing(ranges_us: &[(i64, i64)], ts_us: i64) -> Vec<usize> {
    ranges_us.iter().enumerate().filter(|(_, (from, to))| (*from..=*to).contains(&ts_us)).map(|(i, _)| i).collect()
}

/// `img` cut to its first `width` columns. `PoseEstimator::yuv_to_gray` makes the row stride the image width, so a
/// padded frame carries the padding as extra columns; the tracker must see the frame's real width. The same Arc when
/// there is nothing to cut.
pub fn crop_to_width(img: Arc<GrayImage>, width: u32) -> Arc<GrayImage> {
    let (stride, height) = img.dimensions();
    if width == 0 || width >= stride { return img; }
    let mut pixels = Vec::with_capacity(width as usize * height as usize);
    for row in img.as_raw().chunks_exact(stride as usize) {
        pixels.extend_from_slice(&row[..width as usize]);
    }
    match GrayImage::from_raw(width, height, pixels) {
        Some(cropped) => Arc::new(cropped),
        None => img,
    }
}

/// What `solve_windows` needs besides the tracks. `ranges_us`: the windows in scaled µs (`AutosyncProcess`'s
/// `scaled_ranges_us`); `sync_params`: offsets and search size in ms; `quats`: the gyro quaternions
/// (`GyroSource::quaternions`); `has_tracker`: false when built without a tracker, which fails every window as no_opencv.
pub struct SolveInput<'a> { pub ranges_us: &'a [(i64, i64)], pub sync_params: &'a SyncParams, pub quats: &'a TimeQuat, pub cfg: &'a OpticalConfig, pub has_tracker: bool }

/// One row per range, in range order. None when cancelled. `progress` gets 0..1 across windows.
///
/// `windows[i]` holds the tracks of `ranges_us[i]` (a missing one counts as empty). A row is (window centre, offset,
/// cost, confidence) in ms, ms, px; a window that fails gives (centre, initial offset, 0, 0). Every window is logged
/// (target `sync`): `[optical] seg i: …` with the result, or `[optical] seg i: failed reason=…`.
pub fn solve_windows(windows: &[WindowTracks], input: &SolveInput, pool: &rayon::ThreadPool, cancel: &AtomicBool,
                     progress: &(dyn Fn(f64) + Sync)) -> Option<Vec<(f64, f64, f64, f64)>> {
    if input.sync_params.calc_initial_fast {
        log::info!(target: "sync", "[optical] calc_initial_fast ignored (not applicable to the optical method)");
    }
    let n = input.ranges_us.len();
    let sg = SgCache::new();
    let no_tracks = WindowTracks { pairs: Vec::new(), focal_px: 0.0 };
    let mut rows = Vec::with_capacity(n);
    progress(0.0);
    for (i, &(from, to)) in input.ranges_us.iter().enumerate() {
        if cancel.load(Relaxed) { return None; }
        let outcome = if input.has_tracker {
            solve_window(i, windows.get(i).unwrap_or(&no_tracks), input, &sg, pool, cancel)?
        } else {
            let o = failed(FailReason::NoOpencv, input.sync_params.initial_offset);
            log::info!(target: "sync", "[optical] seg {}: failed reason={}", i, FailReason::NoOpencv.as_str());
            o
        };
        rows.push(((from + to) as f64 / 2.0 / 1000.0, outcome.offset_ms, outcome.cost_px, outcome.conf));
        progress((i + 1) as f64 / n as f64);
    }
    if cancel.load(Relaxed) { return None; }
    Some(rows)
}

/// One quaternion table per search interval, in interval order: interval `(a, b)` gets the gyro times of the row-time
/// span `(lo, hi)` minus every offset in `[a − TABLE_MARGIN_MS, b + TABLE_MARGIN_MS]` (gyro time = video time − offset).
/// A single table across two disjoint intervals would also sample the gap between them, which grows with |initial
/// offset|; this way the memory follows the intervals' and the span's lengths only.
fn interval_tables(quats: &TimeQuat, (lo, hi): (f64, f64), intervals: &[(f64, f64)]) -> Vec<QuatTable> {
    intervals.iter().map(|&(a, b)| QuatTable::build(quats, lo - b - TABLE_MARGIN_MS, hi - a + TABLE_MARGIN_MS)).collect()
}

/// Index of the interval nearest to offset `d` (the one holding it, else the first of the equally near ones). The
/// search stays within TABLE_MARGIN_MS of an interval, so that interval's table covers `d`.
fn table_for(intervals: &[(f64, f64)], d: f64) -> usize {
    let distance = |&(a, b): &(f64, f64)| (a - d).max(d - b).max(0.0);
    (0..intervals.len()).min_by(|&x, &y| distance(&intervals[x]).total_cmp(&distance(&intervals[y]))).unwrap_or(0)
}

/// `run_search` fails as no_gyro_overlap when no coarse grid point could be evaluated, which also happens when the gyro
/// data is there but no band could be fitted (too few long tracks). `covered` tells whether the window's row-time span
/// minus at least one grid offset lies inside the gyro data; when it does, the failure is few_measurements.
fn relabel_unfitted(mut outcome: SearchOutcome, covered: impl FnOnce() -> bool) -> SearchOutcome {
    if outcome.fail == Some(FailReason::NoGyroOverlap) && covered() {
        outcome.fail = Some(FailReason::FewMeasurements);
    }
    outcome
}

/// The search of one window, logged; None when cancelled.
///
/// Set up as spec §6 asks: one quaternion table per search interval (`interval_tables`), the coarse scan takes
/// `cfg.coarse_points` points per pair by whole track segments and COARSE_IRLS_ROUNDS reweighting rounds, and a full
/// evaluation that is covered but fits no band counts as unmeasured. A coarse curve without a valid point is
/// no_gyro_overlap only when the gyro data covers none of the grid offsets (`relabel_unfitted`). In the log line,
/// `fine` is the summed wall time of the full evaluations (they run one after another) and `coarse` the rest of the
/// search's wall time.
fn solve_window(i: usize, w: &WindowTracks, input: &SolveInput, sg: &SgCache, pool: &rayon::ThreadPool, cancel: &AtomicBool) -> Option<SearchOutcome> {
    let sp = input.sync_params;
    let cfg = input.cfg;
    let p = SearchParams {
        init_ms: sp.initial_offset,
        search_ms: sp.search_size,
        check_negative: sp.initial_offset_inv && sp.initial_offset.abs() > 1.0,
        step_ms: cfg.coarse_step_ms,
        g_full: cfg.g_full,
        total_pairs: w.pairs.len(),
    };
    let fine_ns = AtomicU64::new(0);
    let started = Instant::now();
    let (outcome, coarse_points) = match row_time_span_ms(w) {
        Some((lo, hi)) => {
            let intervals = search_intervals(p.init_ms, p.search_ms, p.check_negative);
            let tables = interval_tables(input.quats, (lo, hi), &intervals);
            let ctxs: Vec<CostContext> = tables.iter().map(|table| CostContext { window: w, quats: table, sg }).collect();
            let ctx = |d: f64| &ctxs[table_for(&intervals, d)];
            let subset = select_tracks(w, cfg.coarse_points);
            let coarse = |d: f64| eval_coarse(ctx(d), &subset, d, COARSE_IRLS_ROUNDS);
            let full = |d: f64| {
                let t = Instant::now();
                let r = eval_full(ctx(d), d).filter(|r| r.bands > 0).map(|r| FullEval { cost: r.cost_px, pairs_measured: r.pairs_measured });
                fine_ns.fetch_add(t.elapsed().as_nanos() as u64, Relaxed);
                r
            };
            let outcome = run_search(&p, &coarse, &full, pool, cancel)?;
            // Gyro time = video time - offset; `covers` is about the gyro data, the same for every table
            let covered = || grid(&intervals, p.step_ms).iter().any(|&d| tables[table_for(&intervals, d)].covers(lo - d, hi - d));
            (relabel_unfitted(outcome, covered), subset.per_pair.iter().map(Vec::len).sum::<usize>())
        }
        // No observation at all, so no time span to build a table over and nothing to cover (the cost's rule for an
        // empty span): with pairs but no observations the failure is few_measurements. Without pairs (a window that
        // received no frames) the search fails as window_too_short before evaluating anything.
        None => (relabel_unfitted(run_search(&p, &|_| None, &|_| None, pool, cancel)?, || true), 0),
    };
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
    let fine_ms = fine_ns.load(Relaxed) as f64 / 1e6;

    match outcome.fail {
        Some(reason) => log::info!(target: "sync", "[optical] seg {}: failed reason={}", i, reason.as_str()),
        None => {
            let pairs = w.pairs.len();
            let per_pair = |n: usize| n as f64 / pairs.max(1) as f64;
            let points = w.pairs.iter().map(|pd| pd.ids.len()).sum::<usize>();
            log::info!(target: "sync",
                "[optical] seg {}: offset={:.2} conf={:.3} G={:.3} second={} cost={:.3}px cands={} near={} pairs={} pts/pair={:.1} coarse_pts/pair={:.1} coarse={:.0}ms fine={:.0}ms",
                i, outcome.offset_ms, outcome.conf, outcome.g,
                outcome.second_ms.map_or("-".to_string(), |s| format!("{:.1}ms", s)),
                outcome.cost_px, outcome.far_candidates,
                outcome.near_ms.map_or("-".to_string(), |s| format!("{:+.1}", s)),
                pairs, per_pair(points), per_pair(coarse_points), (elapsed_ms - fine_ms).max(0.0), fine_ms);
        }
    }
    if sync_diag::is_enabled() {
        sync_diag::record_optical_curve(i, false, &outcome.coarse);
        sync_diag::record_optical_curve(i, true, &outcome.fine);
    }
    Some(outcome)
}

/// Min / max row time over every observation of the window, video ms; None without observations
fn row_time_span_ms(w: &WindowTracks) -> Option<(f64, f64)> {
    let (lo, hi) = w.pairs.iter()
        .flat_map(|pd| pd.fa.iter().map(|&f| row_time_ms(&pd.a, f)).chain(pd.fb.iter().map(|&f| row_time_ms(&pd.b, f))))
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), t| (lo.min(t), hi.max(t)));
    (lo <= hi).then_some((lo, hi))
}

/// The `[optical] run:` line of a finished run (target `sync`). `track_ms` and `decode_wait_ms` as in
/// `SessionTimings`; `search_ms` is the wall time of `solve_windows`.
pub fn log_run(windows: usize, timings: &SessionTimings, search_ms: f64) {
    log::info!(target: "sync", "[optical] run: windows={} track_ms={:.0} decode_wait_ms={:.0} search_ms={:.0}",
        windows, timings.track_ms, timings.decode_wait_ms, search_ms);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering::SeqCst;
    use crate::synchronization::optical_motion::testutil::{ synth_window, SynthSpec };

    /// First call after a reset: no observations; every later call: 30 observations with ids 0..30. `calls` counts
    /// the frames tracked.
    struct FakeTracker { n: u32, calls: Arc<AtomicUsize> }
    impl FrameTracker for FakeTracker {
        fn reset(&mut self) { self.n = 0; }
        fn track(&mut self, _img: &GrayImage) -> Result<(Vec<Observation>, (u32, u32)), String> {
            self.n += 1;
            self.calls.fetch_add(1, SeqCst);
            let obs = if self.n == 1 {
                Vec::new()
            } else {
                (0..30).map(|id| Observation { id, a: [id as f32, 10.0], b: [id as f32 + 1.0, 10.0] }).collect()
            };
            Ok((obs, (960, 540)))
        }
    }
    fn fake() -> Option<Box<dyn FrameTracker>> { Some(Box::new(FakeTracker { n: 0, calls: Default::default() })) }
    fn counter(done: Arc<AtomicUsize>) -> Arc<dyn Fn() + Send + Sync> { Arc::new(move || { done.fetch_add(1, SeqCst); }) }
    fn session(windows: usize, done: Arc<AtomicUsize>) -> OpticalSession {
        OpticalSession::new(windows, 50.0, 1, Arc::new(AtomicBool::new(false)), &fake, counter(done))
    }
    fn img() -> Arc<GrayImage> { Arc::new(GrayImage::new(8, 8)) }
    fn cfg() -> OpticalConfig { OpticalConfig { g_full: 2.0, replenish: 0.85, coarse_step_ms: 10.0, coarse_points: 200, track_width: 960 } }
    fn empty() -> WindowTracks { WindowTracks { pairs: Vec::new(), focal_px: 1500.0 } }
    fn pool() -> rayon::ThreadPool { rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap() }

    #[test] fn frames_become_pairs_per_window() {
        let done = Arc::new(AtomicUsize::new(0));
        let s = session(2, done.clone());
        for i in 0..10 { s.feed(0, i * 20_000, img()); }
        for i in 100..105 { s.feed(1, i * 20_000, img()); }
        let raw = s.finish();
        assert_eq!((raw[0].pairs.len(), raw[1].pairs.len(), done.load(SeqCst)), (9, 4, 15));
        assert_eq!((raw[0].track_size, raw[0].pairs[0].obs.len()), ((960, 540), 30));
        assert_eq!((raw[1].pairs[0].a.index, raw[1].pairs[0].b.index, raw[1].pairs[0].b.ts_ms), (100, 101, 2020.0));
    }
    #[test] fn decode_retry_restarts_the_window() {
        let s = session(1, Default::default());
        for i in 0..6 { s.feed(0, i * 20_000, img()); }
        for i in 0..10 { s.feed(0, i * 20_000, img()); }       // software retry from the start
        let raw = s.finish();
        assert_eq!((raw[0].restarts, raw[0].pairs.len()), (1, 9));
    }
    #[test] fn overlapping_windows_both_receive_frames() {
        assert_eq!(windows_containing(&[(0, 1_500_000), (1_000_000, 2_500_000)], 1_200_000), vec![0, 1]);
        assert_eq!(windows_containing(&[(0, 1_500_000), (1_000_000, 2_500_000)], 2_000_000), vec![1]);
        assert!(windows_containing(&[(0, 1_500_000)], 1_600_000).is_empty());
        assert_eq!(windows_containing(&[(0, 1_500_000)], 1_500_000), vec![0]);   // inclusive, like the existing window test
    }
    #[test] fn overlapping_ranges_in_decoder_order_keep_both_windows() {
        // Windows 200..600 ms and 400..800 ms at 50 fps: frames 10..=30 and 20..=40. The decoder delivers the first
        // range, whose frames 20..=30 go to both windows, then seeks back to the second range's start and delivers
        // it, the overlap a second time
        let ranges = [(200_000, 600_000), (400_000, 800_000)];
        let done = Arc::new(AtomicUsize::new(0));
        let calls = Arc::new(AtomicUsize::new(0));
        let make = || -> Option<Box<dyn FrameTracker>> { Some(Box::new(FakeTracker { n: 0, calls: calls.clone() })) };
        let s = OpticalSession::new(2, 50.0, 1, Arc::new(AtomicBool::new(false)), &make, counter(done.clone()));
        let mut jobs = 0;
        for (from, to) in ranges {
            for i in from / 20_000..=to / 20_000 {
                for w in windows_containing(&ranges, i * 20_000) {
                    s.feed(w, i * 20_000, img());
                    jobs += 1;
                }
            }
        }
        let raw = s.finish();
        assert_eq!((jobs, done.load(SeqCst)), (64, 64));   // every frame reported done, the skipped ones too
        assert_eq!(calls.load(SeqCst), 64 - 11);              // the overlap delivered again never reaches window 0's tracker
        let pairs = |w: &RawWindow| w.pairs.iter().map(|p| (p.a.index, p.b.index)).collect::<Vec<_>>();
        let consecutive = |from: usize, to: usize| (from..to).map(|i| (i, i + 1)).collect::<Vec<_>>();
        assert_eq!(pairs(&raw[0]), consecutive(10, 30));
        assert_eq!((raw[0].restarts, raw[0].duplicates), (0, 11));
        assert_eq!(pairs(&raw[1]), consecutive(20, 40));
        assert_eq!((raw[1].restarts, raw[1].duplicates), (1, 0));
    }
    #[test] fn cancelled_session_drains_without_tracking() {
        let done = Arc::new(AtomicUsize::new(0));
        let cancel = Arc::new(AtomicBool::new(true));
        let s = OpticalSession::new(2, 50.0, 1, cancel, &fake, counter(done.clone()));
        // More frames than both queues hold: feed must not block for good
        for i in 0..40 { s.feed((i % 2) as usize, i * 20_000, img()); }
        let raw = s.finish();
        assert_eq!(done.load(SeqCst), 40);
        assert!(raw.iter().all(|w| w.pairs.is_empty() && w.frames == 0));
    }
    #[test] fn finish_twice_is_harmless() {
        let done = Arc::new(AtomicUsize::new(0));
        let s = session(2, done.clone());
        for i in 0..5 { s.feed(0, i * 20_000, img()); }
        assert_eq!(s.finish()[0].pairs.len(), 4);
        let again = s.finish();
        assert_eq!(again.len(), 2);
        assert!(again.iter().all(|w| w.pairs.is_empty()));
        // A frame fed after finish is dropped, but still reported done
        s.feed(0, 5 * 20_000, img());
        assert_eq!(done.load(SeqCst), 6);
    }
    #[test] fn missing_tracker_session_records_frames_without_pairs() {
        let s = OpticalSession::new(1, 50.0, 1, Arc::new(AtomicBool::new(false)), &|| None, Arc::new(|| ()));
        assert!(!s.has_tracker());
        for i in 0..5 { s.feed(0, i * 20_000, img()); }
        let raw = s.finish();
        assert_eq!((raw[0].frames, raw[0].pairs.len()), (5, 0));
        assert!(session(1, Default::default()).has_tracker());
    }
    #[test] fn padded_rows_are_cropped_to_the_frame_width() {
        // 3 wide, stride 4: the 4th column is padding
        let padded = Arc::new(GrayImage::from_raw(4, 2, vec![1, 2, 3, 99, 4, 5, 6, 99]).unwrap());
        let c = crop_to_width(padded, 3);
        assert_eq!((c.dimensions(), c.as_raw().clone()), ((3, 2), vec![1, 2, 3, 4, 5, 6]));
        let exact = img();
        assert!(Arc::ptr_eq(&crop_to_width(exact.clone(), 8), &exact));   // nothing to crop: no copy
    }
    #[test] fn empty_windows_still_yield_rows() {
        let sp = SyncParams { initial_offset: 120.0, search_size: 5000.0, ..Default::default() };
        let quats = TimeQuat::new();
        let cfg = cfg();
        let input = SolveInput { ranges_us: &[(1_000_000, 2_500_000), (4_000_000, 5_500_000)], sync_params: &sp, quats: &quats, cfg: &cfg, has_tracker: true };
        let rows = solve_windows(&[empty(), empty()], &input, &pool(), &AtomicBool::new(false), &|_: f64| {}).unwrap();
        assert_eq!(rows, vec![(1750.0, 120.0, 0.0, 0.0), (4750.0, 120.0, 0.0, 0.0)]);
    }
    #[test] fn rows_follow_window_order_and_centres() {
        let (w0, quats) = synth_window(&SynthSpec::default());   // video time 5000..8000 ms, truth -700 ms
        let sp = SyncParams { initial_offset: 0.0, search_size: 5000.0, ..Default::default() };
        let cfg = cfg();
        let input = SolveInput { ranges_us: &[(5_000_000, 8_000_000), (10_000_000, 11_500_000)], sync_params: &sp, quats: &quats, cfg: &cfg, has_tracker: true };
        let progress = std::sync::Mutex::new(Vec::new());
        // All cores: the ±5 s coarse scan of a 3 s window is slow in the debug profile
        let all_cores = rayon::ThreadPoolBuilder::new().build().unwrap();
        let rows = solve_windows(&[w0, empty()], &input, &all_cores, &AtomicBool::new(false), &|p: f64| progress.lock().unwrap().push(p)).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, 6500.0);
        assert!((rows[0].1 + 700.0).abs() < 1.0 && rows[0].3 >= 0.9, "{:?}", rows[0]);
        assert_eq!(rows[1], (10750.0, 0.0, 0.0, 0.0));
        assert_eq!(*progress.lock().unwrap(), vec![0.0, 0.5, 1.0]);
    }
    #[test] fn disjoint_search_intervals_get_one_table_each() {
        // Video time 5000..6500 ms, truth -700 ms; short window and searches keep the debug profile fast
        let (w0, quats) = synth_window(&SynthSpec { duration_ms: 1500.0, ..Default::default() });
        let span = row_time_span_ms(&w0).unwrap();
        // One table: the span, an interval of 2·search and the margins, at STEP_MS (plus one sample of rounding slack)
        let bound = |search: f64| ((span.1 - span.0 + 2.0 * search + 2.0 * TABLE_MARGIN_MS) / QuatTable::STEP_MS).ceil() as usize + 2;
        // |init| = 1 h: two tables of about 23k samples each, where one table across both intervals took 14.4M (460 MB)
        let intervals = search_intervals(3_600_000.0, 5000.0, true);
        let tables = interval_tables(&quats, span, &intervals);
        assert_eq!(tables.len(), 2);
        assert!(tables.iter().all(|t| t.samples() <= bound(5000.0)), "{:?}", tables.iter().map(QuatTable::samples).collect::<Vec<_>>());
        // Merged intervals keep a single table, as before
        assert_eq!(interval_tables(&quats, span, &search_intervals(100.0, 1000.0, true)).len(), 1);
        // Each offset takes the table of its interval, margins included
        let two = [(-1500.0, -500.0), (500.0, 1500.0)];
        assert_eq!([-1545.0, -700.0, -455.0, 455.0, 1545.0, 0.0].map(|d| table_for(&two, d)), [0, 0, 0, 1, 1, 0]);

        // ±1000 ± 500 ms with the negative side: the truth lies in the interval below zero
        let sp = SyncParams { initial_offset: 1000.0, search_size: 500.0, initial_offset_inv: true, ..Default::default() };
        assert_eq!(search_intervals(sp.initial_offset, sp.search_size, true), two.to_vec());
        let cfg = cfg();
        let input = SolveInput { ranges_us: &[(5_000_000, 6_500_000)], sync_params: &sp, quats: &quats, cfg: &cfg, has_tracker: true };
        let all_cores = rayon::ThreadPoolBuilder::new().build().unwrap();
        let rows = solve_windows(&[w0], &input, &all_cores, &AtomicBool::new(false), &|_: f64| {}).unwrap();
        assert!((rows[0].1 + 700.0).abs() < 1.0 && rows[0].3 >= 0.9, "{:?}", rows[0]);
    }
    #[test] fn unfittable_window_is_few_measurements_unless_uncovered() {
        // 20 tracks: every band has fewer than MIN_BAND_POINTS points, so no coarse grid point gets a cost even where
        // the gyro data covers the window
        let (w, quats) = synth_window(&SynthSpec { tracks: 20, ..Default::default() });
        let cfg = cfg();
        let solve = |init: f64| {
            let sp = SyncParams { initial_offset: init, search_size: 1000.0, ..Default::default() };
            let input = SolveInput { ranges_us: &[(5_000_000, 8_000_000)], sync_params: &sp, quats: &quats, cfg: &cfg, has_tracker: true };
            solve_window(0, &w, &input, &SgCache::new(), &pool(), &AtomicBool::new(false)).unwrap()
        };
        // Gyro data spans -8000..20000 ms of gyro time and the window about 5000..8000 ms of video time
        let o = solve(0.0);
        assert_eq!((o.fail, o.offset_ms, o.conf), (Some(FailReason::FewMeasurements), 0.0, 0.0));
        assert!(!o.coarse.is_empty() && o.coarse.iter().all(|c| c.1.is_nan()));
        // Partly covered: only some grid offsets are inside the gyro data, which is enough
        assert_eq!(solve(-12000.0).fail, Some(FailReason::FewMeasurements));
        // Every grid offset puts the window before the gyro start
        let o = solve(30000.0);
        assert_eq!((o.fail, o.offset_ms, o.conf), (Some(FailReason::NoGyroOverlap), 30000.0, 0.0));
        // Other failures keep their reason; an uncovered one stays no_gyro_overlap
        assert!(relabel_unfitted(failed(FailReason::Edge, 0.0), || true).fail == Some(FailReason::Edge));
        assert!(relabel_unfitted(failed(FailReason::NoGyroOverlap, 0.0), || false).fail == Some(FailReason::NoGyroOverlap));
    }
    #[test] fn missing_tracker_reports_no_opencv() {
        let (w0, quats) = synth_window(&SynthSpec { duration_ms: 1000.0, tracks: 50, ..Default::default() });
        let sp = SyncParams { initial_offset: -33.0, search_size: 5000.0, ..Default::default() };
        let cfg = cfg();
        let input = SolveInput { ranges_us: &[(5_000_000, 6_000_000), (7_000_000, 8_000_000)], sync_params: &sp, quats: &quats, cfg: &cfg, has_tracker: false };
        let rows = solve_windows(&[w0, empty()], &input, &pool(), &AtomicBool::new(false), &|_: f64| {}).unwrap();
        assert_eq!(rows, vec![(5500.0, -33.0, 0.0, 0.0), (7500.0, -33.0, 0.0, 0.0)]);
    }
    #[test] fn cancelled_search_returns_none() {
        let (w0, quats) = synth_window(&SynthSpec { duration_ms: 1000.0, tracks: 50, ..Default::default() });
        let sp = SyncParams { search_size: 5000.0, ..Default::default() };
        let cfg = cfg();
        let input = SolveInput { ranges_us: &[(5_000_000, 6_000_000)], sync_params: &sp, quats: &quats, cfg: &cfg, has_tracker: true };
        assert!(solve_windows(&[w0], &input, &pool(), &AtomicBool::new(true), &|_: f64| {}).is_none());
    }
}
