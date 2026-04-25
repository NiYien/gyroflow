"""Local git operations — subprocess-based, returns structured results.

Ported from legacy control_center.py git helpers.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

# On Windows, when control_center.pyw runs under pythonw.exe (no console),
# spawning subprocesses still pops a brief black console window for each
# git invocation — visible as a flicker when the publish view first opens.
# CREATE_NO_WINDOW (0x08000000) suppresses that.
_NO_WINDOW_KWARGS: dict = {}
if sys.platform == "win32":
    _NO_WINDOW_KWARGS["creationflags"] = 0x08000000  # CREATE_NO_WINDOW


def run_git(workdir: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(
        ["git", *args],
        cwd=str(workdir),
        text=True,
        capture_output=True,
        check=check,
        **_NO_WINDOW_KWARGS,
    )


def get_current_branch(workdir: Path) -> str:
    try:
        return run_git(workdir, "branch", "--show-current").stdout.strip()
    except Exception:
        return ""


def get_head_commit_sha(workdir: Path) -> str:
    try:
        return run_git(workdir, "rev-parse", "HEAD").stdout.strip()
    except Exception:
        return ""


def get_head_commit_subject(workdir: Path) -> str:
    try:
        return run_git(
            workdir,
            "-c", "i18n.logOutputEncoding=utf-8",
            "log", "-1", "--pretty=%s",
        ).stdout.strip()
    except Exception:
        return ""


def get_worktree_status_summary(workdir: Path) -> str:
    try:
        return run_git(workdir, "status", "--short").stdout.strip()
    except Exception:
        return ""


def local_tag_exists(workdir: Path, tag: str) -> bool:
    try:
        run_git(workdir, "rev-parse", "-q", "--verify", f"refs/tags/{tag}")
        return True
    except subprocess.CalledProcessError:
        return False


def remote_tag_exists(workdir: Path, remote: str, tag: str) -> bool:
    try:
        result = run_git(workdir, "ls-remote", "--tags", remote, tag)
        return bool(result.stdout.strip())
    except Exception:
        return False


def get_remote_branch_sha(workdir: Path, remote: str, branch: str) -> str:
    if not branch:
        return ""
    try:
        return run_git(
            workdir, "ls-remote", "--heads", remote, f"refs/heads/{branch}"
        ).stdout.strip().split("\t")[0].strip()
    except Exception:
        return ""


def create_and_push_tag(workdir: Path, remote: str, tag: str) -> None:
    """Raises CalledProcessError on any failure."""
    run_git(workdir, "tag", tag)
    run_git(workdir, "push", remote, tag)
