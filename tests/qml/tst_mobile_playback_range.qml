// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest

TestCase {
    id: test
    name: "MobilePlaybackRange"
    when: windowShown
    property var timeline
    function init() {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/VideoArea.qml"), false);
        request.send();
        const source = request.responseText;
        const start = source.indexOf("                property bool prevRestrictTrim:");
        verify(start >= 0);
        const end = source.indexOf("\n            }", start);
        verify(end > start);
        // Execute the production Timeline properties and handlers with a recording player.
        const declaration = 'import QtQuick; Item { id: timeline; property var trimRanges: []; property bool restrictTrim: true;' +
            'property QtObject window: QtObject { property bool useMobileWorkspace: false }' +
            'property QtObject vid: QtObject { property real duration: 9000; property var range: [0, -1]; function setPlaybackRange(a, b) { range = [a, b]; } }' +
            'property QtObject controller: QtObject { property string exported: ""; function set_trim_ranges(value) { exported = value; } }' +
            'function getTrimRanges() { return trimRanges.length ? trimRanges : [[0, 1]]; }' + source.slice(start, end) + '}';
        timeline = Qt.createQmlObject(declaration, test, "production-playback-range");
        verify(timeline !== null);
    }
    function cleanup() { timeline.destroy(); }
    function setRanges() {
        timeline.trimRanges = [[500 / 9000, 2000 / 9000], [3500 / 9000, 5000 / 9000], [6000 / 9000, 7500 / 9000]];
    }
    function test_desktop_keeps_trim_playback() {
        setRanges();
        compare(timeline.vid.range, [500, 7500]);
    }
    function test_mobile_keeps_export_trims_and_previews_full_video() {
        timeline.window.useMobileWorkspace = true;
        setRanges();
        compare(timeline.vid.range, [0, -1]);
        compare(timeline.trimRanges.length, 3);
        compare(timeline.controller.exported, timeline.trimRanges.map(x => x[0] + ":" + x[1]).join(";"));
        verify(timeline.restrictTrim);
    }
    function test_switching_modes_restores_the_desktop_preference() {
        setRanges();
        timeline.window.useMobileWorkspace = true;
        compare(timeline.vid.range, [0, -1]);
        timeline.window.useMobileWorkspace = false;
        compare(timeline.vid.range, [500, 7500]);
        timeline.restrictTrim = false;
        compare(timeline.vid.range, [0, -1]);
    }
}
