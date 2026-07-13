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
    opened: false;
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
        const angles = root.currentMode === "custom"
            ? [root.customPitch, root.customRoll, root.customYaw]
            : root.presetAngles[root.currentMode];
        if (angles) {
            controller.set_imu_rotation(angles[0], angles[1], angles[2]);
            // Mounting is a global device property: propagate to every queued
            // job too, so jobs enqueued before this change follow it (the
            // enqueue-time snapshot only covers later ones). No-op per job
            // when the value is unchanged.
            render_queue.apply_mounting_rotation_to_all(angles[0], angles[1], angles[2]);
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

    // Adopt the mounting rotation stored in a loaded .gyroflow project: the
    // project reflects the device orientation of that shooting session, so the
    // UI and the global setting both follow it. A null/absent rotation is "no
    // statement" (same semantics as the import guard) and leaves the current
    // state untouched.
    function loadGyroflow(obj: var): void {
        const gyro = obj.gyro_source || {};
        const rot = gyro.rotation;
        if (!rot || rot.length !== 3) return;
        let mode = "custom";
        for (const key in root.presetAngles) {
            const a = root.presetAngles[key];
            if (a[0] === rot[0] && a[1] === rot[1] && a[2] === rot[2]) { mode = key; break; }
        }
        if (mode === "custom") {
            root.customPitch = rot[0];
            root.customRoll  = rot[1];
            root.customYaw   = rot[2];
        }
        root.currentMode = mode;
        const idx = root.modeKeys.indexOf(mode);
        modeCombo.currentIndex = idx >= 0 ? idx : 0;
        root.saveSettings();
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
        // A project loaded before this selector finished its async load left
        // its rotation parked — adopt it now (overrides restored settings).
        if (window.pendingMountingRotation) {
            root.loadGyroflow({ gyro_source: { rotation: window.pendingMountingRotation } });
            window.pendingMountingRotation = null;
        }
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
