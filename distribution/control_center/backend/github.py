"""GitHub REST API client — ported from legacy control_center.py::GitHubClient.

Only the methods needed for Dashboard + release/action listing are included
in this first cut. Additional methods (dispatch_workflow, create_tag,
list_run_artifacts, …) will be added as later features come online.
"""

from __future__ import annotations

import requests

from .helpers import build_proxy_mapping, normalize_proxy_url


class GitHubClient:
    def __init__(self, owner: str, repo: str, token: str = "", proxy_url: str = ""):
        self.owner = owner.strip()
        self.repo = repo.strip()
        self.token = token.strip()
        self.proxy_url = normalize_proxy_url(proxy_url)

    def _headers(self) -> dict:
        headers = {"Accept": "application/vnd.github+json"}
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        return headers

    def _public_headers(self) -> dict:
        return {"Accept": "application/vnd.github+json"}

    def _request_kwargs(self, *, timeout: int, **kwargs) -> dict:
        payload = dict(kwargs)
        payload["timeout"] = timeout
        proxies = build_proxy_mapping(self.proxy_url)
        if proxies:
            payload["proxies"] = proxies
        return payload

    def _ensure_ready(self) -> None:
        if not self.owner or not self.repo:
            raise RuntimeError("Missing GitHub owner/repo")

    def _get(self, url: str, *, params: dict | None = None, timeout: int = 30):
        """GET with token first, fall back to public headers on 403/404 when token set."""
        response = requests.get(
            url,
            headers=self._headers(),
            params=params,
            **self._request_kwargs(timeout=timeout),
        )
        if response.status_code in {403, 404} and self.token:
            response.close()
            response = requests.get(
                url,
                headers=self._public_headers(),
                params=params,
                **self._request_kwargs(timeout=timeout),
            )
        return response

    # ---- Releases ----

    def list_releases(self) -> list[dict]:
        return self.list_repo_releases(self.owner, self.repo)

    def list_repo_releases(self, owner: str, repo: str) -> list[dict]:
        self._ensure_ready()
        url = f"https://api.github.com/repos/{owner}/{repo}/releases"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    def get_latest_release(self, owner: str | None = None, repo: str | None = None) -> dict:
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/releases/latest"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    # ---- Action runs ----

    def list_repo_workflow_runs(
        self,
        owner: str | None = None,
        repo: str | None = None,
        *,
        branch: str = "",
        per_page: int = 20,
        status: str = "completed",
    ) -> list[dict]:
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        params = {
            "per_page": max(1, min(int(per_page), 100)),
            "exclude_pull_requests": "true",
        }
        if status:
            params["status"] = status
        if branch:
            params["branch"] = branch
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/runs"
        response = self._get(url, params=params, timeout=30)
        response.raise_for_status()
        payload = response.json()
        return payload.get("workflow_runs", []) if isinstance(payload, dict) else []

    def list_workflows(self, owner: str | None = None, repo: str | None = None) -> list[dict]:
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/workflows"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        payload = response.json()
        return payload.get("workflows", []) if isinstance(payload, dict) else []

    # ---- Repo / branches / tags (remote operations, no local clone needed) ----

    def get_repo(self, owner: str | None = None, repo: str | None = None) -> dict:
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()

    def get_default_branch_sha(self, owner: str | None = None, repo: str | None = None) -> str:
        """Resolve owner/repo's default branch head commit sha in one logical call."""
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        meta = self.get_repo(owner, repo)
        branch = str(meta.get("default_branch", "")).strip() or "main"
        url = f"https://api.github.com/repos/{owner}/{repo}/git/refs/heads/{branch}"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        payload = response.json()
        return str(payload.get("object", {}).get("sha", "")).strip()

    def list_repo_tags(self, owner: str | None = None, repo: str | None = None, per_page: int = 100) -> list[dict]:
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/tags"
        response = self._get(url, params={"per_page": max(1, min(int(per_page), 100))}, timeout=30)
        response.raise_for_status()
        payload = response.json()
        return payload if isinstance(payload, list) else []

    def create_remote_tag(self, owner: str, repo: str, tag: str, sha: str) -> dict:
        """POST /repos/{owner}/{repo}/git/refs — create a lightweight tag pointing at sha.

        Returns the created ref payload on success. Raises RuntimeError with a
        friendly message if the tag already exists (GitHub returns 422).
        """
        self._ensure_ready()
        if not self.token:
            raise RuntimeError("Missing GitHub token for remote tag creation")
        owner = owner.strip()
        repo = repo.strip()
        tag = tag.strip()
        sha = sha.strip()
        if not (owner and repo and tag and sha):
            raise RuntimeError("owner/repo/tag/sha are all required")
        url = f"https://api.github.com/repos/{owner}/{repo}/git/refs"
        response = requests.post(
            url,
            headers=self._headers(),
            json={"ref": f"refs/tags/{tag}", "sha": sha},
            **self._request_kwargs(timeout=30),
        )
        if response.status_code == 422:
            try:
                msg = response.json().get("message", "tag already exists")
            except Exception:
                msg = "tag already exists"
            raise RuntimeError(f"GitHub 拒绝创建 tag: {msg}")
        if response.status_code in (401, 403):
            try:
                msg = response.json().get("message", "forbidden")
            except Exception:
                msg = "forbidden"
            raise RuntimeError(
                f"GitHub 拒绝创建 tag ({response.status_code}: {msg})。可能原因:\n"
                f"  1. fine-grained PAT 未授权 {owner}/{repo} 的 Contents: Read and write 权限\n"
                f"     → Settings → Developer settings → Personal access tokens → Fine-grained → 编辑 token,Repository access 加入此仓库并勾 Contents write\n"
                f"  2. Classic PAT 缺少 'repo' scope\n"
                f"  3. 仓库/组织设有 push restriction"
            )
        response.raise_for_status()
        return response.json()

    def get_workflow_run(self, owner: str | None = None, repo: str | None = None, run_id: int = 0) -> dict | None:
        """Returns run payload, or None if 404 (run deleted / expired)."""
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/runs/{int(run_id)}"
        response = self._get(url, timeout=30)
        if response.status_code == 404:
            return None
        response.raise_for_status()
        return response.json()

    def list_run_artifacts(self, owner: str | None = None, repo: str | None = None, run_id: int = 0) -> list[dict]:
        """List artifacts attached to a workflow run. Empty list when 404."""
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/runs/{int(run_id)}/artifacts"
        response = self._get(url, params={"per_page": 100}, timeout=30)
        if response.status_code == 404:
            return []
        response.raise_for_status()
        payload = response.json()
        artifacts = payload.get("artifacts") if isinstance(payload, dict) else []
        return [item for item in artifacts or [] if isinstance(item, dict)]

    def list_repo_artifacts(
        self,
        owner: str | None = None,
        repo: str | None = None,
        *,
        name: str = "",
        per_page: int = 100,
    ) -> list[dict]:
        """List repo-level artifacts. With `name`, GitHub server-filters by exact
        artifact name and returns each artifact's workflow_run.id, so the latest
        run that produced a given artifact is one API call away.
        """
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/artifacts"
        params: dict = {"per_page": max(1, min(int(per_page), 100))}
        if name:
            params["name"] = name
        response = self._get(url, params=params, timeout=30)
        if response.status_code == 404:
            return []
        response.raise_for_status()
        payload = response.json()
        artifacts = payload.get("artifacts") if isinstance(payload, dict) else []
        return [item for item in artifacts or [] if isinstance(item, dict)]

    def dispatch_workflow(
        self,
        workflow: str,
        ref: str,
        inputs: dict | None = None,
        *,
        owner: str | None = None,
        repo: str | None = None,
    ) -> bool:
        """POST /actions/workflows/<workflow>/dispatches — requires write token.

        owner/repo default to self.owner/self.repo so existing callers stay
        unchanged; override them to dispatch into a sibling repository (e.g.
        plugin builds from the gyroflow-targeted client without spinning up
        a separate GitHubClient instance).
        """
        self._ensure_ready()
        if not self.token:
            raise RuntimeError("Missing GitHub token for workflow dispatch")
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        url = f"https://api.github.com/repos/{owner}/{repo}/actions/workflows/{workflow}/dispatches"
        response = requests.post(
            url,
            headers=self._headers(),
            json={"ref": ref, "inputs": inputs or {}},
            **self._request_kwargs(timeout=30),
        )
        response.raise_for_status()
        return True

    def get_branch_head_commit(
        self,
        owner: str | None = None,
        repo: str | None = None,
        branch: str = "",
    ) -> dict:
        """Fetch /repos/{owner}/{repo}/commits/{branch} payload.

        Useful when the repo is not locally cloned and we need the latest
        commit message (e.g. to prefill a workflow_dispatch build_label
        without forcing the operator to clone the plugin / lens repo).
        """
        self._ensure_ready()
        owner = (owner or self.owner).strip()
        repo = (repo or self.repo).strip()
        ref = branch.strip()
        if not ref:
            meta = self.get_repo(owner, repo)
            ref = str(meta.get("default_branch", "")).strip() or "main"
        url = f"https://api.github.com/repos/{owner}/{repo}/commits/{ref}"
        response = self._get(url, timeout=30)
        response.raise_for_status()
        return response.json()
