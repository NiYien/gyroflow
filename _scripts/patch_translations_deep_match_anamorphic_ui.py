"""One-shot patch: add translations for the 2 new deep-match anamorphic
lens-choice strings (change deep-match-anamorphic-lens-choice) to all 22
language .ts files.

The lens-choice pre-flight gate gained an "anamorphic" reason: with manual
edit ON and a resolvable anamorphic group configured, the dialog now asks
which lens group the video was shot with even when the video carries a
telemetry focal length. Two new strings:

  ANAQ  - anamorphic-mode dialog question
  SPHER - "spherical lens" escape entry (starts the probe with no injection)

Like patch_translations_deep_match_refusal_ui.py, this inserts the <message>
blocks directly after the existing lens-choice question in the RenderQueue
context, so the new entries land in the right context without lupdate
rewriting the whole file.

Run from repo root:
    python _scripts/patch_translations_deep_match_anamorphic_ui.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

# Anchor: the existing bare-mode lens-choice question (RenderQueue context).
# New entries are inserted immediately after this message block.
ANCHOR = (
    "Which lens group was this video shot with? "
    "(The correct lens group makes deep match much more accurate.)"
)

# Source line numbers in src/ui/RenderQueue.qml (informational only; lrelease
# matches by context + source, not by location).
LINES = {
    "ANAQ": 432,
    "SPHER": 462,
}

SOURCES = {
    "ANAQ": (
        "Was this video shot with an anamorphic lens? Pick its lens group, "
        "or pick spherical to continue. (The correct choice makes deep match "
        "much more accurate.)"
    ),
    "SPHER": "Spherical lens (not anamorphic)",
}

# Per-language translations. "Anamorphic lens" / "deep match" terminology
# reuses each language's existing translations of those strings.
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "ANAQ": (
            "Bylo toto video natočeno anamorfním objektivem? Vyberte jeho "
            "skupinu objektivů, nebo pokračujte volbou sférický. (Správná "
            "volba výrazně zpřesní hluboké párování.)"
        ),
        "SPHER": "Sférický objektiv (ne anamorfní)",
    },
    "da": {
        "ANAQ": (
            "Blev denne video optaget med et anamorf-objektiv? Vælg dets "
            "objektivgruppe, eller vælg sfærisk for at fortsætte. (Det "
            "rigtige valg gør dyb matchning meget mere præcis.)"
        ),
        "SPHER": "Sfærisk objektiv (ikke anamorft)",
    },
    "de": {
        "ANAQ": (
            "Wurde dieses Video mit einem anamorphen Objektiv aufgenommen? "
            "Wählen Sie dessen Objektivgruppe oder sphärisch, um "
            "fortzufahren. (Die richtige Wahl macht den Tiefenabgleich "
            "deutlich genauer.)"
        ),
        "SPHER": "Sphärisches Objektiv (nicht anamorph)",
    },
    "el": {
        "ANAQ": (
            "Γυρίστηκε αυτό το βίντεο με αναμορφικό φακό; Επιλέξτε την "
            "ομάδα φακών του, ή σφαιρικό για να συνεχίσετε. (Η σωστή "
            "επιλογή κάνει τη βαθιά αντιστοίχιση πολύ πιο ακριβή.)"
        ),
        "SPHER": "Σφαιρικός φακός (όχι αναμορφικός)",
    },
    "es": {
        "ANAQ": (
            "¿Este video se grabó con un objetivo anamórfico? Elija su "
            "grupo de objetivos, o esférico para continuar. (La elección "
            "correcta hace que el emparejamiento profundo sea mucho más "
            "preciso.)"
        ),
        "SPHER": "Objetivo esférico (no anamórfico)",
    },
    "fi": {
        "ANAQ": (
            "Kuvattiinko tämä video anamorfisella objektiivilla? Valitse "
            "sen objektiiviryhmä tai jatka valitsemalla sfäärinen. (Oikea "
            "valinta tekee syväsovituksesta paljon tarkemman.)"
        ),
        "SPHER": "Sfäärinen objektiivi (ei anamorfinen)",
    },
    "fr": {
        "ANAQ": (
            "Cette vidéo a-t-elle été filmée avec un objectif anamorphique ? "
            "Choisissez son groupe d'objectifs, ou sphérique pour continuer. "
            "(Le bon choix rend l'appariement profond beaucoup plus précis.)"
        ),
        "SPHER": "Objectif sphérique (non anamorphique)",
    },
    "gl": {
        "ANAQ": (
            "Este vídeo gravouse cun obxectivo anamórfico? Escolla o seu "
            "grupo de obxectivos, ou esférico para continuar. (A escolla "
            "correcta fai que o emparellamento profundo sexa moito máis "
            "preciso.)"
        ),
        "SPHER": "Obxectivo esférico (non anamórfico)",
    },
    "id": {
        "ANAQ": (
            "Apakah video ini direkam dengan lensa anamorfik? Pilih grup "
            "lensanya, atau pilih sferis untuk melanjutkan. (Pilihan yang "
            "tepat membuat pencocokan mendalam jauh lebih akurat.)"
        ),
        "SPHER": "Lensa sferis (bukan anamorfik)",
    },
    "it": {
        "ANAQ": (
            "Questo video è stato girato con un obiettivo anamorfico? "
            "Scegli il suo gruppo di obiettivi, oppure sferico per "
            "continuare. (La scelta corretta rende l'abbinamento profondo "
            "molto più preciso.)"
        ),
        "SPHER": "Obiettivo sferico (non anamorfico)",
    },
    "ja": {
        "ANAQ": (
            "この動画はアナモルフィックレンズで撮影されましたか？対応するレンズグループを"
            "選ぶか、球面レンズを選んで続行してください。（正しい選択でディープマッチングの"
            "精度が大幅に向上します。）"
        ),
        "SPHER": "球面レンズ（アナモルフィックではない）",
    },
    "ko": {
        "ANAQ": (
            "이 영상은 아나모픽 렌즈로 촬영되었나요? 해당 렌즈 그룹을 선택하거나 "
            "구면 렌즈를 선택해 계속하세요. (올바른 선택은 딥 매칭 정확도를 크게 "
            "향상시킵니다.)"
        ),
        "SPHER": "구면 렌즈 (아나모픽 아님)",
    },
    "no": {
        "ANAQ": (
            "Ble denne videoen filmet med et anamorf-objektiv? Velg "
            "objektivgruppen dens, eller velg sfærisk for å fortsette. "
            "(Riktig valg gjør dyp matching mye mer nøyaktig.)"
        ),
        "SPHER": "Sfærisk objektiv (ikke anamorft)",
    },
    "pl": {
        "ANAQ": (
            "Czy to wideo nakręcono obiektywem anamorficznym? Wybierz jego "
            "grupę obiektywów lub sferyczny, aby kontynuować. (Właściwy "
            "wybór znacznie zwiększa dokładność głębokiego dopasowania.)"
        ),
        "SPHER": "Obiektyw sferyczny (nieanamorficzny)",
    },
    "pt": {
        "ANAQ": (
            "Este vídeo foi gravado com uma objetiva anamórfica? Escolha o "
            "seu grupo de objetivas, ou esférica para continuar. (A escolha "
            "correta torna a correspondência profunda muito mais precisa.)"
        ),
        "SPHER": "Objetiva esférica (não anamórfica)",
    },
    "pt_BR": {
        "ANAQ": (
            "Este vídeo foi gravado com uma lente anamórfica? Escolha o "
            "grupo de lentes dela, ou esférica para continuar. (A escolha "
            "correta torna a correspondência profunda muito mais precisa.)"
        ),
        "SPHER": "Lente esférica (não anamórfica)",
    },
    "ru": {
        "ANAQ": (
            "Это видео снято анаморфным объективом? Выберите его группу "
            "объективов или сферический, чтобы продолжить. (Правильный "
            "выбор делает глубокое сопоставление намного точнее.)"
        ),
        "SPHER": "Сферический объектив (не анаморфный)",
    },
    "sk": {
        "ANAQ": (
            "Bolo toto video natočené anamorfným objektívom? Vyberte jeho "
            "skupinu objektívov, alebo pokračujte voľbou sférický. (Správna "
            "voľba výrazne spresní hlboké párovanie.)"
        ),
        "SPHER": "Sférický objektív (nie anamorfný)",
    },
    "tr": {
        "ANAQ": (
            "Bu video anamorfik bir lensle mi çekildi? Lens grubunu seçin "
            "veya devam etmek için küresel'i seçin. (Doğru seçim derin "
            "eşleştirmeyi çok daha doğru hale getirir.)"
        ),
        "SPHER": "Küresel lens (anamorfik değil)",
    },
    "uk": {
        "ANAQ": (
            "Це відео знято анаморфним об'єктивом? Виберіть його групу "
            "об'єктивів або сферичний, щоб продовжити. (Правильний вибір "
            "робить глибоке зіставлення набагато точнішим.)"
        ),
        "SPHER": "Сферичний об'єктив (не анаморфний)",
    },
    "zh_CN": {
        "ANAQ": "这个视频是用变形镜头拍摄的吗？请选择对应的镜头组，或选择球面镜头继续。（正确的选择能显著提高深度匹配的准确度。）",
        "SPHER": "球面镜头（非变形）",
    },
    "zh_TW": {
        "ANAQ": "這個影片是用變形鏡頭拍攝的嗎？請選擇對應的鏡頭群組，或選擇球面鏡頭繼續。（正確的選擇能顯著提高深度匹配的準確度。）",
        "SPHER": "球面鏡頭（非變形）",
    },
}

KEY_ORDER = ["ANAQ", "SPHER"]


def xml_escape(text: str) -> str:
    # Match lupdate's escaping: &, <, > always; " and ' as entities.
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
        .replace("'", "&apos;")
    )


def patch_file(path: pathlib.Path, trans: dict[str, str] | None) -> bool:
    # trans=None -> source template (gyroflow.ts): emit unfinished translations.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    # Already patched? (first new source present)
    if f"<source>{xml_escape(SOURCES['ANAQ'])}</source>" in raw:
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

    blocks = ""
    for key in KEY_ORDER:
        if trans is None:
            translation = '<translation type="unfinished"></translation>'
        else:
            translation = f"<translation>{xml_escape(trans[key])}</translation>"
        blocks += (
            f"    <message>{eol}"
            f'        <location filename="../../src/ui/RenderQueue.qml" line="{LINES[key]}"/>{eol}'
            f"        <source>{xml_escape(SOURCES[key])}</source>{eol}"
            f"        {translation}{eol}"
            f"    </message>{eol}"
        )

    raw = raw[:insert_pos] + blocks + raw[insert_pos:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    # None -> source template gets unfinished entries (English fallback at runtime).
    targets: list[tuple[str, dict[str, str] | None]] = [("gyroflow", None)]
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
        # Verify both entries are present after patching.
        raw = path.read_bytes().decode("utf-8")
        missing = [
            k for k in KEY_ORDER
            if f"<source>{xml_escape(SOURCES[k])}</source>" not in raw
        ]
        status = "patched" if patched else "no-op"
        if missing:
            print(f"{lang}: {status} MISSING={missing}")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
