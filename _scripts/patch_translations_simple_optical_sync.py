"""Inject simple-mode optical sync messages; repeated runs are a no-op.

Run from the repository root, then compile with the MinGW lrelease executable.
"""
from __future__ import annotations
import pathlib
import re
import sys

QML = "../../src/ui/menu/SimpleStabilization.qml"
ITEM_LINE = 456
TIP_LINE = 457
ITEM_SRC = "Optical sync"
TIP_SRC = "Synchronize by tracking features in the video instead of using optical flow. Also used by batch matching and deep matching. Experimental: it may find no sync point for extreme rotation or very smooth motion."

TRANS: dict[str, tuple[str, str]] = {
"cs": ("Optická synchronizace", "Synchronizuje sledováním prvků ve videu místo optického toku. Používá se také při dávkovém a hloubkovém párování. Experimentální: při extrémní rotaci nebo velmi plynulém pohybu nemusí najít žádný synchronizační bod."),
"da": ("Optisk synkronisering", "Synkroniserer ved at spore detaljer i videoen i stedet for at bruge optisk flow. Bruges også til batchmatchning og dyb matchning. Eksperimentelt: ved ekstrem rotation eller meget jævn bevægelse findes muligvis intet synkroniseringspunkt."),
"de": ("Optische Synchronisierung", "Synchronisiert durch Verfolgung von Bildmerkmalen im Video statt durch optischen Fluss. Wird auch beim Stapelabgleich und Tiefenabgleich verwendet. Experimentell: Bei extremer Drehung oder sehr gleichmäßiger Bewegung wird möglicherweise kein Synchronisierungspunkt gefunden."),
"el": ("Οπτικός συγχρονισμός", "Συγχρονίζει παρακολουθώντας χαρακτηριστικά στο βίντεο αντί να χρησιμοποιεί οπτική ροή. Χρησιμοποιείται επίσης στη μαζική και στη βαθιά αντιστοίχιση. Πειραματικό: μπορεί να μη βρει σημείο συγχρονισμού σε ακραία περιστροφή ή πολύ ομαλή κίνηση."),
"es": ("Sincronización óptica", "Sincroniza siguiendo características del vídeo en lugar de utilizar flujo óptico. También se utiliza en la asociación por lotes y la asociación profunda. Experimental: puede no encontrar ningún punto de sincronización con rotación extrema o movimiento muy suave."),
"fi": ("Optinen synkronointi", "Synkronoi seuraamalla videon piirteitä optisen virtauksen sijaan. Käytetään myös erä- ja syväkohdistuksessa. Kokeellinen: voimakas kierto tai hyvin tasainen liike voi estää synkronointipisteen löytymisen."),
"fr": ("Synchronisation optique", "Synchronise en suivant les caractéristiques de l’image dans la vidéo plutôt qu’en utilisant le flux optique. Également utilisée pour l’association par lots et l’association approfondie. Expérimental : une rotation extrême ou un mouvement très fluide peut empêcher de trouver un point de synchronisation."),
"gl": ("Sincronización óptica", "Sincroniza seguindo características do vídeo en lugar de usar fluxo óptico. Tamén se usa na asociación por lotes e na asociación profunda. Experimental: pode non atopar ningún punto de sincronización con rotación extrema ou movemento moi suave."),
"id": ("Sinkronisasi optik", "Menyinkronkan dengan melacak fitur dalam video alih-alih menggunakan aliran optik. Juga digunakan untuk pencocokan batch dan pencocokan mendalam. Eksperimental: mungkin tidak menemukan titik sinkronisasi saat rotasi ekstrem atau gerakan sangat halus."),
"it": ("Sincronizzazione ottica", "Sincronizza seguendo le caratteristiche nel video anziché utilizzare il flusso ottico. Utilizzata anche per l’abbinamento in batch e l’abbinamento approfondito. Sperimentale: potrebbe non trovare alcun punto di sincronizzazione in caso di rotazione estrema o movimento molto fluido."),
"ja": ("光学同期", "オプティカルフローの代わりに動画内の特徴点を追跡して同期します。一括マッチングと詳細マッチングにも使用されます。実験的機能：極端な回転や非常に滑らかな動きでは同期ポイントが見つからない場合があります。"),
"ko": ("광학 동기화", "광학 흐름 대신 영상의 특징점을 추적하여 동기화합니다. 일괄 매칭과 심층 매칭에도 사용됩니다. 실험적 기능: 극심한 회전이나 매우 부드러운 움직임에서는 동기화 지점을 찾지 못할 수 있습니다."),
"no": ("Optisk synkronisering", "Synkroniserer ved å spore detaljer i videoen i stedet for å bruke optisk flyt. Brukes også til batchmatching og dyp matching. Eksperimentelt: ved ekstrem rotasjon eller svært jevn bevegelse finnes kanskje ikke noe synkroniseringspunkt."),
"pl": ("Synchronizacja optyczna", "Synchronizuje przez śledzenie cech w filmie zamiast używania przepływu optycznego. Używana także w dopasowaniu wsadowym i głębokim. Eksperymentalna: przy skrajnym obrocie lub bardzo płynnym ruchu może nie znaleźć punktu synchronizacji."),
"pt": ("Sincronização óptica", "Sincroniza seguindo características no vídeo em vez de usar fluxo óptico. Também utilizada na correspondência em lote e na correspondência profunda. Experimental: pode não encontrar nenhum ponto de sincronização com rotação extrema ou movimento muito suave."),
"pt_BR": ("Sincronização óptica", "Sincroniza rastreando características no vídeo em vez de usar fluxo óptico. Também utilizada na correspondência em lote e na correspondência profunda. Experimental: pode não encontrar nenhum ponto de sincronização com rotação extrema ou movimento muito suave."),
"ru": ("Оптическая синхронизация", "Синхронизирует, отслеживая признаки в видео вместо использования оптического потока. Также используется при пакетном и глубоком сопоставлении. Экспериментальная функция: при экстремальном вращении или очень плавном движении точка синхронизации может не найтись."),
"sk": ("Optická synchronizácia", "Synchronizuje sledovaním prvkov vo videu namiesto optického toku. Používa sa aj pri dávkovom a hĺbkovom párovaní. Experimentálne: pri extrémnej rotácii alebo veľmi plynulom pohybe nemusí nájsť žiadny synchronizačný bod."),
"tr": ("Optik senkronizasyon", "Optik akış yerine videodaki özellikleri izleyerek senkronize eder. Toplu eşleştirme ve derin eşleştirmede de kullanılır. Deneysel: aşırı dönüş veya çok yumuşak hareket sırasında senkronizasyon noktası bulamayabilir."),
"uk": ("Оптична синхронізація", "Синхронізує, відстежуючи ознаки у відео замість використання оптичного потоку. Також використовується для пакетного та глибокого зіставлення. Експериментальна функція: за екстремального обертання або дуже плавного руху точка синхронізації може не знайтися."),
"zh_CN": ("光学同步", "通过跟踪画面中的特征点来同步，不使用光流。批量匹配和深度匹配同样使用。实验功能：极端旋转或非常平滑的运动可能找不到同步点。"),
"zh_TW": ("光學同步", "透過追蹤畫面中的特徵點來同步，不使用光流。批次匹配和深度匹配同樣使用。實驗功能：極端旋轉或非常平滑的運動可能找不到同步點。"),
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
    tip_done = f"<source>{TIP_SRC}</source>" in content
    if item_done and tip_done:
        return f"SKIP {path.name} (already patched)"

    if not item_done:
        msg = make_message(ITEM_LINE, ITEM_SRC, tr[0] if tr else None, nl)
        content = insert_after(content, "SimpleStabilization", "AI SYNC</source>", msg, nl)
        if content is None:
            return f"FAIL {path.name}: SimpleStabilization/AI SYNC anchor not found"
    if not tip_done:
        msg = make_message(TIP_LINE, TIP_SRC, tr[1] if tr else None, nl)
        content = insert_after(content, "SimpleStabilization", ITEM_SRC + "</source>", msg, nl)
        if content is None:
            return f"FAIL {path.name}: SimpleStabilization/Optical sync anchor not found"
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
