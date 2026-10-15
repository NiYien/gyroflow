// SPDX-License-Identifier: GPL-3.0-or-later
// Run beside an isolated application executable as ui/main_window.qml.
import QtQuick
Item {
    id: test;
    property var applicationWindow: null;
    property var video: null;
    property bool loadingInitialClip: false;
    property url clip: "file:///C:/Users/Jhe/Desktop/github/gyroflow/target/mobile-folder-fixtures/A/clip1.mp4";
    property var decoders: ["FFmpeg:threads=2", "NonexistentDecoderForRegression", "", "FFmpeg:avformat_options=start_number=1", "headers:X-Gyroflow-Test: preview"];
    property int step: -1;
    property int frames: 0;
    property double started: Date.now();
    property bool finished: false;
    property bool playStarted: false;

    function load() {
        started = Date.now();
        frames = 0;
        playStarted = false;
        console.warn("DECODER_SMOKE_LOAD", step, decoders[step]);
        video.setUrl(clip, decoders[step]);
    }
    function end(ok, reason) {
        finished = true;
        if (video) video.pause();
        if (applicationWindow) applicationWindow.closeConfirmed = true;
        console.warn(ok ? "DECODER_SMOKE_PASS" : "DECODER_SMOKE_FAIL", reason);
        Qt.exit(ok ? 0 : 1);
    }
    Connections {
        target: test.video;
        function onCurrentFrameChanged() { if (Date.now() - test.started > 400) test.frames++; }
    }
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/src/ui/main_window.qml");
        applicationWindow = component.createObject(null);
        applicationWindow.title = "Decoder regression test";
        applicationWindow.hide();
        Qt.callLater(function() { applicationWindow.show(); });
    }
    Timer {
        interval: 100;
        repeat: true;
        running: !test.finished;
        onTriggered: {
            if (test.step < 0) {
                const app = test.applicationWindow ? test.applicationWindow.getApp() : null;
                if (Date.now() - test.started > 25000) { test.end(false, "initial load timeout"); return; }
                if (!app || !app.advanced || !app.sync || !app.stab || !app.videoArea.queue || Date.now() - test.started < 6000) return;
                app.onboardingActive = true;
                if (!test.loadingInitialClip) {
                    test.applicationWindow.showNormal();
                    test.video = app.videoArea.vid;
                    console.warn("DECODER_SMOKE_WINDOW", test.applicationWindow.visibility, test.applicationWindow.width, test.applicationWindow.height, video.width, video.height, video.visible, video.opacity);
                    video.visible = true;
                    video.opacity = 1;
                    app.videoArea.loadFile(test.clip, true, 0, "", true);
                    test.loadingInitialClip = true;
                    return;
                }
                if (app.videoArea.previewLoading || app.videoArea.videoLoader.active || !test.video.loaded) return;
                app.videoArea.stabEnabledBtn.checked = false;
                test.step = 0;
                test.load();
                return;
            }
            if (!test.playStarted && Date.now() - test.started > 500) {
                video.play();
                test.playStarted = true;
            }
            if (Date.now() - test.started < 2600) return;
            const valid = test.step !== 1;
            console.warn("DECODER_SMOKE_RESULT", test.step, test.frames, video.videoWidth, video.videoHeight);
            if (valid && (test.frames < 3 || video.videoWidth !== 640 || video.videoHeight !== 360)) {
                test.end(false, "valid decoder did not play at step " + test.step);
                return;
            }
            if (!valid && test.frames > 0) {
                test.end(false, "the explicit decoder was ignored");
                return;
            }
            if (++test.step === test.decoders.length) test.end(true, "explicit selection, failure recovery, container options and headers");
            else test.load();
        }
    }
}
