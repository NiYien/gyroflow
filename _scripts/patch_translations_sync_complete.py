"""One-shot patch: inject 4 new UI strings (Sync complete + Canon CRM messages)
into all 23 .ts files since lupdate.exe is not available in the local Qt bundle.

Run from repo root:
    python _scripts/patch_translations_sync_complete.py
"""
from __future__ import annotations

import pathlib
import sys

# Translation matrix. \n in a value becomes a real newline in <translation>.
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "A": "Synchronizace dokončena",
        "B": "Synchronizace dokončena: %1",
        "C": "Soubory Canon CRM jsou podporovány pouze prostřednictvím proxy workflow.\nExportujte projektový soubor a použijte ho ve svém RAW workflow.",
        "D": "Soubory Canon CRM je nutné načíst společně s proxy videem se stejným názvem.",
    },
    "da": {
        "A": "Synkronisering fuldført",
        "B": "Synkronisering fuldført: %1",
        "C": "Canon CRM-filer understøttes kun via proxy-arbejdsflowet.\nEksportér en projektfil, og brug den i dit RAW-arbejdsflow.",
        "D": "Canon CRM-filer skal indlæses sammen med en proxy-video med samme navn.",
    },
    "de": {
        "A": "Synchronisierung abgeschlossen",
        "B": "Synchronisierung abgeschlossen: %1",
        "C": "Canon CRM-Dateien werden nur über den Proxy-Workflow unterstützt.\nExportiere eine Projektdatei und nutze sie in deinem RAW-Workflow.",
        "D": "Canon CRM-Dateien müssen zusammen mit einem Proxy-Video gleichen Namens geladen werden.",
    },
    "el": {
        "A": "Ο συγχρονισμός ολοκληρώθηκε",
        "B": "Ο συγχρονισμός ολοκληρώθηκε: %1",
        "C": "Τα αρχεία Canon CRM υποστηρίζονται μόνο μέσω της ροής εργασίας με proxy.\nΕξάγετε ένα αρχείο έργου και χρησιμοποιήστε το στη ροή RAW.",
        "D": "Τα αρχεία Canon CRM πρέπει να φορτωθούν μαζί με ένα ομώνυμο proxy βίντεο.",
    },
    "es": {
        "A": "Sincronización completada",
        "B": "Sincronización completada: %1",
        "C": "Los archivos Canon CRM solo se admiten mediante el flujo de trabajo con proxy.\nExporta un archivo de proyecto y úsalo con tu flujo de trabajo RAW.",
        "D": "Los archivos Canon CRM deben cargarse junto con un vídeo proxy del mismo nombre.",
    },
    "fi": {
        "A": "Synkronointi valmis",
        "B": "Synkronointi valmis: %1",
        "C": "Canon CRM -tiedostoja tuetaan vain proxy-työnkulun kautta.\nVie projektitiedosto ja käytä sitä RAW-työnkulussasi.",
        "D": "Canon CRM -tiedostot on ladattava yhdessä samannimisen proxy-videon kanssa.",
    },
    "fr": {
        "A": "Synchronisation terminée",
        "B": "Synchronisation terminée : %1",
        "C": "Les fichiers Canon CRM sont uniquement pris en charge via le flux de travail proxy.\nExportez un fichier de projet et utilisez-le dans votre flux RAW.",
        "D": "Les fichiers Canon CRM doivent être chargés avec une vidéo proxy portant le même nom.",
    },
    "gl": {
        "A": "Sincronización completada",
        "B": "Sincronización completada: %1",
        "C": "Os ficheiros Canon CRM só se admiten a través do fluxo de traballo con proxy.\nExporta un ficheiro de proxecto e úsao co teu fluxo RAW.",
        "D": "Os ficheiros Canon CRM deben cargarse xunto cun vídeo proxy co mesmo nome.",
    },
    "id": {
        "A": "Sinkronisasi selesai",
        "B": "Sinkronisasi selesai: %1",
        "C": "File Canon CRM hanya didukung melalui alur kerja proxy.\nEkspor file proyek dan gunakan dengan alur kerja RAW Anda.",
        "D": "File Canon CRM harus dimuat bersama video proxy dengan nama yang sama.",
    },
    "it": {
        "A": "Sincronizzazione completata",
        "B": "Sincronizzazione completata: %1",
        "C": "I file Canon CRM sono supportati solo tramite il flusso di lavoro con proxy.\nEsporta un file di progetto e usalo nel tuo flusso di lavoro RAW.",
        "D": "I file Canon CRM devono essere caricati insieme a un video proxy con lo stesso nome.",
    },
    "ja": {
        "A": "同期完了",
        "B": "同期完了: %1",
        "C": "Canon CRM ファイルはプロキシワークフローでのみ対応しています。\nプロジェクトファイルをエクスポートし、RAW ワークフローで使用してください。",
        "D": "Canon CRM ファイルは同名のプロキシ動画と一緒に読み込む必要があります。",
    },
    "ko": {
        "A": "동기화 완료",
        "B": "동기화 완료: %1",
        "C": "Canon CRM 파일은 프록시 워크플로에서만 지원됩니다.\n프로젝트 파일을 내보내 RAW 워크플로에서 사용하세요.",
        "D": "Canon CRM 파일은 같은 이름의 프록시 영상과 함께 불러와야 합니다.",
    },
    "no": {
        "A": "Synkronisering fullført",
        "B": "Synkronisering fullført: %1",
        "C": "Canon CRM-filer støttes kun gjennom proxy-arbeidsflyten.\nEksporter en prosjektfil og bruk den i RAW-arbeidsflyten din.",
        "D": "Canon CRM-filer må lastes sammen med en proxy-video med samme navn.",
    },
    "pl": {
        "A": "Synchronizacja zakończona",
        "B": "Synchronizacja zakończona: %1",
        "C": "Pliki Canon CRM są obsługiwane tylko poprzez proces proxy.\nWyeksportuj plik projektu i użyj go w swoim procesie RAW.",
        "D": "Pliki Canon CRM muszą być wczytane razem z wideo proxy o tej samej nazwie.",
    },
    "pt": {
        "A": "Sincronização concluída",
        "B": "Sincronização concluída: %1",
        "C": "Os ficheiros Canon CRM apenas são suportados através do fluxo de trabalho com proxy.\nExporte um ficheiro de projeto e utilize-o no seu fluxo de trabalho RAW.",
        "D": "Os ficheiros Canon CRM têm de ser carregados em conjunto com um vídeo proxy com o mesmo nome.",
    },
    "pt_BR": {
        "A": "Sincronização concluída",
        "B": "Sincronização concluída: %1",
        "C": "Os arquivos Canon CRM só são suportados pelo fluxo de trabalho com proxy.\nExporte um arquivo de projeto e use-o em seu fluxo de trabalho RAW.",
        "D": "Os arquivos Canon CRM precisam ser carregados junto com um vídeo proxy de mesmo nome.",
    },
    "ru": {
        "A": "Синхронизация завершена",
        "B": "Синхронизация завершена: %1",
        "C": "Файлы Canon CRM поддерживаются только через прокси-процесс.\nЭкспортируйте файл проекта и используйте его в своём RAW-процессе.",
        "D": "Файлы Canon CRM необходимо загружать вместе с прокси-видео с тем же именем.",
    },
    "sk": {
        "A": "Synchronizácia dokončená",
        "B": "Synchronizácia dokončená: %1",
        "C": "Súbory Canon CRM sú podporované iba prostredníctvom proxy pracovného postupu.\nVyexportujte projektový súbor a použite ho vo svojom RAW pracovnom postupe.",
        "D": "Súbory Canon CRM je potrebné načítať spolu s proxy videom s rovnakým názvom.",
    },
    "tr": {
        "A": "Senkronizasyon tamamlandı",
        "B": "Senkronizasyon tamamlandı: %1",
        "C": "Canon CRM dosyaları yalnızca proxy iş akışıyla desteklenir.\nBir proje dosyası dışa aktarın ve RAW iş akışınızda kullanın.",
        "D": "Canon CRM dosyaları, aynı adlı bir proxy videoyla birlikte yüklenmelidir.",
    },
    "uk": {
        "A": "Синхронізацію завершено",
        "B": "Синхронізацію завершено: %1",
        "C": "Файли Canon CRM підтримуються лише через робочий процес із проксі.\nЕкспортуйте файл проєкту та використовуйте його у своєму RAW-процесі.",
        "D": "Файли Canon CRM потрібно завантажувати разом із проксі-відео з тією самою назвою.",
    },
    "zh_CN": {
        "A": "同步完成",
        "B": "同步完成：%1",
        "C": "Canon CRM 文件仅支持通过代理工作流使用。\n请导出项目文件，并在 RAW 工作流中使用。",
        "D": "必须与同名代理视频一起加载 Canon CRM 文件。",
    },
    "zh_TW": {
        "A": "同步完成",
        "B": "同步完成：%1",
        "C": "Canon CRM 檔案僅支援透過代理工作流程使用。\n請匯出專案檔，並在 RAW 工作流程中使用。",
        "D": "必須與同名代理影片一起載入 Canon CRM 檔案。",
    },
}

SOURCES = {
    "A": "Sync complete",
    "B": "Sync complete: %1",
    "C": "Canon CRM files are supported through the proxy workflow only.\nExport a project file and use it with your RAW workflow.",
    "D": "Canon CRM files must be loaded together with a same-name proxy video.",
}

# QML locations per source string (matches `lupdate` behavior of merging multiple
# `<location>` entries when the same source string appears multiple times in one
# QML file/context).
LOCS = {
    # Sync complete used at two RenderQueue.qml lines
    "A": [("RenderQueue.qml", 1719), ("RenderQueue.qml", 1774)],
    "B": [("RenderQueue.qml", 1635)],
    "C": [("App.qml", 2136)],
    # Canon CRM "must be loaded" appears once in RenderQueue and four times in
    # VideoArea; we'll attach the right subset per context below.
    "D_RQ": [("RenderQueue.qml", 1985)],
    "D_VA": [
        ("VideoArea.qml", 400),
        ("VideoArea.qml", 547),
        ("VideoArea.qml", 585),
        ("VideoArea.qml", 589),
    ],
}


def make_message(locs: list[tuple[str, int]], source: str, translation: str | None) -> str:
    loc_lines = "\n".join(
        f'        <location filename="../../src/ui/{q}" line="{line}"/>'
        for q, line in locs
    )
    if translation is None:
        trans_tag = '<translation type="unfinished"></translation>'
    else:
        trans_tag = f'<translation>{translation}</translation>'
    return (
        "    <message>\n"
        f"{loc_lines}\n"
        f"        <source>{source}</source>\n"
        f"        {trans_tag}\n"
        "    </message>\n"
    )


def build_injections(t: dict[str, str] | None) -> tuple[str, str, str]:
    """Return (app_block, render_queue_block, video_area_block)."""
    def tx(key: str) -> str | None:
        return None if t is None else t[key]

    app_block = make_message(LOCS["C"], SOURCES["C"], tx("C"))
    rq_block = (
        make_message(LOCS["A"], SOURCES["A"], tx("A"))
        + make_message(LOCS["B"], SOURCES["B"], tx("B"))
        + make_message(LOCS["D_RQ"], SOURCES["D"], tx("D"))
    )
    va_block = make_message(LOCS["D_VA"], SOURCES["D"], tx("D"))
    return app_block, rq_block, va_block


def patch_file(path: pathlib.Path, t: dict[str, str] | None) -> str:
    content = path.read_text(encoding="utf-8")

    # Idempotency: skip if any of the new sources is already present.
    if "Canon CRM files are supported through the proxy workflow only." in content:
        return f"SKIP {path.name} (already patched)"

    app_block, rq_block, va_block = build_injections(t)

    new_content = content
    replacements = [
        ("    <name>App</name>\n", app_block, "App"),
        ("    <name>RenderQueue</name>\n", rq_block, "RenderQueue"),
        ("    <name>VideoArea</name>\n", va_block, "VideoArea"),
    ]
    for needle, block, ctx_name in replacements:
        if needle not in new_content:
            return f"FAIL {path.name}: context <{ctx_name}> not found"
        new_content = new_content.replace(needle, needle + block, 1)

    path.write_text(new_content, encoding="utf-8")
    return f"OK   {path.name}"


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    if not base.is_dir():
        print(f"Translation dir missing: {base}", file=sys.stderr)
        return 1

    # Source ts: unfinished
    src = base / "gyroflow.ts"
    print(patch_file(src, None))

    # 22 language ts files
    for lang in TRANS:
        path = base / f"{lang}.ts"
        if not path.is_file():
            print(f"MISS {path.name} (file not found)")
            continue
        print(patch_file(path, TRANS[lang]))

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
