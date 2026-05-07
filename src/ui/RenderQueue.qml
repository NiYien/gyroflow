// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Controls as QQC
import QtQuick.Dialogs as QQD

import "components/"
import "Util.js" as Util;

Item {
    id: root;

    property alias dt: dt;
    property alias isDragging: lv.isDragging;
    property bool shown: false;
    readonly property bool lightTheme: style === "light"

    // Session-scoped choice applied to all remaining convert_format errors in the current batch
    // (simple-mode only). Cleared on queue_finished / clear / stop so the next batch re-asks.
    property string pendingConvertFormatChoice: ""

    function ensureExternalSdkForQueue(urls, continuation) {
        const sdkUrl = render_queue.first_file_requiring_external_sdk(JSON.stringify(urls.map(u => u.toString())));
        if (!sdkUrl) return true;

        window.videoArea.promptExternalSdkInstall(sdkUrl, function(_) {
            Qt.callLater(continuation);
        });
        return false;
    }

    Connections {
        target: render_queue;
        function onQueue_finished(): void { root.pendingConvertFormatChoice = ""; }
    }

    // --- Batch gyro match state ---
    property var gyroFilesInfo: []
    readonly property var darkGyroColors:  ["#76baed", "#70e574", "#f6a00b", "#e87de8", "#ed7676", "#5ce0d8"]
    readonly property var lightGyroColors: ["#2f78b6", "#2d8c4d", "#ad6a00", "#9b55b7", "#c55d5d", "#1f8f9f"]
    readonly property var gyroColors: lightTheme ? lightGyroColors : darkGyroColors
    readonly property color gyroTimeTextColor: lightTheme ? "#17324d" : "#ffffff"
    readonly property color matchedStatusColor: lightTheme ? "#256c3f" : "#70e574"
    readonly property color manualStatusColor: lightTheme ? "#9a5f00" : "#f0c040"
    readonly property color calibrationStatusColor: lightTheme ? "#1f6fa8" : "#76baed"
    readonly property color skippedStatusColor: lightTheme ? "#5b6470" : "#888888"
    readonly property color finishedStatusColor: lightTheme ? "#2a8a4c" : "#70e574"
    readonly property color errorStatusColor: lightTheme ? "#d16b6b" : "#ed7676"
    readonly property color pendingSyncStatusColor: lightTheme ? "#5b6470" : "#9aa3ad"
    readonly property color queueOutlineColor: lightTheme ? "#1f111111" : "#70ffffff"
    readonly property real matchedGyroOpacity: lightTheme ? 0.28 : 0.30
    readonly property real unmatchedGyroOpacity: lightTheme ? 0.18 : 0.15
    property bool hasGyroFiles: render_queue.has_gyro_files()
    property int pairingGyroIndex: -1
    property string pairingGyroFilename: ""
    property string matchWarning: ""
    property bool _queueLayoutPending: false
    // [T14] Global matchExecuted flag: whether matching has run.
    property bool matchExecuted: false
    // [T19] Match version counter. Incrementing it forces delegate bindings to re-evaluate.
    property int matchVersion: 0
    property int syncStatusVersion: 0
    property string lastBatchSyncPromptKind: "none"
    // ── Batch selection ──
    // CheckBox column is always visible on every row. Tap a checkbox (mouse or touch)
    // to toggle selection; drag across checkboxes with mouse button held to add a range.
    // Touch drag is intentionally NOT hooked into drag-select — it scrolls the list instead.
    property var selectedJobs: ({})
    property int selectedCount: Object.keys(selectedJobs).length
    property int primarySelectedJobId: 0
    property int _lastClickedIndex: -1
    property bool _dragSelecting: false
    property int _dragSelectStartIndex: -1
    property bool _dragSelectAddMode: true
    property var _dragSelectSnapshot: ({})
    property real _dragSelectViewY: -1
    // Touch: long-press in the checkbox column arms a selection-drag mode that the
    // touch DragHandler then takes over. ListView.interactive is suspended while
    // active so the Flickable does not steal the gesture.
    property bool _touchSelectActive: false

    function jobIdAtModelIndex(modelIndex) {
        return render_queue.get_job_id_at_model_index(modelIndex);
    }

    function selectedJobsWithRange(baseSelection, fromIndex, toIndex, addMode) {
        let s = Object.assign({}, baseSelection);
        const from = Math.min(fromIndex, toIndex);
        const to = Math.max(fromIndex, toIndex);
        for (let i = from; i <= to; i++) {
            const jobId = jobIdAtModelIndex(i);
            if (!jobId) continue;
            if (addMode) s[jobId] = true;
            else delete s[jobId];
        }
        return s;
    }

    function setSelectedJobs(newSelectedJobs, primaryJobId) {
        selectedJobs = newSelectedJobs;
        if (primaryJobId !== undefined && newSelectedJobs[primaryJobId]) {
            primarySelectedJobId = primaryJobId;
            return;
        }
        if (primarySelectedJobId && newSelectedJobs[primarySelectedJobId]) {
            return;
        }
        primarySelectedJobId = 0;
        for (let i = 0; i < lv.count; i++) {
            const jobId = jobIdAtModelIndex(i);
            if (jobId && newSelectedJobs[jobId]) {
                primarySelectedJobId = jobId;
                return;
            }
        }
    }

    function getPrimarySelectedJobId() {
        if (primarySelectedJobId && selectedJobs[primarySelectedJobId]) {
            return primarySelectedJobId;
        }
        for (let i = 0; i < lv.count; i++) {
            const jobId = jobIdAtModelIndex(i);
            if (jobId && selectedJobs[jobId]) return jobId;
        }
        return 0;
    }

    function toggleJobSelection(jobId) {
        let s = Object.assign({}, selectedJobs);
        if (s[jobId]) delete s[jobId];
        else s[jobId] = true;
        setSelectedJobs(s, s[jobId] ? jobId : undefined);
    }
    function handleSelectionClick(jobId, modelIndex, modifiers) {
        // Excel-style anchor: Shift+click extends from the last plain click without moving
        // the anchor; only a plain click resets the anchor for subsequent ranges.
        if ((modifiers & Qt.ShiftModifier) && _lastClickedIndex >= 0) {
            setSelectedJobs(selectedJobsWithRange(selectedJobs, _lastClickedIndex, modelIndex, true), jobId);
            return;
        }
        toggleJobSelection(jobId);
        _lastClickedIndex = modelIndex;
    }
    function selectAllJobs() {
        let s = {};
        let firstJobId = 0;
        for (let i = 0; i < lv.count; i++) {
            const jobId = jobIdAtModelIndex(i);
            if (jobId) {
                s[jobId] = true;
                if (!firstJobId) firstJobId = jobId;
            }
        }
        setSelectedJobs(s, firstJobId);
    }
    function deselectAllJobs() { setSelectedJobs({}); }
    function beginDragSelection(startIndex, addMode) {
        _dragSelecting = true;
        _dragSelectStartIndex = startIndex;
        _dragSelectAddMode = addMode;
        _dragSelectSnapshot = Object.assign({}, selectedJobs);
        _dragSelectViewY = -1;
        _lastClickedIndex = startIndex;
    }
    function updateDragSelectionAtContentY(contentY) {
        if (!_dragSelecting || _dragSelectStartIndex < 0) return;
        const idx = lv.indexAt(1, contentY);
        if (idx < 0) return;
        const s = idx === _dragSelectStartIndex
            ? Object.assign({}, _dragSelectSnapshot)
            : selectedJobsWithRange(_dragSelectSnapshot, _dragSelectStartIndex, idx, _dragSelectAddMode);
        setSelectedJobs(s, jobIdAtModelIndex(_dragSelectStartIndex));
    }
    function updateDragSelectionFromViewY(viewY) {
        if (!_dragSelecting || _dragSelectStartIndex < 0 || viewY < 0) return;
        _dragSelectViewY = viewY;
        updateDragSelectionAtContentY(lv.contentY + viewY);
    }
    function endDragSelection() {
        _dragSelecting = false;
        _dragSelectStartIndex = -1;
        _dragSelectSnapshot = ({});
        _dragSelectViewY = -1;
    }

    // [queue-gyro-column] 左列宽度，有陀螺仪时展开
    property real gyroColumnWidth: hasGyroFiles ? 65 * dpiScale : 0
    Ease on gyroColumnWidth { }

    // [queue-gyro-column] 格式化陀螺仪文件创建时间
    function formatGyroTime(gyroIndex) {
        if (gyroIndex < 0 || gyroIndex >= gyroFilesInfo.length) return "";
        var ms = gyroFilesInfo[gyroIndex].created_at_ms;
        if (ms === null || ms === undefined) return "??:??:??";
        var d = new Date(ms);
        return Qt.formatTime(d, "HH:mm:ss");
    }
    function withAlpha(color, alpha) {
        return Qt.rgba(color.r, color.g, color.b, alpha);
    }
    property bool allGyroParsed: {
        if (gyroFilesInfo.length === 0) return false;
        for (let i = 0; i < gyroFilesInfo.length; i++) {
            if (!gyroFilesInfo[i].parsed) return false;
        }
        return true;
    }
    function requestQueueLayout(): void {
        if (_queueLayoutPending) {
            return;
        }
        _queueLayoutPending = true;
        Qt.callLater(function() {
            _queueLayoutPending = false;
            if (lv && lv.forceLayout) {
                lv.forceLayout();
            }
        });
    }
    onWidthChanged: requestQueueLayout();
    onShownChanged: requestQueueLayout();

    Connections {
        target: render_queue;
        function onGyro_files_changed(): void {
            let infos = [];
            for (let i = 0; i < render_queue.get_gyro_file_count(); i++) {
                infos.push(JSON.parse(render_queue.get_gyro_file_info_json(i)));
            }
            root.gyroFilesInfo = infos;
            root.hasGyroFiles = render_queue.has_gyro_files();
            // [T14] Reset matchExecuted when gyro files are cleared.
            if (!root.hasGyroFiles) root.matchExecuted = false;
            // Gyro file changes don't reorder the video queue — only video
            // batch loads (sort_jobs_by_filename) and match (sort_jobs_by_created_at) do.
            root.requestQueueLayout();
        }
        function onMatch_results_changed(): void {
            // [T14] Update the global matchExecuted flag.
            root.matchExecuted = render_queue.has_match_results();
            // [T19] Increment the version counter to refresh delegate bindings without rebuilding delegates.
            root.matchVersion++;
            // [T22] Only reset the matching state here; match_apply_finished closes the overlay.
            autoMatchBtn.matching = false;
            root.requestQueueLayout();
            // Check unmatched items.
            Qt.callLater(function() {
                let unmatchedCount = 0;
                for (let i = 0; i < lv.count; i++) {
                    const queueItem = render_queue.queue[i];
                    if (!queueItem || queueItem.job_id === undefined) {
                        continue;
                    }
                    let status = JSON.parse(render_queue.get_match_status_json(queueItem.job_id));
                    if (status.status === "Unmatched" || status.status === "NoCreationTime") {
                        unmatchedCount++;
                    }
                }
                if (unmatchedCount > 0) {
                    root.matchWarning = qsTr("No calibration pair found for %1 video(s). Please pair manually.").arg(unmatchedCount);
                }
            });
        }
        // [T22] Close the overlay after matching and data loading have both finished.
        function onMatch_apply_finished(): void {
            loader.active = false;
            root.matchVersion++;
            root.requestQueueLayout();
        }
        function onBatch_sync_status_changed(): void {
            root.syncStatusVersion++;
            root.requestQueueLayout();
            const kind = render_queue.get_batch_sync_prompt_kind();
            if (kind === "none") {
                root.lastBatchSyncPromptKind = "none";
                return;
            }
            if (kind === root.lastBatchSyncPromptKind) {
                return;
            }
            root.lastBatchSyncPromptKind = kind;
            if (kind === "repair") {
                messageBox(Modal.Question, qsTr("Some videos could not be reliably synchronized. Try to repair them automatically?"), [
                    { text: qsTr("Repair"), accent: true, clicked: () => render_queue.confirm_batch_sync_repair() },
                    { text: qsTr("Skip"), clicked: () => render_queue.skip_batch_sync_repair() }
                ]);
            } else if (kind === "all_yellow") {
                messageBox(Modal.Warning, qsTr("Batch synchronization did not produce a reliable result. Check gyro splitting or batch matching and try again."), [
                    { text: qsTr("Ok") }
                ]);
            } else if (kind === "finished_with_yellow") {
                messageBox(Modal.Warning, qsTr("Some videos are still not reliably synchronized after repair."), [
                    { text: qsTr("Ok") }
                ]);
            }
        }
        function onProcessing_done(job_id, by_preset): void {
            root.matchVersion++;
        }
        function onPairing_mode_changed(): void {
            if (!render_queue.is_in_pairing_mode()) {
                root.pairingGyroIndex = -1;
                root.pairingGyroFilename = "";
            }
        }
    }
    Connections {
        target: window
        function onIsSimpleModeChanged(): void {
            root.requestQueueLayout();
        }
    }
    opacity: shown? 1 : 0;
    visible: opacity > 0;
    anchors.bottomMargin: (shown? 10 : 30) * dpiScale;
    anchors.topMargin: (shown? 10 : -20) * dpiScale;
    Ease on opacity { }
    Ease on anchors.bottomMargin { }
    Ease on anchors.topMargin { }

    // Consume pointer events over the render-queue panel so clicks, right-clicks, hover and
    // wheel gestures do not leak to the video preview / timeline underneath.
    MouseArea {
        anchors.fill: parent;
        preventStealing: true;
        acceptedButtons: Qt.AllButtons;
        hoverEnabled: true;
        onWheel: (wheel) => { wheel.accepted = true; }
        onPressed: (mouse) => { mouse.accepted = true; }
    }

    Rectangle {
        color: styleBackground2
        opacity: 0.85;
        anchors.fill: parent;
        radius: 5 * dpiScale;
        border.width: 1;
        border.color: styleVideoBorderColor;
    }

    BasicText {
        id: titleText;
        y: 12 * dpiScale;
        x: 5 * dpiScale;
        text: qsTr("Render queue");
        font.pixelSize: 15 * dpiScale;
        font.bold: true;
    }

    LinkButton {
        id: closeBtn;
        anchors.right: parent.right;
        width: 34 * dpiScale;
        height: 34 * dpiScale;
        textColor: styleTextColor;
        iconName: "close";
        leftPadding: 0;
        rightPadding: 0;
        topPadding: 10 * dpiScale;
        onClicked: root.shown = false;
    }

    Hr { width: parent.width - 10 * dpiScale; y: 35 * dpiScale; color: "#fff"; opacity: 0.3; }

    FileDialog {
        id: mobileAddFilesDialog;
        title: qsTr("Choose files")
        nameFilters: Qt.platform.os == "android"? undefined : [qsTr("Video files") + " (*." + fileDialog.extensions.concat(fileDialog.extensions.map(x => x.toUpperCase())).join(" *.") + ")"];
        type: "video";
        fileMode: FileDialog.OpenFiles;
        onAccepted: {
            // On Android the JNI bridge (urls_opened -> pendingPickerCallback)
            // delivers the picked URIs because Qt 6.7.3's SAF parser fails on
            // MIUI; suppress this branch to avoid duplicate dispatch on
            // non-MIUI Android (where both paths fire).
            if (Qt.platform.os === "android") return;
            dt.loadFiles(selectedFiles);
        }
        onRejected: { if (Qt.platform.os === "android") window.pendingPickerCallback = null; }
    }

    QQD.FolderDialog {
        id: mobileAddFolderDialog;
        title: qsTr("Choose folder")
        onAccepted: {
            if (Qt.platform.os === "android") return; // urls_opened handles it
            filesystem.folder_access_granted(selectedFolder);
            Qt.callLater(filesystem.save_allowed_folders);
            dt.loadFiles([selectedFolder]);
        }
        onRejected: { if (Qt.platform.os === "android") window.pendingPickerCallback = null; }
    }

    Row {
        id: progressRow;
        y: 55 * dpiScale;
        spacing: 10 * dpiScale;
        x: 10 * dpiScale;
        Column {
            id: topCol;
            spacing: 5 * dpiScale;
            width: parent.parent.width - x - mainBtn.width - 3 * parent.spacing;
            property bool queueProgressUsesJobs: render_queue.queue_progress_uses_jobs;
            property real progress: render_queue.queue_progress;
            property real estimatedRemainingMs: render_queue.estimated_remaining_ms;
            property string queueStatus: render_queue.status;
            property real remainingBaseMs: -1;
            property double remainingBaseTimestamp: 0;
            onProgressChanged: updateTimes();
            onQueueProgressUsesJobsChanged: updateTimes();
            onEstimatedRemainingMsChanged: refreshRemainingEstimate();
            onQueueStatusChanged: refreshRemainingEstimate();
            Component.onCompleted: {
                refreshRemainingEstimate();
            }
            function formatRemainingMs(ms: real): string {
                return ms <= 0? qsTr("0s") : Util.timeToStr(ms / 1000.0);
            }
            function refreshRemainingEstimate(): void {
                const estimate = render_queue.estimated_remaining_ms;
                if (queueStatus == "active" && estimate >= 0) {
                    remainingBaseMs = estimate;
                    remainingBaseTimestamp = Date.now();
                } else if (queueStatus != "active") {
                    remainingBaseMs = -1;
                    remainingBaseTimestamp = 0;
                }
                updateTimes();
            }
            function currentRemainingMs(): real {
                if (queueStatus != "active" || remainingBaseMs < 0) {
                    return -1;
                }
                return Math.max(1000, remainingBaseMs - (Date.now() - remainingBaseTimestamp));
            }
            function updateTimes(): void {
                const queueActive = queueStatus == "active";
                const progressFrame = queueProgressUsesJobs ? 0 : render_queue.current_frame;
                const endTimestamp = queueProgressUsesJobs ? Date.now() : render_queue.end_timestamp;
                const times = Util.calculateTimesAndFps(progress, progressFrame, render_queue.start_timestamp, endTimestamp);
                if (queueActive && progress >= 0.0 && progress < 1.0) {
                    if (times !== false) {
                        totalTime.elapsed = times[0];
                        if (!queueProgressUsesJobs && times.length > 2) {
                            totalTime.fps = times[2];
                        } else if (queueProgressUsesJobs) {
                            totalTime.fps = 0;
                        }
                    } else {
                        totalTime.elapsed = "---";
                        if (queueProgressUsesJobs) totalTime.fps = 0;
                    }
                    window.reportProgress(progress, "queue");
                } else {
                    window.reportProgress(-1, "queue");
                    totalTime.elapsed = "---";
                    if (queueProgressUsesJobs) totalTime.fps = 0;
                }
                const remainingMs = currentRemainingMs();
                totalTime.remaining = remainingMs >= 0
                    ? formatRemainingMs(remainingMs)
                    : (times !== false && times.length > 1 && progress < 1.0 ? times[1] : (progress >= 1.0? formatRemainingMs(0) : "---"));
            }
            Timer {
                interval: 1000;
                repeat: true;
                running: topCol.queueStatus == "active" && topCol.remainingBaseMs >= 0;
                onTriggered: topCol.updateTimes();
            }

            Item {
                width: parent.width;
                height: (twoLines? 35 : 20) * dpiScale;
                id: totalTime;
                property string elapsed: "---";
                property string remaining: "---";
                property real fps: 0;
                property string fpsText: topCol.queueProgressUsesJobs ? "" : (topCol.progress > 0? qsTr(" @ %1fps").arg(fps.toFixed(1)) : "");
                onWidthChanged: Qt.callLater(totalTime.updateLayout);
                property bool twoLines: false;
                function updateLayout(): void {
                    const totalTextSize = progressText1.width + progressText2.width + progressText3.width + 25 * dpiScale;
                    twoLines = totalTextSize > totalTime.width;
                }

                BasicText {
                    id: progressText1;
                    leftPadding: 0;
                    text: qsTr("Elapsed: %1").arg("<b>" + totalTime.elapsed + "</b>");
                    onWidthChanged: Qt.callLater(totalTime.updateLayout);
                }
                BasicText {
                    id: progressText2;
                    leftPadding: 0;
                    anchors.horizontalCenter: parent.horizontalCenter;
                    textFormat: Text.RichText;
                    text: topCol.queueProgressUsesJobs
                        ? `<b>${(topCol.progress*100).toFixed(2)}%</b> <small>(${render_queue.queue_done_jobs}/${render_queue.queue_total_jobs})</small>`
                        : `<b>${(topCol.progress*100).toFixed(2)}%</b> <small>(${render_queue.current_frame}/${render_queue.total_frames}${totalTime.fpsText})</small>`;
                    y: totalTime.twoLines? progressText1.height + 5 * dpiScale : 0;
                    onWidthChanged: Qt.callLater(totalTime.updateLayout);
                }
                BasicText {
                    id: progressText3;
                    leftPadding: 0;
                    anchors.right: parent.right;
                    text: qsTr("Remaining: %1").arg("<b>" + totalTime.remaining + "</b>");
                    onWidthChanged: Qt.callLater(totalTime.updateLayout);
                }
            }
            QQC.ProgressBar {
                id: pb;
                width: parent.width;
                value: topCol.progress;
            }
        }
        Connections {
            target: render_queue;
            function onAdded(job_id: real): void {
                delete loader.pendingJobs[job_id];
                loader.updateStatus();
                // Sort the queue by filename whenever a job actually lands
                // in the model (q.push happens here, not at add_file return).
                // dt.add and r3dSeqLoader can't sort synchronously because
                // add_file is async — the queue is still empty at that point.
                render_queue.sort_jobs_by_filename();
                if (r3dSeqLoader.waiting) {
                    r3dSeqLoader.waiting = false;
                    r3dSeqLoader.loadNext();
                }
            }
            function onError(job_id: real, text: string, arg: string, callback: string): void {
                if (job_id == render_queue.main_job_id || loader.pendingJobs[job_id]) {
                    text = getReadableError(qsTr(text).arg(arg));
                    if (text) {
                        // if (text.includes("failed to decode picture"))
                        //     window.advanced.gpudecode.checked = false;
                        messageBox(Modal.Error, text, [ { "text": qsTr("Ok"), clicked: window[callback] } ]);
                    }
                }
                delete loader.pendingJobs[job_id];
                loader.updateStatus();
                if (r3dSeqLoader.waiting) {
                    r3dSeqLoader.waiting = false;
                    r3dSeqLoader.loadNext();
                }
            }
            function onRender_progress(job_id: real, progress: real, frame: int, total_frames: int, finished: bool, start_time: real, is_conversion: bool): void {
                if (job_id == render_queue.main_job_id) {
                    window.videoArea.videoLoader.active = !finished;
                    window.videoArea.videoLoader.currentFrame = frame;
                    window.videoArea.videoLoader.totalFrames = total_frames;
                    window.videoArea.videoLoader.additional = "";
                    window.videoArea.videoLoader.text = window.videoArea.videoLoader.active? (is_conversion? qsTr("Converting to %1 %2...").arg(window.advanced.r3dConvertFormat.currentText) : qsTr("Rendering %1...")) : "";
                    window.videoArea.videoLoader.progress = window.videoArea.videoLoader.active? progress : -1;
                    window.videoArea.videoLoader.cancelable = true;
                    window.videoArea.videoLoader.startTime = start_time;

                    if (total_frames > 0 && finished) {
                        render_queue.main_job_id = 0;
                        const folder = render_queue.get_job_output_folder(job_id);
                        const filename = render_queue.get_job_output_filename(job_id);
                        let options = [];
                        if (Qt.platform.os != "ios" && !(window.exportSettings.exportTrimsSeparately.checked && window.videoArea.timeline.trimRanges.length > 1)) {
                            options.push({ text: qsTr("Open rendered file"), clicked: () => filesystem.open_file_externally(filesystem.get_file_url(folder, filename, false)) });
                        }
                        if (Qt.platform.os != "android" && Qt.platform.os != "ios") {
                            options.push({ text: qsTr("Open file location"), clicked: () => filesystem.open_file_externally(folder) });
                        }
                        options.push({ text: qsTr("Ok") });

                        messageBox(Modal.Success, qsTr("Rendering completed. The file was written to: %1.").arg("<br><b>" + filesystem.display_folder_filename(folder, filename) + "</b>"), options);
                    }
                }
            }
            function onConvert_format(job_id: real, format: string, supported: string, candidate: string): void {
                if (job_id == render_queue.main_job_id) {
                    let buttons = supported.split(",").map(f => ({
                        text: f,
                        accent: f.toLowerCase() == candidate,
                        clicked: () => {
                            render_queue.set_pixel_format(job_id, f);
                            render_queue.render_job(job_id);
                        }
                    }));
                    buttons.push({
                        text: qsTr("Render using CPU"),
                        accent: candidate == '',
                        clicked: () => {
                            render_queue.set_pixel_format(job_id, "cpu");
                            render_queue.render_job(job_id);
                        }
                    });
                    buttons.push({ text: qsTr("Cancel") });

                    messageBox(Modal.Question, qsTr("GPU accelerated encoder doesn't support this pixel format (%1).\nDo you want to convert to a different supported pixel format or keep the original one and render on the CPU?").arg(format), buttons);
                }
                delete loader.pendingJobs[job_id];
                loader.updateStatus();
            }
            function onEncoder_initialized(job_id: real, encoder_name: string): void {

            }
            function onRequest_close(): void {
                main_window.closeConfirmed = true;
                Qt.callLater(Qt.quit);
            }
        }

        Button {
            id: mainBtn;
            accent: true;
            visible: !window.isSimpleMode;
            property string status: render_queue.status;
            property var statuses: ({
                "stopped": [qsTr("Start exporting"), "play",  styleAccentColor, "start"],
                "paused":  [qsTr("Resume"),          "play",  "#70e574",        "resume"],
                "active":  [qsTr("Pause"),           "pause", "#f6a00b",        "pause"],
            })
            text: statuses[status][0];
            iconName: statuses[status][1];
            accentColor: statuses[status][2];
            icon.width: 15 * dpiScale;
            icon.height: 15 * dpiScale;
            height: 28 * dpiScale;
            leftPadding: 8 * dpiScale;
            rightPadding: 8 * dpiScale;
            topPadding: 3 * dpiScale;
            bottomPadding: 3 * dpiScale;
            font.pixelSize: 12 * dpiScale;
            anchors.verticalCenter: parent.verticalCenter;
            Component.onCompleted: contentItem.children[1].elide = Text.ElideNone;
            clip: true;
            Ease on implicitWidth { }
            Behavior on accentColor { ColorAnimation { duration: 700; easing.type: Easing.OutExpo; } }
            onClicked: render_queue[statuses[status][3]]();
        }
    }

    // T7: Pairing mode banner
    Rectangle {
        id: pairingBanner;
        x: 10 * dpiScale;
        anchors.top: progressRow.bottom;
        anchors.topMargin: 5 * dpiScale;
        width: parent.width - 20 * dpiScale;
        height: 0;
        visible: height > 0;
        clip: true;
        Ease on height { }
        color: styleAccentColor;
        radius: 4 * dpiScale;
        Row {
            anchors.centerIn: parent;
            spacing: 8 * dpiScale;
            BasicText {
                text: qsTr("Pairing: %1 — Click a video to pair").arg("<b>" + root.pairingGyroFilename + "</b>");
                color: styleTextColorOnAccent;
                font.pixelSize: 12 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
            }
            LinkButton {
                text: qsTr("Cancel");
                textColor: styleTextColorOnAccent;
                font.pixelSize: 12 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
                onClicked: {
                    render_queue.exit_pairing_mode();
                    root.pairingGyroIndex = -1;
                    root.pairingGyroFilename = "";
                }
            }
        }
    }

    // T10: Match warning message
    Rectangle {
        id: matchWarningBar;
        x: 10 * dpiScale;
        anchors.top: pairingBanner.bottom;
        anchors.topMargin: root.matchWarning.length > 0 ? 5 * dpiScale : 0;
        width: parent.width - 20 * dpiScale;
        height: root.matchWarning.length > 0 ? 28 * dpiScale : 0;
        visible: height > 0;
        clip: true;
        Ease on height { }
        color: "#40f6a00b";
        border.color: "#f6a00b";
        border.width: 1;
        radius: 4 * dpiScale;
        Row {
            anchors.centerIn: parent;
            spacing: 8 * dpiScale;
            BasicText {
                text: root.matchWarning;
                color: "#f6a00b";
                font.pixelSize: 12 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
            }
            LinkButton {
                text: "✕";
                textColor: "#f6a00b";
                font.pixelSize: 12 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
                onClicked: root.matchWarning = "";
            }
        }
    }

    Rectangle {
        id: mobileAddArea;
        visible: window.isMobileLayout;
        x: 10 * dpiScale;
        anchors.top: matchWarningBar.bottom;
        anchors.topMargin: visible ? 6 * dpiScale : 0;
        width: parent.width - 20 * dpiScale;
        height: visible ? 42 * dpiScale : 0;
        color: "transparent";
        clip: true;

        Row {
            anchors.fill: parent;
            spacing: 8 * dpiScale;

            Button {
                text: qsTr("Add files");
                iconName: "plus";
                width: (parent.width - parent.spacing) / 2;
                height: parent.height;
                font.pixelSize: 13 * dpiScale;
                leftPadding: 10 * dpiScale;
                rightPadding: 10 * dpiScale;
                onClicked: {
                    // Route picked URIs into the render queue batch loader on Android.
                    if (Qt.platform.os === "android") {
                        window.pendingPickerCallback = function(urls) {
                            dt.loadFiles(urls);
                        };
                    }
                    mobileAddFilesDialog.open2();
                }
            }
            Button {
                text: qsTr("Add folder");
                iconName: "folder";
                width: (parent.width - parent.spacing) / 2;
                height: parent.height;
                font.pixelSize: 13 * dpiScale;
                leftPadding: 10 * dpiScale;
                rightPadding: 10 * dpiScale;
                onClicked: {
                    if (Qt.platform.os === "android") {
                        window.pendingPickerCallback = function(urls) {
                            // FolderDialog SAF returns a tree URI (e.g.
                            // content://.../tree/primary%3ADCIM). Do NOT pipe
                            // it through dt.loadFiles: the no-extension
                            // heuristic there is unreliable for tree URIs and
                            // a misclassification feeds the URI to mdk's
                            // video opener, which trips a JNI abort because
                            // ContentResolver rejects tree URIs as file URIs.
                            console.log("[AddFolder] picker returned urls.length=" + (urls ? urls.length : "null"));
                            if (!urls || !urls.length) return;
                            const folderUrl = urls[0];
                            console.log("[AddFolder] folderUrl=" + folderUrl);
                            try {
                                filesystem.folder_access_granted(folderUrl);
                                Qt.callLater(filesystem.save_allowed_folders);
                                console.log("[AddFolder] folder_access_granted ok");
                            } catch (e) {
                                console.log("[AddFolder] folder_access_granted FAILED:", e);
                                return;
                            }
                            try {
                                render_queue.add_gyro_folder(folderUrl.toString());
                                console.log("[AddFolder] add_gyro_folder ok");
                            } catch (e) {
                                console.log("[AddFolder] add_gyro_folder FAILED:", e);
                                return;
                            }
                            let more = [];
                            try {
                                const jsonStr = render_queue.list_video_files_in_folder(
                                    folderUrl.toString(),
                                    JSON.stringify(fileDialog.extensions)
                                );
                                console.log("[AddFolder] list_video_files_in_folder json.length=" + (jsonStr ? jsonStr.length : 0));
                                more = JSON.parse(jsonStr);
                                console.log("[AddFolder] parsed more.length=" + more.length);
                            } catch (e) {
                                console.log("[AddFolder] list_video_files_in_folder FAILED:", e);
                                return;
                            }
                            if (more.length === 0) {
                                console.log("[AddFolder] no video files found in folder; nothing to enqueue");
                                return;
                            }
                            console.log("[AddFolder] first video URL=" + more[0]);
                            try {
                                // Real video URLs from list_video_files_in_folder: safe to
                                // hand back to dt.loadFiles which routes through
                                // add() -> render_queue.add_file. If any are still
                                // tree-style URIs the per-step log above will show it.
                                dt.loadFiles(more);
                                console.log("[AddFolder] dt.loadFiles dispatched " + more.length + " urls");
                            } catch (e) {
                                console.log("[AddFolder] dt.loadFiles FAILED:", e);
                            }
                        };
                    }
                    mobileAddFolderDialog.open();
                }
            }
        }
    }

    ListView {
        id: lv;
        anchors.left: parent.left;
        anchors.leftMargin: 10 * dpiScale;
        anchors.right: parent.right;
        anchors.rightMargin: 10 * dpiScale;
        anchors.top: mobileAddArea.bottom;
        anchors.topMargin: 5 * dpiScale;
        anchors.bottom: multiSelectBar.visible ? multiSelectBar.top : (topGyroButtons.visible ? topGyroButtons.top : parent.bottom);
        anchors.bottomMargin: (multiSelectBar.visible || topGyroButtons.visible) ? 5 * dpiScale : 30 * dpiScale;
        clip: true;
        model: render_queue.queue;
        // [queue-lifecycle T1] Historical restore timer and save connections were removed.
        // [T20] Spacing is handled inside delegates so same-gyro color bars can stay continuous.
        spacing: 0;
        onCountChanged: root.requestQueueLayout();
        onContentYChanged: root.updateDragSelectionFromViewY(root._dragSelectViewY);
        QQC.ScrollIndicator.vertical: QQC.ScrollIndicator { }

        // Autoscroll while drag-selecting: when the cursor / finger nears the top or
        // bottom edge of the visible region, advance contentY so the selection range
        // can extend past the viewport. onContentYChanged then re-runs the selection
        // recompute against the updated viewport, no extra wiring needed.
        Timer {
            id: autoScrollTimer;
            interval: 16;
            repeat: true;
            running: root._dragSelecting;
            onTriggered: {
                if (root._dragSelectViewY < 0) return;
                const edge = 40 * dpiScale;
                const speed = 6 * dpiScale;
                if (root._dragSelectViewY < edge) {
                    lv.contentY = Math.max(lv.originY, lv.contentY - speed);
                } else if (root._dragSelectViewY > lv.height - edge) {
                    const maxY = Math.max(lv.originY, lv.contentHeight - lv.height + lv.originY);
                    lv.contentY = Math.min(maxY, lv.contentY + speed);
                }
            }
        }

        // [queue-lifecycle T3] Manual drag-reorder state was removed.
        property bool isDragging: false  // Kept as a constant for external references in VideoArea.qml.
        delegate: Item {
            // https://doc.qt.io/qt-6/qtquick-tutorials-dynamicview-dynamicview3-example.html
            // [T20] Inner spacing: no spacing inside a matched gyro group, 5px otherwise.
            property real delegateSpacing: (dlg.isMatched && dlg.sameGyroAsNext) ? 0 : 5 * dpiScale
            property bool showProgressColumn: !window.isMobileLayout && !dlg.isFinished && !dlg.isSkipped
            property real progressColumnWidth: showProgressColumn ? Math.max(200 * dpiScale, progressText.implicitWidth, time.implicitWidth) : 0
            property real progressColumnGap: 10 * dpiScale
            property real trailingControlsWidth: btnsRow.width + (showProgressColumn ? progressColumnWidth + progressColumnGap : 0)
            property real textRightLimit: Math.max(0, innerItm.width - trailingControlsWidth)
            property real delegateContentHeight: Math.max(
                70 * dpiScale,
                Math.max(textColumn.implicitHeight, textColumn.childrenRect.height, textColumn.height) + 20 * dpiScale,
                showProgressColumn ? Math.max(progressColumn.implicitHeight, progressColumn.childrenRect.height, progressColumn.height) + 20 * dpiScale : 0,
                btnsRow.height + 20 * dpiScale
            )
            implicitHeight: delegateContentHeight + 2*innerItm.y + messageAreaParent.height + delegateSpacing;
            height: implicitHeight;
            onImplicitHeightChanged: root.requestQueueLayout();
            onWidthChanged: root.requestQueueLayout();
            width: parent? parent.width : 0;
            id: dlg;
            property int jobId: job_id;
            property bool isSelected: !!root.selectedJobs[job_id];
            property var displayParams: { root.matchVersion; try { return JSON.parse(render_queue.get_job_display_params(job_id)); } catch(e) { return {}; } }
            property real progress: current_frame / total_frames;
            property bool isFinished: current_frame >= total_frames && total_frames > 0;
            property bool isError: error_string.length > 0 && !isQuestion && !isInfo;
            property bool isInfo: error_string == "uses_cpu";
            property bool isQuestion: error_string.startsWith("convert_format:") || error_string.startsWith("file_exists:");
            property bool isProcessing: processing_progress > 0.0 && processing_progress < 1.0;
            property bool isSkipped: skip_reason.length > 0;
            property string skipReason: skip_reason;
            property string errorString: error_string;
            property real basicTextSize: (window.isMobileLayout? 10 : 12) * dpiScale;
            property var syncStatus: { root.syncStatusVersion; try { return JSON.parse(render_queue.get_batch_sync_status_json(job_id)); } catch(e) { return { color: "none" }; } }
            property string syncColor: syncStatus.color || "none"
            property bool syncDonePending: syncColor === "done_pending"
            property bool hasSyncStatus: syncColor === "green" || syncColor === "yellow" || syncDonePending
            property bool isInProgress: (!isFinished && !isError && !isSkipped && !isQuestion && total_frames > 0) && (current_frame > 0 || isProcessing || syncDonePending);
            property bool canStopProgress: isInProgress && !syncDonePending
            property bool canResetStatus: isError || isFinished || isQuestion || isSkipped
            function isDonePendingJob(id) {
                root.syncStatusVersion;
                try {
                    const status = JSON.parse(render_queue.get_batch_sync_status_json(id));
                    return (status.color || "none") === "done_pending";
                } catch(e) {
                    return false;
                }
            }

            // T5: Match status for this delegate.
            // [T19] matchVersion forces re-evaluation when match results change.
            property var matchStatus: { root.matchVersion; return root.hasGyroFiles ? JSON.parse(render_queue.get_match_status_json(job_id)) : ({status: "none"}); }
            property string matchState: matchStatus.status || "none"
            property int matchGyroIndex: matchStatus.gyro_index !== undefined && matchStatus.gyro_index !== null ? matchStatus.gyro_index : -1
            property color matchColor: matchGyroIndex >= 0 ? root.gyroColors[matchGyroIndex % root.gyroColors.length] : "transparent"
            property string gyroFilename: matchStatus.gyro_filename || ""
            property int manualGyroIndex: { root.matchVersion; return render_queue.get_manual_pair_gyro_index(job_id); }
            // [queue-gyro-column T8, T14] Dual display mode: matched vs. unmatched.
            // isMatched now follows the global matchExecuted flag.
            property bool isMatched: root.matchExecuted
            property int unmatchedGyroIndex: index < root.gyroFilesInfo.length ? index : -1
            property int displayGyroIndex: isMatched ? matchGyroIndex : unmatchedGyroIndex
            property color statusAccentColor: isSkipped ? root.skippedStatusColor
                : hasSyncStatus ? (syncColor === "green" ? root.finishedStatusColor : (syncDonePending ? root.pendingSyncStatusColor : root.manualStatusColor))
                : isFinished ? root.finishedStatusColor
                : isError ? root.errorStatusColor
                : isQuestion ? styleAccentColor
                : "transparent"
            // [T15] Adjacent same-gyro state is computed in Rust to avoid QML binding timing issues.
            // [T22] Read sameGyro state from a cache built after matching.
            property bool sameGyroAsPrev: { root.matchVersion; return root.matchExecuted && render_queue.get_cached_same_gyro_prev(job_id); }
            property bool sameGyroAsNext: { root.matchVersion; return root.matchExecuted && render_queue.get_cached_same_gyro_next(job_id); }
            onProgressChanged: {
                const times = Util.calculateTimesAndFps(progress, current_frame, start_timestamp);
                if (times !== false) {
                    time.elapsed = times[0];
                    time.remaining = times[1];
                    if (times.length > 2) time.fps = times[2];
                    if (start_timestamp_frame > 0 && start_timestamp2 > 0) {
                        const progress2 = (current_frame - start_timestamp_frame) / (total_frames - start_timestamp_frame);
                        const avgTimes = Util.calculateTimesAndFps(progress2, current_frame - start_timestamp_frame, start_timestamp2);
                        if (avgTimes !== false) {
                            time.remaining = avgTimes[1];
                            if (avgTimes.length > 2) time.fps = avgTimes[2];
                        }
                    }
                } else {
                    time.elapsed = "";
                }
            }
            onErrorStringChanged: {
                if (job_id == render_queue.main_job_id && error_string == "uses_cpu") {
                    window.videoArea.videoLoader.infoMessage.type = InfoMessage.Warning;
                    window.videoArea.videoLoader.infoMessage.text = window.getReadableError(error_string);
                    window.videoArea.videoLoader.infoMessage.show = true;
                }
            }

            // [queue-lifecycle T3] Drag-reorder support was removed.
            // T7: Lower opacity for already-paired items when in pairing mode.
            opacity: (root.pairingGyroIndex >= 0 && dlg.matchState === "Matched" ? 0.5 : 1);
            Ease on opacity { }

            ContextMenuMouseArea {
                id: rowContextArea;
                // Do not cover the checkbox column. A parent MouseArea that anchors.fills
                // the row would grab pointer events away from the checkbox column's
                // DragHandler / TapHandler. Anchoring left to checkboxCol.right gives the
                // checkbox column an independent hit region.
                anchors.fill: undefined;
                anchors.left: checkboxCol.right;
                anchors.right: parent.right;
                anchors.top: parent.top;
                anchors.bottom: parent.bottom;
                acceptedButtons: Qt.LeftButton | Qt.RightButton;
                hoverEnabled: true;
                onContextMenu: (isHold, x, y) => contextMenu.popup(dlg, x, y)

                // Mobile single-finger long-press → context menu. The
                // ContextMenuMouseArea's internal TapHandler can't see this
                // event because acceptedButtons above includes LeftButton, so
                // the outer MouseArea grabs single-touch first. We drive a
                // manual timer from the MouseArea's own onPressed/onReleased
                // here. Gated to mobile so desktop input keeps the existing
                // onContextMenu (right-click) path untouched.
                property real _lpStartX: 0;
                property real _lpStartY: 0;
                Timer {
                    id: rowLongPressTimer;
                    interval: 600;
                    onTriggered: contextMenu.popup(dlg, rowContextArea._lpStartX, rowContextArea._lpStartY);
                }

                onPressed: (mouse) => {
                    // Pairing mode is triggered on press (not click) so it fires before
                    // any potential drag-reorder swallows the gesture.
                    if (root.pairingGyroIndex >= 0 && mouse.button === Qt.LeftButton) {
                        render_queue.manual_set_calibration_pair(job_id, root.pairingGyroIndex);
                        render_queue.exit_pairing_mode();
                        root.pairingGyroIndex = -1;
                        root.pairingGyroFilename = "";
                        return;
                    }
                    if (Qt.platform.os === "android" || Qt.platform.os === "ios") {
                        rowContextArea._lpStartX = mouse.x;
                        rowContextArea._lpStartY = mouse.y;
                        rowLongPressTimer.restart();
                    }
                }
                onReleased: rowLongPressTimer.stop();
                onPositionChanged: (mouse) => {
                    if (!rowLongPressTimer.running) return;
                    const dx = mouse.x - rowContextArea._lpStartX;
                    const dy = mouse.y - rowContextArea._lpStartY;
                    if (dx*dx + dy*dy > 18*18) rowLongPressTimer.stop();
                }
                onClicked: (mouse) => {
                    // Selection on the row body mirrors the checkbox column:
                    //   plain click  → toggle
                    //   Shift+click  → range select anchored at the last plain click
                    //   Ctrl+click   → toggle (explicit alias)
                    // Use onClicked instead of onPressed: a drag suppresses onClicked,
                    // so starting a drag does not first toggle the anchor row. (Drag
                    // itself is only wired in the checkbox column.)
                    if (mouse.button !== Qt.LeftButton) return;
                    if (root.pairingGyroIndex >= 0) return;
                    root.handleSelectionClick(job_id, index, mouse.modifiers);
                }
            }
            Component {
                id: gyroPairActionComponent;
                Action {
                    property int gyroIdx: -1
                }
            }

            Menu {
                id: contextMenu;
                font.pixelSize: 11.5 * dpiScale;
                Action {
                    iconName: "play";
                    text: qsTr("Render now");
                    enabled: !isFinished && !isInProgress;
                    onTriggered: {
                        // [queue-render-skip] Skipped 状态先重置再渲染
                        if (isSkipped) render_queue.reset_job(job_id);
                        render_queue.render_job(job_id);
                    }
                }
                Action {
                    iconName: "pencil";
                    text: qsTr("Edit");
                    enabled: !isInProgress;
                    onTriggered: {
                        // Part B fix E: also mark this job as selected so
                        // LensGroupConfig.batchScope kicks in and the
                        // per-job hint + "Apply globally" button appear in
                        // the main view editor.
                        root.setSelectedJobs({ [job_id]: true }, job_id);
                        const data = render_queue.get_gyroflow_data(job_id);
                        if (data) {
                            window.videoArea.loadGyroflowData(JSON.parse(data), job_id);
                        }
                        root.shown = false;
                    }
                }
                // [queue-lifecycle T3] Manual "Move up" / "Move down" actions were removed.
                Action {
                    iconName: canStopProgress? "close" : "spinner";
                    text: canStopProgress? qsTr("Stop") : qsTr("Reset status");
                    enabled: canResetStatus || canStopProgress;
                    // [batch-reset] If the right-clicked row is part of a multi-selection, reset every selected job
                    onTriggered: {
                        if (root.selectedCount > 1 && root.selectedJobs[job_id]) {
                            const ids = Object.keys(root.selectedJobs).map(Number);
                            for (const id of ids) {
                                if (dlg.isDonePendingJob(id)) continue;
                                render_queue.reset_job(id);
                            }
                        } else {
                            render_queue.reset_job(job_id);
                        }
                    }
                }
                // T14: Manual gyro pairing sub-menu
                Menu {
                    id: gyroSubMenu;
                    title: qsTr("Pair with Gyro");
                    enabled: root.hasGyroFiles;
                    width: 300 * dpiScale;
                    property var dynamicGyroActions: []
                    function clearDynamicGyroActions(): void {
                        for (let i = 0; i < dynamicGyroActions.length; ++i) {
                            const action = dynamicGyroActions[i];
                            if (action) {
                                gyroSubMenu.removeAction(action);
                                action.destroy();
                            }
                        }
                        dynamicGyroActions = [];
                    }
                    onAboutToShow: {
                        clearDynamicGyroActions();
                        let actions = [];
                        // Add items for each gyro file
                        for (let i = 0; i < root.gyroFilesInfo.length; i++) {
                            const info = root.gyroFilesInfo[i];
                            const label = info.filename + (info.duration_ms ? " (" + (info.duration_ms / 1000).toFixed(1) + "s)" : "");
                            const action = gyroPairActionComponent.createObject(gyroSubMenu, {
                                text: label,
                                gyroIdx: i
                            });
                            if (!action)
                                continue;
                            action.triggered.connect(function() {
                                render_queue.manual_set_calibration_pair(job_id, action.gyroIdx);
                            });
                            gyroSubMenu.addAction(action);
                            actions.push(action);
                        }
                        dynamicGyroActions = actions;
                    }
                    onClosed: clearDynamicGyroActions()
                    Component.onDestruction: clearDynamicGyroActions()
                }
                // [queue-pair-ux T2] Unpair 选项已移除，Rust 端 unpair_video() 保留
            }

            Rectangle {
                anchors.fill: parent;
                color: styleBackground2;
                opacity: 0.2;
                radius: 5 * dpiScale;
                border.width: window.isMobileLayout && !statusBg.shown? 1 * dpiScale : 0;
                border.color: root.queueOutlineColor;
            }
            // Always-visible selection column. TapHandler handles tap (both mouse and touch);
            // DragHandler is restricted to PointerDevice.Mouse so touch drags fall through to
            // the ListView's Flickable and scroll the list instead of hijacking into drag-select.
            Item {
                id: checkboxCol;
                width: 32 * dpiScale;
                anchors.left: parent.left;
                anchors.top: parent.top;
                anchors.bottom: parent.bottom;
                z: 10;

                CheckBox {
                    anchors.verticalCenter: parent.verticalCenter;
                    anchors.horizontalCenter: parent.horizontalCenter;
                    checked: dlg.isSelected;
                    // Input goes through TapHandler/DragHandler; checkbox is a visual indicator only
                    enabled: false;
                    opacity: 1.0;
                    scale: 0.85;
                }

                // Mouse: split by Shift modifier. acceptedModifiers filters which keyboard
                // state each handler accepts; only the matching one fires per click. Each
                // is restricted to PointerDevice.Mouse so touch input does not get split.
                // gesturePolicy defaults to DragThreshold, so press-then-drag yields the
                // grab to the sibling DragHandler.
                TapHandler {
                    acceptedDevices: PointerDevice.Mouse;
                    acceptedModifiers: Qt.NoModifier;
                    onTapped: root.handleSelectionClick(dlg.jobId, index, 0);
                }
                TapHandler {
                    acceptedDevices: PointerDevice.Mouse;
                    acceptedModifiers: Qt.ShiftModifier;
                    onTapped: root.handleSelectionClick(dlg.jobId, index, Qt.ShiftModifier);
                }
                TapHandler {
                    acceptedDevices: PointerDevice.Mouse;
                    acceptedModifiers: Qt.ControlModifier;
                    onTapped: root.handleSelectionClick(dlg.jobId, index, 0);
                }
                // Touch path: Qt 6.7's TapHandler.onLongPressed is cancelled by
                // tiny finger jitter on Android (DragThreshold ~10px) and almost
                // never fires, so we replace it with a MouseArea + manual Timer.
                // The MouseArea also handles cross-row drag selection because it
                // keeps the touch grab through the whole press→release sequence
                // regardless of finger position; on mobile the sibling
                // touchDragSelectHandler would compete for the grab so we disable
                // it (see its enabled binding below). Desktop mouse still routes
                // through the Mouse-restricted TapHandlers above.
                MouseArea {
                    id: touchSelectArea;
                    anchors.fill: parent;
                    enabled: Qt.platform.os === "android" || Qt.platform.os === "ios";
                    property real _pressX: 0;
                    property real _pressY: 0;
                    Timer {
                        id: armMultiSelectTimer;
                        interval: 600;
                        onTriggered: {
                            root.beginDragSelection(index, !dlg.isSelected);
                            root._touchSelectActive = true;
                            lv.interactive = false;
                        }
                    }
                    onPressed: (mouse) => {
                        touchSelectArea._pressX = mouse.x;
                        touchSelectArea._pressY = mouse.y;
                        armMultiSelectTimer.restart();
                    }
                    onReleased: {
                        armMultiSelectTimer.stop();
                        if (root._touchSelectActive) {
                            root.endDragSelection();
                            root._touchSelectActive = false;
                            lv.interactive = true;
                        }
                    }
                    onClicked: (mouse) => {
                        // Short tap (timer didn't fire because user released before 600ms):
                        // toggle row selection. After arm + drag we already cleared
                        // _touchSelectActive in onReleased above, so guard against the
                        // post-release onClicked re-toggling.
                        if (root._dragSelecting) return;
                        root.handleSelectionClick(dlg.jobId, index, 0);
                    }
                    onPositionChanged: (mouse) => {
                        if (root._touchSelectActive) {
                            // Post-arm: paint selection across rows. mouse.x/y is local
                            // to this MouseArea; map to lv content for the row hit-test
                            // and to lv viewport for the autoscroll edge logic.
                            const contentPt = touchSelectArea.mapToItem(lv.contentItem, mouse.x, mouse.y);
                            const viewPt = touchSelectArea.mapToItem(lv, mouse.x, mouse.y);
                            root._dragSelectViewY = viewPt.y;
                            root.updateDragSelectionAtContentY(contentPt.y);
                            return;
                        }
                        if (!armMultiSelectTimer.running) return;
                        const dx = mouse.x - touchSelectArea._pressX;
                        const dy = mouse.y - touchSelectArea._pressY;
                        if (dx*dx + dy*dy > 18*18) armMultiSelectTimer.stop();
                    }
                }

                // Touch laser-brush. Disabled until the long-press arms it, otherwise it
                // would compete with the ListView Flickable for normal scroll gestures.
                DragHandler {
                    id: touchDragSelectHandler;
                    acceptedDevices: PointerDevice.TouchScreen;
                    // On mobile the sibling MouseArea (touchSelectArea) owns the
                    // touch grab and runs the cross-row paint loop itself; this
                    // handler is for desktop touchscreens (e.g. touch laptops)
                    // where the mouse-restricted TapHandlers won't fire.
                    enabled: root._touchSelectActive
                          && Qt.platform.os !== "android"
                          && Qt.platform.os !== "ios";
                    target: null;
                    onActiveChanged: {
                        if (!active) {
                            root.endDragSelection();
                            root._touchSelectActive = false;
                            lv.interactive = true;
                        }
                    }
                    onCentroidChanged: {
                        if (!active) return;
                        const contentPt = touchDragSelectHandler.parent.mapToItem(lv.contentItem, centroid.position.x, centroid.position.y);
                        const viewPt = touchDragSelectHandler.parent.mapToItem(lv, centroid.position.x, centroid.position.y);
                        root._dragSelectViewY = viewPt.y;
                        root.updateDragSelectionAtContentY(contentPt.y);
                    }
                }

                DragHandler {
                    id: dragSelectHandler;
                    acceptedDevices: PointerDevice.Mouse;
                    target: null;
                    // iOS Photos-style "laser brush", driven entirely by cursor position:
                    //   - Drag activation records the anchor row, snapshots the selection and
                    //     picks a paint mode (add / remove) based on the anchor's prior state.
                    //     It does NOT toggle the anchor — TapHandler handles short clicks,
                    //     and leaving the anchor untouched lets reverse-drag back to the anchor
                    //     fully restore the original selection (including the anchor itself).
                    //   - Each centroid change rebuilds selection from snapshot:
                    //       idx === startIndex → no rows painted (selection == snapshot)
                    //       idx !== startIndex → [min,max] painted with paint mode (anchor included)
                    //   Dragging forward paints outward; dragging back to the anchor fully
                    //   reverses; crossing the anchor paints the other side.
                    onActiveChanged: {
                        if (active) {
                            root.beginDragSelection(index, !dlg.isSelected);
                        } else {
                            root.endDragSelection();
                        }
                    }
                    onCentroidChanged: {
                        if (!active) return;
                        const contentPt = dragSelectHandler.parent.mapToItem(lv.contentItem, centroid.position.x, centroid.position.y);
                        const viewPt = dragSelectHandler.parent.mapToItem(lv, centroid.position.x, centroid.position.y);
                        root._dragSelectViewY = viewPt.y;
                        root.updateDragSelectionAtContentY(contentPt.y);
                    }
                }
            }
            // [queue-gyro-column] 左列 gyro 区域（从 gyroColorBar 改造而来）
            // [queue-gyro-column T8] 双模式：已匹配时按 matchGyroIndex 对齐，未匹配时按行 index 填入
            Item {
                id: gyroArea;
                visible: width > 0;
                width: root.gyroColumnWidth;
                anchors.left: checkboxCol.right;
                anchors.top: parent.top;
                // [T22] 颜色条填满整个 delegate 高度（含 spacing 区域），
                // 同组时 delegateSpacing=0 自然无间隙，不同组时 spacing 区域也着色避免视觉断裂
                anchors.bottom: parent.bottom;
                Ease on width { }

                // 颜色背景（半透明），独立 Rectangle 避免影响文字 opacity
                // [queue-gyro-column T8] 已匹配用 matchColor/0.3，未匹配用 gyroColors[unmatchedGyroIndex]/0.15
                Rectangle {
                    id: gyroFill;
                    anchors.fill: parent;
                    property color baseColor: {
                        if (dlg.isMatched) return dlg.matchColor;
                        if (dlg.unmatchedGyroIndex >= 0) return root.gyroColors[dlg.unmatchedGyroIndex % root.gyroColors.length];
                        return "transparent";
                    }
                    color: baseColor;
                    opacity: {
                        if (dlg.isMatched) return root.matchedGyroOpacity;
                        if (dlg.unmatchedGyroIndex >= 0) return root.unmatchedGyroOpacity;
                        return 0;
                    }
                    radius: (dlg.isMatched && (dlg.sameGyroAsPrev || dlg.sameGyroAsNext)) ? 0 : 3 * dpiScale;
                    border.width: (root.lightTheme && baseColor.a > 0) ? 1 * dpiScale : 0;
                    border.color: root.withAlpha(baseColor, dlg.isMatched ? 0.40 : 0.32);
                    Ease on opacity { }
                }

                // [queue-gyro-column T8+T10] 时间文字叠加，置顶对齐
                // 已匹配: 仅组内第一行显示（!sameGyroAsPrev）
                // 未匹配: 每行都显示（每个 gyro 独占一行）
                BasicText {
                    id: gyroTimeText;
                    anchors.top: parent.top;
                    anchors.topMargin: 4 * dpiScale;
                    anchors.horizontalCenter: parent.horizontalCenter;
                    visible: root.hasGyroFiles && dlg.displayGyroIndex >= 0
                             && (dlg.isMatched ? (dlg.matchGyroIndex >= 0 && !dlg.sameGyroAsPrev) : true);
                    text: root.formatGyroTime(dlg.displayGyroIndex);
                    color: root.gyroTimeTextColor;
                    font.pixelSize: 11 * dpiScale;
                    font.bold: true;
                    leftPadding: 0;
                }

                // [T20] 断开分隔条已移至 gyroArea 外部的 separatorCol

                // T6: Tooltip showing gyro filename and time range
                MouseArea {
                    anchors.fill: parent;
                    hoverEnabled: true;
                    acceptedButtons: Qt.LeftButton | Qt.RightButton;
                    // T11: Right-click to enter pairing mode
                    onClicked: (mouse) => {
                        if (mouse.button === Qt.RightButton && dlg.displayGyroIndex >= 0) {
                            let gIdx = dlg.displayGyroIndex;
                            root.pairingGyroIndex = gIdx;
                            root.pairingGyroFilename = dlg.isMatched ? dlg.gyroFilename
                                : (gIdx < root.gyroFilesInfo.length ? root.gyroFilesInfo[gIdx].filename : "");
                            render_queue.enter_pairing_mode(gIdx);
                        }
                    }
                    ToolTip {
                        text: {
                            if (dlg.isMatched) {
                                return dlg.gyroFilename + (dlg.matchStatus.gyro_start_ms !== undefined ? "\n" + (dlg.matchStatus.gyro_start_ms / 1000).toFixed(1) + "s - " + (dlg.matchStatus.gyro_end_ms / 1000).toFixed(1) + "s" : "");
                            } else if (dlg.unmatchedGyroIndex >= 0 && dlg.unmatchedGyroIndex < root.gyroFilesInfo.length) {
                                return root.gyroFilesInfo[dlg.unmatchedGyroIndex].filename;
                            }
                            return "";
                        }
                        visible: parent.containsMouse && text.length > 0;
                    }
                }
            }
            // [T20] 隔离列：gyroArea 和视频列之间，未匹配时显示斜线纹理
            Item {
                id: separatorCol;
                property bool shouldShow: root.hasGyroFiles && !dlg.isMatched && dlg.unmatchedGyroIndex >= 0;
                visible: shouldShow;
                width: visible ? 12 * dpiScale : 0;
                anchors.left: gyroArea.right;
                anchors.top: parent.top;
                anchors.bottom: parent.bottom;
                clip: true;
                // 斜线纹理背景
                Repeater {
                    model: Math.ceil(separatorCol.height / (6 * dpiScale)) + 1
                    Rectangle {
                        x: 0;
                        y: index * 6 * dpiScale;
                        width: separatorCol.width;
                        height: 3 * dpiScale;
                        color: styleTextColor;
                        opacity: index % 2 === 0 ? 0.15 : 0;
                    }
                }
            }
            Item {
                height: parent.height;
                width: ipb.value * parent.width;
                clip: true;
                visible: opacity > 0;
                opacity: window.isMobileLayout && !statusBg.shown? 1 : 0;
                Ease on opacity { }
                Rectangle {
                    width: parent.parent.width;
                    height: parent.height;
                    radius: 5 * dpiScale;
                    color: root.finishedStatusColor;
                    opacity: root.lightTheme ? 0.22 : 0.35;
                }
            }
            Rectangle {
                id: statusBg;
                anchors.fill: parent;
                color: root.withAlpha(border.color, root.lightTheme ? 0.12 : 0.19);
                radius: 5 * dpiScale;
                opacity: shown? 0.8 : 0;
                Ease on opacity { }
                property bool shown: isFinished || isError || isQuestion || isSkipped || dlg.hasSyncStatus;
                visible: opacity > 0;
                border.color: dlg.statusAccentColor;
                border.width: 1;
            }

            Component {
                id: messageAreaComponent;
                Item {
                    height: messageAreaCol.height + 20 * dpiScale;
                    Hr { y: 2; color: statusBg.border.color; opacity: 0.2; }

                    Column {
                        id: messageAreaCol;
                        width: parent.width;
                        spacing: 10 * dpiScale;
                        y: 10 * dpiScale;

                        BasicText {
                            id: messageAreaText;
                            textFormat: Text.RichText;
                            leftPadding: 0;
                            font.pixelSize: basicTextSize;
                        }
                        Flow {
                            id: messageBtns;
                            visible: btns.model.length > 0;
                            spacing: 5 * dpiScale;
                            width: parent.width;
                            property string errorString: error_string;
                            onErrorStringChanged: {
                                const text = window.getReadableError(errorString).replace(/\n/g, "<br>");
                                messageAreaText.text = text? text : qsTr("Missing required components.");

                                if (errorString.startsWith("convert_format:")) {
                                    const params = errorString.split(":")[1].split(";");
                                    const candidate = params[2];
                                    const supported = params[1].split(",");

                                    // Simple-mode path: single modal per batch, then auto-apply to the rest
                                    if (window.isSimpleMode) {
                                        if (root.pendingConvertFormatChoice !== "") {
                                            Qt.callLater(render_queue.set_pixel_format, job_id, root.pendingConvertFormatChoice);
                                            btns.model = [];
                                            messageAreaText.text = qsTr("Applying pixel format: %1").arg(root.pendingConvertFormatChoice);
                                            return;
                                        }
                                        let modalBtns = supported.map(f => ({
                                            text: f,
                                            accent: f.toLowerCase() == candidate,
                                            clicked: () => {
                                                root.pendingConvertFormatChoice = f;
                                                render_queue.set_pixel_format(job_id, f);
                                            }
                                        }));
                                        modalBtns.push({
                                            text: qsTr("Render using CPU"),
                                            accent: candidate == '',
                                            clicked: () => {
                                                root.pendingConvertFormatChoice = "cpu";
                                                render_queue.set_pixel_format(job_id, "cpu");
                                            }
                                        });
                                        messageBox(Modal.Question,
                                            qsTr("Selected encoder does not support the source pixel format.\nChoose a target pixel format or render on CPU.\nThis choice applies to all remaining jobs in this batch."),
                                            modalBtns);
                                        btns.model = [];
                                        messageAreaText.text = qsTr("Waiting for pixel format selection…");
                                        return;
                                    }

                                    // Full-mode path: original inline buttons, one decision per job
                                    let buttons = supported.map(f => ({
                                        text: f,
                                        accent: f.toLowerCase() == candidate,
                                        clicked: () => { render_queue.set_pixel_format(job_id, f); }
                                    }));
                                    buttons.push({
                                        text: qsTr("Render using CPU"),
                                        accent: candidate == '',
                                        clicked: () => { render_queue.set_pixel_format(job_id, "cpu"); }
                                    });
                                    btns.model = buttons;
                                } else if (errorString.startsWith("file_exists:")) {
                                    const data = JSON.parse(errorString.substring(12));
                                    switch (render_queue.overwrite_mode) {
                                        case 1: Qt.callLater(render_queue.reset_job, job_id); btns.model = []; break; // Overwrite
                                        case 2: Qt.callLater(render_queue.set_job_output_filename, job_id, window.renameOutput(data.filename, data.folder), false); btns.model = []; break; // Rename
                                        case 3: Qt.callLater(render_queue.set_error_string, job_id, qsTr("Output file already exists.")); btns.model = []; break; // Skip
                                        default:
                                            btns.model = [
                                                { text: qsTr("Yes"),    clicked: () => { render_queue.reset_job(job_id); }, accent: true },
                                                { text: qsTr("Rename"), clicked: () => { render_queue.set_job_output_filename(job_id, window.renameOutput(data.filename, data.folder), true); } },
                                                { text: qsTr("No"),     clicked: () => { render_queue.set_error_string(job_id, qsTr("Output file already exists.")); btns.model = []; } },
                                            ];
                                        break;
                                    }
                                }
                            }
                            Repeater {
                                id: btns;
                                model: []
                                Button {
                                    text: modelData.text;
                                    height: 25 * dpiScale;
                                    accent: modelData.accent || false;
                                    leftPadding: 12 * dpiScale;
                                    rightPadding: 12 * dpiScale;
                                    font.pixelSize: 12 * dpiScale;
                                    onClicked: modelData.clicked();
                                }
                            }
                        }
                    }
                }
            }
            Item {
                id: messageAreaParent;
                visible: height > 0;
                anchors.bottom: parent.bottom;
                width: parent.width - 2*x;
                x: 15 * dpiScale;
                height: messageArea.active? messageArea.height : 0;
                Ease on height { }
                Loader {
                    id: messageArea;
                    active: (isError || isQuestion || isInfo) && !isFinished;
                    sourceComponent: messageAreaComponent;
                    width: parent.width;
                }
                clip: true;
            }
            // Selection highlight
            Rectangle {
                anchors.fill: innerItm;
                color: dlg.isSelected ? styleAccentColor : "transparent";
                opacity: 0.1;
                radius: 5 * dpiScale;
            }
            Item {
                id: innerItm;
                // [T20] x accounts for optional multi-select column, gyro column and separator
                x: 5 * dpiScale + checkboxCol.width + gyroArea.width + separatorCol.width;
                width: parent.width - x - 5 * dpiScale;
                height: dlg.delegateContentHeight;
                Image {
                    x: 5 * dpiScale;
                    source: thumbnail_url
                    fillMode: Image.PreserveAspectCrop
                    width: 50 * dpiScale;
                    height: 50 * dpiScale;
                    anchors.verticalCenter: parent.verticalCenter;
                    Rectangle {
                        anchors.fill: parent;
                        anchors.margins: -1 * dpiScale;
                        color: "transparent";
                        radius: 5 * dpiScale;
                        anchors.verticalCenter: parent.verticalCenter;
                        border.width: 1 * dpiScale;
                        border.color: styleVideoBorderColor
                    }
                    QQC.BusyIndicator { anchors.centerIn: parent; visible: !thumbnail_url; scale: 0.5; running: visible; }
                }

                Column {
                    id: textColumn;
                    x: 55 * dpiScale;
                    anchors.verticalCenter: parent.verticalCenter;
                    spacing: 3 * dpiScale;
                    width: Math.max(1 * dpiScale, dlg.textRightLimit - x - 10 * dpiScale);
                    height: childrenRect.height;
                    BasicText {
                        text: input_filename;
                        font.bold: true;
                        font.pixelSize: 14 * dpiScale;
                        width: parent.width;
                        wrapMode: Text.WordWrap;
                    }
                    BasicText {
                        visible: window.isMobileLayout;
                        width: parent.width;
                        wrapMode: Text.WordWrap;
                        font.pixelSize: basicTextSize;
                        property string remainingText: statusBg.shown? "---" : time.remaining;
                        property string eta: remainingText != "---"? (", " + qsTr("ETA %1").arg(remainingText)) : "";
                        text: syncDonePending ? qsTr("Sync complete: %1").arg("<b>100.00%</b>")
                            : isProcessing? qsTr("Synchronizing: %1").arg(`<b>${(processing_progress*100).toFixed(2)}%</b>`)
                                          : qsTr("Rendering: %1").arg(`<b>${(dlg.progress*100).toFixed(2)}%</b> <small>(${current_frame}/${total_frames}${time.fpsText}${eta})</small>`);
                    }
                    BasicText { text: qsTr("Save to: %1").arg("<b>" + display_output_path + "</b>"); font.pixelSize: basicTextSize; width: parent.width; wrapMode: Text.WordWrap; }
                    // Aligned display params. Flow wraps when Full mode narrows the queue panel.
                    Flow {
                        spacing: 10 * dpiScale;
                        width: parent.width;
                        height: visible ? implicitHeight : 0;
                        BasicText {
                            text: qsTranslate("Stabilization", "Smoothness") + " <b>" + ((dlg.displayParams.smoothness || 0.5) * 100).toFixed(0) + "%</b>";
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            text: qsTranslate("Stabilization", "Lock horizon") + " " + ((dlg.displayParams.horizon_lock_amount || 0) > 0 ? "✓" : "✗");
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            text: qsTranslate("Stabilization", "Auto rotate") + " " + (dlg.displayParams.auto_rotate ? "✓" : "✗");
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            property string zm: dlg.displayParams.zoom_mode || "none";
                            text: (zm === "static" ? qsTranslate("Popup", "Static zoom") : zm === "dynamic" ? qsTranslate("Popup", "Dynamic zooming") : qsTranslate("Popup", "No zooming"));
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            visible: (dlg.displayParams.framerate || 0) > 0;
                            text: "<b>" + (dlg.displayParams.framerate || 0).toFixed(0) + "fps</b>";
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            visible: (dlg.displayParams.focal_length || 0) > 0
                                && (dlg.displayParams.lens_group_display_mode || "auto") === "auto";
                            text: "<b>" + (dlg.displayParams.focal_length || 0).toFixed(0) + "mm</b>";
                            font.pixelSize: basicTextSize;
                        }
                    }
                    Flow {
                        visible: (dlg.displayParams.lens_group_display_mode || "auto") !== "auto";
                        width: parent.width;
                        height: visible ? implicitHeight : 0;
                        spacing: 10 * dpiScale;

                        BasicText {
                            text: qsTr("Mode") + " <b>" + ((dlg.displayParams.lens_group_display_mode || "auto") === "local" ? qsTr("Local") : qsTr("Global")) + "</b>";
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            visible: (dlg.displayParams.lens_group_display_number || 0) > 0;
                            text: qsTr("Lens") + " <b>L" + (dlg.displayParams.lens_group_display_number || 0) + "</b>";
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            visible: (dlg.displayParams.lens_group_display_focal_length || 0) > 0;
                            text: qsTr("Focal") + " <b>" + (dlg.displayParams.lens_group_display_focal_length || 0).toFixed(0) + "mm</b>";
                            font.pixelSize: basicTextSize;
                        }
                        BasicText {
                            visible: (dlg.displayParams.lens_group_display_ratio || 0) > 0;
                            text: qsTr("Anamorphic") + " <b>" + (dlg.displayParams.lens_group_display_ratio || 0).toFixed(2) + "x" + (dlg.displayParams.lens_group_display_direction ? ("-" + dlg.displayParams.lens_group_display_direction) : "") + "</b>";
                            font.pixelSize: basicTextSize;
                        }
                    }
                    // T5+T6: Match status annotation with gyro filename.
                    BasicText {
                        visible: root.hasGyroFiles && (dlg.manualGyroIndex >= 0 || (dlg.matchState !== "none" && dlg.matchState !== "Unmatched" && dlg.matchState !== "NoCreationTime"));
                        width: parent.width;
                        wrapMode: Text.WordWrap;
                        color: dlg.manualGyroIndex >= 0 ? root.manualStatusColor
                            : dlg.matchState === "Matched" ? root.matchedStatusColor
                            : root.calibrationStatusColor;
                        font.pixelSize: basicTextSize;
                        font.bold: true;
                        text: dlg.manualGyroIndex >= 0
                            ? qsTr("Manual") + " ⚡ " + (dlg.manualGyroIndex >= 0 && dlg.manualGyroIndex < root.gyroFilesInfo.length ? root.gyroFilesInfo[dlg.manualGyroIndex].filename : "")
                            : dlg.matchState === "Matched"
                                ? "✓ " + dlg.gyroFilename + (dlg.matchStatus.detected_source ? " (" + dlg.matchStatus.detected_source + ")" : "")
                                : qsTr("Calibration") + " · " + dlg.gyroFilename;
                    }
                    BasicText {
                        visible: dlg.hasSyncStatus;
                        text: dlg.syncColor === "green" ? qsTr("Sync confirmed")
                            : dlg.syncDonePending ? qsTr("Sync complete, waiting for batch confirmation")
                            : qsTr("Sync not confirmed");
                        color: dlg.syncColor === "green" ? root.finishedStatusColor
                            : dlg.syncDonePending ? root.pendingSyncStatusColor
                            : root.manualStatusColor;
                        font.pixelSize: basicTextSize;
                        font.bold: true;
                    }
                    // [queue-render-skip] Show skip reason.
                    BasicText {
                        visible: dlg.isSkipped;
                        text: dlg.skipReason === "no_gyro" ? qsTr("Skipped - no gyro data")
                            : dlg.skipReason === "calibration" ? qsTr("Skipped - calibration pair")
                            : "";
                        color: root.skippedStatusColor;
                        font.pixelSize: basicTextSize;
                        font.bold: true;
                    }
                }

                Column {
                    id: progressColumn;
                    anchors.right: btnsRow.left;
                    anchors.rightMargin: dlg.progressColumnGap;
                    spacing: 6 * dpiScale;
                    width: dlg.progressColumnWidth;
                    height: childrenRect.height;
                    anchors.verticalCenter: parent.verticalCenter;
                    // [T19] Hide progress/time information after completion or skip.
                    visible: dlg.showProgressColumn;

                    BasicText {
                        id: progressText;
                        leftPadding: 0;
                        anchors.horizontalCenter: parent.horizontalCenter;
                        horizontalAlignment: Text.AlignHCenter;
                        textFormat: Text.RichText;
                        text: syncDonePending ? "<b>100.00%</b>" :
                                            isProcessing? `<b>${(processing_progress*100).toFixed(2)}%</b>` :
                                            `<b>${(dlg.progress*100).toFixed(2)}%</b> <small>(${current_frame}/${total_frames}${time.fpsText})</small>`;
                    }
                    QQC.ProgressBar {
                        id: ipb;
                        width: 200 * dpiScale;
                        value: syncDonePending ? 1.0 : isProcessing? processing_progress : current_frame / total_frames;
                    }
                    BasicText {
                        id: time;
                        property string elapsed: "---";
                        property string remaining: "---";
                        property real fps: 0;
                        property string fpsText: dlg.progress > 0? qsTr(" @ %1fps").arg(fps.toFixed(1)) : "";
                        leftPadding: 0;
                        anchors.horizontalCenter: parent.horizontalCenter;
                        horizontalAlignment: Text.AlignHCenter;
                        text: syncDonePending ? qsTr("Waiting for batch confirmation")
                                          : isProcessing? qsTr("Synchronizing...")
                                          : qsTr("Elapsed: %1. Remaining: %2").arg("<b>" + elapsed + "</b>").arg("<b>" + (statusBg.shown? "---" : remaining) + "</b>");
                    }
                }

                Item {
                    id: btnsRow;
                    anchors.right: parent.right;
                    anchors.verticalCenter: parent.verticalCenter;
                    width: btnsRowInner.width;
                    height: btnsRowInner.height;
                    Ease on width { }

                    component IconButton: LinkButton {
                        width: 30 * dpiScale;
                        height: 30 * dpiScale;
                        textColor: styleAccentColor;
                        icon.width: 15 * dpiScale;
                        icon.height: 15 * dpiScale;
                        leftPadding: 0;
                        rightPadding: 0;
                        font.underline: false;
                        font.bold: true;
                        Ease on opacity { duration: 300; }
                        opacity: pressed? 0.8 : 1;
                    }

                    Row {
                        id: btnsRowInner;
                        IconButton {
                            visible: dlg.isFinished && Qt.platform.os != "ios";
                            iconName: "play";
                            icon.width: 25 * dpiScale;
                            icon.height: 25 * dpiScale;
                            tooltip: qsTr("Open rendered file");
                            onClicked: filesystem.open_file_externally(filesystem.get_file_url(output_folder, output_filename, false));
                        }
                        IconButton {
                            visible: dlg.isFinished && Qt.platform.os != "android" && Qt.platform.os != "ios";
                            iconName: "folder";
                            tooltip: qsTr("Open file location");
                            onClicked: filesystem.open_file_externally(output_folder);
                        }
                        IconButton {
                            tooltip: qsTr("Remove");
                            textColor: "#f67575"
                            iconName: dlg.isFinished? "close" : "bin";
                            onClicked: render_queue.remove(job_id);
                        }
                    }
                }
            }
            clip: true;
        }
        highlight: Item { }
    }

    // Multi-select toolbar — shown whenever at least one job is selected.
    // "Done" applies the current batch-edit params to all selected jobs and clears the selection.
    Row {
        id: multiSelectBar;
        visible: root.selectedCount > 0;
        anchors.horizontalCenter: parent.horizontalCenter;
        anchors.bottom: topGyroButtons.visible ? topGyroButtons.top : parent.bottom;
        anchors.bottomMargin: topGyroButtons.visible ? 5 * dpiScale : 30 * dpiScale;
        spacing: 10 * dpiScale;

        BasicText {
            text: qsTr("Selected: %1").arg(root.selectedCount);
            color: styleAccentColor;
            font.pixelSize: 12 * dpiScale;
            font.bold: true;
            anchors.verticalCenter: parent.verticalCenter;
            leftPadding: 0;
        }
        LinkButton {
            text: qsTr("Select all");
            font.pixelSize: 12 * dpiScale;
            anchors.verticalCenter: parent.verticalCenter;
            onClicked: root.selectAllJobs();
        }
        LinkButton {
            text: qsTr("Deselect");
            font.pixelSize: 12 * dpiScale;
            anchors.verticalCenter: parent.verticalCenter;
            onClicked: root.deselectAllJobs();
        }
        LinkButton {
            text: qsTr("Done");
            font.pixelSize: 12 * dpiScale;
            anchors.verticalCenter: parent.verticalCenter;
            onClicked: {
                if (typeof window !== "undefined" && window.applyBatchParams) {
                    window.applyBatchParams();
                }
                root.deselectAllJobs();
            }
        }
    }

    // Auto match / Clear 按钮（从标题下方移至 ListView 下方居中）
    Row {
        id: topGyroButtons;
        anchors.bottom: parent.bottom;
        anchors.bottomMargin: 30 * dpiScale;
        anchors.horizontalCenter: parent.horizontalCenter;
        spacing: 8 * dpiScale;
        height: root.hasGyroFiles ? 30 * dpiScale : 0;
        visible: height > 0;
        clip: true;
        Ease on height { }

        Button {
            id: autoMatchBtn;
            property bool matching: false;
            text: matching ? qsTr("Matching...") : qsTr("Auto match");
            enabled: root.hasGyroFiles && root.allGyroParsed && !matching;
            accent: true;
            height: 30 * dpiScale;
            font.pixelSize: 13 * dpiScale;
            leftPadding: 16 * dpiScale;
            rightPadding: 16 * dpiScale;
            function beginMatch(): void {
                matching = true;
                root.matchWarning = "";
                render_queue.auto_rotate = window.batchState ? window.batchState.autoRotate : false;
                loader.text = qsTr("Matching...");
                loader.active = true;
                matchTimer.start();
            }
            onClicked: {
                // Part B fix B: per user request, don't wipe L1-L6 manual focal
                // when re-matching — manual configurations should persist across
                // match passes. (Used to pop a "Re-matching will clear lens group
                // settings. Continue?" modal that called set_lens_group_config("[]").)
                beginMatch();
            }
        }
        Button {
            text: qsTr("Clear");
            height: 30 * dpiScale;
            font.pixelSize: 13 * dpiScale;
            leftPadding: 16 * dpiScale;
            rightPadding: 16 * dpiScale;
            onClicked: {
                render_queue.clear_gyro_files();
                root.matchWarning = "";
            }
        }
    }

    // [queue-gyro-column] 旧的 batchEditPanel 和 gyroButtonRow 已删除

    // [queue-gyro-column] 空状态拖拽提示
    BasicText {
        visible: lv.count === 0 && !window.isMobileLayout;
        text: qsTr("Drop video files or gyroscope data here");
        anchors.centerIn: lv;
        color: styleTextColor;
        opacity: 0.5;
        font.pixelSize: 14 * dpiScale;
        leftPadding: 0;
    }

    DropTarget {
        id: dt;
        color: styleBackground2;
        anchors.margins: 0 * dpiScale;
        anchors.topMargin: lv.y;
        extensions: fileDialog.extensions;
        acceptedFilenameSuffixes: ["_mix.bin", ".rdc"];
        acceptAnyMatchingUrl: true;
        visible: !lv.isDragging;
        function prepareBatchAdditionalData(additional: var): var {
            if (!additional || !additional.output) return additional;

            if (!window.exportSettings.preserveOutputSettings.checked) {
                delete additional.output.output_width;
                delete additional.output.output_height;
            }
            return additional;
        }
        function add(outFolder: string, urls: list<url>): void {
            try {
                const filteredJson = render_queue.filter_paired_gyroflow_siblings(
                    JSON.stringify(urls.map(u => u.toString())),
                    JSON.stringify(fileDialog.extensions)
                );
                urls = JSON.parse(filteredJson);
            } catch (e) {
                console.log("filter_paired_gyroflow_siblings failed:", e);
            }
            if (!urls.length) return;

            let foldersWithoutAccess = [];
            let additional = prepareBatchAdditionalData(window.getAdditionalProjectData());
            if (!outFolder) {
                // Android SAF picker hands out per-file content URIs, so the
                // source folder is never writable. Resolve outFolder from the
                // persisted Export setting; if absent, prompt the user once
                // and re-enter add() so the rest of the pipeline runs uniformly.
                if (Qt.platform.os === "android" && isSandboxed) {
                    const fixed = window.exportSettings ? window.exportSettings.queueFixedOutputPath : "";
                    if (fixed && filesystem.can_create_file(fixed, "check.tmp")) {
                        add(fixed, urls);
                        return;
                    }
                    window.outputFile.selectFolder("", function(folder_url) {
                        if (window.exportSettings) {
                            window.exportSettings.queueFixedOutputPath = folder_url;
                            window.exportSettings.queueOutputMode = 1;
                        }
                        filesystem.folder_access_granted(folder_url);
                        Qt.callLater(filesystem.save_allowed_folders);
                        add(folder_url, urls);
                    });
                    return;
                }
                delete additional.output.output_folder;
                delete additional.output.output_filename;
                if (isSandboxed) {
                    for (const url of urls) {
                        const folder = filesystem.get_folder(url);
                        if (!foldersWithoutAccess.includes(folder) && !filesystem.can_create_file(folder, "check.tmp")) {
                            foldersWithoutAccess.push(folder);
                        }
                    }
                }
            } else {
                additional.output.output_folder = outFolder;
                delete additional.output.output_filename;
                if (isSandboxed) {
                    if (!foldersWithoutAccess.includes(outFolder) && !filesystem.can_create_file(outFolder, "check.tmp")) {
                        foldersWithoutAccess.push(outFolder);
                    }
                }
            }
            if (foldersWithoutAccess.length > 0) {
                console.log("Folders without write access:", foldersWithoutAccess);
                let remaining = foldersWithoutAccess.length;
                for (const folder of foldersWithoutAccess) {
                    remaining--;
                    let el = messageBox(Modal.Info, qsTr("Due to file access restrictions, you need to select the destination folder manually.\nClick Ok and select the destination folder."), [
                        { text: qsTr("Ok"), clicked: () => {
                            outputFile.selectFolder(folder, function(_) { if (!remaining) add(outFolder, urls); });
                        }},
                    ], undefined, Text.AutoText, "file-access-restriction");
                    if (!el) { // Don't show again triggered
                        outputFile.selectFolder(folder, function(_) { if (!remaining) add(outFolder, urls); });
                    }
                }
                return;
            }
            let urlsReady = [];
            let urlsRequiringSdk = [];
            for (const url of urls) {
                if (controller.check_external_sdk(filesystem.get_filename(url))) {
                    urlsRequiringSdk.push(url);
                } else {
                    urlsReady.push(url);
                }
            }
            if (urlsRequiringSdk.length > 0) {
                if (urlsReady.length > 0) {
                    add(outFolder, urlsReady);
                }
                root.ensureExternalSdkForQueue(urlsRequiringSdk, function() { add(outFolder, urlsRequiringSdk); });
                return;
            }
            additional = JSON.stringify(additional);

            // Natural sort the URLs
            const ne = str => str.toString().replace(/\d+/g, n => n.padStart(8, "0"));
            const nc = (a,b) => ne(a).localeCompare(ne(b));
            urls.sort(nc);

            // R3D files must be loaded sequentially (REDline SDK doesn't support concurrent decoding)
            const r3dUrls = urls.filter(u => u.toString().toLowerCase().endsWith(".r3d"));
            const otherUrls = urls.filter(u => !u.toString().toLowerCase().endsWith(".r3d"));
            for (const url of otherUrls) {
                const job_id = render_queue.add_file(url.toString(), "", additional);
                loader.pendingJobs[job_id] = true;
            }
            if (otherUrls.length > 0) loader.updateStatus();
            if (r3dUrls.length > 0) {
                r3dSeqLoader.startSequential(r3dUrls, additional);
            }
            // Filename sorting happens in onAdded (per-job, after q.push).
            // add_file returns synchronously but the actual model insertion is
            // queued, so sorting here would always be a no-op.
        }
        onLoadFiles: (urls) => {
            if (!urls.length) return;
            // [queue-pair-ux T4] 分类文件：_mix.bin 为陀螺仪文件，无扩展名尝试作为文件夹，其他为视频
            try {
                let urlStrings = [];
                for (const url of urls) urlStrings.push(url.toString());
                urls = JSON.parse(render_queue.filter_supported_drop_items(
                    JSON.stringify(urlStrings),
                    JSON.stringify(fileDialog.extensions)
                ));
            } catch (e) {
                console.log("filter_supported_drop_items failed:", e);
            }
            if (!urls.length) return;
            let videoUrls = [];
            for (const url of urls) {
                const fname = filesystem.get_filename(url).toLowerCase();
                if (render_queue.is_gyro_mix_file(url.toString())) {
                    render_queue.add_gyro_file(url.toString());
                } else if (fname.endsWith(".bin")) {
                    continue;
                } else if (!fname.includes(".") || fname.endsWith(".rdc")) {
                    // No extension or RED .RDC: treat as folder. Let Rust recursively scan for
                    // gyro _mix.bin files AND video files (max depth 3, max 600 videos,
                    // filtered by fileDialog.extensions, excluding files whose stem
                    // ends with the configured output suffix, e.g. _stabilized).
                    render_queue.add_gyro_folder(url.toString());
                    try {
                        const jsonStr = render_queue.list_video_files_in_folder(
                            url.toString(),
                            JSON.stringify(fileDialog.extensions)
                        );
                        const more = JSON.parse(jsonStr);
                        for (const v of more) videoUrls.push(v);
                    } catch (e) {
                        console.log("list_video_files_in_folder failed:", e);
                    }
                } else {
                    videoUrls.push(url);
                }
            }
            if (!videoUrls.length) return;
            const firstVideoUrl = render_queue.first_renderable_video_file(
                JSON.stringify(videoUrls.map(u => u.toString())),
                JSON.stringify(fileDialog.extensions)
            );
            if (!firstVideoUrl) {
                add("", videoUrls);
            } else {
                // [queue-batch-streamline T4] 使用 Export 设置的默认路径，跳过弹窗
                let outFolder = "";
                if (window.exportSettings && window.exportSettings.queueOutputMode === 1) {
                    const fixedPath = window.exportSettings.queueFixedOutputPath;
                    if (fixedPath) {
                        outFolder = fixedPath;
                    } else {
                        window.outputFile.selectFolder("", function(folder_url) {
                            if (window.exportSettings) {
                                window.exportSettings.queueFixedOutputPath = folder_url;
                            }
                            add(folder_url, videoUrls);
                        });
                        return;
                    }
                }
                add(outFolder, videoUrls);
            }
        }
    }

    LinkButton {
        visible: !isMobile;
        anchors.left: parent.left;
        anchors.bottom: parent.bottom;
        anchors.margins: 5 * dpiScale;
        leftPadding: 5 * dpiScale; rightPadding: 5 * dpiScale;
        property int currentOption: 0;
        property var options: [
            QT_TRANSLATE_NOOP("Popup", "Do nothing"),
            QT_TRANSLATE_NOOP("Popup", "Shut down the computer"),
            QT_TRANSLATE_NOOP("Popup", "Restart the computer"),
            QT_TRANSLATE_NOOP("Popup", "Sleep"),
            QT_TRANSLATE_NOOP("Popup", "Hibernate"),
            QT_TRANSLATE_NOOP("Popup", "Logout"),
            QT_TRANSLATE_NOOP("Popup", "Close Gyroflow")
        ];
        text: qsTr("When rendering is finished: %1").arg(qsTranslate("Popup", options[currentOption])).trim();
        onClicked: if (p0.visible) { p0.close(); } else { p0.open(); }
        onCurrentOptionChanged: render_queue.when_done = currentOption;
        Popup {
            id: p0;
            model: parent.options;
            currentIndex: parent.currentOption;
            width: maxItemWidth + 10 * dpiScale;
            x: parent.width - width;
            y: itemHeight;
            itemHeight: 25 * dpiScale;
            font.pixelSize: 11 * dpiScale;
            onClicked: i => parent.currentOption = i;
        }
    }
    LinkButton {
        id: queueSettings;
        anchors.right: parent.right;
        anchors.bottom: parent.bottom;
        anchors.margins: 5 * dpiScale;
        leftPadding: 5 * dpiScale; rightPadding: 5 * dpiScale;
        visible: !window.isSimpleMode;
        text: qsTr("Queue settings");
        onClicked: if (queueSettingsMenu.visible) { queueSettingsMenu.dismiss(); } else { queueSettingsMenu.popup(queueSettings, 0, height); }

        function setParallelRenders(v: int, menuItem: Menu): void {
            v = Math.min(6, Math.max(v, 1));

            render_queue.parallel_renders = v;
            // [parallel-default-3] Bumped default from 1 to 3; use a new setting key
            // so legacy stored values don't override the new default on upgrade
            settings.setValue("parallelRenders_v2", v);

            if (!menuItem || typeof menuItem.count !== "number") return;
            for (let i = 0; i < menuItem.count; ++i) {
                const item = menuItem.itemAt(i);
                const action = menuItem.actionAt(i);
                if (item && action && item instanceof QQC.MenuItem) {
                    action.checked = i == v - 1;
                }
            }
        }
        function setOverwriteAction(v: int, menuItem: Menu): void {
            v = Math.min(3, Math.max(v, 0));

            render_queue.overwrite_mode = v;
            settings.setValue("defaultOverwriteAction", v);

            if (!menuItem || typeof menuItem.count !== "number") return;
            for (let i = 0, j = 0; i < menuItem.count; ++i) {
                const item = menuItem.itemAt(i);
                const action = menuItem.actionAt(i);
                if (item && action && item instanceof QQC.MenuItem) {
                    action.checked = j == v;
                    j++;
                }
            }
        }
        function setExportMode(v: int, menuItem: Menu): void {
            v = Math.min(4, Math.max(v, 0));

            render_queue.export_project = v;
            settings.setValue("exportMode", v);

            if (!menuItem || typeof menuItem.count !== "number") return;
            for (let i = 0; i < menuItem.count; ++i) {
                const item = menuItem.itemAt(i);
                const action = menuItem.actionAt(i);
                if (item && action && item instanceof QQC.MenuItem) {
                    action.checked = i == v;
                }
            }
        }

        Menu {
            id: queueSettingsMenu;
            Menu {
                id: parallelRendersMenu;
                title: qsTr("Number of parallel renders");
                Action { text: "1"; onTriggered: queueSettings.setParallelRenders(1, parallelRendersMenu);  }
                Action { text: "2"; onTriggered: queueSettings.setParallelRenders(2, parallelRendersMenu);  }
                Action { text: "3"; onTriggered: queueSettings.setParallelRenders(3, parallelRendersMenu);  }
                Action { text: "4"; onTriggered: queueSettings.setParallelRenders(4, parallelRendersMenu);  }
                Action { text: "5"; onTriggered: queueSettings.setParallelRenders(5, parallelRendersMenu);  }
                Action { text: "6"; onTriggered: queueSettings.setParallelRenders(6, parallelRendersMenu);  }
                Component.onCompleted: queueSettings.setParallelRenders(+settings.value("parallelRenders_v2", 3), parallelRendersMenu);
            }
            Menu {
                id: overwriteActionMenu;
                title: qsTr("Default overwrite action");
                Action { text: qsTr("Ask");            onTriggered: queueSettings.setOverwriteAction(0, overwriteActionMenu); }
                QQC.MenuSeparator { verticalPadding: 5 * dpiScale; }
                Action { text: qsTr("Overwrite file"); onTriggered: queueSettings.setOverwriteAction(1, overwriteActionMenu); }
                Action { text: qsTr("Rename file");    onTriggered: queueSettings.setOverwriteAction(2, overwriteActionMenu); }
                Action { text: qsTr("Skip file");      onTriggered: queueSettings.setOverwriteAction(3, overwriteActionMenu); }
                // Simple mode always uses silent overwrite (1) and must NOT read the Full-mode
                // QSettings "defaultOverwriteAction" — otherwise the RenderQueue loader
                // overwrites App.qml's Component.onCompleted setting and the "file exists"
                // inline prompt reappears for Simple users.
                Component.onCompleted: {
                    if (window && window.isSimpleMode) {
                        render_queue.overwrite_mode = 1;
                        for (let i = 0; i < overwriteActionMenu.count; ++i) {
                            const action = overwriteActionMenu.actionAt(i);
                            if (action) action.checked = false;
                        }
                    } else {
                        queueSettings.setOverwriteAction(+settings.value("defaultOverwriteAction", 0), overwriteActionMenu);
                    }
                }
            }
            Menu {
                id: exportModeMenu;
                title: qsTr("Export mode");
                Action { text: qsTr("Stabilized video");                               onTriggered: queueSettings.setExportMode(0, exportModeMenu); }
                Action { text: qsTr("Project file");                                   onTriggered: queueSettings.setExportMode(1, exportModeMenu); }
                Action { text: qsTr("Project file (including gyro data)");             onTriggered: queueSettings.setExportMode(2, exportModeMenu); }
                Action { text: qsTr("Project file (including processed gyro data)");   onTriggered: queueSettings.setExportMode(3, exportModeMenu); }
                Action { text: qsTr("Stabilized video + Project file with gyro data"); onTriggered: queueSettings.setExportMode(4, exportModeMenu); }
                Component.onCompleted: queueSettings.setExportMode(+settings.value("exportMode", 0), exportModeMenu);
            }
            QQC.MenuSeparator { verticalPadding: 5 * dpiScale; }
            Action { checked: settings.value("showQueueWhenAdding", true); text: qsTr("Show queue when adding an item"); onTriggered: { checked = !checked; settings.setValue("showQueueWhenAdding", checked); } }
            Action { text: qsTr("Clear render queue"); onTriggered: {
                messageBox(Modal.Warning, qsTr("Are you sure you want to remove all items from the render queue?"), [
                    { text: qsTr("Yes"), clicked: function() {
                        render_queue.clear();
                        // [queue-lifecycle T5] 清空队列时同时清空陀螺仪和 match 警告
                        render_queue.clear_gyro_files();
                        root.matchWarning = "";
                        root.pendingConvertFormatChoice = "";
                    }},
                    { text: qsTr("No"), accent: true },
                ]);
            } }
        }
    }

    // [T22] 匹配延迟 Timer，放在 root 级别避免 Button 嵌套问题
    Timer {
        id: matchTimer;
        interval: 100;
        onTriggered: {
            render_queue.auto_rotate = window.batchState ? window.batchState.autoRotate : false;
            render_queue.batch_match_gyro();
        }
    }

    // R3D sequential loader: loads R3D files one at a time to avoid REDline SDK concurrent crash
    QtObject {
        id: r3dSeqLoader;
        property var queue: []
        property string additional: ""
        property bool waiting: false
        function startSequential(urls: list<url>, additionalData: string): void {
            queue = [...urls];
            additional = additionalData;
            waiting = false;
            loadNext();
        }
        function loadNext(): void {
            if (queue.length === 0) {
                // Filename sort happens in onAdded for each R3D added.
                return;
            }
            waiting = true;
            const url = queue.shift();
            const job_id = render_queue.add_file(url.toString(), "", additional);
            loader.pendingJobs[job_id] = true;
            loader.updateStatus();
        }
    }

    LoaderOverlay {
        id: loader;
        active: false;
        property var pendingJobs: ({});
        function updateStatus(): void { active = Object.keys(pendingJobs).length > 0; }
    }
}
