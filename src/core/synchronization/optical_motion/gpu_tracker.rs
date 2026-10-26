// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

//! GPU pyramids and KLT adapted from upstream 684c6a56. Feature selection and IDs stay in the shared tracker.
use super::gpu_klt::GpuKlt;
use crate::synchronization::GrayImage;
use std::{sync::mpsc, time::Duration};
pub(super) const WINDOW: i32 = 21;
pub(super) const KLT_LEVELS: i32 = 3;
pub(super) const KLT_ITERATIONS: i32 = 50;
pub(super) const KLT_EPS: f64 = 0.001;
pub(super) const KLT_MIN_EIG: f64 = 1e-4;

#[derive(Clone, Copy, Debug)]
pub enum KltDevice {
    Cpu,
    Gpu(Option<usize>),
}
impl KltDevice {
    pub fn for_processing_device(selected: i32) -> Self {
        if selected < 0 || !std::env::var("NO_WGPU").unwrap_or_default().is_empty() {
            return Self::Cpu;
        }
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let enabled = *ENABLED.get_or_init(|| {
            let raw = std::env::var("GYROFLOW_OPTICAL_GPU").ok();
            let enabled = match raw.as_deref().map(str::trim) {
                None | Some("1" | "true" | "on") => true,
                Some("0" | "false" | "off") => false,
                Some(value) => { log::warn!(target: "lifecycle", "GYROFLOW_OPTICAL_GPU={value} invalid, using true"); true },
            };
            log::info!(target: "lifecycle", "optical_gpu resolved enabled={enabled} source={}", if raw.is_some() { "env" } else { "default" });
            enabled
        });
        if enabled {
            Self::Gpu(Some(selected as usize))
        } else {
            Self::Cpu
        }
    }
}

pub(super) struct GpuTracker {
    finding: Option<mpsc::Receiver<Result<wgpu::Adapter, String>>>,
    adapter: Option<wgpu::Adapter>,
    gpu: Option<GpuKlt>,
    max_points: usize,
    has_frame: bool,
    #[cfg(test)]
    fail_next: bool,
}
impl GpuTracker {
    pub fn new(selected: Option<usize>, max_points: usize) -> Self {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                let adapter = crate::gpu::wgpu::WgpuWrapper::compute_adapter(selected)
                    .ok_or_else(|| "no hardware GPU".to_string())?;
                GpuKlt::prepare(&adapter)?;
                Ok(adapter)
            })
            .unwrap_or_else(|_| Err("GPU initialization panicked".into()));
            let _ = tx.send(result);
        });
        Self {
            finding: Some(rx),
            adapter: None,
            gpu: None,
            max_points,
            has_frame: false,
            #[cfg(test)]
            fail_next: false,
        }
    }
    pub fn reset(&mut self) {
        self.has_frame = false;
        if let Some(gpu) = &mut self.gpu {
            gpu.reset_history();
        }
    }
    pub fn track(
        &mut self,
        img: &GrayImage,
        points: impl ExactSizeIterator<Item = [f32; 2]>,
    ) -> Result<Vec<([f32; 4], [f32; 4])>, String> {
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next) {
            return Err("Injected GPU failure".into());
        }
        let (width, height) = img.dimensions();
        if self.gpu.as_ref().is_none_or(|g| g.size != (width, height)) {
            if let Some(rx) = self.finding.take() {
                self.adapter = Some(
                    rx.recv_timeout(Duration::from_secs(30))
                        .map_err(|e| format!("GPU setup: {e}"))??,
                );
            }
            let adapter = self.adapter.as_ref().ok_or("No GPU adapter")?;
            let gpu = GpuKlt::new(adapter, width, height, width, 2 * self.max_points)?;
            log::info!(target: "sync", "Optical tracking: GPU {} (GPU corners, local selection)", gpu.name());
            self.gpu = Some(gpu);
        }
        let gpu = self.gpu.as_mut().unwrap();
        gpu.submit(img.as_raw(), points)?;
        gpu.finish()?;
        self.has_frame = true;
        Ok(gpu
            .fwd
            .iter()
            .copied()
            .zip(gpu.bwd.iter().copied())
            .collect())
    }
    pub fn select(&self, occupied: &[[f32; 2]], ratio: f64, first_id: u32) -> Vec<(u32, [f32; 2])> {
        if !self.has_frame {
            return Vec::new();
        }
        self.gpu
            .as_ref()
            .unwrap()
            .corners
            .select(occupied, self.max_points, ratio, first_id)
    }
    pub fn texture_at(&self, point: [f32; 2]) -> f32 {
        self.gpu.as_ref().unwrap().corners.texture_at(point)
    }
    #[cfg(test)]
    pub fn fail_once(&mut self) {
        self.fail_next = true;
    }
}
