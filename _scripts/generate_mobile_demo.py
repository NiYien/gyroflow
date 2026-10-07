# SPDX-License-Identifier: GPL-3.0-or-later
"""Generate original synthetic footage and matching roll motion (requires NumPy)."""
import argparse
import json
import math
from pathlib import Path
import subprocess
import numpy as np

ROOT = Path(__file__).resolve().parents[1]
WIDTH, HEIGHT, FPS, SECONDS = 640, 360, 30, 6


def angle(t):
    return 0.065 * math.sin(2 * math.pi * 1.3 * t) + 0.025 * math.sin(2 * math.pi * 2.2 * t)


def rate(t):
    return 0.065 * 2 * math.pi * 1.3 * math.cos(2 * math.pi * 1.3 * t) + 0.025 * 2 * math.pi * 2.2 * math.cos(2 * math.pi * 2.2 * t)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--ffmpeg", required=True)
    args = parser.parse_args()
    destination = ROOT / "resources/demo"
    destination.mkdir(parents=True, exist_ok=True)
    y, x = np.mgrid[:HEIGHT, :WIDTH].astype(float)
    x -= WIDTH / 2
    y -= HEIGHT / 2
    process = subprocess.Popen([args.ffmpeg, "-y", "-hide_banner", "-loglevel", "error", "-f", "rawvideo", "-pixel_format", "rgb24", "-video_size", f"{WIDTH}x{HEIGHT}", "-framerate", str(FPS), "-i", "-", "-an", "-c:v", "libx264", "-preset", "medium", "-crf", "20", "-pix_fmt", "yuv420p", "-movflags", "+faststart", str(destination / "niyien-demo.mp4")], stdin=subprocess.PIPE)
    for frame in range(FPS * SECONDS):
        a = angle(frame / FPS)
        world_x = np.cos(a) * x - np.sin(a) * y
        world_y = np.sin(a) * x + np.cos(a) * y
        pixels = np.full((HEIGHT, WIDTH, 3), [239, 243, 248], dtype=np.uint8)
        pixels[(np.abs((world_x + 20) % 40 - 20) < 1.3) | (np.abs((world_y + 20) % 40 - 20) < 1.3)] = [169, 187, 203]
        pixels[(world_x > -230) & (world_x < -60) & (world_y > -100) & (world_y < 75)] = [10, 168, 215]
        pixels[(world_x - 140) ** 2 + (world_y + 20) ** 2 < 75 ** 2] = [246, 159, 16]
        pixels[(np.abs(world_y - 112) < 2) & (np.abs(world_x) < 280)] = [36, 42, 51]
        pixels[(np.abs(world_x) < 2) & (np.abs(world_y) < 145)] = [36, 42, 51]
        process.stdin.write(pixels.tobytes())
    process.stdin.close()
    if process.wait() != 0:
        raise RuntimeError("Could not encode the demo")
    gyro = ["GYROFLOW IMU LOG", "version,1.3", "id,NiYien generated roll demonstration", "orientation,XYZ", "tscale,0.001", "gscale,1.0", "t,gx,gy,gz"]
    for i in range(SECONDS * 400 + 1):
        t = i / 400
        gyro.append(f"{t * 1000:.3f},0,0,{-rate(t):.12f}")
    (destination / "niyien-demo.gcsv").write_text("\n".join(gyro) + "\n")
    project = {"title": "NiYien generated demo", "version": 4, "videofile": "niyien-demo.mp4",
        "video_info": {"width": WIDTH, "height": HEIGHT, "fps": FPS, "num_frames": FPS * SECONDS, "duration_ms": SECONDS * 1000, "rotation": 0},
        "gyro_source": {"filepath": "niyien-demo.gcsv", "imu_orientation": "XYZ", "integration_method": 3, "lpf": 0},
        "offsets": {"0": 0},
        "calibration_data": {"name": "NiYien generated pinhole camera", "camera_brand": "NiYien", "camera_model": "Synthetic demo", "calib_dimension": {"w": WIDTH, "h": HEIGHT}, "orig_dimension": {"w": WIDTH, "h": HEIGHT}, "fps": FPS, "distortion_model": "opencv_standard", "fisheye_params": {"camera_matrix": [[450, 0, WIDTH / 2], [0, 450, HEIGHT / 2], [0, 0, 1]], "distortion_coeffs": [0, 0, 0, 0, 0]}},
        "stabilization": {"method": "Default", "smoothing_params": [{"name": "smoothness", "value": 0.5}], "fov": 0.85, "adaptive_zoom_window": 0, "frame_readout_time": 0},
        "output": {"codec": "H.264/AVC", "output_width": WIDTH, "output_height": HEIGHT, "bitrate": 8, "keyframe_distance": 1.0, "use_gpu": True, "audio": False, "pixel_format": "yuv420p", "output_filename": "NiYien-demo-stabilized.mp4"}}
    (destination / "niyien-demo.gyroflow").write_text(json.dumps(project, indent=2) + "\n")


if __name__ == "__main__":
    main()
