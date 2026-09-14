// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Controls as QQC
import "../mobile" as Mobile

QQC.ComboBox {
    id: root;
    readonly property bool mobileStyle: typeof window !== "undefined" && window.useMobileWorkspace === true

    //property alias icon: ti.icon;
    property alias itemHeight: pp.itemHeight;

    implicitWidth: 150 * dpiScale;
    height: (mobileStyle ? 48 : 35) * dpiScale;

    font.pixelSize: (mobileStyle ? Mobile.MobileStyle.body : 13) * dpiScale;
    font.family: mobileStyle ? Mobile.MobileStyle.fontFamily : styleFont;

    hoverEnabled: enabled;

    delegate: pp.contentItem.delegate;

    indicator: DropdownChevron { height: pp.itemHeight / 2.4; }

    background: Rectangle {
        color: root.mobileStyle ? Mobile.MobileStyle.fill(style === "dark") : root.hovered || root.activeFocus? Qt.lighter(styleButtonColor, 1.2) : styleButtonColor;
        opacity: root.down || !parent.enabled? 0.75 : 1.0;
        Ease on opacity { duration: 100; }
        radius: (root.mobileStyle ? 4 : 6) * dpiScale;
        anchors.fill: parent;
        border.width: !root.mobileStyle && style === "light"? (1 * dpiScale) : 0;
        border.color: "#cccccc";
    }
    Keys.onPressed: (e) => {
        if (e.key == Qt.Key_Enter || e.key == Qt.Key_Return) {
            pp.open();
            pp.focus = true;
        }
    }

    scale: root.pressed? 0.98 : 1.0;
    Ease on scale {  }

    contentItem: Text {
        text: root.mobileStyle && !root.displayText ? qsTranslate("MobileWorkspace", "Select") : typeof root.displayText === "string" ? qsTranslate("Popup", root.displayText) : "";
        color: styleTextColor;
        font: root.font;
        anchors.left: parent.left;
        anchors.leftMargin: 10 * dpiScale;
        verticalAlignment: Text.AlignVCenter;
        opacity: parent.enabled? 1.0 : 0.5;
        anchors.right: parent.right;
        anchors.rightMargin: 20 * dpiScale;
        elide: Text.ElideRight;
    }

    popup: Popup {
        id: pp;
        font: root.font;
        model: root.delegateModel;
        currentIndex: root.currentIndex;
        width: root.mobileStyle ? Math.min(window.width - 32 * dpiScale, Math.max(root.width, pp.maxItemWidth + 10 * dpiScale)) : Math.max(root.width, pp.maxItemWidth + 10 * dpiScale);
        x: -(width - parent.width);
        highlightedIndex: root.highlightedIndex;
    }

    property alias tooltip: tt.text;
    ToolTip { id: tt; visible: !isMobile && text.length > 0 && root.hovered; }
}
