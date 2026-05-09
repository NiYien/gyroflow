"""Config file (control_center.config.json) read/write.

Location: parent directory of this package, same place as the old script used.
"""

from __future__ import annotations

import json
from pathlib import Path

# control_center.config.json lives inside distribution/control_center/
# alongside the frontend/ and backend/ directories.
_PACKAGE_DIR = Path(__file__).resolve().parent.parent  # distribution/control_center/
CONFIG_FILE = _PACKAGE_DIR / "control_center.config.json"

DEFAULT_CONFIG: dict = {
    "vercel_token": "",
    "vercel_project_id_or_name": "",
    "vercel_team_id": "",
    "github_token": "",
    "github_owner": "NiYien",
    "github_repo": "gyroflow",
    "telemetry_base_url": "https://www.niyien.com",
    "telemetry_stats_token": "",
    "telemetry_rebuild_token": "",
    "deploy_hook_url": "",
    "distribution_config_path": "distribution/niyien.toml",
    "lens_data_owner": "NiYien",
    "lens_data_repo": "niyien-lens-data",
    "plugins_owner": "NiYien",
    "plugins_repo": "gyroflow-plugins",
    "pan123_client_id": "",
    "pan123_client_secret": "",
    "pan123_releases_root_id": "",
    "network_proxy": "127.0.0.1:6063",
    "git_remote": "origin",
    "publish_defaults": {
        "lens_data_tag": "",
        "plugins_source_mode": "release",
        "plugins_tag": "",
        "plugins_artifact_name": "",
        "sdk_base": "https://www.niyien.com/api/sdk/",
    },
}


def load_config() -> dict:
    """Read config.json, merged on top of DEFAULT_CONFIG so new keys get defaults."""
    merged = dict(DEFAULT_CONFIG)
    if CONFIG_FILE.exists():
        try:
            raw = json.loads(CONFIG_FILE.read_text(encoding="utf-8"))
            if isinstance(raw, dict):
                merged.update(raw)
        except Exception:
            # Fall back to defaults on malformed JSON; caller can detect via comparison.
            pass
    return merged


def save_config(cfg: dict) -> None:
    CONFIG_FILE.write_text(
        json.dumps(cfg, indent=2, ensure_ascii=False),
        encoding="utf-8",
    )
