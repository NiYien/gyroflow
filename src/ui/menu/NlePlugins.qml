// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 Adrian <adrian.eddy at gmail>

import QtQuick
import "../components/"

MenuItem {
    id: root;
    text: qsTr("Video editor plugins");
    iconName: "plugin";
    opened: false;
    objectName: "nlePlugins";

    property var openfx_status: ({});
    property var adobe_status: ({});

    Component.onCompleted: {
        refreshStatuses();
    }
    function refreshStatuses() {
        controller.nle_plugins("status", "openfx");
        controller.nle_plugins("status", "adobe");
    }
    function parseStatus(result: string): var {
        try {
            return JSON.parse(result);
        } catch (e) {
            return ({});
        }
    }
    function statusFor(type: string): var {
        return type === "openfx" ? openfx_status : adobe_status;
    }
    function installedVersion(type: string): string {
        const status = statusFor(type);
        return status.installed_version || "";
    }
    function isLatest(type: string): bool {
        return Boolean(statusFor(type).is_latest);
    }
    function needsUpdate(type: string): bool {
        const status = statusFor(type);
        if (!status || Object.keys(status).length === 0) {
            return false;
        }
        return Boolean(status.update_available);
    }
    function statusColor(type: string): string {
        const version = installedVersion(type);
        if (!version) {
            return "";
        }
        return isLatest(type)? "#10ee14" : "red";
    }
    function latestSuffix(type: string): string {
        const status = statusFor(type);
        if (status.latest_source_mode === "artifact" || status.latest_source_mode === "nightly") {
            return qsTr("(nightly)");
        }
        return "";
    }
    function selectFolder(type: string, folder: string) {
        const dialog = Qt.createQmlObject("import QtQuick.Dialogs; FolderDialog {}", root, "selectFolderNle");
        dialog.title = qsTr("Select %1").arg(folder);
        const initialFolder = "file://" + folder;
        dialog.currentFolder = initialFolder;
        dialog.accepted.connect(function() {
            if (Qt.resolvedUrl(dialog.selectedFolder) != Qt.resolvedUrl(initialFolder)) {
                root.loader = false;
                messageBox(Modal.Error, qsTr("You selected the wrong folder.\nMake sure to select %1.").arg("<b>" + folder + "</b>"), [ { text: qsTr("Ok"), accent: true } ]);
            } else {
                filesystem.folder_access_granted(dialog.selectedFolder);
                controller.nle_plugins("install", type);
            }
        });
        dialog.rejected.connect(function() {
            root.loader = false;
        });
        dialog.open();
    }

    Connections {
        target: controller;
        function onNle_plugins_result(command: string, result: string) {
            if (command == "status") {
                const status = parseStatus(result);
                if (status.typ === "openfx") {
                    openfx_status = status;
                } else if (status.typ === "adobe") {
                    adobe_status = status;
                }
            }
            if (command == "install") {
                if (result.startsWith("An error occured")) {
                    if (result.includes("Failed to copy files from ") && result.includes("PermissionDenied")) {
                        const parts = result.split("Failed to copy files from \\\"").pop().split("\\\" to \\\"");
                        const from = parts[0];
                        const to = parts[1].split("\\\": Error").shift();

                        const mb = messageBox(Modal.Error, qsTr("Unable to copy the plugin due to sandbox limitations.\nOpen <b>Terminal</b> and enter the following command:"), [ { text: qsTr("Ok"), accent: true, clicked: () => {
                            refreshStatuses();
                            root.loader = false;
                        } } ]);
                        mb.isWide = true;
                        const tf = Qt.createComponent("../components/TextField.qml").createObject(mb.mainColumn, { readOnly: true });
                        tf.text = 'sudo mkdir -p "' + to + '" ; sudo mv -f "' + from + '" "' + to + '"';
                        tf.width = mb.mainColumn.width;
                    } else {
                        messageBox(Modal.Error, result, [ { text: qsTr("Ok"), accent: true } ]);
                    }
                }
                refreshStatuses();
                root.loader = false;
            }
        }
    }

    Row {
        BasicText {
            text: 'Adobe: <b><font color="%1">%2</font></b> %3'.arg(statusColor("adobe")).arg(installedVersion("adobe")? installedVersion("adobe") : "---").arg(latestSuffix("adobe")).trim();
            textFormat: Text.StyledText;
            anchors.verticalCenter: parent.verticalCenter;
        }
        LinkButton {
            enabled: !root.loader;
            visible: needsUpdate("adobe");
            text: installedVersion("adobe")? qsTr("Update") : qsTr("Install");
            leftPadding: 7 * dpiScale;
            rightPadding: 7 * dpiScale;
            onClicked: {
                root.loader = true;
                if (Qt.platform.os == "osx" && isSandboxed) {
                    const folder = "/Library/Application Support/Adobe/Common/Plug-ins/7.0/MediaCore";
                    messageBox(Modal.Info, qsTr("At the next prompt, click <b>\"Open\"</b> to grant access to the %1 folder in order for Gyroflow to install the plugin.").arg("<b>\"" + folder + "\"</b>"), [ { text: qsTr("Ok"), accent: true, clicked: () => {
                        root.selectFolder("adobe", folder);
                    } } ]);
                } else {
                    controller.nle_plugins("install", "adobe");
                }
            }
            anchors.verticalCenter: parent.verticalCenter;
        }
    }

    Row {
        BasicText {
            text: 'DaVinci: <b><font color="%1">%2</font></b> %3'.arg(statusColor("openfx")).arg(installedVersion("openfx")? installedVersion("openfx") : "---").arg(latestSuffix("openfx")).trim();
            textFormat: Text.StyledText;
            anchors.verticalCenter: parent.verticalCenter;
        }
        LinkButton {
            id: openfxInstall;
            enabled: !root.loader;
            visible: needsUpdate("openfx");
            text: installedVersion("openfx")? qsTr("Update") : qsTr("Install");
            leftPadding: 7 * dpiScale;
            rightPadding: 7 * dpiScale;
            onClicked: {
                root.loader = true;
                if (Qt.platform.os == "osx" && isSandboxed) {
                    const folder = "/Library/OFX/Plugins";
                    if (!filesystem.exists("file://" + folder)) {
                        const mb = messageBox(Modal.Info, qsTr("%1 folder doesn't exist.\nDue to sandbox limitations, you have to create it yourself.\nOpen <b>Terminal</b> and enter the following command:").arg("<b>\"" + folder + "\"</b>"), [ { text: qsTr("Ok"), accent: true, clicked: () => {
                            openfxInstall.clicked();
                        } }, { text: qsTr("Cancel"), clicked: function() { root.loader = false; } } ]);
                        mb.isWide = true;
                        const tf = Qt.createComponent("../components/TextField.qml").createObject(mb.mainColumn, { readOnly: true });
                        tf.text = "sudo install -m 0755 -o $USER -d /Library/OFX/Plugins";
                        tf.width = mb.mainColumn.width;
                    } else {
                        messageBox(Modal.Info, qsTr("At the next prompt, click <b>\"Open\"</b> to grant access to the %1 folder in order for Gyroflow to install the plugin.").arg("<b>\"" + folder + "\"</b>"), [ { text: qsTr("Ok"), accent: true, clicked: () => {
                            root.selectFolder("openfx", folder);
                        } } ]);
                    }
                } else {
                    controller.nle_plugins("install", "openfx");
                }
            }
            anchors.verticalCenter: parent.verticalCenter;
        }
    }

}
