"""One-shot patch: add translations for the 4 new deep-match refusal strings
(deep-match-refusal-reason-codes, 2026-06-14) to all 22 language .ts files.

start_deep_gyro_match used to return a bare bool, so QML showed a single
catch-all "queue is busy" message for every refusal. It now returns a reason
code and QML maps each to a specific message. The existing "Cannot start deep
match while the queue is busy." string is reused for the queue_active reason;
the four new strings below cover the remaining reasons:

  INFLIGHT   - deep_match_in_flight (another match already running)
  GYROMISS   - gyro_missing (pool file no longer available)
  GYRONOTRDY - gyro_not_ready (gyro data still parsing)
  JOBMISS    - job_missing (job / stab handle gone)

Unlike lupdate-based flows, this inserts the <message> blocks directly after
the existing BUSY entry in the RenderQueue context, so the new entries land in
the right context without lupdate rewriting the whole file.

Run from repo root:
    python _scripts/patch_translations_deep_match_refusal_ui.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

# Anchor: an existing RenderQueue-context string (queue_active reason). New
# entries are inserted immediately after this message block.
ANCHOR = "Cannot start deep match while the queue is busy."

# Source line numbers in src/ui/RenderQueue.qml (informational only; lrelease
# matches by context + source, not by location).
LINES = {
    "INFLIGHT": 246,
    "GYROMISS": 248,
    "GYRONOTRDY": 250,
    "JOBMISS": 252,
}

SOURCES = {
    "INFLIGHT": "Another deep match is already running. Please wait for it to finish.",
    "GYROMISS": "This gyro file is no longer available.",
    "GYRONOTRDY": "The gyro data is still being parsed. Please try again shortly.",
    "JOBMISS": "This job is no longer in the render queue.",
}

# Per-language translations. Reuses each language's established deep-match /
# gyro / queue terminology from patch_translations_deep_match_ui.py.
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "INFLIGHT": "Jedno hluboké párování již probíhá. Počkejte prosím na jeho dokončení.",
        "GYROMISS": "Tento soubor gyroskopu již není k dispozici.",
        "GYRONOTRDY": "Data gyroskopu se stále zpracovávají. Zkuste to prosím za chvíli.",
        "JOBMISS": "Tato úloha již není ve frontě.",
    },
    "da": {
        "INFLIGHT": "En anden dyb matchning kører allerede. Vent venligst, til den er færdig.",
        "GYROMISS": "Denne gyrofil er ikke længere tilgængelig.",
        "GYRONOTRDY": "Gyrodataene behandles stadig. Prøv venligst igen om lidt.",
        "JOBMISS": "Dette job er ikke længere i køen.",
    },
    "de": {
        "INFLIGHT": "Ein anderer Tiefenabgleich läuft bereits. Bitte warten Sie, bis er abgeschlossen ist.",
        "GYROMISS": "Diese Gyro-Datei ist nicht mehr verfügbar.",
        "GYRONOTRDY": "Die Gyro-Daten werden noch verarbeitet. Bitte versuchen Sie es in Kürze erneut.",
        "JOBMISS": "Dieser Auftrag ist nicht mehr in der Warteschlange.",
    },
    "el": {
        "INFLIGHT": "Μια άλλη βαθιά αντιστοίχιση εκτελείται ήδη. Περιμένετε να ολοκληρωθεί.",
        "GYROMISS": "Αυτό το αρχείο γυροσκοπίου δεν είναι πλέον διαθέσιμο.",
        "GYRONOTRDY": "Τα δεδομένα γυροσκοπίου εξακολουθούν να αναλύονται. Δοκιμάστε ξανά σε λίγο.",
        "JOBMISS": "Αυτή η εργασία δεν βρίσκεται πλέον στην ουρά.",
    },
    "es": {
        "INFLIGHT": "Ya se está ejecutando otro emparejamiento profundo. Espere a que termine.",
        "GYROMISS": "Este archivo de giroscopio ya no está disponible.",
        "GYRONOTRDY": "Los datos del giroscopio aún se están analizando. Inténtelo de nuevo en breve.",
        "JOBMISS": "Este trabajo ya no está en la cola.",
    },
    "fi": {
        "INFLIGHT": "Toinen syväsovitus on jo käynnissä. Odota, että se valmistuu.",
        "GYROMISS": "Tämä gyrotiedosto ei ole enää käytettävissä.",
        "GYRONOTRDY": "Gyrotietoja käsitellään edelleen. Yritä hetken kuluttua uudelleen.",
        "JOBMISS": "Tämä työ ei ole enää jonossa.",
    },
    "fr": {
        "INFLIGHT": "Un autre appariement profond est déjà en cours. Veuillez attendre qu'il se termine.",
        "GYROMISS": "Ce fichier gyro n'est plus disponible.",
        "GYRONOTRDY": "Les données gyro sont encore en cours d'analyse. Veuillez réessayer dans un instant.",
        "JOBMISS": "Cette tâche n'est plus dans la file d'attente.",
    },
    "gl": {
        "INFLIGHT": "Xa se está executando outro emparellamento profundo. Agarde a que remate.",
        "GYROMISS": "Este ficheiro de xiroscopio xa non está dispoñible.",
        "GYRONOTRDY": "Os datos do xiroscopio aínda se están analizando. Ténteo de novo nun momento.",
        "JOBMISS": "Este traballo xa non está na cola.",
    },
    "id": {
        "INFLIGHT": "Pencocokan mendalam lain sedang berjalan. Harap tunggu hingga selesai.",
        "GYROMISS": "File giroskop ini sudah tidak tersedia.",
        "GYRONOTRDY": "Data giroskop masih diproses. Silakan coba lagi sebentar lagi.",
        "JOBMISS": "Tugas ini sudah tidak ada dalam antrean.",
    },
    "it": {
        "INFLIGHT": "Un altro abbinamento profondo è già in corso. Attendi che termini.",
        "GYROMISS": "Questo file giroscopio non è più disponibile.",
        "GYRONOTRDY": "I dati del giroscopio sono ancora in elaborazione. Riprova tra poco.",
        "JOBMISS": "Questo lavoro non è più nella coda.",
    },
    "ja": {
        "INFLIGHT": "別のディープマッチングがすでに実行中です。完了するまでお待ちください。",
        "GYROMISS": "このジャイロファイルは利用できなくなりました。",
        "GYRONOTRDY": "ジャイロデータをまだ解析中です。しばらくしてからもう一度お試しください。",
        "JOBMISS": "このジョブはキューに存在しません。",
    },
    "ko": {
        "INFLIGHT": "다른 딥 매칭이 이미 실행 중입니다. 완료될 때까지 기다려 주세요.",
        "GYROMISS": "이 자이로 파일을 더 이상 사용할 수 없습니다.",
        "GYRONOTRDY": "자이로 데이터를 아직 분석하는 중입니다. 잠시 후 다시 시도하세요.",
        "JOBMISS": "이 작업이 더 이상 대기열에 없습니다.",
    },
    "no": {
        "INFLIGHT": "En annen dyp matching kjører allerede. Vent til den er ferdig.",
        "GYROMISS": "Denne gyrofilen er ikke lenger tilgjengelig.",
        "GYRONOTRDY": "Gyrodataene behandles fortsatt. Prøv igjen om litt.",
        "JOBMISS": "Denne jobben er ikke lenger i køen.",
    },
    "pl": {
        "INFLIGHT": "Inne głębokie dopasowanie jest już uruchomione. Poczekaj na jego zakończenie.",
        "GYROMISS": "Ten plik żyroskopu nie jest już dostępny.",
        "GYRONOTRDY": "Dane żyroskopu są nadal przetwarzane. Spróbuj ponownie za chwilę.",
        "JOBMISS": "To zadanie nie jest już w kolejce.",
    },
    "pt": {
        "INFLIGHT": "Outra correspondência profunda já está em execução. Aguarde que termine.",
        "GYROMISS": "Este ficheiro de giroscópio já não está disponível.",
        "GYRONOTRDY": "Os dados do giroscópio ainda estão a ser analisados. Tente novamente dentro de momentos.",
        "JOBMISS": "Esta tarefa já não está na fila.",
    },
    "pt_BR": {
        "INFLIGHT": "Outra correspondência profunda já está em execução. Aguarde até que termine.",
        "GYROMISS": "Este arquivo de giroscópio não está mais disponível.",
        "GYRONOTRDY": "Os dados do giroscópio ainda estão sendo analisados. Tente novamente em instantes.",
        "JOBMISS": "Esta tarefa não está mais na fila.",
    },
    "ru": {
        "INFLIGHT": "Другое глубокое сопоставление уже выполняется. Дождитесь его завершения.",
        "GYROMISS": "Этот файл гироскопа больше недоступен.",
        "GYRONOTRDY": "Данные гироскопа ещё обрабатываются. Повторите попытку через мгновение.",
        "JOBMISS": "Этой задачи больше нет в очереди.",
    },
    "sk": {
        "INFLIGHT": "Iné hlboké párovanie už prebieha. Počkajte na jeho dokončenie.",
        "GYROMISS": "Tento súbor gyroskopu už nie je k dispozícii.",
        "GYRONOTRDY": "Dáta gyroskopu sa stále spracúvajú. Skúste to prosím o chvíľu.",
        "JOBMISS": "Táto úloha už nie je vo fronte.",
    },
    "tr": {
        "INFLIGHT": "Başka bir derin eşleştirme zaten çalışıyor. Lütfen tamamlanmasını bekleyin.",
        "GYROMISS": "Bu jiroskop dosyası artık kullanılamıyor.",
        "GYRONOTRDY": "Jiroskop verileri hâlâ ayrıştırılıyor. Lütfen birazdan tekrar deneyin.",
        "JOBMISS": "Bu iş artık kuyrukta değil.",
    },
    "uk": {
        "INFLIGHT": "Інше глибоке зіставлення вже виконується. Зачекайте, доки воно завершиться.",
        "GYROMISS": "Цей файл гіроскопа більше недоступний.",
        "GYRONOTRDY": "Дані гіроскопа ще обробляються. Повторіть спробу за мить.",
        "JOBMISS": "Цього завдання більше немає в черзі.",
    },
    "zh_CN": {
        "INFLIGHT": "已有一个深度匹配正在进行，请等待其完成。",
        "GYROMISS": "该陀螺仪文件已不可用。",
        "GYRONOTRDY": "陀螺仪数据仍在解析，请稍后再试。",
        "JOBMISS": "该任务已不在渲染队列中。",
    },
    "zh_TW": {
        "INFLIGHT": "已有一個深度匹配正在進行，請等待其完成。",
        "GYROMISS": "該陀螺儀檔案已不可用。",
        "GYRONOTRDY": "陀螺儀資料仍在解析，請稍後再試。",
        "JOBMISS": "該工作已不在算繪佇列中。",
    },
}

KEY_ORDER = ["INFLIGHT", "GYROMISS", "GYRONOTRDY", "JOBMISS"]


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
    if f"<source>{xml_escape(SOURCES['INFLIGHT'])}</source>" in raw:
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
        # Verify all 4 entries now carry a finished translation.
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
