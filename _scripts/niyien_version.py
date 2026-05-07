from __future__ import annotations

import argparse
import os
import pathlib
import tomllib


def cargo_version(repo_root: pathlib.Path) -> str:
    payload = tomllib.loads((repo_root / "Cargo.toml").read_text(encoding="utf-8"))
    package = payload.get("package", {})
    version = package.get("version")
    if isinstance(version, str) and version.strip():
        return version.strip()

    workspace_ref = package.get("version.workspace")
    if workspace_ref is True:
        workspace_package = payload.get("workspace", {}).get("package", {})
        workspace_version = workspace_package.get("version")
        if isinstance(workspace_version, str) and workspace_version.strip():
            return workspace_version.strip()

    if isinstance(package.get("version"), dict) and package["version"].get("workspace") is True:
        workspace_package = payload.get("workspace", {}).get("package", {})
        workspace_version = workspace_package.get("version")
        if isinstance(workspace_version, str) and workspace_version.strip():
            return workspace_version.strip()

    raise RuntimeError(f"unable to resolve package version from {repo_root / 'Cargo.toml'}")


def tag_version() -> str | None:
    ref = os.environ.get("GITHUB_REF", "").strip()
    prefix = "refs/tags/"
    if not ref.startswith(prefix):
        return None
    tag = ref[len(prefix) :].strip()
    if not tag:
        return None
    return tag[1:] if tag.startswith("v") else tag


def numeric_core(version: str) -> str:
    core = version.split("-", 1)[0].split("+", 1)[0].strip()
    parts: list[str] = []
    for part in core.split("."):
        digits = []
        for ch in part:
            if ch.isdigit():
                digits.append(ch)
            else:
                break
        if digits:
            parts.append("".join(digits))
    while len(parts) < 3:
        parts.append("0")
    return ".".join(parts[:3]) if parts else "0.0.0"


def padded_run_number(run_number: int) -> str:
    return f"{run_number:03}" if run_number < 1000 else str(run_number)


def resolve_version_info(repo_root: pathlib.Path) -> dict[str, str]:
    base = cargo_version(repo_root)
    tag = tag_version()
    run_number_raw = os.environ.get("GITHUB_RUN_NUMBER", "").strip()
    build_time = os.environ.get("BUILD_TIME", "1").strip() or "1"

    if tag:
        numeric = f"{numeric_core(tag)}.0"
        return {
            "kind": "release",
            "base": base,
            "canonical": tag,
            "display": f"{tag}(ni)",
            "numeric_core": numeric_core(tag),
            "file": numeric,
            "sequence": "0",
        }

    if run_number_raw.isdigit():
        run_number = int(run_number_raw)
        numeric = f"{numeric_core(base)}.{run_number}"
        return {
            "kind": "action",
            "base": base,
            "canonical": f"{base}-ni.{run_number}",
            "display": f"{base}(ni{padded_run_number(run_number)})",
            "numeric_core": numeric_core(base),
            "file": numeric,
            "sequence": str(run_number),
        }

    return {
        "kind": "dev",
        "base": base,
        "canonical": f"{base}-dev.{build_time}",
        "display": f"{base}(dev{build_time})",
        "numeric_core": numeric_core(base),
        "file": f"{numeric_core(base)}.1",
        "sequence": build_time,
    }


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("field")
    parser.add_argument(
        "--repo-root",
        default=str(pathlib.Path(__file__).resolve().parents[1]),
    )
    args = parser.parse_args()

    info = resolve_version_info(pathlib.Path(args.repo_root).resolve())
    value = info.get(args.field.strip())
    if value is None:
        raise SystemExit(f"unknown field: {args.field}")
    print(value)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
