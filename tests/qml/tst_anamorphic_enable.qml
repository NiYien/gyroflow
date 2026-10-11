// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/menu" as Menu

// Run with QT_QUICK_CONTROLS_STYLE=Basic.
TestCase {
    id: testCase
    name: "AnamorphicEnable"
    when: windowShown
    visible: true
    width: 500
    height: 1000

    property real dpiScale: 1
    property string styleFont: "Arial"
    property string style: "dark"
    property color styleButtonColor: "#333333"
    property color styleTextColor: "#ffffff"
    property color styleTextColorOnAccent: "#ffffff"
    property color styleAccentColor: "#3a9cde"
    property color styleBackground: "#111111"
    property color styleBackground2: "#222222"
    property color styleHighlightColor: "#666666"
    property color stylePopupBorder: "#555555"
    property color styleSliderHandle: "#888888"
    property color styleSliderBackground: "#555555"
    property color styleHrColor: "#444444"
    property bool isMobile: false
    property bool animationsEnabled: false
    property bool isLandscape: true
    property var settings: ({ value: function(key, fallback) { return fallback; }, setValue: function() {} })
    property var window: ({ videoArea: null, exportSettings: null })

    QtObject {
        id: controller
        property bool lens_group_manual_edit: true
        property string lens_group_status: "[]"
        property string lens_group_config: "[]"
        property string project_lens: ""
        property bool device_connected: false
        property string ota_state: ""
        property var writes: []
        property var applied: []
        signal lens_group_status_changed()
        signal lens_group_config_changed()
        signal lens_presets_updated()
        signal project_lens_changed()
        signal keyframe_value_updated(string key, real value)
        signal gyroflow_file_loaded(var data)
        function get_lens_presets() {
            return JSON.stringify([{ id: "test_preset", name: "Test lens", squeeze_ratio: 1.5 }]);
        }
        function set_lens_group_config(json) {
            writes = writes.concat([json]);
            lens_group_config = json;
            lens_group_config_changed();
        }
        function apply_lens_group_to_main(index) {
            applied = applied.concat([JSON.parse(lens_group_config)[index]]);
            return "";
        }
    }
    QtObject {
        id: render_queue
        signal match_results_changed()
        signal match_apply_finished()
        signal manual_camera_changed()
        function get_manual_camera_state_json() { return "{}"; }
        function clear_all_per_job_lens_group_for_indices(indices) {}
        function has_match_results() { return false; }
    }
    Component { id: factory; Menu.LensGroupConfig { opened: true; width: 480 } }
    Item { Item { Item { id: panelHost } } }
    property var panel
    property var toggle

    function anamorphicControl(item) {
        if (item.text === "Anamorphic lens" && item.cb !== undefined)
            return item;
        const children = item.children || [];
        for (let i = 0; i < children.length; ++i) {
            const found = anamorphicControl(children[i]);
            if (found) return found;
        }
        return null;
    }
    function openPanel(config) {
        controller.lens_group_config = JSON.stringify([Object.assign({ lens_index: 0 }, config)]);
        panel = createTemporaryObject(factory, panelHost);
        verify(panel !== null);
        tryCompare(panel, "_bootDone", true);
        toggle = anamorphicControl(panel);
        verify(toggle !== null);
        compare(controller.writes.length, 0, "Loading the panel must not persist display defaults");
    }
    function init() {
        controller.writes = [];
        controller.applied = [];
    }
    function test_first_enable_applies_displayed_ratio() {
        openPanel({});
        compare(panel.currentSqueezeRatio(), 1.33);
        toggle.checked = true;
        compare(controller.writes.length, 1);
        compare(controller.applied.length, 1);
        compare(controller.applied[0].anamorphic_enabled, true);
        compare(controller.applied[0].squeeze_ratio, 1.33);
        compare(JSON.parse(controller.writes[0])[0].squeeze_ratio, 1.33);
    }
    function test_disable_then_enable_applies_default_again() {
        openPanel({ focal_length_mm: 35 });
        toggle.checked = true;
        toggle.checked = false;
        compare(controller.applied[1].squeeze_ratio, null);
        compare(controller.applied[1].focal_length_mm, 35);
        toggle.checked = true;
        compare(controller.applied[2].squeeze_ratio, 1.33);
        compare(controller.applied[2].focal_length_mm, 35);
    }
    function test_enable_preserves_existing_values_data() {
        return [
            { tag: "manual", config: { squeeze_ratio: 1.8, squeeze_direction: "vertical" }, ratio: 1.8 },
            { tag: "preset", config: { preset_id: "test_preset", squeeze_ratio: null }, ratio: null }
        ];
    }
    function test_enable_preserves_existing_values(data) {
        openPanel(data.config);
        toggle.checked = true;
        const config = controller.applied[0];
        compare(config.squeeze_ratio, data.ratio);
        if (data.config.preset_id) compare(config.preset_id, data.config.preset_id);
        if (data.config.squeeze_direction) compare(config.squeeze_direction, data.config.squeeze_direction);
    }
}
