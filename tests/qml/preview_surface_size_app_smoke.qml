// SPDX-License-Identifier: GPL-3.0-or-later
// Run as <case>/ui/main_window.qml with a 640x360 clip at <case>/../clip.mp4.
// Also verify actual texture creation between the SURFACE_SIZE_STAGE log markers.
import QtQuick

Item {
    id: test
    property var applicationWindow: null
    property var app: null
    property var video: null
    property int stage: 0
    property double started: Date.now()
    property double stageStarted: Date.now()
    property bool finished: false
    property url clip: Qt.resolvedUrl("../../clip.mp4")

    function check(condition, message) {
        if (condition) return true;
        finish(false, message);
        return false;
    }
    function finish(ok, message) {
        finished = true;
        if (video) video.pause();
        applicationWindow.closeConfirmed = true;
        console.warn(ok ? "SURFACE_SIZE_PASS" : "SURFACE_SIZE_FAIL", message);
        Qt.exit(ok ? 0 : 1);
    }
    function requested(width, height) {
        return video.surfaceWidth === width && video.surfaceHeight === height;
    }
    Component.onCompleted: {
        applicationWindow = Qt.createComponent("qrc:/src/ui/main_window.qml").createObject(null);
        applicationWindow.title = "Preview surface regression - automatic";
        applicationWindow.hide();
        Qt.callLater(function() { applicationWindow.showNormal(); });
    }
    Timer {
        interval: 100
        repeat: true
        running: !test.finished
        onTriggered: {
            if (Date.now() - test.started > 35000) { test.finish(false, "timeout"); return; }
            if (Date.now() - test.started < 6000) return;
            if (test.stage === 0) {
                const app = test.applicationWindow.getApp();
                if (!app || !app.advanced || !app.sync || !app.stab || !app.videoArea.queue) return;
                test.app = app;
                test.video = app.videoArea.vid;
                app.closeAppUpdateDialog();
                app.isDialogOpened = false;
                app.videoArea.loadFile(test.clip, true, 0, "", true);
                test.stage = 1;
                test.stageStarted = Date.now();
                return;
            }
            if (Date.now() - test.stageStarted < 1500) return;
            if (test.stage === 1) {
                if (!test.video.loaded || test.app.videoArea.previewLoading || test.app.videoArea.videoLoader.active) return;
                if (!test.check(test.video.videoWidth === 640 && test.video.videoHeight === 360, "unexpected fixture dimensions")) return;
                test.video.pause();
                test.app.videoArea.stabEnabledBtn.checked = false;
                console.warn("SURFACE_SIZE_STAGE initial");
                test.app.controller.set_preview_resolution(720, test.video);
            } else if (test.stage === 2) {
                if (!test.check(test.requested(1280, 720), "initial size did not settle")) return;
                test.video.surfaceSizeUpdated(32, 32);
                if (!test.check(test.requested(1280, 720), "stale placeholder changed the requested size")) return;
                test.video.surfaceSizeUpdated(2880, 1080);
                if (!test.check(test.requested(1280, 720), "previous clip changed the requested size")) return;
                test.app.controller.set_preview_resolution(180, test.video);
                test.app.controller.set_preview_resolution(360, test.video);
                test.app.controller.set_preview_resolution(720, test.video);
                test.video.surfaceSizeUpdated(320, 180);
                test.video.surfaceSizeUpdated(640, 360);
                if (!test.check(test.requested(1280, 720), "older completion replaced the latest request")) return;
                console.warn("SURFACE_SIZE_STAGE stale_notifications_passed");
            } else if (test.stage === 3) {
                if (!test.check(!test.video.playing && test.requested(1280, 720), "paused state or latest request changed")) return;
                console.warn("SURFACE_SIZE_STAGE paused_resize");
                // Do not call update or seek here: the setter must schedule the resize itself.
                test.app.controller.set_preview_resolution(360, test.video);
            } else if (test.stage === 4) {
                if (!test.check(!test.video.playing && test.requested(640, 360), "paused resize did not preserve its request")) return;
                console.warn("SURFACE_SIZE_STAGE auto_first");
                test.video.surfaceWidth = 0;
                test.video.surfaceHeight = 0;
                // Release layout anchors so the item itself can change size.
                test.video.anchors.fill = undefined;
                test.video.width = 800;
                test.video.height = 450;
                test.video.update();
            } else if (test.stage === 5) {
                if (!test.check(test.requested(0, 0), "automatic sizing was overwritten by a completion")) return;
                console.warn("SURFACE_SIZE_STAGE auto_second");
                test.video.width = 960;
                test.video.height = 540;
            } else if (test.stage === 6) {
                if (!test.check(test.requested(0, 0), "automatic sizing stopped following the item")) return;
                test.finish(true, "stale notifications, latest request, paused resize and automatic sizing");
                return;
            }
            test.stage++;
            test.stageStarted = Date.now();
        }
    }
}
