// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/menu" as Menu
import "../../src/ui" as Ui
import "../../src/ui/components" as Components

TestCase {
    id: testCase;
    name: "HorizonLockSettings";
    when: windowShown;
    visible: true;
    width: 500;
    height: 900;

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
    property string stylePopupBorder: "#555555";
    property color styleSliderHandle: "#888888";
    property color styleSliderBackground: "#444444";
    property color styleVideoBorderColor: "#444444";
    property color styleHrColor: "#444444";
    property bool isMobile: false;
    property bool isMobileLayout: false;
    property bool animationsEnabled: false;
    property bool isLandscape: true;
    property var smoothingAlgorithms: ["No smoothing", "Default"];

    QtObject {
        id: settingsStub;
        function value(key, fallback) { return fallback; }
        function setValue(key, value) { }
        function init(item) { }
        function propChanged(item) { }
    }
    property alias settings: settingsStub;
    QtObject {
        id: controllerStub;
        signal compute_progress(real id, real progress);
        signal telemetry_loaded(bool main, string filename, string camera, var data);
        signal rolling_shutter_estimated(real value);
        signal keyframes_changed();
        signal keyframe_value_updated(string key, real value);
        signal gyroflow_file_loaded(var data);
        property bool has_gravity_vectors: false;
        property bool has_per_frame_focal_length: false;
        property bool focal_length_smoothing_enabled: false;
        property real focal_length_max_zoom_rate: 50;
        property var lastHorizon: [];
        property int horizonCalls: 0;
        property real frame_readout_time: 0;
        property int frame_readout_direction: 0;
        property real video_speed: 1;
        property bool video_speed_affects_smoothing: false;
        property bool video_speed_affects_zooming: false;
        property bool video_speed_affects_zooming_limit: false;
        property real adaptive_zoom_window: 4;
        property real adaptive_zoom: 4;
        property int zooming_method: 1;
        property real zooming_center_x: 0;
        property real zooming_center_y: 0;
        property real additional_rotation_x: 0;
        property real additional_rotation_y: 0;
        property real additional_rotation_z: 0;
        property real additional_translation_x: 0;
        property real additional_translation_y: 0;
        property real additional_translation_z: 0;
        property real lens_correction_amount: 1;
        property real fov: 1;
        function set_smoothing_method(index) { return []; }
        function set_horizon_lock(amount, roll, lockPitch, pitch, autoLock, threshold, smoothness, multiplier, limit) {
            lastHorizon = [amount, roll, lockPitch, pitch, autoLock, threshold, smoothness, multiplier, limit];
            horizonCalls++;
        }
        function set_use_gravity_vectors(value) { }
        function set_horizon_lock_integration_method(value) { }
        function set_max_zoom(value, iterations) { }
        function set_focal_length_smoothing_enabled(value) { }
        function set_focal_length_max_zoom_rate(value) { }
        function is_keyframed(key) { return false; }
        function get_keyframe_value(key, timestamp) { return 0; }
    }
    property alias controller: controllerStub;
    QtObject {
        id: windowStub;
        property bool isSimpleMode: false;
        property bool useMobileWorkspace: false;
        property real width: 500;
        property real height: 900;
        property var batchState: QtObject {
            property bool active: false;
            property bool horizonLock: false;
            property real horizonLockAmount: 0;
            property real smoothness: 15;
            property real framerate: 0;
        }
        property var motionData: QtObject { property bool hasQuaternions: false; property bool hasRawGyro: true; property int integrationMethod: 1; }
        property var videoArea: QtObject {
            property var vid: QtObject { property bool loaded: true; property real timestamp: 0; property real frameRate: 30; }
            property var timeline: QtObject { function getTimestamp() { return 0; } }
        }
        function modal() { }
    }
    property alias window: windowStub;

    Component { id: panelFactory; Menu.Stabilization { } }
    Component { id: selectorFactory; Ui.SettingsSelector { } }
    Component { id: checkboxFactory; Components.CheckBox { text: "Test checkbox"; width: 180; hoverEnabled: true; z: 100; } }
    Item { Item { Item { id: panelHost; width: 460; height: 900; } } }
    property var panel;

    function find(node, predicate) {
        if (predicate(node)) return node;
        const children = node.children || [];
        for (const child of children) {
            const result = find(child, predicate);
            if (result) return result;
        }
        return null;
    }
    function control(text) {
        const result = find(panel, function(item) { return item.text === text; });
        verify(result !== null, "Missing control: " + text);
        return result;
    }
    function slider(text) {
        const label = control(text);
        const result = find(label, function(item) { return item.slider !== undefined && item.value !== undefined; });
        verify(result !== null, "Missing slider: " + text);
        return result;
    }
    function load(extra) {
        const values = Object.assign({ horizon_lock_amount: 85, horizon_lock_roll: 2, horizon_lock_pitch_enabled: true, horizon_lock_pitch: 3 }, extra);
        panel.loadGyroflow({ stabilization: values });
        wait(30);
    }
    function init() {
        windowStub.batchState.active = false;
        controllerStub.has_gravity_vectors = false;
        panel = createTemporaryObject(panelFactory, panelHost, { width: 460 });
        verify(panel !== null);
        wait(30);
    }
    function test_saved_values_reach_sliders_and_controller() {
        load({ automatic_lock: true, turn_threshold: 8.5, turn_smoothing_ms: 730, turn_multiplier: 1.4, tilt_accel_limit: 7.5 });
        compare(slider("Turn threshold").value, 8.5);
        compare(slider("Turn smoothness").value, 730);
        compare(slider("Turn multiplier").value, 1.4);
        compare(slider("Tilt acceleration limit").value, 7.5);
        compare(controllerStub.lastHorizon, [100, 2, true, 3, true, 8.5, 730, 1.4, 7.5]);
        slider("Turn threshold").value = 9;
        wait(30);
        compare(controllerStub.lastHorizon.slice(5), [9, 730, 1.4, 7.5]);
    }
    function test_missing_and_null_values_restore_defaults_data() {
        return [{tag: "missing", values: {}}, {tag: "null", values: {turn_threshold: null, turn_smoothing_ms: null, turn_multiplier: null, tilt_accel_limit: null}}];
    }
    function test_missing_and_null_values_restore_defaults(data) {
        load({ automatic_lock: true, turn_threshold: 8, turn_smoothing_ms: 750, turn_multiplier: 1.5, tilt_accel_limit: 9 });
        load(data.values);
        compare(controllerStub.lastHorizon.slice(4), [false, 5, 500, 1, Infinity]);
        compare(slider("Turn smoothness").value, 500);
        verify(!control("Limit tilt acceleration").checked);
    }
    function test_gravity_vectors_disable_only_pitch_controls() {
        load({ use_gravity_vectors: true });
        const pitch = control("Lock pitch angle");
        const correction = slider("Pitch angle correction");
        verify(pitch.enabled);
        controllerStub.has_gravity_vectors = true;
        wait(30);
        verify(pitch.unavailable || !pitch.enabled);
        verify(!correction.enabled);
        verify(pitch.checked, "Keep the saved choice for when gravity vectors are turned off");
        control("Use gravity vectors").checked = false;
        wait(30);
        verify(pitch.enabled && !pitch.unavailable);
        verify(correction.enabled);
    }
    function test_batch_updates_do_not_change_preview_horizon() {
        load({ automatic_lock: true, turn_threshold: 8, turn_smoothing_ms: 750, turn_multiplier: 1.5, tilt_accel_limit: 9 });
        const calls = controllerStub.horizonCalls;
        windowStub.batchState.active = true;
        panel.horizonCb.checked = false;
        panel.updateHorizonLock();
        wait(30);
        compare(controllerStub.horizonCalls, calls);
        compare(windowStub.batchState.horizonLock, false);
        compare(windowStub.batchState.horizonLockAmount, 0);
        verify(!control("Lock pitch angle").enabled);
    }
    function test_unavailable_checkbox_keeps_tooltip_and_rejects_input() {
        const checkbox = createTemporaryObject(checkboxFactory, testCase, {unavailable: true, tooltip: "Unavailable reason"});
        verify(checkbox !== null);
        mouseMove(checkbox, 10, 10);
        tryCompare(checkbox, "hovered", true);
        verify(checkbox.enabled);
        compare(checkbox.focusPolicy, Qt.NoFocus);
        mouseClick(checkbox, 10, 10);
        verify(!checkbox.checked);
        checkbox.forceActiveFocus();
        keyClick(Qt.Key_Return);
        verify(!checkbox.checked);
        keyClick(Qt.Key_Space);
        verify(!checkbox.checked);
        checkbox.unavailable = false;
        mouseClick(checkbox, 10, 10);
        verify(checkbox.checked);
        keyClick(Qt.Key_Space);
        verify(!checkbox.checked);
    }
    function test_preset_and_queue_selection_keep_automatic_lock_fields_data() {
        return [{tag: "preset", type: "preset"}, {tag: "queue", type: "apply"}];
    }
    function test_preset_and_queue_selection_keep_automatic_lock_fields(data) {
        const selector = createTemporaryObject(selectorFactory, testCase, { type: data.type });
        verify(selector !== null);
        let selection;
        selector.apply.connect(function(value) { selection = value; });
        selector.clicked(0, false);
        const values = { automatic_lock: true, turn_threshold: 8, turn_smoothing_ms: 750, turn_multiplier: 1.5, tilt_accel_limit: null };
        const filtered = selector.getFilteredObject({stabilization: values}, selection);
        for (const key in values) compare(filtered.stabilization[key], values[key], key);
    }
}
