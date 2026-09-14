// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick
import QtQuick.Controls.Basic as QQC

QQC.ItemDelegate {
    id: root
    property real unit: 1
    property bool dark: true
    property string description: ""
    property string iconName: ""
    property bool navigation: false
    property bool destructive: false
    property bool divider: false
    implicitHeight: Math.max(52 * unit, labels.implicitHeight + 24 * unit)
    leftPadding: 16 * unit; rightPadding: 16 * unit
    topPadding: 12 * unit; bottomPadding: 12 * unit
    Accessible.name: text
    Accessible.description: description
    contentItem: Item {
        implicitHeight: labels.implicitHeight
        MobileIcon {
            id: icon
            visible: root.iconName.length > 0
            width: 22 * root.unit; height: width
            anchors.verticalCenter: parent.verticalCenter
            name: root.iconName
            color: root.destructive ? (root.dark ? "#ff8a85" : "#ba3030") : MobileStyle.secondary(root.dark)
        }
        Column {
            id: labels
            x: icon.visible ? 34 * root.unit : 0
            width: parent.width - x - (root.navigation ? 24 * root.unit : 0)
            anchors.verticalCenter: parent.verticalCenter
            spacing: 4 * root.unit
            MobileText { width: parent.width; unit: root.unit; dark: root.dark; text: root.text; wrapMode: Text.WordWrap; color: root.destructive ? (root.dark ? "#ff6961" : "#d70015") : MobileStyle.text(root.dark) }
            MobileText { visible: root.description.length > 0; width: parent.width; unit: root.unit; dark: root.dark; secondary: true; text: root.description; wrapMode: Text.WordWrap }
        }
        MobileIcon { visible: root.navigation; anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter; width: 16 * root.unit; height: width; name: "chevron"; color: MobileStyle.secondary(root.dark) }
    }
    background: Rectangle {
        color: root.down ? MobileStyle.fill(root.dark) : "transparent"
        radius: 4 * root.unit
        border.width: root.visualFocus ? 2 * root.unit : 0
        border.color: MobileStyle.accent(root.dark)
        Rectangle { visible: root.divider; x: 16 * root.unit; width: parent.width - x; height: 0.5 * root.unit; anchors.bottom: parent.bottom; color: MobileStyle.separator(root.dark) }
    }
    opacity: enabled ? 1 : 0.4
}
