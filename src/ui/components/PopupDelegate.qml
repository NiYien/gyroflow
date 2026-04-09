// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Controls as QQC
import QtQuick.Controls.Material as QQCM
QQC.ItemDelegate {
    property QQC.Popup parentPopup: null;
    property Item lv: null;

    id: dlg;
    width: parent? parent.width : 0;
    implicitHeight: parentPopup.itemHeight;
    readonly property var itemData: modelData
    readonly property string itemText: {
        if (typeof itemData === "string") return itemData;
        if (itemData && typeof itemData === "object") {
            if (typeof itemData.label === "string") return itemData.label;
            if (typeof itemData.name  === "string") return itemData.name;
            if (typeof itemData.text  === "string") return itemData.text;
        }
        return "";
    }
    readonly property string itemIconName: {
        if (itemData && typeof itemData === "object" && typeof itemData.iconName === "string") {
            return itemData.iconName;
        }
        return parentPopup.icons[index] || "";
    }
    readonly property color itemColor: {
        if (itemData && typeof itemData === "object" && itemData.color) {
            return itemData.color;
        }
        return parentPopup.colors[index] || styleTextColor;
    }
    readonly property bool itemEnabled: {
        if (itemData && typeof itemData === "object" && typeof itemData.enabled === "boolean") {
            return itemData.enabled;
        }
        return true;
    }
    text: qsTranslate("Popup", itemText)
    enabled: itemEnabled
    leftPadding: 12 * dpiScale
    rightPadding: 12 * dpiScale
    topPadding: parentPopup.itemHeight / 3.5
    bottomPadding: parentPopup.itemHeight / 3.5
    font: parentPopup.font
    icon.name: itemIconName
    icon.source: itemIconName ? "qrc:/resources/icons/svg/" + itemIconName + ".svg" : ""
    icon.width: parentPopup.itemHeight / 2 + 1 * dpiScale
    icon.height: parentPopup.itemHeight / 2 + 1 * dpiScale
    icon.color: itemEnabled ? itemColor : Qt.rgba(itemColor.r, itemColor.g, itemColor.b, 0.38)

    QQCM.Material.foreground: itemEnabled ? itemColor : Qt.rgba(itemColor.r, itemColor.g, itemColor.b, 0.38)
    onImplicitWidthChanged: {
        if (implicitWidth > parentPopup.maxItemWidth) parentPopup.maxItemWidth = implicitWidth;
    }

    scale: dlg.down? 0.970 : 1.0;
    Ease on scale { }

    MouseArea { anchors.fill: parent; acceptedButtons: Qt.NoButton; cursorShape: Qt.PointingHandCursor; }

    function clickHandler(): void {
        if (!dlg.itemEnabled) return;
        parentPopup.focus = false;
        parentPopup.parent.focus = true;
        parentPopup.clicked(index);
        parentPopup.visible = false;
    }

    onClicked: clickHandler();

    Keys.onPressed: (e) => {
        if (e.key == Qt.Key_Enter || e.key == Qt.Key_Return) {
            clickHandler();
        }
    }

    highlighted: parentPopup.highlightedIndex === index;
}
