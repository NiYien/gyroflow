// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick

Rectangle {
    id: root;
    property var extensions: [];
    anchors.fill: parent;
    color: styleBackground;
    radius: 10 * dpiScale;
    opacity: da.containsDrag? 0.8 : 0.0;
    Ease on opacity { duration: 300; }

    signal loadFile(string path);
    signal loadFiles(var urls);

    BasicText {
        id: dropText;
        text: qsTr("Drop file here");
        font.pixelSize: 30 * dpiScale;
        anchors.centerIn: parent;
        leftPadding: 0;
        scale: dropText.contentWidth > (parent.width - 50 * dpiScale)? (parent.width - 50 * dpiScale) / dropText.contentWidth : 1.0;
    }

    Loader {
        anchors.fill: parent;
        asynchronous: true;
        sourceComponent: Component { DropTargetRect { } }
    }

    DropArea {
        id: da;
        anchors.fill: parent;
        enabled: root.visible;
        onEntered: (drag) => {
            if (!drag.urls.length) return;
            const url = drag.urls[0].toString();
            const ext = url.split(".").pop().toLowerCase();
            // [queue-pair-ux T5] 无扩展名（可能是文件夹）也允许拖入
            const hasExtension = url.includes(".");
            drag.accepted = !hasExtension || root.extensions.indexOf(ext) > -1;
        }
        onDropped: (drop) => {
            root.loadFiles(drop.urls);
            root.loadFile(drop.urls[0])
        }
    }
}
