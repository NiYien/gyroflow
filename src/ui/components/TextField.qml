// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Controls as QQC
import "../mobile" as Mobile

QQC.TextField {
    id: root;
    readonly property bool mobileStyle: typeof window !== "undefined" && window.useMobileWorkspace === true
    selectByMouse: true;
    selectionColor: styleAccentColor;
    placeholderTextColor: Qt.darker(styleTextColor);
    height: (mobileStyle ? 44 : 30) * dpiScale;
    implicitWidth: 150 * dpiScale;

    background: Rectangle {
        radius: (root.mobileStyle ? 4 : 5) * dpiScale;
        color: root.mobileStyle ? Mobile.MobileStyle.fill(style === "dark") : root.activeFocus? styleBackground2 : styleButtonColor;
        border.color: root.mobileStyle ? Mobile.MobileStyle.accent(style === "dark") : styleButtonColor;
        border.width: root.mobileStyle ? (root.activeFocus ? 1 * dpiScale : 0) : 1 * dpiScale;

        opacity: root.hovered && !root.activeFocus? 0.8 : 1.0;

        Rectangle {
            layer.enabled: true;
            visible: root.acceptableInput && !root.mobileStyle;

            width: parent.width - 2*x;
            height: 6 * dpiScale;
            color: root.activeFocus? styleAccentColor : "#9a9a9a";
            anchors.bottom: parent.bottom;
            anchors.bottomMargin: -1 * dpiScale;
            radius: parent.radius;

            Rectangle {
                width: parent.width;
                height: (root.activeFocus? 4 : 5) * dpiScale;
                color: root.activeFocus? styleBackground2 : styleButtonColor;
                y: 0;
            }
        }
    }
    color: styleTextColor;
    opacity: enabled? 1.0 : 0.5;
    verticalAlignment: Text.AlignVCenter;
    font.family: mobileStyle ? Mobile.MobileStyle.fontFamily : styleFont;
    font.pixelSize: (mobileStyle ? Mobile.MobileStyle.body : 13) * dpiScale;
    bottomPadding: 5 * dpiScale;
    topPadding: 5 * dpiScale;
    leftPadding: (mobileStyle ? 12 : 6) * dpiScale;

    property alias tooltip: tt.text;
    ToolTip { id: tt; visible: !isMobile && text.length > 0 && root.hovered; }
}
