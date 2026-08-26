"""Fill Linux AppImage update-handoff translations after Qt lupdate."""

from html import escape
from pathlib import Path


OLD_READY = (
    "The AppImage is ready. Open its folder, then launch it when you are ready. "
    "Gyroflow will stay open."
)
READY = (
    "The AppImage is ready. Open its folder, exit Gyroflow, replace your previous "
    "AppImage, then start the new file. Gyroflow will stay open until you close it."
)
OPEN = "Open containing folder"

TRANSLATIONS = {
    "cs": (
        "Soubor AppImage je připraven. Otevřete jeho složku, ukončete Gyroflow, nahraďte předchozí AppImage a poté spusťte nový soubor. Gyroflow zůstane otevřený, dokud jej nezavřete.",
        "Otevřít složku",
    ),
    "da": (
        "AppImage-filen er klar. Åbn dens mappe, afslut Gyroflow, erstat din tidligere AppImage, og start derefter den nye fil. Gyroflow forbliver åben, indtil du lukker den.",
        "Åbn indeholdende mappe",
    ),
    "de": (
        "Das AppImage ist bereit. Öffne den Ordner, beende Gyroflow, ersetze dein bisheriges AppImage und starte anschließend die neue Datei. Gyroflow bleibt geöffnet, bis du es schließt.",
        "Enthaltenden Ordner öffnen",
    ),
    "el": (
        "Το AppImage είναι έτοιμο. Ανοίξτε τον φάκελό του, κλείστε το Gyroflow, αντικαταστήστε το προηγούμενο AppImage και μετά εκκινήστε το νέο αρχείο. Το Gyroflow θα παραμείνει ανοιχτό μέχρι να το κλείσετε.",
        "Άνοιγμα φακέλου που το περιέχει",
    ),
    "es": (
        "El AppImage está listo. Abre su carpeta, cierra Gyroflow, sustituye el AppImage anterior y luego inicia el archivo nuevo. Gyroflow permanecerá abierto hasta que lo cierres.",
        "Abrir carpeta contenedora",
    ),
    "fi": (
        "AppImage on valmis. Avaa sen kansio, sulje Gyroflow, korvaa aiempi AppImage ja käynnistä sitten uusi tiedosto. Gyroflow pysyy avoinna, kunnes suljet sen.",
        "Avaa sisältävä kansio",
    ),
    "fr": (
        "L’AppImage est prête. Ouvrez son dossier, quittez Gyroflow, remplacez votre ancienne AppImage, puis lancez le nouveau fichier. Gyroflow restera ouvert jusqu’à ce que vous le fermiez.",
        "Ouvrir le dossier contenant",
    ),
    "gl": (
        "A AppImage está lista. Abre o seu cartafol, pecha Gyroflow, substitúe a AppImage anterior e despois inicia o ficheiro novo. Gyroflow permanecerá aberto ata que o peches.",
        "Abrir o cartafol contedor",
    ),
    "id": (
        "AppImage sudah siap. Buka foldernya, keluar dari Gyroflow, ganti AppImage sebelumnya, lalu jalankan file baru. Gyroflow akan tetap terbuka sampai Anda menutupnya.",
        "Buka folder penyimpanan",
    ),
    "it": (
        "L'AppImage è pronta. Apri la cartella, chiudi Gyroflow, sostituisci l'AppImage precedente, quindi avvia il nuovo file. Gyroflow rimarrà aperto finché non lo chiudi.",
        "Apri cartella contenente",
    ),
    "ja": (
        "AppImage の準備ができました。フォルダーを開き、Gyroflow を終了して以前の AppImage を置き換え、その後新しいファイルを起動してください。Gyroflow は閉じるまで開いたままになります。",
        "保存先フォルダーを開く",
    ),
    "ko": (
        "AppImage가 준비되었습니다. 폴더를 열고 Gyroflow를 종료한 다음 이전 AppImage를 교체하고 새 파일을 실행하세요. Gyroflow는 사용자가 닫을 때까지 열린 상태로 유지됩니다.",
        "포함된 폴더 열기",
    ),
    "no": (
        "AppImage-filen er klar. Åpne mappen, avslutt Gyroflow, erstatt den forrige AppImage-filen, og start deretter den nye filen. Gyroflow forblir åpen til du lukker den.",
        "Åpne mappen",
    ),
    "pl": (
        "Plik AppImage jest gotowy. Otwórz jego folder, zamknij Gyroflow, zastąp poprzedni plik AppImage, a następnie uruchom nowy plik. Gyroflow pozostanie otwarty, dopóki go nie zamkniesz.",
        "Otwórz folder zawierający",
    ),
    "pt": (
        "A AppImage está pronta. Abra a pasta, saia do Gyroflow, substitua a AppImage anterior e, em seguida, inicie o novo ficheiro. O Gyroflow permanecerá aberto até o fechar.",
        "Abrir pasta de destino",
    ),
    "pt_BR": (
        "O AppImage está pronto. Abra a pasta, saia do Gyroflow, substitua o AppImage anterior e, em seguida, inicie o novo arquivo. O Gyroflow permanecerá aberto até você fechá-lo.",
        "Abrir pasta do arquivo",
    ),
    "ru": (
        "AppImage готов. Откройте его папку, закройте Gyroflow, замените предыдущий AppImage, затем запустите новый файл. Gyroflow останется открытым, пока вы его не закроете.",
        "Открыть папку с файлом",
    ),
    "sk": (
        "Súbor AppImage je pripravený. Otvorte jeho priečinok, ukončite Gyroflow, nahraďte predchádzajúci AppImage a potom spustite nový súbor. Gyroflow zostane otvorený, kým ho nezavriete.",
        "Otvoriť priečinok",
    ),
    "tr": (
        "AppImage hazır. Klasörünü açın, Gyroflow'dan çıkın, önceki AppImage'ı değiştirin ve ardından yeni dosyayı başlatın. Gyroflow siz kapatana kadar açık kalacaktır.",
        "İçeren klasörü aç",
    ),
    "uk": (
        "AppImage готовий. Відкрийте його папку, закрийте Gyroflow, замініть попередній AppImage, а потім запустіть новий файл. Gyroflow залишатиметься відкритим, доки ви його не закриєте.",
        "Відкрити папку з файлом",
    ),
    "zh_CN": (
        "AppImage 已准备就绪。请打开其所在文件夹，退出 Gyroflow，替换之前的 AppImage，然后启动新文件。Gyroflow 将保持打开状态，直到您将其关闭。",
        "打开文件所在目录",
    ),
    "zh_TW": (
        "AppImage 已準備就緒。請開啟其所在資料夾，退出 Gyroflow，取代先前的 AppImage，然後啟動新檔案。Gyroflow 將保持開啟狀態，直到您將其關閉。",
        "開啟檔案所在資料夾",
    ),
}


def render_message(
    source: str, translation: str | None, line: int, newline: str
) -> str:
    translated = (
        '<translation type="unfinished"></translation>'
        if translation is None
        else f"<translation>{escape(translation, quote=False)}</translation>"
    )
    return newline.join(
        (
            "    <message>",
            f'        <location filename="../../src/ui/App.qml" line="{line}"/>',
            f"        <source>{escape(source, quote=False)}</source>",
            f"        {translated}",
            "    </message>",
        )
    )


def insert_messages(
    text: str,
    ready_translation: str | None,
    open_translation: str | None,
    newline: str,
) -> str:
    ready_token = f"        <source>{escape(READY, quote=False)}</source>"
    if ready_token in text:
        return text

    old_ready_token = f"        <source>{escape(OLD_READY, quote=False)}</source>"
    if old_ready_token in text:
        source_start = text.index(old_ready_token)
        message_start = text.rindex("    <message>", 0, source_start)
        message_end = text.index(f"    </message>{newline}", source_start) + len(
            f"    </message>{newline}"
        )
        replacement = render_message(READY, ready_translation, 2705, newline) + newline
        return text[:message_start] + replacement + text[message_end:]

    if f"<source>{escape(OPEN, quote=False)}</source>" in text:
        raise RuntimeError("Open-containing-folder translation exists without ready text")

    anchor = "        <source>Open DMG and quit</source>"
    if text.count(anchor) != 1:
        raise RuntimeError("expected one Open DMG and quit anchor")
    anchor_start = text.index(anchor)
    message_end = text.index(f"    </message>{newline}", anchor_start) + len(
        f"    </message>{newline}"
    )
    messages = (
        render_message(READY, ready_translation, 2705, newline)
        + newline
        + render_message(OPEN, open_translation, 2712, newline)
        + newline
    )
    return text[:message_end] + messages + text[message_end:]


def main() -> None:
    root = Path(__file__).resolve().parents[1] / "resources" / "translations"
    actual = {path.stem for path in root.glob("*.ts") if path.stem != "gyroflow"}
    if actual != set(TRANSLATIONS):
        raise RuntimeError(
            f"translation catalog mismatch: missing={sorted(set(TRANSLATIONS) - actual)}, "
            f"extra={sorted(actual - set(TRANSLATIONS))}"
        )

    catalogs = {"gyroflow": (None, None), **TRANSLATIONS}
    for language, (ready_translation, open_translation) in catalogs.items():
        path = root / f"{language}.ts"
        text = path.read_bytes().decode("utf-8")
        newline = "\r\n" if "\r\n" in text else "\n"
        text = insert_messages(text, ready_translation, open_translation, newline)
        path.write_bytes(text.encode("utf-8"))
        print(f"updated {path.name}")


if __name__ == "__main__":
    main()
