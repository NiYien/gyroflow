#!/usr/bin/env python3
import argparse
import gzip
import hashlib
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib


ROOT = Path(__file__).resolve().parent.parent
CONFIG_PATH = ROOT / "distribution" / "niyien.toml"
OUTPUT_DIR = ROOT / "_deployment" / "_binaries"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("package_key", choices=["lens"])
    parser.add_argument("--version", type=int, default=int(datetime.now(tz=timezone.utc).strftime("%Y%m%d%H%M%S")))
    return parser.parse_args()


def load_config() -> dict:
    with CONFIG_PATH.open("rb") as fh:
        return tomllib.load(fh)


def encode_cbor(value):
    if isinstance(value, dict):
        items = sorted(value.items(), key=lambda item: item[0])
        return encode_major(5, len(items)) + b"".join(encode_cbor(k) + encode_cbor(v) for k, v in items)
    if isinstance(value, list):
        return encode_major(4, len(value)) + b"".join(encode_cbor(item) for item in value)
    if isinstance(value, bytes):
        return encode_major(2, len(value)) + value
    if isinstance(value, str):
        raw = value.encode("utf-8")
        return encode_major(3, len(raw)) + raw
    if isinstance(value, int):
        if value >= 0:
            return encode_major(0, value)
        return encode_major(1, -1 - value)
    raise TypeError(f"Unsupported CBOR type: {type(value)!r}")


def encode_major(major: int, value: int) -> bytes:
    if value < 24:
        return bytes([(major << 5) | value])
    if value < 256:
        return bytes([(major << 5) | 24, value])
    if value < 65536:
        return bytes([(major << 5) | 25]) + value.to_bytes(2, "big")
    if value < 4294967296:
        return bytes([(major << 5) | 26]) + value.to_bytes(4, "big")
    return bytes([(major << 5) | 27]) + value.to_bytes(8, "big")


def collect_files(source_dir: Path) -> dict[str, bytes]:
    files: dict[str, bytes] = {}
    if not source_dir.exists():
        return files
    for path in sorted(source_dir.rglob("*")):
        if not path.is_file():
            continue
        if path.name.lower() == "readme.md":
            continue
        rel = path.relative_to(source_dir).as_posix()
        files[rel] = path.read_bytes()
    return files


def main() -> int:
    args = parse_args()
    config = load_config()
    package_config = config["data"][args.package_key]
    source_dir = ROOT / package_config["source_dir"]
    output_name = package_config["asset_name"]
    files = collect_files(source_dir)

    bundle = {
        "__version": args.version,
        "__generated_at": datetime.now(tz=timezone.utc).isoformat(),
        "__package": args.package_key,
        "files": files,
    }
    encoded = encode_cbor(bundle)
    compressed = gzip.compress(encoded, compresslevel=9)

    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    output_path = OUTPUT_DIR / output_name
    output_path.write_bytes(compressed)

    metadata = {
        "package": args.package_key,
        "version": args.version,
        "asset_name": output_name,
        "size": len(compressed),
        "sha256": hashlib.sha256(compressed).hexdigest(),
        "source_dir": source_dir.as_posix(),
        "file_count": len(files),
    }
    (OUTPUT_DIR / f"{output_name}.json").write_text(json.dumps(metadata, indent=2), encoding="utf-8")
    print(json.dumps(metadata))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
