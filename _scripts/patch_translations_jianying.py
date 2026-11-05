# SPDX-License-Identifier: GPL-3.0-or-later
"""Add the Jianying installer messages without rewriting existing catalogs."""
from pathlib import Path
import re
from xml.sax.saxutils import escape

SOURCES = (
    "Jianying plugin installed.\nReopen Jianying before using the effect.",
    "Save your work and close Jianying before installing the plugin.",
    "Jianying is not installed",
    "Not available in this release",
)
TRANSLATIONS = {
    "cs": ("Plugin pro 剪映 byl nainstalován.\nPřed použitím efektu znovu otevřete 剪映.", "Před instalací pluginu uložte práci a zavřete 剪映.", "剪映 není nainstalován", "V tomto vydání není k dispozici"),
    "da": ("Plugin til 剪映 er installeret.\nÅbn 剪映 igen, før du bruger effekten.", "Gem dit arbejde, og luk 剪映, før du installerer pluginet.", "剪映 er ikke installeret", "Ikke tilgængelig i denne udgave"),
    "de": ("Plugin für 剪映 installiert.\nÖffne 剪映 erneut, bevor du den Effekt verwendest.", "Speichere deine Arbeit und schließe 剪映 vor der Plugin-Installation.", "剪映 ist nicht installiert", "In dieser Version nicht verfügbar"),
    "el": ("Το πρόσθετο για 剪映 εγκαταστάθηκε.\nΑνοίξτε ξανά το 剪映 πριν χρησιμοποιήσετε το εφέ.", "Αποθηκεύστε την εργασία σας και κλείστε το 剪映 πριν εγκαταστήσετε το πρόσθετο.", "Το 剪映 δεν είναι εγκατεστημένο", "Δεν διατίθεται σε αυτή την έκδοση"),
    "es": ("Plugin de 剪映 instalado.\nVuelve a abrir 剪映 antes de usar el efecto.", "Guarda tu trabajo y cierra 剪映 antes de instalar el plugin.", "剪映 no está instalado", "No disponible en esta versión"),
    "fi": ("剪映-lisäosa on asennettu.\nAvaa 剪映 uudelleen ennen tehosteen käyttöä.", "Tallenna työsi ja sulje 剪映 ennen lisäosan asentamista.", "剪映 ei ole asennettu", "Ei saatavilla tässä julkaisussa"),
    "fr": ("Plugin 剪映 installé.\nRouvrez 剪映 avant d’utiliser l’effet.", "Enregistrez votre travail et fermez 剪映 avant d’installer le plugin.", "剪映 n’est pas installé", "Indisponible dans cette version"),
    "gl": ("Plugin de 剪映 instalado.\nAbre de novo 剪映 antes de usar o efecto.", "Garda o traballo e pecha 剪映 antes de instalar o plugin.", "剪映 non está instalado", "Non dispoñible nesta versión"),
    "id": ("Plugin 剪映 terpasang.\nBuka kembali 剪映 sebelum menggunakan efek.", "Simpan pekerjaan dan tutup 剪映 sebelum memasang plugin.", "剪映 belum terpasang", "Tidak tersedia dalam rilis ini"),
    "it": ("Plugin di 剪映 installato.\nRiapri 剪映 prima di usare l’effetto.", "Salva il lavoro e chiudi 剪映 prima di installare il plugin.", "剪映 non è installato", "Non disponibile in questa versione"),
    "ja": ("剪映プラグインをインストールしました。\nエフェクトを使用する前に剪映を再起動してください。", "プラグインをインストールする前に、作業を保存して剪映を終了してください。", "剪映がインストールされていません", "このリリースでは利用できません"),
    "ko": ("剪映 플러그인이 설치되었습니다.\n효과를 사용하기 전에 剪映을 다시 여세요.", "플러그인을 설치하기 전에 작업을 저장하고 剪映을 종료하세요.", "剪映이 설치되지 않았습니다", "이 릴리스에서는 사용할 수 없습니다"),
    "no": ("Plugin for 剪映 er installert.\nÅpne 剪映 på nytt før du bruker effekten.", "Lagre arbeidet og lukk 剪映 før du installerer pluginen.", "剪映 er ikke installert", "Ikke tilgjengelig i denne utgaven"),
    "pl": ("Wtyczka 剪映 została zainstalowana.\nOtwórz ponownie 剪映 przed użyciem efektu.", "Zapisz pracę i zamknij 剪映 przed instalacją wtyczki.", "剪映 nie jest zainstalowany", "Niedostępne w tej wersji"),
    "pt": ("Plugin do 剪映 instalado.\nVolta a abrir o 剪映 antes de usar o efeito.", "Guarda o trabalho e fecha o 剪映 antes de instalar o plugin.", "O 剪映 não está instalado", "Indisponível nesta versão"),
    "pt_BR": ("Plugin do 剪映 instalado.\nAbra o 剪映 novamente antes de usar o efeito.", "Salve seu trabalho e feche o 剪映 antes de instalar o plugin.", "O 剪映 não está instalado", "Indisponível nesta versão"),
    "ru": ("Плагин 剪映 установлен.\nПеред использованием эффекта откройте 剪映 заново.", "Сохраните работу и закройте 剪映 перед установкой плагина.", "剪映 не установлен", "Недоступно в этом выпуске"),
    "sk": ("Plugin pre 剪映 bol nainštalovaný.\nPred použitím efektu znova otvorte 剪映.", "Pred inštaláciou pluginu uložte prácu a zatvorte 剪映.", "剪映 nie je nainštalovaný", "V tomto vydaní nie je k dispozícii"),
    "tr": ("剪映 eklentisi yüklendi.\nEfekti kullanmadan önce 剪映 uygulamasını yeniden açın.", "Eklentiyi yüklemeden önce çalışmanızı kaydedin ve 剪映 uygulamasını kapatın.", "剪映 yüklü değil", "Bu sürümde kullanılamıyor"),
    "uk": ("Плагін 剪映 встановлено.\nПеред використанням ефекту відкрийте 剪映 знову.", "Збережіть роботу та закрийте 剪映 перед встановленням плагіна.", "剪映 не встановлено", "Недоступно в цьому випуску"),
    "zh_CN": ("剪映插件已安装。\n重新打开剪映后即可使用。", "安装插件前，请先保存工作并完全退出剪映。", "尚未安装剪映", "此发布版本尚未提供剪映插件"),
    "zh_TW": ("剪映外掛已安裝。\n重新開啟剪映後即可使用。", "安裝外掛前，請先儲存工作並完全結束剪映。", "尚未安裝剪映", "此發行版本尚未提供剪映外掛"),
}


def main():
    root = Path(__file__).resolve().parents[1] / "resources/translations"
    for path in sorted(root.glob("*.ts")):
        content = path.read_bytes().decode("utf-8")
        newline = "\r\n" if "\r\n" in content else "\n"
        context = re.search(r"<context>\s*<name>NlePlugins</name>.*?</context>", content, re.S)
        if context is None:
            raise ValueError(f"Missing NlePlugins context: {path}")
        messages = []
        for index, source in enumerate(SOURCES):
            if f"<source>{escape(source)}</source>" in context.group():
                continue
            translated = '<translation type="unfinished"></translation>' if path.stem == "gyroflow" else f"<translation>{escape(TRANSLATIONS[path.stem][index])}</translation>"
            messages.append(f"    <message>\n        <source>{escape(source)}</source>\n        {translated}\n    </message>\n")
        position = context.end() - len("</context>")
        content = content[:position] + "".join(messages).replace("\n", newline) + content[position:]
        path.write_bytes(content.encode("utf-8"))


if __name__ == "__main__":
    main()
