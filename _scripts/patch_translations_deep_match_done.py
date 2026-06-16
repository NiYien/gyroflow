"""One-shot patch for simple-mode-default-match-then-sync task group 9
(deep-match success dialog convergence, 2026-06-16).

The deep-match success dialog dropped its second button. Distribution is no
longer triggered from the dialog: accepting a deep match only records an anchor
and marks the batch dirty, and the next main Export action matches first (which
distributes the recorded anchors) and then syncs/renders. Two consequences for
the RenderQueue translation context:

  1. The "Auto match now" button is gone, so its source string is no longer
     referenced by any qsTr() call. Its <message> block is removed from every
     .ts file (and the source template).

  2. The success-dialog body line changed meaning -- it no longer points at the
     removed "Auto match" button:
        OLD source: "Saved. Deep match more clips, or distribute now with Auto match."
        NEW source: "Saved. Deep match more clips, or Export to match and sync."
     The existing <message> block is rewritten in place (source + translation +
     location line) to the new meaning, so no stale obsolete entry is left and
     no language silently falls back to the old English text.

Both strings were originally added by
_scripts/patch_translations_deep_match_decouple_ui.py; this patch supersedes
that script's two entries.

Chinese (per the change design, no typographic arrow/dash characters):
    NEW source -> 已保存。可继续深度匹配更多片段，或点导出进行匹配并同步。 (zh_CN)
                  已儲存。可繼續深度匹配更多片段，或點匯出進行匹配並同步。 (zh_TW)

Run from repo root:
    python _scripts/patch_translations_deep_match_done.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

# The removed "Auto match now" button source (block deleted entirely).
AUTONOW_SOURCE = "Auto match now"

# The success-dialog body line: rewritten in place to the new meaning.
OLD_SAVED_SOURCE = "Saved. Deep match more clips, or distribute now with Auto match."
NEW_SAVED_SOURCE = "Saved. Deep match more clips, or Export to match and sync."

# Source line number in src/ui/RenderQueue.qml (informational only; lrelease
# matches by context + source, not by location).
LINE_SAVED = 453

# Per-language translation of the new "Saved..." body line. Meaning: the result
# is saved, you may keep deep-matching more clips, or click Export to match and
# sync. No typographic arrow/dash characters.
TRANS: dict[str, str] = {
    "cs": "Uloženo. Spárujte hluboce další klipy, nebo klikněte na Exportovat pro spárování a synchronizaci.",
    "da": "Gemt. Dybmatch flere klip, eller klik på Eksportér for at matche og synkronisere.",
    "de": "Gespeichert. Gleichen Sie weitere Clips tief ab, oder klicken Sie auf Exportieren, um abzugleichen und zu synchronisieren.",
    "el": "Αποθηκεύτηκε. Κάντε βαθιά αντιστοίχιση σε περισσότερα κλιπ ή κάντε κλικ στην Εξαγωγή για αντιστοίχιση και συγχρονισμό.",
    "es": "Guardado. Empareje en profundidad más clips, o haga clic en Exportar para emparejar y sincronizar.",
    "fi": "Tallennettu. Tee syväsovitus useammille leikkeille tai napsauta Vie, niin sovitus ja synkronointi tehdään.",
    "fr": "Enregistré. Faites une correspondance profonde sur d'autres clips, ou cliquez sur Exporter pour faire correspondre et synchroniser.",
    "gl": "Gardado. Empareza en profundidade máis clips, ou fai clic en Exportar para emparellar e sincronizar.",
    "id": "Tersimpan. Cocokkan mendalam klip lainnya, atau klik Ekspor untuk mencocokkan dan menyinkronkan.",
    "it": "Salvato. Esegui l'abbinamento profondo di altri clip, oppure fai clic su Esporta per abbinare e sincronizzare.",
    "ja": "保存しました。他のクリップをディープマッチするか、書き出しをクリックして一致と同期を行ってください。",
    "ko": "저장되었습니다. 다른 클립을 딥 매칭하거나, 내보내기를 클릭하여 매칭하고 동기화하세요.",
    "no": "Lagret. Dybmatch flere klipp, eller klikk Eksporter for å matche og synkronisere.",
    "pl": "Zapisano. Wykonaj głębokie dopasowanie kolejnych klipów lub kliknij Eksportuj, aby dopasować i zsynchronizować.",
    "pt": "Guardado. Faça a correspondência profunda de mais clipes, ou clique em Exportar para fazer a correspondência e sincronizar.",
    "pt_BR": "Salvo. Faça a correspondência profunda de mais clipes, ou clique em Exportar para fazer a correspondência e sincronizar.",
    "ru": "Сохранено. Выполните глубокое сопоставление других клипов или нажмите «Экспорт», чтобы сопоставить и синхронизировать.",
    "sk": "Uložené. Hlboko spárujte ďalšie klipy alebo kliknite na Exportovať na spárovanie a synchronizáciu.",
    "tr": "Kaydedildi. Daha fazla klibi derin eşleştirin veya eşleştirip senkronize etmek için Dışa aktar'a tıklayın.",
    "uk": "Збережено. Виконайте глибоке зіставлення інших кліпів або натисніть «Експорт», щоб зіставити та синхронізувати.",
    "zh_CN": "已保存。可继续深度匹配更多片段，或点导出进行匹配并同步。",
    "zh_TW": "已儲存。可繼續深度匹配更多片段，或點匯出進行匹配並同步。",
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


def find_message_block(raw: str, source: str, eol: str) -> tuple[int, int]:
    """Return (block_start, block_end) byte span of the <message> block whose
    <source> equals `source`. block_start is the start of the indented
    "<message>" line; block_end is just past "</message>\\n". Raises if absent.
    """
    src_tag = f"<source>{xml_escape(source)}</source>"
    src_pos = raw.find(src_tag)
    if src_pos == -1:
        raise RuntimeError(f"source not found: {source!r}")
    msg_open = raw.rfind("<message>", 0, src_pos)
    if msg_open == -1:
        raise RuntimeError(f"enclosing <message> not found for: {source!r}")
    line_start = raw.rfind("\n", 0, msg_open)
    block_start = 0 if line_start == -1 else line_start + 1
    close_tag = f"</message>{eol}"
    close_pos = raw.find(close_tag, src_pos)
    if close_pos == -1:
        raise RuntimeError(f"enclosing </message> not found for: {source!r}")
    return block_start, close_pos + len(close_tag)


def patch_file(path: pathlib.Path, trans: str | None) -> bool:
    # trans=None -> source template (gyroflow.ts): emit unfinished translation.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    # Already patched? (new source present and old strings absent)
    has_new = f"<source>{xml_escape(NEW_SAVED_SOURCE)}</source>" in raw
    has_old_saved = f"<source>{xml_escape(OLD_SAVED_SOURCE)}</source>" in raw
    has_autonow = f"<source>{xml_escape(AUTONOW_SOURCE)}</source>" in raw
    if has_new and not has_old_saved and not has_autonow:
        return False

    # 1. Remove the "Auto match now" message block (button is gone).
    if has_autonow:
        b_start, b_end = find_message_block(raw, AUTONOW_SOURCE, eol)
        raw = raw[:b_start] + raw[b_end:]

    # 2. Rewrite the old "Saved..." block in place to the new source.
    if has_old_saved:
        b_start, b_end = find_message_block(raw, OLD_SAVED_SOURCE, eol)
        if trans is None:
            translation = '<translation type="unfinished"></translation>'
        else:
            translation = f"<translation>{xml_escape(trans)}</translation>"
        new_block = (
            f"    <message>{eol}"
            f'        <location filename="../../src/ui/RenderQueue.qml" line="{LINE_SAVED}"/>{eol}'
            f"        <source>{xml_escape(NEW_SAVED_SOURCE)}</source>{eol}"
            f"        {translation}{eol}"
            f"    </message>{eol}"
        )
        raw = raw[:b_start] + new_block + raw[b_end:]

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
        problems = []
        if f"<source>{xml_escape(NEW_SAVED_SOURCE)}</source>" not in raw:
            problems.append("NEW_SAVED_MISSING")
        if f"<source>{xml_escape(OLD_SAVED_SOURCE)}</source>" in raw:
            problems.append("OLD_SAVED_PRESENT")
        if f"<source>{xml_escape(AUTONOW_SOURCE)}</source>" in raw:
            problems.append("AUTONOW_PRESENT")
        status = "patched" if patched else "no-op"
        if problems:
            print(f"{lang}: {status} PROBLEMS={problems}")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
