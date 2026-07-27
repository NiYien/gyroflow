// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick

import "../components/"

MenuItem {
    id: sync;
    text: qsTr("Synchronization");
    iconName: "sync";
    innerItem.enabled: window.videoArea.vid.loaded && !controller.sync_in_progress;
    loader: controller.sync_in_progress;
    objectName: "synchronization";

    Item {
        id: sett;
        // processingResolution is intentionally not persisted: always start at 1080p on app launch
        property alias initialOffset: initialOffset.value;
        property alias syncSearchSize: syncSearchSize.value;
        property alias maxSyncPoints: maxSyncPoints.value;
        property alias timePerSyncpoint: timePerSyncpoint.value;
        property alias sync_lpf: lpf.value;
        property alias checkNegativeInitialOffset: checkNegativeInitialOffset.checked;
        property alias experimentalAutoSyncPoints: experimentalAutoSyncPoints.checked;
        // property alias syncMethod: syncMethod.currentIndex;
        // property alias offsetMethod: offsetMethod.currentIndex;
        // property alias poseMethod: poseMethod.currentIndex;
        // showFeatures / showOF are intentionally not persisted: the sync overlays start
        // off at every app launch and Full mode can only turn them on for the current
        // session. Adding an alias back here would restore the old QSettings memory and
        // re-open the bug where a stale `true` is written back into the controller during
        // startup. See StabilizationParams::default().
        // This is a specific use case and I don't think we should remember that setting, especially that it's hidden under "Advanced"
        //property alias everyNthFrame: everyNthFrame.value;

        Component.onCompleted: settings.init(sett);
        function propChanged() { settings.propChanged(sett); }
    }

    property alias timePerSyncpoint: timePerSyncpoint;
    property alias everyNthFrame: everyNthFrame;
    property alias poseMethod: poseMethod;
    property var customSyncTimestamps: [];
    property var additionalSyncTimestamps: [];
    // True when initial_offset came from a batch/deep-match anchor — rides
    // along sync_settings so the posterior can pick the anchor-tier prior.
    property bool offsetIsAnchor: false;

    function doAutosync(): void {
        if (controller.sync_in_progress) return;
        autosync.doSync();
    }
    function runAutosync(): void {
        if (controller.sync_in_progress) return;
        if (!controller.gyro_loaded) {
            messageBox(Modal.Info, qsTr("No IMU data is loaded. Load gyro data before synchronizing."), [
                { text: qsTr("Ok"), accent: true },
            ]);
            return;
        }
        if (!controller.lens_loaded) {
            messageBox(Modal.Warning, qsTr("Lens profile is not loaded, synchronization will most likely give wrong results. Are you sure you want to continue?"), [
                { text: qsTr("Yes"), clicked: function() {
                    doAutosync();
                }},
                { text: qsTr("No"), accent: true },
            ]);
        } else {
            doAutosync();
        }
    }

    function loadGyroflow(obj: var): void {
        const o = obj.synchronization || { };
        if (o && Object.keys(o).length > 0) {
            if (o.hasOwnProperty("initial_offset"))     initialOffset.value                 = +o.initial_offset;
            if (o.hasOwnProperty("initial_offset_inv")) checkNegativeInitialOffset.checked  = !!o.initial_offset_inv;
            // No hasOwnProperty guard: a project without the key must clear a
            // stale anchor flag from the previous clip.
            sync.offsetIsAnchor = !!o.offset_is_anchor;
            if (o.hasOwnProperty("search_size"))        syncSearchSize.value                = +o.search_size;
            if (o.hasOwnProperty("calc_initial_fast"))  calculateInitialOffsetFirst.checked = !!o.calc_initial_fast;
            if (o.hasOwnProperty("max_sync_points"))    maxSyncPoints.value                 = +o.max_sync_points;
            if (o.hasOwnProperty("every_nth_frame"))    everyNthFrame.value                 = +o.every_nth_frame;
            if (o.hasOwnProperty("time_per_syncpoint")) timePerSyncpoint.value              = +o.time_per_syncpoint;
            if (o.hasOwnProperty("of_method"))          syncMethod.currentIndex             = syncMethod.ofMethodReverseMap[+o.of_method] || 0;
            if (o.hasOwnProperty("offset_method"))      offsetMethod.currentIndex           = +o.offset_method;
            if (o.hasOwnProperty("pose_method"))        poseMethod.currentIndex             = +o.pose_method;
            if (o.hasOwnProperty("custom_sync_pattern")) sync.customSyncTimestamps          = resolveSyncpointPattern(o.custom_sync_pattern);
            if (o.hasOwnProperty("auto_sync_points")) experimentalAutoSyncPoints.checked    = !!o.auto_sync_points;
            if (o.hasOwnProperty("do_autosync") && o.do_autosync) autosyncTimer.doRun = true;
        }
    }
    Timer {
        id: autosyncTimer;
        interval: 200;
        property bool doRun: false;
        running: controller.lens_loaded
              && controller.gyro_loaded
              && !controller.loading_gyro_in_progress
              && !window.isDialogOpened
              && !window.videoArea.queueEditLoading
              && doRun
              && render_queue.editing_job_id == 0;
        onTriggered: {
            doRun = false;
            if (controller.offsets_model.rowCount() == 0 && !window.motionData.hasAccurateTimestamps)
                sync.doAutosync();
        }
    }
    function getSettings(): var {
        // Simple mode binds of_method entirely to the AI SYNC toggle:
        //   on  → NeuFlow Burn (4),  off → OpenCV DIS (2).
        // The Full-mode dropdown is bypassed here so that toggling AI SYNC off
        // in Simple mode does not silently inherit a NeuFlow selection the
        // user previously made in Full mode (and vice versa).
        // Full mode keeps using the dropdown.
        const aiSyncRaw = settings.value("simpleAiSync", false);
        const aiSyncOn  = (aiSyncRaw === true || aiSyncRaw === "true");
        const useAi     = aiSyncOn && controller.has_neuflow_support();
        const isSimple  = window.isSimpleMode === true;
        const ofMethod  = isSimple ? (useAi ? 4 : 2)
                                   : (useAi ? 4 : syncMethod.ofMethodMap[syncMethod.currentIndex]);
        return {
            "initial_offset":     initialOffset.value,
            "initial_offset_inv": checkNegativeInitialOffset.checked,
            "offset_is_anchor":   sync.offsetIsAnchor,
            "search_size":        syncSearchSize.value,
            "calc_initial_fast":  calculateInitialOffsetFirst.checked,
            "max_sync_points":    maxSyncPoints.value,
            "every_nth_frame":    everyNthFrame.value,
            "time_per_syncpoint": timePerSyncpoint.value,
            "of_method":          ofMethod,
            "offset_method":      offsetMethod.currentIndex,
            "pose_method":        poseMethod.currentIndex,
            "auto_sync_points":   experimentalAutoSyncPoints.checked,
        };
    }
    function getSettingsJson(): string { return JSON.stringify(getSettings()); }

    // Pattern example, all values can be either frames, s or ms
    // {
    //     "start": "1001"    // frames
    //     "interval": "5s"   // s
    //     "gap": "100ms"     // ms
    // }
    // Keep in sync with render_queue.rs
    function resolveDurationToMs(d: var, fps: real): real {
        if (!d) return 0;
             if (d.toString().endsWith("ms")) return +(d.replace("ms", ""));
        else if (d.toString().endsWith("s"))  return +(d.replace("s", "")) * 1000.0;
        else                                  return (+d / fps) * 1000.0;
    }
    function resolveItem(x: var, duration: real, fps: real): list<var> {
        const start = x.hasOwnProperty("start")? resolveDurationToMs(x.start, fps) : 0;
        const interval = x.hasOwnProperty("interval")? resolveDurationToMs(x.interval, fps) : duration;
        const gap = resolveDurationToMs(x.gap, fps);
        let out = [];
        for (let i = start; i < duration; i += interval) {
            out.push(i - gap / 2.0);
            if (gap > 0) {
                out.push(i + gap / 2.0);
            }
        }
        return out;
    }
    function resolveSyncpointPattern(o: var): list<real> {
        const duration = window.videoArea.vid.duration;
        const fps      = window.videoArea.vid.frameRate;

        let timestamps = [];
        if (Array.isArray(o)) {
            for (const x of o) {
                timestamps.push(...resolveItem(x, duration, fps));
            }
        } else if (Object.isObject(o)) {
            timestamps.push(...resolveItem(o, duration, fps));
        }
        timestamps.sort((a, b) => a - b);

        return timestamps;
    }
    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            sync.additionalSyncTimestamps = [];
            if (additional_data.additional_sync_points) {
                for (const x of additional_data.additional_sync_points.split(";")) {
                    sync.additionalSyncTimestamps.push(+x);
                }
            }
        }
    }

    Button {
        id: autosync;
        text: qsTr("Auto sync");
        iconName: "spinner"
        anchors.horizontalCenter: parent.horizontalCenter;
        // enabled: controller.gyro_loaded;
        tooltip: !enabled? qsTr("No motion data loaded, cannot sync.") : "";
        function doSync(): void {
            let maxPoints = maxSyncPoints.value;
            let sync_points = controller.get_optimal_sync_points(maxPoints, initialOffset.value);

            if (!sync_points || !experimentalAutoSyncPoints.checked) {
                let ranges = [];
                const trimRanges = videoArea.timeline.getTrimRanges();
                if (trimRanges.length > 1) {
                    maxPoints = Math.ceil(maxPoints / trimRanges.length) + 1;
                }
                for (const [trimStart, trimEnd] of trimRanges) {
                    const trimmed = trimEnd - trimStart;
                    const chunks = trimmed / maxPoints;
                    const start = trimStart + (chunks / 2);

                    for (let i = 0; i < maxPoints; ++i) {
                        const pos = start + (i*chunks);
                        ranges.push(pos);
                    }
                    const duration = window.videoArea.vid.duration;
                    const filter_ranges = v => (v >= trimStart * duration) && (v <= trimEnd * duration);
                    if (sync.customSyncTimestamps.length > 0) {
                        ranges = sync.customSyncTimestamps.filter(filter_ranges).map(v => v / duration);
                    }
                    if (sync.additionalSyncTimestamps.length > 0) {
                        for (const x of sync.additionalSyncTimestamps.filter(filter_ranges)) {
                            ranges.push(x / duration);
                        }
                    }
                }
                ranges.sort((a, b) => a - b);
                sync_points = ranges.join(";");
            }
            controller.start_autosync(sync_points, sync.getSettingsJson(), "synchronize");
        }
        onClicked: sync.runAutosync()

        CheckBox {
            id: experimentalAutoSyncPoints;
            anchors.left: autosync.right;
            anchors.leftMargin: 5 * dpiScale;
            anchors.verticalCenter: parent.verticalCenter;
            contentItem.visible: false;
            scale: 0.7;
            tooltip: qsTr("Experimental automatic sync point selection.");
        }
    }

    InfoMessageSmall {
        property bool usesQuats: ((window.motionData.hasQuaternions && window.motionData.integrationMethod === 0) || window.motionData.hasAccurateTimestamps) && window.motionData.filename == window.vidInfo.filename;
        show: usesQuats && controller.offsets_model.rowCount() > 0;
        text: qsTr("This file uses synced motion data, additional sync points are not needed and can make the output look worse.");
        onUsesQuatsChanged: sync.opened = !usesQuats;
    }

    Label {
        position: Label.LeftPosition;
        text: qsTr("Rough gyro offset");

        NumberField {
            id: initialOffset;
            width: parent.width - checkNegativeInitialOffset.width;
            height: 25 * dpiScale;
            defaultValue: 0;
            precision: 1;
            unit: qsTr("s");
        }
        CheckBox {
            id: checkNegativeInitialOffset;
            anchors.left: initialOffset.right;
            anchors.leftMargin: 5 * dpiScale;
            anchors.verticalCenter: parent.verticalCenter;
            contentItem.visible: false;
            scale: 0.7;
            tooltip: qsTr("Analyze both positive and negative offset.\nThis doubles the calculation time, so check this only for the initial point and uncheck once you know the offset.");
        }
    }

    Label {
        position: Label.LeftPosition;
        text: qsTr("Sync search size");

        NumberField {
            id: syncSearchSize;
            width: parent.width - (calculateInitialOffsetFirst.visible? calculateInitialOffsetFirst.width : 0);
            height: 25 * dpiScale;
            precision: 1;
            value: 5;
            defaultValue: 5;
            unit: qsTr("s");
            onValueChanged: if (calculateInitialOffsetFirst.visible) calculateInitialOffsetFirst.checked = value > 10;
        }
        CheckBox {
            id: calculateInitialOffsetFirst;
            anchors.left: syncSearchSize.right;
            anchors.leftMargin: 5 * dpiScale;
            anchors.verticalCenter: parent.verticalCenter;
            contentItem.visible: false;
            scale: 0.7;
            visible: offsetMethod.currentIndex > 0;
            tooltip: qsTr("Calculate initial offset first (using essential matrix method), then refine using slower but more accurate rs-sync method.");
        }
    }
    Label {
        position: Label.LeftPosition;
        text: qsTr("Max sync points");

        NumberField {
            id: maxSyncPoints;
            width: parent.width;
            height: 25 * dpiScale;
            value: 3;
            from: 1;
            to: 30;
            onValueChanged: { if (value < 1) value = 1; if (value > 500) value = 500; }
        }
    }

    AdvancedSection {
        Label {
            position: Label.LeftPosition;
            text: qsTr("Analyze every n-th frame");

            NumberField {
                id: everyNthFrame;
                width: parent.width;
                height: 25 * dpiScale;
                value: 1;
                defaultValue: 1;
                from: 1;
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Time to analyze per sync point");

            NumberField {
                id: timePerSyncpoint;
                width: parent.width;
                height: 25 * dpiScale;
                value: 1.5;
                defaultValue: 1.5;
                precision: 2;
                unit: qsTr("s");
                from: 0.01;
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Processing resolution");
            ComboBox {
                id: processingResolution;
                model: [QT_TRANSLATE_NOOP("Popup", "Full"), "4k", "1080p", "720p", "480p"];
                font.pixelSize: 12 * dpiScale;
                width: parent.width;
                currentIndex: 2;
                onCurrentIndexChanged: {
                    let target_height = -1; // Full
                    switch (currentIndex) {
                        case 1: target_height = 2160; break;
                        case 2: target_height = 1080; break;
                        case 3: target_height = 720; break;
                        case 4: target_height = 480; break;
                    }

                    controller.set_processing_resolution(target_height);
                    render_queue.set_processing_resolution(target_height);
                }
            }
        }
        InfoMessageSmall {
            show: syncMethod.currentValue == "AKAZE";
            text: qsTr("The AKAZE method may be more accurate but is significantly slower than OpenCV. Use only if OpenCV doesn't produce good results");
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Optical flow method");

            ComboBox {
                id: syncMethod;
                // Method ids: 0=AKAZE, 1=PyrLK, 2=DIS, 3=NeuFlow-CUDA (removed), 4=NeuFlow-Burn.
                // The NeuFlow v2 CUDA option (id 3) was dropped — Burn replaces it.
                // Legacy .gyroflow projects with of_method=3 are silently mapped to Burn
                // on Win/Mac (or DIS on platforms without neuflow_burn_enabled).
                readonly property bool hasNeuflow: controller.has_neuflow_support();
                readonly property var ofMethodMap:
                    hasNeuflow ? [4, 0, 1, 2] : [0, 1, 2];
                readonly property var ofMethodReverseMap:
                    hasNeuflow ? ({0: 1, 1: 2, 2: 3, 3: 0, 4: 0})
                               : ({0: 0, 1: 1, 2: 2, 3: 2, 4: 2});
                model: hasNeuflow ? [
                    QT_TR_NOOP("NeuFlow v2 Burn"),
                    "AKAZE",
                    "OpenCV (PyrLK)",
                    "OpenCV (DIS)"
                ] : [
                    "AKAZE",
                    "OpenCV (PyrLK)",
                    "OpenCV (DIS)"
                ];
                font.pixelSize: 12 * dpiScale;
                width: parent.width;
                // Default to "OpenCV (DIS)" — last item in both branches.
                currentIndex: hasNeuflow ? 3 : 2;
                onCurrentIndexChanged: controller.set_of_method(ofMethodMap[currentIndex]);
                Component.onCompleted: currentIndexChanged();
            }
        }
        Label {
            text: qsTr("Pose method");
            position: Label.LeftPosition;

            ComboBox {
                id: poseMethod;
                model: ["findEssentialMat", "Almeida", "EightPoint", "findHomography"];
                font.pixelSize: 12 * dpiScale;
                width: parent.width;
                currentIndex: 0;
                onCurrentIndexChanged: controller.set_of_method(syncMethod.ofMethodMap[syncMethod.currentIndex]);
            }
        }
        Label {
            text: qsTr("Offset method");
            position: Label.LeftPosition;

            ComboBox {
                id: offsetMethod;
                model: [QT_TRANSLATE_NOOP("Popup", "Essential matrix"), QT_TRANSLATE_NOOP("Popup", "Visual features"), QT_TRANSLATE_NOOP("Popup", "rs-sync")];
                font.pixelSize: 12 * dpiScale;
                width: parent.width;
                currentIndex: 2;
                property var tooltips: ([
                    qsTr("Calculate camera transformation matrix from optical flow to get the rotation angles of the camera.\nThen try to match these angles to gyroscope angles."),
                    qsTr("Undistort optical flow points using gyro and candidate offset.\nThen calculate lengths of the optical flow lines.\nResulting offset is the one where lines were the shortest, meaning the video was moving the least visually."),
                    qsTr("Rolling shutter video to gyro synchronization algorithm.\nMake sure you have proper rolling shutter value set before syncing.")
                ]);
                tooltip: tooltips[currentIndex];
            }
        }
        CheckBoxWithContent {
            id: lpfcb;
            text: qsTr("Low pass filter");
            onCheckedChanged: controller.set_sync_lpf(checked? lpf.value : 0);

            NumberField {
                id: lpf;
                unit: qsTr("Hz");
                precision: 2;
                value: 0;
                defaultValue: 0;
                from: 0;
                width: parent.width;
                onValueChanged: {
                    controller.set_sync_lpf(lpfcb.checked? lpf.value : 0);
                }
            }
        }
        // `checked: false` matches QQC.CheckBox's own default, so constructing this
        // component does not fire onCheckedChanged and never writes to the controller.
        // This panel is loaded even in Simple mode (its ItemLoader is asynchronous and
        // stays active, only the wrapping Item is hidden), so an initial `true` here
        // used to land in the controller after App.qml's Component.onCompleted had
        // already run — turning the overlays back on behind the user's back.
        CheckBox {
            id: showFeatures;
            text: qsTr("Show detected features");
            checked: false;
            onCheckedChanged: controller.show_detected_features = checked;
        }
        CheckBox {
            id: showOF;
            text: qsTr("Show optical flow");
            checked: false;
            onCheckedChanged: controller.show_optical_flow = checked;
        }
    }
}
