"""Run the installer's real SDK cache check against complete and stale packages."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = (ROOT / "_scripts/macos.just").read_text(encoding="utf-8")
BEGIN = SOURCE.index("    ffmpeg_has_videotoolbox_encoders() {")
END = SOURCE.index("    if ! ffmpeg_installed; then", BEGIN)
FUNCTIONS = textwrap.dedent(SOURCE[BEGIN:END])
PIN = re.search(r'ffmpeg_sha256="([0-9a-f]{64})"', SOURCE).group(1)
BASH = os.environ.get("GYROFLOW_TEST_POSIX_SHELL") or (
    "C:/Program Files/Git/bin/bash.exe" if os.name == "nt" else shutil.which("bash")
)


class MacFfmpegPackageTests(unittest.TestCase):
    def test_sdk_must_match_verified_package_and_remain_complete(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for arch in ("ffmpeg-x86_64", "ffmpeg-arm64"):
                for name in ("include/libavutil/avutil.h", "lib/libavutil.a", "lib/libavcodec.a"):
                    path = root / arch / name
                    path.parent.mkdir(parents=True, exist_ok=True)
                    path.touch()
            script = root / "check.sh"
            script.write_text(
                "nm() { printf '%s\\n' _ff_h264_videotoolbox_encoder "
                "_ff_hevc_videotoolbox_encoder _ff_prores_videotoolbox_encoder; }\n"
                + f"ffmpeg_sha256={PIN}\n" + FUNCTIONS + "\nffmpeg_installed\n",
                encoding="utf-8", newline="\n",
            )

            def accepted():
                return subprocess.run([BASH, "check.sh"], cwd=root, capture_output=True).returncode == 0

            self.assertFalse(accepted(), "the old unmarked SDK must be refreshed")
            for arch in ("ffmpeg-x86_64", "ffmpeg-arm64"):
                (root / arch / ".gyroflow-package-sha256").write_text("old-package\n")
            self.assertFalse(accepted(), "a different package must be refreshed")
            for arch in ("ffmpeg-x86_64", "ffmpeg-arm64"):
                (root / arch / ".gyroflow-package-sha256").write_text(PIN + "\n")
            self.assertTrue(accepted(), "the verified complete package can be reused")
            (root / "ffmpeg-arm64/lib/libavcodec.a").unlink()
            self.assertFalse(accepted(), "a matching marker cannot hide an incomplete install")


if __name__ == "__main__":
    unittest.main()
