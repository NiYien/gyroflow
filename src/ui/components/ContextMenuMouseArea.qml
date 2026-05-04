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

    // Desktop (incl. desktop touchscreen) keeps the original Qt long-press
    // signal — its DragThreshold default works fine when the user is at a
    // desk and not jittering as they would on a hand-held phone.
    TapHandler {
        parent: root.underlyingItem || root.parent;
        acceptedDevices: PointerDevice.TouchScreen;
        enabled: Qt.platform.os !== "android" && Qt.platform.os !== "ios";
        onLongPressed: {
            if (root.ignoreLeftRegionWidth > 0 && point.position.x < root.ignoreLeftRegionWidth) return;
            root.contextMenu(true, point.position.x, point.position.y);
        }
    }

    // Mobile-only: Qt 6.7's onLongPressed is unreliable on Android because
    // the default DragThreshold gesture policy cancels the press the moment
    // a finger jitters past ~10px, which real touches almost always do. We
    // use WithinBounds so jitter does not abort the press, and a manual
    // Timer driven by `pressed` so the menu fires even if onLongPressed
    // itself never does. Gated to mobile so desktop behavior is unchanged.
    Timer {
        id: touchLongPressTimer;
        interval: 600;
        onTriggered: {
            if (root.ignoreLeftRegionWidth > 0 && touchLongPress._lpX < root.ignoreLeftRegionWidth) return;
            root.contextMenu(true, touchLongPress._lpX, touchLongPress._lpY);
        }
    }
    TapHandler {
        id: touchLongPress;
        parent: root.underlyingItem || root.parent;
        acceptedDevices: PointerDevice.TouchScreen;
        enabled: Qt.platform.os === "android" || Qt.platform.os === "ios";
        gesturePolicy: TapHandler.WithinBounds;
        property real _lpX: 0;
        property real _lpY: 0;
        onPressedChanged: {
            if (pressed) {
                touchLongPress._lpX = point.position.x;
                touchLongPress._lpY = point.position.y;
                touchLongPressTimer.restart();
            } else {
                touchLongPressTimer.stop();
            }
        }
    }
}
