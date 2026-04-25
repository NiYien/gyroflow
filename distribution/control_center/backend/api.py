"""Api class — methods exposed to JS via pywebview's `js_api` bridge.

Every method returns a dict with an `ok: bool` key plus data or `error: str`
so the JS side can handle success and failure uniformly without relying on
exception plumbing through the bridge.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import traceback
from datetime import datetime
from pathlib import Path

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
# only). Content bundle (lens/plugin/sdk) lives under a separate `content-*`
# directory whose name is a hash, so we list it independently.
EXPECTED_APP_ASSETS = (
    "gyroflow-niyien-windows64.zip",
    "gyroflow-niyien-mac-universal.dmg",
    "gyroflow-niyien-linux64.AppImage",
    "gyroflow-niyien.apk",
)
CONTENT_MANIFEST_ASSET_NAME = "gyroflow-niyien-content-manifest.json"


def _error(exc: Exception, context: str) -> dict:
    return {
        "ok": False,
        "error": f"{context}: {exc}",
        "type": exc.__class__.__name__,
    }


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
        state["lens"] = {
            "tag": lens_tag,
            "version": str(envs.get("NIYIEN_LENS_VERSION", "")).strip(),
            "source": "vercel" if lens_tag_env else ("defaults" if lens_tag else "none"),
        }

        plugin_mode_env = str(envs.get("NIYIEN_PLUGINS_SOURCE_MODE", "")).strip().lower()
        plugin_mode = plugin_mode_env or str(defaults.get("plugins_source_mode", "release")).strip().lower() or "release"
        plugin_tag_env = str(envs.get("NIYIEN_PLUGINS_TAG", "")).strip()
        plugin_tag = plugin_tag_env or str(defaults.get("plugins_tag", "")).strip()
        plugin_artifact_env = str(envs.get("NIYIEN_PLUGINS_ARTIFACT_NAME", "")).strip()
        plugin_artifact = plugin_artifact_env or str(defaults.get("plugins_artifact_name", "")).strip()
        has_plugin_env = bool(plugin_mode_env or plugin_tag_env or plugin_artifact_env)
        state["plugin"] = {
            "mode": plugin_mode,
            "tag": plugin_tag,
            "artifact_name": plugin_artifact,
            "source": "vercel" if has_plugin_env else ("defaults" if (plugin_tag or plugin_artifact or plugin_mode_env) else "none"),
        }

        sdk_base_env = str(envs.get("NIYIEN_SDK_BASE", "")).strip()
        sdk_base = sdk_base_env or str(defaults.get("sdk_base", "")).strip()
        state["sdk"] = {
            "base": sdk_base,
            "source": "vercel" if sdk_base_env else ("defaults" if sdk_base else "none"),
        }

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
            # Plugin update detection only meaningful in release mode —
            # artifact mode tracks Action runs by id, not release tag.
            plugin_state = state.get("plugin") or {}
            if str(plugin_state.get("mode", "")).lower() == "release":
                pl_owner = str(cfg.get("plugins_owner", "")).strip()
                pl_repo = str(cfg.get("plugins_repo", "")).strip()
                if pl_owner and pl_repo:
                    pl_releases = self._gh_for(pl_owner, pl_repo, cfg).list_repo_releases(
                        pl_owner, pl_repo,
                    )
                    latest_pl = next(
                        (r for r in pl_releases
                         if not r.get("draft") and not r.get("prerelease")),
                        pl_releases[0] if pl_releases else None,
                    )
                    if latest_pl:
                        latest_tag = str(latest_pl.get("tag_name", "")).strip()
                        current_tag = str(plugin_state.get("tag", "")).strip()
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
            if git_ops.local_tag_exists(REPO_ROOT, tag):
                # Give enough context to decide: is this the same commit we're on? or a legacy upstream tag?
                try:
                    existing_sha = git_ops.run_git(REPO_ROOT, "rev-list", "-n", "1", tag).stdout.strip()[:8]
                except Exception:
                    existing_sha = "unknown"
                head_sha = git_ops.get_head_commit_sha(REPO_ROOT)[:8]
                remote_has = git_ops.remote_tag_exists(REPO_ROOT, remote, tag)
                hint = "与当前 HEAD 一致" if existing_sha == head_sha else f"指向历史 commit (当前 HEAD 是 {head_sha})"
                remote_note = f"{remote} 远端{'已有' if remote_has else '没有'}此 tag"
                return {
                    "ok": False,
                    "error": (
                        f"本地已存在 tag {tag} → commit {existing_sha} ({hint});{remote_note}。\n"
                        f"处理方式: (1) 换版本号 (默认建议已 +1);或 (2) 若要覆盖:"
                        f"`git tag -d {tag}` 删本地后再点打 tag"
                    ),
                }
            if git_ops.remote_tag_exists(REPO_ROOT, remote, tag):
                return {"ok": False, "error": f"远端 {remote} 已存在 Tag: {tag}"}
            git_ops.create_and_push_tag(REPO_ROOT, remote, tag)
            return {"ok": True, "tag": tag, "message": f"Tag {tag} 已推送到 {remote}"}
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

    def _push_tag_via_api_or_clone(self, repo_folder_name: str, owner: str, repo: str, tag: str, cfg: dict) -> dict:
        """Preferred: GitHub API (zero local dependency).
        Fallback: local clone + git push (reuses system git credentials).
        """
        # --- Try API first ---
        api_error: str | None = None
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

        # --- Fallback: local clone ---
        local = self._find_local_clone(repo_folder_name)
        if not local:
            return {
                "ok": False,
                "error": f"API 失败 ({api_error}),且未找到本地 clone {repo_folder_name}/",
            }
        remote = (cfg.get("git_remote", "origin") or "origin").strip()
        if git_ops.local_tag_exists(local, tag):
            return {"ok": False, "error": f"本地 {local.name} 已存在 tag: {tag} (API 错误: {api_error})"}
        if git_ops.remote_tag_exists(local, remote, tag):
            return {"ok": False, "error": f"{owner}/{repo} 远端已存在 tag: {tag} (API 错误: {api_error})"}
        try:
            git_ops.create_and_push_tag(local, remote, tag)
        except subprocess.CalledProcessError as e:
            return {
                "ok": False,
                "error": f"API 失败 ({api_error});本地 git 也失败: {e.stderr or e.stdout or str(e)}",
            }
        return {
            "ok": True,
            "tag": tag,
            "repo": f"{owner}/{repo}",
            "via": "local-clone-fallback",
            "workdir": str(local),
            "api_error": api_error,
            "message": f"API 失败({api_error}),已 fallback 到本地 {local.name} 打 tag 并 push 到 {remote}",
        }

    def create_plugin_tag(self, major: int, minor: int, patch: int) -> dict:
        """Create `v<major>.<minor>.<patch>` tag on plugin repo.

        Prefers local clone at `<gyroflow parent>/gyroflow-plugins` + git push
        (reusing the system git credentials you already use for gyroflow).
        Falls back to GitHub API if the local clone isn't present.
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
            return self._push_tag_via_api_or_clone(repo, owner, repo, f"v{major}.{minor}.{patch}", cfg)
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
                    "NIYIEN_CONTENT_RELEASE_TAG": envs.get("NIYIEN_CONTENT_RELEASE_TAG", ""),
                    "NIYIEN_LENS_VERSION": envs.get("NIYIEN_LENS_VERSION", ""),
                    "NIYIEN_LENS_DATA_TAG": envs.get("NIYIEN_LENS_DATA_TAG", ""),
                    "NIYIEN_PLUGINS_SOURCE_MODE": envs.get("NIYIEN_PLUGINS_SOURCE_MODE", ""),
                    "NIYIEN_PLUGINS_TAG": envs.get("NIYIEN_PLUGINS_TAG", ""),
                    "NIYIEN_PLUGINS_ARTIFACT_NAME": envs.get("NIYIEN_PLUGINS_ARTIFACT_NAME", ""),
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
        """
        try:
            lens_tag = str(payload.get("lens_tag", "")).strip()
            plugin_mode = str(payload.get("plugin_mode", "")).strip().lower() or "release"
            plugin_tag = str(payload.get("plugin_tag", "")).strip()
            plugin_artifact = str(payload.get("plugin_artifact_name", "")).strip()
            sdk_base = str(payload.get("sdk_base", "")).strip()
            if not lens_tag:
                return {"ok": False, "error": "Lens Tag 不能为空"}
            if plugin_mode not in ("release", "artifact"):
                return {"ok": False, "error": f"plugin_mode 必须是 release/artifact,不是 {plugin_mode}"}
            if plugin_mode == "release" and not plugin_tag:
                return {"ok": False, "error": "release 模式下 Plugin Tag 不能为空"}
            cfg = config_module.load_config()
            mapping = {
                "NIYIEN_LENS_DATA_TAG": lens_tag,
                "NIYIEN_PLUGINS_SOURCE_MODE": plugin_mode,
                "NIYIEN_PLUGINS_TAG": plugin_tag if plugin_mode == "release" else "",
                "NIYIEN_PLUGINS_ARTIFACT_NAME": plugin_artifact if plugin_mode == "artifact" else "",
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
            self._vercel(cfg).upsert_envs(mapping)
            extras_note = f" (lens metadata: {', '.join(lens_extras)})" if lens_extras else " (lens metadata 未读到)"
            return {"ok": True, "message": f"已 upsert {len(mapping)} 个 env 到 Vercel{extras_note}"}
        except Exception as e:
            return _error(e, "apply_resources_now")

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
                                      app_run_id: int = 0) -> list[str]:
        """Construct the publish_pan123_release.py CLI.

        `app_run_id > 0` switches to artifact mode (`--app-source-mode=artifact
        --app-run-id=N`); zero stays release mode. The `app_tag` is still
        required in either mode because the script uses it to name the 123
        网盘 directory (`pan123.ensure_release_dir(app_tag)` at script L795).
        """
        script_path = REPO_ROOT / "_scripts" / "publish_pan123_release.py"
        # Lens / plugin / sdk pulled from current Vercel env values (same as
        # what the manifest API will hand out — keeps cn 123 mirror in sync).
        lens_tag = str(vercel_envs.get("NIYIEN_LENS_DATA_TAG", "")).strip()
        plugins_mode = str(vercel_envs.get("NIYIEN_PLUGINS_SOURCE_MODE", "release")).strip().lower() or "release"
        plugins_tag = str(vercel_envs.get("NIYIEN_PLUGINS_TAG", "")).strip()
        plugins_artifact = str(vercel_envs.get("NIYIEN_PLUGINS_ARTIFACT_NAME", "")).strip()
        sdk_base = str(vercel_envs.get("NIYIEN_SDK_BASE", "")).strip()

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
        if app_source_mode == "artifact":
            cmd.extend(["--app-run-id", str(int(app_run_id))])
        if lens_tag:
            cmd.extend(["--lens-tag", lens_tag])
        if plugins_tag:
            cmd.extend(["--plugins-tag", plugins_tag])
        if plugins_artifact:
            cmd.extend(["--plugins-artifact-name", plugins_artifact])
        if sdk_base:
            cmd.extend(["--sdk-base", sdk_base])
        return cmd

    def _start_pan123_publish(self, *, app_tag: str, app_version: str,
                              app_run_id: int = 0,
                              on_finalize=None) -> dict:
        """Submit a pan123 publish task. Returns {ok, token} or {ok:False, error}.

        Spawns publish_pan123_release.py in a worker thread; the frontend
        polls poll_publish_progress(token) for live updates.
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

            output_dir = REPO_ROOT / "_deployment" / "_publish_local" / app_tag
            command = self._build_pan123_publish_command(
                app_tag=app_tag, cfg=cfg, vercel_envs=vercel_envs, output_dir=output_dir,
                app_run_id=app_run_id,
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
                            f"finalize_summary: content_tag={finalize_summary.get('content_tag')}, "
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

    def start_pan123_publish_manual(self, tag: str, version: str = "", run_id: int = 0) -> dict:
        """User-initiated retry of pan123 sync for a specific tag.

        `run_id > 0` triggers artifact-mode (--app-source-mode=artifact
        --app-run-id=N). Used by the dashboard "手动上传" button when the
        inventory shows a missing/incomplete tag dir. Returns {ok, token}
        or {ok:False, error}. Single-task model — refuses if another
        publish is already running.
        """
        tag = str(tag or "").strip()
        if not tag:
            return {"ok": False, "error": "tag 必填"}
        try:
            run_id_int = int(run_id or 0)
        except (TypeError, ValueError):
            run_id_int = 0
        return self._start_pan123_publish(
            app_tag=tag,
            app_version=str(version or "").strip(),
            app_run_id=run_id_int,
        )

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
            vercel_envs = {k: r.get("value", "") for k, r in env_records.items()}

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
                })
            # ---- Content bundles (lens/plugin/sdk under content-{hash}/) ----
            # content_tag = sha256[:12], immutable — so we cache all derived
            # fields (app_tag, lens_release_tag, total_size, ...) keyed by
            # content_tag and never re-fetch the manifest after the first
            # successful read.
            content_bundles: list[dict] = []
            content_bundles_error = ""
            try:
                bundle_cache = load_bundle_cache()
                cache_dirty = False

                root_children = client.list_directory(root_id)
                # Dedupe top-level content-* directories by fileID. Multiple
                # dirs with the same name but different fileIDs ARE possible
                # (123 网盘 doesn't enforce uniqueness on concurrent creates)
                # — surface them as duplicates so the operator can clean up.
                seen_dir_ids: set[int] = set()
                name_count: dict[str, int] = {}  # how many fileIDs share this name
                for child in root_children:
                    if int(child.get("type", -1)) != 1:
                        continue
                    name = str(child.get("filename") or child.get("name") or "").strip()
                    if name.startswith("content-"):
                        name_count[name] = name_count.get(name, 0) + 1

                for child in root_children:
                    if int(child.get("type", -1)) != 1:
                        continue
                    name = str(child.get("filename") or child.get("name") or "").strip()
                    if not name.startswith("content-"):
                        continue
                    bundle_dir_id = client._entry_id(child)
                    if not bundle_dir_id or bundle_dir_id in seen_dir_ids:
                        continue
                    seen_dir_ids.add(bundle_dir_id)
                    is_duplicate = name_count.get(name, 0) > 1

                    # Cache key: name + fileID (different fileIDs may have
                    # different manifest content even with the same name).
                    cache_key = f"{name}#{bundle_dir_id}"
                    cached = bundle_cache.get(cache_key)
                    # Only honor cache if it has a clean manifest (no error).
                    # Otherwise we'd permanently latch on a transient failure.
                    if cached and not cached.get("manifest_error"):
                        bundle = dict(cached)
                        bundle["tag"] = name
                        bundle["fileID"] = bundle_dir_id
                        bundle["is_duplicate"] = is_duplicate
                        bundle["from_cache"] = True
                        bundle.setdefault("created_at", str(child.get("createAt") or child.get("createTime") or ""))
                        content_bundles.append(bundle)
                        continue

                    # Cache miss (or stale failed cache) — scan
                    bundle_entries = client.list_directory(bundle_dir_id)
                    top_files = {
                        str(f.get("filename") or f.get("name") or "").strip(): f
                        for f in bundle_entries
                        if int(f.get("type", -1)) == 0
                    }
                    sub_dir_count = sum(1 for f in bundle_entries if int(f.get("type", -1)) == 1)
                    has_manifest = CONTENT_MANIFEST_ASSET_NAME in top_files

                    total_size, file_count = client.directory_total_size(bundle_dir_id)

                    manifest_fields: dict = {}
                    manifest_clean = False
                    if has_manifest:
                        try:
                            manifest_fid = client._entry_id(top_files[CONTENT_MANIFEST_ASSET_NAME])
                            text = client.fetch_file_text(manifest_fid)
                            import json as _json
                            parsed = _json.loads(text)
                            if isinstance(parsed, dict):
                                manifest_fields = {
                                    "manifest_app_tag": parsed.get("app_tag", ""),
                                    "manifest_app_source_mode": parsed.get("app_source_mode", ""),
                                    "manifest_app_source_ref": parsed.get("app_source_ref", ""),
                                    "manifest_lens_release_tag": parsed.get("lens_release_tag", ""),
                                    "manifest_plugins_release_tag": parsed.get("plugins_release_tag", ""),
                                    "manifest_plugin_source_mode": parsed.get("plugin_source_mode", ""),
                                    "manifest_plugin_source_ref": parsed.get("plugin_source_ref", ""),
                                    "manifest_content_hash": parsed.get("content_hash", ""),
                                    "manifest_generated_at": parsed.get("generated_at", ""),
                                }
                                manifest_clean = True
                        except Exception as merr:
                            manifest_fields = {"manifest_error": str(merr)}

                    bundle = {
                        "tag": name,
                        "fileID": bundle_dir_id,
                        "is_duplicate": is_duplicate,
                        "file_count": file_count,
                        "subdir_count": sub_dir_count,
                        "has_manifest": has_manifest,
                        "total_size_mb": round(total_size / 1024 / 1024, 2),
                        "created_at": str(child.get("createAt") or child.get("createTime") or ""),
                        "from_cache": False,
                        **manifest_fields,
                    }
                    content_bundles.append(bundle)
                    # Only cache successful manifest reads — a transient
                    # download failure shouldn't poison cache forever.
                    if manifest_clean:
                        cache_entry = dict(bundle)
                        cache_entry.pop("tag", None)
                        cache_entry.pop("fileID", None)
                        cache_entry.pop("is_duplicate", None)
                        cache_entry.pop("from_cache", None)
                        bundle_cache[cache_key] = cache_entry
                        cache_dirty = True

                if cache_dirty:
                    save_bundle_cache(bundle_cache)

                # Sort: manifest_generated_at desc, then created_at, then tag
                content_bundles.sort(
                    key=lambda b: (b.get("manifest_generated_at") or b.get("created_at") or b.get("tag")),
                    reverse=True,
                )
            except Exception as exc:
                content_bundles_error = f"{exc.__class__.__name__}: {exc}"

            return {
                "ok": True,
                "auto_version": auto_version,
                "items": out,
                "content_bundles": content_bundles,
                "content_bundles_error": content_bundles_error,
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
                               run_id: int = 0) -> None:
        """Add or replace the policy entry for `version` in place.

        `run_id` is non-zero for artifact-mode entries — it lets later
        operations (e.g. dashboard-triggered pan123 re-sync) reconstruct
        the `--app-source-mode=artifact --app-run-id=N` invocation
        without the user having to re-pick the run.
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
        for i, v in enumerate(versions):
            if v.get("version") == version:
                # Preserve unknown fields (e.g. release_summary fields set previously)
                merged = dict(v)
                merged.update(entry)
                # If we're now release-mode (run_id absent), drop any old run_id
                if int(run_id or 0) <= 0:
                    merged.pop("run_id", None)
                versions[i] = merged
                return
        versions.append(entry)

    def _finalize_publish_to_manifest(self, cfg: dict, policy_json: str,
                                       finalize_summary: dict | None = None) -> str:
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

        # Merge runtime-only fields (content_tag, lens metadata, plugin
        # source refs) into the policy entry for `auto_version`.
        upsert_map = {"NIYIEN_RELEASE_POLICY_JSON": policy_json}
        if finalize_summary:
            try:
                policy = _json.loads(policy_json)
                auto_v = str(policy.get("auto_version", "")).strip()
                target = next(
                    (v for v in policy.get("versions", []) if v.get("version") == auto_v),
                    None,
                )
                if target is not None:
                    if finalize_summary.get("content_tag"):
                        target["content_tag"] = str(finalize_summary["content_tag"])
                    if finalize_summary.get("lens_release_tag"):
                        target["lens_tag"] = str(finalize_summary["lens_release_tag"])
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
                upsert_map["NIYIEN_RELEASE_POLICY_JSON"] = _json.dumps(
                    policy, ensure_ascii=False, indent=2,
                )
            except Exception:
                # If anything goes wrong merging, fall back to the bare
                # staged_policy_json — manifest will still push, but
                # plugins_base / lens fields may be empty until the next
                # publish.
                pass

            # Top-level envs that the manifest API also consults.
            if finalize_summary.get("content_tag"):
                upsert_map["NIYIEN_CONTENT_RELEASE_TAG"] = str(finalize_summary["content_tag"])
            if finalize_summary.get("lens_version") is not None:
                upsert_map["NIYIEN_LENS_VERSION"] = str(finalize_summary["lens_version"])
            if finalize_summary.get("lens_sha256"):
                upsert_map["NIYIEN_LENS_SHA256"] = str(finalize_summary["lens_sha256"])

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
        """Fire the Vercel deploy hook to rebuild the manifest CDN."""
        import requests
        url = str(cfg.get("deploy_hook_url", "")).strip()
        if not url:
            return "no deploy_hook_url configured, skipped"
        from .helpers import build_proxy_mapping
        proxies = build_proxy_mapping(cfg.get("network_proxy", ""))
        r = requests.post(url, timeout=30, proxies=proxies)
        r.raise_for_status()
        return f"deploy hook triggered ({r.status_code})"

    def execute_app_action(self, payload: dict) -> dict:
        """One of 5 publish actions — real policy mutation + Vercel upsert.

        manual_only       — add version to policy.versions (channels=['manual']), leave auto_version
        publish_and_push  — add + set auto_version to this version (channels=['auto','manual']), clear others' auto
        switch_auto       — version must already be in policy.versions; switch auto_version to it
        rollback_auto     — same as switch_auto but also forces recommended=True
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
            vercel = self._vercel(cfg)
            env_records = vercel.list_env_records()
            policy = self._load_current_policy(cfg, vercel, env_records)
            versions = policy.get("versions", [])
            already_present = any(v.get("version") == version for v in versions)

            if action == "manual_only":
                self._upsert_version_entry(versions, version, tag, changelog, recommended, ["manual"], run_id=run_id)

            elif action == "publish_and_push":
                self._upsert_version_entry(versions, version, tag, changelog, recommended, ["auto", "manual"], run_id=run_id)
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
                    elif "auto" in v.get("channels", []):
                        v["channels"] = [c for c in v.get("channels", []) if c != "auto"] or ["manual"]
                policy["auto_version"] = version

            elif action == "rollback_auto":
                if not already_present:
                    return {"ok": False, "error": f"版本 {version} 不在白名单,无法回滚到此版本"}
                for v in versions:
                    if v.get("version") == version:
                        v["channels"] = sorted(set(v.get("channels", []) + ["auto", "manual"]))
                        v["recommended"] = True
                        if tag and not str(v.get("tag", "")).strip():
                            v["tag"] = tag
                        if run_id > 0 and int(v.get("run_id", 0) or 0) <= 0:
                            v["run_id"] = run_id
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

                pan_result = self._start_pan123_publish(
                    app_tag=tag, app_version=version, app_run_id=effective_run_id,
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
