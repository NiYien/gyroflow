#!/usr/bin/env python3
import io
import json
import subprocess
import tkinter as tk
import time
import webbrowser
import zipfile
from datetime import datetime
from pathlib import Path
from tkinter import font as tkfont
from tkinter import messagebox, scrolledtext, ttk

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib

import requests


ROOT = Path(__file__).resolve().parent.parent
CONFIG_FILE = Path(__file__).with_suffix(".config.json")
EXAMPLE_CONFIG_FILE = Path(__file__).with_name("control_center.example.json")
RELEASE_SUMMARY_ASSET_NAME = "gyroflow-niyien-release-summary.json"
DEFAULT_GLOBAL_SDK_BASE = "https://api.gyroflow.xyz/sdk/"
DEFAULT_GLOBAL_PLUGINS_BASE = "https://github.com/gyroflow/gyroflow-plugins/releases/latest/download/"
DEFAULT_LENS_DATA_OWNER = "NiYien"
DEFAULT_LENS_DATA_REPO = "niyien-lens-data"
DEFAULT_PLUGINS_OWNER = "gyroflow"
DEFAULT_PLUGINS_REPO = "gyroflow-plugins"
DEFAULT_PLUGINS_SOURCE_MODE = "release"
PLUGINS_SOURCE_MODE_VALUES = ("release", "artifact")
DEFAULT_APP_SOURCE_MODE = "release"
APP_SOURCE_MODE_VALUES = ("release", "artifact")
APP_BUILD_WORKFLOW_FILE = "release.yml"
APP_ARTIFACT_PUBLISH_WORKFLOW_FILE = "publish_action_build.yml"
APP_ARTIFACT_RETENTION_DAYS = 90
DEFAULT_NIGHTLY_LINK_BASE = "https://nightly.link"
DEFAULT_NETWORK_PROXY = "127.0.0.1:6063"

DEFAULT_CONFIG = {
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
    "lens_data_owner": DEFAULT_LENS_DATA_OWNER,
    "lens_data_repo": DEFAULT_LENS_DATA_REPO,
    "plugins_owner": DEFAULT_PLUGINS_OWNER,
    "plugins_repo": DEFAULT_PLUGINS_REPO,
    "network_proxy": DEFAULT_NETWORK_PROXY,
    "git_remote": "origin",
    "repo_workdir": str(ROOT),
}


def load_json_file(path: Path, fallback):
    if not path.exists():
        return json.loads(json.dumps(fallback))
    return json.loads(path.read_text(encoding="utf-8"))


def save_json_file(path: Path, data):
    path.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")


def normalize_version(tag: str) -> str:
    return tag[1:] if tag.startswith("v") else tag


def asset_name_for_platform(platform: str) -> str:
    return {
        "windows": "gyroflow-niyien-windows64.zip",
        "macos": "gyroflow-niyien-mac-universal.dmg",
        "linux": "gyroflow-niyien-linux64.AppImage",
        "android": "gyroflow-niyien.apk",
    }.get(platform, "gyroflow-niyien-windows64.zip")


def normalize_proxy_url(value: str) -> str:
    proxy = str(value or "").strip()
    if not proxy:
        return ""
    if "://" not in proxy:
        proxy = f"http://{proxy}"
    return proxy


def build_proxy_mapping(proxy_url: str) -> dict | None:
    proxy = normalize_proxy_url(proxy_url)
    if not proxy:
        return None
    return {"http": proxy, "https": proxy}


def parse_csv_list(value: str) -> list[str]:
    results: list[str] = []
    for raw in str(value or "").split(","):
        item = raw.strip()
        if item and item not in results:
            results.append(item)
    return results


def action_artifact_aliases(platform: str) -> set[str]:
    aliases = {
        "windows": {
            "gyroflow-niyien-windows64.zip",
            "gyroflow-niyien-windows",
        },
        "macos": {
            "gyroflow-niyien-mac-universal.dmg",
            "gyroflow-niyien-macos",
        },
    }
    return aliases.get(platform, set())


def select_source(config: dict, country: str) -> dict:
    cn = set(config.get("routing", {}).get("cn_countries", []))
    if country.upper() in cn:
        return config["sources"]["cn"]
    return config["sources"]["global"]


class VercelClient:
    def __init__(self, token: str, project: str, team_id: str = "", proxy_url: str = ""):
        self.token = token.strip()
        self.project = project.strip()
        self.team_id = team_id.strip()
        self.proxy_url = normalize_proxy_url(proxy_url)

    def _params(self):
        params = {}
        if self.team_id:
            params["teamId"] = self.team_id
        return params

    def _request_kwargs(self, *, timeout: int, **kwargs):
        payload = dict(kwargs)
        payload["timeout"] = timeout
        proxies = build_proxy_mapping(self.proxy_url)
        if proxies:
            payload["proxies"] = proxies
        return payload

    def _headers(self):
        return {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}

    def list_envs(self) -> dict:
        records = self.list_env_records()
        result = {}
        for key, env in records.items():
            result[key] = env.get("value", "")
        return result

    def list_env_records(self) -> dict:
        self._ensure_ready()
        url = f"https://api.vercel.com/v10/projects/{self.project}/env"
        response = requests.get(
            url,
            headers=self._headers(),
            params={**self._params(), "decrypt": "true"},
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        payload = response.json()
        envs = payload.get("envs") if isinstance(payload, dict) else payload
        result = {}
        for env in envs or []:
            key = env.get("key")
            value = env.get("value")
            target = env.get("target") or []
            if isinstance(target, str):
                target = [target]
            if key and "production" in target:
                result[key] = dict(env)
            elif key and key not in result:
                result[key] = dict(env)
        return result

    def upsert_envs(self, mapping: dict):
        self._ensure_ready()
        url = f"https://api.vercel.com/v10/projects/{self.project}/env"
        body = []
        for key, value in mapping.items():
            body.append(
                {
                    "key": key,
                    "value": value,
                    "type": "encrypted",
                    "target": ["production", "preview", "development"],
                }
            )
        response = requests.post(
            url,
            headers=self._headers(),
            params={**self._params(), "upsert": "true"},
            json=body,
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        return response.json()

    def _ensure_ready(self):
        if not self.token or not self.project:
            raise RuntimeError("Missing Vercel token or project id/name")


class GitHubClient:
    def __init__(self, owner: str, repo: str, token: str = "", proxy_url: str = ""):
        self.owner = owner.strip()
        self.repo = repo.strip()
        self.token = token.strip()
        self.proxy_url = normalize_proxy_url(proxy_url)

    def _headers(self):
        headers = {"Accept": "application/vnd.github+json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        return headers

    def _request_kwargs(self, *, timeout: int, **kwargs):
        payload = dict(kwargs)
        payload["timeout"] = timeout
        proxies = build_proxy_mapping(self.proxy_url)
        if proxies:
            payload["proxies"] = proxies
        return payload

    def _public_headers(self):
        return {"Accept": "application/vnd.github+json"}

    def _get(self, url: str, *, timeout: int = 30):
        response = requests.get(
            url,
            headers=self._headers(),
            **self._request_kwargs(timeout=timeout),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                **self._request_kwargs(timeout=timeout),
            )
        return response

    def _get_binary(self, url: str, *, timeout: int = 60) -> bytes:
        response = requests.get(
            url,
            headers=self._headers(),
            **self._request_kwargs(timeout=timeout, stream=True),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                **self._request_kwargs(timeout=timeout, stream=True),
            )
        response.raise_for_status()
        return response.content

    def list_releases(self):
        return self.list_repo_releases(self.owner, self.repo)

    def list_repo_releases(self, owner: str, repo: str):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{owner}/{repo}/releases"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    def get_release_by_tag(self, tag: str):
        return self.get_repo_release_by_tag(self.owner, self.repo, tag)

    def get_repo_release_by_tag(self, owner: str, repo: str, tag: str):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{owner}/{repo}/releases/tags/{tag}"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    def get_latest_release(self, owner: str, repo: str):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{owner}/{repo}/releases/latest"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    def get_repository(self, owner: str, repo: str):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{owner}/{repo}"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    def download_text_asset(self, url: str) -> str:
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.text

    def list_workflow_runs(
        self,
        workflow: str,
        *,
        branch: str = "",
        event: str = "",
        per_page: int = 20,
    ):
        self._ensure_ready()
        params = {"per_page": max(1, min(int(per_page), 100))}
        if branch:
            params["branch"] = branch
        if event:
            params["event"] = event
        url = f"https://api.github.com/repos/{self.owner}/{self.repo}/actions/workflows/{workflow}/runs"
        response = requests.get(
            url,
            headers=self._headers(),
            params=params,
            **self._request_kwargs(timeout=30),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                params=params,
                **self._request_kwargs(timeout=30),
            )
        response.raise_for_status()
        payload = response.json()
        return payload.get("workflow_runs", []) if isinstance(payload, dict) else []

    def list_repo_workflow_runs(
        self,
        owner: str,
        repo: str,
        *,
        branch: str = "",
        per_page: int = 20,
    ):
        self._ensure_ready()
        params = {
            "per_page": max(1, min(int(per_page), 100)),
            "exclude_pull_requests": "true",
            "status": "completed",
        }
        if branch:
            params["branch"] = branch
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/runs"
        response = requests.get(
            url,
            headers=self._headers(),
            params=params,
            **self._request_kwargs(timeout=30),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                params=params,
                **self._request_kwargs(timeout=30),
            )
        response.raise_for_status()
        payload = response.json()
        return payload.get("workflow_runs", []) if isinstance(payload, dict) else []

    def dispatch_workflow(self, workflow: str, ref: str, inputs: dict | None = None):
        self._ensure_ready()
        if not self.token:
            raise RuntimeError("Missing GitHub token for workflow dispatch")
        url = f"https://api.github.com/repos/{self.owner}/{self.repo}/actions/workflows/{workflow}/dispatches"
        response = requests.post(
            url,
            headers=self._headers(),
            json={"ref": ref, "inputs": inputs or {}},
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        return True

    def list_run_artifacts(self, run_id: int):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{self.owner}/{self.repo}/actions/runs/{int(run_id)}/artifacts"
        response = requests.get(
            url,
            headers=self._headers(),
            params={"per_page": 100},
            **self._request_kwargs(timeout=30),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                params={"per_page": 100},
                **self._request_kwargs(timeout=30),
            )
        response.raise_for_status()
        payload = response.json()
        return payload.get("artifacts", []) if isinstance(payload, dict) else []

    def list_repo_run_artifacts(self, owner: str, repo: str, run_id: int):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/runs/{int(run_id)}/artifacts"
        response = requests.get(
            url,
            headers=self._headers(),
            params={"per_page": 100},
            **self._request_kwargs(timeout=30),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                params={"per_page": 100},
                **self._request_kwargs(timeout=30),
            )
        response.raise_for_status()
        payload = response.json()
        return payload.get("artifacts", []) if isinstance(payload, dict) else []

    def get_workflow_run(self, run_id: int):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{self.owner}/{self.repo}/actions/runs/{int(run_id)}"
        response = requests.get(
            url,
            headers=self._headers(),
            **self._request_kwargs(timeout=30),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                **self._request_kwargs(timeout=30),
            )
        response.raise_for_status()
        return response.json()

    def download_artifact_archive_bytes(self, artifact_id: int) -> bytes:
        self._ensure_ready()
        url = f"https://api.github.com/repos/{self.owner}/{self.repo}/actions/artifacts/{int(artifact_id)}/zip"
        return self._get_binary(url, timeout=120)

    def list_actions_variables(self, owner: str | None = None, repo: str | None = None) -> dict:
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/variables"
        response = requests.get(
            url,
            headers=self._headers(),
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        payload = response.json()
        variables = payload.get("variables") if isinstance(payload, dict) else []
        return {
            item.get("name"): item.get("value")
            for item in variables or []
            if item.get("name")
        }

    def get_actions_variable(self, name: str, owner: str | None = None, repo: str | None = None):
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/variables/{name}"
        response = requests.get(
            url,
            headers=self._headers(),
            **self._request_kwargs(timeout=30),
        )
        if response.status_code == 404:
            return None
        response.raise_for_status()
        return response.json()

    def upsert_actions_variable(self, name: str, value: str, owner: str | None = None, repo: str | None = None):
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        value = str(value)
        if not value:
            raise RuntimeError(f"GitHub Actions Variable {name} 不能为空")
        existing = self.get_actions_variable(name, owner, repo)
        payload = {"name": name, "value": value}
        if existing:
            url = f"https://api.github.com/repos/{owner}/{repo}/actions/variables/{name}"
            response = requests.patch(
                url,
                headers=self._headers(),
                json={"value": value},
                **self._request_kwargs(timeout=30),
            )
        else:
            url = f"https://api.github.com/repos/{owner}/{repo}/actions/variables"
            response = requests.post(
                url,
                headers=self._headers(),
                json=payload,
                **self._request_kwargs(timeout=30),
            )
        response.raise_for_status()
        return True

    def delete_actions_variable(self, name: str, owner: str | None = None, repo: str | None = None):
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/variables/{name}"
        response = requests.delete(
            url,
            headers=self._headers(),
            **self._request_kwargs(timeout=30),
        )
        if response.status_code == 404:
            return False
        response.raise_for_status()
        return True

    def _ensure_ready(self):
        if not self.owner or not self.repo:
            raise RuntimeError("Missing GitHub owner/repo")


class ControlCenter(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title("NiYien 发布中心")
        self.geometry("1180x780")
        self.minsize(900, 620)
        self.config_data = load_json_file(CONFIG_FILE, DEFAULT_CONFIG)
        self.distribution_config = self.load_distribution_config()
        self.current_envs = {}
        self.current_env_records = {}
        self.current_repo_variables = {}
        self.current_policy = self.default_policy()
        self.current_releases = []
        self.current_action_builds = []
        self.pending_release_watch = None
        self.pending_action_build_watch = None
        self._resize_after_id = None
        self.layout_scale = 1.0
        self.responsive_entries = []
        self.responsive_texts = []
        self.responsive_listboxes = []
        self.wrap_widgets = []
        self.palette = {
            "bg": "#F8FAFC",
            "surface": "#FFFFFF",
            "surface_alt": "#EEF2F7",
            "text": "#1E293B",
            "muted": "#64748B",
            "line": "#D9E2EC",
            "primary": "#2563EB",
            "primary_soft": "#DBEAFE",
            "accent": "#F97316",
            "success": "#059669",
            "warning": "#D97706",
            "danger": "#DC2626",
            "nav": "#0F172A",
            "nav_active": "#1D4ED8",
            "nav_text": "#E2E8F0",
            "nav_muted": "#94A3B8",
        }
        self.page_titles = {}
        self.page_frames = {}
        self.sidebar_buttons = {}
        self.init_fonts()
        self.configure_styles()
        self._build_ui()
        self.bind("<Configure>", self.schedule_responsive_refresh)
        self.refresh_runtime_state()
        self.after(50, self.apply_responsive_layout)

    def init_fonts(self):
        family = "Microsoft YaHei UI"
        self.fonts = {
            "default": tkfont.Font(family=family, size=10),
            "nav": tkfont.Font(family=family, size=10, weight="bold"),
            "brand": tkfont.Font(family=family, size=20, weight="bold"),
            "brand_sub": tkfont.Font(family=family, size=10),
            "pill": tkfont.Font(family=family, size=9, weight="bold"),
            "page_title": tkfont.Font(family=family, size=24, weight="bold"),
            "page_subtitle": tkfont.Font(family=family, size=10),
            "header": tkfont.Font(family=family, size=18, weight="bold"),
            "subtle": tkfont.Font(family=family, size=9),
            "card_title": tkfont.Font(family=family, size=12, weight="bold"),
            "body": tkfont.Font(family=family, size=9),
            "body_bold": tkfont.Font(family=family, size=9, weight="bold"),
            "small_bold": tkfont.Font(family=family, size=8, weight="bold"),
            "stat": tkfont.Font(family=family, size=13, weight="bold"),
            "button": tkfont.Font(family=family, size=10, weight="bold"),
        }

    def configure_styles(self):
        self.option_add("*Font", str(self.fonts["default"]))
        style = ttk.Style(self)
        self.style = style
        try:
            style.theme_use("clam")
        except tk.TclError:
            pass
        self.configure(bg=self.palette["bg"])
        style.configure("TNotebook", background=self.palette["bg"], borderwidth=0)
        style.configure("TNotebook.Tab", padding=(14, 8), font=self.fonts["nav"])
        style.configure("TFrame", background=self.palette["bg"])
        style.configure(
            "TLabelframe",
            background=self.palette["bg"],
            borderwidth=1,
            relief="solid",
            bordercolor=self.palette["line"],
        )
        style.configure(
            "TLabelframe.Label",
            background=self.palette["bg"],
            foreground=self.palette["text"],
            font=self.fonts["body_bold"],
        )
        style.configure(
            "Header.TLabel",
            background=self.palette["bg"],
            foreground=self.palette["text"],
            font=self.fonts["header"],
        )
        style.configure(
            "Subtle.TLabel",
            background=self.palette["bg"],
            foreground=self.palette["muted"],
            font=self.fonts["body"],
        )
        style.configure(
            "TEntry",
            fieldbackground=self.palette["surface"],
            foreground=self.palette["text"],
            bordercolor=self.palette["line"],
            lightcolor=self.palette["line"],
            darkcolor=self.palette["line"],
            padding=6,
            font=self.fonts["default"],
        )
        style.map(
            "TEntry",
            bordercolor=[("focus", self.palette["primary"])],
            lightcolor=[("focus", self.palette["primary"])],
            darkcolor=[("focus", self.palette["primary"])],
        )
        style.configure(
            "TCombobox",
            fieldbackground=self.palette["surface"],
            foreground=self.palette["text"],
            bordercolor=self.palette["line"],
            lightcolor=self.palette["line"],
            darkcolor=self.palette["line"],
            arrowsize=14,
            padding=5,
            font=self.fonts["default"],
        )
        style.map(
            "TCombobox",
            bordercolor=[("focus", self.palette["primary"])],
            lightcolor=[("focus", self.palette["primary"])],
            darkcolor=[("focus", self.palette["primary"])],
        )
        style.configure(
            "TCheckbutton",
            background=self.palette["surface"],
            foreground=self.palette["text"],
            font=self.fonts["default"],
        )
        style.configure(
            "Primary.TButton",
            padding=(12, 8),
            font=self.fonts["button"],
        )
        style.map(
            "Primary.TButton",
            background=[("active", self.palette["accent"]), ("!disabled", self.palette["primary"])],
            foreground=[("!disabled", "#FFFFFF")],
        )
        style.configure(
            "Immediate.TButton",
            padding=(12, 8),
            font=self.fonts["button"],
        )
        style.map(
            "Immediate.TButton",
            background=[("active", "#EA580C"), ("!disabled", self.palette["accent"])],
            foreground=[("!disabled", "#FFFFFF")],
        )
        style.configure(
            "Future.TButton",
            padding=(12, 8),
            font=self.fonts["button"],
        )
        style.map(
            "Future.TButton",
            background=[("active", "#1E40AF"), ("!disabled", "#1D4ED8")],
            foreground=[("!disabled", "#FFFFFF")],
        )
        style.configure(
            "Warning.TButton",
            padding=(11, 8),
            font=self.fonts["button"],
        )
        style.map(
            "Warning.TButton",
            background=[("active", "#B45309"), ("!disabled", self.palette["warning"])],
            foreground=[("!disabled", "#FFFFFF")],
        )
        style.configure(
            "Danger.TButton",
            padding=(11, 8),
            font=self.fonts["button"],
        )
        style.map(
            "Danger.TButton",
            background=[("active", "#B91C1C"), ("!disabled", self.palette["danger"])],
            foreground=[("!disabled", "#FFFFFF")],
        )
        style.configure(
            "Secondary.TButton",
            padding=(10, 7),
            font=self.fonts["button"],
        )
        style.map(
            "Secondary.TButton",
            background=[("active", self.palette["surface_alt"]), ("!disabled", self.palette["surface"])],
            foreground=[("!disabled", self.palette["text"])],
        )
        style.configure("Card.TFrame", background=self.palette["surface"], relief="solid", borderwidth=1)

    def style_text_widget(self, widget):
        widget.configure(
            bg=self.palette["surface"],
            fg=self.palette["text"],
            insertbackground=self.palette["text"],
            relief="flat",
            borderwidth=0,
            highlightthickness=1,
            highlightbackground=self.palette["line"],
            highlightcolor=self.palette["primary"],
            padx=8,
            pady=8,
            selectbackground=self.palette["primary"],
            selectforeground="#FFFFFF",
            font=self.fonts["body"],
        )

    def style_listbox(self, widget):
        widget.configure(
            bg=self.palette["surface"],
            fg=self.palette["text"],
            relief="flat",
            borderwidth=0,
            highlightthickness=1,
            highlightbackground=self.palette["line"],
            highlightcolor=self.palette["primary"],
            selectbackground=self.palette["primary"],
            selectforeground="#FFFFFF",
            activestyle="none",
            font=self.fonts["body"],
        )

    def register_wrap(self, widget, base_wrap: int):
        self.wrap_widgets.append((widget, base_wrap))

    def register_entry(self, widget, base_width: int):
        self.responsive_entries.append((widget, base_width))

    def register_text_widget(self, widget, base_height: int):
        self.responsive_texts.append((widget, base_height))

    def register_listbox(self, widget, base_height: int):
        self.responsive_listboxes.append((widget, base_height))

    def load_distribution_config(self):
        config_path = ROOT / self.config_data.get("distribution_config_path", "distribution/niyien.toml")
        if not config_path.exists():
            return {}
        with config_path.open("rb") as fh:
            return tomllib.load(fh)

    def vercel(self):
        return VercelClient(
            self.config_data.get("vercel_token", ""),
            self.config_data.get("vercel_project_id_or_name", ""),
            self.config_data.get("vercel_team_id", ""),
            self.current_network_proxy(),
        )

    def github(self):
        return GitHubClient(
            self.config_data.get("github_owner", ""),
            self.config_data.get("github_repo", ""),
            self.config_data.get("github_token", ""),
            self.current_network_proxy(),
        )

    def current_network_proxy(self) -> str:
        return normalize_proxy_url(
            self.config_data.get("network_proxy", DEFAULT_NETWORK_PROXY)
        )

    def request_kwargs(self, *, timeout: int, **kwargs):
        payload = dict(kwargs)
        payload["timeout"] = timeout
        proxies = build_proxy_mapping(self.current_network_proxy())
        if proxies:
            payload["proxies"] = proxies
        return payload

    def http_get(self, url: str, *, timeout: int = 30, **kwargs):
        return requests.get(url, **self.request_kwargs(timeout=timeout, **kwargs))

    def http_post(self, url: str, *, timeout: int = 30, **kwargs):
        return requests.post(url, **self.request_kwargs(timeout=timeout, **kwargs))

    def repo_workdir(self) -> Path:
        return Path(self.config_data.get("repo_workdir", str(ROOT))).resolve()

    def git_remote_name(self) -> str:
        return (self.config_data.get("git_remote", "origin") or "origin").strip()

    def run_git(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        return subprocess.run(
            ["git", *args],
            cwd=self.repo_workdir(),
            text=True,
            capture_output=True,
            check=check,
        )

    def get_current_branch(self) -> str:
        try:
            return self.run_git("branch", "--show-current").stdout.strip()
        except Exception:
            return ""

    def get_worktree_status_summary(self) -> str:
        try:
            return self.run_git("status", "--short").stdout.strip()
        except Exception:
            return ""

    def local_tag_exists(self, tag: str) -> bool:
        try:
            self.run_git("rev-parse", "-q", "--verify", f"refs/tags/{tag}")
            return True
        except subprocess.CalledProcessError:
            return False

    def remote_tag_exists(self, tag: str) -> bool:
        try:
            result = self.run_git("ls-remote", "--tags", self.git_remote_name(), tag)
            return bool(result.stdout.strip())
        except Exception:
            return False

    def default_policy(self):
        return {"auto_version": "", "versions": []}

    def load_policy_from_env(self):
        raw = self.current_envs.get("NIYIEN_RELEASE_POLICY_JSON", "").strip()
        if not raw:
            return self.default_policy()
        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError:
            return self.default_policy()
        if not isinstance(parsed, dict) or not isinstance(parsed.get("versions"), list):
            return self.default_policy()
        return parsed

    def _build_ui(self):
        shell = tk.Frame(self, bg=self.palette["bg"])
        shell.pack(fill="both", expand=True)

        sidebar = tk.Frame(shell, bg=self.palette["nav"], width=240)
        sidebar.pack(side="left", fill="y")
        sidebar.pack_propagate(False)
        self.sidebar = sidebar

        brand = tk.Frame(sidebar, bg=self.palette["nav"])
        brand.pack(fill="x", padx=20, pady=(24, 18))
        tk.Label(
            brand,
            text="NiYien",
            bg=self.palette["nav"],
            fg="#FFFFFF",
            font=self.fonts["brand"],
        ).pack(anchor="w")
        tk.Label(
            brand,
            text="Control Center",
            bg=self.palette["nav"],
            fg=self.palette["nav_muted"],
            font=self.fonts["brand_sub"],
        ).pack(anchor="w", pady=(4, 0))
        pill = tk.Label(
            brand,
            text="Release / Content / Routing",
            bg="#13203A",
            fg="#BFDBFE",
            padx=10,
            pady=6,
            font=self.fonts["pill"],
        )
        pill.pack(anchor="w", pady=(14, 0))

        nav = tk.Frame(sidebar, bg=self.palette["nav"])
        nav.pack(fill="x", padx=12, pady=(12, 12))

        main = tk.Frame(shell, bg=self.palette["bg"])
        main.pack(side="left", fill="both", expand=True)

        header = tk.Frame(main, bg=self.palette["bg"])
        header.pack(fill="x", padx=28, pady=(24, 12))
        self.header_frame = header
        self.page_title_label = tk.Label(
            header,
            text="",
            bg=self.palette["bg"],
            fg=self.palette["text"],
            font=self.fonts["page_title"],
        )
        self.page_title_label.pack(anchor="w")
        self.page_subtitle_label = tk.Label(
            header,
            text="",
            bg=self.palette["bg"],
            fg=self.palette["muted"],
            font=self.fonts["page_subtitle"],
        )
        self.page_subtitle_label.pack(anchor="w", pady=(6, 0))

        status_strip = tk.Frame(main, bg=self.palette["bg"])
        status_strip.pack(fill="x", padx=28, pady=(0, 12))
        self.status_strip = status_strip
        self.global_auto_badge = self.create_status_badge(status_strip, "自动推送：-")
        self.global_content_badge = self.create_status_badge(status_strip, "内容版本：-")
        self.global_conn_badge = self.create_status_badge(status_strip, "控制面：未检测")

        body = tk.Frame(main, bg=self.palette["bg"])
        body.pack(fill="both", expand=True, padx=20, pady=(0, 20))
        self.body_frame = body

        content_shell = tk.Frame(body, bg=self.palette["bg"])
        content_shell.pack(fill="both", expand=True)

        self.content_canvas = tk.Canvas(
            content_shell,
            bg=self.palette["bg"],
            highlightthickness=0,
            borderwidth=0,
        )
        self.content_vscroll = ttk.Scrollbar(
            content_shell,
            orient="vertical",
            command=self.content_canvas.yview,
        )
        self.content_hscroll = ttk.Scrollbar(
            content_shell,
            orient="horizontal",
            command=self.content_canvas.xview,
        )
        self.content_canvas.configure(
            yscrollcommand=self.content_vscroll.set,
            xscrollcommand=self.content_hscroll.set,
        )

        self.content_canvas.grid(row=0, column=0, sticky="nsew")
        self.content_vscroll.grid(row=0, column=1, sticky="ns")
        self.content_hscroll.grid(row=1, column=0, sticky="ew")
        content_shell.grid_rowconfigure(0, weight=1)
        content_shell.grid_columnconfigure(0, weight=1)

        self.page_stack = tk.Frame(self.content_canvas, bg=self.palette["bg"])
        self.page_stack_window = self.content_canvas.create_window(
            (0, 0),
            window=self.page_stack,
            anchor="nw",
        )
        self.page_stack.bind("<Configure>", self.on_page_stack_configure)
        self.content_canvas.bind("<Configure>", self.on_content_canvas_configure)
        self.content_canvas.bind_all("<MouseWheel>", self.on_content_mousewheel)
        self.content_canvas.bind_all("<Shift-MouseWheel>", self.on_content_shift_mousewheel)

        self.guide_tab = ttk.Frame(self.page_stack)
        self.app_tab = ttk.Frame(self.page_stack)
        self.data_tab = ttk.Frame(self.page_stack)
        self.route_tab = ttk.Frame(self.page_stack)
        self.stats_tab = ttk.Frame(self.page_stack)
        self.advanced_tab = ttk.Frame(self.page_stack)

        self.page_frames = {
            "guide": self.guide_tab,
            "app": self.app_tab,
            "data": self.data_tab,
            "route": self.route_tab,
            "stats": self.stats_tab,
            "advanced": self.advanced_tab,
        }
        self.page_titles = {
            "guide": ("操作清单", "从首次配置到日常发版，这一页给你最短操作路径。"),
            "app": ("应用发布", "发布版本、开始推送、回滚和隐藏版本都集中在这里。"),
            "data": ("资源编排", "统一管理中国区内容版本，以及下次发版要使用的资源源。"),
            "route": ("下载与路由", "预览不同国家和平台下，客户端真正看到的 manifest。"),
            "stats": ("统计与观测", "查看统计数据、打开 dashboard、执行 rebuild。"),
            "advanced": ("高级设置", "本地保存令牌、项目名和控制面连接信息。"),
        }

        self.build_guide_tab()
        self.build_app_tab()
        self.build_data_tab()
        self.build_route_tab()
        self.build_stats_tab()
        self.build_advanced_tab()

        nav_items = [
            ("guide", "操作清单", "从 0 开始"),
            ("app", "应用发布", "发版与推送"),
            ("data", "资源编排", "Lens / Plugin / SDK"),
            ("route", "下载与路由", "中国区下载"),
            ("stats", "统计与观测", "telemetry"),
            ("advanced", "高级设置", "本地与控制面配置"),
        ]
        for key, title, subtitle in nav_items:
            self.sidebar_buttons[key] = self.create_sidebar_button(nav, key, title, subtitle)

        footer = tk.Label(
            sidebar,
            text="中国区下载走自有入口\n后台自动解析 123 直链",
            justify="left",
            bg=self.palette["nav"],
            fg=self.palette["nav_muted"],
            font=self.fonts["body"],
        )
        footer.pack(side="bottom", anchor="w", padx=20, pady=20)

        self.show_page("guide")

    def create_sidebar_button(self, parent, key: str, title: str, subtitle: str):
        button = tk.Button(
            parent,
            text=f"{title}\n{subtitle}",
            justify="left",
            anchor="w",
            command=lambda: self.show_page(key),
            relief="flat",
            borderwidth=0,
            highlightthickness=0,
            padx=16,
            pady=12,
            bg=self.palette["nav"],
            fg=self.palette["nav_text"],
            activebackground=self.palette["nav_active"],
            activeforeground="#FFFFFF",
            font=self.fonts["nav"],
        )
        button.pack(fill="x", pady=4)
        return button

    def show_page(self, key: str):
        for page_key, frame in self.page_frames.items():
            if page_key == key:
                frame.pack(fill="both", expand=True)
            else:
                frame.pack_forget()

        self.update_idletasks()
        self.refresh_content_scroll_region()

        title, subtitle = self.page_titles.get(key, ("NiYien 发布中心", ""))
        self.page_title_label.configure(text=title)
        self.page_subtitle_label.configure(text=subtitle)

        for page_key, button in self.sidebar_buttons.items():
            active = page_key == key
            button.configure(
                bg=self.palette["nav_active"] if active else self.palette["nav"],
                fg="#FFFFFF" if active else self.palette["nav_text"],
            )

    def on_page_stack_configure(self, _event=None):
        self.refresh_content_scroll_region()

    def on_content_canvas_configure(self, event):
        requested = self.page_stack.winfo_reqwidth()
        target_width = max(event.width, requested)
        self.content_canvas.itemconfigure(self.page_stack_window, width=target_width)
        self.refresh_content_scroll_region()

    def refresh_content_scroll_region(self):
        self.content_canvas.configure(scrollregion=self.content_canvas.bbox("all"))

    def on_content_mousewheel(self, event):
        if event.delta:
            self.content_canvas.yview_scroll(int(-event.delta / 120), "units")

    def on_content_shift_mousewheel(self, event):
        if event.delta:
            self.content_canvas.xview_scroll(int(-event.delta / 120), "units")

    def schedule_responsive_refresh(self, event=None):
        if event is not None and event.widget is not self:
            return
        if self._resize_after_id:
            self.after_cancel(self._resize_after_id)
        self._resize_after_id = self.after(80, self.apply_responsive_layout)

    def apply_responsive_layout(self):
        self._resize_after_id = None
        width = max(self.winfo_width(), 900)
        height = max(self.winfo_height(), 620)

        width_scale = 1.0
        if width < 1050:
            width_scale = 0.74
        elif width < 1180:
            width_scale = 0.82
        elif width < 1320:
            width_scale = 0.9
        elif width < 1500:
            width_scale = 0.96

        height_scale = 1.0
        if height < 700:
            height_scale = 0.74
        elif height < 780:
            height_scale = 0.82
        elif height < 860:
            height_scale = 0.9
        elif height < 940:
            height_scale = 0.96

        self.layout_scale = min(width_scale, height_scale)

        font_sizes = {
            "default": 10,
            "nav": 10,
            "brand": 20,
            "brand_sub": 10,
            "pill": 9,
            "page_title": 24,
            "page_subtitle": 10,
            "header": 18,
            "subtle": 9,
            "card_title": 12,
            "body": 9,
            "body_bold": 9,
            "small_bold": 8,
            "stat": 13,
            "button": 10,
        }
        for key, base in font_sizes.items():
            size = max(8, int(round(base * self.layout_scale)))
            self.fonts[key].configure(size=size)

        try:
            self.sidebar.configure(width=max(180, int(240 * self.layout_scale)))
            self.header_frame.pack_configure(padx=max(12, int(28 * self.layout_scale)), pady=(max(12, int(24 * self.layout_scale)), max(8, int(12 * self.layout_scale))))
            self.status_strip.pack_configure(padx=max(12, int(28 * self.layout_scale)), pady=(0, max(8, int(12 * self.layout_scale))))
            self.body_frame.pack_configure(padx=max(10, int(20 * self.layout_scale)), pady=(0, max(10, int(20 * self.layout_scale))))
        except Exception:
            pass

        if hasattr(self, "style"):
            self.style.configure("TNotebook.Tab", padding=(max(10, int(14 * self.layout_scale)), max(6, int(8 * self.layout_scale))))
            self.style.configure("Primary.TButton", padding=(max(10, int(12 * self.layout_scale)), max(6, int(8 * self.layout_scale))))
            self.style.configure("Immediate.TButton", padding=(max(10, int(12 * self.layout_scale)), max(6, int(8 * self.layout_scale))))
            self.style.configure("Future.TButton", padding=(max(10, int(12 * self.layout_scale)), max(6, int(8 * self.layout_scale))))
            self.style.configure("Warning.TButton", padding=(max(9, int(11 * self.layout_scale)), max(6, int(8 * self.layout_scale))))
            self.style.configure("Danger.TButton", padding=(max(9, int(11 * self.layout_scale)), max(6, int(8 * self.layout_scale))))
            self.style.configure("Secondary.TButton", padding=(max(8, int(10 * self.layout_scale)), max(5, int(7 * self.layout_scale))))

        for widget, base_width in self.responsive_entries:
            try:
                widget.configure(width=max(18, int(base_width * self.layout_scale)))
            except Exception:
                pass

        text_scale = max(0.65, min(1.0, height_scale))
        for widget, base_height in self.responsive_texts:
            try:
                widget.configure(height=max(8, int(base_height * text_scale)))
            except Exception:
                pass

        for widget, base_height in self.responsive_listboxes:
            try:
                widget.configure(height=max(6, int(base_height * text_scale)))
            except Exception:
                pass

        wrap_scale = max(0.7, min(1.0, width_scale))
        for widget, base_wrap in self.wrap_widgets:
            try:
                widget.configure(wraplength=max(220, int(base_wrap * wrap_scale)))
            except Exception:
                pass

        self.reflow_page_layouts(width)
        self.update_idletasks()
        self.refresh_content_scroll_region()

    def configure_grid_columns(self, container, specs: list[tuple[int, int, str | None]]):
        if not container:
            return
        max_column = max((column for column, _weight, _uniform in specs), default=-1)
        for column in range(max_column + 1):
            container.grid_columnconfigure(column, weight=0, uniform="")
        for column, weight, uniform in specs:
            kwargs = {"weight": weight}
            if uniform:
                kwargs["uniform"] = uniform
            container.grid_columnconfigure(column, **kwargs)

    def configure_grid_rows(self, container, specs: list[tuple[int, int]]):
        if not container:
            return
        max_row = max((row for row, _weight in specs), default=-1)
        for row in range(max_row + 1):
            container.grid_rowconfigure(row, weight=0)
        for row, weight in specs:
            container.grid_rowconfigure(row, weight=weight)

    def reflow_page_layouts(self, width: int):
        narrow = width < 1320
        compact = width < 1120
        self.reflow_guide_layout(narrow)
        self.reflow_app_layout(narrow)
        self.reflow_data_layout(narrow)
        self.reflow_route_layout(compact)
        self.reflow_stats_layout(compact)
        self.reflow_advanced_layout(compact)

    def reflow_guide_layout(self, narrow: bool):
        grid = getattr(self, "guide_grid", None)
        if not grid:
            return
        if narrow:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 0, None)])
            self.configure_grid_rows(grid, [(0, 0), (1, 0), (2, 0), (3, 0)])
            placements = [
                (self.guide_quick_card, 0, 0, 1, 1, (0, 0), (0, 12)),
                (self.guide_health_card, 1, 0, 1, 1, (0, 0), (0, 12)),
                (self.guide_release_card, 2, 0, 1, 1, (0, 0), (0, 12)),
                (self.guide_ops_card, 3, 0, 1, 1, (0, 0), (0, 0)),
            ]
        else:
            self.configure_grid_columns(grid, [(0, 2, None), (1, 1, None)])
            self.configure_grid_rows(grid, [(0, 0), (1, 0)])
            placements = [
                (self.guide_quick_card, 0, 0, 1, 1, (0, 10), (0, 12)),
                (self.guide_health_card, 0, 1, 1, 1, (10, 0), (0, 12)),
                (self.guide_release_card, 1, 0, 1, 1, (0, 10), (0, 0)),
                (self.guide_ops_card, 1, 1, 1, 1, (10, 0), (0, 0)),
            ]
        for widget, row, column, rowspan, columnspan, padx, pady in placements:
            widget.grid_configure(row=row, column=column, rowspan=rowspan, columnspan=columnspan, padx=padx, pady=pady, sticky="nsew")

    def reflow_app_layout(self, narrow: bool):
        grid = getattr(self, "app_grid", None)
        if not grid:
            return
        if narrow:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 0, None), (2, 0, None)])
            self.configure_grid_rows(grid, [(0, 0), (1, 0), (2, 0), (3, 0)])
            placements = [
                (self.app_release_card, 0, 0, 1, 1, (0, 0), (0, 12)),
                (self.app_overview_card, 1, 0, 1, 1, (0, 0), (0, 12)),
                (self.app_action_card, 2, 0, 1, 1, (0, 0), (0, 12)),
                (self.app_policy_card, 3, 0, 1, 1, (0, 0), (0, 0)),
            ]
        else:
            self.configure_grid_columns(grid, [(0, 1, "app"), (1, 1, "app"), (2, 1, "app")])
            self.configure_grid_rows(grid, [(0, 0), (1, 0)])
            placements = [
                (self.app_release_card, 0, 0, 2, 1, (0, 12), (0, 12)),
                (self.app_overview_card, 0, 1, 1, 1, (0, 12), (0, 12)),
                (self.app_action_card, 0, 2, 1, 1, (0, 0), (0, 12)),
                (self.app_policy_card, 1, 1, 1, 2, (0, 0), (0, 0)),
            ]
        for widget, row, column, rowspan, columnspan, padx, pady in placements:
            widget.grid_configure(row=row, column=column, rowspan=rowspan, columnspan=columnspan, padx=padx, pady=pady, sticky="nsew")

    def reflow_data_layout(self, narrow: bool):
        grid = getattr(self, "data_grid", None)
        if not grid:
            return
        if narrow:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 0, None)])
            self.configure_grid_rows(grid, [(0, 0), (1, 0), (2, 0)])
            placements = [
                (self.data_current_card, 0, 0, 1, 1, (0, 0), (0, 12)),
                (self.data_source_card, 1, 0, 1, 1, (0, 0), (0, 12)),
                (self.data_notes_card, 2, 0, 1, 1, (0, 0), (0, 0)),
            ]
        else:
            self.configure_grid_columns(grid, [(0, 1, "data"), (1, 1, "data")])
            self.configure_grid_rows(grid, [(0, 0), (1, 0)])
            placements = [
                (self.data_current_card, 0, 0, 1, 1, (0, 10), (0, 12)),
                (self.data_source_card, 0, 1, 1, 1, (10, 0), (0, 12)),
                (self.data_notes_card, 1, 0, 1, 2, (0, 0), (0, 0)),
            ]
        for widget, row, column, rowspan, columnspan, padx, pady in placements:
            widget.grid_configure(row=row, column=column, rowspan=rowspan, columnspan=columnspan, padx=padx, pady=pady, sticky="nsew")

    def reflow_route_layout(self, compact: bool):
        grid = getattr(self, "route_grid", None)
        if not grid:
            return
        if compact:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 0, None)])
            self.configure_grid_rows(grid, [(0, 0), (1, 0)])
            placements = [
                (self.route_form_card, 0, 0, (0, 0)),
                (self.route_preview_card, 1, 0, (0, 0)),
            ]
        else:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 2, None)])
            self.configure_grid_rows(grid, [(0, 0)])
            placements = [
                (self.route_form_card, 0, 0, (0, 10)),
                (self.route_preview_card, 0, 1, (10, 0)),
            ]
        for widget, row, column, padx in placements:
            widget.grid_configure(row=row, column=column, padx=padx, pady=(0, 12 if compact and row == 0 else 0), sticky="nsew")

    def reflow_stats_layout(self, compact: bool):
        grid = getattr(self, "stats_grid", None)
        if not grid:
            return
        if compact:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 0, None)])
            self.configure_grid_rows(grid, [(0, 0), (1, 0)])
            placements = [
                (self.stats_query_card, 0, 0, (0, 0)),
                (self.stats_result_card, 1, 0, (0, 0)),
            ]
        else:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 2, None)])
            self.configure_grid_rows(grid, [(0, 0)])
            placements = [
                (self.stats_query_card, 0, 0, (0, 10)),
                (self.stats_result_card, 0, 1, (10, 0)),
            ]
        for widget, row, column, padx in placements:
            widget.grid_configure(row=row, column=column, padx=padx, pady=(0, 12 if compact and row == 0 else 0), sticky="nsew")

    def reflow_advanced_layout(self, compact: bool):
        grid = getattr(self, "advanced_grid", None)
        if not grid:
            return
        if compact:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 0, None)])
            self.configure_grid_rows(grid, [(0, 0), (1, 0)])
            placements = [
                (self.advanced_config_card, 0, 0, (0, 0)),
                (self.advanced_env_card, 1, 0, (0, 0)),
            ]
        else:
            self.configure_grid_columns(grid, [(0, 1, None), (1, 1, None)])
            self.configure_grid_rows(grid, [(0, 0)])
            placements = [
                (self.advanced_config_card, 0, 0, (0, 10)),
                (self.advanced_env_card, 0, 1, (10, 0)),
            ]
        for widget, row, column, padx in placements:
            widget.grid_configure(row=row, column=column, padx=padx, pady=(0, 12 if compact and row == 0 else 0), sticky="nsew")

    def create_card(self, parent, title: str, description: str = "", kind: str = "default"):
        kind_palette = {
            "default": (self.palette["surface"], self.palette["line"], self.palette["line"]),
            "live": (self.palette["surface"], "#BBF7D0", "#22C55E"),
            "immediate": (self.palette["surface"], "#FED7AA", self.palette["accent"]),
            "next": (self.palette["surface"], "#BFDBFE", self.palette["primary"]),
            "danger": (self.palette["surface"], "#FECACA", self.palette["danger"]),
            "guide": (self.palette["surface"], "#FDE68A", self.palette["warning"]),
            "readonly": (self.palette["surface"], "#CBD5E1", "#64748B"),
        }
        bg, border, accent = kind_palette.get(kind, kind_palette["default"])
        card = tk.Frame(
            parent,
            bg=bg,
            highlightthickness=1,
            highlightbackground=border,
            bd=0,
        )
        accent_bar = tk.Frame(card, bg=accent, width=6)
        accent_bar.pack(side="left", fill="y")
        content = tk.Frame(card, bg=bg)
        content.pack(side="left", fill="both", expand=True)
        header = tk.Frame(content, bg=bg)
        header.pack(fill="x", padx=18, pady=(16, 8))
        tk.Label(
            header,
            text=title,
            bg=bg,
            fg=self.palette["text"],
            font=self.fonts["card_title"],
        ).pack(anchor="w")
        if description:
            desc_label = tk.Label(
                header,
                text=description,
                bg=bg,
                fg=self.palette["muted"],
                wraplength=520,
                justify="left",
                font=self.fonts["body"],
            )
            desc_label.pack(anchor="w", pady=(4, 0))
            self.register_wrap(desc_label, 520)
        body = tk.Frame(content, bg=bg)
        body.pack(fill="both", expand=True, padx=18, pady=(0, 18))
        return card, body

    def create_stat_block(self, parent, label: str, value: str, tone: str = "blue"):
        palette = {
            "blue": ("#DBEAFE", "#1D4ED8"),
            "orange": ("#FFEDD5", "#C2410C"),
            "slate": ("#E2E8F0", "#334155"),
        }
        bg, fg = palette.get(tone, palette["blue"])
        block = tk.Frame(parent, bg=bg, padx=12, pady=10)
        tk.Label(
            block,
            text=label,
            bg=bg,
            fg=fg,
            font=self.fonts["body_bold"],
        ).pack(anchor="w")
        value_label = tk.Label(
            block,
            text=value,
            bg=bg,
            fg=self.palette["text"],
            font=self.fonts["stat"],
        )
        value_label.pack(anchor="w", pady=(6, 0))
        block.value_label = value_label
        return block

    def make_clickable(self, widget, callback):
        if widget is None or callback is None:
            return
        try:
            widget.configure(cursor="hand2")
        except tk.TclError:
            pass
        widget.bind("<Button-1>", lambda _event: callback())
        for child in widget.winfo_children():
            try:
                child.configure(cursor="hand2")
            except tk.TclError:
                pass
            child.bind("<Button-1>", lambda _event: callback())

    def create_note_box(self, parent, title: str, lines: list[str], tone: str = "slate"):
        tone_palette = {
            "slate": ("#F1F5F9", "#475569"),
            "blue": ("#EFF6FF", "#1D4ED8"),
            "orange": ("#FFF7ED", "#C2410C"),
        }
        bg, fg = tone_palette.get(tone, tone_palette["slate"])
        box = tk.Frame(parent, bg=bg, padx=14, pady=14)
        tk.Label(
            box,
            text=title,
            bg=bg,
            fg=fg,
            font=self.fonts["body_bold"],
        ).pack(anchor="w")
        body_label = tk.Label(
            box,
            text="\n".join(lines),
            bg=bg,
            fg=self.palette["text"],
            justify="left",
            wraplength=560,
            font=self.fonts["body"],
        )
        body_label.pack(anchor="w", pady=(8, 0))
        self.register_wrap(body_label, 560)
        return box

    def create_action_tile(
        self,
        parent,
        title: str,
        description: str,
        badges: list[tuple[str, str]],
        button_text: str,
        command,
        button_style: str,
        header_text: str = "",
        header_kind: str = "readonly",
    ):
        header_palette = {
            "immediate": ("#FFF7ED", "#C2410C"),
            "next": ("#EFF6FF", "#1D4ED8"),
            "danger": ("#FEF2F2", "#B91C1C"),
            "warning": ("#FFFBEB", "#B45309"),
            "readonly": ("#F8FAFC", "#475569"),
            "guide": ("#F8FAFC", "#475569"),
        }
        header_prefix = {
            "immediate": "[LIVE]",
            "next": "[NEXT]",
            "danger": "[DANGER]",
            "warning": "[WARN]",
            "readonly": "[READ]",
            "guide": "[GUIDE]",
        }
        head_bg, head_fg = header_palette.get(header_kind, header_palette["readonly"])
        tile = tk.Frame(
            parent,
            bg=self.palette["surface_alt"],
            highlightthickness=1,
            highlightbackground=self.palette["line"],
            bd=0,
            padx=12,
            pady=12,
        )
        if header_text:
            tk.Label(
                tile,
                text=f"{header_prefix.get(header_kind, '[INFO]')} {header_text}",
            bg=head_bg,
            fg=head_fg,
            padx=10,
            pady=5,
            font=self.fonts["small_bold"],
            ).pack(anchor="w", pady=(0, 10))
        tk.Label(
            tile,
            text=title,
            bg=self.palette["surface_alt"],
            fg=self.palette["text"],
            font=self.fonts["body_bold"],
        ).pack(anchor="w")
        desc_label = tk.Label(
            tile,
            text=description,
            bg=self.palette["surface_alt"],
            fg=self.palette["muted"],
            justify="left",
            wraplength=260,
            font=self.fonts["body"],
        )
        desc_label.pack(anchor="w", pady=(6, 8))
        self.register_wrap(desc_label, 260)
        badge_row = tk.Frame(tile, bg=self.palette["surface_alt"])
        badge_row.pack(fill="x", pady=(0, 10))
        for text, kind in badges:
            self.create_scope_tag(badge_row, text, kind)
        ttk.Button(tile, text=button_text, command=command, style=button_style).pack(fill="x")
        return tile

    def create_status_badge(self, parent, label: str):
        badge = tk.Label(
            parent,
            text=label,
            bg="#E2E8F0",
            fg="#334155",
            padx=10,
            pady=6,
            font=self.fonts["body_bold"],
        )
        badge.pack(side="left", padx=(0, 8))
        return badge

    def create_scope_tag(self, parent, text: str, kind: str = "slate"):
        palette = {
            "live": ("#DCFCE7", "#166534"),
            "immediate": ("#FFEDD5", "#C2410C"),
            "next": ("#DBEAFE", "#1D4ED8"),
            "readonly": ("#E2E8F0", "#334155"),
            "guide": ("#FEF3C7", "#92400E"),
            "slate": ("#E2E8F0", "#334155"),
        }
        bg, fg = palette.get(kind, palette["slate"])
        label = tk.Label(
            parent,
            text=text,
            bg=bg,
            fg=fg,
            padx=10,
            pady=5,
            font=self.fonts["body_bold"],
        )
        label.pack(side="left", padx=(0, 8), pady=(0, 8))
        return label

    def create_scope_row(self, parent, items: list[tuple[str, str]]):
        row = tk.Frame(parent, bg=parent.cget("bg"))
        row.pack(fill="x", pady=(0, 10))
        for text, kind in items:
            self.create_scope_tag(row, text, kind)
        return row

    def add_section_header(self, parent, title: str, description: str):
        ttk.Label(parent, text=title, style="Header.TLabel").pack(anchor="w", pady=(0, 4))
        label = ttk.Label(parent, text=description, style="Subtle.TLabel", wraplength=1200)
        label.pack(anchor="w", pady=(0, 14))
        self.register_wrap(label, 1200)

    def set_stat_value(self, widget, value: str):
        if hasattr(widget, "value_label"):
            widget.value_label.configure(text=str(value))

    def set_status_badge(self, widget, label: str, ok: bool | None):
        if widget is None:
            return
        if ok is True:
            bg, fg = "#DCFCE7", "#166534"
        elif ok is False:
            bg, fg = "#FEE2E2", "#991B1B"
        else:
            bg, fg = "#E2E8F0", "#334155"
        widget.configure(text=label, bg=bg, fg=fg)

    def confirm_action(self, title: str, lines: list[str], danger: bool = False) -> bool:
        message = "\n".join(lines)
        icon = "warning" if danger else "info"
        return messagebox.askyesno(title, message, icon=icon)

    def build_guide_tab(self):
        wrapper = ttk.Frame(self.guide_tab)
        wrapper.pack(fill="both", expand=True, padx=16, pady=16)
        self.add_section_header(
            wrapper,
            "控制台首页",
            "把当前推送状态、内容版本、下一步操作和最近发布记录集中在一页里，进来先看这里。",
        )

        top_stats = tk.Frame(wrapper, bg=self.palette["bg"])
        top_stats.pack(fill="x", pady=(0, 14))
        self.dashboard_auto_chip = self.create_stat_block(top_stats, "当前自动推送", "-", "blue")
        self.dashboard_auto_chip.pack(side="left", fill="x", expand=True, padx=(0, 6))
        self.dashboard_content_chip = self.create_stat_block(top_stats, "当前内容版本", "-", "orange")
        self.dashboard_content_chip.pack(side="left", fill="x", expand=True, padx=6)
        self.dashboard_next_lens_chip = self.create_stat_block(top_stats, "下次发版 Lens 源", "-", "slate")
        self.dashboard_next_lens_chip.pack(side="left", fill="x", expand=True, padx=6)
        self.dashboard_next_plugin_chip = self.create_stat_block(top_stats, "下次发版 Plugin 源", "-", "slate")
        self.dashboard_next_plugin_chip.pack(side="left", fill="x", expand=True, padx=(6, 0))
        self.make_clickable(self.dashboard_auto_chip, lambda: self.show_page("app"))
        self.make_clickable(self.dashboard_content_chip, lambda: self.show_page("data"))
        self.make_clickable(self.dashboard_next_lens_chip, lambda: self.show_page("data"))
        self.make_clickable(self.dashboard_next_plugin_chip, lambda: self.show_page("data"))

        grid = tk.Frame(wrapper, bg=self.palette["bg"])
        grid.pack(fill="both", expand=True)
        grid.grid_columnconfigure(0, weight=2)
        grid.grid_columnconfigure(1, weight=1)
        grid.grid_rowconfigure(0, weight=1)
        grid.grid_rowconfigure(1, weight=1)
        self.guide_grid = grid

        quick_card, quick_body = self.create_card(
            grid,
            "日常发版流程",
            "以后最常用的 4 步，直接按这个顺序做。",
        )
        self.guide_quick_card = quick_card
        quick_card.grid(row=0, column=0, sticky="nsew", padx=(0, 10), pady=(0, 12))
        quick_note = self.create_note_box(
            quick_body,
            "发版最短路径",
            [
                "1. 如需更换资源版本，先去“资源编排”保存下次发版默认源。",
                "2. 提交代码并打应用版本 tag。",
                "3. 等 GitHub Actions 完成。",
                "4. 去“应用发布”选择“发布新应用，但不推送”或“发布并立即推送”。",
            ],
            tone="blue",
        )
        quick_note.pack(fill="x")
        quick_actions = tk.Frame(quick_body, bg=self.palette["surface"])
        quick_actions.pack(fill="x", pady=(14, 0))
        ttk.Button(quick_actions, text="去应用发布", command=lambda: self.show_page("app"), style="Primary.TButton").pack(side="left", fill="x", expand=True)
        ttk.Button(quick_actions, text="去资源编排", command=lambda: self.show_page("data"), style="Secondary.TButton").pack(side="left", fill="x", expand=True, padx=(10, 0))
        home_actions = tk.Frame(quick_body, bg=self.palette["surface"])
        home_actions.pack(fill="x", pady=(12, 0))
        ttk.Button(home_actions, text="创建并推送 Tag", command=self.create_and_push_tag, style="Primary.TButton").pack(fill="x")
        ttk.Button(home_actions, text="刷新 Releases", command=self.load_releases, style="Secondary.TButton").pack(fill="x")
        ttk.Button(home_actions, text="刷新控制面状态", command=self.refresh_runtime_state, style="Secondary.TButton").pack(fill="x", pady=(10, 0))
        ttk.Button(home_actions, text="读取当前默认源", command=self.load_resource_source_state, style="Secondary.TButton").pack(fill="x", pady=(10, 0))
        ttk.Button(home_actions, text="预览中国区 manifest", command=self.preview_default_cn_manifest, style="Future.TButton").pack(fill="x", pady=(10, 0))

        health_card, health_body = self.create_card(
            grid,
            "系统状态",
            "这里显示控制台当前能否连上 GitHub、Vercel，以及 123 配置是否齐全。",
        )
        self.guide_health_card = health_card
        health_card.grid(row=0, column=1, sticky="nsew", padx=(10, 0), pady=(0, 12))
        badges = tk.Frame(health_body, bg=self.palette["surface"])
        badges.pack(fill="x")
        self.github_status_badge = self.create_status_badge(badges, "GitHub：未检测")
        self.vercel_status_badge = self.create_status_badge(badges, "Vercel：未检测")
        self.pan123_status_badge = self.create_status_badge(badges, "123：未配置")
        health_note = self.create_note_box(
            health_body,
            "如果状态异常",
            [
                "GitHub 未连接：先检查 github_token。",
                "Vercel 未连接：先检查 vercel_token 和 project id/name。",
                "123 未配置：检查 GitHub Secrets 和 Vercel Env 中的 PAN123_*。",
            ],
            tone="slate",
        )
        health_note.pack(fill="x", pady=(14, 0))

        release_card, release_body = self.create_card(
            grid,
            "最近 Releases",
            "这里只展示最近几个应用版本，方便确认最近一次发版是否已进入列表。",
        )
        self.guide_release_card = release_card
        release_card.grid(row=1, column=0, sticky="nsew", padx=(0, 10))
        self.dashboard_release_list = tk.Listbox(release_body, height=10)
        self.dashboard_release_list.pack(fill="both", expand=True)
        self.style_listbox(self.dashboard_release_list)
        self.dashboard_release_list.bind("<<ListboxSelect>>", self.on_dashboard_release_select)
        self.register_listbox(self.dashboard_release_list, 10)

        ops_card, ops_body = self.create_card(
            grid,
            "常用操作",
            "遇到问题时，通常先看这里，不必翻完整说明。",
        )
        self.guide_ops_card = ops_card
        ops_card.grid(row=1, column=1, sticky="nsew", padx=(10, 0))
        ops_note = self.create_note_box(
            ops_body,
            "常见判断",
            [
                "只上线不推送：去“应用发布”点“发布新应用，但不推送”。",
                "立刻推送：去“应用发布”点“发布并立即推送”。",
                "只改当前内容版本：去“资源编排”点“仅切换当前内容版本”。",
            ],
            tone="orange",
        )
        ops_note.pack(fill="x")
        ops_actions = tk.Frame(ops_body, bg=self.palette["surface"])
        ops_actions.pack(fill="x", pady=(14, 0))
        ttk.Button(ops_actions, text="去下载与路由", command=lambda: self.show_page("route"), style="Secondary.TButton").pack(fill="x")
        ttk.Button(ops_actions, text="去高级设置", command=lambda: self.show_page("advanced"), style="Secondary.TButton").pack(fill="x", pady=(10, 0))

    def add_labeled_entry(self, parent, label, variable, row, width=40, show=None):
        ttk.Label(parent, text=label).grid(row=row, column=0, sticky="w", pady=4, padx=(0, 8))
        entry = ttk.Entry(parent, textvariable=variable, width=width, show=show)
        entry.grid(row=row, column=1, sticky="we", pady=4)
        self.register_entry(entry, width)

    def build_app_tab(self):
        wrapper = ttk.Frame(self.app_tab)
        wrapper.pack(fill="both", expand=True, padx=16, pady=16)
        self.add_section_header(
            wrapper,
            "应用发布",
            "只做两件事：先发版本，再决定是否推送。上传、资源同步、下载入口更新都会自动完成。",
        )
        grid = tk.Frame(wrapper, bg=self.palette["bg"])
        grid.pack(fill="both", expand=True)
        grid.grid_columnconfigure(0, weight=1, uniform="app")
        grid.grid_columnconfigure(1, weight=1, uniform="app")
        grid.grid_columnconfigure(2, weight=1, uniform="app")
        grid.grid_rowconfigure(1, weight=1)
        self.app_grid = grid

        release_card, release_body = self.create_card(
            grid,
            "1. 选择来源",
            "正式长期版本继续走 GitHub Release；无 Tag 临时版本走 Action 构建。",
            "readonly",
        )
        self.app_release_card = release_card
        release_card.grid(row=0, column=0, rowspan=2, sticky="nsew", padx=(0, 12), pady=(0, 12))
        self.create_scope_row(release_body, [("来源切换", "guide"), ("待选择版本", "readonly")])
        self.app_source_mode_var = tk.StringVar(value=DEFAULT_APP_SOURCE_MODE)
        source_form = ttk.Frame(release_body)
        source_form.pack(fill="x")
        ttk.Label(source_form, text="来源模式").grid(row=0, column=0, sticky="w", pady=4, padx=(0, 8))
        self.app_source_mode_combo = ttk.Combobox(
            source_form,
            textvariable=self.app_source_mode_var,
            values=list(APP_SOURCE_MODE_VALUES),
            state="readonly",
            width=18,
        )
        self.app_source_mode_combo.grid(row=0, column=1, sticky="w", pady=4)
        self.register_entry(self.app_source_mode_combo, 18)
        self.app_source_mode_combo.bind("<<ComboboxSelected>>", lambda _event: self.on_app_source_mode_change())
        self.app_mode_hint = self.create_note_box(
            release_body,
            "模式说明",
            [
                "release：使用 GitHub Release，适合正式长期版本。",
                "artifact：使用无 Tag Action 构建，适合临时版本，默认最长保留 90 天。",
            ],
            tone="blue",
        )
        self.app_mode_hint.pack(fill="x", pady=(10, 12))

        self.release_source_frame = tk.Frame(release_body, bg=self.palette["surface"])
        release_toolbar = tk.Frame(self.release_source_frame, bg=self.palette["surface"])
        release_toolbar.pack(fill="x", pady=(0, 10))
        ttk.Button(release_toolbar, text="刷新 Releases", command=self.load_releases, style="Primary.TButton").pack(fill="x")
        ttk.Button(release_toolbar, text="刷新当前推送状态", command=self.refresh_runtime_state, style="Secondary.TButton").pack(fill="x", pady=(8, 0))
        self.release_list = tk.Listbox(self.release_source_frame, width=38, height=28)
        self.release_list.pack(fill="both", expand=True)
        self.release_list.bind("<<ListboxSelect>>", self.on_release_select)
        self.style_listbox(self.release_list)
        self.register_listbox(self.release_list, 28)

        self.action_build_frame = tk.Frame(release_body, bg=self.palette["surface"])
        build_toolbar = tk.Frame(self.action_build_frame, bg=self.palette["surface"])
        build_toolbar.pack(fill="x", pady=(0, 10))
        ttk.Button(build_toolbar, text="刷新 Action 构建", command=self.load_action_builds, style="Primary.TButton").pack(fill="x")
        ttk.Button(build_toolbar, text="执行 Action 编译（不创建 Tag）", command=self.trigger_action_build_without_tag, style="Secondary.TButton").pack(fill="x", pady=(8, 0))
        self.action_build_list = tk.Listbox(self.action_build_frame, width=38, height=28)
        self.action_build_list.pack(fill="both", expand=True)
        self.action_build_list.bind("<<ListboxSelect>>", self.on_action_build_select)
        self.style_listbox(self.action_build_list)
        self.register_listbox(self.action_build_list, 28)
        link_row = tk.Frame(self.action_build_frame, bg=self.palette["surface"])
        link_row.pack(fill="x", pady=(10, 0))
        ttk.Button(
            link_row,
            text="打开选中构建页面",
            command=self.open_selected_action_build_url,
            style="Secondary.TButton",
        ).pack(anchor="w")
        self.action_build_url_var = tk.StringVar(value="构建页面：未选择")
        self.action_build_url_label = tk.Label(
            self.action_build_frame,
            textvariable=self.action_build_url_var,
            bg=self.palette["surface"],
            fg=self.palette["muted"],
            justify="left",
            anchor="w",
            cursor="arrow",
            font=self.fonts["body"],
        )
        self.action_build_url_label.pack(fill="x", pady=(8, 0))
        self.register_wrap(self.action_build_url_label, 620)
        self.action_build_url_label.bind("<Button-1>", lambda _event: self.open_selected_action_build_url())
        self.selected_action_build_url = ""

        overview_card, overview_body = self.create_card(
            grid,
            "2. 版本信息",
            "确认版本号、来源标识和更新说明，再决定是仅发布还是立即推送。",
            "guide",
        )
        self.app_overview_card = overview_card
        overview_card.grid(row=0, column=1, sticky="nsew", padx=(0, 12), pady=(0, 12))
        self.create_scope_row(overview_body, [("待确认信息", "guide")])
        stats_row = tk.Frame(overview_body, bg=self.palette["surface"])
        stats_row.pack(fill="x", pady=(0, 12))
        self.auto_version_chip = self.create_stat_block(stats_row, "当前自动推送", "-", "blue")
        self.auto_version_chip.pack(side="left", fill="x", expand=True, padx=(0, 6))
        self.manual_count_chip = self.create_stat_block(stats_row, "手动可见版本数", "0", "orange")
        self.manual_count_chip.pack(side="left", fill="x", expand=True, padx=(6, 0))

        form = ttk.Frame(overview_body)
        form.pack(fill="x")
        self.app_version_var = tk.StringVar()
        self.app_tag_var = tk.StringVar()
        self.app_changelog_var = tk.StringVar()
        self.app_recommended_var = tk.BooleanVar(value=True)
        ttk.Label(form, text="版本号").grid(row=0, column=0, sticky="w", pady=4, padx=(0, 8))
        self.app_version_entry = ttk.Entry(form, textvariable=self.app_version_var, width=40)
        self.app_version_entry.grid(row=0, column=1, sticky="we", pady=4)
        self.register_entry(self.app_version_entry, 40)
        self.app_tag_label = ttk.Label(form, text="Tag")
        self.app_tag_label.grid(row=1, column=0, sticky="w", pady=4, padx=(0, 8))
        self.app_tag_entry = ttk.Entry(form, textvariable=self.app_tag_var, width=40)
        self.app_tag_entry.grid(row=1, column=1, sticky="we", pady=4)
        self.register_entry(self.app_tag_entry, 40)
        ttk.Label(form, text="更新说明").grid(row=2, column=0, sticky="w", pady=4, padx=(0, 8))
        self.app_changelog_entry = ttk.Entry(form, textvariable=self.app_changelog_var, width=64)
        self.app_changelog_entry.grid(row=2, column=1, sticky="we", pady=4)
        self.register_entry(self.app_changelog_entry, 64)
        ttk.Checkbutton(form, text="推荐版本", variable=self.app_recommended_var).grid(row=3, column=1, sticky="w", pady=8)
        tag_tools = tk.Frame(overview_body, bg=self.palette["surface"])
        tag_tools.pack(fill="x", pady=(12, 0))
        ttk.Button(tag_tools, text="创建并推送 Tag", command=self.create_and_push_tag, style="Primary.TButton").pack(fill="x")
        ttk.Button(tag_tools, text="执行 Action 编译（不创建 Tag）", command=self.trigger_action_build_without_tag, style="Secondary.TButton").pack(fill="x", pady=(8, 0))
        self.action_build_note = self.create_note_box(
            overview_body,
            "无 Tag 构建提示",
            [
                "artifact 模式下，内部构建ID会自动按 run-<run_id> 生成。",
                f"这类版本默认按临时版本处理，GitHub artifact 最长保留 {APP_ARTIFACT_RETENTION_DAYS} 天。",
            ],
            tone="orange",
        )
        self.action_build_note.pack(fill="x", pady=(12, 0))
        self.on_app_source_mode_change()

        action_card, action_body = self.create_card(
            grid,
            "3. 发布动作",
            "把版本加入手动列表，或直接切换成当前自动推送版本。",
            "immediate",
        )
        self.app_action_card = action_card
        action_card.grid(row=0, column=2, sticky="nsew", pady=(0, 12))
        self.create_scope_row(
            action_body,
            [("发布但不推送：不改当前推送", "readonly"), ("发布并立即推送：立即生效", "immediate")],
        )
        live_note = self.create_note_box(
            action_body,
            "线上影响",
            [
                "橙色按钮会立即改变用户能否收到更新提示。",
                "红色按钮会让版本从可见列表中消失，请谨慎操作。",
            ],
            tone="orange",
        )
        live_note.pack(fill="x", pady=(0, 12))
        action_tiles = tk.Frame(action_body, bg=self.palette["surface"])
        action_tiles.pack(fill="x")
        for cfg in [
            (
                "发布新应用，但不推送",
                "让版本进入手动可见列表，但不改变当前自动推送版本。",
                [("手动可见", "readonly"), ("不改当前推送", "guide")],
                "加入手动列表",
                self.action_add_manual_only,
                "Primary.TButton",
                "手动可见动作",
                "readonly",
            ),
            (
                "发布并立即推送",
                "把这个版本直接切成当前自动推送版本，后续用户会收到更新提示。",
                [("立即生效", "immediate"), ("影响更新提示", "immediate")],
                "立即推送这个版本",
                self.action_publish_and_promote,
                "Immediate.TButton",
                "立即生效动作",
                "immediate",
            ),
            (
                "开始推送已发布版本",
                "不重新打包或上传，只把已经发布过的版本切成当前自动推送版本。",
                [("立即生效", "immediate"), ("不重新上传", "readonly")],
                "切换为当前推送版本",
                self.action_promote_existing,
                "Immediate.TButton",
                "立即生效动作",
                "immediate",
            ),
            (
                "回滚自动推送版本",
                "把自动推送版本切回旧版本，影响后续用户看到的推荐更新。",
                [("立即生效", "immediate"), ("警示操作", "guide")],
                "回滚到选中版本",
                self.action_rollback_auto_version,
                "Warning.TButton",
                "警示操作",
                "warning",
            ),
            (
                "隐藏某个版本",
                "把版本从手动版本列表里移除。如果它是当前推送版本，会自动切到其他版本。",
                [("危险操作", "immediate"), ("会移出列表", "readonly")],
                "隐藏这个版本",
                self.action_hide_selected_version,
                "Danger.TButton",
                "危险操作",
                "danger",
            ),
        ]:
            tile = self.create_action_tile(
                action_tiles,
                cfg[0],
                cfg[1],
                cfg[2],
                cfg[3],
                cfg[4],
                cfg[5],
                cfg[6],
                cfg[7],
            )
            tile.pack(fill="x", pady=(0, 10))
        note = self.create_note_box(
            action_body,
            "操作说明",
            [
                "发布但不推送：版本上线，但不会弹更新。",
                "发布并立即推送：同时上线并切到当前推送版本。",
                "回滚：不会重新打包，只切换推送状态。",
            ],
            tone="blue",
        )
        note.pack(fill="x", pady=(14, 0))

        policy_card, policy_body = self.create_card(
            grid,
            "当前发布策略",
            "这里展示当前线上 release policy，便于核对白名单和自动推送版本。",
            "live",
        )
        self.app_policy_card = policy_card
        policy_card.grid(row=1, column=1, columnspan=2, sticky="nsew")
        self.create_scope_row(policy_body, [("当前线上", "live"), ("只读结果", "readonly")])
        self.policy_text = scrolledtext.ScrolledText(policy_body, wrap="word", height=20)
        self.policy_text.pack(fill="both", expand=True)
        self.style_text_widget(self.policy_text)
        self.register_text_widget(self.policy_text, 20)

    def build_data_tab(self):
        wrapper = ttk.Frame(self.data_tab)
        wrapper.pack(fill="both", expand=True, padx=16, pady=16)
        self.add_section_header(
            wrapper,
            "资源编排",
            "这里统一管理中国区内容版本，以及下次发版要使用的 Lens/CameraDB、Plugin、SDK 源版本。",
        )

        self.content_tag_var = tk.StringVar()
        self.lens_version_var = tk.StringVar()
        self.lens_sha_var = tk.StringVar()
        self.resource_lens_tag_var = tk.StringVar()
        self.resource_plugins_source_mode_var = tk.StringVar(value=DEFAULT_PLUGINS_SOURCE_MODE)
        self.resource_plugins_tag_var = tk.StringVar()
        self.resource_plugins_artifact_name_var = tk.StringVar()
        self.resource_sdk_base_var = tk.StringVar(value=DEFAULT_GLOBAL_SDK_BASE)
        self.resource_saved_snapshot = {}
        self.resource_latest_snapshot = {}
        self.resource_selection_label_var = tk.StringVar(value="当前表单：未选择")
        for watched_var in (
            self.resource_lens_tag_var,
            self.resource_plugins_source_mode_var,
            self.resource_plugins_tag_var,
            self.resource_plugins_artifact_name_var,
            self.resource_sdk_base_var,
        ):
            watched_var.trace_add("write", lambda *_args: self.update_resource_status_text())

        stats_row = tk.Frame(wrapper, bg=self.palette["bg"])
        stats_row.pack(fill="x", pady=(0, 14))
        self.content_tag_chip = self.create_stat_block(stats_row, "当前内容版本", "-", "blue")
        self.content_tag_chip.pack(side="left", fill="x", expand=True, padx=(0, 6))
        self.lens_version_chip = self.create_stat_block(stats_row, "当前 Lens 版本", "-", "orange")
        self.lens_version_chip.pack(side="left", fill="x", expand=True, padx=6)
        self.sdk_source_chip = self.create_stat_block(stats_row, "下次发版 SDK 源", "api.gyroflow.xyz", "slate")
        self.sdk_source_chip.pack(side="left", fill="x", expand=True, padx=(6, 0))

        top = tk.Frame(wrapper, bg=self.palette["bg"])
        top.pack(fill="both", expand=True)
        top.grid_columnconfigure(0, weight=1, uniform="data")
        top.grid_columnconfigure(1, weight=1, uniform="data")
        top.grid_rowconfigure(0, weight=1)
        top.grid_rowconfigure(1, weight=1)
        self.data_grid = top

        current_frame, current_body = self.create_card(
            top,
            "当前线上内容版本",
            "这里展示现在中国区下载实际在使用的内容版本和 Lens 元信息。",
            "live",
        )
        self.data_current_card = current_frame
        current_frame.grid(row=0, column=0, sticky="nsew", padx=(0, 10), pady=(0, 12))
        self.create_scope_row(current_body, [("当前线上", "live"), ("立即生效", "immediate")])
        current_effect_note = self.create_note_box(
            current_body,
            "注意",
            [
                "这里的切换会直接影响中国区用户当前下载到的内容版本。",
                "它不会等待下一次应用发版。",
            ],
            tone="orange",
        )
        current_effect_note.pack(fill="x", pady=(0, 12))
        form = ttk.Frame(current_body)
        form.pack(fill="x")
        self.add_labeled_entry(form, "内容 Release Tag", self.content_tag_var, 0, width=58)
        self.add_labeled_entry(form, "lens 版本", self.lens_version_var, 1)
        self.add_labeled_entry(form, "lens sha256", self.lens_sha_var, 2, width=78)
        current_actions = tk.Frame(current_body, bg=self.palette["surface"])
        current_actions.pack(fill="x", pady=(12, 10))
        current_read_tile = self.create_action_tile(
            current_actions,
            "从选中应用 Release 读取",
            "从当前选中的应用 Release 自动读取内容版本、Lens 版本和 sha256。",
            [("读取摘要", "readonly")],
            "读取版本摘要",
            self.load_data_release_metadata,
            "Secondary.TButton",
            "只读辅助动作",
            "readonly",
        )
        current_read_tile.pack(fill="x", pady=(0, 10))
        current_switch_tile = self.create_action_tile(
            current_actions,
            "仅切换当前内容版本",
            "直接修改当前线上内容版本，不等待下一次应用发版。",
            [("立即生效", "immediate"), ("影响中国区下载", "immediate")],
            "切换当前内容版本",
            self.action_update_data_envs,
            "Immediate.TButton",
            "立即生效动作",
            "immediate",
        )
        current_switch_tile.pack(fill="x")
        self.data_status_text = scrolledtext.ScrolledText(current_body, wrap="word", height=12)
        self.data_status_text.pack(fill="both", expand=True, pady=(4, 0))
        self.style_text_widget(self.data_status_text)
        self.register_text_widget(self.data_status_text, 12)

        source_frame, source_body = self.create_card(
            top,
            "下次发版资源源",
            "这里决定下一次应用发版时，将自动使用哪些 Lens/CameraDB、Plugin 和 SDK 下载源。Plugin 可切换为 Release 或 Action artifact。",
            "next",
        )
        self.data_source_card = source_frame
        source_frame.grid(row=0, column=1, sticky="nsew", padx=(10, 0), pady=(0, 12))
        self.create_scope_row(source_body, [("下次发版生效", "next")])
        source_effect_note = self.create_note_box(
            source_body,
            "影响范围",
            [
                "这里保存的是下一次应用发版要自动使用的资源源。",
                "不会改变当前线上用户下载到的内容。",
                "这里的 SDK 字段写入的是 GitHub Actions Variable：NIYIEN_SDK_BASE。",
                "它用于发版脚本下载 SDK，并进入发版摘要；不是当前线上全局运行时变量 NIYIEN_GLOBAL_SDK_BASE。",
                "Plugin 选择 Action artifact 时，Artifact 名称可留空，留空会自动取插件仓库默认分支最近成功 run。",
            ],
            tone="blue",
        )
        source_effect_note.pack(fill="x", pady=(0, 12))
        source_form = ttk.Frame(source_body)
        source_form.pack(fill="x")
        self.add_labeled_entry(source_form, "Lens/CameraDB Tag", self.resource_lens_tag_var, 0, width=46)
        ttk.Label(source_form, text="Plugin 来源模式").grid(row=1, column=0, sticky="w", pady=4, padx=(0, 8))
        self.plugins_source_mode_combo = ttk.Combobox(
            source_form,
            textvariable=self.resource_plugins_source_mode_var,
            values=list(PLUGINS_SOURCE_MODE_VALUES),
            state="readonly",
            width=18,
        )
        self.plugins_source_mode_combo.grid(row=1, column=1, sticky="w", pady=4)
        self.register_entry(self.plugins_source_mode_combo, 18)
        self.plugins_source_mode_combo.bind("<<ComboboxSelected>>", lambda _event: self.on_plugin_source_mode_change())
        self.add_labeled_entry(source_form, "Plugin Release Tag", self.resource_plugins_tag_var, 2, width=46)
        self.add_labeled_entry(source_form, "Plugin Artifact 名称", self.resource_plugins_artifact_name_var, 3, width=46)
        self.add_labeled_entry(source_form, "发版用 SDK 下载源 (NIYIEN_SDK_BASE)", self.resource_sdk_base_var, 4, width=58)
        selection_label = tk.Label(
            source_body,
            textvariable=self.resource_selection_label_var,
            bg=self.palette["surface"],
            fg=self.palette["muted"],
            justify="left",
            anchor="w",
            font=self.fonts["body"],
        )
        selection_label.pack(fill="x", pady=(10, 0))
        source_actions = tk.Frame(source_body, bg=self.palette["surface"])
        source_actions.pack(fill="x", pady=(12, 10))
        source_tiles = tk.Frame(source_actions, bg=self.palette["surface"])
        source_tiles.pack(fill="x")
        for cfg in [
            (
                "使用上次默认源",
                "从 GitHub Actions Variables 读取上一次保存、下次发版会实际使用的资源源。",
                [("只读", "readonly")],
                "切换到上次默认源",
                self.load_resource_source_state,
                "Secondary.TButton",
                "只读辅助动作",
                "readonly",
            ),
            (
                "使用最新推荐",
                "自动查询最新 Lens/CameraDB Release，并按当前 Plugin 来源模式填入最新推荐值；artifact 模式会自动定位最近成功的 Action run。",
                [("自动查询", "guide"), ("不会立即生效", "next")],
                "切换到最新推荐",
                self.load_latest_resource_sources,
                "Secondary.TButton",
                "预填充动作",
                "guide",
            ),
            (
                "只刷新 Lens 最新",
                "仅自动把 Lens/CameraDB Tag 填成当前最新 release 的 tag。",
                [("预填充", "guide"), ("不会立即生效", "next")],
                "填入最新 Lens/CameraDB Tag",
                self.load_latest_lens_tag,
                "Secondary.TButton",
                "预填充动作",
                "guide",
            ),
            (
                "只刷新 Plugin 最新",
                "仅刷新 Plugin 源；release 模式会填最新 tag，artifact 模式会定位最近成功且可用的 Action run。",
                [("预填充", "guide"), ("不会立即生效", "next")],
                "填入最新 Plugin 源",
                self.load_latest_plugin_tag,
                "Secondary.TButton",
                "预填充动作",
                "guide",
            ),
            (
                "保存为下次发版默认源",
                "把当前填写的 Lens/CameraDB、Plugin、SDK 源写入 GitHub Actions Variables。",
                [("下次发版生效", "next"), ("不影响当前线上", "readonly")],
                "保存为默认资源源",
                self.action_save_resource_sources,
                "Future.TButton",
                "下次发版动作",
                "next",
            ),
        ]:
            tile = self.create_action_tile(
                source_tiles,
                cfg[0],
                cfg[1],
                cfg[2],
                cfg[3],
                cfg[4],
                cfg[5],
                cfg[6],
                cfg[7],
            )
            tile.pack(fill="x", pady=(0, 10))
        self.resource_status_text = scrolledtext.ScrolledText(source_body, wrap="word", height=12)
        self.resource_status_text.pack(fill="both", expand=True, pady=(4, 0))
        self.style_text_widget(self.resource_status_text)
        self.register_text_widget(self.resource_status_text, 12)

        notes_frame, notes_body = self.create_card(
            top,
            "资源策略说明",
            "这块用来区分“现在正在用什么”和“下次发版准备使用什么”。",
            "guide",
        )
        self.data_notes_card = notes_frame
        notes_frame.grid(row=1, column=0, columnspan=2, sticky="nsew")
        self.create_scope_row(notes_body, [("规则说明", "guide")])
        note = self.create_note_box(
            notes_body,
            "你只需要记住两件事",
            [
                "改当前内容版本：只影响现在下载到的内容包。",
                "保存下次发版默认源：只影响下一次应用发版时会自动带上的资源来源。",
                "Lens 和 CameraDB 现在共用同一个 Tag。",
                "Plugin 用 Release 模式时看 Tag；用 Action artifact 模式时看 Artifact 名称。",
            ],
            tone="orange",
        )
        note.pack(fill="x")
        self.on_plugin_source_mode_change()

    def build_route_tab(self):
        wrapper = ttk.Frame(self.route_tab)
        wrapper.pack(fill="both", expand=True, padx=16, pady=16)
        self.add_section_header(
            wrapper,
            "下载与路由",
            "预览客户端实际会拿到的 manifest，重点检查中国区是否走自有下载入口。",
        )
        stats_row = tk.Frame(wrapper, bg=self.palette["bg"])
        stats_row.pack(fill="x", pady=(0, 14))
        self.route_country_chip = self.create_stat_block(stats_row, "当前国家", "CN", "blue")
        self.route_country_chip.pack(side="left", fill="x", expand=True, padx=(0, 6))
        self.route_platform_chip = self.create_stat_block(stats_row, "当前平台", "windows", "slate")
        self.route_platform_chip.pack(side="left", fill="x", expand=True, padx=6)
        self.route_mode_chip = self.create_stat_block(stats_row, "下载模式", "中国区自有入口", "orange")
        self.route_mode_chip.pack(side="left", fill="x", expand=True, padx=(6, 0))

        layout = tk.Frame(wrapper, bg=self.palette["bg"])
        layout.pack(fill="both", expand=True)
        layout.grid_columnconfigure(0, weight=1)
        layout.grid_columnconfigure(1, weight=2)
        layout.grid_rowconfigure(0, weight=1)
        self.route_grid = layout

        form_card, form_body = self.create_card(
            layout,
            "查询条件",
            "输入国家和平台，模拟客户端发起 manifest 请求时的真实返回。",
            "guide",
        )
        self.route_form_card = form_card
        form_card.grid(row=0, column=0, sticky="nsew", padx=(0, 10))
        self.create_scope_row(form_body, [("调试输入", "guide")])
        self.preview_country_var = tk.StringVar(value="CN")
        self.preview_platform_var = tk.StringVar(value="windows")

        form = ttk.Frame(form_body)
        form.pack(fill="x")
        self.add_labeled_entry(form, "国家代码", self.preview_country_var, 0)
        ttk.Label(form, text="平台").grid(row=1, column=0, sticky="w", pady=4)
        ttk.Combobox(
            form,
            textvariable=self.preview_platform_var,
            values=["windows", "macos", "linux", "android"],
            state="readonly",
            width=20,
        ).grid(row=1, column=1, sticky="w", pady=4)
        ttk.Button(form_body, text="预览 manifest 返回结果", command=self.preview_manifest, style="Primary.TButton").pack(fill="x", pady=(12, 12))
        hint = self.create_note_box(
            form_body,
            "建议检查",
            [
                "CN + windows：app/lens/sdk/plugin 都应走 /api/download/...",
                "US + windows：应保持全球源。",
            ],
            tone="blue",
        )
        hint.pack(fill="x")

        preview_card, preview_body = self.create_card(
            layout,
            "Manifest 预览结果",
            "这里直接展示客户端最终会拿到的 JSON 返回。",
            "readonly",
        )
        self.route_preview_card = preview_card
        preview_card.grid(row=0, column=1, sticky="nsew", padx=(10, 0))
        self.create_scope_row(preview_body, [("只读结果", "readonly"), ("中国区重点检查", "immediate")])
        self.route_preview_text = scrolledtext.ScrolledText(preview_body, wrap="word", height=32)
        self.route_preview_text.pack(fill="both", expand=True)
        self.style_text_widget(self.route_preview_text)
        self.register_text_widget(self.route_preview_text, 32)

    def build_stats_tab(self):
        wrapper = ttk.Frame(self.stats_tab)
        wrapper.pack(fill="both", expand=True, padx=16, pady=16)
        self.add_section_header(
            wrapper,
            "统计与观测",
            "把 telemetry 的日常查询和 rebuild 保持在同一页，减少来回切换。",
        )
        layout = tk.Frame(wrapper, bg=self.palette["bg"])
        layout.pack(fill="both", expand=True)
        layout.grid_columnconfigure(0, weight=1)
        layout.grid_columnconfigure(1, weight=2)
        layout.grid_rowconfigure(0, weight=1)
        self.stats_grid = layout

        self.stats_days_var = tk.StringVar(value="7")
        self.stats_product_var = tk.StringVar(value="gyroflow_niyien")
        self.stats_source_var = tk.StringVar(value="")
        self.stats_event_var = tk.StringVar(value="")
        self.rebuild_start_var = tk.StringVar(value="")
        self.rebuild_end_var = tk.StringVar(value="")

        query_card, query_body = self.create_card(
            layout,
            "查询与运维",
            "设置过滤条件、打开 dashboard，或者对某个时间段执行 rebuild。",
            "guide",
        )
        self.stats_query_card = query_card
        query_card.grid(row=0, column=0, sticky="nsew", padx=(0, 10))
        self.create_scope_row(query_body, [("查询操作", "guide"), ("Rebuild 会立即生效", "immediate")])
        form = ttk.Frame(query_body)
        form.pack(fill="x")
        self.add_labeled_entry(form, "统计天数", self.stats_days_var, 0)
        self.add_labeled_entry(form, "product_id", self.stats_product_var, 1)
        self.add_labeled_entry(form, "source_app_id", self.stats_source_var, 2)
        self.add_labeled_entry(form, "event", self.stats_event_var, 3)
        ttk.Button(query_body, text="获取统计 JSON", command=self.fetch_stats, style="Primary.TButton").pack(fill="x", pady=(12, 0))
        ttk.Button(query_body, text="打开 stats.html", command=self.open_stats_page, style="Secondary.TButton").pack(fill="x", pady=(10, 0))
        note = self.create_note_box(
            query_body,
            "Rebuild",
            [
                "只有在统计口径或原始事件需要重建时才使用。",
                "建议先填开始和结束日期，再触发。",
            ],
            tone="slate",
        )
        note.pack(fill="x", pady=(14, 10))
        rebuild_form = ttk.Frame(query_body)
        rebuild_form.pack(fill="x")
        ttk.Label(rebuild_form, text="Rebuild 开始").grid(row=0, column=0, sticky="w", pady=4)
        ttk.Entry(rebuild_form, textvariable=self.rebuild_start_var, width=18).grid(row=0, column=1, sticky="w", pady=4)
        ttk.Label(rebuild_form, text="结束").grid(row=1, column=0, sticky="w", pady=4)
        ttk.Entry(rebuild_form, textvariable=self.rebuild_end_var, width=18).grid(row=1, column=1, sticky="w", pady=4)
        ttk.Button(query_body, text="触发 telemetry rebuild", command=self.trigger_rebuild, style="Warning.TButton").pack(fill="x", pady=(10, 0))

        result_card, result_body = self.create_card(
            layout,
            "统计结果",
            "JSON 查询结果直接显示在这里，便于快速复制和核对。",
            "readonly",
        )
        self.stats_result_card = result_card
        result_card.grid(row=0, column=1, sticky="nsew", padx=(10, 0))
        self.create_scope_row(result_body, [("只读结果", "readonly")])
        self.stats_text = scrolledtext.ScrolledText(result_body, wrap="word", height=28)
        self.stats_text.pack(fill="both", expand=True)
        self.style_text_widget(self.stats_text)
        self.register_text_widget(self.stats_text, 28)

    def build_advanced_tab(self):
        wrapper = ttk.Frame(self.advanced_tab)
        wrapper.pack(fill="both", expand=True, padx=16, pady=16)
        self.add_section_header(
            wrapper,
            "高级设置",
            "这里保存本地连接信息。令牌只需要填写一次，后续打开控制中心会自动读取。",
        )
        layout = tk.Frame(wrapper, bg=self.palette["bg"])
        layout.pack(fill="both", expand=True)
        layout.grid_columnconfigure(0, weight=1)
        layout.grid_columnconfigure(1, weight=1)
        layout.grid_rowconfigure(0, weight=1)
        self.advanced_grid = layout

        self.config_vars = {}
        keys = [
            "vercel_token",
            "vercel_project_id_or_name",
            "vercel_team_id",
            "github_token",
            "github_owner",
            "github_repo",
            "network_proxy",
            "lens_data_owner",
            "lens_data_repo",
            "plugins_owner",
            "plugins_repo",
            "telemetry_base_url",
            "telemetry_stats_token",
            "telemetry_rebuild_token",
            "deploy_hook_url",
        ]
        config_card, config_body = self.create_card(
            layout,
            "本地连接配置",
            "这些配置会保存在本地，下次打开控制中心时自动加载。",
            "guide",
        )
        self.advanced_config_card = config_card
        config_card.grid(row=0, column=0, sticky="nsew", padx=(0, 10))
        self.create_scope_row(config_body, [("本地保存", "live"), ("修改后立即生效", "immediate")])
        form = ttk.Frame(config_body)
        form.pack(fill="x")
        for row, key in enumerate(keys):
            default_value = DEFAULT_NETWORK_PROXY if key == "network_proxy" else ""
            var = tk.StringVar(value=self.config_data.get(key, default_value))
            self.config_vars[key] = var
            self.add_labeled_entry(
                form,
                key,
                var,
                row,
                width=80,
                show="*" if "token" in key and key != "telemetry_base_url" else None,
            )

        actions = tk.Frame(config_body, bg=self.palette["surface"])
        actions.pack(fill="x", pady=(14, 10))
        ttk.Button(actions, text="保存本地配置", command=self.save_config, style="Primary.TButton").pack(fill="x")
        ttk.Button(actions, text="刷新 Vercel 环境变量快照", command=self.refresh_runtime_state, style="Secondary.TButton").pack(fill="x", pady=(10, 0))
        ttk.Button(actions, text="触发 deploy hook", command=self.trigger_deploy_hook, style="Secondary.TButton").pack(fill="x", pady=(10, 0))
        note = self.create_note_box(
            config_body,
            "建议",
            [
                "owner/repo 一般保持默认，除非你真的更换了资源源仓库。",
                "token 只需要填写一次，保存后下次会自动读取。",
                "network_proxy 支持直接填 127.0.0.1:6063，也支持带协议地址。",
            ],
            tone="blue",
        )
        note.pack(fill="x")

        env_card, env_body = self.create_card(
            layout,
            "当前环境变量快照",
            "这里展示当前从 Vercel 读到的线上环境变量，方便你快速核对。",
            "live",
        )
        self.advanced_env_card = env_card
        env_card.grid(row=0, column=1, sticky="nsew", padx=(10, 0))
        self.create_scope_row(env_body, [("当前线上", "live"), ("只读结果", "readonly")])
        self.env_snapshot_text = scrolledtext.ScrolledText(env_body, wrap="word", height=18)
        self.env_snapshot_text.pack(fill="both", expand=True)
        self.style_text_widget(self.env_snapshot_text)
        self.register_text_widget(self.env_snapshot_text, 18)

    def load_releases(self):
        try:
            releases = self.github().list_releases()
            self.current_releases = releases
            self.release_list.delete(0, tk.END)
            for item in releases:
                if item.get("draft"):
                    continue
                suffix = " [pre]" if item.get("prerelease") else ""
                self.release_list.insert(tk.END, f"{item.get('tag_name', '')}{suffix}")
            self.refresh_dashboard_releases()
            if self.pending_release_watch:
                self.check_pending_release_watch()
        except Exception as err:  # pragma: no cover - UI path
            messagebox.showerror("GitHub error", str(err))

    def refresh_runtime_state(self):
        vercel_ok = False
        github_ok = False
        github_variables_ok = False
        github_variables_error = ""
        try:
            self.current_env_records = self.vercel().list_env_records()
            self.current_envs = {
                key: env.get("value", "") for key, env in self.current_env_records.items()
            }
            vercel_ok = True
        except Exception as err:
            self.current_envs = {}
            self.current_env_records = {}
            self.env_snapshot_text.delete("1.0", tk.END)
            self.env_snapshot_text.insert("1.0", f"Failed to load Vercel envs:\n{err}\n")
            self.policy_text.delete("1.0", tk.END)
            self.policy_text.insert("1.0", "{}\n")
            self.refresh_dashboard_releases()
            self.refresh_visual_summaries(vercel_ok=False, github_ok=False)
            return

        try:
            self.current_repo_variables = self.github().list_actions_variables()
            github_variables_ok = True
        except Exception as err:
            self.current_repo_variables = {}
            github_variables_error = str(err)

        try:
            if not self.current_releases:
                self.current_releases = self.github().list_releases()
            github_ok = True
        except Exception:
            github_ok = False

        self.current_policy = self.load_policy_from_env()
        self.policy_text.delete("1.0", tk.END)
        self.policy_text.insert(
            "1.0", json.dumps(self.current_policy, indent=2, ensure_ascii=False)
        )
        self.env_snapshot_text.delete("1.0", tk.END)
        self.env_snapshot_text.insert(
            "1.0", json.dumps(self.current_envs, indent=2, ensure_ascii=False)
        )

        self.content_tag_var.set(self.current_envs.get("NIYIEN_CONTENT_RELEASE_TAG", ""))
        self.lens_version_var.set(str(self.current_envs.get("NIYIEN_LENS_VERSION", "")))
        self.lens_sha_var.set(self.current_envs.get("NIYIEN_LENS_SHA256", ""))
        self.update_data_status_text()
        self.load_resource_source_state()
        self.refresh_dashboard_releases()
        self.refresh_visual_summaries(
            vercel_ok=vercel_ok,
            github_ok=github_ok,
            github_variables_ok=github_variables_ok,
            github_variables_error=github_variables_error,
        )

    def refresh_dashboard_releases(self):
        if not hasattr(self, "dashboard_release_list"):
            return
        self.dashboard_release_list.delete(0, tk.END)
        visible = [item for item in self.current_releases if not item.get("draft")]
        if not visible:
            self.dashboard_release_list.insert(tk.END, "暂无已加载的 Release，先点击“刷新 Releases”")
            return
        for item in visible[:8]:
            suffix = " [pre]" if item.get("prerelease") else ""
            self.dashboard_release_list.insert(tk.END, f"{item.get('tag_name', '')}{suffix}")

    def get_app_source_mode(self) -> str:
        mode = self.app_source_mode_var.get().strip().lower() if hasattr(self, "app_source_mode_var") else ""
        return mode if mode in APP_SOURCE_MODE_VALUES else DEFAULT_APP_SOURCE_MODE

    def get_string_var_value(self, attr_name: str, default: str = "") -> str:
        variable = getattr(self, attr_name, None)
        if variable is None:
            return default
        try:
            return str(variable.get()).strip()
        except Exception:
            return default

    def set_app_tag_entry_state(self, readonly: bool):
        if not hasattr(self, "app_tag_entry"):
            return
        self.app_tag_entry.configure(state="normal")
        if readonly:
            self.app_tag_entry.configure(state="readonly")

    def on_app_source_mode_change(self):
        mode = self.get_app_source_mode()
        if hasattr(self, "release_source_frame"):
            self.release_source_frame.pack_forget()
        if hasattr(self, "action_build_frame"):
            self.action_build_frame.pack_forget()

        if mode == "artifact":
            self.action_build_frame.pack(fill="both", expand=True)
            self.app_tag_label.configure(text="Tag / 构建ID")
            self.set_app_tag_entry_state(True)
            if not self.current_action_builds and hasattr(self, "action_build_list"):
                self.load_action_builds()
            elif hasattr(self, "action_build_list") and self.action_build_list.curselection():
                selected_index = self.action_build_list.curselection()[0]
                if selected_index < len(self.current_action_builds):
                    self.update_action_build_link(self.current_action_builds[selected_index])
        else:
            self.release_source_frame.pack(fill="both", expand=True)
            self.app_tag_label.configure(text="Tag")
            self.set_app_tag_entry_state(False)
            self.update_action_build_link(None)

        self.refresh_visual_summaries()

    def get_plugin_source_mode(self) -> str:
        mode = self.get_string_var_value("resource_plugins_source_mode_var").lower()
        return mode if mode in PLUGINS_SOURCE_MODE_VALUES else DEFAULT_PLUGINS_SOURCE_MODE

    def get_plugin_source_summary(self) -> str:
        mode = self.get_plugin_source_mode()
        if mode == "artifact":
            artifact_name = self.get_string_var_value("resource_plugins_artifact_name_var")
            return f"artifact / {artifact_name or '自动最新'}"
        tag = self.get_string_var_value("resource_plugins_tag_var")
        return f"release / {tag or 'latest'}"

    def get_next_release_sdk_base(self) -> str:
        value = (
            self.get_string_var_value("resource_sdk_base_var")
            or str(self.current_repo_variables.get("NIYIEN_SDK_BASE", "")).strip()
            or DEFAULT_GLOBAL_SDK_BASE
        )
        return value.rstrip("/") + "/"

    def get_preview_global_sdk_base(self, auto_entry: dict | None) -> str:
        value = (
            str((auto_entry or {}).get("global_sdk_base", "")).strip()
            or str(self.current_repo_variables.get("NIYIEN_SDK_BASE", "")).strip()
            or str(self.current_envs.get("NIYIEN_GLOBAL_SDK_BASE", "")).strip()
            or DEFAULT_GLOBAL_SDK_BASE
        )
        return value.rstrip("/") + "/"

    def get_preview_global_plugins_base(self, auto_entry: dict | None) -> str:
        value = (
            str((auto_entry or {}).get("global_plugins_base", "")).strip()
            or str(self.current_envs.get("NIYIEN_GLOBAL_PLUGINS_BASE", "")).strip()
            or DEFAULT_GLOBAL_PLUGINS_BASE
        )
        return value.rstrip("/") + "/"

    def collect_resource_source_values(self) -> dict:
        return {
            "NIYIEN_LENS_DATA_TAG": self.resource_lens_tag_var.get().strip(),
            "NIYIEN_PLUGINS_SOURCE_MODE": self.get_plugin_source_mode(),
            "NIYIEN_PLUGINS_TAG": self.resource_plugins_tag_var.get().strip(),
            "NIYIEN_PLUGINS_ARTIFACT_NAME": self.resource_plugins_artifact_name_var.get().strip(),
            "NIYIEN_SDK_BASE": self.resource_sdk_base_var.get().strip() or DEFAULT_GLOBAL_SDK_BASE,
        }

    def snapshot_resource_source_values_from_repo_vars(self) -> dict:
        plugin_source_mode = str(
            self.current_repo_variables.get("NIYIEN_PLUGINS_SOURCE_MODE", DEFAULT_PLUGINS_SOURCE_MODE)
        ).strip().lower()
        if plugin_source_mode not in PLUGINS_SOURCE_MODE_VALUES:
            plugin_source_mode = DEFAULT_PLUGINS_SOURCE_MODE
        return {
            "NIYIEN_LENS_DATA_TAG": str(self.current_repo_variables.get("NIYIEN_LENS_DATA_TAG", "")).strip(),
            "NIYIEN_PLUGINS_SOURCE_MODE": plugin_source_mode,
            "NIYIEN_PLUGINS_TAG": str(self.current_repo_variables.get("NIYIEN_PLUGINS_TAG", "")).strip(),
            "NIYIEN_PLUGINS_ARTIFACT_NAME": str(self.current_repo_variables.get("NIYIEN_PLUGINS_ARTIFACT_NAME", "")).strip(),
            "NIYIEN_SDK_BASE": str(self.current_repo_variables.get("NIYIEN_SDK_BASE", DEFAULT_GLOBAL_SDK_BASE)).strip() or DEFAULT_GLOBAL_SDK_BASE,
        }

    def apply_resource_source_values(self, values: dict, *, selection_label: str = ""):
        self.resource_lens_tag_var.set(str(values.get("NIYIEN_LENS_DATA_TAG", "")).strip())
        plugin_source_mode = str(
            values.get("NIYIEN_PLUGINS_SOURCE_MODE", DEFAULT_PLUGINS_SOURCE_MODE)
        ).strip().lower()
        if plugin_source_mode not in PLUGINS_SOURCE_MODE_VALUES:
            plugin_source_mode = DEFAULT_PLUGINS_SOURCE_MODE
        self.resource_plugins_source_mode_var.set(plugin_source_mode)
        self.resource_plugins_tag_var.set(str(values.get("NIYIEN_PLUGINS_TAG", "")).strip())
        self.resource_plugins_artifact_name_var.set(
            str(values.get("NIYIEN_PLUGINS_ARTIFACT_NAME", "")).strip()
        )
        self.resource_sdk_base_var.set(
            str(values.get("NIYIEN_SDK_BASE", DEFAULT_GLOBAL_SDK_BASE)).strip() or DEFAULT_GLOBAL_SDK_BASE
        )
        if selection_label:
            self.resource_selection_label_var.set(selection_label)
        self.on_plugin_source_mode_change()
        self.update_resource_status_text()

    def is_resource_source_incomplete(self, values: dict | None = None) -> bool:
        payload = values or self.collect_resource_source_values()
        if not str(payload.get("NIYIEN_LENS_DATA_TAG", "")).strip():
            return True
        mode = str(payload.get("NIYIEN_PLUGINS_SOURCE_MODE", DEFAULT_PLUGINS_SOURCE_MODE)).strip().lower()
        if mode == "artifact":
            return False
        return not str(payload.get("NIYIEN_PLUGINS_TAG", "")).strip()

    def fetch_latest_resource_source_values(self) -> dict:
        latest_values, _meta = self.fetch_latest_resource_source_values_with_meta()
        return latest_values

    def fetch_latest_resource_source_values_with_meta(self) -> tuple[dict, dict]:
        lens_release, _lens_ref = self.get_latest_release_with_fallback(
            owner=self.config_data.get("lens_data_owner", DEFAULT_LENS_DATA_OWNER),
            repo=self.config_data.get("lens_data_repo", DEFAULT_LENS_DATA_REPO),
            fallback_owner=DEFAULT_LENS_DATA_OWNER,
            fallback_repo=DEFAULT_LENS_DATA_REPO,
            label="Lens/CameraDB",
        )
        plugin_values, plugin_meta = self.resolve_latest_plugin_source_values()
        latest_values = {
            "NIYIEN_LENS_DATA_TAG": str(lens_release.get("tag_name", "")).strip(),
            "NIYIEN_PLUGINS_SOURCE_MODE": plugin_values["NIYIEN_PLUGINS_SOURCE_MODE"],
            "NIYIEN_PLUGINS_TAG": plugin_values["NIYIEN_PLUGINS_TAG"],
            "NIYIEN_PLUGINS_ARTIFACT_NAME": plugin_values["NIYIEN_PLUGINS_ARTIFACT_NAME"],
            "NIYIEN_SDK_BASE": DEFAULT_GLOBAL_SDK_BASE,
        }
        return latest_values, {
            "plugin": plugin_meta,
        }

    def resolve_latest_plugin_source_values(
        self,
        *,
        source_mode: str | None = None,
        artifact_name: str | None = None,
    ) -> tuple[dict, dict]:
        mode = str(source_mode or self.get_plugin_source_mode()).strip().lower()
        if mode not in PLUGINS_SOURCE_MODE_VALUES:
            mode = DEFAULT_PLUGINS_SOURCE_MODE

        requested_artifact_name = (
            self.get_string_var_value("resource_plugins_artifact_name_var")
            if artifact_name is None
            else str(artifact_name or "").strip()
        )
        if mode == "artifact":
            artifact_source, source_ref = self.get_latest_plugin_artifact_source_with_fallback(
                owner=self.config_data.get("plugins_owner", DEFAULT_PLUGINS_OWNER),
                repo=self.config_data.get("plugins_repo", DEFAULT_PLUGINS_REPO),
                fallback_owner=DEFAULT_PLUGINS_OWNER,
                fallback_repo=DEFAULT_PLUGINS_REPO,
                artifact_name=requested_artifact_name,
            )
            run_id = int(artifact_source.get("run_id", 0) or 0)
            source_label = f"{source_ref} run #{run_id}"
            branch = str(artifact_source.get("branch", "")).strip()
            artifact_names = [
                str(item).strip()
                for item in (artifact_source.get("artifact_names") or [])
                if str(item).strip()
            ]
            effective_artifact_name = requested_artifact_name or ", ".join(artifact_names)
            if branch:
                source_label = f"{source_label} ({branch})"
            return {
                "NIYIEN_PLUGINS_SOURCE_MODE": "artifact",
                "NIYIEN_PLUGINS_TAG": "",
                "NIYIEN_PLUGINS_ARTIFACT_NAME": effective_artifact_name,
            }, {
                "mode": "artifact",
                "source_ref": source_ref,
                "source_label": source_label,
                "run_id": run_id,
                "branch": branch,
                "url": str(artifact_source.get("html_url", "")).strip(),
                "artifact_names": artifact_names,
                "effective_artifact_name": effective_artifact_name,
            }

        plugin_release, source_ref = self.get_latest_release_with_fallback(
            owner=self.config_data.get("plugins_owner", DEFAULT_PLUGINS_OWNER),
            repo=self.config_data.get("plugins_repo", DEFAULT_PLUGINS_REPO),
            fallback_owner=DEFAULT_PLUGINS_OWNER,
            fallback_repo=DEFAULT_PLUGINS_REPO,
            label="Plugin",
        )
        tag = str(plugin_release.get("tag_name", "")).strip()
        return {
            "NIYIEN_PLUGINS_SOURCE_MODE": "release",
            "NIYIEN_PLUGINS_TAG": tag,
            "NIYIEN_PLUGINS_ARTIFACT_NAME": "",
        }, {
            "mode": "release",
            "source_ref": source_ref,
            "source_label": source_ref,
            "tag": tag,
            "url": str(plugin_release.get("html_url", "")).strip(),
        }

    def on_plugin_source_mode_change(self):
        self.update_resource_status_text()

    def get_latest_release_with_fallback(
        self,
        *,
        owner: str,
        repo: str,
        fallback_owner: str,
        fallback_repo: str,
        label: str,
    ) -> tuple[dict, str]:
        primary_owner = str(owner or "").strip() or fallback_owner
        primary_repo = str(repo or "").strip() or fallback_repo
        candidates = [(primary_owner, primary_repo)]
        if (primary_owner, primary_repo) != (fallback_owner, fallback_repo):
            candidates.append((fallback_owner, fallback_repo))

        errors = []
        for candidate_owner, candidate_repo in candidates:
            try:
                release = self.github().get_latest_release(candidate_owner, candidate_repo)
                return release, f"{candidate_owner}/{candidate_repo}"
            except Exception as err:
                errors.append(f"{candidate_owner}/{candidate_repo}: {err}")

        raise RuntimeError(
            f"无法读取 {label} 最新 Release。\n" + "\n".join(errors)
        )

    def get_latest_plugin_artifact_source_with_fallback(
        self,
        *,
        owner: str,
        repo: str,
        fallback_owner: str,
        fallback_repo: str,
        artifact_name: str = "",
    ) -> tuple[dict, str]:
        primary_owner = str(owner or "").strip() or fallback_owner
        primary_repo = str(repo or "").strip() or fallback_repo
        candidates = [(primary_owner, primary_repo)]
        if (primary_owner, primary_repo) != (fallback_owner, fallback_repo):
            candidates.append((fallback_owner, fallback_repo))

        requested_artifacts = parse_csv_list(artifact_name)
        requested_set = set(requested_artifacts)
        github = self.github()
        errors = []
        for candidate_owner, candidate_repo in candidates:
            try:
                repository = github.get_repository(candidate_owner, candidate_repo)
                branch = str(repository.get("default_branch", "")).strip()
                if not branch:
                    raise RuntimeError(f"无法确定默认分支：{candidate_owner}/{candidate_repo}")
                runs = github.list_repo_workflow_runs(
                    candidate_owner,
                    candidate_repo,
                    branch=branch,
                    per_page=20,
                )
                if not runs:
                    raise RuntimeError(f"{candidate_owner}/{candidate_repo} 的 {branch} 分支没有 workflow runs")

                rejected: list[str] = []
                for run in runs:
                    if str(run.get("conclusion", "")).strip().lower() != "success":
                        continue
                    run_id = int(run.get("id", 0) or 0)
                    if run_id <= 0:
                        continue
                    artifacts = github.list_repo_run_artifacts(candidate_owner, candidate_repo, run_id)
                    valid_artifacts = [
                        item
                        for item in artifacts
                        if not bool(item.get("expired"))
                        and (
                            not requested_set
                            or str(item.get("name", "")).strip() in requested_set
                        )
                    ]
                    if requested_artifacts:
                        matched_names = {
                            str(item.get("name", "")).strip()
                            for item in valid_artifacts
                        }
                        missing = [name for name in requested_artifacts if name not in matched_names]
                        if missing:
                            rejected.append(f"run {run_id} 缺少 artifact：{', '.join(missing)}")
                            continue
                    elif not valid_artifacts:
                        rejected.append(f"run {run_id} 没有可用 artifact")
                        continue

                    return {
                        "run_id": run_id,
                        "branch": branch,
                        "artifact_names": [
                            str(item.get("name", "")).strip()
                            for item in valid_artifacts
                            if str(item.get("name", "")).strip()
                        ],
                        "html_url": str(run.get("html_url", "")).strip()
                        or f"https://github.com/{candidate_owner}/{candidate_repo}/actions/runs/{run_id}",
                    }, f"{candidate_owner}/{candidate_repo}"

                if rejected:
                    raise RuntimeError(
                        "最近成功构建里没有匹配的 artifact。"
                        + "；".join(rejected[:3])
                    )
                raise RuntimeError("最近 20 次 completed runs 中没有成功且可用的 artifact run")
            except Exception as err:
                errors.append(f"{candidate_owner}/{candidate_repo}: {err}")

        requested_text = ", ".join(requested_artifacts) if requested_artifacts else "自动最新"
        raise RuntimeError(
            "无法读取 Plugin 最新 Artifact。\n"
            f"当前 artifact 过滤：{requested_text}\n"
            + "\n".join(errors)
        )

    def refresh_visual_summaries(
        self,
        vercel_ok: bool | None = None,
        github_ok: bool | None = None,
        github_variables_ok: bool | None = None,
        github_variables_error: str = "",
    ):
        auto_version = self.current_policy.get("auto_version", "") or "-"
        manual_count = len(
            [item for item in self.current_policy.get("versions", []) if "manual" in item.get("channels", [])]
        )
        content_tag_value = self.get_string_var_value("content_tag_var")
        lens_version_value = self.get_string_var_value("lens_version_var")
        sdk_base_value = self.get_next_release_sdk_base()
        resource_lens_tag_value = self.get_string_var_value("resource_lens_tag_var")
        self.set_stat_value(getattr(self, "auto_version_chip", None), auto_version)
        self.set_stat_value(getattr(self, "manual_count_chip", None), manual_count)
        self.set_stat_value(getattr(self, "dashboard_auto_chip", None), auto_version)
        self.set_stat_value(
            getattr(self, "content_tag_chip", None),
            content_tag_value or "-",
        )
        self.set_stat_value(
            getattr(self, "dashboard_content_chip", None),
            content_tag_value or "-",
        )
        self.set_stat_value(
            getattr(self, "lens_version_chip", None),
            lens_version_value or "-",
        )
        self.set_stat_value(
            getattr(self, "sdk_source_chip", None),
            sdk_base_value.replace("https://", "").replace("http://", "").rstrip("/"),
        )
        self.set_stat_value(
            getattr(self, "dashboard_next_lens_chip", None),
            resource_lens_tag_value or "-",
        )
        self.set_stat_value(
            getattr(self, "dashboard_next_plugin_chip", None),
            self.get_plugin_source_summary(),
        )
        self.set_stat_value(getattr(self, "route_country_chip", None), self.preview_country_var.get().strip() if hasattr(self, "preview_country_var") else "CN")
        self.set_stat_value(getattr(self, "route_platform_chip", None), self.preview_platform_var.get().strip() if hasattr(self, "preview_platform_var") else "windows")
        is_cn = False
        if hasattr(self, "preview_country_var"):
            is_cn = self.preview_country_var.get().strip().upper() in set(
                self.distribution_config.get("routing", {}).get("cn_countries", [])
            )
        self.set_stat_value(
            getattr(self, "route_mode_chip", None),
            "中国区自有入口" if is_cn else "全球直连源",
        )
        if vercel_ok is None:
            vercel_ok = bool(self.current_envs)
        if github_ok is None:
            github_ok = bool(self.current_releases)
        if github_variables_ok is None:
            github_variables_ok = bool(self.current_repo_variables)
        pan123_ok = all(self.is_vercel_env_configured(key) for key in ("PAN123_CLIENT_ID", "PAN123_CLIENT_SECRET", "PAN123_RELEASES_ROOT_ID"))
        self.set_status_badge(getattr(self, "vercel_status_badge", None), f"Vercel：{'已连接' if vercel_ok else '未连接'}", vercel_ok)
        if github_ok and github_variables_ok:
            github_label = "GitHub：已连接"
            github_state = True
        elif github_ok and not github_variables_ok:
            github_label = "GitHub：Variables 权限不足"
            github_state = False
        else:
            github_label = "GitHub：未连接"
            github_state = False
        self.set_status_badge(getattr(self, "github_status_badge", None), github_label, github_state)
        self.set_status_badge(getattr(self, "pan123_status_badge", None), f"123：{'已配置' if pan123_ok else '未配置'}", pan123_ok)
        self.set_status_badge(
            getattr(self, "global_auto_badge", None),
            f"自动推送：{auto_version}",
            bool(self.current_policy.get("auto_version", "")),
        )
        self.set_status_badge(
            getattr(self, "global_content_badge", None),
            f"内容版本：{content_tag_value or '-'}",
            bool(content_tag_value),
        )
        overall_ok = vercel_ok and github_ok and pan123_ok
        self.set_status_badge(
            getattr(self, "global_conn_badge", None),
            "控制面：已就绪" if overall_ok else "控制面：待配置",
            overall_ok,
        )

    def get_head_commit_sha(self) -> str:
        try:
            return self.run_git("rev-parse", "HEAD").stdout.strip()
        except Exception:
            return ""

    def get_remote_branch_sha(self, branch: str) -> str:
        if not branch:
            return ""
        try:
            result = self.run_git(
                "ls-remote",
                "--heads",
                self.git_remote_name(),
                f"refs/heads/{branch}",
            ).stdout.strip()
        except Exception:
            return ""
        if not result:
            return ""
        return result.split()[0].strip()

    def make_action_build_id(self, run_id: int | str) -> str:
        return f"run-{int(run_id)}"

    def normalize_action_build_entry(self, run: dict, artifacts: list[dict]) -> dict:
        artifact_names = sorted(
            str(item.get("name", "")).strip()
            for item in artifacts or []
            if not bool(item.get("expired")) and str(item.get("name", "")).strip()
        )
        artifact_set = set(artifact_names)
        normalized_artifact_names = {name.strip().lower() for name in artifact_names}
        return {
            "run_id": int(run.get("id", 0) or 0),
            "status": str(run.get("status", "")).strip(),
            "conclusion": str(run.get("conclusion", "")).strip(),
            "branch": str(run.get("head_branch", "")).strip(),
            "head_sha": str(run.get("head_sha", "")).strip(),
            "title": str(run.get("display_title", "")).strip(),
            "created_at": str(run.get("created_at", "")).strip(),
            "updated_at": str(run.get("updated_at", "")).strip(),
            "html_url": str(run.get("html_url", "")).strip(),
            "artifact_names": artifact_names,
            "has_windows": bool(
                artifact_set.intersection(action_artifact_aliases("windows"))
                or normalized_artifact_names.intersection(
                    {item.lower() for item in action_artifact_aliases("windows")}
                )
            ),
            "has_macos": bool(
                artifact_set.intersection(action_artifact_aliases("macos"))
                or normalized_artifact_names.intersection(
                    {item.lower() for item in action_artifact_aliases("macos")}
                )
            ),
        }

    def format_action_build_entry(self, entry: dict) -> str:
        run_id = entry.get("run_id", 0)
        branch = entry.get("branch", "") or "-"
        status = entry.get("status", "") or "-"
        conclusion = entry.get("conclusion", "") or "-"
        availability = []
        if entry.get("has_windows"):
            availability.append("win")
        if entry.get("has_macos"):
            availability.append("mac")
        artifacts = "/".join(availability) if availability else "-"
        state = status if status != "completed" else conclusion
        run_url = entry.get("html_url", "") or "-"
        return f"#{run_id} | {branch} | {state} | {artifacts} | {run_url}"

    def update_action_build_link(self, entry: dict | None = None):
        url = str((entry or {}).get("html_url", "")).strip()
        self.selected_action_build_url = url
        if not hasattr(self, "action_build_url_var") or not hasattr(self, "action_build_url_label"):
            return
        if url:
            self.action_build_url_var.set(f"构建页面：{url}")
            self.action_build_url_label.configure(
                fg=self.palette["primary"],
                cursor="hand2",
            )
        else:
            self.action_build_url_var.set("构建页面：未选择")
            self.action_build_url_label.configure(
                fg=self.palette["muted"],
                cursor="arrow",
            )

    def open_selected_action_build_url(self):
        url = str(getattr(self, "selected_action_build_url", "")).strip()
        if url:
            webbrowser.open(url)

    def select_action_build_in_list(self, run_id: int) -> bool:
        target = int(run_id or 0)
        if target <= 0 or not hasattr(self, "action_build_list"):
            return False
        for index, entry in enumerate(self.current_action_builds):
            if int(entry.get("run_id", 0) or 0) == target:
                self.action_build_list.selection_clear(0, tk.END)
                self.action_build_list.selection_set(index)
                self.action_build_list.activate(index)
                self.action_build_list.see(index)
                self.on_action_build_select()
                return True
        return False

    def load_action_builds(self):
        try:
            runs = self.github().list_workflow_runs(
                APP_BUILD_WORKFLOW_FILE,
                event="workflow_dispatch",
                per_page=20,
            )
            current_selection = None
            if hasattr(self, "action_build_list") and self.action_build_list.curselection():
                selected_index = self.action_build_list.curselection()[0]
                if selected_index < len(self.current_action_builds):
                    current_selection = int(self.current_action_builds[selected_index].get("run_id", 0) or 0)

            entries = []
            for run in runs:
                run_id = int(run.get("id", 0) or 0)
                artifacts = self.github().list_run_artifacts(run_id) if run_id > 0 else []
                entries.append(self.normalize_action_build_entry(run, artifacts))

            self.current_action_builds = entries
            self.action_build_list.delete(0, tk.END)
            self.update_action_build_link(None)
            if not entries:
                self.action_build_list.insert(tk.END, "暂无 Action 构建，先点击“执行 Action 编译（不创建 Tag）”")
            else:
                for entry in entries:
                    self.action_build_list.insert(tk.END, self.format_action_build_entry(entry))
            if current_selection:
                self.select_action_build_in_list(current_selection)
            if self.pending_action_build_watch:
                self.check_pending_action_build_watch()
        except Exception as err:
            messagebox.showerror("GitHub error", str(err))

    def on_action_build_select(self, _event=None):
        index = self.action_build_list.curselection()
        if not index:
            return
        if index[0] >= len(self.current_action_builds):
            return
        entry = self.current_action_builds[index[0]]
        self.app_tag_var.set(self.make_action_build_id(entry["run_id"]))
        if not self.app_changelog_var.get().strip():
            self.app_changelog_var.set(entry.get("title", ""))
        self.update_action_build_link(entry)
        self.refresh_visual_summaries()

    def trigger_action_build_without_tag(self):
        branch = self.get_current_branch().strip()
        if not branch:
            messagebox.showerror("无法触发", "当前不是一个可识别的本地分支")
            return
        local_head = self.get_head_commit_sha().strip()
        remote_head = self.get_remote_branch_sha(branch).strip()
        if not local_head:
            messagebox.showerror("无法触发", "无法读取当前本地 HEAD")
            return
        if not remote_head:
            messagebox.showerror("无法触发", f"远端 {self.git_remote_name()} 上还没有分支 {branch}，请先 push")
            return
        if local_head != remote_head:
            messagebox.showerror(
                "请先 push",
                "\n".join(
                    [
                        f"当前分支：{branch}",
                        "Action 编译只会基于远端已推送分支运行。",
                        "请先把当前提交 push 到远端，再回来点击这个按钮。",
                    ]
                ),
            )
            return

        lines = [
            f"仓库路径：{self.repo_workdir()}",
            f"当前分支：{branch}",
            f"远端：{self.git_remote_name()}",
            f"远端 HEAD：{remote_head[:12]}",
            "这个操作会触发 workflow_dispatch 编译，但不会创建 Tag。",
        ]
        if not self.confirm_action("确认执行 Action 编译", lines):
            return

        try:
            label = f"control-center-{int(time.time())}"
            self.github().dispatch_workflow(
                APP_BUILD_WORKFLOW_FILE,
                branch,
                inputs={"build_label": label},
            )
            self.app_source_mode_var.set("artifact")
            self.on_app_source_mode_change()
            self.begin_action_build_watch(branch=branch, head_sha=remote_head, started_at=time.time())
            messagebox.showinfo("已触发", "Action 编译已触发，正在自动刷新构建列表。")
        except Exception as err:
            messagebox.showerror("触发失败", str(err))

    def begin_action_build_watch(self, *, branch: str, head_sha: str, started_at: float):
        self.pending_action_build_watch = {
            "branch": branch.strip(),
            "head_sha": head_sha.strip(),
            "started_at": float(started_at),
            "attempt": 0,
            "max_attempts": 36,
            "interval_ms": 5000,
        }
        self.show_page("app")
        self.load_action_builds()
        self.after(5000, self.poll_pending_action_build_watch)

    def poll_pending_action_build_watch(self):
        if not self.pending_action_build_watch:
            return
        watch = self.pending_action_build_watch
        watch["attempt"] += 1
        self.load_action_builds()
        if not self.pending_action_build_watch:
            return
        if watch["attempt"] >= watch["max_attempts"]:
            self.pending_action_build_watch = None
            messagebox.showinfo(
                "构建已触发",
                "Action 构建已经触发成功。如果列表里还没出现，可以稍后手动点“刷新 Action 构建”。",
            )
            return
        self.after(watch["interval_ms"], self.poll_pending_action_build_watch)

    def check_pending_action_build_watch(self):
        watch = self.pending_action_build_watch
        if not watch:
            return
        for entry in self.current_action_builds:
            created_at = self.parse_time_seconds(entry.get("created_at", ""))
            if (
                entry.get("branch") == watch.get("branch")
                and entry.get("head_sha") == watch.get("head_sha")
                and created_at >= watch.get("started_at", 0) - 5
            ):
                self.pending_action_build_watch = None
                self.select_action_build_in_list(int(entry.get("run_id", 0) or 0))
                messagebox.showinfo(
                    "构建已进入列表",
                    f"Action 构建 #{entry.get('run_id')} 已出现在列表中，并已自动选中。",
                )
                return

    def parse_time_seconds(self, value: str) -> float:
        text = str(value or "").strip()
        if not text:
            return 0.0
        try:
            return datetime.fromisoformat(text.replace("Z", "+00:00")).timestamp()
        except ValueError:
            return 0.0

    def selected_action_build_payload(self):
        index = self.action_build_list.curselection()
        if not index:
            raise RuntimeError("请先选择一个 Action 构建")
        if index[0] >= len(self.current_action_builds):
            raise RuntimeError("无法找到选中的 Action 构建")
        entry = self.current_action_builds[index[0]]
        if str(entry.get("status", "")).strip() != "completed" or str(entry.get("conclusion", "")).strip() != "success":
            raise RuntimeError("选中的 Action 构建还没有成功完成，暂时不能发布")
        if not entry.get("has_windows") or not entry.get("has_macos"):
            raise RuntimeError("选中的 Action 构建缺少 Windows 或 macOS 包，暂时不能发布")
        version = self.app_version_var.get().strip()
        if not version:
            raise RuntimeError("请先填写版本号")
        build_id = self.make_action_build_id(entry["run_id"])
        changelog = self.app_changelog_var.get().strip() or entry.get("title", "")
        return {
            "source_mode": "artifact",
            "version": version,
            "tag": build_id,
            "changelog": changelog,
            "recommended": bool(self.app_recommended_var.get()),
            "run_id": int(entry["run_id"]),
            "branch": entry.get("branch", ""),
            "head_sha": entry.get("head_sha", ""),
            "title": entry.get("title", ""),
        }

    def selected_publish_payload(self):
        if self.get_app_source_mode() == "artifact":
            return self.selected_action_build_payload()
        payload = self.selected_release_payload()
        payload["source_mode"] = "release"
        return payload

    def selected_existing_version_payload(self):
        if self.get_app_source_mode() != "artifact":
            return self.selected_release_payload()
        version = self.app_version_var.get().strip()
        if not version:
            raise RuntimeError("请先填写版本号")
        entry = next(
            (item for item in self.current_policy.get("versions", []) if item.get("version") == version),
            None,
        )
        if not entry:
            raise RuntimeError("目标版本不在白名单中，请先执行“发布新应用，但不推送”")
        self.app_tag_var.set(str(entry.get("tag", "")))
        if entry.get("changelog") and not self.app_changelog_var.get().strip():
            self.app_changelog_var.set(str(entry.get("changelog", "")))
        return {
            "version": version,
            "tag": str(entry.get("tag", "")).strip(),
            "changelog": self.app_changelog_var.get().strip() or str(entry.get("changelog", "")),
            "recommended": bool(self.app_recommended_var.get()),
            "source_mode": str(entry.get("app_source_mode", "artifact")).strip().lower() or "artifact",
        }

    def wait_for_publish_run(self, *, branch: str, build_id: str, started_at: float, existing_run_ids: set[int]) -> dict:
        deadline = time.time() + 30 * 60
        last_status = ""
        target_run_id = 0
        while time.time() < deadline:
            target = None
            if target_run_id > 0:
                target = self.github().get_workflow_run(target_run_id)
            else:
                runs = self.github().list_workflow_runs(
                    APP_ARTIFACT_PUBLISH_WORKFLOW_FILE,
                    branch=branch,
                    event="workflow_dispatch",
                    per_page=20,
                )
                for run in runs:
                    run_id = int(run.get("id", 0) or 0)
                    created_at = self.parse_time_seconds(run.get("created_at", ""))
                    if run_id > 0 and run_id not in existing_run_ids and created_at >= started_at - 5:
                        target_run_id = run_id
                        target = run
                        break
            if target:
                status = str(target.get("status", "")).strip()
                conclusion = str(target.get("conclusion", "")).strip()
                if status == "completed":
                    if conclusion != "success":
                        raise RuntimeError(
                            "\n".join(
                                [
                                    f"发布工作流执行失败：{conclusion or 'unknown'}",
                                    str(target.get("html_url", "")).strip(),
                                ]
                            )
                        )
                    return target
                last_status = status or last_status
            self.update_idletasks()
            self.update()
            time.sleep(5)
        raise RuntimeError(
            f"等待发布工作流超时，build_id={build_id}，最后状态：{last_status or 'unknown'}"
        )

    def extract_json_from_artifact(self, artifact_id: int, artifact_name: str) -> dict:
        payload = self.github().download_artifact_archive_bytes(artifact_id)
        buffer = io.BytesIO(payload)
        if zipfile.is_zipfile(buffer):
            buffer.seek(0)
            with zipfile.ZipFile(buffer, "r") as archive:
                for name in archive.namelist():
                    if name.endswith(artifact_name):
                        with archive.open(name) as fh:
                            return json.loads(fh.read().decode("utf-8"))
                if archive.namelist():
                    with archive.open(archive.namelist()[0]) as fh:
                        return json.loads(fh.read().decode("utf-8"))
        return json.loads(payload.decode("utf-8"))

    def read_workflow_run_summary(self, run_id: int) -> dict:
        artifacts = self.github().list_run_artifacts(run_id)
        target = next(
            (
                item
                for item in artifacts
                if str(item.get("name", "")).strip() == RELEASE_SUMMARY_ASSET_NAME
                and not bool(item.get("expired"))
            ),
            None,
        )
        if not target:
            raise RuntimeError("发布工作流未产出 release summary artifact")
        return self.extract_json_from_artifact(int(target.get("id", 0) or 0), RELEASE_SUMMARY_ASSET_NAME)

    def publish_selected_action_build(self, payload: dict) -> dict:
        branch = str(payload.get("branch", "")).strip() or self.get_current_branch().strip()
        if not branch:
            raise RuntimeError("无法确定发布工作流要使用的分支")
        started_at = time.time()
        existing_run_ids = {
            int(run.get("id", 0) or 0)
            for run in self.github().list_workflow_runs(
                APP_ARTIFACT_PUBLISH_WORKFLOW_FILE,
                branch=branch,
                event="workflow_dispatch",
                per_page=20,
            )
            if int(run.get("id", 0) or 0) > 0
        }
        self.github().dispatch_workflow(
            APP_ARTIFACT_PUBLISH_WORKFLOW_FILE,
            branch,
            inputs={
                "source_run_id": str(int(payload["run_id"])),
                "build_id": payload["tag"],
            },
        )
        publish_run = self.wait_for_publish_run(
            branch=branch,
            build_id=payload["tag"],
            started_at=started_at,
            existing_run_ids=existing_run_ids,
        )
        return self.read_workflow_run_summary(int(publish_run.get("id", 0) or 0))

    def is_vercel_env_configured(self, key: str) -> bool:
        record = self.current_env_records.get(key)
        if not record:
            return False
        if str(record.get("value", "")).strip():
            return True
        return str(record.get("type", "")).strip().lower() == "sensitive"

    def load_resource_source_state(self):
        saved_values = self.snapshot_resource_source_values_from_repo_vars()
        self.resource_saved_snapshot = dict(saved_values)
        self.apply_resource_source_values(
            saved_values,
            selection_label="当前表单：上次默认源",
        )
        if self.is_resource_source_incomplete(saved_values):
            try:
                self.load_latest_resource_sources(
                    silent=True,
                    selection_label="当前表单：最新推荐（因为上次默认源不完整，已自动填入）",
                )
            except Exception:
                pass
        self.refresh_visual_summaries()

    def update_resource_status_text(self):
        payload = {
            "current_form": self.collect_resource_source_values(),
            "saved_default": self.resource_saved_snapshot,
            "latest_candidate": self.resource_latest_snapshot,
            "NIYIEN_GLOBAL_SDK_BASE(只读参考)": self.current_envs.get("NIYIEN_GLOBAL_SDK_BASE", ""),
            "lens_repo": f"{self.config_data.get('lens_data_owner', DEFAULT_LENS_DATA_OWNER)}/{self.config_data.get('lens_data_repo', DEFAULT_LENS_DATA_REPO)}",
            "plugins_repo": f"{self.config_data.get('plugins_owner', DEFAULT_PLUGINS_OWNER)}/{self.config_data.get('plugins_repo', DEFAULT_PLUGINS_REPO)}",
        }
        if hasattr(self, "resource_status_text"):
            self.resource_status_text.delete("1.0", tk.END)
            self.resource_status_text.insert(
                "1.0", json.dumps(payload, indent=2, ensure_ascii=False)
            )
        self.refresh_visual_summaries()

    def load_latest_lens_tag(self):
        try:
            release, source_ref = self.get_latest_release_with_fallback(
                owner=self.config_data.get("lens_data_owner", DEFAULT_LENS_DATA_OWNER),
                repo=self.config_data.get("lens_data_repo", DEFAULT_LENS_DATA_REPO),
                fallback_owner=DEFAULT_LENS_DATA_OWNER,
                fallback_repo=DEFAULT_LENS_DATA_REPO,
                label="Lens/CameraDB",
            )
            self.resource_lens_tag_var.set(str(release.get("tag_name", "")).strip())
            self.resource_selection_label_var.set(f"当前表单：手动调整（已刷新 Lens 最新，来源 {source_ref}）")
            self.update_resource_status_text()
        except Exception as err:
            messagebox.showerror("读取失败", str(err))

    def load_latest_plugin_tag(self):
        try:
            plugin_values, plugin_meta = self.resolve_latest_plugin_source_values()
            self.resource_plugins_source_mode_var.set(plugin_values["NIYIEN_PLUGINS_SOURCE_MODE"])
            self.resource_plugins_tag_var.set(plugin_values["NIYIEN_PLUGINS_TAG"])
            self.resource_plugins_artifact_name_var.set(plugin_values["NIYIEN_PLUGINS_ARTIFACT_NAME"])
            refresh_target = "Plugin 最新 Artifact" if plugin_meta.get("mode") == "artifact" else "Plugin 最新 Release"
            self.resource_selection_label_var.set(
                f"当前表单：手动调整（已刷新 {refresh_target}，来源 {plugin_meta.get('source_label', '')}）"
            )
            self.on_plugin_source_mode_change()
            self.update_resource_status_text()
        except Exception as err:
            messagebox.showerror("读取失败", str(err))

    def load_latest_resource_sources(self, silent: bool = False, selection_label: str = "当前表单：最新推荐"):
        try:
            latest_values, meta = self.fetch_latest_resource_source_values_with_meta()
            self.resource_latest_snapshot = dict(latest_values)
            self.apply_resource_source_values(latest_values, selection_label=selection_label)
            if not silent:
                plugin_meta = meta.get("plugin") or {}
                if plugin_meta.get("mode") == "artifact":
                    plugin_line = (
                        f"Plugin Artifact：{latest_values['NIYIEN_PLUGINS_ARTIFACT_NAME'] or '自动最新'}"
                        f"（来源 {plugin_meta.get('source_label', '-')}）"
                    )
                    action_url = str(plugin_meta.get("url", "")).strip()
                else:
                    plugin_line = (
                        f"Plugin Release：{latest_values['NIYIEN_PLUGINS_TAG'] or '-'}"
                        f"（来源 {plugin_meta.get('source_label', '-')}）"
                    )
                    action_url = ""
                lines = [
                    f"Lens/CameraDB：{latest_values['NIYIEN_LENS_DATA_TAG'] or '-'}",
                    plugin_line,
                    f"SDK 下载源：{latest_values['NIYIEN_SDK_BASE'] or '-'}",
                ]
                if action_url:
                    lines.append(f"Action URL：{action_url}")
                messagebox.showinfo(
                    "已填入最新推荐",
                    "\n".join(lines),
                )
        except Exception as err:
            if not silent:
                messagebox.showerror("读取失败", str(err))

    def action_save_resource_sources(self):
        mapping = {
            "NIYIEN_LENS_DATA_TAG": self.resource_lens_tag_var.get().strip(),
            "NIYIEN_PLUGINS_SOURCE_MODE": self.get_plugin_source_mode(),
            "NIYIEN_PLUGINS_TAG": self.resource_plugins_tag_var.get().strip(),
            "NIYIEN_PLUGINS_ARTIFACT_NAME": self.resource_plugins_artifact_name_var.get().strip(),
            "NIYIEN_SDK_BASE": self.resource_sdk_base_var.get().strip() or DEFAULT_GLOBAL_SDK_BASE,
        }
        try:
            if not self.confirm_action(
                "确认保存下次发版默认源",
                [
                    f"Lens/CameraDB Tag：{mapping['NIYIEN_LENS_DATA_TAG'] or '-'}",
                    f"Plugin 来源模式：{mapping['NIYIEN_PLUGINS_SOURCE_MODE']}",
                    f"Plugin Release Tag：{mapping['NIYIEN_PLUGINS_TAG'] or '-'}",
                    f"Plugin Artifact 名称：{mapping['NIYIEN_PLUGINS_ARTIFACT_NAME'] or '自动最新'}",
                    "这个操作不会影响当前线上内容。",
                    "只会影响下一次应用发版时自动使用的资源源。",
                ],
            ):
                return
            optional_empty_allowed = {
                "NIYIEN_PLUGINS_TAG",
                "NIYIEN_PLUGINS_ARTIFACT_NAME",
            }
            github = self.github()
            for key, value in mapping.items():
                if key in optional_empty_allowed and not str(value).strip():
                    github.delete_actions_variable(key)
                    self.current_repo_variables.pop(key, None)
                    continue
                github.upsert_actions_variable(key, value)
                self.current_repo_variables[key] = value
            self.resource_saved_snapshot = dict(mapping)
            self.resource_selection_label_var.set("当前表单：上次默认源（刚保存）")
            self.update_resource_status_text()
            messagebox.showinfo("完成", "下次发版默认资源源已保存到 GitHub Actions Variables")
        except Exception as err:
            messagebox.showerror("保存失败", str(err))

    def selected_release_payload(self):
        index = self.release_list.curselection()
        if not index:
            raise RuntimeError("请先选择一个 GitHub Release")
        tag_display = self.release_list.get(index[0]).replace(" [pre]", "")
        tag = tag_display.strip()
        release = next(
            (item for item in self.current_releases if item.get("tag_name") == tag), None
        )
        if not release:
            raise RuntimeError("无法找到选中的 Release")
        version = self.app_version_var.get().strip() or normalize_version(tag)
        changelog = self.app_changelog_var.get().strip() or (release.get("body") or "")
        return {
            "version": version,
            "tag": tag,
            "changelog": changelog,
            "recommended": bool(self.app_recommended_var.get()),
        }

    def on_release_select(self, _event=None):
        try:
            payload = self.selected_release_payload()
        except Exception:
            return
        self.app_version_var.set(payload["version"])
        self.app_tag_var.set(payload["tag"])
        changelog = payload["changelog"].splitlines()[0] if payload["changelog"] else ""
        self.app_changelog_var.set(changelog)
        self.refresh_visual_summaries()

    def select_release_in_main_list(self, tag: str):
        if not tag:
            return False
        target = tag.strip()
        for index in range(self.release_list.size()):
            item = self.release_list.get(index).replace(" [pre]", "").strip()
            if item == target:
                self.release_list.selection_clear(0, tk.END)
                self.release_list.selection_set(index)
                self.release_list.activate(index)
                self.release_list.see(index)
                self.on_release_select()
                return True
        return False

    def on_dashboard_release_select(self, _event=None):
        if not self.dashboard_release_list.curselection():
            return
        tag = self.dashboard_release_list.get(self.dashboard_release_list.curselection()[0])
        tag = tag.replace(" [pre]", "").strip()
        if not tag:
            return
        self.show_page("app")
        self.app_source_mode_var.set("release")
        self.on_app_source_mode_change()
        if self.release_list.size() == 0:
            self.load_releases()
        self.select_release_in_main_list(tag)

    def preview_default_cn_manifest(self):
        if hasattr(self, "preview_country_var"):
            self.preview_country_var.set("CN")
        if hasattr(self, "preview_platform_var"):
            self.preview_platform_var.set("windows")
        self.show_page("route")
        self.preview_manifest()

    def build_target_tag(self) -> str:
        raw_tag = self.app_tag_var.get().strip()
        if raw_tag:
            return raw_tag
        version = self.app_version_var.get().strip()
        if not version:
            return ""
        return version if version.startswith("v") else f"v{version}"

    def begin_release_watch(self, tag: str):
        if not tag:
            return
        self.pending_release_watch = {
            "tag": tag,
            "attempt": 0,
            "max_attempts": 24,
            "interval_ms": 5000,
        }
        self.show_page("app")
        self.load_releases()
        self.after(5000, self.poll_pending_release_watch)

    def poll_pending_release_watch(self):
        if not self.pending_release_watch:
            return
        watch = self.pending_release_watch
        watch["attempt"] += 1
        self.load_releases()
        if not self.pending_release_watch:
            return
        if watch["attempt"] >= watch["max_attempts"]:
            tag = watch["tag"]
            self.pending_release_watch = None
            messagebox.showinfo(
                "Tag 已推送",
                "\n".join(
                    [
                        f"Tag {tag} 已推送成功。",
                        "GitHub Actions 应该已经开始运行。",
                        "如果 Release 还没出现，可以稍后手动点“刷新 Releases”。",
                    ]
                ),
            )
            return
        self.after(watch["interval_ms"], self.poll_pending_release_watch)

    def check_pending_release_watch(self):
        watch = self.pending_release_watch
        if not watch:
            return
        if self.select_release_in_main_list(watch["tag"]):
            self.pending_release_watch = None
            messagebox.showinfo(
                "Release 已出现",
                f"{watch['tag']} 已进入 Releases 列表，并已自动选中。",
            )

    def create_and_push_tag(self):
        tag = self.build_target_tag()
        if not tag:
            messagebox.showerror("缺少 Tag", "请先填写版本号或 Tag")
            return

        branch = self.get_current_branch() or "(unknown)"
        worktree_status = self.get_worktree_status_summary()
        if self.local_tag_exists(tag):
            messagebox.showerror("Tag 已存在", f"本地已经存在 Tag：{tag}")
            return
        if self.remote_tag_exists(tag):
            messagebox.showerror("Tag 已存在", f"远端 {self.git_remote_name()} 已存在 Tag：{tag}")
            return

        lines = [
            f"仓库路径：{self.repo_workdir()}",
            f"当前分支：{branch}",
            f"远端：{self.git_remote_name()}",
            f"准备创建并推送 Tag：{tag}",
        ]
        if worktree_status:
            lines.extend(
                [
                    "",
                    "当前工作区有未提交改动：",
                    worktree_status[:500],
                    "",
                    "Tag 会打在当前 HEAD 上，不会自动提交这些改动。",
                ]
            )
        else:
            lines.extend(["", "当前工作区干净，Tag 会打在当前 HEAD 上。"])

        if not self.confirm_action("确认创建并推送 Tag", lines, danger=True):
            return

        try:
            self.run_git("tag", tag)
            self.run_git("push", self.git_remote_name(), tag)
            self.app_tag_var.set(tag)
            self.begin_release_watch(tag)
        except subprocess.CalledProcessError as err:
            stderr = (err.stderr or "").strip()
            stdout = (err.stdout or "").strip()
            detail = stderr or stdout or str(err)
            messagebox.showerror("创建 Tag 失败", detail)

    def release_by_tag(self, tag: str):
        release = next((item for item in self.current_releases if item.get("tag_name") == tag), None)
        if release:
            return release
        release = self.github().get_release_by_tag(tag)
        self.current_releases = [
            item for item in self.current_releases if item.get("tag_name") != tag
        ] + [release]
        return release

    def get_release_asset_map(self, release: dict) -> dict:
        return {asset.get("name"): asset for asset in release.get("assets", []) if asset.get("name")}

    def read_json_asset(self, asset):
        if not asset:
            return {}
        return json.loads(self.github().download_text_asset(asset["browser_download_url"]))

    def load_release_summary(self, tag: str) -> dict:
        if not tag:
            return {}
        release = self.release_by_tag(tag)
        return self.read_json_asset(self.get_release_asset_map(release).get(RELEASE_SUMMARY_ASSET_NAME))

    def merge_release_summary_into_entry(self, entry: dict, summary: dict):
        if not summary:
            return entry
        if summary.get("app_source_mode"):
            entry["app_source_mode"] = str(summary["app_source_mode"]).strip().lower()
        if summary.get("app_source_ref"):
            entry["app_source_ref"] = str(summary["app_source_ref"]).strip()
        if isinstance(summary.get("global_app_urls"), dict):
            entry["app_urls"] = {
                str(key).strip().lower(): str(value).strip()
                for key, value in summary["global_app_urls"].items()
                if str(key).strip() and str(value).strip()
            }
        if summary.get("content_tag"):
            entry["content_tag"] = str(summary["content_tag"]).strip()
        if summary.get("lens_version") not in (None, ""):
            entry["lens_version"] = summary["lens_version"]
        if summary.get("lens_sha256"):
            entry["lens_sha256"] = str(summary["lens_sha256"]).strip()
        if summary.get("global_sdk_base"):
            entry["global_sdk_base"] = str(summary["global_sdk_base"]).strip()
        if summary.get("global_plugins_base"):
            entry["global_plugins_base"] = str(summary["global_plugins_base"]).strip()
        return entry

    def build_content_env_mapping_from_entry(self, entry: dict | None) -> dict:
        if not entry:
            return {}
        mapping = {}
        if entry.get("content_tag"):
            mapping["NIYIEN_CONTENT_RELEASE_TAG"] = str(entry["content_tag"]).strip()
        if entry.get("lens_version") not in (None, ""):
            mapping["NIYIEN_LENS_VERSION"] = str(entry["lens_version"]).strip()
        if entry.get("lens_sha256"):
            mapping["NIYIEN_LENS_SHA256"] = str(entry["lens_sha256"]).strip()
        return mapping

    def download_api_base(self) -> str:
        base = self.config_data.get("telemetry_base_url", "").rstrip("/")
        if not base:
            base = "https://www.niyien.com"
        return f"{base}/api/download"

    def build_cn_download_url(self, scope: str, tag: str, relative_path: str) -> str:
        encoded_tag = requests.utils.quote(tag.strip(), safe="")
        encoded_path = "/".join(
            requests.utils.quote(part, safe="")
            for part in relative_path.split("/")
            if part
        )
        return f"{self.download_api_base()}/{scope}/{encoded_tag}/{encoded_path}"

    def write_policy(self, policy, extra_envs=None):
        raw = json.dumps(policy, indent=2, ensure_ascii=False)
        mapping = {"NIYIEN_RELEASE_POLICY_JSON": raw}
        if extra_envs:
            mapping.update(extra_envs)
        self.vercel().upsert_envs(mapping)
        self.current_policy = policy
        self.policy_text.delete("1.0", tk.END)
        self.policy_text.insert("1.0", raw)
        if extra_envs:
            self.current_envs.update(extra_envs)
            self.content_tag_var.set(self.current_envs.get("NIYIEN_CONTENT_RELEASE_TAG", ""))
            self.lens_version_var.set(str(self.current_envs.get("NIYIEN_LENS_VERSION", "")))
            self.lens_sha_var.set(self.current_envs.get("NIYIEN_LENS_SHA256", ""))
            self.update_data_status_text()
        if self.config_data.get("deploy_hook_url"):
            self.trigger_deploy_hook(silent=True)

    def upsert_policy_entry(self, version, tag, changelog, recommended, channels, release_summary=None):
        policy = json.loads(json.dumps(self.current_policy))
        versions = [
            item for item in policy.get("versions", []) if item.get("version") != version
        ]
        entry = {
            "version": version,
            "tag": tag,
            "channels": channels,
            "changelog": changelog,
            "recommended": recommended,
        }
        self.merge_release_summary_into_entry(entry, release_summary or {})
        versions.append(entry)
        versions.sort(key=lambda item: item.get("version", ""), reverse=True)
        policy["versions"] = versions
        return policy

    def action_add_manual_only(self):
        try:
            payload = self.selected_publish_payload()
            if not self.confirm_action(
                "确认发布但不推送",
                [
                    f"版本：{payload['version']}",
                    "来源：Action 构建" if payload.get("source_mode") == "artifact" else "来源：GitHub Release",
                    "这个操作会让版本进入手动可见列表。",
                    "不会影响当前自动推送版本。",
                ],
            ):
                return
            release_summary = (
                self.publish_selected_action_build(payload)
                if payload.get("source_mode") == "artifact"
                else self.load_release_summary(payload["tag"])
            )
            policy = self.upsert_policy_entry(
                payload["version"],
                payload["tag"],
                payload["changelog"],
                payload["recommended"],
                ["manual"],
                release_summary=release_summary,
            )
            self.write_policy(policy)
            messagebox.showinfo("完成", f"{payload['version']} 已加入手动版本白名单")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_publish_and_promote(self):
        try:
            payload = self.selected_publish_payload()
            if not self.confirm_action(
                "确认发布并立即推送",
                [
                    f"版本：{payload['version']}",
                    "来源：Action 构建" if payload.get("source_mode") == "artifact" else "来源：GitHub Release",
                    "这个操作会立即把该版本设为当前自动推送版本。",
                    "之后已安装用户会收到更新提示。",
                ],
                danger=True,
            ):
                return
            release_summary = (
                self.publish_selected_action_build(payload)
                if payload.get("source_mode") == "artifact"
                else self.load_release_summary(payload["tag"])
            )
            policy = self.upsert_policy_entry(
                payload["version"],
                payload["tag"],
                payload["changelog"],
                payload["recommended"],
                ["auto", "manual"],
                release_summary=release_summary,
            )
            policy["auto_version"] = payload["version"]
            for item in policy["versions"]:
                if item["version"] != payload["version"] and "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item["channels"] if c != "auto"] or [
                        "manual"
                    ]
            target_entry = next(
                (item for item in policy["versions"] if item.get("version") == payload["version"]),
                None,
            )
            self.write_policy(policy, self.build_content_env_mapping_from_entry(target_entry))
            messagebox.showinfo("完成", f"{payload['version']} 已设为自动推送版本")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_promote_existing(self):
        try:
            payload = self.selected_existing_version_payload()
            if not self.confirm_action(
                "确认开始推送已发布版本",
                [
                    f"版本：{payload['version']}",
                    "这个操作不会重新打包或重新上传。",
                    "只会把该版本切换成当前自动推送版本。",
                ],
                danger=True,
            ):
                return
            version = payload["version"]
            policy = json.loads(json.dumps(self.current_policy))
            found = False
            release_summary = (
                self.load_release_summary(payload["tag"])
                if payload.get("source_mode") != "artifact"
                else {}
            )
            for item in policy.get("versions", []):
                if item.get("version") == version:
                    found = True
                    item["channels"] = sorted(
                        set(item.get("channels", []) + ["auto", "manual"])
                    )
                    item["recommended"] = bool(self.app_recommended_var.get())
                    self.merge_release_summary_into_entry(item, release_summary)
                elif "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item["channels"] if c != "auto"] or [
                        "manual"
                    ]
            if not found:
                raise RuntimeError("目标版本不在白名单中，请先执行“发布新应用，但不推送”")
            policy["auto_version"] = version
            target_entry = next(
                (item for item in policy["versions"] if item.get("version") == version),
                None,
            )
            self.write_policy(policy, self.build_content_env_mapping_from_entry(target_entry))
            messagebox.showinfo("完成", f"{version} 已开始自动推送")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_rollback_auto_version(self):
        try:
            payload = self.selected_existing_version_payload()
            if not self.confirm_action(
                "确认回滚自动推送版本",
                [
                    f"回滚到版本：{payload['version']}",
                    "这个操作会立即改变后续用户看到的推荐更新版本。",
                    "不会强制降级已经安装新版本的用户。",
                ],
                danger=True,
            ):
                return
            version = payload["version"]
            policy = json.loads(json.dumps(self.current_policy))
            release_summary = (
                self.load_release_summary(payload["tag"])
                if payload.get("source_mode") != "artifact"
                else {}
            )
            for item in policy.get("versions", []):
                if item.get("version") == version:
                    item["channels"] = sorted(
                        set(item.get("channels", []) + ["auto", "manual"])
                    )
                    item["recommended"] = True
                    self.merge_release_summary_into_entry(item, release_summary)
                elif "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item["channels"] if c != "auto"] or [
                        "manual"
                    ]
            policy["auto_version"] = version
            target_entry = next(
                (item for item in policy["versions"] if item.get("version") == version),
                None,
            )
            self.write_policy(policy, self.build_content_env_mapping_from_entry(target_entry))
            messagebox.showinfo("完成", f"自动推送版本已回滚到 {version}")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_hide_selected_version(self):
        try:
            payload = self.selected_existing_version_payload()
            lines = [
                f"版本：{payload['version']}",
                "这个操作会把该版本从手动版本列表中移除。",
            ]
            if self.current_policy.get("auto_version") == payload["version"]:
                lines.append("它当前还是自动推送版本，隐藏后系统会自动切到其他可用版本。")
            if not self.confirm_action("确认隐藏版本", lines, danger=True):
                return
            version = payload["version"]
            policy = json.loads(json.dumps(self.current_policy))
            policy["versions"] = [
                item
                for item in policy.get("versions", [])
                if item.get("version") != version
            ]
            extra_envs = None
            if policy.get("auto_version") == version:
                policy["auto_version"] = (
                    policy["versions"][0]["version"] if policy["versions"] else ""
                )
                extra_envs = self.build_content_env_mapping_from_entry(
                    policy["versions"][0] if policy["versions"] else None
                )
            self.write_policy(policy, extra_envs)
            messagebox.showinfo("完成", f"{version} 已从白名单中移除")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def load_data_release_metadata(self):
        tag = self.app_tag_var.get().strip() or self.content_tag_var.get().strip()
        if not tag:
            messagebox.showerror("缺少 tag", "请先填写内容 Release Tag")
            return
        try:
            release = self.release_by_tag(tag)
            asset_map = self.get_release_asset_map(release)
            summary = self.read_json_asset(asset_map.get(RELEASE_SUMMARY_ASSET_NAME))
            if summary:
                self.content_tag_var.set(str(summary.get("content_tag", "")))
                self.lens_version_var.set(str(summary.get("lens_version", "")))
                self.lens_sha_var.set(str(summary.get("lens_sha256", "")))
            else:
                lens_meta = self.read_json_asset(asset_map.get("gyroflow-niyien-lens.cbor.gz.json"))
                self.lens_version_var.set(str(lens_meta.get("version", "")))
                self.lens_sha_var.set(lens_meta.get("sha256", ""))
            self.update_data_status_text()
        except Exception as err:
            messagebox.showerror("读取失败", str(err))

    def action_update_data_envs(self):
        mapping = {
            "NIYIEN_CONTENT_RELEASE_TAG": self.content_tag_var.get().strip(),
            "NIYIEN_LENS_VERSION": self.lens_version_var.get().strip(),
            "NIYIEN_LENS_SHA256": self.lens_sha_var.get().strip(),
        }
        try:
            if not self.confirm_action(
                "确认切换当前内容版本",
                [
                    f"内容版本：{mapping['NIYIEN_CONTENT_RELEASE_TAG'] or '-'}",
                    f"Lens 版本：{mapping['NIYIEN_LENS_VERSION'] or '-'}",
                    "这个操作会立即影响中国区用户当前下载到的内容。",
                ],
                danger=True,
            ):
                return
            self.vercel().upsert_envs(mapping)
            self.current_envs.update(mapping)
            self.update_data_status_text()
            if self.config_data.get("deploy_hook_url"):
                self.trigger_deploy_hook(silent=True)
            messagebox.showinfo("完成", "数据资源环境变量已更新")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def update_data_status_text(self):
        payload = {
            "NIYIEN_CONTENT_RELEASE_TAG": self.content_tag_var.get().strip(),
            "NIYIEN_LENS_VERSION": self.lens_version_var.get().strip(),
            "NIYIEN_LENS_SHA256": self.lens_sha_var.get().strip(),
        }
        self.data_status_text.delete("1.0", tk.END)
        self.data_status_text.insert(
            "1.0", json.dumps(payload, indent=2, ensure_ascii=False)
        )
        self.refresh_visual_summaries()

    def preview_manifest(self):
        country = self.preview_country_var.get().strip().upper() or "US"
        platform = self.preview_platform_var.get().strip() or "windows"
        source = select_source(self.distribution_config, country)
        is_cn = country in set(self.distribution_config.get("routing", {}).get("cn_countries", []))
        policy = self.current_policy
        auto_version = policy.get("auto_version", "")
        versions = policy.get("versions", [])
        auto_entry = next(
            (item for item in versions if item.get("version") == auto_version),
            versions[0] if versions else None,
        )
        content_tag = (
            (auto_entry or {}).get("content_tag", "")
            or self.content_tag_var.get().strip()
            or self.current_envs.get("NIYIEN_CONTENT_RELEASE_TAG", "")
        )
        auto_source_mode = str((auto_entry or {}).get("app_source_mode", "release")).strip().lower()
        sdk_base = self.get_preview_global_sdk_base(auto_entry)
        plugins_base = self.get_preview_global_plugins_base(auto_entry)
        preview = {
            "country": country,
            "region": "cn" if is_cn else "global",
            "app": {
                "version": auto_entry["version"] if auto_entry else "",
                "url": (
                    self.build_cn_download_url("app", auto_entry["tag"], asset_name_for_platform(platform))
                    if is_cn and auto_entry
                    else str((auto_entry or {}).get("app_urls", {}).get(platform, "")).strip()
                    if auto_entry and auto_source_mode == "artifact"
                    else f"{source['base']}/{auto_entry['tag']}/{asset_name_for_platform(platform)}"
                    if auto_entry
                    else ""
                ),
                "changelog": auto_entry.get("changelog", "") if auto_entry else "",
                "manual_versions": [
                    {
                        "version": item.get("version", ""),
                        "url": (
                            self.build_cn_download_url("app", item.get("tag", ""), asset_name_for_platform(platform))
                            if is_cn
                            else str(item.get("app_urls", {}).get(platform, "")).strip()
                            if str(item.get("app_source_mode", "release")).strip().lower() == "artifact"
                            else f"{source['base']}/{item.get('tag', '')}/{asset_name_for_platform(platform)}"
                        ),
                        "changelog": item.get("changelog", ""),
                        "recommended": bool(item.get("recommended", False)),
                    }
                    for item in versions
                    if "manual" in item.get("channels", [])
                ],
            },
            "lens": {
                "version": int(self.lens_version_var.get() or "0"),
                "url": (
                    self.build_cn_download_url("content", content_tag, self.distribution_config["data"]["lens"]["asset_name"])
                    if is_cn and content_tag
                    else self.build_cn_download_url("content", content_tag, self.distribution_config["data"]["lens"]["asset_name"])
                    if auto_entry and auto_source_mode == "artifact" and content_tag
                    else f"{source['base']}/{auto_entry['tag']}/{self.distribution_config['data']['lens']['asset_name']}"
                    if auto_entry
                    else ""
                ),
                "sha256": self.lens_sha_var.get().strip(),
            },
            "sdk_base": (
                self.build_cn_download_url("content", content_tag, "sdk") + "/"
                if is_cn and content_tag
                else self.build_cn_download_url("content", content_tag, "sdk") + "/"
                if auto_entry and auto_source_mode == "artifact" and content_tag
                else sdk_base
            ),
            "plugins_base": (
                self.build_cn_download_url("content", content_tag, "plugins") + "/"
                if is_cn and content_tag
                else self.build_cn_download_url("content", content_tag, "plugins") + "/"
                if auto_entry and auto_source_mode == "artifact" and content_tag
                else plugins_base
            ),
        }
        self.route_preview_text.delete("1.0", tk.END)
        self.route_preview_text.insert(
            "1.0", json.dumps(preview, indent=2, ensure_ascii=False)
        )
        self.refresh_visual_summaries()

    def fetch_stats(self):
        base = self.config_data.get("telemetry_base_url", "").rstrip("/")
        if not base:
            messagebox.showerror("配置缺失", "telemetry_base_url 未配置")
            return
        params = {"days": self.stats_days_var.get().strip() or "7"}
        if self.stats_product_var.get().strip():
            params["product_id"] = self.stats_product_var.get().strip()
        if self.stats_source_var.get().strip():
            params["source_app_id"] = self.stats_source_var.get().strip()
        if self.stats_event_var.get().strip():
            params["event"] = self.stats_event_var.get().strip()
        headers = {}
        token = self.config_data.get("telemetry_stats_token", "").strip()
        if token:
            headers["X-Stats-Token"] = token
        try:
            response = self.http_get(
                f"{base}/api/telemetry-stats",
                params=params,
                headers=headers,
                timeout=30,
            )
            response.raise_for_status()
            payload = response.json()
            self.stats_text.delete("1.0", tk.END)
            self.stats_text.insert("1.0", json.dumps(payload, indent=2, ensure_ascii=False))
        except Exception as err:
            messagebox.showerror("统计获取失败", str(err))

    def open_stats_page(self):
        base = self.config_data.get("telemetry_base_url", "").rstrip("/")
        if base:
            webbrowser.open(f"{base}/stats.html")

    def trigger_rebuild(self):
        base = self.config_data.get("telemetry_base_url", "").rstrip("/")
        token = self.config_data.get("telemetry_rebuild_token", "").strip()
        if not base or not token:
            messagebox.showerror(
                "配置缺失", "需要 telemetry_base_url 和 telemetry_rebuild_token"
            )
            return
        payload = {
            "start_day": self.rebuild_start_var.get().strip(),
            "end_day": self.rebuild_end_var.get().strip(),
            "dry_run": False,
            "apply": True,
            "reset_day_keys": False,
        }
        try:
            response = self.http_post(
                f"{base}/api/telemetry-rebuild",
                headers={"X-Rebuild-Token": token, "Content-Type": "application/json"},
                json=payload,
                timeout=60,
            )
            response.raise_for_status()
            messagebox.showinfo(
                "Rebuild 结果", json.dumps(response.json(), indent=2, ensure_ascii=False)
            )
        except Exception as err:
            messagebox.showerror("Rebuild 失败", str(err))

    def trigger_deploy_hook(self, silent=False):
        url = self.config_data.get("deploy_hook_url", "").strip()
        if not url:
            if not silent:
                messagebox.showerror("缺少 deploy hook", "deploy_hook_url 未配置")
            return
        try:
            response = self.http_post(url, timeout=30)
            response.raise_for_status()
            if not silent:
                messagebox.showinfo("完成", "已触发 Vercel redeploy")
        except Exception as err:
            if not silent:
                messagebox.showerror("触发失败", str(err))

    def save_config(self):
        for key, var in self.config_vars.items():
            value = var.get()
            if key == "network_proxy":
                value = normalize_proxy_url(value)
                var.set(value)
            self.config_data[key] = value
        save_json_file(CONFIG_FILE, self.config_data)
        self.update_resource_status_text()
        self.refresh_visual_summaries()
        messagebox.showinfo("完成", f"配置已保存到 {CONFIG_FILE}")


if __name__ == "__main__":
    if not CONFIG_FILE.exists() and EXAMPLE_CONFIG_FILE.exists():
        save_json_file(CONFIG_FILE, load_json_file(EXAMPLE_CONFIG_FILE, DEFAULT_CONFIG))
    app = ControlCenter()
    app.mainloop()
