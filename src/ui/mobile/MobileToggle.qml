// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick
import QtQuick.Controls.Basic as QQC

QQC.Switch {
    id: root
    property real unit: 1
    property bool dark: true
    property string valueText: ""
    Accessible.name: text + (valueText ? " " + valueText : "")
    implicitHeight: Math.max(52 * unit, label.implicitHeight + 16 * unit)
    leftPadding: 0; rightPadding: 0
    font.family: MobileStyle.fontFamily
    font.pixelSize: MobileStyle.body * unit
    indicator: Rectangle {
        x: root.width - width; y: (root.height - height) / 2
        width: 51 * root.unit; height: 31 * root.unit; radius: height / 2
        color: root.checked ? "#245ac8" : MobileStyle.fill(root.dark)
        Rectangle {
            x: (root.checked ? 22 : 2) * root.unit; y: 2 * root.unit
            width: 27 * root.unit; height: width; radius: width / 2; color: "#ffffff"
            Behavior on x { NumberAnimation { duration: 120 } }
        }
    }
    contentItem: MobileText {
        id: label
        unit: root.unit; dark: root.dark; text: root.text
        rightPadding: 67 * root.unit + (valueLabel.visible ? valueLabel.implicitWidth + 12 * root.unit : 0); wrapMode: Text.WordWrap
    }
    MobileText {
        id: valueLabel
        objectName: "mobileToggleValue"
        anchors.right: root.indicator.left; anchors.rightMargin: 12 * root.unit
        anchors.verticalCenter: parent.verticalCenter
        unit: root.unit; dark: root.dark; secondary: true
        text: root.valueText; visible: text.length > 0
    }
    opacity: enabled ? 1 : 0.4
}
