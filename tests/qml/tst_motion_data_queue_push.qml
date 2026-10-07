// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/menu" as Menu

// What the experimental panel pushes to the render queue after a clip or a project is loaded.
// Run: ext/6.7.3/mingw_64/bin/qmltestrunner.exe -platform offscreen -input tests/qml/tst_motion_data_queue_push.qml
TestCase {
    id: testCase;
    name: "MotionDataQueuePush";
    when: windowShown;
    visible: true;
    width: 400;
    height: 800;

    property real dpiScale: 1;
    property string styleFont: "Arial";
    property string style: "dark";
    property color styleButtonColor: "#333333";
    property color styleTextColor: "#ffffff";
    property color styleTextColorOnAccent: "#ffffff";
    property color styleAccentColor: "#3a9cde";
    property color styleBackground: "#111111";
    property color styleBackground2: "#222222";
    property color styleHighlightColor: "#666666";
    property color stylePopupBorder: "#555555";
    property color styleSliderHandle: "#888888";
    property color styleHrColor: "#444444";
    property color styleVideoBorderColor: "#444444";
    property bool isMobile: false;
    property bool animationsEnabled: false;
    property bool isLandscape: true;
    property bool isSimpleMode: false;

    QtObject {
        id: settingsStub;
        function value(key, def) { return def; }
        function setValue(key, value) { }
    }
    property alias settings: settingsStub;

    QtObject {
        id: controllerStub;
        signal telemetry_loaded(bool is_main_video, string filename, string camera, var additional_data);
        signal chart_data_changed();
        property bool loading_gyro_in_progress: false;
        property bool video_loading_in_progress: false;
        property bool gyro_has_raw_imu: true;
        property bool gyro_has_quaternions: false;
        property bool gyro_has_accurate_timestamps: false;
        // What the core reports: a clip without a correction result reports no strength
        property var opticalInfo: ({ "available": false, "ignore_file_motion": false, "has_motion": true });
        function optical_correction_info() { return JSON.stringify(opticalInfo); }
        function translation_stabilization_info() {
            return JSON.stringify({ "requested": true, "reference": 1.0, "smoothness_s": 1.0, "along_axis": true, "has_motion": true });
        }
        function stab_reconstruction_info() { return JSON.stringify({ "requested": false, "has_motion": true }); }
        function set_integration_method(index) { }
        function set_imu_lpf(v) { }
        function set_imu_median_filter(v) { }
        function set_imu_rotation(p, r, y) { }
        function set_acc_rotation(p, r, y) { }
        function set_imu_orientation(v) { }
        function set_imu_bias(x, y, z) { }
        function set_glitch_filter(enabled, strength) { }
        function recompute_gyro() { }
        function load_telemetry() { }
        function quats_at_timestamp(ts) { return [1, 0, 0, 0]; }
        function mesh_at_frame(frame) { return []; }
    }
    property alias controller: controllerStub;

    QtObject {
        id: renderQueueStub;
        property var pushed: [];
        function set_jobs_optical_settings(json) { pushed = pushed.concat([json]); }
    }
    property alias render_queue: renderQueueStub;

    QtObject {
        id: windowStub;
        property var videoArea: QtObject {
            property url loadedFileUrl: "";
            property int outWidth: 3840;
            property int outHeight: 2160;
            property var vid: QtObject { property bool loaded: true; property int currentFrame: 0; property int videoWidth: 3840; property int videoHeight: 2160; }
            property var timeline: QtObject { function updateDurations() { } }
            property var statistics: QtObject { property bool active: false; property var item: null; }
        }
        function saveProject() { }
    }
    property alias window: windowStub;

    QtObject {
        id: filesystemStub;
        function get_file_url(a, b, c) { return ""; }
        function get_filename(a) { return ""; }
        function get_folder(a) { return ""; }
    }
    property alias filesystem: filesystemStub;

    Component { id: factory; Menu.MotionData { } }
    property var motion;

    function lastPush(): var {
        verify(renderQueueStub.pushed.length > 0, "the panel pushed its settings");
        return JSON.parse(renderQueueStub.pushed[renderQueueStub.pushed.length - 1]);
    }

    function init() {
        renderQueueStub.pushed = [];
        controllerStub.opticalInfo = { "available": false, "ignore_file_motion": false, "has_motion": true };
        motion = createTemporaryObject(factory, testCase, { width: 380 });
        verify(motion !== null);
    }

    function test_a_clip_without_a_correction_keeps_the_strength_a_number() {
        // Loading a clip refreshes the panel from the core, which has no correction to report a strength for
        motion.refreshOpticalInfo(true, false);
        motion.sendQueueOpticalSettings();
        const pushed = lastPush();
        verify(typeof pushed.strength === "number" && isFinite(pushed.strength), "strength pushed as " + JSON.stringify(pushed.strength));
        compare(pushed.strength, 0.5, "the slider keeps its value");
        compare(pushed.translation, true);
    }

    function test_a_correction_brings_its_strength() {
        controllerStub.opticalInfo = { "available": true, "enabled": true, "strength": 0.3, "ignore_file_motion": false, "has_motion": true };
        motion.refreshOpticalInfo(true, false);
        motion.sendQueueOpticalSettings();
        compare(lastPush().strength, 0.3);
        // A later clip without one keeps it
        controllerStub.opticalInfo = { "available": false, "ignore_file_motion": false, "has_motion": true };
        motion.refreshOpticalInfo(true, false);
        motion.sendQueueOpticalSettings();
        compare(lastPush().strength, 0.3);
    }
}
