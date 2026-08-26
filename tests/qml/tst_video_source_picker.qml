// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "VideoSourcePicker"
    when: windowShown

    property var hostObject: null
    property var filesystemObject: null
    property var fallbackDialog: null
    property var picker: null
    property var callbackUrls: null

    Component {
        id: hostFactory
        QtObject {
            property var pendingPickerCallback: null
            property int openPickerCalls: 0
            property int messageBoxCalls: 0
            property int lastMode: -1
            property bool lastAllowMultiple: false
            property var lastCallback: null
            property var lastFallback: null
            property int lastMessageType: -1
            property string lastMessageText: ""
            property var lastButtons: []

            function openPicker(mode, allowMultiple, callback, fallback): void {
                openPickerCalls++
                lastMode = mode
                lastAllowMultiple = allowMultiple
                lastCallback = callback
                lastFallback = fallback
            }
            function messageBox(type, text, buttons): var {
                messageBoxCalls++
                lastMessageType = type
                lastMessageText = text
                lastButtons = buttons
                return null
            }
        }
    }

    Component {
        id: filesystemFactory
        QtObject {
            signal picker_cancelled()
            signal picker_error(string message)
            property int nativeOpenCalls: 0
            property bool nativeOpenResult: true

            function open_ios_video_picker(): bool {
                nativeOpenCalls++
                return nativeOpenResult
            }
        }
    }

    Component {
        id: fallbackFactory
        QtObject {
            property int open2Calls: 0
            property int openCalls: 0
            function open2(): void { open2Calls++ }
            function open(): void { openCalls++ }
        }
    }

    function selectedCallback(urls): void {
        callbackUrls = urls
    }

    function init(): void {
        hostObject = hostFactory.createObject(testCase)
        filesystemObject = filesystemFactory.createObject(testCase)
        fallbackDialog = fallbackFactory.createObject(testCase)
        const component = Qt.createComponent("../../src/ui/components/VideoSourcePicker.qml")
        verify(component.status === Component.Ready, component.errorString())
        picker = component.createObject(testCase, {
            hostObject: hostObject,
            filesystemObject: filesystemObject,
            questionType: 7,
            errorType: 9
        })
        verify(picker !== null)
    }

    function cleanup(): void {
        if (picker) picker.destroy()
        if (fallbackDialog) fallbackDialog.destroy()
        if (filesystemObject) filesystemObject.destroy()
        if (hostObject) hostObject.destroy()
        picker = null
        fallbackDialog = null
        filesystemObject = null
        hostObject = null
        callbackUrls = null
    }

    function test_nonIosUsesExistingPicker(): void {
        picker.open("android", selectedCallback, fallbackDialog)

        compare(hostObject.openPickerCalls, 1)
        compare(hostObject.lastMode, 0)
        compare(hostObject.lastAllowMultiple, true)
        verify(hostObject.lastCallback === selectedCallback)
        verify(hostObject.lastFallback === fallbackDialog)
        compare(hostObject.messageBoxCalls, 0)
    }

    function test_iosPhotosUsesNativePicker(): void {
        picker.open("ios", selectedCallback, fallbackDialog)

        compare(hostObject.messageBoxCalls, 1)
        compare(hostObject.lastMessageType, 7)
        compare(hostObject.lastButtons.length, 3)
        hostObject.lastButtons[0].clicked()
        compare(filesystemObject.nativeOpenCalls, 1)
        verify(hostObject.pendingPickerCallback === selectedCallback)
        compare(fallbackDialog.open2Calls, 0)
    }

    function test_iosFilesUsesDocumentPicker(): void {
        picker.open("ios", selectedCallback, fallbackDialog)

        hostObject.lastButtons[1].clicked()
        compare(fallbackDialog.open2Calls, 1)
        compare(fallbackDialog.openCalls, 0)
        compare(filesystemObject.nativeOpenCalls, 0)
        compare(hostObject.pendingPickerCallback, null)
    }

    function test_nativeOpenFailureClearsPendingCallback(): void {
        filesystemObject.nativeOpenResult = false
        picker.open("ios", selectedCallback, fallbackDialog)

        hostObject.lastButtons[0].clicked()
        compare(filesystemObject.nativeOpenCalls, 1)
        compare(hostObject.pendingPickerCallback, null)
        compare(hostObject.messageBoxCalls, 2)
        compare(hostObject.lastMessageType, 9)
    }

    function test_cancelAndErrorClearPendingCallback(): void {
        picker.open("ios", selectedCallback, fallbackDialog)
        hostObject.lastButtons[0].clicked()
        verify(hostObject.pendingPickerCallback === selectedCallback)

        filesystemObject.picker_cancelled()
        compare(hostObject.pendingPickerCallback, null)

        hostObject.pendingPickerCallback = selectedCallback
        filesystemObject.picker_error("clip.mov")
        compare(hostObject.pendingPickerCallback, null)
        compare(hostObject.messageBoxCalls, 2)
        compare(hostObject.lastMessageType, 9)
        verify(hostObject.lastMessageText.indexOf("clip.mov") >= 0)
    }
}
