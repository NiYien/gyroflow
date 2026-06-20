// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

import QtQuick

// First-launch onboarding overlay. Driven by `steps` + `index`. Each step
// either spotlights a real control (4 dim bands around a hole + a card) or
// shows a centered explanatory card (target == null / not currently visible).
Item {
    id: root;
    anchors.fill: parent;
    z: 5000;
    visible: active;

    // Each step: { target, section, openQueue, title, body, note, image, imageAnchor }
    //   target      - control to spotlight, or null for a centered card.
    //   section     - optional MenuItem to expand (opened=true) before anchoring.
    //   openQueue   - optional: open the render queue panel before anchoring.
    //   closeQueue  - optional: close the render queue panel (the last step returns
    //                 to the main preview where the stabilization-preview button lives).
    //   title/body  - already-translated strings.
    //   note        - optional smaller, greyer caption shown under the body.
    //   image       - optional qrc path for a canvas-anchored visual (NOT inside
    //                 the card); empty string => not rendered.
    //   imageAnchor - "queue" | "preview"; where the canvas visual is anchored.
    property var steps: [];
    property int index: 0;
    property bool active: false;
    // SidePanel whose internal Flickable holds the simple-mode cards
    // (set to rightPanel from App.qml). Used to scroll a target into view.
    property var scrollPanel: null;

    // styleTextColor is injected from Rust as a QString ("#ffffff"), not a QColor.
    // Declaring a `property color` coerces it to a real colour so .r/.g/.b resolve
    // (reading them off the raw string yields undefined -> Qt.rgba collapses to
    // black, which disappears on the dark card surface).
    readonly property color textColorC: styleTextColor;

    // Mock data for the persistent simulated queue row (set from App.qml).
    property var queueRowMock: [];   // array of row mock objects (see App.qml)

    // Spotlight hole geometry (root-local). holeActive=false => centered card.
    property bool holeActive: false;
    property real holeX: 0;
    property real holeY: 0;
    property real holeW: 0;
    property real holeH: 0;

    readonly property var currentStep: (active && index >= 0 && index < steps.length) ? steps[index] : null;
    readonly property bool isLast: index >= steps.length - 1;
    readonly property bool isFirst: index <= 0;
    // True on steps that demonstrate a right-click action (carry `menuHighlight`):
    // the simulated queue row shows its dropdown menu with that item highlighted.
    readonly property bool simMenuShown: !!(currentStep && currentStep.menuHighlight);

    signal closed(bool completed);

    // No `: void` annotations — Qt 6.7.3 V4 JIT miscompiles the error path of
    // void-returning JS functions reached via Qt.callLater (project_qt67_v4_jit_void_av).
    function start()  { if (!steps || steps.length === 0) return; index = 0; active = true; prepareStep(); }
    function next()   { if (isLast) { finish(); } else { index = index + 1; } }
    function prev()   { if (!isFirst) index = index - 1; }
    function finish() { active = false; closed(true); }
    function skip()   { active = false; closed(false); }

    // Read the step straight from the array by index. prepareStep() runs inside
    // onIndexChanged, where the `currentStep` binding has NOT re-evaluated yet, so
    // reading it there returns the PREVIOUS step (off-by-one). Indexing directly is
    // always fresh.
    function stepAt(i) { return (i >= 0 && i < steps.length) ? steps[i] : null; }
    function resolveTarget() {
        var s = stepAt(index);
        if (!s) return null;
        var t = s.target;
        if (typeof t === "function") t = t();
        return t || null;
    }

    function scrollIntoView(t) {
        if (!scrollPanel || !scrollPanel.col || !t) return;
        var col = scrollPanel.col;
        // Only scroll when the target actually lives inside the panel's column.
        // Bottom-bar / preview targets (Load video, Render queue, Export) are always
        // visible and must NOT move the panel.
        var inPanel = false, p = t;
        while (p) { if (p === col) { inPanel = true; break; } p = p.parent; }
        if (!inPanel) return;
        // Flickable puts its declared children into contentItem, so col.parent is that
        // contentItem (no contentY), not the Flickable. Walk up to the item that scrolls.
        var flick = col.parent;
        while (flick && typeof flick.contentY !== "number") flick = flick.parent;
        if (!flick) return;
        var yInCol = t.mapToItem(col, 0, 0).y;
        var margin = 40 * dpiScale;
        var maxY = Math.max(0, flick.contentHeight - flick.height);
        flick.contentY = Math.max(0, Math.min(maxY, yInCol - margin));
    }

    function recomputePlacement() {
        var t = resolveTarget();
        if (!t || !t.visible || t.width <= 0 || t.height <= 0) {
            holeActive = false;
            return;
        }
        var p = t.mapToItem(root, 0, 0);
        holeX = p.x; holeY = p.y; holeW = t.width; holeH = t.height;
        holeActive = true;
    }

    function prepareStep() {
        holeActive = false;
        var s = stepAt(index);
        if (!s) return;
        // Reveal the target: open the queue panel and/or expand its collapsed section.
        if (s.openQueue && window.videoArea && window.videoArea.queue) window.videoArea.queue.shown = true;
        else if (s.closeQueue && window.videoArea && window.videoArea.queue) window.videoArea.queue.shown = false;
        if (s.section) s.section.opened = true;
        var t = resolveTarget();
        // Scroll even if the target reads !visible right now — a just-expanded section
        // animates its content opacity, so visibility settles a frame later; geometry
        // (mapToItem) is already valid for scrolling.
        if (t) scrollIntoView(t);
        recomputePlacement();
    }

    onIndexChanged: if (active) prepareStep();
    onActiveChanged: if (active) prepareStep();

    // Continuously re-place the hole while the tour is open so it glues to the target
    // through scroll / section-expand / queue-open animations and window resizes —
    // mapToItem is not reactive, so a per-frame recompute keeps the highlight in sync
    // (replaces the old one-shot settle timer that caused a ~0.75s lag/jump).
    Timer {
        interval: 16;
        repeat: true;
        running: root.active;
        onTriggered: root.recomputePlacement();
    }

    // Block all interaction with the UI underneath.
    MouseArea {
        anchors.fill: parent;
        hoverEnabled: true;
        preventStealing: true;
        acceptedButtons: Qt.AllButtons;
        onClicked: (mouse) => { mouse.accepted = true; }
        onWheel: (wheel) => { wheel.accepted = true; }
    }

    // Full-screen dim (centered-card mode).
    Rectangle {
        anchors.fill: parent;
        visible: !root.holeActive;
        color: "#9A000000";
    }

    // Four dim bands around the hole (spotlight mode).
    Item {
        anchors.fill: parent;
        visible: root.holeActive;
        Rectangle { color: "#9A000000"; x: 0; y: 0; width: parent.width; height: Math.max(0, root.holeY); }
        Rectangle { color: "#9A000000"; x: 0; y: root.holeY + root.holeH; width: parent.width; height: Math.max(0, parent.height - (root.holeY + root.holeH)); }
        Rectangle { color: "#9A000000"; x: 0; y: root.holeY; width: Math.max(0, root.holeX); height: Math.max(0, root.holeH); }
        Rectangle { color: "#9A000000"; x: root.holeX + root.holeW; y: root.holeY; width: Math.max(0, parent.width - (root.holeX + root.holeW)); height: Math.max(0, root.holeH); }
        Rectangle {
            // Clamp the accent border into the overlay so an edge-flush target (e.g. step
            // 1's previewArea, whose left/top map to holeX/holeY ≈ 0) still shows all four
            // sides. Drawing unconditionally at holeX-2 / holeY-2 pushes the left/top lines
            // to ≈ -2px, where the window clips them — the bug where the blue box lost its
            // left and top edges. Right/bottom are unaffected (positive, on-screen).
            readonly property real bx: Math.max(0, root.holeX - 2 * dpiScale);
            readonly property real by: Math.max(0, root.holeY - 2 * dpiScale);
            x: bx; y: by;
            width:  Math.min(parent.width,  root.holeX + root.holeW + 2 * dpiScale) - bx;
            height: Math.min(parent.height, root.holeY + root.holeH + 2 * dpiScale) - by;
            color: "transparent";
            radius: 8 * dpiScale;
            border.color: styleAccentColor;
            border.width: 2 * dpiScale;
        }
    }

    // ---- Persistent simulated queue row. ----
    // Appears whenever the render queue panel is shown during the tutorial (step 2
    // onward) and stays until it closes (step 7) — representing the (simulated) loaded
    // video, so every queue-visible step shows it without flicker. The replica
    // right-click menu only appears on steps that demonstrate a click (4/6).
    Item {
        id: queueRowLayer;
        anchors.fill: parent;

        readonly property var q: (window.videoArea && window.videoArea.queue) ? window.videoArea.queue : null;
        // The render queue's root binds `visible: opacity > 0`, so this tracks open/close.
        readonly property bool queueShown: root.active && q && q.visible && q.width > 0 && q.height > 0;
        property real refX: 0;
        property real refY: 0;
        property real refW: 0;

        // mapToItem is not reactive — recompute the queue rect each frame while shown.
        function recompute() {
            if (queueShown) {
                var p = q.mapToItem(root, 0, 0);
                refX = p.x; refY = p.y; refW = q.width;
            }
        }
        Timer { interval: 16; repeat: true; running: queueRowLayer.queueShown; onTriggered: queueRowLayer.recompute(); }
        onQueueShownChanged: if (queueShown) recompute();

        visible: queueShown;

        // Three stacked replica rows (root.queueRowMock is an array). Row 0 is the
        // deep-matched anchor; rows 1-2 are auto-matched siblings of the same gyro.
        Column {
            id: tutorialRows;
            visible: queueRowLayer.queueShown && queueRowLayer.refW > 0;
            x: queueRowLayer.refX + 8 * dpiScale;
            // Below the queue header (title + progress row), where the first real row sits.
            y: queueRowLayer.refY + 92 * dpiScale;
            spacing: 4 * dpiScale;
            Repeater {
                model: root.queueRowMock;
                TutorialQueueRow {
                    rowData: modelData;
                    width: implicitWidth;
                    height: implicitHeight;
                    // Full queue width so the params Flow wraps like a real row.
                    rowWidth: Math.max(220 * dpiScale, queueRowLayer.refW - 16 * dpiScale);
                    // Evolving row state: matched after the deep-search step, synced after export.
                    matched: !!(root.currentStep && root.currentStep.rowMatched);
                    synced: !!(root.currentStep && root.currentStep.rowSynced);
                    // The replica right-click menu only hangs off the first (deep-matched)
                    // anchor row, and only on steps that demonstrate a right-click action.
                    menuShown: index === 0 && root.simMenuShown;
                    highlightItem: (index === 0 && root.simMenuShown) ? root.currentStep.menuHighlight : "";
                    // First row stacks above the others so its floating menu overlays them.
                    z: index === 0 ? 10 : 0;
                }
            }
        }
    }

    // ---- Lean isolated explanatory card. ----
    // The screenshot/diagram is NOT inside the card anymore (see queueRowLayer);
    // the card stays a slim text panel that clearly floats above the chrome via a
    // large soft shadow + an elevated surface + a 4px accent top bar.
    Item {
        id: cardWrap;
        width: cardSurface.width;
        height: cardSurface.height;

        // Centered when no hole; otherwise below the hole if there is room,
        // else above, else screen-centered.
        property real gap: 16 * dpiScale;
        x: !root.holeActive
           ? (root.width - width) / 2
           : Math.max(10 * dpiScale, Math.min(root.width - width - 10 * dpiScale, root.holeX + root.holeW / 2 - width / 2));
        y: {
            // Steps that show the simulated dropdown menu (4/6): the row + menu (+submenu)
            // sit in the upper part of the queue region. Center the card in the empty band
            // BELOW the menu instead of gluing it to the screen bottom (which read as "too
            // low"). Estimate the menu region bottom from the queue-row-layer geometry:
            //   rows top (refY + 92) + row0 height (~86) + menu gap (4) + menu/submenu (~150).
            if (!root.holeActive && root.simMenuShown) {
                var q = queueRowLayer;
                var menuBottom = (q && q.queueShown)
                    ? q.refY + (92 + 86 + 4 + 150) * dpiScale
                    : root.height * 0.40;
                var bandTop = menuBottom + gap;
                var maxY = root.height - height - 16 * dpiScale;
                var yMid = (bandTop + maxY) / 2;          // midpoint of the band below the menu
                return Math.min(maxY, Math.max(bandTop, yMid));
            }
            if (!root.holeActive) return (root.height - height) / 2;
            if (root.holeY + root.holeH + gap + height < root.height) return root.holeY + root.holeH + gap;
            if (root.holeY - gap - height > 0) return root.holeY - gap - height;
            return (root.height - height) / 2;
        }

        // Large soft drop shadow faked with stacked translucent rounded rects,
        // each slightly larger and offset down with a low black alpha. No shader
        // effects (MultiEffect / Qt5Compat DropShadow crash on Qt 6.7.3 here).
        Repeater {
            model: 4;
            Rectangle {
                property real grow: (index + 1) * 5 * dpiScale;
                x: cardSurface.x - grow;
                y: cardSurface.y - grow + 10 * dpiScale;
                width: cardSurface.width + grow * 2;
                height: cardSurface.height + grow * 2;
                radius: cardSurface.radius + grow;
                color: "#000000";
                opacity: 0.11;
            }
        }

        // Elevated card surface — deliberately brighter than styleBackground2 so it
        // reads as a floating layer, and no styleHrColor border (the shadow separates it).
        Rectangle {
            id: cardSurface;
            width: 360 * dpiScale;
            height: cardCol.height + 48 * dpiScale;
            radius: 16 * dpiScale;
            color: Qt.lighter(styleBackground2, 1.2);

            // 4px accent top bar — rounded top corners, square bottom. Built as an
            // accent rect masked at the bottom by a thin surface-coloured strip so the
            // accent only shows as a 4px band hugging the rounded top.
            Rectangle {
                id: accentBar;
                x: 0; y: 0;
                width: parent.width;
                height: 12 * dpiScale;
                radius: cardSurface.radius;
                color: styleAccentColor;
                Rectangle {
                    // Cover everything below the top 4px so only a 4px accent band remains.
                    x: 0; y: 4 * dpiScale;
                    width: parent.width;
                    height: parent.height - 4 * dpiScale;
                    color: cardSurface.color;
                }
            }

            Column {
                id: cardCol;
                x: 24 * dpiScale;
                y: 24 * dpiScale;
                width: parent.width - 48 * dpiScale;
                spacing: 12 * dpiScale;

                // Step counter — accent + bold so progress is obvious at a glance.
                BasicText {
                    leftPadding: 0;
                    width: parent.width;
                    text: qsTr("Step %1 of %2").arg(root.index + 1).arg(root.steps.length);
                    font.pixelSize: 13 * dpiScale;
                    font.bold: true;
                    color: styleAccentColor;
                }
                // Title — large and bold.
                BasicText {
                    leftPadding: 0;
                    width: parent.width;
                    text: root.currentStep ? root.currentStep.title : "";
                    font.bold: true;
                    font.pixelSize: 20 * dpiScale;
                    wrapMode: Text.WordWrap;
                }
                // Body — comfortable size with relaxed line height.
                BasicText {
                    leftPadding: 0;
                    width: parent.width;
                    text: root.currentStep ? root.currentStep.body : "";
                    font.pixelSize: 15 * dpiScale;
                    lineHeight: 1.5;
                    lineHeightMode: Text.ProportionalHeight;
                    opacity: 0.9;
                    wrapMode: Text.WordWrap;
                }
                // Optional caption (note) — only when the step provides one.
                BasicText {
                    leftPadding: 0;
                    width: parent.width;
                    text: (root.currentStep && root.currentStep.note) ? root.currentStep.note : "";
                    visible: text != "";
                    font.pixelSize: 12.5 * dpiScale;
                    lineHeight: 1.4;
                    lineHeightMode: Text.ProportionalHeight;
                    opacity: 0.6;
                    wrapMode: Text.WordWrap;
                }

                // Progress dots — equal round dots; the active step is an accent
                // dot, the others are dimmed text-colour dots (no wide pill).
                Row {
                    anchors.horizontalCenter: parent.horizontalCenter;
                    topPadding: 4 * dpiScale;
                    spacing: 7 * dpiScale;
                    Repeater {
                        model: root.steps.length;
                        Rectangle {
                            width: 8 * dpiScale;
                            height: 8 * dpiScale;
                            radius: width / 2;
                            color: index === root.index ? styleAccentColor : Qt.rgba(root.textColorC.r, root.textColorC.g, root.textColorC.b, 0.35);
                        }
                    }
                }

                // Navigation: Skip on the left (quiet link), Back + Next/Done on the right.
                Item {
                    width: parent.width;
                    height: nextBtn.height;
                    LinkButton {
                        anchors.left: parent.left;
                        anchors.verticalCenter: parent.verticalCenter;
                        leftPadding: 0;
                        text: qsTr("Skip");
                        textColor: Qt.rgba(root.textColorC.r, root.textColorC.g, root.textColorC.b, 0.6);
                        transparent: true;
                        onClicked: root.skip();
                    }
                    Row {
                        anchors.right: parent.right;
                        anchors.verticalCenter: parent.verticalCenter;
                        spacing: 8 * dpiScale;
                        Button {
                            text: qsTr("Back");
                            enabled: !root.isFirst;
                            onClicked: root.prev();
                        }
                        Button {
                            id: nextBtn;
                            text: root.isLast ? qsTr("Done") : qsTr("Next");
                            accent: true;
                            onClicked: root.next();
                        }
                    }
                }
            }
        }
    }
}
