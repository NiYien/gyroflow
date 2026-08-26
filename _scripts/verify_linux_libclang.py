#!/usr/bin/env python3
"""Validate the exact libclang shared library used by Linux Cargo builds."""

import argparse
import ctypes
import re
import sys
from pathlib import Path


REQUIRED_SYMBOL = "clang_Cursor_isInlineNamespace"


class CXString(ctypes.Structure):
    _fields_ = [("data", ctypes.c_void_p), ("private_flags", ctypes.c_uint)]


def validate_version(version: str, minimum_major: int) -> int:
    match = re.search(r"\bversion\s+(\d+)(?:\.\d+)", version, re.IGNORECASE)
    if not match:
        raise ValueError(f"Unable to parse the libclang version: {version!r}")

    major = int(match.group(1))
    if major < minimum_major:
        raise ValueError(
            f"The OpenCV binding generator requires libclang {minimum_major} or newer; "
            f"loaded version {major} from {version!r}"
        )
    return major


def _read_version(library: ctypes.CDLL) -> str:
    library.clang_getClangVersion.argtypes = []
    library.clang_getClangVersion.restype = CXString
    library.clang_getCString.argtypes = [CXString]
    library.clang_getCString.restype = ctypes.c_char_p
    library.clang_disposeString.argtypes = [CXString]
    library.clang_disposeString.restype = None

    value = library.clang_getClangVersion()
    try:
        raw = library.clang_getCString(value)
        if raw is None:
            raise ValueError("libclang returned an empty version string")
        return raw.decode("utf-8", errors="replace")
    finally:
        library.clang_disposeString(value)


def probe_libclang(lib_dir: Path, minimum_major: int) -> str:
    library_path = lib_dir / "libclang.so"
    if not library_path.is_file():
        raise FileNotFoundError(f"Expected libclang shared library: {library_path}")

    try:
        library = ctypes.CDLL(str(library_path))
    except OSError as error:
        raise RuntimeError(f"Unable to load {library_path}: {error}") from error

    version = _read_version(library)
    validate_version(version, minimum_major)

    try:
        getattr(library, REQUIRED_SYMBOL)
    except AttributeError as error:
        raise RuntimeError(
            f"{library_path} does not export required symbol {REQUIRED_SYMBOL}"
        ) from error
    return version


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lib_dir", type=Path)
    parser.add_argument("--minimum-major", type=int, default=9)
    args = parser.parse_args()

    try:
        version = probe_libclang(args.lib_dir, args.minimum_major)
    except (FileNotFoundError, RuntimeError, ValueError) as error:
        print(f"libclang validation failed: {error}", file=sys.stderr)
        return 1

    print(f"Using {version} from {args.lib_dir / 'libclang.so'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
