"""Spot-check that the 5 new entries landed correctly in sampled languages."""
import pathlib
import xml.etree.ElementTree as ET

BASE = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"

NEW_SOURCES = {
    "Sync complete",
    "Sync complete: %1",
    "Canon CRM files are supported through the proxy workflow only.\nExport a project file and use it with your RAW workflow.",
    "Canon CRM files must be loaded together with a same-name proxy video.",
}

LANGS = ["ja", "de", "fr", "ru", "zh_TW", "el", "ko", "tr"]

for lang in LANGS:
    tree = ET.parse(BASE / f"{lang}.ts")
    root = tree.getroot()
    print(f"--- {lang} ---")
    for ctx in root.findall("context"):
        cname = ctx.find("name").text
        if cname not in ("App", "RenderQueue", "VideoArea"):
            continue
        for msg in ctx.findall("message"):
            src_el = msg.find("source")
            if src_el is None or src_el.text is None:
                continue
            if src_el.text not in NEW_SOURCES:
                continue
            tr = msg.find("translation")
            tr_text = (tr.text or "") if tr is not None else ""
            tr_type = tr.get("type", "") if tr is not None else ""
            first_src = src_el.text.split("\n")[0][:60]
            first_tr = tr_text.split("\n")[0][:80]
            tag = " [UNFINISHED]" if tr_type == "unfinished" else ""
            print(f"  [{cname:11s}] {first_src!r}\n      -> {first_tr!r}{tag}")
