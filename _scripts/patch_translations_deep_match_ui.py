"""One-shot patch: fill translations for the 5 deep-match UI polish strings
(2026-06-11 UI fine-tuning) in all 22 language .ts files.

The strings were introduced by editing src/ui/RenderQueue.qml and running
lupdate (which left them as <translation type="unfinished"/>):

  BUSY     - queue-busy refusal when starting a deep match
  CLICK    - success-dialog hint that Ok chains into Auto match
  MOTION   - simplified low_motion failure message
  NOMATCH  - simplified not_in_range failure message
  DEEPHINT - post-Auto-match warning suggesting a deep search

Run from repo root:
    python _scripts/patch_translations_deep_match_ui.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

SOURCES = {
    "BUSY": "Cannot start deep match while the queue is busy.",
    "CLICK": "Click Ok to run Auto match and assign the data.",
    "MOTION": "Not enough camera motion. Try a video with more movement.",
    "NOMATCH": "No match found in this gyro file. Try another gyro file.",
    "DEEPHINT": (
        "No match found for %1 video(s). Try a deep search: pick a video "
        "with camera motion, right-click it → \"Deep match with gyro\", "
        "then select a gyro file."
    ),
}

# Per-language translations. Deep-match / Auto-match terminology reuses each
# language's existing translations of "Deep match with gyro" and "Auto match".
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "BUSY": "Hluboké párování nelze spustit, dokud je fronta zaneprázdněná.",
        "CLICK": "Klikněte na Ok pro spuštění automatické shody a přiřazení dat.",
        "MOTION": "Nedostatečný pohyb kamery. Zkuste video s větším pohybem.",
        "NOMATCH": "V tomto souboru gyroskopu nebyla nalezena shoda. Zkuste jiný soubor gyroskopu.",
        "DEEPHINT": (
            "Nenalezena shoda pro %1 videí. Zkuste hluboké hledání: vyberte video "
            "s pohybem kamery, klikněte na něj pravým tlačítkem → "
            "„Hluboké párování s gyroskopem“ a vyberte soubor gyroskopu."
        ),
    },
    "da": {
        "BUSY": "Dyb matchning kan ikke startes, mens køen er optaget.",
        "CLICK": "Klik på Ok for at køre Auto match og tildele dataene.",
        "MOTION": "Ikke nok kamerabevægelse. Prøv en video med mere bevægelse.",
        "NOMATCH": "Intet match fundet i denne gyrofil. Prøv en anden gyrofil.",
        "DEEPHINT": (
            "Intet match fundet for %1 video(er). Prøv en dyb søgning: vælg en "
            "video med kamerabevægelse, højreklik på den → \"Dyb matchning med "
            "gyro\", og vælg en gyrofil."
        ),
    },
    "de": {
        "BUSY": "Tiefenabgleich kann nicht gestartet werden, während die Warteschlange beschäftigt ist.",
        "CLICK": "Klicken Sie auf Ok, um die automatische Übereinstimmung auszuführen und die Daten zuzuweisen.",
        "MOTION": "Nicht genug Kamerabewegung. Versuchen Sie ein Video mit mehr Bewegung.",
        "NOMATCH": "Keine Übereinstimmung in dieser Gyro-Datei gefunden. Versuchen Sie eine andere Gyro-Datei.",
        "DEEPHINT": (
            "Keine Übereinstimmung für %1 Video(s) gefunden. Versuchen Sie eine "
            "Tiefensuche: Wählen Sie ein Video mit Kamerabewegung, Rechtsklick "
            "darauf → „Tiefenabgleich mit Gyro“, dann wählen Sie eine Gyro-Datei."
        ),
    },
    "el": {
        "BUSY": "Δεν είναι δυνατή η έναρξη βαθιάς αντιστοίχισης όσο η ουρά είναι απασχολημένη.",
        "CLICK": "Κάντε κλικ στο Ok για να εκτελέσετε την αυτόματη αντιστοίχιση και να αντιστοιχίσετε τα δεδομένα.",
        "MOTION": "Ανεπαρκής κίνηση κάμερας. Δοκιμάστε ένα βίντεο με περισσότερη κίνηση.",
        "NOMATCH": "Δεν βρέθηκε αντιστοίχιση σε αυτό το αρχείο γυροσκοπίου. Δοκιμάστε άλλο αρχείο γυροσκοπίου.",
        "DEEPHINT": (
            "Δεν βρέθηκε αντιστοίχιση για %1 βίντεο. Δοκιμάστε βαθιά αναζήτηση: "
            "επιλέξτε ένα βίντεο με κίνηση κάμερας, κάντε δεξί κλικ → «Βαθιά "
            "αντιστοίχιση με γυροσκόπιο» και επιλέξτε ένα αρχείο γυροσκοπίου."
        ),
    },
    "es": {
        "BUSY": "No se puede iniciar el emparejamiento profundo mientras la cola está ocupada.",
        "CLICK": "Haga clic en Ok para ejecutar la coincidencia automática y asignar los datos.",
        "MOTION": "No hay suficiente movimiento de cámara. Pruebe con un video con más movimiento.",
        "NOMATCH": "No se encontró coincidencia en este archivo de giroscopio. Pruebe con otro archivo de giroscopio.",
        "DEEPHINT": (
            "No se encontró coincidencia para %1 video(s). Pruebe una búsqueda "
            "profunda: elija un video con movimiento de cámara, haga clic derecho "
            "→ \"Emparejamiento profundo con giroscopio\" y seleccione un archivo "
            "de giroscopio."
        ),
    },
    "fi": {
        "BUSY": "Syväsovitusta ei voi aloittaa, kun jono on varattu.",
        "CLICK": "Napsauta Ok suorittaaksesi automaattisen sovituksen ja määrittääksesi tiedot.",
        "MOTION": "Kameran liikettä ei ole tarpeeksi. Kokeile videota, jossa on enemmän liikettä.",
        "NOMATCH": "Tästä gyrotiedostosta ei löytynyt osumaa. Kokeile toista gyrotiedostoa.",
        "DEEPHINT": (
            "Osumaa ei löytynyt %1 videolle. Kokeile syvähakua: valitse video, "
            "jossa on kameran liikettä, napsauta sitä hiiren oikealla painikkeella "
            "→ \"Syväsovitus gyroon\" ja valitse gyrotiedosto."
        ),
    },
    "fr": {
        "BUSY": "Impossible de démarrer l'appariement profond pendant que la file d'attente est occupée.",
        "CLICK": "Cliquez sur Ok pour lancer la correspondance automatique et attribuer les données.",
        "MOTION": "Mouvement de caméra insuffisant. Essayez une vidéo avec plus de mouvement.",
        "NOMATCH": "Aucune correspondance trouvée dans ce fichier gyro. Essayez un autre fichier gyro.",
        "DEEPHINT": (
            "Aucune correspondance trouvée pour %1 vidéo(s). Essayez une recherche "
            "profonde : choisissez une vidéo avec du mouvement de caméra, clic "
            "droit dessus → « Appariement profond avec gyro », puis sélectionnez "
            "un fichier gyro."
        ),
    },
    "gl": {
        "BUSY": "Non se pode iniciar o emparellamento profundo mentres a cola está ocupada.",
        "CLICK": "Prema Ok para executar a coincidencia automática e asignar os datos.",
        "MOTION": "Non hai suficiente movemento de cámara. Probe cun vídeo con máis movemento.",
        "NOMATCH": "Non se atopou coincidencia neste ficheiro de xiroscopio. Probe con outro ficheiro de xiroscopio.",
        "DEEPHINT": (
            "Non se atopou coincidencia para %1 vídeo(s). Probe unha busca "
            "profunda: escolla un vídeo con movemento de cámara, prema co botón "
            "dereito → \"Emparellamento profundo co xiroscopio\" e seleccione un "
            "ficheiro de xiroscopio."
        ),
    },
    "id": {
        "BUSY": "Tidak dapat memulai pencocokan mendalam saat antrean sedang sibuk.",
        "CLICK": "Klik Ok untuk menjalankan pencocokan otomatis dan menetapkan data.",
        "MOTION": "Gerakan kamera tidak cukup. Coba video dengan lebih banyak gerakan.",
        "NOMATCH": "Tidak ditemukan kecocokan di file giroskop ini. Coba file giroskop lain.",
        "DEEPHINT": (
            "Tidak ditemukan kecocokan untuk %1 video. Coba pencarian mendalam: "
            "pilih video dengan gerakan kamera, klik kanan → \"Pencocokan "
            "mendalam dengan giroskop\", lalu pilih file giroskop."
        ),
    },
    "it": {
        "BUSY": "Impossibile avviare l'abbinamento profondo mentre la coda è occupata.",
        "CLICK": "Fai clic su Ok per eseguire la corrispondenza automatica e assegnare i dati.",
        "MOTION": "Movimento della fotocamera insufficiente. Prova un video con più movimento.",
        "NOMATCH": "Nessuna corrispondenza trovata in questo file giroscopio. Prova un altro file giroscopio.",
        "DEEPHINT": (
            "Nessuna corrispondenza trovata per %1 video. Prova una ricerca "
            "profonda: scegli un video con movimento della fotocamera, fai clic "
            "destro → \"Abbinamento profondo con il giroscopio\", poi seleziona "
            "un file giroscopio."
        ),
    },
    "ja": {
        "BUSY": "キューが動作中のため、ディープマッチングを開始できません。",
        "CLICK": "Ok をクリックすると自動一致が実行され、データが割り当てられます。",
        "MOTION": "カメラの動きが足りません。動きの多い動画でお試しください。",
        "NOMATCH": "このジャイロファイルでは一致が見つかりませんでした。別のジャイロファイルをお試しください。",
        "DEEPHINT": (
            "%1 個の動画で一致が見つかりませんでした。ディープ検索をお試しください："
            "カメラの動きがある動画を選び、右クリック →「ジャイロとディープマッチング」"
            "でジャイロファイルを選択します。"
        ),
    },
    "ko": {
        "BUSY": "대기열이 작동 중일 때는 딥 매칭을 시작할 수 없습니다.",
        "CLICK": "Ok를 클릭하면 자동 매칭이 실행되어 데이터가 할당됩니다.",
        "MOTION": "카메라 움직임이 부족합니다. 움직임이 더 많은 영상으로 시도하세요.",
        "NOMATCH": "이 자이로 파일에서 일치 항목을 찾지 못했습니다. 다른 자이로 파일을 시도하세요.",
        "DEEPHINT": (
            "%1개 영상에 대한 일치 항목을 찾지 못했습니다. 딥 검색을 시도하세요: "
            "카메라 움직임이 있는 영상을 선택하고 마우스 오른쪽 버튼 클릭 → "
            "\"자이로와 딥 매칭\"에서 자이로 파일을 선택하세요."
        ),
    },
    "no": {
        "BUSY": "Kan ikke starte dyp matching mens køen er opptatt.",
        "CLICK": "Klikk Ok for å kjøre automatch og tilordne dataene.",
        "MOTION": "Ikke nok kamerabevegelse. Prøv en video med mer bevegelse.",
        "NOMATCH": "Ingen treff funnet i denne gyrofilen. Prøv en annen gyrofil.",
        "DEEPHINT": (
            "Ingen treff funnet for %1 video(er). Prøv et dypsøk: velg en video "
            "med kamerabevegelse, høyreklikk på den → \"Dyp matching med gyro\", "
            "og velg en gyrofil."
        ),
    },
    "pl": {
        "BUSY": "Nie można rozpocząć głębokiego dopasowania, gdy kolejka jest zajęta.",
        "CLICK": "Kliknij Ok, aby uruchomić automatyczne dopasowanie i przypisać dane.",
        "MOTION": "Za mało ruchu kamery. Spróbuj wideo z większą ilością ruchu.",
        "NOMATCH": "Nie znaleziono dopasowania w tym pliku żyroskopu. Spróbuj innego pliku żyroskopu.",
        "DEEPHINT": (
            "Nie znaleziono dopasowania dla %1 wideo. Spróbuj głębokiego "
            "wyszukiwania: wybierz wideo z ruchem kamery, kliknij je prawym "
            "przyciskiem → „Głębokie dopasowanie z żyroskopem“, a następnie "
            "wybierz plik żyroskopu."
        ),
    },
    "pt": {
        "BUSY": "Não é possível iniciar a correspondência profunda enquanto a fila está ocupada.",
        "CLICK": "Clique em Ok para executar a correspondência automática e atribuir os dados.",
        "MOTION": "Movimento de câmara insuficiente. Experimente um vídeo com mais movimento.",
        "NOMATCH": "Nenhuma correspondência encontrada neste ficheiro de giroscópio. Experimente outro ficheiro de giroscópio.",
        "DEEPHINT": (
            "Nenhuma correspondência encontrada para %1 vídeo(s). Experimente uma "
            "pesquisa profunda: escolha um vídeo com movimento de câmara, clique "
            "com o botão direito → \"Correspondência profunda com giroscópio\" e "
            "selecione um ficheiro de giroscópio."
        ),
    },
    "pt_BR": {
        "BUSY": "Não é possível iniciar a correspondência profunda enquanto a fila está ocupada.",
        "CLICK": "Clique em Ok para executar a correspondência automática e atribuir os dados.",
        "MOTION": "Movimento de câmera insuficiente. Tente um vídeo com mais movimento.",
        "NOMATCH": "Nenhuma correspondência encontrada neste arquivo de giroscópio. Tente outro arquivo de giroscópio.",
        "DEEPHINT": (
            "Nenhuma correspondência encontrada para %1 vídeo(s). Tente uma busca "
            "profunda: escolha um vídeo com movimento de câmera, clique com o "
            "botão direito → \"Correspondência profunda com giroscópio\" e "
            "selecione um arquivo de giroscópio."
        ),
    },
    "ru": {
        "BUSY": "Невозможно начать глубокое сопоставление, пока очередь занята.",
        "CLICK": "Нажмите Ok, чтобы запустить автоматическое сопоставление и назначить данные.",
        "MOTION": "Недостаточно движения камеры. Попробуйте видео с большим движением.",
        "NOMATCH": "В этом файле гироскопа совпадение не найдено. Попробуйте другой файл гироскопа.",
        "DEEPHINT": (
            "Совпадение не найдено для %1 видео. Попробуйте глубокий поиск: "
            "выберите видео с движением камеры, щёлкните по нему правой кнопкой "
            "→ «Глубокое сопоставление с гироскопом» и выберите файл гироскопа."
        ),
    },
    "sk": {
        "BUSY": "Hlboké párovanie nie je možné spustiť, kým je front zaneprázdnený.",
        "CLICK": "Kliknite na Ok pre spustenie automatického párovania a priradenie dát.",
        "MOTION": "Nedostatočný pohyb kamery. Skúste video s väčším pohybom.",
        "NOMATCH": "V tomto súbore gyroskopu sa nenašla zhoda. Skúste iný súbor gyroskopu.",
        "DEEPHINT": (
            "Nenašla sa zhoda pre %1 videí. Skúste hlboké hľadanie: vyberte video "
            "s pohybom kamery, kliknite naň pravým tlačidlom → „Hlboké párovanie "
            "s gyroskopom“ a vyberte súbor gyroskopu."
        ),
    },
    "tr": {
        "BUSY": "Kuyruk meşgulken derin eşleştirme başlatılamaz.",
        "CLICK": "Ok'a tıklayın; otomatik eşleştirme çalışıp verileri atayacaktır.",
        "MOTION": "Yeterli kamera hareketi yok. Daha fazla hareket içeren bir video deneyin.",
        "NOMATCH": "Bu jiroskop dosyasında eşleşme bulunamadı. Başka bir jiroskop dosyası deneyin.",
        "DEEPHINT": (
            "%1 video için eşleşme bulunamadı. Derin arama deneyin: kamera "
            "hareketi olan bir video seçin, sağ tıklayın → \"Jiroskopla derin "
            "eşleştirme\", ardından bir jiroskop dosyası seçin."
        ),
    },
    "uk": {
        "BUSY": "Неможливо розпочати глибоке зіставлення, поки черга зайнята.",
        "CLICK": "Натисніть Ok, щоб запустити автоматичне зіставлення та призначити дані.",
        "MOTION": "Недостатньо руху камери. Спробуйте відео з більшим рухом.",
        "NOMATCH": "У цьому файлі гіроскопа збігів не знайдено. Спробуйте інший файл гіроскопа.",
        "DEEPHINT": (
            "Збігів не знайдено для %1 відео. Спробуйте глибокий пошук: виберіть "
            "відео з рухом камери, клацніть по ньому правою кнопкою → «Глибоке "
            "зіставлення з гіроскопом» і виберіть файл гіроскопа."
        ),
    },
    "zh_CN": {
        "BUSY": "队列忙碌时无法开始深度匹配。",
        "CLICK": "点击确定后将自动运行自动匹配并分配数据。",
        "MOTION": "相机运动不足，请换一个运动更多的视频。",
        "NOMATCH": "在该陀螺仪文件中未找到匹配，请尝试其他陀螺仪文件。",
        "DEEPHINT": (
            "未匹配到 %1 个视频。可尝试深度搜索：选择一个有相机运动的视频，"
            "右键 →「与陀螺仪深度匹配」，再选择一个陀螺仪文件。"
        ),
    },
    "zh_TW": {
        "BUSY": "佇列忙碌時無法開始深度匹配。",
        "CLICK": "點擊確定後將自動執行自動配對並分配資料。",
        "MOTION": "相機運動不足，請換一個運動更多的影片。",
        "NOMATCH": "在該陀螺儀檔案中未找到匹配，請嘗試其他陀螺儀檔案。",
        "DEEPHINT": (
            "未匹配到 %1 個影片。可嘗試深度搜尋：選擇一個有相機運動的影片，"
            "按右鍵 →「與陀螺儀深度匹配」，再選擇一個陀螺儀檔案。"
        ),
    },
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
        # Verify all 5 entries now carry a translation (patched now or before).
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
