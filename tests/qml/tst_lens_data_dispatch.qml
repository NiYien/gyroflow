// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest

// Execute the production dialog and dispatch callbacks. Requires QML_XHR_ALLOW_FILE_READ=1.
TestCase {
    id: testCase
    name: "LensDataDispatch"
    property string appSource
    property var result
    property var dialogs
    property var calls
    property var motionData: null
    property var videoArea: ({ queue: { notifyBatchSyncStarted: function() { calls.push("notify"); } } })
    QtObject {
        id: backend
        property int export_project: 0
        function get_lens_groups_missing_data_json(intent) { calls.push("check:" + intent); return JSON.stringify(result); }
        function skip_lens_data_jobs(ids) {
            calls.push("skip:" + ids);
            const excluded = JSON.parse(ids);
            result = { jobs: result.jobs.filter(function(job) { return excluded.indexOf(job.job_id) < 0; }), ready_count: result.ready_count };
        }
        function dispatch_blocker_reason(intent) { return ""; }
        function get_anamorphic_applied_count() { return 0; }
        function has_crm_proxy_jobs() { return false; }
        function start_batch_autosync() { calls.push("sync"); }
        function prepare_finished_jobs_for_video_export() { calls.push("prepare-export"); }
        function prepare_video_exports_for_rerender() { calls.push("prepare-reexport"); }
        function start() { calls.push("export"); }
    }
    function messageBox(type, text, buttons) { dialogs.push({ text: text, buttons: buttons }); }
    function queueVideoOutputFolderGatePasses(includeFinished) { return true; }
    function runQueueOutputAction(callback) { callback(); }
    function lensDataGatePasses(intent, callback) { return production("lensDataGatePasses", intent, callback); }
    function runSimpleBatchSync() { production("runSimpleBatchSync"); }
    function runSimpleBatchExport() { production("runSimpleBatchExport"); }
    function production(name, intent, callback, kind) {
        const start = appSource.indexOf("function " + name + "(");
        verify(start >= 0);
        const bodyStart = appSource.indexOf("{", start) + 1;
        const end = appSource.indexOf("\n    }", bodyStart);
        const action = new Function("window", "render_queue", "videoArea", "lensDataGatePasses",
            "queueVideoOutputFolderGatePasses", "messageBox", "Modal", "qsTr", "intent", "onContinue", "kind",
            appSource.slice(bodyStart, end));
        return action(testCase, backend, videoArea, lensDataGatePasses, queueVideoOutputFolderGatePasses,
            messageBox, { Warning: 1, Question: 2 }, function(text) { return text; }, intent, callback, kind);
    }
    function initTestCase() {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/App.qml"), false);
        request.send();
        appSource = request.responseText.replace(/\r\n/g, "\n");
        verify(appSource.length > 0);
    }
    function init() {
        result = { jobs: [{ job_id: 7, filename: "private-clip.mp4", lens_index: 2, reasons: ["sensor", "anamorphic"] }], ready_count: 1 };
        dialogs = [];
        calls = [];
        backend.export_project = 0;
    }
    function test_valid_queue_runs_without_a_dialog() {
        result = { jobs: [], ready_count: 2 };
        runSimpleBatchSync();
        compare(dialogs.length, 0);
        compare(calls.join(","), "check:sync,notify,sync");
    }
    function test_mixed_queue_reports_only_the_number_and_waits() {
        runSimpleBatchSync();
        compare(dialogs.length, 1);
        compare(dialogs[0].text, "1 video(s) have problems and will be skipped.");
        compare(dialogs[0].buttons.length, 2);
        compare(dialogs[0].buttons[0].text, "Continue processing");
        compare(calls.join(","), "check:sync");
    }
    function test_cancel_does_not_skip_or_dispatch() {
        runSimpleBatchSync();
        verify(!dialogs[0].buttons[1].clicked);
        compare(calls.join(","), "check:sync");
        compare(result.jobs.length, 1);
    }
    function test_continue_skips_then_starts_stabilization() {
        runSimpleBatchSync();
        dialogs[0].buttons[0].clicked();
        compare(calls.join(","), "check:sync,skip:[7],check:sync,notify,sync");
        compare(dialogs.length, 1);
    }
    function test_continue_skips_then_starts_export() {
        runSimpleBatchExport();
        dialogs[0].buttons[0].clicked();
        compare(calls.join(","), "check:export,skip:[7],check:export,prepare-export,export");
        compare(backend.export_project, 4);
    }
    function test_reexport_continues_without_repeating_the_reexport_question() {
        production("confirmReExport", undefined, undefined, "video");
        dialogs[0].buttons[0].clicked();
        compare(dialogs.length, 2);
        compare(calls.join(","), "check:reexport");
        dialogs[1].buttons[0].clicked();
        compare(calls.join(","), "check:reexport,skip:[7],prepare-reexport,export");
        compare(dialogs.length, 2);
    }
    function test_all_invalid_has_only_acknowledgement_and_no_followup() {
        result.ready_count = 0;
        runSimpleBatchSync();
        compare(dialogs[0].buttons.length, 1);
        compare(dialogs[0].buttons[0].text, "Ok");
        verify(!dialogs[0].buttons[0].clicked);
        compare(dialogs[0].text, "1 video(s) have problems. No videos can be processed.");
        compare(calls.join(","), "check:sync");
        compare(result.jobs.length, 1);
    }
    function test_count_is_per_video_instead_of_per_reason_or_group() {
        result.jobs.push({ job_id: 8, lens_index: 2, reasons: ["sensor"] });
        runSimpleBatchExport();
        compare(dialogs[0].text, "2 video(s) have problems and will be skipped.");
        dialogs[0].buttons[0].clicked();
        verify(calls.indexOf("skip:[7,8]") >= 0);
    }
}
