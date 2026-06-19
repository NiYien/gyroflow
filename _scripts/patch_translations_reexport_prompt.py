"""One-shot patch for simple-mode-reexport-and-reset-pairing (2026-06-19).

Adds ONE new source string used by the Simple-mode re-export confirmation
dialog in src/ui/App.qml (App context, confirmReExport helper):

    "Already exported. Re-export?"

It is inserted as a fresh <message> block inside the App context, right after
the existing "Reset pairing" message (a stable, App-context-unique anchor added
by patch_translations_reset_pairing.py).

No typographic arrow/dash characters are used in any string.

Run from repo root:
    python _scripts/patch_translations_reexport_prompt.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op (keyed on the new
source already being present in the App context).
"""
from __future__ import annotations

import pathlib
import sys

# New source string.
SOURCE = "Already exported. Re-export?"

# The existing App-context message we anchor after ("Reset pairing" tooltip).
ANCHOR_SOURCE = "Reset pairing"

# Current source line in src/ui/App.qml (informational; lrelease matches by
# context + source, not by location).
LINE = 1921

# Per-language translations.
TRANS: dict[str, str] = {
    "cs": "Již exportováno. Exportovat znovu?",
    "da": "Allerede eksporteret. Eksportér igen?",
    "de": "Bereits exportiert. Erneut exportieren?",
    "el": "Έχει ήδη εξαχθεί. Εξαγωγή ξανά;",
    "es": "Ya exportado. ¿Volver a exportar?",
    "fi": "Jo viety. Vie uudelleen?",
    "fr": "Déjà exporté. Exporter à nouveau ?",
    "gl": "Xa exportado. Exportar de novo?",
    "id": "Sudah diekspor. Ekspor lagi?",
    "it": "Già esportato. Esportare di nuovo?",
    "ja": "すでにエクスポート済みです。再エクスポートしますか？",
    "ko": "이미 내보냈습니다. 다시 내보낼까요?",
    "no": "Allerede eksportert. Eksporter på nytt?",
    "pl": "Już wyeksportowano. Eksportować ponownie?",
    "pt": "Já exportado. Exportar novamente?",
    "pt_BR": "Já exportado. Exportar novamente?",
    "ru": "Уже экспортировано. Экспортировать снова?",
    "sk": "Už exportované. Exportovať znova?",
    "tr": "Zaten dışa aktarıldı. Yeniden dışa aktarılsın mı?",
    "uk": "Вже експортовано. Експортувати знову?",
    "zh_CN": "已导出。是否重新导出？",
    "zh_TW": "已匯出。是否重新匯出？",
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
    """Return (start, end) byte offsets of the <context> whose <name>App</name> is."""
    name_pos = raw.find("<name>App</name>")
    if name_pos == -1:
        raise RuntimeError("App context not found")
    ctx_start = raw.rfind("<context>", 0, name_pos)
    ctx_end = raw.find("</context>", name_pos)
    if ctx_start == -1 or ctx_end == -1:
        raise RuntimeError("App <context> bounds not found")
    return ctx_start, ctx_end


def patch_file(path: pathlib.Path, trans: str | None) -> bool:
    # trans=None -> source template (gyroflow.ts): emit unfinished translation.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    ctx_start, ctx_end = app_context_bounds(raw)

    # Already patched? (new source present inside the App context)
    new_tag = f"<source>{xml_escape(SOURCE)}</source>"
    if new_tag in raw[ctx_start:ctx_end]:
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

    if trans is None:
        tr = '<translation type="unfinished"></translation>'
    else:
        tr = f"<translation>{xml_escape(trans)}</translation>"

    block = (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/App.qml" line="{LINE}"/>{eol}'
        f"        <source>{xml_escape(SOURCE)}</source>{eol}"
        f"        {tr}{eol}"
        f"    </message>{eol}"
    )

    raw = raw[:insert_at] + block + raw[insert_at:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    targets: list[tuple[str, str | None]] = [("gyroflow", None)]
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
        status = "patched" if patched else "no-op"
        if f"<source>{xml_escape(SOURCE)}</source>" not in raw:
            print(f"{lang}: {status} MISSING SOURCE")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
