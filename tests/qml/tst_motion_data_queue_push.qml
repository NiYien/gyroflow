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
        property var values: ({});
        function value(key, def) { return values.hasOwnProperty(key) ? values[key] : def; }
        function setValue(key, value) { values[key] = value; }
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
        property real translationReference: 1.0;
        property real translationSmoothness: 1.0;
        property bool translationAlongAxis: true;
        property bool translationAuto: true;
        property bool reconstructionRequested: false;
        function optical_correction_info() { return JSON.stringify(opticalInfo); }
        function translation_stabilization_info() {
            return JSON.stringify({ "requested": translationRequested, "reference": translationReference,
                "smoothness_s": translationSmoothness, "along_axis": translationAlongAxis,
                "auto": translationAuto, "has_motion": true });
        }
        function stab_reconstruction_info() { return JSON.stringify({ "requested": reconstructionRequested, "has_motion": true }); }
        function set_stab_reconstruction_enabled(enabled) {
            reconstructionRequested = enabled;
            optical_correction_changed();
        }
        function set_translation_stabilization_enabled(enabled) { translationRequested = enabled; optical_correction_changed(); }
        function set_translation_reference(value) { translationReference = value; optical_correction_changed(); }
        function set_translation_smoothness(value) { translationSmoothness = value; optical_correction_changed(); }
        function set_translation_along_axis(value) { translationAlongAxis = value; optical_correction_changed(); }
        function set_translation_auto(value) { translationAuto = value; optical_correction_changed(); }
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
        settingsStub.values = {};
        renderQueueStub.pushed = [];
        renderQueueStub.processingSettings = null;
        windowStub.isSimpleMode = true;
        windowStub.useMobileWorkspace = false;
        controllerStub.translationRequested = true;
        controllerStub.translationReference = 1.0;
        controllerStub.translationSmoothness = 1.0;
        controllerStub.translationAlongAxis = true;
        controllerStub.translationAuto = true;
        controllerStub.reconstructionRequested = false;
        controllerStub.opticalInfo = { "available": false, "ignore_file_motion": false, "has_motion": true };
        motion = createTemporaryObject(factory, testCase, { width: 380 });
        verify(motion !== null);
    }

    function restartPanel() {
        motion.destroy();
        wait(0);
        controllerStub.translationRequested = false;
        controllerStub.translationReference = 1.0;
        controllerStub.translationSmoothness = 1.0;
        controllerStub.translationAlongAxis = true;
        controllerStub.translationAuto = true;
        renderQueueStub.pushed = [];
        motion = createTemporaryObject(factory, testCase, { width: 380 });
        verify(motion !== null);
        wait(0);
    }

    function controlMatching(item, predicate) {
        if (predicate(item)) return item;
        const children = item.children || [];
        for (let i = 0; i < children.length; ++i) {
            const found = controlMatching(children[i], predicate);
            if (found) return found;
        }
        return null;
    }

    function test_translation_edits_survive_a_restart_and_reach_the_queue() {
        motion.restoreOpticalControls(false);
        motion.changeOpticalMode("translation", false);
        motion.changeOpticalMode("translation", true);
        const automatic = controlMatching(motion.experimentalControls, x => x.text === "Automatic parameters");
        const alongAxis = controlMatching(motion.experimentalControls, x => x.text === "Compensate movement along the lens axis");
        const reference = controlMatching(motion.experimentalControls, x => x.from === 0 && x.to === 200);
        const smoothness = controlMatching(motion.experimentalControls, x => x.from === -1 && x.to === 1);
        verify(automatic && alongAxis && reference && smoothness);
        automatic.checked = false;
        alongAxis.checked = false;
        reference.value = 150;
        smoothness.value = Math.log(2.5) / Math.LN10;
        // Close before the debounce timers fire; the last user choices must still be saved.
        restartPanel();
        compare(controllerStub.translationRequested, true);
        compare(controllerStub.translationAuto, false);
        compare(controllerStub.translationAlongAxis, false);
        compare(controllerStub.translationReference, 0.5);
        fuzzyCompare(controllerStub.translationSmoothness, 2.5, 0.000001);
        compare(lastPush().translation, true);
        compare(lastPush().translation_auto, false);
        compare(lastPush().translation_along_axis, false);
        compare(lastPush().translation_reference, 0.5);
        fuzzyCompare(lastPush().translation_smoothness, 2.5, 0.000001);
        motion.changeOpticalMode("translation", false);
        restartPanel();
        compare(lastPush().translation, false);
        compare(lastPush().translation_reference, 0.5);
    }

    function test_project_readback_wins_without_changing_saved_preferences() {
        settingsStub.values = { translationStabilization: JSON.stringify({ enabled: true, auto: true, reference: 0.5, smoothness_s: 2.5, along_axis: false }) };
        restartPanel();
        const saved = settingsStub.value("translationStabilization", "");
        const pushes = renderQueueStub.pushed.length;
        controllerStub.translationRequested = false;
        controllerStub.translationReference = 1.75;
        controllerStub.translationSmoothness = 0.2;
        controllerStub.translationAlongAxis = true;
        controllerStub.translationAuto = false;
        motion.loadGyroflow({ gyro_source: { translation_stabilization_enabled: false,
            translation_reference: 1.75, translation_smoothness: 0.2, translation_along_axis: true, translation_auto: false } });
        wait(250);
        compare(settingsStub.value("translationStabilization", ""), saved);
        compare(renderQueueStub.pushed.length, pushes);
        dispatchFromApp("runSimpleBatchExport");
        compare(renderQueueStub.processingSettings.translation, false);
        compare(renderQueueStub.processingSettings.translation_reference, 1.75);
        motion.changeOpticalMode("translation", true);
        compare(settingsStub.value("translationStabilization", ""), saved);
        restartPanel();
        compare(lastPush().translation_reference, 0.5);
        compare(lastPush().translation_auto, true);
    }

    function test_missing_or_invalid_preferences_keep_defaults_data() {
        return [
            { tag: "missing", saved: "{}" },
            { tag: "broken-json", saved: "{" },
            { tag: "null", saved: "null" },
            { tag: "array", saved: "[]" },
            { tag: "wrong-types", saved: '{"enabled":"true","reference":null,"auto":0}' }
        ];
    }

    function test_missing_or_invalid_preferences_keep_defaults(data) {
        settingsStub.values = { translationStabilization: data.saved };
        restartPanel();
        compare(controllerStub.translationRequested, false);
        compare(controllerStub.translationAuto, true);
        compare(controllerStub.translationReference, 1.0);
        compare(renderQueueStub.pushed.length, 0);
    }

    function test_saved_parameters_are_clamped_to_the_control_ranges() {
        settingsStub.values = { translationStabilization: '{"reference":-1,"smoothness_s":100}' };
        restartPanel();
        compare(lastPush().translation_reference, 0);
        compare(lastPush().translation_smoothness, 10);
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

    function test_translation_and_reconstruction_can_be_selected_in_either_order() {
        motion.restoreOpticalControls(false);
        motion.changeOpticalMode("stab", true);
        wait(0);
        compare(lastPush().translation, true);
        compare(lastPush().reconstruction, true);
        motion.changeOpticalMode("translation", false);
        motion.changeOpticalMode("translation", true);
        wait(0);
        compare(lastPush().translation, true);
        compare(lastPush().reconstruction, true);
        motion.changeOpticalMode("stab", false);
        wait(0);
        compare(lastPush().translation, true);
        compare(lastPush().reconstruction, false);
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
