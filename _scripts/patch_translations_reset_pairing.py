"""One-shot patch for simple-mode-default-match-then-sync task group 12 (2026-06-16).

Adds two NEW source strings used by the "Reset pairing" reset-all control in
src/ui/App.qml (App context):

    1. "Reset pairing"
       Tooltip of the reset-all icon button placed left of "Clear render queue".
    2. "Are you sure you want to reset the status of all jobs?"
       Confirmation prompt shown when the button is clicked.

Both are inserted as fresh <message> blocks inside the App context, right after
the existing "Are you sure you want to remove all items from the render queue?"
message (the adjacent clearQueueBtn prompt), which is a stable App-context anchor.

Chinese is locked per the change design:
    "Reset pairing"                                      -> 重置配对
    "Are you sure you want to reset the status of all jobs?" -> 确定要重置所有任务的状态吗？

No typographic arrow/dash characters are used in any string.

Run from repo root:
    python _scripts/patch_translations_reset_pairing.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op (keyed on the new
"Reset pairing" source already being present in the App context).
"""
from __future__ import annotations

import pathlib
import sys

# New source strings.
TOOLTIP_SOURCE = "Reset pairing"
PROMPT_SOURCE = "Are you sure you want to reset the status of all jobs?"

# The existing App-context message we anchor after (clearQueueBtn confirm prompt).
ANCHOR_SOURCE = "Are you sure you want to remove all items from the render queue?"

# Current source line numbers in src/ui/App.qml (informational; lrelease matches
# by context + source, not by location).
LINE_TOOLTIP = 1217
LINE_PROMPT = 1218

# Per-language translations.
#   TOOLTIP -> "Reset pairing"
#   PROMPT  -> "Are you sure you want to reset the status of all jobs?"
TRANS: dict[str, dict[str, str]] = {
    "cs": {"TOOLTIP": "Obnovit párování", "PROMPT": "Opravdu chcete obnovit stav všech úloh?"},
    "da": {"TOOLTIP": "Nulstil parring", "PROMPT": "Er du sikker på, at du vil nulstille status for alle job?"},
    "de": {"TOOLTIP": "Zuordnung zurücksetzen", "PROMPT": "Möchten Sie den Status aller Aufträge wirklich zurücksetzen?"},
    "el": {"TOOLTIP": "Επαναφορά αντιστοίχισης", "PROMPT": "Είστε βέβαιοι ότι θέλετε να επαναφέρετε την κατάσταση όλων των εργασιών;"},
    "es": {"TOOLTIP": "Restablecer emparejamiento", "PROMPT": "¿Seguro que quieres restablecer el estado de todos los trabajos?"},
    "fi": {"TOOLTIP": "Nollaa parinmuodostus", "PROMPT": "Haluatko varmasti nollata kaikkien töiden tilan?"},
    "fr": {"TOOLTIP": "Réinitialiser l'association", "PROMPT": "Voulez-vous vraiment réinitialiser l'état de toutes les tâches ?"},
    "gl": {"TOOLTIP": "Restablecer emparellamento", "PROMPT": "Seguro que queres restablecer o estado de todos os traballos?"},
    "id": {"TOOLTIP": "Setel ulang pemasangan", "PROMPT": "Yakin ingin menyetel ulang status semua tugas?"},
    "it": {"TOOLTIP": "Reimposta abbinamento", "PROMPT": "Vuoi davvero reimpostare lo stato di tutti i lavori?"},
    "ja": {"TOOLTIP": "ペアリングをリセット", "PROMPT": "すべてのジョブの状態をリセットしてもよろしいですか？"},
    "ko": {"TOOLTIP": "페어링 재설정", "PROMPT": "모든 작업의 상태를 재설정하시겠습니까?"},
    "no": {"TOOLTIP": "Tilbakestill paring", "PROMPT": "Er du sikker på at du vil tilbakestille statusen for alle jobber?"},
    "pl": {"TOOLTIP": "Resetuj parowanie", "PROMPT": "Czy na pewno chcesz zresetować stan wszystkich zadań?"},
    "pt": {"TOOLTIP": "Repor emparelhamento", "PROMPT": "Tem a certeza de que pretende repor o estado de todas as tarefas?"},
    "pt_BR": {"TOOLTIP": "Redefinir pareamento", "PROMPT": "Tem certeza de que deseja redefinir o status de todos os trabalhos?"},
    "ru": {"TOOLTIP": "Сбросить сопоставление", "PROMPT": "Вы уверены, что хотите сбросить статус всех заданий?"},
    "sk": {"TOOLTIP": "Obnoviť párovanie", "PROMPT": "Naozaj chcete obnoviť stav všetkých úloh?"},
    "tr": {"TOOLTIP": "Eşleştirmeyi sıfırla", "PROMPT": "Tüm işlerin durumunu sıfırlamak istediğinizden emin misiniz?"},
    "uk": {"TOOLTIP": "Скинути зіставлення", "PROMPT": "Ви впевнені, що хочете скинути стан усіх завдань?"},
    "zh_CN": {"TOOLTIP": "重置配对", "PROMPT": "确定要重置所有任务的状态吗？"},
    "zh_TW": {"TOOLTIP": "重設配對", "PROMPT": "確定要重設所有工作的狀態嗎？"},
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


def app_context_bounds(raw: str) -> tuple[int, int]:
    """Return (start, end) byte offsets of the <context> whose <name>App</name> is."""
    name_pos = raw.find("<name>App</name>")
    if name_pos == -1:
        raise RuntimeError("App context not found")
    ctx_start = raw.rfind("<context>", 0, name_pos)
    ctx_end = raw.find("</context>", name_pos)
    if ctx_start == -1 or ctx_end == -1:
        raise RuntimeError("App <context> bounds not found")
    return ctx_start, ctx_end


def patch_file(path: pathlib.Path, trans: dict[str, str] | None) -> bool:
    # trans=None -> source template (gyroflow.ts): emit unfinished translations.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    ctx_start, ctx_end = app_context_bounds(raw)

    # Already patched? (new tooltip source present inside the App context)
    new_tag = f"<source>{xml_escape(TOOLTIP_SOURCE)}</source>"
    if new_tag in raw[ctx_start:ctx_end]:
        return False

    # Find the anchor message's closing </message> inside the App context.
    anchor_tag = f"<source>{xml_escape(ANCHOR_SOURCE)}</source>"
    anchor_pos = raw.find(anchor_tag, ctx_start, ctx_end)
    if anchor_pos == -1:
        raise RuntimeError(f"anchor source not found in App context of {path.name}")
    close_tag = f"</message>{eol}"
    close_pos = raw.find(close_tag, anchor_pos)
    if close_pos == -1 or close_pos > ctx_end:
        raise RuntimeError(f"anchor </message> not found in App context of {path.name}")
    insert_at = close_pos + len(close_tag)

    if trans is None:
        tooltip_tr = '<translation type="unfinished"></translation>'
        prompt_tr = '<translation type="unfinished"></translation>'
    else:
        tooltip_tr = f"<translation>{xml_escape(trans['TOOLTIP'])}</translation>"
        prompt_tr = f"<translation>{xml_escape(trans['PROMPT'])}</translation>"

    tooltip_block = (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/App.qml" line="{LINE_TOOLTIP}"/>{eol}'
        f"        <source>{xml_escape(TOOLTIP_SOURCE)}</source>{eol}"
        f"        {tooltip_tr}{eol}"
        f"    </message>{eol}"
    )
    prompt_block = (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/App.qml" line="{LINE_PROMPT}"/>{eol}'
        f"        <source>{xml_escape(PROMPT_SOURCE)}</source>{eol}"
        f"        {prompt_tr}{eol}"
        f"    </message>{eol}"
    )

    raw = raw[:insert_at] + tooltip_block + prompt_block + raw[insert_at:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
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
        raw = path.read_bytes().decode("utf-8")
        missing = []
        if f"<source>{xml_escape(TOOLTIP_SOURCE)}</source>" not in raw:
            missing.append("TOOLTIP")
        if f"<source>{xml_escape(PROMPT_SOURCE)}</source>" not in raw:
            missing.append("PROMPT")
        status = "patched" if patched else "no-op"
        if missing:
            print(f"{lang}: {status} MISSING={missing}")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
