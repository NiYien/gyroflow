// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! Runs an `OpticalMotionAnalysis` as a pipeline. The caller's (decoder) thread hands over grayscale frames, two
//! workers prepare them (the pyramid and what a replenish needs of them, which doesn't depend on the tracks), one
//! thread tracks them and one measures what was tracked. Every stage handles the frames in decoding order, with the
//! same inputs as `feed_frame`, so the measurements are the same as feeding the frames one by one.

use std::sync::{ Arc, Mutex, atomic::{ AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed } };
use std::sync::mpsc::{ sync_channel, Receiver, RecvTimeoutError, SyncSender };
use std::thread::JoinHandle;
use std::time::{ Duration, Instant };

use super::{ CancelCheck, FrameSequencer, OpticalMeasurements, OpticalMotionAnalysis, TRACK_WIDTH };
use super::super::optical_motion::{ FrameTracker, tracker::{ KltTracker, PreparedFrame } };
use super::super::optical_motion::tracks::Observation;
use crate::synchronization::GrayImage;

/// Frames prepared side by side: preparing one takes about as long as decoding two, tracking about half of that
pub(super) const PREPARE_WORKERS: usize = 2;
/// Frames waiting for, and coming out of, each preparing worker
pub(super) const PREPARE_QUEUE: usize = 2;
/// Tracked frames waiting to be measured: the measurement runs every `CHUNK` pairs and takes a while
pub(super) const MEASURE_QUEUE: usize = 128;

/// A frame the decoder handed over, in decoding order
struct Job {
    index: usize,
    ts_ms: f64,
    continuous: bool,
    size: (u32, u32),
    /// None: fewer pixels than the frame size says. The frame is then measured without any tracks
    image: Option<GrayImage>,
}

struct Prepared {
    job: Job,
    frame: Option<Result<PreparedFrame, String>>,
}

struct Tracked {
    index: usize,
    ts_ms: f64,
    size: (u32, u32),
    obs: Vec<Observation>,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    error: Mutex<Option<String>>,
    measured_frames: AtomicUsize,
    /// Nanoseconds: the decoder blocked on full queues; preparing; tracking; measuring
    blocked_ns: AtomicU64,
    prepare_ns: AtomicU64,
    track_ns: AtomicU64,
    measure_ns: AtomicU64,
    /// Holds the measuring stage, to see the queues fill up
    #[cfg(test)]
    hold_measuring: AtomicBool,
}

impl Shared {
    /// Keeps the first error and stops every stage
    fn fail(&self, e: String) {
        self.error.lock().unwrap().get_or_insert(e);
        self.stop.store(true, Relaxed);
    }
    fn add(counter: &AtomicU64, since: Instant) {
        counter.fetch_add(since.elapsed().as_nanos() as u64, Relaxed);
    }
}

/// The decoder's end: what to decode and where the frames go. Dropping it tells the stages no more frames come
pub struct FrameSink {
    sequencer: FrameSequencer,
    senders: Vec<SyncSender<Job>>,
    next: usize,
    total_frames: usize,
    cancel: CancelCheck,
    shared: Arc<Shared>,
    decode_end: Arc<Mutex<Option<Instant>>>,
}

impl FrameSink {
    /// Whether the frame at this time is analyzed, see `OpticalMotionAnalysis::wants_frame`
    pub fn wants_frame(&self, timestamp_us: i64) -> bool { self.sequencer.wants_frame(timestamp_us) }

    /// Cancelled, or a stage failed: nothing more is needed from the decoder
    pub fn stopped(&self) -> bool { self.shared.stop.load(Relaxed) || self.cancel.is_cancelled() }

    /// Frames measured so far and to be analyzed, like `OpticalMotionAnalysis::progress`
    pub fn progress(&self) -> (usize, usize) {
        let ready = self.shared.measured_frames.load(Relaxed);
        (ready, self.total_frames.max(ready))
    }

    /// Hands over the next decoded frame, 8-bit luma, like `OpticalMotionAnalysis::feed_frame`. Waits while the
    /// stages are busy. Err once the analysis stopped: the decoder can stop too, the reason comes from `join`
    pub fn push(&mut self, timestamp_us: i64, width: u32, height: u32, stride: usize, pixels: &[u8]) -> Result<(), String> {
        if self.stopped() { return Err("Stopped".into()); }
        let Some(slot) = self.sequencer.accept(timestamp_us) else { return Ok(()) };
        let image = if pixels.len() < stride * height as usize {
            None
        } else {
            let mut packed = Vec::with_capacity(width as usize * height as usize);
            for row in 0..height as usize {
                let start = row * stride;
                packed.extend_from_slice(&pixels[start..start + width as usize]);
            }
            match GrayImage::from_raw(width, height, packed) {
                Some(image) => Some(image),
                None => {
                    let e = "Invalid frame size".to_string();
                    self.shared.fail(e.clone());
                    return Err(e);
                }
            }
        };
        let job = Job { index: slot.index, ts_ms: slot.ts_ms, continuous: slot.continuous, size: (width, height), image };
        let worker = self.next % self.senders.len();
        self.next += 1;
        let blocked = Instant::now();
        let sent = self.senders[worker].send(job);
        Shared::add(&self.shared.blocked_ns, blocked);
        sent.map_err(|_| "Stopped".to_string())
    }
}

impl Drop for FrameSink {
    fn drop(&mut self) {
        self.decode_end.lock().unwrap().get_or_insert_with(Instant::now);
    }
}

/// The running stages. `join` waits for them to finish what was handed over; dropping it stops them first
pub struct AnalysisPipeline {
    started: Instant,
    shared: Arc<Shared>,
    decode_end: Arc<Mutex<Option<Instant>>>,
    workers: Vec<JoinHandle<()>>,
    tracker: Option<JoinHandle<()>>,
    measurer: Option<JoinHandle<OpticalMotionAnalysis>>,
}

/// What the stages left: the analysis with everything measured, and the first error of any of them
pub struct PipelineOutcome {
    pub analysis: OpticalMotionAnalysis,
    pub error: Option<String>,
    started: Instant,
    decode_end: Option<Instant>,
    shared: Arc<Shared>,
}

impl AnalysisPipeline {
    pub fn start(analysis: OpticalMotionAnalysis) -> Result<(Self, FrameSink), String> {
        let started = analysis.started;
        let shared = Arc::new(Shared::default());
        let decode_end = Arc::new(Mutex::new(None));
        let cancel = analysis.cancel.clone();
        let track_width = TRACK_WIDTH;

        let mut senders = Vec::with_capacity(PREPARE_WORKERS);
        let mut prepared_rx = Vec::with_capacity(PREPARE_WORKERS);
        let mut workers = Vec::with_capacity(PREPARE_WORKERS);
        for i in 0..PREPARE_WORKERS {
            let (tx, rx) = sync_channel::<Job>(PREPARE_QUEUE);
            let (out_tx, out_rx) = sync_channel::<Prepared>(PREPARE_QUEUE);
            let (shared, cancel) = (shared.clone(), cancel.clone());
            let worker = std::thread::Builder::new().name(format!("optical-prepare-{i}"))
                .spawn(move || prepare_stage(rx, out_tx, track_width, &shared, &cancel))
                .map_err(|e| format!("Cannot start the optical analysis: {e}"))?;
            senders.push(tx);
            prepared_rx.push(out_rx);
            workers.push(worker);
        }
        let (tracked_tx, tracked_rx) = sync_channel::<Tracked>(MEASURE_QUEUE);
        let tracker = {
            let (shared, cancel) = (shared.clone(), cancel.clone());
            let tracker = OpticalMotionAnalysis::new_tracker();
            std::thread::Builder::new().name("optical-track".into())
                .spawn(move || track_stage(prepared_rx, tracked_tx, tracker, &shared, &cancel))
                .map_err(|e| format!("Cannot start the optical analysis: {e}"))?
        };
        let sequencer = analysis.sequencer.clone();
        let total_frames = analysis.progress().1;
        let measurer = {
            let shared = shared.clone();
            std::thread::Builder::new().name("optical-measure".into())
                .spawn(move || measure_stage(tracked_rx, analysis, &shared))
                .map_err(|e| format!("Cannot start the optical analysis: {e}"))?
        };
        let sink = FrameSink { sequencer, senders, next: 0, total_frames, cancel, shared: shared.clone(), decode_end: decode_end.clone() };
        Ok((Self { started, shared, decode_end, workers, tracker: Some(tracker), measurer: Some(measurer) }, sink))
    }

    /// Waits until the stages have handled every frame handed over (the sink must be dropped by then, or they
    /// stopped). A panicking stage counts as an error
    pub fn join(mut self) -> Result<PipelineOutcome, String> {
        let analysis = self.join_all();
        let error = self.shared.error.lock().unwrap().clone();
        let decode_end = *self.decode_end.lock().unwrap();
        match analysis {
            Some(analysis) => Ok(PipelineOutcome { analysis, error, started: self.started, decode_end, shared: self.shared.clone() }),
            None => Err(error.unwrap_or_else(|| "The optical analysis stopped unexpectedly".into())),
        }
    }

    fn join_all(&mut self) -> Option<OpticalMotionAnalysis> {
        let mut panicked = false;
        for worker in self.workers.drain(..) { panicked |= worker.join().is_err(); }
        if let Some(tracker) = self.tracker.take() { panicked |= tracker.join().is_err(); }
        let analysis = self.measurer.take().and_then(|m| m.join().ok());
        if panicked || analysis.is_none() {
            self.shared.fail("The optical analysis stopped unexpectedly".into());
        }
        analysis
    }
}

#[cfg(test)]
impl AnalysisPipeline {
    pub(super) fn hold_measuring(&self, hold: bool) { self.shared.hold_measuring.store(hold, Relaxed); }
}

impl Drop for AnalysisPipeline {
    fn drop(&mut self) {
        if self.measurer.is_some() {
            self.shared.stop.store(true, Relaxed);
            self.join_all();
        }
    }
}

impl PipelineOutcome {
    pub fn is_cancelled(&self) -> bool { self.analysis.is_cancelled() }

    /// Measures what's left, see `OpticalMotionAnalysis::finish`, and logs the speed of the analysis
    pub fn finish(self) -> Result<OpticalMeasurements, String> {
        let PipelineOutcome { analysis, started, decode_end, shared, .. } = self;
        let frames = analysis.progress().0;
        let tail = Instant::now();
        let result = analysis.finish();
        log_speed(match &result { Ok(_) => "ok", Err(e) if e == "Cancelled" => "cancelled", Err(_) => "error" },
            frames, started, decode_end, Some(tail.elapsed()), &shared);
        result
    }

    /// Logs the speed of an analysis that ends without `finish`
    pub fn log_unfinished(&self, outcome: &str) {
        log_speed(outcome, self.analysis.progress().0, self.started, self.decode_end, None, &self.shared);
    }
}

fn log_speed(outcome: &str, frames: usize, started: Instant, decode_end: Option<Instant>, tail: Option<Duration>, shared: &Shared) {
    let wall = started.elapsed().as_secs_f64();
    let ms = |ns: &AtomicU64| ns.load(Relaxed) as f64 / 1e6;
    let decoded_ms = decode_end.map(|t| t.duration_since(started).as_secs_f64() * 1000.0);
    ::log::info!("Optical analysis speed: {outcome}, {frames} frames in {wall:.3} s ({:.2} fps), decoding ended at {} s; busy ms: decoder {} (waited {:.0} on full queues), prepare {:.0} ({PREPARE_WORKERS} workers), track {:.0}, measure {:.0}, final measure {}",
        frames as f64 / wall.max(1e-9),
        decoded_ms.map_or_else(|| "-".into(), |t| format!("{:.3}", t / 1000.0)),
        decoded_ms.map_or_else(|| "-".into(), |t| format!("{:.0}", t - ms(&shared.blocked_ns))),
        ms(&shared.blocked_ns), ms(&shared.prepare_ns), ms(&shared.track_ns), ms(&shared.measure_ns),
        tail.map_or_else(|| "-".into(), |t| format!("{:.0}", t.as_secs_f64() * 1000.0)));
}

/// Prepares each frame independently of the tracks: the pyramid, and what a replenish needs of the frame
fn prepare_stage(rx: Receiver<Job>, tx: SyncSender<Prepared>, track_width: u32, shared: &Shared, cancel: &CancelCheck) {
    loop {
        // Waits in short steps: a stop must end it even while the sink still exists
        let job = match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(job) => job,
            Err(RecvTimeoutError::Timeout) if !(shared.stop.load(Relaxed) || cancel.is_cancelled()) => continue,
            Err(_) => break,
        };
        if shared.stop.load(Relaxed) || cancel.is_cancelled() { break; }
        let started = Instant::now();
        let frame = job.image.as_ref().map(|image| KltTracker::prepare(image, track_width).map(|mut frame| {
            frame.precompute_corners();
            frame
        }));
        Shared::add(&shared.prepare_ns, started);
        if tx.send(Prepared { job, frame }).is_err() { break; }
    }
}

/// Tracks the frames in decoding order, taking them round-robin from the workers they were dealt to
fn track_stage(rx: Vec<Receiver<Prepared>>, tx: SyncSender<Tracked>, mut tracker: KltTracker, shared: &Shared, cancel: &CancelCheck) {
    for k in 0.. {
        let Ok(Prepared { job, frame }) = rx[k % rx.len()].recv() else { break };
        if shared.stop.load(Relaxed) || cancel.is_cancelled() { break; }
        let started = Instant::now();
        // As `feed_frame`: a gap starts new tracks, a frame without its pixels has none
        if !job.continuous { tracker.reset(); }
        let obs = match frame {
            None => Vec::new(),
            Some(frame) => match tracker.track_prepared(frame) {
                Ok((obs, tracked_size)) if tracked_size == job.size => obs,
                Ok((_, tracked_size)) => {
                    shared.fail(format!("Tracking size {:?} differs from input {:?}", tracked_size, job.size));
                    break;
                },
                Err(e) => { shared.fail(e); break; },
            },
        };
        Shared::add(&shared.track_ns, started);
        if tx.send(Tracked { index: job.index, ts_ms: job.ts_ms, size: job.size, obs }).is_err() { break; }
    }
}

/// Measures the tracked frames in decoding order; hands the analysis back for `finish`
fn measure_stage(rx: Receiver<Tracked>, mut analysis: OpticalMotionAnalysis, shared: &Shared) -> OpticalMotionAnalysis {
    for tracked in rx {
        #[cfg(test)]
        while shared.hold_measuring.load(Relaxed) && !shared.stop.load(Relaxed) { std::thread::sleep(Duration::from_millis(1)); }
        if shared.stop.load(Relaxed) || analysis.is_cancelled() { break; }
        let started = Instant::now();
        analysis.push_tracked_frame(tracked.index, tracked.ts_ms, tracked.size, tracked.obs);
        Shared::add(&shared.measure_ns, started);
        shared.measured_frames.store(analysis.progress().0, Relaxed);
    }
    analysis
}
