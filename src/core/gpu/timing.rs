// SPDX-License-Identifier: GPL-3.0-or-later

use std::cell::{Cell, RefCell};
use std::time::Instant;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

pub const GPU_PASS_SAMPLE_INTERVAL: u64 = 8;

pub(crate) struct PassSamples {
    epoch: AtomicU64,
    calls: [AtomicU64; 4],
    last_us: [AtomicU64; 4],
}

impl Default for PassSamples {
    fn default() -> Self {
        Self {
            epoch: AtomicU64::new(u64::MAX),
            calls: std::array::from_fn(|_| AtomicU64::new(0)),
            last_us: std::array::from_fn(|_| AtomicU64::new(u64::MAX)),
        }
    }
}

impl PassSamples {
    pub(crate) fn should_sample(&self, plane: usize) -> bool {
        let epoch = EXPORT_EPOCH.with(Cell::get);
        if self.epoch.swap(epoch, Relaxed) != epoch {
            for plane in 0..4 {
                self.calls[plane].store(0, Relaxed);
                self.last_us[plane].store(u64::MAX, Relaxed);
            }
        }
        self.calls[plane].fetch_add(1, Relaxed) % GPU_PASS_SAMPLE_INTERVAL == 0
    }

    pub(crate) fn record(&self, plane: usize, us: Option<u64>) {
        self.last_us[plane].store(us.unwrap_or(u64::MAX), Relaxed);
    }

    pub(crate) fn estimate(&self, plane: usize) -> Option<u64> {
        let us = self.last_us[plane].load(Relaxed);
        (us != u64::MAX).then_some(us)
    }
}

pub(crate) enum Stage { Upload, Wait, Readback }

pub(crate) struct StageTimer(Stage, Instant);

impl StageTimer {
    pub(crate) fn new(stage: Stage) -> Self { Self(stage, Instant::now()) }
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        let us = self.1.elapsed().as_micros() as u64;
        let mut times = GpuStageTimes::default();
        match self.0 {
            Stage::Upload => times.upload_us = us,
            Stage::Wait => { times.gpu_wait_us = us; times.gpu_wait_count = 1; }
            Stage::Readback => times.readback_us = us,
        }
        add(times);
    }
}

/// CPU-side stage totals for the current thread, in microseconds.
/// GPU pass time is a subset of the wait, never an additional wall-time stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuStageTimes {
    pub stab_data_us: u64,
    pub upload_us: u64,
    pub gpu_wait_us: u64,
    pub readback_us: u64,
    pub gpu_pass_us: u64,
    pub gpu_pass_available: bool,
    pub gpu_wait_count: u64,
    pub gpu_pass_samples: u64,
    pub gpu_pass_count: u64,
}

thread_local! {
    static STAGES: Cell<GpuStageTimes> = Cell::new(GpuStageTimes::default());
    static DEVICE: RefCell<(&'static str, String)> = RefCell::new(("", String::new()));
    static EXPORT_EPOCH: Cell<u64> = const { Cell::new(0) };
}

/// Cached wrappers must take a fresh sample instead of borrowing one from a previous export.
pub fn begin_export_timing() {
    take_gpu_stage_times();
    EXPORT_EPOCH.with(|epoch| epoch.set(epoch.get().wrapping_add(1)));
}

pub(crate) fn record_device(backend: &'static str, name: &str) {
    DEVICE.with(|device| {
        let mut device = device.borrow_mut();
        if device.0 != backend || device.1 != name {
            *device = (backend, name.to_owned());
        }
    });
}

pub(crate) fn add(times: GpuStageTimes) {
    STAGES.with(|cell| {
        let mut total = cell.get();
        total.stab_data_us = total.stab_data_us.saturating_add(times.stab_data_us);
        total.upload_us = total.upload_us.saturating_add(times.upload_us);
        total.gpu_wait_us = total.gpu_wait_us.saturating_add(times.gpu_wait_us);
        total.readback_us = total.readback_us.saturating_add(times.readback_us);
        total.gpu_pass_us = total.gpu_pass_us.saturating_add(times.gpu_pass_us);
        total.gpu_wait_count = total.gpu_wait_count.saturating_add(times.gpu_wait_count);
        total.gpu_pass_samples = total.gpu_pass_samples.saturating_add(times.gpu_pass_samples);
        total.gpu_pass_count = total.gpu_pass_count.saturating_add(times.gpu_pass_count);
        total.gpu_pass_available |= times.gpu_pass_available;
        cell.set(total);
    });
}

/// Drain only this thread; preview and plugin callers do not emit export logs.
pub fn take_gpu_stage_times() -> GpuStageTimes {
    STAGES.with(|cell| cell.replace(GpuStageTimes::default()))
}

pub fn device_name(backend: &str) -> String {
    if backend == "cpu" { return "CPU".into(); }
    DEVICE.with(|device| {
        let device = device.borrow();
        if device.0 == backend { device.1.clone() } else { String::new() }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_stage_timing_samples_each_plane_independently() {
        let samples = PassSamples::default();
        for frame in 0..19 {
            for plane in 0..3 {
                assert_eq!(samples.should_sample(plane), frame % GPU_PASS_SAMPLE_INTERVAL == 0);
                if frame % GPU_PASS_SAMPLE_INTERVAL == 0 {
                    samples.record(plane, Some(frame + plane as u64));
                }
                assert_eq!(samples.estimate(plane), Some(frame / GPU_PASS_SAMPLE_INTERVAL * GPU_PASS_SAMPLE_INTERVAL + plane as u64));
            }
        }
        samples.record(1, None);
        assert_eq!(samples.estimate(1), None);
        assert_eq!(samples.estimate(0), Some(16));
        begin_export_timing();
        assert!(samples.should_sample(0));
        assert_eq!(samples.estimate(0), None);
        assert!(samples.should_sample(1));
    }

    #[test]
    fn export_stage_timing_accumulates_and_isolates_threads() {
        take_gpu_stage_times();
        let sample = GpuStageTimes { upload_us: 7, gpu_wait_us: 11, gpu_pass_available: true, ..Default::default() };
        add(sample);
        std::thread::spawn(move || {
            assert_eq!(take_gpu_stage_times(), GpuStageTimes::default());
            add(GpuStageTimes { readback_us: 19, ..Default::default() });
            assert_eq!(take_gpu_stage_times().readback_us, 19);
        }).join().unwrap();
        add(sample);
        let total = take_gpu_stage_times();
        assert_eq!(total.upload_us, 14);
        assert_eq!(total.gpu_wait_us, 22);
        assert_eq!(total.readback_us, 0);
        assert!(total.gpu_pass_available);
        assert_eq!(take_gpu_stage_times(), GpuStageTimes::default());
    }
}
