// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick
import QtQuick.Controls.Basic as QQC

QQC.ComboBox {
    id: root
    property real unit: 1
    property bool dark: true
    implicitHeight: 48 * unit
    font.family: MobileStyle.fontFamily; font.pixelSize: MobileStyle.body * unit
    leftPadding: 12 * unit; rightPadding: 36 * unit
    contentItem: MobileText { unit: root.unit; dark: root.dark; text: root.displayText; elide: Text.ElideRight }
    indicator: MobileIcon {
        name: "chevron"; rotation: 90; width: 18 * root.unit; height: width
        x: root.width - width - 12 * root.unit; y: (root.height - height) / 2
        color: MobileStyle.secondary(root.dark)
    }
    background: Rectangle { radius: 4 * root.unit; color: MobileStyle.fill(root.dark); opacity: root.down ? 0.7 : 1 }
    delegate: QQC.ItemDelegate {
        required property int index
        required property var modelData
        width: root.width; height: Math.max(48 * root.unit, choiceLabel.implicitHeight + 20 * root.unit)
        contentItem: MobileText {
            id: choiceLabel
            unit: root.unit; dark: root.dark
            text: root.textRole ? parent.modelData[root.textRole] : parent.modelData
            wrapMode: Text.WordWrap
        }
        highlighted: root.highlightedIndex === index
        background: Rectangle { color: parent.highlighted ? MobileStyle.fill(root.dark) : "transparent"; radius: 4 * root.unit }
    }
    popup: QQC.Popup {
        y: root.height + 4 * root.unit; width: root.width
        implicitHeight: Math.min(contentItem.implicitHeight + 8 * root.unit, 320 * root.unit)
        padding: 4 * root.unit; margins: 12 * root.unit
        background: Rectangle { color: MobileStyle.surface(root.dark); radius: 5 * root.unit; border.width: 0.5; border.color: MobileStyle.separator(root.dark) }
        contentItem: ListView {
            clip: true; implicitHeight: contentHeight
            model: root.popup.visible ? root.delegateModel : null
            currentIndex: root.highlightedIndex
            QQC.ScrollIndicator.vertical: QQC.ScrollIndicator {}
        }
    }
    opacity: enabled ? 1 : 0.4
}
