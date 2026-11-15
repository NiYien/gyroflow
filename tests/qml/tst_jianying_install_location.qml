// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "JianyingInstallLocation"
    when: windowShown
    visible: true
    width: 500
    height: 700

    property real dpiScale: 1
    property string styleFont: "Segoe UI"
    property color styleAccentColor: "orange"
    property color styleTextColor: "white"
    property color styleButtonColor: "#444444"
    property color stylePopupBorder: "#666666"
    property color styleBackground2: "#222222"
    property bool isMobile: false
    property var window: ({ useMobileWorkspace: false, reportProgress: function() {} })
    property var controller: controllerStub
    property var filesystem: filesystemStub
    property var settings: settingsStub
    property var panels: []
    property var messages: []

    Item { width: 500; height: 700
        Item { anchors.fill: parent
            Item { anchors.fill: parent
                Item { id: panelParent; anchors.fill: parent }
            }
        }
    }

    QtObject {
        id: settingsStub
        function value(key, fallback) { return fallback }
        function setValue(key, value) {}
    }
    QtObject {
        id: controllerStub
        property var calls: []
        property string selectionResult: ""
        signal nle_plugins_result(string command, string result)
        function nle_plugins(command, type) {
            calls.push({ command: command, argument: type })
            return command === "set_jianying_location" ? selectionResult : ""
        }
    }
    QtObject {
        id: filesystemStub
        function url_to_path(url) { return "resolved:" + url.toString() }
    }

    function messageBox(kind, text, buttons) { messages.push(text) }

    function init() {
        const component = Qt.createComponent("../../src/ui/menu/NlePlugins.qml")
        compare(component.status, Component.Ready, component.errorString())
        // Exercise the same component in the full menu and the locked Simple-mode card.
        panels = [component.createObject(panelParent, { opened: true }),
                  component.createObject(panelParent, { locked: true })]
        verify(panels[0] !== null && panels[1] !== null)
        wait(0)
        controllerStub.calls = []
        controllerStub.selectionResult = ""
        messages = []
    }
    function cleanup() {
        panels.forEach(function(panel) { panel.destroy() })
        panels = []
    }
    function sendStatus(state, updateAvailable) {
        controllerStub.nle_plugins_result("status", JSON.stringify({
            typ: "jianying", state: state, installed_version: "",
            update_available: updateAvailable, is_latest: false
        }))
    }
    function test_missingHostOffersManualSelectionAndDetectionEnablesInstall() {
        sendStatus("host_missing", false)
        panels.forEach(function(panel) {
            tryVerify(function() { return findChild(panel, "jianyingPluginSelectLocationButton").visible })
            verify(findChild(panel, "jianyingPluginSelectLocationButton").enabled)
            verify(!findChild(panel, "jianyingPluginInstallButton").enabled)
        })
        sendStatus("not_installed", true)
        panels.forEach(function(panel) {
            verify(!findChild(panel, "jianyingPluginSelectLocationButton").visible)
            verify(findChild(panel, "jianyingPluginInstallButton").enabled)
        })
    }
    function test_unavailablePackageDoesNotOfferLocationSelection() {
        sendStatus("unavailable", false)
        panels.forEach(function(panel) {
            verify(!findChild(panel, "jianyingPluginSelectLocationButton").visible)
            verify(!findChild(panel, "jianyingPluginInstallButton").enabled)
        })
    }
    function test_selectingLocationValidatesAndRefreshesStatus() {
        panels[0].selectJianyingFolder()
        const dialog = findChild(panels[0], "jianyingInstallationFolderDialog")
        verify(dialog !== null)
        dialog.selectedFolder = Qt.resolvedUrl("../../")
        const expectedPath = filesystemStub.url_to_path(dialog.selectedFolder)
        dialog.accept()
        compare(controllerStub.calls.length, 2)
        compare(controllerStub.calls[0].command, "set_jianying_location")
        compare(controllerStub.calls[0].argument, expectedPath)
        compare(controllerStub.calls[1].command, "status")
        compare(controllerStub.calls[1].argument, "jianying")
        compare(messages.length, 0)
    }
    function test_invalidLocationShowsErrorAndKeepsManualSelectionAvailable() {
        sendStatus("host_missing", false)
        controllerStub.selectionResult = "An error occured: no JianyingPro.exe"
        panels[0].selectJianyingFolder()
        const dialog = findChild(panels[0], "jianyingInstallationFolderDialog")
        dialog.selectedFolder = Qt.resolvedUrl("../../")
        dialog.accept()
        compare(controllerStub.calls.length, 1)
        compare(messages.length, 1)
        compare(messages[0], "CapCut was not found in the selected folder.")
        tryVerify(function() { return findChild(panels[0], "jianyingPluginSelectLocationButton").visible })
    }
    function test_cancelingSelectionDoesNotChangeLocation() {
        panels[0].selectJianyingFolder()
        const dialog = findChild(panels[0], "jianyingInstallationFolderDialog")
        dialog.reject()
        compare(controllerStub.calls.length, 0)
        compare(messages.length, 0)
    }
}
