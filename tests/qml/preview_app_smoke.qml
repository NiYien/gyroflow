// SPDX-License-Identifier: GPL-3.0-or-later
// Run beside the application as ui/main_window.qml with target/mobile-folder-fixtures present.
import QtQuick

Item {
    id: smoke
    property var applicationWindow: null
    property int stage: 0
    property int ticks: 0
    property int jobId: 0
    property bool completed: false
    function check(condition, message) {
        if (!condition) {
            completed = true;
            console.warn("PREVIEW_SMOKE_FAIL", stage, message);
            if (applicationWindow) applicationWindow.closeConfirmed = true;
            Qt.exit(1);
        }
    }
    function findItem(item, name) {
        if (item.objectName === name) return item;
        for (const child of item.children || []) {
            const found = findItem(child, name);
            if (found) return found;
        }
        return null;
    }
    function checkSimpleActions(app) {
        const points = [];
        for (const name of ["simpleAutoSyncButton", "simpleExportStabilizedButton"]) {
            const button = findItem(app, name);
            smoke.check(!!button && button.visible, name + " is visible");
            if (!button) return;
            const point = button.mapToItem(app, 0, 0);
            smoke.check(point.x >= 0 && point.x + button.width <= app.width
                && point.y >= 0 && point.y + button.height <= app.height,
                name + " stays inside the window");
            points.push(point);
        }
        smoke.check(Math.abs(points[0].y - points[1].y) < 1, "desktop actions share the same row");
    }
    Component.onCompleted: {
        console.warn("PREVIEW_SMOKE_START");
        const component = Qt.createComponent(Qt.resolvedUrl("../../../src/ui/main_window.qml"));
        check(component.status === Component.Ready, component.errorString());
        applicationWindow = component.createObject(null);
    }
    Timer {
        interval: 250; repeat: true; running: !smoke.completed
        onTriggered: {
            if (++smoke.ticks > 240) { smoke.check(false, "timeout"); return; }
            if (smoke.ticks < 20) return;
            const app = smoke.applicationWindow ? smoke.applicationWindow.getApp() : null;
            if (smoke.ticks === 20) console.warn("PREVIEW_SMOKE_READY", !!app, app && !!app.advanced, app && !!app.lensProfile, app && !!app.videoArea.queue);
            if (!app || !app.advanced || !app.lensProfile || !app.sync || !app.stab || !app.motionData || !app.exportSettings || !app.videoArea.queue) return;
            app.onboardingActive = true;
            const video = app.videoArea;
            if (smoke.stage === 0) {
                smoke.checkSimpleActions(app);
                smoke.check(video.stabPreviewBtn.previewState === 1, "startup defaults to stabilized preview");
                smoke.check(!video.timeline.visible, "empty preview hides the timeline");
                smoke.check(Math.abs(video.previewArea.height - video.height) < 1, "empty preview uses the available height");
                app.deepMatchStabilizePending = true;
                video.loadFile(Qt.resolvedUrl("../../../target/mobile-folder-fixtures/A/clip1.mp4"), true, 0, "", true);
                smoke.check(!video.shouldShowStabilizeHint(), "loading must not report missing stabilization");
                smoke.stage++;
            } else if (smoke.stage === 1) {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.checkSimpleActions(app);
                smoke.check(video.timeline.visible, "loaded video restores the timeline");
                smoke.check(!app.controller.gyro_loaded && !app.controller.lens_loaded, "fixture has no stabilization metadata");
                smoke.check(video.stabPreviewBtn.previewState === 1, "plain video defaults to stabilized preview");
                smoke.check(video.infoMessages.children[0].visible, "stabilized preview retains missing-lens warning");
                video.stabEnabledBtn.checked = false;
                video.applyDefaultPreview();
                smoke.check(!video.stabEnabledBtn.checked, "completed load does not override manual comparison");
                smoke.check(!video.infoMessages.children[0].visible, "original playback has no missing-lens warning");
                video.vid.play();
                smoke.check(!video.shouldShowStabilizeHint(), "plain playback ignores another clip's deep match");
                video.stabEnabledBtn.checked = true;
                smoke.check(video.infoMessages.children[0].visible, "explicit stabilization retains lens warning");
                smoke.check(!video.shouldShowStabilizeHint(), "unrelated video never receives queue reminder");
                video.vid.pause();
                smoke.jobId = render_queue.add_file(Qt.resolvedUrl("../../../target/mobile-folder-fixtures/A/clip2.mp4").toString(), "", app.getAdditionalProjectDataJson());
                smoke.check(smoke.jobId > 0, "queue fixture added");
                smoke.stage++;
            } else if (smoke.stage === 2) {
                const data = render_queue.get_gyroflow_data(smoke.jobId);
                if (!data) return;
                video.fovOverviewBtn.checked = true;
                video.loadGyroflowData(JSON.parse(data), smoke.jobId);
                smoke.check(!video.shouldShowStabilizeHint(), "queue project import suppresses transient reminder");
                smoke.stage++;
            } else if (smoke.stage === 3) {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.check(video.stabPreviewBtn.previewState === 1, "queue preview resets overview to stabilized");
                smoke.check(!video.secondPreview.show, "queue preview resets split view");
                smoke.check(video.shouldShowStabilizeHint(), "unfinished queue work still receives reminder in stabilization view");
                video.queueEditLoading = true;
                smoke.check(!video.shouldShowStabilizeHint(), "incomplete project data cannot trigger reminder");
                video.queueEditLoading = false;
                video.stabEnabledBtn.checked = false;
                smoke.check(!video.shouldShowStabilizeHint(), "original playback remains quiet");
                video.loadGyroflowData(JSON.parse(render_queue.get_gyroflow_data(smoke.jobId)), smoke.jobId);
                smoke.stage++;
            } else if (smoke.stage === 4) {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.check(video.stabPreviewBtn.previewState === 1, "reopening the same queue video restores stabilization");
                app.advanced.setThemeIndex(2, false);
                smoke.stage++;
            } else if (smoke.stage === 5) {
                if (!app.isMobileLayout) return;
                app.advanced.setThemeIndex(1, false);
                smoke.stage++;
            } else if (smoke.stage === 6) {
                if (app.isMobileLayout) return;
                smoke.checkSimpleActions(app);
                video.loadFile(Qt.resolvedUrl("../../../target/mobile-folder-fixtures/A/clip1.mp4"), true, 0, "", true);
                smoke.stage++;
            } else {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.checkSimpleActions(app);
                console.warn("PREVIEW_SMOKE_PASS startup, default stabilization, queue replay, overview reset, load guards, manual comparison, theme roundtrip and action visibility");
                smoke.completed = true;
                smoke.applicationWindow.closeConfirmed = true;
                Qt.quit();
            }
        }
    }
}
