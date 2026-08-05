"""One-shot patch for plugin-only-skip-revive-and-scope (2026-08-05).

Two things change in the translation catalogues:

  1. The queue's aggregated skip notice was two sentences naming four formats
     and re-explaining the plugin workflow. Live review found it far too wordy
     (the whole popup was ~60 CJK characters for one instruction), and the row
     badges already say which clips were skipped and why. It collapses to one
     sentence carrying only the count and the next action, phrased like the
     existing single-video notice ("This format cannot be exported directly.
     Use ... instead."):
        NEW: "%1 video(s) cannot be exported directly. Use \"%2\" instead."
     Its old two halves are removed outright.

  2. Five new strings in the App context back the "never return without
     feedback" guarantee: when neither Simple-mode button can start work, it now
     says why instead of doing nothing. Reasons are picked by
     RenderQueue::dispatch_blocker_reason.

No typographic arrow/dash characters in any string (house rule).

Run from repo root:
    python _scripts/patch_translations_queue_stuck_recovery.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent, and convergent from either the pristine catalogues or the earlier
intermediate revision of this patch (which had widened the format list in place
before the copy was cut down).
"""
from __future__ import annotations

import pathlib
import sys

# Every source string this patch retires. Both the pristine wording and the
# intermediate one are listed so a half-patched catalogue converges.
OBSOLETE_SOURCES = [
    "%1 video(s) (R3D / N-RAW) cannot be exported directly and were skipped.",
    "%1 video(s) (R3D / N-RAW / BRAW / DNG) cannot be exported directly and were skipped.",
    "Press \"%1\" instead, then finish these clips in your video editor with the Gyroflow plugin.",
    "These videos (R3D / N-RAW / BRAW / DNG) cannot be exported directly.",
]

# (qml file, source line, source string, per-language translations)
NEW_STRINGS: list[tuple[str, str, int, str, dict[str, str]]] = [
    (
        "RenderQueue",
        "RenderQueue.qml",
        96,
        '%1 video(s) cannot be exported directly. Use "%2" instead.',
        {
            "cs": 'Přímý export %1 videí není možný. Použijte místo toho "%2".',
            "da": '%1 video(er) kan ikke eksporteres direkte. Brug "%2" i stedet.',
            "de": '%1 Video(s) können nicht direkt exportiert werden. Verwenden Sie stattdessen "%2".',
            "el": '%1 βίντεο δεν μπορούν να εξαχθούν απευθείας. Χρησιμοποιήστε "%2".',
            "es": '%1 video(s) no se pueden exportar directamente. Usa "%2" en su lugar.',
            "fi": '%1 videota ei voi viedä suoraan. Käytä sen sijaan "%2".',
            "fr": '%1 vidéo(s) ne peuvent pas être exportées directement. Utilisez "%2" à la place.',
            "gl": '%1 vídeo(s) non se poden exportar directamente. Usa "%2" no seu lugar.',
            "id": '%1 video tidak dapat diekspor langsung. Gunakan "%2" sebagai gantinya.',
            "it": '%1 video non possono essere esportati direttamente. Usa "%2" al suo posto.',
            "ja": "%1 本の動画は直接書き出せません。代わりに「%2」を使用してください。",
            "ko": '%1개의 영상은 직접 내보낼 수 없습니다. 대신 "%2"을(를) 사용하세요.',
            "no": '%1 video(er) kan ikke eksporteres direkte. Bruk "%2" i stedet.',
            "pl": 'Nie można bezpośrednio wyeksportować %1 filmów. Użyj zamiast tego "%2".',
            "pt": '%1 vídeo(s) não podem ser exportados diretamente. Use "%2" em vez disso.',
            "pt_BR": '%1 vídeo(s) não podem ser exportados diretamente. Use "%2" em vez disso.',
            "ru": "%1 видео нельзя экспортировать напрямую. Используйте «%2».",
            "sk": 'Priamy export %1 videí nie je možný. Použite namiesto toho "%2".',
            "tr": '%1 video doğrudan dışa aktarılamaz. Bunun yerine "%2" kullanın.',
            "uk": "%1 відео не можна експортувати безпосередньо. Скористайтеся «%2».",
            "zh_CN": '%1 个视频不支持直接导出。请改用"%2"。',
            "zh_TW": '%1 個影片不支援直接匯出。請改用"%2"。',
        },
    ),
    (
        "App",
        "App.qml",
        2151,
        'These videos cannot be exported directly. Use "%1" instead.',
        {
            "cs": 'Tato videa nelze exportovat přímo. Použijte místo toho "%1".',
            "da": 'Disse videoer kan ikke eksporteres direkte. Brug "%1" i stedet.',
            "de": 'Diese Videos können nicht direkt exportiert werden. Verwenden Sie stattdessen "%1".',
            "el": 'Αυτά τα βίντεο δεν μπορούν να εξαχθούν απευθείας. Χρησιμοποιήστε "%1".',
            "es": 'Estos videos no se pueden exportar directamente. Usa "%1" en su lugar.',
            "fi": 'Näitä videoita ei voi viedä suoraan. Käytä sen sijaan "%1".',
            "fr": 'Ces vidéos ne peuvent pas être exportées directement. Utilisez "%1" à la place.',
            "gl": 'Estes vídeos non se poden exportar directamente. Usa "%1" no seu lugar.',
            "id": 'Video ini tidak dapat diekspor langsung. Gunakan "%1" sebagai gantinya.',
            "it": 'Questi video non possono essere esportati direttamente. Usa "%1" al suo posto.',
            "ja": "これらの動画は直接書き出せません。代わりに「%1」を使用してください。",
            "ko": '이 영상들은 직접 내보낼 수 없습니다. 대신 "%1"을(를) 사용하세요.',
            "no": 'Disse videoene kan ikke eksporteres direkte. Bruk "%1" i stedet.',
            "pl": 'Tych filmów nie można wyeksportować bezpośrednio. Użyj zamiast tego "%1".',
            "pt": 'Estes vídeos não podem ser exportados diretamente. Use "%1" em vez disso.',
            "pt_BR": 'Estes vídeos não podem ser exportados diretamente. Use "%1" em vez disso.',
            "ru": "Эти видео нельзя экспортировать напрямую. Используйте «%1».",
            "sk": 'Tieto videá nie je možné exportovať priamo. Použite namiesto toho "%1".',
            "tr": 'Bu videolar doğrudan dışa aktarılamaz. Bunun yerine "%1" kullanın.',
            "uk": "Ці відео не можна експортувати безпосередньо. Скористайтеся «%1».",
            "zh_CN": '这些视频不支持直接导出。请改用"%1"。',
            "zh_TW": '這些影片不支援直接匯出。請改用"%1"。',
        },
    ),
    (
        "App",
        "App.qml",
        2154,
        "None of the videos in the queue have gyro data.",
        {
            "cs": "Žádné z videí ve frontě nemá data gyroskopu.",
            "da": "Ingen af videoerne i køen har gyrodata.",
            "de": "Keines der Videos in der Warteschlange hat Gyro-Daten.",
            "el": "Κανένα από τα βίντεο στην ουρά δεν έχει δεδομένα γυροσκοπίου.",
            "es": "Ninguno de los videos de la cola tiene datos de giroscopio.",
            "fi": "Yhdelläkään jonon videolla ei ole gyrodataa.",
            "fr": "Aucune des vidéos de la file d'attente n'a de données gyroscopiques.",
            "gl": "Ningún dos vídeos da cola ten datos de xiroscopio.",
            "id": "Tidak ada video dalam antrean yang memiliki data giroskop.",
            "it": "Nessuno dei video in coda ha dati giroscopici.",
            "ja": "キュー内のどの動画にもジャイロデータがありません。",
            "ko": "대기열의 영상 중 자이로 데이터가 있는 것이 없습니다.",
            "no": "Ingen av videoene i køen har gyrodata.",
            "pl": "Żaden z filmów w kolejce nie ma danych żyroskopu.",
            "pt": "Nenhum dos vídeos na fila tem dados de giroscópio.",
            "pt_BR": "Nenhum dos vídeos na fila tem dados de giroscópio.",
            "ru": "Ни у одного видео в очереди нет данных гироскопа.",
            "sk": "Žiadne z videí vo fronte nemá údaje gyroskopu.",
            "tr": "Kuyruktaki videoların hiçbirinde jiroskop verisi yok.",
            "uk": "Жодне з відео в черзі не має даних гіроскопа.",
            "zh_CN": "队列中的视频都没有陀螺仪数据。",
            "zh_TW": "佇列中的影片都沒有陀螺儀資料。",
        },
    ),
    (
        "App",
        "App.qml",
        2157,
        "The queue only contains calibration videos, which are not processed on their own.",
        {
            "cs": "Fronta obsahuje pouze kalibrační videa, která se samostatně nezpracovávají.",
            "da": "Køen indeholder kun kalibreringsvideoer, som ikke behandles alene.",
            "de": "Die Warteschlange enthält nur Kalibrierungsvideos, die nicht eigenständig verarbeitet werden.",
            "el": "Η ουρά περιέχει μόνο βίντεο βαθμονόμησης, τα οποία δεν επεξεργάζονται από μόνα τους.",
            "es": "La cola solo contiene videos de calibración, que no se procesan por sí solos.",
            "fi": "Jonossa on vain kalibrointivideoita, joita ei käsitellä yksinään.",
            "fr": "La file d'attente ne contient que des vidéos d'étalonnage, qui ne sont pas traitées seules.",
            "gl": "A cola só contén vídeos de calibración, que non se procesan por si sós.",
            "id": "Antrean hanya berisi video kalibrasi, yang tidak diproses sendiri.",
            "it": "La coda contiene solo video di calibrazione, che non vengono elaborati da soli.",
            "ja": "キューにはキャリブレーション動画しかありません。これらは単独では処理されません。",
            "ko": "대기열에 보정용 영상만 있습니다. 이 영상은 단독으로 처리되지 않습니다.",
            "no": "Køen inneholder bare kalibreringsvideoer, som ikke behandles alene.",
            "pl": "Kolejka zawiera tylko filmy kalibracyjne, które nie są przetwarzane samodzielnie.",
            "pt": "A fila contém apenas vídeos de calibração, que não são processados sozinhos.",
            "pt_BR": "A fila contém apenas vídeos de calibração, que não são processados sozinhos.",
            "ru": "В очереди только калибровочные видео, которые не обрабатываются сами по себе.",
            "sk": "Fronta obsahuje iba kalibračné videá, ktoré sa samostatne nespracúvajú.",
            "tr": "Kuyrukta yalnızca kalibrasyon videoları var; bunlar tek başına işlenmez.",
            "uk": "У черзі лише калібрувальні відео, які не обробляються окремо.",
            "zh_CN": "队列中只有标定视频，标定视频不会单独处理。",
            "zh_TW": "佇列中只有校正影片，校正影片不會單獨處理。",
        },
    ),
    (
        "App",
        "App.qml",
        2160,
        "The videos in the queue were stopped. Use the restart button on a row to run it again.",
        {
            "cs": "Videa ve frontě byla zastavena. Použijte tlačítko restartu na řádku a spusťte je znovu.",
            "da": "Videoerne i køen blev stoppet. Brug genstartsknappen på en række for at køre den igen.",
            "de": "Die Videos in der Warteschlange wurden gestoppt. Verwenden Sie die Neustart-Schaltfläche in einer Zeile, um sie erneut auszuführen.",
            "el": "Τα βίντεο στην ουρά σταμάτησαν. Χρησιμοποιήστε το κουμπί επανεκκίνησης σε μια σειρά για να εκτελεστεί ξανά.",
            "es": "Los videos de la cola se detuvieron. Usa el botón de reinicio de una fila para volver a ejecutarla.",
            "fi": "Jonon videot pysäytettiin. Käytä rivin uudelleenkäynnistyspainiketta ajaaksesi sen uudelleen.",
            "fr": "Les vidéos de la file d'attente ont été arrêtées. Utilisez le bouton de redémarrage d'une ligne pour la relancer.",
            "gl": "Os vídeos da cola detivéronse. Usa o botón de reinicio dunha fila para executala de novo.",
            "id": "Video dalam antrean dihentikan. Gunakan tombol mulai ulang pada baris untuk menjalankannya lagi.",
            "it": "I video in coda sono stati fermati. Usa il pulsante di riavvio su una riga per eseguirla di nuovo.",
            "ja": "キュー内の動画は停止されました。行の再開ボタンを押すともう一度実行できます。",
            "ko": "대기열의 영상이 중지되었습니다. 행의 다시 시작 버튼으로 다시 실행하세요.",
            "no": "Videoene i køen ble stoppet. Bruk omstartsknappen på en rad for å kjøre den igjen.",
            "pl": "Filmy w kolejce zostały zatrzymane. Użyj przycisku ponownego uruchomienia w wierszu, aby uruchomić go ponownie.",
            "pt": "Os vídeos na fila foram parados. Use o botão de reinício numa linha para a executar novamente.",
            "pt_BR": "Os vídeos na fila foram parados. Use o botão de reinício em uma linha para executá-la novamente.",
            "ru": "Видео в очереди были остановлены. Нажмите кнопку перезапуска в строке, чтобы выполнить её снова.",
            "sk": "Videá vo fronte boli zastavené. Použite tlačidlo reštartu v riadku na jeho opätovné spustenie.",
            "tr": "Kuyruktaki videolar durduruldu. Yeniden çalıştırmak için satırdaki yeniden başlat düğmesini kullanın.",
            "uk": "Відео в черзі було зупинено. Скористайтеся кнопкою перезапуску в рядку, щоб виконати його знову.",
            "zh_CN": "队列中的视频已被停止。点击某一行的重新渲染按钮可以再跑一次。",
            "zh_TW": "佇列中的影片已被停止。點選某一列的重新算圖按鈕可以再跑一次。",
        },
    ),
    (
        "App",
        "App.qml",
        2163,
        "There are no videos in the queue that can be processed.",
        {
            "cs": "Ve frontě nejsou žádná videa, která lze zpracovat.",
            "da": "Der er ingen videoer i køen, der kan behandles.",
            "de": "In der Warteschlange gibt es keine Videos, die verarbeitet werden können.",
            "el": "Δεν υπάρχουν βίντεο στην ουρά που μπορούν να επεξεργαστούν.",
            "es": "No hay videos en la cola que se puedan procesar.",
            "fi": "Jonossa ei ole videoita, joita voitaisiin käsitellä.",
            "fr": "Il n'y a aucune vidéo dans la file d'attente qui puisse être traitée.",
            "gl": "Non hai vídeos na cola que se poidan procesar.",
            "id": "Tidak ada video dalam antrean yang dapat diproses.",
            "it": "Non ci sono video in coda che possano essere elaborati.",
            "ja": "キューに処理できる動画がありません。",
            "ko": "대기열에 처리할 수 있는 영상이 없습니다.",
            "no": "Det er ingen videoer i køen som kan behandles.",
            "pl": "W kolejce nie ma filmów, które można przetworzyć.",
            "pt": "Não há vídeos na fila que possam ser processados.",
            "pt_BR": "Não há vídeos na fila que possam ser processados.",
            "ru": "В очереди нет видео, которые можно обработать.",
            "sk": "Vo fronte nie sú žiadne videá, ktoré je možné spracovať.",
            "tr": "Kuyrukta işlenebilecek video yok.",
            "uk": "У черзі немає відео, які можна обробити.",
            "zh_CN": "队列中没有可以处理的视频。",
            "zh_TW": "佇列中沒有可以處理的影片。",
        },
    ),
]

LANGS = sorted(NEW_STRINGS[0][4].keys())
FORBIDDEN = ("→", "—", "⟷")


def xml_escape(text: str) -> str:
    # Match lupdate's escaping: &, <, > always; " and ' as entities.
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
        .replace("'", "&apos;")
    )


def eol_of(raw: str) -> str:
    return "\r\n" if "\r\n" in raw else "\n"


def find_message_block(raw: str, source: str, eol: str) -> tuple[int, int] | None:
    """Byte span of the <message> block whose <source> equals `source`."""
    src_tag = f"<source>{xml_escape(source)}</source>"
    src_pos = raw.find(src_tag)
    if src_pos == -1:
        return None
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


def message_block(qml: str, line: int, source: str, trans: str | None, eol: str) -> str:
    translation = (
        '<translation type="unfinished"></translation>'
        if trans is None
        else f"<translation>{xml_escape(trans)}</translation>"
    )
    return (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/{qml}" line="{line}"/>{eol}'
        f"        <source>{xml_escape(source)}</source>{eol}"
        f"        {translation}{eol}"
        f"    </message>{eol}"
    )


def patch_file(path: pathlib.Path, lang: str | None) -> bool:
    raw = path.read_bytes().decode("utf-8")
    eol = eol_of(raw)
    original = raw

    # 1. Drop retired sources (pristine and intermediate wordings alike).
    for source in OBSOLETE_SOURCES:
        while (span := find_message_block(raw, source, eol)) is not None:
            raw = raw[: span[0]] + raw[span[1] :]

    # 2. Insert the new strings into their context, if missing.
    for context, qml, line, source, table in NEW_STRINGS:
        if f"<source>{xml_escape(source)}</source>" in raw:
            continue
        ctx_pos = raw.find(f"<name>{context}</name>{eol}")
        if ctx_pos == -1:
            raise RuntimeError(f"{context} context not found")
        insert_at = raw.find(eol, ctx_pos) + len(eol)
        block = message_block(
            qml, line, source, None if lang is None else table[lang], eol
        )
        raw = raw[:insert_at] + block + raw[insert_at:]

    if raw == original:
        return False
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    targets: list[tuple[str, str | None]] = [("gyroflow", None)]
    targets += [(lang, lang) for lang in LANGS]
    for name, lang in targets:
        path = root / f"{name}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        try:
            patched = patch_file(path, lang)
        except RuntimeError as e:
            print(f"{name}: ERROR {e}")
            ok = False
            continue
        raw = path.read_bytes().decode("utf-8")
        problems = []
        for source in OBSOLETE_SOURCES:
            if f"<source>{xml_escape(source)}</source>" in raw:
                problems.append(f"OBSOLETE_PRESENT:{source[:28]}")
        for _ctx, _qml, _line, source, table in NEW_STRINGS:
            if f"<source>{xml_escape(source)}</source>" not in raw:
                problems.append(f"MISSING:{source[:28]}")
            if lang is not None:
                for bad in FORBIDDEN:
                    if bad in table[lang]:
                        problems.append(f"FORBIDDEN_CHAR:{bad!r}")
        status = "patched" if patched else "no-op"
        if problems:
            print(f"{name}: {status} PROBLEMS={problems}")
            ok = False
        else:
            print(f"{name}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
