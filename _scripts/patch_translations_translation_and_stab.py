# SPDX-License-Identifier: GPL-3.0-or-later
"""Insert translation stabilization and reconstruction messages into all 23 TS files.

Compile with Qt 6.7.3 MinGW lrelease after running from the repository root.
Existing XML and line endings are preserved. Validate all files before writing.
"""
from __future__ import annotations

import pathlib
import re
import sys
import xml.etree.ElementTree as ET

QML = "../../src/ui/menu/MotionData.qml"
SOURCES = (
    "Translation stabilization",
    "Measure from the video how the camera moved sideways and up and down, and shift the whole picture to hold one distance steady. Needs motion data from the file; the analysis goes through every frame of the selected trim range. Can't be used together with in-camera stabilization reconstruction.",
    "Reference distance",
    "Which distance to hold steady: 0% keeps the picture as the gyro stabilization has it, 100% steadies the middle of the tracked points, higher values nearer objects.",
    "Translation smoothness",
    "Low values only remove fast shakes. High values also remove slow drifts and come close to locking the picture.",
    "Compensate movement along the lens axis",
    "Click Analyze to measure the camera movement",
    "Analyze again to measure the camera movement",
    "Settings changed, analyze again",
    "Needs motion data from the file",
    "Measured in %1 of %2 frames, shift up to %3% of the frame, effective smoothness %4 s",
    "Reconstruct in-camera stabilization",
    "For footage shot with in-camera stabilization (IBIS or lens OIS) on but without its data in the file: measure what the camera compensated from the video, so it isn't compensated twice. Can't be used together with Translation stabilization or Optical correction.",
    "Click Analyze to reconstruct the in-camera stabilization",
    "Analyze again to reconstruct the in-camera stabilization",
    "Measured in %1 of %2 frames, compensation up to %3°, cut-off %4 Hz",
    "%1 s",
)
# Locations refer to the implementation when these messages were introduced.
LINES = (465, 466, 481, 483, 492, 494, 514, 475, 474, 476, 473, 478, 535, 536, 545, 544, 548, 508)
ANCHOR = "How far the correction may take the motion data from what it says. Lower values only correct small, fast errors like vibration. Higher values let the image override the motion data also where it's off by a lot or for longer, like gyro glitches, but follow the image's own mistakes (moving objects, water) more too."
TRANSLATIONS = {
    "cs": """Stabilizace posunu
Změří z videa pohyb kamery do stran a nahoru a dolů a posune celý obraz tak, aby zvolená vzdálenost zůstala stabilní. Vyžaduje pohybová data ze souboru; analýza projde každý snímek vybraného rozsahu ořezu. Nelze používat společně s rekonstrukcí stabilizace ve fotoaparátu.
Referenční vzdálenost
Kterou vzdálenost stabilizovat: 0% zachová obraz po stabilizaci gyroskopem, 100% stabilizuje střední vzdálenost sledovaných bodů, vyšší hodnoty bližší objekty.
Vyhlazení posunu
Nízké hodnoty odstraní pouze rychlé otřesy. Vyšší hodnoty odstraní i pomalý pohyb a téměř zafixují obraz.
Kompenzovat pohyb podél osy objektivu
Kliknutím na Analyzovat změříte pohyb kamery
Pro změření pohybu kamery spusťte analýzu znovu
Nastavení se změnilo, spusťte analýzu znovu
Vyžaduje pohybová data ze souboru
Změřeno v %1 z %2 snímků, posun až %3% obrazu, skutečné vyhlazení %4 s
Rekonstruovat stabilizaci ve fotoaparátu
Pro záběry pořízené se zapnutou stabilizací ve fotoaparátu (IBIS nebo OIS objektivu), ale bez jejích dat v souboru: změří z videa kompenzaci fotoaparátu, aby se nekompenzovala dvakrát. Nelze používat společně se stabilizací posunu nebo optickou korekcí.
Kliknutím na Analyzovat rekonstruujete stabilizaci ve fotoaparátu
Pro rekonstrukci stabilizace ve fotoaparátu spusťte analýzu znovu
Změřeno v %1 z %2 snímků, kompenzace až %3°, mezní frekvence %4 Hz
%1 s""",
    "da": """Stabilisering af forskydning
Mål fra videoen, hvordan kameraet bevægede sig sidelæns og op og ned, og forskyd hele billedet for at holde én afstand stabil. Kræver bevægelsesdata fra filen; analysen gennemgår hvert billede i det valgte beskæringsinterval. Kan ikke bruges sammen med rekonstruktion af kameraets stabilisering.
Referenceafstand
Hvilken afstand der skal holdes stabil: 0% bevarer billedet som efter gyrostabilisering, 100% stabiliserer midten af de sporede punkter, højere værdier nærmere objekter.
Udjævning af forskydning
Lave værdier fjerner kun hurtige rystelser. Høje værdier fjerner også langsomme bevægelser og låser næsten billedet.
Kompensér bevægelse langs objektivets akse
Klik på Analyser for at måle kamerabevægelsen
Analyser igen for at måle kamerabevægelsen
Indstillingerne er ændret, analyser igen
Kræver bevægelsesdata fra filen
Målt i %1 af %2 billeder, forskydning op til %3% af billedet, effektiv udjævning %4 s
Rekonstruér kameraets stabilisering
Til optagelser med kameraets stabilisering (IBIS eller objektivets OIS) slået til, men uden dens data i filen: mål kameraets kompensation fra videoen, så den ikke kompenseres to gange. Kan ikke bruges sammen med stabilisering af forskydning eller optisk korrektion.
Klik på Analyser for at rekonstruere kameraets stabilisering
Analyser igen for at rekonstruere kameraets stabilisering
Målt i %1 af %2 billeder, kompensation op til %3°, grænsefrekvens %4 Hz
%1 s""",
    "de": """Translationsstabilisierung
Misst im Video die seitliche und vertikale Bewegung der Kamera und verschiebt das ganze Bild, um eine Entfernung stabil zu halten. Benötigt Bewegungsdaten aus der Datei; die Analyse verarbeitet jedes Bild des ausgewählten Schnittbereichs. Kann nicht zusammen mit der Rekonstruktion der kamerainternen Stabilisierung verwendet werden.
Referenzentfernung
Welche Entfernung stabilisiert werden soll: 0% belässt das Bild wie nach der Gyrostabilisierung, 100% stabilisiert die mittlere Entfernung der verfolgten Punkte, höhere Werte nähere Objekte.
Translationsglättung
Niedrige Werte entfernen nur schnelle Erschütterungen. Hohe Werte entfernen auch langsames Driften und fixieren das Bild nahezu.
Bewegung entlang der Objektivachse kompensieren
Klicken Sie auf Analysieren, um die Kamerabewegung zu messen
Analysieren Sie erneut, um die Kamerabewegung zu messen
Einstellungen geändert, erneut analysieren
Benötigt Bewegungsdaten aus der Datei
In %1 von %2 Bildern gemessen, Verschiebung bis %3% des Bildes, wirksame Glättung %4 s
Kamerainterne Stabilisierung rekonstruieren
Für Aufnahmen mit eingeschalteter kamerainterner Stabilisierung (IBIS oder Objektiv-OIS), deren Daten in der Datei fehlen: misst die Kompensation der Kamera im Video, damit sie nicht doppelt kompensiert wird. Kann nicht zusammen mit Translationsstabilisierung oder optischer Korrektur verwendet werden.
Klicken Sie auf Analysieren, um die kamerainterne Stabilisierung zu rekonstruieren
Analysieren Sie erneut, um die kamerainterne Stabilisierung zu rekonstruieren
In %1 von %2 Bildern gemessen, Kompensation bis %3°, Grenzfrequenz %4 Hz
%1 s""",
    "el": """Σταθεροποίηση μετατόπισης
Μετρά από το βίντεο την πλάγια και κατακόρυφη κίνηση της κάμερας και μετατοπίζει ολόκληρη την εικόνα ώστε μία απόσταση να παραμένει σταθερή. Απαιτεί δεδομένα κίνησης από το αρχείο· η ανάλυση επεξεργάζεται κάθε καρέ του επιλεγμένου εύρους περικοπής. Δεν μπορεί να χρησιμοποιηθεί μαζί με ανακατασκευή της σταθεροποίησης της κάμερας.
Απόσταση αναφοράς
Ποια απόσταση διατηρείται σταθερή: το 0% αφήνει την εικόνα όπως μετά τη γυροσκοπική σταθεροποίηση, το 100% σταθεροποιεί το μέσο των παρακολουθούμενων σημείων, οι υψηλότερες τιμές τα κοντινότερα αντικείμενα.
Εξομάλυνση μετατόπισης
Οι χαμηλές τιμές αφαιρούν μόνο γρήγορα τραντάγματα. Οι υψηλές τιμές αφαιρούν και την αργή μετακίνηση και σχεδόν κλειδώνουν την εικόνα.
Αντιστάθμιση κίνησης κατά μήκος του άξονα του φακού
Πατήστε Ανάλυση για να μετρήσετε την κίνηση της κάμερας
Επαναλάβετε την ανάλυση για να μετρήσετε την κίνηση της κάμερας
Οι ρυθμίσεις άλλαξαν, επαναλάβετε την ανάλυση
Απαιτούνται δεδομένα κίνησης από το αρχείο
Μετρήθηκε σε %1 από %2 καρέ, μετατόπιση έως %3% της εικόνας, αποτελεσματική εξομάλυνση %4 s
Ανακατασκευή σταθεροποίησης της κάμερας
Για πλάνα με ενεργή σταθεροποίηση στην κάμερα (IBIS ή OIS φακού), αλλά χωρίς τα δεδομένα της στο αρχείο: μετρά από το βίντεο την αντιστάθμιση της κάμερας ώστε να μην εφαρμοστεί δύο φορές. Δεν μπορεί να χρησιμοποιηθεί μαζί με σταθεροποίηση μετατόπισης ή οπτική διόρθωση.
Πατήστε Ανάλυση για να ανακατασκευάσετε τη σταθεροποίηση της κάμερας
Επαναλάβετε την ανάλυση για να ανακατασκευάσετε τη σταθεροποίηση της κάμερας
Μετρήθηκε σε %1 από %2 καρέ, αντιστάθμιση έως %3°, συχνότητα αποκοπής %4 Hz
%1 s""",
    "es": """Estabilización de desplazamiento
Mide en el vídeo cómo se movió la cámara lateral y verticalmente, y desplaza toda la imagen para mantener estable una distancia. Necesita datos de movimiento del archivo; el análisis recorre cada fotograma del intervalo de recorte seleccionado. No se puede usar junto con la reconstrucción de la estabilización interna de la cámara.
Distancia de referencia
Qué distancia mantener estable: 0% conserva la imagen como la deja la estabilización giroscópica, 100% estabiliza la distancia media de los puntos seguidos y los valores mayores estabilizan objetos más cercanos.
Suavizado del desplazamiento
Los valores bajos solo eliminan sacudidas rápidas. Los altos también eliminan desplazamientos lentos y casi fijan la imagen.
Compensar el movimiento a lo largo del eje del objetivo
Haz clic en Analizar para medir el movimiento de la cámara
Analiza de nuevo para medir el movimiento de la cámara
La configuración ha cambiado, analiza de nuevo
Necesita datos de movimiento del archivo
Medido en %1 de %2 fotogramas, desplazamiento de hasta el %3% de la imagen, suavizado efectivo de %4 s
Reconstruir la estabilización interna de la cámara
Para grabaciones con estabilización interna (IBIS u OIS del objetivo) activada, pero sin sus datos en el archivo: mide en el vídeo lo que compensó la cámara para evitar compensarlo dos veces. No se puede usar junto con la estabilización de desplazamiento o la corrección óptica.
Haz clic en Analizar para reconstruir la estabilización interna de la cámara
Analiza de nuevo para reconstruir la estabilización interna de la cámara
Medido en %1 de %2 fotogramas, compensación de hasta %3°, frecuencia de corte de %4 Hz
%1 s""",
    "fi": """Siirtymän vakautus
Mittaa videosta kameran sivuttais- ja pystysuuntaisen liikkeen ja siirtää koko kuvaa pitääkseen yhden etäisyyden vakaana. Vaatii liiketiedot tiedostosta; analyysi käy läpi valitun leikkausalueen jokaisen ruudun. Ei voi käyttää yhdessä kameran sisäisen vakautuksen rekonstruoinnin kanssa.
Viite-etäisyys
Mikä etäisyys pidetään vakaana: 0% säilyttää gyrovakautuksen tuottaman kuvan, 100% vakauttaa seurattujen pisteiden keskietäisyyden, suuremmat arvot lähempänä olevat kohteet.
Siirtymän tasoitus
Pienet arvot poistavat vain nopean tärinän. Suuret arvot poistavat myös hitaan liukumisen ja lähes lukitsevat kuvan.
Kompensoi liike objektiivin akselin suunnassa
Mittaa kameran liike napsauttamalla Analysoi
Analysoi uudelleen kameran liikkeen mittaamiseksi
Asetukset muuttuivat, analysoi uudelleen
Vaatii liiketiedot tiedostosta
Mitattu %1 ruudussa %2 ruudusta, siirtymä enintään %3% kuvasta, tehollinen tasoitus %4 s
Rekonstruoi kameran sisäinen vakautus
Materiaalille, jossa kameran sisäinen vakautus (IBIS tai objektiivin OIS) oli käytössä mutta sen tiedot puuttuvat tiedostosta: mittaa kameran kompensaatio videosta, jotta sitä ei kompensoida kahdesti. Ei voi käyttää yhdessä siirtymän vakautuksen tai optisen korjauksen kanssa.
Rekonstruoi kameran sisäinen vakautus napsauttamalla Analysoi
Analysoi uudelleen kameran sisäisen vakautuksen rekonstruoimiseksi
Mitattu %1 ruudussa %2 ruudusta, kompensaatio enintään %3°, rajataajuus %4 Hz
%1 s""",
    "fr": """Stabilisation des déplacements
Mesure dans la vidéo les mouvements latéraux et verticaux de la caméra et déplace toute l'image pour stabiliser une distance donnée. Nécessite les données de mouvement du fichier ; l'analyse parcourt chaque image de la plage de découpe sélectionnée. Incompatible avec la reconstruction de la stabilisation interne de la caméra.
Distance de référence
Distance à stabiliser : 0% conserve l'image obtenue par la stabilisation gyroscopique, 100% stabilise la distance médiane des points suivis, les valeurs supérieures stabilisent des objets plus proches.
Lissage des déplacements
Les valeurs faibles éliminent uniquement les secousses rapides. Les valeurs élevées éliminent aussi les dérives lentes et figent presque l'image.
Compenser le mouvement dans l'axe de l'objectif
Cliquez sur Analyser pour mesurer le mouvement de la caméra
Relancez l'analyse pour mesurer le mouvement de la caméra
Les réglages ont changé, relancez l'analyse
Nécessite les données de mouvement du fichier
Mesuré sur %1 des %2 images, déplacement jusqu'à %3% de l'image, lissage effectif de %4 s
Reconstruire la stabilisation interne de la caméra
Pour les vidéos enregistrées avec la stabilisation interne (IBIS ou OIS de l'objectif) activée, mais sans ses données dans le fichier : mesure la compensation de la caméra dans la vidéo pour éviter de la compenser deux fois. Incompatible avec la stabilisation des déplacements ou la correction optique.
Cliquez sur Analyser pour reconstruire la stabilisation interne de la caméra
Relancez l'analyse pour reconstruire la stabilisation interne de la caméra
Mesuré sur %1 des %2 images, compensation jusqu'à %3°, fréquence de coupure %4 Hz
%1 s""",
    "gl": """Estabilización do desprazamento
Mide no vídeo como se moveu a cámara lateral e verticalmente e despraza toda a imaxe para manter estable unha distancia. Precisa datos de movemento do ficheiro; a análise percorre cada fotograma do intervalo de recorte seleccionado. Non se pode usar xunto coa reconstrución da estabilización interna da cámara.
Distancia de referencia
Que distancia manter estable: 0% conserva a imaxe da estabilización xiroscópica, 100% estabiliza a distancia media dos puntos seguidos e os valores maiores estabilizan obxectos máis próximos.
Suavizado do desprazamento
Os valores baixos só eliminan sacudidas rápidas. Os altos tamén eliminan desprazamentos lentos e case fixan a imaxe.
Compensar o movemento ao longo do eixe do obxectivo
Preme en Analizar para medir o movemento da cámara
Analiza de novo para medir o movemento da cámara
A configuración cambiou, analiza de novo
Precisa datos de movemento do ficheiro
Medido en %1 de %2 fotogramas, desprazamento de ata o %3% da imaxe, suavizado efectivo de %4 s
Reconstruír a estabilización interna da cámara
Para gravacións coa estabilización interna (IBIS ou OIS do obxectivo) activada pero sen os seus datos no ficheiro: mide no vídeo a compensación da cámara para evitar compensala dúas veces. Non se pode usar xunto coa estabilización do desprazamento ou a corrección óptica.
Preme en Analizar para reconstruír a estabilización interna da cámara
Analiza de novo para reconstruír a estabilización interna da cámara
Medido en %1 de %2 fotogramas, compensación de ata %3°, frecuencia de corte de %4 Hz
%1 s""",
    "id": """Stabilisasi perpindahan
Ukur dari video bagaimana kamera bergerak ke samping dan ke atas atau bawah, lalu geser seluruh gambar agar satu jarak tetap stabil. Memerlukan data gerakan dari berkas; analisis memproses setiap bingkai dalam rentang pemangkasan yang dipilih. Tidak dapat digunakan bersama rekonstruksi stabilisasi dalam kamera.
Jarak acuan
Jarak yang dijaga stabil: 0% mempertahankan gambar hasil stabilisasi giroskop, 100% menstabilkan jarak tengah titik yang dilacak, nilai lebih tinggi menstabilkan objek yang lebih dekat.
Penghalusan perpindahan
Nilai rendah hanya menghilangkan guncangan cepat. Nilai tinggi juga menghilangkan pergeseran lambat dan hampir mengunci gambar.
Kompensasi gerakan sepanjang sumbu lensa
Klik Analisis untuk mengukur gerakan kamera
Analisis lagi untuk mengukur gerakan kamera
Pengaturan berubah, analisis lagi
Memerlukan data gerakan dari berkas
Diukur pada %1 dari %2 bingkai, pergeseran hingga %3% gambar, penghalusan efektif %4 s
Rekonstruksi stabilisasi dalam kamera
Untuk rekaman dengan stabilisasi dalam kamera (IBIS atau OIS lensa) aktif tetapi tanpa datanya dalam berkas: ukur kompensasi kamera dari video agar tidak dikompensasi dua kali. Tidak dapat digunakan bersama stabilisasi perpindahan atau koreksi optik.
Klik Analisis untuk merekonstruksi stabilisasi dalam kamera
Analisis lagi untuk merekonstruksi stabilisasi dalam kamera
Diukur pada %1 dari %2 bingkai, kompensasi hingga %3°, frekuensi potong %4 Hz
%1 dtk""",
    "it": """Stabilizzazione della traslazione
Misura dal video gli spostamenti laterali e verticali della fotocamera e sposta l'intera immagine per mantenere stabile una distanza. Richiede i dati di movimento del file; l'analisi esamina ogni fotogramma dell'intervallo di ritaglio selezionato. Non può essere usata insieme alla ricostruzione della stabilizzazione interna.
Distanza di riferimento
Quale distanza mantenere stabile: 0% conserva l'immagine ottenuta dalla stabilizzazione giroscopica, 100% stabilizza la distanza mediana dei punti tracciati, valori maggiori stabilizzano oggetti più vicini.
Fluidità della traslazione
Valori bassi eliminano solo le vibrazioni rapide. Valori alti eliminano anche le derive lente e quasi bloccano l'immagine.
Compensa il movimento lungo l'asse dell'obiettivo
Fai clic su Analizza per misurare il movimento della fotocamera
Analizza di nuovo per misurare il movimento della fotocamera
Impostazioni modificate, analizza di nuovo
Richiede i dati di movimento del file
Misurato in %1 di %2 fotogrammi, spostamento fino al %3% dell'immagine, fluidità effettiva %4 s
Ricostruisci la stabilizzazione interna
Per filmati registrati con stabilizzazione interna (IBIS o OIS dell'obiettivo) attiva ma senza i relativi dati nel file: misura dal video la compensazione della fotocamera per evitare di compensarla due volte. Non può essere usata insieme alla stabilizzazione della traslazione o alla correzione ottica.
Fai clic su Analizza per ricostruire la stabilizzazione interna
Analizza di nuovo per ricostruire la stabilizzazione interna
Misurato in %1 di %2 fotogrammi, compensazione fino a %3°, frequenza di taglio %4 Hz
%1 s""",
    "ja": """並進ブレ補正
映像からカメラの左右・上下方向の移動を測定し、画面全体を移動して特定の距離を安定させます。ファイル内のモーションデータが必要です。解析は選択したトリミング範囲の全フレームを処理します。カメラ内手ブレ補正の再構築とは併用できません。
基準距離
安定させる距離：0%ではジャイロ補正後の映像を維持し、100%では追跡点の中央の距離を安定させ、より高い値では近くの被写体を安定させます。
並進の平滑化
低い値では速い揺れだけを除去します。高い値ではゆっくりした移動も除去し、画面をほぼ固定します。
レンズの光軸方向の移動を補正
「解析」をクリックしてカメラの移動を測定
カメラの移動を測定するには再解析が必要です
設定が変更されました。再解析してください
ファイル内のモーションデータが必要です
%2 フレーム中 %1 フレームで測定、最大移動量は画面の %3%、実効平滑化時間は %4 秒
カメラ内手ブレ補正を再構築
カメラ内手ブレ補正（IBIS またはレンズ OIS）を有効にして撮影したものの、そのデータがファイルにない映像向けです。映像からカメラの補正量を測定し、二重補正を防ぎます。並進ブレ補正や光学補正とは併用できません。
「解析」をクリックしてカメラ内手ブレ補正を再構築
カメラ内手ブレ補正を再構築するには再解析が必要です
%2 フレーム中 %1 フレームで測定、最大補正量 %3°、カットオフ周波数 %4 Hz
%1 秒""",
    "ko": """이동 흔들림 보정
영상에서 카메라의 좌우 및 상하 이동을 측정하고 화면 전체를 이동시켜 특정 거리를 안정시킵니다. 파일의 모션 데이터가 필요하며 선택한 자르기 범위의 모든 프레임을 분석합니다. 카메라 내 손떨림 보정 재구성과 함께 사용할 수 없습니다.
기준 거리
안정시킬 거리: 0%는 자이로 보정 결과를 유지하고, 100%는 추적 지점의 중간 거리를 안정시키며, 더 높은 값은 더 가까운 물체를 안정시킵니다.
이동 평활화
낮은 값은 빠른 흔들림만 제거합니다. 높은 값은 느린 이동도 제거하여 화면을 거의 고정합니다.
렌즈 축 방향의 이동 보정
분석을 클릭하여 카메라 이동 측정
카메라 이동을 측정하려면 다시 분석하세요
설정이 변경되었습니다. 다시 분석하세요
파일의 모션 데이터가 필요합니다
%2개 프레임 중 %1개에서 측정, 최대 이동은 화면의 %3%, 실제 평활화 시간 %4초
카메라 내 손떨림 보정 재구성
카메라 내 손떨림 보정(IBIS 또는 렌즈 OIS)을 켜고 촬영했지만 해당 데이터가 파일에 없는 영상용입니다. 영상에서 카메라가 보정한 양을 측정하여 이중 보정을 방지합니다. 이동 흔들림 보정 또는 광학 보정과 함께 사용할 수 없습니다.
분석을 클릭하여 카메라 내 손떨림 보정 재구성
카메라 내 손떨림 보정을 재구성하려면 다시 분석하세요
%2개 프레임 중 %1개에서 측정, 최대 보정 %3°, 차단 주파수 %4 Hz
%1초""",
    "no": """Stabilisering av forskyvning
Mål fra videoen hvordan kameraet beveget seg sidelengs og opp og ned, og forskyv hele bildet for å holde én avstand stabil. Krever bevegelsesdata fra filen; analysen går gjennom hvert bilde i det valgte klippeområdet. Kan ikke brukes sammen med rekonstruksjon av kameraets stabilisering.
Referanseavstand
Hvilken avstand som skal holdes stabil: 0% beholder bildet etter gyrostabiliseringen, 100% stabiliserer midten av de sporede punktene, høyere verdier nærmere objekter.
Utjevning av forskyvning
Lave verdier fjerner bare raske rystelser. Høye verdier fjerner også langsom drift og låser nesten bildet.
Kompenser bevegelse langs objektivets akse
Klikk på Analyser for å måle kamerabevegelsen
Analyser på nytt for å måle kamerabevegelsen
Innstillingene er endret, analyser på nytt
Krever bevegelsesdata fra filen
Målt i %1 av %2 bilder, forskyvning opptil %3% av bildet, effektiv utjevning %4 s
Rekonstruer kameraets stabilisering
For opptak med kameraets stabilisering (IBIS eller objektivets OIS) slått på, men uten tilhørende data i filen: mål kameraets kompensasjon fra videoen slik at den ikke kompenseres to ganger. Kan ikke brukes sammen med stabilisering av forskyvning eller optisk korreksjon.
Klikk på Analyser for å rekonstruere kameraets stabilisering
Analyser på nytt for å rekonstruere kameraets stabilisering
Målt i %1 av %2 bilder, kompensasjon opptil %3°, grensefrekvens %4 Hz
%1 s""",
    "pl": """Stabilizacja przesunięcia
Mierzy z filmu ruch kamery na boki oraz w górę i w dół i przesuwa cały obraz, aby ustabilizować jedną odległość. Wymaga danych ruchu z pliku; analiza obejmuje każdą klatkę wybranego zakresu przycięcia. Nie można używać razem z rekonstrukcją stabilizacji w kamerze.
Odległość odniesienia
Którą odległość ustabilizować: 0% zachowuje obraz po stabilizacji żyroskopowej, 100% stabilizuje środkową odległość śledzonych punktów, wyższe wartości bliższe obiekty.
Wygładzanie przesunięcia
Niskie wartości usuwają tylko szybkie drgania. Wysokie usuwają również powolny dryf i niemal blokują obraz.
Kompensuj ruch wzdłuż osi obiektywu
Kliknij Analizuj, aby zmierzyć ruch kamery
Przeprowadź analizę ponownie, aby zmierzyć ruch kamery
Ustawienia zmienione, przeprowadź analizę ponownie
Wymaga danych ruchu z pliku
Zmierzono w %1 z %2 klatek, przesunięcie do %3% obrazu, efektywne wygładzanie %4 s
Zrekonstruuj stabilizację w kamerze
Dla nagrań z włączoną stabilizacją w kamerze (IBIS lub OIS obiektywu), ale bez jej danych w pliku: mierzy kompensację kamery z filmu, aby uniknąć podwójnej kompensacji. Nie można używać razem ze stabilizacją przesunięcia ani korekcją optyczną.
Kliknij Analizuj, aby zrekonstruować stabilizację w kamerze
Przeprowadź analizę ponownie, aby zrekonstruować stabilizację w kamerze
Zmierzono w %1 z %2 klatek, kompensacja do %3°, częstotliwość odcięcia %4 Hz
%1 s""",
    "pt": """Estabilização da translação
Mede no vídeo o movimento lateral e vertical da câmara e desloca toda a imagem para manter estável uma distância. Requer dados de movimento do ficheiro; a análise percorre cada fotograma do intervalo de corte selecionado. Não pode ser usada em conjunto com a reconstrução da estabilização interna da câmara.
Distância de referência
Que distância manter estável: 0% mantém a imagem resultante da estabilização giroscópica, 100% estabiliza a distância intermédia dos pontos seguidos, valores superiores estabilizam objetos mais próximos.
Suavização da translação
Valores baixos eliminam apenas tremores rápidos. Valores altos também eliminam deslocamentos lentos e quase fixam a imagem.
Compensar movimento ao longo do eixo da objetiva
Clique em Analisar para medir o movimento da câmara
Analise novamente para medir o movimento da câmara
As definições mudaram, analise novamente
Requer dados de movimento do ficheiro
Medido em %1 de %2 fotogramas, deslocamento até %3% da imagem, suavização efetiva %4 s
Reconstruir a estabilização interna da câmara
Para gravações com estabilização interna (IBIS ou OIS da objetiva) ativa mas sem os respetivos dados no ficheiro: mede no vídeo a compensação da câmara para evitar compensá-la duas vezes. Não pode ser usada em conjunto com a estabilização da translação ou a correção ótica.
Clique em Analisar para reconstruir a estabilização interna da câmara
Analise novamente para reconstruir a estabilização interna da câmara
Medido em %1 de %2 fotogramas, compensação até %3°, frequência de corte %4 Hz
%1 s""",
    "pt_BR": """Estabilização de translação
Mede no vídeo o movimento lateral e vertical da câmera e desloca toda a imagem para manter uma distância estável. Requer dados de movimento do arquivo; a análise percorre cada quadro do intervalo de corte selecionado. Não pode ser usada junto com a reconstrução da estabilização interna da câmera.
Distância de referência
Qual distância manter estável: 0% mantém a imagem resultante da estabilização giroscópica, 100% estabiliza a distância intermediária dos pontos rastreados, valores maiores estabilizam objetos mais próximos.
Suavização da translação
Valores baixos removem apenas tremores rápidos. Valores altos também removem deslocamentos lentos e quase fixam a imagem.
Compensar movimento ao longo do eixo da lente
Clique em Analisar para medir o movimento da câmera
Analise novamente para medir o movimento da câmera
As configurações mudaram, analise novamente
Requer dados de movimento do arquivo
Medido em %1 de %2 quadros, deslocamento de até %3% da imagem, suavização efetiva de %4 s
Reconstruir a estabilização interna da câmera
Para gravações com estabilização interna (IBIS ou OIS da lente) ativada, mas sem seus dados no arquivo: mede no vídeo a compensação da câmera para evitar compensá-la duas vezes. Não pode ser usada junto com a estabilização de translação ou a correção óptica.
Clique em Analisar para reconstruir a estabilização interna da câmera
Analise novamente para reconstruir a estabilização interna da câmera
Medido em %1 de %2 quadros, compensação de até %3°, frequência de corte de %4 Hz
%1 s""",
    "ru": """Стабилизация перемещения
Измеряет по видео боковое и вертикальное перемещение камеры и сдвигает весь кадр, чтобы стабилизировать одну дистанцию. Нужны данные движения из файла; анализ проходит по всем кадрам выбранного диапазона обрезки. Нельзя использовать вместе с реконструкцией стабилизации камеры.
Опорная дистанция
Какую дистанцию стабилизировать: 0% сохраняет результат гироскопической стабилизации, 100% стабилизирует среднюю дистанцию отслеживаемых точек, более высокие значения — более близкие объекты.
Сглаживание перемещения
Низкие значения убирают только быструю тряску. Высокие также убирают медленный дрейф и почти фиксируют кадр.
Компенсировать движение вдоль оси объектива
Нажмите «Анализировать», чтобы измерить перемещение камеры
Повторите анализ, чтобы измерить перемещение камеры
Настройки изменились, повторите анализ
Нужны данные движения из файла
Измерено в %1 из %2 кадров, сдвиг до %3% кадра, эффективное сглаживание %4 с
Реконструировать стабилизацию камеры
Для видео, снятого с включённой стабилизацией камеры (IBIS или OIS объектива), но без её данных в файле: измеряет по видео компенсацию камеры, чтобы не компенсировать её дважды. Нельзя использовать вместе со стабилизацией перемещения или оптической коррекцией.
Нажмите «Анализировать», чтобы реконструировать стабилизацию камеры
Повторите анализ, чтобы реконструировать стабилизацию камеры
Измерено в %1 из %2 кадров, компенсация до %3°, частота среза %4 Гц
%1 с""",
    "sk": """Stabilizácia posunu
Zmeria z videa pohyb kamery do strán a nahor a nadol a posunie celý obraz tak, aby zvolená vzdialenosť zostala stabilná. Vyžaduje pohybové údaje zo súboru; analýza prejde každý snímok vybraného rozsahu orezania. Nemožno používať spolu s rekonštrukciou stabilizácie vo fotoaparáte.
Referenčná vzdialenosť
Ktorú vzdialenosť stabilizovať: 0% zachová obraz po stabilizácii gyroskopom, 100% stabilizuje strednú vzdialenosť sledovaných bodov, vyššie hodnoty bližšie objekty.
Vyhladenie posunu
Nízke hodnoty odstránia len rýchle otrasy. Vysoké hodnoty odstránia aj pomalý pohyb a takmer zafixujú obraz.
Kompenzovať pohyb pozdĺž osi objektívu
Kliknutím na Analyzovať zmeriate pohyb kamery
Na zmeranie pohybu kamery spustite analýzu znova
Nastavenia sa zmenili, spustite analýzu znova
Vyžaduje pohybové údaje zo súboru
Zmerané v %1 z %2 snímok, posun až %3% obrazu, skutočné vyhladenie %4 s
Rekonštruovať stabilizáciu vo fotoaparáte
Pre zábery so zapnutou stabilizáciou vo fotoaparáte (IBIS alebo OIS objektívu), ale bez jej údajov v súbore: zmeria z videa kompenzáciu fotoaparátu, aby sa nekompenzovala dvakrát. Nemožno používať spolu so stabilizáciou posunu alebo optickou korekciou.
Kliknutím na Analyzovať rekonštruujete stabilizáciu vo fotoaparáte
Na rekonštrukciu stabilizácie vo fotoaparáte spustite analýzu znova
Zmerané v %1 z %2 snímok, kompenzácia až %3°, medzná frekvencia %4 Hz
%1 s""",
    "tr": """Öteleme sabitleme
Videodan kameranın yanlara ve yukarı aşağı hareketini ölçer ve bir mesafeyi sabit tutmak için tüm görüntüyü kaydırır. Dosyadaki hareket verileri gerekir; analiz seçilen kırpma aralığının her karesini işler. Kamera içi sabitlemenin yeniden oluşturulmasıyla birlikte kullanılamaz.
Referans mesafesi
Sabit tutulacak mesafe: 0% jiroskop sabitlemesinin görüntüsünü korur, 100% izlenen noktaların orta mesafesini sabitler, daha yüksek değerler daha yakın nesneleri sabitler.
Öteleme yumuşatma
Düşük değerler yalnızca hızlı sarsıntıları giderir. Yüksek değerler yavaş kaymaları da giderir ve görüntüyü neredeyse kilitler.
Lens ekseni boyunca hareketi telafi et
Kamera hareketini ölçmek için Analiz et'e tıklayın
Kamera hareketini ölçmek için yeniden analiz edin
Ayarlar değişti, yeniden analiz edin
Dosyadaki hareket verileri gerekir
%2 karenin %1 tanesinde ölçüldü, görüntünün %3% kadar kayması, etkin yumuşatma %4 s
Kamera içi sabitlemeyi yeniden oluştur
Kamera içi sabitleme (IBIS veya lens OIS) açıkken çekilen ancak dosyada bu verileri bulunmayan görüntüler için: iki kez telafi edilmemesi amacıyla kameranın telafisini videodan ölçer. Öteleme sabitleme veya optik düzeltmeyle birlikte kullanılamaz.
Kamera içi sabitlemeyi yeniden oluşturmak için Analiz et'e tıklayın
Kamera içi sabitlemeyi yeniden oluşturmak için yeniden analiz edin
%2 karenin %1 tanesinde ölçüldü, %3° kadar telafi, kesim frekansı %4 Hz
%1 sn""",
    "uk": """Стабілізація переміщення
Вимірює з відео бокове та вертикальне переміщення камери й зсуває весь кадр, щоб стабілізувати одну відстань. Потрібні дані руху з файлу; аналіз охоплює кожен кадр вибраного діапазону обрізання. Не можна використовувати разом із реконструкцією стабілізації камери.
Опорна відстань
Яку відстань стабілізувати: 0% зберігає результат гіроскопічної стабілізації, 100% стабілізує середню відстань відстежуваних точок, вищі значення — ближчі об'єкти.
Згладжування переміщення
Низькі значення прибирають лише швидке тремтіння. Високі також прибирають повільний дрейф і майже фіксують кадр.
Компенсувати рух уздовж осі об'єктива
Натисніть «Аналізувати», щоб виміряти переміщення камери
Повторіть аналіз, щоб виміряти переміщення камери
Налаштування змінилися, повторіть аналіз
Потрібні дані руху з файлу
Виміряно в %1 з %2 кадрів, зсув до %3% кадру, ефективне згладжування %4 с
Реконструювати стабілізацію камери
Для відео, знятого з увімкненою стабілізацією камери (IBIS або OIS об'єктива), але без її даних у файлі: вимірює з відео компенсацію камери, щоб не компенсувати її двічі. Не можна використовувати разом зі стабілізацією переміщення або оптичною корекцією.
Натисніть «Аналізувати», щоб реконструювати стабілізацію камери
Повторіть аналіз, щоб реконструювати стабілізацію камери
Виміряно в %1 з %2 кадрів, компенсація до %3°, частота зрізу %4 Гц
%1 с""",
    "zh_CN": """位移防抖
从视频中测量相机左右和上下的位移，并平移整幅画面以稳住一个距离。需要文件自带的运动数据；分析会遍历所选剪辑范围的每一帧。不能与重建机内防抖同时使用。
参考距离
选择要稳住的距离：0% 保持陀螺仪防抖后的画面，100% 稳住跟踪点深度的中间层，更高的值稳住更近的物体。
位移平滑度
较低的值只去掉快速晃动。较高的值也去掉缓慢飘移，使画面接近锁定。
补偿前后方向的位移
点击"分析"测量相机位移
需要重新分析以测量相机位移
设置已变化，需要重新分析
需要文件自带的运动数据
在 %1 / %2 帧中测得，最大平移为画面的 %3%，实际平滑度 %4 秒
重建机内防抖
用于拍摄时开启了机内防抖（IBIS 或镜头 OIS），但文件未记录其数据的素材：从视频测量相机已补偿的运动，避免重复补偿。不能与位移防抖或光学校正同时使用。
点击"分析"重建机内防抖
需要重新分析以重建机内防抖
在 %1 / %2 帧中测得，补偿最大 %3°，截止频率 %4 Hz
%1 秒""",
    "zh_TW": """位移防抖
從影片中測量相機左右和上下的位移，並平移整幅畫面以穩住一個距離。需要檔案自帶的運動資料；分析會遍歷所選剪輯範圍的每一影格。不能與重建機內防抖同時使用。
參考距離
選擇要穩住的距離：0% 保持陀螺儀防抖後的畫面，100% 穩住追蹤點深度的中間層，更高的值穩住更近的物體。
位移平滑度
較低的值只去掉快速晃動。較高的值也去掉緩慢飄移，使畫面接近鎖定。
補償前後方向的位移
點擊「分析」測量相機位移
需要重新分析以測量相機位移
設定已變更，需要重新分析
需要檔案自帶的運動資料
在 %1 / %2 影格中測得，最大平移為畫面的 %3%，實際平滑度 %4 秒
重建機內防抖
用於拍攝時開啟了機內防抖（IBIS 或鏡頭 OIS），但檔案未記錄其資料的素材：從影片測量相機已補償的運動，避免重複補償。不能與位移防抖或光學校正同時使用。
點擊「分析」重建機內防抖
需要重新分析以重建機內防抖
在 %1 / %2 影格中測得，補償最大 %3°，截止頻率 %4 Hz
%1 秒""",
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
    parsed = ET.fromstring(data)
    motion = next((c for c in parsed.findall("context") if c.findtext("name") == "MotionData"), None)
    if motion is None:
        raise ValueError("MotionData context not found")
    for i, source in enumerate(SOURCES):
        matches = [m for m in motion.findall("message") if m.findtext("source") == source]
        if len(matches) > 1:
            raise ValueError(f"Duplicate message: {source}")
        if matches:
            tr = matches[0].find("translation")
            expected = translations[i] if translations else ""
            if tr is None or (tr.text or "") != expected or (translations is None and tr.get("type") != "unfinished"):
                raise ValueError(f"Existing translation differs: {source}")
    context = re.search(r"<context>\s*<name>MotionData</name>(.*?)</context>", content, re.S)
    if context is None:
        raise ValueError("MotionData context not found")
    body = context.group(1)
    missing = [i for i, source in enumerate(SOURCES) if f"<source>{esc(source)}</source>" not in body]
    if not missing:
        return data, 0
    anchor = re.search(r"<message>\s*(?:(?!</message>).)*<source>" + re.escape(esc(ANCHOR)) +
                       r"</source>(?:(?!</message>).)*</message>" + re.escape(nl), body, re.S)
    if anchor is None:
        raise ValueError("MotionData/optical correction final message anchor not found")
    messages = "".join(make_message(SOURCES[i], translations[i] if translations else None, LINES[i], nl) for i in missing)
    pos = context.start(1) + anchor.end()
    result = (content[:pos] + messages + content[pos:]).encode("utf-8")
    ET.fromstring(result)
    return result, len(missing)


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    try:
        translations = {lang: tuple(text.splitlines()) for lang, text in TRANSLATIONS.items()}
        if len(translations) != 22 or len(LINES) != len(SOURCES):
            raise ValueError("Expected 22 complete languages and one location per source")
        if {p.stem for p in base.glob("*.ts")} != {"gyroflow", *translations}:
            raise ValueError("TS language inventory differs")
        for lang, values in translations.items():
            if len(values) != len(SOURCES) or any(not value.strip() for value in values):
                raise ValueError(f"{lang}: incomplete translations")
            for source, value in zip(SOURCES, values):
                if sorted(re.findall(r"%\d+|%n", source)) != sorted(re.findall(r"%\d+|%n", value)):
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
        # Check all originals before starting the batch write.
        for path, original, _, _ in pending:
            if path.read_bytes() != original:
                raise ValueError(f"{path.name}: changed during patching")
        for path, _, result, added in pending:
            if added:
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
