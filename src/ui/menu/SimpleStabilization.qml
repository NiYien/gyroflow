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
    property alias autoRotateCb: autoRotateCb;

    readonly property var _batchGyroFilesInfo: {
        if (window && window.videoArea && window.videoArea.queue && window.videoArea.queue.gyroFilesInfo) {
            return window.videoArea.queue.gyroFilesInfo;
        }
        return [];
    }
    readonly property string _detectedGyroSource: {
        if (root._batchActive) {
            for (const info of root._batchGyroFilesInfo) {
                if (root.isSenseFlowSource(info.detected_source || "")) {
                    return info.detected_source || "";
                }
            }
            if (window && window.batchState && window.batchState.detectedSource) {
                return window.batchState.detectedSource;
            }
        }
        if (window && window.motionData && window.motionData.detectedFormat) {
            return window.motionData.detectedFormat;
        }
        return "";
    }
    readonly property bool isSenseFlow: root.isSenseFlowSource(root._detectedGyroSource)
    property int _baselineRotation: 0
    property int _baselineOutWidth: 0
    property int _baselineOutHeight: 0
    property bool _autoRotateApplied: false
    property int _autoRotationDeg: 0
    property bool _metadataRotationApplied: false
    property bool _syncingBatchAutoRotate: false

    readonly property bool _batchActive: window.batchState && window.batchState.active

    function updateHorizonLock(): void {
        if (window.batchState && window.batchState.active) return;
        const lockAmount = horizonCb.checked ? horizonSlider.value : 0.0;
        const roll = horizonCb.checked ? horizonRollSlider.value : 0.0;
        controller.set_horizon_lock(lockAmount, roll, false, 0, false, 5.0, 500.0, 1.0, Infinity);
        controller.set_use_gravity_vectors(false);
        controller.set_horizon_lock_integration_method(1);
    }

    function isSenseFlowSource(source: string): bool {
        return (source || "").indexOf("SenseFlow") >= 0;
    }

    function readoutDirectionToInt(direction: var): int {
        if (typeof direction === "number") {
            return +direction;
        }
        switch (direction) {
            case "BottomToTop": return 1;
            case "LeftToRight": return 2;
            case "RightToLeft": return 3;
            default: return 0;
        }
    }

    function setDisplayedRotation(rotation: int): void {
        if (window.vidInfo) {
            window.vidInfo.videoRotation = rotation;
            window.vidInfo.updateEntry("Rotation", rotation + " °");
        }
        controller.set_video_rotation(rotation);
    }

    function captureAutoRotateBaseline(): void {
        _baselineRotation = window.vidInfo ? +(window.vidInfo.metadataRotation || 0) : 0;
        _baselineOutWidth = window.videoArea.outWidth || window.exportSettings.originalWidth || window.videoArea.vid.videoWidth;
        _baselineOutHeight = window.videoArea.outHeight || window.exportSettings.originalHeight || window.videoArea.vid.videoHeight;
        _autoRotateApplied = false;
    }

    function applyAutoRotation(): void {
        if (root._batchActive) return;
        if (root._metadataRotationApplied) return;

        let outWidth = _baselineOutWidth;
        let outHeight = _baselineOutHeight;
        if (_autoRotationDeg === 90 || _autoRotationDeg === 270) {
            outWidth = _baselineOutHeight;
            outHeight = _baselineOutWidth;
        }

        root.setDisplayedRotation(_autoRotationDeg);
        window.videoArea.outWidth = outWidth;
        window.videoArea.outHeight = outHeight;
        controller.set_output_size(outWidth, outHeight);
        _autoRotateApplied = true;
    }

    function revertAutoRotation(): void {
        if (root._batchActive) return;
        if (root._metadataRotationApplied) return;

        root.setDisplayedRotation(_baselineRotation);
        window.videoArea.outWidth = _baselineOutWidth;
        window.videoArea.outHeight = _baselineOutHeight;
        controller.set_output_size(_baselineOutWidth, _baselineOutHeight);
        _autoRotateApplied = false;
    }

    function loadGyroflow(obj: var): void {
        const stab = obj && obj.stabilization ? obj.stabilization : null;
        if (!stab || typeof stab.frame_readout_time === "undefined") {
            return;
        }

        const importedTime = Math.abs(+stab.frame_readout_time || 0);
        const importedDirection = readoutDirectionToInt(stab.frame_readout_direction);

        shutter.value = importedTime;
        shutterCb.checked = importedTime > 0;
        controller.frame_readout_direction = importedDirection;
        controller.frame_readout_time = shutterCb.checked ? importedTime : 0.0;
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

    CheckBox {
        id: autoRotateCb;
        visible: root.isSenseFlow;
        text: qsTranslate("Stabilization", "Auto rotate");
        checked: false;
        onCheckedChanged: {
            if (root._batchActive) {
                if (root._syncingBatchAutoRotate) return;
                if (window.batchState) {
                    window.batchState.autoRotate = checked;
                }
                if (window.videoArea && window.videoArea.queue) {
                    const jobIds = Object.keys(window.videoArea.queue.selectedJobs || {}).map(Number);
                    if (jobIds.length > 0) {
                        render_queue.set_batch_auto_rotate(JSON.stringify(jobIds), checked);
                        if (render_queue.has_match_results()) {
                            render_queue.reapply_batch_auto_rotate(JSON.stringify(jobIds));
                        }
                        window.videoArea.queue.matchVersion++;
                    }
                }
                return;
            }
            if (root._metadataRotationApplied) return;
            if (checked) {
                root.applyAutoRotation();
            } else if (!checked && root._autoRotateApplied) {
                root.revertAutoRotation();
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
        function onGyroflow_file_loaded(obj: var): void {
            root.loadGyroflow(obj);
        }
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            if (is_main_video) {
                const isSenseFlow = root.isSenseFlowSource(root._batchActive ? window.batchState.detectedSource : camera);
                root.captureAutoRotateBaseline();
                root._autoRotationDeg = 0;
                root._metadataRotationApplied = false;

                const metaRot = root._baselineRotation;
                const isR3D = (filename || "").toLowerCase().endsWith(".r3d");
                if (!isR3D && metaRot !== 0 && !root._batchActive) {
                    // Priority 1: video metadata rotation
                    // Dimensions already swapped in videoInfoLoaded (via VideoInformation),
                    // so just mark as applied — no dimension swap needed here.
                    root._autoRotationDeg = metaRot;
                    root._metadataRotationApplied = true;
                    root._autoRotateApplied = true;
                } else if (!isR3D && isSenseFlow && additional_data && additional_data.auto_rotation_deg !== undefined) {
                    // Priority 2: gyroscope rotation → only apply if checkbox is on
                    root._autoRotationDeg = +additional_data.auto_rotation_deg;
                    if (autoRotateCb.checked && !root._batchActive) {
                        root.applyAutoRotation();
                    }
                }
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
