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
//! Model I/O (same as ORT version):
//! - Input: img0 [1, 3, 480, 640] float32, img1 [1, 3, 480, 640] float32 (0-255 range)
//! - Output: flow [1, 2, 480, 640] float32 (dense optical flow)

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

use burn::prelude::*;
use burn::tensor::TensorData;
use burn_store::ModuleSnapshot;
use half::f16;

use neuflow_v2_clean::Model;

// Backend type: Wgpu<f16> — all compute in FP16.
// Enables CMMA (cooperative matrix) on Vulkan via SPV_KHR_cooperative_matrix.
// Generated constants patched to f16 via .convert::<half::f16>().
// .bpk weights auto-converted F32→F16 via F16LoadAdapter at load time.
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

/// Request sent to the inference thread.
enum InferRequest {
    /// Run inference on a pair of preprocessed images.
    Infer {
        img0: Vec<f32>,
        img1: Vec<f32>,
        h: usize,
        w: usize,
        reply: std::sync::mpsc::Sender<Result<Vec<f32>, String>>,
    },
    /// Warm up the model (dummy inference to trigger autotune caching).
    Warmup {
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
}

// InferRequest contains Sender which is Send, and Vec<f32> is Send.
// The Burn model and device live only on the inference thread.

/// Handle to the inference thread, holding the sender end of the channel.
struct InferenceHandle {
    sender: mpsc::Sender<InferRequest>,
}

static INFERENCE_HANDLE: OnceLock<Result<InferenceHandle, String>> = OnceLock::new();

/// Spawn the inference thread: loads model, then loops on channel receiving requests.
fn spawn_inference_thread() -> Result<InferenceHandle, String> {
    let (tx, rx) = mpsc::channel::<InferRequest>();

    // Use a oneshot to propagate init errors back to the caller.
    let (init_tx, init_rx) = mpsc::channel::<Result<(), String>>();

    std::thread::Builder::new()
        .name("neuflow-infer".to_string())
        .spawn(move || {
            // Set autotune level before any Burn/CubeCL operation on this thread.
            // Level 0 (minimal) tries fewest strategies → least VRAM for benchmarks.
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

    // Wait for initialization to complete.
    let init_result = init_rx.recv()
        .map_err(|_| "Inference thread init channel closed unexpectedly".to_string())?;
    init_result?;

    Ok(InferenceHandle { sender: tx })
}

/// The main loop running on the inference thread.
/// Owns the Model and WgpuDevice — all Burn operations happen here.
fn inference_loop(model: Model<B>, device: burn::backend::wgpu::WgpuDevice, rx: mpsc::Receiver<InferRequest>) {
    for request in rx {
        match request {
            InferRequest::Infer { img0, img1, h, w, reply } => {
                let result = run_inference(&model, &device, &img0, &img1, h, w);
                let _ = reply.send(result);
            }
            InferRequest::Warmup { reply } => {
                let result = run_warmup(&model, &device);
                let _ = reply.send(result);
            }
        }
    }
    log::info!("NeuFlow Burn: inference thread exiting (channel closed)");
}

/// Execute a single inference on the inference thread.
fn run_inference(model: &Model<B>, device: &burn::backend::wgpu::WgpuDevice, img0: &[f32], img1: &[f32], h: usize, w: usize) -> Result<Vec<f32>, String> {
    // Create input tensors [1, 3, H, W]
    let shape = [1usize, 3, h, w];
    let img0_data = TensorData::new(img0.to_vec(), shape);
    let img1_data = TensorData::new(img1.to_vec(), shape);

    let img0_tensor = Tensor::<B, 4>::from_data(img0_data, device);
    let img1_tensor = Tensor::<B, 4>::from_data(img1_data, device);

    // Run inference
    let flow_tensor = model.forward(img0_tensor, img1_tensor);

    // Extract flow: [1, 2, H, W] -> interleaved [H*W*2]
    // Output tensor is F16, convert to F32 for caller compatibility
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

    // CHW [2, H, W] → interleaved HW2 [dx0,dy0,dx1,dy1,...]
    let mut flow = vec![0.0f32; plane * 2];
    let (dx_plane, dy_plane) = (&flow_vec[..plane], &flow_vec[plane..2 * plane]);
    for i in 0..plane {
        flow[i * 2] = dx_plane[i];
        flow[i * 2 + 1] = dy_plane[i];
    }

    Ok(flow)
}

/// Run a dummy inference to trigger autotune caching on the inference thread.
fn run_warmup(model: &Model<B>, device: &burn::backend::wgpu::WgpuDevice) -> Result<(), String> {
    let h = 480usize;
    let w = 640usize;
    let dummy = vec![128.0f32; 3 * h * w];
    let shape = [1usize, 3, h, w];

    let img0_data = TensorData::new(dummy.clone(), shape);
    let img1_data = TensorData::new(dummy, shape);

    let img0_tensor = Tensor::<B, 4>::from_data(img0_data, device);
    let img1_tensor = Tensor::<B, 4>::from_data(img1_data, device);

    // This triggers autotune strategy search + caching to disk
    let _flow = model.forward(img0_tensor, img1_tensor);
    // Force sync to ensure autotune completes
    let _data = _flow.into_data();

    log::info!("NeuFlow Burn: warmup inference complete (autotune cached)");
    Ok(())
}

// ---------------------------------------------------------------------------
// Model initialization (called on the inference thread)
// ---------------------------------------------------------------------------

/// Initialize the Burn model with Wgpu (Vulkan/Metal) backend.
/// Called once on the inference thread.
fn init_model() -> Result<(Model<B>, burn::backend::wgpu::WgpuDevice), String> {
    let path = find_weight_file()
        .ok_or_else(|| "NeuFlow Burn model (.bpk) not found".to_string())?;

    log::info!("NeuFlow Burn: loading model from {} (F16 mode)", path.display());

    let device = burn::backend::wgpu::WgpuDevice::default();

    // Create model structure (all params uninitialized, will be filled from .bpk)
    let mut model = Model::<B>::new(&device);

    // Load weights with F16 adapter: F32 .bpk data → F16 tensors
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

    // Also try relative to the executable
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
/// img0, img1: CHW float32 `[3 * H * W]` in 0-255 range, where H=480, W=640.
/// Returns interleaved flow `[H*W*2]`: `[dx0,dy0,dx1,dy1,...]`.
///
/// Thread-safe: sends request to the dedicated inference thread via channel.
/// Multiple callers (rayon threads) can call this concurrently; requests are
/// serialized on the inference thread.
pub fn infer(img0: &[f32], img1: &[f32], h: usize, w: usize) -> Result<Vec<f32>, String> {
    let expected = 3 * h * w;
    if img0.len() != expected || img1.len() != expected {
        return Err(format!(
            "Input size mismatch: expected {expected}, got img0={}, img1={}",
            img0.len(),
            img1.len()
        ));
    }

    // Catch panics from channel operations
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
fn infer_via_channel(img0: &[f32], img1: &[f32], h: usize, w: usize) -> Result<Vec<f32>, String> {
    let handle = INFERENCE_HANDLE
        .get_or_init(|| spawn_inference_thread())
        .as_ref()
        .map_err(|e| e.clone())?;

    let (reply_tx, reply_rx) = mpsc::channel();

    handle.sender.send(InferRequest::Infer {
        img0: img0.to_vec(),
        img1: img1.to_vec(),
        h,
        w,
        reply: reply_tx,
    }).map_err(|_| "Inference thread channel closed".to_string())?;

    reply_rx.recv()
        .map_err(|_| "Inference thread reply channel closed (thread may have panicked)".to_string())?
}

/// Pre-initialize the model and warm up the GPU.
/// Call from a background thread at app startup to hide init latency.
///
/// Spawns the inference thread if not already running, then sends a warmup
/// request that executes a dummy inference to trigger autotune caching.
/// The autotune runs on the inference thread (correct thread-local context)
/// while VRAM is still free (before the rendering pipeline starts).
pub fn ensure_ready() {
    if !is_available() {
        log::info!("NeuFlow Burn: model not found, skipping pre-init");
        return;
    }

    let start = std::time::Instant::now();
    let handle = INFERENCE_HANDLE.get_or_init(|| spawn_inference_thread());
    match handle {
        Ok(h) => {
            // Send warmup request to the inference thread
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
        let result = infer(&bad, &bad, 480, 640);
        assert!(result.is_err());
    }

    /// End-to-end inference performance test.
    /// Verifies ≤40ms average inference latency on GPU (warmup excluded).
    #[test]
    #[ignore] // Run with: cargo test --features neuflow --release -- --ignored test_e2e_inference_perf
    fn test_e2e_inference_perf() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        let img0 = vec![128.0f32; size];
        let img1 = vec![100.0f32; size];

        // Warmup (triggers autotune + JIT compilation + GPU stabilization)
        ensure_ready();
        for _ in 0..5 {
            let _ = infer(&img0, &img1, h, w).expect("Warmup inference failed");
        }

        // Benchmark 10 runs (steady state)
        let mut times = Vec::new();
        for _ in 0..10 {
            let start = std::time::Instant::now();
            let flow = infer(&img0, &img1, h, w).expect("Inference failed");
            let elapsed = start.elapsed();
            times.push(elapsed);

            // Verify output shape and finiteness
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

        assert!(
            avg_ms <= 40.0,
            "Average inference too slow: {avg_ms:.1}ms (target ≤ 40ms)"
        );
    }

    /// End-to-end pipeline test: RGB frame → preprocess_frame → infer → sample_from_dense_flow.
    /// Verifies the full chain produces valid point correspondences.
    #[test]
    #[ignore] // Run with: cargo test --features neuflow --release -- --ignored test_e2e_pipeline
    fn test_e2e_pipeline() {
        let w = 640u32;
        let h = 480u32;

        // Create a synthetic RGB frame with texture (checkerboard pattern)
        let mut rgb0 = vec![0u8; (w * h * 3) as usize];
        let mut rgb1 = vec![0u8; (w * h * 3) as usize];
        for y in 0..h {
            for x in 0..w {
                let idx = ((y * w + x) * 3) as usize;
                // Checkerboard pattern for texture
                let block = ((x / 32) + (y / 32)) % 2 == 0;
                let val = if block { 200u8 } else { 50u8 };
                rgb0[idx] = val;
                rgb0[idx + 1] = val;
                rgb0[idx + 2] = val;
                // Slightly shifted for rgb1 (simulate small motion)
                let x2 = (x + 2).min(w - 1);
                let idx2 = ((y * w + x2) * 3) as usize;
                rgb1[idx] = rgb0[idx2.min(rgb0.len() - 3)];
                rgb1[idx + 1] = rgb0[(idx2 + 1).min(rgb0.len() - 2)];
                rgb1[idx + 2] = rgb0[(idx2 + 2).min(rgb0.len() - 1)];
            }
        }

        // Preprocess (reuse the function from optical_flow::neuflow)
        // Since preprocess_frame is not public from here, replicate the logic inline
        let target_h = 480usize;
        let target_w = 640usize;
        let scale = (target_w as f32 / w as f32).min(target_h as f32 / h as f32);
        let new_w = (w as f32 * scale) as usize;
        let new_h = (h as f32 * scale) as usize;
        let pad_top = (target_h - new_h) / 2;
        let pad_left = (target_w - new_w) / 2;
        let inv_scale = 1.0 / scale;
        let plane = target_h * target_w;

        let preprocess = |rgb: &[u8]| -> (Vec<f32>, Vec<u8>) {
            let mut chw = vec![0.0f32; 3 * plane];
            let mut gray = vec![0u8; plane];
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
                    gray[out_idx] = chw[out_idx] as u8;
                }
            }
            (chw, gray)
        };

        let (img0, gray0) = preprocess(&rgb0);
        let (img1, _) = preprocess(&rgb1);

        // Warmup
        ensure_ready();

        // Infer
        let flow = infer(&img0, &img1, target_h, target_w)
            .expect("Inference failed in pipeline test");

        // Verify flow shape
        assert_eq!(flow.len(), target_h * target_w * 2, "Flow output size mismatch");
        assert!(flow.iter().all(|v| v.is_finite()), "Non-finite flow values in pipeline");

        // Sample sparse points (inline version of sample_from_dense_flow)
        let step = (target_w / 15).max(4);
        let mut from_pts = Vec::new();
        let mut to_pts = Vec::new();
        for x in (0..target_w).step_by(step) {
            for y in (0..target_h).step_by(step) {
                let idx = (y * target_w + x) * 2;
                let dx = flow[idx];
                let dy = flow[idx + 1];
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
        assert!(
            from_pts.len() >= 10,
            "Too few point correspondences: {} (expected >= 10)",
            from_pts.len()
        );
    }

    /// Thread safety test: multiple threads call infer() concurrently.
    /// Verifies no panics occur (requests are serialized on the inference thread).
    #[test]
    #[ignore] // Run with: cargo test --features neuflow --release -- --ignored test_inference_thread_safety
    fn test_inference_thread_safety() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        // Warmup
        ensure_ready();
        let img = vec![128.0f32; size];
        let _ = infer(&img, &img, h, w);

        // Spawn 4 threads each doing 3 inferences
        let threads: Vec<_> = (0..4).map(|tid| {
            std::thread::spawn(move || {
                let img0 = vec![100.0f32 + tid as f32 * 10.0; size];
                let img1 = vec![120.0f32 + tid as f32 * 10.0; size];
                for i in 0..3 {
                    match infer(&img0, &img1, h, w) {
                        Ok(flow) => {
                            assert_eq!(flow.len(), h * w * 2);
                            assert!(flow.iter().all(|v| v.is_finite()));
                        }
                        Err(e) => {
                            panic!("Thread {tid} run {i} failed: {e}");
                        }
                    }
                }
            })
        }).collect();

        for t in threads {
            t.join().expect("Thread panicked during concurrent inference test");
        }
        eprintln!("Thread safety test: 4 threads × 3 inferences completed without panic");
    }

    /// Verify that fusion is enabled and inference doesn't panic.
    /// With the dedicated inference thread, fusion stream safety is guaranteed.
    #[test]
    #[ignore] // Run with: cargo test --features neuflow --release -- --ignored test_fusion_no_panic
    fn test_fusion_no_panic() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        ensure_ready();

        // Run 5 consecutive inferences — fusion stream accumulates operations
        for i in 0..5 {
            let img0 = vec![80.0f32 + i as f32 * 20.0; size];
            let img1 = vec![90.0f32 + i as f32 * 20.0; size];
            let result = infer(&img0, &img1, h, w);
            assert!(result.is_ok(), "Fusion panic on run {i}: {:?}", result.err());
            let flow = result.unwrap();
            assert_eq!(flow.len(), h * w * 2);
            assert!(flow.iter().all(|v| v.is_finite()), "Non-finite on run {i}");
        }
        eprintln!("Fusion test: 5 consecutive inferences completed without panic");
    }
}
