// SPDX-License-Identifier: GPL-3.0-or-later
// Run beside an isolated executable as ui/main_window.qml, with a sibling
// crm-test-config.json specifying the source, dimensions and optional gyro.
import QtQuick
import QtQml.Models
Item {
    id: smoke
    property var config: null
    property var applicationWindow: null
    property int stage: 0
    property int ticks: 0
    property int stageTicks: 0
    property int jobId: 0
    property bool completed: false
    property bool savedImage: false
    property bool thumbnailSeen: false
    property bool deepDone: false
    property bool deepSucceeded: false
    property double deepOffsetMs: 0
    property string deepReason: ""
    Instantiator {
        model: render_queue.queue
        delegate: QtObject {
            property string thumbnail: thumbnail_url
            onThumbnailChanged: if (thumbnail.indexOf("data:image/jpg;base64,") === 0 && thumbnail.length > 100) smoke.thumbnailSeen = true
        }
    }
    Connections {
        target: render_queue
        function onDeep_match_finished(job_id, success, error_kind, offset_ms) {
            if (job_id !== smoke.jobId) return;
            smoke.deepReason = error_kind.toString();
            smoke.deepSucceeded = success;
            smoke.deepOffsetMs = offset_ms;
            smoke.deepDone = true;
            console.warn("CRM_GEOMETRY_DEEP_RESULT", success, error_kind, offset_ms);
        }
    }
    function check(value, message) {
        if (!value) {
            completed = true;
            console.warn("CRM_GEOMETRY_APP_FAIL", stage, message);
            if (applicationWindow) applicationWindow.closeConfirmed = true;
            Qt.exit(1);
        }
        return value;
    }
    function advance() { stage++; stageTicks = 0; }
    Component.onCompleted: {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../crm-test-config.json"), false);
        request.send();
        config = JSON.parse(request.responseText);
        const component = Qt.createComponent("qrc:/src/ui/main_window.qml");
        if (!check(component.status === Component.Ready, component.errorString())) return;
        applicationWindow = component.createObject(null);
        applicationWindow.title = "CRM automated verification";
        applicationWindow.width = 1280;
        applicationWindow.height = 800;
        applicationWindow.hide();
        Qt.callLater(function() { smoke.applicationWindow.show(); });
    }
    Timer {
        interval: 100; repeat: true; running: !smoke.completed
        onTriggered: {
            if (++smoke.ticks > 2400) { smoke.check(false, "timeout"); return; }
            const app = smoke.applicationWindow ? smoke.applicationWindow.getApp() : null;
            if (app && app.isSimpleMode) app.isSimpleMode = false;
            if (smoke.ticks < 30 || !app || !app.advanced || !app.videoArea.queue || !app.motionData) return;
            app.onboardingActive = true;
            const video = app.videoArea;
            smoke.stageTicks++;
            if (smoke.stageTicks % 50 === 0) console.warn("CRM_GEOMETRY_APP_WAIT", smoke.stage, video.vid.timestamp);
            if (smoke.stage === 0) {
                video.loadMultipleFiles([smoke.config.crm], true);
                smoke.advance();
            } else if (smoke.stage === 1) {
                if (video.previewLoading || video.videoLoader.active || smoke.stageTicks < 10) return;
                if (!smoke.check(video.vid.videoWidth === smoke.config.width && video.vid.videoHeight === smoke.config.height, "source geometry")) return;
                if (!smoke.check(video.vid.frameCount === smoke.config.frames, "source frame count")) return;
                if (smoke.config.exportGyroUrl) {
                    app.controller.export_gyro_data(smoke.config.exportGyroUrl, {
                        export_all_samples: true,
                        original: { gyroscope: true, accelerometer: true },
                        stabilized: {}, zooming: {}
                    });
                }
                video.vid.seekToTimestamp(smoke.config.seekMs, true);
                smoke.advance();
            } else if (smoke.stage === 2) {
                if (Math.abs(video.vid.timestamp - smoke.config.seekMs) > 40 || smoke.stageTicks < 5) return;
                video.vid.grabToImage(function(result) {
                    smoke.savedImage = result.saveToFile(smoke.config.previewPng);
                });
                if (smoke.config.playback) video.vid.play();
                smoke.advance();
            } else if (smoke.stage === 3) {
                if (!smoke.savedImage) return;
                if (smoke.config.playback && video.vid.timestamp < (smoke.config.playUntilMs || smoke.config.seekMs + 500)) return;
                console.warn("CRM_GEOMETRY_PREVIEW_PASS", video.vid.timestamp);
                video.vid.pause();
                if (smoke.config.backwardSeekMs !== undefined) video.vid.seekToTimestamp(smoke.config.backwardSeekMs, true);
                smoke.jobId = render_queue.add_file(smoke.config.crm, "", app.getAdditionalProjectDataJson());
                if (!smoke.check(smoke.jobId > 0, "queue entry")) return;
                smoke.advance();
            } else if (smoke.stage === 4) {
                if (smoke.config.backwardSeekMs !== undefined && Math.abs(video.vid.timestamp - smoke.config.backwardSeekMs) > 40) return;
                if (!render_queue.get_gyroflow_data(smoke.jobId) || !smoke.thumbnailSeen) return;
                console.warn("CRM_GEOMETRY_THUMBNAIL_PASS", smoke.jobId);
                if (smoke.config.gyro) render_queue.add_gyro_file(smoke.config.gyro);
                smoke.advance();
            } else if (smoke.stage === 5) {
                if (smoke.config.gyro) {
                    const info = JSON.parse(render_queue.get_gyro_file_info_json(0));
                    if (!info.parsed) return;
                    const result = render_queue.start_deep_gyro_match(smoke.jobId, 0, -1).toString();
                    if (!smoke.check(result === "ok", "deep start: " + result)) return;
                } else smoke.deepDone = true;
                smoke.advance();
            } else {
                if (!smoke.deepDone) return;
                if (!smoke.check(["", "not_in_range", "low_motion"].indexOf(smoke.deepReason) >= 0, "deep result: " + smoke.deepReason)) return;
                if (smoke.config.requireDeepSuccess && !smoke.check(smoke.deepSucceeded, "expected a matching gyro stream")) return;
                if (smoke.config.requireDeepSuccess && !smoke.check(Math.abs(smoke.deepOffsetMs - smoke.config.expectedOffsetMs) <= smoke.config.offsetToleranceMs, "known sync offset")) return;
                if (smoke.config.requireDeepFailure && !smoke.check(!smoke.deepSucceeded, "unrelated gyro must not match")) return;
                console.warn("CRM_GEOMETRY_APP_PASS", smoke.config.crm, "dimensions, preview, seek, thumbnail, deep=" + smoke.deepReason);
                smoke.completed = true;
                smoke.applicationWindow.closeConfirmed = true;
                Qt.quit();
            }
        }
    }
}
