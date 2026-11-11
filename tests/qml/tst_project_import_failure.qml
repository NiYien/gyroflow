// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "ProjectImportFailure"
    when: windowShown
    property var subject

    function init() {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/VideoArea.qml"), false);
        request.send();
        const source = request.responseText;
        const start = source.indexOf("        function onGyroflow_file_load_failed(): void {");
        verify(start >= 0);
        const end = source.indexOf("        function onGyroflow_file_loaded(", start);
        verify(end > start);
        // Execute the production failure handler with a recording controller.
        const declaration = 'import QtQuick; Item { id: root;' +
            'property url pendingExternalGyroFallbackUrl: "file:///recording.bin";' +
            'property int pendingExternalGyroFallbackProjectVersion: 4;' +
            'property bool queueEditLoading: true;' +
            'property QtObject controller: QtObject {' +
                'signal gyroflow_file_load_failed(); property int telemetryLoads: 0;' +
                'function load_telemetry() { telemetryLoads++; }' +
            '}' +
            'Connections { target: root.controller;' + source.slice(start, end) + '}' +
            '}';
        subject = Qt.createQmlObject(declaration, testCase, "production-project-import-failure");
        verify(subject !== null);
    }

    function cleanup() { subject.destroy(); }

    function test_failure_discards_pending_gyro_fallback() {
        subject.controller.gyroflow_file_load_failed();
        compare(subject.pendingExternalGyroFallbackUrl.toString(), "");
        compare(subject.pendingExternalGyroFallbackProjectVersion, 0);
        compare(subject.queueEditLoading, false);
        compare(subject.controller.telemetryLoads, 0);
    }
}
