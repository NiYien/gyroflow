// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

import QtQuick

import "../components/"

Column {
    id: root;
    width: parent.width;
    spacing: 5 * dpiScale;

    property alias horizonCb: horizonCb;
    property alias horizonSlider: horizonSlider;
    property alias smoothnessSlider: smoothnessSlider;
    property alias croppingMode: croppingMode;
    property alias lensCorrectionToggle: lensCorrectionToggle;

    readonly property bool _batchActive: window.batchState && window.batchState.active

    function updateHorizonLock(): void {
        if (window.batchState && window.batchState.active) return;
        const lockAmount = horizonCb.checked ? horizonSlider.value : 0.0;
        const roll = horizonCb.checked ? horizonRollSlider.value : 0.0;
        controller.set_horizon_lock(lockAmount, roll, false, 0, false, 5.0, 500.0, 1.0, Infinity);
        controller.set_use_gravity_vectors(false);
        controller.set_horizon_lock_integration_method(1);
    }

    // ── Smoothness ──
    Label {
        position: Label.LeftPosition;
        text: qsTranslate("Stabilization", "Smoothness");
        width: parent.width;
        SliderWithField {
            id: smoothnessSlider;
            width: parent.width;
            from: 0.1;
            to: 100.0;
            value: 50.0;
            defaultValue: 50.0;
            unit: qsTr("%");
            precision: 1;
            scaler: 1.0;
            keyframe: "SmoothingParamSmoothness";
            onValueChanged: {
                if (window.batchState && window.batchState.active) return;
                controller.set_smoothing_param("smoothness", value / 100.0);
            }
        }
    }

    // ── Horizon Lock ──
    CheckBoxWithContent {
        id: horizonCb;
        text: qsTranslate("Stabilization", "Lock horizon");
        cb.onCheckedChanged: Qt.callLater(root.updateHorizonLock);

        Label {
            position: Label.LeftPosition;
            text: qsTranslate("Stabilization", "Lock amount", "Horizon locking amount");
            width: parent.width;
            SliderWithField {
                id: horizonSlider;
                defaultValue: 100;
                to: 100;
                width: parent.width;
                unit: qsTr("%");
                precision: 0;
                value: 100;
                keyframe: "LockHorizonAmount";
                onValueChanged: Qt.callLater(root.updateHorizonLock);
            }
        }

        Label {
            position: Label.LeftPosition;
            width: parent.width;
            text: qsTranslate("Stabilization", "Roll angle correction");
            SliderWithField {
                id: horizonRollSlider;
                enabled: !root._batchActive;
                opacity: root._batchActive ? 0.4 : 1.0;
                width: parent.width;
                from: -180;
                to: 180;
                value: 0;
                defaultValue: 0;
                unit: qsTr("°");
                precision: 1;
                keyframe: "LockHorizonRoll";
                onValueChanged: Qt.callLater(root.updateHorizonLock);
            }
        }
    }

    // ── Zoom Mode ──
    ComboBox {
        id: croppingMode;
        currentIndex: 1;
        font.pixelSize: 12 * dpiScale;
        width: parent.width;
        model: [QT_TRANSLATE_NOOP("Popup", "No zooming"), QT_TRANSLATE_NOOP("Popup", "Dynamic zooming"), QT_TRANSLATE_NOOP("Popup", "Static zoom")];
        onCurrentIndexChanged: {
            if (window.batchState && window.batchState.active) return;
            switch (currentIndex) {
                case 0: controller.adaptive_zoom = 0.0; break;
                case 1: controller.adaptive_zoom = 4.0; break;
                case 2: controller.adaptive_zoom = -1.0; break;
            }
        }
    }

    // ── Lens Correction Toggle ──
    CheckBox {
        id: lensCorrectionToggle;
        text: qsTranslate("Stabilization", "Lens correction");
        checked: true;
        onCheckedChanged: {
            if (window.batchState && window.batchState.active) return;
            controller.lens_correction_amount = checked ? 100.0 : 0.0;
        }
    }

    // Batch-only: framerate override
    Label {
        visible: root._batchActive;
        position: Label.LeftPosition;
        text: qsTr("Frame rate (0=unchanged)");
        width: parent.width;
        NumberField {
            id: simpleBatchFramerateField;
            width: parent.width;
            value: window.batchState ? window.batchState.framerate : 0;
            defaultValue: 0;
            from: 0; to: 240;
            unit: "fps";
            onValueChanged: { if (window.batchState) window.batchState.framerate = value; }
        }
    }

    // ── Rolling Shutter ──
    CheckBoxWithContent {
        id: shutterCb;
        text: qsTranslate("Stabilization", "Rolling shutter correction");
        enabled: !root._batchActive; opacity: root._batchActive ? 0.4 : 1.0;
        cb.onCheckedChanged: {
            controller.frame_readout_time = cb.checked ? shutter.value : 0.0;
        }

        Label {
            position: Label.LeftPosition;
            text: qsTranslate("Stabilization", "Frame readout time");
            width: parent.width;
            SliderWithField {
                id: shutter;
                defaultValue: 0;
                from: 0.0;
                to: 1000 / Math.max(1, window.videoArea.timeline.scaledFps);
                width: parent.width;
                unit: qsTr("ms");
                precision: 2;
                onValueChanged: {
                    controller.frame_readout_time = value;
                }
            }
        }
    }

    // ── Sync with Full mode ──
    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            if (is_main_video) {
                if (Math.abs(+additional_data.frame_readout_time) > 0) {
                    shutter.value = Math.abs(+additional_data.frame_readout_time);
                    shutterCb.checked = true;
                }
            }
        }
        function onRolling_shutter_estimated(rolling_shutter: real): void {
            shutter.value = Math.abs(rolling_shutter);
            shutterCb.checked = Math.abs(rolling_shutter) > 0;
        }
    }
}
