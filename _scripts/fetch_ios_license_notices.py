# SPDX-License-Identifier: GPL-3.0-or-later
"""Recover omitted crate notices from each package's exact upstream commit."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
from pathlib import Path, PurePosixPath
import urllib.parse
import urllib.request
import urllib.error

from generate_ios_licenses import ROOT, collect


def fetch(url):
    request = urllib.request.Request(url, headers={"User-Agent": "NiYien-License-Audit/1.0"})
    with urllib.request.urlopen(request, timeout=30) as response:
        return response.read()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("metadata")
    args = parser.parse_args()
    metadata = json.loads(Path(args.metadata).read_text())
    _, _, missing = collect(metadata)
    groups = {}
    for package in metadata["packages"]:
        key = f"{package['name']} {package['version']}"
        if key not in missing:
            continue
        folder = Path(package["manifest_path"]).parent
        vcs_file = folder / ".cargo_vcs_info.json"
        vcs = json.loads(vcs_file.read_text()) if vcs_file.exists() else {}
        source = package.get("source") or ""
        repository = package.get("repository") or ""
        commit = vcs.get("git", {}).get("sha1", "")
        if source.startswith("git+"):
            parsed = urllib.parse.urlsplit(source[4:])
            repository = f"{parsed.scheme}://{parsed.netloc}{parsed.path}"
            commit = parsed.fragment
        repository = repository.replace("http://github.com/", "https://github.com/").removesuffix(".git").rstrip("/")
        if not repository.startswith("https://github.com/") or not commit:
            continue
        repo = "/".join(repository.removeprefix("https://github.com/").split("/")[:2])
        groups.setdefault((repo, commit), []).append((key, vcs.get("path_in_vcs", "")))

    destination = ROOT / "resources/legal/dependency-notices"
    destination.mkdir(parents=True, exist_ok=True)
    index_file = ROOT / "resources/legal/notice-sources.json"
    index = json.loads(index_file.read_text()) if index_file.exists() else {}

    def recover(group):
        (repo, commit), packages = group
        try:
            try:
                tree = json.loads(fetch(f"https://api.github.com/repos/{repo}/git/trees/{commit}?recursive=1"))["tree"]
            except urllib.error.HTTPError as error:
                if error.code != 403:
                    raise
                # Raw commit URLs remain available when the public tree API is rate limited.
                tree = [{"type": "blob", "path": name} for name in (
                    "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "LICENSE.txt", "LICENSE-MIT.txt",
                    "LICENSE-APACHE.txt", "COPYING", "LICENSE.md", "LICENSE-MPL-2.0", "LICENCE", "license.txt")]
            found = {}
            for key, crate_path in packages:
                parents = {"", crate_path}
                parents.update(str(p) if str(p) != "." else "" for p in PurePosixPath(crate_path).parents)
                paths = [entry["path"] for entry in tree if entry["type"] == "blob"
                         and PurePosixPath(entry["path"]).name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"))
                         and str(PurePosixPath(entry["path"]).parent).replace(".", "", 1) in parents]
                records = []
                for path in paths:
                    url = f"https://raw.githubusercontent.com/{repo}/{commit}/{urllib.parse.quote(path)}"
                    try:
                        data = fetch(url)
                    except urllib.error.HTTPError as error:
                        if error.code == 404:
                            continue
                        raise
                    if not data.strip():
                        continue
                    digest = hashlib.sha256(data).hexdigest()
                    filename = digest + ".txt"
                    (destination / filename).write_bytes(data)
                    records.append({"file": "dependency-notices/" + filename, "source": url, "sha256": digest})
                if records:
                    found[key] = records
            return found, None
        except Exception as error:
            return {}, f"{repo}@{commit}: {error}"

    with ThreadPoolExecutor(max_workers=5) as pool:
        for found, error in pool.map(recover, groups.items()):
            index.update(found)
            if error:
                print(error, flush=True)
            elif found:
                print("Recovered: " + ", ".join(found), flush=True)
    index_file.write_text(json.dumps(index, indent=2, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
