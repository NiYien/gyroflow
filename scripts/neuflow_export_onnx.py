#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Export NeuFlow v2 PyTorch model to ONNX with dual-input interface.

Usage:
    git clone https://github.com/neufieldrobotics/NeuFlow_v2
    cd NeuFlow_v2
    python /path/to/neuflow_export_onnx.py --checkpoint neuflow_mixed.pth --output neuflow_v2.onnx

    # Export 432x768, iter5, mixed FP16 for ORT CUDA:
    python /path/to/neuflow_export_onnx.py --checkpoint neuflow_mixed.pth \
        --output neuflow_v2_mixed_432x768.onnx --height 432 --width 768 --iters 5 --fp16

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

    def __init__(self, model, iters_s16=1, iters_s8=8):
        super().__init__()
        self.model = model
        self.iters_s16 = iters_s16
        self.iters_s8 = iters_s8

    def forward(self, img0: torch.Tensor, img1: torch.Tensor) -> torch.Tensor:
        # NeuFlow.forward does img /= 255. internally
        results = self.model(img0, img1, iters_s16=self.iters_s16, iters_s8=self.iters_s8)
        if isinstance(results, (list, tuple)):
            return results[-1]
        return results


def convert_to_fp16(input_path, output_path):
    """Convert FP32 ONNX model to mixed FP16 for ORT CUDA.

    Blocks ops with type constraints (LayerNormalization) from FP16 conversion
    to avoid ORT's "Type parameter bound to different types" error.
    """
    import onnx
    from onnxconverter_common import float16

    print(f"Converting to mixed FP16: {input_path} -> {output_path}")
    model = onnx.load(input_path)

    # Block ops that have type parameter constraints incompatible with mixed types.
    # LayerNormalization requires all inputs to share the same type parameter T.
    model_fp16 = float16.convert_float_to_float16(
        model,
        keep_io_types=True,         # Keep inputs/outputs as FP32
        op_block_list=["LayerNormalization"],  # Block problematic ops
    )

    onnx.save(model_fp16, output_path)
    print(f"FP16 conversion done: {os.path.getsize(output_path) / 1024 / 1024:.1f} MB")


def main():
    parser = argparse.ArgumentParser(description="Export NeuFlow v2 to ONNX")
    parser.add_argument("--checkpoint", required=True, help="Path to .pth checkpoint")
    parser.add_argument("--output", default="neuflow_v2.onnx", help="Output ONNX path")
    parser.add_argument("--height", type=int, default=480)
    parser.add_argument("--width", type=int, default=640)
    parser.add_argument("--iters", type=int, default=None,
                        help="Override refine iterations (e.g. 5). Default: model default (s16=1, s8=8)")
    parser.add_argument("--iters-s16", type=int, default=1, help="s16 refine iterations (default: 1)")
    parser.add_argument("--iters-s8", type=int, default=8, help="s8 refine iterations (default: 8)")
    parser.add_argument("--fp16", action="store_true",
                        help="Convert to mixed FP16 for ORT CUDA (blocks LayerNorm)")
    parser.add_argument("--simplify", action="store_true")
    args = parser.parse_args()

    assert args.height % 16 == 0, f"Height must be multiple of 16"
    assert args.width % 16 == 0, f"Width must be multiple of 16"

    # --iters N is shorthand for --iters-s16 1 --iters-s8 N
    iters_s16 = args.iters_s16
    iters_s8 = args.iters if args.iters is not None else args.iters_s8

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
    print(f"Initializing model for {args.height}x{args.width} (iters_s16={iters_s16}, iters_s8={iters_s8})...")
    model.init_bhwd(1, args.height, args.width, device="cpu", amp=False)

    wrapper = NeuFlowWrapper(model, iters_s16=iters_s16, iters_s8=iters_s8)
    wrapper.eval()

    img0 = torch.randn(1, 3, args.height, args.width)
    img1 = torch.randn(1, 3, args.height, args.width)

    # NOTE: dynamic_axes disabled for NeuFlow because positional encodings
    # are baked to fixed spatial dims at init_bhwd() time.  Re-export at
    # different resolution if needed.
    dynamic_axes = None

    # Export FP32 first (FP16 conversion is a post-processing step)
    fp32_path = args.output if not args.fp16 else args.output.replace(".onnx", "_fp32_tmp.onnx")

    print(f"Exporting: {fp32_path} ({args.height}x{args.width})")
    # Use legacy TorchScript-based exporter (dynamo=False) because NeuFlow
    # uses runtime-created buffers (pos_s16, grid, delta, radius_emb) that
    # are set via init_bhwd() and stored as plain attributes, which the new
    # dynamo exporter cannot trace.
    torch.onnx.export(
        wrapper, (img0, img1), fp32_path,
        opset_version=17,
        input_names=["img0", "img1"],
        output_names=["flow"],
        dynamic_axes=dynamic_axes,
        do_constant_folding=True,
        dynamo=False,
    )
    print(f"Exported FP32: {fp32_path} ({os.path.getsize(fp32_path) / 1024 / 1024:.1f} MB)")

    if args.simplify:
        try:
            import onnx, onnxsim
            print("Simplifying...")
            m = onnx.load(fp32_path)
            m2, ok = onnxsim.simplify(m)
            if ok:
                onnx.save(m2, fp32_path)
                print("Simplified OK")
        except ImportError:
            print("onnxsim not installed, skipping")

    if args.fp16:
        convert_to_fp16(fp32_path, args.output)
        os.remove(fp32_path)
        print(f"Removed intermediate FP32: {fp32_path}")

    final_path = args.output
    print(f"Final: {final_path} ({os.path.getsize(final_path) / 1024 / 1024:.1f} MB)")


if __name__ == "__main__":
    main()
