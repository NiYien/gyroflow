// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "FolderSelectionContinuation"
    when: windowShown

    property var continuation: null
    property var acceptedValues: []
    property int rejectedCalls: 0

    function init(): void {
        const component = Qt.createComponent("../../src/ui/components/FolderSelectionContinuation.qml")
        verify(component.status === Component.Ready, component.errorString())
        continuation = component.createObject(testCase)
        verify(continuation !== null)
        acceptedValues = []
        rejectedCalls = 0
    }

    function cleanup(): void {
        if (continuation) continuation.destroy()
        continuation = null
    }

    function recordAccepted(value): void {
        acceptedValues.push(value)
    }

    function recordRejected(): void {
        rejectedCalls++
    }

    function test_acceptIsOneShot(): void {
        continuation.begin(recordAccepted, recordRejected)

        continuation.accept("file:///output/")
        continuation.accept("file:///unexpected/")

        compare(acceptedValues, ["file:///output/"])
        compare(rejectedCalls, 0)
    }

    function test_rejectClearsTheOldAcceptCallback(): void {
        continuation.begin(recordAccepted, recordRejected)
        continuation.reject()

        continuation.begin(function(value) { acceptedValues.push("new:" + value) }, recordRejected)
        continuation.accept("file:///later/")

        compare(rejectedCalls, 1)
        compare(acceptedValues, ["new:file:///later/"])
    }
}
