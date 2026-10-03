// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls.Basic as QQC
import "MobileLogic.js" as Logic

Rectangle {
    id: root
    objectName: "mobileGyroData"
    property real unit: 1
    property bool dark: true
    property var records: []
    property bool expanded: false
    property real maximumListHeight: 180 * unit
    implicitHeight: 44 * unit + (expanded ? Math.min(recordList.contentHeight, maximumListHeight) : 0)
    color: MobileStyle.surface(dark)
    onRecordsChanged: if (!records.length) expanded = false
    MobileButton {
        objectName: "mobileGyroSummary"
        width: parent.width; height: 44 * root.unit
        unit: root.unit; dark: root.dark; quiet: true
        text: qsTranslate("MobileWorkspace", "Gyroscope data") + " · " + root.records.length
        contentItem: Item {
            MobileText {
                x: 12 * root.unit; width: parent.width - 44 * root.unit; height: parent.height
                unit: root.unit; dark: root.dark; elide: Text.ElideRight
                text: qsTranslate("MobileWorkspace", "Gyroscope data") + " · " + root.records.length
            }
            MobileIcon {
                name: "chevron"; rotation: root.expanded ? 90 : 0
                color: MobileStyle.secondary(root.dark); width: 14 * root.unit; height: width
                anchors.right: parent.right; anchors.rightMargin: 12 * root.unit; anchors.verticalCenter: parent.verticalCenter
            }
        }
        onClicked: root.expanded = !root.expanded
    }
    ListView {
        id: recordList
        objectName: "mobileGyroRecordList"
        visible: root.expanded
        y: 44 * root.unit; width: parent.width; height: Math.max(0, root.height - y)
        clip: true; boundsBehavior: Flickable.StopAtBounds
        model: root.records
        QQC.ScrollIndicator.vertical: QQC.ScrollIndicator {}
        delegate: Item {
            required property var modelData
            width: recordList.width; height: 48 * root.unit
            Rectangle { width: parent.width; height: 0.5 * root.unit; color: MobileStyle.separator(root.dark) }
            MobileText {
                objectName: "mobileGyroFilename"
                x: 12 * root.unit; width: parent.width - duration.width - 36 * root.unit; height: parent.height
                unit: root.unit; dark: root.dark; text: modelData.filename || ""; elide: Text.ElideMiddle
            }
            MobileText {
                id: duration
                objectName: "mobileGyroDuration"
                anchors.right: parent.right; anchors.rightMargin: 12 * root.unit
                width: implicitWidth; height: parent.height
                unit: root.unit; dark: root.dark; secondary: true
                text: modelData.duration_ms > 0 ? Logic.duration(modelData.duration_ms) : "—"
            }
        }
    }
}
