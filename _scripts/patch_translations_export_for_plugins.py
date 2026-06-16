"""One-shot patch for simple-mode-default-match-then-sync (2026-06-16).

Two source-string changes in src/ui/App.qml's Simple-mode export bar:

  1. The sync button label was renamed (its meaning changed from "auto sync"
     to "export a project file for the NLE plugin"):
        OLD source: "Auto sync for plugins"
        NEW source: "Export for plugins"
     Because the *source* (and thus the meaning) changed, the existing
     translations -- which all say some variant of "Auto sync" -- are now
     inaccurate and are rewritten to the new "Export for plugins" meaning.
     This patch edits the existing <message> block in the App context in place
     (source + translation + location line), so no stale obsolete entry is left
     behind and no language silently falls back to the old English label.

  2. A new lightweight notice when the batch is already current:
        NEW source: "Already synced"
     Inserted as a fresh <message> in the App context right after the renamed
     entry.

Chinese is locked per the change design:
    "Export for plugins" -> 导出（搭配插件使用）
    "Already synced"      -> 已同步

No typographic arrow/dash characters are used in any string.

Run from repo root:
    python _scripts/patch_translations_export_for_plugins.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

# The old source string whose <message> block we rewrite to the new label.
OLD_SOURCE = "Auto sync for plugins"
NEW_SOURCE = "Export for plugins"
NOTICE_SOURCE = "Already synced"

# Current source line numbers in src/ui/App.qml (informational; lrelease matches
# by context + source, not by location).
LINE_EXPORT = 791
LINE_NOTICE = 838

# Per-language translations.
#   EXPORT -> "Export for plugins" (export a project file for the NLE plugin)
#   NOTICE -> "Already synced"
TRANS: dict[str, dict[str, str]] = {
    "cs": {"EXPORT": "Exportovat pro pluginy", "NOTICE": "Již synchronizováno"},
    "da": {"EXPORT": "Eksportér til plugins", "NOTICE": "Allerede synkroniseret"},
    "de": {"EXPORT": "Für Plugins exportieren", "NOTICE": "Bereits synchronisiert"},
    "el": {"EXPORT": "Εξαγωγή για πρόσθετα", "NOTICE": "Ήδη συγχρονισμένο"},
    "es": {"EXPORT": "Exportar para complementos", "NOTICE": "Ya sincronizado"},
    "fi": {"EXPORT": "Vie liitännäisiä varten", "NOTICE": "Jo synkronoitu"},
    "fr": {"EXPORT": "Exporter pour les plugins", "NOTICE": "Déjà synchronisé"},
    "gl": {"EXPORT": "Exportar para complementos", "NOTICE": "Xa sincronizado"},
    "id": {"EXPORT": "Ekspor untuk plugin", "NOTICE": "Sudah disinkronkan"},
    "it": {"EXPORT": "Esporta per i plugin", "NOTICE": "Già sincronizzato"},
    "ja": {"EXPORT": "プラグイン用に書き出し", "NOTICE": "同期済み"},
    "ko": {"EXPORT": "플러그인용으로 내보내기", "NOTICE": "이미 동기화됨"},
    "no": {"EXPORT": "Eksporter for plugins", "NOTICE": "Allerede synkronisert"},
    "pl": {"EXPORT": "Eksportuj dla wtyczek", "NOTICE": "Już zsynchronizowano"},
    "pt": {"EXPORT": "Exportar para plugins", "NOTICE": "Já sincronizado"},
    "pt_BR": {"EXPORT": "Exportar para plugins", "NOTICE": "Já sincronizado"},
    "ru": {"EXPORT": "Экспорт для плагинов", "NOTICE": "Уже синхронизировано"},
    "sk": {"EXPORT": "Exportovať pre pluginy", "NOTICE": "Už synchronizované"},
    "tr": {"EXPORT": "Eklentiler için dışa aktar", "NOTICE": "Zaten senkronize"},
    "uk": {"EXPORT": "Експорт для плагінів", "NOTICE": "Уже синхронізовано"},
    "zh_CN": {"EXPORT": "导出（搭配插件使用）", "NOTICE": "已同步"},
    "zh_TW": {"EXPORT": "匯出（搭配外掛使用）", "NOTICE": "已同步"},
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


def patch_file(path: pathlib.Path, trans: dict[str, str] | None) -> bool:
    # trans=None -> source template (gyroflow.ts): emit unfinished translations.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    # Already patched? (new source present)
    if f"<source>{xml_escape(NEW_SOURCE)}</source>" in raw:
        return False

    old_src_tag = f"<source>{xml_escape(OLD_SOURCE)}</source>"
    src_pos = raw.find(old_src_tag)
    if src_pos == -1:
        raise RuntimeError(f"old source not found in {path.name}")

    # Locate the bounds of the <message> block containing the old source.
    # Anchor block_start at the start of the indented "<message>" line so the
    # 4-space indent is inside the replaced span (our rebuilt blocks carry their
    # own indent), avoiding indent duplication.
    msg_open = raw.rfind("<message>", 0, src_pos)
    if msg_open == -1:
        raise RuntimeError(f"enclosing <message> not found in {path.name}")
    line_start = raw.rfind("\n", 0, msg_open)
    block_start = 0 if line_start == -1 else line_start + 1
    close_tag = f"</message>{eol}"
    close_pos = raw.find(close_tag, src_pos)
    if close_pos == -1:
        raise RuntimeError(f"enclosing </message> not found in {path.name}")
    block_end = close_pos + len(close_tag)

    if trans is None:
        export_tr = '<translation type="unfinished"></translation>'
        notice_tr = '<translation type="unfinished"></translation>'
    else:
        export_tr = f"<translation>{xml_escape(trans['EXPORT'])}</translation>"
        notice_tr = f"<translation>{xml_escape(trans['NOTICE'])}</translation>"

    # Rebuild the renamed (Export) block + append the new (Already synced) block.
    new_export_block = (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/App.qml" line="{LINE_EXPORT}"/>{eol}'
        f"        <source>{xml_escape(NEW_SOURCE)}</source>{eol}"
        f"        {export_tr}{eol}"
        f"    </message>{eol}"
    )
    new_notice_block = (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/App.qml" line="{LINE_NOTICE}"/>{eol}'
        f"        <source>{xml_escape(NOTICE_SOURCE)}</source>{eol}"
        f"        {notice_tr}{eol}"
        f"    </message>{eol}"
    )

    raw = raw[:block_start] + new_export_block + new_notice_block + raw[block_end:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    targets: list[tuple[str, dict[str, str] | None]] = [("gyroflow", None)]
    targets += list(TRANS.items())
    for lang, trans in targets:
        path = root / f"{lang}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        try:
            patched = patch_file(path, trans)
        except RuntimeError as e:
            print(f"{lang}: ERROR {e}")
            ok = False
            continue
        raw = path.read_bytes().decode("utf-8")
        missing = []
        if f"<source>{xml_escape(NEW_SOURCE)}</source>" not in raw:
            missing.append("EXPORT")
        if f"<source>{xml_escape(NOTICE_SOURCE)}</source>" not in raw:
            missing.append("NOTICE")
        status = "patched" if patched else "no-op"
        if missing:
            print(f"{lang}: {status} MISSING={missing}")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
