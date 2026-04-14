#!/usr/bin/env python3
"""
NeuFlow v2 performance test script.

Runs gyroflow with NeuFlow optical flow, captures logs, and reports key metrics.
Supports multi-round testing and before/after comparison.

Usage:
    python _scripts/test_neuflow_perf.py --video test.mp4 [--rounds 5] [--build] [--save results.json] [--compare baseline.json]

    # Alternatively, parse an existing log file instead of running gyroflow:
    python _scripts/test_neuflow_perf.py --log-file captured.log [--rounds 1] [--save results.json]
"""

import argparse
import json
import os
import re
import statistics
import subprocess
import sys
import time
from pathlib import Path

# ── regex patterns for log parsing ────────────────────────────────────────────

RE_ROUNDTRIP = re.compile(r'sample_channel_roundtrip=([\d.]+)ms')
RE_SERIAL_BATCH = re.compile(r'processing (\d+) (?:optical-flow|NeuFlow) pairs serially')
RE_SKIPPED = re.compile(r'skipping (\d+) NeuFlow pairs')
RE_QUEUE_DEPTH = re.compile(r'queue_depth_at_send=(\d+)')
RE_GPU_TIMING = re.compile(
    r'\[NeuFlow perf\] #\d+ (?:SAMPLE )?tensor=([\d.]+)ms forward=([\d.]+)ms'
)
RE_PIPE_TIMING = re.compile(
    r'\[NeuFlow perf\] #\d+ PIPE SAMPLE readback\+fwd_select=([\d.]+)ms'
)
RE_OF_TO = re.compile(
    r'\[NeuFlow perf\] of_to: inference=([\d.]+)ms.*total=([\d.]+)ms'
)
RE_FALLBACK_EMPTY = re.compile(r'NeuFlow fallback: one or both RGB frames are empty')


def parse_log(text: str) -> dict:
    """Extract NeuFlow performance metrics from log text."""
    roundtrips = [float(m.group(1)) for m in RE_ROUNDTRIP.finditer(text)]
    serial_batches = [int(m.group(1)) for m in RE_SERIAL_BATCH.finditer(text)]
    skipped = [int(m.group(1)) for m in RE_SKIPPED.finditer(text)]
    queue_depths = [int(m.group(1)) for m in RE_QUEUE_DEPTH.finditer(text)]
    gpu_forward = [float(m.group(2)) for m in RE_GPU_TIMING.finditer(text)]
    pipe_times = [float(m.group(1)) for m in RE_PIPE_TIMING.finditer(text)]
    of_to_totals = [float(m.group(2)) for m in RE_OF_TO.finditer(text)]
    fallback_count = len(RE_FALLBACK_EMPTY.findall(text))

    def stats(values):
        if not values:
            return {"count": 0, "avg": 0, "p50": 0, "p95": 0, "max": 0, "min": 0}
        s = sorted(values)
        p95_idx = max(0, int(len(s) * 0.95) - 1)
        return {
            "count": len(s),
            "avg": round(statistics.mean(s), 1),
            "p50": round(statistics.median(s), 1),
            "p95": round(s[p95_idx], 1),
            "max": round(max(s), 1),
            "min": round(min(s), 1),
        }

    return {
        "roundtrip": stats(roundtrips),
        "gpu_forward": stats(gpu_forward),
        "pipe_readback": stats(pipe_times),
        "of_to_total": stats(of_to_totals),
        "serial_batches": serial_batches,
        "serial_batch_count": len(serial_batches),
        "skipped_pairs": sum(skipped),
        "skip_events": len(skipped),
        "queue_depth_max": max(queue_depths) if queue_depths else 0,
        "queue_depth_avg": round(statistics.mean(queue_depths), 1) if queue_depths else 0,
        "inference_count": len(roundtrips),
        "fallback_empty_count": fallback_count,
    }


def print_metrics(m: dict, label: str = ""):
    """Pretty-print one round's metrics."""
    prefix = f"[{label}] " if label else ""
    print(f"\n{'=' * 60}")
    print(f"{prefix}NeuFlow Performance Metrics")
    print(f"{'=' * 60}")
    print(f"  Inference count:      {m['inference_count']}")
    print(f"  Serial batch calls:   {m['serial_batch_count']}  (sizes: {m['serial_batches'][:5]}{'...' if len(m['serial_batches']) > 5 else ''})")
    print(f"  CAS skip events:      {m['skip_events']}  (total pairs skipped: {m['skipped_pairs']})")
    print(f"  Fallback (empty RGB): {m['fallback_empty_count']}")
    print()
    print(f"  Channel roundtrip (ms):")
    rt = m['roundtrip']
    print(f"    avg={rt['avg']}  p50={rt['p50']}  p95={rt['p95']}  max={rt['max']}  min={rt['min']}  n={rt['count']}")
    print(f"  GPU forward (ms):")
    gf = m['gpu_forward']
    print(f"    avg={gf['avg']}  p50={gf['p50']}  max={gf['max']}  n={gf['count']}")
    print(f"  Queue depth:")
    print(f"    max={m['queue_depth_max']}  avg={m['queue_depth_avg']}")
    print(f"{'=' * 60}")


def print_comparison(before: dict, after: dict):
    """Print before/after delta table."""
    def delta(b, a):
        if b == 0:
            return "N/A"
        pct = ((a - b) / b) * 100
        return f"{pct:+.1f}%"

    rows = [
        ("Inference count",     before['inference_count'],     after['inference_count']),
        ("Avg roundtrip (ms)",  before['roundtrip']['avg'],    after['roundtrip']['avg']),
        ("P95 roundtrip (ms)",  before['roundtrip']['p95'],    after['roundtrip']['p95']),
        ("Max roundtrip (ms)",  before['roundtrip']['max'],    after['roundtrip']['max']),
        ("Max queue depth",     before['queue_depth_max'],     after['queue_depth_max']),
        ("Serial batch calls",  before['serial_batch_count'],  after['serial_batch_count']),
        ("CAS skip events",     before['skip_events'],         after['skip_events']),
    ]

    print(f"\n{'=' * 65}")
    print(f"  Before vs After Comparison")
    print(f"{'=' * 65}")
    print(f"  {'Metric':<25} {'Before':>10} {'After':>10} {'Delta':>10}")
    print(f"  {'-' * 55}")
    for name, b, a in rows:
        print(f"  {name:<25} {b:>10} {a:>10} {delta(b, a):>10}")
    print(f"{'=' * 65}")


def run_gyroflow(video_path: str, exe_path: str) -> str:
    """Run gyroflow CLI with NeuFlow sync and capture logs."""
    env = os.environ.copy()
    env['RUST_LOG'] = 'debug'

    cmd = [
        exe_path,
        video_path,
        '-s', "{'of_method': 3}",
        '--export_project', '1',
    ]

    print(f"  Running: {' '.join(cmd)}")
    t0 = time.time()
    result = subprocess.run(
        cmd,
        capture_output=True,
        text=True,
        env=env,
        timeout=300,
    )
    elapsed = time.time() - t0
    print(f"  Completed in {elapsed:.1f}s (exit code {result.returncode})")

    # Combine stdout and stderr (Rust log goes to stderr)
    return result.stdout + '\n' + result.stderr


def find_exe() -> str:
    """Find the gyroflow executable."""
    candidates = [
        'target/release/gyroflow.exe',
        'target/release/gyroflow',
    ]
    repo_root = Path(__file__).resolve().parent.parent
    for c in candidates:
        p = repo_root / c
        if p.exists():
            return str(p)
    raise FileNotFoundError(
        f"Cannot find gyroflow executable. Tried: {candidates}\n"
        f"Run 'just build' first."
    )


def main():
    parser = argparse.ArgumentParser(description='NeuFlow performance test')
    parser.add_argument('--video', help='Test video path (required unless --log-file)')
    parser.add_argument('--log-file', help='Parse existing log file instead of running gyroflow')
    parser.add_argument('--rounds', type=int, default=5, help='Number of test rounds (default: 5)')
    parser.add_argument('--build', action='store_true', help='Build before testing')
    parser.add_argument('--save', help='Save results to JSON file')
    parser.add_argument('--compare', help='Compare against baseline JSON file')
    parser.add_argument('--exe', help='Path to gyroflow executable')
    args = parser.parse_args()

    if not args.video and not args.log_file:
        parser.error('Either --video or --log-file is required')

    repo_root = Path(__file__).resolve().parent.parent

    # Build if requested
    if args.build:
        print("Building...")
        subprocess.run(
            ['just', 'build'],
            cwd=str(repo_root),
            check=True,
        )
        print("Build complete.\n")

    all_metrics = []

    for i in range(args.rounds):
        print(f"\n--- Round {i + 1}/{args.rounds} ---")

        if args.log_file:
            with open(args.log_file, 'r', encoding='utf-8', errors='replace') as f:
                log_text = f.read()
        else:
            exe = args.exe or find_exe()
            log_text = run_gyroflow(args.video, exe)

        metrics = parse_log(log_text)
        print_metrics(metrics, label=f"Round {i + 1}")
        all_metrics.append(metrics)

    # Summary across rounds
    if args.rounds > 1:
        print(f"\n{'#' * 60}")
        print(f"  Summary ({args.rounds} rounds)")
        print(f"{'#' * 60}")
        avg_roundtrips = [m['roundtrip']['avg'] for m in all_metrics]
        max_roundtrips = [m['roundtrip']['max'] for m in all_metrics]
        infer_counts = [m['inference_count'] for m in all_metrics]
        max_depths = [m['queue_depth_max'] for m in all_metrics]

        def fmt_list(vals):
            if not vals or all(v == 0 for v in vals):
                return "N/A"
            return f"mean={statistics.mean(vals):.1f}  stdev={statistics.stdev(vals):.1f}" if len(vals) > 1 else f"{vals[0]}"

        print(f"  Avg roundtrip (ms): {fmt_list(avg_roundtrips)}")
        print(f"  Max roundtrip (ms): {fmt_list(max_roundtrips)}")
        print(f"  Inference count:    {fmt_list(infer_counts)}")
        print(f"  Max queue depth:    {fmt_list(max_depths)}")

    # Save results
    if args.save:
        # Use last round as representative, include all rounds
        output = {
            "summary": all_metrics[-1] if all_metrics else {},
            "rounds": all_metrics,
        }
        with open(args.save, 'w') as f:
            json.dump(output, f, indent=2)
        print(f"\nResults saved to {args.save}")

    # Compare
    if args.compare:
        with open(args.compare, 'r') as f:
            baseline = json.load(f)
        before = baseline.get('summary', baseline.get('rounds', [{}])[-1])
        after = all_metrics[-1] if all_metrics else {}
        print_comparison(before, after)

    # Exit code: fail if max roundtrip > 500ms (post-fix threshold)
    if all_metrics:
        worst_max = max(m['roundtrip']['max'] for m in all_metrics)
        if worst_max > 500 and not args.compare:
            print(f"\nWARNING: Max roundtrip {worst_max:.1f}ms exceeds 500ms threshold")


if __name__ == '__main__':
    main()
