// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/mobile" as Mobile

TestCase {
    id: test
    name: "MobileSeek"
    when: windowShown
    visible: true
    width: 900; height: 1000
    property var workspace
    property var requests: []
    QtObject {
        id: backend
        property int editing_job_id: 1
        property string status: "stopped"
        signal queue_changed()
        signal queue_finished()
        signal mobile_delivery_changed()
        signal render_progress(int job_id, real progress, int current_frame, int total_frames, bool finished, real start_time, bool is_conversion)
        function get_mobile_queue_snapshot() { return JSON.stringify({ rows: [{ id: 1, filename: "test.mov", duration: 9000, frames: 270, previewable: true }], workersActive: false, deepActive: false }); }
    }
    QtObject {
        id: host
        function openMobilePreview(id) { backend.editing_job_id = id; }
        function setMobileComparison(stable) {}
        function attachMobileWorkspace() {}
        property var batchState: ({smoothness: 50, horizonLock: false, horizonLockAmount: 100, autoRotate: false, zoomMode: 1, lensCorrection: 1})
        property bool mobileAutoRotateAvailable: false
        property var exportSettings: null
        property var advanced: null
        property QtObject controller: QtObject {
            property bool video_loading_in_progress: false
            property bool loading_gyro_in_progress: false
            property bool gyro_loaded: true
            signal sync_progress(real progress, int ready, int total)
            function stabilize_step_pending_for_preview() { return false; }
        }
        property QtObject videoArea: QtObject {
            property real mobilePreviewAspectRatio: 16 / 9
            property bool queueEditLoading: false
            property var queue: null
            property QtObject vid: QtObject {
                property bool loaded: true
                property bool playing: false
                property real duration: 9000
                property real frameRate: 30
                property int frameCount: 270
                property real timestamp: 0
                function seekToTimestamp(value, exact) { test.requests = test.requests.concat([{ value: value, exact: exact }]); }
                function pause() { playing = false; }
                function play() { playing = true; }
            }
        }
    }
    Component { id: factory; Mobile.MobileWorkspace { unit: 1; dark: false } }
    function init() {
        requests = [];
        host.videoArea.vid.timestamp = 0;
        host.videoArea.vid.frameRate = 30;
        host.videoArea.vid.frameCount = 270;
        workspace = createTemporaryObject(factory, test, { width: 560, height: 900, host: host, backend: backend });
        verify(workspace !== null);
        workspace.refresh();
        workspace.openPreview(1);
        workspace.previewLoaded();
        verify(workspace.previewReady);
        waitForRendering(workspace);
    }
    function test_visible_track_positions_data() {
        let rows = [];
        for (const input of ["mouse", "touch"])
            for (const fraction of [0.1, 0.5, 0.9, 1.0])
                rows.push({ tag: input + "-" + fraction, input: input, fraction: fraction });
        return rows;
    }
    function test_visible_track_positions(data) {
        const slider = findChild(workspace, "mobilePlaybackSeek");
        verify(slider !== null);
        const x = slider.background.x + data.fraction * slider.background.width;
        if (data.input === "touch") {
            const touch = touchEvent(slider);
            touch.press(0, slider, x, slider.height / 2).commit();
            wait(16);
            touch.release(0, slider, x, slider.height / 2).commit();
        } else {
            mouseClick(slider, x, slider.height / 2);
        }
        verify(requests.length > 0);
        const requested = requests[requests.length - 1];
        verify(requested.exact);
        verify(requested.value < host.videoArea.vid.duration, "Seeking to EOF cannot display a frame");
        const lastFrameMs = (host.videoArea.vid.frameCount - 1) * 1000 / host.videoArea.vid.frameRate;
        verify(Math.abs(requested.value - data.fraction * lastFrameMs) < 25,
               "Visible track position must match the requested timestamp: " + requested.value);
    }
    function test_touch_hold_keeps_position_while_player_reports_an_older_frame() {
        const slider = findChild(workspace, "mobilePlaybackSeek");
        host.videoArea.vid.timestamp = 7000;
        tryCompare(slider, "value", 7000);
        const x = slider.handle.x + slider.handle.width / 2;
        const touch = touchEvent(slider);
        touch.press(0, slider, x, slider.height / 2).commit();
        tryCompare(slider, "pressed", true);
        host.videoArea.vid.timestamp = 1000;
        wait(16);
        compare(slider.value, 7000);
        touch.release(0, slider, x, slider.height / 2).commit();
    }
    function test_incomplete_metadata_keeps_a_bounded_seek_range_data() {
        return [{tag:"unknown-fps", fps:0, frames:270, expected:8999},
                {tag:"unknown-count", fps:30, frames:0, expected:8966.666666666666},
                {tag:"single-frame", fps:30, frames:1, expected:0}];
    }
    function test_incomplete_metadata_keeps_a_bounded_seek_range(data) {
        host.videoArea.vid.frameRate = data.fps;
        host.videoArea.vid.frameCount = data.frames;
        const slider = findChild(workspace, "mobilePlaybackSeek");
        verify(Math.abs(slider.to - data.expected) < 0.001);
    }
}
