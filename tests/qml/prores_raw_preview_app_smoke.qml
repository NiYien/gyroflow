// SPDX-License-Identifier: GPL-3.0-or-later
// Supply the first clip through --open; expected entries are [width, height, frames, fps].
import QtQuick
Item {
    id: test
    property var clips: []
    property var expected: []
    property var applicationWindow: null
    property var video: null
    property int clip: 0
    property int stage: 0
    property int frameEvents: 0
    property int stageFrames: 0
    property double started: Date.now()
    property double stageStarted: Date.now()
    property bool finished: false

    function nextStage(value) { stage = value; stageStarted = Date.now(); stageFrames = frameEvents }
    function end(ok, reason) {
        finished = true
        console.warn(ok ? "PRAW_PREVIEW_PASS" : "PRAW_PREVIEW_FAIL", clip, stage, reason)
        if (video) video.pause()
        if (applicationWindow) applicationWindow.closeConfirmed = true
        Qt.exit(ok ? 0 : 1)
    }
    Component.onCompleted: {
        applicationWindow = Qt.createComponent("qrc:/src/ui/main_window.qml").createObject(null)
        applicationWindow.title = "Automatic ProRes RAW preview regression"
        applicationWindow.showNormal()
    }
    Connections {
        target: test.video
        function onCurrentFrameChanged() { test.frameEvents++ }
    }
    Timer {
        interval: 50; repeat: true; running: !test.finished
        onTriggered: {
            if (Date.now() - started > 180000) { end(false, "overall timeout"); return }
            const app = applicationWindow ? applicationWindow.getApp() : null
            if (!app || !app.videoArea || !app.videoArea.vid) return
            const area = app.videoArea
            if (!video) {
                if (!clips.length || clips.length !== expected.length) { end(false, "missing clip expectations"); return }
                video = area.vid
                app.controller.set_preview_resolution(720, video)
                console.warn("PRAW_PREVIEW_LOAD", clip, clips[clip])
                nextStage(1)
            }
            const elapsed = Date.now() - stageStarted
            if (stage === 1) {
                if (elapsed > 20000) { end(false, "load timeout"); return }
                if (!video.loaded || area.previewLoading || area.videoLoader.active) return
                const info = expected[clip]
                if (video.videoWidth !== info[0] || video.videoHeight !== info[1]
                    || video.frameCount !== info[2] || Math.abs(video.frameRate - info[3]) > 0.01) {
                    end(false, "source metadata mismatch: " + [video.videoWidth, video.videoHeight, video.frameCount, video.frameRate].join(","))
                    return
                }
                console.warn("PRAW_PREVIEW_LOADED", clip, video.videoWidth, video.videoHeight, video.frameCount)
                video.play()
                nextStage(2)
            } else if (stage === 2 && elapsed > 700) {
                if (frameEvents - stageFrames < 3) {
                    if (elapsed > 6000) end(false, "playback did not advance")
                    return
                }
                video.pause()
                nextStage(21)
            } else if (stage === 21) {
                if (video.playing) {
                    if (elapsed > 3000) end(false, "pause did not settle")
                    return
                }
                video.timestamp = video.duration * 0.5
                nextStage(3)
            } else if (stage === 3) {
                if (Math.abs(video.timestamp - video.duration * 0.5) > 150 || video.playing) {
                    if (elapsed > 6000) end(false, "paused seek did not settle: " + [video.timestamp, video.duration, video.playing, video.buffering].join(","))
                    return
                }
                video.setProperty("scale", "640x480")
                app.controller.set_preview_resolution(480, video)
                nextStage(4)
            } else if (stage === 4 && elapsed > 250) {
                video.play()
                nextStage(5)
            } else if (stage === 5 && elapsed > 500) {
                if (frameEvents - stageFrames < 3) {
                    if (elapsed > 6000) end(false, "playback after resize did not advance")
                    return
                }
                video.pause()
                console.warn("PRAW_PREVIEW_CLIP_PASS", clip, clips[clip], frameEvents - stageFrames)
                if (++clip === clips.length) { end(true, "load, playback, seek, resize and mixed-format switches"); return }
                app.controller.set_preview_resolution(720, video)
                console.warn("PRAW_PREVIEW_LOAD", clip, clips[clip])
                area.loadFile(clips[clip], true, 0, "", true)
                nextStage(1)
            }
        }
    }
}
