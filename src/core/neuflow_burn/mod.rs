// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! NeuFlow v2 optical flow inference via Burn framework.
//!
//! Uses Burn + CubeCL JIT to run NeuFlow v2 on Vulkan (CMMA/FP16) or Metal.
//! Replaces the previous ONNX Runtime backend.
//!
//! Architecture: All Burn operations (tensor creation, model.forward, data extraction)
//! run on a dedicated inference thread. Callers send requests via mpsc channel and
//! receive results via oneshot channel. This ensures:
//! - burn-fusion stream safety (thread-local state stays on one thread)
//! - wgpu command queue is never submitted concurrently
//! - autotune cache is bound to the correct thread-local context
//!
//! Optimizations:
//! - Zero-copy: `infer()` takes owned `Vec<f32>`, moved directly into TensorData
//! - Double-buffer: inference loop prefetches next request's tensors during readback
//! - Async readback: `into_data_async()` allows GPU readback to overlap with next frame's work
//! - CHW direct output: no interleave step, caller reads planar layout
//!
//! Model I/O:
//! - Input: img0 [1, 3, H, W] float32, img1 [1, 3, H, W] float32
//! - Current fixed export baseline is H=480, W=640 (already landscape 640x480)
//! - Output: flow [2, H, W] float32 CHW layout (dx plane, dy plane)

#[allow(
    clippy::let_and_return,
    clippy::approx_constant,
    clippy::all,
    unused_variables,
    dead_code
)]
#[path = "generated_mixed/neuflow_v2_mixed_fp16.rs"]
mod neuflow_v2_generated_fp16;

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, mpsc};
use std::time::Instant;

use burn::prelude::*;
use burn::tensor::TensorData;
use burn_store::ModuleSnapshot;
use half::f16;
use neuflow_trace_shared::{self as neuflow_trace, DurationMetric, TracePhase};

use neuflow_v2_generated_fp16::Model;

// Backend type: Wgpu<f16> — all compute in FP16.
type B = burn::backend::wgpu::Wgpu<f16>;

fn is_optimized_weight_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.contains("optimized"))
}

// ---------------------------------------------------------------------------
// Channel-based inference thread
// ---------------------------------------------------------------------------

/// GPU-side sparse sampling result: flow values at selected grid points.
pub struct SampledFlow {
    /// Grid point coordinates (x, y) in model space (640×480).
    pub grid_points: Vec<(usize, usize)>,
    /// dx values at each grid point.
    pub dx: Vec<f32>,
    /// dy values at each grid point.
    pub dy: Vec<f32>,
}

/// Request sent to the inference thread.
enum InferRequest {
    /// Run inference, read back full flow tensor (for testing/benchmarks).
    Infer {
        seq: u64,
        img0: Vec<f32>,
        img1: Vec<f32>,
        h: usize,
        w: usize,
        reply: mpsc::Sender<Result<Vec<f32>, String>>,
    },
    /// Run inference + GPU-side sparse sampling (main production path).
    /// Only reads back ~400 floats instead of 614,400.
    InferAndSample {
        seq: u64,
        img0: Vec<f32>,
        img1: Vec<f32>,
        h: usize,
        w: usize,
        grid_points: Vec<(usize, usize)>,
        linear_indices: Vec<i32>,
        reply: mpsc::Sender<Result<SampledFlow, String>>,
    },
    /// Warm up the model (dummy inference to trigger autotune caching).
    Warmup {
        reply: mpsc::Sender<Result<(), String>>,
    },
}

/// Handle to the inference thread, holding the sender end of the channel.
struct InferenceHandle {
    sender: mpsc::Sender<InferRequest>,
}

static INFERENCE_HANDLE: OnceLock<Result<InferenceHandle, String>> = OnceLock::new();
static QUEUE_DEPTH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Spawn the inference thread: loads model, then loops on channel receiving requests.
fn spawn_inference_thread() -> Result<InferenceHandle, String> {
    let (tx, rx) = mpsc::channel::<InferRequest>();
    let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("neuflow-infer".to_string())
        .spawn(move || {
            if std::env::var_os("CUBECL_AUTOTUNE_LEVEL").is_none() {
                unsafe { std::env::set_var("CUBECL_AUTOTUNE_LEVEL", "0"); }
            }

            let model_result = init_model();
            match model_result {
                Ok((model, device)) => {
                    let _ = init_tx.send(Ok(()));
                    inference_loop(model, device, rx);
                }
                Err(e) => {
                    let _ = init_tx.send(Err(e));
                }
            }
        })
        .map_err(|e| format!("Failed to spawn inference thread: {e}"))?;

    let init_result = init_rx.recv()
        .map_err(|_| "Inference thread init channel closed unexpectedly".to_string())?;
    init_result?;

    Ok(InferenceHandle { sender: tx })
}

// ---------------------------------------------------------------------------
// Inference helpers (run on inference thread only)
// ---------------------------------------------------------------------------

/// Create GPU tensors from owned f32 data. Zero-copy into TensorData.
fn prepare_tensors(
    img0: Vec<f32>, img1: Vec<f32>, h: usize, w: usize,
    device: &burn::backend::wgpu::WgpuDevice,
) -> (Tensor<B, 4>, Tensor<B, 4>) {
    let shape = [1usize, 3, h, w];
    let img0_data = TensorData::new(img0, shape);
    let img1_data = TensorData::new(img1, shape);
    let img0_tensor = Tensor::<B, 4>::from_data(img0_data, device);
    let img1_tensor = Tensor::<B, 4>::from_data(img1_data, device);
    (img0_tensor, img1_tensor)
}

/// Extract flow from GPU tensor → CHW Vec<f32>. No interleave.
/// Async version: uses into_data_async() for non-blocking GPU readback.
async fn extract_flow_async(flow_tensor: Tensor<B, 4>, h: usize, w: usize) -> Result<Vec<f32>, String> {
    let flow_data = flow_tensor.into_data_async().await
        .map_err(|e| format!("Async readback failed: {e:?}"))?
        .convert::<f32>();
    let flow_vec: Vec<f32> = flow_data.to_vec()
        .map_err(|e| format!("Failed to extract flow data: {e:?}"))?;

    let plane = h * w;
    if flow_vec.len() != 2 * plane {
        return Err(format!(
            "Unexpected flow size: expected {}, got {}",
            2 * plane,
            flow_vec.len()
        ));
    }

    Ok(flow_vec)
}

// ---------------------------------------------------------------------------
// Pipelined inference with concurrent GPU readback + CPU forward
// ---------------------------------------------------------------------------
//
// Key insight: burn-fusion's model.forward() is LAZY — it builds a fusion
// graph on the CPU but submits NO GPU commands. All GPU work triggers when
// into_data_async() is called (which flushes the graph). So simply deferring
// the tensor doesn't help — we must explicitly START the readback (via poll)
// to trigger the GPU flush, then do CPU work while the GPU computes.
//
// Pipeline (using poll_fn for concurrent polling):
//
//   1. poll readback(N-1)  → triggers GPU flush, GPU starts computing N-1
//   2. prepare + forward(N) → CPU builds fusion graph while GPU runs N-1
//   3. await readback(N-1)  → GPU should be done, ~0ms wait
//   4. Store N's tensor for deferred readback
//
// This overlaps GPU compute with CPU graph-building, reducing effective
// per-frame time from forward+readback (~27ms) to max(forward, readback) (~14ms).

/// Readback a flow tensor synchronously (full readback path).
fn readback_flow_sync(flow_tensor: Tensor<B, 4>, h: usize, w: usize) -> Result<Vec<f32>, String> {
    pollster::block_on(extract_flow_async(flow_tensor, h, w))
}

/// Readback a sampled tensor synchronously (sparse sampling path).
fn readback_sampled_sync(sampled: Tensor<B, 2>, num_pts: usize, grid_points: Vec<(usize, usize)>) -> Result<SampledFlow, String> {
    let sampled_data = pollster::block_on(sampled.into_data_async())
        .map_err(|e| format!("Readback failed: {e:?}"))?
        .convert::<f32>();
    let sampled_vec: Vec<f32> = sampled_data.to_vec()
        .map_err(|e| format!("Failed to extract sampled data: {e:?}"))?;
    if sampled_vec.len() == num_pts * 2 {
        Ok(SampledFlow {
            grid_points,
            dx: sampled_vec[..num_pts].to_vec(),
            dy: sampled_vec[num_pts..].to_vec(),
        })
    } else {
        Err(format!("Sampled flow size mismatch: expected {}, got {}", num_pts * 2, sampled_vec.len()))
    }
}

/// Overlap prev frame's GPU readback with current frame's CPU forward pass.
///
/// Uses poll_fn to drive both concurrently within a single pollster::block_on:
/// 1st poll: kicks off readback (GPU flush) + runs sync forward (CPU)
/// subsequent polls: waits for readback completion
fn overlap_readback_with_forward<R, F>(
    readback_seq: u64,
    readback_future: F,
    sync_seq: u64,
    sync_work: impl FnOnce(),
) -> R
where
    F: std::future::Future<Output = R>,
{
    use std::task::Poll;

    pollster::block_on(async {
        let mut readback = core::pin::pin!(readback_future);
        let mut sync_work = Some(sync_work);
        let mut readback_ready: Option<R> = None;

        core::future::poll_fn(|cx| {
            // 1. Poll readback — first poll triggers GPU flush + command submission
            if readback_ready.is_none() {
                let polled = {
                    let _trace_guard = neuflow_trace::enter(Some(readback_seq), TracePhase::Readback);
                    readback.as_mut().poll(cx)
                };
                if let Poll::Ready(val) = polled {
                    readback_ready = Some(val);
                }
            }
            // 2. Run sync work once — CPU builds next frame's fusion graph
            //    while GPU computes prev frame in the background
            if let Some(f) = sync_work.take() {
                let _trace_guard = neuflow_trace::enter(Some(sync_seq), TracePhase::Forward);
                f();
            }
            if let Some(val) = readback_ready.take() {
                return Poll::Ready(val);
            }
            // 3. Poll readback again — GPU may have finished during sync work
            let _trace_guard = neuflow_trace::enter(Some(readback_seq), TracePhase::Readback);
            readback.as_mut().poll(cx)
        }).await
    })
}

/// Pending readback from the previous frame.
enum PendingReadback {
    Infer {
        flow_tensor: Tensor<B, 4>,
        h: usize,
        w: usize,
        reply: mpsc::Sender<Result<Vec<f32>, String>>,
        seq: u64,
    },
    Sample {
        sampled_tensor: Tensor<B, 2>,
        num_pts: usize,
        grid_points: Vec<(usize, usize)>,
        reply: mpsc::Sender<Result<SampledFlow, String>>,
        seq: u64,
    },
}

/// The main inference loop with pipelined GPU readback.
fn inference_loop(model: Model<B>, device: burn::backend::wgpu::WgpuDevice, rx: mpsc::Receiver<InferRequest>) {
    let mut pending: Option<InferRequest> = None;
    let mut deferred: Option<PendingReadback> = None;

    loop {
        let request = if let Some(req) = pending.take() {
            req
        } else {
            match rx.recv() {
                Ok(req) => req,
                Err(_) => break,
            }
        };
        QUEUE_DEPTH.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);

        match request {
            InferRequest::Infer { seq, img0, img1, h, w, reply } => {
                let t0 = Instant::now();
                let mut t_tensor = t0;
                let mut t_fwd = t0;
                let mut flow_tensor: Option<Tensor<B, 4>> = None;

                // Overlap: readback(prev) + forward(curr) concurrently
                if let Some(prev) = deferred.take() {
                    let t_pipe = Instant::now();
                    match prev {
                        PendingReadback::Infer { flow_tensor: prev_ft, h: ph, w: pw, reply: prev_reply, seq: prev_seq } => {
                            neuflow_trace::note_overlap(prev_seq, seq);
                            let prev_result = overlap_readback_with_forward(
                                prev_seq,
                                extract_flow_async(prev_ft, ph, pw),
                                seq,
                                || {
                                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::TensorUpload);
                                    let (t0_t, t1_t) = prepare_tensors(img0, img1, h, w, &device);
                                    t_tensor = Instant::now();
                                    drop(_trace_guard);
                                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::Forward);
                                    flow_tensor = Some(model.forward(t0_t, t1_t));
                                    t_fwd = Instant::now();
                                },
                            );
                            let t_done = Instant::now();
                            let _ = prev_reply.send(prev_result);
                            let pipe_ms = (t_done - t_pipe).as_secs_f64() * 1000.0;
                            let fwd_ms = (t_fwd - t_tensor).as_secs_f64() * 1000.0;
                            log::debug!(
                                "[NeuFlow perf] #{prev_seq} PIPE readback+forward={pipe_ms:.1}ms (fwd={fwd_ms:.1}ms overlapped)"
                            );
                        }
                        PendingReadback::Sample { sampled_tensor: prev_st, num_pts: pn, grid_points: pg, reply: prev_reply, seq: prev_seq, .. } => {
                            neuflow_trace::note_overlap(prev_seq, seq);
                            let prev_result = overlap_readback_with_forward(
                                prev_seq,
                                prev_st.into_data_async(),
                                seq,
                                || {
                                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::TensorUpload);
                                    let (t0_t, t1_t) = prepare_tensors(img0, img1, h, w, &device);
                                    t_tensor = Instant::now();
                                    drop(_trace_guard);
                                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::Forward);
                                    flow_tensor = Some(model.forward(t0_t, t1_t));
                                    t_fwd = Instant::now();
                                },
                            );
                            let t_done = Instant::now();
                            let result = match prev_result {
                                Ok(data) => {
                                    let sv: Vec<f32> = data.convert::<f32>().to_vec().unwrap_or_default();
                                    if sv.len() == pn * 2 {
                                        Ok(SampledFlow { grid_points: pg, dx: sv[..pn].to_vec(), dy: sv[pn..].to_vec() })
                                    } else {
                                        Err(format!("Sampled flow size mismatch"))
                                    }
                                }
                                Err(e) => Err(format!("Readback failed: {e:?}")),
                            };
                            let _ = prev_reply.send(result);
                            let pipe_ms = (t_done - t_pipe).as_secs_f64() * 1000.0;
                            log::debug!("[NeuFlow perf] #{prev_seq} PIPE SAMPLE readback+forward={pipe_ms:.1}ms");
                        }
                    }
                } else {
                    // No prev — just forward
                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::TensorUpload);
                    let (t0_t, t1_t) = prepare_tensors(img0, img1, h, w, &device);
                    t_tensor = Instant::now();
                    drop(_trace_guard);
                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::Forward);
                    flow_tensor = Some(model.forward(t0_t, t1_t));
                    t_fwd = Instant::now();
                }

                let flow_tensor = flow_tensor.unwrap();

                // Prefetch next request
                pending = rx.try_recv().ok();

                if pending.is_some() {
                    deferred = Some(PendingReadback::Infer {
                        flow_tensor, h, w, reply, seq,
                    });
                } else {
                    // Queue empty — readback immediately
                    let t_rb = Instant::now();
                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::Readback);
                    let result = readback_flow_sync(flow_tensor, h, w);
                    let rb_ms = t_rb.elapsed().as_secs_f64() * 1000.0;
                    let _ = reply.send(result);
                    log::debug!("[NeuFlow perf] #{seq} IMMEDIATE readback={rb_ms:.1}ms");
                }

                let tensor_ms = (t_tensor - t0).as_secs_f64() * 1000.0;
                let forward_ms = (t_fwd - t_tensor).as_secs_f64() * 1000.0;
                neuflow_trace::record_duration(seq, DurationMetric::TensorUpload, t_tensor - t0);
                neuflow_trace::record_duration(seq, DurationMetric::ForwardTotal, t_fwd - t_tensor);
                log::debug!(
                    "[NeuFlow perf] #{seq} tensor={tensor_ms:.1}ms forward={forward_ms:.1}ms deferred={} prefetched={}",
                    deferred.is_some(), pending.is_some()
                );
            }
            InferRequest::InferAndSample { seq, img0, img1, h, w, grid_points, linear_indices, reply } => {
                let t0 = Instant::now();
                let mut t_tensor = t0;
                let mut t_fwd = t0;
                let mut t_select = t0;
                let mut sampled_out: Option<Tensor<B, 2>> = None;
                let num_pts = linear_indices.len();

                // Overlap: readback(prev) + forward+select(curr)
                if let Some(prev) = deferred.take() {
                    let t_pipe = Instant::now();
                    let do_forward_select = || {
                        let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::TensorUpload);
                        let (t0_t, t1_t) = prepare_tensors(img0, img1, h, w, &device);
                        t_tensor = Instant::now();
                        drop(_trace_guard);
                        let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::Forward);
                        let ft = model.forward(t0_t, t1_t);
                        t_fwd = Instant::now();
                        drop(_trace_guard);
                        let plane = h * w;
                        let idx_t = Tensor::<B, 1, burn::tensor::Int>::from_data(
                            TensorData::new(linear_indices, [num_pts]), &device,
                        );
                        let sampled = ft.reshape([2, plane]).select(1, idx_t);
                        t_select = Instant::now();
                        sampled_out = Some(sampled);
                    };
                    match prev {
                        PendingReadback::Infer { flow_tensor: prev_ft, h: ph, w: pw, reply: prev_reply, seq: prev_seq, .. } => {
                            neuflow_trace::note_overlap(prev_seq, seq);
                            let prev_result = overlap_readback_with_forward(
                                prev_seq, extract_flow_async(prev_ft, ph, pw), seq, do_forward_select,
                            );
                            let _ = prev_reply.send(prev_result);
                            log::debug!("[NeuFlow perf] #{prev_seq} PIPE readback+fwd_select={:.1}ms", (Instant::now() - t_pipe).as_secs_f64() * 1000.0);
                        }
                        PendingReadback::Sample { sampled_tensor: prev_st, num_pts: pn, grid_points: pg, reply: prev_reply, seq: prev_seq, .. } => {
                            neuflow_trace::note_overlap(prev_seq, seq);
                            let prev_result = overlap_readback_with_forward(prev_seq, prev_st.into_data_async(), seq, do_forward_select);
                            let result = match prev_result {
                                Ok(data) => {
                                    let sv: Vec<f32> = data.convert::<f32>().to_vec().unwrap_or_default();
                                    if sv.len() == pn * 2 { Ok(SampledFlow { grid_points: pg, dx: sv[..pn].to_vec(), dy: sv[pn..].to_vec() }) }
                                    else { Err("Sampled flow size mismatch".into()) }
                                }
                                Err(e) => Err(format!("Readback failed: {e:?}")),
                            };
                            let _ = prev_reply.send(result);
                            log::debug!("[NeuFlow perf] #{prev_seq} PIPE SAMPLE readback+fwd_select={:.1}ms", (Instant::now() - t_pipe).as_secs_f64() * 1000.0);
                        }
                    }
                } else {
                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::TensorUpload);
                    let (t0_t, t1_t) = prepare_tensors(img0, img1, h, w, &device);
                    t_tensor = Instant::now();
                    drop(_trace_guard);
                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::Forward);
                    let ft = model.forward(t0_t, t1_t);
                    t_fwd = Instant::now();
                    drop(_trace_guard);
                    let plane = h * w;
                    let idx_t = Tensor::<B, 1, burn::tensor::Int>::from_data(
                        TensorData::new(linear_indices, [num_pts]), &device,
                    );
                    let sampled = ft.reshape([2, plane]).select(1, idx_t);
                    t_select = Instant::now();
                    sampled_out = Some(sampled);
                }

                let sampled = sampled_out.unwrap();
                pending = rx.try_recv().ok();

                if pending.is_some() {
                    deferred = Some(PendingReadback::Sample {
                        sampled_tensor: sampled, num_pts, grid_points, reply, seq,
                    });
                } else {
                    let t_rb = Instant::now();
                    let _trace_guard = neuflow_trace::enter(Some(seq), TracePhase::Readback);
                    let result = readback_sampled_sync(sampled, num_pts, grid_points);
                    let rb_ms = t_rb.elapsed().as_secs_f64() * 1000.0;
                    let _ = reply.send(result);
                    log::debug!("[NeuFlow perf] #{seq} SAMPLE IMMEDIATE readback={rb_ms:.1}ms pts={num_pts}");
                }

                let tensor_ms = (t_tensor - t0).as_secs_f64() * 1000.0;
                let forward_ms = (t_fwd - t_tensor).as_secs_f64() * 1000.0;
                let select_ms = (t_select - t_fwd).as_secs_f64() * 1000.0;
                neuflow_trace::record_duration(seq, DurationMetric::TensorUpload, t_tensor - t0);
                neuflow_trace::record_duration(seq, DurationMetric::ForwardTotal, t_fwd - t_tensor);
                log::debug!(
                    "[NeuFlow perf] #{seq} SAMPLE tensor={tensor_ms:.1}ms forward={forward_ms:.1}ms select={select_ms:.1}ms deferred={} prefetched={}",
                    deferred.is_some(), pending.is_some()
                );
            }
            InferRequest::Warmup { reply } => {
                if let Some(prev) = deferred.take() {
                    match prev {
                        PendingReadback::Infer { flow_tensor, h, w, reply: prev_reply, .. } => {
                            let _ = prev_reply.send(readback_flow_sync(flow_tensor, h, w));
                        }
                        PendingReadback::Sample { sampled_tensor, num_pts, grid_points, reply: prev_reply, .. } => {
                            let _ = prev_reply.send(readback_sampled_sync(sampled_tensor, num_pts, grid_points));
                        }
                    }
                }
                let result = run_warmup(&model, &device);
                let _ = reply.send(result);
            }
        }
    }

    // Drain last deferred readback
    if let Some(prev) = deferred.take() {
        match prev {
            PendingReadback::Infer { flow_tensor, h, w, reply, .. } => {
                let _ = reply.send(readback_flow_sync(flow_tensor, h, w));
            }
            PendingReadback::Sample { sampled_tensor, num_pts, grid_points, reply, .. } => {
                let _ = reply.send(readback_sampled_sync(sampled_tensor, num_pts, grid_points));
            }
        }
    }
    log::info!("NeuFlow Burn: inference thread exiting (channel closed)");
}

/// Run a dummy inference to trigger autotune caching on the inference thread.
fn run_warmup(model: &Model<B>, device: &burn::backend::wgpu::WgpuDevice) -> Result<(), String> {
    let h = 480usize;
    let w = 640usize;
    let dummy = vec![128.0f32; 3 * h * w];

    let (img0_tensor, img1_tensor) = prepare_tensors(dummy.clone(), dummy, h, w, device);
    let _flow = model.forward(img0_tensor, img1_tensor);
    let _data = _flow.into_data();

    log::info!("NeuFlow Burn: warmup inference complete (autotune cached)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Model initialization (called on the inference thread)
// ---------------------------------------------------------------------------

fn init_model() -> Result<(Model<B>, burn::backend::wgpu::WgpuDevice), String> {
    let path = find_weight_file()
        .ok_or_else(|| "NeuFlow Burn model (.bpk) not found".to_string())?;

    let optimized = is_optimized_weight_file(&path);
    let weight_variant = if optimized { "optimized" } else { "unoptimized" };
    log::info!(
        "NeuFlow Burn: loading generated_mixed model from {} (weight_variant={weight_variant})",
        path.display()
    );

    let device = burn::backend::wgpu::WgpuDevice::default();
    let mut model = Model::<B>::new(&device);

    let path_str = path.to_string_lossy().to_string();
    let mut store = burn_store::BurnpackStore::from_file(&path_str);
    model.load_from(&mut store)
        .map_err(|e| format!("Failed to load model weights: {e}"))?;

    log::info!(
        "NeuFlow Burn: generated_mixed model loaded on {:?} (weight_variant={weight_variant})",
        device
    );
    Ok((model, device))
}

fn format_ms(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.3}"))
        .unwrap_or_else(|| "None".to_string())
}

fn log_trace_summary(seq: u64, kind: &str) {
    if !neuflow_trace::enabled() {
        return;
    }

    let stats = neuflow_trace::take_frame(seq).unwrap_or_default();
    let current_seq = stats.overlap_current_seq.unwrap_or(seq);
    let summary = format!(
        "[NeuFlow perf] owner_seq={seq} current_seq={current_seq} kind={kind} tensor_upload_ms={:.3} forward_total_ms={:.3} forward_client_submit_ms={:.3} forward_client_backpressure_ms={:.3} forward_server_register_ms={:.3} forward_policy_update_ms={:.3} forward_plan_find_ms={:.3} forward_plan_hit_count={} forward_plan_miss_count={} forward_plan_add_count={} readback_total_ms={:.3} readback_drain_ms={:.3} readback_flush_submit_ms={:.3} readback_gpu_profile_ms={} readback_copy_to_staging_ms={:.3} readback_map_wait_ms={:.3} channel_roundtrip_ms={:.3}",
        stats.tensor_upload_ms,
        stats.forward_total_ms,
        stats.forward_client_submit_ms,
        stats.forward_client_backpressure_ms,
        stats.forward_server_register_ms,
        stats.forward_policy_update_ms,
        stats.forward_plan_find_ms,
        stats.forward_plan_hit_count,
        stats.forward_plan_miss_count,
        stats.forward_plan_add_count,
        stats.readback_total_ms,
        stats.readback_drain_ms,
        stats.readback_flush_submit_ms,
        format_ms(stats.readback_gpu_profile_ms),
        stats.readback_copy_to_staging_ms,
        stats.readback_map_wait_ms,
        stats.channel_roundtrip_ms,
    );
    log::debug!("{summary}");
    if std::env::var_os("NEUFLOW_TRACE_STDERR").is_some() {
        eprintln!("{summary}");
    }

    if neuflow_trace::verbose() {
        let summary_v2 = format!(
            "[NeuFlow perf][v2] owner_seq={seq} current_seq={current_seq} kind={kind} forward_client_output_alloc_ms={:.3} forward_client_enqueue_ms={:.3} forward_runner_task_ms={:.3} forward_register_call_count={} forward_action_defer_count={} readback_get_mapped_range_ms={:.3}",
            stats.forward_client_output_alloc_ms,
            stats.forward_client_enqueue_ms,
            stats.forward_runner_task_ms,
            stats.forward_register_call_count,
            stats.forward_action_defer_count,
            stats.readback_get_mapped_range_ms,
        );
        log::debug!("{summary_v2}");
        if std::env::var_os("NEUFLOW_TRACE_STDERR").is_some() {
            eprintln!("{summary_v2}");
        }
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Find the NeuFlow v2 weight file (.bpk format).
pub fn find_weight_file() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![
        "src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk".into(),
        "../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk".into(),
        "../../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk".into(),
        "neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk".into(),
        "generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk".into(),
        "src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk".into(),
        "../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk".into(),
        "../../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk".into(),
        "neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk".into(),
        "generated_mixed/neuflow_v2_mixed_fp16.bpk".into(),
    ];

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk"));
            candidates.push(dir.join("../generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk"));
            candidates.push(dir.join("../../generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk"));
            candidates.push(dir.join("src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk"));
            candidates.push(dir.join("../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk"));
            candidates.push(dir.join("../../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16_optimized.bpk"));
            candidates.push(dir.join("generated_mixed/neuflow_v2_mixed_fp16.bpk"));
            candidates.push(dir.join("../generated_mixed/neuflow_v2_mixed_fp16.bpk"));
            candidates.push(dir.join("../../generated_mixed/neuflow_v2_mixed_fp16.bpk"));
            candidates.push(dir.join("src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk"));
            candidates.push(dir.join("../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk"));
            candidates.push(dir.join("../../src/core/neuflow_burn/generated_mixed/neuflow_v2_mixed_fp16.bpk"));
        }
    }

    candidates.into_iter().find(|p| p.exists())
}

/// Check if NeuFlow v2 model is available.
pub fn is_available() -> bool {
    find_weight_file().is_some()
}

/// Run NeuFlow inference on a pair of preprocessed images.
///
/// img0, img1: owned CHW float32 `[3 * H * W]` in 0-255 range, where H=480, W=640.
/// Returns CHW flow `[2 * H * W]`: `[dx_plane..., dy_plane...]`.
///
/// Thread-safe: sends request to the dedicated inference thread via channel.
/// Zero-copy: owned Vecs are moved directly into TensorData without cloning.
pub fn infer(img0: Vec<f32>, img1: Vec<f32>, h: usize, w: usize) -> Result<Vec<f32>, String> {
    let expected = 3 * h * w;
    if img0.len() != expected || img1.len() != expected {
        return Err(format!(
            "Input size mismatch: expected {expected}, got img0={}, img1={}",
            img0.len(),
            img1.len()
        ));
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        infer_via_channel(img0, img1, h, w)
    }));

    match result {
        Ok(r) => r,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else if let Some(s) = e.downcast_ref::<&str>() {
                s.to_string()
            } else {
                "Unknown panic in NeuFlow inference".to_string()
            };
            Err(format!("NeuFlow Burn panicked: {msg}"))
        }
    }
}

/// Send an inference request to the dedicated thread and wait for the result.
fn infer_via_channel(img0: Vec<f32>, img1: Vec<f32>, h: usize, w: usize) -> Result<Vec<f32>, String> {
    let handle = INFERENCE_HANDLE
        .get_or_init(|| spawn_inference_thread())
        .as_ref()
        .map_err(|e| e.clone())?;

    let (reply_tx, reply_rx) = mpsc::channel();
    let seq = neuflow_trace::next_seq();

    let t_send = Instant::now();

    // Zero-copy: move owned Vecs directly into the request
    handle.sender.send(InferRequest::Infer {
        seq,
        img0,
        img1,
        h,
        w,
        reply: reply_tx,
    }).map_err(|_| "Inference thread channel closed".to_string())?;

    let result = reply_rx.recv()
        .map_err(|_| "Inference thread reply channel closed (thread may have panicked)".to_string())?;

    if neuflow_trace::enabled() {
        neuflow_trace::record_duration(seq, DurationMetric::ChannelRoundtrip, t_send.elapsed());
        log_trace_summary(seq, "full");
    } else {
        let roundtrip_ms = t_send.elapsed().as_secs_f64() * 1000.0;
        log::debug!("[NeuFlow perf] channel_roundtrip={roundtrip_ms:.1}ms");
    }

    result
}

/// Run inference + GPU-side sparse sampling. Main production path.
///
/// Instead of reading back the full flow tensor (614K floats), this performs
/// a GPU `select()` to extract flow values only at the specified grid points,
/// then reads back just ~400 floats. ~1500x less data transfer.
///
/// grid_points: (x, y) coordinates in model space
/// linear_indices: corresponding y*w+x values as i32
pub fn infer_and_sample(
    img0: Vec<f32>, img1: Vec<f32>, h: usize, w: usize,
    grid_points: Vec<(usize, usize)>,
    linear_indices: Vec<i32>,
) -> Result<SampledFlow, String> {
    let expected = 3 * h * w;
    if img0.len() != expected || img1.len() != expected {
        return Err(format!(
            "Input size mismatch: expected {expected}, got img0={}, img1={}",
            img0.len(), img1.len()
        ));
    }
    if grid_points.is_empty() {
        return Err("No grid points provided".to_string());
    }

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let handle = INFERENCE_HANDLE
            .get_or_init(|| spawn_inference_thread())
            .as_ref()
            .map_err(|e| e.clone())?;

        let (reply_tx, reply_rx) = mpsc::channel();
        let seq = neuflow_trace::next_seq();
        let t_send = Instant::now();

        let depth = QUEUE_DEPTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        log::debug!("[NeuFlow perf] queue_depth_at_send={}", depth + 1);

        handle.sender.send(InferRequest::InferAndSample {
            seq,
            img0, img1, h, w, grid_points, linear_indices,
            reply: reply_tx,
        }).map_err(|_| "Inference thread channel closed".to_string())?;

        let result = reply_rx.recv()
            .map_err(|_| "Inference thread reply channel closed".to_string())?;

        if neuflow_trace::enabled() {
            neuflow_trace::record_duration(seq, DurationMetric::ChannelRoundtrip, t_send.elapsed());
            log_trace_summary(seq, "sample");
        } else {
            let roundtrip_ms = t_send.elapsed().as_secs_f64() * 1000.0;
            log::debug!("[NeuFlow perf] sample_channel_roundtrip={roundtrip_ms:.1}ms");
        }

        result
    }));

    match result {
        Ok(r) => r,
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<String>() { s.clone() }
                else if let Some(s) = e.downcast_ref::<&str>() { s.to_string() }
                else { "Unknown panic".to_string() };
            Err(format!("NeuFlow Burn panicked: {msg}"))
        }
    }
}

/// Pre-initialize the model and warm up the GPU.
pub fn ensure_ready() {
    if !is_available() {
        log::info!("NeuFlow Burn: model not found, skipping pre-init");
        return;
    }

    let start = Instant::now();
    let handle = INFERENCE_HANDLE.get_or_init(|| spawn_inference_thread());
    match handle {
        Ok(h) => {
            let (reply_tx, reply_rx) = mpsc::channel();
            if h.sender.send(InferRequest::Warmup { reply: reply_tx }).is_ok() {
                match reply_rx.recv() {
                    Ok(Ok(())) => log::info!("NeuFlow Burn: pre-init + warmup complete in {:?}", start.elapsed()),
                    Ok(Err(e)) => log::error!("NeuFlow Burn: warmup failed: {e}"),
                    Err(_) => log::error!("NeuFlow Burn: warmup reply channel closed"),
                }
            } else {
                log::error!("NeuFlow Burn: failed to send warmup request");
            }
        }
        Err(e) => log::error!("NeuFlow Burn: pre-init failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_weight_file() {
        let _ = find_weight_file();
    }

    #[test]
    fn test_is_available_no_crash() {
        let _ = is_available();
    }

    #[test]
    fn test_infer_input_validation() {
        let bad = vec![0.0f32; 10];
        let result = infer(bad.clone(), bad, 480, 640);
        assert!(result.is_err());
    }

    /// End-to-end inference performance test.
    /// Verifies ≤40ms average inference latency on GPU (warmup excluded).
    #[test]
    #[ignore]
    fn test_e2e_inference_perf() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        ensure_ready();
        for _ in 0..10 {
            let img0 = vec![128.0f32; size];
            let img1 = vec![100.0f32; size];
            let _ = infer(img0, img1, h, w).expect("Warmup inference failed");
        }

        let mut times = Vec::new();
        for _ in 0..10 {
            let img0 = vec![128.0f32; size];
            let img1 = vec![100.0f32; size];
            let start = Instant::now();
            let flow = infer(img0, img1, h, w).expect("Inference failed");
            let elapsed = start.elapsed();
            times.push(elapsed);

            assert_eq!(flow.len(), h * w * 2, "Flow size mismatch");
            assert!(flow.iter().all(|v| v.is_finite()), "Non-finite flow values");
        }

        let avg_ms = times.iter().map(|t| t.as_secs_f64() * 1000.0).sum::<f64>() / times.len() as f64;
        let min_ms = times.iter().map(|t| t.as_secs_f64() * 1000.0).fold(f64::INFINITY, f64::min);
        let max_ms = times.iter().map(|t| t.as_secs_f64() * 1000.0).fold(0.0f64, f64::max);

        eprintln!("NeuFlow Burn inference perf (10 runs after warmup):");
        eprintln!("  Avg: {avg_ms:.1}ms, Min: {min_ms:.1}ms, Max: {max_ms:.1}ms");
        for (i, t) in times.iter().enumerate() {
            eprintln!("  Run {}: {:.1}ms", i + 1, t.as_secs_f64() * 1000.0);
        }

        assert!(avg_ms <= 40.0, "Average inference too slow: {avg_ms:.1}ms (target ≤ 40ms)");
    }

    /// End-to-end pipeline test: RGB frame → preprocess → infer (CHW) → sample.
    #[test]
    #[ignore]
    fn test_e2e_pipeline() {
        let w = 640u32;
        let h = 480u32;

        let mut rgb0 = vec![0u8; (w * h * 3) as usize];
        let mut rgb1 = vec![0u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 3) as usize;
                let block = ((x / 32) + (y / 32)) % 2 == 0;
                let val = if block { 200u8 } else { 50u8 };
                rgb0[idx] = val;
                rgb0[idx + 1] = val;
                rgb0[idx + 2] = val;
                let x2 = (x + 2).min(w - 1);
                let idx2 = ((y * w + x2) * 3) as usize;
                rgb1[idx] = rgb0[idx2.min(rgb0.len() - 3)];
                rgb1[idx + 1] = rgb0[(idx2 + 1).min(rgb0.len() - 2)];
                rgb1[idx + 2] = rgb0[(idx2 + 2).min(rgb0.len() - 1)];
            }
        }

        let target_h = 480usize;
        let target_w = 640usize;
        let scale = (target_w as f32 / w as f32).min(target_h as f32 / h as f32);
        let new_w = (w as f32 * scale) as usize;
        let new_h = (h as f32 * scale) as usize;
        let pad_top = (target_h - new_h) / 2;
        let pad_left = (target_w - new_w) / 2;
        let inv_scale = 1.0 / scale;
        let plane = target_h * target_w;

        let preprocess = |rgb: &[u8]| -> Vec<f32> {
            let mut chw = vec![0.0f32; 3 * plane];
            for out_y in pad_top..(pad_top + new_h) {
                let src_yf = (out_y - pad_top) as f32 * inv_scale;
                let sy0 = (src_yf as usize).min(h as usize - 1);
                let sy1 = (sy0 + 1).min(h as usize - 1);
                let fy = src_yf - sy0 as f32;
                for out_x in pad_left..(pad_left + new_w) {
                    let src_xf = (out_x - pad_left) as f32 * inv_scale;
                    let sx0 = (src_xf as usize).min(w as usize - 1);
                    let sx1 = (sx0 + 1).min(w as usize - 1);
                    let fx = src_xf - sx0 as f32;
                    let w00 = (1.0 - fx) * (1.0 - fy);
                    let w10 = fx * (1.0 - fy);
                    let w01 = (1.0 - fx) * fy;
                    let w11 = fx * fy;
                    let i00 = (sy0 * w as usize + sx0) * 3;
                    let i10 = (sy0 * w as usize + sx1) * 3;
                    let i01 = (sy1 * w as usize + sx0) * 3;
                    let i11 = (sy1 * w as usize + sx1) * 3;
                    let out_idx = out_y * target_w + out_x;
                    for c in 0..3 {
                        let v = rgb[i00 + c] as f32 * w00
                            + rgb[i10 + c] as f32 * w10
                            + rgb[i01 + c] as f32 * w01
                            + rgb[i11 + c] as f32 * w11;
                        chw[c * plane + out_idx] = v;
                    }
                }
            }
            chw
        };

        let img0 = preprocess(&rgb0);
        let img1 = preprocess(&rgb1);

        ensure_ready();

        let flow = infer(img0, img1, target_h, target_w)
            .expect("Inference failed in pipeline test");

        // CHW layout: flow[0..plane] = dx, flow[plane..2*plane] = dy
        assert_eq!(flow.len(), target_h * target_w * 2, "Flow output size mismatch");
        assert!(flow.iter().all(|v| v.is_finite()), "Non-finite flow values in pipeline");

        // Sample sparse points using CHW layout
        let step = (target_w / 15).max(4);
        let mut from_pts = Vec::new();
        let mut to_pts = Vec::new();
        for x in (0..target_w).step_by(step) {
            for y in (0..target_h).step_by(step) {
                let idx = y * target_w + x;
                let dx = flow[idx];
                let dy = flow[plane + idx];
                if !dx.is_finite() || !dy.is_finite() { continue; }
                let to_x = x as f32 + dx;
                let to_y = y as f32 + dy;
                if to_x >= 0.0 && to_x < target_w as f32 && to_y >= 0.0 && to_y < target_h as f32 {
                    from_pts.push((x as f32, y as f32));
                    to_pts.push((to_x, to_y));
                }
            }
        }

        eprintln!("Pipeline test: {} point correspondences from {}x{} flow",
            from_pts.len(), target_w, target_h);
        assert!(from_pts.len() >= 10, "Too few point correspondences: {}", from_pts.len());
    }

    /// Thread safety test: multiple threads call infer() concurrently.
    #[test]
    #[ignore]
    fn test_inference_thread_safety() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        ensure_ready();
        let img = vec![128.0f32; size];
        let _ = infer(img.clone(), img, h, w);

        let threads: Vec<_> = (0..4).map(|tid| {
            std::thread::spawn(move || {
                for i in 0..3 {
                    let img0 = vec![100.0f32 + tid as f32 * 10.0; size];
                    let img1 = vec![120.0f32 + tid as f32 * 10.0; size];
                    match infer(img0, img1, h, w) {
                        Ok(flow) => {
                            assert_eq!(flow.len(), h * w * 2);
                            assert!(flow.iter().all(|v| v.is_finite()));
                        }
                        Err(e) => panic!("Thread {tid} run {i} failed: {e}"),
                    }
                }
            })
        }).collect();

        for t in threads {
            t.join().expect("Thread panicked during concurrent inference test");
        }
        eprintln!("Thread safety test: 4 threads × 3 inferences completed without panic");
    }

    /// Verify fusion enabled + no panics under dedicated thread.
    #[test]
    #[ignore]
    fn test_fusion_no_panic() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        ensure_ready();

        for i in 0..5 {
            let img0 = vec![80.0f32 + i as f32 * 20.0; size];
            let img1 = vec![90.0f32 + i as f32 * 20.0; size];
            let result = infer(img0, img1, h, w);
            assert!(result.is_ok(), "Fusion panic on run {i}: {:?}", result.err());
            let flow = result.unwrap();
            assert_eq!(flow.len(), h * w * 2);
            assert!(flow.iter().all(|v| v.is_finite()), "Non-finite on run {i}");
        }
        eprintln!("Fusion test: 5 consecutive inferences completed without panic");
    }

    /// GPU sparse sampling correctness + performance test.
    #[test]
    #[ignore]
    fn test_gpu_sparse_sample() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        ensure_ready();
        // Warmup
        for _ in 0..5 {
            let img = vec![128.0f32; size];
            let _ = infer(img.clone(), img, h, w);
        }

        let img0 = vec![128.0f32; size];
        let img1 = vec![100.0f32; size];

        // Generate grid points (simulating compute_sample_grid)
        let step = (w / 15).max(4);
        let mut grid_points = Vec::new();
        let mut linear_indices = Vec::new();
        for x in (0..w).step_by(step) {
            for y in (0..h).step_by(step) {
                grid_points.push((x, y));
                linear_indices.push((y * w + x) as i32);
            }
        }
        let num_pts = grid_points.len();
        eprintln!("GPU sparse sample test: {num_pts} grid points");

        // Run infer_and_sample
        let start = Instant::now();
        let sampled = infer_and_sample(
            img0.clone(), img1.clone(), h, w,
            grid_points.clone(), linear_indices.clone(),
        ).expect("infer_and_sample failed");
        let sample_ms = start.elapsed().as_secs_f64() * 1000.0;

        // Run full infer for comparison
        let start2 = Instant::now();
        let full_flow = infer(img0, img1, h, w).expect("infer failed");
        let full_ms = start2.elapsed().as_secs_f64() * 1000.0;

        // Verify correctness: sampled values should match full flow at grid points
        assert_eq!(sampled.dx.len(), num_pts);
        assert_eq!(sampled.dy.len(), num_pts);
        let plane = h * w;
        for (i, &(x, y)) in sampled.grid_points.iter().enumerate() {
            let idx = y * w + x;
            let expected_dx = full_flow[idx];
            let expected_dy = full_flow[plane + idx];
            let actual_dx = sampled.dx[i];
            let actual_dy = sampled.dy[i];
            assert!(
                (actual_dx - expected_dx).abs() < 0.1,
                "dx mismatch at ({x},{y}): expected {expected_dx}, got {actual_dx}"
            );
            assert!(
                (actual_dy - expected_dy).abs() < 0.1,
                "dy mismatch at ({x},{y}): expected {expected_dy}, got {actual_dy}"
            );
        }

        eprintln!("GPU sparse sample: {sample_ms:.1}ms vs full readback: {full_ms:.1}ms (speedup: {:.1}x)",
            full_ms / sample_ms);
        eprintln!("All {num_pts} points match between GPU select and full readback");
    }

    /// Throughput burst test: 4 threads × 5 inferences, measures aggregate throughput.
    #[test]
    #[ignore]
    fn test_throughput_burst() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        ensure_ready();
        // Extra warmup
        for _ in 0..3 {
            let img = vec![128.0f32; size];
            let _ = infer(img.clone(), img, h, w);
        }

        let start = Instant::now();
        let threads: Vec<_> = (0..4).map(|tid| {
            std::thread::spawn(move || {
                for _ in 0..5 {
                    let img0 = vec![100.0f32 + tid as f32 * 10.0; size];
                    let img1 = vec![120.0f32 + tid as f32 * 10.0; size];
                    infer(img0, img1, h, w).expect("Burst inference failed");
                }
            })
        }).collect();

        for t in threads {
            t.join().expect("Burst thread panicked");
        }
        let total = start.elapsed();
        let throughput = 20.0 / total.as_secs_f64();

        eprintln!("Burst throughput: {throughput:.1} infer/sec, total: {total:?} for 20 inferences");
        // 20 × ~31ms = 620ms theoretical min. Allow 80% margin.
        assert!(total.as_millis() <= 1100, "Burst too slow: {total:?} (target ≤ 1100ms)");
    }
}
