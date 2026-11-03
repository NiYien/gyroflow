// SPDX-License-Identifier: GPL-3.0-or-later
// Run beside an isolated application executable as ui/main_window.qml.
import QtQuick
import QtQml.Models
Item {
    id: test
    property var applicationWindow: null
    property int stage: 0
    property double started: Date.now()
    property double stageStarted: Date.now()
    property bool completed: false
    property bool sawEnd: false
    property bool savedImage: false
    property string thumbnail: ""
    property double jobId: 0
    property url source: "file:///D:/下载视频未分类/FX5 X-OCN C2/A092C087_2609131L.mxf"
    property url ordinary: "file:///C:/Users/Jhe/Desktop/github/gyroflow/target/mobile-folder-fixtures/A/clip1.mp4"
    Image { id: thumbnailImage; visible: false; source: test.thumbnail }
    Instantiator {
        model: render_queue.queue
        delegate: QtObject {
            property string thumbnailUrl: thumbnail_url
            onThumbnailUrlChanged: if (thumbnailUrl.length) test.thumbnail = thumbnailUrl
        }
    }
    function check(value, message) {
        if (value) return true;
        console.warn("XOCN_APP_FAIL", stage, message);
        completed = true;
        if (applicationWindow) applicationWindow.closeConfirmed = true;
        Qt.exit(1);
        return false;
    }
    function advance(value) { stage = value; stageStarted = Date.now(); }
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/src/ui/main_window.qml");
        if (!check(component.status === Component.Ready, component.errorString())) return;
        applicationWindow = component.createObject(null);
        applicationWindow.title = "【自动验收，请勿操作】FX5 X-OCN";
        applicationWindow.width = 1280;
        applicationWindow.height = 800;
        applicationWindow.hide();
        Qt.callLater(function() { test.applicationWindow.show(); });
    }
    Timer {
        interval: 100; repeat: true; running: !test.completed
        onTriggered: {
            if (Date.now() - test.started > 150000) { test.check(false, "overall timeout"); return; }
            const app = test.applicationWindow ? test.applicationWindow.getApp() : null;
            if (!app || !app.advanced || !app.videoArea.queue || Date.now() - test.started < 6000) return;
            app.onboardingActive = true;
            const area = app.videoArea;
            const video = area.vid;
            const elapsed = Date.now() - test.stageStarted;
            if (elapsed > 25000) { test.check(false, "stage timeout timestamp=" + video.timestamp); return; }
            if (test.stage === 0) {
                video.muted = true;
                area.loadFile(test.source, true, 0, "", true);
                test.advance(1);
            } else if (test.stage === 1) {
                if (area.previewLoading || area.videoLoader.active || !video.loaded) return;
                if (!test.check(video.videoWidth === 4992 && video.videoHeight === 3328 && video.frameCount === 352 && Math.abs(video.frameRate - 50) < 0.001 && Math.abs(video.duration - 7040) < 1,
                        "source parameters: " + [video.videoWidth, video.videoHeight, video.frameCount, video.frameRate, video.duration])) return;
                if (!test.check(app.controller.is_native_xocn(test.source) && render_queue.is_plugin_only_video(test.source.toString()), "X-OCN routing and project-only workflow")) return;
                if (!test.check(app.controller.gyro_loaded && app.controller.gyro_has_raw_imu, "embedded motion data was not loaded")) return;
                area.stabEnabledBtn.checked = false;
                video.seekToTimestamp(3000, true);
                test.advance(2);
            } else if (test.stage === 2) {
                if (Math.abs(video.timestamp - 3000) > 40) return;
                video.grabToImage(function(result) { test.savedImage = result.saveToFile("C:/Users/Jhe/Desktop/github/gyroflow/target/fx5-xocn-investigation/app-preview.png"); });
                video.play();
                test.advance(3);
            } else if (test.stage === 3) {
                if (video.timestamp < 3500) return;
                video.pause();
                video.seekToTimestamp(400, true);
                test.advance(4);
            } else if (test.stage === 4) {
                if (Math.abs(video.timestamp - 400) > 40) return;
                video.seekToTimestamp(6800, true);
                test.advance(5);
            } else if (test.stage === 5) {
                if (Math.abs(video.timestamp - 6800) > 40) return;
                test.sawEnd = false;
                video.play();
                test.advance(6);
            } else if (test.stage === 6) {
                if (video.timestamp >= 6800) test.sawEnd = true;
                if (!test.sawEnd || elapsed < 600 || video.timestamp > 3000) return;
                video.pause();
                video.seekToTimestamp(0, true);
                test.jobId = render_queue.add_file(test.source.toString(), "", JSON.stringify({output: {output_folder: "file:///C:/Users/Jhe/Desktop/github/gyroflow/target/fx5-xocn-investigation/", output_filename: "source-no-render.mp4"}}));
                if (!test.check(test.jobId > 0, "queue rejected source")) return;
                test.advance(7);
            } else if (test.stage === 7) {
                const text = render_queue.get_gyroflow_data(test.jobId);
                if (!text || !test.savedImage || thumbnailImage.status !== Image.Ready) return;
                const data = JSON.parse(text);
                if (!test.check(data.video_info.width === 4992 && data.video_info.height === 3328 && data.video_info.num_frames === 352, "queue source dimensions changed")) return;
                if (!test.check(data.videofile.toLowerCase().endsWith(".mxf") && render_queue.is_job_plugin_only(test.jobId), "project lost original MXF")) return;
                console.warn("XOCN_APP_QUEUE", JSON.stringify(data.video_info), "thumbnail", thumbnailImage.sourceSize.width, thumbnailImage.sourceSize.height);
                area.loadGyroflowData(data, test.jobId);
                test.advance(8);
            } else if (test.stage === 8) {
                if (area.previewLoading || area.videoLoader.active || !video.loaded) return;
                area.loadFile(test.ordinary, true, 0, "", true);
                test.advance(9);
            } else if (test.stage === 9) {
                if (area.previewLoading || area.videoLoader.active || !video.loaded) return;
                if (!test.check(video.videoWidth === 640 && video.videoHeight === 360 && !app.controller.is_native_xocn(test.ordinary) && !render_queue.is_plugin_only_video(test.ordinary.toString()), "ordinary video changed after X-OCN")) return;
                video.play();
                test.advance(10);
            } else if (test.stage === 10 && elapsed > 1000) {
                if (!test.check(video.currentFrame >= 3, "ordinary playback did not resume")) return;
                console.warn("XOCN_APP_PASS load, motion data, source geometry, seeks, playback, loop, thumbnail, queue, project reload and ordinary video");
                test.completed = true;
                test.applicationWindow.closeConfirmed = true;
                Qt.quit();
            }
        }
    }
}
