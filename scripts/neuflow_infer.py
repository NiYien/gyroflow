#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""NeuFlow v2 ONNX inference server — persistent process called by gyroflow.

Runs as a long-lived subprocess. Loads the model ONCE, then loops reading
inference requests from stdin and writing results to stdout.

Protocol (line-based):
  Request:  INFER <img0_path> <img1_path> <flow_path> <height> <width>\n
  Response: OK <elapsed_ms>\n  or  ERROR <message>\n
  Shutdown: EXIT\n
"""

import os
import sys
import glob
import time
import numpy as np

# --- Pre-load CUDA/cuDNN DLLs before importing onnxruntime ---
def _preload_cuda_dlls():
    if sys.platform != "win32":
        return
    try:
        import ctypes
        import importlib
        for pkg in ["nvidia.cublas", "nvidia.cuda_runtime", "nvidia.cudnn"]:
            try:
                mod = importlib.import_module(pkg)
                bin_dir = os.path.join(mod.__path__[0], "bin")
                if os.path.isdir(bin_dir):
                    os.add_dll_directory(bin_dir)
                    for dll in sorted(glob.glob(os.path.join(bin_dir, "*.dll"))):
                        try:
                            ctypes.WinDLL(dll)
                        except OSError:
                            pass
            except ImportError:
                pass
        # Second pass for cuDNN (depends on cublas/cuda_runtime)
        try:
            mod = importlib.import_module("nvidia.cudnn")
            bin_dir = os.path.join(mod.__path__[0], "bin")
            for dll in sorted(glob.glob(os.path.join(bin_dir, "*.dll"))):
                try:
                    ctypes.WinDLL(dll)
                except OSError:
                    pass
        except ImportError:
            pass
    except Exception:
        pass

_preload_cuda_dlls()

import onnxruntime as ort


def main():
    if len(sys.argv) < 2:
        print("Usage: neuflow_infer.py <model_path>", file=sys.stderr)
        sys.exit(1)

    model_path = sys.argv[1]

    # Select best available provider
    available = ort.get_available_providers()
    providers = []
    if "CUDAExecutionProvider" in available:
        providers.append("CUDAExecutionProvider")
    providers.append("CPUExecutionProvider")

    # Load model
    t0 = time.time()
    sess = ort.InferenceSession(model_path, providers=providers)
    active = sess.get_providers()
    load_ms = (time.time() - t0) * 1000

    # Warmup
    warmup_img = np.zeros((1, 3, 480, 640), dtype=np.float32)
    sess.run(None, {"img0": warmup_img, "img1": warmup_img})

    # Signal ready
    ep_name = active[0] if active else "none"
    print(f"READY {ep_name} {load_ms:.0f}ms", flush=True)

    # Main loop: read requests from stdin
    for line in sys.stdin:
        line = line.strip()
        if not line or line == "EXIT":
            break

        try:
            parts = line.split()
            if parts[0] != "INFER" or len(parts) != 6:
                print(f"ERROR bad request: {line}", flush=True)
                continue

            _, img0_path, img1_path, flow_path, h_str, w_str = parts
            h, w = int(h_str), int(w_str)

            # Read inputs
            img0 = np.fromfile(img0_path, dtype=np.float32).reshape(1, 3, h, w)
            img1 = np.fromfile(img1_path, dtype=np.float32).reshape(1, 3, h, w)

            # Inference
            t0 = time.time()
            result = sess.run(None, {"img0": img0, "img1": img1})
            elapsed_ms = (time.time() - t0) * 1000

            flow = result[0]  # [1, 2, H, W]

            # Convert planar to interleaved
            dx = flow[0, 0, :, :]
            dy = flow[0, 1, :, :]
            interleaved = np.stack([dx, dy], axis=-1).reshape(-1)
            interleaved.astype(np.float32).tofile(flow_path)

            print(f"OK {elapsed_ms:.1f}ms", flush=True)

        except Exception as e:
            print(f"ERROR {e}", flush=True)


if __name__ == "__main__":
    main()
