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

    // Mobile-only long-press detection. Qt's own onLongPressed is unusable on
    // Android: the default DragThreshold policy aborts the press as soon as a
    // finger wobbles past the system drag threshold (~10px), which hand-held
    // touches almost always do. So the menu is driven by our own Timer, and the
    // jitter tolerance is defined here (_cancelDistance) instead of inherited.
    //
    // HARD CONSTRAINT 1 - this handler MUST NOT take the exclusive grab.
    // A TapHandler with any gesturePolicy other than DragThreshold grabs
    // exclusively on press, which (a) suppresses Qt's touch->mouse synthesis
    // for the item underneath, so text fields never focus, sliders never drag
    // and buttons never fire onClicked, and (b) starves sibling handlers that
    // hold only a passive grab, such as the video area's double-tap-to-
    // fullscreen. Both classes of breakage shipped on Android for months
    // because of exactly that. PointHandler only ever takes a passive grab, so
    // everything underneath keeps receiving its events.
    //
    // HARD CONSTRAINT 2 - `target` MUST stay null. PointerHandler.target
    // defaults to parentItem, and PointHandler moves its target to the point
    // position, so leaving it unset makes the field or slider follow the finger.
    Timer {
        id: touchLongPressTimer;
        interval: 600;
        onTriggered: {
            if (root.ignoreLeftRegionWidth > 0 && touchLongPress._lpX < root.ignoreLeftRegionWidth) return;
            root.contextMenu(true, touchLongPress._lpX, touchLongPress._lpY);
        }
    }
    PointHandler {
        id: touchLongPress;
        parent: root.underlyingItem || root.parent;
        target: null;
        acceptedDevices: PointerDevice.TouchScreen;
        enabled: Qt.platform.os === "android" || Qt.platform.os === "ios";
        property real _lpX: 0;
        property real _lpY: 0;
        // Generous compared to the ~10px system drag threshold: this is the
        // wobble of a finger holding still, not a deliberate drag.
        readonly property real _cancelDistance: 30 * dpiScale;
        onActiveChanged: {
            if (active) {
                touchLongPress._lpX = point.position.x;
                touchLongPress._lpY = point.position.y;
                touchLongPressTimer.restart();
            } else {
                touchLongPressTimer.stop();
            }
        }
        onPointChanged: {
            if (!touchLongPressTimer.running) return;
            const dx = point.position.x - touchLongPress._lpX;
            const dy = point.position.y - touchLongPress._lpY;
            const limit = touchLongPress._cancelDistance;
            if (dx * dx + dy * dy > limit * limit) touchLongPressTimer.stop();
        }
    }
}
