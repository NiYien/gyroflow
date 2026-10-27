// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "SimpleSyncPolicy"
    property var synchronization

    function initTestCase() {
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/menu/Synchronization.qml"), false);
        request.send();
        const source = request.responseText;
        const start = source.indexOf("    function getSettings(): var");
        const end = source.indexOf("    function getSettingsJson(): string", start);
        verify(start >= 0 && end > start);
        synchronization = Qt.createQmlObject(`import QtQuick; QtObject {
            property var window: ({ isSimpleMode: true })
            property var controller: ({
                opticalSupport: true,
                has_optical_sync_support: function() { return this.opticalSupport; },
                has_neuflow_support: function() { return false; }
            })
            property var settings: ({ value: function() { return false; } })
            property var sync: ({ offsetIsAnchor: false })
            property var initialOffset: ({ value: -1.5 })
            property var checkNegativeInitialOffset: ({ checked: false })
            property var syncSearchSize: ({ value: 5 })
            property var calculateInitialOffsetFirst: ({ checked: true })
            property var maxSyncPoints: ({ value: 3 })
            property var everyNthFrame: ({ value: 1 })
            property var timePerSyncpoint: ({ value: 1 })
            property var poseMethod: ({ currentIndex: 0 })
            property var experimentalAutoSyncPoints: ({ checked: false })
            property var syncMethod: ({ currentIndex: 2, ofMethodMap: [0, 1, 2] })
            property var offsetMethod: ({ currentIndex: 2 })
            ${source.substring(start, end)}
        }`, testCase, "SynchronizationProductionSettings");
    }

    function test_mode_and_backend_select_the_sync_method_data() {
        const cases = [];
        for (const simple of [true, false]) {
            for (const supported of [true, false]) {
                for (let method = 0; method < 4; ++method) {
                    cases.push({
                        tag: `${simple ? "simple" : "full"}_${supported}_${method}`,
                        simple: simple,
                        supported: supported,
                        method: method,
                        expected: simple ? (supported ? 3 : (method === 3 ? 2 : method)) : method
                    });
                }
            }
        }
        return cases;
    }

    function test_mode_and_backend_select_the_sync_method(data) {
        synchronization.window = { isSimpleMode: data.simple };
        synchronization.controller.opticalSupport = data.supported;
        synchronization.offsetMethod = { currentIndex: data.method };
        const result = synchronization.getSettings();
        compare(result.offset_method, data.expected);
        compare(result.initial_offset, -1.5);
        compare(result.max_sync_points, 3);
        compare(result.calc_initial_fast, true);
    }
}
