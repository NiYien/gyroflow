// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

import QtQuick

// Vertical two-option switch: both option labels are always visible, stacked
// vertically, and the knob points at the active one. Clicking a label selects
// that option (radio-like), clicking the track toggles.
Item {
    id: sw;
    property bool checked: false;
    property string textOff; // top option, active when checked == false
    property string textOn;  // bottom option, active when checked == true
    property alias tooltip: tt.text;

    implicitHeight: rows.implicitHeight;
    opacity: enabled? 1.0 : 0.5;

    activeFocusOnTab: true;
    Keys.onPressed: (e) => {
        if (e.key == Qt.Key_Enter || e.key == Qt.Key_Return || e.key == Qt.Key_Space) {
            checked = !checked;
        }
    }

    Rectangle {
        id: track;
        width: 20 * dpiScale;
        height: rows.height;
        radius: width / 2;
        // Outline style like the unchecked CheckBox/RadioButton — a filled
        // styleSliderBackground capsule is too bright on the dark theme.
        color: "transparent";
        border.width: 1 * dpiScale;
        border.color: "#999999";
        opacity: sw.activeFocus? 0.8 : 1.0;
        Ease on opacity { }

        Rectangle {
            id: knob;
            width: 16 * dpiScale;
            height: width;
            radius: width;
            x: (parent.width - width) / 2;
            y: sw.checked? parent.height - height - 2 * dpiScale : 2 * dpiScale;
            Behavior on y { NumberAnimation { duration: 300; easing.type: Easing.OutExpo; } }
            color: styleSliderHandle;
            Rectangle {
                radius: width;
                height: parent.height * 0.7;
                width: height;
                scale: hoverArea.pressed? 1.1 : hoverArea.containsMouse? 0.9 : 1.0;
                Ease on scale { duration: 200; }
                anchors.centerIn: parent;
                color: styleAccentColor;
            }
        }
    }

    Column {
        id: rows;
        anchors.left: track.right;
        anchors.leftMargin: 8 * dpiScale;
        anchors.right: parent.right;

        Text {
            width: parent.width;
            height: 24 * dpiScale;
            text: sw.textOff;
            font.pixelSize: 13 * dpiScale;
            font.family: styleFont;
            font.bold: !sw.checked;
            color: styleTextColor;
            opacity: sw.checked? 0.45 : 1.0;
            Ease on opacity { duration: 300; }
            elide: Text.ElideRight;
            verticalAlignment: Text.AlignVCenter;
            MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: sw.checked = false; }
        }
        Text {
            width: parent.width;
            height: 24 * dpiScale;
            text: sw.textOn;
            font.pixelSize: 13 * dpiScale;
            font.family: styleFont;
            font.bold: sw.checked;
            color: styleTextColor;
            opacity: sw.checked? 1.0 : 0.45;
            Ease on opacity { duration: 300; }
            elide: Text.ElideRight;
            verticalAlignment: Text.AlignVCenter;
            MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: sw.checked = true; }
        }
    }

    MouseArea {
        id: hoverArea;
        anchors.fill: track;
        hoverEnabled: true;
        cursorShape: Qt.PointingHandCursor;
        onClicked: sw.checked = !sw.checked;
    }

    ToolTip { id: tt; visible: !isMobile && text.length > 0 && hoverArea.containsMouse; }
}
