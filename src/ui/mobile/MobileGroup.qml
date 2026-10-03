// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick

Column {
    id: root
    property real unit: 1
    property bool dark: true
    property string title: ""
    property real contentSpacing: 0
    property real contentInset: 16 * unit
    default property alias content: contents.data
    spacing: title.length ? 8 * unit : 0
    MobileText {
        visible: root.title.length > 0
        x: 4 * root.unit; width: parent.width - 8 * root.unit
        unit: root.unit; dark: root.dark; secondary: true
        font.weight: Font.DemiBold
        text: root.title; wrapMode: Text.WordWrap
    }
    Rectangle {
        width: parent.width; height: contents.height + 16 * root.unit
        color: MobileStyle.surface(root.dark); radius: 5 * root.unit
        Column {
            id: contents
            x: root.contentInset; y: 8 * root.unit; width: parent.width - 2 * x
            spacing: root.contentSpacing
        }
    }
}
