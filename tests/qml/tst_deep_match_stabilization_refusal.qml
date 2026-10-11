// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest

// Exercise the production launch and completion handlers. Requires QML_XHR_ALLOW_FILE_READ=1.
TestCase {
    id: testCase
    name: "DeepMatchStabilizationRefusal"
    property string queueSource
    property string mobileSource
    property var starter
    property var finishedConnection
    property var notices
    property int dialogsOpened: 0
    property int dialogsClosed: 0
    property var window: ({ useMobileWorkspace: false })
    property var root: ({ matchDirty: false })
    property var controller: ({ lens_group_manual_edit: false })
    property var gyroFilesInfo: [{ filename: "pool.bin" }]
    property var deepMatchDialogComponent: ({
        createObject: function(parent, properties) {
            dialogsOpened++;
            return { opened: false };
        }
    })
    property var deepMatchDialog: ({ jobId: 1, close: function() { dialogsClosed++; } })
    QtObject {
        id: backend
        property string refusal: "image_stabilization"
        property string entry: ""
        function start_deep_gyro_match(job, gyro, lens) { entry = "file"; return refusal; }
        function start_deep_gyro_match_all(job, lens) { entry = "pool"; return refusal; }
        signal deep_match_finished(int job_id, bool success, string error_kind, real offset_ms)
    }
    property alias render_queue: backend

    function messageBox(kind, text, buttons) { notices.push(text); }
    function readSource(path) {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl(path), false);
        request.send();
        verify(request.responseText.length > 0);
        return request.responseText.replace(/\r\n/g, "\n");
    }
    function productionFunction(source, marker, indent) {
        const start = source.indexOf(marker);
        verify(start >= 0, marker + " exists");
        const closing = "\n" + indent + "}";
        const end = source.indexOf(closing, start) + closing.length;
        verify(end > start);
        return source.slice(start, end);
    }
    function initTestCase() {
        queueSource = readSource("../../src/ui/RenderQueue.qml");
        mobileSource = readSource("../../src/ui/mobile/MobileWorkspace.qml");
    }
    function init() {
        notices = [];
        dialogsOpened = 0;
        dialogsClosed = 0;
        backend.refusal = "image_stabilization";
        backend.entry = "";
        starter = Qt.createQmlObject('import QtQuick; import "../../src/ui/components"; QtObject {'
            + productionFunction(queueSource, "function startDeepMatch(", "    ") + '}', testCase);
        finishedConnection = Qt.createQmlObject('import QtQuick; import "../../src/ui/components"; Connections { target: render_queue;'
            + productionFunction(queueSource, "function onDeep_match_finished(job_id: int", "                ") + '}', testCase);
    }
    function cleanup() { starter.destroy(); finishedConnection.destroy(); }
    function test_refused_launch_data() {
        return [{ tag: "file", gyro: 0 }, { tag: "pool", gyro: -1 }];
    }
    function test_refused_launch(data) {
        starter.startDeepMatch(1, data.gyro, "clip.mp4", -1);
        compare(backend.entry, data.tag);
        compare(dialogsOpened, 0, "a refused probe never opens a progress dialog");
        compare(notices.length, 1);
        verify(notices[0].indexOf("In-camera stabilization") >= 0);
    }
    function test_supported_launch_data() { return test_refused_launch_data(); }
    function test_supported_launch(data) {
        backend.refusal = "ok";
        starter.startDeepMatch(1, data.gyro, "clip.mp4", -1);
        compare(dialogsOpened, 1);
        compare(notices.length, 0);
    }
    function test_refused_after_loading_closes_the_dialog() {
        backend.deep_match_finished(1, false, "image_stabilization", 0);
        compare(dialogsClosed, 1);
        compare(notices.length, 1);
        verify(notices[0].indexOf("In-camera stabilization") >= 0);
    }
    function test_cancel_still_closes_silently() {
        backend.deep_match_finished(1, false, "cancelled", 0);
        compare(dialogsClosed, 1);
        compare(notices.length, 0);
    }
    function test_another_jobs_result_does_not_close_the_dialog() {
        backend.deep_match_finished(2, false, "image_stabilization", 0);
        compare(dialogsClosed, 0);
        compare(notices.length, 0);
    }
    function test_mobile_completion_clears_the_operation_and_reports_the_reason() {
        const mobile = Qt.createQmlObject('import QtQuick; QtObject {'
            + 'property int deepJobId: 1; property bool deepSucceeded: false;'
            + 'property var operation: ({ kind: "deep", active: true });'
            + 'property string summary: ""; property string panel: ""; property string lastTaskMessage: "";'
            + 'function skipDetailText(reason) { return "In-camera stabilization"; } function refresh() {}'
            + productionFunction(mobileSource, "function deepFinished(", "    ") + '}', testCase);
        mobile.deepFinished(1, false, "image_stabilization", 0);
        compare(mobile.operation.active, false);
        compare(mobile.summary, "In-camera stabilization");
        compare(mobile.lastTaskMessage, mobile.summary);
        compare(mobile.panel, "task");
        mobile.destroy();
    }
}
