// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

import QtQuick
import QtQuick.Controls as QQC

// First-launch onboarding overlay. Driven by `steps` + `index`. This Task-1
// version renders every step as a centered explanatory card; Task 2 adds the
// spotlight hole for steps that have a real target control.
Item {
    id: root;
    anchors.fill: parent;
    z: 5000;
    visible: active;

    // Each step: { target, section, title, body }
    //   target  - control to spotlight (Task 2), or null for a centered card.
    //   section - optional MenuItem to expand before anchoring (Task 2).
    //   title/body - already-translated strings.
    property var steps: [];
    property int index: 0;
    property bool active: false;
    // SidePanel whose internal Flickable holds the simple-mode cards (Task 2).
    property var scrollPanel: null;

    readonly property var currentStep: (active && index >= 0 && index < steps.length) ? steps[index] : null;
    readonly property bool isLast: index >= steps.length - 1;
    readonly property bool isFirst: index <= 0;

    signal closed(bool completed);

    // No `: void` annotations — Qt 6.7.3 V4 JIT miscompiles the error path of
    // void-returning JS functions reached via Qt.callLater (project_qt67_v4_jit_void_av).
    function start()  { if (!steps || steps.length === 0) return; index = 0; active = true; }
    function next()   { if (isLast) { finish(); } else { index = index + 1; } }
    function prev()   { if (!isFirst) index = index - 1; }
    function finish() { active = false; closed(true); }
    function skip()   { active = false; closed(false); }

    // Block all interaction with the UI underneath.
    MouseArea {
        anchors.fill: parent;
        hoverEnabled: true;
        preventStealing: true;
        acceptedButtons: Qt.AllButtons;
        onClicked: (mouse) => { mouse.accepted = true; }
        onWheel: (wheel) => { wheel.accepted = true; }
    }

    // Full-screen dim.
    Rectangle { anchors.fill: parent; color: "#9A000000"; }

    // Explanatory card.
    Rectangle {
        id: card;
        width: Math.min(root.width * 0.5, 460 * dpiScale);
        height: cardCol.height + 28 * dpiScale;
        anchors.centerIn: parent;
        color: styleBackground2;
        radius: 8 * dpiScale;
        border.color: styleHrColor;
        border.width: Math.max(1, 1 * dpiScale);

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
