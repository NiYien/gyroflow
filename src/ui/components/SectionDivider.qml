// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 NiYien

import QtQuick

Column {
    property string label: ""
    width: parent.width;
    spacing: 0;
    topPadding: 8 * dpiScale;
    bottomPadding: 4 * dpiScale;

    Rectangle {
        width: parent.width;
        height: 1 * dpiScale;
        color: styleHrColor;
        opacity: 0.5;
    }
    Item { width: 1; height: label.length > 0 ? 8 * dpiScale : 2 * dpiScale; }
    Row {
        visible: label.length > 0;
        spacing: 6 * dpiScale;
        Rectangle {
            width: 3 * dpiScale;
            height: 13 * dpiScale;
            radius: width / 2;
            color: styleAccentColor;
            anchors.verticalCenter: parent.verticalCenter;
        }
        BasicText {
            text: label;
            font.pixelSize: 11 * dpiScale;
            font.bold: true;
            color: styleTextColor;
            opacity: 0.55;
            leftPadding: 0;
            anchors.verticalCenter: parent.verticalCenter;
        }
    }
}
