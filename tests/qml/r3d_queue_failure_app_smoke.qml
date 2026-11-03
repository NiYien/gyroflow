// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
Item {
    id: test
    property url failingClip
    property url recoveryClip
    property string outputFolder
    property double jobId: 0
    property int stage: 0
    property int ticks: 0
    property bool finished: false
    function end(ok, reason) {
        finished = true;
        console.warn(ok ? "R3D_QUEUE_FAILURE_PASS" : "R3D_QUEUE_FAILURE_FAIL", reason);
        Qt.exit(ok ? 0 : 1);
    }
    Connections {
        target: render_queue
        function onError(id, text, arg, callback) {
            if (test.stage !== 1 || id !== test.jobId || arg !== "video_read_failed") { test.end(false, "unexpected error: " + arg); return; }
            console.warn("R3D_QUEUE_FAILURE_REPORTED", arg);
            render_queue.remove(test.jobId);
            test.stage = 2;
        }
        function onProcessing_done(id, byPreset) {
            if (id === test.jobId && !byPreset) {
                if (test.stage !== 3) { test.end(false, "failed decode reported success"); return; }
                render_queue.remove(test.jobId);
                test.end(true, "error is terminal and subsequent MP4 is processed");
            }
        }
    }
    Timer {
        interval: 100; repeat: true; running: !test.finished
        onTriggered: {
            if (++test.ticks > 300) { test.end(false, "timeout"); return; }
            if (test.stage === 0 || test.stage === 2) {
                const url = test.stage === 0 ? test.failingClip : test.recoveryClip;
                test.stage++;
                test.jobId = render_queue.add_file(url, "", JSON.stringify({output: {output_folder: test.outputFolder, output_filename: "no-render.mp4"}}));
                if (!test.jobId) test.end(false, "file was not queued");
            }
        }
    }
}
