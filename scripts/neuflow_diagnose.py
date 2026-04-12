#!/usr/bin/env python3
"""Diagnose NeuFlow optical flow on real video frames.

Extracts two adjacent frames from a video, runs NeuFlow ONNX inference,
and saves a visualization of the flow field + statistics.

Usage:
    python scripts/neuflow_diagnose.py --video "D:/path/to/video.MOV" --model resources/neuflow_v2.onnx
"""

import argparse
import os
import sys
import glob
import numpy as np

# Pre-load CUDA DLLs
def _preload():
    if sys.platform != "win32":
        return
    try:
        import ctypes, importlib
        for pkg in ["nvidia.cublas", "nvidia.cuda_runtime", "nvidia.cudnn"]:
            try:
                mod = importlib.import_module(pkg)
                bd = os.path.join(mod.__path__[0], "bin")
                if os.path.isdir(bd):
                    os.add_dll_directory(bd)
                    for dll in sorted(glob.glob(os.path.join(bd, "*.dll"))):
                        try: ctypes.WinDLL(dll)
                        except: pass
            except ImportError: pass
        try:
            mod = importlib.import_module("nvidia.cudnn")
            bd = os.path.join(mod.__path__[0], "bin")
            for dll in sorted(glob.glob(os.path.join(bd, "*.dll"))):
                try: ctypes.WinDLL(dll)
                except: pass
        except: pass
    except: pass

_preload()

import onnxruntime as ort


def extract_frames(video_path, frame_indices):
    """Extract specific frames as RGB numpy arrays."""
    import cv2
    cap = cv2.VideoCapture(video_path)
    frames = {}
    for idx in sorted(frame_indices):
        cap.set(cv2.CAP_PROP_POS_FRAMES, idx)
        ret, frame = cap.read()
        if ret:
            frames[idx] = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
    cap.release()
    return frames


def preprocess(img, target_h=480, target_w=640):
    """Resize with letterbox, return CHW float32 0-255."""
    import cv2
    h, w = img.shape[:2]
    scale = min(target_w / w, target_h / h)
    new_w, new_h = int(w * scale), int(h * scale)
    resized = cv2.resize(img, (new_w, new_h), interpolation=cv2.INTER_LINEAR)

    pad_top = (target_h - new_h) // 2
    pad_left = (target_w - new_w) // 2

    canvas = np.zeros((target_h, target_w, 3), dtype=np.float32)
    canvas[pad_top:pad_top+new_h, pad_left:pad_left+new_w] = resized.astype(np.float32)

    chw = canvas.transpose(2, 0, 1)[np.newaxis]  # [1, 3, H, W]
    return chw, scale, pad_top, pad_left, new_h, new_w


def visualize_flow(img0, flow, scale, pad_top, pad_left, new_h, new_w, output_path):
    """Draw flow vectors on the image and save."""
    import cv2
    h, w = img0.shape[:2]
    vis = img0.copy()

    # Flow is [1, 2, 480, 640] planar
    dx = flow[0, 0]  # [480, 640]
    dy = flow[0, 1]  # [480, 640]

    step = max(w // 30, 10)
    for y in range(0, h, step):
        for x in range(0, w, step):
            # Map to model space
            mx = int(x * scale) + pad_left
            my = int(y * scale) + pad_top
            if 0 <= mx < 640 and 0 <= my < 480:
                fx = dx[my, mx] / scale
                fy = dy[my, mx] / scale
                mag = np.sqrt(fx**2 + fy**2)
                if mag > 0.5:  # skip very small flow
                    end_x = int(x + fx)
                    end_y = int(y + fy)
                    color = (0, 255, 0) if mag < 20 else (0, 0, 255)
                    cv2.arrowedLine(vis, (x, y), (end_x, end_y), color, 1, tipLength=0.3)

    cv2.imwrite(output_path, cv2.cvtColor(vis, cv2.COLOR_RGB2BGR))
    print(f"Saved: {output_path}")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--video", required=True)
    parser.add_argument("--model", default="resources/neuflow_v2.onnx")
    parser.add_argument("--frame", type=int, default=60, help="First frame index")
    parser.add_argument("--gap", type=int, default=3, help="Frame gap")
    parser.add_argument("--output", default="neuflow_diag.png")
    args = parser.parse_args()

    print(f"Video: {args.video}")
    print(f"Model: {args.model}")
    print(f"Frames: {args.frame} and {args.frame + args.gap}")

    # Extract frames
    frames = extract_frames(args.video, [args.frame, args.frame + args.gap])
    if len(frames) < 2:
        print("ERROR: Could not extract frames")
        return

    img0 = frames[args.frame]
    img1 = frames[args.frame + args.gap]
    print(f"Frame shape: {img0.shape}")

    # Preprocess
    chw0, scale, pad_top, pad_left, new_h, new_w = preprocess(img0)
    chw1, _, _, _, _, _ = preprocess(img1)
    print(f"Model input: {chw0.shape}, scale={scale:.3f}, pad=({pad_top},{pad_left})")

    # Load model
    providers = ["CUDAExecutionProvider", "CPUExecutionProvider"]
    avail = ort.get_available_providers()
    providers = [p for p in providers if p in avail]
    sess = ort.InferenceSession(args.model, providers=providers)
    print(f"EP: {sess.get_providers()}")

    # Inference
    import time
    t0 = time.time()
    result = sess.run(None, {"img0": chw0, "img1": chw1})
    elapsed = (time.time() - t0) * 1000
    flow = result[0]
    print(f"Inference: {elapsed:.0f}ms")

    # Flow statistics
    dx = flow[0, 0]
    dy = flow[0, 1]
    valid_mask = (np.arange(480)[:, None] >= pad_top) & (np.arange(480)[:, None] < pad_top + new_h) & \
                 (np.arange(640)[None, :] >= pad_left) & (np.arange(640)[None, :] < pad_left + new_w)

    dx_valid = dx[valid_mask]
    dy_valid = dy[valid_mask]
    mag = np.sqrt(dx_valid**2 + dy_valid**2)

    print(f"\n--- Flow Statistics (valid region only) ---")
    print(f"dx: min={dx_valid.min():.2f}, max={dx_valid.max():.2f}, mean={dx_valid.mean():.2f}, std={dx_valid.std():.2f}")
    print(f"dy: min={dy_valid.min():.2f}, max={dy_valid.max():.2f}, mean={dy_valid.mean():.2f}, std={dy_valid.std():.2f}")
    print(f"mag: min={mag.min():.2f}, max={mag.max():.2f}, mean={mag.mean():.2f}, median={np.median(mag):.2f}")
    print(f"Points with mag > 1.0: {(mag > 1.0).sum()} / {len(mag)}")
    print(f"Points with mag > 5.0: {(mag > 5.0).sum()} / {len(mag)}")

    # Identical frame test (sanity check)
    result_same = sess.run(None, {"img0": chw0, "img1": chw0})
    flow_same = result_same[0]
    same_mag = np.sqrt(flow_same[0,0]**2 + flow_same[0,1]**2)
    print(f"\nSanity check (same frame): max_mag={same_mag.max():.4f} (should be <1.0)")

    # Visualize
    try:
        visualize_flow(img0, flow, scale, pad_top, pad_left, new_h, new_w, args.output)
    except ImportError:
        print("cv2 not available for visualization")

    # Verdict
    print(f"\n--- Verdict ---")
    if mag.mean() < 0.1:
        print("WARNING: Very small flow. Frames might be too similar or model failing.")
    elif mag.mean() > 50:
        print("WARNING: Very large flow. Model might be producing garbage.")
    elif same_mag.max() > 2.0:
        print("WARNING: Same-frame flow is too large. Model might not be working correctly.")
    else:
        print("Flow looks reasonable. Issue likely in Rust integration (coordinate transform, pose estimation).")


if __name__ == "__main__":
    main()
