"""Api class — methods exposed to JS via pywebview's `js_api` bridge.

Every method returns a dict with an `ok: bool` key plus data or `error: str`
so the JS side can handle success and failure uniformly without relying on
exception plumbing through the bridge.
"""

from __future__ import annotations

import importlib.util as _impl_util
import os
import re
import subprocess
import sys
import traceback
from datetime import datetime
from pathlib import Path

from . import cargo as cargo_ops
from . import config as config_module
from . import git as git_ops
from . import telemetry as telemetry_api
from .github import GitHubClient
from .helpers import mask_sensitive, normalize_version
from .pan123 import TASKS, Pan123Client, run_publish_subprocess, load_bundle_cache, save_bundle_cache
from .vercel import VercelClient, parse_policy_from_envs

# GitHub Action workflow that does release builds
APP_BUILD_WORKFLOW_FILE = "release.yml"
REPO_ROOT = Path(__file__).resolve().parents[3]  # gyroflow/

# Action types that should auto-trigger pan123 publishing after policy push.
PAN123_AUTO_PUBLISH_ACTIONS = {"publish_and_push", "switch_auto", "rollback_auto"}

# Asset names expected per release tag — app bundle (the gyroflow installers
# only). Lens/plugin/sdk live under separate `lens-<sha12>/`, `plugin-<sha12>/`,
# `sdk/` directories on 123 网盘, listed independently below.
FALLBACK_EXPECTED_APP_ASSETS = (
    "gyroflow-niyien-windows64-setup.exe",
    "gyroflow-niyien-windows64.zip",
    "gyroflow-niyien-mac-universal.dmg",
)
EXPECTED_APP_ASSETS = FALLBACK_EXPECTED_APP_ASSETS


def _load_publish_script_constants():
    """Load `_scripts/publish_pan123_release.py` as an importable module so
    we can read its `SDK_FILENAMES` / `PLUGIN_ASSET_NAMES` tuples without
    duplicating them. The publish script is the source of truth — those
    lists grow as new SDK / plugin filenames ship, and this dashboard probe
    must track those changes automatically (no hardcoded mirror in api.py).

    `_scripts/` is not a Python package, so plain `from ... import ...`
    won't work; we load by file path via importlib. The publish script is
    safe to import: module-level only imports stdlib + requests (no env
    vars / network at import time), and `if __name__ == "__main__"` (line
    1895) guards the CLI entrypoint.
    """
    pub_path = REPO_ROOT / "_scripts" / "publish_pan123_release.py"
    spec = _impl_util.spec_from_file_location(
        "publish_pan123_release_static", pub_path
    )
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Cannot prepare module spec for {pub_path}")
    module = _impl_util.module_from_spec(spec)
    # Register in sys.modules BEFORE exec_module — the publish script uses
    # @dataclass, and dataclass introspection internally calls
    # `sys.modules.get(cls.__module__).__dict__`. If the freshly-built
    # module hasn't been registered yet, that lookup returns None and the
    # @dataclass decorator dies with `AttributeError: 'NoneType' object
    # has no attribute '__dict__'`.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


try:
    _PUB_MODULE = _load_publish_script_constants()
    # REQUIRED_APP_ASSET_REMOTE_NAMES is the inventory-comparison source: it
    # rewrites wrapped extensions (.exe / .apk → nightly-style short-name zip)
    # so the directory listing on 123 disk matches. REQUIRED_APP_ASSET_NAMES
    # (raw build artifact names) is still used by the publish pipeline for
    # local-file checks; both come from the same single source of truth.
    EXPECTED_APP_ASSETS = tuple(_PUB_MODULE.REQUIRED_APP_ASSET_REMOTE_NAMES)
    EXPECTED_SDK_ASSETS = tuple(_PUB_MODULE.SDK_FILENAMES)
    EXPECTED_PLUGIN_ASSETS = tuple(_PUB_MODULE.PLUGIN_ASSET_NAMES)
    # Decoupled bundle filename lists (include the per-bundle manifest).
    EXPECTED_LENS_FILENAMES = tuple(_PUB_MODULE.EXPECTED_LENS_FILENAMES)
    EXPECTED_PLUGIN_FILENAMES = tuple(_PUB_MODULE.EXPECTED_PLUGIN_FILENAMES)
    LENS_ASSET_NAME = str(_PUB_MODULE.LENS_ASSET_NAME)
    LENS_METADATA_ASSET_NAME = str(_PUB_MODULE.LENS_METADATA_ASSET_NAME)
    LENS_MANIFEST_ASSET_NAME = str(_PUB_MODULE.LENS_MANIFEST_ASSET_NAME)
    PLUGIN_MANIFEST_ASSET_NAME = str(_PUB_MODULE.PLUGIN_MANIFEST_ASSET_NAME)
    _PUB_LOAD_ERROR: str | None = None
except Exception as _e:
    # Keep the dashboard usable if the publish script ever becomes
    # unimportable (e.g. a syntax error in a future edit). The probes
    # below detect empty constant tuples and report "publish 脚本未加载"
    # instead of crashing the whole get_dashboard_state call.
    EXPECTED_APP_ASSETS = FALLBACK_EXPECTED_APP_ASSETS
    EXPECTED_SDK_ASSETS = ()
    EXPECTED_PLUGIN_ASSETS = ()
    EXPECTED_LENS_FILENAMES = ()
    EXPECTED_PLUGIN_FILENAMES = ()
    LENS_ASSET_NAME = "gyroflow-niyien-lens.cbor.gz"
    LENS_METADATA_ASSET_NAME = "gyroflow-niyien-lens.cbor.gz.json"
    LENS_MANIFEST_ASSET_NAME = "gyroflow-niyien-lens-manifest.json"
    PLUGIN_MANIFEST_ASSET_NAME = "gyroflow-niyien-plugin-manifest.json"
    _PUB_LOAD_ERROR = f"{_e.__class__.__name__}: {_e}"


def _error(exc: Exception, context: str) -> dict:
    return {
        "ok": False,
        "error": f"{context}: {exc}",
        "type": exc.__class__.__name__,
    }


def _bump_cargo_and_commit_if_needed(
    workdir: Path, remote: str, target_version: str
) -> str | None:
    """Sync `[workspace.package].version` in `workdir/Cargo.toml` to `target_version`,
    then commit + push if a change was needed.

    Used right before `git tag` so the build that the tag triggers embeds the
    correct PE FileVersion (build.rs reads CARGO_PKG_VERSION). Without this
    sync, a stale Cargo.toml means the released binary's internal version
    does not match the git tag (see v2.1.2 plugin incident, 2026-04-25).

    Returns:
      None — if Cargo.toml is missing, not a Rust workspace, or already at target.
      "<old> -> <new>" — when a bump + commit + push completed successfully.

    Raises subprocess.CalledProcessError on git failure (caller wraps into _error).
    Raises RuntimeError when the version line cannot be located in Cargo.toml.
    """
    cargo_path = workdir / "Cargo.toml"
    current = cargo_ops.read_workspace_version(cargo_path)
    if current is None or current == target_version:
        return None
    if not cargo_ops.write_workspace_version(cargo_path, target_version):
        raise RuntimeError(
            f"Failed to rewrite [workspace.package].version in {cargo_path!s} "
            f"(current={current!r}, target={target_version!r}) — pattern not matched"
        )
    # File is now dirty on disk. If anything below fails before the commit lands,
    # we must `git checkout -- Cargo.toml` to roll back; otherwise a retry would
    # see the file already at target_version and skip the bump path entirely,
    # leaving an unpushed local commit + a tag referencing it (orphan tag risk).
    cargo_dirty = True
    try:
        branch = git_ops.get_current_branch(workdir)
        if not branch:
            raise RuntimeError(
                f"Cannot determine current branch in {workdir!s} (detached HEAD?). "
                f"Cargo.toml has been edited — please resolve manually."
            )
        # `commit -- Cargo.toml` commits this path's current state without
        # depending on the staging area, so any unrelated `git add`-ed files
        # the user has staged elsewhere are NOT swept into this version-bump
        # commit. Once it returns 0 the working tree is clean for Cargo.toml.
        git_ops.run_git(
            workdir,
            "commit",
            "-m",
            f"chore: bump workspace version to {target_version}",
            "--",
            "Cargo.toml",
        )
        cargo_dirty = False
        git_ops.run_git(workdir, "push", remote, branch)
    except (subprocess.CalledProcessError, RuntimeError):
        # Best-effort rollback so the next retry restarts from a clean state.
        # If the commit succeeded but `push` failed (cargo_dirty is False at
        # that point), we deliberately leave the local commit in place — the
        # user must `git push` manually or `git reset --hard HEAD~1` to undo.
        if cargo_dirty:
            try:
                git_ops.run_git(workdir, "checkout", "--", "Cargo.toml")
            except Exception:
                # Surface the original error; rollback failure is best-effort.
                pass
        raise
    return f"{current} -> {target_version}"


class Api:
    """Exposed to JS as `window.pywebview.api.<method_name>()`."""

    # ---- basic ----

    def ping(self) -> dict:
        return {
            "ok": True,
            "message": "pong",
            "python": sys.version.split()[0],
            "timestamp": datetime.now().isoformat(timespec="seconds"),
        }

    # ---- config ----

    def get_config(self) -> dict:
        try:
            cfg = config_module.load_config()
            return {
                "ok": True,
                "path": str(config_module.CONFIG_FILE),
                "config": mask_sensitive(cfg),
            }
        except Exception as e:
            return _error(e, "get_config")

    def save_config(self, partial: dict) -> dict:
        """Merge `partial` into current config and write. Supports one level
        of dotted key like `publish_defaults.sdk_base`.
        """
        try:
            cfg = config_module.load_config()
            for key, value in (partial or {}).items():
                if "." in key:
                    top, sub = key.split(".", 1)
                    if not isinstance(cfg.get(top), dict):
                        cfg[top] = {}
                    cfg[top][sub] = value
                else:
                    cfg[key] = value
            config_module.save_config(cfg)
            return {"ok": True, "path": str(config_module.CONFIG_FILE)}
        except Exception as e:
            return _error(e, "save_config")

    # Keys shown as read-only constants in the Settings view
    _READ_ONLY_CONSTANT_KEYS = (
        "github_owner", "github_repo",
        "lens_data_owner", "lens_data_repo",
        "plugins_owner", "plugins_repo",
        "telemetry_base_url", "distribution_config_path",
    )

    def get_config_for_edit(self) -> dict:
        """Full config for the settings view. Sensitive values returned as-is
        so the UI can populate password fields (they stay masked in the input
        type=password element). No mask_sensitive here.
        """
        try:
            cfg = config_module.load_config()
            constants = {k: cfg.get(k, "") for k in self._READ_ONLY_CONSTANT_KEYS}
            return {"ok": True, "config": cfg, "constants": constants, "path": str(config_module.CONFIG_FILE)}
        except Exception as e:
            return _error(e, "get_config_for_edit")

    # ---- client factories ----

    def _vercel(self, cfg: dict | None = None) -> VercelClient:
        cfg = cfg if cfg is not None else config_module.load_config()
        return VercelClient(
            token=cfg.get("vercel_token", ""),
            project=cfg.get("vercel_project_id_or_name", ""),
            team_id=cfg.get("vercel_team_id", ""),
            proxy_url=cfg.get("network_proxy", ""),
        )

    def _github(self, cfg: dict | None = None) -> GitHubClient:
        cfg = cfg if cfg is not None else config_module.load_config()
        return GitHubClient(
            owner=cfg.get("github_owner", ""),
            repo=cfg.get("github_repo", ""),
            token=cfg.get("github_token", ""),
            proxy_url=cfg.get("network_proxy", ""),
        )

    # ---- dashboard ----

    def get_dashboard_state(self) -> dict:
        """Aggregated dashboard payload: 4 component versions + recent releases.

        Errors in individual sections are isolated — e.g. if GitHub is down
        we still return Vercel-sourced info.
        """
        cfg = config_module.load_config()
        state: dict = {
            "ok": True,
            "app": None,
            "lens": None,
            "plugin": None,
            "sdk": None,
            "recent_releases": [],
            "errors": {},
        }

        # --- Vercel envs → policy + lens + plugin + sdk ---
        # list_envs_decrypted() transparently falls back to single-env endpoint
        # for `type=encrypted` records (list endpoint leaves them wrapped as
        # `{"v":"v2","c":"..."}` even with ?decrypt=true).
        envs: dict = {}
        try:
            vercel = self._vercel(cfg)
            envs = vercel.list_envs_decrypted()
        except Exception as e:
            state["errors"]["vercel"] = f"{e.__class__.__name__}: {e}"

        defaults = (cfg.get("publish_defaults") or {})

        if envs:
            policy = parse_policy_from_envs(envs)
            if policy.get("encrypted"):
                state["errors"]["policy"] = (
                    "NIYIEN_RELEASE_POLICY_JSON 仍是加密态 — 检查 Vercel token scope"
                )
            # Current pushed app version = policy.auto_version if set, else 1st version in list
            auto_version = (policy.get("auto_version") or "").strip()
            versions = policy.get("versions") or []
            current_app = None
            if auto_version:
                current_app = next(
                    (v for v in versions if str(v.get("version", "")).strip() == auto_version),
                    None,
                )
            if current_app is None and versions:
                current_app = versions[0]
            if current_app:
                state["app"] = {
                    "version": current_app.get("version", ""),
                    "tag": current_app.get("tag", ""),
                    "recommended": bool(current_app.get("recommended", False)),
                    "is_auto_pushed": bool(auto_version and current_app.get("version") == auto_version),
                    "source": "vercel",
                    "missing_from_github": False,
                }

        # Lens/Plugin/SDK: fallback to publish_defaults if Vercel envs missing
        lens_tag_env = str(envs.get("NIYIEN_LENS_DATA_TAG", "")).strip()
        lens_tag = lens_tag_env or str(defaults.get("lens_data_tag", "")).strip()
        # NIYIEN_LENS_RELEASE_TAG = the bundle directory name on 123 网盘
        # (`lens-<sha12>/`). Set by the publish script's finalize_summary.
        lens_release_tag = str(envs.get("NIYIEN_LENS_RELEASE_TAG", "")).strip()
        state["lens"] = {
            "tag": lens_tag,
            "version": str(envs.get("NIYIEN_LENS_VERSION", "")).strip(),
            "source": "vercel" if lens_tag_env else ("defaults" if lens_tag else "none"),
            # Pan123 probe results (parallel to plugin / sdk probes).
            "missing_files": None,
            "pan123_error": None,
            "release_tag": lens_release_tag,
            "expected_count": len(EXPECTED_LENS_FILENAMES),
        }

        plugin_mode_env = str(envs.get("NIYIEN_PLUGINS_SOURCE_MODE", "")).strip().lower()
        plugin_mode = plugin_mode_env or str(defaults.get("plugins_source_mode", "release")).strip().lower() or "release"
        plugin_tag_env = str(envs.get("NIYIEN_PLUGINS_TAG", "")).strip()
        plugin_tag = plugin_tag_env or str(defaults.get("plugins_tag", "")).strip()
        plugin_artifact_env = str(envs.get("NIYIEN_PLUGINS_ARTIFACT_NAME", "")).strip()
        plugin_artifact = plugin_artifact_env or str(defaults.get("plugins_artifact_name", "")).strip()
        has_plugin_env = bool(plugin_mode_env or plugin_tag_env or plugin_artifact_env)
        # NIYIEN_PLUGIN_RELEASE_TAG = the bundle directory name on 123 网盘
        # (`plugin-<sha12>/`). Set by the publish script's finalize_summary.
        plugin_release_tag = str(envs.get("NIYIEN_PLUGIN_RELEASE_TAG", "")).strip()
        state["plugin"] = {
            "mode": plugin_mode,
            "tag": plugin_tag,
            "artifact_name": plugin_artifact,
            "source": "vercel" if has_plugin_env else ("defaults" if (plugin_tag or plugin_artifact or plugin_mode_env) else "none"),
            # Filled below by the plugin pan123 probe (parallel to the SDK probe).
            # None = probe didn't run; [] = all expected files present;
            # non-empty list = filenames missing from `releases/plugin-<x>/`.
            "missing_files": None,
            "pan123_error": None,
            "release_tag": plugin_release_tag,
            "expected_count": len(EXPECTED_PLUGIN_ASSETS),
        }

        sdk_base_env = str(envs.get("NIYIEN_SDK_BASE", "")).strip()
        sdk_base = sdk_base_env or str(defaults.get("sdk_base", "")).strip()
        state["sdk"] = {
            "base": sdk_base,
            "source": "vercel" if sdk_base_env else ("defaults" if sdk_base else "none"),
            # Filled below by the pan123 probe. None = probe didn't run (no creds
            # / network error); [] = all expected SDK files present; non-empty
            # list = filenames missing from `releases/sdk/` on pan123.
            "missing_files": None,
            "pan123_error": None,
            "expected_count": len(EXPECTED_SDK_ASSETS),
        }

        # --- pan123 probe: detect missing SDK assets in `releases/sdk/` ---
        # Cross-check against EXPECTED_SDK_ASSETS (sourced live from the publish
        # script — see _load_publish_script_constants). Failure here is isolated
        # — sdk metadata above stays usable even if the probe blows up (no
        # creds, network down, pan123 down, publish script unimportable).
        try:
            if not EXPECTED_SDK_ASSETS:
                state["sdk"]["pan123_error"] = (
                    f"publish 脚本未加载 (跳过 SDK 探测): {_PUB_LOAD_ERROR}"
                )
            else:
                cid = self._get_publish_secret("PAN123_CLIENT_ID", vercel_envs=envs, cfg=cfg)
                csec = self._get_publish_secret("PAN123_CLIENT_SECRET", vercel_envs=envs, cfg=cfg)
                root_id = self._pan123_releases_root_id(cfg=cfg, vercel_envs=envs)
                if cid and csec and root_id:
                    client = Pan123Client(
                        cid, csec, proxy_url=str(cfg.get("network_proxy", "") or "")
                    )
                    sdk_entry = client.find_child(root_id, "sdk", is_dir=True)
                    if not sdk_entry:
                        # No `sdk/` directory at all — every expected file is missing.
                        state["sdk"]["missing_files"] = list(EXPECTED_SDK_ASSETS)
                        state["sdk"]["pan123_error"] = "pan123 上 releases/sdk/ 目录不存在"
                    else:
                        sdk_dir_id = Pan123Client._entry_id(sdk_entry)
                        files = client.list_directory(sdk_dir_id)
                        present = {
                            str(item.get("filename") or item.get("name") or "").strip()
                            for item in files
                        }
                        state["sdk"]["missing_files"] = [
                            name for name in EXPECTED_SDK_ASSETS if name not in present
                        ]
                else:
                    state["sdk"]["pan123_error"] = "pan123 凭据未配置 (无法探测 SDK)"
        except Exception as e:
            state["sdk"]["pan123_error"] = f"{e.__class__.__name__}: {e}"

        # --- pan123 probe: lens bundle in `releases/<lens_release_tag>/` ---
        try:
            cid = self._get_publish_secret("PAN123_CLIENT_ID", vercel_envs=envs, cfg=cfg)
            csec = self._get_publish_secret("PAN123_CLIENT_SECRET", vercel_envs=envs, cfg=cfg)
            root_id = self._pan123_releases_root_id(cfg=cfg, vercel_envs=envs)
            if not (cid and csec and root_id):
                state["lens"]["pan123_error"] = "pan123 凭据未配置 (无法探测 lens)"
            elif lens_release_tag:
                client = Pan123Client(
                    cid, csec, proxy_url=str(cfg.get("network_proxy", "") or "")
                )
                lens_entry = client.find_child(root_id, lens_release_tag, is_dir=True)
                if not lens_entry:
                    expected = list(EXPECTED_LENS_FILENAMES) if EXPECTED_LENS_FILENAMES \
                        else [LENS_ASSET_NAME, LENS_METADATA_ASSET_NAME]
                    state["lens"]["missing_files"] = expected
                    state["lens"]["pan123_error"] = (
                        f"pan123 上 releases/{lens_release_tag}/ 目录不存在"
                    )
                else:
                    lens_dir_id = Pan123Client._entry_id(lens_entry)
                    files = client.list_directory(lens_dir_id)
                    present = {
                        str(item.get("filename") or item.get("name") or "").strip()
                        for item in files
                    }
                    expected = list(EXPECTED_LENS_FILENAMES) if EXPECTED_LENS_FILENAMES \
                        else [LENS_ASSET_NAME, LENS_METADATA_ASSET_NAME]
                    state["lens"]["missing_files"] = [
                        name for name in expected if name not in present
                    ]
            else:
                state["lens"]["pan123_error"] = (
                    "无 NIYIEN_LENS_RELEASE_TAG (运行一次「全量」发布以初始化)"
                )
        except Exception as e:
            state["lens"]["pan123_error"] = f"{e.__class__.__name__}: {e}"

        # --- pan123 probe: plugin bundle in `releases/<plugin_release_tag>/` ---
        # Flat layout (no `plugins/` subdir). EXPECTED_PLUGIN_ASSETS comes
        # from the publish script.
        try:
            if not EXPECTED_PLUGIN_ASSETS:
                state["plugin"]["pan123_error"] = (
                    f"publish 脚本未加载 (跳过 plugin 探测): {_PUB_LOAD_ERROR}"
                )
            else:
                cid = self._get_publish_secret("PAN123_CLIENT_ID", vercel_envs=envs, cfg=cfg)
                csec = self._get_publish_secret("PAN123_CLIENT_SECRET", vercel_envs=envs, cfg=cfg)
                root_id = self._pan123_releases_root_id(cfg=cfg, vercel_envs=envs)
                if not (cid and csec and root_id):
                    state["plugin"]["pan123_error"] = "pan123 凭据未配置 (无法探测 plugin)"
                elif plugin_release_tag:
                    client = Pan123Client(
                        cid, csec, proxy_url=str(cfg.get("network_proxy", "") or "")
                    )
                    plugin_entry = client.find_child(root_id, plugin_release_tag, is_dir=True)
                    if not plugin_entry:
                        state["plugin"]["missing_files"] = list(EXPECTED_PLUGIN_ASSETS)
                        state["plugin"]["pan123_error"] = (
                            f"pan123 上 releases/{plugin_release_tag}/ 目录不存在"
                        )
                    else:
                        plugin_dir_id = Pan123Client._entry_id(plugin_entry)
                        files = client.list_directory(plugin_dir_id)
                        present = {
                            str(item.get("filename") or item.get("name") or "").strip()
                            for item in files
                        }
                        state["plugin"]["missing_files"] = [
                            name for name in EXPECTED_PLUGIN_ASSETS
                            if name not in present
                        ]
                else:
                    state["plugin"]["pan123_error"] = (
                        "无 NIYIEN_PLUGIN_RELEASE_TAG (运行一次「全量」发布以初始化)"
                    )
        except Exception as e:
            state["plugin"]["pan123_error"] = f"{e.__class__.__name__}: {e}"

        # --- GitHub recent releases (top 3) + cross-check app.tag existence ---
        try:
            gh = self._github(cfg)
            releases = gh.list_releases()
            recent = []
            for r in releases[:3]:
                recent.append({
                    "tag": r.get("tag_name", ""),
                    "name": r.get("name", ""),
                    "published_at": r.get("published_at", ""),
                    "prerelease": bool(r.get("prerelease", False)),
                    "draft": bool(r.get("draft", False)),
                })
            state["recent_releases"] = recent
            # Cross-check: does the app's tag still exist on GitHub?
            #   run-<digits>  → artifact mode; check if the Action run itself still exists
            #   other tags    → release mode; check against releases list
            if state["app"]:
                app_tag = str(state["app"].get("tag", "")).strip()
                if app_tag:
                    m = re.fullmatch(r"run-(\d+)", app_tag)
                    if m:
                        try:
                            run = gh.get_workflow_run(run_id=int(m.group(1)))
                            state["app"]["missing_from_github"] = run is None
                        except Exception:
                            # Leave as False if probe fails — avoid false positives on transient errors
                            pass
                    else:
                        all_tag_names = {str(r.get("tag_name", "")).strip() for r in releases}
                        state["app"]["missing_from_github"] = app_tag not in all_tag_names
        except Exception as e:
            state["errors"]["github"] = f"{e.__class__.__name__}: {e}"

        # ---- Lens / Plugin: detect latest upstream release tag ----
        # Compare the latest GitHub release of each upstream repo against the
        # tag currently pushed via Vercel envs. UI shows a ⬆ banner so the
        # operator can opt-in to switch.
        state["updates_available"] = {}
        try:
            lens_owner = str(cfg.get("lens_data_owner", "")).strip()
            lens_repo = str(cfg.get("lens_data_repo", "")).strip()
            if lens_owner and lens_repo:
                lens_releases = self._gh_for(lens_owner, lens_repo, cfg).list_repo_releases(
                    lens_owner, lens_repo,
                )
                # Skip drafts/prereleases for the "is there a stable update?" check
                latest_lens = next(
                    (r for r in lens_releases
                     if not r.get("draft") and not r.get("prerelease")),
                    lens_releases[0] if lens_releases else None,
                )
                if latest_lens:
                    latest_tag = str(latest_lens.get("tag_name", "")).strip()
                    current_tag = str(state.get("lens", {}).get("tag", "")).strip()
                    if latest_tag and latest_tag != current_tag:
                        state["updates_available"]["lens"] = {
                            "latest_tag": latest_tag,
                            "current_tag": current_tag,
                            "published_at": str(latest_lens.get("published_at", "")),
                        }
        except Exception as e:
            state["errors"]["lens_update"] = f"{e.__class__.__name__}: {e}"

        try:
            # Plugin update detection + missing-from-github check. Both rely on
            # listing the upstream plugin repo's releases — artifact mode tracks
            # Action runs by id rather than release tag, so neither check is
            # meaningful in that mode.
            plugin_state = state.get("plugin") or {}
            plugin_state["missing_from_github"] = False
            if str(plugin_state.get("mode", "")).lower() == "release":
                pl_owner = str(cfg.get("plugins_owner", "")).strip()
                pl_repo = str(cfg.get("plugins_repo", "")).strip()
                if pl_owner and pl_repo:
                    pl_releases = self._gh_for(pl_owner, pl_repo, cfg).list_repo_releases(
                        pl_owner, pl_repo,
                    )
                    # Cross-check: is the currently-pushed plugin tag still on
                    # GitHub? (User may have manually deleted the release.)
                    current_tag = str(plugin_state.get("tag", "")).strip()
                    if current_tag:
                        all_tag_names = {
                            str(r.get("tag_name", "")).strip() for r in pl_releases
                        }
                        plugin_state["missing_from_github"] = (
                            current_tag not in all_tag_names
                        )
                    latest_pl = next(
                        (r for r in pl_releases
                         if not r.get("draft") and not r.get("prerelease")),
                        pl_releases[0] if pl_releases else None,
                    )
                    if latest_pl:
                        latest_tag = str(latest_pl.get("tag_name", "")).strip()
                        if latest_tag and latest_tag != current_tag:
                            state["updates_available"]["plugin"] = {
                                "latest_tag": latest_tag,
                                "current_tag": current_tag,
                                "published_at": str(latest_pl.get("published_at", "")),
                            }
        except Exception as e:
            state["errors"]["plugin_update"] = f"{e.__class__.__name__}: {e}"

        return state

    # ---- lower-level probes (kept for debugging from JS console) ----

    def get_current_policy(self) -> dict:
        try:
            envs = self._vercel().list_envs_decrypted()
            policy = parse_policy_from_envs(envs)
            return {"ok": True, "policy": policy}
        except Exception as e:
            return _error(e, "get_current_policy")

    def update_resource_field(self, field: str, value: str) -> dict:
        """Surgically update one Vercel env (lens / plugin / sdk) without
        touching the others.

        Used by the dashboard's "⬆ 切换到新版本" buttons. apply_resources_now()
        overwrites the full payload (and would clear plugin if you only meant
        to bump lens) — this method is the non-destructive alternative.
        """
        allowed = {
            "lens_tag": "NIYIEN_LENS_DATA_TAG",
            "plugin_tag": "NIYIEN_PLUGINS_TAG",
            "sdk_base": "NIYIEN_SDK_BASE",
        }
        env_name = allowed.get(str(field or "").strip())
        if not env_name:
            return {"ok": False, "error": f"未知字段: {field} (允许: {list(allowed.keys())})"}
        v = str(value or "").strip()
        if not v:
            return {"ok": False, "error": "value 不能为空"}
        try:
            self._vercel().upsert_envs({env_name: v})
            return {"ok": True, "field": field, "env_name": env_name, "value": v,
                    "message": f"已更新 {env_name} = {v}"}
        except Exception as e:
            return _error(e, "update_resource_field")

    def list_releases(self) -> dict:
        try:
            releases = self._github().list_releases()
            return {
                "ok": True,
                "releases": [
                    {
                        "tag": r.get("tag_name", ""),
                        "name": r.get("name", ""),
                        "body": r.get("body", ""),
                        "published_at": r.get("published_at", ""),
                        "prerelease": bool(r.get("prerelease", False)),
                        "draft": bool(r.get("draft", False)),
                    }
                    for r in releases
                ],
            }
        except Exception as e:
            return _error(e, "list_releases")

    def list_policy_orphan_versions(self) -> dict:
        """policy.versions[] entries whose `tag` no longer exists as a GitHub release.

        These are leftovers from releases that were deleted on GitHub but never
        pruned from the policy whitelist. Surface them so the user can run
        hide_version against them without needing them to appear in the
        live release list.
        """
        try:
            cfg = config_module.load_config()
            vercel = self._vercel(cfg)
            env_records = vercel.list_env_records()
            policy = self._load_current_policy(cfg, vercel, env_records)
            versions = policy.get("versions", []) or []
            auto_version = str(policy.get("auto_version", "") or "").strip()

            gh = self._github(cfg)
            releases = gh.list_releases()
            release_tags = {str(r.get("tag_name", "")).strip() for r in releases}

            # An entry is "orphan" if its tag is non-empty and not present in
            # the GitHub releases list. (Entries with empty tag — e.g.
            # artifact-mode rows from older flows — are also orphans.)
            out = []
            for v in versions:
                version = str(v.get("version", "")).strip()
                tag = str(v.get("tag", "")).strip()
                if not version:
                    continue
                missing = (not tag) or (tag not in release_tags)
                if not missing:
                    continue
                out.append({
                    "version": version,
                    "tag": tag,
                    "channels": list(v.get("channels", []) or []),
                    "recommended": bool(v.get("recommended", False)),
                    "changelog": str(v.get("changelog", "") or ""),
                    "is_auto_version": version == auto_version,
                })
            return {"ok": True, "orphans": out, "auto_version": auto_version}
        except Exception as e:
            return _error(e, "list_policy_orphan_versions")

    def list_action_builds(self, limit: int = 20) -> dict:
        """Recent Action runs for the configured owner/repo.

        Each entry includes `run_number` (repo-local sequential ID, matches
        build.rs's `GITHUB_RUN_NUMBER` used for canonical version suffix).
        """
        try:
            gh = self._github()
            runs = gh.list_repo_workflow_runs(per_page=limit, status="")
            out = []
            for r in runs:
                out.append({
                    "run_id": int(r.get("id", 0) or 0),
                    "run_number": int(r.get("run_number", 0) or 0),
                    "name": r.get("name", ""),
                    "title": r.get("display_title", "") or r.get("head_commit", {}).get("message", "").split("\n", 1)[0],
                    "branch": r.get("head_branch", ""),
                    "head_sha": r.get("head_sha", ""),
                    "status": r.get("status", ""),
                    "conclusion": r.get("conclusion", ""),
                    "url": r.get("html_url", ""),
                    "created_at": r.get("created_at", ""),
                })
            return {"ok": True, "builds": out}
        except Exception as e:
            return _error(e, "list_action_builds")

    def list_plugin_action_builds(self, limit: int = 20) -> dict:
        """Recent Action runs for the configured plugins owner/repo."""
        try:
            cfg = config_module.load_config()
            owner = str(cfg.get("plugins_owner", "")).strip()
            repo = str(cfg.get("plugins_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 plugins_owner / plugins_repo"}
            gh = self._gh_for(owner, repo, cfg)
            runs = gh.list_repo_workflow_runs(owner, repo, per_page=limit, status="")
            out = []
            for r in runs:
                run_id = int(r.get("id", 0) or 0)
                artifact_names = []
                if run_id:
                    for a in gh.list_run_artifacts(owner, repo, run_id=run_id):
                        if bool(a.get("expired")):
                            continue
                        name = str(a.get("name", "") or "").strip()
                        if name:
                            artifact_names.append(name)
                out.append({
                    "run_id": run_id,
                    "run_number": int(r.get("run_number", 0) or 0),
                    "name": r.get("name", ""),
                    "title": r.get("display_title", "") or r.get("head_commit", {}).get("message", "").split("\n", 1)[0],
                    "branch": r.get("head_branch", ""),
                    "head_sha": r.get("head_sha", ""),
                    "status": r.get("status", ""),
                    "conclusion": r.get("conclusion", ""),
                    "url": r.get("html_url", ""),
                    "created_at": r.get("created_at", ""),
                    "artifact_names": artifact_names,
                    "artifact_name": ",".join(artifact_names),
                })
            return {"ok": True, "builds": out}
        except Exception as e:
            return _error(e, "list_plugin_action_builds")

    # ---- Publish actions ----

    def get_head_commit_subject(self) -> dict:
        """Return gyroflow repo HEAD commit subject for prefilling build_label."""
        try:
            subject = git_ops.get_head_commit_subject(REPO_ROOT)
            branch = git_ops.get_current_branch(REPO_ROOT)
            return {"ok": True, "subject": subject, "branch": branch}
        except Exception as e:
            return _error(e, "get_head_commit_subject")

    def trigger_action_build(self, build_label: str = "") -> dict:
        """Dispatch APP_BUILD_WORKFLOW against current branch. No tag created.

        `build_label` surfaces in the Action run name. Empty → fall back to
        the current HEAD commit subject so the old auto-label behavior is
        preserved when the UI doesn't provide one.
        """
        try:
            branch = git_ops.get_current_branch(REPO_ROOT)
            if not branch:
                return {"ok": False, "error": "无法读取当前分支"}
            local_head = git_ops.get_head_commit_sha(REPO_ROOT)
            cfg = config_module.load_config()
            remote = (cfg.get("git_remote", "origin") or "origin").strip()
            remote_head = git_ops.get_remote_branch_sha(REPO_ROOT, remote, branch)
            if not remote_head:
                return {"ok": False, "error": f"远端 {remote} 上还没有分支 {branch},请先 push"}
            if local_head != remote_head:
                return {
                    "ok": False,
                    "error": f"本地 HEAD ({local_head[:8]}) 与远端 ({remote_head[:8]}) 不一致,Action 只会基于远端已推送的提交编译,请先 git push",
                }
            label = str(build_label or "").strip() or git_ops.get_head_commit_subject(REPO_ROOT) or branch
            self._github(cfg).dispatch_workflow(
                APP_BUILD_WORKFLOW_FILE,
                branch,
                inputs={"build_label": label[:80]},
            )
            return {
                "ok": True,
                "branch": branch,
                "label": label[:80],
                "message": f"已在 {branch} 分支触发 {APP_BUILD_WORKFLOW_FILE}",
            }
        except Exception as e:
            return _error(e, "trigger_action_build")

    def create_and_push_tag(self, major: int, minor: int, patch: int) -> dict:
        """Create and push `v<major>.<minor>.<patch>` tag at current HEAD."""
        try:
            for v in (major, minor, patch):
                if not isinstance(v, int) or v < 0 or v > 999:
                    return {"ok": False, "error": "版本号必须是 0-999 的整数"}
            tag = f"v{major}.{minor}.{patch}"
            cfg = config_module.load_config()
            remote = (cfg.get("git_remote", "origin") or "origin").strip()
            stale_cleanup: str | None = None
            if git_ops.local_tag_exists(REPO_ROOT, tag):
                # Decide whether the local tag is actually stale (remote already
                # cleared it — typical when the user deleted the GitHub release
                # with "Also delete tag" checked, then wants to redo the same
                # version) or genuinely conflicts with remote.
                remote_has = git_ops.remote_tag_exists(REPO_ROOT, remote, tag)
                if not remote_has:
                    # Stale: remote is the source of truth. Drop the local ref
                    # so the retry can proceed without forcing the user to
                    # `git tag -d` by hand.
                    try:
                        git_ops.run_git(REPO_ROOT, "tag", "-d", tag)
                    except subprocess.CalledProcessError as e:
                        return {
                            "ok": False,
                            "error": (
                                f"清理 stale 本地 tag {tag} 失败: "
                                f"{e.stderr or e.stdout or str(e)};请手动 `git tag -d {tag}` 后重试"
                            ),
                        }
                    stale_cleanup = f"已清理 stale 本地 tag {tag} (远端已不存在)"
                else:
                    # Genuine conflict — both sides have it. Give the user the
                    # context they need to decide (different commit? same?).
                    try:
                        existing_sha = git_ops.run_git(REPO_ROOT, "rev-list", "-n", "1", tag).stdout.strip()[:8]
                    except Exception:
                        existing_sha = "unknown"
                    head_sha = git_ops.get_head_commit_sha(REPO_ROOT)[:8]
                    hint = "与当前 HEAD 一致" if existing_sha == head_sha else f"指向历史 commit (当前 HEAD 是 {head_sha})"
                    return {
                        "ok": False,
                        "error": (
                            f"本地与远端都已存在 tag {tag} → commit {existing_sha} ({hint})。\n"
                            f"处理方式: (1) 换版本号 (默认建议已 +1);或 (2) 若要覆盖:"
                            f"先在 GitHub 上删 release+tag, 然后 `git tag -d {tag}` 删本地后再点打 tag"
                        ),
                    }
            if git_ops.remote_tag_exists(REPO_ROOT, remote, tag):
                return {"ok": False, "error": f"远端 {remote} 已存在 Tag: {tag}"}
            target_version = f"{major}.{minor}.{patch}"
            try:
                bump_diff = _bump_cargo_and_commit_if_needed(
                    REPO_ROOT, remote, target_version
                )
            except subprocess.CalledProcessError as e:
                return {
                    "ok": False,
                    "error": (
                        f"Cargo.toml bump 失败 (git): {e.stderr or e.stdout or str(e)};"
                        f" Cargo.toml 可能已被本地修改但未提交,请手动检查 git status"
                    ),
                }
            except RuntimeError as e:
                return {"ok": False, "error": f"Cargo.toml bump 失败: {e}"}
            git_ops.create_and_push_tag(REPO_ROOT, remote, tag)
            parts = []
            if stale_cleanup:
                parts.append(stale_cleanup)
            if bump_diff:
                parts.append(f"Cargo.toml workspace.version 已同步 ({bump_diff}) 并提交")
            parts.append(f"Tag {tag} 已推送到 {remote}")
            return {
                "ok": True,
                "tag": tag,
                "cargo_bump": bump_diff,
                "stale_cleanup": stale_cleanup,
                "message": ";".join(parts),
            }
        except subprocess.CalledProcessError as e:
            return {
                "ok": False,
                "error": f"git 命令失败: {e.stderr or e.stdout or str(e)}",
            }
        except Exception as e:
            return _error(e, "create_and_push_tag")

    # ---- Remote tag creation for plugin / lens repos (no local clone needed) ----

    def _gh_for(self, owner: str, repo: str, cfg: dict | None = None) -> GitHubClient:
        """GitHubClient targeting a specific owner/repo (vs. main gyroflow)."""
        cfg = cfg if cfg is not None else config_module.load_config()
        return GitHubClient(
            owner=owner,
            repo=repo,
            token=cfg.get("github_token", ""),
            proxy_url=cfg.get("network_proxy", ""),
        )

    # ---- Local-clone probing + tag suggestion ----

    def _find_local_clone(self, repo_name: str) -> Path | None:
        """Return local path to a sibling clone if it exists as a git repo.

        Convention: clones live alongside gyroflow at `<gyroflow parent>/<repo_name>`.
        e.g. C:/Users/Jhe/Desktop/github/gyroflow-plugins
        """
        candidate = REPO_ROOT.parent / repo_name
        if candidate.is_dir() and (candidate / ".git").exists():
            return candidate
        return None

    @staticmethod
    def _parse_semver_tag(tag: str) -> tuple[int, int, int] | None:
        """Parse `vX.Y.Z` or `X.Y.Z` (ignoring any trailing pre-release)."""
        m = re.match(r"^v?(\d+)\.(\d+)\.(\d+)", str(tag or "").strip())
        if not m:
            return None
        return int(m.group(1)), int(m.group(2)), int(m.group(3))

    def _latest_semver_tag(self, tags: list[dict]) -> tuple[int, int, int] | None:
        best: tuple[int, int, int] | None = None
        for t in tags:
            parsed = self._parse_semver_tag(t.get("name", ""))
            if parsed and (best is None or parsed > best):
                best = parsed
        return best

    def _cargo_version(self) -> tuple[int, int, int] | None:
        try:
            cargo = (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
            m = re.search(r'^\s*version\s*=\s*"(\d+)\.(\d+)\.(\d+)', cargo, re.MULTILINE)
            if m:
                return int(m.group(1)), int(m.group(2)), int(m.group(3))
        except Exception:
            pass
        return None

    def get_gyroflow_latest_tag_suggestion(self) -> dict:
        """Suggest next gyroflow tag — always the max(latest tag, Cargo.toml)
        with patch incremented by 1. So an existing `v1.6.3` or a Cargo.toml
        pinned at `1.6.3` both produce `1.6.4` as the next default.
        """
        try:
            cfg = config_module.load_config()
            gh = self._github(cfg)
            candidates: list[tuple[int, int, int]] = []
            try:
                tags = gh.list_repo_tags(gh.owner, gh.repo, per_page=100)
                latest = self._latest_semver_tag(tags)
                if latest:
                    candidates.append(latest)
            except Exception:
                pass
            cargo = self._cargo_version()
            if cargo:
                candidates.append(cargo)
            if not candidates:
                return {"ok": True, "major": 1, "minor": 0, "patch": 1, "source": "default"}
            base = max(candidates)
            return {
                "ok": True,
                "major": base[0],
                "minor": base[1],
                "patch": base[2] + 1,
                "source": "latest_tag" if base == candidates[0] else "cargo_toml",
            }
        except Exception as e:
            return _error(e, "get_gyroflow_latest_tag_suggestion")

    def get_plugin_latest_tag_suggestion(self) -> dict:
        """Suggest next plugin tag — max(latest plugin tag, gyroflow Cargo.toml) + 1.
        Plugin versions typically track gyroflow main's version, so the
        Cargo.toml fallback keeps them aligned on a fresh plugin repo.
        """
        try:
            cfg = config_module.load_config()
            owner = str(cfg.get("plugins_owner", "")).strip()
            repo = str(cfg.get("plugins_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 plugins_owner / plugins_repo"}
            candidates: list[tuple[int, int, int]] = []
            try:
                gh = self._gh_for(owner, repo, cfg)
                tags = gh.list_repo_tags(owner, repo, per_page=100)
                latest = self._latest_semver_tag(tags)
                if latest:
                    candidates.append(latest)
            except Exception:
                pass
            cargo = self._cargo_version()
            if cargo:
                candidates.append(cargo)
            if not candidates:
                return {"ok": True, "major": 1, "minor": 0, "patch": 1, "source": "default"}
            base = max(candidates)
            return {
                "ok": True,
                "major": base[0],
                "minor": base[1],
                "patch": base[2] + 1,
                "source": "latest_tag" if base == candidates[0] else "cargo_toml",
            }
        except Exception as e:
            return _error(e, "get_plugin_latest_tag_suggestion")

    def _push_tag_via_api_or_clone(
        self,
        repo_folder_name: str,
        owner: str,
        repo: str,
        tag: str,
        cfg: dict,
        *,
        bump_cargo_to: str | None = None,
    ) -> dict:
        """Preferred: GitHub API (zero local dependency).
        Fallback: local clone + git push (reuses system git credentials).

        When `bump_cargo_to` is set, the GitHub API path is skipped — editing a
        file (Cargo.toml) requires a local checkout. The local clone path then
        bumps `[workspace.package].version` if it does not already match
        `bump_cargo_to`, commits the change, and pushes before tagging.
        """
        api_error: str | None = None
        if bump_cargo_to is None:
            # --- Try API first (no Cargo edit needed) ---
            try:
                gh = self._gh_for(owner, repo, cfg)
                sha = gh.get_default_branch_sha(owner, repo)
                if not sha:
                    raise RuntimeError(f"{owner}/{repo} 默认分支 HEAD sha 读取失败")
                gh.create_remote_tag(owner, repo, tag, sha)
                return {
                    "ok": True,
                    "tag": tag,
                    "repo": f"{owner}/{repo}",
                    "via": "api",
                    "sha": sha,
                    "message": f"已通过 GitHub API 为 {owner}/{repo} 创建 tag {tag}",
                }
            except Exception as e:
                api_error = f"{e.__class__.__name__}: {e}"

        # --- Local clone path (forced when bump_cargo_to is set) ---
        local = self._find_local_clone(repo_folder_name)
        if not local:
            if bump_cargo_to is not None:
                return {
                    "ok": False,
                    "error": (
                        f"需要本地 clone {repo_folder_name}/ 来同步 Cargo.toml workspace.version"
                        f" 到 {bump_cargo_to},但未找到。请先 clone {owner}/{repo} 到 {repo_folder_name}/。"
                    ),
                }
            return {
                "ok": False,
                "error": f"API 失败 ({api_error}),且未找到本地 clone {repo_folder_name}/",
            }
        remote = (cfg.get("git_remote", "origin") or "origin").strip()
        stale_cleanup: str | None = None
        if git_ops.local_tag_exists(local, tag):
            # Same logic as the main repo path: if the remote already cleared
            # the tag (typical when the user deleted the GitHub release with
            # "Also delete tag" checked, then wants to redo the same version),
            # the local ref is stale — drop it and continue.
            if not git_ops.remote_tag_exists(local, remote, tag):
                try:
                    git_ops.run_git(local, "tag", "-d", tag)
                except subprocess.CalledProcessError as e:
                    return {
                        "ok": False,
                        "error": (
                            f"清理 stale 本地 tag {tag} ({local.name}) 失败: "
                            f"{e.stderr or e.stdout or str(e)};"
                            f" 请手动 `git tag -d {tag}` 后重试"
                        ),
                    }
                stale_cleanup = f"已清理 stale 本地 tag {tag} (远端已不存在)"
            else:
                note = "" if bump_cargo_to is None else " (Cargo.toml 未做改动)"
                return {
                    "ok": False,
                    "error": (
                        f"本地与 {owner}/{repo} 远端都已存在 tag: {tag}{note}。"
                        f"请先在 GitHub 上删 release+tag, 然后 `git tag -d {tag}` 删本地后重试"
                        + (f" (API 错误: {api_error})" if api_error else "")
                    ),
                }
        elif git_ops.remote_tag_exists(local, remote, tag):
            note = "" if bump_cargo_to is None else " (Cargo.toml 未做改动)"
            return {
                "ok": False,
                "error": (
                    f"{owner}/{repo} 远端已存在 tag: {tag}{note}"
                    + (f" (API 错误: {api_error})" if api_error else "")
                ),
            }
        bump_diff: str | None = None
        if bump_cargo_to is not None:
            try:
                bump_diff = _bump_cargo_and_commit_if_needed(
                    local, remote, bump_cargo_to
                )
            except subprocess.CalledProcessError as e:
                return {
                    "ok": False,
                    "error": (
                        f"Cargo.toml bump 失败 (git): {e.stderr or e.stdout or str(e)};"
                        f" 本地 {local!s} 的 Cargo.toml 可能已被改动但未提交,请手动 git status / checkout 处理"
                    ),
                }
            except RuntimeError as e:
                return {"ok": False, "error": f"Cargo.toml bump 失败: {e}"}
        try:
            git_ops.create_and_push_tag(local, remote, tag)
        except subprocess.CalledProcessError as e:
            err_msg = (
                f"本地 git 失败: {e.stderr or e.stdout or str(e)}"
                if bump_cargo_to is not None
                else f"API 失败 ({api_error});本地 git 也失败: {e.stderr or e.stdout or str(e)}"
            )
            return {"ok": False, "error": err_msg}
        result = {
            "ok": True,
            "tag": tag,
            "repo": f"{owner}/{repo}",
            "via": "local-clone" if bump_cargo_to is not None else "local-clone-fallback",
            "workdir": str(local),
            "cargo_bump": bump_diff,
            "stale_cleanup": stale_cleanup,
        }
        if bump_cargo_to is not None:
            parts: list[str] = []
            if stale_cleanup:
                parts.append(stale_cleanup)
            if bump_diff:
                parts.append(f"Cargo.toml workspace.version 已同步 ({bump_diff}) 并提交")
            parts.append(f"已在本地 {local.name} 打 tag {tag} 并 push 到 {remote}")
            result["message"] = "; ".join(parts)
        else:
            result["api_error"] = api_error
            cleanup_prefix = f"{stale_cleanup}; " if stale_cleanup else ""
            result["message"] = (
                f"{cleanup_prefix}API 失败({api_error}),"
                f"已 fallback 到本地 {local.name} 打 tag 并 push 到 {remote}"
            )
        return result

    def get_plugin_head_commit_subject(self) -> dict:
        """Return remote default branch HEAD commit subject for the plugin repo.

        Plugin repo is not locally cloned (per setup convention), so we hit
        GitHub `/repos/{owner}/{repo}/commits/{default_branch}` and take the
        first line of the commit message. Matches the prefill semantics of
        gyroflow's own `get_head_commit_subject` (which uses `git log`).
        """
        try:
            cfg = config_module.load_config()
            owner = str(cfg.get("plugins_owner", "")).strip()
            repo = str(cfg.get("plugins_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 plugins_owner / plugins_repo"}
            gh = self._gh_for(owner, repo, cfg)
            meta = gh.get_repo(owner, repo)
            branch = str(meta.get("default_branch", "")).strip() or "main"
            commit = gh.get_branch_head_commit(owner, repo, branch)
            message = str((commit.get("commit") or {}).get("message", "")).strip()
            subject = message.splitlines()[0].strip() if message else ""
            return {"ok": True, "subject": subject, "branch": branch, "owner": owner, "repo": repo}
        except Exception as e:
            return _error(e, "get_plugin_head_commit_subject")

    def trigger_plugin_action_build(self, build_label: str = "") -> dict:
        """Dispatch the plugin repo's release.yml on its default branch — no tag.

        Mirrors `trigger_action_build` for the plugin repo. There is no local
        clone to compare HEAD against, so we just dispatch on whatever the
        remote default branch points at right now. Empty `build_label` falls
        back to the latest pushed commit's subject (same behavior as the
        gyroflow trigger).
        """
        try:
            cfg = config_module.load_config()
            owner = str(cfg.get("plugins_owner", "")).strip()
            repo = str(cfg.get("plugins_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 plugins_owner / plugins_repo"}
            gh = self._gh_for(owner, repo, cfg)
            meta = gh.get_repo(owner, repo)
            branch = str(meta.get("default_branch", "")).strip() or "main"
            label = str(build_label or "").strip()
            if not label:
                try:
                    commit = gh.get_branch_head_commit(owner, repo, branch)
                    message = str((commit.get("commit") or {}).get("message", "")).strip()
                    label = (message.splitlines()[0].strip() if message else "") or branch
                except Exception:
                    label = branch
            gh.dispatch_workflow(
                APP_BUILD_WORKFLOW_FILE,
                branch,
                inputs={"build_label": label[:80]},
                owner=owner,
                repo=repo,
            )
            return {
                "ok": True,
                "branch": branch,
                "label": label[:80],
                "owner": owner,
                "repo": repo,
                "message": f"已在 {owner}/{repo} 的 {branch} 分支触发 {APP_BUILD_WORKFLOW_FILE}",
            }
        except Exception as e:
            return _error(e, "trigger_plugin_action_build")

    def create_plugin_tag(self, major: int, minor: int, patch: int) -> dict:
        """Create `v<major>.<minor>.<patch>` tag on plugin repo.

        Forces the local-clone path (bypassing the GitHub API tag shortcut) so we
        can sync `[workspace.package].version` in the plugin repo's Cargo.toml
        before tagging. plugin build.rs reads CARGO_PKG_VERSION at compile time
        to embed the PE FileVersion — without this sync the tagged release
        carries a stale internal version (see v2.1.2 incident on 2026-04-25).
        """
        try:
            for v in (major, minor, patch):
                if not isinstance(v, int) or v < 0 or v > 999:
                    return {"ok": False, "error": "版本号必须是 0-999 的整数"}
            cfg = config_module.load_config()
            owner = str(cfg.get("plugins_owner", "")).strip()
            repo = str(cfg.get("plugins_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 plugins_owner / plugins_repo"}
            target_version = f"{major}.{minor}.{patch}"
            return self._push_tag_via_api_or_clone(
                repo,
                owner,
                repo,
                f"v{target_version}",
                cfg,
                bump_cargo_to=target_version,
            )
        except Exception as e:
            return _error(e, "create_plugin_tag")

    def get_lens_next_tag_suggestion(self) -> dict:
        """Pre-fill suggestion for lens tag UI: today's YYYYMMDD + next N.

        N is computed by scanning existing tags for `data-v<today>.*` and
        adding 1 to the max suffix (start at 1 if none).
        """
        try:
            cfg = config_module.load_config()
            owner = str(cfg.get("lens_data_owner", "")).strip()
            repo = str(cfg.get("lens_data_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 lens_data_owner / lens_data_repo"}
            today = datetime.now().strftime("%Y%m%d")
            try:
                tags = self._gh_for(owner, repo, cfg).list_repo_tags(owner, repo, per_page=100)
            except Exception as e:
                return {
                    "ok": True,
                    "date": today,
                    "suggested_n": 1,
                    "suggested_tag": f"data-v{today}.1",
                    "warning": f"list_repo_tags 失败 ({e.__class__.__name__}: {e});序号默认为 1",
                }
            max_n = 0
            prefix = f"data-v{today}."
            for t in tags:
                name = str(t.get("name", ""))
                if name.startswith(prefix):
                    try:
                        n = int(name[len(prefix):])
                        if n > max_n:
                            max_n = n
                    except ValueError:
                        continue
            suggested_n = max_n + 1
            return {
                "ok": True,
                "date": today,
                "suggested_n": suggested_n,
                "suggested_tag": f"data-v{today}.{suggested_n}",
            }
        except Exception as e:
            return _error(e, "get_lens_next_tag_suggestion")

    def create_lens_tag(self, date: str = "", suffix_n: int = 0) -> dict:
        """Create `data-v<YYYYMMDD>.<N>` tag on lens repo default branch.

        date: empty → today's YYYYMMDD; otherwise must be 8-digit string.
        suffix_n: 0 → auto-compute as max existing + 1; otherwise use as given.
        """
        try:
            cfg = config_module.load_config()
            owner = str(cfg.get("lens_data_owner", "")).strip()
            repo = str(cfg.get("lens_data_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 lens_data_owner / lens_data_repo"}
            date = str(date or "").strip() or datetime.now().strftime("%Y%m%d")
            if not re.fullmatch(r"\d{8}", date):
                return {"ok": False, "error": f"date 必须是 8 位数字 YYYYMMDD,收到: {date!r}"}
            gh = self._gh_for(owner, repo, cfg)
            n = int(suffix_n or 0)
            if n <= 0:
                tags = gh.list_repo_tags(owner, repo, per_page=100)
                prefix = f"data-v{date}."
                max_n = 0
                for t in tags:
                    name = str(t.get("name", ""))
                    if name.startswith(prefix):
                        try:
                            max_n = max(max_n, int(name[len(prefix):]))
                        except ValueError:
                            continue
                n = max_n + 1
            tag = f"data-v{date}.{n}"
            return self._push_tag_via_api_or_clone(repo, owner, repo, tag, cfg)
        except Exception as e:
            return _error(e, "create_lens_tag")

    def get_plugin_latest_run(self) -> dict:
        """Return plugin repo's most recent successful workflow run, so the
        resources view can show 'currently serving run #N · <title>'.
        """
        try:
            cfg = config_module.load_config()
            owner = str(cfg.get("plugins_owner", "")).strip()
            repo = str(cfg.get("plugins_repo", "")).strip()
            if not (owner and repo):
                return {"ok": False, "error": "config 里缺少 plugins_owner / plugins_repo"}
            runs = self._gh_for(owner, repo, cfg).list_repo_workflow_runs(
                owner, repo, per_page=10, status=""
            )
            latest = next(
                (r for r in runs if r.get("conclusion") == "success"),
                None,
            )
            if not latest:
                return {"ok": True, "run": None, "note": "plugin 仓库没有 successful run"}
            return {
                "ok": True,
                "run": {
                    "run_id": int(latest.get("id", 0) or 0),
                    "run_number": int(latest.get("run_number", 0) or 0),
                    "title": latest.get("display_title", "") or latest.get("head_commit", {}).get("message", "").split("\n", 1)[0],
                    "branch": latest.get("head_branch", ""),
                    "html_url": latest.get("html_url", ""),
                    "created_at": latest.get("created_at", ""),
                },
            }
        except Exception as e:
            return _error(e, "get_plugin_latest_run")

    # ---- Resources orchestration ----

    def get_resources_state(self) -> dict:
        """Current Lens/Plugin/SDK envs from Vercel + stored publish_defaults."""
        try:
            cfg = config_module.load_config()
            defaults = (cfg.get("publish_defaults") or {})
            envs = {}
            try:
                envs = self._vercel(cfg).list_envs_decrypted()
            except Exception as e:
                return {
                    "ok": True,
                    "defaults": defaults,
                    "current": {},
                    "error": f"Vercel 读取失败: {e}",
                }
            return {
                "ok": True,
                "defaults": defaults,
                "current": {
                    # Decoupled bundle tags (`lens-<sha12>` / `plugin-<sha12>`).
                    "NIYIEN_LENS_RELEASE_TAG": envs.get("NIYIEN_LENS_RELEASE_TAG", ""),
                    "NIYIEN_PLUGIN_RELEASE_TAG": envs.get("NIYIEN_PLUGIN_RELEASE_TAG", ""),
                    "NIYIEN_LENS_VERSION": envs.get("NIYIEN_LENS_VERSION", ""),
                    "NIYIEN_LENS_DATA_TAG": envs.get("NIYIEN_LENS_DATA_TAG", ""),
                    "NIYIEN_PLUGINS_SOURCE_MODE": envs.get("NIYIEN_PLUGINS_SOURCE_MODE", ""),
                    "NIYIEN_PLUGINS_TAG": envs.get("NIYIEN_PLUGINS_TAG", ""),
                    "NIYIEN_PLUGINS_ARTIFACT_NAME": envs.get("NIYIEN_PLUGINS_ARTIFACT_NAME", ""),
                    "NIYIEN_PLUGINS_RUN_ID": envs.get("NIYIEN_PLUGINS_RUN_ID", ""),
                    "NIYIEN_SDK_BASE": envs.get("NIYIEN_SDK_BASE", ""),
                },
            }
        except Exception as e:
            return _error(e, "get_resources_state")

    def _fetch_lens_metadata_for_tag(self, cfg: dict, lens_tag: str) -> dict:
        """Read lens release's metadata.json to extract version + sha256.

        Returns {"version": int|str, "sha256": str} or {} on any failure.
        Used by apply_resources_now to populate NIYIEN_LENS_VERSION /
        NIYIEN_LENS_SHA256 envs alongside NIYIEN_LENS_DATA_TAG — without
        these the manifest API hands clients lens.version=0 which
        disables the auto-update path for bundled lens data.
        """
        if not lens_tag:
            return {}
        try:
            owner = str(cfg.get("lens_data_owner", "")).strip()
            repo = str(cfg.get("lens_data_repo", "")).strip()
            if not (owner and repo):
                return {}
            gh = self._gh_for(owner, repo, cfg)
            releases = gh.list_repo_releases(owner, repo)
            target = next(
                (r for r in releases if str(r.get("tag_name", "")).strip() == lens_tag),
                None,
            )
            if not target:
                return {}
            asset = next(
                (a for a in target.get("assets", [])
                 if str(a.get("name", "")).strip() == "gyroflow-niyien-lens.cbor.gz.json"),
                None,
            )
            download_url = str((asset or {}).get("browser_download_url", "")).strip()
            if not download_url:
                return {}
            import json as _json
            import requests
            from .helpers import build_proxy_mapping
            token = self._get_publish_secret("GITHUB_TOKEN", cfg=cfg)
            headers = {"Accept": "application/json"}
            if token:
                headers["Authorization"] = f"Bearer {token}"
            proxies = build_proxy_mapping(cfg.get("network_proxy", ""))
            kwargs = {"headers": headers, "timeout": 30}
            if proxies:
                kwargs["proxies"] = proxies
            resp = requests.get(download_url, **kwargs)
            resp.raise_for_status()
            meta = _json.loads(resp.text)
            if isinstance(meta, dict):
                return {"version": meta.get("version"), "sha256": meta.get("sha256")}
        except Exception:
            pass
        return {}

    def apply_resources_now(self, payload: dict) -> dict:
        """Immediately upsert Lens/Plugin/SDK env vars to Vercel (live).

        `payload` keys: lens_tag, plugin_mode, plugin_tag, plugin_artifact_name, sdk_base.

        `lens_tag` is the niyien-lens-data GitHub release tag (mapped to
        `entry.lens_release_tag`). `plugin_tag` is the gyroflow-plugins
        release tag (release mode only). The 123 网盘 directory names
        (`lens-<sha12>` / `plugin-<sha12>`) are read back from vercel envs
        `NIYIEN_LENS_RELEASE_TAG` / `NIYIEN_PLUGIN_RELEASE_TAG`, which the
        publish flow set on the prior `publish_and_push`. This lets the
        switch operate on already-uploaded bundles without re-running the
        whole publish pipeline.
        """
        try:
            lens_tag = str(payload.get("lens_tag", "")).strip()
            plugin_mode = str(payload.get("plugin_mode", "")).strip().lower() or "release"
            plugin_tag = str(payload.get("plugin_tag", "")).strip()
            plugin_artifact = str(payload.get("plugin_artifact_name", "")).strip()
            try:
                plugin_run_id = int(payload.get("plugin_run_id", 0) or 0)
            except (TypeError, ValueError):
                plugin_run_id = 0
            sdk_base = str(payload.get("sdk_base", "")).strip()
            if not lens_tag:
                return {"ok": False, "error": "Lens Tag 不能为空"}
            if plugin_mode not in ("release", "artifact"):
                return {"ok": False, "error": f"plugin_mode 必须是 release/artifact,不是 {plugin_mode}"}
            if plugin_mode == "release" and not plugin_tag:
                return {"ok": False, "error": "release 模式下 Plugin Tag 不能为空"}
            if plugin_mode == "artifact" and not plugin_artifact:
                return {"ok": False, "error": "artifact 模式下 Plugin Artifact Name 不能为空"}
            cfg = config_module.load_config()
            mapping = {
                "NIYIEN_LENS_DATA_TAG": lens_tag,
                "NIYIEN_PLUGINS_SOURCE_MODE": plugin_mode,
                "NIYIEN_PLUGINS_TAG": plugin_tag if plugin_mode == "release" else "",
                "NIYIEN_PLUGINS_ARTIFACT_NAME": plugin_artifact if plugin_mode == "artifact" else "",
                "NIYIEN_PLUGINS_RUN_ID": str(plugin_run_id) if plugin_mode == "artifact" and plugin_run_id > 0 else "",
                "NIYIEN_SDK_BASE": sdk_base,
            }
            # Pull lens version/sha256 from the release's metadata.json so
            # the manifest API can hand a non-zero lens.version to clients.
            meta = self._fetch_lens_metadata_for_tag(cfg, lens_tag)
            lens_extras: list[str] = []
            if meta.get("version") is not None:
                mapping["NIYIEN_LENS_VERSION"] = str(meta["version"])
                lens_extras.append(f"version={meta['version']}")
            if meta.get("sha256"):
                mapping["NIYIEN_LENS_SHA256"] = str(meta["sha256"])
                lens_extras.append(f"sha256={str(meta['sha256'])[:10]}...")
            extras_note = f" (lens metadata: {', '.join(lens_extras)})" if lens_extras else " (lens metadata 未读到)"
            # Read the 123 网盘 dir tags that the prior publish flow recorded.
            # Without these the policy entry can't surface a working
            # `entry.lens_tag` / `entry.plugin_tag` for CN clients (CN URLs
            # walk RELEASES_ROOT/<dir_tag>/...).
            current_envs = self._vercel(cfg).list_envs_decrypted()
            lens_dir_tag = str(current_envs.get("NIYIEN_LENS_RELEASE_TAG", "")).strip()
            plugin_dir_tag = str(current_envs.get("NIYIEN_PLUGIN_RELEASE_TAG", "")).strip()
            # Resolve plugin nightly.link in artifact mode. Prefer an explicit
            # run id from the selected artifact list; fall back to the most
            # recent run that produced the requested artifact name.
            plugin_owner = str(cfg.get("plugins_owner", "") or "NiYien").strip()
            plugin_repo = str(cfg.get("plugins_repo", "") or "gyroflow-plugins").strip()
            artifact_resolve_note = ""
            if plugin_mode == "artifact" and plugin_artifact:
                # plugin_artifact may be a CSV list (publish scripts treat it
                # the same way). All names in one publish come from the same
                # workflow run, so any one of them resolves to the same run_id.
                # Try them in order and stop at the first match.
                csv_names = [
                    item.strip()
                    for item in str(plugin_artifact).split(",")
                    if item.strip()
                ]
                if not csv_names:
                    artifact_resolve_note = "⚠ plugin_artifact_name 为空, global_plugins_base 留空"
                elif plugin_run_id > 0:
                    gh_client = self._gh_for(plugin_owner, plugin_repo, cfg)
                    try:
                        artifacts = gh_client.list_run_artifacts(
                            plugin_owner, plugin_repo, run_id=plugin_run_id,
                        )
                    except Exception as e:
                        artifact_resolve_note = f"⚠ 指定 run_id={plugin_run_id} 的 artifact 查询失败({e})"
                        artifacts = []
                    valid = [
                        a for a in artifacts
                        if not bool(a.get("expired"))
                        and str(a.get("name", "")).strip() in csv_names
                    ]
                    matched_names = {str(a.get("name", "")).strip() for a in valid}
                    if all(name in matched_names for name in csv_names):
                        artifact_resolve_note = f"plugin nightly.link 已解析: run-{plugin_run_id} (explicit run_id)"
                    else:
                        artifact_resolve_note = (
                            f"⚠ 指定 run_id={plugin_run_id} 未找到全部 artifact name, "
                            "global_plugins_base 留空"
                        )
                        plugin_run_id = 0
                else:
                    gh_client = self._gh_for(plugin_owner, plugin_repo, cfg)
                    last_err: Exception | None = None
                    tried_names: list[str] = []
                    for name in csv_names:
                        tried_names.append(name)
                        try:
                            artifacts = gh_client.list_repo_artifacts(
                                plugin_owner, plugin_repo, name=name, per_page=10,
                            )
                        except Exception as e:
                            last_err = e
                            continue
                        valid = [
                            a for a in artifacts
                            if not bool(a.get("expired"))
                            and isinstance(a.get("workflow_run"), dict)
                            and int(a["workflow_run"].get("id", 0) or 0) > 0
                        ]
                        if valid:
                            # GitHub returns artifacts newest-first.
                            plugin_run_id = int(valid[0]["workflow_run"]["id"])
                            artifact_resolve_note = (
                                f"plugin nightly.link 已解析: run-{plugin_run_id} "
                                f"(via artifact={name})"
                            )
                            break
                    if plugin_run_id == 0:
                        if last_err is not None:
                            artifact_resolve_note = (
                                f"⚠ GitHub artifact 查询失败({last_err}), "
                                f"已试 {len(tried_names)} 个 name, global_plugins_base 留空"
                            )
                        else:
                            artifact_resolve_note = (
                                f"⚠ {len(tried_names)} 个 artifact name 都没有未过期的 run, "
                                f"global_plugins_base 留空(docs 将回退到 release-latest)"
                            )
            if plugin_mode == "artifact":
                target_plugin_source_ref = (
                    f"actions-run-{int(plugin_run_id)}" if int(plugin_run_id or 0) > 0 else ""
                )
                if not target_plugin_source_ref:
                    return {
                        "ok": False,
                        "error": (
                            "artifact 模式未能解析插件 GitHub Actions run；"
                            f"{artifact_resolve_note or '请检查 Plugin Artifact Name'}"
                        ),
                    }
                plugin_dir_tag = self._resolve_plugin_bundle_tag_for_source_ref(
                    cfg=cfg,
                    vercel_envs=current_envs,
                    current_tag=plugin_dir_tag,
                    target_source_ref=target_plugin_source_ref,
                )
                mapping["NIYIEN_PLUGIN_RELEASE_TAG"] = plugin_dir_tag
            mapping["NIYIEN_PLUGINS_RUN_ID"] = (
                str(plugin_run_id) if plugin_mode == "artifact" and plugin_run_id > 0 else ""
            )
            sync_note, policy_json = self._prepare_resources_policy_update(
                cfg,
                vercel_envs=current_envs,
                lens_release_tag=lens_tag,
                lens_dir_tag=lens_dir_tag,
                lens_version=meta.get("version"),
                lens_sha=str(meta.get("sha256") or ""),
                plugin_mode=plugin_mode,
                plugin_release_tag=plugin_tag,
                plugin_dir_tag=plugin_dir_tag,
                plugin_artifact_name=plugin_artifact,
                plugin_run_id=plugin_run_id,
                plugin_owner=plugin_owner,
                plugin_repo=plugin_repo,
            )
            if policy_json:
                mapping["NIYIEN_RELEASE_POLICY_JSON"] = policy_json
            self._vercel(cfg).upsert_envs(mapping)
            # Vercel runtime env vars only affect new deployments. Without a
            # redeploy the manifest API keeps serving the previous env
            # snapshot, which is what made earlier "立即切换" calls look
            # like no-ops. Trigger unconditionally; _trigger_deploy_hook
            # falls back to the Vercel REST API if no hook is configured.
            try:
                hook_note = self._trigger_deploy_hook(cfg)
            except Exception as e:
                hook_note = f"redeploy 触发失败: {e}"
            artifact_segment = f"; {artifact_resolve_note}" if artifact_resolve_note else ""
            return {
                "ok": True,
                "message": f"已 upsert {len(mapping)} 个 env 到 Vercel{extras_note}{artifact_segment}; {sync_note}; {hook_note}",
                "deploy_hook": hook_note,
                "policy_sync": sync_note,
                "artifact_resolve": artifact_resolve_note,
            }
        except Exception as e:
            return _error(e, "apply_resources_now")

    def _resolve_plugin_bundle_tag_for_source_ref(self, *, cfg: dict,
                                                  vercel_envs: dict,
                                                  current_tag: str,
                                                  target_source_ref: str) -> str:
        client_id = self._get_publish_secret("PAN123_CLIENT_ID", vercel_envs=vercel_envs, cfg=cfg)
        client_secret = self._get_publish_secret("PAN123_CLIENT_SECRET", vercel_envs=vercel_envs, cfg=cfg)
        root_id = self._pan123_releases_root_id(cfg=cfg, vercel_envs=vercel_envs)
        if not (client_id and client_secret and root_id):
            raise RuntimeError("artifact 模式需要 PAN123 凭据来确认对应的 plugin bundle")
        client = Pan123Client(client_id, client_secret, proxy_url=cfg.get("network_proxy", ""))
        bundles = self._list_plugin_bundle_sources(client, root_id)
        return self._select_plugin_bundle_tag_for_source_ref(
            bundles,
            current_tag=current_tag,
            target_source_ref=target_source_ref,
        )

    def _list_plugin_bundle_sources(self, client: Pan123Client, root_id: int) -> list[dict]:
        if not EXPECTED_PLUGIN_FILENAMES:
            raise RuntimeError("EXPECTED_PLUGIN_FILENAMES is empty; refusing to validate plugin bundles")
        bundles: list[dict] = []
        for child in client.list_directory(root_id):
            if int(child.get("type", -1)) != 1:
                continue
            tag = str(child.get("filename") or child.get("name") or "").strip()
            if not tag.startswith("plugin-"):
                continue
            bundle_dir_id = client._entry_id(child)
            if bundle_dir_id <= 0:
                continue
            entries = client.list_directory(bundle_dir_id)
            file_names = {
                str(entry.get("filename") or entry.get("name") or "").strip()
                for entry in entries
                if int(entry.get("type", -1)) == 0
            }
            files_missing = [name for name in EXPECTED_PLUGIN_FILENAMES if name not in file_names]
            manifest, manifest_error = self._read_plugin_bundle_manifest(client, entries)
            bundles.append({
                "tag": tag,
                "plugin_source_ref": str(manifest.get("plugin_source_ref", "")).strip(),
                "complete": not files_missing,
                "files_missing": files_missing,
                "manifest_error": manifest_error,
            })
        return bundles

    def _read_plugin_bundle_manifest(self, client: Pan123Client, entries: list[dict]) -> tuple[dict, str]:
        manifest_entry = next(
            (
                entry for entry in entries
                if str(entry.get("filename") or entry.get("name") or "").strip() == PLUGIN_MANIFEST_ASSET_NAME
                and int(entry.get("type", -1)) == 0
            ),
            None,
        )
        if not manifest_entry:
            return {}, "missing manifest"
        manifest_id = client._entry_id(manifest_entry)
        if manifest_id <= 0:
            return {}, "invalid manifest file id"
        try:
            import json as _json
            parsed = _json.loads(client.fetch_file_text(manifest_id))
        except Exception as err:
            return {}, str(err)
        if not isinstance(parsed, dict):
            return {}, "manifest is not a JSON object"
        return parsed, ""

    @staticmethod
    def _select_plugin_bundle_tag_for_source_ref(bundles: list[dict], *,
                                                 current_tag: str,
                                                 target_source_ref: str) -> str:
        target_ref = str(target_source_ref or "").strip()
        if not target_ref:
            raise RuntimeError("plugin_source_ref 不能为空")
        current = str(current_tag or "").strip()
        matches: list[str] = []
        seen: list[str] = []
        for bundle in bundles:
            tag = str(bundle.get("tag", "")).strip()
            source_ref = str(bundle.get("plugin_source_ref", "")).strip()
            complete = bool(bundle.get("complete", False))
            if tag:
                suffix = "" if complete else " (incomplete)"
                manifest_error = str(bundle.get("manifest_error", "")).strip()
                error_suffix = f" ({manifest_error})" if manifest_error else ""
                seen.append(f"{tag}:{source_ref or 'no-source-ref'}{suffix}{error_suffix}")
            if tag and complete and source_ref == target_ref:
                matches.append(tag)
        if current and current in matches:
            return current
        if matches:
            return matches[0]
        details = ", ".join(seen[:8]) if seen else "no plugin bundles found"
        raise RuntimeError(f"123 plugin bundle not found for {target_ref}; available: {details}")

    def _prepare_resources_policy_update(self, cfg: dict, *,
                                         vercel_envs: dict | None = None,
                                         lens_release_tag: str = "",
                                         lens_dir_tag: str = "",
                                         lens_version=None,
                                         lens_sha: str = "",
                                         plugin_mode: str = "",
                                         plugin_release_tag: str = "",
                                         plugin_dir_tag: str = "",
                                         plugin_artifact_name: str = "",
                                         plugin_run_id: int = 0,
                                         plugin_owner: str = "NiYien",
                                         plugin_repo: str = "gyroflow-plugins") -> tuple[str, str]:
        """Mirror Lens/Plugin fields into every NIYIEN_RELEASE_POLICY_JSON entry.

        Two distinct lens identifiers exist and the manifest reads them in
        different places:
        - `lens_release_tag` is the niyien-lens-data GitHub release tag, used
          by the docs Global branch to build `releases/download/<tag>/...`.
        - `lens_dir_tag` is the 123 网盘 directory name (`lens-<sha12>`),
          used by the CN branch to walk `RELEASES_ROOT/<tag>/...`.
        The same split applies to plugins (`plugin_release_tag` vs
        `plugin_dir_tag`). Empty inputs leave the existing entry value
        untouched so a partial switch can't blank out the other component's
        state.

        In artifact mode the function also computes `global_plugins_base`
        (nightly.link wrapper) and `plugins_source_ref` from `plugin_run_id`
        so the docs Global branch returns nightly URLs without requiring a
        full publish_and_push run.
        """
        import json as _json
        envs = vercel_envs if vercel_envs is not None else self._vercel(cfg).list_envs_decrypted()
        raw = str(envs.get("NIYIEN_RELEASE_POLICY_JSON", "")).strip()
        if not raw:
            return "policy 为空,跳过 entry 镜像", ""
        try:
            policy = _json.loads(raw)
        except _json.JSONDecodeError:
            return "policy 不是合法 JSON,跳过 entry 镜像", ""
        versions = policy.get("versions") or []
        if not versions:
            return "policy.versions 为空,跳过 entry 镜像", ""
        try:
            lens_version_val = int(lens_version) if lens_version is not None and str(lens_version).strip() != "" else None
        except (TypeError, ValueError):
            lens_version_val = lens_version
        plugin_mode_norm = str(plugin_mode or "").strip().lower()
        global_plugins_base = ""
        plugins_source_ref = ""
        plugins_source_tag = ""
        if plugin_mode_norm == "artifact":
            if int(plugin_run_id or 0) > 0 and plugin_owner and plugin_repo:
                global_plugins_base = (
                    f"https://nightly.link/{plugin_owner}/{plugin_repo}"
                    f"/actions/runs/{int(plugin_run_id)}/"
                )
                plugins_source_ref = f"actions-run-{int(plugin_run_id)}"
            if plugin_artifact_name:
                plugins_source_tag = plugin_artifact_name
        elif plugin_mode_norm == "release":
            if plugin_release_tag:
                plugins_source_ref = plugin_release_tag
                plugins_source_tag = plugin_release_tag
        changed_entries = 0
        for entry in versions:
            if not isinstance(entry, dict):
                continue
            entry_changed = False
            # Lens
            if lens_release_tag and entry.get("lens_release_tag") != lens_release_tag:
                entry["lens_release_tag"] = lens_release_tag
                entry_changed = True
            if lens_dir_tag and entry.get("lens_tag") != lens_dir_tag:
                entry["lens_tag"] = lens_dir_tag
                entry_changed = True
            if lens_version_val is not None and entry.get("lens_version") != lens_version_val:
                entry["lens_version"] = lens_version_val
                entry_changed = True
            if lens_sha and entry.get("lens_sha256") != lens_sha:
                entry["lens_sha256"] = lens_sha
                entry_changed = True
            # Plugin
            if plugin_mode_norm and entry.get("plugins_source_mode") != plugin_mode_norm:
                entry["plugins_source_mode"] = plugin_mode_norm
                entry_changed = True
            if plugin_dir_tag and entry.get("plugin_tag") != plugin_dir_tag:
                entry["plugin_tag"] = plugin_dir_tag
                entry_changed = True
            if global_plugins_base and entry.get("global_plugins_base") != global_plugins_base:
                entry["global_plugins_base"] = global_plugins_base
                entry_changed = True
            if plugins_source_ref and entry.get("plugins_source_ref") != plugins_source_ref:
                entry["plugins_source_ref"] = plugins_source_ref
                entry_changed = True
            if plugins_source_tag and entry.get("plugins_source_tag") != plugins_source_tag:
                entry["plugins_source_tag"] = plugins_source_tag
                entry_changed = True
            # Mode switched away from artifact → strip stale nightly.link state
            # so the docs Global branch falls back to the release-latest base
            # instead of pinning to a stale run.
            if plugin_mode_norm == "release":
                if entry.get("global_plugins_base"):
                    entry["global_plugins_base"] = ""
                    entry_changed = True
            if entry_changed:
                changed_entries += 1
        if not changed_entries:
            return "所有 entry 已与新值一致,无需写回", ""
        policy_json = _json.dumps(policy, ensure_ascii=False, indent=2)
        return f"已镜像到 {changed_entries}/{len(versions)} 个 policy entry", policy_json

    def _sync_resources_into_policy_entries(self, cfg: dict, **kwargs) -> str:
        sync_note, policy_json = self._prepare_resources_policy_update(cfg, **kwargs)
        if policy_json:
            self._vercel(cfg).upsert_envs({"NIYIEN_RELEASE_POLICY_JSON": policy_json})
        return sync_note

    def save_resources_defaults(self, payload: dict) -> dict:
        """Save Lens/Plugin/SDK defaults to control_center.config.json's publish_defaults."""
        try:
            cfg = config_module.load_config()
            defaults = dict(cfg.get("publish_defaults") or {})
            defaults["lens_data_tag"] = str(payload.get("lens_tag", "")).strip()
            defaults["plugins_source_mode"] = str(payload.get("plugin_mode", "release")).strip().lower() or "release"
            defaults["plugins_tag"] = str(payload.get("plugin_tag", "")).strip()
            defaults["plugins_artifact_name"] = str(payload.get("plugin_artifact_name", "")).strip()
            defaults["sdk_base"] = str(payload.get("sdk_base", "")).strip()
            cfg["publish_defaults"] = defaults
            config_module.save_config(cfg)
            return {"ok": True, "message": f"已保存到 {config_module.CONFIG_FILE}"}
        except Exception as e:
            return _error(e, "save_resources_defaults")

    # ---- Pan123 secrets resolution ----
    # Three-layer fallback (vercel -> os.environ -> config) — copied from the
    # legacy Tkinter UI so we keep the same operator workflow.

    @staticmethod
    def _pan123_config_key(env_name: str) -> str:
        return {
            "PAN123_CLIENT_ID": "pan123_client_id",
            "PAN123_CLIENT_SECRET": "pan123_client_secret",
            "PAN123_RELEASES_ROOT_ID": "pan123_releases_root_id",
            "GITHUB_TOKEN": "github_token",
        }.get(env_name, "")

    def _get_publish_secret(self, env_name: str, *, vercel_envs: dict | None = None,
                            cfg: dict | None = None) -> str:
        """Resolve PAN123_* / GITHUB_TOKEN from vercel envs → os.environ → config."""
        if vercel_envs is None:
            vercel_envs = {}
        if cfg is None:
            cfg = config_module.load_config()
        for candidate in (
            str(vercel_envs.get(env_name, "")).strip(),
            str(os.environ.get(env_name, "")).strip(),
            str(cfg.get(self._pan123_config_key(env_name), "")).strip()
            if self._pan123_config_key(env_name) else "",
        ):
            if candidate:
                return candidate
        return ""

    def _pan123_releases_root_id(self, *, cfg: dict | None = None,
                                 vercel_envs: dict | None = None) -> int:
        raw = self._get_publish_secret("PAN123_RELEASES_ROOT_ID",
                                       vercel_envs=vercel_envs, cfg=cfg)
        try:
            return int(raw)
        except (TypeError, ValueError):
            return 0

    # ---- Pan123 auto-publish on action ----

    def _build_pan123_publish_command(self, *, app_tag: str, cfg: dict,
                                      vercel_envs: dict, output_dir: Path,
                                      app_run_id: int = 0,
                                      scope: list[str] | None = None,
                                      publish_overrides: dict | None = None) -> list[str]:
        """Construct the publish_pan123_release.py CLI.

        `app_run_id > 0` switches to artifact mode (`--app-source-mode=artifact
        --app-run-id=N`); zero stays release mode. The `app_tag` is still
        required in either mode because the script uses it to name the 123
        网盘 directory (`pan123.ensure_release_dir(app_tag)`).

        `scope` selects which bundles to publish; defaults to all three
        (app + lens + plugin). When narrower (e.g. ["plugin"]) the script
        skips other downloads/uploads and `finalize_summary` only carries
        the published component's tags.
        """
        script_path = REPO_ROOT / "_scripts" / "publish_pan123_release.py"
        overrides = dict(publish_overrides or {})
        # Lens / plugin / sdk pulled from current Vercel env values (same as
        # what the manifest API will hand out — keeps cn 123 mirror in sync).
        # Fall back to publish_defaults from control_center.config.json when a
        # Vercel env is missing, so a stale or unset env doesn't silently feed
        # the publish script empty / outdated artifact names.
        defaults = dict(cfg.get("publish_defaults") or {})
        lens_tag = str(overrides.get("lens_tag", "")).strip() \
            or str(vercel_envs.get("NIYIEN_LENS_DATA_TAG", "")).strip() \
            or str(defaults.get("lens_data_tag", "")).strip()
        plugins_mode = str(overrides.get("plugin_mode", "")).strip().lower() \
            or str(vercel_envs.get("NIYIEN_PLUGINS_SOURCE_MODE", "")).strip().lower() \
            or str(defaults.get("plugins_source_mode", "")).strip().lower() \
            or "release"
        plugins_tag = str(overrides.get("plugin_tag", "")).strip() \
            or str(vercel_envs.get("NIYIEN_PLUGINS_TAG", "")).strip() \
            or str(defaults.get("plugins_tag", "")).strip()
        plugins_artifact = str(overrides.get("plugin_artifact_name", "")).strip() \
            or str(vercel_envs.get("NIYIEN_PLUGINS_ARTIFACT_NAME", "")).strip() \
            or str(defaults.get("plugins_artifact_name", "")).strip()
        try:
            plugins_run_id = int(
                overrides.get("plugin_run_id", "")
                or vercel_envs.get("NIYIEN_PLUGINS_RUN_ID", "")
                or defaults.get("plugins_run_id", "")
                or 0
            )
        except (TypeError, ValueError):
            plugins_run_id = 0
        sdk_base = str(overrides.get("sdk_base", "")).strip() \
            or str(vercel_envs.get("NIYIEN_SDK_BASE", "")).strip() \
            or str(defaults.get("sdk_base", "")).strip()

        app_source_mode = "artifact" if int(app_run_id or 0) > 0 else "release"
        cmd: list[str] = [
            sys.executable,
            str(script_path),
            "--workspace", str(REPO_ROOT),
            "--output-dir", str(output_dir),
            "--app-tag", app_tag,
            "--app-source-mode", app_source_mode,
            "--app-owner", str(cfg.get("github_owner", "")).strip(),
            "--app-repo", str(cfg.get("github_repo", "")).strip(),
            "--lens-owner", str(cfg.get("lens_data_owner", "")).strip(),
            "--lens-repo", str(cfg.get("lens_data_repo", "")).strip(),
            "--plugins-owner", str(cfg.get("plugins_owner", "")).strip(),
            "--plugins-repo", str(cfg.get("plugins_repo", "")).strip(),
            "--plugins-source-mode", plugins_mode,
        ]
        if scope:
            cmd.extend(["--scope", ",".join(scope)])
        if app_source_mode == "artifact":
            cmd.extend(["--app-run-id", str(int(app_run_id))])
        if lens_tag:
            cmd.extend(["--lens-tag", lens_tag])
        if plugins_tag:
            cmd.extend(["--plugins-tag", plugins_tag])
        if plugins_artifact:
            cmd.extend(["--plugins-artifact-name", plugins_artifact])
        if plugins_mode == "artifact" and plugins_run_id > 0:
            cmd.extend(["--plugins-run-id", str(plugins_run_id)])
        if sdk_base:
            cmd.extend(["--sdk-base", sdk_base])
        return cmd

    def _start_pan123_publish(self, *, app_tag: str, app_version: str,
                              app_run_id: int = 0,
                              scope: list[str] | None = None,
                              publish_overrides: dict | None = None,
                              on_finalize=None) -> dict:
        """Submit a pan123 publish task. Returns {ok, token} or {ok:False, error}.

        Spawns publish_pan123_release.py in a worker thread; the frontend
        polls poll_publish_progress(token) for live updates.

        `scope` is a subset of {"app","lens","plugin"} (default: all three).
        Narrow scopes are rejected when NIYIEN_LENS_RELEASE_TAG /
        NIYIEN_PLUGIN_RELEASE_TAG aren't seeded yet — first publish on the
        decoupled layout must run full to initialize those envs.
        """
        try:
            cfg = config_module.load_config()
            try:
                vercel_envs = self._vercel(cfg).list_envs_decrypted()
            except Exception:
                vercel_envs = {}
            # Validate required secrets up-front so the user sees a clear error
            # instead of a buried subprocess crash.
            missing = [
                name for name in ("GITHUB_TOKEN", "PAN123_CLIENT_ID",
                                  "PAN123_CLIENT_SECRET", "PAN123_RELEASES_ROOT_ID")
                if not self._get_publish_secret(name, vercel_envs=vercel_envs, cfg=cfg)
            ]
            if missing:
                return {
                    "ok": False,
                    "error": "缺少必要凭据: " + ", ".join(missing)
                             + " — 在 Vercel envs / 系统环境变量 / control_center.config.json 任一处配齐",
                }

            # Default scope = full publish. Lens-only / plugin-only narrow
            # scopes need both new envs already populated — otherwise the
            # manifest URL builder produces broken `lens.url` / `plugins_base`
            # for the *other* component. App-only scope doesn't touch
            # lens/plugin URLs, so it doesn't need this guard.
            full_scope = ["app", "lens", "plugin"]
            scope_list = list(scope) if scope else list(full_scope)
            touches_resources = "lens" in scope_list or "plugin" in scope_list
            is_full = scope_list == full_scope
            if touches_resources and not is_full:
                if not str(vercel_envs.get("NIYIEN_LENS_RELEASE_TAG", "")).strip() \
                   or not str(vercel_envs.get("NIYIEN_PLUGIN_RELEASE_TAG", "")).strip():
                    return {
                        "ok": False,
                        "error": "首次解耦发布请先用「推送全量」初始化 NIYIEN_LENS_RELEASE_TAG / "
                                 "NIYIEN_PLUGIN_RELEASE_TAG,然后再用「仅 Lens」或「仅 Plugin」",
                    }

            output_dir = REPO_ROOT / "_deployment" / "_publish_local" / app_tag
            command = self._build_pan123_publish_command(
                app_tag=app_tag, cfg=cfg, vercel_envs=vercel_envs, output_dir=output_dir,
                app_run_id=app_run_id, scope=scope_list, publish_overrides=publish_overrides,
            )

            # Build env: copy os.environ + inject required secrets + proxy
            env = dict(os.environ)
            for name in ("GITHUB_TOKEN", "PAN123_CLIENT_ID",
                         "PAN123_CLIENT_SECRET", "PAN123_RELEASES_ROOT_ID"):
                env[name] = self._get_publish_secret(name, vercel_envs=vercel_envs, cfg=cfg)
            from .helpers import normalize_proxy_url
            proxy = normalize_proxy_url(cfg.get("network_proxy", ""))
            if proxy:
                for k in ("HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY",
                          "http_proxy", "https_proxy", "all_proxy"):
                    env[k] = proxy

            stdout_log = output_dir / "publish_local_stdout.log"
            script_path = REPO_ROOT / "_scripts" / "publish_pan123_release.py"

            mode_label = f"artifact (run={app_run_id})" if app_run_id else "release"

            def _runner(reporter, task):
                reporter.status(f"准备同步 {app_tag} → 123 网盘 [{mode_label}] (version={app_version})")
                summary = run_publish_subprocess(
                    reporter, task,
                    script_path=script_path,
                    command=command,
                    cwd=REPO_ROOT,
                    env=env,
                    stdout_log_path=stdout_log,
                )
                summary["app_tag"] = app_tag
                summary["app_version"] = app_version
                summary["app_run_id"] = app_run_id

                # Phase 2 of two-phase commit: now that 123 网盘 has the
                # files, push policy to Vercel + trigger deploy hook so
                # cn clients see the new manifest. Run this inside the
                # task runner (vs. the previous on_success callback) so
                # the operator sees the phase 2 progress in the log
                # panel as it happens, and so a manifest push failure
                # turns into a real task failure (task.ok=False).
                if callable(on_finalize):
                    reporter.status("phase 2/2: 推送 policy + 触发 deploy hook")
                    reporter.log("123 上传完成,开始推送 manifest...")
                    finalize_summary = summary.get("finalize_summary") or {}
                    if finalize_summary:
                        reporter.log(
                            f"finalize_summary: scope={finalize_summary.get('scope')}, "
                            f"lens_tag={finalize_summary.get('lens_tag') or '(skipped)'}, "
                            f"plugin_tag={finalize_summary.get('plugin_tag') or '(skipped)'}, "
                            f"lens_version={finalize_summary.get('lens_version')}, "
                            f"lens_sha256={(finalize_summary.get('lens_sha256') or '')[:16]}..."
                        )
                    try:
                        hook_note = on_finalize(finalize_summary)
                    except Exception as e:
                        reporter.log(f"✗ manifest push failed: {e}")
                        raise
                    summary["manifest_finalize_note"] = hook_note
                    summary["manifest_pushed"] = True
                    reporter.log(f"✓ manifest pushed: {hook_note}")
                return summary

            return TASKS.submit(_runner)
        except Exception as e:
            return _error(e, "_start_pan123_publish")

    # ---- Public API for frontend ----

    def start_pan123_publish_manual(self, tag: str = "", version: str = "",
                                    run_id: int = 0,
                                    scope: list[str] | None = None) -> dict:
        """User-initiated retry of pan123 sync for a specific tag.

        `run_id > 0` triggers artifact-mode (--app-source-mode=artifact
        --app-run-id=N). `scope` selects which bundles to publish (default:
        all three). When scope omits 'app', `tag` may be empty — the script
        won't touch any app installer directory in that case.

        Used by the dashboard "手动上传" button (full scope) and by the
        inventory shows a missing/incomplete tag dir. Returns {ok, token}
        or {ok:False, error}. Single-task model — refuses if another
        publish is already running.
        """
        tag = str(tag or "").strip()
        scope_list = list(scope) if scope else ["app", "lens", "plugin"]
        if "app" in scope_list and not tag:
            return {"ok": False, "error": "tag 必填 (当 scope 包含 app 时)"}
        try:
            run_id_int = int(run_id or 0)
        except (TypeError, ValueError):
            run_id_int = 0
        # Manual publish runs need a finalize callback too — without it the
        # newly-uploaded lens_tag / plugin_tag would never get persisted to
        # Vercel envs, leaving manifest API stuck on the old (or empty)
        # values. policy is NOT touched (that's execute_app_action's job).
        cfg_capture = config_module.load_config()
        def _on_resource_finalize(finalize_summary: dict) -> str:
            return self._finalize_resource_envs_to_vercel(cfg_capture, finalize_summary)
        return self._start_pan123_publish(
            app_tag=tag,
            app_version=str(version or "").strip(),
            app_run_id=run_id_int,
            scope=scope_list,
            on_finalize=_on_resource_finalize,
        )

    def _finalize_resource_envs_to_vercel(self, cfg: dict, finalize_summary: dict) -> str:
        """Upsert lens/plugin/lens-version envs after a manual publish.

        Same retry behaviour as `_finalize_publish_to_manifest`, but doesn't
        touch NIYIEN_RELEASE_POLICY_JSON. Used by `start_pan123_publish_manual`
        (dashboard "手动上传" + resources-view scoped publishes) — those
        flows must not move auto_version or rewrite policy entries.
        """
        import time as _time
        upsert: dict[str, str] = {}
        if finalize_summary.get("lens_tag"):
            upsert["NIYIEN_LENS_RELEASE_TAG"] = str(finalize_summary["lens_tag"])
        if finalize_summary.get("plugin_tag"):
            upsert["NIYIEN_PLUGIN_RELEASE_TAG"] = str(finalize_summary["plugin_tag"])
        if finalize_summary.get("lens_version") is not None:
            upsert["NIYIEN_LENS_VERSION"] = str(finalize_summary["lens_version"])
        if finalize_summary.get("lens_sha256"):
            upsert["NIYIEN_LENS_SHA256"] = str(finalize_summary["lens_sha256"])
        if not upsert:
            return "no resource env to upsert (scope=app only?)"
        last_err: Exception | None = None
        for attempt in range(3):
            try:
                self._vercel(cfg).upsert_envs(upsert)
                last_err = None
                break
            except Exception as e:
                last_err = e
                if attempt < 2:
                    _time.sleep(1.5 * (2 ** attempt))
        if last_err is not None:
            raise RuntimeError(f"upsert resource envs failed after 3 attempts: {last_err}")
        try:
            hook_note = self._trigger_deploy_hook(cfg)
            return f"resource envs upserted ({', '.join(upsert.keys())}); {hook_note}"
        except Exception as e:
            return f"resource envs upserted but deploy hook failed: {e}"

    def poll_publish_progress(self, token: str) -> dict:
        """Frontend polls this every ~500ms while a publish task is running."""
        return TASKS.poll(token)

    def cancel_publish(self, token: str) -> dict:
        """User-initiated cancel — sends SIGTERM to the publish subprocess."""
        return TASKS.cancel(token)

    def get_pan123_inventory(self) -> dict:
        """For each entry in policy.versions[], list which expected files exist
        on 123 网盘. Returns a list of {version, tag, files_present, files_missing,
        complete, is_auto_version}.

        Only checks app installer files (per EXPECTED_APP_ASSETS). Lens/plugin/
        sdk are uploaded under the same tag dir but their filenames depend on
        the upstream release content, so we don't gate completeness on them.
        """
        try:
            cfg = config_module.load_config()
            vercel = self._vercel(cfg)
            env_records = vercel.list_env_records()
            policy = self._load_current_policy(cfg, vercel, env_records)
            # IMPORTANT: list_env_records returns raw `value` field which is
            # the encrypted envelope (`{"v":"v2","c":"..."}` or its base64
            # form starting with `eyJ`) for any env with `type=encrypted`.
            # NIYIEN_LENS_RELEASE_TAG / NIYIEN_PLUGIN_RELEASE_TAG are written
            # as encrypted (upsert_envs default), so we must use
            # list_envs_decrypted which falls back to the single-env decrypt
            # endpoint per record. Without this, dashboard surfaces the raw
            # envelope blob as "current: <ciphertext>".
            vercel_envs = vercel.list_envs_decrypted()

            cid = self._get_publish_secret("PAN123_CLIENT_ID", vercel_envs=vercel_envs, cfg=cfg)
            csec = self._get_publish_secret("PAN123_CLIENT_SECRET", vercel_envs=vercel_envs, cfg=cfg)
            root_id = self._pan123_releases_root_id(cfg=cfg, vercel_envs=vercel_envs)
            if not (cid and csec and root_id):
                return {"ok": False, "error": "pan123 凭据未配置 (PAN123_CLIENT_ID/SECRET/RELEASES_ROOT_ID)"}

            client = Pan123Client(cid, csec, proxy_url=cfg.get("network_proxy", ""))
            auto_version = str(policy.get("auto_version", "") or "").strip()
            versions = policy.get("versions", []) or []

            out: list[dict] = []
            for v in versions:
                tag = str(v.get("tag", "")).strip()
                version = str(v.get("version", "")).strip()
                if not version:
                    continue
                run_id = int(v.get("run_id", 0) or 0)
                packages = v.get("packages") if isinstance(v.get("packages"), dict) else {}
                if not tag:
                    # Legacy artifact-mode entry without a synthetic tag —
                    # can't address a pan123 directory. Newer entries get
                    # `tag=run-<run_id>` set on creation so they go through
                    # the normal exists/complete checks below.
                    out.append({
                        "version": version, "tag": "",
                        "channels": list(v.get("channels", []) or []),
                        "is_auto_version": version == auto_version,
                        "exists": False,
                        "files_present": [],
                        "files_missing": [],
                        "complete": False,
                        "no_tag": True,
                        "run_id": run_id,
                        "packages": packages,
                    })
                    continue
                tag_entry = client.find_child(root_id, tag, is_dir=True)
                if not tag_entry:
                    out.append({
                        "version": version, "tag": tag,
                        "channels": list(v.get("channels", []) or []),
                        "is_auto_version": version == auto_version,
                        "exists": False,
                        "files_present": [],
                        "files_missing": list(EXPECTED_APP_ASSETS),
                        "complete": False,
                        "run_id": run_id,
                        "packages": packages,
                    })
                    continue
                tag_dir_id = int(tag_entry.get("fileID") or tag_entry.get("fileId") or 0)
                children = client.list_directory(tag_dir_id)
                names = {str(c.get("filename") or c.get("name") or "").strip() for c in children}
                present = [n for n in EXPECTED_APP_ASSETS if n in names]
                missing = [n for n in EXPECTED_APP_ASSETS if n not in names]
                out.append({
                    "version": version, "tag": tag,
                    "channels": list(v.get("channels", []) or []),
                    "is_auto_version": version == auto_version,
                    "exists": True,
                    "files_present": present,
                    "files_missing": missing,
                    "complete": not missing,
                    "run_id": run_id,
                    "packages": packages,
                })
            # ---- Decoupled bundle scan: lens-* / plugin-* / sdk/ ----
            # Per-component bundle directories are hash-named (immutable
            # contents), so we cache derived fields keyed by `<name>#<fileID>`
            # and re-use across refreshes. SDK is a single flat directory.
            current_lens_tag = str(vercel_envs.get("NIYIEN_LENS_RELEASE_TAG", "")).strip()
            current_plugin_tag = str(vercel_envs.get("NIYIEN_PLUGIN_RELEASE_TAG", "")).strip()
            lens_bundles: list[dict] = []
            plugin_bundles: list[dict] = []
            sdk_status: dict = {
                "exists": False,
                "files_present": [],
                "files_missing": list(EXPECTED_SDK_ASSETS),
                "complete": False,
                "total_size_mb": 0.0,
                "file_count": 0,
            }
            scan_error = ""
            try:
                bundle_cache = load_bundle_cache()
                cache_dirty = False

                root_children = client.list_directory(root_id)

                def _scan_bundle(child: dict, expected_filenames: tuple,
                                 manifest_filename: str, current_tag: str) -> dict:
                    """Scan a single bundle directory; reuse cached manifest fields."""
                    name = str(child.get("filename") or child.get("name") or "").strip()
                    bundle_dir_id = client._entry_id(child)
                    cache_key = f"{name}#{bundle_dir_id}"
                    cached = bundle_cache.get(cache_key)
                    # Cache stores manifest_* fields only — file presence still
                    # has to be re-verified each scan because partial uploads
                    # can leave a directory in a half-populated state that the
                    # cache can't observe.
                    bundle_entries = client.list_directory(bundle_dir_id)
                    file_names = {
                        str(f.get("filename") or f.get("name") or "").strip()
                        for f in bundle_entries
                        if int(f.get("type", -1)) == 0
                    }
                    files_present = [n for n in expected_filenames if n in file_names]
                    files_missing = [n for n in expected_filenames if n not in file_names]
                    has_manifest = manifest_filename in file_names
                    total_size, file_count = client.directory_total_size(bundle_dir_id)

                    manifest_fields: dict = {}
                    manifest_clean = False
                    if cached and not cached.get("manifest_error"):
                        manifest_fields = {k: v for k, v in cached.items()
                                           if k.startswith("manifest_")}
                        manifest_clean = True
                    elif has_manifest:
                        try:
                            manifest_entry = next(
                                (f for f in bundle_entries
                                 if str(f.get("filename") or f.get("name") or "").strip() == manifest_filename
                                 and int(f.get("type", -1)) == 0),
                                None,
                            )
                            if manifest_entry:
                                manifest_fid = client._entry_id(manifest_entry)
                                text = client.fetch_file_text(manifest_fid)
                                import json as _json
                                parsed = _json.loads(text)
                                if isinstance(parsed, dict):
                                    # Surface the manifest fields useful to ops.
                                    # Both lens and plugin manifests share these
                                    # keys in the new schema (see publish
                                    # script's build_lens_manifest /
                                    # build_plugin_manifest).
                                    for k in (
                                        "lens_tag", "lens_hash", "lens_release_tag",
                                        "plugin_tag", "plugin_hash",
                                        "plugin_source_mode", "plugin_source_ref",
                                        "plugins_release_tag",
                                        "generated_at", "kind",
                                    ):
                                        if k in parsed:
                                            manifest_fields[f"manifest_{k}"] = parsed[k]
                                    manifest_clean = True
                        except Exception as merr:
                            manifest_fields = {"manifest_error": str(merr)}

                    bundle = {
                        "tag": name,
                        "fileID": bundle_dir_id,
                        "is_current": bool(current_tag) and name == current_tag,
                        "files_present": files_present,
                        "files_missing": files_missing,
                        "complete": not files_missing,
                        "has_manifest": has_manifest,
                        "file_count": file_count,
                        "expected_count": len(expected_filenames),
                        "total_size_mb": round(total_size / 1024 / 1024, 2),
                        "created_at": str(child.get("createAt") or child.get("createTime") or ""),
                        "from_cache": bool(cached and not cached.get("manifest_error")),
                        **manifest_fields,
                    }
                    if manifest_clean and not cached:
                        # Only cache when the manifest read succeeded fresh.
                        cache_entry = {k: v for k, v in bundle.items()
                                       if k.startswith("manifest_")}
                        bundle_cache[cache_key] = cache_entry
                        nonlocal cache_dirty
                        cache_dirty = True
                    return bundle

                for child in root_children:
                    if int(child.get("type", -1)) != 1:
                        continue
                    name = str(child.get("filename") or child.get("name") or "").strip()
                    if name.startswith("lens-"):
                        if EXPECTED_LENS_FILENAMES:
                            lens_bundles.append(_scan_bundle(
                                child, EXPECTED_LENS_FILENAMES,
                                LENS_MANIFEST_ASSET_NAME, current_lens_tag,
                            ))
                    elif name.startswith("plugin-"):
                        if EXPECTED_PLUGIN_FILENAMES:
                            plugin_bundles.append(_scan_bundle(
                                child, EXPECTED_PLUGIN_FILENAMES,
                                PLUGIN_MANIFEST_ASSET_NAME, current_plugin_tag,
                            ))
                    elif name == "sdk":
                        # SDK = flat dir at releases/sdk/. Files carry version
                        # in their filename so older clients still find their
                        # version even after a new SDK was uploaded.
                        sdk_dir_id = client._entry_id(child)
                        sdk_entries = client.list_directory(sdk_dir_id)
                        sdk_names = {
                            str(f.get("filename") or f.get("name") or "").strip()
                            for f in sdk_entries
                            if int(f.get("type", -1)) == 0
                        }
                        total_size, file_count = client.directory_total_size(sdk_dir_id)
                        present = [n for n in EXPECTED_SDK_ASSETS if n in sdk_names]
                        missing = [n for n in EXPECTED_SDK_ASSETS if n not in sdk_names]
                        sdk_status = {
                            "exists": True,
                            "fileID": sdk_dir_id,
                            "files_present": present,
                            "files_missing": missing,
                            "complete": not missing,
                            "expected_count": len(EXPECTED_SDK_ASSETS),
                            "total_size_mb": round(total_size / 1024 / 1024, 2),
                            "file_count": file_count,
                        }

                if cache_dirty:
                    save_bundle_cache(bundle_cache)

                # Sort each bundle list: current first, then by
                # manifest_generated_at desc / tag asc.
                def _bundle_sort_key(b: dict):
                    return (
                        not b.get("is_current"),  # current first (False sorts before True)
                        -1 * (1 if b.get("manifest_generated_at") else 0),
                        b.get("manifest_generated_at") or "",
                        b.get("tag") or "",
                    )
                lens_bundles.sort(key=_bundle_sort_key)
                plugin_bundles.sort(key=_bundle_sort_key)
            except Exception as exc:
                scan_error = f"{exc.__class__.__name__}: {exc}"

            return {
                "ok": True,
                "auto_version": auto_version,
                "current_lens_tag": current_lens_tag,
                "current_plugin_tag": current_plugin_tag,
                "app_versions": out,
                # `items` kept as alias for older clients that still call it.
                "items": out,
                "lens_bundles": lens_bundles,
                "plugin_bundles": plugin_bundles,
                "sdk_status": sdk_status,
                "scan_error": scan_error,
            }
        except Exception as e:
            return _error(e, "get_pan123_inventory")

    # ---- Real policy mutation (execute_app_action) ----

    def _load_current_policy(self, cfg: dict, vercel: VercelClient, env_records: dict) -> dict:
        """Fetch and decode the current NIYIEN_RELEASE_POLICY_JSON from Vercel.
        Uses the single-env endpoint to get a decrypted value.
        Returns a policy dict or an empty default if nothing exists yet.
        """
        import json as _json
        policy_rec = env_records.get("NIYIEN_RELEASE_POLICY_JSON") or {}
        env_id = str(policy_rec.get("id", "")).strip()
        default = {"auto_version": "", "versions": []}
        if not env_id:
            return default
        try:
            plain = vercel.get_env_value(env_id)
        except Exception as e:
            raise RuntimeError(f"无法读取当前 policy (single-env decrypt 失败): {e}")
        if not plain:
            return default
        try:
            parsed = _json.loads(plain)
        except _json.JSONDecodeError as e:
            raise RuntimeError(f"当前 policy value 不是合法 JSON: {e}")
        if not isinstance(parsed, dict):
            return default
        if not isinstance(parsed.get("versions"), list):
            parsed["versions"] = []
        if "auto_version" not in parsed:
            parsed["auto_version"] = ""
        return parsed

    @staticmethod
    def _upsert_version_entry(versions: list[dict], version: str, tag: str,
                               changelog: str, recommended: bool, channels: list[str],
                               run_id: int = 0,
                               app_source_mode: str = "",
                               app_urls: dict | None = None) -> None:
        """Add or replace the policy entry for `version` in place.

        `run_id` is non-zero for artifact-mode entries — it lets later
        operations (e.g. dashboard-triggered pan123 re-sync) reconstruct
        the `--app-source-mode=artifact --app-run-id=N` invocation
        without the user having to re-pick the run.

        `app_source_mode` ("artifact" or "release") + `app_urls`
        ({platform: {installer_url?, package_url?}}) tell the docs
        manifest API how to build global-region URLs. Without these,
        artifact-mode synthetic tags (run-<id>) leak into the
        release-mode GitHub URL builder and produce a 404.
        """
        entry = {
            "version": version,
            "tag": tag,
            "channels": channels,
            "changelog": changelog,
            "recommended": recommended,
        }
        if int(run_id or 0) > 0:
            entry["run_id"] = int(run_id)
        if app_source_mode:
            entry["app_source_mode"] = app_source_mode
        if app_urls:
            entry["app_urls"] = app_urls
        for i, v in enumerate(versions):
            if v.get("version") == version:
                # Preserve unknown fields (e.g. release_summary fields set previously)
                merged = dict(v)
                merged.update(entry)
                # If we're now release-mode (run_id absent), drop any old run_id
                # and clear artifact-only fields so the docs API stops treating
                # this as artifact mode.
                if int(run_id or 0) <= 0:
                    merged.pop("run_id", None)
                    if not app_source_mode:
                        merged.pop("app_source_mode", None)
                    if not app_urls:
                        merged.pop("app_urls", None)
                versions[i] = merged
                return
        versions.append(entry)

    def _finalize_publish_to_manifest(self, cfg: dict, policy_json: str,
                                       finalize_summary: dict | None = None,
                                       *, target_version: str = "",
                                       extra_envs: dict | None = None) -> str:
        """Two-phase commit step 2: push policy to Vercel + trigger deploy hook.

        Called either inline (manual_only / hide_version actions, no
        finalize_summary) or from the pan123 task success callback
        (publish_and_push / switch_auto / rollback_auto, with summary).

        When `finalize_summary` is provided, augment the staged policy
        entry with content_tag / lens_release_tag / plugin_source_* and
        upsert NIYIEN_LENS_VERSION / NIYIEN_LENS_SHA256 /
        NIYIEN_CONTENT_RELEASE_TAG envs alongside the policy. Without
        these, the manifest API can't construct plugins_base or surface
        a non-zero lens.version to clients.

        Vercel upsert is retried with exponential backoff because by the
        time we call this from the pan123 success path, the operator has
        already waited 5-30 minutes for upload — a single transient
        Vercel hiccup shouldn't force a re-publish.
        """
        import json as _json
        import time as _time

        # Merge runtime-only fields (lens_tag/plugin_tag, lens metadata,
        # plugin source refs) into the policy entry for `auto_version`.
        # Decoupled layout: lens_tag and plugin_tag are independent and
        # may be missing from finalize_summary when scope was narrower
        # (e.g. scope=["plugin"] only carries plugin_tag). We mutate
        # only fields actually present so a partial publish doesn't
        # clobber the other component's existing tag.
        upsert_map = {"NIYIEN_RELEASE_POLICY_JSON": policy_json}
        if finalize_summary:
            try:
                policy = _json.loads(policy_json)
                auto_v = str(target_version or policy.get("auto_version", "")).strip()
                target = next(
                    (v for v in policy.get("versions", []) if v.get("version") == auto_v),
                    None,
                )
                if target is not None:
                    if finalize_summary.get("lens_tag"):
                        target["lens_tag"] = str(finalize_summary["lens_tag"])
                    if finalize_summary.get("plugin_tag"):
                        target["plugin_tag"] = str(finalize_summary["plugin_tag"])
                    if finalize_summary.get("lens_release_tag"):
                        target["lens_release_tag"] = str(finalize_summary["lens_release_tag"])
                    if finalize_summary.get("lens_version") is not None:
                        target["lens_version"] = finalize_summary["lens_version"]
                    if finalize_summary.get("lens_sha256"):
                        target["lens_sha256"] = str(finalize_summary["lens_sha256"])
                    if finalize_summary.get("plugins_release_tag"):
                        target["plugins_release_tag"] = str(finalize_summary["plugins_release_tag"])
                    if finalize_summary.get("plugin_source_mode"):
                        target["plugins_source_mode"] = str(finalize_summary["plugin_source_mode"])
                    if finalize_summary.get("plugin_source_ref"):
                        target["plugins_source_ref"] = str(finalize_summary["plugin_source_ref"])
                    if finalize_summary.get("plugin_source_tag"):
                        target["plugins_source_tag"] = str(finalize_summary["plugin_source_tag"])
                    # Without this mirror, docs manifest Global+artifact branch
                    # has no nightly.link base for plugins and falls back to
                    # release-latest, which is wrong for nightly publishes.
                    if finalize_summary.get("global_plugins_base"):
                        target["global_plugins_base"] = str(finalize_summary["global_plugins_base"])
                    packages = finalize_summary.get("packages")
                    if isinstance(packages, dict):
                        target["packages"] = packages
                upsert_map["NIYIEN_RELEASE_POLICY_JSON"] = _json.dumps(
                    policy, ensure_ascii=False, indent=2,
                )
            except Exception:
                # If anything goes wrong merging, fall back to the bare
                # staged_policy_json — manifest will still push, but
                # plugins_base / lens fields may be empty until the next
                # publish.
                pass

            # Top-level envs that the manifest API also consults. Only
            # write the ones that the publish actually produced — a
            # plugin-only publish must not nuke NIYIEN_LENS_RELEASE_TAG.
            if finalize_summary.get("lens_tag"):
                upsert_map["NIYIEN_LENS_RELEASE_TAG"] = str(finalize_summary["lens_tag"])
            if finalize_summary.get("plugin_tag"):
                upsert_map["NIYIEN_PLUGIN_RELEASE_TAG"] = str(finalize_summary["plugin_tag"])
            if finalize_summary.get("lens_version") is not None:
                upsert_map["NIYIEN_LENS_VERSION"] = str(finalize_summary["lens_version"])
            if finalize_summary.get("lens_sha256"):
                upsert_map["NIYIEN_LENS_SHA256"] = str(finalize_summary["lens_sha256"])
        if extra_envs:
            for key, value in extra_envs.items():
                if key:
                    upsert_map[str(key)] = "" if value is None else str(value)

        last_err: Exception | None = None
        for attempt in range(3):
            try:
                self._vercel(cfg).upsert_envs(upsert_map)
                last_err = None
                break
            except Exception as e:
                last_err = e
                if attempt < 2:
                    _time.sleep(1.5 * (2 ** attempt))  # 1.5s, 3s
        if last_err is not None:
            raise RuntimeError(f"upsert envs failed after 3 attempts: {last_err}")
        try:
            return self._trigger_deploy_hook(cfg)
        except Exception as e:
            return f"deploy hook failed: {e}"

    def _trigger_deploy_hook(self, cfg: dict) -> str:
        """Force a fresh production deployment so upserted env vars actually
        take effect. Try the configured deploy hook first; fall back to the
        Vercel REST API (clone the latest production deployment's gitSource
        into a new one) when no hook is configured or the hook call fails.
        """
        import requests
        from .helpers import build_proxy_mapping
        proxies = build_proxy_mapping(cfg.get("network_proxy", ""))
        url = str(cfg.get("deploy_hook_url", "")).strip()
        hook_err: Exception | None = None
        if url:
            try:
                r = requests.post(url, timeout=30, proxies=proxies)
                r.raise_for_status()
                return f"deploy hook triggered ({r.status_code})"
            except Exception as e:
                hook_err = e
        try:
            self._vercel(cfg).redeploy_production()
            if hook_err is not None:
                return f"deploy hook failed ({hook_err}); fell back to Vercel REST API redeploy"
            return "deploy hook absent; redeployed via Vercel REST API"
        except Exception as e:
            if hook_err is not None:
                raise RuntimeError(f"hook failed ({hook_err}); REST fallback failed: {e}") from e
            raise

    @staticmethod
    def _normalize_release_plan_resources(payload: dict) -> dict:
        resources = payload.get("resources") if isinstance(payload.get("resources"), dict) else {}

        def _pick(key: str, default: str = "") -> str:
            value = resources.get(key, payload.get(key, default))
            return str(value or "").strip()

        def _pick_bool(key: str, default: bool = True) -> bool:
            value = resources.get(key, payload.get(key, default))
            if isinstance(value, bool):
                return value
            text = str(value).strip().lower()
            if text in {"0", "false", "no", "off"}:
                return False
            if text in {"1", "true", "yes", "on"}:
                return True
            return default

        plugin_mode = (_pick("plugin_mode", "release") or "release").lower()
        return {
            "lens_tag": _pick("lens_tag"),
            "plugin_mode": plugin_mode,
            "plugin_tag": _pick("plugin_tag"),
            "plugin_artifact_name": _pick("plugin_artifact_name"),
            "plugin_run_id": _pick("plugin_run_id"),
            "sdk_base": _pick("sdk_base"),
            "include_sdk": _pick_bool("include_sdk", True),
        }

    @staticmethod
    def _normalize_release_plan_app(payload: dict) -> dict:
        source_kind = str(payload.get("source_kind", payload.get("kind", ""))).strip().lower()
        version = str(payload.get("version", "")).strip()
        tag = str(payload.get("tag", "")).strip()
        try:
            run_id = int(payload.get("run_id", payload.get("runId", 0)) or 0)
        except (TypeError, ValueError):
            run_id = 0
        if source_kind == "artifact" and run_id > 0 and not tag:
            tag = f"run-{run_id}"
        return {
            "action": str(payload.get("action", "")).strip(),
            "source_kind": source_kind,
            "version": version,
            "tag": tag,
            "run_id": run_id,
            "changelog": str(payload.get("changelog", "")).strip(),
            "recommended": payload.get("recommended") if "recommended" in payload else None,
        }

    @staticmethod
    def _normalize_scope(raw_scope) -> list[str]:
        if isinstance(raw_scope, str):
            items = [item.strip().lower() for item in raw_scope.split(",") if item.strip()]
        elif isinstance(raw_scope, (list, tuple, set)):
            items = [str(item).strip().lower() for item in raw_scope if str(item).strip()]
        else:
            items = []
        seen: list[str] = []
        for item in items:
            if item not in seen:
                seen.append(item)
        return seen

    def execute_release_plan(self, payload: dict) -> dict:
        try:
            return self._execute_release_plan(payload or {})
        except Exception as e:
            return _error(e, "execute_release_plan")

    def _execute_release_plan(self, payload: dict) -> dict:
        import json as _json

        plan = self._normalize_release_plan_app(payload)
        resources = self._normalize_release_plan_resources(payload)
        action = plan["action"]
        if action in {"hide_version"}:
            return self.execute_app_action(payload)
        rollback_auto = action == "rollback_auto"
        if rollback_auto:
            action = "switch_auto"
            plan["recommended"] = True

        version = plan["version"]
        if not version:
            return {"ok": False, "error": "缺少 version"}
        tag = plan["tag"]
        if not tag and action in {"manual_only", "publish_and_push", "switch_auto"}:
            return {"ok": False, "error": "缺少 tag"}

        cfg = config_module.load_config()
        vercel = self._vercel(cfg)
        env_records = vercel.list_env_records()
        policy = self._load_current_policy(cfg, vercel, env_records)
        versions = policy.get("versions", [])

        app_source_mode_field = ""
        app_urls_field: dict | None = None
        if plan["source_kind"] == "artifact" and plan["run_id"] > 0:
            app_source_mode_field = "artifact"
            if _PUB_MODULE is not None:
                app_urls_field = _PUB_MODULE.build_global_artifact_app_urls(
                    tag,
                    _PUB_MODULE.REQUIRED_APP_ASSET_NAMES,
                )

        already_present = any(v.get("version") == version for v in versions)
        if action == "manual_only":
            self._upsert_version_entry(
                versions, version, tag, plan["changelog"], bool(plan["recommended"]), ["manual"],
                run_id=plan["run_id"],
                app_source_mode=app_source_mode_field,
                app_urls=app_urls_field,
            )
        elif action == "publish_and_push":
            self._upsert_version_entry(
                versions, version, tag, plan["changelog"], bool(plan["recommended"]), ["auto", "manual"],
                run_id=plan["run_id"],
                app_source_mode=app_source_mode_field,
                app_urls=app_urls_field,
            )
            for item in versions:
                if item.get("version") != version and "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item.get("channels", []) if c != "auto"] or ["manual"]
            policy["auto_version"] = version
        elif action == "switch_auto":
            if not already_present:
                return {
                    "ok": False,
                    "error": f"版本 {version} 不在 policy.versions 白名单中,请先发布或加入手动列表",
                }
            for item in versions:
                if item.get("version") == version:
                    item["channels"] = sorted(set(item.get("channels", []) + ["auto", "manual"]))
                    if plan["recommended"] is not None:
                        item["recommended"] = plan["recommended"]
                    if rollback_auto:
                        item["recommended"] = True
                    if tag and not str(item.get("tag", "")).strip():
                        item["tag"] = tag
                    if plan["run_id"] > 0 and int(item.get("run_id", 0) or 0) <= 0:
                        item["run_id"] = plan["run_id"]
                    if app_source_mode_field and not str(item.get("app_source_mode", "")).strip():
                        item["app_source_mode"] = app_source_mode_field
                    if app_urls_field and not item.get("app_urls"):
                        item["app_urls"] = app_urls_field
                elif "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item.get("channels", []) if c != "auto"] or ["manual"]
            policy["auto_version"] = version
        else:
            return {"ok": False, "error": f"未知发布动作: {action}"}

        policy["versions"].sort(key=lambda x: x.get("version", ""), reverse=True)
        staged_policy_json = _json.dumps(policy, ensure_ascii=False, indent=2)

        scope_list = self._normalize_scope(payload.get("scope"))
        if not scope_list:
            if tag:
                scope_list.append("app")
            if resources.get("lens_tag"):
                scope_list.append("lens")
            if resources.get("plugin_mode") == "artifact":
                if resources.get("plugin_artifact_name"):
                    scope_list.append("plugin")
            elif resources.get("plugin_tag"):
                scope_list.append("plugin")
        scope_list = [item for item in scope_list if item in {"app", "lens", "plugin"}]
        if not scope_list:
            return {"ok": False, "error": "没有可执行的发布内容"}

        publish_overrides = {k: v for k, v in resources.items() if k != "include_sdk"}
        if not publish_overrides.get("plugin_mode"):
            publish_overrides["plugin_mode"] = "release"
        if not resources.get("include_sdk", True):
            publish_overrides["sdk_base"] = ""

        scope_set = set(scope_list)

        def _extra_envs() -> dict[str, str]:
            plugin_mode = str(resources.get("plugin_mode", "release")).strip().lower() or "release"
            lens_tag = str(resources.get("lens_tag", "")).strip()
            plugin_tag = str(resources.get("plugin_tag", "")).strip()
            plugin_artifact_name = str(resources.get("plugin_artifact_name", "")).strip()
            plugin_run_id = str(resources.get("plugin_run_id", "")).strip()
            sdk_base = str(resources.get("sdk_base", "")).strip()
            include_sdk = bool(resources.get("include_sdk", True))
            extra: dict[str, str] = {}
            if "lens" in scope_set and lens_tag:
                extra["NIYIEN_LENS_DATA_TAG"] = lens_tag
            if "plugin" in scope_set:
                if plugin_mode == "release" and plugin_tag:
                    extra["NIYIEN_PLUGINS_SOURCE_MODE"] = "release"
                    extra["NIYIEN_PLUGINS_TAG"] = plugin_tag
                    extra["NIYIEN_PLUGINS_ARTIFACT_NAME"] = ""
                    extra["NIYIEN_PLUGINS_RUN_ID"] = ""
                elif plugin_mode == "artifact" and plugin_artifact_name:
                    extra["NIYIEN_PLUGINS_SOURCE_MODE"] = "artifact"
                    extra["NIYIEN_PLUGINS_TAG"] = ""
                    extra["NIYIEN_PLUGINS_ARTIFACT_NAME"] = plugin_artifact_name
                    extra["NIYIEN_PLUGINS_RUN_ID"] = plugin_run_id
                if include_sdk and sdk_base:
                    extra["NIYIEN_SDK_BASE"] = sdk_base
            elif "lens" in scope_set and include_sdk and sdk_base:
                extra["NIYIEN_SDK_BASE"] = sdk_base
            return extra

        base_message = (
            f"已执行 {action} · policy.versions 共 {len(policy['versions'])} 条 · "
            f"auto_version={policy.get('auto_version') or '(空)'}"
        )
        result = {
            "ok": True,
            "action": action,
            "version": version,
            "auto_version": policy.get("auto_version", ""),
            "versions_count": len(policy["versions"]),
            "staged_until_pan123": False,
            "plan_scope": scope_list,
        }

        effective_run_id = plan["run_id"] or int(
            next((v.get("run_id", 0) for v in policy.get("versions", []) if v.get("version") == version), 0) or 0
        )

        def _on_pan123_finalize(finalize_summary: dict) -> str:
            return self._finalize_publish_to_manifest(
                cfg,
                staged_policy_json,
                finalize_summary,
                target_version=version,
                extra_envs=_extra_envs(),
            )

        pan_result = self._start_pan123_publish(
            app_tag=tag,
            app_version=version,
            app_run_id=effective_run_id,
            scope=scope_list,
            publish_overrides=publish_overrides,
            on_finalize=_on_pan123_finalize,
        )
        if pan_result.get("ok"):
            result["pan123_task_token"] = pan_result["token"]
            result["pan123_target_tag"] = tag
            result["staged_until_pan123"] = True
            result["message"] = (
                f"{base_message} · pan123 同步已启动 (token={pan_result['token'][:8]}); "
                f"上传成功后才会自动推送 manifest"
            )
        else:
            result["pan123_error"] = pan_result.get("error", "pan123 同步启动失败")
            result["ok"] = False
            result["error"] = result["pan123_error"]
        return result

    def execute_app_action(self, payload: dict) -> dict:
        """One of 5 publish actions — real policy mutation + Vercel upsert.

        manual_only       — add version to policy.versions (channels=['manual']), leave auto_version
        publish_and_push  — add + set auto_version to this version (channels=['auto','manual']), clear others' auto
        switch_auto       — version must already be in policy.versions; switch auto_version to it
        rollback_auto     — legacy action id; same behavior as switch_auto but also forces recommended=True
        hide_version      — remove from policy.versions; if it was auto_version, pick next available

        Scope (by design): this method only mutates policy JSON and upserts to Vercel. It
        does NOT:
        - upload anything to PAN123 (use a separate publish script)
        - sync NIYIEN_CONTENT_RELEASE_TAG / NIYIEN_LENS_* / NIYIEN_PLUGINS_* / NIYIEN_SDK_BASE
          envs — for that use "资源编排 → 立即切换当前内容版本"
        - fetch and merge GitHub release summary assets
        """
        import json as _json
        try:
            action = str(payload.get("action", "")).strip()
            valid = {"manual_only", "publish_and_push", "switch_auto", "rollback_auto", "hide_version"}
            if action not in valid:
                return {"ok": False, "error": f"未知发布动作: {action}"}
            if action == "manual_only":
                return self.execute_release_plan(payload)
            version = str(payload.get("version", "")).strip()
            if not version:
                return {"ok": False, "error": "缺少 version"}
            tag = str(payload.get("tag", "")).strip()
            source_kind = str(payload.get("source_kind", "")).strip().lower()
            run_id = int(payload.get("run_id", 0) or 0)
            # Artifact-mode entries don't have a real GitHub release tag.
            # Synthesize a `run-<run_id>` virtual tag — this is the same
            # name `pan123.ensure_release_dir(app_tag)` will use as the 123
            # 网盘 directory, so cn manifest URLs (download.niyien.com/
            # releases/<tag>/<asset>) line up with what's actually uploaded.
            if source_kind == "artifact" and run_id > 0 and not tag:
                tag = f"run-{run_id}"
            changelog = str(payload.get("changelog", "")).strip()
            recommended = bool(payload.get("recommended", False))

            cfg = config_module.load_config()
            # Build app_source_mode + app_urls for artifact-mode entries so
            # docs manifest can serve global users through the 123 download route.
            # Without these the entry looks like a release-mode entry, and
            # docs builds github.com/.../releases/download/run-<id>/<asset>
            # which 404s because the synthetic tag has no GitHub release.
            app_source_mode_field = ""
            app_urls_field: dict | None = None
            if source_kind == "artifact" and run_id > 0:
                app_source_mode_field = "artifact"
                if _PUB_MODULE is not None:
                    app_urls_field = _PUB_MODULE.build_global_artifact_app_urls(
                        tag,
                        _PUB_MODULE.REQUIRED_APP_ASSET_NAMES,
                    )
            vercel = self._vercel(cfg)
            env_records = vercel.list_env_records()
            policy = self._load_current_policy(cfg, vercel, env_records)
            versions = policy.get("versions", [])
            already_present = any(v.get("version") == version for v in versions)

            if action == "manual_only":
                self._upsert_version_entry(
                    versions, version, tag, changelog, recommended, ["manual"],
                    run_id=run_id,
                    app_source_mode=app_source_mode_field,
                    app_urls=app_urls_field,
                )

            elif action == "publish_and_push":
                self._upsert_version_entry(
                    versions, version, tag, changelog, recommended, ["auto", "manual"],
                    run_id=run_id,
                    app_source_mode=app_source_mode_field,
                    app_urls=app_urls_field,
                )
                # Clear auto channel from other versions
                for v in versions:
                    if v.get("version") != version and "auto" in v.get("channels", []):
                        v["channels"] = [c for c in v.get("channels", []) if c != "auto"] or ["manual"]
                policy["auto_version"] = version

            elif action == "switch_auto":
                if not already_present:
                    return {
                        "ok": False,
                        "error": f"版本 {version} 不在 policy.versions 白名单中,请先用 "
                                 f"'加入手动版本列表(不推送)' 或 '发布并立即自动推送' 将它加入",
                    }
                for v in versions:
                    if v.get("version") == version:
                        v["channels"] = sorted(set(v.get("channels", []) + ["auto", "manual"]))
                        if payload.get("recommended") is not None:
                            v["recommended"] = recommended
                        # Backfill tag/run_id for legacy entries that were
                        # written before artifact-mode synthesized a tag.
                        if tag and not str(v.get("tag", "")).strip():
                            v["tag"] = tag
                        if run_id > 0 and int(v.get("run_id", 0) or 0) <= 0:
                            v["run_id"] = run_id
                        # Backfill artifact-mode fields for legacy entries
                        # written before docs needed app_source_mode/app_urls.
                        if app_source_mode_field and not str(v.get("app_source_mode", "")).strip():
                            v["app_source_mode"] = app_source_mode_field
                        if app_urls_field and not v.get("app_urls"):
                            v["app_urls"] = app_urls_field
                    elif "auto" in v.get("channels", []):
                        v["channels"] = [c for c in v.get("channels", []) if c != "auto"] or ["manual"]
                policy["auto_version"] = version

            elif action == "rollback_auto":
                if not already_present:
                    return {"ok": False, "error": f"版本 {version} 不在白名单,无法切换到此版本"}
                for v in versions:
                    if v.get("version") == version:
                        v["channels"] = sorted(set(v.get("channels", []) + ["auto", "manual"]))
                        v["recommended"] = True
                        if tag and not str(v.get("tag", "")).strip():
                            v["tag"] = tag
                        if run_id > 0 and int(v.get("run_id", 0) or 0) <= 0:
                            v["run_id"] = run_id
                        if app_source_mode_field and not str(v.get("app_source_mode", "")).strip():
                            v["app_source_mode"] = app_source_mode_field
                        if app_urls_field and not v.get("app_urls"):
                            v["app_urls"] = app_urls_field
                    elif "auto" in v.get("channels", []):
                        v["channels"] = [c for c in v.get("channels", []) if c != "auto"] or ["manual"]
                policy["auto_version"] = version

            elif action == "hide_version":
                policy["versions"] = [v for v in versions if v.get("version") != version]

            # Sort versions desc by version string, like legacy did. We sort
            # FIRST and only then pick the auto_version fallback for hide —
            # otherwise hide_version ends up promoting whatever happened to
            # be unsorted-first instead of the highest remaining version.
            policy["versions"].sort(key=lambda x: x.get("version", ""), reverse=True)

            if action == "hide_version" and policy.get("auto_version") == version:
                policy["auto_version"] = (
                    policy["versions"][0].get("version", "") if policy["versions"] else ""
                )

            # Stage policy_json — it will be pushed to Vercel either inline
            # (no pan123 needed) or from the pan123 task success callback
            # (after upload completes, so cn clients see new manifest only
            # when 123 网盘 already has the files).
            staged_policy_json = _json.dumps(policy, ensure_ascii=False, indent=2)

            base_message = (
                f"已执行 {action} · policy.versions 共 {len(policy['versions'])} 条 · "
                f"auto_version={policy.get('auto_version') or '(空)'}"
            )
            result = {
                "ok": True,
                "action": action,
                "version": version,
                "auto_version": policy.get("auto_version", ""),
                "versions_count": len(policy["versions"]),
                "staged_until_pan123": False,
            }

            needs_pan123 = action in PAN123_AUTO_PUBLISH_ACTIONS and tag
            if needs_pan123:
                # For switch_auto / rollback_auto the entry was created earlier;
                # reach back into policy.versions[] to recover its run_id (if any)
                # so artifact-mode entries also fire artifact-mode pan123 sync.
                target_entry = next(
                    (v for v in policy.get("versions", []) if v.get("version") == version),
                    {},
                )
                effective_run_id = run_id or int(target_entry.get("run_id", 0) or 0)

                # Two-phase commit: pan123 first, then manifest. The
                # finalize hook runs inside the task runner so its
                # progress shows in the log panel and any failure marks
                # the whole task as failed (manifest never pushed).
                policy_to_push = staged_policy_json
                cfg_for_finalize = cfg
                def _on_pan123_finalize(finalize_summary: dict) -> str:
                    return self._finalize_publish_to_manifest(
                        cfg_for_finalize, policy_to_push, finalize_summary,
                    )

                # Strict scope: app-only. execute_app_action publishes a new
                # gyroflow version — lens / plugin are managed independently
                # via the resources-view scoped publish buttons. Without this,
                # an app release would also re-pull whatever NIYIEN_LENS_DATA_TAG
                # currently points at and overwrite NIYIEN_LENS_RELEASE_TAG /
                # NIYIEN_PLUGIN_RELEASE_TAG, leaking unrelated resource changes
                # into an app-only release.
                pan_result = self._start_pan123_publish(
                    app_tag=tag, app_version=version, app_run_id=effective_run_id,
                    scope=["app"],
                    on_finalize=_on_pan123_finalize,
                )
                if pan_result.get("ok"):
                    result["pan123_task_token"] = pan_result["token"]
                    result["pan123_target_tag"] = tag
                    result["staged_until_pan123"] = True
                    result["message"] = (
                        f"{base_message} · pan123 同步已启动 (token={pan_result['token'][:8]}); "
                        f"上传成功后才会自动推送 manifest"
                    )
                else:
                    # pan123 didn't even start (e.g. missing creds). Don't
                    # push policy either — the operator likely wants to
                    # fix creds and retry, not commit to a manifest the
                    # files won't be there for.
                    result["pan123_error"] = pan_result.get("error", "pan123 同步启动失败")
                    result["message"] = (
                        f"{base_message} · ⚠ pan123 启动失败 (manifest 未推送): {result['pan123_error']}"
                    )
            else:
                # Actions that don't move release artifacts (manual_only,
                # hide_version): push manifest immediately.
                hook_note = self._finalize_publish_to_manifest(cfg, staged_policy_json)
                result["deploy_hook"] = hook_note
                result["message"] = f"{base_message} · {hook_note}"

            return result
        except Exception as e:
            return _error(e, "execute_app_action")

    # ---- Telemetry ----

    def fetch_stats(self, days: int = 7, event: str = "") -> dict:
        try:
            cfg = config_module.load_config()
            data = telemetry_api.fetch_stats(
                base_url=cfg.get("telemetry_base_url", ""),
                token=cfg.get("telemetry_stats_token", ""),
                days=days,
                event=event,
                proxy_url=cfg.get("network_proxy", ""),
            )
            return {"ok": True, "data": data}
        except Exception as e:
            return _error(e, "fetch_stats")

    def trigger_rebuild(self, start_day: str, end_day: str) -> dict:
        try:
            cfg = config_module.load_config()
            data = telemetry_api.trigger_rebuild(
                base_url=cfg.get("telemetry_base_url", ""),
                token=cfg.get("telemetry_rebuild_token", ""),
                start_day=start_day,
                end_day=end_day,
                proxy_url=cfg.get("network_proxy", ""),
            )
            return {"ok": True, "data": data}
        except Exception as e:
            return _error(e, "trigger_rebuild")

    def preview_manifest(self, country: str = "CN", platform: str = "windows") -> dict:
        try:
            cfg = config_module.load_config()
            data = telemetry_api.fetch_manifest(
                base_url=cfg.get("telemetry_base_url", ""),
                country=country,
                platform=platform,
                proxy_url=cfg.get("network_proxy", ""),
            )
            return {"ok": True, "data": data}
        except Exception as e:
            return _error(e, "preview_manifest")

    def _trace(self) -> str:
        return traceback.format_exc()
