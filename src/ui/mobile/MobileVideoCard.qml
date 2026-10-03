// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick
import QtQuick.Controls as QQC

Rectangle {
    id: root
    property real unit: 1
    property bool dark: true
    required property var record
    property string statusText: ""
    property string metadataText: ""
    property color statusColor: MobileStyle.secondary(dark)
    property bool selected: false
    property bool selecting: false
    property bool scrolling: false
    readonly property bool compact: width / unit < 320
    property color accentColor: MobileStyle.accent(dark)
    property bool first: false
    property bool last: false
    property real progress: -1
    signal activated()
    signal playRequested()
    signal informationRequested()
    signal selectionRequested()
    signal held()
    signal dragged(real px, real py)
    signal dragEnded()
    radius: 0
    color: selected ? (dark ? "#283446" : "#e9eef7") : MobileStyle.surface(dark)
    Rectangle { visible: !root.first; width: parent.width; height: parent.height / 2; color: root.color }
    Rectangle { visible: !root.last; y: parent.height / 2; width: parent.width; height: parent.height / 2; color: root.color }
    clip: true
    Accessible.role: Accessible.ListItem
    Accessible.name: (record.filename || "") + ", " + statusText
    Accessible.selected: selected
    Rectangle {
        visible: true
        x: 12 * root.unit; anchors.verticalCenter: parent.verticalCenter
        width: 24 * root.unit; height: width; radius: width / 2
        color: root.selected ? root.accentColor : "transparent"
        border.width: root.selected ? 0 : 1.5 * root.unit
        border.color: root.dark ? "#636366" : "#c7c7cc"
        MobileIcon { visible: root.selected; anchors.centerIn: parent; width: 18 * root.unit; height: width; name: "check"; color: "#ffffff" }
    }
    Item {
        id: thumbnail
        visible: !root.compact
        x: 48 * root.unit
        width: 64 * root.unit; height: 44 * root.unit
        anchors.verticalCenter: parent.verticalCenter
        Rectangle { anchors.fill: parent; color: MobileStyle.fill(root.dark); radius: 5 * root.unit }
        Image { anchors.fill: parent; source: root.record.thumbnail || ""; fillMode: Image.PreserveAspectCrop }
    }
    Column {
        id: textColumn
        x: root.compact ? 48 * root.unit : thumbnail.x + thumbnail.width + 12 * root.unit
        width: parent.width - x - 48 * root.unit
        anchors.verticalCenter: parent.verticalCenter
        spacing: 3 * root.unit
        MobileText {
            unit: root.unit; dark: root.dark
            width: parent.width
            text: root.record.filename || ""
            font.weight: Font.DemiBold
            objectName: "mobileVideoFilename"
            wrapMode: root.compact ? Text.WrapAnywhere : Text.NoWrap
            elide: root.compact ? Text.ElideRight : Text.ElideMiddle
            maximumLineCount: root.compact ? 2 : 1
        }
        MobileText {
            unit: root.unit; dark: root.dark; secondary: true
            width: parent.width
            text: root.metadataText
            objectName: "mobileVideoMetadata"
            elide: Text.ElideRight
        }
        MobileText {
            unit: root.unit; dark: root.dark; secondary: true
            width: parent.width
            text: root.statusText
            objectName: "mobileVideoStatus"
            visible: root.record.status !== "Queued" || !!root.record.deepMatched
            color: root.statusColor
            elide: Text.ElideRight
        }
    }
    Rectangle {
        visible: !root.last
        x: textColumn.x; width: parent.width - x
        height: 0.5 * root.unit; anchors.bottom: parent.bottom; color: MobileStyle.separator(root.dark)
    }
    Rectangle {
        visible: root.progress >= 0
        anchors.bottom: parent.bottom; anchors.left: parent.left
        width: parent.width * Math.max(0, Math.min(1, root.progress))
        height: 3 * root.unit; color: root.accentColor
    }
    MouseArea {
        id: pointer
        anchors.fill: parent
        property bool armed: false
        property bool longPress: false
        property real startX: 0
        property real startY: 0
        preventStealing: armed
        Timer {
            id: hold
            interval: 600
            onTriggered: {
                if (root.scrolling) return;
                pointer.armed = true;
                pointer.longPress = true;
                root.held();
            }
        }
        onPressed: mouse => {
            startX = mouse.x; startY = mouse.y;
            armed = false; longPress = false;
            hold.restart();
        }
        onPositionChanged: mouse => {
            if (armed) { root.dragged(mouse.x, mouse.y); return; }
            if (Math.hypot(mouse.x - startX, mouse.y - startY) > 18 * root.unit) hold.stop();
        }
        onReleased: { hold.stop(); if (armed) root.dragEnded(); armed = false; }
        onCanceled: { hold.stop(); if (armed) root.dragEnded(); armed = false; }
        onClicked: mouse => {
            if (longPress) return;
            if (mouse.x < 44 * root.unit) root.selectionRequested();
            else if (mouse.x >= thumbnail.x && mouse.x <= thumbnail.x + thumbnail.width
                    && mouse.y >= thumbnail.y && mouse.y <= thumbnail.y + thumbnail.height) root.playRequested();
            else root.activated();
        }
    }
    Item {
        objectName: "mobileCardSelect"
        x: 0; y: 0; width: 44 * root.unit; height: parent.height
        Accessible.role: Accessible.CheckBox
        Accessible.name: root.record.filename || ""
        Accessible.checkable: true
        Accessible.checked: root.selected
        Accessible.onPressAction: root.selectionRequested()
    }
    Item {
        objectName: "mobileCardPlay"
        visible: thumbnail.visible
        x: thumbnail.x; y: thumbnail.y; width: thumbnail.width; height: thumbnail.height
        Accessible.role: Accessible.Button
        Accessible.name: qsTranslate("MobileWorkspace", "Play")
        Accessible.onPressAction: root.playRequested()
        // The row owns touch input so holding or dragging the thumbnail still selects.
        Rectangle { visible: !!root.record.thumbnail; anchors.centerIn: parent; width: 28 * root.unit; height: width; radius: 4 * root.unit; color: "#99000000" }
        MobileIcon { anchors.centerIn: parent; width: 20 * root.unit; height: width; name: "play"; color: root.record.thumbnail ? "white" : MobileStyle.secondary(root.dark) }
    }
    MobileButton {
        objectName: "mobileCardInfo"
        unit: root.unit; dark: root.dark; quiet: true; iconOnly: true; iconName: "info"
        anchors.right: parent.right; anchors.rightMargin: 2 * root.unit; anchors.verticalCenter: parent.verticalCenter
        text: qsTranslate("MobileWorkspace", "Video information")
        onClicked: root.informationRequested()
    }
}
