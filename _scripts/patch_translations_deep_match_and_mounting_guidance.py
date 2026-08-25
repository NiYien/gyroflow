"""Patch translations for deep-match troubleshooting and mounting guidance.

This inserts two QML source strings into all 23 TS files, removes the three
superseded deep-match failure strings, and verifies that every released
language has a finished translation. It is idempotent.
"""
from __future__ import annotations

import pathlib
import sys


DEEP_SOURCE = (
    "No match found. Possible reasons:\n"
    "1. Not enough camera motion in the video.\n"
    "2. The gyro data does not cover the video's recording time.\n"
    "3. In-camera or lens stabilization was not turned off.\n"
    "4. The mounting position is incorrect.\n\n"
    "Please check and try again."
)
MOUNT_SOURCE = (
    "The device mounting position is relative to the camera, regardless of "
    "landscape or portrait orientation."
)

OLD_RENDERQUEUE_SOURCES = (
    "Not enough camera motion. Try a video with more movement.",
    "No match found (the gyro file may not cover this video, or the video's motion is too weak).",
    "No match found in any gyro file. The recordings may not cover this video, or the video's motion may be unreliable.",
)

TRANS: dict[str, tuple[str, str]] = {
    "cs": (
        "Nebyla nalezena shoda. Možné příčiny:\n1. Ve videu není dostatečný pohyb kamery.\n2. Data gyroskopu nepokrývají dobu záznamu videa.\n3. Stabilizace v těle fotoaparátu nebo objektivu nebyla vypnuta.\n4. Poloha uchycení je nesprávná.\n\nZkontrolujte nastavení a zkuste to znovu.",
        "Poloha zařízení se určuje vzhledem ke kameře bez ohledu na orientaci na šířku nebo na výšku.",
    ),
    "da": (
        "Intet match fundet. Mulige årsager:\n1. Der er ikke nok kamerabevægelse i videoen.\n2. Gyrodataene dækker ikke videoens optagelsestidspunkt.\n3. Stabilisering i kameraet eller objektivet blev ikke slået fra.\n4. Monteringspositionen er forkert.\n\nKontrollér dette, og prøv igen.",
        "Enhedens monteringsposition er i forhold til kameraet, uanset om der optages liggende eller stående.",
    ),
    "de": (
        "Keine Übereinstimmung gefunden. Mögliche Ursachen:\n1. Das Video enthält nicht genügend Kamerabewegung.\n2. Die Gyrodaten decken den Aufnahmezeitraum des Videos nicht ab.\n3. Die Stabilisierung in Kamera oder Objektiv wurde nicht ausgeschaltet.\n4. Die Montageposition ist falsch.\n\nBitte prüfen und erneut versuchen.",
        "Die Montageposition des Geräts bezieht sich auf die Kamera und ist unabhängig von Quer- oder Hochformat.",
    ),
    "el": (
        "Δεν βρέθηκε αντιστοίχιση. Πιθανές αιτίες:\n1. Δεν υπάρχει αρκετή κίνηση κάμερας στο βίντεο.\n2. Τα δεδομένα γυροσκοπίου δεν καλύπτουν τον χρόνο εγγραφής του βίντεο.\n3. Η σταθεροποίηση στην κάμερα ή στον φακό δεν απενεργοποιήθηκε.\n4. Η θέση τοποθέτησης είναι λανθασμένη.\n\nΕλέγξτε τα παραπάνω και δοκιμάστε ξανά.",
        "Η θέση τοποθέτησης της συσκευής είναι σε σχέση με την κάμερα, ανεξάρτητα από οριζόντιο ή κατακόρυφο προσανατολισμό.",
    ),
    "es": (
        "No se encontró ninguna coincidencia. Posibles causas:\n1. No hay suficiente movimiento de cámara en el video.\n2. Los datos del giroscopio no cubren el momento de grabación del video.\n3. No se desactivó la estabilización de la cámara o del objetivo.\n4. La posición de montaje es incorrecta.\n\nCompruébelo e inténtelo de nuevo.",
        "La posición de montaje del dispositivo es relativa a la cámara, independientemente de si se graba en horizontal o vertical.",
    ),
    "fi": (
        "Vastaavuutta ei löytynyt. Mahdolliset syyt:\n1. Videossa ei ole riittävästi kameran liikettä.\n2. Gyrodata ei kata videon tallennusajankohtaa.\n3. Kameran tai objektiivin vakautusta ei poistettu käytöstä.\n4. Asennusasento on virheellinen.\n\nTarkista nämä ja yritä uudelleen.",
        "Laitteen asennusasento määritetään suhteessa kameraan riippumatta vaaka- tai pystysuuntaisesta kuvauksesta.",
    ),
    "fr": (
        "Aucune correspondance trouvée. Causes possibles :\n1. Le mouvement de la caméra dans la vidéo est insuffisant.\n2. Les données gyroscopiques ne couvrent pas la période d’enregistrement de la vidéo.\n3. La stabilisation du boîtier ou de l’objectif n’a pas été désactivée.\n4. La position de montage est incorrecte.\n\nVérifiez ces points et réessayez.",
        "La position de montage de l’appareil est définie par rapport à la caméra, indépendamment d’une prise de vue horizontale ou verticale.",
    ),
    "gl": (
        "Non se atopou coincidencia. Posibles causas:\n1. Non hai movemento de cámara abondo no vídeo.\n2. Os datos do xiroscopio non cobren o momento de gravación do vídeo.\n3. Non se desactivou a estabilización da cámara ou do obxectivo.\n4. A posición de montaxe é incorrecta.\n\nRevise estes puntos e ténteo de novo.",
        "A posición de montaxe do dispositivo é relativa á cámara, independentemente de gravar en horizontal ou vertical.",
    ),
    "id": (
        "Kecocokan tidak ditemukan. Kemungkinan penyebab:\n1. Gerakan kamera dalam video tidak cukup.\n2. Data giroskop tidak mencakup waktu perekaman video.\n3. Stabilisasi pada kamera atau lensa belum dimatikan.\n4. Posisi pemasangan tidak benar.\n\nPeriksa lalu coba lagi.",
        "Posisi pemasangan perangkat mengacu pada kamera, tidak bergantung pada orientasi lanskap atau potret.",
    ),
    "it": (
        "Nessuna corrispondenza trovata. Possibili cause:\n1. Il movimento della fotocamera nel video non è sufficiente.\n2. I dati del giroscopio non coprono il momento di registrazione del video.\n3. La stabilizzazione della fotocamera o dell’obiettivo non è stata disattivata.\n4. La posizione di montaggio non è corretta.\n\nControlla e riprova.",
        "La posizione di montaggio del dispositivo è relativa alla fotocamera, indipendentemente dall’orientamento orizzontale o verticale.",
    ),
    "ja": (
        "一致するデータが見つかりませんでした。考えられる原因：\n1. 動画内のカメラの動きが不足しています。\n2. ジャイロデータが動画の撮影時刻をカバーしていません。\n3. カメラ内またはレンズの手ぶれ補正がオフになっていません。\n4. 取り付け位置が正しくありません。\n\n確認してからもう一度お試しください。",
        "デバイスの取り付け位置はカメラを基準とし、横向き撮影か縦向き撮影かには関係ありません。",
    ),
    "ko": (
        "일치 항목을 찾지 못했습니다. 가능한 원인:\n1. 영상에 카메라 움직임이 충분하지 않습니다.\n2. 자이로 데이터가 영상 촬영 시간을 포함하지 않습니다.\n3. 카메라 또는 렌즈 손떨림 보정이 꺼져 있지 않습니다.\n4. 장착 위치가 올바르지 않습니다.\n\n확인한 후 다시 시도하세요.",
        "장치 장착 위치는 카메라를 기준으로 하며 가로 또는 세로 촬영 방향과 관계없습니다.",
    ),
    "no": (
        "Ingen treff funnet. Mulige årsaker:\n1. Det er ikke nok kamerabevegelse i videoen.\n2. Gyrodataene dekker ikke tidspunktet da videoen ble tatt opp.\n3. Stabilisering i kameraet eller objektivet ble ikke slått av.\n4. Monteringsposisjonen er feil.\n\nKontroller dette og prøv igjen.",
        "Enhetens monteringsposisjon er i forhold til kameraet, uavhengig av liggende eller stående opptak.",
    ),
    "pl": (
        "Nie znaleziono dopasowania. Możliwe przyczyny:\n1. W filmie jest za mało ruchu kamery.\n2. Dane żyroskopu nie obejmują czasu nagrania filmu.\n3. Stabilizacja w aparacie lub obiektywie nie została wyłączona.\n4. Pozycja montażu jest nieprawidłowa.\n\nSprawdź te punkty i spróbuj ponownie.",
        "Pozycja montażu urządzenia jest określana względem kamery, niezależnie od nagrywania poziomo lub pionowo.",
    ),
    "pt": (
        "Nenhuma correspondência encontrada. Possíveis causas:\n1. Não há movimento de câmara suficiente no vídeo.\n2. Os dados do giroscópio não abrangem o momento da gravação do vídeo.\n3. A estabilização da câmara ou da objetiva não foi desativada.\n4. A posição de montagem está incorreta.\n\nVerifique e tente novamente.",
        "A posição de montagem do dispositivo é relativa à câmara, independentemente da gravação horizontal ou vertical.",
    ),
    "pt_BR": (
        "Nenhuma correspondência encontrada. Possíveis causas:\n1. Não há movimento de câmera suficiente no vídeo.\n2. Os dados do giroscópio não abrangem o momento da gravação do vídeo.\n3. A estabilização da câmera ou da lente não foi desativada.\n4. A posição de montagem está incorreta.\n\nVerifique e tente novamente.",
        "A posição de montagem do dispositivo é relativa à câmera, independentemente da gravação horizontal ou vertical.",
    ),
    "ru": (
        "Совпадение не найдено. Возможные причины:\n1. В видео недостаточно движения камеры.\n2. Данные гироскопа не охватывают время записи видео.\n3. Стабилизация в камере или объективе не была отключена.\n4. Положение установки указано неверно.\n\nПроверьте и повторите попытку.",
        "Положение установки устройства задаётся относительно камеры и не зависит от горизонтальной или вертикальной съёмки.",
    ),
    "sk": (
        "Zhoda sa nenašla. Možné príčiny:\n1. Vo videu nie je dostatočný pohyb kamery.\n2. Údaje gyroskopu nepokrývajú čas záznamu videa.\n3. Stabilizácia v tele fotoaparátu alebo objektíve nebola vypnutá.\n4. Poloha uchytenia je nesprávna.\n\nSkontrolujte nastavenie a skúste to znova.",
        "Poloha zariadenia sa určuje vzhľadom na kameru bez ohľadu na orientáciu na šírku alebo na výšku.",
    ),
    "tr": (
        "Eşleşme bulunamadı. Olası nedenler:\n1. Videoda yeterli kamera hareketi yok.\n2. Jiroskop verileri videonun kayıt zamanını kapsamıyor.\n3. Kamera içi veya lens sabitleme kapatılmamış.\n4. Montaj konumu yanlış.\n\nKontrol edip tekrar deneyin.",
        "Cihazın montaj konumu kameraya göredir; yatay veya dikey çekimden bağımsızdır.",
    ),
    "uk": (
        "Збігів не знайдено. Можливі причини:\n1. У відео недостатньо руху камери.\n2. Дані гіроскопа не охоплюють час запису відео.\n3. Стабілізацію в камері або об’єктиві не було вимкнено.\n4. Положення встановлення вказано неправильно.\n\nПеревірте та повторіть спробу.",
        "Положення встановлення пристрою задається відносно камери й не залежить від горизонтальної чи вертикальної зйомки.",
    ),
    "zh_CN": (
        "未找到匹配，可能原因：\n1. 视频中的相机运动不足\n2. 陀螺仪数据未覆盖视频录制时间\n3. 相机机身或镜头防抖未关闭\n4. 安装位置选择不正确\n\n请检查后重试。",
        "设备安装位置是相对于相机，与横竖拍无关",
    ),
    "zh_TW": (
        "未找到匹配，可能原因：\n1. 影片中的相機運動不足\n2. 陀螺儀資料未涵蓋影片錄製時間\n3. 相機機身或鏡頭防震未關閉\n4. 安裝位置選擇不正確\n\n請檢查後再試。",
        "裝置安裝位置是相對於相機，與橫拍或直拍無關",
    ),
}


def xml_escape(text: str) -> str:
    return (
        text.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace('"', "&quot;")
        .replace("'", "&apos;")
    )


def context_bounds(raw: str, name: str) -> tuple[int, int]:
    name_pos = raw.find(f"<name>{name}</name>")
    if name_pos == -1:
        raise RuntimeError(f"context {name} not found")
    start = raw.rfind("<context>", 0, name_pos)
    end = raw.find("</context>", name_pos)
    if start == -1 or end == -1:
        raise RuntimeError(f"context bounds for {name} not found")
    return start, end


def remove_message(raw: str, context: str, source: str, eol: str) -> str:
    while True:
        start, end = context_bounds(raw, context)
        source_pos = raw.find(f"<source>{xml_escape(source)}</source>", start, end)
        if source_pos == -1:
            return raw
        msg_start = raw.rfind(f"    <message>{eol}", start, source_pos)
        msg_end = raw.find(f"    </message>{eol}", source_pos, end)
        if msg_start == -1 or msg_end == -1:
            raise RuntimeError(f"message bounds for {source!r} not found")
        raw = raw[:msg_start] + raw[msg_end + len(f"    </message>{eol}") :]


def message_block(eol: str, filename: str, line: int, source: str, trans: str | None) -> str:
    translation = (
        '<translation type="unfinished"></translation>'
        if trans is None
        else f"<translation>{xml_escape(trans)}</translation>"
    )
    return (
        f"    <message>{eol}"
        f'        <location filename="{filename}" line="{line}"/>{eol}'
        f"        <source>{xml_escape(source)}</source>{eol}"
        f"        {translation}{eol}"
        f"    </message>{eol}"
    )


def insert_after_anchor(
    raw: str,
    context: str,
    anchor: str,
    source: str,
    block: str,
    eol: str,
) -> tuple[str, bool]:
    start, end = context_bounds(raw, context)
    if f"<source>{xml_escape(source)}</source>" in raw[start:end]:
        return raw, False
    anchor_pos = raw.find(f"<source>{xml_escape(anchor)}</source>", start, end)
    if anchor_pos == -1:
        raise RuntimeError(f"anchor {anchor!r} not found in {context}")
    close_pos = raw.find(f"    </message>{eol}", anchor_pos, end)
    if close_pos == -1:
        raise RuntimeError(f"anchor closing message not found in {context}")
    insert_at = close_pos + len(f"    </message>{eol}")
    return raw[:insert_at] + block + raw[insert_at:], True


def patch_file(path: pathlib.Path, deep_trans: str | None, mount_trans: str | None) -> list[str]:
    raw = path.read_bytes().decode("utf-8")
    original = raw
    eol = "\r\n" if "\r\n" in raw else "\n"
    for source in OLD_RENDERQUEUE_SOURCES:
        raw = remove_message(raw, "RenderQueue", source, eol)
    raw, deep_added = insert_after_anchor(
        raw,
        "RenderQueue",
        "Deep match succeeded (offset %1 s). Clips from the same day will be matched automatically.",
        DEEP_SOURCE,
        message_block(eol, "../../src/ui/RenderQueue.qml", 651, DEEP_SOURCE, deep_trans),
        eol,
    )
    raw, mount_added = insert_after_anchor(
        raw,
        "MountingPresetSelector",
        "Mounting position",
        MOUNT_SOURCE,
        message_block(
            eol,
            "../../src/ui/menu/MountingPresetSelector.qml",
            280,
            MOUNT_SOURCE,
            mount_trans,
        ),
        eol,
    )
    if raw != original:
        path.write_bytes(raw.encode("utf-8"))
    return [name for name, added in (("deep", deep_added), ("mount", mount_added)) if added]


def validate_file(path: pathlib.Path, require_finished: bool) -> list[str]:
    raw = path.read_bytes().decode("utf-8")
    errors: list[str] = []
    for source in (DEEP_SOURCE, MOUNT_SOURCE):
        tag = f"<source>{xml_escape(source)}</source>"
        if tag not in raw:
            errors.append(f"missing {source[:24]!r}")
        elif require_finished:
            tail = raw[raw.find(tag) + len(tag) :]
            translation = tail[: tail.find("</message>")]
            if 'type="unfinished"' in translation or "<translation></translation>" in translation:
                errors.append(f"unfinished {source[:24]!r}")
    for source in OLD_RENDERQUEUE_SOURCES:
        if f"<source>{xml_escape(source)}</source>" in raw:
            errors.append(f"obsolete source remains {source[:24]!r}")
    return errors


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    expected = {"gyroflow", *TRANS.keys()}
    actual = {path.stem for path in root.glob("*.ts")}
    if actual != expected:
        print(f"TS SET MISMATCH: missing={sorted(expected-actual)} extra={sorted(actual-expected)}")
        return 1

    ok = True
    for name in sorted(expected):
        path = root / f"{name}.ts"
        translations = TRANS.get(name)
        try:
            patched = patch_file(path, *(translations or (None, None)))
            errors = validate_file(path, require_finished=name != "gyroflow")
        except RuntimeError as exc:
            print(f"{name}: ERROR {exc}")
            ok = False
            continue
        if errors:
            print(f"{name}: patched={patched} ERRORS={errors}")
            ok = False
        else:
            print(f"{name}: patched={patched}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
