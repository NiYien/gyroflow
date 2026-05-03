"""Config loader for log_center.config.json.

Schema is defined inline (no pydantic dependency required) but documented in
the spec under `feedback-management-tool/spec.md` and the design doc D2.

Required structure:

    {
      "niyien_api_base": "https://niyien.com/api",
      "feedback_admin_token": "<32+ byte hex>",
      "pan123": {
        "client_id": "...",
        "client_secret": "...",
        "feedback_root_dir": "/feedback"
      },
      "r2": {
        "account_id": "...",
        "access_key_id": "...",
        "secret_access_key": "...",
        "bucket": "gyroflow-feedback"
      },
      "upstash_kv": {
        "url": "https://....upstash.io",
        "token": "..."
      },
      "local_cache_dir": "_cache/feedback"
    }

Missing required fields raise ConfigError pointing at log_center.example.json.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

# log_center.config.json lives inside distribution/log_center/ alongside
# the frontend/ and backend/ directories.
_PACKAGE_DIR = Path(__file__).resolve().parent.parent
CONFIG_FILE = _PACKAGE_DIR / "log_center.config.json"
EXAMPLE_FILE = _PACKAGE_DIR / "log_center.example.json"
DEFAULT_LOCAL_CACHE_DIR = "_cache/feedback"


class ConfigError(RuntimeError):
    """Raised on invalid / missing log_center.config.json."""


@dataclass
class Pan123Config:
    client_id: str
    client_secret: str
    feedback_root_dir: str = "/feedback"


@dataclass
class R2Config:
    account_id: str
    access_key_id: str
    secret_access_key: str
    bucket: str = "gyroflow-feedback"


@dataclass
class UpstashKvConfig:
    url: str
    token: str


@dataclass
class Config:
    niyien_api_base: str
    feedback_admin_token: str
    pan123: Pan123Config
    r2: R2Config
    upstash_kv: UpstashKvConfig
    local_cache_dir: str = DEFAULT_LOCAL_CACHE_DIR
    # Resolved absolute path of local_cache_dir (filled by load_config).
    cache_root: Path = field(default_factory=Path)
    # Full path to the loaded config file (for diagnostics).
    config_path: Path = field(default_factory=Path)


def _require(d: dict, key: str, ctx: str) -> Any:
    if key not in d or d[key] in ("", None):
        raise ConfigError(
            f"Missing required field '{ctx}.{key}' in log_center.config.json; "
            f"see {EXAMPLE_FILE.name} for a template"
        )
    return d[key]


def _section(d: dict, key: str) -> dict:
    section = d.get(key)
    if not isinstance(section, dict):
        raise ConfigError(
            f"Missing required section '{key}' in log_center.config.json; "
            f"see {EXAMPLE_FILE.name} for the expected shape"
        )
    return section


def load_config(path: Path | str | None = None) -> Config:
    """Read log_center.config.json, validate required fields, return Config.

    Raises ConfigError on any structural or value problem so the caller can
    surface a single friendly message and exit non-zero.
    """
    cfg_path = Path(path) if path is not None else CONFIG_FILE
    if not cfg_path.exists():
        raise ConfigError(
            f"Config file not found at {cfg_path}. "
            f"Copy {EXAMPLE_FILE.name} to log_center.config.json and fill it in."
        )
    try:
        raw = json.loads(cfg_path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise ConfigError(
            f"log_center.config.json is not valid JSON ({exc.msg} at line {exc.lineno})"
        ) from exc
    if not isinstance(raw, dict):
        raise ConfigError("log_center.config.json must contain a JSON object at the top level")

    pan123_d = _section(raw, "pan123")
    r2_d = _section(raw, "r2")
    kv_d = _section(raw, "upstash_kv")

    cfg = Config(
        niyien_api_base=str(_require(raw, "niyien_api_base", "")).rstrip("/"),
        feedback_admin_token=str(_require(raw, "feedback_admin_token", "")),
        pan123=Pan123Config(
            client_id=str(_require(pan123_d, "client_id", "pan123")),
            client_secret=str(_require(pan123_d, "client_secret", "pan123")),
            feedback_root_dir=str(pan123_d.get("feedback_root_dir") or "/feedback"),
        ),
        r2=R2Config(
            account_id=str(_require(r2_d, "account_id", "r2")),
            access_key_id=str(_require(r2_d, "access_key_id", "r2")),
            secret_access_key=str(_require(r2_d, "secret_access_key", "r2")),
            bucket=str(r2_d.get("bucket") or "gyroflow-feedback"),
        ),
        upstash_kv=UpstashKvConfig(
            url=str(_require(kv_d, "url", "upstash_kv")).rstrip("/"),
            token=str(_require(kv_d, "token", "upstash_kv")),
        ),
        local_cache_dir=str(raw.get("local_cache_dir") or DEFAULT_LOCAL_CACHE_DIR),
        config_path=cfg_path,
    )
    # Resolve cache_root relative to the package dir so a relative path in
    # the config still lands inside distribution/log_center/_cache/...
    cache = Path(cfg.local_cache_dir)
    cfg.cache_root = cache if cache.is_absolute() else (_PACKAGE_DIR / cache).resolve()
    cfg.cache_root.mkdir(parents=True, exist_ok=True)
    return cfg
