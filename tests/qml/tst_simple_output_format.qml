// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/menu" as Menu

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
    property alias exportSettings: exportPanel
    property var videoArea: ({vid: {loaded: false}, loadedFileUrl: ""})
    property var vidInfo: ({pixelFormat: "8 bit", videoRotation: 0})
    property var stab: ({videoSpeed: {value: 1, isKeyframed: false}, maxValues: {maxZoom: 1}})
    property var outputFile: ({filename: "clip.mp4", folderUrl: "file:///test/",
        setFilename: function(value) { window.outputFile.filename = value; }})

    QtObject {
        id: settings
        function value(key, fallback) { return fallback; }
        function setValue(key, value) {}
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
        function set_queue_output_path(mode, path) {}
        function set_pending_output_format(options) {}
    }
    Item {
        Item {
            Item {
                Menu.Export { id: exportPanel; width: 400; visible: false }
            }
        }
    }
    Menu.SimpleExport { id: simplePanel; width: 400 }

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
