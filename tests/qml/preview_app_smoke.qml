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
                app.deepMatchStabilizePending = true;
                video.loadFile(Qt.resolvedUrl("../../../target/mobile-folder-fixtures/A/clip1.mp4"), true, 0, "", true);
                smoke.check(!video.shouldShowStabilizeHint(), "loading must not report missing stabilization");
                smoke.stage++;
            } else if (smoke.stage === 1) {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.check(!app.controller.gyro_loaded && !app.controller.lens_loaded, "fixture has no stabilization metadata");
                smoke.check(!video.stabEnabledBtn.checked, "plain video defaults to original playback");
                smoke.check(!video.infoMessages.children[0].visible, "plain playback has no missing-lens warning");
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
                video.loadGyroflowData(JSON.parse(data), smoke.jobId);
                smoke.check(!video.shouldShowStabilizeHint(), "queue project import suppresses transient reminder");
                smoke.stage++;
            } else {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.check(!video.stabEnabledBtn.checked, "queue video without gyro also defaults to original");
                smoke.check(!video.infoMessages.children[0].visible, "queue original has no lens warning");
                video.stabEnabledBtn.checked = true;
                smoke.check(video.shouldShowStabilizeHint(), "unfinished queue work still receives reminder in stabilization view");
                video.queueEditLoading = true;
                smoke.check(!video.shouldShowStabilizeHint(), "incomplete project data cannot trigger reminder");
                video.queueEditLoading = false;
                video.stabEnabledBtn.checked = false;
                smoke.check(!video.shouldShowStabilizeHint(), "original playback remains quiet");
                console.warn("PREVIEW_SMOKE_PASS plain video, queue project, load guards, original playback, explicit stabilization warnings");
                smoke.completed = true;
                smoke.applicationWindow.closeConfirmed = true;
                Qt.quit();
            }
        }
    }
}
