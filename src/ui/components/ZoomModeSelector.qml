// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

Item {
    id: root;
    // Keep serialized 0=none, 1=dynamic, 2=static; legacy none selects neither item.
    property int currentIndex: 1;
    property alias font: choices.font;
    height: choices.height;

    ComboBox {
        id: choices;
        objectName: "zoomChoices";
        width: parent.width;
        model: [QT_TRANSLATE_NOOP("Popup", "Dynamic zooming"), QT_TRANSLATE_NOOP("Popup", "Static zoom")];
        currentIndex: root.currentIndex - 1;
        displayText: currentIndex < 0 ? "—" : currentText;
        onActivated: root.currentIndex = currentIndex + 1;
    }
}
