# SPDX-License-Identifier: GPL-3.0-or-later
"""Insert optical correction messages into MotionData in all 23 TS files.

Run from the repository root, then compile with Qt 6.7.3 MinGW lrelease.
Existing messages and file line endings are preserved; repeated runs are a no-op.
"""
from __future__ import annotations

import pathlib
import re
import sys
import xml.etree.ElementTree as ET

QML = "../../src/ui/menu/MotionData.qml"
SOURCES = (
    "Optical correction",
    "Measure the camera rotation from the video itself and correct the motion data where they disagree. Useful when vibrations corrupt the gyro data, e.g. on a hard-mounted FPV camera. The analysis goes through every frame of the selected trim range.",
    "Analyze",
    "Clear",
    "Click Analyze to measure the motion from the video.",
    "The motion data, the sync, the lens or the rolling shutter changed since the analysis. Analyze again to apply the correction.",
    "Analyze again to apply the new strength.",
    "Measured in %1 of %2 frames, correction %3°",
    "Ignore motion data from the file",
    "Measure all of the camera motion from the video, like for a file without motion data, instead of correcting the motion data. For motion data too broken to correct, e.g. a gyro that glitches or saturates for seconds at a time.",
    "Strength",
    "How far the correction may take the motion data from what it says. Lower values only correct small, fast errors like vibration. Higher values let the image override the motion data also where it's off by a lot or for longer, like gyro glitches, but follow the image's own mistakes (moving objects, water) more too.",
)
LINES = (321, 322, 346, 353, 366, 368, 369, 371, 375, 376, 381, 386)

# Each language supplies one translation per source, in the order above.
TRANSLATIONS = {
    "cs": """Optická korekce
Změří otáčení kamery přímo z videa a opraví pohybová data tam, kde se rozcházejí. Hodí se při poškození dat gyroskopu vibracemi, například u pevně připevněné FPV kamery. Analýza zpracuje každý snímek vybraného rozsahu ořezu.
Analyzovat
Vymazat
Kliknutím na Analyzovat změříte pohyb z videa.
Od analýzy se změnila pohybová data, synchronizace, objektiv nebo řádková závěrka. Pro použití korekce spusťte analýzu znovu.
Pro použití nové intenzity spusťte analýzu znovu.
Změřeno v %1 z %2 snímků, korekce %3°
Ignorovat pohybová data ze souboru
Změří veškerý pohyb kamery z videa, stejně jako u souboru bez pohybových dat, namísto opravování pohybových dat. Určeno pro data příliš poškozená na opravu, například když gyroskop selhává nebo se saturuje na několik sekund.
Intenzita
Jak moc se mohou opravená pohybová data odchýlit od původních hodnot. Nižší hodnoty opravují jen malé, rychlé chyby, jako jsou vibrace. Vyšší hodnoty umožňují obrazu převážit nad pohybovými daty i při velkých či delších odchylkách, například při chybách gyroskopu, ale také více následují chyby samotného obrazu (pohybující se objekty, voda).""",
    "da": """Optisk korrektion
Mål kamerarotationen direkte fra videoen, og korriger bevægelsesdataene, hvor de ikke stemmer overens. Nyttigt, når vibrationer ødelægger gyrodataene, f.eks. på et fastmonteret FPV-kamera. Analysen gennemgår hvert billede i det valgte beskæringsinterval.
Analyser
Ryd
Klik på Analyser for at måle bevægelsen fra videoen.
Bevægelsesdataene, synkroniseringen, objektivet eller den rullende lukker er ændret siden analysen. Analyser igen for at anvende korrektionen.
Analyser igen for at anvende den nye styrke.
Målt i %1 af %2 billeder, korrektion %3°
Ignorer bevægelsesdata fra filen
Mål al kamerabevægelse fra videoen, som for en fil uden bevægelsesdata, i stedet for at korrigere bevægelsesdataene. Til bevægelsesdata, der er for beskadigede til at blive korrigeret, f.eks. en gyro, der fejler eller mættes i flere sekunder ad gangen.
Styrke
Hvor langt korrektionen må flytte bevægelsesdataene fra deres oprindelige værdier. Lavere værdier korrigerer kun små, hurtige fejl som vibrationer. Højere værdier lader billedet tilsidesætte bevægelsesdataene også ved store eller længerevarende afvigelser, som gyrofejl, men følger også billedets egne fejl (bevægelige objekter, vand) mere.""",
    "de": """Optische Korrektur
Misst die Kameradrehung direkt aus dem Video und korrigiert die Bewegungsdaten dort, wo sie voneinander abweichen. Nützlich, wenn Vibrationen die Gyrodaten verfälschen, z. B. bei einer starr montierten FPV-Kamera. Die Analyse verarbeitet jedes Bild des ausgewählten Schnittbereichs.
Analysieren
Löschen
Klicken Sie auf Analysieren, um die Bewegung aus dem Video zu messen.
Die Bewegungsdaten, die Synchronisierung, das Objektiv oder der Rolling Shutter wurden seit der Analyse geändert. Analysieren Sie erneut, um die Korrektur anzuwenden.
Analysieren Sie erneut, um die neue Stärke anzuwenden.
In %1 von %2 Bildern gemessen, Korrektur %3°
Bewegungsdaten aus der Datei ignorieren
Misst die gesamte Kamerabewegung aus dem Video, wie bei einer Datei ohne Bewegungsdaten, statt die Bewegungsdaten zu korrigieren. Für Bewegungsdaten, die zu stark beschädigt sind, etwa wenn ein Gyroskop mehrere Sekunden lang fehlerhafte oder gesättigte Werte liefert.
Stärke
Wie weit die Korrektur die Bewegungsdaten von ihren ursprünglichen Werten abweichen lassen darf. Niedrigere Werte korrigieren nur kleine, schnelle Fehler wie Vibrationen. Höhere Werte lassen das Bild die Bewegungsdaten auch bei großen oder längeren Abweichungen, etwa Gyrofehlern, übersteuern, folgen aber auch stärker den Fehlern des Bildes selbst (bewegte Objekte, Wasser).""",
    "el": """Οπτική διόρθωση
Μετρά την περιστροφή της κάμερας από το ίδιο το βίντεο και διορθώνει τα δεδομένα κίνησης όπου διαφωνούν. Χρήσιμο όταν οι δονήσεις αλλοιώνουν τα δεδομένα του γυροσκοπίου, π.χ. σε άκαμπτα στερεωμένη κάμερα FPV. Η ανάλυση επεξεργάζεται κάθε καρέ του επιλεγμένου εύρους περικοπής.
Ανάλυση
Εκκαθάριση
Κάντε κλικ στην Ανάλυση για να μετρήσετε την κίνηση από το βίντεο.
Τα δεδομένα κίνησης, ο συγχρονισμός, ο φακός ή το κυλιόμενο κλείστρο άλλαξαν μετά την ανάλυση. Αναλύστε ξανά για να εφαρμόσετε τη διόρθωση.
Αναλύστε ξανά για να εφαρμόσετε τη νέα ένταση.
Μετρήθηκε σε %1 από %2 καρέ, διόρθωση %3°
Παράβλεψη δεδομένων κίνησης από το αρχείο
Μετρά όλη την κίνηση της κάμερας από το βίντεο, όπως σε αρχείο χωρίς δεδομένα κίνησης, αντί να διορθώνει τα δεδομένα κίνησης. Για δεδομένα πολύ κατεστραμμένα ώστε να διορθωθούν, π.χ. γυροσκόπιο που παρουσιάζει σφάλματα ή κορεσμό για αρκετά δευτερόλεπτα.
Ένταση
Πόσο μπορεί η διόρθωση να απομακρύνει τα δεδομένα κίνησης από τις αρχικές τιμές τους. Οι χαμηλότερες τιμές διορθώνουν μόνο μικρά, γρήγορα σφάλματα όπως οι δονήσεις. Οι υψηλότερες τιμές επιτρέπουν στην εικόνα να υπερισχύει των δεδομένων κίνησης και σε μεγάλες ή παρατεταμένες αποκλίσεις, όπως σφάλματα γυροσκοπίου, αλλά ακολουθούν περισσότερο και τα σφάλματα της ίδιας της εικόνας (κινούμενα αντικείμενα, νερό).""",
    "es": """Corrección óptica
Mide la rotación de la cámara a partir del propio vídeo y corrige los datos de movimiento donde no coinciden. Útil cuando las vibraciones alteran los datos del giroscopio, por ejemplo, en una cámara FPV montada de forma rígida. El análisis procesa cada fotograma del intervalo de recorte seleccionado.
Analizar
Borrar
Haz clic en Analizar para medir el movimiento a partir del vídeo.
Los datos de movimiento, la sincronización, el objetivo o el obturador rodante han cambiado desde el análisis. Analiza de nuevo para aplicar la corrección.
Analiza de nuevo para aplicar la nueva intensidad.
Medido en %1 de %2 fotogramas, corrección %3°
Ignorar los datos de movimiento del archivo
Mide todo el movimiento de la cámara a partir del vídeo, como en un archivo sin datos de movimiento, en lugar de corregir esos datos. Para datos demasiado dañados para corregirlos, por ejemplo, un giroscopio que falla o se satura durante varios segundos seguidos.
Intensidad
Cuánto puede alejar la corrección los datos de movimiento de sus valores originales. Los valores bajos solo corrigen errores pequeños y rápidos, como las vibraciones. Los valores altos permiten que la imagen prevalezca sobre los datos de movimiento también ante desviaciones grandes o prolongadas, como fallos del giroscopio, pero siguen más los errores de la propia imagen (objetos en movimiento, agua).""",
    "fi": """Optinen korjaus
Mittaa kameran kierron itse videosta ja korjaa liiketiedot kohdissa, joissa ne poikkeavat toisistaan. Hyödyllinen, kun tärinä vääristää gyroskoopin tietoja, esimerkiksi jäykästi kiinnitetyssä FPV-kamerassa. Analyysi käsittelee valitun leikkausalueen jokaisen ruudun.
Analysoi
Tyhjennä
Mittaa liike videosta napsauttamalla Analysoi.
Liiketiedot, synkronointi, objektiivi tai vierivä suljin ovat muuttuneet analyysin jälkeen. Analysoi uudelleen ottaaksesi korjauksen käyttöön.
Analysoi uudelleen ottaaksesi uuden voimakkuuden käyttöön.
Mitattu %1 ruudussa %2 ruudusta, korjaus %3°
Ohita tiedoston liiketiedot
Mittaa kameran kaiken liikkeen videosta, kuten tiedostolle ilman liiketietoja, liiketietojen korjaamisen sijaan. Tarkoitettu liian vaurioituneille liiketiedoille, esimerkiksi gyroskoopille, joka antaa virheellisiä tai saturoituneita arvoja useita sekunteja kerrallaan.
Voimakkuus
Kuinka paljon korjaus saa poiketa liiketietojen alkuperäisistä arvoista. Pienemmät arvot korjaavat vain pieniä, nopeita virheitä, kuten tärinää. Suuremmat arvot antavat kuvan ohittaa liiketiedot myös suurissa tai pitkäkestoisissa poikkeamissa, kuten gyroskooppivirheissä, mutta seuraavat enemmän myös kuvan omia virheitä (liikkuvat kohteet, vesi).""",
    "fr": """Correction optique
Mesure la rotation de la caméra à partir de la vidéo elle-même et corrige les données de mouvement là où elles divergent. Utile lorsque les vibrations faussent les données du gyroscope, par exemple sur une caméra FPV fixée rigidement. L'analyse traite chaque image de la plage de découpe sélectionnée.
Analyser
Effacer
Cliquez sur Analyser pour mesurer le mouvement à partir de la vidéo.
Les données de mouvement, la synchronisation, l'objectif ou l'obturateur déroulant ont changé depuis l'analyse. Relancez l'analyse pour appliquer la correction.
Relancez l'analyse pour appliquer la nouvelle intensité.
Mesuré dans %1 images sur %2, correction de %3°
Ignorer les données de mouvement du fichier
Mesure tous les mouvements de la caméra à partir de la vidéo, comme pour un fichier sans données de mouvement, au lieu de corriger ces données. Pour des données trop endommagées pour être corrigées, par exemple un gyroscope qui dysfonctionne ou sature pendant plusieurs secondes.
Intensité
Jusqu'où la correction peut éloigner les données de mouvement de leurs valeurs d'origine. Les valeurs faibles corrigent uniquement les petites erreurs rapides, comme les vibrations. Les valeurs élevées permettent à l'image de remplacer les données de mouvement aussi lors d'écarts importants ou prolongés, comme des défauts du gyroscope, mais suivent davantage les erreurs propres à l'image (objets mobiles, eau).""",
    "gl": """Corrección óptica
Mide a rotación da cámara a partir do propio vídeo e corrixe os datos de movemento onde non coinciden. Útil cando as vibracións alteran os datos do xiroscopio, por exemplo, nunha cámara FPV montada de xeito ríxido. A análise procesa cada fotograma do intervalo de recorte seleccionado.
Analizar
Borrar
Preme en Analizar para medir o movemento a partir do vídeo.
Os datos de movemento, a sincronización, o obxectivo ou o obturador rodante cambiaron desde a análise. Analiza de novo para aplicar a corrección.
Analiza de novo para aplicar a nova intensidade.
Medido en %1 de %2 fotogramas, corrección %3°
Ignorar os datos de movemento do ficheiro
Mide todo o movemento da cámara a partir do vídeo, como nun ficheiro sen datos de movemento, en lugar de corrixir eses datos. Para datos demasiado danados para corrixilos, por exemplo, un xiroscopio que falla ou se satura durante varios segundos seguidos.
Intensidade
Canto pode afastar a corrección os datos de movemento dos seus valores orixinais. Os valores baixos só corrixen erros pequenos e rápidos, como as vibracións. Os valores altos permiten que a imaxe prevaleza sobre os datos de movemento tamén ante desviacións grandes ou prolongadas, como fallos do xiroscopio, pero seguen máis os erros da propia imaxe (obxectos en movemento, auga).""",
    "id": """Koreksi optik
Mengukur rotasi kamera dari video itu sendiri dan mengoreksi data gerakan saat keduanya tidak sesuai. Berguna saat getaran merusak data giroskop, misalnya pada kamera FPV yang dipasang secara kaku. Analisis memproses setiap bingkai dalam rentang pemangkasan yang dipilih.
Analisis
Hapus
Klik Analisis untuk mengukur gerakan dari video.
Data gerakan, sinkronisasi, lensa, atau rana bergulir telah berubah sejak analisis. Analisis ulang untuk menerapkan koreksi.
Analisis ulang untuk menerapkan kekuatan baru.
Diukur pada %1 dari %2 bingkai, koreksi %3°
Abaikan data gerakan dari berkas
Mengukur semua gerakan kamera dari video, seperti pada berkas tanpa data gerakan, alih-alih mengoreksi data gerakan. Untuk data gerakan yang terlalu rusak untuk dikoreksi, misalnya giroskop yang mengalami gangguan atau saturasi selama beberapa detik sekaligus.
Kekuatan
Seberapa jauh koreksi boleh mengubah data gerakan dari nilai aslinya. Nilai rendah hanya mengoreksi kesalahan kecil dan cepat seperti getaran. Nilai tinggi memungkinkan gambar mengesampingkan data gerakan juga saat penyimpangannya besar atau berkepanjangan, seperti gangguan giroskop, tetapi juga lebih mengikuti kesalahan gambar itu sendiri (objek bergerak, air).""",
    "it": """Correzione ottica
Misura la rotazione della fotocamera dal video stesso e corregge i dati di movimento dove non corrispondono. Utile quando le vibrazioni alterano i dati del giroscopio, ad esempio su una fotocamera FPV montata rigidamente. L'analisi elabora ogni fotogramma dell'intervallo di taglio selezionato.
Analizza
Cancella
Fai clic su Analizza per misurare il movimento dal video.
I dati di movimento, la sincronizzazione, l'obiettivo o l'otturatore progressivo sono cambiati dopo l'analisi. Analizza di nuovo per applicare la correzione.
Analizza di nuovo per applicare la nuova intensità.
Misurato in %1 di %2 fotogrammi, correzione %3°
Ignora i dati di movimento del file
Misura tutto il movimento della fotocamera dal video, come per un file senza dati di movimento, invece di correggere tali dati. Per dati troppo danneggiati per essere corretti, ad esempio un giroscopio che presenta errori o si satura per diversi secondi consecutivi.
Intensità
Quanto la correzione può allontanare i dati di movimento dai valori originali. I valori più bassi correggono solo errori piccoli e rapidi, come le vibrazioni. I valori più alti consentono all'immagine di prevalere sui dati di movimento anche in caso di scostamenti grandi o prolungati, come errori del giroscopio, ma seguono maggiormente anche gli errori dell'immagine stessa (oggetti in movimento, acqua).""",
    "ja": """光学補正
映像そのものからカメラの回転を測定し、モーションデータと一致しない部分を補正します。固定されたFPVカメラなど、振動によってジャイロデータが乱れる場合に有効です。解析は選択したトリム範囲の全フレームを処理します。
解析
クリア
「解析」をクリックして、映像から動きを測定してください。
解析後にモーションデータ、同期、レンズ、またはローリングシャッターが変更されました。補正を適用するには再解析してください。
新しい強度を適用するには再解析してください。
%2フレーム中%1フレームで測定、補正%3°
ファイル内のモーションデータを無視
モーションデータを補正する代わりに、モーションデータのないファイルと同様に、カメラのすべての動きを映像から測定します。ジャイロの異常や飽和が数秒続くなど、補正できないほど損傷したモーションデータに使用します。
強度
補正によってモーションデータを元の値からどの程度変更できるかを指定します。低い値では、振動などの小さく速い誤差のみを補正します。高い値では、ジャイロの異常など、大きな誤差や長く続く誤差でも映像がモーションデータより優先されますが、映像自体の誤差（動く物体、水面）にも追従しやすくなります。""",
    "ko": """광학 보정
영상 자체에서 카메라 회전을 측정하고 움직임 데이터와 일치하지 않는 부분을 보정합니다. 단단히 고정된 FPV 카메라 등에서 진동으로 자이로 데이터가 손상될 때 유용합니다. 분석은 선택한 트림 범위의 모든 프레임을 처리합니다.
분석
지우기
분석을 클릭하여 영상에서 움직임을 측정하세요.
분석 이후 움직임 데이터, 동기화, 렌즈 또는 롤링 셔터가 변경되었습니다. 보정을 적용하려면 다시 분석하세요.
새 강도를 적용하려면 다시 분석하세요.
%2개 프레임 중 %1개에서 측정, 보정 %3°
파일의 움직임 데이터 무시
움직임 데이터를 보정하는 대신 움직임 데이터가 없는 파일처럼 영상에서 모든 카메라 움직임을 측정합니다. 자이로 오류나 포화가 수초 동안 지속되는 등 보정할 수 없을 정도로 손상된 움직임 데이터에 사용합니다.
강도
보정이 움직임 데이터를 원래 값에서 얼마나 벗어나게 할 수 있는지를 설정합니다. 낮은 값은 진동처럼 작고 빠른 오류만 보정합니다. 높은 값은 자이로 오류처럼 편차가 크거나 오래 지속될 때도 영상이 움직임 데이터보다 우선하도록 하지만, 영상 자체의 오류(움직이는 물체, 물)도 더 많이 따라갑니다.""",
    "no": """Optisk korrigering
Mål kamerarotasjonen fra selve videoen og korriger bevegelsesdataene der de ikke stemmer overens. Nyttig når vibrasjoner ødelegger gyrodataene, for eksempel på et fastmontert FPV-kamera. Analysen behandler hvert bilde i det valgte trimområdet.
Analyser
Tøm
Klikk på Analyser for å måle bevegelsen fra videoen.
Bevegelsesdataene, synkroniseringen, objektivet eller den rullende lukkeren er endret siden analysen. Analyser på nytt for å bruke korrigeringen.
Analyser på nytt for å bruke den nye styrken.
Målt i %1 av %2 bilder, korrigering %3°
Ignorer bevegelsesdata fra filen
Mål all kamerabevegelse fra videoen, som for en fil uten bevegelsesdata, i stedet for å korrigere bevegelsesdataene. For bevegelsesdata som er for skadet til å korrigeres, for eksempel en gyro som feiler eller mettes i flere sekunder av gangen.
Styrke
Hvor langt korrigeringen kan flytte bevegelsesdataene fra de opprinnelige verdiene. Lavere verdier korrigerer bare små, raske feil som vibrasjoner. Høyere verdier lar bildet overstyre bevegelsesdataene også ved store eller langvarige avvik, som gyrofeil, men følger også bildets egne feil (objekter i bevegelse, vann) mer.""",
    "pl": """Korekcja optyczna
Mierzy obrót kamery bezpośrednio z filmu i koryguje dane ruchu tam, gdzie się różnią. Przydatne, gdy drgania zakłócają dane żyroskopu, np. w sztywno zamontowanej kamerze FPV. Analiza przetwarza każdą klatkę wybranego zakresu przycięcia.
Analizuj
Wyczyść
Kliknij Analizuj, aby zmierzyć ruch z filmu.
Dane ruchu, synchronizacja, obiektyw lub migawka krocząca zmieniły się od czasu analizy. Uruchom analizę ponownie, aby zastosować korekcję.
Uruchom analizę ponownie, aby zastosować nową siłę.
Zmierzono w %1 z %2 klatek, korekcja %3°
Ignoruj dane ruchu z pliku
Mierzy cały ruch kamery z filmu, tak jak dla pliku bez danych ruchu, zamiast korygować dane ruchu. Dla danych zbyt uszkodzonych, aby je skorygować, np. gdy żyroskop działa błędnie lub jest nasycony przez kilka sekund.
Siła
Jak bardzo korekcja może odchylić dane ruchu od ich pierwotnych wartości. Niższe wartości korygują tylko małe, szybkie błędy, takie jak drgania. Wyższe wartości pozwalają obrazowi zastąpić dane ruchu także przy dużych lub długotrwałych odchyleniach, takich jak błędy żyroskopu, ale silniej podążają również za błędami samego obrazu (ruchome obiekty, woda).""",
    "pt": """Correção ótica
Mede a rotação da câmara a partir do próprio vídeo e corrige os dados de movimento onde não coincidem. Útil quando as vibrações corrompem os dados do giroscópio, por exemplo, numa câmara FPV montada rigidamente. A análise processa cada fotograma do intervalo de corte selecionado.
Analisar
Limpar
Clique em Analisar para medir o movimento a partir do vídeo.
Os dados de movimento, a sincronização, a objetiva ou o obturador rolante foram alterados desde a análise. Analise novamente para aplicar a correção.
Analise novamente para aplicar a nova intensidade.
Medido em %1 de %2 fotogramas, correção %3°
Ignorar os dados de movimento do ficheiro
Mede todo o movimento da câmara a partir do vídeo, como num ficheiro sem dados de movimento, em vez de corrigir esses dados. Para dados demasiado danificados para serem corrigidos, por exemplo, um giroscópio que falha ou satura durante vários segundos seguidos.
Intensidade
Quanto a correção pode afastar os dados de movimento dos seus valores originais. Os valores mais baixos corrigem apenas erros pequenos e rápidos, como vibrações. Os valores mais altos permitem que a imagem se sobreponha aos dados de movimento também em desvios grandes ou prolongados, como falhas do giroscópio, mas seguem mais os erros da própria imagem (objetos em movimento, água).""",
    "pt_BR": """Correção óptica
Mede a rotação da câmera a partir do próprio vídeo e corrige os dados de movimento onde não coincidem. Útil quando as vibrações corrompem os dados do giroscópio, por exemplo, em uma câmera FPV montada rigidamente. A análise processa cada quadro do intervalo de corte selecionado.
Analisar
Limpar
Clique em Analisar para medir o movimento a partir do vídeo.
Os dados de movimento, a sincronização, a lente ou o obturador rolante foram alterados desde a análise. Analise novamente para aplicar a correção.
Analise novamente para aplicar a nova intensidade.
Medido em %1 de %2 quadros, correção %3°
Ignorar os dados de movimento do arquivo
Mede todo o movimento da câmera a partir do vídeo, como em um arquivo sem dados de movimento, em vez de corrigir esses dados. Para dados danificados demais para serem corrigidos, por exemplo, um giroscópio que falha ou satura durante vários segundos seguidos.
Intensidade
Quanto a correção pode afastar os dados de movimento dos seus valores originais. Valores mais baixos corrigem apenas erros pequenos e rápidos, como vibrações. Valores mais altos permitem que a imagem prevaleça sobre os dados de movimento também em desvios grandes ou prolongados, como falhas do giroscópio, mas seguem mais os erros da própria imagem (objetos em movimento, água).""",
    "ru": """Оптическая коррекция
Измеряет вращение камеры по самому видео и корректирует данные движения там, где они расходятся. Полезно, когда вибрации искажают данные гироскопа, например на жёстко закреплённой FPV-камере. Анализ обрабатывает каждый кадр выбранного диапазона обрезки.
Анализировать
Очистить
Нажмите «Анализировать», чтобы измерить движение по видео.
После анализа изменились данные движения, синхронизация, объектив или скользящий затвор. Выполните анализ заново, чтобы применить коррекцию.
Выполните анализ заново, чтобы применить новую силу коррекции.
Измерено в %1 из %2 кадров, коррекция %3°
Игнорировать данные движения из файла
Измеряет всё движение камеры по видео, как для файла без данных движения, вместо коррекции этих данных. Для данных, слишком повреждённых для коррекции, например если гироскоп выдаёт сбои или насыщается на несколько секунд подряд.
Сила коррекции
Насколько коррекция может отклонить данные движения от исходных значений. Низкие значения исправляют только небольшие быстрые ошибки, например вибрацию. Высокие значения позволяют изображению заменить данные движения и при больших или длительных отклонениях, например сбоях гироскопа, но также сильнее следуют ошибкам самого изображения (движущиеся объекты, вода).""",
    "sk": """Optická korekcia
Meria otáčanie kamery priamo z videa a opravuje pohybové dáta tam, kde sa rozchádzajú. Užitočné, keď vibrácie poškodzujú dáta gyroskopu, napríklad pri pevne pripevnenej FPV kamere. Analýza spracuje každý snímok vybraného rozsahu orezania.
Analyzovať
Vymazať
Kliknutím na Analyzovať zmeriate pohyb z videa.
Od analýzy sa zmenili pohybové dáta, synchronizácia, objektív alebo riadková uzávierka. Na použitie korekcie spustite analýzu znova.
Na použitie novej intenzity spustite analýzu znova.
Zmerané v %1 z %2 snímok, korekcia %3°
Ignorovať pohybové dáta zo súboru
Meria celý pohyb kamery z videa, rovnako ako pri súbore bez pohybových dát, namiesto opravovania pohybových dát. Pre dáta príliš poškodené na opravu, napríklad keď gyroskop zlyháva alebo sa saturuje na niekoľko sekúnd.
Intenzita
Ako veľmi sa môžu opravené pohybové dáta odchýliť od pôvodných hodnôt. Nižšie hodnoty opravujú iba malé, rýchle chyby, ako sú vibrácie. Vyššie hodnoty umožňujú obrazu prevážiť nad pohybovými dátami aj pri veľkých alebo dlhších odchýlkach, napríklad chybách gyroskopu, ale viac sledujú aj chyby samotného obrazu (pohybujúce sa objekty, voda).""",
    "tr": """Optik düzeltme
Kamera dönüşünü videonun kendisinden ölçer ve uyuşmadıkları yerlerde hareket verilerini düzeltir. Örneğin sabit monte edilmiş bir FPV kamerada titreşimler jiroskop verilerini bozduğunda yararlıdır. Analiz, seçilen kırpma aralığındaki her kareyi işler.
Analiz et
Temizle
Videodan hareketi ölçmek için Analiz et'e tıklayın.
Analizden sonra hareket verileri, senkronizasyon, lens veya yuvarlanan deklanşör değişti. Düzeltmeyi uygulamak için yeniden analiz edin.
Yeni gücü uygulamak için yeniden analiz edin.
%2 karenin %1 tanesinde ölçüldü, düzeltme %3°
Dosyadaki hareket verilerini yok say
Hareket verilerini düzeltmek yerine, hareket verisi olmayan bir dosyada olduğu gibi tüm kamera hareketini videodan ölçer. Birkaç saniye boyunca arızalanan veya doygunluğa ulaşan bir jiroskop gibi, düzeltilemeyecek kadar bozuk hareket verileri için kullanılır.
Güç
Düzeltmenin hareket verilerini özgün değerlerinden ne kadar uzaklaştırabileceği. Düşük değerler yalnızca titreşim gibi küçük ve hızlı hataları düzeltir. Yüksek değerler, jiroskop arızaları gibi büyük veya uzun süren sapmalarda da görüntünün hareket verilerinin yerine geçmesine izin verir, ancak görüntünün kendi hatalarını (hareketli nesneler, su) da daha fazla izler.""",
    "uk": """Оптична корекція
Вимірює обертання камери за самим відео та коригує дані руху там, де вони розходяться. Корисно, коли вібрації спотворюють дані гіроскопа, наприклад на жорстко закріпленій FPV-камері. Аналіз обробляє кожен кадр вибраного діапазону обрізання.
Аналізувати
Очистити
Натисніть «Аналізувати», щоб виміряти рух за відео.
Після аналізу змінилися дані руху, синхронізація, об'єктив або ковзний затвор. Виконайте аналіз знову, щоб застосувати корекцію.
Виконайте аналіз знову, щоб застосувати нову силу корекції.
Виміряно в %1 із %2 кадрів, корекція %3°
Ігнорувати дані руху з файлу
Вимірює весь рух камери за відео, як для файлу без даних руху, замість коригування цих даних. Для даних, надто пошкоджених для корекції, наприклад якщо гіроскоп дає збої або насичується на кілька секунд поспіль.
Сила корекції
Наскільки корекція може відхилити дані руху від початкових значень. Низькі значення виправляють лише невеликі швидкі помилки, наприклад вібрацію. Високі значення дозволяють зображенню замінювати дані руху й за великих або тривалих відхилень, наприклад збоїв гіроскопа, але також сильніше слідують помилкам самого зображення (рухомі об'єкти, вода).""",
    "zh_CN": """光学校正
从视频画面测量相机旋转，并在与运动数据不一致处校正运动数据。适用于振动破坏陀螺数据的情况，例如硬装的 FPV 相机。分析会处理所选剪辑范围内的每一帧。
分析
清除
点击「分析」，从视频中测量运动。
分析之后，运动数据、同步、镜头或卷帘快门已更改。请重新分析以应用校正。
重新分析以应用新的强度。
在 %2 帧中的 %1 帧测得，校正 %3°
忽略文件自带的运动数据
完全从视频测量相机运动（与没有运动数据的文件相同），而不是校正运动数据。适用于损坏到无法校正的运动数据，例如会连续数秒故障或饱和的陀螺仪。
强度
校正可让运动数据偏离其原值的程度。较低的值只校正振动这类小而快的误差。较高的值允许画面在偏差较大或持续较久时（如陀螺故障）覆盖运动数据，但也更容易跟随画面自身的错误（移动物体、水面）。""",
    "zh_TW": """光學校正
從影片畫面測量相機旋轉，並在與運動資料不一致處校正運動資料。適用於振動破壞陀螺資料的情況，例如硬裝的 FPV 相機。分析會處理所選剪輯範圍內的每一影格。
分析
清除
點擊「分析」，從影片中測量運動。
分析之後，運動資料、同步、鏡頭或捲簾快門已更改。請重新分析以套用校正。
重新分析以套用新的強度。
在 %2 影格中的 %1 影格測得，校正 %3°
忽略檔案自帶的運動資料
完全從影片測量相機運動（與沒有運動資料的檔案相同），而不是校正運動資料。適用於損壞到無法校正的運動資料，例如會連續數秒故障或飽和的陀螺儀。
強度
校正可讓運動資料偏離其原值的程度。較低的值只校正振動這類小而快的誤差。較高的值允許畫面在偏差較大或持續較久時（如陀螺故障）覆蓋運動資料，但也更容易跟隨畫面自身的錯誤（移動物體、水面）。""",
}


def esc(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def make_message(source: str, translation: str | None, line: int, nl: str) -> str:
    tag = ('<translation type="unfinished"></translation>' if translation is None
           else f"<translation>{esc(translation)}</translation>")
    return ("    <message>\n"
            f'        <location filename="{QML}" line="{line}"/>\n'
            f"        <source>{esc(source)}</source>\n"
            f"        {tag}\n"
            "    </message>\n").replace("\n", nl)


def patched_bytes(data: bytes, translations: tuple[str, ...] | None) -> tuple[bytes, int]:
    content = data.decode("utf-8")
    nl = "\r\n" if "\r\n" in content else "\n"
    context = re.search(r"<context>\s*<name>MotionData</name>(.*?)</context>", content, re.S)
    if context is None:
        raise ValueError("MotionData context not found")
    body = context.group(1)
    missing = [i for i, source in enumerate(SOURCES)
               if f"<source>{esc(source)}</source>" not in body]
    if not missing:
        return data, 0
    anchor = re.search(r"<message>\s*(?:(?!</message>).)*<source>Median filter</source>"
                       r"(?:(?!</message>).)*</message>" + re.escape(nl), body, re.S)
    if anchor is None:
        raise ValueError("MotionData/Median filter anchor not found")
    messages = "".join(make_message(SOURCES[i], translations[i] if translations else None,
                                    LINES[i], nl) for i in missing)
    pos = context.start(1) + anchor.end()
    result = (content[:pos] + messages + content[pos:]).encode("utf-8")
    ET.fromstring(result)
    return result, len(missing)


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    try:
        translations = {lang: tuple(text.splitlines()) for lang, text in TRANSLATIONS.items()}
        for lang, values in translations.items():
            if len(values) != len(SOURCES) or any(not value.strip() for value in values):
                raise ValueError(f"{lang}: incomplete translations")
            for source, value in zip(SOURCES, values):
                if sorted(re.findall(r"%\d+", source)) != sorted(re.findall(r"%\d+", value)):
                    raise ValueError(f"{lang}: placeholders differ for {source}")
                if source.count("°") != value.count("°"):
                    raise ValueError(f"{lang}: degree symbol differs for {source}")
        # Validate every file before changing any file; do not serialize existing XML.
        pending = []
        for lang, values in {"gyroflow": None, **translations}.items():
            path = base / f"{lang}.ts"
            original = path.read_bytes()
            result, added = patched_bytes(original, values)
            pending.append((path, original, result, added))
        for path, original, result, added in pending:
            if added:
                if path.read_bytes() != original:
                    raise ValueError(f"{path.name}: changed during patching")
                path.write_bytes(result)
                print(f"OK   {path.name}: added {added} messages")
            else:
                print(f"SKIP {path.name}: already patched")
    except (OSError, ValueError, ET.ParseError) as error:
        print(f"FAIL {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
