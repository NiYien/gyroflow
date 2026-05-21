// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien
//
// Env-gated diagnostic logging for batch-sync UI stutter triage. All emissions
// are on target="lifecycle" and prefixed with `batch_sync.` so triage greps
// can pull them out cleanly. When GYROFLOW_BATCH_SYNC_DIAG is unset or
// non-truthy every helper short-circuits to a no-op variant (constructor
// stores None, Drop sees None and does nothing). Each call site therefore
// pays at most one atomic-bool read when diagnostics are disabled.

use parking_lot::Mutex;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

static ENABLED: OnceLock<bool> = OnceLock::new();
static TRIGGER_THROTTLE: OnceLock<TriggerThrottle> = OnceLock::new();

fn parse_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

/// Returns whether GYROFLOW_BATCH_SYNC_DIAG enables the diagnostic loci.
/// Reads the env var exactly once, caches in an OnceLock. Truthy values are
/// `1`, `true`, `yes`, `on` (case-insensitive). Anything else, including the
/// var being unset, returns false.
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| match std::env::var("GYROFLOW_BATCH_SYNC_DIAG") {
        Ok(raw) => parse_truthy(&raw),
        Err(_) => false,
    })
}

/// Runs the closure only when diagnostics are enabled. Cheap shortcut for
/// instrumentation that should not allocate or measure when off.
#[inline]
pub fn run_if_enabled<F: FnOnce()>(f: F) {
    if enabled() {
        f();
    }
}

// --- Locus A: on_finished + lock acquire/hold spans ---------------------

pub struct OnFinishedSpan {
    inner: Option<OnFinishedInner>,
}

struct OnFinishedInner {
    job_id: u32,
    offsets: usize,
    start: Instant,
}

impl OnFinishedSpan {
    pub fn new(job_id: u32, offsets: usize) -> Self {
        if enabled() {
            log::info!(
                target: "lifecycle",
                "batch_sync.on_finished_start job={} offsets={}",
                job_id,
                offsets
            );
            Self {
                inner: Some(OnFinishedInner {
                    job_id,
                    offsets,
                    start: Instant::now(),
                }),
            }
        } else {
            Self { inner: None }
        }
    }
}

impl Drop for OnFinishedSpan {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let wall_ms = inner.start.elapsed().as_secs_f64() * 1e3;
            log::info!(
                target: "lifecycle",
                "batch_sync.on_finished_end job={} offsets={} wall_ms={:.3}",
                inner.job_id,
                inner.offsets,
                wall_ms
            );
        }
    }
}

pub struct LockAcquireSpan {
    inner: Option<LockAcquireInner>,
}

struct LockAcquireInner {
    kind: &'static str,
    job_id: u32,
    requested: Instant,
}

impl LockAcquireSpan {
    /// `kind` becomes the prefix of the emitted line, e.g. "gyro_write" →
    /// `batch_sync.gyro_write_acquired`. The span should be dropped right
    /// after the corresponding `.write()` returns so wait_ms measures the
    /// real wait-for-lock duration.
    pub fn new(kind: &'static str, job_id: u32) -> Self {
        if enabled() {
            Self {
                inner: Some(LockAcquireInner {
                    kind,
                    job_id,
                    requested: Instant::now(),
                }),
            }
        } else {
            Self { inner: None }
        }
    }
}

impl Drop for LockAcquireSpan {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let wait_ms = inner.requested.elapsed().as_secs_f64() * 1e3;
            log::info!(
                target: "lifecycle",
                "batch_sync.{}_acquired job={} wait_ms={:.3}",
                inner.kind,
                inner.job_id,
                wait_ms
            );
        }
    }
}

pub struct LockHoldSpan {
    inner: Option<LockHoldInner>,
}

struct LockHoldInner {
    kind: &'static str,
    job_id: u32,
    held_since: Instant,
}

impl LockHoldSpan {
    /// Construct immediately after the lock is acquired; drop at the point
    /// the lock guard is released so hold_ms measures actual lock-held time.
    pub fn new(kind: &'static str, job_id: u32) -> Self {
        if enabled() {
            Self {
                inner: Some(LockHoldInner {
                    kind,
                    job_id,
                    held_since: Instant::now(),
                }),
            }
        } else {
            Self { inner: None }
        }
    }
}

impl Drop for LockHoldSpan {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            let hold_ms = inner.held_since.elapsed().as_secs_f64() * 1e3;
            log::info!(
                target: "lifecycle",
                "batch_sync.{}_released job={} hold_ms={:.3}",
                inner.kind,
                inner.job_id,
                hold_ms
            );
        }
    }
}

// --- Locus B: progress callback rate sliding window ---------------------

pub struct ProgressRateWindow {
    job_id: u32,
    inner: Mutex<(Instant, u32)>,
}

impl ProgressRateWindow {
    pub fn new(job_id: u32) -> Self {
        Self {
            job_id,
            inner: Mutex::new((Instant::now(), 0)),
        }
    }

    /// Counts one progress event. Emits a `batch_sync.progress_rate` line
    /// and resets the window when ≥1 second has elapsed since window_start.
    /// The owning Option<Arc<...>> already gates construction on enabled(),
    /// but the inner check guards against an in-flight ENABLED flip.
    pub fn tick(&self) {
        if !enabled() {
            return;
        }
        let mut g = self.inner.lock();
        g.1 = g.1.saturating_add(1);
        let elapsed = g.0.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let count = g.1;
            let secs = elapsed.as_secs_f64();
            let rate = if secs > 0.0 { count as f64 / secs } else { 0.0 };
            log::info!(
                target: "lifecycle",
                "batch_sync.progress_rate job={} count={} rate_hz={:.2}",
                self.job_id,
                count,
                rate
            );
            g.0 = Instant::now();
            g.1 = 0;
        }
    }
}

// --- Locus C: UI thread queued-event latency ---------------------------

pub struct UiLatencyAccumulator {
    job_id: u32,
    kind: &'static str,
    inner: Mutex<(Instant, u32, u64, u64)>, // (window_start, count, sum_ns, max_ns)
}

impl UiLatencyAccumulator {
    pub fn new(job_id: u32, kind: &'static str) -> Self {
        Self {
            job_id,
            kind,
            inner: Mutex::new((Instant::now(), 0, 0, 0)),
        }
    }

    /// Records one queued-callback latency sample (now - enqueued_at).
    /// Emits a `batch_sync.ui_latency` line and resets when the window
    /// ≥1 second has elapsed.
    pub fn record(&self, enqueued_at: Instant) {
        if !enabled() {
            return;
        }
        let lat_ns = enqueued_at.elapsed().as_nanos() as u64;
        let mut g = self.inner.lock();
        g.1 = g.1.saturating_add(1);
        g.2 = g.2.saturating_add(lat_ns);
        if lat_ns > g.3 {
            g.3 = lat_ns;
        }
        let elapsed = g.0.elapsed();
        if elapsed >= Duration::from_secs(1) {
            let count = g.1;
            let avg_us = if count > 0 {
                (g.2 as f64 / count as f64) / 1e3
            } else {
                0.0
            };
            let max_us = g.3 as f64 / 1e3;
            log::info!(
                target: "lifecycle",
                "batch_sync.ui_latency job={} kind={} count={} avg_us={:.1} max_us={:.1}",
                self.job_id,
                self.kind,
                count,
                avg_us,
                max_us
            );
            g.0 = Instant::now();
            g.1 = 0;
            g.2 = 0;
            g.3 = 0;
        }
    }
}

/// RAII span for measuring a queued-callback consumer body's wall time.
/// Drop emits `batch_sync.ui_exec_ms`. Use one per queued-callback closure
/// so we can compare ui_latency (enqueue→start) against ui_exec_ms (body).
pub struct UiExecSpan {
    inner: Option<(u32, &'static str, Instant)>,
}

impl UiExecSpan {
    pub fn new(job_id: u32, kind: &'static str) -> Self {
        if enabled() {
            Self {
                inner: Some((job_id, kind, Instant::now())),
            }
        } else {
            Self { inner: None }
        }
    }
}

impl Drop for UiExecSpan {
    fn drop(&mut self) {
        if let Some((job_id, kind, start)) = self.inner.take() {
            let exec_ms = start.elapsed().as_secs_f64() * 1e3;
            log::info!(
                target: "lifecycle",
                "batch_sync.ui_exec_ms job={} kind={} exec_ms={:.3}",
                job_id,
                kind,
                exec_ms
            );
        }
    }
}

// --- Locus D: recompute trigger throttle + body span -------------------

pub struct TriggerThrottle {
    last: Mutex<Option<Instant>>,
    min_gap: Duration,
}

impl TriggerThrottle {
    fn new(min_gap_ms: u64) -> Self {
        Self {
            last: Mutex::new(None),
            min_gap: Duration::from_millis(min_gap_ms),
        }
    }

    /// Logs a `batch_sync.recompute_trigger` line at most every 200 ms.
    /// `last_gap_ms=-1` indicates the first call after process start.
    pub fn maybe_emit(&self, source: &str) {
        if !enabled() {
            return;
        }
        let now = Instant::now();
        let mut g = self.last.lock();
        let gap = match *g {
            Some(t) => now.duration_since(t),
            None => Duration::MAX,
        };
        if gap >= self.min_gap {
            let last_gap_ms = if g.is_some() {
                gap.as_secs_f64() * 1e3
            } else {
                -1.0
            };
            log::info!(
                target: "lifecycle",
                "batch_sync.recompute_trigger source={} last_gap_ms={:.1}",
                source,
                last_gap_ms
            );
            *g = Some(now);
        }
    }
}

/// Process-wide 200 ms-gated trigger throttle. One instance per process,
/// shared by every call site (currently just controller::recompute_threaded).
pub fn recompute_trigger_throttle() -> &'static TriggerThrottle {
    TRIGGER_THROTTLE.get_or_init(|| TriggerThrottle::new(200))
}

pub struct RecomputeBodySpan {
    inner: Option<(u64, Instant)>,
}

impl RecomputeBodySpan {
    pub fn new(compute_id: u64) -> Self {
        if enabled() {
            Self {
                inner: Some((compute_id, Instant::now())),
            }
        } else {
            Self { inner: None }
        }
    }
}

impl Drop for RecomputeBodySpan {
    fn drop(&mut self) {
        if let Some((compute_id, start)) = self.inner.take() {
            let wall_ms = start.elapsed().as_secs_f64() * 1e3;
            log::info!(
                target: "lifecycle",
                "batch_sync.recompute_body compute_id={} wall_ms={:.3}",
                compute_id,
                wall_ms
            );
        }
    }
}
