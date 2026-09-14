// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
pragma ComponentBehavior: Bound
import QtQuick

Column {
    id: root
    property var host: null
    property real unit: 1
    property bool dark: true
    property bool busy: false
    property string section: "stabilization"
    property color accentColor: MobileStyle.accent(dark)
    spacing: 24 * unit
    component Divider: Rectangle {
        width: parent.width; height: 0.5 * root.unit; color: MobileStyle.separator(root.dark)
    }
    MobileGroup {
        visible: root.section === "stabilization"
        width: parent.width; unit: root.unit; dark: root.dark
        enabled: !root.busy; opacity: enabled ? 1 : 0.4
        Item {
            width: parent.width; height: 44 * root.unit
            MobileText { anchors.left: parent.left; anchors.verticalCenter: parent.verticalCenter; unit: root.unit; dark: root.dark; text: qsTr("Smoothness") }
            MobileText { anchors.right: parent.right; anchors.verticalCenter: parent.verticalCenter; unit: root.unit; dark: root.dark; color: MobileStyle.secondary(root.dark); text: root.host ? Math.round(root.host.batchState.smoothness) + "%" : "50%" }
        }
        MobileSlider {
            width: parent.width; unit: root.unit; dark: root.dark
            from: 0; to: 100; stepSize: 1
            value: root.host ? root.host.batchState.smoothness : 50
            Accessible.name: qsTr("Smoothness")
            onMoved: if (root.host) root.host.batchState.smoothness = value
        }
        Divider {}
        MobileToggle {
            objectName: "mobileHorizonLock"
            width: parent.width; unit: root.unit; dark: root.dark
            text: qsTr("Lock horizon")
            checked: root.host ? root.host.batchState.horizonLock : false
            valueText: checked && root.host ? Math.round(root.host.batchState.horizonLockAmount) + "%" : ""
            onToggled: if (root.host) root.host.batchState.horizonLock = checked
        }
        MobileSlider {
            objectName: "mobileHorizonLockAmount"
            visible: !!(root.host && root.host.batchState.horizonLock)
            width: parent.width; unit: root.unit; dark: root.dark
            from: 0; to: 100; stepSize: 1
            value: root.host ? root.host.batchState.horizonLockAmount : 100
            Accessible.name: qsTr("Horizon lock amount")
            onMoved: if (root.host) root.host.batchState.horizonLockAmount = value
        }
        MobileToggle {
            visible: !!(root.host && root.host.mobileAutoRotateAvailable)
            width: parent.width; unit: root.unit; dark: root.dark
            text: qsTr("Auto rotate")
            checked: root.host ? root.host.batchState.autoRotate : false
            onToggled: if (root.host) root.host.batchState.autoRotate = checked
        }
        Divider {}
        MobileText { width: parent.width; height: 44 * root.unit; unit: root.unit; dark: root.dark; text: qsTr("Zoom") }
        MobileTabs {
            width: parent.width; unit: root.unit; dark: root.dark
            model: [qsTr("Dynamic zoom"), qsTr("Static zoom")]
            currentIndex: root.host ? root.host.batchState.zoomMode - 1 : 0
            onActivated: index => { if (root.host) root.host.batchState.zoomMode = index + 1; }
        }
        Item { width: 1; height: 8 * root.unit }
        Divider {}
        MobileToggle {
            width: parent.width; unit: root.unit; dark: root.dark
            text: qsTr("Lens correction")
            checked: root.host ? root.host.batchState.lensCorrection >= 0.5 : true
            onToggled: if (root.host) root.host.batchState.lensCorrection = checked ? 1 : 0
        }
    }
    MobileGroup {
        objectName: "mobileOutputSettings"
        visible: root.section === "app"
        width: parent.width; unit: root.unit; dark: root.dark
        enabled: !root.busy; opacity: enabled ? 1 : 0.4
        contentSpacing: 8 * root.unit
        MobileText { width: parent.width; height: 36 * root.unit; unit: root.unit; dark: root.dark; text: qsTranslate("SettingsSelector", "Output path") }
        MobileTabs {
            id: outputMode
            width: parent.width; unit: root.unit; dark: root.dark
            model: [qsTranslate("Export", "Same as source file"), qsTranslate("Export", "Fixed path")]
            currentIndex: root.host && root.host.exportSettings ? root.host.exportSettings.queueOutputMode : 0
            onActivated: index => { if (root.host && root.host.exportSettings) root.host.exportSettings.queueOutputMode = index; }
        }
        MobileActionRow {
            visible: outputMode.currentIndex === 1
            width: parent.width; unit: root.unit; dark: root.dark; navigation: true; iconName: "folder"
            text: qsTranslate("SettingsSelector", "Output path")
            description: root.host && root.host.exportSettings && root.host.exportSettings.queueFixedOutputPath.toString()
                ? (typeof filesystem !== "undefined" ? filesystem.display_url(root.host.exportSettings.queueFixedOutputPath) : root.host.exportSettings.queueFixedOutputPath.toString()) : qsTranslate("Export", "Browse")
            onClicked: if (root.host && root.host.exportSettings) root.host.exportSettings.browseQueueOutputFolder()
        }
    }
    MobileGroup {
        objectName: "mobileAppPreferences"
        visible: root.section === "app"
        width: parent.width; unit: root.unit; dark: root.dark
        contentSpacing: 8 * root.unit
        MobileText { width: parent.width; height: 36 * root.unit; unit: root.unit; dark: root.dark; text: qsTr("Language") }
        MobileChoice {
            width: parent.width; unit: root.unit; dark: root.dark
            model: root.host && root.host.advanced ? root.host.advanced.langList.langs.map(item => item[0]) : []
            currentIndex: root.host && root.host.advanced ? root.host.advanced.langList.currentIndex : 0
            Accessible.name: qsTr("Language")
            onActivated: index => { if (root.host && root.host.advanced) root.host.advanced.langList.currentIndex = index; }
        }
        MobileText { width: parent.width; height: 36 * root.unit; unit: root.unit; dark: root.dark; text: qsTr("Theme") }
        MobileTabs {
            width: parent.width; unit: root.unit; dark: root.dark
            model: [qsTranslate("Popup", "Light"), qsTranslate("Popup", "Dark")]
            currentIndex: root.dark ? 1 : 0
            Accessible.name: qsTr("Theme")
            onActivated: index => { if (root.host && root.host.advanced) root.host.advanced.setThemeIndex(index, true); }
        }
        Item { width: 1; height: 4 * root.unit }
        Divider {}
        MobileActionRow { width: parent.width; unit: root.unit; dark: root.dark; navigation: true; text: qsTr("Updates"); iconName: "update"; divider: true; onClicked: if (root.host) root.host.showAvailableAppVersions() }
        MobileActionRow { width: parent.width; unit: root.unit; dark: root.dark; navigation: true; text: qsTr("Feedback"); iconName: "message"; onClicked: if (root.host) root.host.feedbackDialog.open() }
    }
}
