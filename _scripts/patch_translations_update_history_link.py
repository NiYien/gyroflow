"""One-shot patch for changelog-history-page-and-cumulative-notes (2026-07-15).

Adds ONE new source string used by the update dialog option card in
src/ui/App.qml (App context) when the aggregated release notes were
truncated to the newest 5 versions:

    "See the full update history for earlier versions"

Inserted as a fresh <message> block inside the App context, right after
the existing "Available updates" message (a stable, App-context anchor
from the same dialog family).

No typographic arrow/dash characters are used in any string.

Run from repo root:
    python _scripts/patch_translations_update_history_link.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op (keyed on the
source already being present in the App context).
"""
from __future__ import annotations

import pathlib
import sys

# New source string.
SOURCE_LINK = "See the full update history for earlier versions"

# The existing App-context message we anchor after.
ANCHOR_SOURCE = "Available updates"

# Current source line in src/ui/App.qml (informational; lrelease matches
# by context + source, not by location).
LINE_LINK = 245

# Per-language translations for the history-page link line.
TRANS_LINK: dict[str, str] = {
    "cs": "Zobrazit úplnou historii aktualizací starších verzí",
    "da": "Se den fulde opdateringshistorik for tidligere versioner",
    "de": "Vollständigen Updateverlauf für frühere Versionen ansehen",
    "el": "Δείτε το πλήρες ιστορικό ενημερώσεων για παλαιότερες εκδόσεις",
    "es": "Ver el historial completo de actualizaciones de versiones anteriores",
    "fi": "Katso aiempien versioiden koko päivityshistoria",
    "fr": "Voir l'historique complet des mises à jour des versions précédentes",
    "gl": "Ver o historial completo de actualizacións de versións anteriores",
    "id": "Lihat riwayat pembaruan lengkap untuk versi sebelumnya",
    "it": "Vedi la cronologia completa degli aggiornamenti delle versioni precedenti",
    "ja": "これより前のバージョンの更新履歴を見る",
    "ko": "이전 버전의 전체 업데이트 기록 보기",
    "no": "Se hele oppdateringshistorikken for tidligere versjoner",
    "pl": "Zobacz pełną historię aktualizacji wcześniejszych wersji",
    "pt": "Ver o histórico completo de atualizações de versões anteriores",
    "pt_BR": "Ver o histórico completo de atualizações de versões anteriores",
    "ru": "Посмотреть полную историю обновлений предыдущих версий",
    "sk": "Zobraziť úplnú históriu aktualizácií starších verzií",
    "tr": "Önceki sürümlerin tam güncelleme geçmişini gör",
    "uk": "Переглянути повну історію оновлень попередніх версій",
    "zh_CN": "查看更早版本的完整更新记录",
    "zh_TW": "查看更早版本的完整更新記錄",
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


def app_context_bounds(raw: str) -> tuple[int, int]:
    """Return (start, end) byte offsets of the <context> whose name is App."""
    name_pos = raw.find("<name>App</name>")
    if name_pos == -1:
        raise RuntimeError("App context not found")
    ctx_start = raw.rfind("<context>", 0, name_pos)
    ctx_end = raw.find("</context>", name_pos)
    if ctx_start == -1 or ctx_end == -1:
        raise RuntimeError("App <context> bounds not found")
    return ctx_start, ctx_end


def message_block(eol: str, line: int, source: str, trans: str | None) -> str:
    if trans is None:
        tr = '<translation type="unfinished"></translation>'
    else:
        tr = f"<translation>{xml_escape(trans)}</translation>"
    return (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/App.qml" line="{line}"/>{eol}'
        f"        <source>{xml_escape(source)}</source>{eol}"
        f"        {tr}{eol}"
        f"    </message>{eol}"
    )


def patch_file(path: pathlib.Path, trans_link: str | None) -> bool:
    # trans_link=None -> source template (gyroflow.ts): emit unfinished translation.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    ctx_start, ctx_end = app_context_bounds(raw)

    # Already patched? (link source present inside the App context)
    link_tag = f"<source>{xml_escape(SOURCE_LINK)}</source>"
    if link_tag in raw[ctx_start:ctx_end]:
        return False

    # Find the anchor message's closing </message> inside the App context.
    anchor_tag = f"<source>{xml_escape(ANCHOR_SOURCE)}</source>"
    anchor_pos = raw.find(anchor_tag, ctx_start, ctx_end)
    if anchor_pos == -1:
        raise RuntimeError(f"anchor source not found in App context of {path.name}")
    close_tag = f"</message>{eol}"
    close_pos = raw.find(close_tag, anchor_pos)
    if close_pos == -1 or close_pos > ctx_end:
        raise RuntimeError(f"anchor </message> not found in App context of {path.name}")
    insert_at = close_pos + len(close_tag)

    block = message_block(eol, LINE_LINK, SOURCE_LINK, trans_link)

    raw = raw[:insert_at] + block + raw[insert_at:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    targets: list[tuple[str, str | None]] = [("gyroflow", None)]
    for lang in TRANS_LINK:
        targets.append((lang, TRANS_LINK[lang]))
    for lang, tl in targets:
        path = root / f"{lang}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        try:
            patched = patch_file(path, tl)
        except RuntimeError as e:
            print(f"{lang}: ERROR {e}")
            ok = False
            continue
        raw = path.read_bytes().decode("utf-8")
        status = "patched" if patched else "no-op"
        if f"<source>{xml_escape(SOURCE_LINK)}</source>" not in raw:
            print(f"{lang}: {status} MISSING SOURCE")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
