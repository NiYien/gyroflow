#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Export NeuFlow v2 PyTorch model to ONNX with an inference-oriented graph.

This exporter keeps the current dual-input interface used by gyroflow:

    img0: [1, 3, H, W] float/half in 0-255 range
    img1: [1, 3, H, W] float/half in 0-255 range

Internally it aligns closer to ibaiGorordo's inference fork:

- BatchNorm is fused into ConvBlock before export
- iteration counts are explicit export parameters
- optional FP16 export uses `init_bhwd(..., amp=True)`
- ONNX simplification is enabled by default
- constant folding is configurable (disabled by default to avoid baking
  large runtime buffers into the graph unless requested)

Usage:
    git clone https://github.com/neufieldrobotics/NeuFlow_v2
    cd NeuFlow_v2
    python /path/to/neuflow_export_onnx.py --checkpoint neuflow_mixed.pth --output neuflow_v2.onnx
"""

import argparse
import os
import sys

import torch
import torch.nn as nn
import torch.nn.functional as F


def format_size_mb(path: str) -> str:
    return f"{os.path.getsize(path) / 1024 / 1024:.2f} MB"


def fuse_conv_and_bn(conv: torch.nn.Conv2d, bn: torch.nn.BatchNorm2d) -> torch.nn.Conv2d:
    """Fuse an inference BatchNorm2d into the preceding Conv2d."""
    fused = (
        torch.nn.Conv2d(
            conv.in_channels,
            conv.out_channels,
            kernel_size=conv.kernel_size,
            stride=conv.stride,
            padding=conv.padding,
            dilation=conv.dilation,
            groups=conv.groups,
            bias=True,
        )
        .requires_grad_(False)
        .to(conv.weight.device)
    )

    weight = conv.weight.detach().float().view(conv.out_channels, -1)
    conv_bias = (
        torch.zeros(conv.weight.shape[0], device=conv.weight.device, dtype=torch.float32)
        if conv.bias is None
        else conv.bias.detach().float()
    )
    gamma = bn.weight.detach().float()
    beta = bn.bias.detach().float()
    mean = bn.running_mean.detach().float()
    var = bn.running_var.detach().float()
    inv_std = gamma / torch.sqrt(var + bn.eps)
    weight_bn = torch.diag(inv_std)

    fused.weight.copy_(torch.mm(weight_bn, weight).view(fused.weight.shape))
    fused.bias.copy_(torch.mm(weight_bn, conv_bias.reshape(-1, 1)).reshape(-1) + (beta - gamma * mean / torch.sqrt(var + bn.eps)))
    return fused


def fuse_model_conv_and_bn(model) -> None:
    """Apply Conv+BN fusion to official NeuFlow ConvBlock modules."""
    from NeuFlow.backbone_v7 import ConvBlock

    fused = 0
    for module in model.modules():
        if type(module) is ConvBlock and hasattr(module, "norm1") and hasattr(module, "norm2"):
            module.conv1 = fuse_conv_and_bn(module.conv1, module.norm1)
            module.conv2 = fuse_conv_and_bn(module.conv2, module.norm2)
            delattr(module, "norm1")
            delattr(module, "norm2")
            module.forward = module.forward_fuse
            fused += 2

    print(f"  Fused {fused} Conv+BatchNorm pair(s)")


class NeuFlowExportWrapper(nn.Module):
    """Inference-only NeuFlow forward used for export.

    This mirrors ibaiGorordo's simplified inference fork while keeping the
    original PyTorch checkpoint and module definitions.
    """

    def __init__(self, model, iters_s16: int, iters_s8: int):
        super().__init__()
        self.model = model
        self.iters_s16 = iters_s16
        self.iters_s8 = iters_s8
        from NeuFlow import config
        self.config = config

    def forward(self, img0: torch.Tensor, img1: torch.Tensor) -> torch.Tensor:
        # Keep the current 0-255 external interface, but move normalization out of
        # the official model's internal forward so the exported graph is based on
        # the inference path rather than the training-oriented wrapper.
        img0 = img0 / 255.0
        img1 = img1 / 255.0

        flow_list = []

        features_s16, features_s8 = self.model.backbone(torch.cat([img0, img1], dim=0))
        features_s16 = self.model.cross_attn_s16(features_s16)

        features_s16, context_s16 = self.model.split_features(
            features_s16,
            self.config.context_dim_s16,
            self.config.feature_dim_s16,
        )
        features_s8, context_s8 = self.model.split_features(
            features_s8,
            self.config.context_dim_s8,
            self.config.feature_dim_s8,
        )

        feature0_s16, feature1_s16 = features_s16.chunk(chunks=2, dim=0)
        flow0 = self.model.matching_s16.global_correlation_softmax(feature0_s16, feature1_s16)

        corr_pyr_s16 = self.model.corr_block_s16.init_corr_pyr(feature0_s16, feature1_s16)
        iter_context_s16 = self.model.init_iter_context_s16
        for _ in range(self.iters_s16):
            corrs = self.model.corr_block_s16(corr_pyr_s16, flow0)
            iter_context_s16, delta_flow = self.model.refine_s16(corrs, context_s16, iter_context_s16, flow0)
            flow0 = flow0 + delta_flow

        flow0 = F.interpolate(flow0, scale_factor=2, mode="nearest") * 2
        features_s16 = F.interpolate(features_s16, scale_factor=2, mode="nearest")
        features_s8 = self.model.merge_s8(torch.cat([features_s8, features_s16], dim=1))

        feature0_s8, feature1_s8 = features_s8.chunk(chunks=2, dim=0)
        corr_pyr_s8 = self.model.corr_block_s8.init_corr_pyr(feature0_s8, feature1_s8)

        context_s16 = F.interpolate(context_s16, scale_factor=2, mode="nearest")
        context_s8 = self.model.context_merge_s8(torch.cat([context_s8, context_s16], dim=1))

        iter_context_s8 = self.model.init_iter_context_s8
        for _ in range(self.iters_s8):
            corrs = self.model.corr_block_s8(corr_pyr_s8, flow0)
            iter_context_s8, delta_flow = self.model.refine_s8(corrs, context_s8, iter_context_s8, flow0)
            flow0 = flow0 + delta_flow

        feature0_s1 = self.model.conv_s8(img0)
        up_flow0 = self.model.upsample_s8(feature0_s1, flow0) * 8
        flow_list.append(up_flow0)

        return flow_list[-1]


def simplify_onnx(path: str) -> None:
    try:
        import onnx
        import onnxsim
    except ImportError:
        print("Skipping ONNX simplification: install onnx and onnxsim to enable it.")
        return

    before = format_size_mb(path)
    model = onnx.load(path)
    simplified, ok = onnxsim.simplify(model)
    if not ok:
        raise RuntimeError("onnxsim returned ok=False")
    onnx.save(simplified, path)
    after = format_size_mb(path)
    print(f"Simplified ONNX: {before} -> {after}")


def parse_args():
    parser = argparse.ArgumentParser(description="Export NeuFlow v2 to ONNX")
    parser.add_argument("--checkpoint", required=True, help="Path to the NeuFlow .pth checkpoint")
    parser.add_argument("--output", default="neuflow_v2.onnx", help="Output ONNX path")
    parser.add_argument("--height", type=int, default=480, help="Input height H (default: 480)")
    parser.add_argument("--width", type=int, default=640, help="Input width W (default: 640)")
    parser.add_argument("--iters-s16", type=int, default=1, help="Number of s16 refinement iterations")
    parser.add_argument("--iters-s8", type=int, default=8, help="Number of s8 refinement iterations")
    parser.add_argument("--half", action="store_true", help="Export the model graph in FP16 where possible")
    parser.add_argument("--constant-folding", action="store_true", help="Enable ONNX constant folding during export")
    parser.add_argument("--no-simplify", action="store_true", help="Skip ONNX simplification")
    parser.add_argument("--no-fuse-bn", action="store_true", help="Skip Conv+BatchNorm fusion before export")
    parser.add_argument("--opset", type=int, default=17, help="ONNX opset version (default: 17)")
    return parser.parse_args()


def main():
    args = parse_args()

    if args.height % 16 != 0 or args.width % 16 != 0:
        raise ValueError("Height and width must be multiples of 16")

    try:
        from NeuFlow.neuflow import NeuFlow
    except ImportError:
        print("ERROR: Run this from the NeuFlow_v2 repo root.", file=sys.stderr)
        print("  git clone https://github.com/neufieldrobotics/NeuFlow_v2", file=sys.stderr)
        print("  cd NeuFlow_v2", file=sys.stderr)
        sys.exit(1)

    print(f"Loading checkpoint: {args.checkpoint}")
    checkpoint = torch.load(args.checkpoint, map_location="cpu", weights_only=False)
    state = checkpoint.get("model", checkpoint.get("state_dict", checkpoint))

    model = NeuFlow()
    model.load_state_dict(state, strict=True)
    model.eval()

    if not args.no_fuse_bn:
        print("Applying Conv+BatchNorm fusion...")
        fuse_model_conv_and_bn(model)
    else:
        print("Skipping Conv+BatchNorm fusion")

    if args.half:
        print("Switching export model to mixed FP16 (keeping LayerNorm in FP32)")
        # Convert all parameters to FP16 first
        model.half()
        # Restore LayerNorm parameters to FP32 to preserve precision
        restored = []
        for name, module in model.named_modules():
            if isinstance(module, torch.nn.LayerNorm):
                for pname, param in module.named_parameters():
                    param.data = param.data.float()
                    restored.append(f"{name}.{pname}")
                for bname, buf in module.named_buffers():
                    buf.data = buf.data.float()
                    restored.append(f"{name}.{bname}")
        if restored:
            print(f"  Restored {len(restored)} LayerNorm params to FP32: {restored}")

    print(
        f"Initializing export buffers with H={args.height}, W={args.width}, "
        f"iters_s16={args.iters_s16}, iters_s8={args.iters_s8}, half={args.half}"
    )
    model.init_bhwd(1, args.height, args.width, device="cpu", amp=args.half)

    wrapper = NeuFlowExportWrapper(
        model=model,
        iters_s16=args.iters_s16,
        iters_s8=args.iters_s8,
    ).eval()

    dtype = torch.float16 if args.half else torch.float32
    img0 = torch.randn(1, 3, args.height, args.width, dtype=dtype)
    img1 = torch.randn(1, 3, args.height, args.width, dtype=dtype)

    print(
        f"Exporting ONNX to {args.output} "
        f"(H={args.height}, W={args.width}, opset={args.opset}, constant_folding={args.constant_folding})"
    )
    with torch.no_grad():
        torch.onnx.export(
            wrapper,
            (img0, img1),
            args.output,
            verbose=False,
            opset_version=args.opset,
            input_names=["img0", "img1"],
            output_names=["flow"],
            do_constant_folding=args.constant_folding,
            dynamic_axes=None,
            dynamo=False,
        )

    print(f"Export complete: {args.output} ({format_size_mb(args.output)})")

    if not args.no_simplify:
        simplify_onnx(args.output)
    else:
        print("Skipping ONNX simplification")

    print(
        "Export summary:"
        f" H={args.height}, W={args.width}, half={args.half},"
        f" iters_s16={args.iters_s16}, iters_s8={args.iters_s8},"
        f" constant_folding={args.constant_folding}"
    )


if __name__ == "__main__":
    main()
