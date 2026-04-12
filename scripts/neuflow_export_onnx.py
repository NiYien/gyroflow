#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Export NeuFlow v2 PyTorch model to ONNX with dual-input interface.

Usage:
    git clone https://github.com/neufieldrobotics/NeuFlow_v2
    cd NeuFlow_v2
    python /path/to/neuflow_export_onnx.py --checkpoint neuflow_mixed.pth --output neuflow_v2.onnx

The exported model accepts:
    img0: [1, 3, H, W] float32, 0-255
    img1: [1, 3, H, W] float32, 0-255
    (H, W must be multiples of 16)

Output:
    flow: [1, 2, H, W] float32 (pixel displacement)
"""

import argparse
import sys
import os
import torch
import torch.nn as nn


class NeuFlowWrapper(nn.Module):
    """Wraps NeuFlow v2 to accept dual inputs (0-255 range).

    NeuFlow.forward() already divides by 255 internally, so we do NOT
    re-normalize here.
    """

    def __init__(self, model):
        super().__init__()
        self.model = model

    def forward(self, img0: torch.Tensor, img1: torch.Tensor) -> torch.Tensor:
        # NeuFlow.forward does img /= 255. internally
        results = self.model(img0, img1)
        if isinstance(results, (list, tuple)):
            return results[-1]
        return results


def main():
    parser = argparse.ArgumentParser(description="Export NeuFlow v2 to ONNX")
    parser.add_argument("--checkpoint", required=True, help="Path to .pth checkpoint")
    parser.add_argument("--output", default="neuflow_v2.onnx", help="Output ONNX path")
    parser.add_argument("--height", type=int, default=480)
    parser.add_argument("--width", type=int, default=640)
    parser.add_argument("--simplify", action="store_true")
    args = parser.parse_args()

    assert args.height % 16 == 0, f"Height must be multiple of 16"
    assert args.width % 16 == 0, f"Width must be multiple of 16"

    # Import NeuFlow from the cloned repo
    try:
        from NeuFlow.neuflow import NeuFlow
    except ImportError:
        print("ERROR: Run from NeuFlow_v2 repo root directory.")
        print("  git clone https://github.com/neufieldrobotics/NeuFlow_v2")
        print("  cd NeuFlow_v2")
        print(f"  python {sys.argv[0]} --checkpoint neuflow_mixed.pth --output neuflow_v2.onnx")
        sys.exit(1)

    print(f"Loading: {args.checkpoint}")
    model = NeuFlow()
    ckpt = torch.load(args.checkpoint, map_location="cpu", weights_only=False)
    state = ckpt.get("model", ckpt.get("state_dict", ckpt))
    model.load_state_dict(state)
    model.eval()

    # NeuFlow requires init_bhwd() to create positional encodings and
    # coordinate grids used by backbone, matching, corr_block, and refine.
    print(f"Initializing model for {args.height}x{args.width}...")
    model.init_bhwd(1, args.height, args.width, device="cpu", amp=False)

    wrapper = NeuFlowWrapper(model)
    wrapper.eval()

    img0 = torch.randn(1, 3, args.height, args.width)
    img1 = torch.randn(1, 3, args.height, args.width)

    # NOTE: dynamic_axes disabled for NeuFlow because positional encodings
    # are baked to fixed spatial dims at init_bhwd() time.  Re-export at
    # different resolution if needed.
    dynamic_axes = None

    print(f"Exporting: {args.output} ({args.height}x{args.width})")
    # Use legacy TorchScript-based exporter (dynamo=False) because NeuFlow
    # uses runtime-created buffers (pos_s16, grid, delta, radius_emb) that
    # are set via init_bhwd() and stored as plain attributes, which the new
    # dynamo exporter cannot trace.
    torch.onnx.export(
        wrapper, (img0, img1), args.output,
        opset_version=17,
        input_names=["img0", "img1"],
        output_names=["flow"],
        dynamic_axes=dynamic_axes,
        do_constant_folding=True,
        dynamo=False,
    )
    print(f"Exported: {args.output}")

    if args.simplify:
        try:
            import onnx, onnxsim
            print("Simplifying...")
            m = onnx.load(args.output)
            m2, ok = onnxsim.simplify(m)
            if ok:
                onnx.save(m2, args.output)
                print("Simplified OK")
        except ImportError:
            print("onnxsim not installed, skipping")

    print(f"Size: {os.path.getsize(args.output) / 1024 / 1024:.1f} MB")


if __name__ == "__main__":
    main()
