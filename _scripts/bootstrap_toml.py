from __future__ import annotations

import ast
from pathlib import Path
from typing import Any

try:
    import tomllib as _toml
except ModuleNotFoundError:
    try:
        import tomli as _toml
    except ModuleNotFoundError:
        _toml = None


def _strip_comment(value: str) -> str:
    quote = ""
    escaped = False
    for index, char in enumerate(value):
        if escaped:
            escaped = False
            continue
        if quote == '"' and char == "\\":
            escaped = True
            continue
        if char in {'"', "'"}:
            if quote == char:
                quote = ""
            elif not quote:
                quote = char
            continue
        if char == "#" and not quote:
            return value[:index]
    return value


def _parse_scalar(value: str) -> Any:
    normalized = _strip_comment(value).strip()
    if normalized == "true":
        return True
    if normalized == "false":
        return False
    try:
        return ast.literal_eval(normalized)
    except (SyntaxError, ValueError) as error:
        raise ValueError(f"unsupported bootstrap TOML value: {normalized}") from error


def _fallback_table_value(path: Path, table: str, key: str) -> Any:
    current_table = ""
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if line.startswith("[") and line.endswith("]"):
            current_table = line[1:-1].strip() if not line.startswith("[[") else ""
            continue
        if current_table != table or "=" not in line:
            continue
        candidate, value = line.split("=", 1)
        if candidate.strip() == key:
            return _parse_scalar(value)
    raise KeyError(f"missing TOML value {table}.{key} in {path}")


def table_value(path: Path, table: str, key: str) -> Any:
    if _toml is None:
        return _fallback_table_value(path, table, key)

    with path.open("rb") as source:
        value: Any = _toml.load(source)
    for part in table.split(".") if table else ():
        value = value[part]
    for part in key.split("."):
        value = value[part]
    return value


def dotted_value(path: Path, dotted_path: str) -> Any:
    if _toml is not None:
        with path.open("rb") as source:
            value: Any = _toml.load(source)
        for part in dotted_path.split("."):
            value = value[part]
        return value

    parts = dotted_path.split(".")
    for split_at in range(len(parts) - 1, -1, -1):
        table = ".".join(parts[:split_at])
        key = ".".join(parts[split_at:])
        try:
            return _fallback_table_value(path, table, key)
        except KeyError:
            continue
    raise KeyError(f"missing TOML value {dotted_path} in {path}")
