# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (c) 2026 Gyroflow contributors
"""smooth_diag analyzer

Reads dump.csv from the current directory and writes plot.png + summary.txt.
Requires: numpy, pandas, matplotlib. Install: pip install numpy pandas matplotlib

Usage:
    python plot.py
    python plot.py --self-check    # parse-only, exits 0 if imports work
"""
import argparse
import json
import sys
from pathlib import Path

def self_check() -> int:
    try:
        import numpy  # noqa: F401
        import pandas  # noqa: F401
        import matplotlib  # noqa: F401
    except ImportError as e:
        print(f"missing dependency: {e}", file=sys.stderr)
        return 1
    return 0

def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--self-check", action="store_true")
    ns = ap.parse_args()
    if ns.self_check:
        return self_check()

    import numpy as np
    import pandas as pd
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    here = Path(__file__).resolve().parent
    dump = here / "dump.csv"
    if not dump.exists():
        print(f"no dump.csv at {dump}", file=sys.stderr)
        return 2
    meta_path = here / "meta.json"
    meta = json.loads(meta_path.read_text()) if meta_path.exists() else {}

    df = pd.read_csv(dump)
    if df.empty:
        print("dump.csv has no rows", file=sys.stderr)
        return 3

    t = df["ts_ms"].to_numpy() / 1000.0
    fps = float(meta.get("video", {}).get("fps") or (1.0 / np.mean(np.diff(t)) if len(t) > 1 else 30.0))

    fig, axes = plt.subplots(2, 3, figsize=(18, 10))
    fig.suptitle(f"smooth_diag - {meta.get('video', {}).get('path', 'unknown')}", fontsize=12)

    # (0,0) delta angle per axis
    ax = axes[0, 0]
    ax.plot(t, df["delta_pitch_deg"], label="delta pitch")
    ax.plot(t, df["delta_yaw_deg"], label="delta yaw")
    ax.plot(t, df["delta_roll_deg"], label="delta roll")
    ax.set_title("delta angle per axis (q_smooth vs q_raw)")
    ax.set_xlabel("time (s)")
    ax.set_ylabel("deg")
    ax.legend(fontsize=8)
    ax.grid(True, alpha=0.3)

    # (0,1) delta_total over time
    ax = axes[0, 1]
    ax.plot(t, df["delta_total_deg"], color="tab:red")
    ax.set_title("delta_total (axis-angle of q_raw_inv * q_smooth)")
    ax.set_xlabel("time (s)")
    ax.set_ylabel("deg")
    ax.grid(True, alpha=0.3)

    # (0,2) velocity per axis
    ax = axes[0, 2]
    ax.plot(t, df["vel_pitch_deg_s"], label="pitch")
    ax.plot(t, df["vel_yaw_deg_s"], label="yaw")
    ax.plot(t, df["vel_roll_deg_s"], label="roll")
    ax.set_title("angular velocity (raw)")
    ax.set_xlabel("time (s)")
    ax.set_ylabel("deg/s")
    ax.legend(fontsize=8)
    ax.grid(True, alpha=0.3)

    # (1,0) FFT magnitude of velocity per axis
    ax = axes[1, 0]
    for col, name in [("vel_pitch_deg_s", "pitch"), ("vel_yaw_deg_s", "yaw"), ("vel_roll_deg_s", "roll")]:
        v = df[col].dropna().to_numpy()
        if len(v) >= 16:
            mag = np.abs(np.fft.rfft(v - v.mean()))
            freq = np.fft.rfftfreq(len(v), d=1.0 / fps)
            ax.loglog(freq[1:], mag[1:] + 1e-9, label=name)
    ax.set_title("velocity FFT spectrum (raw)")
    ax.set_xlabel("Hz")
    ax.set_ylabel("magnitude")
    ax.legend(fontsize=8)
    ax.grid(True, which="both", alpha=0.3)

    # (1,1) FFT of delta_total
    ax = axes[1, 1]
    d = df["delta_total_deg"].dropna().to_numpy()
    if len(d) >= 16:
        mag = np.abs(np.fft.rfft(d - d.mean()))
        freq = np.fft.rfftfreq(len(d), d=1.0 / fps)
        ax.loglog(freq[1:], mag[1:] + 1e-9, color="tab:red")
    ax.set_title("delta_total FFT (frequencies smoothing failed to nullify)")
    ax.set_xlabel("Hz")
    ax.set_ylabel("magnitude")
    ax.grid(True, which="both", alpha=0.3)

    # (1,2) fov_baseline vs delta_total scatter
    ax = axes[1, 2]
    ax.scatter(df["delta_total_deg"], df["fov_baseline"], s=5, alpha=0.4)
    if len(df) > 2:
        slope, intercept = np.polyfit(df["delta_total_deg"], df["fov_baseline"], 1)
        xs = np.linspace(df["delta_total_deg"].min(), df["delta_total_deg"].max(), 50)
        ax.plot(xs, slope * xs + intercept, color="tab:red", linewidth=1)
        ax.set_title(f"fov_baseline vs delta_total (slope={slope:.4f})")
    else:
        ax.set_title("fov_baseline vs delta_total")
    ax.set_xlabel("delta_total (deg)")
    ax.set_ylabel("fov_baseline")
    ax.grid(True, alpha=0.3)

    plt.tight_layout()
    out_png = here / "plot.png"
    fig.savefig(out_png, dpi=110)

    # summary.txt
    def band_energy(x: np.ndarray, fps_: float, lo: float, hi: float) -> float:
        if len(x) < 16:
            return 0.0
        mag = np.abs(np.fft.rfft(x - x.mean())) ** 2
        freq = np.fft.rfftfreq(len(x), d=1.0 / fps_)
        sel = (freq >= lo) & (freq <= hi)
        total = mag.sum() + 1e-12
        return float(mag[sel].sum() / total * 100.0)

    lines = []
    lines.append("[Trend / mid / high energy of velocity per axis]")
    for col, name in [("vel_pitch_deg_s", "pitch"), ("vel_yaw_deg_s", "yaw"), ("vel_roll_deg_s", "roll")]:
        v = df[col].dropna().to_numpy()
        lines.append(
            f"  {name}: <1Hz = {band_energy(v, fps, 0, 1):.1f}%, "
            f"1-3Hz = {band_energy(v, fps, 1, 3):.1f}%, "
            f">5Hz = {band_energy(v, fps, 5, fps / 2):.1f}%"
        )
    lines.append("")
    lines.append("[Jerk]")
    for col, name in [("jerk_pitch_deg_s3", "pitch"), ("jerk_yaw_deg_s3", "yaw"), ("jerk_roll_deg_s3", "roll")]:
        j = df[col].dropna().to_numpy()
        if len(j) > 0:
            lines.append(f"  {name}: RMS = {np.sqrt(np.mean(j ** 2)):.1f}, p99 = {np.percentile(np.abs(j), 99):.1f} deg/s^3")
    lines.append("")
    lines.append("[Delta angle (q_smooth vs q_raw)]")
    d = df["delta_total_deg"].to_numpy()
    lines.append(f"  p50 = {np.percentile(d, 50):.3f} deg")
    lines.append(f"  p95 = {np.percentile(d, 95):.3f} deg")
    lines.append(f"  p99 = {np.percentile(d, 99):.3f} deg")
    lines.append(f"  max = {d.max():.3f} deg @ frame {int(np.argmax(np.abs(d)))}")
    lines.append("")
    lines.append("[FOV]")
    fb = df["fov_baseline"].to_numpy()
    ff = df["fov_final"].to_numpy()
    lines.append(f"  fov_baseline: min = {fb.min():.4f}, p05 = {np.percentile(fb, 5):.4f}, median = {np.median(fb):.4f}")
    lines.append(f"  fov_final:    min = {ff.min():.4f}, p05 = {np.percentile(ff, 5):.4f}, median = {np.median(ff):.4f}")
    lines.append("")
    lines.append("[Linearity check]")
    if len(df) > 2:
        r = np.corrcoef(df["delta_total_deg"], df["fov_baseline"])[0, 1]
        lines.append(f"  Pearson r(delta_total, fov_baseline) = {r:.3f}")

    (here / "summary.txt").write_text("\n".join(lines), encoding="utf-8")
    print(f"wrote {out_png} and summary.txt")
    return 0

if __name__ == "__main__":
    sys.exit(main())
