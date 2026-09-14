// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
pragma ComponentBehavior: Bound
import QtQuick
import QtQuick.Controls.Basic as QQC

Item {
    id: root
    property real unit: 1
    property bool dark: true
    property var filesystemService: null
    property var queueService: null
    property string kind: "video"
    property string currentUrl: ""
    property var ancestors: []
    property var folders: []
    property var files: []
    property var locations: []
    property bool filesMode: false
    property bool choosingLocation: false
    property var selection: ({})
    property int requestId: 0
    property bool loading: false
    property string error: ""
    readonly property int selectedCount: Object.keys(selection).length
    readonly property bool compact: height < 400 * unit
    readonly property string currentPath: currentUrl && filesystemService ? filesystemService.display_url(currentUrl) : currentUrl
    readonly property string currentName: currentPath.replace(/\/$/, "").split("/").pop() || qsTr("This folder")
    readonly property var entries: filesMode ? folders.concat(files.filter(file => acceptsFile(file.name)).map(file => Object.assign({}, file, { isFile: true }))) : folders
    signal accepted(var urls, string kind)
    signal locationRequested(string kind)
    function goUp() {
        if (!ancestors.length) return false;
        const parents = ancestors.slice(); browse(parents.pop(), parents); return true;
    }
    function acceptsFile(name) {
        const lower = name.toLowerCase();
        if (kind === "gyro") return lower.endsWith("_mix.bin");
        const extensions = queueService && queueService.mobileVideoExtensions ? queueService.mobileVideoExtensions : [];
        return !extensions.length || extensions.includes(lower.split(".").pop());
    }
    function refreshLocations() {
        locations = filesystemService && filesystemService.get_mobile_locations ? JSON.parse(filesystemService.get_mobile_locations()) : [];
    }
    function start(mediaKind, selectFiles) {
        kind = mediaKind; filesMode = !!selectFiles; selection = {};
        if (filesMode) {
            if (!currentUrl) chooseLocation(); else browse(currentUrl, ancestors);
            return true;
        }
        refreshLocations();
        if (!locations.length) return false;
        const normalize = url => Qt.url(url).toString().replace(/\/+$/, "");
        const allowed = locations.some(url => normalize(url) === normalize(currentUrl)
            || ancestors.some(parent => normalize(parent) === normalize(url)));
        if (currentUrl && allowed) browse(currentUrl, ancestors);
        else {
            // Prefer the broadest granted location without adding a saved-locations page.
            const ordered = locations.slice().sort((a, b) => {
                const left = filesystemService.display_url(a), right = filesystemService.display_url(b);
                return left.split("/").length - right.split("/").length || left.localeCompare(right);
            });
            browse(ordered[0], []);
        }
        return true;
    }
    function chooseLocation() {
        refreshLocations(); choosingLocation = true;
    }
    function addLocation() {
        if (queueService) queueService.requestMobileFolderLocation(function(url) {
            if (root.filesMode) root.browse(url, []);
            else root.accepted([url], root.kind);
        });
    }
    function browse(url, parents) {
        choosingLocation = false;
        currentUrl = url; ancestors = parents; folders = []; files = []; error = ""; loading = true;
        if (filesystemService) filesystemService.list_mobile_folders(url, ++requestId);
        else loading = false;
    }
    function toggleFolder(folder, parents) {
        const copy = Object.assign({}, selection);
        if (copy[folder.url]) delete copy[folder.url];
        else {
            // Avoid importing both an ancestor and its descendant in one batch.
            for (const url of Object.keys(copy)) {
                if (parents.includes(url) || copy[url].parents.includes(folder.url)) delete copy[url];
            }
            copy[folder.url] = { name: folder.name, url: folder.url, parents: parents };
        }
        selection = copy;
    }
    Connections {
        target: root.filesystemService
        function onMobile_folders_listed(request_id, result) {
            if (request_id !== root.requestId) return;
            const parsed = JSON.parse(result);
            if (Qt.url(parsed.url).toString() !== Qt.url(root.currentUrl).toString()) return;
            root.folders = parsed.folders || []; root.files = parsed.files || []; root.error = parsed.error || ""; root.loading = false;
        }
    }
    Column {
        id: locationHeader
        visible: !root.choosingLocation
        width: parent.width; spacing: 4 * root.unit
        MobileButton { width: parent.width; unit: root.unit; dark: root.dark; iconName: "folder"; text: root.filesMode ? qsTranslate("MobileWorkspace", "Choose folders") : qsTr("Other locations"); onClicked: { if (root.filesMode) root.chooseLocation(); else root.locationRequested(root.kind); } }
        Row {
            width: parent.width; spacing: 4 * root.unit
            MobileButton {
                objectName: "mobileFolderUp"
                unit: root.unit; dark: root.dark; quiet: true; iconOnly: true; iconName: "up"; text: qsTr("Up one level")
                enabled: root.ancestors.length > 0; onClicked: root.goUp()
            }
            MobileText { width: parent.width - 48 * root.unit; height: 44 * root.unit; unit: root.unit; dark: root.dark; secondary: true; text: root.currentPath; elide: Text.ElideLeft }
        }
    }
    component FolderRow: Rectangle {
        id: entry
        required property var folder
        required property var parents
        property bool current: false
        property bool fileEntry: false
        readonly property bool inheritedSelection: parents.some(url => !!root.selection[url])
        width: root.width; height: Math.max((current && !root.compact ? 64 : 52) * root.unit, openFolder.implicitHeight)
        color: MobileStyle.surface(root.dark)
        QQC.CheckBox {
            objectName: "mobileFolderCheck"
            visible: entry.fileEntry
            width: 48 * root.unit; height: parent.height; padding: 0
            checked: !!root.selection[entry.folder.url] || entry.inheritedSelection
            enabled: !root.loading && !root.error && !entry.inheritedSelection
            Accessible.name: entry.folder.name
            onClicked: root.toggleFolder(entry.folder, entry.parents)
            indicator: Rectangle {
                x: 12 * root.unit; anchors.verticalCenter: parent.verticalCenter
                width: 24 * root.unit; height: width; radius: 4 * root.unit
                color: parent.checked ? MobileStyle.accent(root.dark) : "transparent"
                border.color: MobileStyle.secondary(root.dark); border.width: parent.checked ? 0 : root.unit
                MobileIcon { visible: parent.parent.checked; anchors.centerIn: parent; width: 18 * root.unit; height: width; name: "check"; color: "white" }
            }
        }
        MobileActionRow {
            id: openFolder
            objectName: "mobileFolderOpen"
            x: entry.fileEntry ? 48 * root.unit : 0; width: parent.width - x; height: parent.height
            unit: root.unit; dark: root.dark; iconName: entry.fileEntry ? "file" : "folder"; navigation: !entry.current && !entry.fileEntry
            text: entry.folder.name; description: entry.current && !root.compact ? qsTr("This folder") : ""
            enabled: !root.loading && !root.error
            onClicked: {
                if (entry.fileEntry) root.toggleFolder(entry.folder, entry.parents);
                else if (entry.current) { if (!entry.inheritedSelection) root.toggleFolder(entry.folder, entry.parents); }
                else root.browse(entry.folder.url, entry.parents);
            }
        }
        Rectangle { x: 48 * root.unit; width: parent.width - x; height: root.unit * 0.5; anchors.bottom: parent.bottom; color: MobileStyle.separator(root.dark) }
    }
    ListView {
        id: list
        visible: !root.choosingLocation
        objectName: "mobileFolderList"
        y: locationHeader.height + 4 * root.unit; width: parent.width
        height: Math.max(0, selectionFooter.y - y - 8 * root.unit)
        clip: true; boundsBehavior: Flickable.StopAtBounds
        model: root.entries
        QQC.ScrollIndicator.vertical: QQC.ScrollIndicator {}
        delegate: FolderRow {
            required property var modelData
            folder: modelData; parents: root.ancestors.concat([root.currentUrl])
            fileEntry: !!modelData.isFile
        }
        MobileText {
            visible: root.loading || !!root.error || (!root.entries.length && !!root.currentUrl)
            anchors.centerIn: parent; width: parent.width - 24 * root.unit
            unit: root.unit; dark: root.dark; secondary: true; wrapMode: Text.WordWrap; horizontalAlignment: Text.AlignHCenter
            text: root.loading ? qsTranslate("MobileWorkspace", "Reading…") : root.error ? qsTr("Unable to read this folder. Choose the location again.") : root.filesMode ? qsTr("No matching files") : qsTr("No subfolders")
        }
    }
    Column {
        id: selectionFooter
        visible: !root.choosingLocation
        anchors.bottom: parent.bottom; width: parent.width; spacing: 8 * root.unit
        ListView {
            visible: root.selectedCount > 0
            width: parent.width; height: 44 * root.unit; orientation: ListView.Horizontal; spacing: 6 * root.unit; clip: true
            model: Object.keys(root.selection)
            delegate: MobileButton {
                required property string modelData
                width: Math.min(implicitWidth, 200 * root.unit); height: 44 * root.unit
                unit: root.unit; dark: root.dark; iconName: "close"
                text: root.selection[modelData] ? root.selection[modelData].name : ""
                Accessible.name: qsTranslate("MobileWorkspace", "Remove") + " " + text
                onClicked: root.toggleFolder(root.selection[modelData], [])
            }
        }
        MobileButton {
            objectName: "mobileConfirmFolders"
            width: parent.width; unit: root.unit; dark: root.dark; emphasized: true
            text: root.filesMode ? qsTr("Add %1 files").arg(root.selectedCount) : qsTr("Add this folder")
            enabled: (root.selectedCount > 0 || (!root.filesMode && !!root.currentUrl)) && !root.loading && !root.error
            onClicked: root.accepted(root.selectedCount ? Object.keys(root.selection) : [root.currentUrl], root.kind)
        }
    }
    Column {
        id: locationsHeader
        visible: root.filesMode && root.choosingLocation
        width: parent.width; spacing: 12 * root.unit
        MobileText { visible: root.filesMode; width: parent.width; unit: root.unit; dark: root.dark; text: qsTr("Choose location"); heading: true }
        MobileText { visible: root.filesMode; width: parent.width; unit: root.unit; dark: root.dark; secondary: true; wrapMode: Text.WordWrap; text: qsTr("Choose a folder once to browse its files here.") }
        MobileButton { objectName: "mobileAddLocation"; width: parent.width; unit: root.unit; dark: root.dark; emphasized: true; iconName: "folder"; text: qsTranslate("MobileWorkspace", "Choose folders"); onClicked: root.addLocation() }
        MobileText { visible: root.locations.length > 0; width: parent.width; unit: root.unit; dark: root.dark; secondary: true; text: qsTr("Saved folders") }
    }
    ListView {
        objectName: "mobileLocations"
        visible: root.filesMode && root.choosingLocation
        y: locationsHeader.height + 12 * root.unit; width: parent.width; height: Math.max(0, parent.height - y)
        model: root.locations; clip: true; boundsBehavior: Flickable.StopAtBounds
        QQC.ScrollIndicator.vertical: QQC.ScrollIndicator {}
        delegate: MobileActionRow {
            required property string modelData
            width: root.width; unit: root.unit; dark: root.dark; iconName: "folder"; navigation: root.filesMode; divider: true
            text: root.filesystemService ? root.filesystemService.display_url(modelData) : modelData
            onClicked: { if (root.filesMode) root.browse(modelData, []); else root.accepted([modelData], root.kind); }
        }
    }
}
