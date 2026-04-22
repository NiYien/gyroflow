// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

pragma ComponentBehavior: Bound

import QtQuick

import "../components/"

MenuItem {
    id: root;
    text: qsTr("Mounting position");
    iconName: "axes";
    objectName: "simple-mounting";
    opened: true;
    btnHeight: 28 * dpiScale;

    property string currentMode: ""
    property real customPitch: 0
    property real customRoll: 0
    property real customYaw: 0
    property bool initialized: false

    readonly property var presetAngles: ({
        "top":    [0, 0, 0],
        "bottom": [0, 180, 0],
        "left":   [0, -90, 0],
        "right":  [0, 90, 0]
    })
    readonly property var modeKeys:   ["top", "bottom", "left", "right", "custom"]
    readonly property var modeLabels: [qsTr("Top"), qsTr("Bottom"), qsTr("Left"), qsTr("Right"), qsTr("Custom")]

    function applyMode(): void {
        if (!root.initialized) return;
        if (root.currentMode === "custom") {
            controller.set_imu_rotation(root.customPitch, root.customRoll, root.customYaw);
        } else {
            const angles = root.presetAngles[root.currentMode];
            if (angles) {
                controller.set_imu_rotation(angles[0], angles[1], angles[2]);
            }
        }
        Qt.callLater(controller.recompute_gyro);
        root.saveSettings();
    }

    function saveSettings(): void {
        settings.setValue("mountingMode", root.currentMode);
        settings.setValue("mountingCustomPitch", root.customPitch.toString());
        settings.setValue("mountingCustomRoll", root.customRoll.toString());
        settings.setValue("mountingCustomYaw", root.customYaw.toString());
    }

    function restoreSettings(): void {
        let mode = settings.value("mountingMode", "");
        if (!mode) {
            // Migration from old settings
            const oldPos = settings.value("mountingPosition", "top");
            const oldRot = parseInt(settings.value("mountingRotation", "0")) || 0;
            mode = (oldRot === 0 && root.presetAngles.hasOwnProperty(oldPos)) ? oldPos : "custom";
            settings.setValue("mountingMode", mode);
        }
        root.currentMode = mode;
        root.customPitch = parseFloat(settings.value("mountingCustomPitch", "0")) || 0;
        root.customRoll  = parseFloat(settings.value("mountingCustomRoll", "0")) || 0;
        root.customYaw   = parseFloat(settings.value("mountingCustomYaw", "0")) || 0;

        // Sync ComboBox index
        const idx = root.modeKeys.indexOf(root.currentMode);
        modeCombo.currentIndex = idx >= 0 ? idx : 0;
    }

    Component.onCompleted: {
        root.restoreSettings();
        root.initialized = true;
        root.applyMode();
    }

    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            // Re-apply mounting rotation after MotionData clears it
            Qt.callLater(root.applyMode);
        }
    }

    // ── Mode selector ──
    ComboBox {
        id: modeCombo;
        model: root.modeLabels;
        font.pixelSize: 12 * dpiScale;
        width: parent.width;
        currentIndex: 0;
        onCurrentIndexChanged: {
            if (!root.initialized) return;
            root.currentMode = root.modeKeys[currentIndex];
            root.applyMode();
        }
    }

    // ── Custom rotation angles (visible only in custom mode) ──
    Flow {
        width: parent.width;
        spacing: 5 * dpiScale;
        visible: root.currentMode === "custom";

        Label {
            position: Label.LeftPosition;
            text: qsTr("Pitch");
            width: undefined;
            inner.width: 50 * dpiScale;
            spacing: 5 * dpiScale;
            NumberField {
                id: pitchField;
                unit: "°";
                precision: 1;
                from: -360;
                to: 360;
                width: 50 * dpiScale;
                value: root.customPitch;
                onValueChanged: {
                    if (!root.initialized) return;
                    root.customPitch = value;
                    root.applyMode();
                }
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Roll");
            width: undefined;
            inner.width: 50 * dpiScale;
            spacing: 5 * dpiScale;
            NumberField {
                id: rollField;
                unit: "°";
                precision: 1;
                from: -360;
                to: 360;
                width: 50 * dpiScale;
                value: root.customRoll;
                onValueChanged: {
                    if (!root.initialized) return;
                    root.customRoll = value;
                    root.applyMode();
                }
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Yaw");
            width: undefined;
            inner.width: 50 * dpiScale;
            spacing: 5 * dpiScale;
            NumberField {
                id: yawField;
                unit: "°";
                precision: 1;
                from: -360;
                to: 360;
                width: 50 * dpiScale;
                value: root.customYaw;
                onValueChanged: {
                    if (!root.initialized) return;
                    root.customYaw = value;
                    root.applyMode();
                }
            }
        }
    }
}
