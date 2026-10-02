"""One-shot patch: inject the "Optical motion" offset-method strings into all 23 .ts files.

Run from repo root:
    python _scripts/patch_translations_optical_motion_sync.py

Two messages are added:
  * Popup context:           "Optical motion" (dropdown item), after the "rs-sync" message
  * Synchronization context: the tooltip, after the rs-sync tooltip message

Then regenerate the .qm bundles with ext/6.7.3/mingw_64/bin/lrelease.exe (mingw only).

Idempotent: re-running on already-patched files is a no-op.
"""
from __future__ import annotations

import pathlib
import re
import sys

QML = "../../src/ui/menu/Synchronization.qml"
ITEM_LINE = 450
TIP_LINE = 458

ITEM_SRC = "Optical motion"
TIP_SRC = ("Tracks image features and matches their motion directly to the gyroscope.\n"
           "Needs some camera shake; very smooth motion may produce no sync points.")
TIP_ANCHOR = "Rolling shutter video to gyro synchronization algorithm."

# lang -> (dropdown item, tooltip with a real newline between the two sentences)
TRANS: dict[str, tuple[str, str]] = {
    "cs": ("Optický pohyb",
           "Sleduje prvky obrazu a jejich pohyb přímo porovnává s gyroskopem.\n"
           "Vyžaduje určité chvění kamery; velmi plynulý pohyb nemusí poskytnout žádné synchronizační body."),
    "da": ("Optisk bevægelse",
           "Sporer billedfunktioner og matcher deres bevægelse direkte med gyroskopet.\n"
           "Kræver en vis kamerarystelse; meget jævn bevægelse giver muligvis ingen synkroniseringspunkter."),
    "de": ("Optische Bewegung",
           "Verfolgt Bildmerkmale und gleicht deren Bewegung direkt mit dem Gyroskop ab.\n"
           "Benötigt etwas Kameraverwacklung; bei sehr gleichmäßiger Bewegung entstehen möglicherweise keine Synchronisationspunkte."),
    "el": ("Οπτική κίνηση",
           "Παρακολουθεί χαρακτηριστικά της εικόνας και ταιριάζει απευθείας την κίνησή τους με το γυροσκόπιο.\n"
           "Απαιτεί κάποιο τρέμουλο της κάμερας· πολύ ομαλή κίνηση μπορεί να μην δώσει σημεία συγχρονισμού."),
    "es": ("Movimiento óptico",
           "Rastrea características de la imagen y compara su movimiento directamente con el giroscopio.\n"
           "Necesita algo de vibración de la cámara; un movimiento muy suave puede no generar puntos de sincronización."),
    "fi": ("Optinen liike",
           "Seuraa kuvan piirteitä ja täsmää niiden liikkeen suoraan gyroskooppiin.\n"
           "Vaatii jonkin verran kameran tärinää; hyvin tasainen liike ei välttämättä tuota synkronointipisteitä."),
    "fr": ("Mouvement optique",
           "Suit les caractéristiques de l'image et fait correspondre directement leur mouvement au gyroscope.\n"
           "Nécessite un peu de tremblement de la caméra ; un mouvement très fluide peut ne produire aucun point de synchronisation."),
    "gl": ("Movemento óptico",
           "Rastrexa características da imaxe e compara o seu movemento directamente co xiroscopio.\n"
           "Precisa algo de vibración da cámara; un movemento moi suave pode non xerar puntos de sincronización."),
    "id": ("Gerakan optik",
           "Melacak fitur gambar dan mencocokkan gerakannya langsung dengan giroskop.\n"
           "Membutuhkan sedikit guncangan kamera; gerakan yang sangat halus mungkin tidak menghasilkan titik sinkronisasi."),
    "it": ("Movimento ottico",
           "Traccia le caratteristiche dell'immagine e ne confronta direttamente il movimento con il giroscopio.\n"
           "Richiede un po' di vibrazione della fotocamera; un movimento molto fluido potrebbe non produrre punti di sincronizzazione."),
    "ja": ("オプティカルモーション",
           "画像の特徴点を追跡し、その動きをジャイロスコープと直接照合します。\n"
           "ある程度のカメラの揺れが必要です。非常に滑らかな動きでは同期ポイントが得られない場合があります。"),
    "ko": ("광학 모션",
           "영상의 특징점을 추적하여 그 움직임을 자이로스코프와 직접 맞춥니다.\n"
           "어느 정도의 카메라 흔들림이 필요하며, 매우 부드러운 움직임에서는 동기화 지점이 생성되지 않을 수 있습니다."),
    "no": ("Optisk bevegelse",
           "Sporer bildefunksjoner og matcher bevegelsen deres direkte mot gyroskopet.\n"
           "Krever noe kamerarystelse; svært jevn bevegelse gir kanskje ingen synkroniseringspunkter."),
    "pl": ("Ruch optyczny",
           "Śledzi cechy obrazu i dopasowuje ich ruch bezpośrednio do żyroskopu.\n"
           "Wymaga pewnych drgań kamery; bardzo płynny ruch może nie dać żadnych punktów synchronizacji."),
    "pt": ("Movimento óptico",
           "Rastreia características da imagem e compara o seu movimento diretamente com o giroscópio.\n"
           "Requer alguma vibração da câmara; um movimento muito suave pode não produzir pontos de sincronização."),
    "pt_BR": ("Movimento óptico",
              "Rastreia características da imagem e compara o movimento delas diretamente com o giroscópio.\n"
              "Requer alguma vibração da câmera; um movimento muito suave pode não gerar pontos de sincronização."),
    "ru": ("Оптическое движение",
           "Отслеживает признаки изображения и напрямую сопоставляет их движение с гироскопом.\n"
           "Требуется некоторая тряска камеры; при очень плавном движении точки синхронизации могут не найтись."),
    "sk": ("Optický pohyb",
           "Sleduje prvky obrazu a ich pohyb priamo porovnáva s gyroskopom.\n"
           "Vyžaduje určité chvenie kamery; veľmi plynulý pohyb nemusí poskytnúť žiadne synchronizačné body."),
    "tr": ("Optik hareket",
           "Görüntü özelliklerini izler ve hareketlerini doğrudan jiroskopla eşleştirir.\n"
           "Biraz kamera sarsıntısı gerektirir; çok yumuşak hareket hiç senkronizasyon noktası üretmeyebilir."),
    "uk": ("Оптичний рух",
           "Відстежує ознаки зображення та напряму зіставляє їхній рух із гіроскопом.\n"
           "Потрібна певна тряска камери; за дуже плавного руху точки синхронізації можуть не знайтися."),
    "zh_CN": ("光学运动",
              "跟踪画面特征点，直接将其运动与陀螺仪匹配。\n"
              "需要一定的相机抖动；非常平滑的运动可能得不到同步点。"),
    "zh_TW": ("光學運動",
              "追蹤畫面特徵點，直接將其運動與陀螺儀比對。\n"
              "需要一定的相機晃動；非常平滑的運動可能得不到同步點。"),
}


def esc(s: str) -> str:
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def make_message(line: int, source: str, translation: str | None, nl: str) -> str:
    if translation is None:
        trans_tag = '<translation type="unfinished"></translation>'
    else:
        trans_tag = f"<translation>{esc(translation)}</translation>"
    text = (
        "    <message>\n"
        f'        <location filename="{QML}" line="{line}"/>\n'
        f"        <source>{esc(source)}</source>\n"
        f"        {trans_tag}\n"
        "    </message>\n"
    )
    # Every newline (markup and the embedded tooltip line break) follows the file's style.
    return text.replace("\n", nl)


def insert_after(content: str, context: str, anchor_source: str, message: str, nl: str) -> str | None:
    """Insert `message` after the <message> whose <source> starts with `anchor_source` inside `context`."""
    cm = re.search(r"<context>\s*<name>" + re.escape(context) + r"</name>(.*?)</context>", content, re.S)
    if not cm:
        return None
    body = cm.group(1)
    sm = body.find("<source>" + anchor_source)
    if sm < 0:
        return None
    end_tag = "</message>" + nl
    e = body.find(end_tag, sm)
    if e < 0:
        return None
    pos = cm.start(1) + e + len(end_tag)
    return content[:pos] + message + content[pos:]


def patch_file(path: pathlib.Path, tr: tuple[str, str] | None) -> str:
    content = path.read_bytes().decode("utf-8")
    nl = "\r\n" if "\r\n" in content else "\n"
    item_done = f"<source>{ITEM_SRC}</source>" in content
    tip_done = "<source>Tracks image features and matches" in content
    if item_done and tip_done:
        return f"SKIP {path.name} (already patched)"

    if not item_done:
        msg = make_message(ITEM_LINE, ITEM_SRC, tr[0] if tr else None, nl)
        content = insert_after(content, "Popup", "rs-sync</source>", msg, nl)
        if content is None:
            return f"FAIL {path.name}: Popup/rs-sync anchor not found"
    if not tip_done:
        msg = make_message(TIP_LINE, TIP_SRC, tr[1] if tr else None, nl)
        content = insert_after(content, "Synchronization", TIP_ANCHOR, msg, nl)
        if content is None:
            return f"FAIL {path.name}: Synchronization/rs-sync tooltip anchor not found"
    path.write_bytes(content.encode("utf-8"))
    return f"OK   {path.name}"


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    if not base.is_dir():
        print(f"Translation dir missing: {base}", file=sys.stderr)
        return 1

    results = [patch_file(base / "gyroflow.ts", None)]
    for lang, tr in TRANS.items():
        path = base / f"{lang}.ts"
        if not path.is_file():
            results.append(f"MISS {path.name}")
            continue
        results.append(patch_file(path, tr))
    rc = 0
    for r in results:
        print(r)
        if r.startswith(("FAIL", "MISS")):
            rc = 1
    return rc


if __name__ == "__main__":
    sys.exit(main())
