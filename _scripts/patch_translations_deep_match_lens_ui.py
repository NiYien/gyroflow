"""One-shot patch: fill translations for the 4 deep-match lens-dialog strings
(change deep-match-builtin-gyro-bare-lens) in all 22 language .ts files.

The strings were introduced by editing src/ui/RenderQueue.qml and running
lupdate (which left them as <translation type="unfinished"/>):

  TITLE    - lens-group confirmation dialog question
  BODY     - dialog body explaining why the lens group matters
  NOGROUPS - guidance when no lens group focal lengths are configured
  NOTRUN   - probe_not_run failure message

Run from repo root:
    python _scripts/patch_translations_deep_match_lens_ui.py

Then regenerate the runtime .qm bundles:
    pwsh -c 'Get-ChildItem resources/translations/*.ts | ForEach-Object \
        { & ext/6.7.3/msvc2019_64/bin/lrelease.exe -silent $_.FullName }'

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import sys

SOURCES = {
    "TITLE": "Which lens group was this video shot with?",
    "BODY": (
        "This video has no lens information — the correct lens group makes "
        "deep match much more accurate."
    ),
    "NOGROUPS": (
        "No lens group focal lengths are configured. Set them in "
        "Sensor & Lens first, then run deep match."
    ),
    "NOTRUN": "Deep match could not run. Check the logs for details.",
}

# Per-language translations. Lens-group / deep-match / panel terminology
# reuses each language's existing translations of "Lens group",
# "Sensor && Lens" and "Deep match with gyro".
TRANS: dict[str, dict[str, str]] = {
    "cs": {
        "TITLE": "Kterou skupinou objektivů bylo toto video natočeno?",
        "BODY": (
            "Toto video nemá informace o objektivu — správná skupina "
            "objektivů výrazně zpřesní hluboké párování."
        ),
        "NOGROUPS": (
            "Nejsou nakonfigurovány žádné ohniskové vzdálenosti skupin "
            "objektivů. Nejprve je nastavte v „Senzor & Objektiv“ a poté "
            "spusťte hluboké párování."
        ),
        "NOTRUN": "Hluboké párování se nepodařilo spustit. Podrobnosti najdete v protokolech.",
    },
    "da": {
        "TITLE": "Hvilken objektivgruppe blev denne video optaget med?",
        "BODY": (
            "Denne video har ingen objektivoplysninger — den rigtige "
            "objektivgruppe gør dyb matchning meget mere præcis."
        ),
        "NOGROUPS": (
            "Der er ikke konfigureret brændvidder for objektivgrupper. "
            "Indstil dem først under \"Sensor & Objektiv\", og kør derefter "
            "dyb matchning."
        ),
        "NOTRUN": "Dyb matchning kunne ikke køre. Se logfilerne for detaljer.",
    },
    "de": {
        "TITLE": "Mit welcher Objektivgruppe wurde dieses Video aufgenommen?",
        "BODY": (
            "Dieses Video enthält keine Objektivinformationen — die richtige "
            "Objektivgruppe macht den Tiefenabgleich deutlich genauer."
        ),
        "NOGROUPS": (
            "Es sind keine Brennweiten für Objektivgruppen konfiguriert. "
            "Legen Sie sie zuerst unter „Sensor & Objektiv“ fest und starten "
            "Sie dann den Tiefenabgleich."
        ),
        "NOTRUN": "Tiefenabgleich konnte nicht ausgeführt werden. Prüfen Sie die Protokolle für Details.",
    },
    "el": {
        "TITLE": "Με ποια ομάδα φακών γυρίστηκε αυτό το βίντεο;",
        "BODY": (
            "Αυτό το βίντεο δεν έχει πληροφορίες φακού — η σωστή ομάδα φακών "
            "κάνει τη βαθιά αντιστοίχιση πολύ πιο ακριβή."
        ),
        "NOGROUPS": (
            "Δεν έχουν ρυθμιστεί εστιακές αποστάσεις ομάδων φακών. Ορίστε "
            "τις πρώτα στο «Αισθητήρας & Φακός» και έπειτα εκτελέστε τη "
            "βαθιά αντιστοίχιση."
        ),
        "NOTRUN": "Η βαθιά αντιστοίχιση δεν μπόρεσε να εκτελεστεί. Ελέγξτε τα αρχεία καταγραφής για λεπτομέρειες.",
    },
    "es": {
        "TITLE": "¿Con qué grupo de objetivos se grabó este video?",
        "BODY": (
            "Este video no tiene información del objetivo — el grupo de "
            "objetivos correcto hace que el emparejamiento profundo sea "
            "mucho más preciso."
        ),
        "NOGROUPS": (
            "No hay distancias focales de grupos de objetivos configuradas. "
            "Configúrelas primero en «Sensor & Objetivo» y luego ejecute el "
            "emparejamiento profundo."
        ),
        "NOTRUN": "El emparejamiento profundo no se pudo ejecutar. Consulte los registros para más detalles.",
    },
    "fi": {
        "TITLE": "Millä objektiiviryhmällä tämä video kuvattiin?",
        "BODY": (
            "Tällä videolla ei ole objektiivitietoja — oikea objektiiviryhmä "
            "tekee syväsovituksesta paljon tarkemman."
        ),
        "NOGROUPS": (
            "Objektiiviryhmien polttovälejä ei ole määritetty. Aseta ne "
            "ensin kohdassa \"Kenno & Objektiivi\" ja suorita sitten "
            "syväsovitus."
        ),
        "NOTRUN": "Syväsovitusta ei voitu suorittaa. Katso lisätiedot lokeista.",
    },
    "fr": {
        "TITLE": "Avec quel groupe d'objectifs cette vidéo a-t-elle été filmée ?",
        "BODY": (
            "Cette vidéo ne contient aucune information d'objectif — le bon "
            "groupe d'objectifs rend l'appariement profond beaucoup plus "
            "précis."
        ),
        "NOGROUPS": (
            "Aucune focale de groupe d'objectifs n'est configurée. "
            "Définissez-les d'abord dans « Capteur & Objectif », puis lancez "
            "l'appariement profond."
        ),
        "NOTRUN": "L'appariement profond n'a pas pu s'exécuter. Consultez les journaux pour plus de détails.",
    },
    "gl": {
        "TITLE": "Con que grupo de obxectivos se gravou este vídeo?",
        "BODY": (
            "Este vídeo non ten información do obxectivo — o grupo de "
            "obxectivos correcto fai que o emparellamento profundo sexa "
            "moito máis preciso."
        ),
        "NOGROUPS": (
            "Non hai distancias focais de grupos de obxectivos configuradas. "
            "Configúreas primeiro en «Sensor & Obxectivo» e logo execute o "
            "emparellamento profundo."
        ),
        "NOTRUN": "O emparellamento profundo non se puido executar. Consulte os rexistros para máis detalles.",
    },
    "id": {
        "TITLE": "Dengan grup lensa mana video ini direkam?",
        "BODY": (
            "Video ini tidak memiliki informasi lensa — grup lensa yang "
            "tepat membuat pencocokan mendalam jauh lebih akurat."
        ),
        "NOGROUPS": (
            "Belum ada panjang fokus grup lensa yang dikonfigurasi. Atur "
            "dulu di \"Sensor & Lensa\", lalu jalankan pencocokan mendalam."
        ),
        "NOTRUN": "Pencocokan mendalam tidak dapat berjalan. Periksa log untuk detailnya.",
    },
    "it": {
        "TITLE": "Con quale gruppo di obiettivi è stato girato questo video?",
        "BODY": (
            "Questo video non ha informazioni sull'obiettivo — il gruppo di "
            "obiettivi corretto rende l'abbinamento profondo molto più "
            "preciso."
        ),
        "NOGROUPS": (
            "Non sono configurate lunghezze focali dei gruppi di obiettivi. "
            "Impostale prima in «Sensore & Obiettivo», poi esegui "
            "l'abbinamento profondo."
        ),
        "NOTRUN": "L'abbinamento profondo non è stato eseguito. Controlla i log per i dettagli.",
    },
    "ja": {
        "TITLE": "この動画はどのレンズグループで撮影されましたか？",
        "BODY": (
            "この動画にはレンズ情報がありません — 正しいレンズグループを選ぶと"
            "ディープマッチングの精度が大幅に向上します。"
        ),
        "NOGROUPS": (
            "レンズグループの焦点距離が設定されていません。まず「センサーとレンズ」で"
            "設定してから、ディープマッチングを実行してください。"
        ),
        "NOTRUN": "ディープマッチングを実行できませんでした。詳細はログを確認してください。",
    },
    "ko": {
        "TITLE": "이 영상은 어떤 렌즈 그룹으로 촬영되었나요?",
        "BODY": (
            "이 영상에는 렌즈 정보가 없습니다 — 올바른 렌즈 그룹을 선택하면 "
            "딥 매칭 정확도가 크게 향상됩니다."
        ),
        "NOGROUPS": (
            "설정된 렌즈 그룹 초점 거리가 없습니다. 먼저 \"센서 및 렌즈\"에서 "
            "설정한 후 딥 매칭을 실행하세요."
        ),
        "NOTRUN": "딥 매칭을 실행할 수 없습니다. 자세한 내용은 로그를 확인하세요.",
    },
    "no": {
        "TITLE": "Hvilken objektivgruppe ble denne videoen filmet med?",
        "BODY": (
            "Denne videoen har ingen objektivinformasjon — riktig "
            "objektivgruppe gjør dyp matching mye mer nøyaktig."
        ),
        "NOGROUPS": (
            "Ingen brennvidder for objektivgrupper er konfigurert. Sett dem "
            "først under \"Sensor & Objektiv\", og kjør deretter dyp "
            "matching."
        ),
        "NOTRUN": "Dyp matching kunne ikke kjøre. Se loggene for detaljer.",
    },
    "pl": {
        "TITLE": "Którą grupą obiektywów nakręcono to wideo?",
        "BODY": (
            "To wideo nie ma informacji o obiektywie — właściwa grupa "
            "obiektywów znacznie zwiększa dokładność głębokiego dopasowania."
        ),
        "NOGROUPS": (
            "Nie skonfigurowano ogniskowych grup obiektywów. Najpierw ustaw "
            "je w „Matryca & Obiektyw“, a następnie uruchom głębokie "
            "dopasowanie."
        ),
        "NOTRUN": "Głębokie dopasowanie nie mogło zostać uruchomione. Sprawdź logi, aby poznać szczegóły.",
    },
    "pt": {
        "TITLE": "Com que grupo de objetivas foi gravado este vídeo?",
        "BODY": (
            "Este vídeo não tem informações da objetiva — o grupo de "
            "objetivas correto torna a correspondência profunda muito mais "
            "precisa."
        ),
        "NOGROUPS": (
            "Não há distâncias focais de grupos de objetivas configuradas. "
            "Defina-as primeiro em «Sensor & Objetiva» e depois execute a "
            "correspondência profunda."
        ),
        "NOTRUN": "A correspondência profunda não pôde ser executada. Consulte os registos para mais detalhes.",
    },
    "pt_BR": {
        "TITLE": "Com qual grupo de lentes este vídeo foi gravado?",
        "BODY": (
            "Este vídeo não tem informações da lente — o grupo de lentes "
            "correto torna a correspondência profunda muito mais precisa."
        ),
        "NOGROUPS": (
            "Não há distâncias focais de grupos de lentes configuradas. "
            "Defina-as primeiro em \"Sensor & Lente\" e depois execute a "
            "correspondência profunda."
        ),
        "NOTRUN": "A correspondência profunda não pôde ser executada. Verifique os logs para mais detalhes.",
    },
    "ru": {
        "TITLE": "Какой группой объективов снято это видео?",
        "BODY": (
            "У этого видео нет информации об объективе — правильная группа "
            "объективов делает глубокое сопоставление намного точнее."
        ),
        "NOGROUPS": (
            "Фокусные расстояния групп объективов не настроены. Сначала "
            "задайте их в «Сенсор и Объектив», затем запустите глубокое "
            "сопоставление."
        ),
        "NOTRUN": "Глубокое сопоставление не удалось запустить. Подробности см. в журналах.",
    },
    "sk": {
        "TITLE": "Ktorou skupinou objektívov bolo toto video natočené?",
        "BODY": (
            "Toto video nemá informácie o objektíve — správna skupina "
            "objektívov výrazne spresní hlboké párovanie."
        ),
        "NOGROUPS": (
            "Nie sú nakonfigurované žiadne ohniskové vzdialenosti skupín "
            "objektívov. Najprv ich nastavte v „Snímač & Objektív“ a potom "
            "spustite hlboké párovanie."
        ),
        "NOTRUN": "Hlboké párovanie sa nepodarilo spustiť. Podrobnosti nájdete v protokoloch.",
    },
    "tr": {
        "TITLE": "Bu video hangi lens grubuyla çekildi?",
        "BODY": (
            "Bu videoda lens bilgisi yok — doğru lens grubu derin "
            "eşleştirmeyi çok daha doğru hale getirir."
        ),
        "NOGROUPS": (
            "Yapılandırılmış lens grubu odak uzaklığı yok. Önce "
            "\"Sensör & Lens\" bölümünde ayarlayın, ardından derin "
            "eşleştirmeyi çalıştırın."
        ),
        "NOTRUN": "Derin eşleştirme çalıştırılamadı. Ayrıntılar için günlüklere bakın.",
    },
    "uk": {
        "TITLE": "Якою групою об'єктивів знято це відео?",
        "BODY": (
            "У цього відео немає інформації про об'єктив — правильна група "
            "об'єктивів робить глибоке зіставлення набагато точнішим."
        ),
        "NOGROUPS": (
            "Фокусні відстані груп об'єктивів не налаштовані. Спочатку "
            "задайте їх у «Сенсор і Об'єктив», потім запустіть глибоке "
            "зіставлення."
        ),
        "NOTRUN": "Глибоке зіставлення не вдалося запустити. Деталі див. у журналах.",
    },
    "zh_CN": {
        "TITLE": "这个视频是用哪个镜头组拍摄的？",
        "BODY": "该视频没有镜头信息——选择正确的镜头组能显著提高深度匹配的准确度。",
        "NOGROUPS": "尚未配置任何镜头组焦距。请先在「传感器和镜头」中设置，再运行深度匹配。",
        "NOTRUN": "深度匹配未能运行，请查看日志了解详情。",
    },
    "zh_TW": {
        "TITLE": "這個影片是用哪個鏡頭群組拍攝的？",
        "BODY": "該影片沒有鏡頭資訊——選擇正確的鏡頭群組能顯著提高深度匹配的準確度。",
        "NOGROUPS": "尚未設定任何鏡頭群組焦距。請先在「感光元件與鏡頭」中設定，再執行深度匹配。",
        "NOTRUN": "深度匹配未能執行，請查看日誌了解詳情。",
    },
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


def patch_file(path: pathlib.Path, trans: dict[str, str]) -> list[str]:
    raw = path.read_bytes().decode("utf-8")
    eol = "\r\n" if "\r\n" in raw else "\n"
    patched = []
    for key, source in SOURCES.items():
        src = xml_escape(source)
        needle = (
            f"<source>{src}</source>{eol}"
            f'        <translation type="unfinished"></translation>'
        )
        if needle not in raw:
            continue  # already patched, or entry missing (reported by caller)
        replacement = (
            f"<source>{src}</source>{eol}"
            f"        <translation>{xml_escape(trans[key])}</translation>"
        )
        raw = raw.replace(needle, replacement, 1)
        patched.append(key)
    if patched:
        path.write_bytes(raw.encode("utf-8"))
    return patched


def main() -> int:
    root = pathlib.Path(__file__).resolve().parent.parent / "resources" / "translations"
    ok = True
    for lang, trans in TRANS.items():
        path = root / f"{lang}.ts"
        if not path.exists():
            print(f"MISSING FILE: {path}")
            ok = False
            continue
        patched = patch_file(path, trans)
        # Verify all 4 entries now carry a translation (patched now or before).
        raw = path.read_bytes().decode("utf-8")
        missing = [
            k for k, s in SOURCES.items()
            if f"<source>{xml_escape(s)}</source>" not in raw
        ]
        unfinished = [
            k for k, s in SOURCES.items()
            if f"<source>{xml_escape(s)}</source>" in raw
            and f"<source>{xml_escape(s)}</source>" + ("\r\n" if "\r\n" in raw else "\n")
            + '        <translation type="unfinished"></translation>' in raw
        ]
        status = f"patched={patched}" if patched else "no-op"
        if missing or unfinished:
            print(f"{lang}: {status} MISSING={missing} STILL_UNFINISHED={unfinished}")
            ok = False
        else:
            print(f"{lang}: {status}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
