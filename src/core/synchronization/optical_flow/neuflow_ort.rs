// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

//! ORT/CUDA backend — original niyien approach: NV12 direct → preprocess_frame_nv12 → ORT infer.

use super::neuflow::preprocess_frame_nv12;

/// Returns (flow_hw2, gray).
pub(super) fn neuflow_inference_ort(
    nv12_0: &[u8], w0: u32, h0: u32, stride0: usize,
    nv12_1: &[u8], w1: u32, h1: u32, stride1: usize,
) -> Result<(Vec<f32>, Vec<u8>), String> {
    let (img0, gray0, proc_h, proc_w) = preprocess_frame_nv12(nv12_0, w0, h0, stride0)?;
    let (img1, _, _, _) = preprocess_frame_nv12(nv12_1, w1, h1, stride1)?;

    let start = std::time::Instant::now();

    let flow = crate::neuflow::infer(&img0, &img1, proc_h, proc_w)?;

    let elapsed = start.elapsed();
    if elapsed.as_secs() > 60 {
        return Err(format!("NeuFlow ORT inference timeout: {elapsed:?}"));
    }

    log::debug!("NeuFlow ORT inference: {}x{} in {:?}", proc_w, proc_h, elapsed);
    Ok((flow, gray0))
}
