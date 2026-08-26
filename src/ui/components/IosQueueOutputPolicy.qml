// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

QtObject {
    id: root

    required property string platformOs
    required property var filesystemObject
    required property var renderQueueObject
    required property var outputFileObject
    required property var exportSettingsObject

    function deferAddTimeOutputCheck(urls: var): bool {
        if (root.platformOs !== "ios" || !urls || urls.length === 0) return false
        for (const url of urls) {
            if (!root.filesystemObject.is_ios_photo_import(url)) return false
        }
        return true
    }

    function runBeforeExport(callback: var): void {
        if (root.platformOs !== "ios"
            || !root.renderQueueObject.ios_photo_imports_need_output_folder()) {
            callback()
            return
        }

        root.outputFileObject.selectFolder("", function(folderUrl) {
            if (!folderUrl || !folderUrl.toString()) return
            root.exportSettingsObject.queueFixedOutputPath = folderUrl
            root.exportSettingsObject.queueOutputMode = 1
            Qt.callLater(callback)
        })
    }
}
