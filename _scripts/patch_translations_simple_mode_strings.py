"""One-shot patch: inject two simple-mode UX strings across all 23 .ts files.

Adds, per file:
  - App context     -> "Auto sync for plugins"   (reuses existing "Auto sync" translation)
  - RenderQueue ctx -> "Change framerate"        (uses TRANS table below)

Run from repo root:
    python _scripts/patch_translations_simple_mode_strings.py

Then regenerate .qm bundles via lrelease (see patch_translations_batch_sync_prompts.py
docstring for the lrelease invocation).

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import re
import sys

APP_QML_LINE = 744          # src/ui/App.qml      qsTr("Auto sync for plugins")
RQ_QML_LINE = 1247          # src/ui/RenderQueue.qml  qsTr("Change framerate")

# "Change framerate" per-language translations.
CHANGE_FRAMERATE: dict[str, str] = {
    "cs": "Změnit snímkovou frekvenci",
    "da": "Skift billedhastighed",
    "de": "Bildrate ändern",
    "el": "Αλλαγή ρυθμού καρέ",
    "es": "Cambiar velocidad de fotogramas",
    "fi": "Muuta kuvataajuutta",
    "fr": "Modifier la fréquence d'images",
    "gl": "Cambiar a taxa de fotogramas",
    "id": "Ubah frame rate",
    "it": "Cambia frame rate",
    "ja": "フレームレートを変更",
    "ko": "프레임 속도 변경",
    "no": "Endre bildefrekvens",
    "pl": "Zmień liczbę klatek na sekundę",
    "pt": "Alterar taxa de fotogramas",
    "pt_BR": "Alterar taxa de quadros",
    "ru": "Изменить частоту кадров",
    "sk": "Zmeniť snímkovú frekvenciu",
    "tr": "Kare hızını değiştir",
    "uk": "Змінити частоту кадрів",
    "zh_CN": "更改帧率",
    "zh_TW": "更改幀率",
}

# Sentinels used for idempotency — both reference our specific line numbers
SENT_AUTO_SYNC = (
    f'<location filename="../../src/ui/App.qml" line="{APP_QML_LINE}"/>\n'
    '        <source>Auto sync for plugins</source>'
)
SENT_FRAMERATE = (
    f'<location filename="../../src/ui/RenderQueue.qml" line="{RQ_QML_LINE}"/>\n'
    '        <source>Change framerate</source>'
)

# Locate "Auto sync" source/translation pair anywhere in the file so we can
# reuse whichever translation already exists (App or Synchronization context).
AUTO_SYNC_RE = re.compile(
    r"<source>Auto sync</source>\s*\n\s*<translation(?:\s+type=\"[^\"]+\")?>([^<]*)</translation>"
)


def make_message(source: str, translation: str | None, qml: str, line: int) -> str:
    loc = f'        <location filename="../../src/ui/{qml}" line="{line}"/>'
    if translation is None or translation == "":
        trans_tag = '<translation type="unfinished"></translation>'
    else:
        trans_tag = f"<translation>{translation}</translation>"
    return (
        "    <message>\n"
        f"{loc}\n"
        f"        <source>{source}</source>\n"
        f"        {trans_tag}\n"
        "    </message>\n"
    )


def extract_existing_auto_sync_translation(content: str) -> str | None:
    """Return first non-unfinished Auto sync translation; None if only unfinished."""
    for m in AUTO_SYNC_RE.finditer(content):
        text = m.group(1).strip()
        if text:
            return text
    return None


def patch_file(path: pathlib.Path, is_source: bool, fr_translation: str | None) -> str:
    content = path.read_text(encoding="utf-8")
    notes: list[str] = []

    # ---- App context: Auto sync for plugins ----
    if SENT_AUTO_SYNC in content:
        notes.append("auto_sync=skip")
    else:
        app_needle = "    <name>App</name>\n"
        if app_needle not in content:
            return f"FAIL {path.name}: <App> context not found"

        if is_source:
            as_trans = None  # gyroflow.ts: unfinished
        else:
            as_trans = extract_existing_auto_sync_translation(content)
            # Fall back to English literal if a language file somehow has no
            # translated Auto sync (defensive — shouldn't happen in practice).
            if as_trans is None:
                as_trans = "Auto sync"

        content = content.replace(
            app_needle,
            app_needle + make_message("Auto sync for plugins", as_trans, "App.qml", APP_QML_LINE),
            1,
        )
        notes.append("auto_sync=add")

    # ---- RenderQueue context: Change framerate ----
    if SENT_FRAMERATE in content:
        notes.append("framerate=skip")
    else:
        rq_needle = "    <name>RenderQueue</name>\n"
        if rq_needle not in content:
            return f"FAIL {path.name}: <RenderQueue> context not found"

        cf_trans = None if is_source else fr_translation
        content = content.replace(
            rq_needle,
            rq_needle + make_message("Change framerate", cf_trans, "RenderQueue.qml", RQ_QML_LINE),
            1,
        )
        notes.append("framerate=add")

    path.write_text(content, encoding="utf-8")
    return f"OK   {path.name:<14} {' '.join(notes)}"


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    if not base.is_dir():
        print(f"Translation dir missing: {base}", file=sys.stderr)
        return 1

    # Source file first
    print(patch_file(base / "gyroflow.ts", is_source=True, fr_translation=None))

    for lang, tx in CHANGE_FRAMERATE.items():
        path = base / f"{lang}.ts"
        if not path.is_file():
            print(f"MISS {path.name}")
            continue
        print(patch_file(path, is_source=False, fr_translation=tx))
    return 0


if __name__ == "__main__":
    sys.exit(main())
