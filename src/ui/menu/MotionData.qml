// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Dialogs

import "../components/"

MenuItem {
    id: root;
    text: qsTr("Motion data");
    iconName: "chart";
    loader: controller.loading_gyro_in_progress;
    objectName: "motiondata";

    property alias hasQuaternions: integrator.hasQuaternions;
    // Guards the rotation setters against the change signals fired while this
    // component constructs asynchronously: the initial (0,0,0) writes raced
    // with (and cleared) the mounting rotation MountingPresetSelector had just
    // applied to the main stab at startup.
    property bool initialized: false;
    Component.onCompleted: root.initialized = true;
    property bool hasAccurateTimestamps: false;
    property alias hasRawGyro: integrator.hasRawGyro;
    property alias integrationMethod: integrator.currentIndex;
    property alias orientationIndicator: orientationIndicator;
    property bool allMetadata: allMetadataCb.visible && allMetadataCb.checked;
    property string filename: "";
    property string detectedFormat: "";
    property url lastSelectedFile: "";
    property alias extensions: fileDialog.extensions;

    FileDialog {
        id: fileDialog;
        property var extensions: [ "csv", "txt", "bbl", "bfl", "mp4", "mov", "mxf", "insv", "gcsv", "360", "log", "bin", "braw", "r3d", "nev", "gpmf", "crm" ];

        title: qsTr("Choose a motion data file")
        nameFilters: Qt.platform.os == "android"? undefined : [qsTr("Motion data files") + " (*." + extensions.concat(extensions.map(x => x.toUpperCase())).join(" *.") + ")"];
        type: "video";
        onAccepted: loadFile(selectedFile);
    }
    function openFileDialog(): void {
        fileDialog.open2();
    }
    function loadFile(url: url): void {
        if (!window.videoArea.vid.loaded) {
            messageBox(Modal.Error, qsTr("Video file is not loaded."), [ { text: qsTr("Ok"), accent: true } ]);
            return;
        }
        root.cancelOpticalEdits();
        lastSelectedFile = url;
        controller.load_telemetry(url, root.allMetadata, window.videoArea.vid, currentLog.visible && currentLog.currentIndex > 0? currentLog.currentIndex - 1 : -1, 0);
    }

    // Qt signals can call back synchronously while a batch of user choices is being applied.
    property bool updatingOpticalControls: false;
    property bool hasExplicitOpticalRequest: false;
    function cancelOpticalEdits(): void {
        opticalStrengthTimer.stop();
        translationReferenceTimer.stop();
        translationSmoothnessTimer.stop();
        translationAlongAxisTimer.stop();
    }
    function refreshOpticalInfo(restoreParameters: bool, restoreOpticalRequest: bool): void {
        if (root.updatingOpticalControls) return;
        if (restoreParameters) {
            root.cancelOpticalEdits();
            // Keep the payload's explicit optical choice through later statistics refreshes.
            root.hasExplicitOpticalRequest = restoreOpticalRequest;
        }
        root.updatingOpticalControls = true;
        try {
            opticalcb.info = JSON.parse(controller.optical_correction_info());
            translationcb.info = JSON.parse(controller.translation_stabilization_info());
            stabcb.info = JSON.parse(controller.stab_reconstruction_info());
            translationcb.checked = !!translationcb.info.requested;
            stabcb.checked = !!stabcb.info.requested;
            // Without a result, keep the visible optical choice until Analyze explicitly submits it.
            opticalcb.checked = !stabcb.checked && (restoreOpticalRequest ? !!opticalcb.info.requested :
                (!root.hasExplicitOpticalRequest && !!opticalcb.info.ignore_file_motion) ||
                (opticalcb.info.available ? !!opticalcb.info.enabled : opticalcb.checked));
            ignoreFileMotion.checked = !!opticalcb.info.ignore_file_motion;
            // Statistics refreshes must not replace edits still waiting for their debounce timers.
            if (restoreParameters || !translationReferenceTimer.running)
                translationReference.value = translationcb.info.reference * 100;
            if (restoreParameters || !translationSmoothnessTimer.running)
                translationSmoothness.value = Math.log(translationcb.info.smoothness_s) / Math.LN10;
            if (restoreParameters || !translationAlongAxisTimer.running)
                translationAlongAxis.checked = !!translationcb.info.along_axis;
            if (restoreParameters) opticalStrength.value = opticalcb.info.strength * 100;
        } finally {
            root.updatingOpticalControls = false;
        }
    }
    function restoreOpticalControls(restoreOpticalRequest: bool): void { root.refreshOpticalInfo(true, restoreOpticalRequest); }
    function changeOpticalMode(mode: string, enabled: bool): void {
        if (!root.initialized || root.updatingOpticalControls) return;
        root.updatingOpticalControls = true;
        try {
            if (mode === "stab") {
                if (enabled) {
                    opticalcb.checked = false;
                    translationcb.checked = false;
                    // An explicit optical-off action restores the file motion, unlike project readback.
                    controller.set_optical_correction_enabled(false);
                    controller.set_ignore_file_motion(false);
                }
                controller.set_stab_reconstruction_enabled(enabled);
            } else if (mode === "translation") {
                if (enabled) stabcb.checked = false;
                controller.set_translation_stabilization_enabled(enabled);
            } else {
                if (enabled) stabcb.checked = false;
                controller.set_optical_correction_enabled(enabled);
                if (!enabled) controller.set_ignore_file_motion(false);
            }
        } finally {
            root.updatingOpticalControls = false;
        }
        root.refreshOpticalInfo(false, false);
    }
    function analyzeOpticalModes(): void {
        const optical = opticalcb.checked;
        const translation = translationcb.checked;
        const reconstruction = stabcb.checked;
        root.updatingOpticalControls = true;
        try {
            // Submit the visible choices even when a checkbox has never changed from its initial false value.
            controller.set_optical_correction_enabled(optical);
            if (!optical) controller.set_ignore_file_motion(false);
            controller.set_translation_stabilization_enabled(translation);
            controller.set_stab_reconstruction_enabled(reconstruction);
        } finally {
            root.updatingOpticalControls = false;
        }
        root.refreshOpticalInfo(false, false);
        controller.analyze_optically();
    }
    function loadGyroflow(obj: var): void {
        root.cancelOpticalEdits();
        const gyro = obj.gyro_source || { };
        // Rebuilding the model resets the selection to its first entry (Complementary without quaternions),
        // so keep the current method unless the payload explicitly sets one.
        const currentMethod = integrator.hasQuaternions? integrator.currentIndex : integrator.currentIndex + 1;
        integrator.hasRawGyro = controller.gyro_has_raw_imu;
        integrator.hasQuaternions = !controller.gyro_has_quaternions;
        integrator.hasQuaternions = controller.gyro_has_quaternions;
        root.hasAccurateTimestamps = controller.gyro_has_accurate_timestamps;
        let method = gyro.hasOwnProperty("integration_method")? +gyro.integration_method : currentMethod;
        // Built-in quaternions ("None") only exist in the list when the motion source provides them.
        if (!integrator.hasQuaternions && method < 1) method = 2; // VQF
        integrator.currentIndex = integrator.hasQuaternions? method : method - 1;
        if (gyro && Object.keys(gyro).length > 0) {
            if (gyro.rotation && gyro.rotation.length == 3) {
                p.value = gyro.rotation[0];
                r.value = gyro.rotation[1];
                y.value = gyro.rotation[2];
                rot.checked = Math.abs(p.value) > 0 || Math.abs(r.value) > 0 || Math.abs(y.value) > 0;
            }
            if (gyro.acc_rotation && gyro.acc_rotation.length == 3) {
                ap.value = gyro.acc_rotation[0];
                ar.value = gyro.acc_rotation[1];
                ay.value = gyro.acc_rotation[2];
                arot.checked = Math.abs(ap.value) > 0 || Math.abs(ar.value) > 0 || Math.abs(ay.value) > 0;
                arot_action.checked = arot.checked;
            }
            if (gyro.imu_orientation) orientation.text = gyro.imu_orientation;
            if (+gyro.lpf > 0) {
                lpf.value = +gyro.lpf;
                lpfcb.checked = lpf.value > 0;
            }
            root.updatingOpticalControls = true;
            try {
                if (typeof gyro.optical_correction_strength === "number") opticalStrength.value = gyro.optical_correction_strength * 100;
                if (typeof gyro.translation_stabilization_enabled === "boolean") translationcb.checked = gyro.translation_stabilization_enabled;
                if (typeof gyro.translation_reference === "number") translationReference.value = gyro.translation_reference * 100;
                if (typeof gyro.translation_smoothness === "number") translationSmoothness.value = Math.log(gyro.translation_smoothness) / Math.LN10;
                if (typeof gyro.translation_along_axis === "boolean") translationAlongAxis.checked = gyro.translation_along_axis;
                if (typeof gyro.stab_reconstruction_enabled === "boolean") stabcb.checked = gyro.stab_reconstruction_enabled;
            } finally {
                root.updatingOpticalControls = false;
            }
            if (typeof gyro.sample_index === "number") {
                currentLog.currentIndex = gyro.sample_index + 1;
            }
            // [Fix1] 从项目数据中恢复文件名和检测格式（batch match 编辑时）
            if (gyro.detected_source) {
                root.detectedFormat = gyro.detected_source;
                info.updateEntry("Detected format", gyro.detected_source);
            }
            if (gyro.filepath) {
                const fn_ = filesystem.get_filename(gyro.filepath);
                root.lastSelectedFile = gyro.filepath;
                root.filename = fn_;
                info.updateEntry("File name", fn_);
            }
        }
        Qt.callLater(root.restoreOpticalControls, typeof gyro.optical_correction_enabled === "boolean");
        const stab = obj.stabilization || { };
        if (stab && Object.keys(stab).length > 0) {
            focb.checked = +stab.frame_offset > 0;
            fo.value = +stab.frame_offset;
        }
    }
    function setGyroLpf(v: real): void {
        lpf.value = v;
        lpfcb.checked = +v > 0;
    }

    function msToTime(ms: real): string {
        if (ms >= 60*60*1000) {
            return new Date(ms).toISOString().substring(11, 11+8);
        } else {
            return new Date(ms).toISOString().substring(11+3, 11+8);
        }
    }
    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            root.cancelOpticalEdits();
            root.filename = filename || "";
            root.detectedFormat = camera || "";
            info.updateEntry("File name", filename || "---");
            info.updateEntry("Detected format", camera || "---");
            orientation.text = additional_data.imu_orientation;

            // Twice to trigger change signal
            integrator.hasRawGyro = additional_data.contains_raw_gyro;
            integrator.hasQuaternions = !additional_data.contains_quats;
            integrator.hasQuaternions = additional_data.contains_quats;
            root.hasAccurateTimestamps = additional_data.has_accurate_timestamps || false;
            if (additional_data.contains_quats && !is_main_video) {
                if (integrator.hasRawGyro) {
                    integrator.currentIndex = 2;
                } else {
                    integrator.currentIndex = 0;
                }
                integrateTimer.start();
            }
            if (!additional_data.contains_quats) {
                integrator.currentIndex = 1; // Default to VQF
            }

            controller.set_imu_lpf(lpfcb.checked? lpf.value : 0);
            controller.set_imu_median_filter(mfcb.checked? mf.value : 0);
            Qt.callLater(root.restoreOpticalControls, false);
            controller.set_imu_rotation(rot.checked? p.value : 0, rot.checked? r.value : 0, rot.checked? y.value : 0);
            controller.set_acc_rotation(arot.checked? ap.value : 0, arot.checked? ar.value : 0, arot.checked? ay.value : 0);
            Qt.callLater(controller.recompute_gyro);

            Qt.callLater(window.videoArea.timeline.updateDurations);

            currentLog.preventChange = true;
            if (additional_data.usable_logs && additional_data.usable_logs.length > 0) {
                let model = ["All logs combined"];
                for (const log of additional_data.usable_logs) {
                    const [logIndex, startTimestamp, duration] = log.split(";");
                    model.push("#" + (+logIndex + 1) + " | " + msToTime(+startTimestamp) + " - " + msToTime(+startTimestamp + (+duration)) + " (" + msToTime(+duration) + ")");
                }
                if (currentLog.model != model)
                    currentLog.model = model;
            } else {
                currentLog.model = [];
            }
            currentLog.preventChange = false;
        }
        function onBias_estimated(biasX: real, biasY: real, biasZ: real): void {
            gyrobias.checked = true;
            bx.value = biasX;
            by.value = biasY;
            bz.value = biasZ;
        }
        function onOrientation_guessed(value: string): void {
             orientation.text = value;
        }
        function onChart_data_changed(): void {
            Qt.callLater(orientationIndicator.requestPaint);
            Qt.callLater(() => root.refreshOpticalInfo(false, false));
        }
        function onOptical_correction_changed(): void { root.refreshOpticalInfo(false, false); }
        function onVideo_loading_in_progressChanged(): void {
            if (controller.video_loading_in_progress) root.cancelOpticalEdits();
        }
        function onLoading_gyro_in_progressChanged(): void {
            if (controller.loading_gyro_in_progress) root.cancelOpticalEdits();
        }
    }

    Button {
        text: qsTr("Open file");
        iconName: "file-empty"
        anchors.horizontalCenter: parent.horizontalCenter;
        onClicked: root.openFileDialog();
    }
    InfoMessageSmall {
        show: Qt.platform.os == "android" && !root.detectedFormat && root.lastSelectedFile.toString();
        type: InfoMessage.Info;
        text: qsTr("In order to detect multiple motion data files, click here and grant access to the directory with files.");
        OutputPathField { id: opf; visible: false; }
        MouseArea {
            anchors.fill: parent;
            cursorShape: Qt.PointingHandCursor;
            onClicked: {
                opf.selectFolder("", function(_) {
                    root.loadFile(root.lastSelectedFile);
                });
            }
        }
    }

    TableList {
        id: info;

        Component.onCompleted: {
            QT_TRANSLATE_NOOP("TableList", "File name"),
            QT_TRANSLATE_NOOP("TableList", "Detected format")
        }

        model: ({
            "File name": "---",
            "Detected format": "---"
        })
    }
    Label {
        position: Label.LeftPosition;
        text: qsTr("Select log");
        visible: currentLog.count > 1;

        ComboBox {
            id: currentLog;
            property bool preventChange: false;
            model: [QT_TRANSLATE_NOOP("Popup", "All logs combined")];
            font.pixelSize: 12 * dpiScale;
            width: parent.width;
            onCurrentIndexChanged: {
                if (!preventChange && count > 1) {
                    root.loadFile(root.lastSelectedFile);
                }
            }
        }
    }
    CheckBox {
        id: allMetadataCb;
        text: qsTr("Load all metadata");
        visible: root.lastSelectedFile.toString() && root.lastSelectedFile != window.videoArea.loadedFileUrl;
        checked: false;
        onCheckedChanged: allMetadataDebounce.start();
        Timer {
            id: allMetadataDebounce;
            interval: 1;
            running: false;
            repeat: false;
            onTriggered: root.loadFile(root.lastSelectedFile);
        }
    }
    CheckBoxWithContent {
        id: focb;
        text: qsTr("Frame offset");
        visible: allMetadataCb.visible;
        onCheckedChanged: {
            controller.frame_offset = focb.checked? fo.value : 0;
        }
        NumberField {
            id: fo;
            unit: qsTr("frames");
            precision: 0;
            value: 0;
            from: -100000;
            to: 100000;
            width: parent.width;
            tooltip: qsTr("Add or subtract frames from the video to align with motion data");
            onValueChanged: {
                controller.frame_offset = focb.checked? fo.value : 0;
            }
        }
    }
    CheckBoxWithContent {
        id: lpfcb;
        text: qsTr("Low pass filter");
        onCheckedChanged: {
            controller.set_imu_lpf(checked? lpf.value : 0);
            Qt.callLater(controller.recompute_gyro);
        }

        NumberField {
            id: lpf;
            unit: qsTr("Hz");
            precision: 2;
            value: 50;
            from: 0;
            width: parent.width;
            tooltip: qsTr("Lower cutoff frequency means more filtering");
            onValueChanged: {
                controller.set_imu_lpf(lpfcb.checked? value : 0);
                Qt.callLater(controller.recompute_gyro);
            }
        }
    }
    CheckBoxWithContent {
        id: mfcb;
        text: qsTr("Median filter");
        onCheckedChanged: {
            controller.set_imu_median_filter(checked? mf.value : 0);
            Qt.callLater(controller.recompute_gyro);
        }

        NumberField {
            id: mf;
            unit: qsTr("samples");
            precision: 0;
            value: 5;
            from: 0;
            width: parent.width;
            onValueChanged: {
                controller.set_imu_median_filter(mfcb.checked? value : 0);
                Qt.callLater(controller.recompute_gyro);
            }
        }
    }
    CheckBoxWithContent {
        id: opticalcb;
        text: qsTr("Optical correction");
        cb.tooltip: qsTr("Measure the camera rotation from the video itself and correct the motion data where they disagree. Useful when vibrations corrupt the gyro data, e.g. on a hard-mounted FPV camera. The analysis samples frames at an integer interval near 25 fps within the selected trim range.");
        property var info: ({ available: false });
        cb.enabled: !stabcb.checked;
        onCheckedChanged: root.changeOpticalMode("optical", checked);

        BasicText {
            width: parent.width;
            wrapMode: Text.WordWrap;
            horizontalAlignment: Text.AlignHCenter;
            visible: text.length > 0;
            text: opticalcb.info.ignore_file_motion && !opticalcb.info.from_video? qsTr("Click Analyze to measure the motion from the video.") :
                  !opticalcb.info.available? "" :
                  opticalcb.info.stale? qsTr("The motion data, the sync, the lens or the rolling shutter changed since the analysis. Analyze again to apply the correction.") :
                  opticalcb.info.outdated && opticalcb.info.has_motion? qsTr("Analyze again to apply the new strength.") :
                  opticalcb.info.from_video? "" :
                  qsTr("Measured in %1 of %2 frames, correction %3°").arg(opticalcb.info.measured_frames).arg(opticalcb.info.frames).arg((+opticalcb.info.rms_deg).toFixed(3));
        }
        CheckBox {
            id: ignoreFileMotion;
            text: qsTr("Ignore motion data from the file");
            tooltip: qsTr("Measure all of the camera motion from the video, like for a file without motion data, instead of correcting the motion data. For motion data too broken to correct, e.g. a gyro that glitches or saturates for seconds at a time.");
            onCheckedChanged: if (root.initialized && !root.updatingOpticalControls) controller.set_ignore_file_motion(checked);
        }

        Label {
            text: qsTr("Strength");
            width: parent.width;
            // Without motion data to correct (none in the file, or ignored), the motion and its correction come from
            // the same tracks: the correction only refines within the frames, the strength changes next to nothing
            visible: !!opticalcb.info.has_motion;
            tooltip: qsTr("How far the correction may take the motion data from what it says. Lower values only correct small, fast errors like vibration. Higher values let the image override the motion data also where it's off by a lot or for longer, like gyro glitches, but follow the image's own mistakes (moving objects, water) more too.");
            SliderWithField {
                id: opticalStrength;
                defaultValue: 50;
                to: 100;
                value: 50;
                unit: "%";
                precision: 0;
                width: parent.width;
                // Each change refits the correction (up to half a second on a long clip): not on every step of a drag
                onValueChanged: if (root.initialized && !root.updatingOpticalControls) opticalStrengthTimer.restart();
                Timer {
                    id: opticalStrengthTimer;
                    interval: 150;
                    onTriggered: if (!controller.video_loading_in_progress && !controller.loading_gyro_in_progress) controller.set_optical_correction_strength(opticalStrength.value / 100);
                }
            }
        }
    }
    CheckBoxWithContent {
        id: translationcb;
        text: qsTr("Translation stabilization");
        cb.tooltip: qsTr("Measure from the video how the camera moved sideways and up and down, and shift the whole picture to hold one distance steady. Needs motion data from the file; the analysis samples frames at an integer interval near 25 fps within the selected trim range. Can't be used together with in-camera stabilization reconstruction. The reported shift is sampled at frame centers; output cropping changes its apparent size.");
        property var info: ({ available: false });
        onCheckedChanged: root.changeOpticalMode("translation", checked);
        BasicText {
            width: parent.width;
            wrapMode: Text.WordWrap;
            horizontalAlignment: Text.AlignHCenter;
            text: !translationcb.info.has_motion || translationcb.info.ignore_file_motion ? qsTr("Needs motion data from the file") :
                  !translationcb.info.available && translationcb.info.requested && translationcb.info.analyzed_without ? qsTr("Analyze again to measure the camera movement") :
                  !translationcb.info.available && translationcb.info.requested ? qsTr("Click Analyze to measure the camera movement") :
                  translationcb.info.stale ? qsTr("Settings changed, analyze again") :
                  !translationcb.info.available ? "" :
                  qsTr("Measured in %1 of %2 frames, applied shift up to %3% of the source frame's short side").arg(translationcb.info.measured_frames).arg(translationcb.info.frames).arg((+translationcb.info.max_shift_pct).toFixed(1));
        }
        Label {
            text: qsTr("Reference distance");
            width: parent.width;
            tooltip: qsTr("Which distance to hold steady: 0% keeps the picture as the gyro stabilization has it, 100% steadies the middle of the tracked points, higher values nearer objects.");
            SliderWithField {
                id: translationReference;
                width: parent.width;
                from: 0; to: 200; defaultValue: 100; value: 100; precision: 0; unit: "%";
                onValueChanged: if (root.initialized && !root.updatingOpticalControls) translationReferenceTimer.restart();
            }
        }
        Label {
            text: qsTr("Translation smoothness");
            width: parent.width;
            tooltip: qsTr("Low values only remove fast shakes. High values also remove slow drifts and come close to locking the picture.");
            Row {
                width: parent.width;
                spacing: 5 * dpiScale;
                Slider {
                    id: translationSmoothness;
                    width: parent.width - translationSmoothnessValue.width - parent.spacing;
                    from: -1; to: 1; value: 0;
                    onValueChanged: if (root.initialized && !root.updatingOpticalControls) translationSmoothnessTimer.restart();
                }
                BasicText {
                    id: translationSmoothnessValue;
                    width: 55 * dpiScale;
                    anchors.verticalCenter: parent.verticalCenter;
                    text: qsTr("%1 s").arg(Math.pow(10, translationSmoothness.value).toFixed(1));
                }
            }
        }
        CheckBox {
            id: translationAlongAxis;
            text: qsTr("Compensate movement along the lens axis");
            onCheckedChanged: if (root.initialized && !root.updatingOpticalControls) translationAlongAxisTimer.restart();
        }
    }
    Timer {
        id: translationReferenceTimer;
        interval: 150;
        onTriggered: if (!controller.video_loading_in_progress && !controller.loading_gyro_in_progress) controller.set_translation_reference(translationReference.value / 100);
    }
    Timer {
        id: translationSmoothnessTimer;
        interval: 150;
        onTriggered: if (!controller.video_loading_in_progress && !controller.loading_gyro_in_progress) controller.set_translation_smoothness(Math.pow(10, translationSmoothness.value));
    }
    Timer {
        id: translationAlongAxisTimer;
        interval: 150;
        onTriggered: if (!controller.video_loading_in_progress && !controller.loading_gyro_in_progress) controller.set_translation_along_axis(translationAlongAxis.checked);
    }
    CheckBoxWithContent {
        id: stabcb;
        text: qsTr("Reconstruct in-camera stabilization");
        cb.tooltip: qsTr("For footage shot with in-camera stabilization (IBIS or lens OIS) on but without its data in the file: measure what the camera compensated from the video, so it isn't compensated twice. Can't be used together with Translation stabilization or Optical correction.");
        property var info: ({ available: false });
        onCheckedChanged: root.changeOpticalMode("stab", checked);
        BasicText {
            width: parent.width;
            wrapMode: Text.WordWrap;
            horizontalAlignment: Text.AlignHCenter;
            text: !stabcb.info.has_motion || stabcb.info.ignore_file_motion ? qsTr("Needs motion data from the file") :
                  !stabcb.info.available && stabcb.info.requested && stabcb.info.analyzed_without ? qsTr("Analyze again to reconstruct the in-camera stabilization") :
                  !stabcb.info.available && stabcb.info.requested ? qsTr("Click Analyze to reconstruct the in-camera stabilization") :
                  stabcb.info.stale ? qsTr("Settings changed, analyze again") :
                  !stabcb.info.available ? "" :
                  qsTr("Measured in %1 of %2 frames, compensation up to %3°, cut-off %4 Hz").arg(stabcb.info.measured_frames).arg(stabcb.info.frames).arg((+stabcb.info.max_deg).toFixed(2)).arg((+stabcb.info.cutoff_hz).toFixed(2));
        }
    }
    Row {
        anchors.horizontalCenter: parent.horizontalCenter;
        spacing: 5 * dpiScale;
        visible: opticalcb.checked || translationcb.checked || stabcb.checked;
        Button {
            text: qsTr("Analyze");
            iconName: "spinner";
            enabled: !controller.sync_in_progress && window.videoArea.vid.loaded;
            onClicked: root.analyzeOpticalModes();
        }
        LinkButton {
            anchors.verticalCenter: parent.verticalCenter;
            text: qsTr("Clear");
            leftPadding: 6 * dpiScale;
            rightPadding: 6 * dpiScale;
            visible: !!opticalcb.info.available || !!translationcb.info.available || !!stabcb.info.available;
            enabled: !controller.sync_in_progress;
            onClicked: controller.clear_optical_correction();
        }
    }
    Item {
        width: parent.width;
        height: rot.height;
        CheckBoxWithContent {
            id: rot;
            text: qsTr("Rotation");
            onCheckedChanged: update_rotation();
            function update_rotation(): void {
                if (!root.initialized) return;
                controller.set_imu_rotation(rot.checked? p.value : 0, rot.checked? r.value : 0, rot.checked? y.value : 0);
                Qt.callLater(controller.recompute_gyro);
            }
            ContextMenuMouseArea {
                parent: rot.cb;
                cursorShape: Qt.PointingHandCursor;
                onContextMenu: (isHold, x, y) => { contextMenu.popup(rot, x, y); }
            }

            Flow {
                width: parent.width;
                spacing: 5 * dpiScale;
                Label {
                    position: Label.LeftPosition;
                    text: qsTr("Pitch");
                    width: undefined;
                    inner.width: 50 * dpiScale;
                    spacing: 5 * dpiScale;
                    NumberField { id: p; unit: "°"; precision: 1; from: -360; to: 360; width: 50 * dpiScale; onValueChanged: rot.update_rotation(); tooltip: qsTr("Pitch is camera angle up/down when using FPV blackbox data"); }
                }
                Label {
                    position: Label.LeftPosition;
                    text: qsTr("Roll");
                    width: undefined;
                    inner.width: 50 * dpiScale;
                    spacing: 5 * dpiScale;
                    NumberField { id: r; unit: "°"; precision: 1; from: -360; to: 360; width: 50 * dpiScale; onValueChanged: rot.update_rotation(); }
                }
                Label {
                    position: Label.LeftPosition;
                    text: qsTr("Yaw");
                    width: undefined;
                    inner.width: 50 * dpiScale;
                    spacing: 5 * dpiScale;
                    NumberField { id: y; unit: "°"; precision: 1; from: -360; to: 360; width: 50 * dpiScale; onValueChanged: rot.update_rotation(); }
                }
            }
        }
        Menu {
            id: contextMenu;
            font.pixelSize: 11.5 * dpiScale;
            Action {
                id: arot_action;
                iconName: "axes";
                text: qsTr("Separate accelerometer rotation");
                checkable: true;
            }
        }
    }
    CheckBoxWithContent {
        id: arot;
        visible: arot_action.checked;
        text: qsTr("Accelerometer rotation");
        onCheckedChanged: update_rotation();
        function update_rotation(): void {
            if (!root.initialized) return;
            controller.set_acc_rotation(arot.checked? ap.value : 0, arot.checked? ar.value : 0, arot.checked? ay.value : 0);
            Qt.callLater(controller.recompute_gyro);
        }

        Flow {
            width: parent.width;
            spacing: 5 * dpiScale;
            Label {
                position: Label.LeftPosition;
                text: qsTr("Pitch");
                width: undefined;
                inner.width: 50 * dpiScale;
                spacing: 5 * dpiScale;
                NumberField { id: ap; unit: "°"; precision: 1; from: -360; to: 360; width: 50 * dpiScale; onValueChanged: arot.update_rotation(); }
            }
            Label {
                position: Label.LeftPosition;
                text: qsTr("Roll");
                width: undefined;
                inner.width: 50 * dpiScale;
                spacing: 5 * dpiScale;
                NumberField { id: ar; unit: "°"; precision: 1; from: -360; to: 360; width: 50 * dpiScale; onValueChanged: arot.update_rotation(); }
            }
            Label {
                position: Label.LeftPosition;
                text: qsTr("Yaw");
                width: undefined;
                inner.width: 50 * dpiScale;
                spacing: 5 * dpiScale;
                NumberField { id: ay; unit: "°"; precision: 1; from: -360; to: 360; width: 50 * dpiScale; onValueChanged: arot.update_rotation(); }
            }
        }
    }
    CheckBoxWithContent {
        id: gyrobias;
        text: qsTr("Gyro bias");
        onCheckedChanged: update_bias();
        function update_bias(): void {
            controller.set_imu_bias(gyrobias.checked? bx.value : 0, gyrobias.checked? by.value : 0, gyrobias.checked? bz.value : 0);
            Qt.callLater(controller.recompute_gyro);
        }

        Flow {
            width: parent.width;
            spacing: 5 * dpiScale;
            Label {
                position: Label.LeftPosition;
                text: qsTr("X");
                width: undefined;
                inner.width: 65 * dpiScale;
                spacing: 5 * dpiScale;
                NumberField { id: bx; unit: "°/s"; precision: 2; width: 65 * dpiScale; onValueChanged: gyrobias.update_bias(); }
            }
            Label {
                position: Label.LeftPosition;
                text: qsTr("Y");
                width: undefined;
                inner.width: 65 * dpiScale;
                spacing: 5 * dpiScale;
                NumberField { id: by; unit: "°/s"; precision: 2; width: 65 * dpiScale; onValueChanged: gyrobias.update_bias(); }
            }
            Label {
                position: Label.LeftPosition;
                text: qsTr("Z");
                width: undefined;
                inner.width: 65 * dpiScale;
                spacing: 5 * dpiScale;
                NumberField { id: bz; unit: "°/s"; precision: 2; width: 65 * dpiScale; onValueChanged: gyrobias.update_bias(); }
            }
        }
    }
    Label {
        position: Label.LeftPosition;
        text: qsTr("IMU orientation");

        TextField {
            id: orientation;
            width: parent.width;
            text: "XYZ";
            validator: RegularExpressionValidator { regularExpression: /[XYZxyz]{3}/; }
            tooltip: qsTr("Uppercase is positive, lowercase is negative. eg. zYX");
            onTextChanged: if (acceptableInput) { controller.set_imu_orientation(text); Qt.callLater(controller.recompute_gyro); }
        }
    }
    Label {
        position: Label.LeftPosition;
        text: qsTr("Integration method");

        ComboBox {
            id: integrator;
            property bool hasQuaternions: false;
            property bool hasRawGyro: false;
            model: hasQuaternions? [QT_TRANSLATE_NOOP("Popup", "None"), "Complementary", "VQF", "Simple gyro", "Simple gyro + accel", "Mahony", "Madgwick" ] : ["Complementary", "VQF", "Simple gyro", "Simple gyro + accel", "Mahony", "Madgwick"];
            font.pixelSize: 12 * dpiScale;
            width: parent.width;
            tooltip: hasQuaternions && currentIndex === 0? qsTr("Use built-in quaternions instead of IMU data") : qsTr("IMU integration method for calculating motion data");
            function setMethod(): void {
                controller.set_integration_method(hasQuaternions? currentIndex : currentIndex + 1);
            }
            onCurrentIndexChanged: integrateTimer.start();
            onHasQuaternionsChanged: integrateTimer.start();
            Timer {
                id: integrateTimer;
                interval: 300;
                onTriggered: Qt.callLater(integrator.setMethod);
            }
        }
    }

    CheckBoxWithContent {
        id: orientationCheckbox;
        text: qsTr("Orientation indicator");
        onCheckedChanged: Qt.callLater(orientationIndicator.requestPaint);

        Canvas {
            id: orientationIndicator
            width: parent.width
            height: 100
            property real currentTimestamp: 0
            property bool initialDraw: false
            onPaint: {
                if (orientationCheckbox.checked || !initialDraw) {
                    initialDraw = true
                    let ctx = getContext("2d");
                    ctx.reset();
                    const veclen = 30;
                    const xv = Qt.vector3d(0,veclen,0)
                    const yv = Qt.vector3d(-veclen,0,0)
                    const zv = Qt.vector3d(0,0,veclen)
                    const vecs = [xv, yv, zv]
                    const colors = style === "light" ? ['#cc0000', '#00cc00', '#0000cc'] : ['#ff0000', '#00ff00', '#4444ff'];
                    // inspired by blender camera
                    const cam_width = 30;
                    const cam_height = 15;
                    const cam_length = 30;
                    const cam_vertices = [[-cam_width,-cam_height,-cam_length],
                                          [cam_width, -cam_height,-cam_length],
                                          [cam_width, cam_height, -cam_length],
                                          [-cam_width, cam_height, -cam_length],
                                          [0,0,0]]
                    const cam_vert_vecs = cam_vertices.map(vert => Qt.vector3d(vert[0],vert[1],vert[2]))
                    const lines = [[0,1,2,3,0],
                                   [0,4,1],
                                   [2,4,3]]

                    const quats = controller.quats_at_timestamp(Math.round(currentTimestamp))
                    const transform = Qt.quaternion( quats[0], quats[1],  quats[2], quats[3]); // wxyz
                    const maincolor = style === "light" ? "rgba(0,0,0,0.9)" : "rgba(255,255,255,0.9)";
                    const transform_smooth = transform.times(Qt.quaternion( quats[4], quats[5],  quats[6], quats[7]).inverted());
                    const transforms = [transform, transform_smooth]

                    // center dots
                    for (let i = 0; i < 3; i++) {
                        ctx.beginPath();
                        ctx.arc(width/6*(i*2+1), height/2, 4, 0, 2 * Math.PI, false);
                        ctx.fillStyle = maincolor;
                        ctx.fill();
                        ctx.stroke();
                    }

                    for (let i = 0; i < 3; i++) {
                        ctx.beginPath();
                        ctx.moveTo(width/6, height/2);
                        const transformedvec = transform.times(vecs[i])
                        ctx.lineTo(width/6 + transformedvec.x, height/2 - transformedvec.y);
                        ctx.lineWidth = 3;
                        ctx.strokeStyle = colors[i];
                        ctx.globalAlpha = 0.5;
                        ctx.stroke();
                        ctx.globalAlpha = Math.max(0.1, Math.min(transformedvec.z/(veclen*2)+0.5,1));
                        ctx.beginPath();
                        ctx.arc(width/6 + transformedvec.x, height/2 - transformedvec.y, 4, 0, 2 * Math.PI, false);
                        ctx.fillStyle = colors[i];
                        ctx.fill();
                        ctx.stroke();
                    }

                    ctx.lineWidth = 1.5;
                    ctx.strokeStyle = maincolor;
                    ctx.globalAlpha = 0.8;
                    ctx.lineJoin = "bevel";
                    for (let view = 0; view < 2; view++) {
                        for (let linenum = 0; linenum < lines.length; linenum++) {
                            ctx.beginPath()
                            for (let pointnum=0; pointnum < lines[linenum].length; pointnum++) {
                                const transformedvec = transforms[view].times(cam_vert_vecs[lines[linenum][pointnum]]);
                                if (pointnum == 0) {
                                    ctx.moveTo(transformedvec.x + width/6*(view*2 + 3), -transformedvec.y + height/2);
                                }
                                else {
                                    ctx.lineTo(transformedvec.x + width/6*(view*2 + 3), -transformedvec.y + height/2);
                                }
                            }
                            ctx.stroke();
                        }
                    }
                }
            }
            function updateOrientation(timestamp: real): void {
                currentTimestamp = timestamp;
                requestPaint();
                meshCorrection.requestPaint();
                focalPlaneDistortion.requestPaint();
            }
        }
        Canvas {
            id: meshCorrection
            width: parent.width
            height: width * (orgH / orgW);
            visible: false;

            property real orgW: window.videoArea.outWidth  || window.videoArea.vid.videoWidth;
            property real orgH: window.videoArea.outHeight || window.videoArea.vid.videoHeight;
            property bool initialDraw: false
            onPaint: {
                if (orientationCheckbox.checked || !initialDraw) {
                    initialDraw = true
                    let ctx = getContext("2d");
                    ctx.reset();
                    const maincolor = style === "light" ? "rgba(0,0,0,0.9)" : "rgba(255,255,255,0.9)";
                    const margin = 15 * dpiScale;

                    const mesh = controller.mesh_at_frame(window.videoArea.vid.currentFrame);
                    if (!mesh.length || mesh[0] < 10) { meshCorrection.visible = false; return; }
                    const divisions = [mesh[1], mesh[2]];
                    const mesh_size = [mesh[3], mesh[4]];

                    meshCorrection.visible = divisions[0] > 0;

                    for (let i = 0; i < (divisions[0]*divisions[1]*2); i += 2) {
                        const x = margin + (width  - 2*margin) * mesh[9 + i + 0] / mesh_size[0];
                        const y = margin + (height - 2*margin) * mesh[9 + i + 1] / mesh_size[1];
                        ctx.beginPath();
                        ctx.arc(x, y, 2 * dpiScale, 0, 2 * Math.PI, false);
                        ctx.fillStyle = maincolor;
                        ctx.fill();
                    }

                    ctx.lineWidth = 1 * dpiScale;
                    ctx.strokeStyle = maincolor;
                    ctx.beginPath();
                    for (let i = 0; i < (divisions[0]*divisions[1]*2); i += 2) {
                        const x = margin + (width  - 2*margin) * (mesh[9 + i + 0] / mesh_size[0]);
                        const y = margin + (height - 2*margin) * (mesh[9 + i + 1] / mesh_size[1]);
                        ctx.moveTo(x, y);
                        if (((i + 2) / 2) % divisions[1] != 0) {
                            const xx = margin + (width  - 2*margin) * mesh[9 + i + 2 + 0] / mesh_size[0];
                            const yy = margin + (height - 2*margin) * mesh[9 + i + 2 + 1] / mesh_size[1];
                            ctx.lineTo(xx, yy);
                        }
                        if (i + divisions[1]*2 < divisions[0]*divisions[1]*2) {
                            const xxx = margin + (width  - 2*margin) * (mesh[9 + i + divisions[0]*2 + 0] / mesh_size[0]);
                            const yyy = margin + (height - 2*margin) * (mesh[9 + i + divisions[0]*2 + 1] / mesh_size[1]);
                            ctx.moveTo(x, y);
                            ctx.lineTo(xxx, yyy);
                        }
                    }
                    ctx.stroke();
                }
            }
        }
        Canvas {
            id: focalPlaneDistortion;
            width: parent.width
            height: width * (orgH / orgW);
            visible: false;

            property real orgW: window.videoArea.outWidth  || window.videoArea.vid.videoWidth;
            property real orgH: window.videoArea.outHeight || window.videoArea.vid.videoHeight;
            property bool initialDraw: false
            onPaint: {
                if (orientationCheckbox.checked || !initialDraw) {
                    initialDraw = true
                    let ctx = getContext("2d");
                    ctx.reset();
                    const maincolor = style === "light" ? "rgba(0,0,0,0.9)" : "rgba(255,255,255,0.9)";
                    const margin = 15 * dpiScale;

                    const mesh = controller.mesh_at_frame(window.videoArea.vid.currentFrame);
                    if (!mesh.length || mesh[0] == 0 || mesh[mesh[0]] == 0) { focalPlaneDistortion.visible = false; return; }
                    const mesh_size = [mesh[3], mesh[4]];

                    focalPlaneDistortion.visible = true;

                    ctx.lineWidth = 1 * dpiScale;
                    ctx.strokeStyle = maincolor;
                    const o = mesh[0];
                    const stblz_grid = mesh[o + 2] > 0 ? mesh[o + 2] : mesh_size[1] / 8; // band height comes with the table, as in the kernels

                    let points = [];
                    for (let i = 0; i < 8; ++i) {
                        // corners of the rectangle; the last band carries on to the bottom of the sensor, as the kernels apply it
                        const top = i * stblz_grid;
                        const bottom = i == 7 ? Math.max((i + 1) * stblz_grid, mesh_size[1]) : (i + 1) * stblz_grid;
                        points.push([0, top]);
                        points.push([mesh_size[0], top]);
                        points.push([mesh_size[0], bottom]);
                        points.push([0, bottom]);

                        points.push([0, top]);
                    }

                    for (let i = 0; i < points.length; ++i) {
                        const idx = Math.min(7, Math.max(0, Math.floor(points[i][1] / stblz_grid)));
                        const delta = points[i][1] - stblz_grid * idx;
                        points[i][0] += mesh[o + 4 + idx * 2 + 0] * delta;
                        points[i][1] += mesh[o + 4 + idx * 2 + 1] * delta;
                        for (let j = 0; j < idx; j++) {
                            points[i][0] += mesh[o + 4 + j * 2 + 0] * stblz_grid;
                            points[i][1] += mesh[o + 4 + j * 2 + 1] * stblz_grid;
                        }
                    }

                    ctx.beginPath();
                    for (let i = 0; i < points.length; ++i) {
                        const x = margin + (width  - 2*margin) * points[i][0] / mesh_size[0];
                        const y = margin + (height - 2*margin) * points[i][1] / mesh_size[1];
                        if (i == 0) ctx.moveTo(x, y);
                        else ctx.lineTo(x, y);
                    }
                    ctx.stroke();
                }
            }
        }
    }

    Row {
        anchors.horizontalCenter: parent.horizontalCenter;
        LinkButton {
            text: qsTr("Statistics");
            onClicked: {
                if (window.videoArea.statistics.item) window.videoArea.statistics.item.shown = !window.videoArea.statistics.item.shown;
                window.videoArea.statistics.active = true;
            }
        }
        LinkButton {
            id: exportGyroBtn;
            text: qsTr("Export");
            onClicked: {
                menuLoader.toggle(exportGyroBtn, 0, height);
            }
            Component {
                id: menu;
                Menu {
                    id: menuInner;
                    FileDialog {
                        id: exportFileDialog;
                        fileMode: FileDialog.SaveFile;
                        title: qsTr("Select file destination");
                        type: "gyro-csv";
                        property var exportData: ({});
                        onAccepted: {
                            if (exportData === "full") {
                                controller.export_full_metadata(selectedFile, root.lastSelectedFile.toString()? root.lastSelectedFile : window.videoArea.loadedFileUrl);
                            } else if (exportData == "parsed") {
                                controller.export_parsed_metadata(selectedFile);
                            } else {
                                controller.export_gyro_data(selectedFile, exportData);
                            }
                        }
                    }
                    Action {
                        text: qsTr("Export camera data (CSV/JSON/USD/AE)");
                        onTriggered: {
                            const el = Qt.createComponent("../SettingsSelector.qml").createObject(window, {
                                desc: [
                                    {
                                        "Original|original": {
                                            "Gyroscope":       ["gyroscope"],
                                            "Accelerometer":   ["accelerometer"],
                                            "Quaternion":      ["quaternion"],
                                            "Euler angles":    ["euler_angles"],
                                            "Focus distances": ["focus_distances"],
                                        },
                                    },
                                    {
                                        "Stabilized|stabilized": {
                                            "Quaternion":    ["quaternion"],
                                            "Euler angles":  ["euler_angles"],
                                        },
                                    },
                                    {
                                        "Zooming|zooming": {
                                            "Minimal FOV scale":  ["minimal_fovs"],
                                            "Smoothed FOV scale": ["fovs"],
                                            "Focal length (if available)": ["focal_length"],
                                        },
                                    }
                                ],
                                type: "gyro_csv"
                            });
                            let savedState = settings.value("CSVExportSelection", "");
                            if (savedState) {
                                try {
                                    el.loadSelection(JSON.parse(savedState));
                                } catch(e) { }
                            }
                            el.opened = true;
                            el.onApply.connect((obj) => {
                                settings.setValue("CSVExportSelection", JSON.stringify(obj));

                                if (Qt.platform.os == "ios") {
                                    const exportToFolder = ext => {
                                        const folder = filesystem.get_folder(root.lastSelectedFile.toString()? root.lastSelectedFile : window.videoArea.loadedFileUrl);
                                        const opf = Qt.createComponent("../components/OutputPathField.qml").createObject(window, { visible: false });
                                        opf.selectFolder(folder, function(folder_url) {
                                            const filename = root.filename.replace(/\.[^/.]+$/, "." + ext);
                                            controller.export_gyro_data(filesystem.get_file_url(folder_url, filename, true), obj);
                                            opf.destroy();
                                        });
                                    };
                                    messageBox(Modal.Question, qsTr("Which format do you want to use?"), [
                                        { text: "CSV",                         clicked: function() { exportToFolder("csv"); } },
                                        { text: "JSON", accent: true,          clicked: function() { exportToFolder("json"); } },
                                        { text: qsTr("Universal Scene Description"), clicked: function() { exportToFolder("usd"); } },
                                        { text: qsTr("After Effects Script"),        clicked: function() { exportToFolder("jsx"); } },
                                        { text: qsTr("Cancel") },
                                    ]);
                                    return;
                                }

                                exportFileDialog.nameFilters = [
                                    "CSV (*.csv)",
                                    "JSON (*.json)",
                                    qsTr("Universal Scene Description") + " (*.usd)",
                                    qsTr("After Effects Script") + " (*.jsx)"
                                ];
                                exportFileDialog.exportData = obj;
                                exportFileDialog.open2();
                            });
                        }
                    }
                    Action {
                        text: qsTr("Export full metadata");
                        onTriggered: {
                            if (Qt.platform.os == "ios") {
                                const folder = filesystem.get_folder(root.lastSelectedFile.toString()? root.lastSelectedFile : window.videoArea.loadedFileUrl);
                                const opf = Qt.createComponent("../components/OutputPathField.qml").createObject(window, { visible: false });
                                opf.selectFolder(folder, function(folder_url) {
                                    const filename = root.filename.replace(/\.[^/.]+$/, ".json");
                                    controller.export_full_metadata(filesystem.get_file_url(folder_url, filename, true), root.lastSelectedFile.toString()? root.lastSelectedFile : window.videoArea.loadedFileUrl);
                                    opf.destroy();
                                });
                                return;
                            }

                            exportFileDialog.nameFilters = ["JSON (*.json)"];
                            exportFileDialog.exportData = "full";
                            exportFileDialog.open2();
                        }
                    }
                    Action {
                        text: qsTr("Export parsed metadata");
                        onTriggered: {
                            if (Qt.platform.os == "ios") {
                                const folder = filesystem.get_folder(root.lastSelectedFile.toString()? root.lastSelectedFile : window.videoArea.loadedFileUrl);
                                const opf = Qt.createComponent("../components/OutputPathField.qml").createObject(window, { visible: false });
                                opf.selectFolder(folder, function(folder_url) {
                                    const filename = root.filename.replace(/\.[^/.]+$/, ".json");
                                    controller.export_parsed_metadata(filesystem.get_file_url(folder_url, filename, true));
                                    opf.destroy();
                                });
                                return;
                            }
                            exportFileDialog.nameFilters = ["JSON (*.json)"];
                            exportFileDialog.exportData = "parsed";
                            exportFileDialog.open2();
                        }
                    }
                    Action {
                        text: qsTr("Export project file (including processed gyro data)");
                        onTriggered: window.saveProject("WithProcessedData");
                    }
                }
            }
            ContextMenuLoader {
                id: menuLoader;
                sourceComponent: menu
            }
        }
    }

    DropTarget {
        parent: root.innerItem;
        color: styleBackground2;
        z: 999;
        anchors.rightMargin: -28 * dpiScale;
        anchors.topMargin: 35 * dpiScale;
        anchors.bottomMargin: -35 * dpiScale;
        extensions: fileDialog.extensions;
        onLoadFile: (url) => root.loadFile(url)
    }
}
