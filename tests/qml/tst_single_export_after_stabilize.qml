// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest

// Exercise the production export handlers with a completed project-export job.
// Run with QML_XHR_ALLOW_FILE_READ=1.
TestCase {
    id: testCase
    name: "SingleExportAfterStabilize"
    property var renderBtn
    property var singleButton
    property string appSource
    property var jobs
    property var calls
    property bool isSandboxed: false
    property bool isMobile: false
    property real dpiScale: 1
    property bool folderAllowed: true
    property bool crm: false
    property int notices: 0
    property var trimRanges: []
    property string savedBaseline: ""
    property var window: testCase
    property var exportSettings: ({ outCodec: "H.265/HEVC", outGpu: false, outBitrate: 20 })
    property var vidInfo: ({ item: { filename: "clip.mp4" } })
    property var motionData: ({ item: { hasQuaternions: false, hasAccurateTimestamps: true, filename: "clip.mp4" } })
    property var controller: ({ lens_loaded: true, offsets_model: { rowCount: function() { return 1; } },
        image_to_b64: function(image) { return "thumbnail"; } })
    property var filesystem: ({ exists_in_folder: function(folder, filename) { return false; } })
    property var outputFile: ({ folderUrl: "file:///output/", filename: "clip_stabilized.mp4" })
    property var settings: ({ value: function(key, fallback) { return fallback; } })
    property var delayAddQueue: ({ start: function() {} })
    property var videoArea: ({
        loadedFileUrl: "file:///clip.mp4", queue: { shown: false },
        captureEditingBaseline: function() { savedBaseline = getAdditionalProjectDataJson(); },
        vid: { parent: { ratio: 16 / 9 }, pause: function() {},
            grabToImage: function(callback, size) { Qt.callLater(function() { callback({ image: null }); }); } }
    })
    QtObject {
        id: queue
        property int export_project: 2
        property int editing_job_id: 1
        property int main_job_id: 0
        property int parallel_renders: 1
        property int activeCount: 0
        property int nextId: 3
        function is_plugin_only_video(url) { return false; }
        function get_default_encoder(codec, gpu) { return "libx265"; }
        function file_exists_in_folder(folder, filename) { return false; }
        function get_active_render_count() { return activeCount; }
        function get_job_output_filename(id) { return jobs[id] ? jobs[id].output : ""; }
        function add(data, thumbnail) {
            calls.push("save");
            const id = editing_job_id || nextId++;
            editing_job_id = 0;
            if (!jobs[id]) {
                if (Object.keys(jobs).some(key => jobs[key].source === videoArea.loadedFileUrl)) {
                    notices++;
                    return id;
                }
                jobs[id] = { status: "queued", source: videoArea.loadedFileUrl };
            }
            jobs[id].output = outputFile.filename;
            jobs[id].trim = JSON.parse(data).trim_ranges;
            return id;
        }
        function reset_job(id) { calls.push("reset:" + id); if (jobs[id]) jobs[id].status = "queued"; }
        function render_job(id) {
            calls.push("render:" + id);
            if (jobs[id] && jobs[id].status === "queued") jobs[id].status = "rendering";
        }
        function start() { calls.push("start"); }
    }
    property alias render_queue: queue
    function isCanonCrmWorkflow() { return crm; }
    function showCanonCrmProjectOnlyMessage() { notices++; }
    function singleVideoOutputFolderGatePasses() { return folderAllowed; }
    function getAdditionalProjectDataJson() { return JSON.stringify({ trim_ranges: trimRanges }); }

    function productionFunction(name) {
        const marker = "                        function " + name + "(): void {";
        const start = appSource.indexOf(marker);
        verify(start >= 0, name + " exists");
        const end = appSource.indexOf("\n                        }", start) + "\n                        }".length;
        verify(end > start, name + " has a closing brace");
        return appSource.slice(start, end);
    }
    function initTestCase() {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/App.qml"), false);
        request.send();
        appSource = request.responseText.replace(/\r\n/g, "\n");
        verify(appSource.length > 0);
    }
    function init() {
        jobs = {
            1: { status: "finished", offset: 42, source: "file:///clip.mp4", output: "clip_stabilized.mp4", trim: [[0, 1]] },
            2: { status: "finished", offset: 56, source: "file:///other.mp4", output: "other_stabilized.mp4", trim: [[0.2, 0.8]] }
        };
        calls = [];
        folderAllowed = true;
        crm = false;
        notices = 0;
        trimRanges = [[0, 1]];
        savedBaseline = "";
        videoArea.loadedFileUrl = "file:///clip.mp4";
        videoArea.queue.shown = false;
        queue.export_project = 2;
        queue.editing_job_id = 1;
        queue.main_job_id = 0;
        queue.activeCount = 0;
        queue.nextId = 3;
        renderBtn = Qt.createQmlObject('import QtQuick; QtObject {'
            + 'property bool isAddToQueue: false; property bool tempIsAddToQueue: false;'
            + 'property bool allowFile: false; property bool allowLens: false; property bool allowSync: false;'
            + 'property bool addQueueDelayed: false; property var btn: ({ enabled: true });'
            + productionFunction("render") + '}', testCase);
        singleButton = Qt.createQmlObject('import QtQuick; QtObject {' + productionFunction("doSingleRender") + '}', testCase);
    }
    function cleanup() { singleButton.destroy(); renderBtn.destroy(); }
    function verifySingleExport(id) {
        tryVerify(function() { return jobs[id] && jobs[id].status === "rendering"; });
        compare(calls[0], "save");
        compare(calls[calls.length - 1], "render:" + id);
        verify(calls.indexOf("start") < 0, "a single export does not start the batch");
        compare(queue.export_project, 0);
        compare(queue.main_job_id, id);
        compare(jobs[id].status, "rendering");
        compare(jobs[2].status, "finished", "another stabilized video stays finished");
        compare(jobs[1].offset, 42, "the saved synchronization is preserved");
    }
    function test_play_completed_stabilization_then_export() {
        singleButton.doSingleRender();
        verifySingleExport(1);
    }
    function test_export_after_queue_navigation_clears_save_intent() {
        renderBtn.isAddToQueue = true;
        renderBtn.tempIsAddToQueue = true;
        singleButton.doSingleRender();
        verifySingleExport(1);
    }
    function test_save_keeps_completed_stabilization() {
        renderBtn.isAddToQueue = true;
        renderBtn.render();
        tryCompare(renderBtn, "addQueueDelayed", true);
        compare(calls.join(","), "save");
        compare(jobs[1].status, "finished");
    }
    function test_new_single_video_still_exports() {
        queue.editing_job_id = 0;
        videoArea.loadedFileUrl = "file:///new.mp4";
        singleButton.doSingleRender();
        verifySingleExport(3);
    }
    function test_export_twice_uses_new_trim_without_changing_other_job() {
        const otherBefore = JSON.stringify(jobs[2]);
        trimRanges = [[0.1, 0.4]];
        singleButton.doSingleRender();
        verifySingleExport(1);
        compare(JSON.stringify(jobs[1].trim), JSON.stringify(trimRanges));
        jobs[1].status = "finished";
        queue.main_job_id = 0;
        calls = [];

        trimRanges = [[0.6, 0.9]];
        singleButton.doSingleRender();
        tryVerify(function() { return calls.length > 0; });
        compare(notices, 0, "repeat export must not try to enqueue a duplicate");
        verifySingleExport(1);
        compare(JSON.stringify(jobs[1].trim), JSON.stringify(trimRanges));
        compare(Object.keys(jobs).length, 2);
        compare(JSON.stringify(jobs[2]), otherBefore);
        compare(queue.editing_job_id, 1);
        compare(savedBaseline, getAdditionalProjectDataJson());
        compare(videoArea.queue.shown, false);
    }
    function test_new_preview_can_export_twice() {
        queue.editing_job_id = 0;
        videoArea.loadedFileUrl = "file:///new.mp4";
        singleButton.doSingleRender();
        verifySingleExport(3);
        jobs[3].status = "finished";
        calls = [];
        trimRanges = [[0.4, 0.7]];
        singleButton.doSingleRender();
        tryVerify(function() { return calls.length > 0; });
        compare(notices, 0);
        verifySingleExport(3);
        compare(JSON.stringify(jobs[3].trim), JSON.stringify(trimRanges));
        compare(Object.keys(jobs).length, 3);
    }
    function test_rejected_duplicate_does_not_start_or_bind_a_nonexistent_job() {
        queue.editing_job_id = 0;
        const before = JSON.stringify(jobs);
        singleButton.doSingleRender();
        tryCompare(testCase, "notices", 1);
        compare(calls.join(","), "save");
        compare(queue.main_job_id, 0);
        compare(queue.editing_job_id, 0);
        compare(JSON.stringify(jobs), before);
        compare(savedBaseline, "");
    }
    function test_rejected_addition_clears_temporary_queue_intent() {
        queue.editing_job_id = 0;
        renderBtn.tempIsAddToQueue = true;
        renderBtn.render();
        tryCompare(testCase, "notices", 1);
        compare(renderBtn.tempIsAddToQueue, false);
        compare(calls.join(","), "save");
        compare(renderBtn.addQueueDelayed, false);
    }
    function test_full_render_slots_requeue_only_current_video() {
        queue.activeCount = 1;
        singleButton.doSingleRender();
        tryCompare(renderBtn, "addQueueDelayed", true);
        compare(calls[0], "save");
        compare(calls[calls.length - 1], "start");
        compare(jobs[1].status, "queued");
        compare(jobs[2].status, "finished");
    }
    function test_rejected_output_folder_does_not_reset_job() {
        folderAllowed = false;
        singleButton.doSingleRender();
        wait(20);
        compare(calls.length, 0);
        compare(jobs[1].status, "finished");
    }
    function test_crm_stays_project_only() {
        crm = true;
        singleButton.doSingleRender();
        compare(notices, 1);
        compare(calls.length, 0);
        compare(jobs[1].status, "finished");
    }
}
