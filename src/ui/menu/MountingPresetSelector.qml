// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls as QQC

import "../components/"
import "mounting_orientation.js" as MountingOrientation

MenuItem {
    id: root;
    text: qsTr("Mounting position");
    iconName: "axes";
    objectName: "simple-mounting";
    opened: true;

    property string currentPosition: settings.value("mountingPosition", "top")
    property int currentRotation: parseInt(settings.value("mountingRotation", "0")) || 0
    property string lastKnownOrientation: ""
    property bool isCustom: false

    readonly property bool lightTheme: style === "light"
    readonly property color cardBorderColor: root.lightTheme ? "#d8dde6" : Qt.rgba(1, 1, 1, 0.14)

    readonly property var positions: [
        { face: "top",    label: qsTr("Top") },
        { face: "left",   label: qsTr("Left") },
        { face: "right",  label: qsTr("Right") },
        { face: "bottom", label: qsTr("Bottom") }
    ]
    readonly property var rotations: [
        { angle: 0,    label: "0\u00B0" },
        { angle: 90,   label: "+90\u00B0" },
        { angle: -90,  label: "-90\u00B0" },
        { angle: 180,  label: "180\u00B0" }
    ]
    function mountingSvgSource(): string {
        return "qrc:/resources/mounting/mount_" + root.currentPosition + "_" + root.currentRotation + ".svg";
    }

    function applyPreset(): void {
        const str = MountingOrientation.getOrientationString(root.currentPosition, root.currentRotation);
        if (str) {
            root.isCustom = false;
            root.lastKnownOrientation = str;
            controller.set_imu_orientation(str);
            Qt.callLater(controller.recompute_gyro);
            root.savePreset();
        }
    }

    function updateFromOrientation(orientationStr: string): void {
        if (!orientationStr || orientationStr.length === 0) return;
        root.lastKnownOrientation = orientationStr;
        const preset = MountingOrientation.reverseMapping(orientationStr);
        if (preset) {
            root.currentPosition = preset.face;
            root.currentRotation = preset.rotation;
            root.isCustom = false;
            root.savePreset();
        } else {
            root.isCustom = true;
        }
    }

    function savePreset(): void {
        settings.setValue("mountingPosition", root.currentPosition);
        settings.setValue("mountingRotation", root.currentRotation.toString());
    }

    function restorePreset(): void {
        const pos = settings.value("mountingPosition", "top");
        const rot = parseInt(settings.value("mountingRotation", "0")) || 0;
        root.currentPosition = pos;
        root.currentRotation = rot;
        root.isCustom = false;
    }

    Component.onCompleted: root.restorePreset()

    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            // User preset takes priority over video orientation.
            // Only update UI if no saved preset exists (first launch scenario).
            if (additional_data.imu_orientation) {
                root.lastKnownOrientation = additional_data.imu_orientation;
                // Check if video orientation matches a preset, update UI display only
                const preset = MountingOrientation.reverseMapping(additional_data.imu_orientation);
                if (preset) {
                    // Don't override user's saved preset — just note the match
                } else {
                    // Video has a non-preset orientation
                }
            }
        }
        function onOrientation_guessed(value: string): void {
            // Guess overrides user preset (user actively triggered Guess)
            root.updateFromOrientation(value);
        }
    }

    // ── Position buttons ──
    Row {
        width: parent.width;
        spacing: 8 * dpiScale;

        Repeater {
            model: root.positions
            delegate: Rectangle {
                id: posBtn
                required property var modelData
                required property int index
                width: (parent.width - (root.positions.length - 1) * parent.spacing) / root.positions.length
                height: 30 * dpiScale
                radius: 6 * dpiScale
                color: root.currentPosition === modelData.face && !root.isCustom
                    ? Qt.rgba(styleAccentColor.r, styleAccentColor.g, styleAccentColor.b, 0.18)
                    : posArea.containsMouse ? Qt.lighter(styleButtonColor, 1.2) : styleButtonColor
                border.width: root.currentPosition === modelData.face && !root.isCustom ? 1 : 0
                border.color: styleAccentColor
                scale: posArea.pressed ? 0.97 : 1.0
                Ease on scale { duration: 200; }

                BasicText {
                    anchors.centerIn: parent;
                    text: posBtn.modelData.label;
                    font.pixelSize: 12 * dpiScale;
                    color: styleTextColor;
                    leftPadding: 0;
                }
                MouseArea {
                    id: posArea;
                    anchors.fill: parent;
                    hoverEnabled: true;
                    cursorShape: Qt.PointingHandCursor;
                    onClicked: {
                        root.currentPosition = posBtn.modelData.face;
                        root.applyPreset();
                    }
                }
            }
        }
    }

    // ── 3D camera illustration ──
    Item {
        width: parent.width;
        height: 140 * dpiScale;

        Image {
            id: mountingImage;
            anchors.centerIn: parent;
            height: parent.height - 6 * dpiScale;
            fillMode: Image.PreserveAspectFit;
            source: root.isCustom ? "" : root.mountingSvgSource();
            visible: !root.isCustom;
            opacity: 1.0;
            Ease on opacity { duration: 300; }
            onSourceChanged: { opacity = 0.3; opacity = 1.0; }
        }

        // Custom state label
        BasicText {
            anchors.centerIn: parent;
            visible: root.isCustom;
            text: qsTr("Custom");
            font.pixelSize: 13 * dpiScale;
            color: styleTextColor;
            opacity: 0.5;
            leftPadding: 0;
        }
    }

    // ── Rotation buttons ──
    Row {
        width: parent.width;
        spacing: 8 * dpiScale;

        Repeater {
            model: root.rotations
            delegate: Rectangle {
                id: rotBtn
                required property var modelData
                required property int index
                width: (parent.width - (root.rotations.length - 1) * parent.spacing) / root.rotations.length
                height: 30 * dpiScale
                radius: 6 * dpiScale
                color: root.currentRotation === modelData.angle && !root.isCustom
                    ? Qt.rgba(styleAccentColor.r, styleAccentColor.g, styleAccentColor.b, 0.18)
                    : rotArea.containsMouse ? Qt.lighter(styleButtonColor, 1.2) : styleButtonColor
                border.width: root.currentRotation === modelData.angle && !root.isCustom ? 1 : 0
                border.color: styleAccentColor
                scale: rotArea.pressed ? 0.97 : 1.0
                Ease on scale { duration: 200; }

                BasicText {
                    anchors.centerIn: parent;
                    text: rotBtn.modelData.label;
                    font.pixelSize: 12 * dpiScale;
                    color: styleTextColor;
                    leftPadding: 0;
                }
                MouseArea {
                    id: rotArea;
                    anchors.fill: parent;
                    hoverEnabled: true;
                    cursorShape: Qt.PointingHandCursor;
                    onClicked: {
                        root.currentRotation = rotBtn.modelData.angle;
                        root.applyPreset();
                    }
                }
            }
        }
    }
}
