"""One-shot patch: fill translations for the 2 reworded deep-match failure
hint strings (2026-06-12 UX feedback) in all 22 language .ts files.

The strings were introduced by editing src/ui/RenderQueue.qml and running
lupdate (which left them as <translation type="unfinished"/>):

  NOMATCH2  - not_in_range failure: both directions (wrong gyro file OR
              unreliable video motion)
  UNMATCHED - post-auto-match warning, shortened to the deep-match action
  SYNCFAIL  - all_yellow batch-sync prompt, replacing the old 3-section
              calibration-video guide with the deep-match action

Run from repo root:
    python _scripts/patch_translations_deep_match_failure_hints.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

SOURCES = {
    "NOMATCH2": (
        "No match found. The gyro file may not cover this video, or the "
        "video's motion may be unreliable — try another gyro file or "
        "another video."
    ),
    "UNMATCHED": (
        "%1 video(s) not matched. Right-click a video with clear camera "
        "motion → \"Deep match with gyro\"."
    ),
    "SYNCFAIL": (
        "Could not establish time sync. Right-click a video with clear "
        "camera motion → \"Deep match with gyro\"."
    ),
}

# Deep-match terminology reuses each language's existing translation of
# "Deep match with gyro".
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "NOMATCH2": (
            "Shoda nenalezena. Soubor gyroskopu nemusí toto video pokrývat, "
            "nebo je pohyb videa nespolehlivý — zkuste jiný soubor gyroskopu "
            "nebo jiné video."
        ),
        "UNMATCHED": (
            "%1 videí nespárováno. Klikněte pravým tlačítkem na video se "
            "zřetelným pohybem kamery → „Hluboké párování s gyroskopem“."
        ),
    },
    "da": {
        "NOMATCH2": (
            "Intet match fundet. Gyrofilen dækker muligvis ikke denne video, "
            "eller videoens bevægelse kan være upålidelig — prøv en anden "
            "gyrofil eller en anden video."
        ),
        "UNMATCHED": (
            "%1 video(er) ikke matchet. Højreklik på en video med tydelig "
            "kamerabevægelse → \"Dyb matchning med gyro\"."
        ),
    },
    "de": {
        "NOMATCH2": (
            "Keine Übereinstimmung gefunden. Die Gyro-Datei deckt dieses "
            "Video möglicherweise nicht ab, oder die Bewegung des Videos ist "
            "unzuverlässig — versuchen Sie eine andere Gyro-Datei oder ein "
            "anderes Video."
        ),
        "UNMATCHED": (
            "%1 Video(s) nicht zugeordnet. Rechtsklick auf ein Video mit "
            "deutlicher Kamerabewegung → „Tiefenabgleich mit Gyro“."
        ),
    },
    "el": {
        "NOMATCH2": (
            "Δεν βρέθηκε αντιστοίχιση. Το αρχείο γυροσκοπίου ίσως δεν "
            "καλύπτει αυτό το βίντεο, ή η κίνηση του βίντεο είναι "
            "αναξιόπιστη — δοκιμάστε άλλο αρχείο γυροσκοπίου ή άλλο βίντεο."
        ),
        "UNMATCHED": (
            "%1 βίντεο χωρίς αντιστοίχιση. Δεξί κλικ σε ένα βίντεο με "
            "καθαρή κίνηση κάμερας → «Βαθιά αντιστοίχιση με γυροσκόπιο»."
        ),
    },
    "es": {
        "NOMATCH2": (
            "No se encontró coincidencia. Puede que el archivo de giroscopio "
            "no cubra este video, o que el movimiento del video no sea "
            "fiable — pruebe con otro archivo de giroscopio u otro video."
        ),
        "UNMATCHED": (
            "%1 video(s) sin emparejar. Haga clic derecho en un video con "
            "movimiento de cámara claro → \"Emparejamiento profundo con "
            "giroscopio\"."
        ),
    },
    "fi": {
        "NOMATCH2": (
            "Osumaa ei löytynyt. Gyrotiedosto ei ehkä kata tätä videota, tai "
            "videon liike on epäluotettava — kokeile toista gyrotiedostoa "
            "tai toista videota."
        ),
        "UNMATCHED": (
            "%1 videota ilman osumaa. Napsauta hiiren oikealla videota, "
            "jossa on selkeää kameran liikettä → \"Syväsovitus gyroon\"."
        ),
    },
    "fr": {
        "NOMATCH2": (
            "Aucune correspondance trouvée. Le fichier gyro ne couvre "
            "peut-être pas cette vidéo, ou le mouvement de la vidéo est "
            "peu fiable — essayez un autre fichier gyro ou une autre vidéo."
        ),
        "UNMATCHED": (
            "%1 vidéo(s) non appariée(s). Clic droit sur une vidéo avec un "
            "mouvement de caméra net → « Appariement profond avec gyro »."
        ),
    },
    "gl": {
        "NOMATCH2": (
            "Non se atopou coincidencia. Pode que o ficheiro de xiroscopio "
            "non cubra este vídeo, ou que o movemento do vídeo non sexa "
            "fiable — probe con outro ficheiro de xiroscopio ou outro vídeo."
        ),
        "UNMATCHED": (
            "%1 vídeo(s) sen emparellar. Prema co botón dereito nun vídeo "
            "con movemento de cámara claro → \"Emparellamento profundo co "
            "xiroscopio\"."
        ),
    },
    "id": {
        "NOMATCH2": (
            "Tidak ditemukan kecocokan. File giroskop mungkin tidak "
            "mencakup video ini, atau gerakan video tidak andal — coba file "
            "giroskop lain atau video lain."
        ),
        "UNMATCHED": (
            "%1 video belum cocok. Klik kanan video dengan gerakan kamera "
            "yang jelas → \"Pencocokan mendalam dengan giroskop\"."
        ),
    },
    "it": {
        "NOMATCH2": (
            "Nessuna corrispondenza trovata. Il file giroscopio potrebbe "
            "non coprire questo video, oppure il movimento del video è "
            "inaffidabile — prova un altro file giroscopio o un altro video."
        ),
        "UNMATCHED": (
            "%1 video non abbinati. Fai clic destro su un video con "
            "movimento della fotocamera chiaro → \"Abbinamento profondo con "
            "il giroscopio\"."
        ),
    },
    "ja": {
        "NOMATCH2": (
            "一致が見つかりませんでした。ジャイロファイルがこの動画をカバーしていないか、"
            "動画の動き情報が不安定な可能性があります — 別のジャイロファイルか別の動画でお試しください。"
        ),
        "UNMATCHED": (
            "%1 個の動画が未マッチです。カメラの動きがはっきりした動画を右クリック →"
            "「ジャイロとディープマッチング」を実行してください。"
        ),
    },
    "ko": {
        "NOMATCH2": (
            "일치 항목을 찾지 못했습니다. 자이로 파일이 이 영상을 포함하지 않거나 "
            "영상의 움직임 정보가 불안정할 수 있습니다 — 다른 자이로 파일이나 다른 "
            "영상으로 시도하세요."
        ),
        "UNMATCHED": (
            "%1개 영상이 매칭되지 않았습니다. 카메라 움직임이 뚜렷한 영상을 마우스 "
            "오른쪽 버튼으로 클릭 → \"자이로와 딥 매칭\"을 실행하세요."
        ),
    },
    "no": {
        "NOMATCH2": (
            "Ingen treff funnet. Gyrofilen dekker kanskje ikke denne "
            "videoen, eller videoens bevegelse er upålitelig — prøv en "
            "annen gyrofil eller en annen video."
        ),
        "UNMATCHED": (
            "%1 video(er) uten treff. Høyreklikk på en video med tydelig "
            "kamerabevegelse → \"Dyp matching med gyro\"."
        ),
    },
    "pl": {
        "NOMATCH2": (
            "Nie znaleziono dopasowania. Plik żyroskopu może nie obejmować "
            "tego wideo, albo ruch wideo jest niewiarygodny — spróbuj "
            "innego pliku żyroskopu lub innego wideo."
        ),
        "UNMATCHED": (
            "%1 wideo bez dopasowania. Kliknij prawym przyciskiem wideo z "
            "wyraźnym ruchem kamery → „Głębokie dopasowanie z żyroskopem“."
        ),
    },
    "pt": {
        "NOMATCH2": (
            "Nenhuma correspondência encontrada. O ficheiro de giroscópio "
            "pode não cobrir este vídeo, ou o movimento do vídeo é pouco "
            "fiável — experimente outro ficheiro de giroscópio ou outro "
            "vídeo."
        ),
        "UNMATCHED": (
            "%1 vídeo(s) por corresponder. Clique com o botão direito num "
            "vídeo com movimento de câmara claro → \"Correspondência "
            "profunda com giroscópio\"."
        ),
    },
    "pt_BR": {
        "NOMATCH2": (
            "Nenhuma correspondência encontrada. O arquivo de giroscópio "
            "pode não cobrir este vídeo, ou o movimento do vídeo é pouco "
            "confiável — tente outro arquivo de giroscópio ou outro vídeo."
        ),
        "UNMATCHED": (
            "%1 vídeo(s) sem correspondência. Clique com o botão direito em "
            "um vídeo com movimento de câmera claro → \"Correspondência "
            "profunda com giroscópio\"."
        ),
    },
    "ru": {
        "NOMATCH2": (
            "Совпадение не найдено. Файл гироскопа может не охватывать это "
            "видео, либо движение видео ненадёжно — попробуйте другой файл "
            "гироскопа или другое видео."
        ),
        "UNMATCHED": (
            "%1 видео не сопоставлено. Щёлкните правой кнопкой по видео с "
            "чётким движением камеры → «Глубокое сопоставление с "
            "гироскопом»."
        ),
    },
    "sk": {
        "NOMATCH2": (
            "Zhoda sa nenašla. Súbor gyroskopu nemusí toto video pokrývať, "
            "alebo je pohyb videa nespoľahlivý — skúste iný súbor gyroskopu "
            "alebo iné video."
        ),
        "UNMATCHED": (
            "%1 videí nespárovaných. Kliknite pravým tlačidlom na video so "
            "zreteľným pohybom kamery → „Hlboké párovanie s gyroskopom“."
        ),
    },
    "tr": {
        "NOMATCH2": (
            "Eşleşme bulunamadı. Jiroskop dosyası bu videoyu kapsamıyor "
            "olabilir veya videonun hareketi güvenilir değil — başka bir "
            "jiroskop dosyası ya da başka bir video deneyin."
        ),
        "UNMATCHED": (
            "%1 video eşleşmedi. Kamera hareketi belirgin bir videoya sağ "
            "tıklayın → \"Jiroskopla derin eşleştirme\"."
        ),
    },
    "uk": {
        "NOMATCH2": (
            "Збігів не знайдено. Файл гіроскопа може не охоплювати це "
            "відео, або рух відео ненадійний — спробуйте інший файл "
            "гіроскопа чи інше відео."
        ),
        "UNMATCHED": (
            "%1 відео не зіставлено. Клацніть правою кнопкою по відео з "
            "чітким рухом камери → «Глибоке зіставлення з гіроскопом»."
        ),
    },
    "zh_CN": {
        "NOMATCH2": "未找到匹配。可能该陀螺仪文件不包含这个视频，也可能是视频的运动信息不可靠——换个陀螺仪文件或换个视频再试。",
        "UNMATCHED": "未匹配到 %1 个视频。右键一个运动明显的视频 →「与陀螺仪深度匹配」。",
    },
    "zh_TW": {
        "NOMATCH2": "未找到匹配。可能該陀螺儀檔案不包含這個影片，也可能是影片的運動資訊不可靠——換個陀螺儀檔案或換個影片再試。",
        "UNMATCHED": "未匹配到 %1 個影片。按右鍵選一個運動明顯的影片 →「與陀螺儀深度匹配」。",
    },
}


# SYNCFAIL added in a follow-up pass (all_yellow prompt rewrite); merged into
# TRANS below so patch_file handles all three strings uniformly.
SYNCFAIL_TRANS: dict[str, str] = {
    "cs": "Nepodařilo se vytvořit časovou synchronizaci. Klikněte pravým tlačítkem na video se zřetelným pohybem kamery → „Hluboké párování s gyroskopem“.",
    "da": "Kunne ikke etablere tidssynkronisering. Højreklik på en video med tydelig kamerabevægelse → \"Dyb matchning med gyro\".",
    "de": "Zeitsynchronisierung konnte nicht hergestellt werden. Rechtsklick auf ein Video mit deutlicher Kamerabewegung → „Tiefenabgleich mit Gyro“.",
    "el": "Δεν ήταν δυνατή η δημιουργία χρονικού συγχρονισμού. Δεξί κλικ σε ένα βίντεο με καθαρή κίνηση κάμερας → «Βαθιά αντιστοίχιση με γυροσκόπιο».",
    "es": "No se pudo establecer la sincronización de tiempo. Haga clic derecho en un video con movimiento de cámara claro → \"Emparejamiento profundo con giroscopio\".",
    "fi": "Aikasynkronointia ei voitu muodostaa. Napsauta hiiren oikealla videota, jossa on selkeää kameran liikettä → \"Syväsovitus gyroon\".",
    "fr": "Impossible d'établir la synchronisation temporelle. Clic droit sur une vidéo avec un mouvement de caméra net → « Appariement profond avec gyro ».",
    "gl": "Non se puido establecer a sincronización de tempo. Prema co botón dereito nun vídeo con movemento de cámara claro → \"Emparellamento profundo co xiroscopio\".",
    "id": "Tidak dapat membuat sinkronisasi waktu. Klik kanan video dengan gerakan kamera yang jelas → \"Pencocokan mendalam dengan giroskop\".",
    "it": "Impossibile stabilire la sincronizzazione temporale. Fai clic destro su un video con movimento della fotocamera chiaro → \"Abbinamento profondo con il giroscopio\".",
    "ja": "時間同期を確立できませんでした。カメラの動きがはっきりした動画を右クリック →「ジャイロとディープマッチング」を実行してください。",
    "ko": "시간 동기화를 설정할 수 없습니다. 카메라 움직임이 뚜렷한 영상을 마우스 오른쪽 버튼으로 클릭 → \"자이로와 딥 매칭\"을 실행하세요.",
    "no": "Kunne ikke etablere tidssynkronisering. Høyreklikk på en video med tydelig kamerabevegelse → \"Dyp matching med gyro\".",
    "pl": "Nie udało się ustalić synchronizacji czasu. Kliknij prawym przyciskiem wideo z wyraźnym ruchem kamery → „Głębokie dopasowanie z żyroskopem“.",
    "pt": "Não foi possível estabelecer a sincronização de tempo. Clique com o botão direito num vídeo com movimento de câmara claro → \"Correspondência profunda com giroscópio\".",
    "pt_BR": "Não foi possível estabelecer a sincronização de tempo. Clique com o botão direito em um vídeo com movimento de câmera claro → \"Correspondência profunda com giroscópio\".",
    "ru": "Не удалось установить синхронизацию времени. Щёлкните правой кнопкой по видео с чётким движением камеры → «Глубокое сопоставление с гироскопом».",
    "sk": "Nepodarilo sa vytvoriť časovú synchronizáciu. Kliknite pravým tlačidlom na video so zreteľným pohybom kamery → „Hlboké párovanie s gyroskopom“.",
    "tr": "Zaman senkronizasyonu kurulamadı. Kamera hareketi belirgin bir videoya sağ tıklayın → \"Jiroskopla derin eşleştirme\".",
    "uk": "Не вдалося встановити синхронізацію часу. Клацніть правою кнопкою по відео з чітким рухом камери → «Глибоке зіставлення з гіроскопом».",
    "zh_CN": "未能建立时间同步。右键一个运动明显的视频 →「与陀螺仪深度匹配」。",
    "zh_TW": "未能建立時間同步。按右鍵選一個運動明顯的影片 →「與陀螺儀深度匹配」。",
}
for _lang, _t in SYNCFAIL_TRANS.items():
    TRANS[_lang]["SYNCFAIL"] = _t


def xml_escape(text: str) -> str:
    # Match lupdate's escaping: &, <, > always; " and ' as entities.
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
        .replace("'", "&apos;")
    )


def patch_file(path: pathlib.Path, trans: dict[str, str]) -> list[str]:
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"
    patched = []
    for key, source in SOURCES.items():
        src = xml_escape(source)
        needle = (
            f"<source>{src}</source>{eol}"
            f'        <translation type="unfinished"></translation>'
        )
        if needle not in raw:
            continue  # already patched, or entry missing (reported by caller)
        replacement = (
            f"<source>{src}</source>{eol}"
            f"        <translation>{xml_escape(trans[key])}</translation>"
        )
        raw = raw.replace(needle, replacement, 1)
        patched.append(key)
    if patched:
        path.write_bytes(raw.encode("utf-8"))
    return patched


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    for lang, trans in TRANS.items():
        path = root / f"{lang}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        patched = patch_file(path, trans)
        raw = path.read_bytes().decode("utf-8")
        missing = [
            k for k, s in SOURCES.items()
            if f"<source>{xml_escape(s)}</source>" not in raw
        ]
        unfinished = [
            k for k, s in SOURCES.items()
            if f"<source>{xml_escape(s)}</source>" in raw
            and f"<source>{xml_escape(s)}</source>" + ("\r\n" if "\r\n" in raw else "\n")
            + '        <translation type="unfinished"></translation>' in raw
        ]
        status = f"patched={patched}" if patched else "no-op"
        if missing or unfinished:
            print(f"{lang}: {status} MISSING={missing} STILL_UNFINISHED={unfinished}")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
