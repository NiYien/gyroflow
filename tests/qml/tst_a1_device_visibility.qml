// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/Util.js" as Util

TestCase {
    name: "A1DeviceVisibility"

    function visibility(data) {
        verify(typeof Util.shouldShowA1RealtimeDevice === "function",
               "Util.shouldShowA1RealtimeDevice must define the A1 card visibility policy")
        if (typeof Util.shouldShowA1RealtimeDevice !== "function")
            return undefined
        return Util.shouldShowA1RealtimeDevice(
            data.platformOs,
            data.connected,
            data.otaState,
            data.connectionStatus)
    }

    function test_visibility_data() {
        return [
            {
                tag: "iOS hides a connected A1",
                platformOs: "ios",
                connected: true,
                otaState: "none",
                connectionStatus: "connected",
                expected: false
            },
            {
                tag: "iOS hides unsupported status",
                platformOs: "ios",
                connected: false,
                otaState: "none",
                connectionStatus: "unsupported",
                expected: false
            },
            {
                tag: "iOS hides active OTA state",
                platformOs: "ios",
                connected: false,
                otaState: "transferring",
                connectionStatus: "idle",
                expected: false
            },
            {
                tag: "Android keeps permission errors visible",
                platformOs: "android",
                connected: false,
                otaState: "none",
                connectionStatus: "permission_denied",
                expected: true
            },
            {
                tag: "Desktop keeps connected devices visible",
                platformOs: "osx",
                connected: true,
                otaState: "none",
                connectionStatus: "connected",
                expected: true
            },
            {
                tag: "Idle non-iOS device stays hidden",
                platformOs: "windows",
                connected: false,
                otaState: "none",
                connectionStatus: "idle",
                expected: false
            }
        ]
    }

    function test_visibility(data) {
        compare(visibility(data), data.expected)
    }
}
