#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Convert NeuFlow v2 PyTorch checkpoint to Safetensors (with BN fusion + FP16).

Usage:
    python neuflow_convert.py --input neuflow_v2.pth
    python neuflow_convert.py --input neuflow_v2.pth --output weights.safetensors
    python neuflow_convert.py --input neuflow_v2.pth --no-fp16

Requirements:
    pip install torch safetensors
"""

import argparse
import os
import sys
from collections import OrderedDict

import torch
from safetensors.torch import save_file


def fuse_bn_into_conv(state_dict: dict) -> dict:
    """Fuse BatchNorm parameters into preceding Conv layers.

    Looks for matching pairs of conv weight/bias and BN gamma/beta/mean/var
    keys and produces a fused weight and bias, eliminating the BN parameters.

    The fusion formula for inference-mode BN following a Conv is:
        fused_weight = weight * (gamma / sqrt(var + eps))
        fused_bias   = gamma * (bias - mean) / sqrt(var + eps) + beta

    Returns a new state dict with BN keys removed and Conv keys updated.
    """
    keys = list(state_dict.keys())
    fused = OrderedDict()
    consumed_prefixes = set()

    # Collect all BN prefixes (e.g. "backbone.layer1.1." for a BN after conv)
    bn_prefixes = set()
    for k in keys:
        if k.endswith(".running_mean"):
            prefix = k[: -len("running_mean")]
            bn_prefixes.add(prefix)

    # For each BN, try to find the preceding conv layer
    # Common patterns: conv is at same prefix with .0. and BN at .1.,
    # or conv_prefix.weight / bn_prefix.weight in sequential blocks
    for bn_prefix in sorted(bn_prefixes):
        # Try to derive the conv prefix from the BN prefix
        # Pattern 1: sequential — "module.X.1." (BN) -> "module.X.0." (Conv)
        parts = bn_prefix.rstrip(".").rsplit(".", 1)
        conv_prefix = None
        if len(parts) == 2:
            parent, idx_str = parts
            try:
                idx = int(idx_str)
                candidate = f"{parent}.{idx - 1}."
                if f"{candidate}weight" in state_dict:
                    conv_prefix = candidate
            except ValueError:
                pass

        if conv_prefix is None:
            # Cannot find matching conv — keep BN keys as-is
            continue

        conv_w_key = f"{conv_prefix}weight"
        conv_b_key = f"{conv_prefix}bias"
        bn_gamma_key = f"{bn_prefix}weight"
        bn_beta_key = f"{bn_prefix}bias"
        bn_mean_key = f"{bn_prefix}running_mean"
        bn_var_key = f"{bn_prefix}running_var"

        # Verify all BN keys exist
        required_bn = [bn_gamma_key, bn_beta_key, bn_mean_key, bn_var_key]
        if not all(k in state_dict for k in required_bn):
            continue

        conv_w = state_dict[conv_w_key].float()
        conv_b = state_dict.get(conv_b_key, torch.zeros(conv_w.shape[0])).float()
        gamma = state_dict[bn_gamma_key].float()
        beta = state_dict[bn_beta_key].float()
        mean = state_dict[bn_mean_key].float()
        var = state_dict[bn_var_key].float()
        eps = 1e-5

        # Fuse
        inv_std = gamma / torch.sqrt(var + eps)
        # For conv weight: multiply each output channel by its scale factor
        # conv_w shape: [out_channels, in_channels, kH, kW]
        fused_w = conv_w * inv_std.view(-1, *([1] * (conv_w.dim() - 1)))
        fused_b = (conv_b - mean) * inv_std + beta

        fused[conv_w_key] = fused_w
        fused[conv_b_key] = fused_b
        consumed_prefixes.add(bn_prefix)
        consumed_prefixes.add(conv_prefix)

    # Build final state dict: fused conv params + all non-consumed params
    result = OrderedDict()
    for k, v in state_dict.items():
        # Skip BN keys that were fused
        skip = False
        for prefix in consumed_prefixes:
            if k.startswith(prefix) and prefix in bn_prefixes:
                skip = True
                break
        if skip:
            continue
        # Use fused version if available, otherwise original
        result[k] = fused.get(k, v)

    fused_count = len(consumed_prefixes & bn_prefixes)
    if fused_count > 0:
        print(f"  Fused {fused_count} BatchNorm layer(s) into Conv layers")
    else:
        print("  No BatchNorm layers found to fuse (model may already be fused)")

    return result


def remap_keys(state_dict: dict) -> dict:
    """Remap PyTorch state dict keys to Burn-compatible record paths.

    Transformations applied:
      - Strip "module." prefix (from DataParallel wrapping)
      - Strip trailing numeric suffixes on num_batches_tracked
      - Normalize separator style

    Returns a new OrderedDict with remapped keys.
    """
    new_dict = OrderedDict()
    skipped = []

    for key, value in state_dict.items():
        new_key = key

        # Remove DataParallel "module." prefix
        if new_key.startswith("module."):
            new_key = new_key[len("module."):]

        # Skip num_batches_tracked (BN bookkeeping, not needed for inference)
        if "num_batches_tracked" in new_key:
            skipped.append(key)
            continue

        if new_key in new_dict:
            print(f"  WARNING: duplicate key after remapping: {new_key} "
                  f"(from {key}), skipping")
            continue

        new_dict[new_key] = value

    if skipped:
        print(f"  Skipped {len(skipped)} num_batches_tracked key(s)")

    return new_dict


def extract_state_dict(checkpoint: dict) -> dict:
    """Extract the model state dict from various checkpoint formats.

    Handles:
      - Raw state dict (keys are parameter names with tensor values)
      - Dict with "model" key
      - Dict with "state_dict" key
      - Dict with "model_state_dict" key
    """
    if isinstance(checkpoint, OrderedDict):
        # Likely already a raw state dict
        return checkpoint

    for candidate_key in ("model", "state_dict", "model_state_dict"):
        if candidate_key in checkpoint:
            print(f"  Extracted state dict from checkpoint['{candidate_key}']")
            return checkpoint[candidate_key]

    # Check if this looks like a raw state dict (all values are tensors)
    if all(isinstance(v, torch.Tensor) for v in checkpoint.values()):
        return checkpoint

    # Last resort: look for the largest dict-valued entry
    best_key = None
    best_size = 0
    for k, v in checkpoint.items():
        if isinstance(v, (dict, OrderedDict)) and len(v) > best_size:
            best_key = k
            best_size = len(v)

    if best_key is not None:
        print(f"  Extracted state dict from checkpoint['{best_key}'] "
              f"({best_size} entries)")
        return checkpoint[best_key]

    raise ValueError(
        "Cannot find model state dict in checkpoint. "
        f"Top-level keys: {list(checkpoint.keys())}"
    )


def main():
    parser = argparse.ArgumentParser(
        description="Convert NeuFlow v2 PyTorch checkpoint to Safetensors format.",
        epilog="Example: python neuflow_convert.py --input neuflow_v2.pth",
    )
    parser.add_argument(
        "--input", "-i",
        required=True,
        help="Path to the PyTorch checkpoint file (.pth / .pt / .ckpt)",
    )
    parser.add_argument(
        "--output", "-o",
        default="resources/neuflow_v2_fp16.safetensors",
        help="Output Safetensors file path "
             "(default: resources/neuflow_v2_fp16.safetensors)",
    )
    parser.add_argument(
        "--no-fp16",
        action="store_true",
        help="Keep weights in FP32 instead of converting to FP16",
    )
    parser.add_argument(
        "--no-fuse-bn",
        action="store_true",
        help="Skip BatchNorm fusion into Conv layers",
    )
    args = parser.parse_args()

    # --- Load checkpoint ---
    if not os.path.isfile(args.input):
        print(f"ERROR: Input file not found: {args.input}", file=sys.stderr)
        sys.exit(1)

    print(f"Loading checkpoint: {args.input}")
    checkpoint = torch.load(args.input, map_location="cpu", weights_only=False)

    # --- Extract state dict ---
    print("Extracting state dict...")
    state_dict = extract_state_dict(checkpoint)
    print(f"  Found {len(state_dict)} parameters")

    # --- Fuse BatchNorm ---
    if not args.no_fuse_bn:
        print("Fusing BatchNorm layers...")
        state_dict = fuse_bn_into_conv(state_dict)

    # --- Remap keys ---
    print("Remapping keys...")
    state_dict = remap_keys(state_dict)
    print(f"  Final key count: {len(state_dict)}")

    # --- Convert to FP16 ---
    use_fp16 = not args.no_fp16
    if use_fp16:
        print("Converting to FP16...")
        state_dict = OrderedDict(
            (k, v.half() if v.is_floating_point() else v)
            for k, v in state_dict.items()
        )

    # --- Save as Safetensors ---
    os.makedirs(os.path.dirname(args.output) or ".", exist_ok=True)
    print(f"Saving to: {args.output}")
    save_file(state_dict, args.output)

    # --- Print stats ---
    file_size = os.path.getsize(args.output)
    total_params = sum(v.numel() for v in state_dict.values())
    dtype_str = "FP16" if use_fp16 else "FP32"

    print()
    print("=== Conversion Summary ===")
    print(f"  Parameters : {total_params:,}")
    print(f"  Keys       : {len(state_dict)}")
    print(f"  Dtype      : {dtype_str}")
    print(f"  File size  : {file_size / 1024 / 1024:.2f} MB")
    print(f"  Output     : {args.output}")
    print("Done.")


if __name__ == "__main__":
    main()
