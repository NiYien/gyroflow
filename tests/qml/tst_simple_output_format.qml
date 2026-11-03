// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/menu" as Menu
import "../../src/ui/mobile" as Mobile

TestCase {
    id: window
    name: "SimpleOutputFormat"
    when: windowShown
    width: 500
    height: 700
    visible: true
    property bool isSimpleMode: true
    property bool isMobile: false
    property bool useMobileWorkspace: false
    property bool animationsEnabled: false
    property real dpiScale: 1
    property string style: "dark"
    property string styleFont: "Arial"
    property color styleTextColor: "white"
    property color styleTextColorOnAccent: "white"
    property color styleAccentColor: "blue"
    property color styleBackground: "black"
    property color styleButtonColor: "gray"
    property color styleHighlightColor: "gray"
    property color stylePopupBorder: "gray"
    property string defaultInitializedDevice: ""
    property var advanced: null
    property var batchState: ({smoothness:50, horizonLock:false, horizonLockAmount:100, zoomMode:1, lensCorrection:1})
    property bool mobileAutoRotateAvailable: false
    property alias exportSettings: exportPanel
    property var videoArea: ({vid: {loaded: false}, loadedFileUrl: ""})
    property var vidInfo: ({pixelFormat: "8 bit", videoRotation: 0})
    property var stab: ({videoSpeed: {value: 1, isKeyframed: false}, maxValues: {maxZoom: 1}})
    property var outputFile: ({filename: "clip.mp4", folderUrl: "file:///test/",
        setFilename: function(value) { window.outputFile.filename = value; }})

    QtObject {
        id: settings
        property var stored: ({})
        function value(key, fallback) { return stored.hasOwnProperty(key) ? stored[key] : fallback; }
        function setValue(key, value) { stored[key] = value; }
        function init(item) {}
        function propChanged(item) {}
    }
    QtObject {
        id: controller
        signal gpu_list_loaded(var list)
        function set_output_size(w, h) {}
    }
    QtObject {
        id: render_queue
        property string status: "stopped"
        property string default_suffix: ""
        property int exportSettingsCalls: 0
        property int lastDivisor: 0
        property real lastBitrate: -1
        function set_queue_output_path(mode, path) {}
        function set_mobile_gallery_output(value) {}
        function set_pending_output_format(options) {}
        function set_simple_export_settings(divisor, bitrate) {
            ++exportSettingsCalls;
            lastDivisor = divisor;
            lastBitrate = bitrate;
        }
    }
    Item {
        Item {
            Item {
                Menu.Export { id: exportPanel; width: 400; visible: false }
            }
        }
    }
    Menu.SimpleExport { id: simplePanel; width: 400 }
    Mobile.MobileSettings { id: mobilePanel; host: window; section: "app"; width: 400; visible: false }

    function test_effective_bitrate_uses_simple_preset_and_full_mode_value() {
        exportPanel.sourceBitrate = 180;
        exportPanel.outBitrate = 200;
        exportPanel.setSimpleOutputBitrate(3);
        compare(exportPanel.effectiveBitrate, 20);
        compare(exportPanel.getExportOptions().bitrate, 20);
        exportPanel.setSimpleOutputBitrate(2);
        compare(exportPanel.effectiveBitrate, 50);
        exportPanel.setSimpleOutputBitrate(0);
        compare(exportPanel.effectiveBitrate, 180);
        window.isSimpleMode = false;
        compare(exportPanel.effectiveBitrate, 200);
        window.isSimpleMode = true;
        exportPanel.sourceBitrate = 0;
    }

    function test_export_presets_user_activation_and_project_readback() {
        render_queue.status = "stopped";
        exportPanel.setSimpleOutputSize(0);
        exportPanel.setSimpleOutputBitrate(0);
        render_queue.exportSettingsCalls = 0;
        const size = findChild(simplePanel, "simpleOutputSizeChoice");
        const bitrate = findChild(simplePanel, "simpleOutputBitrateChoice");
        verify(size.enabled);
        compare(size.count, 4);
        compare(bitrate.count, 4);
        size.forceActiveFocus();
        keyClick(Qt.Key_Down);
        compare(exportPanel.simpleOutputSizeDivisor, 2);
        compare(render_queue.lastDivisor, 2);
        bitrate.forceActiveFocus();
        keyClick(Qt.Key_Down);
        keyClick(Qt.Key_Down);
        compare(exportPanel.simpleOutputBitrate, 50);
        compare(render_queue.lastBitrate, 50);
        compare(settings.stored.simpleOutputSizeDivisor, 2);
        compare(settings.stored.simpleOutputBitrate, 50);
        const calls = render_queue.exportSettingsCalls;
        exportPanel.loadGyroflow({output: {output_width: 640, output_height: 360, bitrate: 8}});
        wait(20);
        compare(exportPanel.simpleOutputSizeDivisor, 2);
        compare(exportPanel.simpleOutputBitrate, 50);
        compare(render_queue.exportSettingsCalls, calls);
        render_queue.status = "active";
        verify(!size.enabled);
        verify(!bitrate.enabled);
        render_queue.status = "stopped";
    }

    function test_mobile_presets_share_preferences_and_busy_gate() {
        exportPanel.setSimpleOutputSize(1);
        exportPanel.setSimpleOutputBitrate(2);
        simplePanel.visible = false;
        mobilePanel.visible = true;
        const size = findChild(mobilePanel, "mobileOutputSizeChoice");
        const bitrate = findChild(mobilePanel, "mobileOutputBitrateChoice");
        compare(size.currentIndex, exportPanel.simpleOutputSizeDivisor - 1);
        size.forceActiveFocus();
        keyClick(Qt.Key_Down);
        compare(exportPanel.simpleOutputSizeDivisor, 3);
        bitrate.forceActiveFocus();
        keyClick(Qt.Key_Down);
        compare(exportPanel.simpleOutputBitrate, 20);
        compare(render_queue.lastBitrate, 20);
        mobilePanel.busy = true;
        verify(!size.enabled);
        verify(!bitrate.enabled);
        mobilePanel.busy = false;
        mobilePanel.visible = false;
        simplePanel.visible = true;
    }

    function test_saved_export_preferences_restore_on_component_creation() {
        exportPanel.setSimpleOutputSize(2);
        exportPanel.setSimpleOutputBitrate(3);
        const component = Qt.createComponent("../../src/ui/menu/Export.qml");
        compare(component.status, Component.Ready);
        const restored = component.createObject(exportPanel.parent, {width: 400, visible: false});
        verify(restored !== null);
        compare(restored.simpleOutputSizeDivisor, 3);
        compare(restored.simpleOutputBitrate, 20);
        restored.destroy();
    }

    function test_selection_and_hidden_panel() {
        const combos = Array.from(simplePanel.children).filter(x => x.count !== undefined);
        compare(combos.length, 2);
        const codec = combos[0];
        const variant = combos[1];
        compare(codec.count, exportPanel.exportFormats.length);
        compare(codec.visible, Qt.platform.os === "osx");
        verify(codec.enabled);
        exportPanel.codec.currentIndex = 2;
        compare(codec.currentText, "ProRes");
        compare(variant.currentText, "HQ");
        exportPanel.updateCodecParams();
        compare(variant.currentText, "HQ");
        compare(exportPanel.getExportOptions().codec_options, "HQ");
        exportPanel.visible = true;
        exportPanel.visible = false;
        compare(exportPanel.getExportOptions().codec_options, "HQ");
        exportPanel.codecOptions.currentIndex = 1;
        compare(variant.currentText, "LT");
        exportPanel.updateCodecParams();
        compare(variant.currentText, "LT");
        // Exercise real control input without breaking the readback bindings.
        // On Windows this only overrides visibility for the interaction check.
        codec.visible = true;
        variant.visible = true;
        variant.forceActiveFocus();
        keyClick(Qt.Key_Down);
        keyClick(Qt.Key_Down);
        keyClick(Qt.Key_Down);
        compare(exportPanel.getExportOptions().codec_options, "4444");
        exportPanel.codecOptions.currentIndex = 3;
        compare(variant.currentText, "HQ");
        codec.forceActiveFocus();
        keyClick(Qt.Key_Up);
        compare(exportPanel.outCodec, "H.265/HEVC");
        compare(variant.count, 0);
        compare(exportPanel.getExportOptions().codec_options, "");
        exportPanel.codec.currentIndex = 2;
        compare(codec.currentText, "ProRes");
        compare(variant.currentText, "HQ");
        exportPanel.codec.currentIndex = 3;
        compare(variant.currentText, "DNxHR HQ");
        exportPanel.loadGyroflow({ output: { codec: "ProRes", codec_options: "LT" } });
        compare(codec.currentText, "ProRes");
        compare(variant.currentText, "LT");
        render_queue.status = "active";
        verify(!codec.enabled);
        verify(!variant.enabled);
        wait(20);
    }
}
