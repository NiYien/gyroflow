// SPDX-License-Identifier: GPL-3.0-or-later

use std::{cell::RefCell, rc::Rc, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};
use gyroflow_core::gpu::timing::{GpuStageTimes, take_gpu_stage_times};

#[derive(Clone, Copy)]
pub enum Stage { Demux, Decode, Download, Convert, Upload, Encode, Mux }

#[derive(Default)]
pub struct ExportTiming {
    start: Option<Instant>,
    wall: Option<Duration>,
    pub frames: u64,
    pub decoder: String,
    pub encoder: String,
    pub in_size: (usize, usize),
    pub out_size: (usize, usize),
    pub in_fmt: String,
    pub out_fmt: String,
    pub backend: String,
    pub device: String,
    demux_us: u64,
    decode_us: u64,
    download_us: u64,
    convert_us: u64,
    upload_us: u64,
    encode_us: u64,
    mux_us: u64,
    gpu: GpuStageTimes,
}

#[derive(Clone, Default)]
pub struct Timing(pub Rc<RefCell<ExportTiming>>);

impl Timing {
    pub fn add_stab_data(&self, elapsed: Duration) {
        self.0.borrow_mut().gpu.stab_data_us += elapsed.as_micros() as u64;
    }

    pub fn set_backend(&self, backend: &str) {
        let mut t = self.0.borrow_mut();
        if !t.backend.eq_ignore_ascii_case(backend) {
            let backend = backend.to_ascii_lowercase();
            t.device = gyroflow_core::gpu::timing::device_name(&backend);
            t.backend = backend;
        }
    }
}

pub struct StageTimer {
    timing: Option<Timing>,
    start: Option<Instant>,
    stage: Stage,
}

impl StageTimer {
    pub fn new(timing: &Option<Timing>, stage: Stage) -> Self {
        Self { timing: timing.clone(), start: timing.as_ref().map(|_| Instant::now()), stage }
    }
}

impl Drop for StageTimer {
    fn drop(&mut self) {
        if let (Some(timing), Some(start)) = (&self.timing, self.start) {
            let us = start.elapsed().as_micros() as u64;
            let mut t = timing.0.borrow_mut();
            let total = match self.stage {
                Stage::Demux => &mut t.demux_us, Stage::Decode => &mut t.decode_us,
                Stage::Download => &mut t.download_us, Stage::Convert => &mut t.convert_us,
                Stage::Upload => &mut t.upload_us, Stage::Encode => &mut t.encode_us,
                Stage::Mux => &mut t.mux_us,
            };
            *total = total.saturating_add(us);
        }
    }
}

pub fn measure<T>(timing: &Option<Timing>, stage: Stage, f: impl FnOnce() -> T) -> T {
    let _timer = StageTimer::new(timing, stage);
    f()
}

pub struct WallTimer(Option<Timing>);
impl WallTimer {
    pub fn new(timing: &Option<Timing>) -> Self {
        if let Some(timing) = timing { timing.0.borrow_mut().start = Some(Instant::now()); }
        Self(timing.clone())
    }
}
impl Drop for WallTimer {
    fn drop(&mut self) {
        if let Some(timing) = &self.0 {
            let mut t = timing.0.borrow_mut();
            t.wall = t.start.map(|start| start.elapsed());
        }
    }
}

pub struct FrameTimer(Timing);
impl FrameTimer {
    pub fn new(timing: &Timing) -> Self {
        take_gpu_stage_times();
        Self(timing.clone())
    }
}
impl Drop for FrameTimer {
    fn drop(&mut self) {
        let frame = take_gpu_stage_times();
        let mut t = self.0.0.borrow_mut();
        t.gpu.stab_data_us += frame.stab_data_us;
        t.gpu.upload_us += frame.upload_us;
        t.gpu.gpu_wait_us += frame.gpu_wait_us;
        t.gpu.readback_us += frame.readback_us;
        t.gpu.gpu_pass_us += frame.gpu_pass_us;
        t.gpu.gpu_wait_count += frame.gpu_wait_count;
        t.gpu.gpu_pass_samples += frame.gpu_pass_samples;
        t.gpu.gpu_pass_count += frame.gpu_pass_count;
        t.gpu.gpu_pass_available |= frame.gpu_pass_available;
    }
}

pub struct Attempt {
    pub timing: Timing,
    cancel: Arc<AtomicBool>,
    pub completed: bool,
}
impl Attempt {
    pub fn new(cancel: Arc<AtomicBool>) -> Self {
        gyroflow_core::gpu::timing::begin_export_timing();
        Self { timing: Timing::default(), cancel, completed: false }
    }
}

struct Summary {
    other_us: u64,
    overrun_us: u64,
    copy_us: u64,
    copy_share: f64,
    speedup_bound: f64,
    copy_kind: &'static str,
}

fn summarize(wall_us: u64, stages_us: u64, copy_base_us: u64, wait_us: u64, pass_us: Option<u64>) -> Summary {
    let copy_us = copy_base_us + pass_us.map(|pass| wait_us.saturating_sub(pass)).unwrap_or(0);
    let copy_share = if wall_us == 0 { 0.0 } else { copy_us as f64 / wall_us as f64 };
    Summary {
        other_us: wall_us.saturating_sub(stages_us), overrun_us: stages_us.saturating_sub(wall_us),
        copy_us, copy_share, speedup_bound: 1.0 / (1.0 - copy_share),
        copy_kind: if pass_us.is_some() { "split" } else { "lower_bound" },
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let t = self.timing.0.borrow();
        let status = if self.cancel.load(Ordering::Relaxed) { "cancelled" } else if self.completed { "ok" } else { "error" };
        let wall_us = t.wall.or_else(|| t.start.map(|start| start.elapsed())).unwrap_or_default().as_micros() as u64;
        let gpu = &t.gpu;
        let sum = t.demux_us + t.decode_us + t.download_us + t.convert_us + gpu.stab_data_us
            + gpu.upload_us + gpu.gpu_wait_us + gpu.readback_us + t.upload_us + t.encode_us + t.mux_us;
        let pass_us = (t.backend == "wgpu" && gpu.gpu_pass_available && gpu.gpu_pass_count == gpu.gpu_wait_count).then_some(gpu.gpu_pass_us);
        let summary = summarize(wall_us, sum, t.download_us + gpu.upload_us + gpu.readback_us + t.upload_us, gpu.gpu_wait_us, pass_us);
        if summary.overrun_us > 0 {
            log::warn!(target: "video.render", "Export timing stages exceed wall time by {:.3} ms; other_ms is zero", summary.overrun_us as f64 / 1000.0);
        }
        let ms = |us: u64| us as f64 / 1000.0;
        let pass = pass_us.map(|us| format!("{:.3}", ms(us))).unwrap_or_else(|| "na".into());
        fn unknown(s: &str) -> &str { if s.is_empty() { "unknown" } else { s } }
        let backend = if t.backend.is_empty() { "cpu" } else { &t.backend };
        log::info!(target: "video.render",
            "export.timing status={status} frames={} wall_ms={:.3} decoder={} encoder={} in_size={}x{} out_size={}x{} in_fmt={} out_fmt={} backend={} device={} demux_ms={:.3} decode_ms={:.3} hw_download_ms={:.3} convert_ms={:.3} stab_data_ms={:.3} upload_ms={:.3} gpu_wait_ms={:.3} readback_ms={:.3} hw_upload_ms={:.3} encode_ms={:.3} mux_ms={:.3} other_ms={:.3} gpu_pass_ms={} copy_ms={:.3} copy_kind={} copy_share={:.3} speedup_bound={:.2} gpu_wait_count={} gpu_pass_samples={} gpu_pass_count={} gpu_pass_sample_interval={}",
            t.frames, ms(wall_us), unknown(&t.decoder), unknown(&t.encoder), t.in_size.0, t.in_size.1, t.out_size.0, t.out_size.1,
            unknown(&t.in_fmt), unknown(&t.out_fmt), backend, serde_json::to_string(unknown(&t.device)).unwrap_or_else(|_| "\"unknown\"".into()),
            ms(t.demux_us), ms(t.decode_us), ms(t.download_us), ms(t.convert_us), ms(gpu.stab_data_us), ms(gpu.upload_us),
            ms(gpu.gpu_wait_us), ms(gpu.readback_us), ms(t.upload_us), ms(t.encode_us), ms(t.mux_us), ms(summary.other_us), pass,
            ms(summary.copy_us), summary.copy_kind, summary.copy_share, summary.speedup_bound, gpu.gpu_wait_count, gpu.gpu_pass_samples,
            gpu.gpu_pass_count, gyroflow_core::gpu::timing::GPU_PASS_SAMPLE_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn export_stage_timing_summary_uses_fixed_formula() {
        let s = summarize(10_000_000, 8_000_000, 2_000_000, 2_000_000, Some(1_500_000));
        assert_eq!(s.copy_us, 2_500_000);
        assert_eq!(format!("{:.3}/{:.2}", s.copy_share, s.speedup_bound), "0.250/1.33");
        assert_eq!(s.copy_kind, "split");
        assert_eq!(s.other_us, 2_000_000);
    }
    #[test]
    fn export_stage_timing_without_queries_is_lower_bound() {
        let s = summarize(10_000_000, 8_000_000, 2_500_000, 2_000_000, None);
        assert_eq!(s.copy_us, 2_500_000);
        assert_eq!(s.copy_kind, "lower_bound");
    }
    #[test]
    fn export_stage_timing_overrun_is_reported_without_negative_other() {
        let s = summarize(10_000, 10_100, 1_000, 1_000, None);
        assert_eq!(s.other_us, 0);
        assert_eq!(s.overrun_us, 100);
    }
}
