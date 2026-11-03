// SPDX-License-Identifier: GPL-3.0-or-later
// Run with the isolated failing R3D plugin; MP4 remains an independent recovery path.
import QtQuick
Item {
    id: test
    property url failingClip
    property url recoveryClip
    property var applicationWindow: null
    property int stage: 0
    property double started: Date.now()
    property double stageStarted: Date.now()
    property bool finished: false
    function end(ok, message) {
        finished = true;
        console.warn(ok ? "R3D_FAILURE_APP_PASS" : "R3D_FAILURE_APP_FAIL", stage, message);
        if (applicationWindow) applicationWindow.closeConfirmed = true;
        Qt.exit(ok ? 0 : 1);
    }
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/src/ui/main_window.qml");
        applicationWindow = component.createObject(null);
        applicationWindow.title = "R3D failure regression - automatic";
        applicationWindow.hide();
        Qt.callLater(function() { applicationWindow.showNormal(); });
    }
    Timer {
        interval: 100; repeat: true; running: !test.finished
        onTriggered: {
            const app = test.applicationWindow ? test.applicationWindow.getApp() : null;
            if (Date.now() - test.started > 45000) { test.end(false, "overall timeout"); return; }
            if (!app || !app.advanced || !app.sync || !app.stab || Date.now() - test.started < 6000) return;
            const area = app.videoArea;
            const video = area.vid;
            app.onboardingActive = true;
            if (test.stage === 0) {
                area.loadFile(test.failingClip, true, 0, "", true);
                test.stage = 1; test.stageStarted = Date.now();
            } else if (test.stage === 1) {
                if (Date.now() - test.stageStarted > 12000) { test.end(false, "failed load did not finish"); return; }
                if (!video.errorShown || app.controller.video_loading_in_progress || area.videoLoader.active) return;
                if (video.loaded) { test.end(false, "failed video reported loaded"); return; }
                console.warn("R3D_FAILURE_APP_LOAD_ENDED");
                area.loadFile(test.recoveryClip, true, 0, "", true);
                test.stage = 2; test.stageStarted = Date.now();
            } else if (test.stage === 2) {
                if (Date.now() - test.stageStarted > 12000) { test.end(false, "recovery load timeout"); return; }
                if (app.controller.video_loading_in_progress || area.videoLoader.active || !video.loaded) return;
                area.stabEnabledBtn.checked = false;
                video.play(); test.stage = 3; test.stageStarted = Date.now();
            } else if (test.stage === 3 && Date.now() - test.stageStarted > 1000) {
                test.end(video.currentFrame > 5, "failed load releases the guard and next file plays");
            }
        }
    }
}
