#!/usr/bin/env python3
# Insert the tutorial-ui-redesign strings into resources/translations/zh_CN.ts.
#
# Byte-exact CRLF insertion (the .ts files use CRLF; mixing in LF would create a
# noisy/again-corrupting diff). Idempotent: re-running is a no-op once patched.
#
# Three target contexts:
#   App             - tutorial step titles / bodies / notes (built in App.qml)
#   TutorialOverlay - the "Step %1 of %2" counter (in TutorialOverlay.qml)
#   VideoArea       - the 3-state preview button tooltips (in VideoArea.qml)
#
# Strings that already exist in the App context with the right translation
# (Render queue / Export / Stabilization settings / Deep search) are reused by Qt
# automatically and are intentionally NOT re-inserted. zh_CN only for now; the
# remaining 21 languages are deferred (design 2026-06-19, future work).

import os
import sys

TS = os.path.join(os.path.dirname(__file__), "..", "resources", "translations", "zh_CN.ts")
TS = os.path.abspath(TS)
CRLF = "\r\n"


def block(src, tr):
    return ("    <message>" + CRLF +
            "        <source>" + src + "</source>" + CRLF +
            "        <translation>" + tr + "</translation>" + CRLF +
            "    </message>" + CRLF)


APP = [
    ("Load your footage", "加载素材"),
    ("Drag your videos and gyro data right here.", "将视频和陀螺仪数据全部拖放到这里"),
    ("Check mounting and lens data", "检查陀螺仪安装和镜头数据"),
    ("Set the mounting orientation; for manual-focus or anamorphic lenses, also set the lens group.",
     "设置安装方向，手动对焦或变宽镜头还需设置镜头组数据。"),
    ("Open the queue to batch-manage videos; you can also reset processing or clear the queue.",
     "展开队列批量管理视频，可重置处理或清空队列。"),
    ('Pick a clip with clear motion, then right-click and choose "Deep match with gyro" to align the video with the gyro data.',
     '选一段运动明显的视频，右键选择 "与陀螺仪深度匹配"，自动对齐视频与陀螺仪数据。'),
    ("Note: the video must fall within the gyro's recorded time range.",
     "注意：视频时间需落在陀螺仪记录范围内。"),
    ("Adjust smoothness, horizon lock and zoom mode. In the render queue you can multi-select or batch-edit clips.",
     "调整平滑度、地平线锁定、缩放模式，在渲染队列中可以多选修改，也可以批量修改。"),
    ('For a finished clip choose "Export stabilized video"; to use the editor plugins choose "Export for plugins". After you export, stabilization begins.',
     '直接出成片选"导出稳定后的视频"，用插件选"导出（搭配插件使用）"。导出后，视频开始稳定处理。'),
    ("Preview", "预览"),
    ('Right-click a video in the queue and choose "Edit" to preview the result.',
     '队列里右键视频，选"编辑"预览效果。'),
    ("Preview and adjust", "预览与调整"),
    ("Click the stabilization-preview button to switch between Original, Stabilized and Overview. You can also fine-tune the stabilization settings per clip.",
     "点稳定预览按钮，在原片/稳定/概览间切换对比。也可以单独调整稳定参数。"),
    ("Press Ctrl+S / Cmd+S to save once you're happy.", "调好参数后 Ctrl+S/Cmd+S 保存。"),
]
TUT = [
    ("Step %1 of %2", "第 %1 / %2 步"),
]
VA = [
    ("Preview: original", "预览：原片"),
    ("Preview: stabilized", "预览：稳定"),
    ("Preview: overview", "预览：稳定概览"),
]


def main():
    with open(TS, "r", encoding="utf-8", newline="") as f:
        content = f.read()

    if "<source>Load your footage</source>" in content:
        print("zh_CN.ts already patched; nothing to do.")
        return 0

    # App: insert right after the App context's <name> line.
    anchor_app = "    <name>App</name>" + CRLF
    if content.count(anchor_app) != 1:
        print("ERROR: App anchor not found uniquely (%d)" % content.count(anchor_app))
        return 1
    content = content.replace(anchor_app, anchor_app + "".join(block(s, t) for s, t in APP), 1)

    # VideoArea: insert before its closing </context> (right after Playback speed).
    anchor_va = "        <translation>回放速度</translation>" + CRLF + "    </message>" + CRLF + "</context>"
    if content.count(anchor_va) != 1:
        print("ERROR: VideoArea anchor not found uniquely (%d)" % content.count(anchor_va))
        return 1
    content = content.replace(
        anchor_va,
        "        <translation>回放速度</translation>" + CRLF + "    </message>" + CRLF +
        "".join(block(s, t) for s, t in VA) + "</context>", 1)

    # TutorialOverlay: insert before the final </context></TS>.
    anchor_tut = "    </message>" + CRLF + "</context>" + CRLF + "</TS>"
    if content.count(anchor_tut) != 1:
        print("ERROR: TutorialOverlay anchor not found uniquely (%d)" % content.count(anchor_tut))
        return 1
    content = content.replace(
        anchor_tut,
        "    </message>" + CRLF + "".join(block(s, t) for s, t in TUT) + "</context>" + CRLF + "</TS>", 1)

    with open(TS, "w", encoding="utf-8", newline="") as f:
        f.write(content)
    print("Patched zh_CN.ts: App +%d, VideoArea +%d, TutorialOverlay +%d" % (len(APP), len(VA), len(TUT)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
