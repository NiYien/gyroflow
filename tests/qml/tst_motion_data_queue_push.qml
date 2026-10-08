// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/menu" as Menu

// What the experimental panel pushes to the render queue after a clip or a project is loaded.
// Run with QML_XHR_ALLOW_FILE_READ=1: ext/6.7.3/mingw_64/bin/qmltestrunner.exe -platform offscreen -input tests/qml/tst_motion_data_queue_push.qml
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
        signal optical_correction_changed();
        property bool loading_gyro_in_progress: false;
        property bool video_loading_in_progress: false;
        property bool gyro_has_raw_imu: true;
        property bool gyro_has_quaternions: false;
        property bool gyro_has_accurate_timestamps: false;
        // What the core reports: a clip without a correction result reports no strength
        property var opticalInfo: ({ "available": false, "ignore_file_motion": false, "has_motion": true });
        property bool translationRequested: true;
        property bool reconstructionRequested: false;
        function optical_correction_info() { return JSON.stringify(opticalInfo); }
        function translation_stabilization_info() {
            return JSON.stringify({ "requested": translationRequested, "reference": 1.0, "smoothness_s": 1.0, "along_axis": true, "has_motion": true });
        }
        function stab_reconstruction_info() { return JSON.stringify({ "requested": reconstructionRequested, "has_motion": true }); }
        function set_stab_reconstruction_enabled(enabled) {
            reconstructionRequested = enabled;
            if (enabled) translationRequested = false;
            optical_correction_changed();
        }
        function set_optical_correction_enabled(enabled) { }
        function set_ignore_file_motion(enabled) { }
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
        property var processingSettings: null;
        function set_jobs_optical_settings(json) { pushed = pushed.concat([json]); }
        function dispatch_blocker_reason(intent) { return ""; }
        function get_anamorphic_applied_count() { return 0; }
        function has_crm_proxy_jobs() { return false; }
        property int export_project: 0;
        function prepare_finished_jobs_for_video_export() { }
        function start_batch_autosync() { processingSettings = JSON.parse(pushed[pushed.length - 1]); }
        function start() { start_batch_autosync(); }
    }
    property alias render_queue: renderQueueStub;

    QtObject {
        id: windowStub;
        property bool isSimpleMode: true;
        property bool useMobileWorkspace: false;
        property var motionData: testCase.motion;
        function runQueueOutputAction(callback) { callback(); }
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
    property string appSource;

    function initTestCase() {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/App.qml"), false);
        request.send();
        appSource = request.responseText.replace(/\r\n/g, "\n");
        verify(appSource.length > 0);
    }

    function dispatchFromApp(name) {
        // Run the real processing entry point with the panel and a queue that records its settings at dispatch.
        const start = appSource.indexOf("function " + name + "(): void {");
        verify(start >= 0);
        const bodyStart = appSource.indexOf("{", start) + 1;
        const end = appSource.indexOf("\n    }", bodyStart);
        const action = new Function("window", "render_queue", "videoArea", "lensDataGatePasses", "queueVideoOutputFolderGatePasses", appSource.slice(bodyStart, end));
        action(windowStub, renderQueueStub, {}, function() { return true; }, function() { return true; });
    }

    function lastPush(): var {
        verify(renderQueueStub.pushed.length > 0, "the panel pushed its settings");
        return JSON.parse(renderQueueStub.pushed[renderQueueStub.pushed.length - 1]);
    }

    function init() {
        renderQueueStub.pushed = [];
        renderQueueStub.processingSettings = null;
        windowStub.isSimpleMode = true;
        windowStub.useMobileWorkspace = false;
        controllerStub.translationRequested = true;
        controllerStub.reconstructionRequested = false;
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

    function test_processing_after_reload_uses_the_visible_choices_data() {
        return [
            { tag: "stabilize-off", action: "runSimpleBatchSync", translation: false },
            { tag: "export-off", action: "runSimpleBatchExport", translation: false },
            { tag: "stabilize-translation", action: "runSimpleBatchSync", translation: true },
            { tag: "export-translation", action: "runSimpleBatchExport", translation: true }
        ];
    }

    function test_processing_after_reload_uses_the_visible_choices(data) {
        motion.changeOpticalMode("stab", true);
        tryVerify(function() { return renderQueueStub.pushed.length > 0; });
        compare(lastPush().reconstruction, true);
        const edits = renderQueueStub.pushed.length;

        // Reloading a clip turns off the preview's reconstruction; browsing alone must not broadcast it.
        controllerStub.reconstructionRequested = false;
        controllerStub.translationRequested = data.translation;
        motion.restoreOpticalControls(false);
        wait(0);
        compare(renderQueueStub.pushed.length, edits);
        compare(lastPush().reconstruction, true);

        dispatchFromApp(data.action);
        verify(renderQueueStub.processingSettings !== null);
        compare(renderQueueStub.processingSettings.correction, false);
        compare(renderQueueStub.processingSettings.translation, data.translation);
        compare(renderQueueStub.processingSettings.reconstruction, false);
    }

    function test_processing_without_panel_edits_does_not_override_jobs() {
        motion.restoreOpticalControls(false);
        motion.syncQueueOpticalSettingsForProcessing();
        compare(renderQueueStub.pushed.length, 0);
    }

    function test_processing_in_other_modes_keeps_its_existing_scope_data() {
        return [
            { tag: "full", simple: false, mobile: false },
            { tag: "mobile", simple: true, mobile: true }
        ];
    }

    function test_processing_in_other_modes_keeps_its_existing_scope(data) {
        motion.changeOpticalMode("stab", true);
        tryVerify(function() { return renderQueueStub.pushed.length > 0; });
        const edits = renderQueueStub.pushed.length;
        controllerStub.reconstructionRequested = false;
        motion.restoreOpticalControls(false);
        windowStub.isSimpleMode = data.simple;
        windowStub.useMobileWorkspace = data.mobile;
        motion.syncQueueOpticalSettingsForProcessing();
        compare(renderQueueStub.pushed.length, edits);
        compare(lastPush().reconstruction, true);
    }

    function test_unticking_before_the_deferred_push_reaches_processing() {
        motion.changeOpticalMode("stab", true);
        tryVerify(function() { return renderQueueStub.pushed.length > 0; });
        motion.changeOpticalMode("stab", false);
        dispatchFromApp("runSimpleBatchSync");
        compare(renderQueueStub.processingSettings.reconstruction, false);
    }
}
