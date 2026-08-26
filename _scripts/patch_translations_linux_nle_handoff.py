"""Add localized Linux OpenFX manual-install guidance to all catalogs."""

from html import escape
from pathlib import Path


SOURCE = (
    "Automatic installation could not be completed. Open Terminal and run the "
    "following command:"
)

TRANSLATIONS = {
    "cs": "Automatickou instalaci se nepodařilo dokončit. Otevřete Terminál a spusťte následující příkaz:",
    "da": "Den automatiske installation kunne ikke fuldføres. Åbn Terminal, og kør følgende kommando:",
    "de": "Die automatische Installation konnte nicht abgeschlossen werden. Öffne das Terminal und führe den folgenden Befehl aus:",
    "el": "Η αυτόματη εγκατάσταση δεν ολοκληρώθηκε. Ανοίξτε το Τερματικό και εκτελέστε την ακόλουθη εντολή:",
    "es": "No se pudo completar la instalación automática. Abre Terminal y ejecuta el siguiente comando:",
    "fi": "Automaattista asennusta ei voitu suorittaa loppuun. Avaa Pääte ja suorita seuraava komento:",
    "fr": "L’installation automatique n’a pas pu être terminée. Ouvrez le Terminal et exécutez la commande suivante :",
    "gl": "Non se puido completar a instalación automática. Abre o Terminal e executa o seguinte comando:",
    "id": "Instalasi otomatis tidak dapat diselesaikan. Buka Terminal dan jalankan perintah berikut:",
    "it": "Non è stato possibile completare l'installazione automatica. Apri Terminale ed esegui il seguente comando:",
    "ja": "自動インストールを完了できませんでした。ターミナルを開き、次のコマンドを実行してください:",
    "ko": "자동 설치를 완료할 수 없습니다. 터미널을 열고 다음 명령을 실행하세요:",
    "no": "Den automatiske installasjonen kunne ikke fullføres. Åpne Terminal og kjør følgende kommando:",
    "pl": "Nie udało się ukończyć automatycznej instalacji. Otwórz Terminal i uruchom następujące polecenie:",
    "pt": "Não foi possível concluir a instalação automática. Abra o Terminal e execute o seguinte comando:",
    "pt_BR": "Não foi possível concluir a instalação automática. Abra o Terminal e execute o seguinte comando:",
    "ru": "Не удалось завершить автоматическую установку. Откройте Терминал и выполните следующую команду:",
    "sk": "Automatickú inštaláciu sa nepodarilo dokončiť. Otvorte Terminál a spustite nasledujúci príkaz:",
    "tr": "Otomatik kurulum tamamlanamadı. Terminal'i açın ve aşağıdaki komutu çalıştırın:",
    "uk": "Не вдалося завершити автоматичне встановлення. Відкрийте Термінал і виконайте таку команду:",
    "zh_CN": "无法完成自动安装。请打开终端并运行以下命令：",
    "zh_TW": "無法完成自動安裝。請開啟終端機並執行以下命令：",
}


def render_message(translation: str | None, newline: str) -> str:
    translated = (
        '<translation type="unfinished"></translation>'
        if translation is None
        else f"<translation>{escape(translation, quote=False)}</translation>"
    )
    return newline.join(
        (
            "    <message>",
            '        <location filename="../../src/ui/menu/NlePlugins.qml" line="102"/>',
            f"        <source>{escape(SOURCE, quote=False)}</source>",
            f"        {translated}",
            "    </message>",
        )
    )


def insert_message(text: str, translation: str | None, newline: str) -> str:
    source_token = f"<source>{escape(SOURCE, quote=False)}</source>"
    if source_token in text:
        return text

    anchor = "        <source>Unable to copy the plugin due to sandbox limitations."
    if text.count(anchor) != 1:
        raise RuntimeError("expected one sandbox-copy guidance anchor")
    anchor_start = text.index(anchor)
    message_end = text.index(f"    </message>{newline}", anchor_start) + len(
        f"    </message>{newline}"
    )
    return text[:message_end] + render_message(translation, newline) + newline + text[message_end:]


def main() -> None:
    root = Path(__file__).resolve().parents[1] / "resources" / "translations"
    actual = {path.stem for path in root.glob("*.ts") if path.stem != "gyroflow"}
    if actual != set(TRANSLATIONS):
        raise RuntimeError(
            f"translation catalog mismatch: missing={sorted(set(TRANSLATIONS) - actual)}, "
            f"extra={sorted(actual - set(TRANSLATIONS))}"
        )

    catalogs = {"gyroflow": None, **TRANSLATIONS}
    for language, translation in catalogs.items():
        path = root / f"{language}.ts"
        text = path.read_bytes().decode("utf-8")
        newline = "\r\n" if "\r\n" in text else "\n"
        text = insert_message(text, translation, newline)
        path.write_bytes(text.encode("utf-8"))
        print(f"updated {path.name}")


if __name__ == "__main__":
    main()
