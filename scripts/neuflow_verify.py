#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Verify NeuFlow v2 weight conversion accuracy.

Compares an original PyTorch checkpoint against a converted Safetensors file
to ensure the conversion preserved weights correctly (within FP16 tolerance).

Usage:
    python neuflow_verify.py --original neuflow_v2.pth --converted neuflow_v2_fp16.safetensors

Requirements:
    pip install torch safetensors numpy
"""

import argparse
import os
import sys
from collections import OrderedDict

import numpy as np
import torch
from safetensors.torch import load_file


def extract_state_dict(checkpoint: dict) -> dict:
    """Extract the model state dict from various checkpoint formats."""
    if isinstance(checkpoint, OrderedDict):
        return checkpoint

    for candidate_key in ("model", "state_dict", "model_state_dict"):
        if candidate_key in checkpoint:
            return checkpoint[candidate_key]

    if all(isinstance(v, torch.Tensor) for v in checkpoint.values()):
        return checkpoint

    best_key = None
    best_size = 0
    for k, v in checkpoint.items():
        if isinstance(v, (dict, OrderedDict)) and len(v) > best_size:
            best_key = k
            best_size = len(v)

    if best_key is not None:
        return checkpoint[best_key]

    raise ValueError(
        "Cannot find model state dict in checkpoint. "
        f"Top-level keys: {list(checkpoint.keys())}"
    )


def normalize_key(key: str) -> str:
    """Normalize a key for comparison (strip module. prefix)."""
    if key.startswith("module."):
        key = key[len("module."):]
    return key


def main():
    parser = argparse.ArgumentParser(
        description="Verify NeuFlow v2 weight conversion accuracy.",
        epilog="Example: python neuflow_verify.py "
               "--original neuflow_v2.pth "
               "--converted neuflow_v2_fp16.safetensors",
    )
    parser.add_argument(
        "--original", "-a",
        required=True,
        help="Path to the original PyTorch checkpoint (.pth / .pt)",
    )
    parser.add_argument(
        "--converted", "-b",
        required=True,
        help="Path to the converted Safetensors file",
    )
    parser.add_argument(
        "--tolerance",
        type=float,
        default=0.01,
        help="Maximum acceptable mean absolute error for PASS "
             "(default: 0.01, generous for FP16)",
    )
    parser.add_argument(
        "--verbose", "-v",
        action="store_true",
        help="Print per-key error details",
    )
    args = parser.parse_args()

    # --- Validate inputs ---
    for path, label in [(args.original, "original"), (args.converted, "converted")]:
        if not os.path.isfile(path):
            print(f"ERROR: {label} file not found: {path}", file=sys.stderr)
            sys.exit(1)

    # --- Load original ---
    print(f"Loading original: {args.original}")
    checkpoint = torch.load(args.original, map_location="cpu", weights_only=False)
    orig_dict = extract_state_dict(checkpoint)

    # Normalize original keys
    orig_normalized = {}
    for k, v in orig_dict.items():
        nk = normalize_key(k)
        if "num_batches_tracked" in nk:
            continue
        orig_normalized[nk] = v

    # --- Load converted ---
    print(f"Loading converted: {args.converted}")
    conv_dict = load_file(args.converted)

    # --- Compare key counts ---
    orig_keys = set(orig_normalized.keys())
    conv_keys = set(conv_dict.keys())

    print()
    print("=== Key Comparison ===")
    print(f"  Original keys  : {len(orig_keys)}")
    print(f"  Converted keys : {len(conv_keys)}")

    only_in_orig = orig_keys - conv_keys
    only_in_conv = conv_keys - orig_keys
    common_keys = orig_keys & conv_keys

    if only_in_orig:
        print(f"  Keys only in original ({len(only_in_orig)}):")
        for k in sorted(only_in_orig)[:20]:
            print(f"    - {k}")
        if len(only_in_orig) > 20:
            print(f"    ... and {len(only_in_orig) - 20} more")

    if only_in_conv:
        print(f"  Keys only in converted ({len(only_in_conv)}):")
        for k in sorted(only_in_conv)[:20]:
            print(f"    - {k}")
        if len(only_in_conv) > 20:
            print(f"    ... and {len(only_in_conv) - 20} more")

    if not common_keys:
        print("\nFAIL: No common keys found between original and converted files.")
        print("      Key remapping may have changed the naming scheme.")
        print("      If BatchNorm fusion was applied, BN keys are expected to "
              "be absent in the converted file.")
        sys.exit(1)

    print(f"  Common keys    : {len(common_keys)}")

    # --- FP16 conversion error analysis ---
    print()
    print("=== FP16 Conversion Error Analysis ===")

    all_abs_errors = []
    max_error_global = 0.0
    max_error_key = ""
    worst_keys = []

    for key in sorted(common_keys):
        orig_t = orig_normalized[key].float()
        conv_t = conv_dict[key].float()

        if orig_t.shape != conv_t.shape:
            print(f"  SHAPE MISMATCH: {key} — "
                  f"original {list(orig_t.shape)} vs "
                  f"converted {list(conv_t.shape)}")
            continue

        abs_err = torch.abs(orig_t - conv_t)
        max_err = abs_err.max().item()
        mean_err = abs_err.mean().item()

        all_abs_errors.append(mean_err)

        if max_err > max_error_global:
            max_error_global = max_err
            max_error_key = key

        worst_keys.append((mean_err, max_err, key))

        if args.verbose:
            print(f"  {key:60s}  mean={mean_err:.6e}  max={max_err:.6e}")

    if not all_abs_errors:
        print("  No comparable tensors found.")
        sys.exit(1)

    overall_mean = np.mean(all_abs_errors)
    overall_max = max(all_abs_errors)

    print(f"  Compared {len(all_abs_errors)} tensors")
    print(f"  Overall mean absolute error : {overall_mean:.6e}")
    print(f"  Overall max  absolute error : {max_error_global:.6e} ({max_error_key})")

    # Show top-5 worst keys
    worst_keys.sort(key=lambda x: x[0], reverse=True)
    if not args.verbose and len(worst_keys) > 0:
        print()
        print("  Top-5 highest mean error keys:")
        for mean_e, max_e, k in worst_keys[:5]:
            print(f"    {k:55s}  mean={mean_e:.6e}  max={max_e:.6e}")

    # --- Verdict ---
    print()
    passed = overall_mean <= args.tolerance
    if passed:
        print(f"PASS: Mean absolute error ({overall_mean:.6e}) "
              f"<= tolerance ({args.tolerance})")
    else:
        print(f"FAIL: Mean absolute error ({overall_mean:.6e}) "
              f"> tolerance ({args.tolerance})")

    if only_in_orig:
        bn_keys = [k for k in only_in_orig
                   if any(x in k for x in ("running_mean", "running_var",
                                            "weight", "bias"))]
        non_bn = only_in_orig - set(bn_keys)
        if non_bn:
            print(f"WARNING: {len(non_bn)} non-BN key(s) missing from converted file")
            passed = False

    sys.exit(0 if passed else 1)


if __name__ == "__main__":
    main()
