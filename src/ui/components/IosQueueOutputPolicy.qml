// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

QtObject {
    id: root

    required property string platformOs
    required property var filesystemObject
    required property var renderQueueObject
    required property var outputFileObject
    required property var exportSettingsObject
    property bool selecting: false

    function deferAddTimeOutputCheck(urls: var): bool {
        if (root.platformOs !== "ios" || !urls || urls.length === 0) return false
        for (const url of urls) {
            if (!root.filesystemObject.is_ios_photo_import(url)) return false
        }
        return true
    }

    function resolveOutputFolder(callback: var, rejectedCallback: var): void {
        if (root.selecting) return
        root.selecting = true
        root.outputFileObject.selectFolder("", function(folderUrl) {
            root.selecting = false
            if (!folderUrl || !folderUrl.toString()) {
                if (rejectedCallback) rejectedCallback()
                return
            }
            root.exportSettingsObject.queueFixedOutputPath = folderUrl
            root.exportSettingsObject.queueOutputMode = 1
            Qt.callLater(callback)
        }, function() {
            root.selecting = false
            if (rejectedCallback) rejectedCallback()
        })
    }

    function runBeforeAction(callback: var): void {
        if (root.platformOs !== "ios"
                || !root.renderQueueObject.ios_photo_jobs_need_output_folder()) {
            root.renderQueueObject.clear_output_folder_block()
            callback()
            return
        }
        root.resolveOutputFolder(function() {
            root.renderQueueObject.clear_output_folder_block()
            callback()
        }, function() {
            // Cancelling is intentionally inert: the caller has not yet
            // changed batch bookkeeping or requeued finished jobs.
        })
    }
}
