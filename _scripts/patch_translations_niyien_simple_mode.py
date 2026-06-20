#!/usr/bin/env python3
# Insert the NiYien simple-mode + tutorial-nav strings (only translated in zh_CN) into
# the other 21 language .ts files. Source list: _scripts/_tut_tr/niyien_strings.json
# (context/source/idx). Translations: _scripts/_tut_tr/niyien_translations.json
# ({lang: {"1".."30": text}}).
#
# Byte-exact, per-file line ending (zh_TW=CRLF, the other 20=LF). Idempotent: a source
# already present in its target context is skipped (so re-running is a no-op and there
# are no duplicates with the earlier tutorial strings). Both target contexts (App,
# TutorialOverlay) already exist in every file.

import os
import sys
import json

HERE = os.path.dirname(os.path.abspath(__file__))
TRANS = os.path.abspath(os.path.join(HERE, "..", "resources", "translations"))
TRJSON = os.path.join(HERE, "_tut_tr")

STRINGS = json.load(open(os.path.join(TRJSON, "niyien_strings.json"), encoding="utf-8"))
# idx(str) -> (context, source)
SOURCES = {str(d["idx"]): (d["context"], d["source"]) for d in STRINGS}
LANGS = json.load(open(os.path.join(TRJSON, "niyien_translations.json"), encoding="utf-8"))


def esc(s):
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
    return (i, j)


def patch_content(content, tr, fname, nl):
    for ctxname in ("App", "TutorialOverlay"):
        cb = ctx_block(content, ctxname)
        if cb is None:
            print("  WARN %s: no %s context" % (fname, ctxname))
            continue
        i, j = cb
        ctx_text = content[i:j]
        ins = ""
        for idx, (c, src) in SOURCES.items():
            if c != ctxname:
                continue
            if ("<source>" + esc(src) + "</source>") in ctx_text:
                continue  # already present -> skip (idempotent / no dup)
            ins += block(src, tr[idx], nl)
        if ins:
            anchor = "    <name>" + ctxname + "</name>" + nl
            if content.count(anchor) != 1:
                print("  ERROR %s: %s anchor not unique" % (fname, ctxname))
                return None
            content = content.replace(anchor, anchor + ins, 1)
    return content


def main():
    patched = 0
    for code, tr in sorted(LANGS.items()):
        path = os.path.join(TRANS, code + ".ts")
        if not os.path.exists(path):
            print("  skip %s (no .ts)" % code)
            continue
        with open(path, "r", encoding="utf-8", newline="") as f:
            content = f.read()
        nl = "\r\n" if "\r\n" in content else "\n"
        new = patch_content(content, tr, code + ".ts", nl)
        if new is None:
            continue
        if new == content:
            print("  %s already complete (no change)" % code)
            continue
        with open(path, "w", encoding="utf-8", newline="") as f:
            f.write(new)
        print("  patched %s (%s)" % (code, "CRLF" if nl == "\r\n" else "LF"))
        patched += 1
    print("Done. Patched %d files." % patched)
    return 0


if __name__ == "__main__":
    sys.exit(main())
