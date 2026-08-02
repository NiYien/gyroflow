"""One-shot patch: inject the device lens-display strings into the
LensGroupConfig context of all 23 .ts files (device-language-and-lens-display).

Strings:
    "Display on Device"      (button, matches the NiYien Tool wording)
    "Clear Device Display"   (button, matches the NiYien Tool wording)
    "Sent to the device."    (transient fire-and-forget notice)

Run from repo root:
    python _scripts/patch_translations_device_lens_display.py

Then regenerate .qm bundles via lrelease (see patch_translations_batch_sync_prompts.py
docstring for the lrelease invocation).

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

QML = "../../src/ui/menu/LensGroupConfig.qml"

# (source, line, {lang: translation})
MESSAGES: list[tuple[str, int, dict[str, str]]] = [
    (
        "Display on Device",
        755,
        {
            "cs": "Zobrazit na zařízení",
            "da": "Vis på enheden",
            "de": "Auf dem Gerät anzeigen",
            "el": "Εμφάνιση στη συσκευή",
            "es": "Mostrar en el dispositivo",
            "fi": "Näytä laitteessa",
            "fr": "Afficher sur l'appareil",
            "gl": "Mostrar no dispositivo",
            "id": "Tampilkan di perangkat",
            "it": "Mostra sul dispositivo",
            "ja": "デバイスに表示",
            "ko": "장치에 표시",
            "no": "Vis på enheten",
            "pl": "Wyświetl na urządzeniu",
            "pt": "Mostrar no dispositivo",
            "pt_BR": "Mostrar no dispositivo",
            "ru": "Показать на устройстве",
            "sk": "Zobraziť na zariadení",
            "tr": "Cihazda göster",
            "uk": "Показати на пристрої",
            "zh_CN": "在设备中显示",
            "zh_TW": "在裝置中顯示",
        },
    ),
    (
        "Clear Device Display",
        765,
        {
            "cs": "Vymazat displej zařízení",
            "da": "Ryd enhedens visning",
            "de": "Geräteanzeige löschen",
            "el": "Καθαρισμός οθόνης συσκευής",
            "es": "Borrar pantalla del dispositivo",
            "fi": "Tyhjennä laitteen näyttö",
            "fr": "Effacer l'affichage de l'appareil",
            "gl": "Borrar a pantalla do dispositivo",
            "id": "Hapus tampilan perangkat",
            "it": "Cancella display del dispositivo",
            "ja": "デバイス表示をクリア",
            "ko": "장치 표시 지우기",
            "no": "Tøm enhetens visning",
            "pl": "Wyczyść wyświetlacz urządzenia",
            "pt": "Limpar ecrã do dispositivo",
            "pt_BR": "Limpar exibição do dispositivo",
            "ru": "Очистить дисплей устройства",
            "sk": "Vymazať displej zariadenia",
            "tr": "Cihaz ekranını temizle",
            "uk": "Очистити дисплей пристрою",
            "zh_CN": "清除设备显示",
            "zh_TW": "清除裝置顯示",
        },
    ),
    (
        "Sent to the device.",
        759,
        {
            "cs": "Odesláno do zařízení.",
            "da": "Sendt til enheden.",
            "de": "An das Gerät gesendet.",
            "el": "Στάλθηκε στη συσκευή.",
            "es": "Enviado al dispositivo.",
            "fi": "Lähetetty laitteelle.",
            "fr": "Envoyé à l'appareil.",
            "gl": "Enviado ao dispositivo.",
            "id": "Terkirim ke perangkat.",
            "it": "Inviato al dispositivo.",
            "ja": "デバイスに送信しました。",
            "ko": "장치로 전송했습니다.",
            "no": "Sendt til enheten.",
            "pl": "Wysłano do urządzenia.",
            "pt": "Enviado para o dispositivo.",
            "pt_BR": "Enviado para o dispositivo.",
            "ru": "Отправлено на устройство.",
            "sk": "Odoslané do zariadenia.",
            "tr": "Cihaza gönderildi.",
            "uk": "Надіслано на пристрій.",
            "zh_CN": "已发送到设备。",
            "zh_TW": "已傳送到裝置。",
        },
    ),
]


def make_message(source: str, line: int, translation: str | None) -> str:
    if translation is None:
        trans_tag = '<translation type="unfinished"></translation>'
    else:
        trans_tag = f"<translation>{translation}</translation>"
    return (
        "    <message>\n"
        f'        <location filename="{QML}" line="{line}"/>\n'
        f"        <source>{source}</source>\n"
        f"        {trans_tag}\n"
        "    </message>\n"
    )


def patch_file(path: pathlib.Path, lang: str | None) -> str:
    content = path.read_text(encoding="utf-8")
    sentinel = "<source>Display on Device</source>"
    if sentinel in content:
        return f"SKIP {path.name} (already patched)"

    needle = "    <name>LensGroupConfig</name>\n"
    if needle not in content:
        return f"FAIL {path.name}: <LensGroupConfig> context not found"

    block = "".join(
        make_message(source, line, None if lang is None else trans.get(lang))
        for source, line, trans in MESSAGES
    )
    path.write_text(content.replace(needle, needle + block, 1), encoding="utf-8")
    return f"OK   {path.name}"


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    if not base.is_dir():
        print(f"Translation dir missing: {base}", file=sys.stderr)
        return 1

    print(patch_file(base / "gyroflow.ts", None))

    for lang in MESSAGES[0][2]:
        path = base / f"{lang}.ts"
        if not path.is_file():
            print(f"MISS {path.name}")
            continue
        print(patch_file(path, lang))
    return 0


if __name__ == "__main__":
    sys.exit(main())
