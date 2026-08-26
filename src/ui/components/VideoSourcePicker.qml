// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick

Item {
    id: root
    width: 0
    height: 0
    visible: false

    required property var hostObject
    required property var filesystemObject
    required property int questionType
    required property int errorType

    function open(platformOs: string, callback: var, fallbackDialog: var): void {
        if (platformOs !== "ios") {
            hostObject.openPicker(0, true, callback, fallbackDialog)
            return
        }

        hostObject.messageBox(questionType, qsTr("Choose video source"), [
            {
                text: qsTr("Photos"),
                accent: true,
                clicked: function() {
                    hostObject.pendingPickerCallback = callback
                    if (!filesystemObject.open_ios_video_picker()) {
                        hostObject.pendingPickerCallback = null
                        hostObject.messageBox(
                            errorType,
                            qsTr("Unable to open the photo library."),
                            [ { text: qsTranslate("App", "Ok") } ]
                        )
                    }
                }
            },
            {
                text: qsTr("Files and external storage"),
                clicked: function() {
                    if (fallbackDialog.open2) fallbackDialog.open2()
                    else fallbackDialog.open()
                }
            },
            { text: qsTranslate("App", "Cancel") }
        ])
    }

    Connections {
        target: root.filesystemObject

        function onPicker_cancelled(): void {
            root.hostObject.pendingPickerCallback = null
        }

        function onPicker_error(message: string): void {
            root.hostObject.pendingPickerCallback = null
            root.hostObject.messageBox(
                root.errorType,
                qsTr("Some videos could not be imported: %1").arg(message),
                [ { text: qsTranslate("App", "Ok") } ]
            )
        }
    }
}
