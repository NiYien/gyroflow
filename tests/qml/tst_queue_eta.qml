// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "QueueEta"
    property var estimateView

    function initTestCase() {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/RenderQueue.qml"), false);
        request.send();
        const source = request.responseText;
        const start = source.indexOf("            function refreshRemainingEstimate(): void");
        const end = source.indexOf("            function currentRemainingMs(): real", start);
        verify(start >= 0 && end > start);
        estimateView = Qt.createQmlObject(`import QtQuick; QtObject {
            property string queueStatus: "active"
            property real remainingBaseMs: -1
            property double remainingBaseTimestamp: 0
            property var render_queue: ({ estimated_remaining_ms: 10000 })
            property int updates: 0
            function updateTimes() { updates++; }
            ${source.substring(start, end)}
        }`, testCase, "QueueEtaProductionFunction");
    }

    function init() {
        estimateView.queueStatus = "active";
        estimateView.remainingBaseMs = -1;
        estimateView.remainingBaseTimestamp = 0;
        estimateView.render_queue = { estimated_remaining_ms: 10000 };
        estimateView.updates = 0;
    }

    function test_available_estimate_starts_the_countdown() {
        estimateView.refreshRemainingEstimate();
        compare(estimateView.remainingBaseMs, 10000);
        verify(estimateView.remainingBaseTimestamp > 0);
        compare(estimateView.updates, 1);
    }

    function test_analysis_without_an_estimate_clears_the_old_countdown() {
        estimateView.refreshRemainingEstimate();
        estimateView.render_queue = { estimated_remaining_ms: -1 };
        estimateView.refreshRemainingEstimate();
        compare(estimateView.remainingBaseMs, -1);
        compare(estimateView.remainingBaseTimestamp, 0);
        compare(estimateView.updates, 2);
    }

    function test_stopped_queue_clears_the_countdown() {
        estimateView.refreshRemainingEstimate();
        estimateView.queueStatus = "stopped";
        estimateView.refreshRemainingEstimate();
        compare(estimateView.remainingBaseMs, -1);
        compare(estimateView.remainingBaseTimestamp, 0);
    }
}
