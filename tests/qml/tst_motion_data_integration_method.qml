// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/menu" as Menu

// Run: ext/6.7.3/mingw_64/bin/qmltestrunner.exe -platform offscreen -input tests/qml/tst_motion_data_integration_method.qml
TestCase {
    id: testCase;
    name: "MotionDataIntegrationMethod";
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
        property bool gyro_has_raw_imu: true;
        property bool gyro_has_quaternions: false;
        property bool gyro_has_accurate_timestamps: false;
        property int lastMethod: -1;
        property var methodCalls: [];
        function set_integration_method(index) { lastMethod = index; methodCalls = methodCalls.concat([index]); }
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

    // Core method values: 0 = built-in quaternions, 1 = Complementary, 2 = VQF.
    function coreMethod(): int {
        return motion.hasQuaternions ? motion.integrationMethod : motion.integrationMethod + 1;
    }
    function loadTelemetry(containsQuats: bool, isMainVideo: bool): void {
        controllerStub.gyro_has_quaternions = containsQuats;
        controllerStub.telemetry_loaded(isMainVideo, "C0416.MP4", "Sony", {
            "contains_raw_gyro": true,
            "contains_quats": containsQuats,
            "imu_orientation": "yXZ"
        });
    }
    function settleMethod(expected: int): void {
        // The ComboBox pushes its value through a 300 ms timer.
        wait(450);
        compare(coreMethod(), expected);
        compare(controllerStub.lastMethod, expected);
    }

    function init() {
        controllerStub.lastMethod = -1;
        controllerStub.methodCalls = [];
        controllerStub.gyro_has_quaternions = false;
        motion = createTemporaryObject(factory, testCase, { width: 380 });
        verify(motion !== null);
    }

    function test_telemetry_defaults_to_vqf() {
        loadTelemetry(false, true);
        settleMethod(2);
    }

    function test_preset_without_motion_data_keeps_vqf() {
        loadTelemetry(false, true);
        settleMethod(2);
        motion.loadGyroflow({ "stabilization": { "frame_offset": 0 } });
        settleMethod(2);
        verify(controllerStub.methodCalls.indexOf(1) < 0, "Complementary was pushed: " + controllerStub.methodCalls);
    }

    function test_partial_motion_preset_keeps_vqf() {
        loadTelemetry(false, true);
        settleMethod(2);
        motion.loadGyroflow({ "gyro_source": { "lpf": 5 } });
        settleMethod(2);
        verify(controllerStub.methodCalls.indexOf(1) < 0, "Complementary was pushed: " + controllerStub.methodCalls);
    }

    function test_explicit_project_method_is_applied() {
        loadTelemetry(false, true);
        settleMethod(2);
        // A queue project saved before its sync finished still carries Complementary.
        motion.loadGyroflow({ "gyro_source": { "integration_method": 1 } });
        settleMethod(1);
        // The queue switches to VQF after sync and writes it into the project.
        motion.loadGyroflow({ "gyro_source": { "integration_method": 2 } });
        settleMethod(2);
    }

    function test_builtin_quaternions_stay_selected() {
        loadTelemetry(true, true);
        wait(450);
        compare(coreMethod(), 0);
        motion.loadGyroflow({ "stabilization": { "frame_offset": 0 } });
        wait(450);
        compare(coreMethod(), 0);
    }

    function test_builtin_quaternion_method_falls_back_to_vqf_without_quaternions() {
        loadTelemetry(true, true);
        wait(450);
        compare(coreMethod(), 0);
        // The new motion source has no quaternions, so "None" no longer exists in the list.
        controllerStub.gyro_has_quaternions = false;
        motion.loadGyroflow({ "gyro_source": { "lpf": 5 } });
        settleMethod(2);
    }
}
