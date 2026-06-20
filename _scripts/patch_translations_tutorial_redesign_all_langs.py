#!/usr/bin/env python3
# Insert the tutorial-ui-redesign strings into the remaining language .ts files.
# (zh_CN was already done by patch_translations_tutorial_redesign.py.)
#
# Byte-exact, per-file line-ending insertion: the zh_* files are CRLF while the
# upstream language files are LF, so each file is patched using its OWN existing line
# ending (never mixing). Idempotent: a file that already contains the new strings is
# skipped. Per-context presence checks mean the four "reused" App titles
# (Render queue / Export / Stabilization settings / Deep search) are only inserted into a
# language that does NOT already have them in its App context.
#
# Translations come from _scripts/_tut_tr/*.json, each a {lang: {"1".."22": text}} object.
# String numbering matches _scripts/_tut_tr/INSTRUCTIONS.md.

import os
import sys
import glob
import json

HERE = os.path.dirname(os.path.abspath(__file__))
TRANS = os.path.abspath(os.path.join(HERE, "..", "resources", "translations"))
TRJSON = os.path.join(HERE, "_tut_tr")

# number -> (context, source). App = 1..18, VideoArea = 19..21, TutorialOverlay = 22.
SOURCES = {
    "1":  ("App", "Load your footage"),
    "2":  ("App", "Drag your videos and gyro data right here."),
    "3":  ("App", "Check mounting and lens data"),
    "4":  ("App", "Set the mounting orientation; for manual-focus or anamorphic lenses, also set the lens group."),
    "5":  ("App", "Stabilization settings"),
    "6":  ("App", "Adjust smoothness, horizon lock and zoom mode. In the render queue you can multi-select or batch-edit clips."),
    "7":  ("App", "Deep search"),
    "8":  ("App", 'Pick a clip with clear motion, then right-click and choose "Deep match with gyro" to align the video with the gyro data.'),
    "9":  ("App", "Note: the video must fall within the gyro's recorded time range."),
    "10": ("App", "Export"),
    "11": ("App", 'For a finished clip choose "Export stabilized video"; to use the editor plugins choose "Export for plugins". After you export, stabilization begins.'),
    "12": ("App", "Preview"),
    "13": ("App", 'Right-click a video in the queue and choose "Edit" to preview the result.'),
    "14": ("App", "Preview and adjust"),
    "15": ("App", "Click the stabilization-preview button to switch between Original, Stabilized and Overview. You can also fine-tune the stabilization settings per clip."),
    "16": ("App", "Press Ctrl+S / Cmd+S to save once you're happy."),
    "17": ("App", "Open the queue to batch-manage videos; you can also reset processing or clear the queue."),
    "18": ("App", "Render queue"),
    "19": ("VideoArea", "Preview: original"),
    "20": ("VideoArea", "Preview: stabilized"),
    "21": ("VideoArea", "Preview: overview"),
    "22": ("TutorialOverlay", "Step %1 of %2"),
}


def esc(s):
    # Only &, <, > are illegal in XML text content; " and ' are fine (the existing
    # zh_CN patch stores literal quotes too).
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def block(src, tr, nl):
    return ("    <message>" + nl +
            "        <source>" + esc(src) + "</source>" + nl +
            "        <translation>" + esc(tr) + "</translation>" + nl +
            "    </message>" + nl)


def ctx_block(content, ctxname):
    name = "    <name>" + ctxname + "</name>"
    i = content.find(name)
    if i < 0:
        return None
    j = content.find("</context>", i)
    if j < 0:
        return None
    return (i, j)  # context text = content[i:j]


def load_langs():
    langs = {}
    for f in sorted(glob.glob(os.path.join(TRJSON, "*.json"))):
        langs.update(json.load(open(f, encoding="utf-8")))
    return langs


def patch_content(content, tr, fname, nl):
    # App + VideoArea: insert sources missing from that context, right after its <name>.
    for ctxname in ("App", "VideoArea"):
        cb = ctx_block(content, ctxname)
        if cb is None:
            print("  WARN %s: no %s context" % (fname, ctxname))
            continue
        i, j = cb
        ctx_text = content[i:j]
        ins = ""
        for num, (c, src) in SOURCES.items():
            if c != ctxname:
                continue
            if ("<source>" + esc(src) + "</source>") in ctx_text:
                continue  # reused title already present -> keep existing translation
            ins += block(src, tr[num], nl)
        if ins:
            anchor = "    <name>" + ctxname + "</name>" + nl
            if content.count(anchor) != 1:
                print("  ERROR %s: %s anchor not unique" % (fname, ctxname))
                return None
            content = content.replace(anchor, anchor + ins, 1)

    # TutorialOverlay: a NEW context in non-zh files -> create it before </TS>.
    src22 = SOURCES["22"][1]
    msg = block(src22, tr["22"], nl)
    cb = ctx_block(content, "TutorialOverlay")
    if cb is None:
        newctx = ("<context>" + nl + "    <name>TutorialOverlay</name>" + nl + msg + "</context>" + nl)
        anchor = "</context>" + nl + "</TS>"
        if content.count(anchor) != 1:
            print("  ERROR %s: </context></TS> anchor not unique" % fname)
            return None
        content = content.replace(anchor, "</context>" + nl + newctx + "</TS>", 1)
    else:
        i, j = cb
        if ("<source>" + esc(src22) + "</source>") not in content[i:j]:
            anchor = "    <name>TutorialOverlay</name>" + nl
            content = content.replace(anchor, anchor + msg, 1)
    return content


def main():
    langs = load_langs()
    patched = 0
    for code, tr in sorted(langs.items()):
        path = os.path.join(TRANS, code + ".ts")
        if not os.path.exists(path):
            print("  skip %s (no .ts)" % code)
            continue
        with open(path, "r", encoding="utf-8", newline="") as f:
            content = f.read()
        if "<source>Drag your videos and gyro data right here.</source>" in content:
            print("  %s already patched" % code)
            continue
        nl = "\r\n" if "\r\n" in content else "\n"  # match THIS file's existing ending
        new = patch_content(content, tr, code + ".ts", nl)
        if new is None:
            continue
        with open(path, "w", encoding="utf-8", newline="") as f:
            f.write(new)
        print("  patched %s (%s)" % (code, "CRLF" if nl == "\r\n" else "LF"))
        patched += 1
    print("Done. Patched %d files." % patched)
    return 0


if __name__ == "__main__":
    sys.exit(main())
