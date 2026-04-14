#!/usr/bin/env python3
import json
import tkinter as tk
import webbrowser
from pathlib import Path
from tkinter import messagebox, scrolledtext, ttk

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover
    import tomli as tomllib

import requests


ROOT = Path(__file__).resolve().parent.parent
CONFIG_FILE = Path(__file__).with_suffix(".config.json")
EXAMPLE_CONFIG_FILE = Path(__file__).with_name("control_center.example.json")

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


def select_source(config: dict, country: str) -> dict:
    cn = set(config.get("routing", {}).get("cn_countries", []))
    if country.upper() in cn:
        return config["sources"]["cn"]
    return config["sources"]["global"]


class VercelClient:
    def __init__(self, token: str, project: str, team_id: str = ""):
        self.token = token.strip()
        self.project = project.strip()
        self.team_id = team_id.strip()

    def _params(self):
        params = {}
        if self.team_id:
            params["teamId"] = self.team_id
        return params

    def _headers(self):
        return {"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"}

    def list_envs(self) -> dict:
        self._ensure_ready()
        url = f"https://api.vercel.com/v10/projects/{self.project}/env"
        response = requests.get(
            url,
            headers=self._headers(),
            params={**self._params(), "decrypt": "true"},
            timeout=30,
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
                result[key] = value
            elif key and key not in result:
                result[key] = value
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
            timeout=30,
        )
        response.raise_for_status()
        return response.json()

    def _ensure_ready(self):
        if not self.token or not self.project:
            raise RuntimeError("Missing Vercel token or project id/name")


class GitHubClient:
    def __init__(self, owner: str, repo: str, token: str = ""):
        self.owner = owner.strip()
        self.repo = repo.strip()
        self.token = token.strip()

    def _headers(self):
        headers = {"Accept": "application/vnd.github+json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        return headers

    def list_releases(self):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{self.owner}/{self.repo}/releases"
        response = requests.get(url, headers=self._headers(), timeout=30)
        response.raise_for_status()
        return response.json()

    def get_release_by_tag(self, tag: str):
        self._ensure_ready()
        url = f"https://api.github.com/repos/{self.owner}/{self.repo}/releases/tags/{tag}"
        response = requests.get(url, headers=self._headers(), timeout=30)
        response.raise_for_status()
        return response.json()

    def download_text_asset(self, url: str) -> str:
        response = requests.get(url, headers=self._headers(), timeout=30)
        response.raise_for_status()
        return response.text

    def _ensure_ready(self):
        if not self.owner or not self.repo:
            raise RuntimeError("Missing GitHub owner/repo")


class ControlCenter(tk.Tk):
    def __init__(self):
        super().__init__()
        self.title("NiYien Control Center")
        self.geometry("1280x860")
        self.config_data = load_json_file(CONFIG_FILE, DEFAULT_CONFIG)
        self.distribution_config = self.load_distribution_config()
        self.current_envs = {}
        self.current_policy = self.default_policy()
        self.current_releases = []
        self._build_ui()
        self.refresh_runtime_state()

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
        )

    def github(self):
        return GitHubClient(
            self.config_data.get("github_owner", ""),
            self.config_data.get("github_repo", ""),
            self.config_data.get("github_token", ""),
        )

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
        notebook = ttk.Notebook(self)
        notebook.pack(fill="both", expand=True)

        self.app_tab = ttk.Frame(notebook)
        self.data_tab = ttk.Frame(notebook)
        self.route_tab = ttk.Frame(notebook)
        self.stats_tab = ttk.Frame(notebook)
        self.advanced_tab = ttk.Frame(notebook)

        notebook.add(self.app_tab, text="应用发布")
        notebook.add(self.data_tab, text="数据资源发布")
        notebook.add(self.route_tab, text="下载与路由")
        notebook.add(self.stats_tab, text="统计与观测")
        notebook.add(self.advanced_tab, text="高级设置")

        self.build_app_tab()
        self.build_data_tab()
        self.build_route_tab()
        self.build_stats_tab()
        self.build_advanced_tab()

    def add_labeled_entry(self, parent, label, variable, row, width=40, show=None):
        ttk.Label(parent, text=label).grid(row=row, column=0, sticky="w", pady=4, padx=(0, 8))
        ttk.Entry(parent, textvariable=variable, width=width, show=show).grid(
            row=row, column=1, sticky="we", pady=4
        )

    def build_app_tab(self):
        left = ttk.Frame(self.app_tab)
        right = ttk.Frame(self.app_tab)
        left.pack(side="left", fill="y", padx=10, pady=10)
        right.pack(side="left", fill="both", expand=True, padx=10, pady=10)

        ttk.Label(left, text="GitHub Releases").pack(anchor="w")
        self.release_list = tk.Listbox(left, width=42, height=24)
        self.release_list.pack(fill="y", expand=True)
        self.release_list.bind("<<ListboxSelect>>", self.on_release_select)

        ttk.Button(left, text="刷新 Releases", command=self.load_releases).pack(
            fill="x", pady=(8, 2)
        )
        ttk.Button(left, text="刷新当前推送状态", command=self.refresh_runtime_state).pack(
            fill="x"
        )

        form = ttk.Frame(right)
        form.pack(fill="x")
        self.app_version_var = tk.StringVar()
        self.app_tag_var = tk.StringVar()
        self.app_changelog_var = tk.StringVar()
        self.app_recommended_var = tk.BooleanVar(value=True)

        self.add_labeled_entry(form, "版本号", self.app_version_var, 0)
        self.add_labeled_entry(form, "Tag", self.app_tag_var, 1)
        self.add_labeled_entry(form, "更新说明", self.app_changelog_var, 2, width=80)
        ttk.Checkbutton(form, text="推荐版本", variable=self.app_recommended_var).grid(
            row=3, column=1, sticky="w", pady=6
        )

        buttons = ttk.Frame(right)
        buttons.pack(fill="x", pady=8)
        ttk.Button(
            buttons, text="发布新应用，但不推送", command=self.action_add_manual_only
        ).pack(side="left", padx=4)
        ttk.Button(
            buttons, text="发布并立即推送", command=self.action_publish_and_promote
        ).pack(side="left", padx=4)
        ttk.Button(
            buttons, text="开始推送已发布版本", command=self.action_promote_existing
        ).pack(side="left", padx=4)
        ttk.Button(
            buttons, text="回滚自动推送版本", command=self.action_rollback_auto_version
        ).pack(side="left", padx=4)
        ttk.Button(
            buttons, text="隐藏某个版本", command=self.action_hide_selected_version
        ).pack(side="left", padx=4)

        ttk.Label(right, text="当前 release policy").pack(anchor="w")
        self.policy_text = scrolledtext.ScrolledText(right, wrap="word", height=26)
        self.policy_text.pack(fill="both", expand=True)

    def build_data_tab(self):
        frame = ttk.Frame(self.data_tab)
        frame.pack(fill="both", expand=True, padx=10, pady=10)

        self.content_tag_var = tk.StringVar()
        self.lens_version_var = tk.StringVar()
        self.lens_sha_var = tk.StringVar()

        self.add_labeled_entry(frame, "内容 Release Tag", self.content_tag_var, 0, width=60)
        self.add_labeled_entry(frame, "lens 版本", self.lens_version_var, 1)
        self.add_labeled_entry(frame, "lens sha256", self.lens_sha_var, 2, width=80)

        buttons = ttk.Frame(frame)
        buttons.grid(row=3, column=0, columnspan=2, sticky="w", pady=8)
        ttk.Button(
            buttons, text="从 GitHub Release 读取数据元信息", command=self.load_data_release_metadata
        ).pack(side="left", padx=4)
        ttk.Button(
            buttons, text="发布新数据资源", command=self.action_update_data_envs
        ).pack(side="left", padx=4)
        ttk.Button(
            buttons, text="切换当前数据资源版本", command=self.action_update_data_envs
        ).pack(side="left", padx=4)

        ttk.Label(frame, text="数据资源状态").grid(row=4, column=0, sticky="w", pady=(10, 4))
        self.data_status_text = scrolledtext.ScrolledText(frame, wrap="word", height=24)
        self.data_status_text.grid(row=5, column=0, columnspan=2, sticky="nsew")
        frame.columnconfigure(1, weight=1)
        frame.rowconfigure(5, weight=1)

    def build_route_tab(self):
        frame = ttk.Frame(self.route_tab)
        frame.pack(fill="both", expand=True, padx=10, pady=10)
        self.preview_country_var = tk.StringVar(value="CN")
        self.preview_platform_var = tk.StringVar(value="windows")

        self.add_labeled_entry(frame, "国家代码", self.preview_country_var, 0)
        ttk.Label(frame, text="平台").grid(row=1, column=0, sticky="w", pady=4)
        ttk.Combobox(
            frame,
            textvariable=self.preview_platform_var,
            values=["windows", "macos", "linux", "android"],
            state="readonly",
            width=20,
        ).grid(row=1, column=1, sticky="w", pady=4)
        ttk.Button(frame, text="预览 manifest 返回结果", command=self.preview_manifest).grid(
            row=2, column=1, sticky="w", pady=8
        )

        self.route_preview_text = scrolledtext.ScrolledText(frame, wrap="word", height=32)
        self.route_preview_text.grid(row=3, column=0, columnspan=2, sticky="nsew")
        frame.columnconfigure(1, weight=1)
        frame.rowconfigure(3, weight=1)

    def build_stats_tab(self):
        frame = ttk.Frame(self.stats_tab)
        frame.pack(fill="both", expand=True, padx=10, pady=10)

        self.stats_days_var = tk.StringVar(value="7")
        self.stats_product_var = tk.StringVar(value="gyroflow_niyien")
        self.stats_source_var = tk.StringVar(value="")
        self.stats_event_var = tk.StringVar(value="")
        self.rebuild_start_var = tk.StringVar(value="")
        self.rebuild_end_var = tk.StringVar(value="")

        self.add_labeled_entry(frame, "统计天数", self.stats_days_var, 0)
        self.add_labeled_entry(frame, "product_id", self.stats_product_var, 1)
        self.add_labeled_entry(frame, "source_app_id", self.stats_source_var, 2)
        self.add_labeled_entry(frame, "event", self.stats_event_var, 3)

        button_row = ttk.Frame(frame)
        button_row.grid(row=4, column=0, columnspan=2, sticky="w", pady=8)
        ttk.Button(button_row, text="获取统计 JSON", command=self.fetch_stats).pack(
            side="left", padx=4
        )
        ttk.Button(button_row, text="打开 stats.html", command=self.open_stats_page).pack(
            side="left", padx=4
        )

        rebuild_row = ttk.Frame(frame)
        rebuild_row.grid(row=5, column=0, columnspan=2, sticky="w", pady=8)
        ttk.Label(rebuild_row, text="Rebuild 开始").pack(side="left", padx=(0, 4))
        ttk.Entry(rebuild_row, textvariable=self.rebuild_start_var, width=14).pack(side="left")
        ttk.Label(rebuild_row, text="结束").pack(side="left", padx=(12, 4))
        ttk.Entry(rebuild_row, textvariable=self.rebuild_end_var, width=14).pack(side="left")
        ttk.Button(
            rebuild_row, text="触发 telemetry rebuild", command=self.trigger_rebuild
        ).pack(side="left", padx=12)

        self.stats_text = scrolledtext.ScrolledText(frame, wrap="word", height=28)
        self.stats_text.grid(row=6, column=0, columnspan=2, sticky="nsew")
        frame.columnconfigure(1, weight=1)
        frame.rowconfigure(6, weight=1)

    def build_advanced_tab(self):
        frame = ttk.Frame(self.advanced_tab)
        frame.pack(fill="both", expand=True, padx=10, pady=10)

        self.config_vars = {}
        keys = [
            "vercel_token",
            "vercel_project_id_or_name",
            "vercel_team_id",
            "github_token",
            "github_owner",
            "github_repo",
            "telemetry_base_url",
            "telemetry_stats_token",
            "telemetry_rebuild_token",
            "deploy_hook_url",
        ]
        for row, key in enumerate(keys):
            var = tk.StringVar(value=self.config_data.get(key, ""))
            self.config_vars[key] = var
            self.add_labeled_entry(
                frame,
                key,
                var,
                row,
                width=80,
                show="*" if "token" in key and key != "telemetry_base_url" else None,
            )

        buttons = ttk.Frame(frame)
        buttons.grid(row=len(keys), column=0, columnspan=2, sticky="w", pady=8)
        ttk.Button(buttons, text="保存本地配置", command=self.save_config).pack(
            side="left", padx=4
        )
        ttk.Button(
            buttons, text="刷新 Vercel 环境变量快照", command=self.refresh_runtime_state
        ).pack(side="left", padx=4)
        ttk.Button(buttons, text="触发 deploy hook", command=self.trigger_deploy_hook).pack(
            side="left", padx=4
        )

        ttk.Label(frame, text="当前环境变量快照").grid(
            row=len(keys) + 1, column=0, sticky="w", pady=(10, 4)
        )
        self.env_snapshot_text = scrolledtext.ScrolledText(frame, wrap="word", height=18)
        self.env_snapshot_text.grid(
            row=len(keys) + 2, column=0, columnspan=2, sticky="nsew"
        )

        frame.columnconfigure(1, weight=1)
        frame.rowconfigure(len(keys) + 2, weight=1)

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
        except Exception as err:  # pragma: no cover - UI path
            messagebox.showerror("GitHub error", str(err))

    def refresh_runtime_state(self):
        try:
            self.current_envs = self.vercel().list_envs()
        except Exception as err:
            self.current_envs = {}
            self.env_snapshot_text.delete("1.0", tk.END)
            self.env_snapshot_text.insert("1.0", f"Failed to load Vercel envs:\n{err}\n")
            self.policy_text.delete("1.0", tk.END)
            self.policy_text.insert("1.0", "{}\n")
            return

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

    def write_policy(self, policy):
        raw = json.dumps(policy, indent=2, ensure_ascii=False)
        self.vercel().upsert_envs({"NIYIEN_RELEASE_POLICY_JSON": raw})
        self.current_policy = policy
        self.policy_text.delete("1.0", tk.END)
        self.policy_text.insert("1.0", raw)
        if self.config_data.get("deploy_hook_url"):
            self.trigger_deploy_hook(silent=True)

    def upsert_policy_entry(self, version, tag, changelog, recommended, channels):
        policy = json.loads(json.dumps(self.current_policy))
        versions = [
            item for item in policy.get("versions", []) if item.get("version") != version
        ]
        versions.append(
            {
                "version": version,
                "tag": tag,
                "channels": channels,
                "changelog": changelog,
                "recommended": recommended,
            }
        )
        versions.sort(key=lambda item: item.get("version", ""), reverse=True)
        policy["versions"] = versions
        return policy

    def action_add_manual_only(self):
        try:
            payload = self.selected_release_payload()
            policy = self.upsert_policy_entry(
                payload["version"],
                payload["tag"],
                payload["changelog"],
                payload["recommended"],
                ["manual"],
            )
            self.write_policy(policy)
            messagebox.showinfo("完成", f"{payload['version']} 已加入手动版本白名单")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_publish_and_promote(self):
        try:
            payload = self.selected_release_payload()
            policy = self.upsert_policy_entry(
                payload["version"],
                payload["tag"],
                payload["changelog"],
                payload["recommended"],
                ["auto", "manual"],
            )
            policy["auto_version"] = payload["version"]
            for item in policy["versions"]:
                if item["version"] != payload["version"] and "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item["channels"] if c != "auto"] or [
                        "manual"
                    ]
            self.write_policy(policy)
            messagebox.showinfo("完成", f"{payload['version']} 已设为自动推送版本")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_promote_existing(self):
        try:
            payload = self.selected_release_payload()
            version = payload["version"]
            policy = json.loads(json.dumps(self.current_policy))
            found = False
            for item in policy.get("versions", []):
                if item.get("version") == version:
                    found = True
                    item["channels"] = sorted(
                        set(item.get("channels", []) + ["auto", "manual"])
                    )
                    item["recommended"] = bool(self.app_recommended_var.get())
                elif "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item["channels"] if c != "auto"] or [
                        "manual"
                    ]
            if not found:
                raise RuntimeError("目标版本不在白名单中，请先执行“发布新应用，但不推送”")
            policy["auto_version"] = version
            self.write_policy(policy)
            messagebox.showinfo("完成", f"{version} 已开始自动推送")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_rollback_auto_version(self):
        try:
            payload = self.selected_release_payload()
            version = payload["version"]
            policy = json.loads(json.dumps(self.current_policy))
            for item in policy.get("versions", []):
                if item.get("version") == version:
                    item["channels"] = sorted(
                        set(item.get("channels", []) + ["auto", "manual"])
                    )
                    item["recommended"] = True
                elif "auto" in item.get("channels", []):
                    item["channels"] = [c for c in item["channels"] if c != "auto"] or [
                        "manual"
                    ]
            policy["auto_version"] = version
            self.write_policy(policy)
            messagebox.showinfo("完成", f"自动推送版本已回滚到 {version}")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def action_hide_selected_version(self):
        try:
            payload = self.selected_release_payload()
            version = payload["version"]
            policy = json.loads(json.dumps(self.current_policy))
            policy["versions"] = [
                item
                for item in policy.get("versions", [])
                if item.get("version") != version
            ]
            if policy.get("auto_version") == version:
                policy["auto_version"] = (
                    policy["versions"][0]["version"] if policy["versions"] else ""
                )
            self.write_policy(policy)
            messagebox.showinfo("完成", f"{version} 已从白名单中移除")
        except Exception as err:
            messagebox.showerror("失败", str(err))

    def load_data_release_metadata(self):
        tag = self.content_tag_var.get().strip()
        if not tag:
            messagebox.showerror("缺少 tag", "请先填写内容 Release Tag")
            return
        try:
            release = self.github().get_release_by_tag(tag)
            asset_map = {asset.get("name"): asset for asset in release.get("assets", [])}
            lens_meta = self.read_release_metadata(
                asset_map.get("gyroflow-niyien-lens.cbor.gz.json")
            )
            self.lens_version_var.set(str(lens_meta.get("version", "")))
            self.lens_sha_var.set(lens_meta.get("sha256", ""))
            self.update_data_status_text()
        except Exception as err:
            messagebox.showerror("读取失败", str(err))

    def read_release_metadata(self, asset):
        if not asset:
            return {}
        return json.loads(self.github().download_text_asset(asset["browser_download_url"]))

    def action_update_data_envs(self):
        mapping = {
            "NIYIEN_CONTENT_RELEASE_TAG": self.content_tag_var.get().strip(),
            "NIYIEN_LENS_VERSION": self.lens_version_var.get().strip(),
            "NIYIEN_LENS_SHA256": self.lens_sha_var.get().strip(),
        }
        try:
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

    def preview_manifest(self):
        country = self.preview_country_var.get().strip().upper() or "US"
        platform = self.preview_platform_var.get().strip() or "windows"
        source = select_source(self.distribution_config, country)
        policy = self.current_policy
        auto_version = policy.get("auto_version", "")
        versions = policy.get("versions", [])
        auto_entry = next(
            (item for item in versions if item.get("version") == auto_version),
            versions[0] if versions else None,
        )
        content_tag = (
            self.content_tag_var.get().strip()
            or self.current_envs.get("NIYIEN_CONTENT_RELEASE_TAG", "")
            or (auto_entry["tag"] if auto_entry else "")
        )
        preview = {
            "country": country,
            "region": "cn"
            if country in set(self.distribution_config.get("routing", {}).get("cn_countries", []))
            else "global",
            "app": {
                "version": auto_entry["version"] if auto_entry else "",
                "url": f"{source['base']}/{auto_entry['tag']}/{asset_name_for_platform(platform)}"
                if auto_entry
                else "",
                "changelog": auto_entry.get("changelog", "") if auto_entry else "",
                "manual_versions": [
                    {
                        "version": item.get("version", ""),
                        "url": f"{source['base']}/{item.get('tag', '')}/{asset_name_for_platform(platform)}",
                        "changelog": item.get("changelog", ""),
                        "recommended": bool(item.get("recommended", False)),
                    }
                    for item in versions
                    if "manual" in item.get("channels", [])
                ],
            },
            "lens": {
                "version": int(self.lens_version_var.get() or "0"),
                "url": f"{source['base']}/{content_tag}/{self.distribution_config['data']['lens']['asset_name']}",
                "sha256": self.lens_sha_var.get().strip(),
            },
            "sdk_base": f"{source['base']}/{content_tag}/sdk/",
            "plugins_base": f"{source['base']}/{content_tag}/plugins/",
        }
        self.route_preview_text.delete("1.0", tk.END)
        self.route_preview_text.insert(
            "1.0", json.dumps(preview, indent=2, ensure_ascii=False)
        )

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
            response = requests.get(
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
            response = requests.post(
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
            response = requests.post(url, timeout=30)
            response.raise_for_status()
            if not silent:
                messagebox.showinfo("完成", "已触发 Vercel redeploy")
        except Exception as err:
            if not silent:
                messagebox.showerror("触发失败", str(err))

    def save_config(self):
        for key, var in self.config_vars.items():
            self.config_data[key] = var.get()
        save_json_file(CONFIG_FILE, self.config_data)
        messagebox.showinfo("完成", f"配置已保存到 {CONFIG_FILE}")


if __name__ == "__main__":
    if not CONFIG_FILE.exists() and EXAMPLE_CONFIG_FILE.exists():
        save_json_file(CONFIG_FILE, load_json_file(EXAMPLE_CONFIG_FILE, DEFAULT_CONFIG))
    app = ControlCenter()
    app.mainloop()
