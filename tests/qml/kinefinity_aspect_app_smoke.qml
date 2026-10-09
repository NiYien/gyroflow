// SPDX-License-Identifier: GPL-3.0-or-later
// Deploy as target/release/ui/main_window.qml. Fixtures live in target/anamorphic-clip-inspection.
import QtQuick

Item {
    id: smoke
    property var applicationWindow: null
    property int stage: 0
    property int ticks: 0
    property int settle: 0
    property bool completed: false
    property bool capturing: false
    readonly property url fixtureRoot: Qt.resolvedUrl("../../../target/anamorphic-clip-inspection/")

    function check(ok, message) {
        if (ok) return;
        completed = true;
        console.warn("KINEFINITY_ASPECT_FAIL", stage, message);
        if (applicationWindow) applicationWindow.closeConfirmed = true;
        Qt.exit(1);
        throw new Error(message);
    }
    function next() { stage++; settle = 8; }
    function capture(video, name, size) {
        capturing = true;
        check(video.vid.grabToImage(function(result) {
            const path = fixtureRoot.toString().replace("file:///", "") + name + ".png";
            check(result.saveToFile(path), "save " + name);
            console.warn("KINEFINITY_ASPECT_CAPTURE", name, video.vid.videoWidth, video.vid.videoHeight,
                         video.vid.surfaceWidth, video.vid.surfaceHeight, video.vid.timestamp);
            capturing = false;
            next();
        }, size), "grab " + name);
    }
    Component.onCompleted: {
        const component = Qt.createComponent(Qt.resolvedUrl("../../../src/ui/main_window.qml"));
        check(component.status === Component.Ready, component.errorString());
        applicationWindow = component.createObject(null);
        applicationWindow.showNormal();
        applicationWindow.requestUpdate();
    }
    Timer {
        interval: 250; repeat: true; running: !smoke.completed
        onTriggered: {
            if (++smoke.ticks > 360) { smoke.check(false, "timeout"); return; }
            if (smoke.ticks === 4) {
                smoke.applicationWindow.hide();
                smoke.applicationWindow.showNormal();
            }
            if (smoke.capturing || smoke.ticks < 20) return;
            if (smoke.settle > 0) { smoke.settle--; return; }
            const app = smoke.applicationWindow ? smoke.applicationWindow.getApp() : null;
            if (!app || !app.advanced || !app.lensProfile || !app.stab || !app.motionData || !app.exportSettings) return;
            app.onboardingActive = true;
            const video = app.videoArea;
            const ctl = app.controller;
            const rawSize = Qt.size(960, 616);
            if (smoke.stage === 0) {
                ctl.lens_group_manual_edit = false;
                video.loadFile(smoke.fixtureRoot + "input.mov", true, 0, "", true);
                smoke.next();
            } else if (smoke.stage === 1) {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.check(video.detectedCamera.startsWith("Kinefinity"), "source camera identified");
                smoke.check(video.vid.videoWidth === 3840 && video.vid.videoHeight === 2464, "source dimensions");
                video.stabEnabledBtn.checked = false;
                ctl.set_preview_resolution(616, video.vid);
                video.vid.pause();
                video.vid.seekToTimestamp(2000, true);
                smoke.next();
            } else if (smoke.stage === 2) {
                if (Math.abs(video.vid.timestamp - 2000) > 45) return;
                smoke.capture(video, "fixed-source", rawSize);
            } else if (smoke.stage === 3) {
                video.vid.setSourceAspectRatio(false);
                smoke.next();
            } else if (smoke.stage === 4) {
                smoke.capture(video, "display-sar-baseline", rawSize);
            } else if (smoke.stage === 5) {
                video.vid.setSourceAspectRatio(true);
                smoke.next();
            } else if (smoke.stage === 6) {
                smoke.capture(video, "fixed-paused", rawSize);
            } else if (smoke.stage === 7) {
                ctl.set_preview_resolution(308, video.vid);
                video.vid.seekToTimestamp(2000, true);
                smoke.next();
            } else if (smoke.stage === 8) {
                smoke.check(video.vid.videoWidth === 3840 && video.vid.videoHeight === 2464, "preview resize keeps source dimensions");
                smoke.capture(video, "fixed-small", rawSize);
            } else if (smoke.stage === 9) {
                ctl.load_lens_profile(smoke.fixtureRoot + "input-lens.json");
                ctl.set_lens_group_config(JSON.stringify([{lens_index: 0, focal_length_mm: 35,
                    anamorphic_enabled: true, preset_id: "blazar_apec_35mm_1_33x", squeeze_direction: "horizontal"}]));
                ctl.lens_group_manual_edit = true;
                const outputJson = ctl.apply_lens_group_to_main(0);
                smoke.check(!!outputJson, "fixture lens provides camera geometry");
                const output = JSON.parse(outputJson);
                smoke.check(output.w === 5106 && output.h === 2464, "preset desqueezes once");
                app.exportSettings.setDefaultSize(output.w, output.h);
                video.stabEnabledBtn.checked = true;
                ctl.set_preview_pipeline(0);
                ctl.set_preview_resolution(616, video.vid);
                smoke.next();
            } else if (smoke.stage === 10) {
                ctl.export_gyroflow_file(smoke.fixtureRoot + "preset.gyroflow", "Simple", {});
                smoke.capture(video, "preset-pipeline-0", Qt.size(1277, 616));
            } else if (smoke.stage === 11) {
                ctl.set_preview_pipeline(1);
                video.vid.forceRedraw();
                smoke.next();
            } else if (smoke.stage === 12) {
                smoke.capture(video, "preset-pipeline-1", Qt.size(1277, 616));
            } else if (smoke.stage === 13) {
                ctl.set_preview_pipeline(2);
                // The CPU readback fallback starts after multiple texture attempts.
                video.vid.play();
                smoke.next();
            } else if (smoke.stage === 14) {
                if (video.vid.playing) {
                    video.vid.pause();
                    video.vid.seekToTimestamp(2000, true);
                    smoke.settle = 8;
                    return;
                }
                if (Math.abs(video.vid.timestamp - 2000) > 45) return;
                smoke.capture(video, "preset-pipeline-2", Qt.size(1277, 616));
            } else if (smoke.stage === 15) {
                ctl.lens_group_manual_edit = false;
                video.loadFile(smoke.fixtureRoot + "control.mov", true, 0, "", true);
                smoke.next();
            } else if (smoke.stage === 16) {
                if (video.previewLoading || video.defaultPreviewPending || video.videoLoader.active) return;
                smoke.check(!video.detectedCamera.startsWith("Kinefinity"), "control is another camera");
                video.stabEnabledBtn.checked = false;
                ctl.set_preview_resolution(246, video.vid);
                video.vid.pause();
                video.vid.seekToTimestamp(0, true);
                smoke.next();
            } else if (smoke.stage === 17) {
                smoke.capture(video, "control-default", Qt.size(384, 246));
            } else if (smoke.stage === 18) {
                video.vid.setSourceAspectRatio(false);
                smoke.next();
            } else if (smoke.stage === 19) {
                smoke.capture(video, "control-reset", Qt.size(384, 246));
            } else {
                console.warn("KINEFINITY_ASPECT_PASS source dimensions, paused redraw, resize, preset, three pipelines, camera switch");
                smoke.completed = true;
                smoke.applicationWindow.closeConfirmed = true;
                Qt.quit();
            }
        }
    }
}
