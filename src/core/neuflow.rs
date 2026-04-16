// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

//! NeuFlow v2 optical flow inference via ONNX Runtime (ort crate).
//!
//! Uses a pool of ORT sessions for parallel inference across sync threads.
//! Each session holds its own CUDA context, enabling true GPU parallelism.
//!
//! Model I/O:
//! - Input: img0 [1, 3, 480, 640] float32, img1 [1, 3, 480, 640] float32 (0-255 range)
//! - Output: flow [1, 2, 480, 640] float32 (dense optical flow)

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use ort::session::Session;
use ort::value::TensorRef;
use parking_lot::Mutex;

/// Number of parallel ORT sessions.
/// 8 sessions balances parallelism vs load time (~250MB VRAM for mixed FP16 model).
const SESSION_POOL_SIZE: usize = 8;

/// Pool of ONNX Runtime sessions for parallel inference.
struct SessionPool {
    sessions: Vec<Mutex<Session>>,
    next: AtomicUsize,
}

impl SessionPool {
    fn get(&self) -> &Mutex<Session> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed) % self.sessions.len();
        &self.sessions[idx]
    }
}

static NEUFLOW_POOL: OnceLock<Result<SessionPool, String>> = OnceLock::new();

/// Ensure the ONNX Runtime dynamic library is loaded.
fn ensure_ort_loaded() -> Result<(), String> {
    if std::env::var("ORT_DYLIB_PATH").is_ok() {
        return Ok(());
    }

    let dll_name = if cfg!(windows) { "onnxruntime.dll" }
        else if cfg!(target_os = "macos") { "libonnxruntime.dylib" }
        else { "libonnxruntime.so" };

    let mut candidates: Vec<PathBuf> = vec![
        std::env::current_exe().ok()
            .and_then(|p| p.parent().map(|d| d.join(dll_name)))
            .unwrap_or_default(),
        PathBuf::from(format!("ext/{dll_name}")),
        PathBuf::from(format!("../ext/{dll_name}")),
    ];

    if let Ok(output) = std::process::Command::new("python")
        .args(["-c", "import onnxruntime; import os; print(os.path.dirname(onnxruntime.__file__))"])
        .output()
    {
        if output.status.success() {
            let ort_dir = String::from_utf8_lossy(&output.stdout).trim().to_string();
            candidates.push(PathBuf::from(&ort_dir).join("capi").join(dll_name));
        }
    }

    for candidate in &candidates {
        if candidate.exists() {
            log::info!("NeuFlow: found ONNX Runtime at {}", candidate.display());

            if let Some(dll_dir) = candidate.parent() {
                let dll_dir_str = dll_dir.to_string_lossy().to_string();
                let current_path = std::env::var("PATH").unwrap_or_default();
                if !current_path.contains(&dll_dir_str) {
                    unsafe { std::env::set_var("PATH", format!("{dll_dir_str};{current_path}")); }
                    log::info!("NeuFlow: added {} to PATH for CUDA provider DLLs", dll_dir_str);
                }
            }

            let builder = ort::init_from(candidate)
                .map_err(|e| format!("Failed to init ort from {}: {e}", candidate.display()))?;
            let _ = builder.commit();
            return Ok(());
        }
    }

    log::info!("NeuFlow: no explicit onnxruntime path found, relying on system PATH");
    Ok(())
}

/// Create a single ORT session with CUDA EP.
fn create_session(path: &std::path::Path) -> Result<Session, String> {
    Session::builder()
        .map_err(|e| format!("ORT session builder: {e}"))?
        .with_execution_providers([
            ort::ep::CUDA::default().build(),
        ])
        .map_err(|e| format!("ORT execution providers: {e}"))?
        .with_intra_threads(2)
        .map_err(|e| format!("ORT intra threads: {e}"))?
        .commit_from_file(path)
        .map_err(|e| format!("ORT load model: {e}"))
}

/// Initialize the session pool with parallel session creation + concurrent warmup.
fn init_pool() -> Result<SessionPool, String> {
    ensure_ort_loaded()?;

    let path = find_weight_file()
        .ok_or_else(|| "NeuFlow ONNX model file not found".to_string())?;

    log::info!("NeuFlow: loading {} ORT sessions in parallel from {} (CUDA requested)", SESSION_POOL_SIZE, path.display());

    // Create all sessions in parallel. Session 0 also does CUDA warmup immediately.
    let handles: Vec<_> = (0..SESSION_POOL_SIZE)
        .map(|i| {
            let p = path.clone();
            std::thread::spawn(move || {
                let mut session = create_session(&p)?;
                if i == 0 {
                    // Warmup session 0: compiles CUDA kernels (shared across all sessions)
                    let h = 432usize;
                    let w = 768usize;
                    let dummy = vec![0.0f32; 3 * h * w];
                    let shape = [1usize, 3, h, w];
                    if let Ok(img0) = TensorRef::from_array_view((shape, dummy.as_slice())) {
                        if let Ok(img1) = TensorRef::from_array_view((shape, dummy.as_slice())) {
                            let _ = session.run(ort::inputs!["img0" => img0, "img1" => img1]);
                        }
                    }
                    log::info!("NeuFlow: session 0 created + CUDA warmup done");
                }
                Ok(session)
            })
        })
        .collect();

    let mut sessions = Vec::with_capacity(SESSION_POOL_SIZE);
    for h in handles {
        sessions.push(Mutex::new(
            h.join().unwrap_or_else(|_| Err("Thread panicked".to_string()))?
        ));
    }

    log::info!("NeuFlow: all {} sessions ready", SESSION_POOL_SIZE);
    Ok(SessionPool {
        sessions,
        next: AtomicUsize::new(0),
    })
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
            img0.len(), img1.len()
        ));
    }

    let pool = NEUFLOW_POOL
        .get_or_init(|| init_pool())
        .as_ref()
        .map_err(|e| e.clone())?;

    let mutex = pool.get();
    let mut session = mutex.lock();

    // Create input tensor views (zero-copy over borrowed data)
    let shape = [1usize, 3, h, w];
    let img0_ref = TensorRef::from_array_view((shape, img0))
        .map_err(|e| format!("img0 tensor: {e}"))?;
    let img1_ref = TensorRef::from_array_view((shape, img1))
        .map_err(|e| format!("img1 tensor: {e}"))?;

    let outputs = session.run(ort::inputs![
        "img0" => img0_ref,
        "img1" => img1_ref,
    ]).map_err(|e| format!("ONNX inference failed: {e}"))?;

    // Extract flow output: [1, 2, H, W]
    let flow_value = outputs.get("flow")
        .ok_or_else(|| "Missing 'flow' output".to_string())?;
    let (flow_shape, flow_data) = flow_value.try_extract_tensor::<f32>()
        .map_err(|e| format!("Extract flow: {e}"))?;

    if flow_shape.len() != 4 || flow_shape[0] != 1 || flow_shape[1] != 2 {
        return Err(format!("Unexpected flow shape: {:?}", &*flow_shape));
    }

    let fh = flow_shape[2] as usize;
    let fw = flow_shape[3] as usize;
    let plane = fh * fw;

    // CHW → interleaved HW2 (dx0,dy0,dx1,dy1,...)
    let mut flow = vec![0.0f32; plane * 2];
    let (dx_plane, dy_plane) = (&flow_data[..plane], &flow_data[plane..2 * plane]);
    for i in 0..plane {
        flow[i * 2] = dx_plane[i];
        flow[i * 2 + 1] = dy_plane[i];
    }

    Ok(flow)
}

/// Find the NeuFlow v2 ONNX model file. Prefers FP32 432×768 (iter5), then legacy fallbacks.
pub fn find_weight_file() -> Option<PathBuf> {
    ["resources/neuflow_v2_fp32_432x768.onnx", "../resources/neuflow_v2_fp32_432x768.onnx",
     "resources/neuflow_v2_mixed.onnx", "../resources/neuflow_v2_mixed.onnx",
     "resources/neuflow_v2.onnx", "../resources/neuflow_v2.onnx"]
        .iter().map(PathBuf::from).find(|p| p.exists())
}

/// Check if NeuFlow v2 model is available.
pub fn is_available() -> bool {
    find_weight_file().is_some()
}

/// Pre-initialize the session pool and warm up CUDA.
/// Call from a background thread at app startup to hide init latency.
/// Session creation + warmup happen in parallel inside init_pool().
pub fn ensure_ready() {
    if !is_available() {
        log::info!("NeuFlow: model not found, skipping pre-init");
        return;
    }

    let start = std::time::Instant::now();
    let result = NEUFLOW_POOL.get_or_init(|| init_pool());
    match result {
        Ok(_) => log::info!("NeuFlow: pre-init complete in {:?}", start.elapsed()),
        Err(e) => log::error!("NeuFlow: pre-init failed: {e}"),
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
}
