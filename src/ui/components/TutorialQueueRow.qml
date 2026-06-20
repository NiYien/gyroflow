// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

import QtQuick

// Faithful static replica of ONE render-queue row in its Simple-mode
// "deep-matched + synced" state, plus a static right-click menu panel.
// It mirrors the visual subtree of src/ui/RenderQueue.qml's delegate value-for-
// value (same widths / x-offsets / font sizes / colours / visibility), driven by
// a mock object instead of model roles + render_queue.* calls. Zero backend.
//
// IMPORTANT: the mock property is `rowData`, NOT `data` — `data` is Item's default
// property (its visual-children list); shadowing it makes declared children
// instantiate but never render. See feedback_qml_data_default_property.
Rectangle {
    id: root;
    color: "transparent";

    // --- Public API ---
    property var rowData: ({
        thumbnail:        "",            // qrc path, or "" -> neutral placeholder
        filename:         "DJI_0042.MP4",
        duration_ms:      5300,
        smoothness:       0.5,           // 0..1
        horizon_lock:     true,
        auto_rotate:      true,
        zoom_mode:        "dynamic",     // "static" | "dynamic" | "none"
        framerate:        59.94,         // 0 -> chip hidden
        focal_mm:         24,
        gyro_filename:    "GX010042.MP4",
        gyro_time:        "14:30:05",    // formatGyroTime clock string (HH:mm:ss)
        gyro_duration_ms: 2573300,       // gyro file duration -> submenu "(2573.3s)" suffix
        gyro_color_index: 0
    });
    property real rowWidth: 360 * dpiScale;
    property string highlightItem: "";   // "deep" | "edit" | ""
    property bool menuShown: true;
    // Evolving state through the tutorial:
    //   matched -> gyro colour bar + "Deep ⚡ <gyro>" annotation (after deep-search step)
    //   synced  -> green status background + "Sync confirmed" line (after export step)
    property bool matched: true;
    property bool synced: true;

    implicitWidth:  rowWidth;
    // The replica menu floats (z-stacked) below the row and does NOT add to
    // implicitHeight, so stacked tutorial rows abut cleanly and the first row's
    // menu overlays the rows beneath it instead of pushing them down.
    implicitHeight: rowRect.height;

    // --- Palette (RenderQueue.qml lines 40-54), verbatim values ---
    readonly property bool lightTheme: (style === "light");
    readonly property var darkGyroColors:  ["#76baed","#70e574","#f6a00b","#e87de8","#ed7676","#5ce0d8"];
    readonly property var lightGyroColors: ["#2f78b6","#2d8c4d","#ad6a00","#9b55b7","#c55d5d","#1f8f9f"];
    readonly property var gyroColors: lightTheme ? lightGyroColors : darkGyroColors;
    readonly property color gyroTimeTextColor:   lightTheme ? "#17324d" : "#ffffff";
    readonly property color deepMatchStatusColor: lightTheme ? "#6a3fa8" : "#b88ef0";
    readonly property color finishedStatusColor: lightTheme ? "#2a8a4c" : "#70e574";
    readonly property color queueOutlineColor:   lightTheme ? "#1f111111" : "#70ffffff";
    readonly property real  matchedGyroOpacity:  lightTheme ? 0.28 : 0.30;
    readonly property color matchColor: gyroColors[(rowData.gyro_color_index || 0) % gyroColors.length];

    // styleAccentColor / styleTextColor are injected as QStrings — coerce to color.
    readonly property color accentColorC: styleAccentColor;
    readonly property color textColorC:   styleTextColor;

    // --- Sizes: Simple-mode desktop values from RenderQueue.qml ---
    readonly property real basicTextSize: 14 * dpiScale;     // line 1245 (simple desktop)
    // gyroColumnWidth is 0 until a gyro is matched (line 220: hasGyroFiles ? 65 : 0).
    readonly property real gyroColumnWidth: matched ? 65 * dpiScale : 0;
    // Width of the deep-match submenu panel (used both to size it and to shift the parent
    // menu left so the right-opening submenu stays on-screen).
    readonly property real deepSubmenuWidth: 252 * dpiScale;

    function withAlpha(c, a) { return Qt.rgba(c.r, c.g, c.b, a); }  // line 241

    readonly property string durationStr: (rowData.duration_ms > 0)
        ? (rowData.duration_ms / 1000).toFixed(1) + "s" : "";
    readonly property string zoomStr: {
        const zm = rowData.zoom_mode || "none";
        if (zm === "static")  return qsTranslate("Popup", "Static zoom");
        if (zm === "dynamic") return qsTranslate("Popup", "Dynamic zooming");
        return qsTranslate("Popup", "No zooming");
    }

    // -----------------------------------------------------------------------
    // ROW (mirrors the delegate, RenderQueue.qml 1631-2264)
    // -----------------------------------------------------------------------
    Rectangle {
        id: rowRect;
        width: root.rowWidth;
        // delegateContentHeight: min 86 in simple desktop (line 1221-1226).
        height: Math.max(86 * dpiScale, textColumn.implicitHeight + 20 * dpiScale);
        color: "transparent";
        radius: 5 * dpiScale;

        // Row background (RenderQueue.qml 1631-1638)
        Rectangle {
            anchors.fill: parent;
            color: styleBackground2;
            opacity: 0.2;
            radius: 5 * dpiScale;
        }
        // Synced/finished status background (RenderQueue.qml 1923-1934) — only after export.
        Rectangle {
            anchors.fill: parent;
            visible: root.synced;
            color: root.withAlpha(root.finishedStatusColor, root.lightTheme ? 0.12 : 0.19);
            radius: 5 * dpiScale;
            opacity: 0.8;
            border.color: root.finishedStatusColor;
            border.width: 1;
        }

        // Checkbox column (RenderQueue.qml 1642-1658) — real CheckBox, visual-only.
        Item {
            id: checkboxCol;
            width: 32 * dpiScale;
            anchors.left: parent.left;
            anchors.top: parent.top;
            anchors.bottom: parent.bottom;
            CheckBox {
                anchors.verticalCenter: parent.verticalCenter;
                anchors.horizontalCenter: parent.horizontalCenter;
                checked: false;
                enabled: false;
                opacity: 1.0;
                scale: 0.85;
            }
        }

        // Gyro colour-bar column (RenderQueue.qml 1805-1853)
        Item {
            id: gyroArea;
            width: root.gyroColumnWidth;
            anchors.left: checkboxCol.right;
            anchors.top: parent.top;
            anchors.bottom: parent.bottom;
            Rectangle {
                anchors.fill: parent;
                color: root.matchColor;
                opacity: root.matchedGyroOpacity;
                radius: 3 * dpiScale;
                border.width: root.lightTheme ? 1 * dpiScale : 0;
                border.color: root.withAlpha(root.matchColor, 0.40);
            }
            BasicText {
                // Only on matched rows (column width is 0 otherwise); blank for
                // same-gyro sibling rows so the time shows on the first row only.
                visible: root.matched;
                anchors.top: parent.top;
                anchors.topMargin: 4 * dpiScale;
                anchors.horizontalCenter: parent.horizontalCenter;
                leftPadding: 0;
                text: rowData.gyro_time || "";
                color: root.gyroTimeTextColor;
                font.pixelSize: 11 * dpiScale;
                font.bold: true;
            }
        }

        // Main content (RenderQueue.qml 2069-2264). Matched -> separatorCol width 0,
        // so innerItm.x = 5 + checkbox(32) + gyro(65).
        Item {
            id: innerItm;
            x: 5 * dpiScale + checkboxCol.width + gyroArea.width;
            width: parent.width - x - 5 * dpiScale;
            anchors.top: parent.top;
            anchors.bottom: parent.bottom;

            // Thumbnail (RenderQueue.qml 2075-2092)
            Image {
                id: thumb;
                x: 5 * dpiScale;
                width: 50 * dpiScale;
                height: 50 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
                source: rowData.thumbnail || "";
                fillMode: Image.PreserveAspectCrop;
                // Neutral placeholder until a real captured thumbnail is supplied.
                Rectangle {
                    anchors.fill: parent;
                    radius: 5 * dpiScale;
                    color: root.lightTheme ? "#e0e0e0" : "#2a2a2a";
                    visible: !rowData.thumbnail;
                }
                // 1px border overlay (RenderQueue.qml 2082-2090)
                Rectangle {
                    anchors.fill: parent;
                    anchors.margins: -1 * dpiScale;
                    color: "transparent";
                    radius: 5 * dpiScale;
                    border.width: 1 * dpiScale;
                    border.color: styleVideoBorderColor;
                }
            }

            // Text column (RenderQueue.qml 2094-2253). BasicText defaults match the
            // delegate (font.family styleFont, leftPadding 10).
            Column {
                id: textColumn;
                x: 55 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
                spacing: 3 * dpiScale;
                width: Math.max(1 * dpiScale, parent.width - x - 10 * dpiScale);

                // Filename + duration (line 2101-2109) — bold 14.
                BasicText {
                    text: (rowData.filename || "") +
                          (rowData.duration_ms > 0 ? " <small>(" + root.durationStr + ")</small>" : "");
                    font.bold: true;
                    font.pixelSize: 14 * dpiScale;
                    width: parent.width;
                    wrapMode: Text.WordWrap;
                    textFormat: Text.RichText;
                }
                // Stabilisation params Flow (line 2131-2183) — basicTextSize.
                Flow {
                    spacing: 10 * dpiScale;
                    width: parent.width;
                    BasicText {
                        font.pixelSize: root.basicTextSize;
                        textFormat: Text.RichText;
                        text: qsTranslate("Stabilization", "Smoothness") +
                              " <b>" + Math.round((rowData.smoothness || 0.5) * 100) + "%</b>";
                    }
                    BasicText {
                        font.pixelSize: root.basicTextSize;
                        text: qsTranslate("Stabilization", "Lock horizon") + " " + (rowData.horizon_lock ? "✓" : "✗");
                    }
                    BasicText {
                        font.pixelSize: root.basicTextSize;
                        text: qsTranslate("Stabilization", "Auto rotate") + " " + (rowData.auto_rotate ? "✓" : "✗");
                    }
                    BasicText {
                        font.pixelSize: root.basicTextSize;
                        text: root.zoomStr;
                    }
                    // Framerate (line 2152-2163): integer -> no decimals; else 2-digit trimmed.
                    BasicText {
                        visible: (rowData.framerate || 0) > 0;
                        font.pixelSize: root.basicTextSize;
                        textFormat: Text.RichText;
                        text: {
                            const fps = rowData.framerate || 0;
                            if (fps === 0) return "";
                            if (fps % 1 === 0) return "<b>" + fps.toFixed(0) + "fps</b>";
                            return "<b>" + fps.toFixed(2).replace(/\.?0+$/, "") + "fps</b>";
                        }
                    }
                    BasicText {
                        visible: (rowData.focal_mm || 0) > 0;
                        font.pixelSize: root.basicTextSize;
                        textFormat: Text.RichText;
                        text: "<b>" + Math.round(rowData.focal_mm || 0) + "mm</b>";
                    }
                }
                // Deep-match annotation (line 2220-2242, deep branch) — only on the
                // deep-matched anchor row (rowData.deep), after the deep-search step.
                // Auto-matched rows (deep=false) show no gyro marker, just the sync line.
                BasicText {
                    visible: root.matched && !!root.rowData.deep;
                    width: parent.width;
                    wrapMode: Text.WordWrap;
                    color: root.deepMatchStatusColor;
                    font.pixelSize: root.basicTextSize;
                    font.bold: true;
                    text: qsTranslate("RenderQueue", "Deep") + " ⚡ " + (rowData.gyro_filename || "");
                }
                // Sync status (line 2243-2253) — only after export (green).
                BasicText {
                    visible: root.synced;
                    text: qsTranslate("RenderQueue", "Sync confirmed");
                    color: root.finishedStatusColor;
                    font.pixelSize: root.basicTextSize;
                    font.bold: true;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // REPLICA CONTEXT MENU PANEL
    // Static dropdown-look panel (NOT a real Menu.popup() — popups misbehave
    // under the tutorial's input-blocking overlay). Mirrors the contextMenu
    // item order/labels (RenderQueue.qml 1504-1628).
    // -----------------------------------------------------------------------
    Rectangle {
        id: menuPanel;
        visible: root.menuShown;
        // Float above sibling rows stacked below this one in the tutorial Column.
        z: 50;
        anchors.top: rowRect.bottom;
        anchors.topMargin: 4 * dpiScale;
        anchors.right: rowRect.right;
        // On the deep step the submenu opens to the RIGHT of this menu; shift the whole
        // menu left by (submenu width + gap) so the menu+submenu group stays right-aligned
        // to the row and the submenu does not overflow off-screen. Other steps: glued right.
        anchors.rightMargin: (root.menuShown && root.highlightItem === "deep")
                             ? (8 * dpiScale + root.deepSubmenuWidth + 4 * dpiScale)
                             : 8 * dpiScale;
        width: 220 * dpiScale;
        height: menuCol.implicitHeight + 6 * dpiScale;
        color: "transparent";
        radius: 6 * dpiScale;

        // Soft shadow via a slightly-larger translucent rect (no banned effects).
        Rectangle {
            anchors.fill: parent;
            anchors.margins: -2 * dpiScale;
            anchors.topMargin: 1 * dpiScale;
            color: "#33000000";
            radius: 8 * dpiScale;
            z: -1;
        }
        Rectangle {
            anchors.fill: parent;
            radius: 6 * dpiScale;
            color: root.lightTheme ? "#f5f5f5" : "#2a2a2a";
            border.width: 1 * dpiScale;
            border.color: root.lightTheme ? "#dddddd" : "#4a4a4a";

            Column {
                id: menuCol;
                anchors.left: parent.left;
                anchors.right: parent.right;
                anchors.top: parent.top;
                anchors.topMargin: 3 * dpiScale;
                spacing: 0;

                component MenuItemRow: Rectangle {
                    property string label: "";
                    property bool highlighted: false;
                    width: parent ? parent.width : 0;
                    height: 27 * dpiScale;
                    color: highlighted ? root.withAlpha(root.accentColorC, 0.18) : "transparent";
                    Text {
                        anchors.verticalCenter: parent.verticalCenter;
                        anchors.left: parent.left;
                        anchors.right: parent.right;
                        leftPadding: 8 * dpiScale;
                        rightPadding: 8 * dpiScale;
                        text: parent.label;
                        font.pixelSize: 11.5 * dpiScale;
                        font.family: styleFont;
                        color: parent.highlighted ? root.accentColorC : root.textColorC;
                        elide: Text.ElideRight;
                    }
                }
                component MenuSep: Rectangle {
                    width: parent ? parent.width : 0;
                    height: 1 * dpiScale;
                    color: root.lightTheme ? "#e0e0e0" : "#3a3a3a";
                    opacity: 0.6;
                }

                MenuItemRow { label: qsTranslate("RenderQueue", "Render now"); }
                MenuSep {}
                MenuItemRow { label: qsTranslate("RenderQueue", "Edit"); highlighted: root.highlightItem === "edit"; }
                MenuSep {}
                MenuItemRow { label: qsTranslate("RenderQueue", "Change framerate"); }
                MenuSep {}
                MenuItemRow { id: deepMatchRow; label: qsTranslate("RenderQueue", "Deep match with gyro") + "  ▸"; highlighted: root.highlightItem === "deep"; }
                Item { width: 1; height: 3 * dpiScale; }
            }
        }

        // Deep-match SUBMENU (deep step only). Real deep search is two-level: right-click
        // -> "Deep match with gyro ▸" -> a submenu lists the gyro file(s), and you click
        // the file to start the match. Faithfully replicate that: hang a one-entry submenu
        // off the parent item, opening to the RIGHT (like the real menu). The parent menu is
        // shifted left by (deepSubmenuWidth + gap) on this step (see rightMargin above) so
        // this right-opening submenu's right edge lands at the row's right edge and stays
        // on-screen. The gyro-file entry is THIS step's click target (accent-highlighted);
        // the parent shows as the active/open path. Label mirrors
        // RenderQueue.qml::deepMatchSubMenu: filename + " (" + (duration_ms/1000).toFixed(1) + "s)".
        Rectangle {
            id: deepSubmenu;
            visible: root.menuShown && root.highlightItem === "deep";
            z: 60;
            width: root.deepSubmenuWidth;
            height: subEntry.implicitHeight + 6 * dpiScale;
            // Align vertically with the parent "Deep match" row; open to the RIGHT.
            x: parent.width + 4 * dpiScale;
            y: menuCol.y + deepMatchRow.y;
            color: "transparent";
            radius: 6 * dpiScale;

            // Soft shadow (same technique as the parent menu — no banned effects).
            Rectangle {
                anchors.fill: parent;
                anchors.margins: -2 * dpiScale;
                anchors.topMargin: 1 * dpiScale;
                color: "#33000000";
                radius: 8 * dpiScale;
                z: -1;
            }
            Rectangle {
                anchors.fill: parent;
                radius: 6 * dpiScale;
                color: root.lightTheme ? "#f5f5f5" : "#2a2a2a";
                border.width: 1 * dpiScale;
                border.color: root.lightTheme ? "#dddddd" : "#4a4a4a";

                Rectangle {
                    id: subEntry;
                    x: 3 * dpiScale;
                    y: 3 * dpiScale;
                    width: parent.width - 6 * dpiScale;
                    implicitHeight: 27 * dpiScale;
                    height: implicitHeight;
                    radius: 4 * dpiScale;
                    // The gyro-file entry is the click target -> accent highlight.
                    color: root.withAlpha(root.accentColorC, 0.18);
                    Text {
                        anchors.verticalCenter: parent.verticalCenter;
                        anchors.left: parent.left;
                        anchors.right: parent.right;
                        leftPadding: 8 * dpiScale;
                        rightPadding: 8 * dpiScale;
                        text: (root.rowData.gyro_filename || "") +
                              (root.rowData.gyro_duration_ms ? " (" + (root.rowData.gyro_duration_ms / 1000).toFixed(1) + "s)" : "");
                        font.pixelSize: 11.5 * dpiScale;
                        font.family: styleFont;
                        color: root.accentColorC;
                        elide: Text.ElideRight;
                    }
                }
            }
        }
    }
}
