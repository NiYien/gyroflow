"""One-shot patch: add the "Please load gyro data first." prompt translation
(simple-export-button-gating-and-settings-no-dim, 2026-06-29).

The two Simple-mode export buttons no longer gray out when no gyro data is
loaded; instead they stay clickable and, when activated with no usable
gyro/motion data, pop a one-line notice. The new source string lives in
src/ui/App.qml (context "App"), referenced by two qsTr() call sites that
lupdate would collapse into a single <message> block per .ts.

This inserts the <message> block directly after the existing
"Export stabilized video" App-context anchor in every .ts file (and the
source template gyroflow.ts), matching lupdate's escaping and each file's own
line ending, without lupdate rewriting the whole file.

zh_CN text is the user-specified wording (no trailing period); other languages
use natural wording. No typographic arrow/dash characters are used.

Run from repo root:
    python _scripts/patch_translations_load_gyro_prompt.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

# New source string added in src/ui/App.qml (context "App").
SOURCE = "Please load gyro data first."

# Existing App-context source used as the insertion anchor (unique per file).
ANCHOR = "Export stabilized video"

# Source line in src/ui/App.qml (informational only; lrelease matches by
# context + source, not by location).
LINE = 839

# Per-language translation. Meaning: load gyro/motion data before exporting.
TRANS: dict[str, str] = {
    "cs": "Nejprve načtěte data gyroskopu.",
    "da": "Indlæs venligst gyrodata først.",
    "de": "Bitte laden Sie zuerst die Gyro-Daten.",
    "el": "Φορτώστε πρώτα τα δεδομένα γυροσκοπίου.",
    "es": "Primero cargue los datos del giroscopio.",
    "fi": "Lataa ensin gyroskooppidata.",
    "fr": "Veuillez d'abord charger les données gyroscopiques.",
    "gl": "Primeiro carga os datos do xiroscopio.",
    "id": "Harap muat data giroskop terlebih dahulu.",
    "it": "Carica prima i dati del giroscopio.",
    "ja": "先にジャイロデータを読み込んでください。",
    "ko": "먼저 자이로 데이터를 불러오세요.",
    "no": "Last inn gyrodata først.",
    "pl": "Najpierw wczytaj dane żyroskopu.",
    "pt": "Carregue primeiro os dados do giroscópio.",
    "pt_BR": "Carregue primeiro os dados do giroscópio.",
    "ru": "Сначала загрузите данные гироскопа.",
    "sk": "Najprv načítajte dáta gyroskopu.",
    "tr": "Lütfen önce jiroskop verilerini yükleyin.",
    "uk": "Спочатку завантажте дані гіроскопа.",
    "zh_CN": "请加载陀螺仪数据",
    "zh_TW": "請載入陀螺儀資料",
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


def patch_file(path: pathlib.Path, trans: str | None) -> bool:
    # trans=None -> source template (gyroflow.ts): emit unfinished translation.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    # Already patched?
    if f"<source>{xml_escape(SOURCE)}</source>" in raw:
        return False

    anchor = f"<source>{xml_escape(ANCHOR)}</source>"
    src_pos = raw.find(anchor)
    if src_pos == -1:
        raise RuntimeError(f"anchor source not found in {path.name}")
    close_tag = f"</message>{eol}"
    close_pos = raw.find(close_tag, src_pos)
    if close_pos == -1:
        raise RuntimeError(f"anchor </message> not found in {path.name}")
    insert_pos = close_pos + len(close_tag)

    if trans is None:
        translation = '<translation type="unfinished"></translation>'
    else:
        translation = f"<translation>{xml_escape(trans)}</translation>"
    block = (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/App.qml" line="{LINE}"/>{eol}'
        f"        <source>{xml_escape(SOURCE)}</source>{eol}"
        f"        {translation}{eol}"
        f"    </message>{eol}"
    )

    raw = raw[:insert_pos] + block + raw[insert_pos:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    # None -> source template gets unfinished entry (English fallback at runtime).
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
        if f"<source>{xml_escape(SOURCE)}</source>" not in raw:
            print(f"{lang}: PROBLEM SOURCE_MISSING")
            ok = False
        else:
            print(f"{lang}: {'patched' if patched else 'no-op'}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
