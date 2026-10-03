// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

import QtQuick
import "../mobile" as Mobile

// Vertical two-option switch: both option labels are always visible, stacked
// vertically, and the knob points at the active one. Clicking a label selects
// that option (radio-like), clicking the track toggles.
Item {
    id: sw;
    readonly property bool mobileStyle: typeof window !== "undefined" && window.useMobileWorkspace === true
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
        visible: !sw.mobileStyle;
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
        anchors.left: sw.mobileStyle ? parent.left : track.right;
        anchors.leftMargin: sw.mobileStyle ? 0 : 8 * dpiScale;
        anchors.right: parent.right;

        Text {
            width: parent.width;
            height: sw.mobileStyle ? Math.max(48 * dpiScale, implicitHeight + 16 * dpiScale) : 24 * dpiScale;
            text: sw.textOff;
            font.pixelSize: (sw.mobileStyle ? Mobile.MobileStyle.body : 13) * dpiScale;
            font.family: sw.mobileStyle ? Mobile.MobileStyle.fontFamily : styleFont;
            font.bold: !sw.mobileStyle && !sw.checked;
            color: styleTextColor;
            opacity: sw.mobileStyle ? 1 : sw.checked? 0.45 : 1.0;
            rightPadding: sw.mobileStyle ? 32 * dpiScale : 0;
            wrapMode: sw.mobileStyle ? Text.WordWrap : Text.NoWrap;
            Mobile.MobileIcon { visible: sw.mobileStyle && !sw.checked; name: "check"; color: Mobile.MobileStyle.accent(style === "dark"); width: 22 * dpiScale; height: width; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter }
            Rectangle { visible: sw.mobileStyle; width: parent.width; height: 0.5 * dpiScale; anchors.bottom: parent.bottom; color: Mobile.MobileStyle.separator(style === "dark") }
            Ease on opacity { duration: 300; }
            elide: Text.ElideRight;
            verticalAlignment: Text.AlignVCenter;
            MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: sw.checked = false; }
        }
        Text {
            width: parent.width;
            height: sw.mobileStyle ? Math.max(48 * dpiScale, implicitHeight + 16 * dpiScale) : 24 * dpiScale;
            text: sw.textOn;
            font.pixelSize: (sw.mobileStyle ? Mobile.MobileStyle.body : 13) * dpiScale;
            font.family: sw.mobileStyle ? Mobile.MobileStyle.fontFamily : styleFont;
            font.bold: !sw.mobileStyle && sw.checked;
            color: styleTextColor;
            opacity: sw.mobileStyle ? 1 : sw.checked? 1.0 : 0.45;
            rightPadding: sw.mobileStyle ? 32 * dpiScale : 0;
            wrapMode: sw.mobileStyle ? Text.WordWrap : Text.NoWrap;
            Mobile.MobileIcon { visible: sw.mobileStyle && sw.checked; name: "check"; color: Mobile.MobileStyle.accent(style === "dark"); width: 22 * dpiScale; height: width; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter }
            Ease on opacity { duration: 300; }
            elide: Text.ElideRight;
            verticalAlignment: Text.AlignVCenter;
            MouseArea { anchors.fill: parent; cursorShape: Qt.PointingHandCursor; onClicked: sw.checked = true; }
        }
    }

    MouseArea {
        id: hoverArea;
        visible: !sw.mobileStyle;
        anchors.fill: track;
        hoverEnabled: true;
        cursorShape: Qt.PointingHandCursor;
        onClicked: sw.checked = !sw.checked;
    }

    ToolTip { id: tt; visible: !isMobile && text.length > 0 && hoverArea.containsMouse; }
}
