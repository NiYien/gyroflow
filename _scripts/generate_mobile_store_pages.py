# SPDX-License-Identifier: GPL-3.0-or-later
"""Render public mobile documents from the same text bundled in the app."""
import argparse
import html
import json
from pathlib import Path
import re
import shutil

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "_deployment/ios/store"


def rich_text(text):
    escaped = html.escape(text)
    escaped = re.sub(r"https://[A-Za-z0-9./_-]+", lambda m: '<a href="' + m[0].rstrip('.') + '">' + m[0].rstrip('.') + '</a>' + ('.' if m[0].endswith('.') else ''), escaped)
    return escaped.replace("support@niyien.com", '<a href="mailto:support@niyien.com">support@niyien.com</a>')


def render(kind, data):
    titles = {"privacy": "Privacy Policy · 隐私政策", "help": "Help & Support · 帮助与支持"}
    sections = []
    for language, lang in [("zh", "zh-Hans"), ("en", "en")]:
        blocks = data[language][kind].strip().split("\n\n")
        heading = blocks.pop(0).splitlines()
        lines = ['<section id="' + language + '" lang="' + lang + '">', '<h1>' + html.escape(heading[0]) + '</h1>']
        if len(heading) > 1:
            lines.append('<p class="date">' + rich_text(' '.join(heading[1:])) + '</p>')
        for block in blocks:
            parts = block.split("\n", 1)
            if len(parts) == 2:
                lines.append('<h2>' + html.escape(parts[0]) + '</h2>\n<p>' + rich_text(parts[1]) + '</p>')
            else:
                lines.append('<p>' + rich_text(parts[0]) + '</p>')
        lines.append('</section>')
        sections.append('\n'.join(lines))
    return '''<!doctype html>
<html lang="zh-Hans">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light">
<title>NiYien · ''' + html.escape(titles[kind]) + '''</title>
<link rel="icon" href="niyien-icon.png">
<style>
*{box-sizing:border-box}body{margin:0;background:#f6f7fa;color:#1c2533;font:17px/1.8 -apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif}
a{color:#075da8;text-underline-offset:3px}a:focus-visible{outline:3px solid #0875cb;outline-offset:4px}
header,main,footer{max-width:900px;margin:auto;padding:24px 28px}header{display:flex;align-items:center;justify-content:space-between;gap:20px;flex-wrap:wrap}
.brand{display:flex;align-items:center;gap:12px;color:#16283c;text-decoration:none;font-size:23px;font-weight:700}.brand img{width:44px;height:44px;border-radius:10px}
nav{display:flex;gap:22px;flex-wrap:wrap;font-size:15px}main{background:white;border:1px solid #e4e8ee;border-radius:18px;padding:8px 42px 36px}
section{scroll-margin-top:24px;padding:26px 0}section+section{border-top:1px solid #dfe5ec;margin-top:18px}
h1{font-size:30px;line-height:1.35;letter-spacing:-.5px;margin:12px 0 20px}h2{font-size:20px;line-height:1.45;margin:30px 0 8px}p{margin:8px 0;overflow-wrap:anywhere}.date,footer{color:#526170;font-size:14px}footer{padding-bottom:40px}
@media(max-width:600px){header,footer{padding:20px}main{margin:0 12px;padding:6px 22px 24px;border-radius:14px}h1{font-size:26px}body{font-size:16px}nav{gap:16px}}
</style>
</head>
<body>
<header><a class="brand" href="https://www.niyien.com/"><img src="niyien-icon.png" alt="">NiYien</a>
<nav aria-label="Language"><a href="#zh">简体中文</a><a href="#en" lang="en">English</a></nav></header>
<main>
''' + '\n'.join(sections) + '''
</main>
<footer><nav aria-label="Support"><a href="privacy.html">隐私政策 / Privacy</a><a href="help.html">帮助与支持 / Support</a><a href="mailto:support@niyien.com">support@niyien.com</a><a href="https://www.niyien.com/v2/zh/download.html">官网下载</a></nav></footer>
</body>
</html>
'''


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--website-root", type=Path, help="Optional local checkout of the NiYien/docs website")
    args = parser.parse_args()
    data = json.loads((ROOT / "resources/legal/mobile-documents.json").read_text(encoding="utf-8"))
    OUTPUT.mkdir(parents=True, exist_ok=True)
    for kind in ["privacy", "help"]:
        (OUTPUT / (kind + ".html")).write_text(render(kind, data), encoding="utf-8")
    shutil.copyfile(ROOT / "_deployment/ios/NiYienIcon.png", OUTPUT / "niyien-icon.png")
    if args.website_root:
        if not (args.website_root / "v2/shared/site.js").is_file():
            parser.error("Expected the production NiYien/docs website checkout")
        dest = args.website_root / "mobile"
        dest.mkdir(exist_ok=True)
        for name in ["privacy.html", "help.html", "niyien-icon.png"]:
            shutil.copyfile(OUTPUT / name, dest / name)
    print("Rendered bilingual privacy and support pages from the in-app documents.")


if __name__ == "__main__":
    main()
