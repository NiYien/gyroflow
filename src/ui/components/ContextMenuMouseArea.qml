// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2023 Adrian <adrian.eddy at gmail>

import QtQuick

MouseArea {
    id: root;
    anchors.fill: parent;
    acceptedButtons: Qt.RightButton;
    propagateComposedEvents: true;
    signal contextMenu(bool isHold, real x, real y);

    property Item underlyingItem: null;
    // Suppress touch long-press in the leftmost region (in pixels). Lets a parent
    // reserve a column (e.g. a selection checkbox) for its own long-press gesture.
    // Default 0 = no exclusion, behavior unchanged for existing callers.
    property real ignoreLeftRegionWidth: 0;

    onClicked: mouse => { if (mouse.button === Qt.RightButton) root.contextMenu(false, mouse.x, mouse.y); }

    TapHandler {
        parent: root.underlyingItem || root.parent;
        acceptedDevices: PointerDevice.TouchScreen;
        onLongPressed: {
            if (root.ignoreLeftRegionWidth > 0 && point.position.x < root.ignoreLeftRegionWidth) return;
            root.contextMenu(true, point.position.x, point.position.y);
        }
    }
}
