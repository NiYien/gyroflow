"""One-shot patch: inject tutorial onboarding strings into App context (gyroflow.ts + zh_CN.ts)
and create a new TutorialOverlay context in both files.

Run from repo root:
    python _scripts/patch_translations_tutorial.py

Then regenerate zh_CN.qm:
    ext/6.7.3/msvc2019_64/bin/lrelease.exe resources/translations/zh_CN.ts -qm resources/translations/zh_CN.qm

Idempotent: per-context, per-source check prevents duplicate entries.
- App context: a source is skipped if it already appears inside the App <context> block.
  Expected auto-skips: "Export", "Render queue", "Report a problem".
- TutorialOverlay context: created fresh if it does not exist; skipped entirely if already present.
"""
from __future__ import annotations

import pathlib
import re
import sys

# ---------------------------------------------------------------------------
# App context entries: (source, zh_CN_translation)
# The zh_CN_translation is stored here; gyroflow.ts uses unfinished empty tags.
# ---------------------------------------------------------------------------
APP_ENTRIES: list[tuple[str, str]] = [
    (
        "Replay tutorial",
        "重新观看教程",
    ),
    (
        "Load your video",
        "加载视频",
    ),
    (
        'Click "Open file" (top-left) to pick the clip you want to stabilize. You can select several at once. Most cameras embed gyro data in the video, so it is detected automatically.',
        "点左上角「打开文件」选择要稳定的视频。可一次多选。大多数相机的陀螺仪数据已嵌在视频里，会被自动识别。",
    ),
    (
        "Set the mounting orientation",
        "设置安装方向",
    ),
    (
        "Tell Gyroflow how the camera was mounted. A wrong orientation makes stabilization correct the wrong way.",
        "告诉软件相机是怎么安装的。方向选错会让稳定往反方向纠。",
    ),
    (
        "Lens groups",
        "镜头组数据",
    ),
    (
        "Configure the lens and sensor so lens distortion is corrected correctly for your footage.",
        "配置镜头和传感器信息，让画面畸变得到正确矫正。",
    ),
    (
        "Stabilization settings",
        "稳定参数",
    ),
    (
        "Adjust smoothness, horizon lock and how much the frame is cropped.",
        "调整平滑度、地平线锁定，以及画面裁切的程度。",
    ),
    (
        "Deep search",
        "深度搜索",
    ),
    (
        'When the gyro data is in a separate file and the timing does not line up, right-click a video in the render queue and choose "Deep match with gyro" to find the offset automatically.',
        "当陀螺仪数据在单独的文件里、且时间对不齐时，在渲染队列里右键某个视频，选择「Deep match with gyro」自动找到时间偏移。",
    ),
    # "Export" -> SKIP IF PRESENT in App context (already translated)
    (
        "Export",
        "导出",
    ),
    (
        'Click "Export stabilized video" to render the result, or "Export for plugins" to produce a project file for the editor plugins.',
        "点「导出稳定视频」渲染成片；或用「导出（搭配插件使用）」生成工程文件，配合剪辑软件插件使用。",
    ),
    (
        "Preview the result",
        "预览效果",
    ),
    (
        'Pick any video in the render queue, right-click and choose "Edit" to load it into the main preview and check the stabilization in real time.',
        "在渲染队列里随机选一个视频，右键选择「Edit」（编辑），把它载入主预览，实时查看稳定效果。",
    ),
    # "Render queue" -> SKIP IF PRESENT in App context (already translated)
    (
        "Render queue",
        "渲染队列",
    ),
    (
        "Use this button to show or hide the render queue. Batch processing of multiple videos all happens in the queue.",
        "用这个按钮展开或收起渲染队列。批量处理多个视频都在队列里进行。",
    ),
    (
        "Editor plugins",
        "安装插件",
    ),
    (
        "Install the Gyroflow plugin into your editor (Premiere, DaVinci Resolve and more) to stabilize on the timeline.",
        "把 Gyroflow 插件安装到你的剪辑软件（Premiere、DaVinci Resolve 等），即可在时间线上稳定。",
    ),
    # "Report a problem" -> SKIP IF PRESENT in App context (already translated)
    (
        "Report a problem",
        "报告问题",
    ),
    (
        "Run into a bug? Click here to upload logs and send us feedback.",
        "遇到问题？点这里上传日志并反馈给我们。",
    ),
    # --- Condensed 5-step tour (added 2026-06-18) ---
    (
        "Sensor and lens",
        "传感器与镜头",
    ),
    (
        'Drag a video into the main window or the render queue, or click "Open file" (top-left). Most cameras embed gyro data, so it is detected automatically.',
        "把视频拖进主窗口或渲染队列，或点左上角「打开文件」。大多数相机的陀螺仪数据已嵌在视频里，会被自动识别。",
    ),
    (
        "Set how the camera was mounted (a wrong orientation corrects the wrong way), then pick the lens group so distortion is corrected correctly.",
        "设置相机的安装方向（方向选错会让稳定往反方向纠），再选择镜头组让画面畸变得到正确矫正。",
    ),
    (
        'Batch processing of multiple videos happens here. Right-click a video for "Deep match with gyro" (finds the offset when the gyro is a separate file) or "Edit" (loads it into the main preview to check the result).',
        "批量处理多个视频都在这里进行。右键某个视频可选「Deep match with gyro」（陀螺在单独文件、时间对不齐时自动找偏移）或「Edit」（载入主预览查看稳定效果）。",
    ),
    # --- 6-step refinement: deep match split out, queue step edit-only (added 2026-06-18) ---
    (
        "Deep match",
        "深度匹配",
    ),
    (
        'Batch processing of multiple videos happens here. Right-click a video and choose "Edit" to load it into the main preview and check the result.',
        "批量处理多个视频都在这里进行。右键某个视频选「Edit」（编辑），把它载入主预览查看稳定效果。",
    ),
]

# ---------------------------------------------------------------------------
# TutorialOverlay context entries: (source, zh_CN_translation)
# ---------------------------------------------------------------------------
OVERLAY_ENTRIES: list[tuple[str, str]] = [
    ("Skip", "跳过"),
    ("Back", "上一步"),
    ("Next", "下一步"),
    ("Done", "完成"),
]


def make_unfinished_tag() -> str:
    """Return the unfinished translation tag used in the gyroflow.ts template."""
    return '<translation type="unfinished"></translation>'


def make_finished_tag(zh: str) -> str:
    """Return a finished translation tag with the given Chinese text."""
    return f"<translation>{zh}</translation>"


def make_message_block(source: str, trans_tag: str, eol: str) -> str:
    """Build a <message> block given the source text and a ready trans_tag string."""
    lines = [
        "    <message>",
        f"        <source>{source}</source>",
        f"        {trans_tag}",
        "    </message>",
    ]
    return eol.join(lines) + eol


def make_overlay_context_block(
    entries: list[tuple[str, str]],
    is_template: bool,
    eol: str,
) -> str:
    """Build a complete <context><name>TutorialOverlay</name>...</context> block."""
    lines = [
        "<context>",
        "    <name>TutorialOverlay</name>",
    ]
    for source, zh in entries:
        trans_tag = make_unfinished_tag() if is_template else make_finished_tag(zh)
        lines += [
            "    <message>",
            f"        <source>{source}</source>",
            f"        {trans_tag}",
            "    </message>",
        ]
    lines.append("</context>")
    return eol.join(lines) + eol


def extract_app_context_sources(content: str) -> set[str]:
    """Return the set of <source> strings already inside the App <context> block."""
    match = re.search(
        r"<context>\s*<name>App</name>(.*?)</context>",
        content,
        re.DOTALL,
    )
    if not match:
        return set()
    block = match.group(1)
    return set(re.findall(r"<source>(.*?)</source>", block, re.DOTALL))


def patch_app_context(
    content: str,
    entries: list[tuple[str, str]],
    is_template: bool,
    eol: str,
) -> tuple[str, list[str], list[str]]:
    """Insert missing <message> entries into the App context block.

    Returns (new_content, added_sources, skipped_sources).
    """
    existing = extract_app_context_sources(content)
    added: list[str] = []
    skipped: list[str] = []

    # Insert point: right after <name>App</name>
    needle = f"    <name>App</name>{eol}"
    if needle not in content:
        raise ValueError("App context name tag not found in file")

    # Build the block of new messages to insert before the first existing message
    insert_block = ""
    for source, zh in entries:
        if source in existing:
            skipped.append(source)
            continue
        trans_tag = make_unfinished_tag() if is_template else make_finished_tag(zh)
        msg = make_message_block(source, trans_tag, eol)
        insert_block += msg
        added.append(source)

    if insert_block:
        content = content.replace(needle, needle + insert_block, 1)

    return content, added, skipped


def patch_tutorial_overlay_context(
    content: str,
    entries: list[tuple[str, str]],
    is_template: bool,
    eol: str,
) -> tuple[str, str]:
    """Insert the TutorialOverlay context before </TS> if it does not already exist.

    Returns (new_content, status_message).
    """
    if "<name>TutorialOverlay</name>" in content:
        return content, "SKIP TutorialOverlay context (already present)"

    overlay_block = make_overlay_context_block(entries, is_template, eol)
    # Insert before the closing </TS> tag
    content = content.replace("</TS>", overlay_block + "</TS>", 1)
    return content, "OK   TutorialOverlay context created"


def patch_file(
    path: pathlib.Path,
    app_entries: list[tuple[str, str]],
    overlay_entries: list[tuple[str, str]],
    is_template: bool,
) -> list[str]:
    """Patch a single .ts file. Returns list of status lines."""
    raw = path.read_bytes()
    # Detect line ending from the file itself to preserve it faithfully
    file_eol = "\r\n" if b"\r\n" in raw else "\n"
    content = raw.decode("utf-8")

    lines_out: list[str] = []

    # Patch App context
    try:
        content, added, skipped = patch_app_context(content, app_entries, is_template, file_eol)
    except ValueError as e:
        lines_out.append(f"FAIL {path.name}: {e}")
        return lines_out

    for s in added:
        lines_out.append(f"  ADD  [{path.name}] App/<source>{s[:60]}</source>")
    for s in skipped:
        lines_out.append(f"  SKIP [{path.name}] App/<source>{s[:60]}</source> (already present)")

    # Patch TutorialOverlay context
    content, status = patch_tutorial_overlay_context(content, overlay_entries, is_template, file_eol)
    lines_out.append(f"  {status} in {path.name}")

    path.write_bytes(content.encode("utf-8"))
    return lines_out


def main() -> int:
    base = pathlib.Path(__file__).resolve().parents[1] / "resources" / "translations"
    if not base.is_dir():
        print(f"Translation dir missing: {base}", file=sys.stderr)
        return 1

    # --- Patch gyroflow.ts (source template: unfinished empty translations) ---
    print("\n=== Patching gyroflow.ts ===")
    for line in patch_file(base / "gyroflow.ts", APP_ENTRIES, OVERLAY_ENTRIES, is_template=True):
        print(line)

    # --- Patch zh_CN.ts (Chinese translations, CRLF line endings) ---
    print("\n=== Patching zh_CN.ts ===")
    for line in patch_file(base / "zh_CN.ts", APP_ENTRIES, OVERLAY_ENTRIES, is_template=False):
        print(line)

    print("\nDone.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
