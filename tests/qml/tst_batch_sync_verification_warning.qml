// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/components"

TestCase {
    id: testCase
    name: "BatchSyncVerificationWarning"
    property var root
    property var window: testCase
    property var notices: []
    property bool syncDirty: true
    property bool deepMatchStabilizePending: true
    property bool useMobileWorkspace: false
    QtObject {
        id: backend
        property string kind: "none"
        function get_batch_sync_prompt_kind() { return kind; }
    }
    property alias render_queue: backend
    function messageBox(kind, text, buttons) {
        notices.push({ kind: kind, text: text, buttons: buttons });
    }
    function init() {
        notices = [];
        syncDirty = true;
        deepMatchStabilizePending = true;
        backend.kind = "none";
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/RenderQueue.qml"), false);
        request.send();
        const source = request.responseText.replace(/\r\n/g, "\n");
        const start = source.indexOf("        function onBatch_sync_status_changed()");
        const end = source.indexOf("\n        }", start);
        verify(start >= 0 && end > start);
        root = Qt.createQmlObject('import QtQuick; import "../../src/ui/components"; QtObject {'
            + 'id: root; property int syncStatusVersion: 0;'
            + 'property string lastBatchSyncPromptKind: "none";'
            + 'property bool _batchSyncInFlight: true;'
            + 'function requestQueueLayout() {}'
            + source.slice(start, end + 10) + '}', testCase, "warningHandler");
    }
    function cleanup() { root.destroy(); }
    function test_verification_failure_only_warns_data() {
        return [{ tag: "desktop", mobile: false }, { tag: "mobile", mobile: true }];
    }
    function test_verification_failure_only_warns(data) {
        useMobileWorkspace = data.mobile;
        backend.kind = "verification_warning";
        root.onBatch_sync_status_changed();
        compare(notices.length, 1);
        compare(notices[0].kind, Modal.Warning);
        verify(notices[0].text.indexOf("Individual sync results have been kept") >= 0);
        compare(notices[0].buttons.length, 1);
        compare(root._batchSyncInFlight, false);
        compare(syncDirty, false);
        compare(deepMatchStabilizePending, false);
        root.onBatch_sync_status_changed();
        compare(notices.length, 1);
        backend.kind = "none";
        root.onBatch_sync_status_changed();
        backend.kind = "verification_warning";
        root.onBatch_sync_status_changed();
        compare(notices.length, 2);
    }
    function test_no_gyro_keeps_deep_match_guidance() {
        useMobileWorkspace = false;
        backend.kind = "all_yellow";
        root.onBatch_sync_status_changed();
        compare(notices.length, 1);
        verify(notices[0].text.indexOf("Deep match with gyro") >= 0);
    }
}
