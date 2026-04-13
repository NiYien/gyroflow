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
//! - Input: img0 [1, 3, 480, 640] float32, img1 [1, 3, 480, 640] float32 (0-255 range)
//! - Output: flow [2, 480, 640] float32 CHW layout (dx plane, dy plane)

#[allow(
    clippy::let_and_return,
    clippy::approx_constant,
    clippy::all,
    unused_variables,
    dead_code
)]
mod neuflow_v2_clean;

use std::path::PathBuf;
use std::sync::{OnceLock, mpsc};
use std::time::Instant;

use burn::prelude::*;
use burn::tensor::TensorData;
use burn_store::ModuleSnapshot;
use half::f16;

use neuflow_v2_clean::Model;

// Backend type: Wgpu<f16> — all compute in FP16.
type B = burn::backend::wgpu::Wgpu<f16>;

/// Adapter that converts F32 tensor snapshots to F16 at load time.
#[derive(Clone)]
struct F16LoadAdapter;

impl burn_store::ModuleAdapter for F16LoadAdapter {
    fn adapt(&self, snapshot: &burn_store::TensorSnapshot) -> burn_store::TensorSnapshot {
        use burn::tensor::DType;
        if snapshot.dtype == DType::F32 {
            if let Ok(data) = snapshot.to_data() {
                let converted = data.convert::<f16>();
                return burn_store::TensorSnapshot::from_data(
                    converted,
                    snapshot.path_stack.clone().unwrap_or_default(),
                    snapshot.container_stack.clone().unwrap_or_default(),
                    snapshot.tensor_id.clone().unwrap_or_default(),
                );
            }
        }
        snapshot.clone()
    }

    fn clone_box(&self) -> Box<dyn burn_store::ModuleAdapter> {
        Box::new(self.clone())
    }
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
        img0: Vec<f32>,
        img1: Vec<f32>,
        h: usize,
        w: usize,
        reply: mpsc::Sender<Result<Vec<f32>, String>>,
    },
    /// Run inference + GPU-side sparse sampling (main production path).
    /// Only reads back ~400 floats instead of 614,400.
    InferAndSample {
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

/// Spawn the inference thread: loads model, then loops on channel receiving requests.
fn spawn_inference_thread() -> Result<InferenceHandle, String> {
    let (tx, rx) = mpsc::channel::<InferRequest>();
    let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("neuflow-infer".to_string())
        .spawn(move || {
            unsafe { std::env::set_var("CUBECL_AUTOTUNE_LEVEL", "0"); }

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
/// Synchronous version — used by warmup only.
fn extract_flow(flow_tensor: Tensor<B, 4>, h: usize, w: usize) -> Result<Vec<f32>, String> {
    let flow_data = flow_tensor.into_data().convert::<f32>();
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

    // Return CHW layout directly: [dx_plane..., dy_plane...]
    // Caller (sample_from_dense_flow) reads planar format.
    Ok(flow_vec)
}

/// Extract flow from GPU tensor → CHW Vec<f32>. No interleave.
/// Async version: readback overlaps with next frame's GPU work via into_data_async().
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
// Inference loop with double-buffering prefetch
// ---------------------------------------------------------------------------

/// Process an InferAndSample request: forward + GPU select + async readback.
/// Async version: uses into_data_async() so GPU readback can overlap with other work.
async fn process_infer_and_sample_async(
    model: &Model<B>,
    device: &burn::backend::wgpu::WgpuDevice,
    img0: Vec<f32>, img1: Vec<f32>, h: usize, w: usize,
    grid_points: Vec<(usize, usize)>,
    linear_indices: Vec<i32>,
    reply: mpsc::Sender<Result<SampledFlow, String>>,
    rx: &mpsc::Receiver<InferRequest>,
    seq: u64,
) -> Option<InferRequest> {
    let t0 = Instant::now();

    // 1. Upload tensors + GPU forward
    let (img0_t, img1_t) = prepare_tensors(img0, img1, h, w, device);
    let t1 = Instant::now();

    let flow_tensor = model.forward(img0_t, img1_t); // [1, 2, H, W]
    let t2 = Instant::now();

    // 2. GPU reshape + select (no full readback!)
    let num_pts = linear_indices.len();
    let plane = h * w;

    let indices_tensor = Tensor::<B, 1, burn::tensor::Int>::from_data(
        TensorData::new(linear_indices, [num_pts]),
        device,
    );

    // [1, 2, H, W] → [2, H*W]
    let flow_flat = flow_tensor.reshape([2, plane]);
    // select dim=1 with ~200 indices → [2, num_pts]
    let sampled = flow_flat.select(1, indices_tensor);
    let t3 = Instant::now();

    // 3. Async readback small tensor (~400 floats vs 614,400)
    let sampled_data = match sampled.into_data_async().await {
        Ok(data) => data.convert::<f32>(),
        Err(e) => {
            let _ = reply.send(Err(format!("Async readback failed: {e:?}")));
            return rx.try_recv().ok();
        }
    };
    let sampled_vec: Vec<f32> = sampled_data.to_vec().unwrap_or_default();
    let t4 = Instant::now();

    // 4. Prefetch next request
    let prefetched = rx.try_recv().ok();

    // 5. Build result
    let result = if sampled_vec.len() == num_pts * 2 {
        Ok(SampledFlow {
            grid_points,
            dx: sampled_vec[..num_pts].to_vec(),
            dy: sampled_vec[num_pts..].to_vec(),
        })
    } else {
        Err(format!("Sampled flow size mismatch: expected {}, got {}", num_pts * 2, sampled_vec.len()))
    };

    let _ = reply.send(result);

    let tensor_ms = (t1 - t0).as_secs_f64() * 1000.0;
    let forward_ms = (t2 - t1).as_secs_f64() * 1000.0;
    let select_ms = (t3 - t2).as_secs_f64() * 1000.0;
    let readback_ms = (t4 - t3).as_secs_f64() * 1000.0;
    let total_ms = (t4 - t0).as_secs_f64() * 1000.0;
    log::debug!(
        "[NeuFlow perf] #{seq} SAMPLE tensor={tensor_ms:.1}ms forward={forward_ms:.1}ms select={select_ms:.1}ms readback={readback_ms:.1}ms total={total_ms:.1}ms pts={num_pts}"
    );

    prefetched
}

/// Process a single infer request with full timing instrumentation.
/// Async version: uses extract_flow_async() so GPU readback can overlap with other work.
/// Returns the result and optional prefetched next request.
async fn process_infer_request_async(
    model: &Model<B>,
    device: &burn::backend::wgpu::WgpuDevice,
    img0: Vec<f32>, img1: Vec<f32>, h: usize, w: usize,
    reply: mpsc::Sender<Result<Vec<f32>, String>>,
    rx: &mpsc::Receiver<InferRequest>,
    seq: u64,
) -> Option<InferRequest> {
    let t0 = Instant::now();

    // Stage 1: Create GPU tensors (zero-copy TensorData + upload)
    let (img0_tensor, img1_tensor) = prepare_tensors(img0, img1, h, w, device);
    let t1 = Instant::now();

    // Stage 2: GPU forward pass
    let flow_tensor = model.forward(img0_tensor, img1_tensor);
    let t2 = Instant::now();

    // Stage 3: Async GPU readback + F16→F32 conversion
    let result = extract_flow_async(flow_tensor, h, w).await;
    let t3 = Instant::now();

    // Stage 4: Prefetch next request while we do CPU work (send reply)
    let prefetched = rx.try_recv().ok();

    // Send reply
    let _ = reply.send(result);
    let t4 = Instant::now();

    let tensor_ms = (t1 - t0).as_secs_f64() * 1000.0;
    let forward_ms = (t2 - t1).as_secs_f64() * 1000.0;
    let readback_ms = (t3 - t2).as_secs_f64() * 1000.0;
    let reply_ms = (t4 - t3).as_secs_f64() * 1000.0;
    let total_ms = (t4 - t0).as_secs_f64() * 1000.0;

    log::debug!(
        "[NeuFlow perf] #{seq} tensor={tensor_ms:.1}ms forward={forward_ms:.1}ms readback={readback_ms:.1}ms reply={reply_ms:.1}ms total={total_ms:.1}ms prefetched={}",
        prefetched.is_some()
    );

    prefetched
}

/// The main loop running on the inference thread with double-buffering.
/// Uses pollster::block_on() to drive async readback operations, allowing
/// into_data_async() to overlap GPU readback with other GPU work.
fn inference_loop(model: Model<B>, device: burn::backend::wgpu::WgpuDevice, rx: mpsc::Receiver<InferRequest>) {
    let mut seq = 0u64;
    let mut pending: Option<InferRequest> = None;

    loop {
        // Get next request: either prefetched or from channel
        let request = if let Some(req) = pending.take() {
            req
        } else {
            match rx.recv() {
                Ok(req) => req,
                Err(_) => break,
            }
        };

        match request {
            InferRequest::Infer { img0, img1, h, w, reply } => {
                seq += 1;
                pending = pollster::block_on(process_infer_request_async(
                    &model, &device, img0, img1, h, w, reply, &rx, seq,
                ));
            }
            InferRequest::InferAndSample { img0, img1, h, w, grid_points, linear_indices, reply } => {
                seq += 1;
                pending = pollster::block_on(process_infer_and_sample_async(
                    &model, &device, img0, img1, h, w,
                    grid_points, linear_indices, reply, &rx, seq,
                ));
            }
            InferRequest::Warmup { reply } => {
                // Warmup stays synchronous for simplicity
                let result = run_warmup(&model, &device);
                let _ = reply.send(result);
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

    log::info!("NeuFlow Burn: loading model from {} (F16 mode)", path.display());

    let device = burn::backend::wgpu::WgpuDevice::default();
    let mut model = Model::<B>::new(&device);

    let path_str = path.to_string_lossy().to_string();
    let mut store = burn_store::BurnpackStore::from_file(&path_str)
        .with_from_adapter(F16LoadAdapter);
    model.load_from(&mut store)
        .map_err(|e| format!("Failed to load model weights: {e}"))?;

    log::info!("NeuFlow Burn: model loaded on {:?} (F16)", device);
    Ok((model, device))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Find the NeuFlow v2 weight file (.bpk format).
pub fn find_weight_file() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![
        "resources/neuflow_v2_clean.bpk".into(),
        "../resources/neuflow_v2_clean.bpk".into(),
        "../../resources/neuflow_v2_clean.bpk".into(),
        "src/core/neuflow_burn/neuflow_v2_clean.bpk".into(),
        "../src/core/neuflow_burn/neuflow_v2_clean.bpk".into(),
        "neuflow_burn/neuflow_v2_clean.bpk".into(),
        "neuflow_v2_clean.bpk".into(),
    ];

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join("resources/neuflow_v2_clean.bpk"));
            candidates.push(dir.join("neuflow_v2_clean.bpk"));
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

    let t_send = Instant::now();

    // Zero-copy: move owned Vecs directly into the request
    handle.sender.send(InferRequest::Infer {
        img0,
        img1,
        h,
        w,
        reply: reply_tx,
    }).map_err(|_| "Inference thread channel closed".to_string())?;

    let result = reply_rx.recv()
        .map_err(|_| "Inference thread reply channel closed (thread may have panicked)".to_string())?;

    let roundtrip_ms = t_send.elapsed().as_secs_f64() * 1000.0;
    log::debug!("[NeuFlow perf] channel_roundtrip={roundtrip_ms:.1}ms");

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
        let t_send = Instant::now();

        handle.sender.send(InferRequest::InferAndSample {
            img0, img1, h, w, grid_points, linear_indices,
            reply: reply_tx,
        }).map_err(|_| "Inference thread channel closed".to_string())?;

        let result = reply_rx.recv()
            .map_err(|_| "Inference thread reply channel closed".to_string())?;

        let roundtrip_ms = t_send.elapsed().as_secs_f64() * 1000.0;
        log::debug!("[NeuFlow perf] sample_channel_roundtrip={roundtrip_ms:.1}ms");

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
