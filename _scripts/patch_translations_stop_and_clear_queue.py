"""One-shot patch for clear-queue-clears-learned-clock-shift (2026-07-11).

Adds TWO new source strings used by the "Clear render queue" action in
src/ui/RenderQueue.qml (RenderQueue context) when a render is in progress:

    "A render is in progress. Clearing the queue will stop the whole queue and interrupt the running job. Stop and clear?"
    "Stop and clear"

Both are inserted as fresh <message> blocks inside the RenderQueue context,
right after the existing "Are you sure you want to remove all items from the
render queue?" message (a stable, RenderQueue-context anchor). "Cancel" already
exists, so it is not patched.

No typographic arrow/dash characters are used in any string.

Run from repo root:
    python _scripts/patch_translations_stop_and_clear_queue.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op (keyed on the button
source already being present in the RenderQueue context).
"""
from __future__ import annotations

import pathlib
import sys

# New source strings.
SOURCE_LONG = (
    "A render is in progress. Clearing the queue will stop the whole queue "
    "and interrupt the running job. Stop and clear?"
)
SOURCE_BTN = "Stop and clear"

# The existing RenderQueue-context message we anchor after.
ANCHOR_SOURCE = "Are you sure you want to remove all items from the render queue?"

# Current source lines in src/ui/RenderQueue.qml (informational; lrelease
# matches by context + source, not by location).
LINE_LONG = 3105
LINE_BTN = 3106

# Per-language translations for the long confirmation sentence.
TRANS_LONG: dict[str, str] = {
    "cs": "Probíhá renderování. Vymazání fronty zastaví celou frontu a přeruší běžící úlohu. Zastavit a vymazat?",
    "da": "En rendering er i gang. Rydning af køen stopper hele køen og afbryder det igangværende job. Stop og ryd?",
    "de": "Ein Rendering läuft. Das Leeren der Warteschlange stoppt die gesamte Warteschlange und unterbricht den laufenden Auftrag. Stoppen und löschen?",
    "el": "Μια απόδοση βρίσκεται σε εξέλιξη. Η εκκαθάριση της ουράς θα σταματήσει ολόκληρη την ουρά και θα διακόψει την τρέχουσα εργασία. Διακοπή και εκκαθάριση;",
    "es": "Hay un procesamiento en curso. Vaciar la cola detendrá toda la cola e interrumpirá el trabajo en curso. ¿Detener y limpiar?",
    "fi": "Renderöinti on käynnissä. Jonon tyhjentäminen pysäyttää koko jonon ja keskeyttää käynnissä olevan työn. Pysäytä ja tyhjennä?",
    "fr": "Un rendu est en cours. Vider la file d'attente arrêtera toute la file et interrompra la tâche en cours. Arrêter et vider ?",
    "gl": "Hai un renderizado en curso. Limpar a cola deterá toda a cola e interromperá a tarefa en execución. Deter e limpar?",
    "id": "Render sedang berlangsung. Menghapus antrian akan menghentikan seluruh antrian dan menyela pekerjaan yang sedang berjalan. Hentikan dan hapus?",
    "it": "È in corso un rendering. Cancellare la coda fermerà l'intera coda e interromperà l'operazione in corso. Fermare e cancellare?",
    "ja": "レンダリング中です。キューをクリアするとキュー全体が停止し、実行中のジョブが中断されます。停止してクリアしますか？",
    "ko": "렌더링이 진행 중입니다. 대기열을 비우면 전체 대기열이 중지되고 실행 중인 작업이 중단됩니다. 중지하고 비울까요?",
    "no": "En gjengivelse pågår. Å tømme køen stopper hele køen og avbryter den pågående jobben. Stopp og slett?",
    "pl": "Trwa renderowanie. Wyczyszczenie kolejki zatrzyma całą kolejkę i przerwie bieżące zadanie. Zatrzymać i wyczyścić?",
    "pt": "Há uma renderização em curso. Limpar a fila irá parar toda a fila e interromper a tarefa em execução. Parar e limpar?",
    "pt_BR": "Há uma renderização em andamento. Limpar a fila irá parar toda a fila e interromper a tarefa em execução. Parar e limpar?",
    "ru": "Идёт рендеринг. Очистка очереди остановит всю очередь и прервёт текущую задачу. Остановить и очистить?",
    "sk": "Prebieha renderovanie. Vyčistenie fronty zastaví celú frontu a preruší bežiacu úlohu. Zastaviť a vyčistiť?",
    "tr": "Bir render devam ediyor. Sırayı temizlemek tüm sırayı durdurur ve çalışan işi keser. Durdurulup temizlensin mi?",
    "uk": "Триває рендеринг. Очищення черги зупинить усю чергу та перерве поточне завдання. Зупинити й очистити?",
    "zh_CN": "有渲染任务正在进行。清空队列会停止整个队列并中断正在渲染的任务。是否停止并清空？",
    "zh_TW": "有渲染任務正在進行。清除佇列會停止整個佇列並中斷正在渲染的任務。是否停止並清除？",
}

# Per-language translations for the "Stop and clear" button.
TRANS_BTN: dict[str, str] = {
    "cs": "Zastavit a vymazat",
    "da": "Stop og ryd",
    "de": "Stoppen und löschen",
    "el": "Διακοπή και εκκαθάριση",
    "es": "Detener y limpiar",
    "fi": "Pysäytä ja tyhjennä",
    "fr": "Arrêter et vider",
    "gl": "Deter e limpar",
    "id": "Hentikan dan hapus",
    "it": "Ferma e cancella",
    "ja": "停止してクリア",
    "ko": "중지하고 비우기",
    "no": "Stopp og slett",
    "pl": "Zatrzymaj i wyczyść",
    "pt": "Parar e limpar",
    "pt_BR": "Parar e limpar",
    "ru": "Остановить и очистить",
    "sk": "Zastaviť a vyčistiť",
    "tr": "Durdur ve temizle",
    "uk": "Зупинити й очистити",
    "zh_CN": "停止并清空",
    "zh_TW": "停止並清除",
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


def renderqueue_context_bounds(raw: str) -> tuple[int, int]:
    """Return (start, end) byte offsets of the <context> whose name is RenderQueue."""
    name_pos = raw.find("<name>RenderQueue</name>")
    if name_pos == -1:
        raise RuntimeError("RenderQueue context not found")
    ctx_start = raw.rfind("<context>", 0, name_pos)
    ctx_end = raw.find("</context>", name_pos)
    if ctx_start == -1 or ctx_end == -1:
        raise RuntimeError("RenderQueue <context> bounds not found")
    return ctx_start, ctx_end


def message_block(eol: str, line: int, source: str, trans: str | None) -> str:
    if trans is None:
        tr = '<translation type="unfinished"></translation>'
    else:
        tr = f"<translation>{xml_escape(trans)}</translation>"
    return (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/RenderQueue.qml" line="{line}"/>{eol}'
        f"        <source>{xml_escape(source)}</source>{eol}"
        f"        {tr}{eol}"
        f"    </message>{eol}"
    )


def patch_file(path: pathlib.Path, trans_long: str | None, trans_btn: str | None) -> bool:
    # trans_*=None -> source template (gyroflow.ts): emit unfinished translations.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    ctx_start, ctx_end = renderqueue_context_bounds(raw)

    # Already patched? (button source present inside the RenderQueue context)
    btn_tag = f"<source>{xml_escape(SOURCE_BTN)}</source>"
    if btn_tag in raw[ctx_start:ctx_end]:
        return False

    # Find the anchor message's closing </message> inside the RenderQueue context.
    anchor_tag = f"<source>{xml_escape(ANCHOR_SOURCE)}</source>"
    anchor_pos = raw.find(anchor_tag, ctx_start, ctx_end)
    if anchor_pos == -1:
        raise RuntimeError(f"anchor source not found in RenderQueue context of {path.name}")
    close_tag = f"</message>{eol}"
    close_pos = raw.find(close_tag, anchor_pos)
    if close_pos == -1 or close_pos > ctx_end:
        raise RuntimeError(f"anchor </message> not found in RenderQueue context of {path.name}")
    insert_at = close_pos + len(close_tag)

    block = (
        message_block(eol, LINE_LONG, SOURCE_LONG, trans_long)
        + message_block(eol, LINE_BTN, SOURCE_BTN, trans_btn)
    )

    raw = raw[:insert_at] + block + raw[insert_at:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    targets: list[tuple[str, str | None, str | None]] = [("gyroflow", None, None)]
    for lang in TRANS_LONG:
        targets.append((lang, TRANS_LONG[lang], TRANS_BTN[lang]))
    for lang, tl, tb in targets:
        path = root / f"{lang}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        try:
            patched = patch_file(path, tl, tb)
        except RuntimeError as e:
            print(f"{lang}: ERROR {e}")
            ok = False
            continue
        raw = path.read_bytes().decode("utf-8")
        status = "patched" if patched else "no-op"
        long_ok = f"<source>{xml_escape(SOURCE_LONG)}</source>" in raw
        btn_ok = f"<source>{xml_escape(SOURCE_BTN)}</source>" in raw
        if not (long_ok and btn_ok):
            print(f"{lang}: {status} MISSING SOURCE (long={long_ok} btn={btn_ok})")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
