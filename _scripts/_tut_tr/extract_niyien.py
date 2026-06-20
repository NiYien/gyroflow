# Extract niyien strings that are finished in zh_CN but unfinished/absent in ALL
# sampled languages -> the set that needs translating. Proper XML parsing so
# quotes / comments / special chars are handled exactly. Writes niyien_strings.json.
import os, json
import xml.etree.ElementTree as ET

TRANS = os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "..", "resources", "translations"))

def parse(code):
    root = ET.parse(os.path.join(TRANS, code + ".ts")).getroot()
    msgs = {}
    for ctx in root.findall("context"):
        name = ctx.findtext("name")
        for m in ctx.findall("message"):
            src = m.findtext("source")
            comment = m.findtext("comment")  # disambiguation, part of the key
            tr_el = m.find("translation")
            tr = tr_el.text if tr_el is not None else None
            typ = tr_el.get("type") if tr_el is not None else None
            finished = (tr is not None and tr.strip() != "" and typ != "unfinished")
            msgs[(name, src, comment)] = (finished, tr)
    return msgs

zh = parse("zh_CN")
others = {L: parse(L) for L in ["de", "fr", "ru", "ja", "es", "pl"]}

items = []
for (ctx, src, comment), (fin, tr) in zh.items():
    if not fin:
        continue
    if src and "--REPLACE_WITH_NATIVE" in src:
        continue
    # niyien-added = finished in zh, NOT finished in any sampled other language
    if all(not others[L].get((ctx, src, comment), (False, None))[0] for L in others):
        items.append({"context": ctx, "source": src, "comment": comment, "zh": tr})

# stable order: by context then source
items.sort(key=lambda d: (d["context"], d["source"]))
for i, d in enumerate(items, 1):
    d["idx"] = i

json.dump(items, open(os.path.join(os.path.dirname(__file__), "niyien_strings.json"), "w", encoding="utf-8"),
          ensure_ascii=False, indent=2)

print("total:", len(items))
from collections import Counter
for ctx, n in Counter(d["context"] for d in items).most_common():
    print("  %-18s %d" % (ctx, n))
print("--- with <comment> (need special handling) ---")
for d in items:
    if d["comment"]:
        print("  idx", d["idx"], repr(d["source"]), "comment=", repr(d["comment"]))
