// SPDX-License-Identifier: GPL-3.0-or-later
// Load with real fixture cases containing url, width, height, fps, frames and duration_ms.
import QtQuick

Item {
    id: probe
    property var cases: []
    property int index: 0
    property double jobId: 0
    property bool loaded: false
    property bool finished: false
    property int ticks: 0
    property string outputFolder: ""

    function fail(message) {
        console.warn("PROCESSING_SOURCE_SIZE_FAIL", index, message);
        finished = true;
        Qt.exit(1);
    }
    Connections {
        target: render_queue
        function onProcessing_done(id, byPreset) {
            if (id === probe.jobId && !byPreset) probe.loaded = true;
        }
        function onError(id, text, arg, callback) {
            probe.fail(text + " " + arg);
        }
    }
    Timer {
        interval: 100; repeat: true; running: !probe.finished
        onTriggered: {
            if (++probe.ticks > 600) { probe.fail("timeout"); return; }
            if (!probe.cases.length) { probe.fail("no fixtures"); return; }
            if (!probe.jobId) {
                const fixture = probe.cases[probe.index];
                console.warn("PROCESSING_SOURCE_SIZE_START", probe.index, fixture.url);
                probe.jobId = render_queue.add_file(fixture.url, "", JSON.stringify({output: {
                    output_folder: probe.outputFolder,
                    output_filename: "source-size-no-render.mp4"
                }}));
                if (!probe.jobId) probe.fail("file was not queued");
                return;
            }
            if (!probe.loaded) return;
            const text = render_queue.get_gyroflow_data(probe.jobId);
            if (!text) return;
            const project = JSON.parse(text);
            const info = project.video_info;
            const fixture = probe.cases[probe.index];
            console.warn("PROCESSING_SOURCE_SIZE_RESULT", probe.index, JSON.stringify(info));
            if (info.width !== fixture.width || info.height !== fixture.height
                    || info.num_frames !== fixture.frames || Math.abs(info.fps - fixture.fps) > 0.001
                    || Math.abs(info.duration_ms - fixture.duration_ms) > 1) {
                probe.fail("source properties changed");
                return;
            }
            if (fixture.pixelFocalLength !== undefined) {
                const lens = project.calibration_data;
                if (!lens || lens.calib_dimension.w !== fixture.width || lens.calib_dimension.h !== fixture.height
                        || lens.orig_dimension.w !== fixture.width || lens.orig_dimension.h !== fixture.height
                        || Math.abs(lens.fisheye_params.camera_matrix[0][0] - fixture.pixelFocalLength) > 0.01) {
                    probe.fail("source dimensions and telemetry lens geometry disagree");
                    return;
                }
                console.warn("PROCESSING_SOURCE_LENS_RESULT", probe.index, JSON.stringify(lens));
            }
            render_queue.remove(probe.jobId);
            probe.jobId = 0;
            probe.loaded = false;
            probe.ticks = 0;
            if (++probe.index === probe.cases.length) {
                console.warn("PROCESSING_SOURCE_SIZE_PASS", probe.index);
                probe.finished = true;
                Qt.quit();
            }
        }
    }
}
