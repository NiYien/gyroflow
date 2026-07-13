"""One-shot patch: add translations for the new plugin-install-blocked string
(nle-plugin-install-blocked-message, 2026-07-13) to all 22 language .ts files
plus the gyroflow.ts source template.

When the elevated plugin copy fails (typically because DaVinci Resolve /
Premiere Pro has the plugin DLL loaded on Windows), NlePlugins.qml now shows a
clear "close your video editor and try again" message instead of the raw Debug
error string. The host app names are injected via %1 and must NOT be
translated.

Unlike lupdate-based flows, this inserts the <message> block directly after
the existing "Video editor plugins" entry in the NlePlugins context, so the
new entry lands in the right context without lupdate rewriting the whole file.

Run from repo root:
    python _scripts/patch_translations_plugin_copy_blocked.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

# Anchor: the existing "Video editor plugins" string (menu title, first message
# in the NlePlugins context). The new entry is inserted immediately after this
# message block. It must be unique per file.
ANCHOR = "Video editor plugins"

# Source line number in src/ui/menu/NlePlugins.qml (informational only;
# lrelease matches by context + source, not by location).
LINE = 100

# Real newline inside <source>, matching how lupdate serializes \n in qsTr.
SOURCE = (
    "Unable to install the plugin because the plugin file is in use.\n"
    "If %1 is currently running, please close it and then click install again."
)

# Per-language translations. %1 is the untranslated host app name list.
# "plugin" terminology reuses each language's existing "Video editor plugins"
# translation.
TRANS: dict[str, str] = {
    "cs": "Zásuvný modul nelze nainstalovat, protože soubor zásuvného modulu se právě používá.\nPokud je %1 právě spuštěn, zavřete jej a poté znovu klikněte na instalaci.",
    "da": "Kan ikke installere plugin'et, fordi plugin-filen er i brug.\nHvis %1 kører i øjeblikket, skal du lukke det og derefter klikke på installer igen.",
    "de": "Das Plugin kann nicht installiert werden, da die Plugin-Datei in Verwendung ist.\nWenn %1 gerade ausgeführt wird, schließen Sie es und klicken Sie dann erneut auf Installieren.",
    "el": "Δεν είναι δυνατή η εγκατάσταση του πρόσθετου επειδή το αρχείο του πρόσθετου χρησιμοποιείται.\nΕάν το %1 εκτελείται αυτήν τη στιγμή, κλείστε το και μετά κάντε ξανά κλικ στην εγκατάσταση.",
    "es": "No se puede instalar el complemento porque el archivo del complemento está en uso.\nSi %1 se está ejecutando actualmente, ciérrelo y luego haga clic en instalar de nuevo.",
    "fi": "Liitännäistä ei voi asentaa, koska liitännäistiedosto on käytössä.\nJos %1 on parhaillaan käynnissä, sulje se ja napsauta sitten asennusta uudelleen.",
    "fr": "Impossible d'installer le plugin car le fichier du plugin est en cours d'utilisation.\nSi %1 est en cours d'exécution, fermez-le puis cliquez à nouveau sur installer.",
    "gl": "Non se pode instalar o complemento porque o ficheiro do complemento está en uso.\nSe %1 se está a executar, pécheo e despois prema de novo en instalar.",
    "id": "Tidak dapat memasang plugin karena file plugin sedang digunakan.\nJika %1 sedang berjalan, tutup aplikasi tersebut lalu klik pasang lagi.",
    "it": "Impossibile installare il plugin perché il file del plugin è in uso.\nSe %1 è attualmente in esecuzione, chiudilo e poi fai di nuovo clic su installa.",
    "ja": "プラグインファイルが使用中のため、プラグインをインストールできません。\n%1 が実行中の場合は、終了してからもう一度インストールをクリックしてください。",
    "ko": "플러그인 파일이 사용 중이어서 플러그인을 설치할 수 없습니다.\n%1이(가) 실행 중이라면 종료한 후 다시 설치를 클릭하세요.",
    "no": "Kan ikke installere programtillegget fordi programtilleggsfilen er i bruk.\nHvis %1 kjører, lukk det og klikk deretter på installer igjen.",
    "pl": "Nie można zainstalować wtyczki, ponieważ plik wtyczki jest w użyciu.\nJeśli %1 jest obecnie uruchomiony, zamknij go, a następnie kliknij ponownie instalację.",
    "pt": "Não é possível instalar o plugin porque o ficheiro do plugin está em uso.\nSe o %1 estiver em execução, feche-o e depois clique novamente em instalar.",
    "pt_BR": "Não é possível instalar o plugin porque o arquivo do plugin está em uso.\nSe o %1 estiver em execução, feche-o e depois clique em instalar novamente.",
    "ru": "Не удаётся установить плагин, так как файл плагина используется.\nЕсли %1 сейчас запущен, закройте его и затем снова нажмите установить.",
    "sk": "Zásuvný modul nie je možné nainštalovať, pretože súbor zásuvného modulu sa práve používa.\nAk je %1 práve spustený, zatvorte ho a potom znova kliknite na inštaláciu.",
    "tr": "Eklenti dosyası kullanımda olduğu için eklenti yüklenemiyor.\n%1 şu anda çalışıyorsa, kapatın ve ardından yükle'ye tekrar tıklayın.",
    "uk": "Не вдалося встановити плагін, оскільки файл плагіна використовується.\nЯкщо %1 зараз запущено, закрийте його та знову натисніть встановити.",
    "zh_CN": "无法安装插件，插件文件正在被占用。\n如果 %1 正在运行，请先将其关闭，然后再点击安装。",
    "zh_TW": "無法安裝外掛，外掛檔案正在被佔用。\n如果 %1 正在執行，請先將其關閉，然後再點擊安裝。",
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


def patch_file(path: pathlib.Path, trans: str | None) -> bool:
    # trans=None -> source template (gyroflow.ts): emit unfinished translation.
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"

    escaped_source = xml_escape(SOURCE).replace("\n", eol)

    # Already patched?
    if f"<source>{escaped_source}</source>" in raw:
        return False

    anchor = f"<source>{xml_escape(ANCHOR)}</source>"
    if raw.count(anchor) != 1:
        raise RuntimeError(
            f"anchor not unique in {path.name} (count={raw.count(anchor)})"
        )
    src_pos = raw.find(anchor)
    close_tag = f"</message>{eol}"
    close_pos = raw.find(close_tag, src_pos)
    if close_pos == -1:
        raise RuntimeError(f"anchor </message> not found in {path.name}")
    insert_pos = close_pos + len(close_tag)

    if trans is None:
        translation = '<translation type="unfinished"></translation>'
    else:
        translation = (
            f"<translation>{xml_escape(trans).replace(chr(10), eol)}</translation>"
        )
    block = (
        f"    <message>{eol}"
        f'        <location filename="../../src/ui/menu/NlePlugins.qml" line="{LINE}"/>{eol}'
        f"        <source>{escaped_source}</source>{eol}"
        f"        {translation}{eol}"
        f"    </message>{eol}"
    )

    raw = raw[:insert_pos] + block + raw[insert_pos:]
    path.write_bytes(raw.encode("utf-8"))
    return True


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    # None -> source template gets an unfinished entry (English fallback at runtime).
    targets: list[tuple[str, str | None]] = [("gyroflow", None)]
    targets += list(TRANS.items())
    for lang, trans in targets:
        path = root / f"{lang}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        try:
            patched = patch_file(path, trans)
        except RuntimeError as e:
            print(f"{lang}: ERROR {e}")
            ok = False
            continue
        raw = path.read_bytes().decode("utf-8")
        eol = "\r\n" if "\r\n" in raw else "\n"
        if f"<source>{xml_escape(SOURCE).replace(chr(10), eol)}</source>" not in raw:
            print(f"{lang}: MISSING after patch")
            ok = False
        else:
            print(f"{lang}: {'patched' if patched else 'no-op'}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
