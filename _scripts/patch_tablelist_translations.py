#!/usr/bin/env python3
"""Batch-insert TableList-context translations for "Image stabilization" / "On" / "Off"
into all 22 language .ts files. Idempotent: skip an entry if the <source> already exists
(with any translation) in the TableList context.

Run: python _scripts/patch_tablelist_translations.py
"""
import os
import re
from pathlib import Path

TRANSLATIONS = {
    "cs":    {"Image stabilization": "Stabilizace obrazu",         "On": "Zapnuto",       "Off": "Vypnuto"},
    "da":    {"Image stabilization": "Billedstabilisering",        "On": "Til",           "Off": "Fra"},
    "de":    {"Image stabilization": "Bildstabilisierung",         "On": "Ein",           "Off": "Aus"},
    "el":    {"Image stabilization": "Σταθεροποίηση εικόνας",
              "On": "Ενεργό",
              "Off": "Ανενεργό"},
    "es":    {"Image stabilization": "Estabilización de imagen",  "On": "Activado",  "Off": "Desactivado"},
    "fi":    {"Image stabilization": "Kuvan vakautus",             "On": "Päällä", "Off": "Pois"},
    "fr":    {"Image stabilization": "Stabilisation d'image",      "On": "Activé",   "Off": "Désactivé"},
    "gl":    {"Image stabilization": "Estabilización de imaxe",   "On": "Activado",  "Off": "Desactivado"},
    "id":    {"Image stabilization": "Stabilisasi gambar",         "On": "Aktif",         "Off": "Nonaktif"},
    "it":    {"Image stabilization": "Stabilizzazione dell'immagine",  "On": "Attivo",    "Off": "Disattivo"},
    "ja":    {"Image stabilization": "画像安定化",  "On": "オン",
              "Off": "オフ"},
    "ko":    {"Image stabilization": "이미지 안정화",
              "On": "켜짐",
              "Off": "꺼짐"},
    "no":    {"Image stabilization": "Bildestabilisering",         "On": "På",       "Off": "Av"},
    "pl":    {"Image stabilization": "Stabilizacja obrazu",        "On": "Wł.",      "Off": "Wył."},
    "pt":    {"Image stabilization": "Estabilização de imagem", "On": "Ativado","Off": "Desativado"},
    "pt_BR": {"Image stabilization": "Estabilização de imagem", "On": "Ativado","Off": "Desativado"},
    "ru":    {"Image stabilization": "Стабилизация изображения",
              "On": "Вкл",
              "Off": "Выкл"},
    "sk":    {"Image stabilization": "Stabilizácia obrazu",   "On": "Zapnuté",  "Off": "Vypnuté"},
    "tr":    {"Image stabilization": "Görüntü sabitleme", "On": "Açık","Off": "Kapalı"},
    "uk":    {"Image stabilization": "Стабілізація зображення",
              "On": "Увімк.",
              "Off": "Вимк."},
    "zh_CN": {"Image stabilization": "图像稳定",   "On": "开",        "Off": "关"},
    "zh_TW": {"Image stabilization": "影像穩定",   "On": "開",        "Off": "關"},
}

TABLELIST_CONTEXT_RE = re.compile(
    r"(<context>\s*<name>TableList</name>.*?)(</context>)",
    re.DOTALL,
)


def entry_already_exists(ts_body: str, source: str) -> bool:
    """Check if a <source>SOURCE</source> already exists in the TableList context."""
    m = TABLELIST_CONTEXT_RE.search(ts_body)
    if not m:
        return False
    ctx = m.group(1)
    escaped = re.escape(source)
    return bool(re.search(rf"<source>{escaped}</source>", ctx))


def build_message(source: str, translation: str) -> str:
    return (
        "    <message>\n"
        "        <location filename=\"../../src/ui/menu/VideoInformation.qml\" line=\"60\"/>\n"
        f"        <source>{source}</source>\n"
        f"        <translation>{translation}</translation>\n"
        "    </message>\n"
    )


def patch_file(path: Path, translations: dict) -> tuple[int, int]:
    body = path.read_text(encoding="utf-8")
    if "<name>TableList</name>" not in body:
        print(f"  [skip] {path.name}: no TableList context")
        return 0, 0

    inserts = []
    for source, translation in translations.items():
        if entry_already_exists(body, source):
            continue
        inserts.append(build_message(source, translation))

    if not inserts:
        return 0, 0

    new_body = TABLELIST_CONTEXT_RE.sub(
        lambda m: m.group(1) + "".join(inserts) + m.group(2),
        body,
        count=1,
    )
    path.write_text(new_body, encoding="utf-8")
    return len(inserts), len(translations)


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    trans_dir = root / "resources" / "translations"
    total_added = 0
    for lang, translations in TRANSLATIONS.items():
        ts_path = trans_dir / f"{lang}.ts"
        if not ts_path.is_file():
            print(f"  [skip] missing {ts_path}")
            continue
        added, attempted = patch_file(ts_path, translations)
        total_added += added
        print(f"  {lang:6s} +{added}/{attempted}")
    print(f"Total entries added: {total_added}")


if __name__ == "__main__":
    main()
