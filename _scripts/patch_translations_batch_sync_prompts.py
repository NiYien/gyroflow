"""One-shot patch: replace the AllYellow source string in all 23 .ts files
and inject 3 new strings (long Markdown prompt + Anamorphic lens warning +
Pair with Gyro menu).

Run from repo root:
    python _scripts/patch_translations_batch_sync_prompts.py

Then regenerate the runtime .qm bundles (the app loads .qm at runtime; .ts
edits alone are invisible until lrelease re-emits the binaries):

    pwsh -c '$lrel = "C:\\Qt\\6.7.1\\mingw_64\\bin\\lrelease.exe"; \\
        Get-ChildItem resources/translations/*.ts | ForEach-Object \\
        { & $lrel -silent $_.FullName }'

(adjust the lrelease.exe path to wherever Qt Linguist lives on your machine —
the in-tree ext/6.7.3 bundle does not include lrelease).

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import re
import sys

OLD_SOURCE = (
    "Batch synchronization did not produce a reliable result. "
    "Check gyro splitting or batch matching and try again."
)

# Must match the qsTr literal in src/ui/RenderQueue.qml line 290 byte-for-byte.
# Each bold heading is followed by \n\n (paragraph break), then the body text.
NEW_SOURCE_LONG = (
    "**Batch synchronization did not produce a reliable result.** Please check:\n\n"
    "**1. Calibration videos are loaded**\n\n"
    "Short videos (under 10 seconds) recorded simultaneously with the gyro file.\n\n"
    "**2. Videos and gyro files are correctly paired**\n\n"
    "For any unpaired video, you can pair manually: right-click the video → "
    "**\"Pair with Gyro\"**, then select the matching gyro file.\n\n"
    "**3. If there are not 2 calibration videos for the day**\n\n"
    "- Borrow time-sync data from the previous/next day: copy the calibration "
    "videos and their gyro files into the current day's folder, re-add to the "
    "queue, then pair manually as above.\n"
    "- Or re-shoot calibration videos and re-add to the queue."
)

SOURCES = {
    "LONG": NEW_SOURCE_LONG,
    "ANAM": "%1 video(s) will use Anamorphic lens",
    "PAIR": "Pair with Gyro",
}

# QML source line numbers (verified against src/ui/RenderQueue.qml post-Task-2/3).
LOCS = {
    "LONG": [("RenderQueue.qml", 290)],
    "ANAM": [("RenderQueue.qml", 2354)],
    "PAIR": [("RenderQueue.qml", 1110)],
}

# Per-language translations. Keys: LONG (long markdown), ANAM (one-liner), PAIR (menu title).
# All LONG strings use \n\n after each bold section heading (3 places each), matching the EN source.
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "LONG": (
            "**Hromadná synchronizace neposkytla spolehlivý výsledek.** Zkontrolujte:\n\n"
            "**1. Jsou načtena kalibrační videa**\n\n"
            "Krátká videa (do 10 sekund) natočená současně se souborem gyroskopu.\n\n"
            "**2. Videa a gyroskopy jsou správně spárovány**\n\n"
            "Nespárované video můžete spárovat ručně: klikněte na video pravým tlačítkem → "
            "**\"Spárovat s gyroskopem\"** a vyberte odpovídající soubor gyroskopu.\n\n"
            "**3. Pokud pro daný den nejsou 2 kalibrační videa**\n\n"
            "- Použijte data ze sousedního dne: zkopírujte kalibrační videa a jejich "
            "gyroskopy do složky daného dne, znovu přidejte do fronty a ručně spárujte.\n"
            "- Nebo znovu natočte kalibrační videa a přidejte je do fronty."
        ),
        "ANAM": "%1 videí použije anamorfní objektiv",
        "PAIR": "Spárovat s gyroskopem",
    },
    "da": {
        "LONG": (
            "**Batchsynkronisering gav ikke et pålideligt resultat.** Kontroller:\n\n"
            "**1. Kalibreringsvideoer er indlæst**\n\n"
            "Korte videoer (under 10 sekunder) optaget samtidig med gyrofilen.\n\n"
            "**2. Videoer og gyrofiler er parret korrekt**\n\n"
            "Du kan parre manuelt: højreklik på videoen → **\"Par med gyro\"** og vælg "
            "den tilhørende gyrofil.\n\n"
            "**3. Hvis der ikke er 2 kalibreringsvideoer for dagen**\n\n"
            "- Lån synkroniseringsdata fra dagen før/efter: kopier kalibreringsvideoerne "
            "og deres gyrofiler til dagens mappe, tilføj dem igen, og par manuelt.\n"
            "- Eller optag nye kalibreringsvideoer og tilføj dem igen."
        ),
        "ANAM": "%1 videoer bruger anamorf optik",
        "PAIR": "Par med gyro",
    },
    "de": {
        "LONG": (
            "**Die Stapelsynchronisation lieferte kein zuverlässiges Ergebnis.** Bitte prüfe:\n\n"
            "**1. Kalibriervideos sind geladen**\n\n"
            "Kurze Videos (unter 10 Sekunden), gleichzeitig mit der Gyro-Datei aufgenommen.\n\n"
            "**2. Videos und Gyro-Dateien sind korrekt zugeordnet**\n\n"
            "Nicht zugeordnete Videos kannst du manuell zuordnen: Rechtsklick auf das Video → "
            "**\"Mit Gyro koppeln\"** und die passende Gyro-Datei auswählen.\n\n"
            "**3. Wenn für den Tag keine 2 Kalibriervideos vorhanden sind**\n\n"
            "- Synchronisationsdaten vom Vortag/Folgetag übernehmen: Kalibriervideos und "
            "Gyro-Dateien in den Tagesordner kopieren, neu in die Warteschlange laden und "
            "manuell zuordnen.\n"
            "- Oder neue Kalibriervideos drehen und in die Warteschlange laden."
        ),
        "ANAM": "%1 Videos verwenden ein Anamorphot-Objektiv",
        "PAIR": "Mit Gyro koppeln",
    },
    "el": {
        "LONG": (
            "**Ο μαζικός συγχρονισμός δεν παρήγαγε αξιόπιστο αποτέλεσμα.** Ελέγξτε:\n\n"
            "**1. Έχουν φορτωθεί βίντεο βαθμονόμησης**\n\n"
            "Σύντομα βίντεο (κάτω από 10 δευτερόλεπτα) που έχουν εγγραφεί ταυτόχρονα "
            "με το αρχείο γυροσκοπίου.\n\n"
            "**2. Τα βίντεο και τα γυροσκόπια είναι σωστά αντιστοιχισμένα**\n\n"
            "Για μη αντιστοιχισμένα βίντεο, μπορείτε να αντιστοιχίσετε χειροκίνητα: "
            "δεξί κλικ στο βίντεο → **\"Σύζευξη με γυροσκόπιο\"** και επιλέξτε το αντίστοιχο αρχείο.\n\n"
            "**3. Αν δεν υπάρχουν 2 βίντεο βαθμονόμησης για την ημέρα**\n\n"
            "- Δανειστείτε δεδομένα από την προηγούμενη/επόμενη ημέρα: αντιγράψτε τα "
            "βίντεο βαθμονόμησης και τα γυροσκόπια στον φάκελο της ημέρας, προσθέστε ξανά "
            "στη σειρά και αντιστοιχίστε χειροκίνητα.\n"
            "- Ή ξανακινηματογραφήστε βίντεο βαθμονόμησης και προσθέστε τα ξανά."
        ),
        "ANAM": "%1 βίντεο θα χρησιμοποιήσουν ανάμορφο φακό",
        "PAIR": "Σύζευξη με γυροσκόπιο",
    },
    "es": {
        "LONG": (
            "**La sincronización por lotes no produjo un resultado fiable.** Comprueba:\n\n"
            "**1. Los vídeos de calibración están cargados**\n\n"
            "Vídeos cortos (menos de 10 segundos) grabados simultáneamente con el archivo del giroscopio.\n\n"
            "**2. Los vídeos y los archivos de giroscopio están correctamente emparejados**\n\n"
            "Para los vídeos sin emparejar, puedes hacerlo manualmente: clic derecho en el vídeo → "
            "**\"Emparejar con giroscopio\"** y selecciona el archivo correspondiente.\n\n"
            "**3. Si no hay 2 vídeos de calibración para el día**\n\n"
            "- Toma los datos de sincronización del día anterior o posterior: copia los vídeos "
            "de calibración y sus archivos de giroscopio en la carpeta del día, vuelve a "
            "añadirlos a la cola y empareja manualmente.\n"
            "- O graba nuevos vídeos de calibración y añádelos a la cola."
        ),
        "ANAM": "%1 vídeo(s) usarán óptica anamórfica",
        "PAIR": "Emparejar con giroscopio",
    },
    "fi": {
        "LONG": (
            "**Erien synkronointi ei tuottanut luotettavaa tulosta.** Tarkista:\n\n"
            "**1. Kalibrointivideot on ladattu**\n\n"
            "Lyhyet videot (alle 10 sekuntia), jotka on tallennettu samanaikaisesti gyrotiedoston kanssa.\n\n"
            "**2. Videot ja gyrotiedostot on yhdistetty oikein**\n\n"
            "Voit yhdistää manuaalisesti: napsauta videota hiiren oikealla → "
            "**\"Yhdistä gyroon\"** ja valitse vastaava gyrotiedosto.\n\n"
            "**3. Jos päivälle ei ole 2 kalibrointivideota**\n\n"
            "- Käytä edellisen/seuraavan päivän synkronointidataa: kopioi kalibrointivideot "
            "ja niiden gyrotiedostot päivän kansioon, lisää uudelleen jonoon ja yhdistä manuaalisesti.\n"
            "- Tai kuvaa uusi kalibrointivideo ja lisää se jonoon."
        ),
        "ANAM": "%1 videota käyttää anamorfista objektiivia",
        "PAIR": "Yhdistä gyroon",
    },
    "fr": {
        "LONG": (
            "**La synchronisation par lot n'a pas produit de résultat fiable.** Veuillez vérifier :\n\n"
            "**1. Les vidéos de calibration sont chargées**\n\n"
            "Vidéos courtes (moins de 10 secondes) enregistrées simultanément avec le fichier gyroscopique.\n\n"
            "**2. Les vidéos et fichiers gyroscopiques sont correctement appariés**\n\n"
            "Pour toute vidéo non appariée, vous pouvez l'apparier manuellement : clic droit sur la vidéo → "
            "**\"Apparier avec gyro\"**, puis sélectionnez le fichier correspondant.\n\n"
            "**3. S'il n'y a pas 2 vidéos de calibration pour la journée**\n\n"
            "- Empruntez les données de synchronisation de la veille ou du lendemain : copiez les vidéos "
            "de calibration et leurs fichiers gyroscopiques dans le dossier du jour, réajoutez à la file "
            "et appariez manuellement.\n"
            "- Ou refilmer des vidéos de calibration et les rajouter à la file."
        ),
        "ANAM": "%1 vidéo(s) utiliseront une optique anamorphique",
        "PAIR": "Apparier avec gyro",
    },
    "gl": {
        "LONG": (
            "**A sincronización por lotes non produciu un resultado fiable.** Comproba:\n\n"
            "**1. Os vídeos de calibración están cargados**\n\n"
            "Vídeos curtos (menos de 10 segundos) gravados ao mesmo tempo que o ficheiro do xiroscopio.\n\n"
            "**2. Os vídeos e os ficheiros do xiroscopio están correctamente emparellados**\n\n"
            "Para vídeos sen emparellar, podes facelo manualmente: clic dereito no vídeo → "
            "**\"Emparellar co xiroscopio\"** e selecciona o ficheiro correspondente.\n\n"
            "**3. Se non hai 2 vídeos de calibración para o día**\n\n"
            "- Toma os datos de sincronización do día anterior ou posterior: copia os vídeos de "
            "calibración e os seus ficheiros do xiroscopio ao cartafol do día, engade de novo á cola "
            "e emparella manualmente.\n"
            "- Ou grava novos vídeos de calibración e engádeos á cola."
        ),
        "ANAM": "%1 vídeo(s) usarán óptica anamórfica",
        "PAIR": "Emparellar co xiroscopio",
    },
    "id": {
        "LONG": (
            "**Sinkronisasi batch tidak menghasilkan hasil yang andal.** Mohon periksa:\n\n"
            "**1. Video kalibrasi telah dimuat**\n\n"
            "Video pendek (kurang dari 10 detik) yang direkam bersamaan dengan file giroskop.\n\n"
            "**2. Video dan file giroskop telah dipasangkan dengan benar**\n\n"
            "Untuk video yang belum dipasangkan, Anda dapat memasangkannya secara manual: "
            "klik kanan pada video → **\"Pasangkan dengan Giroskop\"**, lalu pilih file giroskop yang sesuai.\n\n"
            "**3. Jika tidak ada 2 video kalibrasi untuk hari itu**\n\n"
            "- Pinjam data sinkronisasi dari hari sebelumnya/berikutnya: salin video kalibrasi "
            "dan file giroskopnya ke folder hari ini, tambahkan kembali ke antrean, lalu pasangkan secara manual.\n"
            "- Atau rekam ulang video kalibrasi dan tambahkan kembali ke antrean."
        ),
        "ANAM": "%1 video akan menggunakan lensa Anamorphic",
        "PAIR": "Pasangkan dengan Giroskop",
    },
    "it": {
        "LONG": (
            "**La sincronizzazione in batch non ha prodotto un risultato affidabile.** Verifica:\n\n"
            "**1. I video di calibrazione sono caricati**\n\n"
            "Video brevi (meno di 10 secondi) registrati contemporaneamente al file del giroscopio.\n\n"
            "**2. I video e i file del giroscopio sono correttamente accoppiati**\n\n"
            "Per i video non accoppiati puoi farlo manualmente: clic destro sul video → "
            "**\"Abbina al giroscopio\"** e seleziona il file corrispondente.\n\n"
            "**3. Se non ci sono 2 video di calibrazione per la giornata**\n\n"
            "- Prendi i dati di sincronizzazione dal giorno precedente/successivo: copia i video "
            "di calibrazione e i relativi file del giroscopio nella cartella della giornata, "
            "riaggiungi alla coda e abbina manualmente.\n"
            "- Oppure registra nuovi video di calibrazione e riaggiungili alla coda."
        ),
        "ANAM": "%1 video useranno un obiettivo Anamorphic",
        "PAIR": "Abbina al giroscopio",
    },
    "ja": {
        "LONG": (
            "**バッチ同期で信頼できる結果が得られませんでした。** 以下を確認してください：\n\n"
            "**1. 校正用ビデオが読み込まれている**\n\n"
            "ジャイロファイルと同時に撮影された 10 秒未満の短いビデオ。\n\n"
            "**2. ビデオとジャイロファイルが正しく対応付けられている**\n\n"
            "未対応のビデオは手動で対応付けできます：ビデオを右クリック → "
            "**「ジャイロと対応付け」** で該当のジャイロファイルを選択します。\n\n"
            "**3. その日の校正用ビデオが 2 本ない場合**\n\n"
            "- 前日／翌日の対時データを流用：校正用ビデオとそのジャイロファイルを当日のフォルダーに"
            "コピーし、キューに追加し直して上記の手順で手動対応付けします。\n"
            "- または校正用ビデオを撮り直してキューに追加します。"
        ),
        "ANAM": "%1 本のビデオで Anamorphic レンズを使用します",
        "PAIR": "ジャイロと対応付け",
    },
    "ko": {
        "LONG": (
            "**일괄 동기화에서 신뢰할 수 있는 결과를 얻지 못했습니다.** 다음을 확인하세요:\n\n"
            "**1. 캘리브레이션 영상이 로드되었는지**\n\n"
            "자이로 파일과 동시에 녹화된 10 초 미만의 짧은 영상.\n\n"
            "**2. 영상과 자이로 파일이 올바르게 짝지어졌는지**\n\n"
            "짝이 없는 영상은 수동으로 연결할 수 있습니다: 영상에서 마우스 오른쪽 버튼 → "
            "**\"자이로와 페어링\"** 후 해당 자이로 파일을 선택하세요.\n\n"
            "**3. 그날의 캘리브레이션 영상이 2 개가 없는 경우**\n\n"
            "- 전날/다음날의 시간 동기화 데이터를 사용: 캘리브레이션 영상과 자이로 파일을 "
            "그날 폴더로 복사한 뒤, 큐에 다시 추가하고 위 방법으로 수동 페어링합니다.\n"
            "- 또는 새 캘리브레이션 영상을 다시 촬영해 큐에 추가합니다."
        ),
        "ANAM": "%1 개 영상이 Anamorphic 렌즈를 사용합니다",
        "PAIR": "자이로와 페어링",
    },
    "no": {
        "LONG": (
            "**Batchsynkronisering ga ikke et pålitelig resultat.** Kontroller:\n\n"
            "**1. Kalibreringsvideoer er lastet inn**\n\n"
            "Korte videoer (under 10 sekunder) tatt opp samtidig med gyrofilen.\n\n"
            "**2. Videoer og gyrofiler er riktig paret**\n\n"
            "Du kan pare manuelt: høyreklikk på videoen → **\"Par med gyro\"** og velg "
            "tilhørende gyrofil.\n\n"
            "**3. Hvis det ikke finnes 2 kalibreringsvideoer for dagen**\n\n"
            "- Lån synkroniseringsdata fra forrige/neste dag: kopier kalibreringsvideoene og "
            "gyrofilene til dagens mappe, legg dem til i køen igjen og par manuelt.\n"
            "- Eller ta nye kalibreringsvideoer og legg dem til i køen."
        ),
        "ANAM": "%1 video(er) bruker anamorfisk objektiv",
        "PAIR": "Par med gyro",
    },
    "pl": {
        "LONG": (
            "**Synchronizacja wsadowa nie dała wiarygodnego wyniku.** Sprawdź:\n\n"
            "**1. Filmy kalibracyjne są wczytane**\n\n"
            "Krótkie filmy (poniżej 10 sekund) nagrane równocześnie z plikiem żyroskopu.\n\n"
            "**2. Filmy i pliki żyroskopu są poprawnie sparowane**\n\n"
            "Niesparowany film możesz sparować ręcznie: kliknij prawym przyciskiem na film → "
            "**\"Sparuj z żyroskopem\"** i wybierz odpowiedni plik żyroskopu.\n\n"
            "**3. Jeśli na dany dzień nie ma 2 filmów kalibracyjnych**\n\n"
            "- Skorzystaj z danych z poprzedniego/następnego dnia: skopiuj filmy kalibracyjne i ich "
            "pliki żyroskopu do folderu danego dnia, dodaj ponownie do kolejki i sparuj ręcznie.\n"
            "- Lub nagraj nowe filmy kalibracyjne i dodaj je do kolejki."
        ),
        "ANAM": "%1 filmów użyje obiektywu anamorficznego",
        "PAIR": "Sparuj z żyroskopem",
    },
    "pt": {
        "LONG": (
            "**A sincronização em lote não produziu um resultado fiável.** Verifique:\n\n"
            "**1. Os vídeos de calibração estão carregados**\n\n"
            "Vídeos curtos (menos de 10 segundos) gravados ao mesmo tempo que o ficheiro do giroscópio.\n\n"
            "**2. Os vídeos e os ficheiros de giroscópio estão corretamente emparelhados**\n\n"
            "Para vídeos não emparelhados, pode emparelhar manualmente: clique com o botão direito → "
            "**\"Emparelhar com giroscópio\"** e selecione o ficheiro correspondente.\n\n"
            "**3. Se não houver 2 vídeos de calibração para o dia**\n\n"
            "- Utilize os dados de sincronização do dia anterior/seguinte: copie os vídeos de calibração "
            "e os respetivos ficheiros de giroscópio para a pasta do dia, volte a adicionar à fila e "
            "empareie manualmente.\n"
            "- Ou volte a gravar os vídeos de calibração e adicione-os à fila."
        ),
        "ANAM": "%1 vídeo(s) irão usar lente anamórfica",
        "PAIR": "Emparelhar com giroscópio",
    },
    "pt_BR": {
        "LONG": (
            "**A sincronização em lote não produziu um resultado confiável.** Verifique:\n\n"
            "**1. Os vídeos de calibração estão carregados**\n\n"
            "Vídeos curtos (menos de 10 segundos) gravados simultaneamente com o arquivo do giroscópio.\n\n"
            "**2. Os vídeos e arquivos de giroscópio estão pareados corretamente**\n\n"
            "Para vídeos não pareados, você pode parear manualmente: clique com o botão direito no vídeo → "
            "**\"Parear com giroscópio\"** e selecione o arquivo correspondente.\n\n"
            "**3. Se não houver 2 vídeos de calibração para o dia**\n\n"
            "- Use os dados de sincronização do dia anterior/seguinte: copie os vídeos de calibração e seus "
            "arquivos de giroscópio para a pasta do dia, adicione novamente à fila e pareie manualmente.\n"
            "- Ou regrave os vídeos de calibração e adicione-os à fila."
        ),
        "ANAM": "%1 vídeo(s) usarão lente anamórfica",
        "PAIR": "Parear com giroscópio",
    },
    "ru": {
        "LONG": (
            "**Пакетная синхронизация не дала надёжного результата.** Проверьте:\n\n"
            "**1. Калибровочные видео загружены**\n\n"
            "Короткие ролики (менее 10 секунд), записанные одновременно с файлом гироскопа.\n\n"
            "**2. Видео и файлы гироскопа правильно сопоставлены**\n\n"
            "Для несопоставленных видео можно сделать это вручную: щёлкните правой кнопкой по видео → "
            "**\"Сопоставить с гироскопом\"** и выберите соответствующий файл.\n\n"
            "**3. Если для данного дня нет 2 калибровочных видео**\n\n"
            "- Возьмите данные синхронизации с предыдущего/следующего дня: скопируйте калибровочные "
            "видео и файлы гироскопа в папку текущего дня, повторно добавьте в очередь и сопоставьте вручную.\n"
            "- Или переснимите калибровочные видео и добавьте их в очередь."
        ),
        "ANAM": "В %1 видео будет использован анаморфотный объектив",
        "PAIR": "Сопоставить с гироскопом",
    },
    "sk": {
        "LONG": (
            "**Hromadná synchronizácia neposkytla spoľahlivý výsledok.** Skontrolujte:\n\n"
            "**1. Sú načítané kalibračné videá**\n\n"
            "Krátke videá (do 10 sekúnd) natočené súčasne so súborom gyroskopu.\n\n"
            "**2. Videá a súbory gyroskopu sú správne spárované**\n\n"
            "Nespárované video môžete spárovať ručne: pravým tlačidlom na video → "
            "**\"Spárovať s gyroskopom\"** a vyberte zodpovedajúci súbor gyroskopu.\n\n"
            "**3. Ak pre daný deň nie sú 2 kalibračné videá**\n\n"
            "- Použite údaje zo susedného dňa: skopírujte kalibračné videá a ich gyroskopy do priečinka "
            "daného dňa, znova pridajte do frontu a ručne spárujte.\n"
            "- Alebo natočte nové kalibračné videá a pridajte ich do frontu."
        ),
        "ANAM": "%1 videí použije anamorfný objektív",
        "PAIR": "Spárovať s gyroskopom",
    },
    "tr": {
        "LONG": (
            "**Toplu senkronizasyon güvenilir bir sonuç vermedi.** Lütfen şunları kontrol edin:\n\n"
            "**1. Kalibrasyon videoları yüklendi mi**\n\n"
            "Jiroskop dosyasıyla eş zamanlı olarak çekilmiş, 10 saniyenin altındaki kısa videolar.\n\n"
            "**2. Videolar ve jiroskop dosyaları doğru eşleştirildi mi**\n\n"
            "Eşleşmeyen videoları manuel olarak eşleştirebilirsiniz: videoya sağ tıklayın → "
            "**\"Jiroskopla eşleştir\"** ve ilgili jiroskop dosyasını seçin.\n\n"
            "**3. O gün için 2 kalibrasyon videosu yoksa**\n\n"
            "- Önceki/sonraki gün senkron verilerini kullanın: kalibrasyon videolarını ve jiroskop "
            "dosyalarını o günün klasörüne kopyalayın, kuyruğa yeniden ekleyin ve manuel eşleştirin.\n"
            "- Veya yeni bir kalibrasyon videosu çekip kuyruğa ekleyin."
        ),
        "ANAM": "%1 video Anamorfik mercek kullanacak",
        "PAIR": "Jiroskopla eşleştir",
    },
    "uk": {
        "LONG": (
            "**Пакетна синхронізація не дала надійного результату.** Перевірте:\n\n"
            "**1. Калібрувальні відео завантажено**\n\n"
            "Короткі відео (до 10 секунд), записані одночасно з файлом гіроскопа.\n\n"
            "**2. Відео та файли гіроскопа правильно зіставлені**\n\n"
            "Для незіставлених відео можна зробити це вручну: клацніть правою кнопкою на відео → "
            "**\"Зіставити з гіроскопом\"** і виберіть відповідний файл.\n\n"
            "**3. Якщо для цього дня немає 2 калібрувальних відео**\n\n"
            "- Використайте дані синхронізації з попереднього/наступного дня: скопіюйте калібрувальні "
            "відео та їхні файли гіроскопа до папки відповідного дня, знову додайте до черги та "
            "зіставте вручну.\n"
            "- Або перезніміть калібрувальні відео та додайте до черги."
        ),
        "ANAM": "У %1 відео буде використано анаморфотний об'єктив",
        "PAIR": "Зіставити з гіроскопом",
    },
    "zh_CN": {
        "LONG": (
            "**批量同步未取得可靠结果**，请检查以下事项：\n\n"
            "**1. 校准视频已加载**\n\n"
            "与陀螺仪同步录制的短视频，时长应小于 10 秒。\n\n"
            "**2. 视频与陀螺仪已正确配对**\n\n"
            "如有未自动配对的视频，可手动配对：在视频上**右键 →「与陀螺仪配对」**，"
            "选择对应的陀螺仪文件。\n\n"
            "**3. 如果当天没有 2 条校准视频**\n\n"
            "- 借用前一天/后一天的对时数据：将校准视频与对应陀螺仪文件拷贝到当天文件夹，"
            "重新加入队列后按上面方式手动配对\n"
            "- 或补拍校准视频，重新加入队列"
        ),
        "ANAM": "%1 个视频将使用变宽镜头",
        "PAIR": "与陀螺仪配对",
    },
    "zh_TW": {
        "LONG": (
            "**批次同步未取得可靠結果**，請檢查以下事項：\n\n"
            "**1. 校準影片已載入**\n\n"
            "與陀螺儀同步錄製的短影片，時長應小於 10 秒。\n\n"
            "**2. 影片與陀螺儀已正確配對**\n\n"
            "如有未自動配對的影片，可手動配對：在影片上**右鍵 →「與陀螺儀配對」**，"
            "選擇對應的陀螺儀檔案。\n\n"
            "**3. 如果當天沒有 2 條校準影片**\n\n"
            "- 借用前一天／後一天的對時資料：將校準影片與對應陀螺儀檔案複製到當天資料夾，"
            "重新加入佇列後按上面方式手動配對\n"
            "- 或補拍校準影片，重新加入佇列"
        ),
        "ANAM": "%1 個影片將使用變寬鏡頭",
        "PAIR": "與陀螺儀配對",
    },
}


def make_message(locs: list[tuple[str, int]], source: str, translation: str | None) -> str:
    loc_lines = "\n".join(
        f'        <location filename="../../src/ui/{q}" line="{line}"/>'
        for q, line in locs
    )
    if translation is None:
        trans_tag = '<translation type="unfinished"></translation>'
    else:
        trans_tag = f'<translation>{translation}</translation>'
    return (
        "    <message>\n"
        f"{loc_lines}\n"
        f"        <source>{source}</source>\n"
        f"        {trans_tag}\n"
        "    </message>\n"
    )


def remove_old_message(content: str) -> str:
    """Remove the obsolete AllYellow <message> block (idempotent)."""
    # Use + for one-or-more location lines (covers both single and multi-location blocks).
    pattern = re.compile(
        r"    <message>\n"
        r"(?:        <location[^\n]*/>\n)+"
        r"        <source>" + re.escape(OLD_SOURCE) + r"</source>\n"
        r"        <translation[^>]*>.*?</translation>\n"
        r"    </message>\n",
        re.DOTALL,
    )
    return pattern.sub("", content)


def patch_file(path: pathlib.Path, t: dict[str, str] | None) -> str:
    content = path.read_text(encoding="utf-8")

    # Idempotency: skip if the new long source is already present.
    if "**Batch synchronization did not produce a reliable result.**" in content:
        return f"SKIP {path.name} (already patched)"

    # 1. Remove obsolete AllYellow message block.
    content = remove_old_message(content)

    # 2. Build new message blocks.
    def tx(key: str) -> str | None:
        return None if t is None else t[key]

    rq_block = (
        make_message(LOCS["LONG"], SOURCES["LONG"], tx("LONG"))
        + make_message(LOCS["ANAM"], SOURCES["ANAM"], tx("ANAM"))
        + make_message(LOCS["PAIR"], SOURCES["PAIR"], tx("PAIR"))
    )

    # 3. Inject after the RenderQueue context opener.
    needle = "    <name>RenderQueue</name>\n"
    if needle not in content:
        return f"FAIL {path.name}: <RenderQueue> context not found"
    content = content.replace(needle, needle + rq_block, 1)

    path.write_text(content, encoding="utf-8")
    return f"OK   {path.name}"


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    if not base.is_dir():
        print(f"Translation dir missing: {base}", file=sys.stderr)
        return 1

    # Source ts: unfinished translations.
    src = base / "gyroflow.ts"
    print(patch_file(src, None))

    for lang in TRANS:
        path = base / f"{lang}.ts"
        if not path.is_file():
            print(f"MISS {path.name} (file not found)")
            continue
        print(patch_file(path, TRANS[lang]))
    return 0


if __name__ == "__main__":
    sys.exit(main())
