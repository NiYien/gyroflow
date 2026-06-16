"""One-shot patch: add translations for the 2 new deep-match decouple strings
(deep-match-decouple-from-auto-match, 2026-06-16) to all 22 language .ts files.

Deep-match acquisition is now decoupled from distribution: accepting a deep
match only records an anchor. The success dialog therefore replaced its single
"Ok" (which chained straight into Auto match) with two buttons:

  AUTONOW - "Auto match now"  (close + distribute all recorded anchors)
  SAVED   - the success-dialog body line telling the user the result is saved
            and they may keep deep-matching more clips or distribute now

The other new button, "Done", already exists in the RenderQueue context (it is
reused from the existing selection toolbar), so it needs no patching.

"Auto match now" reuses each language's existing translation of "Auto match"
plus a natural "now" qualifier; "Saved..." reuses the same Auto-match / deep-
match terminology.

Unlike lupdate-based flows, this inserts the <message> blocks directly after
the existing "Done" entry in the RenderQueue context, so the new entries land
in the right context without lupdate rewriting the whole file.

Run from repo root:
    python _scripts/patch_translations_deep_match_decouple_ui.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

# Anchor: the existing "Done" RenderQueue-context string (reused selection
# toolbar label). New entries are inserted immediately after this message block.
ANCHOR = "Done"

# Source line numbers in src/ui/RenderQueue.qml (informational only; lrelease
# matches by context + source, not by location).
LINES = {
    "AUTONOW": 436,
    "SAVED": 435,
}

SOURCES = {
    "AUTONOW": "Auto match now",
    "SAVED": "Saved. Deep match more clips, or distribute now with Auto match.",
}

# Per-language translations. "Auto match" terminology reuses each language's
# existing translation of "Auto match".
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "AUTONOW": "Automatická shoda nyní",
        "SAVED": "Uloženo. Spárujte hluboce další klipy nebo nyní rozdělte pomocí Automatické shody.",
    },
    "da": {
        "AUTONOW": "Auto match nu",
        "SAVED": "Gemt. Dybmatch flere klip, eller fordel nu med Auto match.",
    },
    "de": {
        "AUTONOW": "Jetzt automatisch abgleichen",
        "SAVED": "Gespeichert. Gleichen Sie weitere Clips tief ab oder verteilen Sie jetzt mit Automatischer Übereinstimmung.",
    },
    "el": {
        "AUTONOW": "Αυτόματη αντιστοίχιση τώρα",
        "SAVED": "Αποθηκεύτηκε. Κάντε βαθιά αντιστοίχιση σε περισσότερα κλιπ ή διανείμετε τώρα με Αυτόματη αντιστοίχιση.",
    },
    "es": {
        "AUTONOW": "Coincidencia automática ahora",
        "SAVED": "Guardado. Empareje en profundidad más clips o distribuya ahora con la coincidencia automática.",
    },
    "fi": {
        "AUTONOW": "Automaattinen ottelu nyt",
        "SAVED": "Tallennettu. Tee syväsovitus useammille leikkeille tai jaa nyt automaattisella ottelulla.",
    },
    "fr": {
        "AUTONOW": "Correspondance automatique maintenant",
        "SAVED": "Enregistré. Faites une correspondance profonde sur d'autres clips, ou distribuez maintenant avec la correspondance automatique.",
    },
    "gl": {
        "AUTONOW": "Coincidencia automática agora",
        "SAVED": "Gardado. Empareza en profundidade máis clips ou distribúe agora coa coincidencia automática.",
    },
    "id": {
        "AUTONOW": "Cocokkan otomatis sekarang",
        "SAVED": "Tersimpan. Cocokkan mendalam klip lainnya, atau distribusikan sekarang dengan Cocokkan otomatis.",
    },
    "it": {
        "AUTONOW": "Corrispondenza automatica ora",
        "SAVED": "Salvato. Esegui l'abbinamento profondo di altri clip oppure distribuisci ora con la Corrispondenza automatica.",
    },
    "ja": {
        "AUTONOW": "今すぐ自動一致",
        "SAVED": "保存しました。他のクリップをディープマッチするか、自動一致で今すぐ割り当ててください。",
    },
    "ko": {
        "AUTONOW": "지금 자동 매칭",
        "SAVED": "저장되었습니다. 다른 클립을 딥 매칭하거나 자동 매칭으로 지금 분배하세요.",
    },
    "no": {
        "AUTONOW": "Automatch nå",
        "SAVED": "Lagret. Dybmatch flere klipp, eller fordel nå med Automatch.",
    },
    "pl": {
        "AUTONOW": "Automatyczne dopasowanie teraz",
        "SAVED": "Zapisano. Wykonaj głębokie dopasowanie kolejnych klipów lub rozdziel teraz za pomocą Automatycznego dopasowania.",
    },
    "pt": {
        "AUTONOW": "Correspondência automática agora",
        "SAVED": "Guardado. Faça a correspondência profunda de mais clipes ou distribua agora com a Correspondência automática.",
    },
    "pt_BR": {
        "AUTONOW": "Correspondência automática agora",
        "SAVED": "Salvo. Faça a correspondência profunda de mais clipes ou distribua agora com a Correspondência automática.",
    },
    "ru": {
        "AUTONOW": "Автоматическое сопоставление сейчас",
        "SAVED": "Сохранено. Выполните глубокое сопоставление других клипов или распределите сейчас с помощью автоматического сопоставления.",
    },
    "sk": {
        "AUTONOW": "Automatické párovanie teraz",
        "SAVED": "Uložené. Hlboko spárujte ďalšie klipy alebo teraz rozdeľte pomocou Automatického párovania.",
    },
    "tr": {
        "AUTONOW": "Şimdi otomatik eşleştir",
        "SAVED": "Kaydedildi. Daha fazla klibi derin eşleştirin veya şimdi Otomatik eşleştirme ile dağıtın.",
    },
    "uk": {
        "AUTONOW": "Автоматичне зіставлення зараз",
        "SAVED": "Збережено. Виконайте глибоке зіставлення інших кліпів або розподіліть зараз за допомогою Автоматичного зіставлення.",
    },
    "zh_CN": {
        "AUTONOW": "立即自动匹配",
        "SAVED": "已保存。可继续深度匹配更多片段，或现在用自动匹配进行分配。",
    },
    "zh_TW": {
        "AUTONOW": "立即自動配對",
        "SAVED": "已儲存。可繼續深度匹配更多片段，或現在用自動配對進行分配。",
    },
}

KEY_ORDER = ["AUTONOW", "SAVED"]


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
    if f"<source>{xml_escape(SOURCES['AUTONOW'])}</source>" in raw:
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
        # Verify both entries now carry an entry.
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
