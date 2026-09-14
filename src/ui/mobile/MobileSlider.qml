// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick
import QtQuick.Controls.Basic as QQC

QQC.Slider {
    id: root
    property real unit: 1
    property bool dark: true
    implicitHeight: 44 * unit
    leftPadding: 14 * unit; rightPadding: leftPadding
    background: Rectangle {
        x: root.leftPadding; y: (root.height - height) / 2
        width: root.availableWidth; height: 4 * root.unit; radius: height / 2
        color: MobileStyle.fill(root.dark)
        Rectangle { width: root.visualPosition * parent.width; height: parent.height; radius: parent.radius; color: MobileStyle.accent(root.dark) }
    }
    handle: Rectangle {
        x: root.leftPadding + root.visualPosition * root.availableWidth - width / 2
        y: (root.height - height) / 2
        width: 28 * root.unit; height: width; radius: width / 2
        color: root.pressed ? "#f2f2f7" : "#ffffff"
        border.color: root.dark ? "#626266" : "#d1d1d6"
        border.width: 0.5 * root.unit
    }
    opacity: enabled ? 1 : 0.4
}
