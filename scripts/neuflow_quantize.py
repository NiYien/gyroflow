#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""NeuFlow v2 INT8 dynamic quantization script.

Quantizes the FP32 ONNX model to INT8 using onnxruntime dynamic quantization,
then validates quality and speed against the original.

Usage:
    python scripts/neuflow_quantize.py [--video PATH] [--skip-validation]
"""

import os
import sys
import glob
import time
import argparse
import shutil
import numpy as np

# --- Pre-load CUDA/cuDNN DLLs before importing onnxruntime ---
if sys.platform == "win32":
    import ctypes
    import importlib
    for pkg in ["nvidia.cublas", "nvidia.cuda_runtime", "nvidia.cudnn"]:
        try:
            mod = importlib.import_module(pkg)
            bd = os.path.join(mod.__path__[0], "bin")
            if os.path.isdir(bd):
                os.add_dll_directory(bd)
                for dll in sorted(glob.glob(os.path.join(bd, "*.dll"))):
                    try:
                        ctypes.WinDLL(dll)
                    except OSError:
                        pass
        except ImportError:
            pass
    try:
        mod = importlib.import_module("nvidia.cudnn")
        for dll in sorted(glob.glob(os.path.join(mod.__path__[0], "bin", "*.dll"))):
            try:
                ctypes.WinDLL(dll)
            except OSError:
                pass
    except ImportError:
        pass

import onnxruntime as ort
from onnxruntime.quantization import quantize_dynamic, QuantType


SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))
PROJECT_DIR = os.path.dirname(SCRIPT_DIR)
FP32_MODEL = os.path.join(PROJECT_DIR, "resources", "neuflow_v2.onnx")
INT8_MODEL = os.path.join(PROJECT_DIR, "resources", "neuflow_v2_int8.onnx")

MODEL_H, MODEL_W = 480, 640
N_WARMUP = 3
N_BENCH = 10


def create_session(model_path: str) -> ort.InferenceSession:
    """Create an ONNX Runtime session with best available provider."""
    available = ort.get_available_providers()
    providers = []
    if "CUDAExecutionProvider" in available:
        providers.append("CUDAExecutionProvider")
    providers.append("CPUExecutionProvider")
    sess = ort.InferenceSession(model_path, providers=providers)
    return sess


def preprocess_frame(frame: np.ndarray) -> np.ndarray:
    """Letterbox-resize a BGR frame to (1, 3, MODEL_H, MODEL_W) float32 [0-255]."""
    import cv2
    h, w = frame.shape[:2]
    scale = min(MODEL_W / w, MODEL_H / h)
    new_w, new_h = int(w * scale), int(h * scale)
    resized = cv2.resize(frame, (new_w, new_h), interpolation=cv2.INTER_LINEAR)

    # Create padded canvas (black)
    canvas = np.zeros((MODEL_H, MODEL_W, 3), dtype=np.uint8)
    pad_y = (MODEL_H - new_h) // 2
    pad_x = (MODEL_W - new_w) // 2
    canvas[pad_y:pad_y + new_h, pad_x:pad_x + new_w] = resized

    # HWC -> CHW, BGR -> RGB, float32
    img = canvas[:, :, ::-1].transpose(2, 0, 1).astype(np.float32)
    return img[np.newaxis]  # (1, 3, H, W)


def run_inference(sess: ort.InferenceSession, img0: np.ndarray, img1: np.ndarray) -> np.ndarray:
    """Run optical flow inference, return flow [1, 2, H, W]."""
    result = sess.run(None, {"img0": img0, "img1": img1})
    return result[0]


def flow_magnitude(flow: np.ndarray) -> np.ndarray:
    """Compute per-pixel flow magnitude from [1, 2, H, W]."""
    return np.sqrt(flow[0, 0] ** 2 + flow[0, 1] ** 2)


def benchmark(sess: ort.InferenceSession, img0: np.ndarray, img1: np.ndarray,
              n_warmup: int = N_WARMUP, n_runs: int = N_BENCH) -> float:
    """Benchmark inference, return average ms."""
    for _ in range(n_warmup):
        run_inference(sess, img0, img1)

    times = []
    for _ in range(n_runs):
        t0 = time.perf_counter()
        run_inference(sess, img0, img1)
        elapsed = (time.perf_counter() - t0) * 1000
        times.append(elapsed)
    return np.mean(times)


def quantize_model():
    """Step 1: Quantize FP32 -> INT8."""
    print(f"[1/4] Quantizing model...")
    print(f"  Input:  {FP32_MODEL}")
    fp32_size = os.path.getsize(FP32_MODEL) / (1024 * 1024)
    print(f"  FP32 size: {fp32_size:.1f} MB")

    quantize_dynamic(
        model_input=FP32_MODEL,
        model_output=INT8_MODEL,
        weight_type=QuantType.QInt8,
    )

    int8_size = os.path.getsize(INT8_MODEL) / (1024 * 1024)
    ratio = fp32_size / int8_size
    print(f"  Output: {INT8_MODEL}")
    print(f"  INT8 size: {int8_size:.1f} MB  ({ratio:.1f}x smaller)")
    return fp32_size, int8_size


def validate_same_frame(sess_int8: ort.InferenceSession):
    """Step 2a: Same-frame test — flow should be near zero."""
    print(f"\n[2/4] Same-frame noise test (INT8)...")
    dummy = np.random.rand(1, 3, MODEL_H, MODEL_W).astype(np.float32) * 255.0
    flow = run_inference(sess_int8, dummy, dummy)
    mag = flow_magnitude(flow)
    max_mag = float(np.max(mag))
    mean_mag = float(np.mean(mag))
    print(f"  Max flow magnitude:  {max_mag:.4f} px")
    print(f"  Mean flow magnitude: {mean_mag:.4f} px")
    passed = max_mag < 2.0
    print(f"  Threshold (max < 2.0): {'PASS' if passed else 'FAIL'}")
    return passed, max_mag, mean_mag


def validate_with_video(sess_fp32: ort.InferenceSession,
                        sess_int8: ort.InferenceSession,
                        video_path: str):
    """Step 2b: Compare FP32 vs INT8 on real video frames."""
    import cv2
    print(f"\n[3/4] Real video validation: {video_path}")
    cap = cv2.VideoCapture(video_path)
    if not cap.isOpened():
        print(f"  ERROR: Cannot open video")
        return False, None, None, None, None

    cap.set(cv2.CAP_PROP_POS_FRAMES, 120)
    ret0, f0 = cap.read()
    cap.set(cv2.CAP_PROP_POS_FRAMES, 123)
    ret1, f1 = cap.read()
    cap.release()

    if not ret0 or not ret1:
        print(f"  ERROR: Cannot read frames 120/123")
        return False, None, None, None, None

    print(f"  Frame size: {f0.shape[1]}x{f0.shape[0]}")

    img0 = preprocess_frame(f0)
    img1 = preprocess_frame(f1)

    # FP32 inference
    flow_fp32 = run_inference(sess_fp32, img0, img1)

    # INT8 inference
    flow_int8 = run_inference(sess_int8, img0, img1)

    # MAE comparison
    mae = float(np.mean(np.abs(flow_fp32 - flow_int8)))
    max_diff = float(np.max(np.abs(flow_fp32 - flow_int8)))
    print(f"  MAE (FP32 vs INT8):  {mae:.4f} px")
    print(f"  Max diff:            {max_diff:.4f} px")
    passed = mae < 0.5
    print(f"  Threshold (MAE < 0.5): {'PASS' if passed else 'FAIL'}")

    # Speed benchmark
    print(f"\n[4/4] Speed benchmark ({N_BENCH} runs each)...")
    fp32_ms = benchmark(sess_fp32, img0, img1)
    int8_ms = benchmark(sess_int8, img0, img1)
    speedup = fp32_ms / int8_ms if int8_ms > 0 else 0
    print(f"  FP32 avg: {fp32_ms:.1f} ms")
    print(f"  INT8 avg: {int8_ms:.1f} ms")
    print(f"  Speedup:  {speedup:.2f}x")

    return passed, mae, max_diff, fp32_ms, int8_ms


def main():
    parser = argparse.ArgumentParser(description="Quantize NeuFlow v2 to INT8")
    parser.add_argument("--video", type=str,
                        default="D:/视频素材/松下S5_0405/P1004671.MOV",
                        help="Video file for validation")
    parser.add_argument("--skip-validation", action="store_true",
                        help="Skip validation, only quantize")
    parser.add_argument("--no-replace", action="store_true",
                        help="Don't replace the original model")
    args = parser.parse_args()

    if not os.path.exists(FP32_MODEL):
        print(f"ERROR: FP32 model not found: {FP32_MODEL}")
        sys.exit(1)

    # Step 1: Quantize
    fp32_size, int8_size = quantize_model()

    if args.skip_validation:
        print("\n=== Quantization complete (validation skipped) ===")
        return

    # Load sessions
    print(f"\nLoading FP32 session...")
    sess_fp32 = create_session(FP32_MODEL)
    print(f"  Provider: {sess_fp32.get_providers()[0]}")

    print(f"Loading INT8 session...")
    sess_int8 = create_session(INT8_MODEL)
    print(f"  Provider: {sess_int8.get_providers()[0]}")

    # Step 2a: Same-frame test
    sf_pass, sf_max, sf_mean = validate_same_frame(sess_int8)

    # Step 2b + 3: Video validation + speed
    video_exists = os.path.exists(args.video)
    if video_exists:
        vf_pass, mae, max_diff, fp32_ms, int8_ms = validate_with_video(
            sess_fp32, sess_int8, args.video)
    else:
        print(f"\n[3/4] Skipping video validation (file not found: {args.video})")
        print(f"\n[4/4] Speed benchmark (synthetic data, {N_BENCH} runs each)...")
        img0 = np.random.rand(1, 3, MODEL_H, MODEL_W).astype(np.float32) * 255.0
        img1 = np.random.rand(1, 3, MODEL_H, MODEL_W).astype(np.float32) * 255.0
        fp32_ms = benchmark(sess_fp32, img0, img1)
        int8_ms = benchmark(sess_int8, img0, img1)
        speedup = fp32_ms / int8_ms if int8_ms > 0 else 0
        print(f"  FP32 avg: {fp32_ms:.1f} ms")
        print(f"  INT8 avg: {int8_ms:.1f} ms")
        print(f"  Speedup:  {speedup:.2f}x")
        vf_pass = True
        mae = None
        max_diff = None

    # Summary
    print(f"\n{'=' * 50}")
    print(f"  QUANTIZATION SUMMARY")
    print(f"{'=' * 50}")
    print(f"  FP32 model size:     {fp32_size:.1f} MB")
    print(f"  INT8 model size:     {int8_size:.1f} MB")
    print(f"  Compression ratio:   {fp32_size / int8_size:.1f}x")
    print(f"  Same-frame max flow: {sf_max:.4f} px  ({'PASS' if sf_pass else 'FAIL'})")
    if mae is not None:
        print(f"  FP32 vs INT8 MAE:    {mae:.4f} px  ({'PASS' if vf_pass else 'FAIL'})")
        print(f"  FP32 vs INT8 max:    {max_diff:.4f} px")
    print(f"  FP32 speed:          {fp32_ms:.1f} ms")
    print(f"  INT8 speed:          {int8_ms:.1f} ms")
    print(f"  Speedup:             {fp32_ms / int8_ms:.2f}x")
    print(f"{'=' * 50}")

    all_pass = sf_pass and vf_pass
    if all_pass:
        print(f"  ALL TESTS PASSED")
        if not args.no_replace:
            print(f"\n  Replacing FP32 model with INT8 model...")
            shutil.copy2(INT8_MODEL, FP32_MODEL)
            os.remove(INT8_MODEL)
            final_size = os.path.getsize(FP32_MODEL) / (1024 * 1024)
            print(f"  Done. {FP32_MODEL} is now INT8 ({final_size:.1f} MB)")
        else:
            print(f"  INT8 model saved at: {INT8_MODEL}")
    else:
        print(f"  SOME TESTS FAILED — INT8 model NOT replaced")
        print(f"  INT8 model kept at: {INT8_MODEL}")
        sys.exit(1)


if __name__ == "__main__":
    main()
