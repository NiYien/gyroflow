// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2023 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Dialogs as QQD

import "."

TextField {
    id: root;
    text: "";
    width: parent.width;
    rightPadding: linkBtn.width;

    property bool folderOnly: false;
    property bool syncMobileOutput: false;
    property bool androidFolderPending: false;

    property url fullFileUrl;
    property url folderUrl;
    property string filename;

    property alias outputFileDialog: outputFileDialog;
    property alias outputFolderDialog: outputFolderDialog;

    property bool preventChange: false;

    onTextChanged: {
        // When typing manually
        if (!preventChange) {
            if (isSandboxed) {
                setFilename(text.replace(/^.+\//, ""));
            } else {
                setUrl(filesystem.path_to_url(text));
            }
        }
    }

    function prompt(): void {
        linkBtn.clicked();
    }

    function updateText(): void {
        preventChange = true;
        if (!filename && root.folderOnly && root.folderUrl.toString()) {
            text = filesystem.display_folder_filename(root.folderUrl, filename);
            if (!text && root.folderUrl.toString()) text = qsTr('[Selected folder]');
        } else {
            text = fullFileUrl.toString()? filesystem.display_url(fullFileUrl) : filesystem.display_folder_filename(folderUrl, filename);
        }
        preventChange = false;
    }

    function setUrl(url: url): void {
        fullFileUrl = url;
        filename = filesystem.get_filename(url);
        folderUrl = filesystem.get_folder(url);
        updateText();
    }
    function setFilename(fname: string): void {
        if (fname != filename) {
            filename = fname;
            fullFileUrl = "";
            updateText();
        }
    }
    function setFolder(folder: url): void {
        folderUrl = folder;
        if (folder.toString())
            fullFileUrl = "";
        updateText();
    }

    function getFolderForDialog(folder: url): url {
        if (folder.toString()) {
            return folder;
        } else if (window.videoArea.loadedFileUrl.toString()) {
            let parts = window.videoArea.loadedFileUrl.toString().split('/');
            parts.pop();
            return parts.join('/') + '/';
        }
        return "";
    }

    function selectFolder(folder: url, cb: var, rejectedCb: var): void {
        folderContinuation.begin(cb, rejectedCb);
        outputFolderDialog.currentFolder = root.getFolderForDialog(folder);
        if (Qt.platform.os === "android") {
            androidFolderPending = true;
            window.openPicker(1, false, function(urls) {
                if (urls && urls.length) root.acceptFolder(urls[0]);
                else { root.androidFolderPending = false; folderContinuation.reject(); }
            }, outputFolderDialog, folder.toString());
        } else outputFolderDialog.open();
    }

    function acceptFolder(folder: url): void {
        androidFolderPending = false;
        if (root.syncMobileOutput) {
            if (!window.exportSettings.applySelectedOutputFolder(folder)) {
                folderContinuation.reject();
                return;
            }
        } else {
            filesystem.folder_access_granted(folder);
            filesystem.save_allowed_folders();
        }
        root.setFolder(folder);
        if (window.videoArea.loadedFileUrl.toString() && !window.vidInfo.hasAccessToInputDirectory && Qt.resolvedUrl(filesystem.get_folder(window.videoArea.loadedFileUrl)) == Qt.resolvedUrl(folder)) {
            window.vidInfo.hasAccessToInputDirectory = true;
            settings.setValue("folder-video", filesystem.get_folder(window.videoArea.loadedFileUrl).toString());
        }
        folderContinuation.accept(root.folderUrl);
    }

    Connections {
        target: filesystem
        function onPicker_cancelled(): void {
            if (root.androidFolderPending) {
                root.androidFolderPending = false;
                folderContinuation.reject();
            }
        }
    }

    FolderSelectionContinuation {
        id: folderContinuation;
    }

    LinkButton {
        id: linkBtn;
        anchors.right: parent.right;
        height: parent.height - 1 * dpiScale;
        text: "...";
        font.underline: false;
        font.pixelSize: 15 * dpiScale;
        onClicked: {
            if (isSandboxed || root.folderOnly) {
                root.selectFolder(root.folderUrl, null, null);
                return;
            }
            outputFileDialog.defaultSuffix = root.filename.substring(root.filename.length - 3);
            outputFileDialog.currentFolder = root.getFolderForDialog(root.folderUrl);
            outputFileDialog.selectedFile = filesystem.get_file_url(root.folderUrl, root.filename, false);
            outputFileDialog.open();
        }
    }
    FileDialog {
        id: outputFileDialog;
        fileMode: FileDialog.SaveFile;
        title: qsTr("Select file destination");
        nameFilters: Qt.platform.os == "android"? undefined : [qsTr("Video files") + " (*.mp4 *.mov *.png *.exr)"];
        type: "output-video";
        onAccepted: {
            root.setUrl(outputFileDialog.selectedFile);
            window.exportSettings.updateCodecParams();
        }
    }
    QQD.FolderDialog {
        id: outputFolderDialog;
        title: qsTr("Select file destination");
        onAccepted: {
            if (Qt.platform.os !== "android") root.acceptFolder(selectedFolder);
        }
        onRejected: {
            root.androidFolderPending = false;
            if (Qt.platform.os === "android") window.pendingPickerCallback = null;
            folderContinuation.reject();
        }
    }
}
