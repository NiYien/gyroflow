// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/components"

// Run the production enqueue, completion and dispatch functions together.
TestCase {
    id: testCase
    name: "QueueRematchOnAdd"
    property var window: testCase
    property var root
    property var flows
    property string queueSource
    property string appSource
    property var calls: []
    property var notices: []
    property bool syncDirty: true
    property bool useMobileWorkspace: false
    property bool isSandboxed: false
    property var motionData: null
    property var batchState: ({ autoRotate: false })
    property var videoArea: ({ queue: root })
    property var exportSettings: ({ isPreserveActive: function() { return false; } })
    property var fileDialog: ({ extensions: ["mp4", "mov", "gyroflow"] })
    property var controller: ({ supports_native_crm: function() { return true; }, check_external_sdk: function() { return false; } })
    property var filesystem: ({ get_filename: function(url) { return url.toString().split("/").pop(); } })
    property var matchTimer: ({ start: function() { calls.push("match"); } })
    property var r3dSeqLoader: ({ waiting: false, loadNext: function() {} })
    QtObject {
        id: loader
        property var pendingJobs: ({})
        property var skippedFiles: []
        property var duplicateFiles: []
        property bool active: false
        property string text: ""
        function updateStatus() { active = Object.keys(pendingJobs).length > 0; }
    }
    QtObject {
        id: backend
        property int nextId: 10
        property int main_job_id: -1
        property bool noMatches: false
        property var lensMismatches: []
        property bool auto_rotate: false
        property var queue: ({ rowCount: function() { return 2; } })
        function add_file(url, gyro, data) { return nextId++; }
        function filter_paired_gyroflow_siblings(urls, extensions) { return urls; }
        function filter_raw_proxy_siblings(urls, extensions) { return urls; }
        function crm_proxy_pairs(urls) { return "[]"; }
        function sort_jobs_by_filename() {}
        function revive_stuck_jobs(intent) {}
        function batch_motion_ready() { return true; }
        function match_all_no_gyro() { return noMatches; }
        function get_deep_match_lens_mismatches_json() { return JSON.stringify(lensMismatches); }
    }
    property alias render_queue: backend
    function getAdditionalProjectData() { return { output: {} }; }
    function deferIosPhotoQueueOutputCheck(urls) { return false; }
    function saveEditingJobIfDirty(done) { done(); }
    function queueVideoOutputFolderGatePasses(unused) { return true; }
    function runSimpleBatchSync() { calls.push("sync"); }
    function runSimpleBatchExport() { calls.push("export"); }
    function confirmReExport(kind) { calls.push("confirm:" + kind); }
    function messageBox(kind, text, buttons) { notices.push({ kind: kind, text: text, buttons: buttons }); }
    function getReadableError(text) { return text; }

    function readSource(path) {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl(path), false);
        request.send();
        verify(request.responseText.length > 0);
        return request.responseText.replace(/\r\n/g, "\n");
    }
    function extract(source, name, indent) {
        const marker = indent + "function " + name + "(";
        const start = source.indexOf(marker);
        verify(start >= 0, name + " exists in production source");
        const end = source.indexOf("\n" + indent + "}", start);
        verify(end > start, name + " closes at its declaration indentation");
        return source.slice(start, end + indent.length + 2);
    }
    function initTestCase() {
        queueSource = readSource("../../src/ui/RenderQueue.qml");
        appSource = readSource("../../src/ui/App.qml");
    }
    function init() {
        calls = [];
        notices = [];
        syncDirty = true;
        loader.pendingJobs = ({});
        loader.skippedFiles = [];
        loader.duplicateFiles = [];
        backend.nextId = 10;
        backend.noMatches = false;
        backend.lensMismatches = [];
        let code = 'import QtQuick; import "../../src/ui/components"; QtObject { id: root;'
            + 'property bool matchDirty: false; property bool hasGyroFiles: true; property bool shown: true;'
            + 'property string pendingAction: ""; property bool matching: false; property int matchVersion: 0;'
            + 'property var pendingSequenceMeta: ({}); function seqKey(url) { return url.toString(); }'
            + 'function checkBatchDrain() {} function requestQueueLayout() {}'
            + 'property string matchWarning: "";';
        for (const name of ["beginMatchThenSync", "dispatchMatchedAction", "deepMatchLensMismatchLines", "runAutoMatch"])
            code += extract(queueSource, name, "    ");
        // This helper is absent on the pre-fix version; test the old enqueue unchanged.
        if (queueSource.indexOf("    function trackPendingFile(") >= 0)
            code += extract(queueSource, "trackPendingFile", "    ");
        for (const name of ["prepareBatchAdditionalData", "add", "onMatch_apply_finished"])
            code += extract(queueSource, name, "        ");
        for (const name of ["onAdded", "onAdd_skipped", "onError"])
            code += extract(queueSource, name, "            ");
        root = Qt.createQmlObject(code + '}', testCase);
        flows = Qt.createQmlObject('import QtQuick; QtObject {'
            + extract(appSource, "runPluginStabilizeFlow", "    ")
            + extract(appSource, "runStabilizedBatchExport", "    ") + '}', testCase);
    }
    function cleanup() { flows.destroy(); root.destroy(); }
    function enqueue(url) { root.add("file:///output/", [url], {}); return backend.nextId - 1; }
    function test_new_video_rematches_before_stabilize() {
        const id = enqueue("file:///C20261013_1290.MP4");
        compare(root.matchDirty, false, "do not invalidate until enqueue succeeds");
        root.onAdded(id);
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "match", "new Sony motion does not bypass external lens assignment");
        root.onMatch_apply_finished();
        compare(calls.join(","), "match,sync");
        compare(root.matchDirty, false);
        compare(notices.length, 0, "normal matching continues without another click");
    }
    function test_new_video_rematches_before_video_export() {
        root.onAdded(enqueue("file:///next.MP4"));
        flows.runStabilizedBatchExport();
        compare(calls.join(","), "match");
        root.onMatch_apply_finished();
        compare(calls.join(","), "match,export");
        compare(notices.length, 0);
    }
    function test_settled_sync_still_rematches_new_video() {
        syncDirty = false;
        root.onAdded(enqueue("file:///next.MP4"));
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "match", "do not mistake a new input for a completed batch");
    }
    function test_project_import_keeps_saved_assignment() {
        root.onAdded(enqueue("file:///saved.GYROFLOW"));
        compare(root.matchDirty, false);
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "sync");
    }
    function test_duplicate_does_not_rematch() {
        root.onAdd_skipped(enqueue("file:///existing.MP4"), "existing.MP4", "duplicate");
        compare(root.matchDirty, false);
        compare(Object.keys(loader.pendingJobs).length, 0);
    }
    function test_failed_add_does_not_rematch() {
        root.onError(enqueue("file:///bad.MP4"), "%1", "unreadable", "");
        compare(root.matchDirty, false);
        compare(Object.keys(loader.pendingJobs).length, 0);
    }
    function test_edit_or_preset_added_signal_does_not_rematch() {
        root.onAdded(123);
        compare(root.matchDirty, false);
    }
    function test_no_external_gyro_still_syncs_directly() {
        root.hasGyroFiles = false;
        root.onAdded(enqueue("file:///camera.MP4"));
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "sync");
    }
    function test_zero_match_does_not_dispatch_sync() {
        root.onAdded(enqueue("file:///next.MP4"));
        flows.runPluginStabilizeFlow();
        backend.noMatches = true;
        root.onMatch_apply_finished();
        compare(calls.join(","), "match");
    }
    function test_project_completion_does_not_clear_new_video_invalidation() {
        const video = enqueue("file:///next.MP4");
        const project = enqueue("file:///saved.gyroflow");
        root.onAdded(video);
        root.onAdded(project);
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "match");
    }
    function test_repeated_added_after_preset_does_not_rematch() {
        const id = enqueue("file:///next.MP4");
        root.onAdded(id);
        flows.runPluginStabilizeFlow();
        root.onMatch_apply_finished();
        calls = [];
        root.onAdded(id);
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "sync");
    }
    function test_sequential_raw_loader_rematches_on_success() {
        const rawLoader = Qt.createQmlObject('import QtQuick; QtObject {'
            + 'property var queue: ["file:///next.r3d"]; property string additional: "{}"; property bool waiting: false;'
            + extract(queueSource, "loadNext", "        ") + '}', testCase);
        try {
            r3dSeqLoader = rawLoader;
            rawLoader.loadNext();
            compare(root.matchDirty, false);
            root.onAdded(backend.nextId - 1);
            flows.runPluginStabilizeFlow();
            compare(calls.join(","), "match");
        } finally {
            r3dSeqLoader = ({ waiting: false, loadNext: function() {} });
            rawLoader.destroy();
        }
    }
    function test_many_new_videos_need_one_match_and_one_click() {
        for (let i = 0; i < 10; ++i) root.onAdded(enqueue("file:///clip" + i + ".MP4"));
        compare(calls.length, 0, "enqueue does not launch matching or processing");
        compare(notices.length, 0);
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "match", "one batch match, not one per clip");
        root.onMatch_apply_finished();
        compare(calls.join(","), "match,sync");
        compare(notices.length, 0);
        calls = [];
        flows.runPluginStabilizeFlow();
        compare(calls.join(","), "sync", "unchanged inputs do not match again");
    }
    function test_existing_lens_mismatch_review_is_preserved() {
        root.onAdded(enqueue("file:///next.MP4"));
        flows.runPluginStabilizeFlow();
        backend.lensMismatches = [{ filename: "next.MP4", probe_lens_index: 0, assigned_lens_index: 1 }];
        root.onMatch_apply_finished();
        compare(calls.join(","), "match", "do not silently accept a different lens");
        compare(notices.length, 1);
        compare(notices[0].buttons.length, 2);
        notices[0].buttons[1].clicked();
        compare(calls.join(","), "match,sync");
    }
}
