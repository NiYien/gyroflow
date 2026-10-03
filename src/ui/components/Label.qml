// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import "../mobile" as Mobile

Grid {
    id: root;
    readonly property bool mobileStyle: typeof window !== "undefined" && window.useMobileWorkspace === true
    property bool mobileStacked: false
    readonly property int effectivePosition: mobileStyle && mobileStacked ? Label.TopPosition : position

    enum LabelPosition { TopPosition, LeftPosition }

    property int position: Label.TopPosition;
    default property alias data: inner.data;
    property alias text: t.text;
    property alias inner: inner;
    property alias t: t;
    onEffectivePositionChanged: t.height = root.effectivePosition === Label.TopPosition? undefined : Qt.binding(() => inner.height);

    // Only set columns; Grid auto-derives rows from item count. Setting both
    // creates a transient `rows=1,columns=1` mid-update when `position` flips
    // (one binding fires before the other), triggering "Grid contains more
    // visible items (2) than rows*columns (1)" warnings on theme/mode toggles.
    columns: effectivePosition === Label.TopPosition? 1 : 2;
    spacing: 8 * dpiScale;
    width: parent.width;

    BasicText {
        id: t;
        leftPadding: 0;
        verticalAlignment: Text.AlignVCenter;
        height: root.effectivePosition === Label.TopPosition? undefined : inner.height;
        width: root.mobileStyle && root.mobileStacked ? root.width : undefined;
        font.pixelSize: (root.mobileStyle ? Mobile.MobileStyle.caption : 12) * dpiScale;
        color: root.mobileStyle ? Mobile.MobileStyle.secondary(style === "dark") : styleTextColor;
        wrapMode: root.mobileStyle && root.mobileStacked ? Text.WordWrap : Text.NoWrap;
        MouseArea {
            id: ma;
            hoverEnabled: tt.text.length > 0;
            anchors.fill: t;
            acceptedButtons: Qt.LeftButton;

            function traverseChildren(node: QtObject): void {
                for (let i = node.children.length; i > 0; --i) {
                    const child = node.children[i - 1];
                    if (child) {
                        if (child.toString().startsWith("NumberField")) {
                            child.reset();
                        } else {
                            traverseChildren(child);
                        }
                    }
                }
            }
            onDoubleClicked: (mouse) => {
                traverseChildren(inner);
            }
        }
    }

    Item {
        id: inner;
        width: parent.width - (root.effectivePosition === Label.TopPosition? 0 : t.width + root.spacing);
        height: children[0].height + 2 * dpiScale;
    }

    property alias tooltip: tt.text;
    ToolTip { id: tt; visible: !isMobile && text.length > 0 && ma.containsMouse; }
}
