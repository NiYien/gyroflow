// SPDX-License-Identifier: GPL-3.0-or-later
// Run as ui/main_window.qml beside an isolated application executable.
import QtQuick
Item {
    id: test
    property var applicationWindow: null
    property var clips: []
    property int clip: 0
    property int stage: 0
    property double started: Date.now()
    property double stageStarted: Date.now()
    property bool finished: false
    property var failures: []
    property var metadata: ({})
    property int notifications: 0
    function check(value, message) {
        if (!value) {
            failures.push(clip + ": " + message);
            console.warn("VIDEO_INFO_ASSERT", clip, message);
        }
    }
    function finish(reason) {
        if (reason) check(false, reason);
        finished = true;
        console.warn(failures.length ? "VIDEO_INFO_FAIL" : "VIDEO_INFO_PASS", JSON.stringify(failures));
        applicationWindow.closeConfirmed = true;
        Qt.exit(failures.length ? 1 : 0);
    }
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/src/ui/main_window.qml");
        applicationWindow = component.createObject(null);
        applicationWindow.title = "【自动验收，请勿操作】视频信息与换片";
        applicationWindow.width = 1280;
        applicationWindow.height = 800;
        applicationWindow.hide();
        Qt.callLater(function() { applicationWindow.showNormal(); });
    }
    Connections {
        target: test.applicationWindow && test.applicationWindow.getApp() ? test.applicationWindow.getApp().videoArea.vid : null
        function onMetadataLoaded(md) {
            test.metadata = md;
            test.notifications++;
        }
    }
    Timer {
        interval: 100; repeat: true; running: !test.finished
        onTriggered: {
            if (Date.now() - test.started > 180000) { test.finish("overall timeout"); return; }
            const app = test.applicationWindow ? test.applicationWindow.getApp() : null;
            if (!app || !app.advanced || !app.videoArea.queue || Date.now() - test.started < 6000) return;
            app.onboardingActive = true;
            const area = app.videoArea;
            const video = area.vid;
            const expected = test.clips[test.clip];
            if (test.stage === 0) {
                test.metadata = ({});
                test.notifications = 0;
                video.muted = true;
                area.loadFile(expected.url, true, 0, "", true);
                test.stage = 1;
                test.stageStarted = Date.now();
            } else if (test.stage === 1) {
                if (Date.now() - test.stageStarted > 30000) { test.finish("load timeout"); return; }
                if (area.previewLoading || area.videoLoader.active || !video.loaded || video.frameCount < 1) return;
                test.stage = 2;
                test.stageStarted = Date.now();
            } else if (test.stage === 2 && Date.now() - test.stageStarted > 500) {
                const md = test.metadata;
                const model = app.vidInfo.infoList.model;
                console.warn("VIDEO_INFO_SAMPLE", JSON.stringify({url: expected.url, duration: video.duration, frames: video.frameCount,
                    fps: video.frameRate, width: video.videoWidth, height: video.videoHeight, notifications: test.notifications,
                    model: model, metadata: md}));
                test.check(Math.abs(video.duration - expected.duration) < 1, "player duration: " + video.duration);
                test.check(Math.abs(+md["stream.video[0].duration"] - expected.duration) < 1, "metadata duration: " + md["stream.video[0].duration"]);
                test.check(model["Duration"] === expected.displayDuration, "display duration: " + model["Duration"]);
                test.check(!!md["stream.video[0].codec.name"], "missing codec metadata");
                test.check(model["Codec"] !== "---", "missing codec display");
                if (expected.audio !== undefined) {
                    test.check(!!md["stream.audio[0].codec.name"] === expected.audio, "audio metadata does not match source");
                    test.check((model["Audio"] !== "---") === expected.audio, "audio display does not match source");
                }
                if (expected.width) {
                    test.check(video.videoWidth === expected.width && video.videoHeight === expected.height, "source dimensions changed");
                    test.check(+md["stream.video[0].codec.width"] === expected.width && +md["stream.video[0].codec.height"] === expected.height, "metadata source dimensions changed");
                    test.check(model["Dimensions"] === expected.width + "x" + expected.height, "display source dimensions changed");
                }
                if (++test.clip === test.clips.length) { test.finish(""); return; }
                test.stage = 0;
            }
        }
    }
}
