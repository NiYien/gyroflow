// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

import QtQuick

// First-launch onboarding overlay. Driven by `steps` + `index`. Each step
// either spotlights a real control (4 dim bands around a hole + a bubble) or
// shows a centered explanatory card (target == null / not currently visible).
Item {
    id: root;
    anchors.fill: parent;
    z: 5000;
    visible: active;

    // Each step: { target, section, title, body }
    //   target  - control to spotlight, or null for a centered card.
    //   section - optional MenuItem to expand (opened=true) before anchoring.
    //   title/body - already-translated strings.
    property var steps: [];
    property int index: 0;
    property bool active: false;
    // SidePanel whose internal Flickable holds the simple-mode cards
    // (set to rightPanel from App.qml). Used to scroll a target into view.
    property var scrollPanel: null;

    // Spotlight hole geometry (root-local). holeActive=false => centered card.
    property bool holeActive: false;
    property real holeX: 0;
    property real holeY: 0;
    property real holeW: 0;
    property real holeH: 0;

    readonly property var currentStep: (active && index >= 0 && index < steps.length) ? steps[index] : null;
    readonly property bool isLast: index >= steps.length - 1;
    readonly property bool isFirst: index <= 0;

    signal closed(bool completed);

    // No `: void` annotations — Qt 6.7.3 V4 JIT miscompiles the error path of
    // void-returning JS functions reached via Qt.callLater (project_qt67_v4_jit_void_av).
    function start()  { if (!steps || steps.length === 0) return; index = 0; active = true; prepareStep(); }
    function next()   { if (isLast) { finish(); } else { index = index + 1; } }
    function prev()   { if (!isFirst) index = index - 1; }
    function finish() { active = false; closed(true); }
    function skip()   { active = false; closed(false); }

    function resolveTarget() {
        var s = currentStep;
        if (!s) return null;
        var t = s.target;
        if (typeof t === "function") t = t();
        return t || null;
    }

    function scrollIntoView(t) {
        if (!scrollPanel || !t) return;
        var col = scrollPanel.col;
        if (!col) return;
        var flick = col.parent;
        if (!flick || typeof flick.contentY !== "number") return;
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
        var s = currentStep;
        if (!s) return;
        if (s.section) s.section.opened = true;
        var t = resolveTarget();
        if (t && t.visible) scrollIntoView(t);
        recomputePlacement();
        settleTimer.restart(); // re-place after expand/scroll animations settle
    }

    onIndexChanged: if (active) prepareStep();
    onActiveChanged: if (active) prepareStep();

    Timer {
        id: settleTimer;
        interval: 750; // > MenuItem expand animation (700ms)
        onTriggered: {
            var t = root.resolveTarget();
            if (t && t.visible) root.scrollIntoView(t);
            root.recomputePlacement();
        }
    }
    Connections {
        target: window;
        function onWidthChanged()  { if (root.active) root.recomputePlacement(); }
        function onHeightChanged() { if (root.active) root.recomputePlacement(); }
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
            x: root.holeX - 2 * dpiScale; y: root.holeY - 2 * dpiScale;
            width: root.holeW + 4 * dpiScale; height: root.holeH + 4 * dpiScale;
            color: "transparent";
            radius: 8 * dpiScale;
            border.color: styleAccentColor;
            border.width: 2 * dpiScale;
        }
    }

    // Explanatory card / bubble.
    Rectangle {
        id: card;
        width: Math.min(root.width * 0.5, 460 * dpiScale);
        height: cardCol.height + 28 * dpiScale;
        color: styleBackground2;
        radius: 8 * dpiScale;
        border.color: styleHrColor;
        border.width: Math.max(1, 1 * dpiScale);

        // Centered when no hole; otherwise below the hole if there is room,
        // else above, else screen-centered.
        property real gap: 16 * dpiScale;
        x: !root.holeActive
           ? (root.width - width) / 2
           : Math.max(10 * dpiScale, Math.min(root.width - width - 10 * dpiScale, root.holeX + root.holeW / 2 - width / 2));
        y: {
            if (!root.holeActive) return (root.height - height) / 2;
            if (root.holeY + root.holeH + gap + height < root.height) return root.holeY + root.holeH + gap;
            if (root.holeY - gap - height > 0) return root.holeY - gap - height;
            return (root.height - height) / 2;
        }

        Column {
            id: cardCol;
            x: 18 * dpiScale;
            y: 14 * dpiScale;
            width: parent.width - 36 * dpiScale;
            spacing: 10 * dpiScale;
            BasicText {
                width: parent.width;
                text: root.currentStep ? root.currentStep.title : "";
                font.bold: true;
                font.pixelSize: 16 * dpiScale;
                wrapMode: Text.WordWrap;
            }
            BasicText {
                width: parent.width;
                text: root.currentStep ? root.currentStep.body : "";
                font.pixelSize: 13 * dpiScale;
                opacity: 0.85;
                wrapMode: Text.WordWrap;
            }
            Row {
                anchors.horizontalCenter: parent.horizontalCenter;
                spacing: 6 * dpiScale;
                Repeater {
                    model: root.steps.length;
                    Rectangle {
                        width: 7 * dpiScale; height: 7 * dpiScale; radius: width / 2;
                        color: index === root.index ? styleAccentColor : styleHrColor;
                    }
                }
            }
            Row {
                anchors.horizontalCenter: parent.horizontalCenter;
                spacing: 10 * dpiScale;
                Button { text: qsTr("Skip"); onClicked: root.skip(); }
                Button { text: qsTr("Back"); enabled: !root.isFirst; onClicked: root.prev(); }
                Button { text: root.isLast ? qsTr("Done") : qsTr("Next"); accent: true; onClicked: root.next(); }
            }
        }
    }
}
