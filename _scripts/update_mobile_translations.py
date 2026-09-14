# SPDX-License-Identifier: GPL-3.0-or-later
"""Update mobile contexts without rewriting existing desktop translations."""
import argparse
import html
import json
import re
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TRANSLATIONS = ROOT / "resources/translations"
DATA = ROOT / "resources/translations/mobile.json"


def sources():
    result = {}
    for path in sorted((ROOT / "src/ui/mobile").glob("*.qml")):
        result[path.stem] = sorted(set(json.loads('"' + text + '"') for text in re.findall(r'qsTr\("((?:[^"\\]|\\.)*)"\)', path.read_text(encoding="utf-8"))))
    result["App"] = ["This video is still loading."]
    result["RenderQueue"] = ["Search stage %1 of %2", "Scanning segment %1 of %2"]
    result["VideoArea"] = ["Not stabilized yet. Return to Videos and tap Stabilize."]
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", action="store_true")
    args = parser.parse_args()
    catalog = sources()
    supplied = {}
    if DATA.exists():
        data = json.loads(DATA.read_text(encoding="utf-8"))
        supplied = {source: dict(zip(data["languages"], translations.split("|"), strict=True)) for source, translations in data["messages"].items()}
    missing = {}
    updates = []
    for path in sorted(TRANSLATIONS.glob("*.ts")):
        raw = path.read_bytes().decode("utf-8")
        root = ET.fromstring(raw)
        existing = {}
        contexts = {}
        for context in root.findall("context"):
            name = context.findtext("name")
            contexts[name] = context
            for message in context.findall("message"):
                translation = message.find("translation")
                if translation is not None and translation.get("type") not in ("unfinished", "vanished", "obsolete") and translation.text:
                    existing.setdefault(message.findtext("source"), translation.text)
        additions = {}
        for name, texts in catalog.items():
            original = contexts.get(name)
            known = {m.findtext("source") for m in original.findall("message")} if original is not None else set()
            for source in texts:
                if source in known:
                    continue
                translation = supplied.get(source, {}).get(path.stem, existing.get(source))
                if path.stem == "gyroflow":
                    translation = source
                if translation is None:
                    missing.setdefault(source, []).append(path.stem)
                    continue
                assert sorted(re.findall(r"%[1-9]", source)) == sorted(re.findall(r"%[1-9]", translation)), (path, source, translation)
                additions.setdefault(name, []).append("    <message>\n        <source>" + html.escape(source, quote=False) + "</source>\n        <translation>" + html.escape(translation, quote=False) + "</translation>\n    </message>\n")
        for name, messages in additions.items():
            block = "".join(messages)
            pattern = r"(<context>\s*<name>" + re.escape(name) + r"</name>.*?)(</context>)"
            if name in contexts:
                raw = re.sub(pattern, lambda match: match[1] + block + match[2], raw, count=1, flags=re.S)
            else:
                raw = raw.replace("</TS>", "<context>\n    <name>" + name + "</name>\n" + block + "</context>\n</TS>")
        updates.append((path, raw))
    if missing or args.report:
        print(json.dumps(missing, ensure_ascii=False, indent=2))
    if missing:
        raise SystemExit(1)
    if not args.report:
        for path, content in updates:
            path.write_bytes(content.encode("utf-8"))
        print(f"Updated {len(updates)} translation catalogs.")


if __name__ == "__main__":
    main()
