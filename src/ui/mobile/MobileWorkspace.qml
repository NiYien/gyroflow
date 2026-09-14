// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Window
import QtQuick.Controls as QQC
import "MobileLogic.js" as Logic

Rectangle {
    id: root
    objectName: "mobileWorkspace"
    property var host: null
    property string platformOs: Qt.platform.os
    property var backend: null
    property var filesystemService: null
    property var queueService: host && host.videoArea ? host.videoArea.queue : null
    property real unit: 1
    property bool dark: true
    property color accentColor: MobileStyle.accent(dark)
    readonly property color textColor: MobileStyle.text(dark)
    readonly property color mutedColor: MobileStyle.secondary(dark)
    readonly property color surfaceColor: MobileStyle.surface(dark)
    readonly property bool landscape: width > height
    readonly property int columns: landscape ? Math.max(1, Math.floor((width - 24 * unit) / (280 * unit))) : 1
    readonly property real headerHeight: (landscape ? 48 : 56) * unit
    readonly property real gyroBarHeight: gyroRecords.length ? gyroData.implicitHeight : 0
    readonly property var gyroRecords: queueService && queueService.gyroFilesInfo ? queueService.gyroFilesInfo : []
    readonly property bool showLibraryActions: page === "library" && rows.length > 0 && !busy && !footerSelectionMode && (!summary || continueAfterSummary)
    readonly property bool wideFooter: showLibraryActions && !summary && landscape && width >= 640 * unit
    readonly property bool continueAfterSummary: !busy && !!summary && !!operation && ((operation.kind === "deep" && deepSucceeded) || (operation.kind === "sync" && taskCounts.success > 0))
    readonly property bool footerSelectionMode: selectionMode && !continueAfterSummary
    readonly property real footerHeight: busy ? Math.max(taskStatus.implicitHeight, taskStatusAction.height) + 44 * unit : !rows.length && !summary ? 0 : Math.max((landscape ? 56 : 64) * unit, footerActions.implicitHeight + (continueAfterSummary ? 108 : footerSelectionMode && !summary ? 64 : showLibraryActions && !wideFooter ? 80 : landscape ? 8 : 16) * unit)
    readonly property real panelAvailableHeight: {
        const keyboard = Qt.inputMethod.keyboardRectangle;
        if (!Qt.inputMethod.visible || keyboard.height <= 0) return height;
        return Math.max(0, Math.min(height, mapFromItem(null, keyboard.x, keyboard.y).y));
    }
    property string page: "library"
    property string panel: ""
    property var panelTrail: []
    property int importTab: 0
    property bool browseFilesInApp: false
    property int settingsTab: 0
    property var pendingFolderImport: null
    property int previewJobId: 0
    property int detailJobId: 0
    property bool selectionMode: false
    property var selection: ({})
    readonly property int selectionCount: Object.keys(selection).length
    property var rows: []
    property var snapshot: ({})
    property string lastSnapshot: ""
    property var operation: null
    property int operationSequence: 0
    property bool engineBusy: false
    readonly property bool importing: !!(queueService && queueService.importBusy)
    readonly property bool busy: importing || engineBusy || !!(operation && operation.active)
    property string summary: ""
    property string deepStage: ""
    property bool deepSucceeded: false
    property int deepJobId: 0
    property bool dragSelecting: false
    property int dragStart: -1
    property var dragBase: ({})
    property bool dragAdd: true
    property real dragX: 0
    property real dragY: 0
    property bool controlsShown: true
    property bool stablePreview: false
    property bool previewReady: false
    property bool previewRequested: false
    property var videoInfo: ({})
    readonly property var videoInfoKeys: Object.keys(videoInfo).filter(key => key !== "File name")
    property string lastTaskMessage: ""
    property string notice: ""
    property string pendingOpenUrl: ""
    property real savedContentY: 0
    property int savedAnchorId: 0
    property real savedAnchorOffset: 0
    property bool layoutRestoring: false
    readonly property var previewRecord: Logic.findRow(rows, previewJobId)
    readonly property var detailRecord: Logic.findRow(rows, detailJobId)
    readonly property var taskCounts: Logic.counts(operation)
    property alias previewHost: previewHost
    property real previewAspectRatio: 16 / 9
    onPreviewReadyChanged: if (previewReady) updatePreviewAspectRatio()
    readonly property real previewAvailableHeight: Math.max(0, height - headerHeight - (busy ? footerHeight : 0) - 24 * unit)
    property alias settingsContent: settingsContent
    property alias panelFlickable: panelScroll
    property alias libraryView: grid
    color: MobileStyle.background(dark)
    focus: visible
    Accessible.name: qsTr("Videos")

    function refresh() {
        if (!backend) return;
        const raw = backend.get_mobile_queue_snapshot();
        const next = Logic.parse(raw, { rows: [] });
        snapshot = next;
        engineBusy = backend.status === "active" || next.workersActive || next.deepActive
            || !!(queueService && (queueService.matching || queueService.pendingAction));
        if (raw !== lastSnapshot) {
            lastSnapshot = raw;
            const nextRows = next.rows || [];
            for (let i = 0; i < nextRows.length; ++i) {
                if (i >= cards.count) cards.append({ record: nextRows[i] });
                else cards.setProperty(i, "record", nextRows[i]);
            }
            if (cards.count > nextRows.length) cards.remove(nextRows.length, cards.count - nextRows.length);
            rows = nextRows;
            selection = Logic.pruneSelection(rows, selection);
            if (selectionMode && !selectionCount && !dragSelecting) selectionMode = false;
        }
        if (operation && operation.active && operation.kind !== "deep") {
            if (engineBusy && !operation.started) operation = Object.assign({}, operation, { started: true });
            operation = Logic.observeOperation(operation, rows, false);
            if (operation.started && !engineBusy && !(host && host.isDialogOpened)) finishOperation();
            else if (!operation.started && !engineBusy && !(host && host.isDialogOpened) && !settleRequest.running) settleRequest.start();
        }
        if (pendingOpenUrl) {
            const row = rows.find(r => r.url === pendingOpenUrl);
            if (row && row.previewable) { pendingOpenUrl = ""; openPreview(row.id); }
        }
    }
    function rowStatus(row) {
        let label = qsTr("Waiting");
        if (row.status === "Rendering") label = row.processing > 0 && row.processing < 1
            ? qsTr("Stabilizing %1%").arg(Math.round(row.processing * 100))
            : row.frames > 0 && row.frame > 0 ? qsTr("Exporting %1%").arg(Math.round(row.frame / row.frames * 100))
            : operation && operation.kind === "sync" ? qsTr("Stabilizing") : qsTr("Preparing…");
        else if (row.status === "Error") label = qsTr("Needs attention");
        else if (row.status === "Skipped") label = skipLabel(row);
        else if (row.status === "Finished") {
            const sync = Logic.parse(row.sync, {});
            label = row.lastExport === 4 || row.lastExport === 0 ? qsTr("Exported")
                : row.lastExport === 2 ? (sync.color === "yellow" ? qsTr("Sync not confirmed") : qsTr("Stabilized"))
                : row.deepMatched ? qsTr("Deep search complete") : qsTr("Ready");
        } else if (row.deepMatched) label = qsTr("Deep search complete");
        return label;
    }
    function lensLabel(row) {
        if (row.manualLens) return row.lensGroup ? "L" + row.lensGroup : qsTranslate("RenderQueue", "Pending match");
        return row.focalLength > 0 ? Number(row.focalLength).toFixed(1).replace(/\.0$/, "") + " mm" : qsTr("Focal length unknown");
    }
    function rowColor(row) {
        if (row.status === "Error") return dark ? "#ffb4ab" : "#a92b28";
        if (row.status === "Skipped") return mutedColor;
        const sync = Logic.parse(row.sync, {});
        const exported = row.lastExport === 4 || row.lastExport === 0;
        if (!exported && (sync.color === "yellow" || sync.color === "done_pending")) return dark ? "#e5bd72" : "#805610";
        if (row.deepMatched && !exported && row.lastExport !== 2) return dark ? "#cab4ef" : "#71509d";
        if (row.status === "Finished" && (exported || row.lastExport === 2)) return dark ? "#8fd6a3" : "#236b3c";
        return mutedColor;
    }
    function rowProgress(row) {
        if (row.status !== "Rendering") return -1;
        return row.processing > 0 && row.processing < 1 ? row.processing : row.frames > 0 ? row.frame / row.frames : -1;
    }
    function skipLabel(row) {
        if (row.skipReason === "user_stopped") return qsTr("Stopped manually");
        if (row.skipReason === "no_gyro") return qsTr("No gyroscope data");
        if (row.skipReason === "calibration") return qsTr("Calibration pair");
        if (row.skipReason === "plugin_only") return qsTr("Plugin stabilization only");
        if (row.skipReason === "image_stabilization") return qsTr("In-camera stabilization on");
        if (row.error) return host ? host.getReadableError(row.error) : row.error;
        return qsTr("Skip reason not recorded");
    }
    function skipMessage(reason) {
        if (!reason) return "";
        if (reason === "user_stopped") return qsTr("Stopped by you. Retry starts this video from the beginning.");
        if (reason === "no_gyro") return qsTr("No gyroscope data. Add a recording or try Deep search.");
        if (reason === "plugin_only") return qsTr("This format supports stabilization for editing plugins only.");
        if (reason === "calibration") return qsTranslate("RenderQueue", "Skipped - calibration pair");
        if (reason === "image_stabilization") return qsTranslate("App", "In-camera stabilization was on when these videos were recorded, so they cannot be stabilized. Turn off stabilization in the camera and on the lens, then record again.");
        return qsTr("Skip reason not recorded");
    }
    function notify(text) { notice = text; noticeTimer.restart(); }
    function toggleSelection(id) {
        const copy = Object.assign({}, selection);
        if (copy[id]) delete copy[id]; else copy[id] = true;
        selection = copy;
        selectionMode = Object.keys(copy).length > 0;
    }
    function selectAll() {
        if (selectionCount === rows.length) { selection = {}; return; }
        const all = {}; rows.forEach(row => { all[row.id] = true; }); selection = all;
    }
    function finishSelection() { selectionMode = false; selection = {}; endDrag(); }
    function beginDrag(index) {
        selectionMode = true; dragSelecting = true;
        dragStart = index; dragBase = Object.assign({}, selection);
        dragAdd = !selection[rows[index].id];
        selection = Logic.selectionRange(rows, dragBase, index, index, dragAdd);
    }
    function updateDrag(px, py) {
        dragX = px; dragY = py;
        const index = grid.indexAt(px, py + grid.contentY);
        if (index >= 0) selection = Logic.selectionRange(rows, dragBase, dragStart, index, dragAdd);
    }
    function endDrag() { dragSelecting = false; dragStart = -1; if (!selectionCount) selectionMode = false; }
    function rememberList() {
        savedContentY = grid.contentY;
        const index = Math.max(0, grid.indexAt(4 * unit, grid.contentY + 4 * unit));
        savedAnchorId = rows[index] ? rows[index].id : 0;
        savedAnchorOffset = grid.contentY - Math.floor(index / columns) * grid.cellHeight;
    }
    function restoreList() {
        const index = rows.findIndex(row => row.id === savedAnchorId);
        const desired = index >= 0 ? Math.floor(index / columns) * grid.cellHeight + savedAnchorOffset : savedContentY;
        grid.contentY = Math.max(0, Math.min(desired, Math.max(0, grid.contentHeight - grid.height)));
    }
    function resizeFinished() { restoreList(); layoutRestoring = false; }
    function openPreview(id, informationOnly) {
        const row = Logic.findRow(rows, id);
        if (!row) return;
        if (!row.previewable) {
            if (informationOnly) {
                if (host) host.videoArea.vid.pause();
                previewJobId = id; previewReady = false; previewRequested = false;
                videoInfo = { "File name": row.filename, "Duration": Logic.duration(row.duration) };
                if (row.focalLength > 0) videoInfo["Focal length"] = row.focalLength + " mm";
                showPanel("info"); return;
            }
            notify(qsTr("This video is being processed. Preview will be available when it finishes.")); return;
        }
        if (host && (host.videoArea.queueEditLoading || host.controller.video_loading_in_progress)) return;
        if (page === "library") rememberList();
        previewJobId = id; previewReady = false; previewRequested = true; videoInfo = {};
        stablePreview = false; controlsShown = true; panel = "";
        if (!informationOnly) page = "preview";
        else showPanel("info");
        if (host) host.openMobilePreview(id);
    }
    function openInfo(id) {
        if (previewJobId === id && previewReady) showPanel("info");
        else openPreview(id, true);
    }
    function previewLoaded() {
        if (!previewRequested || !host || host.videoArea.queueEditLoading || host.controller.video_loading_in_progress
                || host.controller.loading_gyro_in_progress || backend.editing_job_id !== previewJobId) return;
        if (!host.videoArea.vid.loaded) return;
        videoInfo = Object.assign({}, host.vidInfo ? host.vidInfo.infoList.model : {});
        if (page !== "preview") { host.videoArea.vid.pause(); previewRequested = false; previewReady = true; return; }
        stablePreview = host.controller.gyro_loaded && !host.controller.stabilize_step_pending_for_preview();
        host.setMobileComparison(stablePreview);
        updatePreviewAspectRatio();
        previewRequested = false;
        previewReady = true;
        host.videoArea.vid.play();
        hideControls.restart();
    }
    function updatePreviewAspectRatio() {
        const ratio = host && host.videoArea ? host.videoArea.mobilePreviewAspectRatio : 0;
        if (isFinite(ratio) && ratio > 0) previewAspectRatio = ratio;
    }
    function returnToList() {
        if (host) host.videoArea.vid.pause();
        panel = ""; page = "library"; controlsShown = true;
        Qt.callLater(restoreList);
    }
    function navigate(direction) {
        const index = rows.findIndex(row => row.id === previewJobId);
        if (index + direction >= 0 && index + direction < rows.length) openPreview(rows[index + direction].id);
    }
    function showPanel(name) {
        if (name === "sources") { importTab = 1; name = "add"; }
        else if (name === "add") importTab = 0;
        if (panel && panel !== name) panelTrail = panelTrail.concat([{ name: panel, contentY: panelScroll.contentY }]);
        panelScroll.contentY = 0;
        panel = name; controlsShown = true;
        if (name === "info" && host) host.videoArea.vid.pause();
    }
    function closePanel() {
        if (!panelTrail.length) { panel = ""; return; }
        const history = panelTrail.slice();
        const previous = history.pop();
        panelTrail = history; panel = previous.name;
        Qt.callLater(() => { panelScroll.contentY = previous.contentY; });
    }
    onPanelChanged: if (!panel) { panelTrail = []; pendingFolderImport = null; }
    function dismissPanel() {
        if (panel === "add" || panel === "folders" || panel === "files") returnToList();
        else panel = "";
    }
    function back() {
        if (Qt.inputMethod.visible) { Qt.inputMethod.hide(); return true; }
        if (panel === "add" || panel === "folders" || panel === "files") { dismissPanel(); return true; }
        if (panel) { closePanel(); return true; }
        if (page === "preview") { returnToList(); return true; }
        if (selectionMode) { finishSelection(); return true; }
        return false;
    }
    function inputsAllowed() {
        if (!busy) return true;
        notify(importing ? qsTr("Reading…") : qsTr("Stop the current task before changing its inputs."));
        return false;
    }
    function requestAdd(folder) {
        if (!inputsAllowed() || !queueService) return;
        if (folder) requestFolder("video");
        else if (browseFilesInApp) { showPanel("files"); folderPicker.start("video", true); }
        else { panel = ""; queueService.requestMobileFiles(); }
    }
    function requestPhotos() {
        if (platformOs !== "ios" || !inputsAllowed() || !queueService) return;
        dismissPanel();
        queueService.requestMobilePhotos();
    }
    function requestGyro() {
        if (!inputsAllowed() || !queueService) return;
        if (browseFilesInApp) { showPanel("files"); folderPicker.start("gyro", true); return; }
        panel = "";
        queueService.requestMobileGyroFiles();
    }
    function requestFolder(kind) {
        if (!inputsAllowed() || !queueService) return;
        requestNativeFolder(kind);
    }
    function requestNativeFolder(kind) {
        if (!inputsAllowed() || !queueService) return;
        dismissPanel();
        queueService.requestMobileFolderLocation(function(url) {
            if (!root.inputsAllowed() || !root.queueService) return;
            root.pendingFolderImport = { url: url, kind: kind };
            root.panelTrail = [];
            root.showPanel("folderConfirm");
        }, true);
    }
    function confirmFolderImport() {
        if (!pendingFolderImport || !inputsAllowed() || !queueService) return;
        const picked = pendingFolderImport;
        pendingFolderImport = null;
        dismissPanel();
        if (picked.kind === "gyro") queueService.addMobileGyroUrls([picked.url]);
        else queueService.dt.loadFiles([picked.url]);
    }
    function removeSelected() {
        if (!inputsAllowed()) return;
        Object.keys(selection).forEach(id => backend.remove(Number(id)));
        finishSelection(); refresh();
    }
    function startAction(kind) {
        if (busy || !rows.length) return;
        panel = ""; summaryTimer.stop(); summary = "";
        operation = Logic.beginOperation(++operationSequence, kind, rows);
        if (host) {
            host.flushMobileSettings();
            if (kind === "sync") host.runPluginStabilizeFlow(); else host.runStabilizedBatchExport();
        }
        Qt.callLater(refresh);
    }
    function finishOperation() {
        const started = operation && operation.started;
        operation = Logic.observeOperation(operation, rows, true);
        if (!started) { operation = null; return; }
        const counts = Logic.counts(operation);
        summary = operation.stopping ? qsTr("Stopped · %1 completed").arg(counts.success)
            : counts.failed || counts.skipped ? qsTr("%1 completed · %2 need attention").arg(counts.success).arg(counts.failed + counts.skipped)
            : qsTr("%1 videos completed").arg(counts.success);
        summaryTimer.restart();
        lastTaskMessage = summary;
        if (page === "preview") controlsShown = true;
    }
    function deepStarted(id) {
        operation = Logic.beginOperation(++operationSequence, "deep", rows.filter(row => row.id === id));
        operation = Object.assign({}, operation, { started: true });
        deepJobId = id; deepSucceeded = false; deepStage = qsTr("Preparing search…");
        panel = ""; summary = ""; refresh();
    }
    function startDeep(id) {
        if (!inputsAllowed() || !queueService) return;
        const row = Logic.findRow(rows, id);
        if (row) queueService.maybeStartDeepMatch(id, -1, row.filename);
    }
    function deepFinished(id, success, errorKind, offset) {
        if (id !== deepJobId) return;
        if (!operation || operation.kind !== "deep" || !operation.active) return;
        operation = Object.assign({}, operation, { active: false, results: { [id]: success ? "success" : errorKind === "cancelled" ? "cancelled" : "failed" } });
        deepSucceeded = success;
        if (success) {
            summary = qsTr("Deep search complete");
            finishSelection();
            panel = "deepResult";
        } else if (errorKind !== "cancelled") {
            summary = errorKind === "low_motion" || errorKind === "not_in_range"
                ? qsTr("No match found. Try a video with more camera motion, and check the gyro recording, in-camera stabilization and mounting position.")
                : errorKind === "video_open_failed" ? qsTranslate("RenderQueue", "Unable to open the video for deep matching. Please select the video again.")
                : errorKind === "video_decode_failed" || errorKind === "video_frame_conversion_failed" || errorKind === "video_no_frames"
                    ? qsTranslate("RenderQueue", "Unable to decode the video for deep matching. Please try another video.")
                : errorKind === "probe_not_run" ? qsTranslate("RenderQueue", "Deep match could not run.")
                : qsTranslate("RenderQueue", "Failed to load the gyro file.");
            panel = "task";
        } else { summary = qsTr("Search cancelled"); summaryTimer.restart(); }
        lastTaskMessage = summary;
        refresh();
    }
    function showTaskDetails(id) { detailJobId = id || 0; showPanel("task"); }
    function revealInput() {
        if (!panel || !root.Window.window) return;
        const focused = root.Window.window.activeFocusItem;
        if (!focused || !focused.visible) return;
        let ancestor = focused.parent;
        while (ancestor && ancestor !== panelScroll) ancestor = ancestor.parent;
        if (!ancestor) return;
        const bottom = focused.mapToItem(panelScroll, 0, focused.height).y;
        if (bottom > panelScroll.height - 16 * unit)
            panelScroll.contentY = Math.min(Math.max(0, panelScroll.contentHeight - panelScroll.height), panelScroll.contentY + bottom - panelScroll.height + 16 * unit);
    }
    function retryJob(id) {
        if (!inputsAllowed()) return;
        const row = Logic.findRow(rows, id);
        if (!row || row.skipReason !== "user_stopped") return;
        operation = Logic.beginOperation(++operationSequence, backend.export_project === 2 ? "sync" : "export", [row]);
        backend.reset_job(id);
        backend.render_job(id);
        refresh();
    }
    function stopTask() {
        if (!operation && !engineBusy) return;
        if (operation) operation = Object.assign({}, operation, { stopping: true });
        if (operation && operation.kind === "deep") backend.cancel_deep_gyro_match(deepJobId);
        else {
            if (queueService && queueService.matching) {
                queueService.pendingAction = "";
                host.controller.cancel_current_operation();
            }
            backend.stop();
        }
        Qt.callLater(refresh);
    }
    component OperationProgress: QQC.ProgressBar {
        id: progress
        height: 6 * root.unit
        padding: 0
        indeterminate: !root.operation || root.operation.kind === "deep" || !root.operation.started || (root.queueService && root.queueService.matching)
        from: 0; to: Math.max(1, root.taskCounts.total)
        value: root.taskCounts.settled
        background: Rectangle { color: root.dark ? "#373d47" : "#dce1e8"; radius: 3 * root.unit }
        contentItem: Item {
            clip: true
            Rectangle {
                id: progressFill
                property real phase: 0
                x: progress.indeterminate ? phase * (parent.width + width) - width : 0
                width: progress.indeterminate ? parent.width * 0.35 : parent.width * progress.position
                height: parent.height; radius: 3 * root.unit; color: root.accentColor
                NumberAnimation on phase {
                    from: 0; to: 1; duration: 1400; loops: Animation.Infinite
                    running: progress.visible && progress.indeterminate
                }
            }
        }
    }
    component JobActions: Column {
        required property var record
        width: parent.width; spacing: 0
        MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: root.rowStatus(parent.record); color: root.rowColor(parent.record); wrapMode: Text.WordWrap }
        MobileText { unit: root.unit; dark: root.dark; width: parent.width; topPadding: 6 * root.unit; visible: text.length > 0 && text !== root.rowStatus(parent.record); text: parent.record.error ? (root.host ? root.host.getReadableError(parent.record.error) : parent.record.error) : root.skipMessage(parent.record.skipReason); secondary: true; wrapMode: Text.WordWrap }
        MobileActionRow { visible: parent.record.status === "Rendering"; width: parent.width; unit: root.unit; dark: root.dark; destructive: true; iconName: "pause"; text: qsTr("Stop this video"); onClicked: root.backend.stop_job(parent.record.id) }
        MobileActionRow { visible: parent.record.skipReason === "user_stopped"; width: parent.width; unit: root.unit; dark: root.dark; text: qsTr("Retry"); iconName: "reset"; enabled: !root.busy; onClicked: root.retryJob(parent.record.id) }
        MobileActionRow { visible: parent.record.skipReason === "no_gyro"; width: parent.width; unit: root.unit; dark: root.dark; text: qsTr("Add gyroscope data"); iconName: "plus"; enabled: !root.busy; onClicked: root.requestGyro() }
        MobileActionRow { visible: parent.record.status === "Finished" && (parent.record.lastExport === 4 || parent.record.lastExport === 0) && root.platformOs !== "ios"; width: parent.width; unit: root.unit; dark: root.dark; navigation: true; iconName: "play"; text: qsTranslate("RenderQueue", "Open rendered file"); onClicked: if (root.host) root.host.openMobileOutput(parent.record.id) }
        MobileActionRow { visible: !!parent.record.paired; width: parent.width; unit: root.unit; dark: root.dark; iconName: "reset"; text: qsTranslate("RenderQueue", "Unpair gyro"); enabled: !root.busy; onClicked: { if (root.inputsAllowed()) { root.backend.unpair_video(parent.record.id); root.refresh(); } } }
    }
    function taskTitle() {
        if (importing) return qsTr("Add media");
        if (operation && operation.stopping) return qsTr("Stopping…");
        if (operation && operation.kind === "deep") return qsTr("Deep search · %1").arg((Logic.findRow(rows, deepJobId) || {}).filename || "");
        if (queueService && queueService.matching) return qsTr("Matching videos…");
        return !operation ? qsTr("Preparing…") : operation.kind === "sync" ? qsTr("Stabilizing") : qsTr("Exporting");
    }
    onLandscapeChanged: { endDrag(); controlsShown = true; }
    onWidthChanged: { endDrag(); layoutRestoring = true; Qt.callLater(resizeFinished); }
    onHeightChanged: { endDrag(); layoutRestoring = true; Qt.callLater(resizeFinished); }
    onPanelAvailableHeightChanged: Qt.callLater(revealInput)
    Component.onCompleted: { refresh(); if (host) Qt.callLater(host.attachMobileWorkspace); }

    Timer { interval: 300; running: root.visible; repeat: true; onTriggered: root.refresh() }
    Timer {
        id: settleRequest; interval: 350
        onTriggered: {
            if (root.operation && root.operation.active && !root.operation.started && !root.engineBusy && !(root.host && root.host.isDialogOpened))
                root.operation = null;
        }
    }
    Timer { id: summaryTimer; interval: 4500; onTriggered: root.summary = "" }
    Timer { id: noticeTimer; interval: 4500; onTriggered: root.notice = "" }
    Timer {
        id: hideControls; interval: 3000
        onTriggered: if (root.landscape && !root.panel && !seek.pressed && root.host && root.host.videoArea.vid.playing) root.controlsShown = false
    }
    Timer {
        interval: 40; repeat: true; running: root.dragSelecting
        onTriggered: {
            const margin = 40 * root.unit;
            const delta = root.dragY < margin ? -12 * root.unit : root.dragY > grid.height - margin ? 12 * root.unit : 0;
            if (delta) {
                grid.contentY = Math.max(0, Math.min(grid.contentY + delta, Math.max(0, grid.contentHeight - grid.height)));
                root.updateDrag(root.dragX, root.dragY);
            }
        }
    }
    Connections {
        target: root.backend
        function onQueue_changed() { root.refresh(); }
        function onRender_progress(job_id, progress, current_frame, total_frames, finished, start_time, is_conversion) {
            if (root.operation && root.operation.kind !== "deep") root.operation = Logic.touchOperation(root.operation, job_id);
        }
        function onQueue_finished() { Qt.callLater(root.refresh); }
    }
    Connections {
        target: root.host ? root.host.videoArea : null
        function onQueueEditLoadingChanged() { if (!root.host.videoArea.queueEditLoading) Qt.callLater(root.previewLoaded); }
        function onMobilePreviewAspectRatioChanged() {
            if (root.previewReady && !root.previewRequested) root.updatePreviewAspectRatio();
        }
    }
    Connections {
        target: root.host ? root.host.controller : null
        function onVideo_loading_in_progressChanged() { if (!root.host.controller.video_loading_in_progress) Qt.callLater(root.previewLoaded); }
        function onLoading_gyro_in_progressChanged() { if (!root.host.controller.loading_gyro_in_progress) Qt.callLater(root.previewLoaded); }
    }
    Connections {
        target: Qt.inputMethod
        function onCursorRectangleChanged() { Qt.callLater(root.revealInput); }
    }

    Item {
        id: previewHost
        objectName: "mobilePreviewHost"
        visible: root.page === "preview"
        x: 0; y: root.landscape ? 0 : root.headerHeight + 12 * root.unit + Math.max(0, (root.previewAvailableHeight - height - playbackControls.height - 8 * root.unit) / 2)
        width: parent.width
        height: root.landscape ? parent.height : Math.max(0, Math.min((width - 20 * root.unit) / root.previewAspectRatio + 20 * root.unit,
            root.previewAvailableHeight - playbackControls.height - 8 * root.unit))
    }
    MouseArea {
        visible: root.page === "preview"
        anchors.fill: previewHost
        onClicked: { root.controlsShown = !root.controlsShown; if (root.controlsShown) hideControls.restart(); }
    }
    Rectangle {
        id: header
        width: parent.width; height: root.headerHeight
        visible: root.page === "library" || root.controlsShown || !root.landscape
        color: root.page === "preview" && root.landscape ? (root.dark ? "#ef1c1c1e" : "#eff2f2f7") : root.color
        Row {
            id: headerRow
            anchors.left: parent.left; anchors.leftMargin: 8 * root.unit
            anchors.right: parent.right; anchors.rightMargin: 8 * root.unit
            anchors.verticalCenter: parent.verticalCenter
            spacing: 4 * root.unit
            MobileButton {
                id: previewBackButton
                visible: root.page === "preview"
                unit: root.unit; dark: root.dark; quiet: true
                text: qsTr("‹ Videos"); iconOnly: true; iconName: "back"
                onClicked: root.returnToList()
            }
            MobileText { unit: root.unit; dark: root.dark;
                width: Math.max(0, headerRow.width - [previewBackButton, addButton, infoButton, settingsButton].reduce((sum, button) => sum + (button.visible ? button.width + headerRow.spacing : 0), 0))
                height: 48 * root.unit; verticalAlignment: Text.AlignVCenter
                objectName: "mobilePageTitle"
                text: root.page === "preview" ? (root.previewRecord ? root.previewRecord.filename : "") : qsTr("Videos")
                heading: root.page === "library"
                font.weight: Font.DemiBold; color: root.textColor
                elide: Text.ElideMiddle
            }
            MobileButton {
                id: addButton
                objectName: "mobileAddButton"
                visible: root.page === "library" && root.rows.length > 0
                width: Math.min(implicitWidth, root.width * 0.4)
                unit: root.unit; dark: root.dark; quiet: true; text: qsTr("Add media")
                iconOnly: root.width < 330 * root.unit; iconName: iconOnly ? "plus" : ""
                enabled: !root.busy
                onClicked: root.showPanel("add")
            }
            MobileButton {
                id: infoButton
                visible: root.page === "preview"
                unit: root.unit; dark: root.dark; quiet: true; text: qsTr("Info"); iconOnly: true; iconName: "info"
                onClicked: root.showPanel("info")
            }
            MobileButton {
                id: settingsButton
                objectName: "mobileSettingsButton"
                unit: root.unit; dark: root.dark; quiet: true
                text: qsTr("Settings")
                iconOnly: true; iconName: "settings"
                onClicked: root.showPanel("settings")
            }
        }
    }
    component LibraryActions: Row {
        spacing: 12 * root.unit
        MobileButton {
            objectName: "mobileResetPairing"
            width: (parent.width - parent.spacing) / 2; height: 48 * root.unit
            unit: root.unit; dark: root.dark; text: qsTr("Reset pairing")
            enabled: !root.busy && root.rows.length > 0
            onClicked: if (root.inputsAllowed() && root.host) root.host.resetMobilePairing()
        }
        MobileButton {
            objectName: "mobileClearQueue"
            width: (parent.width - parent.spacing) / 2; height: 48 * root.unit
            unit: root.unit; dark: root.dark; text: qsTr("Clear queue"); destructive: true
            enabled: !root.busy && root.rows.length > 0
            onClicked: if (root.inputsAllowed() && root.host) root.host.clearMobileQueue()
        }
    }
    ListModel { id: cards; dynamicRoles: true }
    MobileGyroBar {
        id: gyroData
        visible: root.page === "library" && root.gyroBarHeight > 0
        x: 16 * root.unit; y: root.headerHeight; width: parent.width - 32 * root.unit; height: root.gyroBarHeight
        unit: root.unit; dark: root.dark; records: root.gyroRecords
        maximumListHeight: Math.min(180 * root.unit, root.height * 0.25)
    }
    GridView {
        id: grid
        objectName: "mobileVideoGrid"
        visible: root.page === "library"
        x: 16 * root.unit; y: root.headerHeight + root.gyroBarHeight + 8 * root.unit
        width: parent.width - 32 * root.unit
        height: parent.height - y - root.footerHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        interactive: !root.dragSelecting
        onContentYChanged: if (!root.layoutRestoring && root.page === "library" && (moving || root.dragSelecting)) root.rememberList()
        onMovementEnded: if (!root.layoutRestoring && root.page === "library") root.rememberList()
        cellWidth: width / root.columns
        cellHeight: 72 * root.unit
        model: cards
        QQC.ScrollIndicator.vertical: QQC.ScrollIndicator {}
        delegate: MobileVideoCard {
            required property int index
            width: grid.cellWidth - (root.columns > 1 ? 8 * root.unit : 0)
            height: grid.cellHeight
            first: index < root.columns
            last: index + root.columns >= cards.count
            unit: root.unit; dark: root.dark; accentColor: root.accentColor
            selected: !!root.selection[record.id]
            selecting: root.selectionMode
            scrolling: grid.moving || grid.dragging
            statusText: root.rowStatus(record); statusColor: root.rowColor(record)
            metadataText: Logic.duration(record.duration || 0) + " · " + root.lensLabel(record)
            progress: root.rowProgress(record)
            onActivated: { if (root.selectionMode) root.toggleSelection(record.id); else root.openPreview(record.id); }
            onPlayRequested: root.openPreview(record.id)
            onInformationRequested: root.openInfo(record.id)
            onSelectionRequested: root.toggleSelection(record.id)
            onHeld: root.beginDrag(index)
            onDragged: (px, py) => { const point = mapToItem(grid, px, py); root.updateDrag(point.x, point.y); }
            onDragEnded: root.endDrag()
        }
    }
    Column {
        visible: root.page === "library" && !root.rows.length
        anchors.centerIn: grid
        width: Math.min(300 * root.unit, grid.width)
        spacing: 16 * root.unit
        MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: qsTr("Your videos, ready to stabilize"); heading: true; wrapMode: Text.WordWrap; horizontalAlignment: Text.AlignHCenter; color: root.textColor }
        MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: qsTr("Add videos to get started."); wrapMode: Text.WordWrap; horizontalAlignment: Text.AlignHCenter; color: root.mutedColor }
        MobileButton { objectName: "mobileEmptyAddButton"; anchors.horizontalCenter: parent.horizontalCenter; unit: root.unit; dark: root.dark; emphasized: true; text: qsTr("Add media"); enabled: !root.busy; onClicked: root.showPanel("add") }
    }
    Rectangle {
        id: playbackControls
        objectName: "mobilePlaybackControls"
        visible: root.page === "preview" && (root.controlsShown || !root.landscape)
        y: root.landscape ? parent.height - height - 12 * root.unit - (root.busy ? root.footerHeight : 0) : previewHost.y + previewHost.height + 8 * root.unit
        width: parent.width
        height: (root.landscape ? 122 : 194) * root.unit
        color: root.landscape ? (root.dark ? "#ef1c1c1e" : "#eff2f2f7") : root.color
        Column {
            x: (parent.width - width) / 2; width: Math.min(parent.width - 32 * root.unit, 640 * root.unit)
            spacing: 8 * root.unit
            Row {
                width: parent.width
                spacing: 8 * root.unit
                Rectangle {
                    objectName: "mobilePreviewComparison"
                    width: parent.width; height: 50 * root.unit
                    color: MobileStyle.fill(root.dark); radius: 10 * root.unit
                    Row {
                        x: 3 * root.unit; y: 3 * root.unit; width: parent.width - 6 * root.unit
                        MobileButton {
                            width: parent.width / 2; height: 44 * root.unit; unit: root.unit; dark: root.dark; segmented: true; checked: !root.stablePreview
                            text: qsTr("Original")
                            onClicked: { root.stablePreview = false; if (root.host) root.host.setMobileComparison(false); }
                        }
                        MobileButton {
                            width: parent.width / 2; height: 44 * root.unit; unit: root.unit; dark: root.dark; segmented: true; checked: root.stablePreview
                            text: qsTr("Stabilized")
                            enabled: root.previewReady && root.host && root.host.controller.gyro_loaded && !root.host.controller.stabilize_step_pending_for_preview()
                            onClicked: { root.stablePreview = true; root.host.setMobileComparison(true); }
                        }
                    }
                }
            }
            Item {
                width: parent.width; height: (root.landscape ? 56 : 128) * root.unit
            MobileSlider {
                id: seek
                unit: root.unit; dark: root.dark
                x: root.landscape ? transportButtons.width + 12 * root.unit : 0
                y: root.landscape ? 4 * root.unit : 0
                width: parent.width - x; height: 48 * root.unit
                from: 0
                to: root.host ? Math.max(1, root.host.videoArea.vid.duration) : 1
                value: !pressed && root.host ? root.host.videoArea.vid.timestamp : 0
                enabled: root.previewReady
                onMoved: if (root.host) root.host.videoArea.vid.seekToTimestamp(value, true)
                onPressedChanged: { if (pressed) hideControls.stop(); else hideControls.restart(); }
                Accessible.name: qsTr("Playback position")
            }
            Row {
                id: transportButtons
                x: root.landscape ? 0 : (parent.width - width) / 2
                y: root.landscape ? 0 : 56 * root.unit
                height: (root.landscape ? 56 : 64) * root.unit
                spacing: (root.landscape ? 8 : 16) * root.unit
                MobileButton { objectName: "mobilePreviousVideo"; width: parent.height; height: parent.height; iconSize: 24 * root.unit; unit: root.unit; dark: root.dark; quiet: true; circular: true; text: qsTr("Previous"); iconOnly: true; iconName: "previous"; enabled: root.rows.findIndex(r => r.id === root.previewJobId) > 0 && root.previewReady; onClicked: root.navigate(-1) }
                MobileButton { objectName: "mobilePlayPause"; width: parent.height; height: parent.height; iconSize: 30 * root.unit; unit: root.unit; dark: root.dark; emphasized: true; circular: true; emphasizedColor: root.dark ? "#e4e7eb" : "#242830"; foreground: root.dark ? "#202329" : "#ffffff"; text: root.host && root.host.videoArea.vid.playing ? qsTr("Pause") : qsTr("Play"); iconOnly: true; iconName: root.host && root.host.videoArea.vid.playing ? "pause" : "play"; enabled: root.previewReady; onClicked: { const v = root.host.videoArea.vid; if (v.playing) v.pause(); else v.play(); hideControls.restart(); } }
                MobileButton { objectName: "mobileNextVideo"; width: parent.height; height: parent.height; iconSize: 24 * root.unit; unit: root.unit; dark: root.dark; quiet: true; circular: true; text: qsTr("Next"); iconOnly: true; iconName: "next"; enabled: root.rows.findIndex(r => r.id === root.previewJobId) < root.rows.length - 1 && root.previewReady; onClicked: root.navigate(1) }
            }
            }
        }
    }
    Rectangle {
        objectName: "mobilePreviewLoadingCover"
        visible: root.page === "preview" && !root.previewReady
        y: root.headerHeight
        width: parent.width; height: Math.max(0, parent.height - y - (root.busy ? root.footerHeight : 0))
        color: root.color
        // Keep the player rendering behind the cover so loading can complete.
        MouseArea { anchors.fill: parent }
        MobileText { anchors.centerIn: parent; unit: root.unit; dark: root.dark; secondary: true; text: qsTr("Reading…") }
    }
    Rectangle {
        id: footer
        visible: (root.page === "library" && root.footerHeight > 0) || root.busy
        anchors.bottom: parent.bottom
        width: parent.width; height: root.footerHeight
        color: root.surfaceColor
        Rectangle { width: parent.width; height: 0.5 * root.unit; color: MobileStyle.separator(root.dark) }
        LibraryActions {
            id: libraryActions
            objectName: "mobileLibraryActions"
            visible: root.showLibraryActions
            x: root.wideFooter ? 16 * root.unit : (parent.width - width) / 2
            y: root.wideFooter ? (parent.height - height) / 2 : root.continueAfterSummary ? 36 * root.unit : 8 * root.unit
            width: root.wideFooter ? parent.width * 0.4 : Math.min(parent.width - 32 * root.unit, 520 * root.unit)
        }
        Row {
            id: selectionToolbar
            objectName: "mobileSelectionToolbar"
            visible: root.footerSelectionMode && !root.busy && !root.summary
            x: 16 * root.unit; y: 4 * root.unit; width: parent.width - 32 * root.unit
            height: 44 * root.unit; spacing: 8 * root.unit
            MobileText { width: Math.max(0, parent.width - allSelected.width - cancelSelection.width - 2 * parent.spacing); height: parent.height; unit: root.unit; dark: root.dark; secondary: true; text: qsTr("Selected %1").arg(root.selectionCount); elide: Text.ElideRight }
            MobileButton { id: allSelected; objectName: "mobileSelectAll"; width: Math.min(implicitWidth, parent.width * 0.3); unit: root.unit; dark: root.dark; quiet: true; text: qsTr("All"); enabled: root.selectionCount < root.rows.length; onClicked: root.selectAll() }
            MobileButton { id: cancelSelection; objectName: "mobileCancelSelection"; width: Math.min(implicitWidth, parent.width * 0.25); unit: root.unit; dark: root.dark; quiet: true; text: qsTr("Cancel"); onClicked: root.finishSelection() }
        }
        Row {
            id: footerActions
            visible: !root.busy && (!root.summary || root.continueAfterSummary)
            x: root.wideFooter ? libraryActions.x + libraryActions.width + 12 * root.unit : (parent.width - width) / 2
            y: root.continueAfterSummary ? 92 * root.unit : root.footerSelectionMode ? 52 * root.unit : root.showLibraryActions && !root.wideFooter ? 64 * root.unit : (parent.height - implicitHeight) / 2
            width: root.wideFooter ? parent.width * 0.6 - 44 * root.unit : Math.min(parent.width - 32 * root.unit, 520 * root.unit)
            spacing: 12 * root.unit
            MobileButton {
                id: stabilizeAction
                unit: root.unit; dark: root.dark; accentColor: root.accentColor; emphasized: !root.footerSelectionMode
                multiline: true
                height: Math.max(implicitHeight, exportAction.implicitHeight)
                width: (parent.width - parent.spacing) / 2
                objectName: "mobilePrimaryStabilize"
                visible: !root.footerSelectionMode || root.selectionCount === 1
                text: root.footerSelectionMode ? qsTr("Deep search") : qsTr("Stabilize (for plugins)")
                enabled: root.footerSelectionMode ? root.selectionCount === 1 : root.rows.length > 0
                onClicked: { if (root.footerSelectionMode) root.startDeep(Number(Object.keys(root.selection)[0])); else root.startAction("sync"); }
            }
            MobileButton {
                id: exportAction
                objectName: "mobilePrimaryExport"
                unit: root.unit; dark: root.dark; emphasized: !root.footerSelectionMode; destructive: root.footerSelectionMode; accentColor: root.accentColor
                multiline: true
                height: Math.max(implicitHeight, stabilizeAction.implicitHeight)
                width: root.footerSelectionMode && root.selectionCount !== 1 ? parent.width : (parent.width - parent.spacing) / 2
                text: root.footerSelectionMode ? qsTr("Remove") : qsTr("Export stabilized video")
                enabled: root.footerSelectionMode ? root.selectionCount > 0 : root.rows.length > 0
                onClicked: { if (root.footerSelectionMode) root.removeSelected(); else root.startAction("export"); }
            }
        }
        Column {
            id: taskStatus
            visible: root.busy || root.summary.length > 0
            x: 16 * root.unit; y: root.busy ? 12 * root.unit + (Math.max(implicitHeight, taskStatusAction.height) - implicitHeight) / 2 : root.continueAfterSummary ? 4 * root.unit : (parent.height - implicitHeight) / 2
            width: parent.width - (root.continueAfterSummary ? 32 * root.unit : taskStatusAction.width + 40 * root.unit)
            spacing: 3 * root.unit
            MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: root.busy ? root.taskTitle() : root.summary; color: root.textColor; elide: Text.ElideMiddle }
            MobileText { unit: root.unit; dark: root.dark;
                objectName: "mobileTaskSecondaryStatus"
                visible: root.busy && !(root.operation && root.operation.kind === "deep")
                width: parent.width
                text: root.importing ? qsTr("Reading…") : qsTr("Processed %1 / %2").arg(root.taskCounts.settled).arg(root.taskCounts.total || root.rows.length)
                color: root.mutedColor; secondary: true; elide: Text.ElideRight
            }
        }
        MouseArea {
            visible: (root.busy || root.summary.length > 0) && !root.continueAfterSummary
            x: 8 * root.unit; width: parent.width - 104 * root.unit; height: parent.height
            onClicked: root.showTaskDetails(0)
        }
        MobileButton {
            id: taskStatusAction
            objectName: "mobileTaskStatusAction"
            visible: (root.busy || root.summary.length > 0) && !root.continueAfterSummary
            anchors.right: parent.right; anchors.rightMargin: 8 * root.unit
            y: root.busy ? 12 * root.unit + (Math.max(taskStatus.implicitHeight, height) - height) / 2 : (parent.height - height) / 2
            width: Math.min(implicitWidth, parent.width * 0.38)
            unit: root.unit; dark: root.dark; emphasized: !root.busy
            text: root.busy ? (root.operation && root.operation.kind === "deep" ? qsTr("Cancel") : qsTr("Stop")) : qsTr("View results")
            enabled: !root.importing && !(root.operation && root.operation.stopping)
            onClicked: { if (root.busy) root.stopTask(); else root.showTaskDetails(0); }
        }
        OperationProgress {
            objectName: "mobileTaskProgress"
            visible: root.busy
            x: 16 * root.unit; width: parent.width - 32 * root.unit
            anchors.bottom: parent.bottom; anchors.bottomMargin: 16 * root.unit
        }
    }
    Rectangle {
        visible: root.notice.length > 0
        z: 10
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: footer.top; anchors.bottomMargin: 12 * root.unit
        width: Math.min(parent.width - 32 * root.unit, 480 * root.unit)
        height: noticeLabel.implicitHeight + 24 * root.unit
        radius: 10 * root.unit; color: root.dark ? "#303640" : "#dfe8f4"
        MobileText { unit: root.unit; dark: root.dark; id: noticeLabel; x: 12 * root.unit; y: 12 * root.unit; width: parent.width - 24 * root.unit; text: root.notice; wrapMode: Text.WordWrap; secondary: true; color: root.textColor }
    }
    Rectangle {
        id: scrim
        visible: root.panel.length > 0
        anchors.fill: parent
        color: "#77000000"
        z: 20
        MouseArea { anchors.fill: parent; onClicked: root.dismissPanel() }
        Rectangle {
            id: sheet
            objectName: "mobileSheet"
            readonly property bool compact: root.panel === "folderConfirm" || root.panel === "deepResult"
            x: compact ? (parent.width - width) / 2 : root.landscape ? parent.width - width : 0
            y: compact ? (root.panelAvailableHeight - height) / 2 : root.landscape || root.panel === "settings" ? 0 : root.panelAvailableHeight - height
            width: compact ? Math.min(parent.width - 32 * root.unit, 420 * root.unit) : root.landscape ? Math.min(parent.width, 380 * root.unit) : parent.width
            height: compact ? Math.max(0, Math.min(root.panelAvailableHeight - 32 * root.unit,
                panelScroll.y + panelColumn.implicitHeight + 16 * root.unit + (folderConfirmActions.visible ? folderConfirmActions.height + 12 * root.unit : taskContinueActions.visible ? taskContinueActions.height + 12 * root.unit : 0))) : root.landscape || root.panel === "settings" ? root.panelAvailableHeight : Math.max(0, Math.min(root.panelAvailableHeight - 16 * root.unit,
                root.panel === "add" ? 360 * root.unit : root.panelAvailableHeight - 16 * root.unit))
            radius: root.panel === "settings" ? 0 : 8 * root.unit; color: root.color
            MouseArea { anchors.fill: parent; onPressed: mouse => mouse.accepted = true; onWheel: wheel => wheel.accepted = true }
            MobileButton {
                id: panelBack
                visible: root.panelTrail.length > 0 || root.panel === "settings"
                x: 4 * root.unit; y: 4 * root.unit
                unit: root.unit; dark: root.dark; quiet: true; iconOnly: true; iconName: "back"; text: qsTr("Back")
                onClicked: root.back()
            }
            MobileText { unit: root.unit; dark: root.dark;
                x: panelBack.visible ? 52 * root.unit : 20 * root.unit; y: 18 * root.unit; width: parent.width - x - 64 * root.unit
                text: root.panel === "settings" ? qsTr("Settings") : root.panel === "info" ? qsTr("Video information")
                    : root.panel === "folderConfirm" ? qsTr("Confirm folder import")
                    : root.panel === "folders" ? qsTr("Choose folders")
                    : root.panel === "files" ? qsTr("Choose files")
                    : root.panel === "add" ? qsTr("Add media") : root.panel === "sources" ? qsTr("Gyroscope data")
                    : root.panel === "deepResult" ? qsTr("Deep search complete") : qsTr("Task details")
                heading: true; color: root.textColor; elide: Text.ElideRight
            }
            MobileButton {
                visible: root.panel !== "settings"
                anchors.right: parent.right; anchors.rightMargin: 8 * root.unit; y: 4 * root.unit
                unit: root.unit; dark: root.dark; quiet: true; text: qsTr("Close"); iconOnly: true; iconName: "close"
                Accessible.name: qsTr("Close")
                onClicked: root.dismissPanel()
            }
            MobileTabs {
                id: panelTabs
                navigation: true
                objectName: "mobilePanelTabs"
                visible: root.panel === "settings" || root.panel === "add"
                x: 16 * root.unit; y: 60 * root.unit; width: parent.width - 32 * root.unit
                unit: root.unit; dark: root.dark
                model: root.panel === "settings" ? [qsTr("Stabilize"), qsTr("Lens"), qsTr("App")] : [qsTr("Videos"), qsTr("External gyroscope")]
                currentIndex: root.panel === "settings" ? root.settingsTab : root.importTab
                onActivated: index => { panelScroll.contentY = 0; if (root.panel === "settings") root.settingsTab = index; else root.importTab = index; }
            }
            Flickable {
                id: panelScroll
                visible: root.panel !== "folders" && root.panel !== "files"
                x: 16 * root.unit; y: panelTabs.visible ? panelTabs.y + panelTabs.height + 16 * root.unit : 64 * root.unit
                width: parent.width - 32 * root.unit; height: parent.height - y - 16 * root.unit - (folderConfirmActions.visible ? folderConfirmActions.height + 12 * root.unit : taskContinueActions.visible ? taskContinueActions.height + 12 * root.unit : 0)
                contentWidth: width; contentHeight: panelColumn.height
                clip: true; boundsBehavior: Flickable.StopAtBounds
                QQC.ScrollIndicator.vertical: QQC.ScrollIndicator {}
                Column {
                    id: panelColumn
                    width: parent.width
                    spacing: 12 * root.unit
                    Column {
                        visible: root.panel === "folderConfirm"
                        width: parent.width; spacing: 16 * root.unit
                        MobileText {
                            objectName: "mobileFolderConfirmationPath"
                            width: parent.width; unit: root.unit; dark: root.dark; wrapMode: Text.WrapAnywhere; textFormat: Text.PlainText
                            text: root.pendingFolderImport ? (root.filesystemService ? root.filesystemService.display_url(root.pendingFolderImport.url) : root.pendingFolderImport.url) : ""
                        }
                    }
                    Column {
                        visible: root.panel === "add" && root.importTab === 0
                        width: parent.width; spacing: 16 * root.unit
                        MobileGroup {
                            width: parent.width; unit: root.unit; dark: root.dark; contentInset: 0
                            MobileActionRow { objectName: "mobileChoosePhotos"; visible: root.platformOs === "ios"; width: parent.width; unit: root.unit; dark: root.dark; navigation: true; iconName: "photos"; divider: true; text: qsTranslate("VideoSourcePicker", "Photos"); enabled: !root.busy; onClicked: root.requestPhotos() }
                            MobileActionRow { objectName: "mobileChooseVideos"; width: parent.width; unit: root.unit; dark: root.dark; navigation: true; iconName: "play"; divider: true; text: qsTr("Choose files"); enabled: !root.busy; onClicked: root.requestAdd(false) }
                            MobileActionRow { objectName: "mobileChooseVideoFolders"; width: parent.width; unit: root.unit; dark: root.dark; navigation: true; iconName: "folder"; text: qsTr("Choose folders"); enabled: !root.busy; onClicked: root.requestAdd(true) }
                        }
                    }
                    Column {
                        visible: root.panel === "settings"
                        width: parent.width
                        spacing: 16 * root.unit
                        MobileText { unit: root.unit; dark: root.dark; visible: root.busy; width: parent.width; text: qsTr("Stop the current task to adjust processing settings."); wrapMode: Text.WordWrap; color: root.mutedColor; secondary: true; }
                        MobileText { unit: root.unit; dark: root.dark; visible: root.settingsTab < 2; width: parent.width; text: qsTr("Changes apply to all videos."); wrapMode: Text.WordWrap; secondary: true }
                        MobileSettings { visible: root.settingsTab !== 1; width: parent.width; host: root.host; unit: root.unit; dark: root.dark; busy: root.busy; section: ["stabilization", "lens", "app"][root.settingsTab] }
                        Column { id: settingsContent; visible: root.settingsTab === 1; width: parent.width; spacing: 16 * root.unit; enabled: !root.busy; opacity: enabled ? 1 : 0.55 }
                    }
                    Column {
                        visible: root.panel === "info"
                        width: parent.width; spacing: 12 * root.unit
                        MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: root.previewRecord ? root.previewRecord.filename : ""; color: root.textColor;  wrapMode: Text.WrapAnywhere }
                        MobileText { unit: root.unit; dark: root.dark; visible: root.previewRequested; text: qsTr("Reading…"); color: root.mutedColor; secondary: true; }
                        MobileGroup {
                            width: parent.width; unit: root.unit; dark: root.dark
                            JobActions { visible: !!root.previewRecord; record: root.previewRecord || {} }
                            MobileActionRow { width: parent.width; unit: root.unit; dark: root.dark; iconName: "play"; text: qsTr("Play"); enabled: !!root.previewRecord && root.previewRecord.previewable; onClicked: root.openPreview(root.previewJobId) }
                        }
                        MobileGroup {
                            visible: root.videoInfoKeys.length > 0
                            width: parent.width; unit: root.unit; dark: root.dark; contentSpacing: 16 * root.unit
                            Repeater {
                                model: root.videoInfoKeys
                                Column {
                                    required property string modelData
                                    width: parent.width
                                    spacing: 4 * root.unit
                                    MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: qsTranslate("TableList", modelData); secondary: true; wrapMode: Text.WordWrap }
                                    MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: root.videoInfo[modelData] || qsTr("Unknown"); wrapMode: Text.WrapAnywhere; textFormat: Text.StyledText }
                                }
                            }
                        }
                    }
                    Column {
                        visible: root.panel === "add" && root.importTab === 1
                        width: parent.width; spacing: 12 * root.unit
                        MobileGroup {
                            width: parent.width; unit: root.unit; dark: root.dark; contentInset: 0
                            MobileActionRow { objectName: "mobileAddGyroButton"; width: parent.width; unit: root.unit; dark: root.dark; navigation: true; iconName: "plus"; divider: true; text: qsTr("Choose files"); enabled: !root.busy; onClicked: root.requestGyro() }
                            MobileActionRow { objectName: "mobileChooseGyroFolders"; width: parent.width; unit: root.unit; dark: root.dark; navigation: true; iconName: "folder"; text: qsTr("Choose folders"); enabled: !root.busy; onClicked: root.requestFolder("gyro") }
                        }
                    }
                    Column {
                        visible: root.panel === "task" || root.panel === "deepResult"
                        width: parent.width; spacing: 16 * root.unit
                        MobileText { objectName: "mobileTaskHeading"; visible: root.panel !== "deepResult"; unit: root.unit; dark: root.dark; width: parent.width; text: root.busy ? root.taskTitle() : root.lastTaskMessage; color: root.textColor; wrapMode: root.busy && root.operation && root.operation.kind === "deep" ? Text.NoWrap : Text.WordWrap; elide: Text.ElideRight }
                        OperationProgress { objectName: "mobileTaskDetailProgress"; visible: root.busy; width: parent.width }
                        Repeater {
                            model: root.panel === "task" ? root.rows.filter(r => !root.detailJobId || r.id === root.detailJobId) : []
                            MobileGroup {
                                required property var modelData
                                width: panelColumn.width
                                unit: root.unit; dark: root.dark; contentSpacing: 8 * root.unit
                                MobileText { unit: root.unit; dark: root.dark; width: parent.width; text: modelData.filename; color: root.textColor;  wrapMode: Text.WrapAnywhere }
                                JobActions { record: modelData }
                            }
                        }
                    }
                }
            }
            Row {
                id: folderConfirmActions
                visible: root.panel === "folderConfirm"
                x: 16 * root.unit; anchors.bottom: parent.bottom; anchors.bottomMargin: 16 * root.unit
                width: parent.width - 32 * root.unit; spacing: 8 * root.unit
                MobileButton { objectName: "mobileCancelFolderImport"; width: (parent.width - parent.spacing) / 2; unit: root.unit; dark: root.dark; text: qsTr("Cancel"); onClicked: root.dismissPanel() }
                MobileButton { objectName: "mobileConfirmFolderImport"; width: (parent.width - parent.spacing) / 2; unit: root.unit; dark: root.dark; emphasized: true; text: qsTr("Import"); enabled: !root.busy && !!root.pendingFolderImport; onClicked: root.confirmFolderImport() }
            }
            Row {
                id: taskContinueActions
                objectName: "mobileContinueProcessing"
                visible: (root.panel === "task" || root.panel === "deepResult") && !root.busy && root.rows.length > 0
                x: 16 * root.unit; anchors.bottom: parent.bottom; anchors.bottomMargin: 16 * root.unit
                width: parent.width - 32 * root.unit; spacing: 8 * root.unit
                MobileButton {
                    id: continueStabilize
                    objectName: "mobileContinueStabilize"
                    width: (parent.width - parent.spacing) / 2; height: Math.max(implicitHeight, continueExport.implicitHeight)
                    unit: root.unit; dark: root.dark; multiline: true; emphasized: true; text: qsTr("Stabilize (for plugins)"); onClicked: root.startAction("sync")
                }
                MobileButton {
                    id: continueExport
                    objectName: "mobileContinueExport"
                    width: (parent.width - parent.spacing) / 2; height: Math.max(implicitHeight, continueStabilize.implicitHeight)
                    unit: root.unit; dark: root.dark; multiline: true; emphasized: true; text: qsTr("Export stabilized video"); onClicked: root.startAction("export")
                }
            }
            MobileFolderPicker {
                id: folderPicker
                objectName: "mobileFolderPicker"
                visible: root.panel === "folders" || root.panel === "files"
                x: 16 * root.unit; y: 64 * root.unit; width: parent.width - 32 * root.unit; height: parent.height - y - 16 * root.unit
                unit: root.unit; dark: root.dark; filesystemService: root.filesystemService; queueService: root.queueService
                onLocationRequested: kind => root.requestNativeFolder(kind)
                onAccepted: (urls, kind) => {
                    if (!root.inputsAllowed() || !root.queueService) return;
                    root.panelTrail = [];
                    root.panel = "";
                    if (kind === "gyro") root.queueService.addMobileGyroUrls(urls);
                    else root.queueService.dt.loadFiles(urls);
                }
            }
        }
    }
    Keys.onReleased: event => {
        if (event.key === Qt.Key_Back || event.key === Qt.Key_Escape) event.accepted = root.back();
    }
    Item {
        objectName: "mobileIosBackEdge"
        visible: root.platformOs === "ios" && (root.page === "preview" || root.panel.length > 0)
        x: root.panel ? sheet.x : 0
        y: root.panel ? sheet.y + 56 * root.unit : root.headerHeight
        width: 20 * root.unit
        height: Math.max(0, root.panel ? sheet.height - 56 * root.unit : root.height - y)
        z: 30
        DragHandler {
            objectName: "mobileIosBackDrag"
            target: null
            acceptedDevices: PointerDevice.TouchScreen
            yAxis.enabled: false
            property real initialWidth: 0
            property real initialHeight: 0
            property bool cancelled: false
            property string initialPage: ""
            property string initialPanel: ""
            property real initialTranslation: 0
            onCanceled: cancelled = true
            onActiveChanged: {
                if (active) { initialWidth = root.width; initialHeight = root.height; initialPage = root.page; initialPanel = root.panel; initialTranslation = persistentTranslation.x; cancelled = false; }
                // activeTranslation is already reset when the touch ends.
                else if (!cancelled && initialWidth === root.width && initialHeight === root.height
                         && initialPage === root.page && initialPanel === root.panel
                         && persistentTranslation.x - initialTranslation > 60 * root.unit) root.back();
            }
        }
    }
}
