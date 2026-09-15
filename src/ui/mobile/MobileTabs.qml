// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
pragma ComponentBehavior: Bound
import QtQuick

Rectangle {
    id: root
    property real unit: 1
    property bool dark: true
    property var model: []
    property int currentIndex: 0
    property bool navigation: false
    signal activated(int index)
    implicitHeight: (navigation ? 44 : 50) * unit
    color: navigation ? "transparent" : MobileStyle.fill(dark); radius: 5 * unit
    Rectangle { visible: root.navigation; width: parent.width; height: root.unit; anchors.bottom: parent.bottom; color: MobileStyle.separator(root.dark) }
    Row {
        anchors.fill: parent; anchors.margins: (root.navigation ? 0 : 3) * root.unit
        Repeater {
            model: root.model
            MobileButton {
                required property int index
                required property string modelData
                objectName: "mobileTab" + index
                width: parent.width / root.model.length; height: parent.height
                unit: root.unit; dark: root.dark; segmented: !root.navigation; quiet: root.navigation; checked: root.currentIndex === index
                foreground: root.navigation && checked ? MobileStyle.text(root.dark) : root.navigation ? MobileStyle.secondary(root.dark) : MobileStyle.text(root.dark)
                font.weight: checked ? Font.DemiBold : Font.Normal
                text: modelData; font.pixelSize: MobileStyle.caption * root.unit
                Rectangle { visible: root.navigation && parent.checked; x: 10 * root.unit; width: parent.width - 20 * root.unit; height: 2 * root.unit; anchors.bottom: parent.bottom; color: MobileStyle.text(root.dark) }
                onClicked: root.activated(index)
            }
        }
    }
}
