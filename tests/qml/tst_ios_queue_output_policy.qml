// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "IosQueueOutputPolicy"
    when: windowShown

    property var filesystemObject: null
    property var renderQueueObject: null
    property var outputFileObject: null
    property var exportSettingsObject: null
    property var policy: null
    property int callbackCalls: 0

    Component {
        id: filesystemFactory
        QtObject {
            function is_ios_photo_import(url): bool {
                return url.toString().indexOf("/ios-photo-imports/") >= 0
            }
        }
    }

    Component {
        id: renderQueueFactory
        QtObject {
            property bool needsOutputFolder: false
            property int clearBlockCalls: 0
            function ios_photo_jobs_need_output_folder(): bool {
                return needsOutputFolder
            }
            function clear_output_folder_block(): void {
                clearBlockCalls++
            }
        }
    }

    Component {
        id: outputFileFactory
        QtObject {
            property int selectFolderCalls: 0
            property var selectedCallback: null
            property var rejectedCallback: null
            function selectFolder(folder, callback, onRejected): void {
                selectFolderCalls++
                selectedCallback = callback
                rejectedCallback = onRejected
            }
        }
    }

    Component {
        id: exportSettingsFactory
        QtObject {
            property int queueOutputMode: 0
            property string queueFixedOutputPath: ""
        }
    }

    function countCallback(): void {
        callbackCalls++
    }

    function init(): void {
        filesystemObject = filesystemFactory.createObject(testCase)
        renderQueueObject = renderQueueFactory.createObject(testCase)
        outputFileObject = outputFileFactory.createObject(testCase)
        exportSettingsObject = exportSettingsFactory.createObject(testCase)

        const component = Qt.createComponent("../../src/ui/components/IosQueueOutputPolicy.qml")
        verify(component.status === Component.Ready, component.errorString())
        policy = component.createObject(testCase, {
            platformOs: "ios",
            filesystemObject: filesystemObject,
            renderQueueObject: renderQueueObject,
            outputFileObject: outputFileObject,
            exportSettingsObject: exportSettingsObject
        })
        verify(policy !== null)
    }

    function cleanup(): void {
        if (policy) policy.destroy()
        if (exportSettingsObject) exportSettingsObject.destroy()
        if (outputFileObject) outputFileObject.destroy()
        if (renderQueueObject) renderQueueObject.destroy()
        if (filesystemObject) filesystemObject.destroy()
        policy = null
        exportSettingsObject = null
        outputFileObject = null
        renderQueueObject = null
        filesystemObject = null
        callbackCalls = 0
    }

    function test_singleAndMultiplePhotosDeferAddTimeOutputCheck(): void {
        verify(policy.deferAddTimeOutputCheck([
            "file:///cache/ios-photo-imports/session/a.mov"
        ]))
        verify(policy.deferAddTimeOutputCheck([
            "file:///cache/ios-photo-imports/session/a.mov",
            "file:///cache/ios-photo-imports/session/b.mov"
        ]))
    }

    function test_filesAndMixedSelectionsKeepExistingAddTimeCheck(): void {
        verify(!policy.deferAddTimeOutputCheck(["file:///On My iPhone/a.mov"]))
        verify(!policy.deferAddTimeOutputCheck([
            "file:///cache/ios-photo-imports/session/a.mov",
            "file:///On My iPhone/b.mov"
        ]))
        policy.platformOs = "android"
        verify(!policy.deferAddTimeOutputCheck([
            "file:///cache/ios-photo-imports/session/a.mov"
        ]))
    }

    function test_missingPhotoOutputSelectsAndRemembersFolder(): void {
        policy.resolveOutputFolder(countCallback)

        compare(callbackCalls, 0)
        compare(outputFileObject.selectFolderCalls, 1)
        verify(outputFileObject.selectedCallback !== null)
        verify(outputFileObject.rejectedCallback !== null)

        outputFileObject.selectedCallback("file:///On My iPhone/Gyroflow/")
        tryCompare(testCase, "callbackCalls", 1)
        compare(policy.selecting, false)
        compare(exportSettingsObject.queueOutputMode, 1)
        compare(exportSettingsObject.queueFixedOutputPath, "file:///On My iPhone/Gyroflow/")
    }

    function test_cancelAllowsANewOutputRequestWithoutStartingTheOldOne(): void {
        policy.resolveOutputFolder(countCallback)
        compare(policy.selecting, true)

        outputFileObject.rejectedCallback()

        compare(policy.selecting, false)
        compare(callbackCalls, 0)
        policy.resolveOutputFolder(countCallback)
        compare(outputFileObject.selectFolderCalls, 2)
    }

    function test_preflightCancelDoesNotRunStatefulAction(): void {
        renderQueueObject.needsOutputFolder = true

        policy.runBeforeAction(countCallback)
        compare(callbackCalls, 0)
        outputFileObject.rejectedCallback()

        compare(callbackCalls, 0)
        compare(renderQueueObject.clearBlockCalls, 0)

        policy.runBeforeAction(countCallback)
        outputFileObject.selectedCallback("file:///On My iPhone/Gyroflow/")
        tryCompare(testCase, "callbackCalls", 1)
        compare(renderQueueObject.clearBlockCalls, 1)
    }

    function test_preflightWithoutMissingOutputRunsImmediately(): void {
        policy.runBeforeAction(countCallback)

        compare(callbackCalls, 1)
        compare(renderQueueObject.clearBlockCalls, 1)
        compare(outputFileObject.selectFolderCalls, 0)
    }
}
