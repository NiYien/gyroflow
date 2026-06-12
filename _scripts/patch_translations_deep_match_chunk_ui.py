"""One-shot patch: fill translations for the chunked-scan segment-progress
string (change deep-match-chunked-scan) in all 22 language .ts files.

The string was introduced by editing src/ui/RenderQueue.qml and running
lupdate (which left it as <translation type="unfinished"/>):

  SEGMENT - "Scanning segment %1 of %2" line in the deep match progress modal

Run from repo root:
    python _scripts/patch_translations_deep_match_chunk_ui.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

SOURCES = {
    "SEGMENT": "Scanning segment %1 of %2",
}

TRANS: dict[str, dict[str, str]] = {
    "cs": {"SEGMENT": "Prohledávání úseku %1 z %2"},
    "da": {"SEGMENT": "Scanner segment %1 af %2"},
    "de": {"SEGMENT": "Durchsuche Abschnitt %1 von %2"},
    "el": {"SEGMENT": "Σάρωση τμήματος %1 από %2"},
    "es": {"SEGMENT": "Escaneando segmento %1 de %2"},
    "fi": {"SEGMENT": "Skannataan osaa %1/%2"},
    "fr": {"SEGMENT": "Analyse du segment %1 sur %2"},
    "gl": {"SEGMENT": "Escaneando segmento %1 de %2"},
    "id": {"SEGMENT": "Memindai segmen %1 dari %2"},
    "it": {"SEGMENT": "Scansione del segmento %1 di %2"},
    "ja": {"SEGMENT": "セグメント %1/%2 をスキャン中"},
    "ko": {"SEGMENT": "세그먼트 %1/%2 스캔 중"},
    "no": {"SEGMENT": "Skanner segment %1 av %2"},
    "pl": {"SEGMENT": "Skanowanie segmentu %1 z %2"},
    "pt": {"SEGMENT": "A analisar o segmento %1 de %2"},
    "pt_BR": {"SEGMENT": "Escaneando segmento %1 de %2"},
    "ru": {"SEGMENT": "Сканирование сегмента %1 из %2"},
    "sk": {"SEGMENT": "Prehľadávanie úseku %1 z %2"},
    "tr": {"SEGMENT": "Segment %1/%2 taranıyor"},
    "uk": {"SEGMENT": "Сканування сегмента %1 з %2"},
    "zh_CN": {"SEGMENT": "正在扫描第 %1 / %2 段"},
    "zh_TW": {"SEGMENT": "正在掃描第 %1 / %2 段"},
}


def xml_escape(text: str) -> str:
    # Match lupdate's escaping: &, <, > always; " and ' as entities.
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
        .replace("'", "&apos;")
    )


def patch_file(path: pathlib.Path, trans: dict[str, str]) -> list[str]:
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"
    patched = []
    for key, source in SOURCES.items():
        src = xml_escape(source)
        needle = (
            f"<source>{src}</source>{eol}"
            f'        <translation type="unfinished"></translation>'
        )
        if needle not in raw:
            continue  # already patched, or entry missing (reported by caller)
        replacement = (
            f"<source>{src}</source>{eol}"
            f"        <translation>{xml_escape(trans[key])}</translation>"
        )
        raw = raw.replace(needle, replacement, 1)
        patched.append(key)
    if patched:
        path.write_bytes(raw.encode("utf-8"))
    return patched


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    for lang, trans in TRANS.items():
        path = root / f"{lang}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        patched = patch_file(path, trans)
        raw = path.read_bytes().decode("utf-8")
        missing = [
            k for k, s in SOURCES.items()
            if f"<source>{xml_escape(s)}</source>" not in raw
        ]
        unfinished = [
            k for k, s in SOURCES.items()
            if f"<source>{xml_escape(s)}</source>" in raw
            and f"<source>{xml_escape(s)}</source>" + ("\r\n" if "\r\n" in raw else "\n")
            + '        <translation type="unfinished"></translation>' in raw
        ]
        status = f"patched={patched}" if patched else "no-op"
        if missing or unfinished:
            print(f"{lang}: {status} MISSING={missing} STILL_UNFINISHED={unfinished}")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
