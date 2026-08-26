"""Fill Linux AppImage update-handoff translations after Qt lupdate."""

from html import escape
from pathlib import Path


READY = (
    "The AppImage is ready. Open its folder, then launch it when you are ready. "
    "Gyroflow will stay open."
)
OPEN = "Open containing folder"

TRANSLATIONS = {
    "cs": (
        "Soubor AppImage je připraven. Otevřete jeho složku a spusťte jej, až budete připraveni. Gyroflow zůstane otevřený.",
        "Otevřít složku",
    ),
    "da": (
        "AppImage-filen er klar. Åbn dens mappe, og start den, når du er klar. Gyroflow forbliver åben.",
        "Åbn indeholdende mappe",
    ),
    "de": (
        "Das AppImage ist bereit. Öffne den Ordner und starte es, wenn du bereit bist. Gyroflow bleibt geöffnet.",
        "Enthaltenden Ordner öffnen",
    ),
    "el": (
        "Το AppImage είναι έτοιμο. Ανοίξτε τον φάκελό του και εκκινήστε το όταν είστε έτοιμοι. Το Gyroflow θα παραμείνει ανοιχτό.",
        "Άνοιγμα φακέλου που το περιέχει",
    ),
    "es": (
        "El AppImage está listo. Abre su carpeta y ejecútalo cuando quieras. Gyroflow permanecerá abierto.",
        "Abrir carpeta contenedora",
    ),
    "fi": (
        "AppImage on valmis. Avaa sen kansio ja käynnistä se, kun olet valmis. Gyroflow pysyy avoinna.",
        "Avaa sisältävä kansio",
    ),
    "fr": (
        "L’AppImage est prête. Ouvrez son dossier, puis lancez-la lorsque vous êtes prêt. Gyroflow restera ouvert.",
        "Ouvrir le dossier contenant",
    ),
    "gl": (
        "A AppImage está lista. Abre o seu cartafol e execútaa cando queiras. Gyroflow permanecerá aberto.",
        "Abrir o cartafol contedor",
    ),
    "id": (
        "AppImage sudah siap. Buka foldernya, lalu jalankan saat Anda siap. Gyroflow akan tetap terbuka.",
        "Buka folder penyimpanan",
    ),
    "it": (
        "L'AppImage è pronta. Apri la cartella e avviala quando vuoi. Gyroflow rimarrà aperto.",
        "Apri cartella contenente",
    ),
    "ja": (
        "AppImage の準備ができました。フォルダーを開き、準備ができたら起動してください。Gyroflow は開いたままになります。",
        "保存先フォルダーを開く",
    ),
    "ko": (
        "AppImage가 준비되었습니다. 폴더를 연 다음 준비가 되면 실행하세요. Gyroflow는 계속 열려 있습니다.",
        "포함된 폴더 열기",
    ),
    "no": (
        "AppImage-filen er klar. Åpne mappen og start den når du er klar. Gyroflow forblir åpen.",
        "Åpne mappen",
    ),
    "pl": (
        "Plik AppImage jest gotowy. Otwórz jego folder i uruchom go, gdy będziesz gotowy. Gyroflow pozostanie otwarty.",
        "Otwórz folder zawierający",
    ),
    "pt": (
        "A AppImage está pronta. Abra a pasta e execute-a quando estiver pronto. O Gyroflow permanecerá aberto.",
        "Abrir pasta de destino",
    ),
    "pt_BR": (
        "O AppImage está pronto. Abra a pasta e execute-o quando estiver pronto. O Gyroflow permanecerá aberto.",
        "Abrir pasta do arquivo",
    ),
    "ru": (
        "AppImage готов. Откройте его папку и запустите, когда будете готовы. Gyroflow останется открытым.",
        "Открыть папку с файлом",
    ),
    "sk": (
        "Súbor AppImage je pripravený. Otvorte jeho priečinok a spustite ho, keď budete pripravení. Gyroflow zostane otvorený.",
        "Otvoriť priečinok",
    ),
    "tr": (
        "AppImage hazır. Klasörünü açın ve hazır olduğunuzda çalıştırın. Gyroflow açık kalacaktır.",
        "İçeren klasörü aç",
    ),
    "uk": (
        "AppImage готовий. Відкрийте його папку та запустіть, коли будете готові. Gyroflow залишиться відкритим.",
        "Відкрити папку з файлом",
    ),
    "zh_CN": (
        "AppImage 已准备就绪。请打开其所在文件夹，并在准备好后启动它。Gyroflow 将保持打开状态。",
        "打开文件所在目录",
    ),
    "zh_TW": (
        "AppImage 已準備就緒。請開啟其所在資料夾，並在準備好後啟動它。Gyroflow 將保持開啟狀態。",
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
    for source in (READY, OPEN):
        if f"<source>{escape(source, quote=False)}</source>" in text:
            raise RuntimeError(f"translation already exists for {source!r}")

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
