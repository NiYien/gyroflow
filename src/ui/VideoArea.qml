// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import MDKVideo

import "components/"
import "menu/" as Menu
import "Util.js" as Util
import "DropRules.js" as DropRules

Item {
    id: root;
    width: parent.width;
    height: parent.height;
    anchors.horizontalCenter: parent.horizontalCenter;

    property alias vid: vid;
    // Preview/drop area only (excludes the timeline), used by the tutorial overlay.
    property alias previewArea: vidParentParent;
    property alias timeline: timeline;
    property alias durationMs: timeline.durationMs;
    property alias videoLoader: videoLoader;
    property alias stabEnabledBtn: stabEnabledBtn;
    property alias fovOverviewBtn: fovOverviewBtn;
    property alias stabPreviewBtn: stabPreviewBtn;
    property alias queue: queue.item;
    property alias statistics: statistics;
    property alias infoMessages: infoMessages;
    property alias gridGuide: gridGuide;
    property alias secondPreview: secondPreview;

    property int outWidth: window? window.exportSettings.outWidth : 0;
    property int outHeight: window? window.exportSettings.outHeight : 0;

    property alias dropRect: dropRect;
    property bool isCalibrator: false;

    property var pendingGyroflowData: null;
    property int pendingQueueJobId: 0;
    property url pendingExternalGyroFallbackUrl: "";
    property int pendingExternalGyroFallbackProjectVersion: 0;
    property url pendingCrmTelemetryUrl: "";
    // Set by loadFile() from its suppressAssociatedGyroflow argument; consumed
    // by fileLoaded() to decide whether to fire the deferred .gyroflow
    // sibling prompt. Reset to false inside fileLoaded after use.
    property bool skipAssociatedGyroflowOnLoad: false;
    property bool queueEditLoading: false;
    property url loadedFileUrl;

    property int fullScreen: 0;
    property string detectedCamera: "";
    property real additionalTopMargin: 0;
    property var mergedFiles: [];

    property Menu.VideoInformation vidInfo: null;

    // Android suspend/resume video recovery: when the OS reclaims the GL
    // surface in the background, the scene graph invalidation destroys the
    // MDK player and it rebuilds with no media (permanent black preview).
    // Snapshot playback state on suspend; on resume, if the scene graph was
    // actually invalidated, re-issue the media at the player level and let
    // fileLoaded() short-circuit into a seek-only restore.
    property real suspendTimestamp: -1;
    property bool suspendWasPlaying: false;
    property bool restoringFromSuspend: false;
    Connections {
        target: Qt.application;
        enabled: Qt.platform.os === "android" && !root.isCalibrator;
        function onStateChanged() {
            if (Qt.application.state === Qt.ApplicationSuspended) {
                if (vid.loaded) {
                    root.suspendTimestamp = vid.timestamp;
                    root.suspendWasPlaying = vid.playing;
                    console.log("suspend snapshot: ts=" + root.suspendTimestamp + " playing=" + root.suspendWasPlaying);
                }
            } else if (Qt.application.state === Qt.ApplicationActive) {
                if (ui_tools.take_scene_graph_invalidated()) {
                    if (root.suspendTimestamp >= 0 && vid.loaded) {
                        root.restoringFromSuspend = true;
                        if (!controller.restore_video_after_resume(vid)) {
                            root.restoringFromSuspend = false;
                        }
                    }
                }
            }
        }
    }

    function loadGyroflowData(obj: var, queueJobId: var): void {
        root.pendingGyroflowData = null;
        root.pendingQueueJobId = 0;
        root.pendingExternalGyroFallbackUrl = "";
        root.pendingExternalGyroFallbackProjectVersion = 0;
        root.pendingCrmTelemetryUrl = "";
        const targetQueueJobId = +queueJobId;
        root.queueEditLoading = targetQueueJobId > 0;

        if (targetQueueJobId > 0 && render_queue.editing_job_id !== targetQueueJobId) {
            render_queue.editing_job_id = targetQueueJobId;
        }

        if (controller.loading_gyro_in_progress) {
            root.pendingGyroflowData = obj;
            root.pendingQueueJobId = targetQueueJobId;
            controller.cancel_current_operation();
            // we'll get called again from telemetry_loaded
            return;
        }

        let urls = null;
        let project_version = +obj.version;

        if (obj.toString() != '[object Object]') { // obj is url
            urls = controller.get_urls_from_gyroflow_file(obj);
            project_version = controller.get_version_from_gyroflow_file(obj);
        } else if (obj.project_file) {
            urls = controller.get_urls_from_gyroflow_file(obj.project_file);
            project_version = controller.get_version_from_gyroflow_file(obj.project_file);
        } else {
            urls = [
                obj.videofile,
                obj.gyro_source?.filepath || ""
            ];
        }
        if ((!urls || !urls[0]) && !vidInfo.filename) {
            messageBox(Modal.Error, qsTr("Preset can be applied only after loading a video."), [ { text: qsTr("Ok") } ]);
            return;
        }

        const isCorrectVideoLoaded = urls[0] && vidInfo.filename == filesystem.get_filename(urls[0]);
        const isCorrectGyroLoaded  = urls[1] && window.motionData.filename == filesystem.get_filename(urls[1]);
        console.log("Video path:", urls[0], "(" + (isCorrectVideoLoaded? "loaded" : "not loaded") + ")", "Gyro path:", urls[1], "(" + (isCorrectGyroLoaded? "loaded" : "not loaded") + ")");

        if (urls[0] && !isCorrectVideoLoaded) {
            root.pendingGyroflowData = obj;
            root.pendingQueueJobId = targetQueueJobId;
            console.log("Loading video file", urls[0]);
            loadFile(urls[0], false, targetQueueJobId);
            if (controller.image_sequence_fps > 0) {
                vid.setFrameRate(controller.image_sequence_fps);
            }
            return;
        }
        if (urls[1] && !isCorrectGyroLoaded && filesystem.exists(urls[1])) {
            console.log("Deferring gyro file fallback", urls[1]);
            window.motionData.lastSelectedFile = urls[1];
            root.pendingExternalGyroFallbackUrl = urls[1];
            root.pendingExternalGyroFallbackProjectVersion = project_version;
        }

        controller.set_prevent_recompute(true);
        if (obj.toString() != '[object Object]') {
            // obj is url
            controller.import_gyroflow_file(obj);
        } else if (obj.project_file) {
            controller.import_gyroflow_file(obj.project_file);
        } else {
            controller.import_gyroflow_data(JSON.stringify(obj));
        }
        render_queue.editing_job_id = targetQueueJobId;
    }
    Connections {
        target: controller;
        function onGyroflow_file_loaded(obj: var): void {
            if (obj) {
                let duration_ms = videoArea.vid.duration;
                const info = obj.video_info || { };
                if (info && Object.keys(info).length > 0) {
                    if (info.hasOwnProperty("vfr_fps") && Math.round(+info.vfr_fps * 1000) != Math.round(+info.fps * 1000)) {
                        vidInfo.updateEntryWithTrigger("Frame rate", +info.vfr_fps);
                    }
                    if (info.hasOwnProperty("rotation")) {
                        vidInfo.updateEntryWithTrigger("Rotation", +info.rotation);
                    }
                    if (info.hasOwnProperty("duration_ms")) {
                        duration_ms = info.duration_ms;
                        const displayDurationMs = info.hasOwnProperty("vfr_duration_ms") && +info.vfr_duration_ms > 0 ? +info.vfr_duration_ms : duration_ms;
                        vidInfo.updateEntry("Duration", vidInfo.getDuration({"stream.video[0].duration": displayDurationMs}));
                    }
                }

                for (const ts in obj.offsets) {
                    controller.set_offset(ts, obj.offsets[ts]);
                }
                if (obj.hasOwnProperty("trim_start")) {
                    timeline.setTrimRanges([[obj.trim_start, obj.trim_end]]);
                }
                if (obj.hasOwnProperty("trim_ranges_ms")) {
                    timeline.setTrimRanges(obj.trim_ranges_ms.map(x => [x[0] / duration_ms, (x[1] < 0? duration_ms + x[1] : x[1]) / duration_ms]));
                } else if (obj.hasOwnProperty("trim_ranges")) {
                    timeline.setTrimRanges(obj.trim_ranges);
                }
                window.motionData.loadGyroflow(obj);
                // Simple-mode mounting selector adopts the project's rotation.
                // Its async loader may not be ready yet when a project is
                // opened via command line (e.g. NLE "Open in Gyroflow") —
                // park the rotation for its Component.onCompleted then.
                if (window.simpleMounting) {
                    window.simpleMounting.loadGyroflow(obj);
                } else {
                    // Park a normalized rotation so the selector's
                    // Component.onCompleted adopts null/absent as top too
                    // (project-authoritative mounting semantics).
                    const projRot = (obj.gyro_source || {}).rotation;
                    window.pendingMountingRotation = (projRot && projRot.length === 3) ? projRot : [0, 0, 0];
                }
                window.stab.loadGyroflow(obj);
                window.advanced.loadGyroflow(obj);
                window.sync.loadGyroflow(obj);
                window.lensProfile.loadGyroflow(obj);
                Qt.callLater(window.exportSettings.loadGyroflow, obj);

                if (obj.hasOwnProperty("image_sequence_start") && +obj.image_sequence_start > 0) {
                    controller.image_sequence_start = +obj.image_sequence_start;
                }
                if (obj.hasOwnProperty("image_sequence_fps") && +obj.image_sequence_fps > 0.0) {
                    vid.setFrameRate(+obj.image_sequence_fps);
                    controller.image_sequence_fps = +obj.image_sequence_fps;
                }
                if (obj.hasOwnProperty("playback_speed")) {
                    let i = 0;
                    const speed = +obj.playback_speed;
                    for (const x of playbackRateCb.model) {
                        const rate = +x.replace("x", "");
                        if (Math.abs(rate - speed) < 0.01) {
                            playbackRateCb.currentIndex = i;
                            break;
                        }
                        ++i;
                    }
                }
                if (obj.hasOwnProperty("muted")) {
                    videoArea.vid.muted = !!obj.muted;
                }

                const fallbackUrl = root.pendingExternalGyroFallbackUrl;
                const fallbackProjectVersion = root.pendingExternalGyroFallbackProjectVersion;
                root.pendingExternalGyroFallbackUrl = "";
                root.pendingExternalGyroFallbackProjectVersion = 0;
                if (fallbackUrl && fallbackUrl.toString() && !controller.gyro_loaded) {
                    console.log("Falling back to external gyro file", fallbackUrl);
                    controller.set_prevent_recompute(false);
                    window.motionData.lastSelectedFile = fallbackUrl;
                    controller.load_telemetry(fallbackUrl, false, window.videoArea.vid, -1, fallbackProjectVersion);
                    return;
                }
            }
            if (!root.pendingGyroflowData && render_queue.editing_job_id > 0) {
                root.queueEditLoading = false;
            }
            controller.set_prevent_recompute(false);
            Qt.callLater(controller.recompute_gyro);
            Qt.callLater(controller.recompute_threaded);
            Qt.callLater(timeline.updateDurations);
        }
        function onExternal_sdk_progress(percent: real, sdk_name: string, error_string: string, url: string): void {
            if (externalSdkModal !== null && externalSdkModal.loader !== null) {
                externalSdkModal.loader.visible = percent < 1;
                externalSdkModal.loader.active = percent < 1;
                externalSdkModal.loader.progress = percent;
                externalSdkModal.loader.text = qsTr("Downloading %1 (%2)").arg(sdk_name);
                if (percent >= 1) {
                    const successCallback = externalSdkSuccessCallback;
                    externalSdkSuccessCallback = null;
                    externalSdkModal.close();
                    externalSdkModal = null;
                    window.isDialogOpened = false;
                    if (!error_string) {
                        if (successCallback) {
                            successCallback(url);
                        } else if (url == "ffmpeg_gpl") {
                            messageBox(Modal.Success, qsTr("Component was installed successfully.\nYou need to restart Gyroflow for changes to take effect.\nYour render queue and current file is saved automatically."), [ { text: qsTr("Ok") } ]);
                        } else {
                            loadFile(url, false);
                        }
                    } else {
                        if (Qt.platform.os == "osx") {
                            error_string += "\n" + qsTr("This is often caused by read-only file system.\nMake sure you copied the Gyroflow app to your Applications folder, instead of running from the .dmg directly.");
                        }
                        if (Qt.platform.os == "windows") {
                            error_string += "\n" + qsTr("This is often caused by read-only file system.\nIf you have Gyroflow in C:\\Program Files\\, then you'll need to run Gyroflow as Administrator in order to extract the SDK to the Gyroflow folder.");
                        }
                        messageBox(Modal.Error, error_string, [ { text: qsTr("Ok") } ]);
                    }
                }
            }
        }

        function onMp4_merge_progress(percent: real, error_string: string, url: url): void {
            if (externalSdkModal !== null && externalSdkModal.loader !== null) {
                externalSdkModal.loader.visible = percent < 1;
                externalSdkModal.loader.active = percent < 1;
                externalSdkModal.loader.progress = percent;
                externalSdkModal.loader.text = qsTr("Merging files to %1 (%2)").arg("<b>" + filesystem.display_url(url) + "</b>");
                if (percent >= 1) {
                    externalSdkModal.close();
                    externalSdkModal = null;
                    window.isDialogOpened = false;
                    if (!error_string) {
                        loadFile(url, true);
                    } else {
                        messageBox(Modal.Error, error_string, [ { text: qsTr("Ok") } ]);
                    }
                }
            }
        }
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            if (is_main_video) {
                root.detectedCamera = camera;
                vidInfo.updateEntry("Detected camera", camera || "---");

                let lens = "";
                if (additional_data.camera_identifier) {
                    const camera_id = additional_data.camera_identifier;
                    if (camera_id) {
                        if (camera_id.lens_model) { lens += camera_id.lens_model; }
                        if (camera_id.lens_info)  { lens += (lens? " " : "") + camera_id.lens_info; }
                    }
                }
                vidInfo.updateEntry("Detected lens", lens || "---");
                vidInfo.updateEntry("Contains gyro", additional_data.contains_motion? "Yes" : "No");
                // If source was detected, but gyro data is empty
                if (camera) {
                    if (additional_data.unsupported_lens) {
                        messageBox(Modal.Warning, qsTr("This video cannot be stabilized, because this lens doesn't support OSS metadata.\nDisable lens stabilization (Optical SteadyShot) in order to use Gyroflow."), [ { "text": qsTr("Ok") } ]);
                    }
                    if (additional_data.contains_raw_gyro && !additional_data.contains_quats) timeline.setDisplayMode(0); // Switch to gyro view
                    if (!additional_data.contains_raw_gyro && additional_data.contains_quats) timeline.setDisplayMode(3); // Switch to quaternions view
                }

                if (additional_data.hasOwnProperty("cam_posture") && additional_data.camera_type == "Insta360 GO 3S") {
                    vidInfo.updateEntryWithTrigger("Rotation", 360 - (+additional_data.cam_posture.replace("CameraRotate", "") + 90));
                } else if (additional_data.hasOwnProperty("cam_posture") && Math.abs(+additional_data.cam_posture.replace("CameraRotate", "")) > 0) {
                    vidInfo.updateEntryWithTrigger("Rotation", +additional_data.cam_posture.replace("CameraRotate", ""));
                }
                if (additional_data.hasOwnProperty("realtime_fps") && +additional_data.realtime_fps > 0) {
                    vidInfo.updateEntryWithTrigger("Frame rate", +additional_data.realtime_fps);
                }
                if (additional_data.hasOwnProperty("recording_settings") && Object.keys(additional_data.recording_settings).length > 0) {
                    vidInfo.cleanupModel();
                    let model = vidInfo.infoList.model;
                    model[""] = " ";
                    for (const x in additional_data.recording_settings) {
                        model[x] = additional_data.recording_settings[x];
                    }
                    vidInfo.infoList.model = model;
                    vidInfo.infoList.modelChanged();
                }
            }
            if (+additional_data.sample_rate > 0.0 && Math.round(+additional_data.sample_rate) < 50) {
                messageBox(Modal.Warning, qsTr("Motion data sampling rate is too low (%1 Hz).\n50 Hz is an absolute minimum and we recommend at least 200 Hz.").arg(additional_data.sample_rate.toFixed(0)), [ { "text": qsTr("Ok") } ]);
            }
            if (root.pendingGyroflowData) {
                Qt.callLater(loadGyroflowData, root.pendingGyroflowData, root.pendingQueueJobId);
            } else {
                Qt.callLater(controller.recompute_threaded);
                if (is_main_video) {
                    controller.load_default_preset();
                }
            }
            if (is_main_video && window.pendingLoadPreset) {
                Qt.callLater(loadGyroflowData, JSON.parse(window.pendingLoadPreset), 0);
                window.pendingLoadPreset = "";
            }
            if (!root.pendingGyroflowData && render_queue.editing_job_id > 0) {
                root.queueEditLoading = false;
            }
            if (is_main_video && root.pendingCrmTelemetryUrl && root.pendingCrmTelemetryUrl.toString()) {
                const crmUrl = root.pendingCrmTelemetryUrl;
                root.pendingCrmTelemetryUrl = "";
                window.motionData.lastSelectedFile = crmUrl;
                controller.load_telemetry(crmUrl, false, window.videoArea.vid, -1, 0);
            }
        }
        function onChart_data_changed(): void {
            timeline.triggerUpdateChart("");
        }
        function onZooming_data_changed(): void {
            timeline.triggerUpdateChart("8");
        }
        function updateKeyframesView(): void {
            controller.update_keyframes_view(timeline.getKeyframesView());
            controller.update_keyframe_values(vid.timestamp);
        }
        function onKeyframes_changed(): void {
            Qt.callLater(updateKeyframesView);
        }
        function onCompute_progress(id: real, progress: real): void {
            videoLoader.active = progress < 1;
            videoLoader.cancelable = false;
        }
        function onSync_progress(progress: real, ready: int, total: int): void {
            videoLoader.active = progress < 1;
            videoLoader.currentFrame = ready;
            videoLoader.totalFrames = total;
            videoLoader.additional = "";
            videoLoader.text = videoLoader.active? qsTr("Analyzing %1...") : "";
            videoLoader.progress = videoLoader.active? progress : -1;
            videoLoader.cancelable = true;
        }
        function onLoading_gyro_progress(progress: real): void {
            videoLoader.active = progress < 1;
            videoLoader.currentFrame = 0;
            videoLoader.totalFrames = 0;
            videoLoader.additional = "";
            videoLoader.text = videoLoader.active? qsTr("Loading gyro data %1...") : "";
            videoLoader.progress = videoLoader.active? progress : -1;
            videoLoader.cancelable = true;
        }
    }
    property Modal externalSdkModal: null;
    property var externalSdkSuccessCallback: null;

    function promptExternalSdkInstall(url, successCallback): bool {
        if (externalSdkModal !== null) {
            return false;
        }

        externalSdkSuccessCallback = successCallback || null;
        const dlg = messageBox(Modal.Info, qsTr("This format requires an external SDK. Do you want to download it now?"), [
            { text: qsTr("Yes"), accent: true, clicked: function() {
                dlg.btnsRow.children[0].enabled = false;
                controller.install_external_sdk(url.toString());
                return false;
            } },
            { text: qsTr("Cancel"), clicked: function() {
                externalSdkModal = null;
                externalSdkSuccessCallback = null;
            } },
        ]);
        externalSdkModal = dlg;
        dlg.addLoader();
        return true;
    }

    function loadFile(url: url, skip_detection: bool, queueJobId: int, crmTelemetryUrl: url, suppressAssociatedGyroflow: bool): void {
        // An explicit new load supersedes any pending post-suspend restore.
        root.restoringFromSuspend = false;
        root.suspendTimestamp = -1;
        const activeProjectFileUrl = controller.project_file_url ? controller.project_file_url.toString() : "";
        const skipAssociatedGyroflow = !!suppressAssociatedGyroflow;
        let filename = filesystem.get_filename(url);
        let folder = filesystem.get_folder(url);

        // .gyroflow routing must come BEFORE the loading gate: import_gyroflow_file
        // has its own wait_until_idle, so .gyroflow drops while a video load is
        // in progress are deferred via that path (LifecycleBusy → toast), not
        // by this gate.
        if (filename.endsWith(".gyroflow")) {
            return loadGyroflowData(url, queueJobId);
        }

        // Video-load-in-progress gate: refuse a second video while the first
        // is still resolving its metadata + telemetry. Closes the Bug 2 race
        // where double setUrl + double GPU invalidate triggered DXGI
        // device-removed. .gyroflow path above is intentionally exempt.
        if (controller.video_loading_in_progress) {
            messageBox(Modal.Warning, qsTr("Previous video is still loading, please wait..."), [{ text: qsTr("Ok") }]);
            return;
        }
        if (filename.endsWith(".RDC")) {
            // Assumes regular filesystem
            let parts = url.toString().split("/");
            parts.push(filename.replace(".RDC", "_001.R3D"));
            url = parts.join("/");
            filename = filesystem.get_filename(url);
            folder = filesystem.get_folder(url);
        }
        if (filename.toLowerCase().endsWith(".crm")) {
            messageBox(Modal.Error, qsTr("Canon CRM files must be loaded together with a same-name proxy video."), [ { text: qsTr("Ok") } ]);
            return;
        }

        // macOS TCC: probe access before handing the file to MDK. A permission
        // denial otherwise reaches MDK as videoWidth=0 and surfaces the misleading
        // "unsupported or invalid". User-picked files are usually granted
        // (user-intent), so this mainly catches programmatic/restored/CLI paths
        // (and GYROFLOW_FORCE_ACCESS_DENIED for testing).
        if (filesystem.check_file_access(url) === "denied") {
            window.showAccessDeniedDialog(url, false);
            return;
        }

        if (isMobile || filename.toLowerCase().endsWith(".r3d") || filename.toLowerCase().endsWith(".nev") || filename.toLowerCase().endsWith(".braw")) {
            // Preview resolution to 1080p
            if (isCalibrator && calibrator_window.lensCalib) {
                if (calibrator_window.lensCalib.previewResolution == 0) {
                    calibrator_window.lensCalib.previewResolution = 2;
                }
            } else {
                if (settings.value("previewResolution", -1) == -1 && window.advanced.previewResolution == 0) {
                    window.advanced.previewResolution = 2;
                }
            }
        }

        stabEnabledBtn.checked = false;

        if (controller.check_external_sdk(filename)) {
            promptExternalSdkInstall(url, null);
            return;
        }

        window.motionData.lastSelectedFile = "";
        if (!(/\.(png|jpg|exr|dng)$/i.test(filename) && filename.includes("%0"))) {
            root.loadedFileUrl = url;
        }

        if (isStorePackage && Qt.platform.os == "osx" && filename.toLowerCase().endsWith(".r3d") && folder.toString().length < 3) {
            messageBox(Modal.Info, qsTr("In order to load all R3D parts, you need to select the entire .RDC folder."), [
                { text: qsTr("OK"), accent: true, clicked: function() {
                    opf.selectFolder("", function(_) {
                        root.loadFile(root.loadedFileUrl, false, 0, "", skipAssociatedGyroflow);
                    });
                } },
            ], null, undefined, "open-rdc-folder");
            return;
        }

        if (!skip_detection) {
            let newUrl;
            if (newUrl = detectImageSequence(folder, filename)) {
                // Remember the first frame so telemetry (camera/lens) parses a real
                // file, not the %0Nd pattern url.
                controller.image_sequence_first_frame_url = filesystem.get_file_url(folder, filename, false);
                // DNG: try to get frame rate from telemetry, skip dialog if successful
                if (/\.dng$/i.test(filename)) {
                    const firstFileUrl = filesystem.get_file_url(folder, filename, false);
                    const detectedFps = controller.get_image_sequence_fps(firstFileUrl);
                    if (detectedFps > 0) {
                        controller.image_sequence_fps = detectedFps;
                        loadFile(newUrl, true, 0, crmTelemetryUrl, skipAssociatedGyroflow);
                        vid.setFrameRate(detectedFps);
                        return;
                    }
                }
                const dlg = messageBox(Modal.Info, qsTr("Image sequence has been detected.\nPlease provide frame rate: "), [
                    { text: qsTr("Ok"), accent: true, clicked: function() {
                        const fps = dlg.mainColumn.children[1].value;
                        settings.setValue("imageSequenceFps", fps);
                        controller.image_sequence_fps = fps;
                        loadFile(newUrl, true, 0, crmTelemetryUrl, skipAssociatedGyroflow);
                        vid.setFrameRate(fps);
                    } },
                    { text: qsTr("Cancel") },
                ]);
                const nf = Qt.createComponent("components/NumberField.qml").createObject(dlg.mainColumn, { precision: 3, unit: "fps", value: +settings.value("imageSequenceFps", "30") });
                nf.anchors.horizontalCenter = dlg.mainColumn.horizontalCenter;
                return;
            }
            let sequenceList;
            if (sequenceList = detectVideoSequence(folder, filename)) {
                const list = "<b>" + sequenceList.join(", ") + "</b>";
                const dlg = messageBox(Modal.Info, qsTr("Split recording has been detected, do you want to automatically join the files (%1) to create one full clip?").arg(list), [
                    { text: qsTr("Yes"), accent: true, clicked: function() {
                        dlg.btnsRow.children[0].enabled = false;
                        getOutputFile(folder, sequenceList[0], "_joined", "", true, function(outFolder, outFilename, outFullFileUrl) {
                            root.mergedFiles = sequenceList.map(x => filesystem.get_file_url(folder, x, false).toString());
                            controller.mp4_merge(sequenceList.map(x => filesystem.get_file_url(folder, x, false).toString()), outFolder, outFilename);
                        });
                        return false;
                    } },
                    { text: qsTr("No"), clicked: function() {
                        externalSdkModal = null;
                        loadFile(url, true, 0, crmTelemetryUrl, skipAssociatedGyroflow);
                    } },
                ])
                externalSdkModal = dlg;
                dlg.addLoader();
                return;
            }
        }
        // Folder-scan (sidecar/sequence/project detection) needs the input directory.
        // Non-sandboxed: even when the file itself is readable via a user-intent grant,
        // scanning the folder can be TCC-denied — detect it so the InfoMessage offers
        // the Settings affordance instead of silently finding nothing.
        vidInfo.hasAccessToInputDirectory = folder.toString().length > 3 && (isSandboxed || filesystem.check_file_access(folder) !== "denied");

        window.stab.fovSlider.value = 1.0;
        vid.loaded = false;
        videoLoader.active = true;
        vidInfo.loader = true;
        //vid.url = url;
        vid.errorShown = false;
        if (queueJobId > 0) {
            render_queue.editing_job_id = queueJobId;
            root.queueEditLoading = true;
        } else {
            render_queue.editing_job_id = 0;
            root.queueEditLoading = false;
        }
        root.pendingExternalGyroFallbackUrl = "";
        root.pendingExternalGyroFallbackProjectVersion = 0;
        root.pendingCrmTelemetryUrl = crmTelemetryUrl || "";
        controller.load_video(url, vid);
        if (!isCalibrator) {
            const suffix = window.advanced.defaultSuffix.text;
            window.outputFile.setFilename(filesystem.filename_with_suffix(filename, suffix).replace(/%0[0-9]+d/, ""));

            const preservedPath = settings.value("preservedOutputPath", "");
            if (window.exportSettings.preserveOutputPath.checked && preservedPath) {
                window.outputFile.setFolder(preservedPath);
            } else {
                window.outputFile.setFolder(folder);
            }
            window.exportSettings.updateCodecParams();
        }
        // Associated .gyroflow prompt is deferred to fileLoaded() so it
        // only fires once vidInfo.filename is set. Asking earlier (while
        // metadata is still being decoded) lets a Yes-click route through
        // loadGyroflowData → isCorrectVideoLoaded=false → reload-original
        // path → late telemetry_loaded then clears pendingGyroflowData and
        // load_default_preset overwrites the imported project (Bug 1).
        root.skipAssociatedGyroflowOnLoad = !!suppressAssociatedGyroflow;

        dropText.loadingFile = filename;
        vidInfo.cleanupModel();
        vidInfo.updateEntry("File name", filename);
        vidInfo.updateEntry("Detected camera", "---");
        vidInfo.updateEntry("Detected lens", "---");
        vidInfo.updateEntry("Contains gyro", "---");
        timeline.editingSyncPoint = false;
    }
    // Sibling-`.gyroflow` prompt logic, deferred from loadFile() to fileLoaded().
    // Runs once the main video's metadata is decoded and vidInfo.filename is set;
    // a Yes click then routes through loadGyroflowData → isCorrectVideoLoaded=true
    // → import_gyroflow_file directly (no reload-original detour, no late
    // load_default_preset overwriting the project).
    function maybePromptAssociatedGyroflow(): void {
        const suppress = root.skipAssociatedGyroflowOnLoad;
        // Reset immediately so subsequent loads start clean even if we early-return.
        root.skipAssociatedGyroflowOnLoad = false;

        if (root.pendingGyroflowData) return;
        if (suppress) return;
        if (!vid.loaded) return;
        if (!vidInfo.filename) return;

        const url = root.loadedFileUrl;
        if (!url || !url.toString()) return;
        const folder = filesystem.get_folder(url);
        let gfBaseFilename = vidInfo.filename;
        if (gfBaseFilename.includes("%0")) {
            gfBaseFilename = gfBaseFilename.replace(/%0(\d+)d/, (_, len) => controller.image_sequence_start.toString().padStart(parseInt(len), '0'));
        }
        const gfFilename = filesystem.filename_with_extension(gfBaseFilename, "gyroflow");
        if (!filesystem.exists_in_folder(folder, gfFilename)) return;

        const gfUrl = filesystem.get_file_url(folder, gfFilename, false);
        const activeProjectFileUrl = controller.project_file_url ? controller.project_file_url.toString() : "";
        if (activeProjectFileUrl && activeProjectFileUrl == gfUrl.toString()) {
            Qt.callLater(() => loadFile(gfUrl, true, 0, "", true));
        } else {
            messageBox(Modal.Question, qsTr("There's a %1 file associated with this video, do you want to load it?").arg("<b>" + gfFilename + "</b>"), [
                { text: qsTr("Yes"), clicked: function() {
                    Qt.callLater(() => loadFile(gfUrl, true));
                } },
                { text: qsTr("No"), accent: true },
            ]);
        }
    }
    function loadCrmProxyPair(pair: var, skip_detection: bool): void {
        if (!pair || !pair.crm_url || !pair.proxy_url) {
            messageBox(Modal.Error, qsTr("Canon CRM files must be loaded together with a same-name proxy video."), [ { text: qsTr("Ok") } ]);
            return;
        }
        root.loadFile(Qt.url(pair.proxy_url), skip_detection, 0, Qt.url(pair.crm_url));
    }
    function fileExtension(url: url): string {
        const filename = filesystem.get_filename(url).toLowerCase();
        const dot = filename.lastIndexOf(".");
        return dot >= 0 ? filename.substring(dot + 1) : "";
    }
    function isVideoOrProjectFile(url: url): bool {
        const videoFirstExtensions = ["mp4", "mov", "mxf", "insv", "braw", "r3d", "nev", "crm", "gyroflow"];
        return videoFirstExtensions.indexOf(fileExtension(url)) >= 0;
    }
    // NOTE: this returns true for `*_mix.bin` as well, because it *is* motion data.
    // Routing, however, is decided at the call sites: they check
    // `render_queue.is_gyro_mix_file()` first and send device gyro files to the render
    // queue instead. Deleting the short-circuit below would not change that - `bin` is
    // itself in MotionData.qml's extension table, so the loop would match it anyway.
    function isSingleMotionDataFile(url: url): bool {
        if (render_queue.is_gyro_mix_file(url.toString())) return true;
        if (isVideoOrProjectFile(url)) return false;
        const extensions = window.motionData ? window.motionData.extensions : [];
        const ext = fileExtension(url);
        for (const accepted of extensions || []) {
            if (ext === accepted.toString().replace(/^\./, "").toLowerCase()) return true;
        }
        return false;
    }
    // A standalone device gyro file (*_mix.bin) means the user is entering the batch
    // matching workflow: deep match, clock-shift learning and cross-clip propagation all
    // live on the render-queue side only. Reveal the queue and promote whatever clip is
    // currently in the main preview into a job, so a "one clip + one gyro file" import
    // reaches the same state as a multi-clip import.
    //
    // Reuses only the shared `render_queue.add()` entry point, deliberately NOT
    // `renderBtn.render()`'s pre-flight chain (overwrite prompt, REDline notice, AMD
    // bitrate warning, sandbox folder picker) - promotion turns footage into a queue
    // entry, it does not start an export. It also never calls render_queue.start().
    function promotePreviewToQueue(): void {
        if (!queue.item) return;
        // The queue is revealed on every path, including the skip cases below: gyro data
        // has arrived, so the queue should become visible regardless.
        queue.item.shown = true;

        if (!vid.loaded || !vidInfo.filename) {
            console.log("[gyro_promote] skip_no_video");
            return;
        }
        if (controller.video_loading_in_progress) {
            // Reading stabilization state mid-load would bypass the video-load guard.
            console.log("[gyro_promote] skip_loading");
            return;
        }
        if (render_queue.editing_job_id > 0) {
            // The preview already *is* a queue job (reached via right-click Edit), so the
            // row exists. Calling add() here would reuse that id and silently overwrite
            // the job with an edit the user never asked to save.
            console.log("[gyro_promote] skip_editing_job id=" + render_queue.editing_job_id);
            return;
        }

        vid.grabToImage(function(result) {
            const job_id = render_queue.add(window.getAdditionalProjectDataJson(), controller.image_to_b64(result.image));
            // add() clears editing_job_id internally, so bind it back: the main preview
            // must stay the edit view of this job, otherwise parameters tweaked in the
            // preview after promotion would silently not reach the exported job.
            // Assigning the property does not reload the video.
            render_queue.editing_job_id = job_id;
            console.log("[gyro_promote] promoted job_id=" + job_id);
        }, Qt.size(50 * dpiScale * vid.parent.ratio, 50 * dpiScale));
    }

    function loadMultipleFiles(urls: list<url>, skip_detection: bool): void {
        if (urls.length > 0) {
            let hasCrm = false;
            let crmCount = 0;
            const urlStrings = [];
            for (const url of urls) {
                urlStrings.push(url.toString());
                if (filesystem.get_filename(url).toLowerCase().endsWith(".crm")) {
                    hasCrm = true;
                    crmCount++;
                }
            }
            if (hasCrm) {
                try {
                    const pairs = JSON.parse(render_queue.crm_proxy_pairs(JSON.stringify(urlStrings)));
                    const firstVideoUrl = render_queue.first_renderable_video_file(
                        JSON.stringify(urlStrings),
                        JSON.stringify(fileDialog.extensions)
                    );
                    const hasRenderableVideo = !!firstVideoUrl;
                    if (pairs.length === 1 && urls.length === 2) {
                        const pair = JSON.parse(render_queue.crm_proxy_pair(JSON.stringify(urlStrings)));
                        loadCrmProxyPair(pair, skip_detection);
                    } else if (pairs.length === crmCount) {
                        queue.item.shown = true;
                        Qt.callLater(function() { queue.item.dt.loadFiles(urls); });
                    } else if (hasRenderableVideo) {
                        const pairedCrmUrls = {};
                        for (const pair of pairs) pairedCrmUrls[pair.crm_url] = true;
                        queue.item.shown = true;
                        const filteredCrmUrls = urls.filter(u => !filesystem.get_filename(u).toLowerCase().endsWith(".crm") || pairedCrmUrls[u.toString()]);
                        Qt.callLater(function() { queue.item.dt.loadFiles(filteredCrmUrls); });
                    } else {
                        messageBox(Modal.Error, qsTr("Canon CRM files must be loaded together with a same-name proxy video."), [ { text: qsTr("Ok") } ]);
                    }
                } catch (e) {
                    console.log("crm_proxy_pair failed:", e);
                    messageBox(Modal.Error, qsTr("Canon CRM files must be loaded together with a same-name proxy video."), [ { text: qsTr("Ok") } ]);
                }
                return;
            }
        }
        const originalUrlCount = urls.length;
        try {
            const filteredJson = render_queue.filter_paired_gyroflow_siblings(
                JSON.stringify(urls.map(u => u.toString())),
                JSON.stringify(fileDialog.extensions)
            );
            urls = JSON.parse(filteredJson);
        } catch (e) {
            console.log("filter_paired_gyroflow_siblings failed:", e);
        }
        const droppedPairedGyroflow = urls.length < originalUrlCount;
        try {
            const filteredJson = render_queue.filter_raw_proxy_siblings(
                JSON.stringify(urls.map(u => u.toString())),
                JSON.stringify(fileDialog.extensions)
            );
            urls = JSON.parse(filteredJson);
        } catch (e) {
            console.log("filter_raw_proxy_siblings failed:", e);
        }
        try {
            const filteredJson = render_queue.filter_non_source_inputs(
                JSON.stringify(urls.map(u => u.toString()))
            );
            urls = JSON.parse(filteredJson);
        } catch (e) {
            console.log("filter_non_source_inputs failed:", e);
        }
        if (urls.length == 1) {
            // Same ordering as the drop handler: device gyro files go to the render queue,
            // everything else keeps the motion-data / main-preview routing. This branch is
            // what the main file dialog and the Android picker callback funnel through, so
            // opening `[video, *_mix.bin]` and dropping it must not disagree.
            if (render_queue.is_gyro_mix_file(urls[0].toString())) {
                render_queue.add_gyro_file(urls[0].toString());
                root.promotePreviewToQueue();
                return;
            }
            if (window.motionData && isSingleMotionDataFile(urls[0])) {
                window.motionData.loadFile(urls[0]);
                return;
            }
            root.loadFile(urls[0], skip_detection, 0, "", droppedPairedGyroflow);
            return;
        }
        if (urls.length < 1) return;

        // If no renderable video remains and this is only .gyroflow data,
        // preserve legacy behavior: load the first one in the main area.
        const firstVideoUrl = render_queue.first_renderable_video_file(
            JSON.stringify(urls.map(u => u.toString())),
            JSON.stringify(fileDialog.extensions)
        );
        let allGyroflow = true;
        for (const u of urls) {
            if (!filesystem.get_filename(u).toLowerCase().endsWith(".gyroflow")) {
                allGyroflow = false;
                break;
            }
        }
        if (!firstVideoUrl && allGyroflow) {
            root.loadFile(urls[0], skip_detection);
            return;
        }

        // Multiple items → batch into render queue. queue.item.dt.loadFiles
        // handles .gyroflow (project or preset) and video URLs uniformly.
        const urlsCopy = [...urls];
        queue.item.shown = true;
        Qt.callLater(function() { queue.item.dt.loadFiles(urlsCopy); });
    }

    function askForOutputLocation(folder: url, filename: string, choice: bool, cb: var): void {
        const dlg = messageBox(Modal.Question, qsTr("Please enter the output path:"), [
            { text: qsTr("Ok"), accent: true, clicked: function() {
                if (choice) {
                    if (dlg.mainColumn.children[1].children[0].checked) { cb("", ""); }
                    if (dlg.mainColumn.children[1].children[1].checked) { const opf = dlg.mainColumn.children[1].children[3]; cb(opf.folderUrl, opf.filename, opf.fullFileUrl); }
                } else {
                    const opf = dlg.mainColumn.children[1];
                    if (!opf.folderUrl.toString() && !opf.fullFileUrl.toString()) {
                        opf.prompt();
                        return false;
                    }
                    cb(opf.folderUrl, opf.filename, opf.fullFileUrl);
                }
            } },
            { text: qsTr("Cancel") },
        ]);

        if (choice) {
            let col = Qt.createQmlObject(`import QtQuick; import "components/";
                Column {
                    width: parent.width;
                    RadioButton { checked: true; }
                    RadioButton { id: custom; }
                    Item { height: 10 * dpiScale; width: 1; }
                    OutputPathField { enabled: custom.checked; folderOnly: true; }
                }`, dlg.mainColumn, "dlgRadios");
            col.children[0].text = qsTr("Same as the original file");
            col.children[1].text = qsTr("Custom path");
            col.children[3].setFolder(folder);
        } else {
            const opf = Qt.createComponent("components/OutputPathField.qml").createObject(dlg.mainColumn, { });
            opf.setFolder(folder);
            opf.setFilename(filename);
        }
    }
    function getOutputFile(folder: url, filename: string, suffix: string, extension: string, ask: bool, cb: var): void {
        if (suffix) filename = filesystem.filename_with_suffix(filename, suffix);
        if (extension) filename = filesystem.filename_with_extension(filename, extension);
        if (ask) {
            askForOutputLocation(folder, filename, false, cb);
        } else {
            cb(folder, filename);
        }
    }

    function detectImageSequence(folder: url, filename: string): var {
        if (!filename.includes("%0")) {
            controller.image_sequence_start = 0;
            controller.image_sequence_fps = 0;
            controller.image_sequence_first_frame_url = "";
        }
        if (/\d+\.(png|jpg|exr|dng)$/i.test(filename)) {
            let firstNum = filename.match(/(\d+)\.(png|jpg|exr|dng)$/i);
            if (firstNum[1]) {
                const ext = firstNum[2];
                firstNum = firstNum[1];
                const firstNumNum = parseInt(firstNum, 10);
                for (let i = firstNumNum + 1; i < firstNumNum + 5; ++i) { // At least 5 frames
                    const newNum = i.toString().padStart(firstNum.length, '0');
                    const newName = filename.replace(firstNum + "." + ext, newNum + "." + ext);
                    if (!filesystem.exists_in_folder(folder, newName)) {
                        return false;
                    }
                }
                controller.image_sequence_start = firstNumNum;
                return filesystem.get_file_url(folder, filename.replace(`${firstNum}.${ext}`, `%0${firstNum.length}d.${ext}`), false);
            }
        }
        return false;
    }
    function detectVideoSequence(folder: url, filename: string): var {
        // url pattern, 1st file index, new path function
        const patterns = [
            // GoPro 1-5
            [/((?:GOPR|GP\d{2})(\d{4})\.MP4)$/i, 0, function(match, i) {
                return (i == 0 ? "GOPR" : "GP" + i.toString().padStart(2, '0')) + match.substring(4);
            }],
            // GoPro 6+
            [/(G[XH]\d{2}(\d{4})\.MP4)$/i, 1, function(match, i) {
                return match.substring(0, 2) + i.toString().padStart(2, '0') + match.substring(4);
            }],
            // DJI Action
            [/(DJI_\d+_(\d+)\.MP4)$/i, null, function(match, i) {
                return match.substring(0, 9) + i.toString().padStart(3, '0') + match.substring(12);
            }],
        ];
        for (const x of patterns) {
            let match = filename.match(x[0]);
            if (match && match[1]) {
                let list = [];
                const firstNum = (x[1] !== null ? x[1] : parseInt(match[2], 10));
                for (let i = firstNum; i < firstNum + 99; ++i) { // Max 99 parts
                    const newName = filename.replace(match[1], x[2](match[1], i));
                    if (filesystem.exists_in_folder(folder, newName)) {
                        list.push(newName);
                    } else {
                        break;
                    }
                }
                if (list.length > 1)
                    return list;
            }
        }
        return false;
    }
    OutputPathField { id: opf; visible: false; }

    Item {
        id: vidParentParent;
        width: parent.width;
        height: parent.height - (root.fullScreen || window.isMobileLayout? 0 : tlcol.height);

        Grid {
            readonly property bool vertical: vidParentParent.height - vidParent.height * 2 > vidParentParent.width - vidParent.width * 2;
            columns: secondPreview.visible? (vertical? 1 : 2) : 1;
            rows:    secondPreview.visible? (vertical? 2 : 1) : 1;
            anchors.centerIn: parent;
            spacing: 10 * dpiScale;
            Item {
                id: vidParent;
                readonly property real orgW: (stabEnabledBtn.checked && root.outWidth > 0? root.outWidth : (vid.videoWidth * window.lensProfile.input_horizontal_stretch));
                readonly property real orgH: (stabEnabledBtn.checked && root.outHeight > 0? root.outHeight : (vid.videoHeight * window.lensProfile.input_vertical_stretch));
                readonly property real ratio: orgW / Math.max(1, orgH);
                readonly property real w: vidParentParent.width  / parent.columns - (root.fullScreen? 0 : 20 * dpiScale);
                readonly property real h: vidParentParent.height / parent.rows    - (root.fullScreen? 0 : 20 * dpiScale);

                width:  (ratio * h) > w ? w : (ratio * h)
                height: (ratio * h) > w ? (w / ratio) : h
                opacity: da.containsDrag? 0.5 : 1.0;

                /*Image {
                    // Transparency grid
                    fillMode: Image.Tile;
                    anchors.fill: parent;
                    source: "data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='14' height='14'><rect fill='%23fff' x='0' y='0' width='7' height='7'/><rect fill='%23aaa' x='7' y='0' width='7' height='7'/><rect fill='%23aaa' x='0' y='7' width='7' height='7'/><rect fill='%23fff' x='7' y='7' width='7' height='7'/></svg>"
                }*/

                MDKVideo {
                    id: vid;
                    visible: opacity > 0;
                    opacity: loaded? 1 : 0;
                    Ease on opacity { }
                    anchors.fill: parent;
                    property bool loaded: false;

                    property bool stabEnabled: stabEnabledBtn.checked;
                    transform: [
                        Scale {
                            readonly property real r: vidInfo.videoRotation * (Math.PI / 180);
                            readonly property real rotW: Math.abs(vidParent.width * Math.cos(r)) + Math.abs(vidParent.height * Math.sin(r));
                            readonly property real rotH: Math.abs(vidParent.width * Math.sin(r)) + Math.abs(vidParent.height * Math.cos(r));
                            origin.x: vid.width / 2; origin.y: vid.height / 2;
                            xScale: vid.stabEnabled? 1 : Math.min(vidParent.h / rotH, vidParent.w / rotW) * (fovOverviewBtn.checked? 0.5 : 1);
                            yScale: xScale;
                        },
                        Rotation {
                            origin.x: vid.width / 2; origin.y: vid.height / 2;
                            angle: vid.stabEnabled? 0 : -vidInfo.videoRotation;
                        }
                    ]

                    function fovChanged(): void {
                        const fov = controller.current_fov;
                        const focal_length = controller.current_focal_length;
                        const crop_factor = window.lensProfile?.cropFactor || 1.0;
                        // const ratio = controller.get_scaling_ratio(); // this shouldn't be called every frame because it locks the params mutex
                        // Normalize per-frame fov so the displayed zoom % matches the panel's "Max zoom"
                        // value for anamorphic clips (where render-side fov was *= width/output_width
                        // and would otherwise inflate the displayed percentage). Mirrors the effective
                        // input width used by StabilizationParams::set_fovs.
                        const hStretch = Math.max(1.0, window.lensProfile?.input_horizontal_stretch || 1.0);
                        const displayFov = fov * hStretch;
                        currentFovText.text = qsTr("Zoom: %1").arg(displayFov > 0? (100 / displayFov).toFixed(2) + "%" : "---");

                        if (+focal_length > 0) {
                            const fl = +focal_length / fov;
                            currentFovText.text += "\n" + qsTr("Focal length: %1 mm").arg(fl.toFixed(2));
                            if (crop_factor && crop_factor != 1.0) {
                                currentFovText.text += " (" + qsTr("full frame equiv.: %1 mm").arg((fl * crop_factor).toFixed(2)) + ")";
                            }
                        }
                    }

                    function updateTurnSpeed(): void {
                        const turnSpeed = controller.get_turn_speed(vid.timestamp);
                        if (isNaN(turnSpeed)) {
                            turnSpeedValue.text = "---";
                        } else {
                            const xAngle = controller.get_x_angle(vid.timestamp);
                            turnSpeedValue.text = turnSpeed.toFixed(2) + "°/s (" + xAngle.toFixed(2) + "°)";
                        }
                    }

                    onCurrentFrameChanged: {
                        fovChanged();
                        controller.update_keyframe_values(timestamp);
                        window.motionData.orientationIndicator.updateOrientation(timeline.position * timeline.durationMs * 1000);
                        updateTurnSpeed();
                    }
                    onMetadataLoaded: (md) => {
                        Qt.callLater(fileLoaded, md);
                    }
                    function fileLoaded(md: var): void {
                        if (root.restoringFromSuspend) {
                            // Post-suspend player-level reload: Rust-side state
                            // (telemetry, stabilization) is intact — only seek
                            // back and restore the pause state. Everything else
                            // (telemetry reload, trim reset, prompts) must NOT run.
                            root.restoringFromSuspend = false;
                            loaded = vid.videoWidth > 0;
                            console.log("resume restore: loaded=" + loaded + " seeking to ts=" + root.suspendTimestamp);
                            if (loaded) {
                                vid.seekToTimestamp(root.suspendTimestamp, true);
                                if (root.suspendWasPlaying) vid.play(); else vid.pause();
                            }
                            return;
                        }
                        loaded = vid.videoWidth > 0;
                        videoLoader.active = false;
                        vidInfo.loader = false;
                        timeline.resetTrim();
                        timeline.resetZoom();

                        controller.video_file_loaded(vid);
                        window.motionData.filename = "";

                        // Always refresh telemetry from the main video first so Video Information
                        // reflects the video's own telemetry-parser metadata during project restore.
                        controller.load_telemetry(root.loadedFileUrl, true, vid, -1, 0);
                        vidInfo.loadFromVideoMetadata(md, vid.videoWidth, vid.videoHeight);
                        window.sync.customSyncTimestamps = [];

                        if (root.mergedFiles.length > 1) {
                            if (loaded) {
                                const copy = [...root.mergedFiles];
                                messageBox(Modal.Question, qsTr("Files merged successfully, do you want to delete the original ones?"), [
                                    { text: qsTr("Yes"), clicked: function() {
                                        for (const x of copy) {
                                            filesystem.move_to_trash(x);
                                        }
                                        return true;
                                    } },
                                    { text: qsTr("No"), accent: true },
                                ], null, undefined, "delete-after-join");
                            }
                            root.mergedFiles = [];
                        }

                        window.lensProfile.selected_manually = false;

                        // Deferred associated .gyroflow prompt — fires here
                        // (after vidInfo.loadFromVideoMetadata) so vidInfo.filename
                        // is set and loadGyroflowData's isCorrectVideoLoaded check
                        // resolves true on the Yes path (avoids the reload-original
                        // detour that opens the Bug 1 race).
                        root.maybePromptAssociatedGyroflow();

                        // for (var i in md) console.info(i, md[i]);
                    }
                    property bool errorShown: false;
                    onMetadataChanged: {
                        controller.log_video_metadata_state(vid.videoWidth, vid.videoHeight, vid.duration, vid.frameRate, vid.frameCount);
                        // Post-suspend restore drives its own seek in fileLoaded
                        // (which runs later via callLater and clears the flag);
                        // the buffer nudge below would fight it back to frame 0.
                        if (root.restoringFromSuspend && vid.videoWidth > 0) return;
                        if (vid.videoWidth > 0) {
                            // Trigger seek to buffer the video frames
                            if (vid.duration == 0) {
                                vid.play();
                                Qt.callLater(function() {
                                    stabEnabledBtn.checked = true;
                                    vid.volume = volumeSlider.value / 100.0;
                                })
                            } else {
                                bufferTrigger.start();
                            }
                        } else if (!errorShown) {
                            // Failed post-suspend restore must not leave the flag
                            // armed — it would short-circuit the next real load.
                            root.restoringFromSuspend = false;
                            // Re-probe: MDK reports videoWidth=0 for permission denials too.
                            // Upgrade the generic "unsupported" message to the actionable
                            // permission dialog when the real cause is a TCC block.
                            if (filesystem.check_file_access(root.loadedFileUrl) === "denied") {
                                window.showAccessDeniedDialog(root.loadedFileUrl, false);
                            } else {
                                messageBox(Modal.Error, qsTr("Failed to load the selected file, it may be unsupported or invalid."), [ { "text": qsTr("Ok") } ]);
                            }
                            errorShown = true;
                            dropText.loadingFile = "";
                            root.pendingGyroflowData = null;
                            stabEnabledBtn.checked = true;
                            // Release the video-load guard. load_telemetry::finished
                            // will never fire (MDK reported videoWidth=0), so without
                            // this call the guard would hang until the watchdog.
                            controller.abort_pending_video_load();
                        }
                    }
                    Timer {
                        id: bufferTrigger;
                        interval: 150;
                        onTriggered: {
                            if (!vid.videoWidth) bufferTrigger.start();
                            Qt.callLater(() => {
                                vid.currentFrame++;
                                Qt.callLater(() => vid.currentFrame = 0);
                                if (vid.videoWidth) {
                                    stabEnabledBtn.checked = true;
                                    vid.volume = volumeSlider.value / 100.0;
                                }
                            });
                        }
                    }

                    backgroundColor: "#111111";
                    Component.onCompleted: {
                        controller.init_player(this);
                    }
                    Rectangle {
                        border.color: styleVideoBorderColor;
                        border.width: 1 * dpiScale;
                        color: "transparent";
                        radius: 5 * dpiScale;
                        anchors.fill: parent;
                        anchors.margins: -border.width;
                    }
                }

                TapHandler {
                    onTapped: timeline.focus = true;
                    onDoubleTapped: root.fullScreen = root.fullScreen? 0 : 1;
                }
                GridGuide {
                    id: gridGuide;
                    anchors.fill: vid;
                    canShow: vid.loaded;
                }
            }
            Item {
                id: secondPreview;
                property bool show: false;
                onShowChanged: settings.setValue("stabOverviewSplit", show);
                Component.onCompleted: show = settings.value("stabOverviewSplit", false);
                visible: show && fovOverviewBtn.checked;
                readonly property real ratio: 1 + 1 / window.stab.fovSlider.value;
                onRatioChanged: {
                    if (visible) {
                        vid.forceRedraw();
                        vidParent.widthChanged();
                    }
                }
                width: vidParent.width;
                height: vidParent.height;
                ShaderEffectSource {
                    id: secondPreviewSource;
                    live: secondPreview.visible;
                    width: parent.width; height: parent.height;
                    sourceItem: vidParent;
                    sourceRect: Qt.rect((vidParent.width - (vidParent.width / secondPreview.ratio)) / 2, (vidParent.height - (vidParent.height / secondPreview.ratio)) / 2, vidParent.width / secondPreview.ratio, vidParent.height / secondPreview.ratio);
                }
                TapHandler {
                    onTapped: timeline.focus = true;
                    onDoubleTapped: root.fullScreen = root.fullScreen? 0 : 1;
                }
            }
        }

        Rectangle {
            id: dropRect;
            border.width: vid.loaded? 0 : (3 * dpiScale);
            border.color: style === "light"? Qt.darker(styleBackground, 1.3) : Qt.lighter(styleBackground, 2);
            anchors.fill: parent;
            anchors.margins: vid.loaded? 0 : (20 * dpiScale);
            anchors.topMargin: vid.loaded? 0 : (50 * dpiScale);
            anchors.bottomMargin: vid.loaded? 0 : (50 * dpiScale);
            color: styleBackground;
            radius: 5 * dpiScale;
            opacity: da.containsDrag? (vid.loaded? 0.8 : 0.3) : vid.loaded? 0 : 1.0;
            Ease on opacity { duration: 300; }
            visible: opacity > 0;
            onVisibleChanged: if (!visible) dropText.loadingFile = "";

            BasicText {
                id: dropText;
                property string loadingFile: "";
                // [queue-gyro-column] 拖拽提示更新，支持陀螺仪数据
                text: loadingFile? qsTr("Loading %1...").arg(loadingFile) : (Qt.platform.os == "ios" || Qt.platform.os == "android"? qsTr("Click here to open a video file") : qsTranslate("RenderQueue", "Drop video files or gyroscope data here"));
                font.pixelSize: (window.isMobileLayout? 23 : 30) * dpiScale;
                anchors.centerIn: parent;
                leftPadding: 0;
                scale: dropText.contentWidth > (parent.width - 50 * dpiScale)? (parent.width - 50 * dpiScale) / dropText.contentWidth : 1.0;
            }
            ItemLoader {
                anchors.fill: dropText;
                anchors.margins: -30 * dpiScale;
                visible: !dropText.loadingFile && !vid.loaded;
                scale: dropText.scale;
                sourceComponent: Component { DropTargetRect { } }
            }
            ItemLoader {
                anchors.fill: parent;
                anchors.margins: 5 * dpiScale;
                visible: !dropText.loadingFile && vid.loaded;
                sourceComponent: Component { DropTargetRect { } }
            }
            MouseArea {
                visible: !vid.loaded;
                anchors.fill: parent;
                cursorShape: Qt.PointingHandCursor;
                onClicked: vidInfo.selectFileRequest();
            }
        }
        DropArea {
            id: da;
            anchors.fill: dropRect;
            enabled: queue.item && !queue.item.shown && !queue.item.isDragging;

            onEntered: (drag) => {
                const count = drag.urls.length;
                if (!count) {
                    console.log("[main_drop:hover] urls=0 accepted=false reason=no_urls");
                    return;
                }
                drag.accepted = (count === 1 && isSingleMotionDataFile(drag.urls[0]))
                    || DropRules.acceptsAnyUrl(drag.urls, fileDialog.extensions, ["_mix.bin", ".rdc", ".rdm"]);
                console.log("[main_drop:hover] urls=" + count + " accepted=" + drag.accepted);
            }
            onDropped: (drop) => {
                const dropCount = drop.urls.length;
                console.log("[main_drop:drop] urls=" + dropCount);
                if (isCalibrator) {
                    calibrator_window.loadFiles(drop.urls);
                    console.log("[main_drop:drop] calibrator_dispatched=" + dropCount);
                    return;
                }
                // Device gyro files belong to the render queue (deep match / clock-shift
                // learning), never to the current-video motion-data path. This check must
                // stay ahead of isSingleMotionDataFile: `bin` is in MotionData.qml's
                // extension table, so the generic check would swallow *_mix.bin.
                if (dropCount === 1 && render_queue.is_gyro_mix_file(drop.urls[0].toString())) {
                    render_queue.add_gyro_file(drop.urls[0].toString());
                    root.promotePreviewToQueue();
                    console.log("[main_drop:dispatch] files=1 target=queue_gyro");
                    return;
                }
                // Other motion-data formats (.bbl / .gcsv / plain .bin blackbox logs) keep
                // loading as the current video's external gyro source.
                if (dropCount === 1 && isSingleMotionDataFile(drop.urls[0])) {
                    root.loadMultipleFiles(drop.urls, false);
                    console.log("[main_drop:dispatch] files=1 target=motion_data");
                    return;
                }
                // [queue-pair-ux T6] separate folders from loose files
                let fileUrls = [];
                let folderUrls = [];
                let hasGyroFile = false;
                let filteredUrls = drop.urls;
                try {
                    let urlStrings = [];
                    for (const url of drop.urls) urlStrings.push(url.toString());
                    filteredUrls = JSON.parse(render_queue.filter_supported_drop_items(
                        JSON.stringify(urlStrings),
                        JSON.stringify(fileDialog.extensions)
                    ));
                } catch (e) {
                    console.log("filter_supported_drop_items failed:", e);
                }
                console.log("[main_drop:filter] input=" + dropCount + " filtered=" + filteredUrls.length);
                if (!filteredUrls.length) {
                    console.log("[main_drop:drop] reason=filtered_empty");
                    return;
                }
                for (const url of filteredUrls) {
                    const fname = filesystem.get_filename(url).toLowerCase();
                    if (render_queue.is_gyro_mix_file(url.toString())) {
                        render_queue.add_gyro_file(url.toString());
                        hasGyroFile = true;
                    } else if (fname.endsWith(".bin")) {
                        continue;
                    } else if (filesystem.is_dir(url)) {
                        // Defer folder expansion to the render queue's loadFiles
                        // handler — the single path that collapses image sequences
                        // (consecutive frames -> one %0Nd job) and resolves their
                        // frame rate. Passing the folder url (not the scanned
                        // result objects) avoids the list<url> coercion that would
                        // turn objects into empty urls, and prevents the folder
                        // from being expanded twice. add_gyro_folder is also called
                        // by the queue handler, so it is not invoked here.
                        folderUrls.push(url);
                    } else {
                        fileUrls.push(url);
                    }
                }
                // Open the render queue panel when a gyro file is dropped (gyro
                // files always pair into the queue).
                if (hasGyroFile && queue.item) {
                    queue.item.shown = true;
                }
                // A single folder with no loose files = one clip's worth of media.
                // Load it in the main preview area just like a single video (a DNG
                // image sequence becomes one clip via detectImageSequence). Only
                // fan out to the render queue when there is more than one clip.
                if (folderUrls.length === 1 && fileUrls.length === 0) {
                    try {
                        const jsonStr = render_queue.list_video_files_in_folder(
                            folderUrls[0].toString(),
                            JSON.stringify(fileDialog.extensions)
                        );
                        const items = JSON.parse(jsonStr);
                        if (items.length === 1) {
                            // Exactly one clip/sequence → main preview area. For a
                            // sequence, load via the first frame so detectImageSequence
                            // collapses it (and resolves its frame rate) exactly like
                            // dropping a single frame.
                            // Still register any _mix.bin gyro sources in the folder
                            // (the multi-clip / queue paths get this via the queue's
                            // loadFiles handler; this single-clip→main path bypasses
                            // it, so call it here to preserve the prior behavior).
                            render_queue.add_gyro_folder(folderUrls[0].toString());
                            const it = items[0];
                            const loadUrl = (it.is_sequence && it.first_frame_url) ? it.first_frame_url : it.url;
                            root.loadFile(loadUrl, false);
                            console.log("[main_drop:dispatch] folder=1 items=1 target=main");
                            return;
                        }
                        if (items.length > 1 && queue.item) {
                            queue.item.shown = true;
                            Qt.callLater(function() { queue.item.dt.loadFiles(folderUrls); });
                            console.log("[main_drop:dispatch] folder=1 items=" + items.length + " target=queue");
                            return;
                        }
                        console.log("[main_drop:drop] reason=empty_folder");
                        return;
                    } catch (e) {
                        console.log("list_video_files_in_folder failed:", e);
                        return;
                    }
                }
                if (folderUrls.length > 0 && queue.item) {
                    // Multiple folders (or a folder dropped alongside loose files)
                    // go to the render queue, which expands each folder via the same
                    // fixed path as a direct queue drop (image-sequence collapse +
                    // fps resolution + per-job image_sequence injection).
                    queue.item.shown = true;
                    const items = folderUrls.concat(fileUrls);
                    Qt.callLater(function() { queue.item.dt.loadFiles(items); });
                    console.log("[main_drop:dispatch] folders=" + folderUrls.length + " files=" + fileUrls.length + " target=queue");
                } else if (fileUrls.length > 0 && hasGyroFile && queue.item) {
                    // A device gyro file came in with the videos, so the whole import
                    // belongs to the render queue. Without this, a single video would fall
                    // into loadMultipleFiles' one-file main-preview branch and the user
                    // would end up staring at an open but empty queue.
                    const gyroDropUrls = [...fileUrls];
                    Qt.callLater(function() { queue.item.dt.loadFiles(gyroDropUrls); });
                    console.log("[main_drop:dispatch] files=" + fileUrls.length + " target=queue_with_gyro");
                } else if (fileUrls.length > 0) {
                    root.loadMultipleFiles(fileUrls, false);
                    console.log("[main_drop:dispatch] files=" + fileUrls.length + " target=" + (fileUrls.length > 1 ? "queue" : "main"));
                } else {
                    console.log("[main_drop:drop] reason=no_video_urls filtered=" + filteredUrls.length);
                }
            }
        }
    }

    Column {
        id: tlcol;
        width: parent.width;
        anchors.horizontalCenter: parent.horizontalCenter;
        anchors.bottom: parent.bottom;
        anchors.bottomMargin: areButtonsUp? 0 : 5 * dpiScale;
        spacing: root.fullScreen || window.isMobileLayout? 0 : 10 * dpiScale;
        property bool areButtonsUp: !window.isMobileLayout;
        onAreButtonsUpChanged: {
            buttonsArea.parent = null;
            bottomPanel.parent = null;
            if (areButtonsUp) {
                buttonsArea.parent = tlcol;
                bottomPanel.parent = tlcol;
            } else {
                bottomPanel.parent = tlcol;
                buttonsArea.parent = tlcol;
            }
        }
        Component.onCompleted: areButtonsUpChanged();

        Item {
            id: buttonsArea;
            width: parent? parent.width : 0;
            height: 40 * dpiScale;
            visible: !root.fullScreen;

            Rectangle {
                visible: window.isMobileLayout || !middleButtons.willFit;
                color: styleBackground;
                opacity: 0.8;
                radius: 5 * dpiScale;
                anchors.fill: textCol;
                anchors.margins: -4 * dpiScale;
            }
            Column {
                id: textCol;
                enabled: vid.loaded;
                y: middleButtons.willFit? ((parent.height - height) / 2) : -buttonsArea.y - tlcol.y + 7 * dpiScale + ((main_window.safeAreaMargins.top || 0) * 0.8);
                anchors.left: parent.left;
                anchors.leftMargin: 10 * dpiScale;
                spacing: 3 * dpiScale;
                property real widthPadded: Math.ceil(width / (20 * dpiScale)) * (20 * dpiScale);
                Row {
                    BasicText {
                        text: timeline.timeAtPosition((vid.currentFrame + 1) / Math.max(1, vid.frameCount));
                        leftPadding: 0;
                        font.pixelSize: 14 * dpiScale;
                        anchors.verticalCenter: parent.verticalCenter;
                    }
                    BasicText {
                        text: `(${vid.currentFrame+1}/${vid.frameCount})`;
                        leftPadding: 5 * dpiScale;
                        font.pixelSize: 11 * dpiScale;
                        anchors.verticalCenter: parent.verticalCenter;
                    }
                }
                Row {
                    visible: window.stab.automaticHorizonLock;
                    BasicText {
                        text: qsTr("Turn Speed (Roll):");
                        leftPadding: 0;
                        font.pixelSize: 11 * dpiScale;
                        anchors.verticalCenter: parent.verticalCenter;
                    }
                    BasicText {
                        id: turnSpeedValue;
                        text: "---";
                        leftPadding: 5 * dpiScale;
                        font.pixelSize: 11 * dpiScale;
                        anchors.verticalCenter: parent.verticalCenter;
                    }
                }
                BasicText {
                    id: currentFovText;
                    font.pixelSize: 11 * dpiScale;
                    leftPadding: 0;
                }
            }

            Item {
                id: middleButtons;
                property real availableWidth: parent.width - textCol.widthPadded - rightButtons.width - 40 * dpiScale;
                width: parent.width - (willFit? textCol.widthPadded + rightButtons.width + 40 * dpiScale : 0);
                height: parent.height;
                x: willFit? textCol.x + textCol.widthPadded + 10 * dpiScale : 0;
                property bool willFit: availableWidth > children[0].width;
                Row {
                    anchors.centerIn: parent;
                    spacing: 5 * dpiScale;
                    enabled: vid.loaded;
                    Button { text: "["; font.bold: true; onClicked: timeline.setTrimStart(timeline.closestTrimRange(timeline.position, true), timeline.position); tooltip: qsTr("Trim start"); transparentOnMobile: true; }
                    Button {
                        iconName: "chevron-left";
                        tooltip: qsTr("Previous frame");
                        transparentOnMobile: true;
                        MouseArea {
                            anchors.fill: parent;
                            onClicked: mouse => {
                                if (mouse.modifiers & Qt.ShiftModifier) {
                                    timeline.jumpToPrevKeyframe("");
                                } else if (mouse.modifiers & Qt.ControlModifier) {
                                    vid.seekToFrameDelta(-10);
                                } else {
                                    vid.seekToFrameDelta(-1);
                                }
                            }
                        }
                    }
                    Button {
                        onClicked: { if (vid.playing) vid.pause(); else vid.play(); }
                        tooltip: vid.playing? qsTr("Pause") : qsTr("Play");
                        iconName: vid.playing? "pause" : "play";
                        transparentOnMobile: true;
                    }
                    Button {
                        iconName: "chevron-right";
                        tooltip: qsTr("Next frame");
                        transparentOnMobile: true;
                        MouseArea {
                            anchors.fill: parent;
                            onClicked: mouse => {
                                if (mouse.modifiers & Qt.ShiftModifier) {
                                    timeline.jumpToNextKeyframe("");
                                } else if (mouse.modifiers & Qt.ControlModifier) {
                                    vid.seekToFrameDelta(10);
                                } else {
                                    vid.seekToFrameDelta(1);
                                }
                            }
                        }
                    }
                    Button { text: "]"; font.bold: true; onClicked: timeline.setTrimEnd(timeline.closestTrimRange(timeline.position, false), timeline.position); tooltip: qsTr("Trim end"); transparentOnMobile: true; }
                    Button { visible: isMobile; iconName: "menu"; onClicked: timeline.toggleContextMenu(this); tooltip: qsTr("Show timeline menu"); transparentOnMobile: true; leftPadding: 10 * dpiScale; rightPadding: 10 * dpiScale; }
                }
            }
            Rectangle {
                visible: window.isMobileLayout || !middleButtons.willFit;
                color: styleBackground;
                opacity: 0.8;
                radius: 5 * dpiScale;
                anchors.fill: rightButtons;
                anchors.margins: -4 * dpiScale;
            }
            Row {
                id: rightButtons;
                enabled: vid.loaded;
                spacing: 5 * dpiScale;
                y: middleButtons.willFit? ((parent.height - height) / 2) : -buttonsArea.y - tlcol.y + ((main_window.safeAreaMargins.top || 0) * 0.8);
                onYChanged: root.additionalTopMargin = middleButtons.willFit? 0 : Math.max(height, textCol.height) + 2*4 * dpiScale + ((main_window.safeAreaMargins.top || 0) * 0.8);
                anchors.right: parent.right;
                anchors.rightMargin: 10 * dpiScale;
                height: parent.height;

                component SmallLinkButton: LinkButton {
                    height: Math.round(parent.height);
                    anchors.verticalCenter: parent.verticalCenter;
                    textColor: !checked? styleTextColor : styleAccentColor;
                    onClicked: checked = !checked;
                    opacity: checked? 1 : 0.5;
                    checked: true;
                    leftPadding: 6 * dpiScale;
                    rightPadding: 6 * dpiScale;
                    topPadding: 8 * dpiScale;
                    bottomPadding: 8 * dpiScale;
                }

                // Single 3-state preview toggle button. It drives two hidden
                // state-holders (stabEnabledBtn / fovOverviewBtn) that keep their
                // original `checked` semantics so external references (public
                // aliases, Shortcuts.qml "s"/"d" keys, and other read sites in
                // this file) and their controller-apply handlers keep working.
                // previewState: 0 = original, 1 = stabilized, 2 = overview.
                // Raw LinkButton (NOT the SmallLinkButton inline component): SmallLinkButton
                // binds its colour to its own `checked` AND runs `onClicked: checked = !checked`,
                // which fights an instance override and desyncs the icon colour from the real
                // state. A raw LinkButton has none of those bindings, so `textColor` applies
                // directly and the colour always matches previewState.
                LinkButton {
                    id: stabPreviewBtn;
                    height: Math.round(parent.height);
                    anchors.verticalCenter: parent.verticalCenter;
                    leftPadding: 6 * dpiScale;
                    rightPadding: 6 * dpiScale;
                    topPadding: 8 * dpiScale;
                    bottomPadding: 8 * dpiScale;
                    // Derive state from the holders so it stays in sync even when toggled
                    // externally (e.g. keyboard "s"/"d"). 0=original 1=stabilized 2=overview.
                    readonly property int previewState: !stabEnabledBtn.checked ? 0 : (fovOverviewBtn.checked ? 2 : 1);
                    iconName: previewState === 2 ? "fov-overview" : "gyroflow";
                    // White gyroflow (original) -> blue gyroflow (stabilized) -> blue fov-overview (overview).
                    textColor: previewState === 0 ? styleTextColor : styleAccentColor;
                    tooltip: previewState === 0 ? qsTr("Preview: original")
                           : previewState === 2 ? qsTr("Preview: overview")
                           : qsTr("Preview: stabilized");
                    // Cycle order: stabilized (1) -> original (0) -> overview (2) -> stabilized (1).
                    onClicked: {
                        var next = previewState === 1 ? 0 : (previewState === 0 ? 2 : 1);
                        stabEnabledBtn.checked = (next !== 0);
                        fovOverviewBtn.checked = (next === 2);
                        // Overview is a single zoomed-out view, never the old Ctrl+ split pane.
                        secondPreview.show = false;
                    }
                }

                // Hidden state-holder: stabilization on/off. Keeps its id, public
                // alias and controller-apply handler. Driven by stabPreviewBtn and
                // by external writers (Shortcuts.qml, video load handlers).
                SmallLinkButton {
                    id: stabEnabledBtn;
                    iconName: "gyroflow";
                    visible: false;
                    width: 0;
                    onCheckedChanged: { controller.stab_enabled = checked; vid.forceRedraw(); vid.fovChanged(); }
                    tooltip: qsTr("Toggle stabilization");
                }

                // Hidden state-holder: stabilization overview on/off. Same rationale
                // as stabEnabledBtn. The former Ctrl+click second-preview gesture
                // has been removed per the redesign.
                SmallLinkButton {
                    id: fovOverviewBtn;
                    iconName: "fov-overview";
                    checked: false;
                    visible: false;
                    width: 0;
                    onCheckedChanged: { controller.fov_overview = checked; vid.forceRedraw(); }
                    tooltip: qsTr("Toggle stabilization overview");
                }

                SmallLinkButton {
                    id: muteBtn;
                    iconName: checked? "sound" : "sound-mute";
                    tooltip: checked? qsTr("Mute") : qsTr("Unmute");
                    checked: !vid.muted;

                    ContextMenuMouseArea {
                        underlyingItem: muteBtn;
                        cursorShape: Qt.PointingHandCursor;
                        onContextMenu: (isHold, x, y) => { volumePopup.open(); if (isHold) vid.muted = !vid.muted; }
                    }
                    onClicked: () => { vid.muted = !vid.muted; }
                    Popup {
                        id: volumePopup;
                        width: volumeLabel.width + 25 * dpiScale;
                        height: 30 * dpiScale;
                        x: -width + muteBtn.width;
                        y: -height;
                        Label {
                            id: volumeLabel;
                            anchors.centerIn: parent;
                            text: qsTr("Volume");
                            position: Label.LeftPosition;
                            width: t.width + volumeSlider.width;
                            Slider {
                                id: volumeSlider;
                                width: 200 * dpiScale;
                                unit: "%";
                                from: 0;
                                to: 100;
                                value: settings.value("volume", 100);
                                precision: 0;
                                onValueChanged: { vid.volume = value / 100.0; settings.setValue("volume", value); }
                            }
                        }
                    }
                }

                ComboBox {
                    id: playbackRateCb;
                    model: ["0.13x", "0.25x", "0.5x", "1x", "2x", "4x", "5x", "8x", "10x", "20x", "50x"];
                    width: 60 * dpiScale;
                    currentIndex: 3;
                    height: 25 * dpiScale;
                    itemHeight: 25 * dpiScale;
                    font.pixelSize: 11 * dpiScale;
                    anchors.verticalCenter: parent.verticalCenter;
                    onCurrentTextChanged: {
                        const rate = +currentText.replace("x", ""); // hacky but simple and it works
                        vid.playbackRate = rate;
                    }
                    tooltip: qsTr("Playback speed");
                }
            }
        }

        ResizablePanel {
            id: bottomPanel;
            direction: ResizablePanel.HandleUp;
            width: parent? parent.width : 0;
            color: "transparent";
            hr.height: 30 * dpiScale;
            hr.enabled: !(queue.item && queue.item.shown);
            hr.opacity: root.fullScreen || window.isMobileLayout? 0.1 : 1.0;
            additionalHeight: timeline.additionalHeight;
            defaultHeight: (window.isMobileLayout? 50 : 165) * dpiScale;
            minHeight: (root.fullScreen || window.isMobileLayout? 50 : 100) * dpiScale;
            lastHeight: settings.value("bottomPanelSize" + (root.fullScreen? "-full" : ""), defaultHeight);
            onHeightAdjusted: settings.setValue("bottomPanelSize" + (root.fullScreen? "-full" : ""), height);
            Connections {
                target: root;
                function onFullScreenChanged(): void {
                    bottomPanel.lastHeight = settings.value("bottomPanelSize" + (root.fullScreen? "-full" : ""), bottomPanel.defaultHeight);
                    if (root.fullScreen == 2) {
                        main_window.visibility = Window.FullScreen;
                    } else {
                        if (main_window.visibility == Window.FullScreen) main_window.visibility = Window.Windowed;
                    }
                }
            }
            visible: root.fullScreen != 2;
            maxHeight: root.height - 50 * dpiScale;
            Timeline {
                id: timeline;
                durationMs: vid.duration;
                scaledFps: vid.frameRate;
                anchors.fill: parent;
                fullScreen: root.fullScreen;
                visible: vid.loaded || !window.isMobileLayout;
                property bool prevRestrictTrim: false;
                Component.onCompleted: prevRestrictTrim = restrictTrim;

                onTrimRangesChanged: {
                    controller.set_trim_ranges(timeline.trimRanges.map(x => x[0] + ":" + x[1]).join(";"));
                    restrictTrimChanged();
                }
                onRestrictTrimChanged: {
                    if (restrictTrim) {
                        const ranges = timeline.getTrimRanges();
                        vid.setPlaybackRange(ranges[0][0] * vid.duration, ranges[ranges.length - 1][1] * vid.duration);
                    } else if (prevRestrictTrim != restrictTrim) {
                        vid.setPlaybackRange(0, -1);
                    }
                    prevRestrictTrim = restrictTrim;
                }
            }
        }
    }
    Item {
        width: vidParentParent.width;
        height: vidParentParent.height;
        LoaderOverlay {
            id: videoLoader;
            background: styleBackground;
            verticalOffset: window.isMobileLayout? -bottomPanel.height / 2 : 0;
            onActiveChanged: { vid.forceRedraw(); vid.fovChanged(); }
            canHide: render_queue.main_job_id > 0;
            onCancel: {
                if (render_queue.main_job_id > 0) {
                    render_queue.cancel_job(render_queue.main_job_id);
                } else {
                    controller.cancel_current_operation();
                }
            }
            onHide: {
                render_queue.main_job_id = 0;
                videoLoader.active = false;
            }
        }
        Column {
            id: infoMessages;
            width: parent.width;
            spacing: 5 * dpiScale;
            visible: children.length > 0;
            y: root.additionalTopMargin;
            InfoMessage {
                type: InfoMessage.Warning;
                visible: vid.loaded && !controller.lens_loaded && !isCalibrator;
                text: qsTr("Lens profile is not loaded, the results will not look correct. Please load a lens profile for your camera.");
            }
        }
    }
    Loader {
        id: queue;
        asynchronous: true;
        anchors.fill: root;
        anchors.margins: 10 * dpiScale;
        sourceComponent: Component {
            RenderQueue {
                onShownChanged: if (statistics.item) statistics.item.shown &= !shown;
            }
        }
    }
    Loader {
        id: statistics;
        asynchronous: true;
        active: false;
        anchors.fill: vidParentParent;
        anchors.margins: 10 * dpiScale;
        onStatusChanged: if (status == Loader.Ready) statistics.item.shown = true;
        sourceComponent: Component {
            Statistics {
                onShownChanged: queue.item.shown &= !shown;
            }
        }
    }
}
