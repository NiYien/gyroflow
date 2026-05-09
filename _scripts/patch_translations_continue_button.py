"""One-shot patch: inject "Continue" translation into App context of all 23 .ts files.

Run from repo root:
    python _scripts/patch_translations_continue_button.py

Then regenerate .qm bundles via lrelease (see patch_translations_batch_sync_prompts.py
docstring for the lrelease invocation).

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

LOC = ("App.qml", 749)

TRANS: dict[str, str] = {
    "cs": "Pokračovat",
    "da": "Fortsæt",
    "de": "Fortfahren",
    "el": "Συνέχεια",
    "es": "Continuar",
    "fi": "Jatka",
    "fr": "Continuer",
    "gl": "Continuar",
    "id": "Lanjutkan",
    "it": "Continua",
    "ja": "続行",
    "ko": "계속",
    "no": "Fortsett",
    "pl": "Kontynuuj",
    "pt": "Continuar",
    "pt_BR": "Continuar",
    "ru": "Продолжить",
    "sk": "Pokračovať",
    "tr": "Devam",
    "uk": "Продовжити",
    "zh_CN": "继续",
    "zh_TW": "繼續",
}


def make_message(translation: str | None) -> str:
    loc = f'        <location filename="../../src/ui/{LOC[0]}" line="{LOC[1]}"/>'
    if translation is None:
        trans_tag = '<translation type="unfinished"></translation>'
    else:
        trans_tag = f"<translation>{translation}</translation>"
    return (
        "    <message>\n"
        f"{loc}\n"
        "        <source>Continue</source>\n"
        f"        {trans_tag}\n"
        "    </message>\n"
    )


def patch_file(path: pathlib.Path, translation: str | None) -> str:
    content = path.read_text(encoding="utf-8")
    # Idempotency: an App-context Continue entry pointing at App.qml line 749 means we already ran.
    sentinel = '<location filename="../../src/ui/App.qml" line="749"/>\n        <source>Continue</source>'
    if sentinel in content:
        return f"SKIP {path.name} (already patched)"

    needle = "    <name>App</name>\n"
    if needle not in content:
        return f"FAIL {path.name}: <App> context not found"

    new_content = content.replace(needle, needle + make_message(translation), 1)
    path.write_text(new_content, encoding="utf-8")
    return f"OK   {path.name}"


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    if not base.is_dir():
        print(f"Translation dir missing: {base}", file=sys.stderr)
        return 1

    print(patch_file(base / "gyroflow.ts", None))

    for lang, tx in TRANS.items():
        path = base / f"{lang}.ts"
        if not path.is_file():
            print(f"MISS {path.name}")
            continue
        print(patch_file(path, tx))
    return 0


if __name__ == "__main__":
    sys.exit(main())
