// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick
import QtQuick.Controls.Basic as QQC

QQC.Button {
    id: root
    property real unit: 1
    property bool dark: true
    property bool emphasized: false
    property bool circular: false
    property bool quiet: false
    property bool destructive: false
    property string iconName: ""
    property bool iconOnly: false
    property real iconSize: 22 * unit
    property bool multiline: false
    property bool segmented: false
    property color accentColor: MobileStyle.accent(dark)
    readonly property color buttonAccent: "#245ac8"
    property color emphasizedColor: buttonAccent
    property color foreground: segmented ? MobileStyle.text(dark) : emphasized ? "#ffffff" : destructive ? (dark ? "#ff8a85" : "#ba3030") : MobileStyle.text(dark)
    implicitHeight: multiline ? Math.max(48 * unit, label.implicitHeight + 16 * unit) : (quiet ? 44 : 48) * unit
    implicitWidth: iconOnly ? 44 * unit : Math.max(44 * unit, Math.ceil(metrics.advanceWidth) + (iconName ? 54 : quiet ? 16 : 24) * unit)
    padding: (quiet ? 8 : 12) * unit
    leftPadding: padding
    rightPadding: padding
    topPadding: 4 * unit
    bottomPadding: 4 * unit
    font.family: MobileStyle.fontFamily
    font.pixelSize: MobileStyle.body * unit
    font.weight: emphasized ? Font.DemiBold : Font.Normal
    Accessible.name: text
    TextMetrics { id: metrics; text: root.text; font: root.font }
    background: Rectangle {
        radius: root.circular ? Math.min(width, height) / 2 : 5 * root.unit
        color: root.segmented ? (root.checked ? MobileStyle.surface(root.dark) : "transparent") : root.emphasized ? root.emphasizedColor : root.down ? MobileStyle.separator(root.dark) : root.quiet ? "transparent" : MobileStyle.fill(root.dark)
        opacity: root.enabled ? (root.down ? 0.7 : 1) : 0.3
        border.width: root.visualFocus ? 2 * root.unit : 0
        border.color: root.accentColor
    }
    contentItem: Item {
        opacity: root.enabled ? 1 : 0.35
        MobileIcon {
            id: symbol
            visible: root.iconName.length > 0
            width: root.iconSize; height: width
            x: root.iconOnly ? (parent.width - width) / 2 : 0
            anchors.verticalCenter: parent.verticalCenter
            name: root.iconName
            color: root.foreground
        }
        Text {
            id: label
            visible: !root.iconOnly
            anchors.left: symbol.visible ? symbol.right : parent.left
            anchors.leftMargin: symbol.visible ? 8 * root.unit : 0
            anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter
            text: root.text; font: root.font
            horizontalAlignment: Text.AlignHCenter
            color: root.foreground
            wrapMode: Text.WordWrap
            maximumLineCount: root.multiline ? 4 : root.quiet ? 1 : 2
            elide: Text.ElideRight
        }
    }
}
