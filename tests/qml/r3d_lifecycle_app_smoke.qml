// SPDX-License-Identifier: GPL-3.0-or-later
// Exercise real preview scaling, warm-up seeks and retained playback state in a visible window.
import QtQuick
Item {
    id: test
    property var applicationWindow: null
    property int clip: 0
    property int stage: 0
    property double started: Date.now()
    property double stageStarted: Date.now()
    property bool finished: false
    property bool sawEnd: false
    property double lastSeekTrace: 0
    property var expected: []
    property var clips: []
    function nextStage(value) { stage = value; stageStarted = Date.now(); }
    function end(ok, reason) {
        finished = true;
        console.warn(ok ? "RAW_LIFECYCLE_PASS" : "RAW_LIFECYCLE_FAIL", clip, stage, reason);
        applicationWindow.closeConfirmed = true;
        Qt.exit(ok ? 0 : 1);
    }
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/src/ui/main_window.qml");
        applicationWindow = component.createObject(null);
        applicationWindow.hide();
        Qt.callLater(function() { applicationWindow.showNormal(); });
        applicationWindow.title = "【自动验收，请勿操作】RAW 加载、跳转、回环与换片";
    }
    Timer {
        interval: 100; repeat: true; running: !test.finished
        onTriggered: {
            const app = test.applicationWindow ? test.applicationWindow.getApp() : null;
            if (Date.now() - test.started > 100000) { test.end(false, "overall timeout"); return; }
            if (!app || !app.advanced || !app.sync || !app.stab || !app.videoArea.queue || Date.now() - test.started < 15000) return;
            app.onboardingActive = true;
            const area = app.videoArea;
            const video = area.vid;
            const elapsed = Date.now() - test.stageStarted;
            if (test.stage === 0) {
                console.warn("RAW_LIFECYCLE_LOAD", test.clip, test.clips[test.clip]);
                area.loadFile(test.clips[test.clip], true, 0, "", true);
                test.nextStage(1);
            } else if (test.stage === 1) {
                if (elapsed > 20000) { test.end(false, "load timeout"); return; }
                if (area.previewLoading || area.videoLoader.active || !video.loaded || video.frameCount < 1) return;
                const expected = test.expected[test.clip];
                if (Math.abs(video.duration - expected[0]) > 1 || video.frameCount !== expected[1] || Math.abs(video.frameRate - expected[2]) > 0.01 || video.videoWidth !== expected[3] || video.videoHeight !== expected[4]) {
                    test.end(false, "metadata mismatch: " + [video.duration, video.frameCount, video.frameRate, video.videoWidth, video.videoHeight].join(","));
                    return;
                }
                area.stabEnabledBtn.checked = false;
                console.warn("RAW_LIFECYCLE_LOADED", test.clip, video.duration, video.frameCount, video.videoWidth, video.videoHeight);
                video.play();
                test.nextStage(2);
            } else if (test.stage === 2 && elapsed > 1500) {
                if (video.currentFrame < 5) { test.end(false, "playback did not advance"); return; }
                video.pause();
                video.timestamp = video.duration * 0.5;
                test.nextStage(3);
            } else if (test.stage === 3 && elapsed > 600) {
                if (Math.abs(video.timestamp - video.duration * 0.5) > 150) {
                    if (elapsed > 10000) test.end(false, "middle seek timeout");
                    return;
                }
                console.warn("RAW_LIFECYCLE_SEEK", test.clip, video.timestamp, video.currentFrame);
                video.timestamp = video.duration - 1000;
                test.sawEnd = false;
                test.nextStage(31);
            } else if (test.stage === 31) {
                if (Math.abs(video.timestamp - (video.duration - 1000)) < 150) {
                    video.play();
                    test.nextStage(4);
                } else if (elapsed > 10000) test.end(false, "end seek timeout");
            } else if (test.stage === 4) {
                if (video.timestamp > video.duration - 1200) test.sawEnd = true;
                if (test.sawEnd && elapsed > 800 && video.timestamp < video.duration * 0.5) {
                    console.warn("RAW_LIFECYCLE_LOOP", test.clip, video.timestamp);
                    video.pause();
                    video.timestamp = 500;
                    video.timestamp = video.duration * 0.6;
                    video.timestamp = 750;
                    test.nextStage(41);
                } else if (elapsed > 10000) test.end(false, "loop timeout");
            } else if (test.stage === 41) {
                if (Date.now() - test.lastSeekTrace > 1000) {
                    test.lastSeekTrace = Date.now();
                    console.warn("RAW_LIFECYCLE_SEEK_STATE", test.clip, video.timestamp, video.currentFrame, video.playing, video.buffering);
                }
                if (Math.abs(video.timestamp - 750) < 150) {
                    video.play();
                    test.nextStage(5);
                } else if (elapsed > 10000) test.end(false, "rapid seek timeout at " + video.timestamp + " playing=" + video.playing + " buffering=" + video.buffering);
            } else if (test.stage === 5 && elapsed > 1300) {
                if (video.timestamp < 750 || video.timestamp > 2600) { test.end(false, "rapid seek did not settle"); return; }
                console.warn("RAW_LIFECYCLE_SWITCH", test.clip, video.timestamp, video.currentFrame);
                if (++test.clip >= test.clips.length) test.end(true, "metadata, playback, pause, seek, loop and clip switches");
                else test.nextStage(0);
            }
        }
    }
}
