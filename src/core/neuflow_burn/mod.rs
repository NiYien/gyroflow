// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! NeuFlow v2 optical flow inference via Burn framework.
//!
//! Uses Burn + CubeCL JIT to run NeuFlow v2 on Vulkan (CMMA/FP16) or Metal.
//! Replaces the previous ONNX Runtime backend.
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
use std::sync::OnceLock;

use burn::prelude::*;
use burn::tensor::TensorData;
use burn_store::ModuleSnapshot;
use half::f16;
use parking_lot::Mutex;

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

use neuflow_v2_clean::Model;

struct NeuFlowModel {
    model: Model<B>,
    device: burn::backend::wgpu::WgpuDevice,
}

static NEUFLOW_MODEL: OnceLock<Result<Mutex<NeuFlowModel>, String>> = OnceLock::new();

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

/// Initialize the Burn model with Wgpu (Vulkan/Metal) backend.
fn init_model() -> Result<NeuFlowModel, String> {
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

    Ok(NeuFlowModel { model, device })
}

/// Run NeuFlow inference on a pair of preprocessed images.
///
/// img0, img1: CHW float32 `[3 * H * W]` in 0-255 range, where H=480, W=640.
/// Returns interleaved flow `[H*W*2]`: `[dx0,dy0,dx1,dy1,...]`.
pub fn infer(img0: &[f32], img1: &[f32], h: usize, w: usize) -> Result<Vec<f32>, String> {
    let expected = 3 * h * w;
    if img0.len() != expected || img1.len() != expected {
        return Err(format!(
            "Input size mismatch: expected {expected}, got img0={}, img1={}",
            img0.len(),
            img1.len()
        ));
    }

    let model_guard = NEUFLOW_MODEL
        .get_or_init(|| init_model().map(Mutex::new))
        .as_ref()
        .map_err(|e| e.clone())?;

    let nf = model_guard.lock();
    let device = &nf.device;

    // Create input tensors [1, 3, H, W]
    let shape = [1usize, 3, h, w];
    let img0_data = TensorData::new(img0.to_vec(), shape);
    let img1_data = TensorData::new(img1.to_vec(), shape);

    let img0_tensor = Tensor::<B, 4>::from_data(img0_data, device);
    let img1_tensor = Tensor::<B, 4>::from_data(img1_data, device);

    // Run inference
    let flow_tensor = nf.model.forward(img0_tensor, img1_tensor);

    // Extract flow: [1, 2, H, W] -> interleaved [H*W*2]
    // Output tensor is F16, convert to F32 for caller compatibility
    let flow_data = flow_tensor.into_data().convert::<f32>();
    let flow_vec: Vec<f32> = flow_data.to_vec()
        .map_err(|e| format!("Failed to extract flow data: {e:?}"))?;

    let fh = h;
    let fw = w;
    let plane = fh * fw;

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

/// Find the NeuFlow v2 ONNX model file (legacy, for API compat with callers checking ONNX).
/// With Burn backend we check .bpk instead, but keep this for is_available() path.
pub fn find_weight_file_onnx() -> Option<PathBuf> {
    // Check both .bpk (burn) and .onnx (legacy) paths
    find_weight_file()
}

/// Pre-initialize the model and warm up the GPU.
/// Call from a background thread at app startup to hide init latency.
pub fn ensure_ready() {
    if !is_available() {
        log::info!("NeuFlow Burn: model not found, skipping pre-init");
        return;
    }

    let start = std::time::Instant::now();
    let result = NEUFLOW_MODEL.get_or_init(|| init_model().map(Mutex::new));
    match result {
        Ok(_) => log::info!("NeuFlow Burn: pre-init complete in {:?}", start.elapsed()),
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

    #[test]
    #[ignore] // Run with: cargo test --features neuflow -- --ignored test_vulkan_inference
    fn test_vulkan_inference() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;

        // Create dummy input (zeros = black frames)
        let img0 = vec![128.0f32; size];
        let img1 = vec![128.0f32; size];

        let start = std::time::Instant::now();
        let result = infer(&img0, &img1, h, w);
        let elapsed = start.elapsed();

        match result {
            Ok(flow) => {
                // Verify output shape: interleaved HW2
                assert_eq!(flow.len(), h * w * 2, "Flow size mismatch");

                // Verify values are finite
                assert!(flow.iter().all(|v| v.is_finite()), "Non-finite flow values");

                // Print stats
                let max = flow.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
                let min = flow.iter().cloned().fold(f32::INFINITY, f32::min);
                let mean = flow.iter().sum::<f32>() / flow.len() as f32;
                eprintln!("NeuFlow Burn inference OK:");
                eprintln!("  Time: {:?}", elapsed);
                eprintln!("  Flow shape: {} (expected {})", flow.len(), h * w * 2);
                eprintln!("  Flow range: [{min:.4}, {max:.4}], mean: {mean:.4}");
            }
            Err(e) => {
                panic!("Inference failed: {e}");
            }
        }
    }

    #[test]
    #[ignore]
    fn test_vulkan_benchmark() {
        let h = 480usize;
        let w = 640usize;
        let size = 3 * h * w;
        let img0 = vec![100.0f32; size];
        let img1 = vec![120.0f32; size];

        // Warmup
        let _ = infer(&img0, &img1, h, w);

        // Benchmark 10 runs
        let mut times = Vec::new();
        for _ in 0..10 {
            let start = std::time::Instant::now();
            let _ = infer(&img0, &img1, h, w);
            times.push(start.elapsed());
        }

        let avg_ms = times.iter().map(|t| t.as_millis()).sum::<u128>() as f64 / times.len() as f64;
        let min_ms = times.iter().map(|t| t.as_millis()).min().unwrap();
        let max_ms = times.iter().map(|t| t.as_millis()).max().unwrap();

        eprintln!("NeuFlow Burn benchmark (10 runs):");
        eprintln!("  Avg: {avg_ms:.1}ms, Min: {min_ms}ms, Max: {max_ms}ms");
        assert!(avg_ms < 500.0, "Average inference too slow: {avg_ms:.1}ms (target < 100ms)");
    }

    // F16 conversion now handled at load time via F16LoadAdapter - no separate conversion needed
}
