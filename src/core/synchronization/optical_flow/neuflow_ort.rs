// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

//! ORT/CUDA backend — accepts preprocessed CHW tensors, returns dense flow.

/// Run ORT inference on preprocessed CHW tensors.
/// Returns interleaved flow [dx0,dy0,dx1,dy1,...].
pub(super) fn neuflow_inference_ort(
    chw0: &[f32], chw1: &[f32], h: usize, w: usize,
) -> Result<Vec<f32>, String> {
    let start = std::time::Instant::now();

    let flow = crate::neuflow::infer(chw0, chw1, h, w)?;

    let elapsed = start.elapsed();
    if elapsed.as_secs() > 60 {
        return Err(format!("NeuFlow ORT inference timeout: {elapsed:?}"));
    }

    log::debug!("NeuFlow ORT inference: {}x{} in {:?}", w, h, elapsed);
    Ok(flow)
}
