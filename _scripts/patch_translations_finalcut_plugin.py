"""Add Final Cut integration status and recovery strings to all catalogs.

The patch is deliberately idempotent and inserts messages into the existing
NlePlugins context without letting lupdate reorder unrelated translations.
Product names are intentionally preserved in every language.

Run from the repository root, then regenerate the runtime catalogs with:
    for ts in resources/translations/*.ts; do
        ext/6.7.3/macos/bin/lrelease -silent "$ts"
    done
"""
from __future__ import annotations

import pathlib
import sys


QML_PATH = "../../src/ui/menu/NlePlugins.qml"
MESSAGES: tuple[tuple[int, str], ...] = (
    (70, "Installed"),
    (72, "Update available"),
    (74, "Template missing"),
    (76, "Broken or untrusted"),
    (78, "Not installed"),
    (89, "Repair"),
    (
        146,
        "Unable to replace the Final Cut integration while related apps may be using it.\n"
        "Close Final Cut Pro, Motion, and Gyroflow NiYien Final Cut, then click Repair or Install again.",
    ),
    (
        148,
        "The Final Cut App was installed, but its Motion template could not be verified.\n"
        "Close Final Cut Pro and Motion, then click Repair again.",
    ),
    (
        150,
        "The downloaded Final Cut integration could not be verified as trusted. No App was installed.\n"
        "Check your network connection and try again later.",
    ),
    (
        181,
        "Final Cut integration installed.\n"
        "Close and reopen Final Cut Pro before using the effect.",
    ),
)


TRANS: dict[str, tuple[str, ...]] = {
    "cs": (
        "Nainstalováno", "Je k dispozici aktualizace", "Chybí šablona",
        "Poškozené nebo nedůvěryhodné", "Nenainstalováno", "Opravit",
        "Integraci Final Cut nelze nahradit, protože ji mohou používat související aplikace.\nZavřete Final Cut Pro, Motion a Gyroflow NiYien Final Cut a poté znovu klikněte na Opravit nebo Nainstalovat.",
        "Aplikace Final Cut byla nainstalována, ale její šablonu Motion se nepodařilo ověřit.\nZavřete Final Cut Pro a Motion a poté znovu klikněte na Opravit.",
        "Staženou integraci Final Cut se nepodařilo ověřit jako důvěryhodnou. Nebyla nainstalována žádná aplikace.\nZkontrolujte síťové připojení a zkuste to později znovu.",
        "Integrace Final Cut byla nainstalována.\nPřed použitím efektu zavřete a znovu otevřete Final Cut Pro.",
    ),
    "da": (
        "Installeret", "Opdatering tilgængelig", "Skabelon mangler",
        "Beskadiget eller ikke godkendt", "Ikke installeret", "Reparer",
        "Final Cut-integrationen kan ikke erstattes, mens relaterede apps muligvis bruger den.\nLuk Final Cut Pro, Motion og Gyroflow NiYien Final Cut, og klik derefter på Reparer eller Installer igen.",
        "Final Cut-appen blev installeret, men dens Motion-skabelon kunne ikke bekræftes.\nLuk Final Cut Pro og Motion, og klik derefter på Reparer igen.",
        "Den downloadede Final Cut-integration kunne ikke bekræftes som pålidelig. Ingen app blev installeret.\nKontrollér din netværksforbindelse, og prøv igen senere.",
        "Final Cut-integrationen er installeret.\nLuk og åbn Final Cut Pro igen, før du bruger effekten.",
    ),
    "de": (
        "Installiert", "Aktualisierung verfügbar", "Vorlage fehlt",
        "Beschädigt oder nicht vertrauenswürdig", "Nicht installiert", "Reparieren",
        "Die Final Cut-Integration kann nicht ersetzt werden, solange zugehörige Apps sie möglicherweise verwenden.\nSchließen Sie Final Cut Pro, Motion und Gyroflow NiYien Final Cut und klicken Sie dann erneut auf Reparieren oder Installieren.",
        "Die Final Cut-App wurde installiert, aber ihre Motion-Vorlage konnte nicht überprüft werden.\nSchließen Sie Final Cut Pro und Motion und klicken Sie dann erneut auf Reparieren.",
        "Die heruntergeladene Final Cut-Integration konnte nicht als vertrauenswürdig bestätigt werden. Es wurde keine App installiert.\nÜberprüfen Sie Ihre Netzwerkverbindung und versuchen Sie es später erneut.",
        "Final Cut-Integration installiert.\nSchließen und öffnen Sie Final Cut Pro erneut, bevor Sie den Effekt verwenden.",
    ),
    "el": (
        "Εγκαταστάθηκε", "Υπάρχει διαθέσιμη ενημέρωση", "Λείπει το πρότυπο",
        "Κατεστραμμένο ή μη αξιόπιστο", "Δεν έχει εγκατασταθεί", "Επιδιόρθωση",
        "Δεν είναι δυνατή η αντικατάσταση της ενσωμάτωσης Final Cut ενώ μπορεί να χρησιμοποιείται από σχετικές εφαρμογές.\nΚλείστε τα Final Cut Pro, Motion και Gyroflow NiYien Final Cut και μετά πατήστε ξανά Επιδιόρθωση ή Εγκατάσταση.",
        "Η εφαρμογή Final Cut εγκαταστάθηκε, αλλά δεν ήταν δυνατή η επαλήθευση του προτύπου Motion.\nΚλείστε τα Final Cut Pro και Motion και μετά πατήστε ξανά Επιδιόρθωση.",
        "Δεν ήταν δυνατή η επαλήθευση της αξιοπιστίας της ληφθείσας ενσωμάτωσης Final Cut. Δεν εγκαταστάθηκε εφαρμογή.\nΕλέγξτε τη σύνδεση δικτύου και δοκιμάστε ξανά αργότερα.",
        "Η ενσωμάτωση Final Cut εγκαταστάθηκε.\nΚλείστε και ανοίξτε ξανά το Final Cut Pro πριν χρησιμοποιήσετε το εφέ.",
    ),
    "es": (
        "Instalado", "Actualización disponible", "Falta la plantilla",
        "Dañado o no fiable", "No instalado", "Reparar",
        "No se puede sustituir la integración de Final Cut mientras otras aplicaciones relacionadas puedan estar usándola.\nCierra Final Cut Pro, Motion y Gyroflow NiYien Final Cut y vuelve a hacer clic en Reparar o Instalar.",
        "La aplicación Final Cut se instaló, pero no se pudo verificar su plantilla de Motion.\nCierra Final Cut Pro y Motion y vuelve a hacer clic en Reparar.",
        "No se pudo verificar que la integración de Final Cut descargada fuera de confianza. No se instaló ninguna aplicación.\nComprueba tu conexión de red e inténtalo de nuevo más tarde.",
        "Integración de Final Cut instalada.\nCierra y vuelve a abrir Final Cut Pro antes de usar el efecto.",
    ),
    "fi": (
        "Asennettu", "Päivitys saatavilla", "Malli puuttuu",
        "Vioittunut tai epäluotettava", "Ei asennettu", "Korjaa",
        "Final Cut -integraatiota ei voi korvata, kun siihen liittyvät sovellukset saattavat käyttää sitä.\nSulje Final Cut Pro, Motion ja Gyroflow NiYien Final Cut ja valitse sitten uudelleen Korjaa tai Asenna.",
        "Final Cut -appi asennettiin, mutta sen Motion-mallia ei voitu vahvistaa.\nSulje Final Cut Pro ja Motion ja valitse sitten uudelleen Korjaa.",
        "Ladattua Final Cut -integraatiota ei voitu vahvistaa luotetuksi. Appia ei asennettu.\nTarkista verkkoyhteys ja yritä myöhemmin uudelleen.",
        "Final Cut -integraatio on asennettu.\nSulje Final Cut Pro ja avaa se uudelleen ennen tehosteen käyttöä.",
    ),
    "fr": (
        "Installé", "Mise à jour disponible", "Modèle manquant",
        "Endommagé ou non fiable", "Non installé", "Réparer",
        "Impossible de remplacer l’intégration Final Cut tant que des apps associées sont susceptibles de l’utiliser.\nFermez Final Cut Pro, Motion et Gyroflow NiYien Final Cut, puis cliquez de nouveau sur Réparer ou Installer.",
        "L’app Final Cut a été installée, mais son modèle Motion n’a pas pu être vérifié.\nFermez Final Cut Pro et Motion, puis cliquez de nouveau sur Réparer.",
        "L’intégration Final Cut téléchargée n’a pas pu être considérée comme fiable. Aucune app n’a été installée.\nVérifiez votre connexion réseau et réessayez ultérieurement.",
        "Intégration Final Cut installée.\nFermez puis rouvrez Final Cut Pro avant d’utiliser l’effet.",
    ),
    "gl": (
        "Instalado", "Actualización dispoñible", "Falta o modelo",
        "Danado ou non fiable", "Non instalado", "Reparar",
        "Non se pode substituír a integración de Final Cut mentres outras aplicacións relacionadas poidan estar a usala.\nPecha Final Cut Pro, Motion e Gyroflow NiYien Final Cut e preme de novo en Reparar ou Instalar.",
        "Instalouse a aplicación Final Cut, pero non se puido verificar o seu modelo de Motion.\nPecha Final Cut Pro e Motion e preme de novo en Reparar.",
        "Non se puido verificar como fiable a integración de Final Cut descargada. Non se instalou ningunha aplicación.\nComproba a conexión de rede e téntao de novo máis tarde.",
        "Integración de Final Cut instalada.\nPecha e volve abrir Final Cut Pro antes de usar o efecto.",
    ),
    "id": (
        "Terpasang", "Pembaruan tersedia", "Templat tidak ada",
        "Rusak atau tidak tepercaya", "Belum terpasang", "Perbaiki",
        "Integrasi Final Cut tidak dapat diganti saat mungkin sedang digunakan oleh aplikasi terkait.\nTutup Final Cut Pro, Motion, dan Gyroflow NiYien Final Cut, lalu klik Perbaiki atau Pasang lagi.",
        "App Final Cut telah dipasang, tetapi templat Motion-nya tidak dapat diverifikasi.\nTutup Final Cut Pro dan Motion, lalu klik Perbaiki lagi.",
        "Integrasi Final Cut yang diunduh tidak dapat diverifikasi sebagai tepercaya. Tidak ada App yang dipasang.\nPeriksa koneksi jaringan Anda dan coba lagi nanti.",
        "Integrasi Final Cut telah dipasang.\nTutup dan buka kembali Final Cut Pro sebelum menggunakan efek.",
    ),
    "it": (
        "Installato", "Aggiornamento disponibile", "Modello mancante",
        "Danneggiato o non attendibile", "Non installato", "Ripara",
        "Impossibile sostituire l’integrazione Final Cut mentre potrebbe essere utilizzata dalle app correlate.\nChiudi Final Cut Pro, Motion e Gyroflow NiYien Final Cut, quindi fai nuovamente clic su Ripara o Installa.",
        "L’app Final Cut è stata installata, ma non è stato possibile verificarne il modello Motion.\nChiudi Final Cut Pro e Motion, quindi fai nuovamente clic su Ripara.",
        "Non è stato possibile verificare come attendibile l’integrazione Final Cut scaricata. Nessuna app è stata installata.\nControlla la connessione di rete e riprova più tardi.",
        "Integrazione Final Cut installata.\nChiudi e riapri Final Cut Pro prima di usare l’effetto.",
    ),
    "ja": (
        "インストール済み", "アップデートあり", "テンプレートがありません",
        "破損または信頼できません", "未インストール", "修復",
        "関連アプリが使用している可能性があるため、Final Cut 連携を置き換えられません。\nFinal Cut Pro、Motion、Gyroflow NiYien Final Cut を終了してから、もう一度［修復］または［インストール］をクリックしてください。",
        "Final Cut App はインストールされましたが、Motion テンプレートを検証できませんでした。\nFinal Cut Pro と Motion を終了してから、もう一度［修復］をクリックしてください。",
        "ダウンロードした Final Cut 連携を信頼できるものとして検証できませんでした。App はインストールされていません。\nネットワーク接続を確認し、後でもう一度お試しください。",
        "Final Cut 連携をインストールしました。\nエフェクトを使用する前に Final Cut Pro を終了して再度開いてください。",
    ),
    "ko": (
        "설치됨", "업데이트 있음", "템플릿 없음",
        "손상되었거나 신뢰할 수 없음", "설치되지 않음", "복구",
        "관련 앱에서 사용 중일 수 있어 Final Cut 통합을 교체할 수 없습니다.\nFinal Cut Pro, Motion 및 Gyroflow NiYien Final Cut을 종료한 다음 복구 또는 설치를 다시 클릭하세요.",
        "Final Cut App은 설치되었지만 Motion 템플릿을 확인할 수 없습니다.\nFinal Cut Pro와 Motion을 종료한 다음 복구를 다시 클릭하세요.",
        "다운로드한 Final Cut 통합을 신뢰할 수 있는 것으로 확인하지 못했습니다. App이 설치되지 않았습니다.\n네트워크 연결을 확인한 후 나중에 다시 시도하세요.",
        "Final Cut 통합이 설치되었습니다.\n효과를 사용하기 전에 Final Cut Pro를 종료했다가 다시 여세요.",
    ),
    "no": (
        "Installert", "Oppdatering tilgjengelig", "Mal mangler",
        "Skadet eller ikke klarert", "Ikke installert", "Reparer",
        "Final Cut-integrasjonen kan ikke erstattes mens relaterte apper kanskje bruker den.\nLukk Final Cut Pro, Motion og Gyroflow NiYien Final Cut, og klikk deretter på Reparer eller Installer igjen.",
        "Final Cut-appen ble installert, men Motion-malen kunne ikke verifiseres.\nLukk Final Cut Pro og Motion, og klikk deretter på Reparer igjen.",
        "Den nedlastede Final Cut-integrasjonen kunne ikke verifiseres som klarert. Ingen app ble installert.\nKontroller nettverkstilkoblingen og prøv igjen senere.",
        "Final Cut-integrasjonen er installert.\nLukk og åpne Final Cut Pro på nytt før du bruker effekten.",
    ),
    "pl": (
        "Zainstalowano", "Dostępna aktualizacja", "Brak szablonu",
        "Uszkodzone lub niezaufane", "Nie zainstalowano", "Napraw",
        "Nie można zastąpić integracji Final Cut, ponieważ powiązane aplikacje mogą jej używać.\nZamknij Final Cut Pro, Motion i Gyroflow NiYien Final Cut, a następnie ponownie kliknij Napraw lub Zainstaluj.",
        "Aplikacja Final Cut została zainstalowana, ale nie można było zweryfikować jej szablonu Motion.\nZamknij Final Cut Pro i Motion, a następnie ponownie kliknij Napraw.",
        "Nie można było potwierdzić, że pobrana integracja Final Cut jest zaufana. Nie zainstalowano aplikacji.\nSprawdź połączenie sieciowe i spróbuj ponownie później.",
        "Integracja Final Cut została zainstalowana.\nZamknij i otwórz ponownie Final Cut Pro przed użyciem efektu.",
    ),
    "pt": (
        "Instalado", "Atualização disponível", "Modelo em falta",
        "Danificado ou não fidedigno", "Não instalado", "Reparar",
        "Não é possível substituir a integração do Final Cut enquanto aplicações relacionadas a possam estar a utilizar.\nFeche o Final Cut Pro, o Motion e o Gyroflow NiYien Final Cut e clique novamente em Reparar ou Instalar.",
        "A aplicação Final Cut foi instalada, mas não foi possível verificar o respetivo modelo do Motion.\nFeche o Final Cut Pro e o Motion e clique novamente em Reparar.",
        "Não foi possível verificar como fidedigna a integração do Final Cut transferida. Não foi instalada nenhuma aplicação.\nVerifique a ligação de rede e tente novamente mais tarde.",
        "Integração do Final Cut instalada.\nFeche e volte a abrir o Final Cut Pro antes de utilizar o efeito.",
    ),
    "pt_BR": (
        "Instalado", "Atualização disponível", "Modelo ausente",
        "Danificado ou não confiável", "Não instalado", "Reparar",
        "Não é possível substituir a integração do Final Cut enquanto aplicativos relacionados podem estar usando-a.\nFeche o Final Cut Pro, o Motion e o Gyroflow NiYien Final Cut e clique novamente em Reparar ou Instalar.",
        "O aplicativo Final Cut foi instalado, mas não foi possível verificar o modelo do Motion.\nFeche o Final Cut Pro e o Motion e clique novamente em Reparar.",
        "Não foi possível verificar se a integração do Final Cut baixada é confiável. Nenhum aplicativo foi instalado.\nVerifique sua conexão de rede e tente novamente mais tarde.",
        "Integração do Final Cut instalada.\nFeche e abra novamente o Final Cut Pro antes de usar o efeito.",
    ),
    "ru": (
        "Установлено", "Доступно обновление", "Шаблон отсутствует",
        "Повреждено или не является доверенным", "Не установлено", "Исправить",
        "Невозможно заменить интеграцию Final Cut, пока её могут использовать связанные приложения.\nЗакройте Final Cut Pro, Motion и Gyroflow NiYien Final Cut, затем снова нажмите «Исправить» или «Установить».",
        "Приложение Final Cut установлено, но не удалось проверить его шаблон Motion.\nЗакройте Final Cut Pro и Motion, затем снова нажмите «Исправить».",
        "Не удалось подтвердить доверенность загруженной интеграции Final Cut. Приложение не установлено.\nПроверьте подключение к сети и повторите попытку позже.",
        "Интеграция Final Cut установлена.\nЗакройте и снова откройте Final Cut Pro перед использованием эффекта.",
    ),
    "sk": (
        "Nainštalované", "K dispozícii je aktualizácia", "Chýba šablóna",
        "Poškodené alebo nedôveryhodné", "Nenainštalované", "Opraviť",
        "Integráciu Final Cut nie je možné nahradiť, kým ju môžu používať súvisiace aplikácie.\nZatvorte Final Cut Pro, Motion a Gyroflow NiYien Final Cut a potom znova kliknite na Opraviť alebo Nainštalovať.",
        "Aplikácia Final Cut bola nainštalovaná, ale jej šablónu Motion sa nepodarilo overiť.\nZatvorte Final Cut Pro a Motion a potom znova kliknite na Opraviť.",
        "Stiahnutú integráciu Final Cut sa nepodarilo overiť ako dôveryhodnú. Nebola nainštalovaná žiadna aplikácia.\nSkontrolujte sieťové pripojenie a skúste to neskôr znova.",
        "Integrácia Final Cut bola nainštalovaná.\nPred použitím efektu zatvorte a znova otvorte Final Cut Pro.",
    ),
    "tr": (
        "Yüklendi", "Güncelleme mevcut", "Şablon eksik",
        "Bozuk veya güvenilir değil", "Yüklü değil", "Onar",
        "İlgili uygulamalar kullanıyor olabileceğinden Final Cut entegrasyonu değiştirilemiyor.\nFinal Cut Pro, Motion ve Gyroflow NiYien Final Cut uygulamalarını kapatın, ardından Onar veya Yükle'ye tekrar tıklayın.",
        "Final Cut App yüklendi ancak Motion şablonu doğrulanamadı.\nFinal Cut Pro ve Motion uygulamalarını kapatın, ardından Onar'a tekrar tıklayın.",
        "İndirilen Final Cut entegrasyonunun güvenilir olduğu doğrulanamadı. Hiçbir App yüklenmedi.\nAğ bağlantınızı kontrol edip daha sonra tekrar deneyin.",
        "Final Cut entegrasyonu yüklendi.\nEfekti kullanmadan önce Final Cut Pro'yu kapatıp yeniden açın.",
    ),
    "uk": (
        "Установлено", "Доступне оновлення", "Шаблон відсутній",
        "Пошкоджено або не є довіреним", "Не встановлено", "Виправити",
        "Неможливо замінити інтеграцію Final Cut, доки її можуть використовувати пов’язані програми.\nЗакрийте Final Cut Pro, Motion і Gyroflow NiYien Final Cut, а потім знову натисніть «Виправити» або «Установити».",
        "Програму Final Cut установлено, але не вдалося перевірити її шаблон Motion.\nЗакрийте Final Cut Pro і Motion, а потім знову натисніть «Виправити».",
        "Не вдалося підтвердити надійність завантаженої інтеграції Final Cut. Програму не встановлено.\nПеревірте підключення до мережі та повторіть спробу пізніше.",
        "Інтеграцію Final Cut установлено.\nЗакрийте й знову відкрийте Final Cut Pro перед використанням ефекту.",
    ),
    "zh_CN": (
        "已安装", "有可用更新", "模板缺失",
        "损坏或不受信任", "未安装", "修复",
        "相关 App 可能正在使用 Final Cut 集成，因此无法替换它。\n请关闭 Final Cut Pro、Motion 和 Gyroflow NiYien Final Cut，然后再次点击“修复”或“安装”。",
        "Final Cut App 已安装，但无法验证其 Motion 模板。\n请关闭 Final Cut Pro 和 Motion，然后再次点击“修复”。",
        "无法验证下载的 Final Cut 集成是否可信。未安装任何 App。\n请检查网络连接，稍后再试。",
        "Final Cut 集成已安装。\n请先关闭并重新打开 Final Cut Pro，再使用该效果。",
    ),
    "zh_TW": (
        "已安裝", "有可用更新", "缺少樣板",
        "損毀或不受信任", "未安裝", "修復",
        "相關 App 可能正在使用 Final Cut 整合，因此無法取代它。\n請關閉 Final Cut Pro、Motion 和 Gyroflow NiYien Final Cut，然後再次按一下「修復」或「安裝」。",
        "Final Cut App 已安裝，但無法驗證其 Motion 樣板。\n請關閉 Final Cut Pro 和 Motion，然後再次按一下「修復」。",
        "無法驗證下載的 Final Cut 整合是否可信。未安裝任何 App。\n請檢查網路連線，稍後再試。",
        "Final Cut 整合已安裝。\n請先關閉並重新開啟 Final Cut Pro，再使用此效果。",
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


def nle_context_bounds(raw: str) -> tuple[int, int]:
    name = "    <name>NlePlugins</name>"
    if raw.count(name) != 1:
        raise RuntimeError(f"NlePlugins context count is {raw.count(name)}")
    start = raw.index(name)
    end = raw.index("</context>", start)
    return start, end


def patch_file(path: pathlib.Path, translations: tuple[str, ...] | None) -> int:
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"
    context_start, context_end = nle_context_bounds(raw)
    context = raw[context_start:context_end]
    inserted = 0

    if translations is not None and len(translations) != len(MESSAGES):
        raise RuntimeError(f"expected {len(MESSAGES)} translations, got {len(translations)}")

    blocks: list[str] = []
    for index, (line, source) in enumerate(MESSAGES):
        escaped_source = xml_escape(source).replace("\n", eol)
        if f"<source>{escaped_source}</source>" in context:
            continue
        if translations is None:
            translation = '<translation type="unfinished"></translation>'
        else:
            translation = (
                "<translation>"
                + xml_escape(translations[index]).replace("\n", eol)
                + "</translation>"
            )
        blocks.append(
            f"    <message>{eol}"
            f'        <location filename="{QML_PATH}" line="{line}"/>{eol}'
            f"        <source>{escaped_source}</source>{eol}"
            f"        {translation}{eol}"
            f"    </message>{eol}"
        )
        inserted += 1

    if blocks:
        raw = raw[:context_end] + "".join(blocks) + raw[context_end:]
        path.write_bytes(raw.encode("utf-8"))
    return inserted


def main() -> int:
    root = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    expected = {path.stem for path in root.glob("*.ts")} - {"gyroflow"}
    if expected != set(TRANS):
        missing = sorted(expected - set(TRANS))
        extra = sorted(set(TRANS) - expected)
        print(f"catalog mismatch: missing={missing}, extra={extra}")
        return 1

    targets: list[tuple[str, tuple[str, ...] | None]] = [("gyroflow", None)]
    targets.extend(sorted(TRANS.items()))
    try:
        for language, translations in targets:
            path = root / f"{language}.ts"
            print(f"{language}: inserted {patch_file(path, translations)}")
    except (OSError, RuntimeError) as error:
        print(f"ERROR: {error}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
