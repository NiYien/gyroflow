"""Read / mutate `Cargo.toml` workspace.package.version in place.

The `_push_tag_via_api_or_clone` and `create_and_push_tag` flows in `api.py`
use these helpers to keep `[workspace.package].version` in sync with the
git tag the user is about to push. plugin / app build.rs files embed the
PE FileVersion from `CARGO_PKG_VERSION`, so a stale workspace version means
the released binary's internal version does not match the git tag — see
the v2.1.2 plugin incident on 2026-04-25 for the symptom.
"""

from __future__ import annotations

import re
import tomllib
from pathlib import Path


def read_workspace_version(cargo_path: Path) -> str | None:
    """Return `[workspace.package].version` as string.

    Returns None when:
      - the file does not exist (e.g. lens-data repo with no Rust workspace),
      - the file is not parseable TOML,
      - there is no `[workspace.package]` table or no string `version` key.
    """
    if not cargo_path.exists():
        return None
    try:
        data = tomllib.loads(cargo_path.read_text(encoding="utf-8"))
    except Exception:
        return None
    workspace = data.get("workspace")
    if not isinstance(workspace, dict):
        return None
    package = workspace.get("package")
    if not isinstance(package, dict):
        return None
    version = package.get("version")
    return version if isinstance(version, str) else None


def write_workspace_version(cargo_path: Path, new_version: str) -> bool:
    """Rewrite the `version = "..."` line inside `[workspace.package]` in place.

    Preserves whitespace, comments, key ordering, and original line endings.
    Returns True if a line was rewritten, False if no matching line was found
    (in which case the file is left untouched).
    """
    text = cargo_path.read_text(encoding="utf-8")
    lines = text.splitlines(keepends=True)
    in_target_section = False
    version_pattern = re.compile(r'^(\s*version\s*=\s*")[^"]*(".*)$')
    for i, line in enumerate(lines):
        stripped = line.strip()
        # Section header detection. We only mutate the version inside
        # [workspace.package] — other sections (e.g. [package] for a non-workspace
        # crate, or [dependencies] tables) keep their version keys intact.
        if stripped.startswith("[") and stripped.endswith("]"):
            in_target_section = stripped == "[workspace.package]"
            continue
        if in_target_section:
            body = line.rstrip("\r\n")
            line_ending = line[len(body):]
            m = version_pattern.match(body)
            if m:
                lines[i] = f"{m.group(1)}{new_version}{m.group(2)}{line_ending}"
                cargo_path.write_text("".join(lines), encoding="utf-8")
                return True
    return False
