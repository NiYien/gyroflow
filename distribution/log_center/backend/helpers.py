"""Helpers shared across orchestrator + frontend bridge.

Includes: zip extraction (with optional zstd-inside-zip support), human
size formatting, OS file-manager opening, .gyroflow project summary,
log tail, and clipboard write.

Clipboard fallback: pywebview lacks a stable cross-platform clipboard
API across all backends, so we use the platform shims below
(Windows tkinter, macOS pbcopy, Linux xclip / wl-copy) and fall back to
writing a ``clipboard_fallback.txt`` next to the cache root.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path
from typing import Iterable, Optional


# ---------------- size formatting ----------------


def human_size(num_bytes: int) -> str:
    """Format a byte count as e.g. ``1.2 MB``. Stops at TB."""
    n = float(num_bytes or 0)
    units = ["B", "KB", "MB", "GB", "TB"]
    i = 0
    while n >= 1024 and i < len(units) - 1:
        n /= 1024
        i += 1
    if i == 0:
        return f"{int(n)} {units[i]}"
    return f"{n:.1f} {units[i]}"


def directory_size(path: Path) -> int:
    """Recursive sum of file sizes under ``path``. Returns 0 if missing."""
    p = Path(path)
    if not p.exists():
        return 0
    total = 0
    for root, _dirs, files in os.walk(p):
        for f in files:
            try:
                total += (Path(root) / f).stat().st_size
            except OSError:
                pass
    return total


# ---------------- file manager ----------------


def open_in_file_manager(path: Path) -> None:
    """Open ``path`` in the OS file manager. Best effort, no exceptions
    propagate (failure shows up in stderr).
    """
    p = Path(path)
    try:
        if sys.platform == "win32":
            # explorer.exe accepts a directory or file path. For files, the
            # /select, switch reveals it inside the parent folder.
            if p.is_file():
                subprocess.Popen(["explorer.exe", "/select,", str(p)])
            else:
                subprocess.Popen(["explorer.exe", str(p)])
        elif sys.platform == "darwin":
            subprocess.Popen(["open", str(p)])
        else:
            subprocess.Popen(["xdg-open", str(p)])
    except Exception as exc:
        print(f"[log_center.helpers] open_in_file_manager failed: {exc}", file=sys.stderr)


# ---------------- zip extraction ----------------


def extract_zip(zip_path: Path, target_dir: Path) -> None:
    """Extract ``zip_path`` into ``target_dir``. Creates the dir if missing.

    Phase 4 client may pack inner files compressed with zstd inside the
    zip (logs can be large); we transparently re-decompress those entries
    that have a ``.zst`` suffix using the ``zstandard`` lib if installed.
    Plain zip entries (no .zst) are left untouched.

    If ``zstandard`` is not installed, .zst entries are extracted as-is and
    the user can decompress them later (we do not raise; surface a hint
    in stderr instead).
    """
    target_dir.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(zip_path) as zf:
        zf.extractall(target_dir)

    # Decompress any .zst files in-place if zstandard is available.
    zstd_files = [p for p in target_dir.rglob("*.zst") if p.is_file()]
    if not zstd_files:
        return
    try:
        import zstandard  # type: ignore
    except ImportError:
        print(
            "[log_center.helpers] note: zip contains .zst files but `zstandard` "
            "is not installed; leaving them compressed (pip install zstandard "
            "to auto-decompress)",
            file=sys.stderr,
        )
        return
    dctx = zstandard.ZstdDecompressor()
    for zst in zstd_files:
        out_path = zst.with_suffix("")  # drop .zst
        try:
            with zst.open("rb") as src, out_path.open("wb") as dst:
                dctx.copy_stream(src, dst)
            zst.unlink()
        except Exception as exc:
            print(f"[log_center.helpers] zstd decompress failed for {zst}: {exc}", file=sys.stderr)


# ---------------- log tail ----------------


def tail_text_file(path: Path, max_bytes: int) -> str:
    """Return the last ``max_bytes`` of a text file. Returns ``""`` if
    missing. Reads in binary then decodes with replacement so we don't
    blow up on partial multibyte characters at the cut boundary.
    """
    p = Path(path)
    if not p.exists() or not p.is_file():
        return ""
    size = p.stat().st_size
    start = max(0, size - max_bytes)
    with p.open("rb") as fh:
        fh.seek(start)
        data = fh.read()
    text = data.decode("utf-8", errors="replace")
    if start > 0:
        # Drop any partial line at the front so the slice starts at a real
        # log line.
        nl = text.find("\n")
        if nl != -1:
            text = text[nl + 1:]
        text = "(... truncated head ...)\n" + text
    return text


# ---------------- gyroflow project summary ----------------


def summarize_gyroflow_project(path: Path) -> str:
    """Read a .gyroflow project file and return a markdown summary of the
    fields that are useful for triage:

      * version, file format
      * camera (brand / model / lens identifier)
      * lens_profile (path / name)
      * synchronization (offsets count, sync method)
      * smoothing method + params
      * gyro_source (kind, sample rate)
      * output (codec, resolution)

    If the file isn't valid JSON or is missing, returns
    ``"(project file not present)"`` so the prompt template still renders.
    """
    p = Path(path)
    if not p.exists():
        return "(project file not present)"
    try:
        raw = p.read_text(encoding="utf-8", errors="replace")
        data = json.loads(raw)
    except (OSError, json.JSONDecodeError) as exc:
        return f"(could not parse project file: {exc})"
    if not isinstance(data, dict):
        return "(project file is not a JSON object)"

    lines: list[str] = []

    def add(key: str, value: object) -> None:
        if value in (None, "", [], {}):
            return
        if isinstance(value, (dict, list)):
            value = json.dumps(value, ensure_ascii=False)
        lines.append(f"- **{key}**: {value}")

    add("version", data.get("version"))
    cam = data.get("camera") or {}
    if isinstance(cam, dict):
        add("camera.brand", cam.get("brand"))
        add("camera.model", cam.get("model"))
        add("camera.lens", cam.get("lens"))
    lens = data.get("lens_profile") or {}
    if isinstance(lens, dict):
        add("lens_profile.path", lens.get("path"))
        add("lens_profile.name", lens.get("name"))
        add("lens_profile.calib_dimension", lens.get("calib_dimension"))
    sync = data.get("synchronization") or {}
    if isinstance(sync, dict):
        offsets = sync.get("offsets") or sync.get("sync_offsets") or []
        if isinstance(offsets, list):
            add("synchronization.offsets_count", len(offsets))
        add("synchronization.method", sync.get("method") or sync.get("sync_method"))
        add("synchronization.initial_offset", sync.get("initial_offset"))
    smooth = data.get("smoothing") or data.get("stabilization") or {}
    if isinstance(smooth, dict):
        add("smoothing.method", smooth.get("method") or smooth.get("name"))
        # Drop any sub-dict of params for compactness
        params = smooth.get("params") or {}
        if isinstance(params, dict) and params:
            short = {k: v for k, v in list(params.items())[:6]}
            add("smoothing.params", short)
    gyro = data.get("gyro_source") or {}
    if isinstance(gyro, dict):
        add("gyro_source.kind", gyro.get("kind") or gyro.get("type"))
        add("gyro_source.sample_rate", gyro.get("sample_rate"))
    out = data.get("output") or data.get("export") or {}
    if isinstance(out, dict):
        add("output.codec", out.get("codec") or out.get("encoder"))
        add("output.resolution", out.get("resolution") or out.get("output_size"))

    if not lines:
        # Nothing matched our schema heuristics — surface top-level keys.
        keys = sorted(data.keys())[:20]
        return "Unrecognized .gyroflow shape; top-level keys: " + ", ".join(keys)
    return "\n".join(lines)


# ---------------- clipboard ----------------


def clipboard_set(text: str, *, fallback_dir: Optional[Path] = None) -> str:
    """Best-effort clipboard write. Returns the actual mechanism used:
    ``"win"`` / ``"mac"`` / ``"xclip"`` / ``"wl-copy"`` / ``"file:<path>"``.

    If every native shim fails we drop the text into
    ``<fallback_dir>/clipboard_fallback.txt`` and return that path so the
    UI can tell the user where to find it.
    """
    if sys.platform == "win32":
        try:
            import tkinter  # part of stdlib on Windows builds
            r = tkinter.Tk()
            r.withdraw()
            r.clipboard_clear()
            r.clipboard_append(text)
            # update() forces tk to push the data to the OS clipboard
            # before we destroy the root.
            r.update()
            r.destroy()
            return "win"
        except Exception as exc:
            print(f"[log_center.helpers] tk clipboard failed: {exc}", file=sys.stderr)
    elif sys.platform == "darwin":
        try:
            p = subprocess.Popen(["pbcopy"], stdin=subprocess.PIPE)
            p.communicate(text.encode("utf-8"))
            if p.returncode == 0:
                return "mac"
        except FileNotFoundError:
            pass
    else:
        for cmd in (["wl-copy"], ["xclip", "-selection", "clipboard"]):
            try:
                p = subprocess.Popen(cmd, stdin=subprocess.PIPE)
                p.communicate(text.encode("utf-8"))
                if p.returncode == 0:
                    return cmd[0]
            except FileNotFoundError:
                continue

    # Fallback: dump to a file the user can open.
    target = (fallback_dir or Path(tempfile.gettempdir())) / "log_center_clipboard_fallback.txt"
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_text(text, encoding="utf-8")
    return f"file:{target}"


# ---------------- find files inside extracted feedback ----------------


def find_first_existing(root: Path, candidates: Iterable[str]) -> Optional[Path]:
    """Return the first path under ``root`` whose suffix matches any of
    ``candidates`` (e.g. ``logs/current-session.log``). Walks the tree so
    archives that nest content one level down still match.
    """
    root = Path(root)
    if not root.exists():
        return None
    candidate_set = {c.replace("\\", "/").strip("/") for c in candidates}
    for path in root.rglob("*"):
        if not path.is_file():
            continue
        rel = path.relative_to(root).as_posix()
        if rel in candidate_set or any(rel.endswith("/" + c) for c in candidate_set):
            return path
    return None


def find_first_with_suffix(root: Path, suffix: str) -> Optional[Path]:
    """First ``*<suffix>`` file under root, or None."""
    root = Path(root)
    if not root.exists():
        return None
    for path in root.rglob(f"*{suffix}"):
        if path.is_file():
            return path
    return None


# ---------------- safe rmtree ----------------


def safe_rmtree(path: Path) -> bool:
    """``shutil.rmtree`` that swallows missing-dir errors. Returns True
    if the directory was removed or already absent.
    """
    p = Path(path)
    if not p.exists():
        return True
    try:
        shutil.rmtree(p, ignore_errors=False)
        return True
    except Exception as exc:
        print(f"[log_center.helpers] rmtree failed for {p}: {exc}", file=sys.stderr)
        return False
