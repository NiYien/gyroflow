#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Post-process onnx2burn generated code to add F32 precision guards.

Inserts .cast(burn::tensor::DType::F32) guards around precision-sensitive
operations in the generated NeuFlow v2 mixed-precision Rust code:
  - Softmax (4x): prevent FP16 exp() overflow
  - GridSample (9x): bilinear interpolation coordinate precision
  - Resize (3x): upsampling precision
  - ReduceSum (1x): accumulation precision

Usage:
    python neuflow_postprocess_f32_guards.py <generated.rs>
"""

import re
import sys


def add_softmax_guards(code: str) -> tuple[str, int]:
    """Wrap softmax inputs with F32 cast."""
    pattern = (
        r"([ \t]+)let (softmax\d+_out1) = "
        r"burn::tensor::activation::softmax\((\w+), (\d+)\);"
    )

    def repl(m):
        indent, var, expr, dim = m.group(1), m.group(2), m.group(3), m.group(4)
        return (
            f"{indent}let {var} = {{\n"
            f"{indent}    let dtype = {expr}.dtype();\n"
            f"{indent}    burn::tensor::activation::softmax("
            f"{expr}.cast(burn::tensor::DType::F32), {dim})\n"
            f"{indent}        .cast(dtype)\n"
            f"{indent}}};"
        )

    result, count = re.subn(pattern, repl, code)
    return result, count


def add_resize_guards(code: str) -> tuple[str, int]:
    """Wrap resize inputs with F32 cast."""
    pattern = (
        r"([ \t]+)let (resize\d+_out1) = self\.(resize\d+)\.forward\((\w+)\);"
    )

    def repl(m):
        indent, var, resize, expr = (
            m.group(1), m.group(2), m.group(3), m.group(4),
        )
        return (
            f"{indent}let {var} = {{\n"
            f"{indent}    let dtype = {expr}.dtype();\n"
            f"{indent}    self.{resize}\n"
            f"{indent}        .forward({expr}.cast(burn::tensor::DType::F32))\n"
            f"{indent}        .cast(dtype)\n"
            f"{indent}}};"
        )

    result, count = re.subn(pattern, repl, code)
    return result, count


def add_reducesum_guards(code: str) -> tuple[str, int]:
    """Wrap sum_dim inputs with F32 cast."""
    pattern = (
        r"([ \t]+)let (reducesum\d+_out1) = \{\n"
        r"([ \t]+)(\w+)(\.sum_dim\(\d+usize\)\.squeeze_dims::<\d+usize>\(&\[\d+\]\))\n"
        r"([ \t]+)\};"
    )

    def repl(m):
        indent = m.group(1)
        var = m.group(2)
        inner_indent = m.group(3)
        expr = m.group(4)
        rest = m.group(5)
        close_indent = m.group(6)
        return (
            f"{indent}let {var} = {{\n"
            f"{inner_indent}let dtype = {expr}.dtype();\n"
            f"{inner_indent}{expr}.cast(burn::tensor::DType::F32)\n"
            f"{inner_indent}    {rest}\n"
            f"{inner_indent}    .cast(dtype)\n"
            f"{close_indent}}};"
        )

    result, count = re.subn(pattern, repl, code)
    return result, count


def add_gridsample_guards(code: str) -> tuple[str, int]:
    """Wrap grid_sample_2d data and grid with F32 cast.

    Handles two patterns:
      1. let gridsampleN_out1 = VAR
             .grid_sample_2d(GRID, ...);
      2. let gridsampleN_out1 = VAR
             .clone()
             .grid_sample_2d(GRID, ...);
    """
    # Pattern with .clone()
    pattern_clone = (
        r"([ \t]+)let (gridsample\d+_out1) = (\w+)\n"
        r"([ \t]+)\.clone\(\)\n"
        r"([ \t]+)\.grid_sample_2d\(\n"
        r"([ \t]+)(\w+),"
    )

    def repl_clone(m):
        indent = m.group(1)
        var = m.group(2)
        data_expr = m.group(3)
        clone_indent = m.group(4)
        gs_indent = m.group(5)
        grid_indent = m.group(6)
        grid_expr = m.group(7)
        return (
            f"{indent}let {var} = {{\n"
            f"{indent}    let data = {data_expr}.clone();\n"
            f"{indent}    let dtype = data.dtype();\n"
            f"{indent}    data.cast(burn::tensor::DType::F32)\n"
            f"{gs_indent}.grid_sample_2d(\n"
            f"{grid_indent}{grid_expr}.cast(burn::tensor::DType::F32),"
        )

    # Pattern without .clone()
    pattern_no_clone = (
        r"([ \t]+)let (gridsample\d+_out1) = (\w+)\n"
        r"([ \t]+)\.grid_sample_2d\(\n"
        r"([ \t]+)(\w+),"
    )

    def repl_no_clone(m):
        indent = m.group(1)
        var = m.group(2)
        data_expr = m.group(3)
        gs_indent = m.group(4)
        grid_indent = m.group(5)
        grid_expr = m.group(6)
        return (
            f"{indent}let {var} = {{\n"
            f"{indent}    let dtype = {data_expr}.dtype();\n"
            f"{indent}    {data_expr}.cast(burn::tensor::DType::F32)\n"
            f"{gs_indent}.grid_sample_2d(\n"
            f"{grid_indent}{grid_expr}.cast(burn::tensor::DType::F32),"
        )

    # Apply clone pattern first (more specific), then no-clone
    result, count_clone = re.subn(pattern_clone, repl_clone, code)
    result, count_no_clone = re.subn(pattern_no_clone, repl_no_clone, result)

    # Now add closing .cast(dtype) after the grid_sample_2d closing );
    # For each gridsample that was patched, find the closing ); and add .cast(dtype)
    # Pattern: match gridsample blocks that have our guard (contain "let dtype =")
    # and end with .with_align_corners(true),\n            );
    pattern_close = (
        r"(let (gridsample\d+_out1) = \{\n"
        r".*?)"  # non-greedy match of the guard block
        r"([ \t]+)\.with_align_corners\(true\),\n"
        r"([ \t]+)\);"
    )

    def repl_close(m):
        prefix = m.group(1)
        var = m.group(2)
        align_indent = m.group(3)
        close_indent = m.group(4)
        return (
            f"{prefix}"
            f"{align_indent}.with_align_corners(true),\n"
            f"{close_indent})\n"
            f"{close_indent}.cast(dtype)\n"
            f"{close_indent[:-4]}}};"  # dedent 4 for closing brace
        )

    result2, count_close = re.subn(
        pattern_close, repl_close, result, flags=re.DOTALL
    )

    # If the closing pattern is too fragile, use a simpler line-by-line approach
    if count_close != count_clone + count_no_clone:
        # Fallback: process line by line
        result2 = _add_gridsample_close_linebyline(result, count_clone + count_no_clone)

    return result2, count_clone + count_no_clone


def _add_gridsample_close_linebyline(code: str, expected_count: int) -> str:
    """Add .cast(dtype) after grid_sample_2d closing paren, line by line."""
    lines = code.split("\n")
    result_lines = []
    in_gridsample_block = False
    paren_depth = 0
    patched = 0

    for i, line in enumerate(lines):
        result_lines.append(line)

        # Detect start of a guarded gridsample block
        if "let gridsample" in line and "let dtype =" in lines[i + 1] if i + 1 < len(lines) else False:
            in_gridsample_block = True
            paren_depth = 0
            continue

        if in_gridsample_block:
            paren_depth += line.count("(") - line.count(")")
            # The grid_sample_2d call ends when we see ");" at the right depth
            stripped = line.strip()
            if stripped == ");" and paren_depth <= 0:
                # Replace ); with )\n    .cast(dtype)\n};
                indent = line[: len(line) - len(line.lstrip())]
                result_lines[-1] = f"{indent})\n{indent}.cast(dtype)\n{indent[:-4]}}};"
                in_gridsample_block = False
                patched += 1

    if patched != expected_count:
        print(
            f"WARNING: gridsample close patching expected {expected_count}, got {patched}",
            file=sys.stderr,
        )

    return "\n".join(result_lines)


def main():
    if len(sys.argv) != 2:
        print(f"usage: {sys.argv[0]} <generated.rs>", file=sys.stderr)
        return 1

    path = sys.argv[1]
    with open(path, "r", encoding="utf-8") as f:
        code = f.read()

    original_len = len(code)

    code, n_softmax = add_softmax_guards(code)
    print(f"  Softmax guards: {n_softmax}")

    code, n_gridsample = add_gridsample_guards(code)
    print(f"  GridSample guards: {n_gridsample}")

    code, n_resize = add_resize_guards(code)
    print(f"  Resize guards: {n_resize}")

    code, n_reducesum = add_reducesum_guards(code)
    print(f"  ReduceSum guards: {n_reducesum}")

    total = n_softmax + n_gridsample + n_resize + n_reducesum
    print(f"  Total: {total}")

    # Strict validation
    errors = []
    if n_softmax != 4:
        errors.append(f"Expected 4 softmax guards, got {n_softmax}")
    if n_gridsample != 9:
        errors.append(f"Expected 9 gridsample guards, got {n_gridsample}")
    if n_resize != 3:
        errors.append(f"Expected 3 resize guards, got {n_resize}")
    if n_reducesum != 1:
        errors.append(f"Expected 1 reducesum guard, got {n_reducesum}")

    if errors:
        print("\nERROR: Guard count mismatch!", file=sys.stderr)
        for e in errors:
            print(f"  {e}", file=sys.stderr)
        print(
            "\nGenerated code structure may have changed. Manual review needed.",
            file=sys.stderr,
        )
        return 1

    with open(path, "w", encoding="utf-8") as f:
        f.write(code)

    print(f"\nDone. File size: {original_len} -> {len(code)} bytes")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
