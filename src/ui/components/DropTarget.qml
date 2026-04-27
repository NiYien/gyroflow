// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick

Rectangle {
    id: root;
    property var extensions: [];
    property var acceptedFilenameSuffixes: [];
    property bool acceptAnyMatchingUrl: false;
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
        function acceptsUrl(url: url): bool {
            const filename = url.toString().split(/[\\/]/).pop().toLowerCase();
            const hasExtension = filename.includes(".");
            if (!hasExtension) return true;
            for (const suffix of root.acceptedFilenameSuffixes) {
                if (filename.endsWith(suffix.toLowerCase())) return true;
            }
            const ext = filename.split(".").pop();
            return root.extensions.indexOf(ext) > -1;
        }
        onEntered: (drag) => {
            if (!drag.urls.length) return;
            if (!root.acceptAnyMatchingUrl) {
                drag.accepted = acceptsUrl(drag.urls[0]);
                return;
            }
            drag.accepted = false;
            for (const url of drag.urls) {
                if (acceptsUrl(url)) {
                    drag.accepted = true;
                    break;
                }
            }
        }
        onDropped: (drop) => {
            root.loadFiles(drop.urls);
            root.loadFile(drop.urls[0])
        }
    }
}
