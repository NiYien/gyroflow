// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/mobile/MobileLogic.js" as Logic

TestCase {
    id: testCase
    name: "MobileExportPolicy"
    property var policy
    property var calls: []
    property bool writable: true
    property string displayedFolder: "file:///old/"
    property var window: testCase
    property var filesystem: ({
        folder_access_granted: function(url) { calls.push("grant:" + url); },
        can_create_file: function(url, name) { calls.push("probe:" + url); return writable; },
        save_allowed_folders: function() { calls.push("persist"); }
    })
    property var outputFile: ({ setFolder: function(url) { displayedFolder = url; calls.push("display:" + url); } })
    function messageBox(type, message, buttons) { calls.push("error"); }
    function readFunction(name) {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/menu/Export.qml"), false);
        request.send();
        const source = request.responseText.replace(/\r\n/g, "\n");
        const start = source.indexOf("    function " + name + "(");
        verify(start >= 0);
        const end = source.indexOf("\n    }", start) + 6;
        return source.slice(start, end);
    }
    function init() {
        calls = []; writable = true; displayedFolder = "file:///old/";
        policy = Qt.createQmlObject('import QtQuick; import "../../src/ui/components"; QtObject {'
            + 'property var root: this;'
            + 'property bool committingOutputFolder: false; property bool mobileSaveToGallery: true;'
            + 'property int queueOutputMode: 0; property string queueFixedOutputPath: "file:///old/";'
            + 'function syncQueueOutputPath() { calls.push("backend:" + queueOutputMode + ":" + queueFixedOutputPath); }'
            + readFunction("applySelectedOutputFolder") + '}', testCase);
    }
    function cleanup() { policy.destroy(); }
    function test_selectedFolderCommitsPermissionPathAndModeBeforeContinuing() {
        verify(policy.applySelectedOutputFolder("file:///new/"));
        compare(policy.mobileSaveToGallery, false);
        compare(policy.queueOutputMode, 1);
        compare(policy.queueFixedOutputPath, "file:///new/");
        compare(displayedFolder, "file:///new/");
        compare(calls.join(","), "grant:file:///new/,probe:file:///new/,persist,display:file:///new/,backend:1:file:///new/");
    }
    function test_unwritableFolderDoesNotChangeOutputSettings() {
        writable = false;
        verify(!policy.applySelectedOutputFolder("file:///denied/"));
        compare(policy.mobileSaveToGallery, true);
        compare(policy.queueFixedOutputPath, "file:///old/");
        compare(displayedFolder, "file:///old/");
        compare(calls.join(","), "grant:file:///denied/,probe:file:///denied/,error");
    }
    function test_emptySelectionIsInert() {
        verify(!policy.applySelectedOutputFolder(""));
        compare(calls.length, 0);
        compare(policy.mobileSaveToGallery, true);
    }
    function test_galleryFailureIsNotReportedAsDelivered() {
        const initial = [{ id: 1, status: "Queued", epoch: 0 }];
        const operation = Logic.beginOperation(1, "export", initial);
        const result = Logic.observeOperation(operation, [{id: 1, status: "Finished", epoch: 1, lastExport: 4, galleryState: "failed"}], true);
        compare(Logic.counts(result).success, 0);
        compare(Logic.counts(result).failed, 1);
    }
}
