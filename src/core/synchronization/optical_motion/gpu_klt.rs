// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

//! GPU pyramids and forward/backward KLT, adapted from upstream 684c6a56.
//! Frames stay on the GPU between pairs; feature selection remains in the shared CPU tracker.

use super::gpu_corners::{GpuCorners, local_maxima};
use super::gpu_tracker::{KLT_EPS, KLT_ITERATIONS, KLT_LEVELS, KLT_MIN_EIG, WINDOW};
use std::sync::{
    Arc, Mutex, PoisonError,
    atomic::{AtomicBool, Ordering::Relaxed},
    mpsc,
};
use std::time::Duration;
use wgpu::util::DeviceExt;

/// Waiting for the GPU this long means it hung, and the tracking goes on on the CPU
const TIMEOUT: Duration = Duration::from_secs(10);

/// What a process makes once for a GPU: the device and the pipelines. A device that failed (lost, an error, a wait for
/// it that timed out) is made again for the next analysis. The adapter is one of `WgpuWrapper`'s, the same one for the
/// same GPU every time (see `compute_adapter`): two GPUs of a kind have the same name and ids
pub(super) struct Shared {
    adapter: wgpu::Adapter,
    pub(super) name: String,
    pub(super) device: wgpu::Device,
    pub(super) queue: wgpu::Queue,
    pub(super) failed: Arc<AtomicBool>,
    pub(super) resources: Mutex<()>,
    to_float: wgpu::ComputePipeline,
    down: wgpu::ComputePipeline,
    scharr: wgpu::ComputePipeline,
    lk: wgpu::ComputePipeline,
    covariance: wgpu::ComputePipeline,
    eigen: wgpu::ComputePipeline,
    corners: wgpu::ComputePipeline,
}

static SHARED: Mutex<Option<Arc<Shared>>> = Mutex::new(None);

impl Shared {
    pub(super) fn get(adapter: &wgpu::Adapter) -> Result<Arc<Self>, String> {
        let info = adapter.get_info();
        let mut cached = SHARED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(s) = cached
            .as_ref()
            .filter(|s| s.adapter == *adapter && !s.failed.load(Relaxed))
        {
            return Ok(s.clone());
        }
        if !adapter
            .get_downlevel_capabilities()
            .flags
            .contains(wgpu::DownlevelFlags::COMPUTE_SHADERS)
        {
            return Err(format!("{} has no compute shaders", info.name));
        }
        let adapter_limits = adapter.limits();
        let mut limits =
            wgpu::Limits::downlevel_defaults().using_resolution(adapter_limits.clone());
        limits.max_buffer_size = adapter_limits.max_buffer_size;
        limits.max_storage_buffer_binding_size = adapter_limits.max_storage_buffer_binding_size;
        limits.max_compute_workgroup_storage_size =
            adapter_limits.max_compute_workgroup_storage_size.min(65536);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("tracking"),
            required_features: adapter.features() & wgpu::Features::SHADER_F64,
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
        }))
        .map_err(|e| e.to_string())?;
        let failed = Arc::new(AtomicBool::new(false));
        let f = failed.clone();
        device.on_uncaptured_error(Arc::new(move |e| {
            log::error!("GPU tracking: {e}");
            f.store(true, Relaxed);
        }));
        let f = failed.clone();
        // Destroyed is also the device dropped, when one made again replaces it
        device.set_device_lost_callback(move |reason, message| {
            if reason == wgpu::DeviceLostReason::Unknown {
                log::error!("GPU tracking: device lost: {message}");
            }
            f.store(true, Relaxed);
        });

        // The backend's compiler (FXC, DXC, Metal's) rejecting a kernel is an internal error, not a validation one. All
        // popped before any is looked at: they go in the reverse order, or wgpu panics
        let scopes = [
            wgpu::ErrorFilter::Validation,
            wgpu::ErrorFilter::Internal,
            wgpu::ErrorFilter::OutOfMemory,
        ]
        .map(|f| device.push_error_scope(f));
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("klt"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_klt.wgsl").into()),
        });
        let pipeline = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let (to_float, down, scharr, lk) = (
            pipeline("to_float"),
            pipeline("pyr_down"),
            pipeline("scharr"),
            pipeline("lk"),
        );
        let (covariance, eigen, corners) = (
            pipeline("corner_covariance"),
            pipeline("corner_eigen"),
            pipeline("corner_candidates"),
        );
        let errors: Vec<wgpu::Error> = scopes
            .into_iter()
            .rev()
            .filter_map(|s| pollster::block_on(s.pop()))
            .collect();
        if let Some(e) = errors.first() {
            return Err(e.to_string());
        }
        // Not kept when it failed some other way while it was made
        if failed.load(Relaxed) {
            return Err("the device failed while setting up".into());
        }

        let shared = Arc::new(Self {
            adapter: adapter.clone(),
            name: format!("{} ({:?})", info.name, info.backend),
            device,
            queue,
            failed,
            resources: Mutex::new(()),
            to_float,
            down,
            scharr,
            lk,
            covariance,
            eigen,
            corners,
        });
        *cached = Some(shared.clone());
        Ok(shared)
    }
}

/// Submitted and not read back yet
struct Pending {
    submission: wgpu::SubmissionIndex,
    mapped: mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    tracked: usize,
}

pub struct GpuKlt {
    shared: Arc<Shared>,
    pub size: (u32, u32),
    pub stride: u32,
    max_points: usize,
    raw: wgpu::Buffer,
    pts: wgpu::Buffer,
    out_fwd: wgpu::Buffer,
    out_bwd: wgpu::Buffer,
    staging: wgpu::Buffer,
    feature_work: wgpu::Buffer,
    candidates: wgpu::Buffer,
    candidate_capacity: usize,
    global_off: u64,
    local_off: u64,
    candidates_off: u64,
    pub corners: GpuCorners,
    /// The pyramid's levels
    dims: Vec<(u32, u32)>,
    /// Readback offsets of the two tracking directions.
    fwd_off: u64,
    bwd_off: u64,
    // Per slot of the two frames kept: the frame going into it
    g_to_float: Vec<wgpu::BindGroup>,
    g_down: Vec<Vec<wgpu::BindGroup>>,
    g_scharr: Vec<Vec<wgpu::BindGroup>>,
    /// LK with I in that slot and J in the other: forward from the points uploaded, back from where they went
    g_fwd: Vec<wgpu::BindGroup>,
    g_bwd: Vec<wgpu::BindGroup>,
    g_covariance: Vec<wgpu::BindGroup>,
    g_eigen: wgpu::BindGroup,
    g_corners: wgpu::BindGroup,
    /// The slot of the last frame, and how many went up
    slot: usize,
    frames: usize,
    pending: Option<Pending>,
    upload: Vec<[f32; 4]>,
    /// After `finish`, per point tracked forward and back: x, y, and 1 where it got there
    pub fwd: Vec<[f32; 4]>,
    pub bwd: Vec<[f32; 4]>,
}

impl GpuKlt {
    pub(super) fn context(selected: Option<usize>) -> Result<Arc<Shared>, String> {
        let adapter =
            crate::gpu::wgpu::WgpuWrapper::compute_adapter(selected).ok_or("No hardware GPU")?;
        Shared::get(&adapter)
    }
    /// Makes the device and the pipelines for `adapter` ahead of the first `new` on it
    pub fn prepare(adapter: &wgpu::Adapter) -> Result<(), String> {
        Shared::get(adapter).map(|_| ())
    }

    pub fn new(
        adapter: &wgpu::Adapter,
        width: u32,
        height: u32,
        stride: u32,
        max_points: usize,
    ) -> Result<Self, String> {
        if width == 0
            || height == 0
            || width > u16::MAX as u32
            || height > u16::MAX as u32
            || stride < width
            || max_points == 0
        {
            return Err("Invalid GPU tracking dimensions".into());
        }
        let shared = Shared::get(adapter)?;
        let _resources = shared
            .resources
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let device = &shared.device;
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let oom = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);

        // The pyramid's sizes, as buildOpticalFlowPyramid stops: before a level as small as the window
        let mut dims = vec![(width, height)];
        while dims.len() <= KLT_LEVELS as usize {
            let (w, h) = *dims.last().unwrap();
            let next = (w.div_ceil(2), h.div_ceil(2));
            if next.0 <= WINDOW as u32 || next.1 <= WINDOW as u32 {
                break;
            }
            dims.push(next);
        }
        let mut off = 0u32;
        let (mut img, mut grad) = ([[0u32; 8]; 2], [[0u32; 8]; 2]);
        for s in 0..2 {
            for (k, (w, h)) in dims.iter().enumerate() {
                img[s][k] = off;
                off += w * h;
                grad[s][k] = off;
                off += 2 * w * h;
            }
        }

        let storage = |size: u64, extra: wgpu::BufferUsages| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size,
                usage: wgpu::BufferUsages::STORAGE | extra,
                mapped_at_creation: false,
            })
        };
        let pyr = storage(off as u64 * 4, wgpu::BufferUsages::empty());
        let raw = storage(
            (stride as u64 * height as u64).div_ceil(4) * 4,
            wgpu::BufferUsages::COPY_DST,
        );
        let pts = storage(max_points as u64 * 16, wgpu::BufferUsages::COPY_DST);
        let out_fwd = storage(max_points as u64 * 16, wgpu::BufferUsages::COPY_SRC);
        let out_bwd = storage(max_points as u64 * 16, wgpu::BufferUsages::COPY_SRC);
        let fwd_off = 0;
        let bwd_off = max_points as u64 * 16;
        let wh = width as u64 * height as u64;
        let candidate_capacity = (wh as usize / 4).max(1024);
        let feature_work = storage(wh * 20, wgpu::BufferUsages::COPY_SRC);
        let candidates = storage(
            8 + candidate_capacity as u64 * 16,
            wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        );
        let global_off = bwd_off + max_points as u64 * 16;
        let local_off = global_off + wh * 4;
        let candidates_off = local_off + wh * 4;
        let staging = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: candidates_off + 8 + candidate_capacity as u64 * 16,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let uniform = |words: &[u32]| {
            device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(words),
                usage: wgpu::BufferUsages::UNIFORM,
            })
        };
        // `Params`: w, h, sw, sh, src, dst, stride, aux
        let params = |v: [u32; 8]| uniform(&v);
        let group = |p: &wgpu::ComputePipeline, entries: &[(u32, &wgpu::Buffer)]| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &p.get_bind_group_layout(0),
                entries: &entries
                    .iter()
                    .map(|(binding, b)| wgpu::BindGroupEntry {
                        binding: *binding,
                        resource: b.as_entire_binding(),
                    })
                    .collect::<Vec<_>>(),
            })
        };
        // `LkParams`, with I in slot `s`
        let lk_params = |s: usize| {
            let mut v = vec![
                max_points as u32,
                dims.len() as u32,
                KLT_ITERATIONS as u32,
                0,
                ((KLT_EPS * KLT_EPS) as f32).to_bits(),
                (KLT_MIN_EIG as f32).to_bits(),
                0,
                0,
            ];
            for k in 0..8 {
                if k < dims.len() {
                    v.extend([
                        img[s][k],
                        grad[s][k],
                        img[1 - s][k],
                        dims[k].0 << 16 | dims[k].1,
                    ]);
                } else {
                    v.extend([0; 4]);
                }
            }
            uniform(&v)
        };
        let (w0, h0) = dims[0];
        let g_to_float = (0..2)
            .map(|s| {
                group(
                    &shared.to_float,
                    &[
                        (0, &params([w0, h0, 0, 0, 0, img[s][0], stride, 0])),
                        (1, &pyr),
                        (3, &raw),
                    ],
                )
            })
            .collect();
        let g_down = (0..2)
            .map(|s| {
                (1..dims.len())
                    .map(|k| {
                        group(
                            &shared.down,
                            &[
                                (
                                    0,
                                    &params([
                                        dims[k].0,
                                        dims[k].1,
                                        dims[k - 1].0,
                                        dims[k - 1].1,
                                        img[s][k - 1],
                                        img[s][k],
                                        0,
                                        0,
                                    ]),
                                ),
                                (1, &pyr),
                            ],
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let g_scharr = (0..2)
            .map(|s| {
                (0..dims.len())
                    .map(|k| {
                        group(
                            &shared.scharr,
                            &[
                                (
                                    0,
                                    &params([
                                        dims[k].0, dims[k].1, 0, 0, img[s][k], grad[s][k], 0, 0,
                                    ]),
                                ),
                                (1, &pyr),
                            ],
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let g_fwd = (0..2)
            .map(|s| {
                group(
                    &shared.lk,
                    &[(4, &lk_params(s)), (2, &pyr), (5, &pts), (6, &out_fwd)],
                )
            })
            .collect();
        let g_bwd = (0..2)
            .map(|s| {
                group(
                    &shared.lk,
                    &[(4, &lk_params(s)), (2, &pyr), (5, &out_fwd), (6, &out_bwd)],
                )
            })
            .collect();
        let g_covariance = (0..2)
            .map(|s| {
                group(
                    &shared.covariance,
                    &[
                        (0, &params([w0, h0, 0, 0, img[s][0], 0, 0, 0])),
                        (1, &pyr),
                        (7, &feature_work),
                    ],
                )
            })
            .collect();
        let g_eigen = group(
            &shared.eigen,
            &[
                (
                    0,
                    &params([w0, h0, 4, 6, 0, wh as u32 * 3, wh as u32 * 4, 0]),
                ),
                (7, &feature_work),
            ],
        );
        let g_corners = group(
            &shared.corners,
            &[
                (
                    0,
                    &params([
                        w0,
                        h0,
                        4,
                        6,
                        wh as u32 * 3,
                        wh as u32 * 4,
                        candidate_capacity as u32,
                        0,
                    ]),
                ),
                (7, &feature_work),
                (8, &candidates),
            ],
        );

        if let Some(e) = pollster::block_on(oom.pop()) {
            return Err(e.to_string());
        }
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(e.to_string());
        }
        Ok(Self {
            shared: shared.clone(),
            size: (width, height),
            stride,
            max_points,
            raw,
            pts,
            out_fwd,
            out_bwd,
            staging,
            dims,
            feature_work,
            candidates,
            candidate_capacity,
            global_off,
            local_off,
            candidates_off,
            corners: GpuCorners::new(width as usize, height as usize),
            fwd_off,
            bwd_off,
            g_to_float,
            g_down,
            g_scharr,
            g_fwd,
            g_bwd,
            g_covariance,
            g_eigen,
            g_corners,
            slot: 1,
            frames: 0,
            pending: None,
            upload: Vec::new(),
            fwd: Vec::new(),
            bwd: Vec::new(),
        })
    }

    /// The GPU's name and backend
    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// Starts a new sequence without retaining a connection to the old frame.
    pub fn reset_history(&mut self) {
        self.frames = 0;
    }

    /// Uploads the next frame, builds its pyramid and tracks the previous points forward and back.
    pub fn submit(
        &mut self,
        pixels: &[u8],
        points: impl ExactSizeIterator<Item = [f32; 2]>,
    ) -> Result<(), String> {
        if self.shared.failed.load(Relaxed) {
            return Err("the device failed".into());
        }
        if points.len() > self.max_points {
            return Err(format!(
                "{} points to track, room for {}",
                points.len(),
                self.max_points
            ));
        }
        let (queue, device) = (&self.shared.queue, &self.shared.device);
        let prev = self.slot;
        let slot = 1 - prev;
        let n = if self.frames > 0 { points.len() } else { 0 };

        let len = self.stride as usize * self.size.1 as usize;
        let padded = len.div_ceil(4) * 4;
        if pixels.len() >= padded {
            queue.write_buffer(&self.raw, 0, &pixels[..padded]);
        } else {
            let mut v = pixels[..len].to_vec();
            v.resize(padded, 0);
            queue.write_buffer(&self.raw, 0, &v);
        }
        if n > 0 {
            self.upload.clear();
            self.upload
                .extend(points.take(n).map(|p| [p[0], p[1], 1.0, 0.0]));
            queue.write_buffer(&self.pts, 0, bytemuck::cast_slice(&self.upload));
        }

        let mut enc = device.create_command_encoder(&Default::default());
        enc.clear_buffer(&self.candidates, 0, Some(8));
        {
            let groups = |(w, h): (u32, u32)| (w.div_ceil(8), h.div_ceil(8));
            let (gx, gy) = groups(self.dims[0]);
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("klt"),
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.shared.to_float);
            pass.set_bind_group(0, &self.g_to_float[slot], &[]);
            pass.dispatch_workgroups(gx, gy, 1);
            pass.set_pipeline(&self.shared.down);
            for k in 1..self.dims.len() {
                let (x, y) = groups(self.dims[k]);
                pass.set_bind_group(0, &self.g_down[slot][k - 1], &[]);
                pass.dispatch_workgroups(x, y, 1);
            }
            pass.set_pipeline(&self.shared.scharr);
            for k in 0..self.dims.len() {
                let (x, y) = groups(self.dims[k]);
                pass.set_bind_group(0, &self.g_scharr[slot][k], &[]);
                pass.dispatch_workgroups(x, y, 1);
            }
            for (pipeline, group) in [
                (&self.shared.covariance, &self.g_covariance[slot]),
                (&self.shared.eigen, &self.g_eigen),
                (&self.shared.corners, &self.g_corners),
            ] {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, group, &[]);
                pass.dispatch_workgroups(gx, gy, 1);
            }
            if n > 0 {
                pass.set_pipeline(&self.shared.lk);
                pass.set_bind_group(0, &self.g_fwd[prev], &[]);
                pass.dispatch_workgroups(n as u32, 1, 1);
                pass.set_bind_group(0, &self.g_bwd[slot], &[]);
                pass.dispatch_workgroups(n as u32, 1, 1);
            }
        }
        if n > 0 {
            enc.copy_buffer_to_buffer(&self.out_fwd, 0, &self.staging, self.fwd_off, n as u64 * 16);
            enc.copy_buffer_to_buffer(&self.out_bwd, 0, &self.staging, self.bwd_off, n as u64 * 16);
        }
        let wh = self.size.0 as u64 * self.size.1 as u64;
        enc.copy_buffer_to_buffer(
            &self.feature_work,
            wh * 12,
            &self.staging,
            self.global_off,
            wh * 4,
        );
        enc.copy_buffer_to_buffer(
            &self.feature_work,
            wh * 16,
            &self.staging,
            self.local_off,
            wh * 4,
        );
        enc.copy_buffer_to_buffer(
            &self.candidates,
            0,
            &self.staging,
            self.candidates_off,
            8 + self.candidate_capacity as u64 * 16,
        );
        let submission = queue.submit(Some(enc.finish()));
        self.corners.find_glare(pixels, self.stride as usize);
        let (tx, mapped) = mpsc::channel();
        self.staging
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
        self.pending = Some(Pending {
            submission,
            mapped,
            tracked: n,
        });
        self.slot = slot;
        self.frames += 1;
        Ok(())
    }

    /// Waits for the submitted pair and reads back only its tracked points.
    pub fn finish(&mut self) -> Result<(), String> {
        let Some(p) = self.pending.take() else {
            return Ok(());
        };
        self.wait(p.submission, &p.mapped)?;
        if self.shared.failed.load(Relaxed) {
            self.staging.unmap();
            return Err("the device failed".into());
        }
        {
            let data = self.staging.slice(..).get_mapped_range();
            let at = |off: u64| &data[off as usize..off as usize + p.tracked * 16];
            self.fwd.clear();
            self.bwd.clear();
            self.fwd
                .extend_from_slice(bytemuck::cast_slice(at(self.fwd_off)));
            self.bwd
                .extend_from_slice(bytemuck::cast_slice(at(self.bwd_off)));
            let count = self.size.0 as usize * self.size.1 as usize;
            for (values, off) in [
                (&mut self.corners.global, self.global_off),
                (&mut self.corners.local, self.local_off),
            ] {
                values.copy_from_slice(bytemuck::cast_slice(
                    &data[off as usize..off as usize + count * 4],
                ));
            }
            let counts: &[u32] = bytemuck::cast_slice(
                &data[self.candidates_off as usize..self.candidates_off as usize + 8],
            );
            for (kind, output) in [
                &mut self.corners.global_candidates,
                &mut self.corners.local_candidates,
            ]
            .into_iter()
            .enumerate()
            {
                output.clear();
                let count = counts[kind] as usize;
                if count <= self.candidate_capacity {
                    let off = self.candidates_off as usize + 8 + kind * self.candidate_capacity * 8;
                    let items: &[[u32; 2]] = bytemuck::cast_slice(&data[off..off + count * 8]);
                    output.extend(items.iter().map(|p| (p[1] as u64) << 32 | p[0] as u64));
                    output.sort_unstable_by(|a, b| b.cmp(a));
                } else {
                    *output = local_maxima(
                        if kind == 0 {
                            &self.corners.global
                        } else {
                            &self.corners.local
                        },
                        self.size.0 as usize,
                        self.size.1 as usize,
                        kind == 1,
                    );
                }
            }
        }
        self.staging.unmap();
        Ok(())
    }

    /// Waits for `submission` and for the buffer it maps. A device lost meanwhile panics in wgpu's poll (it returns only
    /// timeouts), and on the analysis' thread pool a panic takes the app down: an error here. Any failure, a timeout too,
    /// has the device made again for the next analysis
    fn wait(
        &self,
        submission: wgpu::SubmissionIndex,
        mapped: &mpsc::Receiver<Result<(), wgpu::BufferAsyncError>>,
    ) -> Result<(), String> {
        if self.shared.failed.load(Relaxed) {
            return Err("the device failed".into());
        }
        let device = &self.shared.device;
        let waited = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            device.poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(TIMEOUT),
            })
        }))
        .map_err(|_| "the device was lost".to_string())
        .and_then(|polled| polled.map_err(|e| e.to_string()))
        .and_then(|_| {
            mapped
                .recv_timeout(TIMEOUT)
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())
        });
        if waited.is_err() {
            self.shared.failed.store(true, Relaxed);
        }
        waited
    }
}
