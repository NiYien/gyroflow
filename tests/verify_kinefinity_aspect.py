# SPDX-License-Identifier: GPL-3.0-or-later
"""Check images produced by qml/kinefinity_aspect_app_smoke.qml (Pillow + NumPy)."""
import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image


def verify(directory: Path):
    def pixels(name, size=(960, 616)):
        with Image.open(directory / f"{name}.png") as image:
            return np.asarray(image.convert("RGB").resize(size, Image.Resampling.LANCZOS), dtype=float)

    def edge_mean(image):
        return float(np.concatenate((image[8:30, 12:-12], image[-30:-8, 12:-12])).mean())

    original = pixels("fixed-source")
    baseline = pixels("display-sar-baseline")
    report = {"fixed_edge_mean": edge_mean(original), "baseline_edge_mean": edge_mean(baseline)}
    report["source_raster_mean_error"] = float(np.abs(original - pixels("raw-pixels")).mean())
    assert report["source_raster_mean_error"] < 4, report
    assert report["baseline_edge_mean"] < 25, report
    assert report["fixed_edge_mean"] > 50, report
    for name in ["fixed-paused", "fixed-small"]:
        image = pixels(name)
        report[name + "_mean_error"] = float(np.abs(image - original).mean())
        assert edge_mean(image) > 50, name
        assert report[name + "_mean_error"] < 4, report

    reference = pixels("preset-pipeline-0", (1277, 616))
    for pipeline in [1, 2]:
        name = f"preset-pipeline-{pipeline}"
        image = pixels(name, (1277, 616))
        report[name + "_mean_error"] = float(np.abs(image - reference).mean())
        assert report[name + "_mean_error"] < 5, report

    control = pixels("control-default", (384, 246))
    reset = pixels("control-reset", (384, 246))
    report["control_mean_error"] = float(np.abs(control - reset).mean())
    report["control_edge_mean"] = edge_mean(control)
    assert report["control_mean_error"] < 1, report
    assert report["control_edge_mean"] < 25, report
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    print(json.dumps(verify(args.directory), indent=2))
