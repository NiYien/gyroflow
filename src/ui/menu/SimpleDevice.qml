// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls as QQC

import "../components/"
import "device_timezones.js" as DeviceTimezones

MenuItem {
    id: root
    text: qsTranslate("Device", "Device")
    iconName: "plugin"
    objectName: "simple-device"
    opened: true
    visible: controller.device_connected || controller.ota_state !== "none"

    property string deviceTimeText: ""
    property string lastDeviceTimeSource: ""
    property var deviceTimeBase: null
    property double deviceTimeBaseMs: 0
    property var selectedTimezone: ({ key: "Shanghai", offsetMinutes: 480 })
    property var selectedRegion: null
    property var timezoneCatalog: DeviceTimezones.timezoneRegions

    readonly property bool lightTheme: style === "light"
    readonly property color cardColor: root.lightTheme ? "#ffffff" : "#f0282828"
    readonly property color sectionColor: root.lightTheme ? "#f6f8fb" : "#eb191919"
    readonly property color borderColor: root.lightTheme ? "#d8dde6" : Qt.rgba(1, 1, 1, 0.14)
    readonly property color mutedTextColor: root.lightTheme ? "#57677a" : Qt.rgba(1, 1, 1, 0.72)
    readonly property color popupColor: root.lightTheme ? "#ffffff" : styleBackground2
    readonly property color mapOceanColor: root.lightTheme ? "#eff4f8" : "#0f1721"
    readonly property color cityDotColor: root.lightTheme ? "#245d84" : "#8ed9ff"
    readonly property color cityDotSelectedColor: styleAccentColor

    function regionChoices(region: var): var {
        return region && region.choices ? region.choices : []
    }
    function systemOffsetMinutes(): int { return -new Date().getTimezoneOffset() }
    function cityDisplayName(key: string): string {
        return key && key.length > 0 ? qsTranslate("Device", key) : ""
    }
    function formatUtcOffset(offsetMinutes: int): string {
        const sign = offsetMinutes >= 0 ? "+" : "-"
        const total = Math.abs(offsetMinutes)
        const hours = Math.floor(total / 60)
        const minutes = total % 60
        return "UTC" + sign
            + (hours < 10 ? "0" : "") + hours
            + ":"
            + (minutes < 10 ? "0" : "") + minutes
    }
    function currentTimezoneLabel(): string {
        return root.cityDisplayName(root.selectedTimezone.key) + "  " + root.formatUtcOffset(root.selectedTimezone.offsetMinutes)
    }
    function translatedDeviceText(text: string): string {
        return text && text.length > 0 ? qsTranslate("Device", text) : ""
    }
    function applyTimezoneChoice(choice: var, region: var): void {
        root.selectedRegion = region
        root.selectedTimezone = { key: choice.key, offsetMinutes: choice.offsetMinutes }
        settings.setValue("niyienTimezoneKey", choice.key)
        settings.setValue("niyienTimezoneOffsetMinutes", choice.offsetMinutes)
        settings.setValue("niyienTimezoneLabel", choice.key)
        if (region) {
            settings.setValue("niyienTimezoneRegionX", region.x)
            settings.setValue("niyienTimezoneRegionY", region.y)
        }
    }
    function selectRegion(region: var): void {
        const choices = root.regionChoices(region)
        if (choices.length > 0)
            root.applyTimezoneChoice(choices[0], region)
    }
    function choiceMatchesSaved(choice: var, savedKey: string, savedLabel: string, savedOffset: int): bool {
        return choice.offsetMinutes === savedOffset
            && (
                (savedKey && choice.key === savedKey)
                || (!savedKey && savedLabel && (choice.key === savedLabel || root.cityDisplayName(choice.key) === savedLabel))
                || (!savedKey && !savedLabel)
            )
    }
    function matchingRegion(savedKey: string, savedLabel: string, savedOffset: int): var {
        for (let i = 0; i < root.timezoneCatalog.length; ++i) {
            const region = root.timezoneCatalog[i]
            const choices = root.regionChoices(region)
            for (let j = 0; j < choices.length; ++j) {
                if (root.choiceMatchesSaved(choices[j], savedKey, savedLabel, savedOffset))
                    return region
            }
        }
        return null
    }
    function loadSelectedTimezone(): void {
        const savedKey = settings.value("niyienTimezoneKey", "")
        const savedOffsetRaw = settings.value("niyienTimezoneOffsetMinutes", "")
        const savedLabel = settings.value("niyienTimezoneLabel", "")
        const offsetMinutes = savedOffsetRaw === "" ? root.systemOffsetMinutes() : +savedOffsetRaw
        const region = root.matchingRegion(savedKey, savedLabel, offsetMinutes)
        if (region) {
            const choices = root.regionChoices(region)
            for (let i = 0; i < choices.length; ++i) {
                if (root.choiceMatchesSaved(choices[i], savedKey, savedLabel, offsetMinutes)) {
                    root.applyTimezoneChoice(choices[i], region)
                    return
                }
            }
            root.applyTimezoneChoice(choices[0], region)
            return
        }
        root.selectedRegion = null
        root.selectedTimezone = {
            key: savedKey || savedLabel || "System",
            offsetMinutes: offsetMinutes
        }
    }
    function selectSystemTimezone(): void {
        const offsetMinutes = root.systemOffsetMinutes()
        const region = root.matchingRegion("", "", offsetMinutes)
        if (region) {
            root.selectRegion(region)
            return
        }
        root.selectedRegion = null
        root.selectedTimezone = {
            key: "System",
            offsetMinutes: offsetMinutes
        }
        settings.setValue("niyienTimezoneKey", "System")
        settings.setValue("niyienTimezoneOffsetMinutes", offsetMinutes)
        settings.setValue("niyienTimezoneLabel", "System")
    }
    function refreshDeviceTime(): void {
        if (!controller.device_time || controller.device_time.length === 0) {
            root.deviceTimeText = ""
            root.deviceTimeBase = null
            root.lastDeviceTimeSource = ""
            return
        }
        if (controller.device_time !== root.lastDeviceTimeSource) {
            root.lastDeviceTimeSource = controller.device_time
            const parsed = new Date(controller.device_time.replace(" ", "T"))
            if (isNaN(parsed.getTime())) {
                root.deviceTimeBase = null
                root.deviceTimeText = controller.device_time
                return
            }
            root.deviceTimeBase = parsed
            root.deviceTimeBaseMs = Date.now()
        }
        const current = root.deviceTimeBase
            ? new Date(root.deviceTimeBase.getTime() + (Date.now() - root.deviceTimeBaseMs))
            : null
        root.deviceTimeText = current ? Qt.formatDateTime(current, "yyyy-MM-dd hh:mm:ss") : controller.device_time
    }
    function firmwareStatusTitle(): string {
        if (controller.ota_state === "checking") return qsTranslate("Device", "Checking for updates")
        if (controller.ota_state === "update_available") return qsTranslate("Device", "Update available")
        if (controller.ota_state === "updating") return qsTranslate("Device", "Updating firmware")
        if (controller.ota_state === "success") return qsTranslate("Device", "Firmware updated")
        if (controller.ota_state === "failed") return qsTranslate("Device", "Update failed")
        if (controller.device_connected) return qsTranslate("Device", "Firmware is up to date")
        return qsTranslate("Device", "Waiting for device")
    }
    function firmwareStatusColor(): color {
        if (controller.ota_state === "failed") return "#d9534f"
        if (controller.ota_state === "update_available") return "#d68a1e"
        if (controller.ota_state === "success") return "#2f9e67"
        if (controller.ota_state === "updating" || controller.ota_state === "checking") return styleAccentColor
        return root.lightTheme ? "#2b7a57" : "#5fca84"
    }
    function firmwareDetailText(): string {
        const currentVersion = controller.device_soft_version.length > 0 ? controller.device_soft_version : "--"
        if (controller.ota_state === "up_to_date")
            return qsTranslate("Device", "Current firmware: %1. Your device is already on the latest firmware.").arg(currentVersion)
        if (controller.ota_state === "checking")
            return qsTranslate("Device", "Current firmware: %1. Checking automatically after the device is connected...").arg(currentVersion)
        if (controller.ota_state === "update_available")
            return qsTranslate("Device", "Current firmware: %1. Latest firmware: %2.").arg(currentVersion).arg(controller.firmware_latest_version)
        if (controller.ota_state === "success")
            return qsTranslate("Device", "Current firmware: %1. Firmware update completed successfully.").arg(currentVersion)
        if (controller.ota_state === "updating")
            return qsTranslate("Device", "Current firmware: %1. Do not disconnect the device during update.").arg(currentVersion)
        if (controller.ota_state === "failed" && controller.ota_error.length > 0)
            return qsTranslate("Device", "Current firmware: %1. %2").arg(currentVersion).arg(root.translatedDeviceText(controller.ota_error))
        return qsTranslate("Device", "Current firmware: %1. Waiting for automatic firmware check.").arg(currentVersion)
    }

    Connections {
        target: controller
        function onDevice_state_changed(): void { root.refreshDeviceTime() }
        function onDevice_time_sync_finished(success: bool, message: string): void {
            window.showNotification(success ? Modal.Success : Modal.Error, root.translatedDeviceText(message))
        }
    }

    Timer {
        interval: 500
        repeat: true
        running: root.visible && controller.device_time.length > 0
        onTriggered: root.refreshDeviceTime()
    }

    Component.onCompleted: {
        root.loadSelectedTimezone()
        root.refreshDeviceTime()
        QT_TRANSLATE_NOOP("Device", "Device")
        QT_TRANSLATE_NOOP("Device", "System")
        QT_TRANSLATE_NOOP("Device", "Model")
        QT_TRANSLATE_NOOP("Device", "Device time")
        QT_TRANSLATE_NOOP("Device", "Timezone")
        QT_TRANSLATE_NOOP("Device", "Sync Time")
        QT_TRANSLATE_NOOP("Device", "Syncing...")
        QT_TRANSLATE_NOOP("Device", "Set timezone")
        QT_TRANSLATE_NOOP("Device", "Software")
        QT_TRANSLATE_NOOP("Device", "Hardware")
        QT_TRANSLATE_NOOP("Device", "Update Firmware")
        QT_TRANSLATE_NOOP("Device", "Updating...")
        QT_TRANSLATE_NOOP("Device", "Set device timezone")
        QT_TRANSLATE_NOOP("Device", "Current selection")
        QT_TRANSLATE_NOOP("Device", "Nearest city")
        QT_TRANSLATE_NOOP("Device", "Use system timezone")
        QT_TRANSLATE_NOOP("Device", "Close")
        QT_TRANSLATE_NOOP("Device", "Honolulu")
        QT_TRANSLATE_NOOP("Device", "Pago Pago")
        QT_TRANSLATE_NOOP("Device", "Taiohae")
        QT_TRANSLATE_NOOP("Device", "Anchorage")
        QT_TRANSLATE_NOOP("Device", "Los Angeles")
        QT_TRANSLATE_NOOP("Device", "San Francisco")
        QT_TRANSLATE_NOOP("Device", "Vancouver")
        QT_TRANSLATE_NOOP("Device", "Denver")
        QT_TRANSLATE_NOOP("Device", "Phoenix")
        QT_TRANSLATE_NOOP("Device", "Chicago")
        QT_TRANSLATE_NOOP("Device", "Mexico City")
        QT_TRANSLATE_NOOP("Device", "New York")
        QT_TRANSLATE_NOOP("Device", "Toronto")
        QT_TRANSLATE_NOOP("Device", "Caracas")
        QT_TRANSLATE_NOOP("Device", "Halifax")
        QT_TRANSLATE_NOOP("Device", "St. Johns")
        QT_TRANSLATE_NOOP("Device", "Sao Paulo")
        QT_TRANSLATE_NOOP("Device", "Buenos Aires")
        QT_TRANSLATE_NOOP("Device", "Fernando de Noronha")
        QT_TRANSLATE_NOOP("Device", "Praia")
        QT_TRANSLATE_NOOP("Device", "Ponta Delgada")
        QT_TRANSLATE_NOOP("Device", "London")
        QT_TRANSLATE_NOOP("Device", "Lisbon")
        QT_TRANSLATE_NOOP("Device", "Berlin")
        QT_TRANSLATE_NOOP("Device", "Paris")
        QT_TRANSLATE_NOOP("Device", "Cairo")
        QT_TRANSLATE_NOOP("Device", "Johannesburg")
        QT_TRANSLATE_NOOP("Device", "Moscow")
        QT_TRANSLATE_NOOP("Device", "Istanbul")
        QT_TRANSLATE_NOOP("Device", "Tehran")
        QT_TRANSLATE_NOOP("Device", "Dubai")
        QT_TRANSLATE_NOOP("Device", "Abu Dhabi")
        QT_TRANSLATE_NOOP("Device", "Kabul")
        QT_TRANSLATE_NOOP("Device", "Karachi")
        QT_TRANSLATE_NOOP("Device", "Tashkent")
        QT_TRANSLATE_NOOP("Device", "Delhi")
        QT_TRANSLATE_NOOP("Device", "Mumbai")
        QT_TRANSLATE_NOOP("Device", "Kathmandu")
        QT_TRANSLATE_NOOP("Device", "Dhaka")
        QT_TRANSLATE_NOOP("Device", "Thimphu")
        QT_TRANSLATE_NOOP("Device", "Yangon")
        QT_TRANSLATE_NOOP("Device", "Bangkok")
        QT_TRANSLATE_NOOP("Device", "Jakarta")
        QT_TRANSLATE_NOOP("Device", "Shanghai")
        QT_TRANSLATE_NOOP("Device", "Beijing")
        QT_TRANSLATE_NOOP("Device", "Tianjin")
        QT_TRANSLATE_NOOP("Device", "Eucla")
        QT_TRANSLATE_NOOP("Device", "Tokyo")
        QT_TRANSLATE_NOOP("Device", "Seoul")
        QT_TRANSLATE_NOOP("Device", "Adelaide")
        QT_TRANSLATE_NOOP("Device", "Darwin")
        QT_TRANSLATE_NOOP("Device", "Sydney")
        QT_TRANSLATE_NOOP("Device", "Melbourne")
        QT_TRANSLATE_NOOP("Device", "Lord Howe")
        QT_TRANSLATE_NOOP("Device", "Noumea")
        QT_TRANSLATE_NOOP("Device", "Honiara")
        QT_TRANSLATE_NOOP("Device", "Auckland")
        QT_TRANSLATE_NOOP("Device", "Wellington")
        QT_TRANSLATE_NOOP("Device", "Chatham")
        QT_TRANSLATE_NOOP("Device", "Nuku'alofa")
        QT_TRANSLATE_NOOP("Device", "Apia")
        QT_TRANSLATE_NOOP("Device", "Kiritimati")
        QT_TRANSLATE_NOOP("Device", "Current firmware: %1. Your device is already on the latest firmware.")
        QT_TRANSLATE_NOOP("Device", "Current firmware: %1. Checking automatically after the device is connected...")
        QT_TRANSLATE_NOOP("Device", "Current firmware: %1. Latest firmware: %2.")
        QT_TRANSLATE_NOOP("Device", "Current firmware: %1. Firmware update completed successfully.")
        QT_TRANSLATE_NOOP("Device", "Current firmware: %1. Do not disconnect the device during update.")
        QT_TRANSLATE_NOOP("Device", "Current firmware: %1. %2")
        QT_TRANSLATE_NOOP("Device", "Current firmware: %1. Waiting for automatic firmware check.")
        QT_TRANSLATE_NOOP("Device", "Device is not connected")
        QT_TRANSLATE_NOOP("Device", "Device manager is unavailable")
        QT_TRANSLATE_NOOP("Device", "No firmware update is available")
        QT_TRANSLATE_NOOP("Device", "The device was disconnected")
        QT_TRANSLATE_NOOP("Device", "Device time synchronized successfully")
        QT_TRANSLATE_NOOP("Device", "Failed to synchronize device time")
        QT_TRANSLATE_NOOP("Device", "The device was disconnected during OTA transfer")
        QT_TRANSLATE_NOOP("Device", "NiYien A1")
    }

    Rectangle {
        width: parent.width
        height: contentColumn.implicitHeight + 22 * dpiScale
        radius: 12 * dpiScale
        color: root.cardColor
        border.width: 1 * dpiScale
        border.color: root.borderColor

        Column {
            id: contentColumn
            anchors.fill: parent
            anchors.margins: 12 * dpiScale
            spacing: 10 * dpiScale

            Rectangle {
                width: parent.width
                height: headerContent.implicitHeight + 18 * dpiScale
                radius: 10 * dpiScale
                color: root.sectionColor
                border.width: 1 * dpiScale
                border.color: root.borderColor

                Column {
                    id: headerContent
                    anchors.fill: parent
                    anchors.margins: 10 * dpiScale
                    spacing: 6 * dpiScale

                    Row {
                        width: parent.width
                        spacing: 10 * dpiScale

                        Rectangle {
                            width: 40 * dpiScale
                            height: width
                            radius: 11 * dpiScale
                            color: root.lightTheme ? "#1f116cad" : "#2e76baed"
                            border.width: 1 * dpiScale
                            border.color: root.lightTheme ? "#42116cad" : "#6176baed"

                            BasicText {
                                anchors.centerIn: parent
                                leftPadding: 0
                                text: "A1"
                                font.pixelSize: 14 * dpiScale
                                font.bold: true
                            }
                        }

                        Column {
                            width: parent.width - 50 * dpiScale
                            spacing: 4 * dpiScale

                            BasicText {
                                width: parent.width
                                leftPadding: 0
                                text: qsTranslate("Device", "Model") + ": " + (controller.device_name.length > 0 ? controller.device_name : qsTranslate("Device", "NiYien A1"))
                                font.pixelSize: 12 * dpiScale
                                color: root.mutedTextColor
                                elide: Text.ElideRight
                            }

                            BasicText {
                                width: parent.width
                                leftPadding: 0
                                font.pixelSize: 12 * dpiScale
                                color: root.mutedTextColor
                                text: qsTranslate("Device", "Software") + ": " + (controller.device_soft_version.length > 0 ? controller.device_soft_version : "--")
                                      + "    "
                                      + qsTranslate("Device", "Hardware") + ": " + (controller.device_hard_version.length > 0 ? controller.device_hard_version : "--")
                                wrapMode: Text.WordWrap
                            }
                        }
                    }

                    Rectangle {
                        width: Math.min(parent.width, headerStateText.implicitWidth + 18 * dpiScale)
                        height: 28 * dpiScale
                        radius: height / 2
                        color: Qt.rgba(root.firmwareStatusColor().r, root.firmwareStatusColor().g, root.firmwareStatusColor().b, root.lightTheme ? 0.10 : 0.16)
                        border.width: 1 * dpiScale
                        border.color: Qt.rgba(root.firmwareStatusColor().r, root.firmwareStatusColor().g, root.firmwareStatusColor().b, root.lightTheme ? 0.22 : 0.36)

                        BasicText {
                            id: headerStateText
                            anchors.centerIn: parent
                            width: parent.width - 12 * dpiScale
                            leftPadding: 0
                            horizontalAlignment: Text.AlignHCenter
                            text: root.firmwareStatusTitle()
                            color: root.firmwareStatusColor()
                            font.pixelSize: 11 * dpiScale
                            font.bold: true
                            elide: Text.ElideRight
                        }
                    }
                }
            }

            Rectangle {
                width: parent.width
                height: timeSection.implicitHeight + 18 * dpiScale
                radius: 10 * dpiScale
                color: root.sectionColor
                border.width: 1 * dpiScale
                border.color: root.borderColor

                Column {
                    id: timeSection
                    anchors.fill: parent
                    anchors.margins: 10 * dpiScale
                    spacing: 8 * dpiScale

                    BasicText {
                        width: parent.width
                        leftPadding: 0
                        text: qsTranslate("Device", "Device time")
                        font.pixelSize: 12 * dpiScale
                        font.bold: true
                        color: root.mutedTextColor
                    }

                    BasicText {
                        width: parent.width
                        leftPadding: 0
                        text: root.deviceTimeText.length > 0 ? root.deviceTimeText : "--"
                        font.pixelSize: 18 * dpiScale
                        font.bold: true
                    }

                    BasicText {
                        width: parent.width
                        leftPadding: 0
                        text: qsTranslate("Device", "Timezone") + ": " + root.currentTimezoneLabel()
                        color: root.mutedTextColor
                        wrapMode: Text.WordWrap
                    }

                    Row {
                        width: parent.width
                        spacing: 8 * dpiScale

                        Button {
                            width: (parent.width - parent.spacing) / 2
                            accent: true
                            text: controller.device_time_sync_in_progress ? qsTranslate("Device", "Syncing...") : qsTranslate("Device", "Sync Time")
                            enabled: controller.device_connected && !controller.device_time_sync_in_progress && controller.ota_state !== "updating"
                            onClicked: controller.sync_device_time(root.selectedTimezone.offsetMinutes)
                        }

                        Button {
                            width: (parent.width - parent.spacing) / 2
                            text: qsTranslate("Device", "Set timezone")
                            onClicked: timezonePopup.open()
                        }
                    }
                }
            }

            Rectangle {
                width: parent.width
                height: 1 * dpiScale
                color: root.lightTheme ? root.borderColor : Qt.rgba(1, 1, 1, 0.24)
            }

            BasicText {
                width: parent.width
                leftPadding: 0
                text: root.firmwareDetailText()
                color: controller.ota_state === "failed" ? "#c94949" : root.mutedTextColor
                wrapMode: Text.WordWrap
            }

            QQC.ProgressBar {
                width: parent.width
                visible: controller.ota_state === "updating"
                from: 0
                to: 1
                value: controller.ota_progress
            }

            Button {
                width: parent.width
                accent: true
                visible: controller.firmware_update_available
                enabled: controller.device_connected && controller.ota_state !== "updating"
                text: controller.ota_state === "updating" ? qsTranslate("Device", "Updating...") : qsTranslate("Device", "Update Firmware")
                onClicked: controller.start_firmware_update()
            }
        }
    }

    QQC.Popup {
        id: timezonePopup
        parent: window
        modal: true
        focus: true
        width: Math.min(window.width * 0.9, 640 * dpiScale)
        height: Math.min(window.height * 0.86, 520 * dpiScale)
        x: Math.max(0, Math.round((window.width - width) / 2))
        y: Math.max(0, Math.round((window.height - height) / 2))
        padding: 18 * dpiScale
        closePolicy: QQC.Popup.CloseOnEscape | QQC.Popup.CloseOnPressOutside

        background: Rectangle {
            color: root.popupColor
            radius: 14 * dpiScale
            border.width: 1 * dpiScale
            border.color: root.borderColor
        }

        Column {
            width: timezonePopup.availableWidth
            spacing: 14 * dpiScale

            BasicText {
                width: parent.width
                leftPadding: 0
                text: qsTranslate("Device", "Set device timezone")
                horizontalAlignment: Text.AlignHCenter
                font.pixelSize: 20 * dpiScale
                font.bold: true
            }

            BasicText {
                width: parent.width
                leftPadding: 0
                text: qsTranslate("Device", "Current selection") + ": " + root.currentTimezoneLabel()
                horizontalAlignment: Text.AlignHCenter
                color: root.mutedTextColor
                wrapMode: Text.WordWrap
            }

            Rectangle {
                width: parent.width
                height: 260 * dpiScale
                radius: 12 * dpiScale
                color: root.mapOceanColor
                border.width: 1 * dpiScale
                border.color: root.borderColor
                clip: true

                Image {
                    id: mapImage
                    anchors.fill: parent
                    anchors.margins: 12 * dpiScale
                    source: "qrc:/resources/world_map_simple.svg"
                    fillMode: Image.PreserveAspectFit
                    sourceSize.width: Math.max(2000, Math.round(mapViewport.width * 3))
                    sourceSize.height: Math.max(1000, Math.round(mapViewport.height * 3))
                    smooth: false
                    mipmap: true
                    opacity: 1.0
                }

                Item {
                    id: mapViewport
                    anchors.centerIn: mapImage
                    width: Math.min(mapImage.width, mapImage.height * 2)
                    height: width / 2
                }

                Repeater {
                    model: root.timezoneCatalog

                    Rectangle {
                        required property var modelData
                        property bool active: root.selectedRegion === modelData
                        parent: mapViewport
                        x: modelData.x * mapViewport.width - width / 2
                        y: modelData.y * mapViewport.height - height / 2
                        width: active ? 18 * dpiScale : 12 * dpiScale
                        height: width
                        radius: width / 2
                        color: active ? root.cityDotSelectedColor : root.cityDotColor
                        border.width: active ? 2 * dpiScale : 1 * dpiScale
                        border.color: active ? "#ffffff" : Qt.rgba(1, 1, 1, root.lightTheme ? 0.85 : 0.55)

                        Rectangle {
                            visible: parent.active
                            anchors.centerIn: parent
                            width: parent.width + 10 * dpiScale
                            height: width
                            radius: width / 2
                            color: "transparent"
                            border.width: 1 * dpiScale
                            border.color: Qt.rgba(root.cityDotSelectedColor.r, root.cityDotSelectedColor.g, root.cityDotSelectedColor.b, 0.36)
                        }

                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.PointingHandCursor
                            onClicked: root.selectRegion(parent.modelData)
                        }
                    }
                }
            }

            Label {
                position: Label.LeftPosition
                text: qsTranslate("Device", "Nearest city")
                width: parent.width

                ComboBox {
                    id: cityCombo
                    width: parent.width
                    model: root.regionChoices(root.selectedRegion).map(choice => root.cityDisplayName(choice.key) + "  " + root.formatUtcOffset(choice.offsetMinutes))
                    currentIndex: {
                        const choices = root.regionChoices(root.selectedRegion)
                        for (let i = 0; i < choices.length; ++i) {
                            if (choices[i].key === root.selectedTimezone.key && choices[i].offsetMinutes === root.selectedTimezone.offsetMinutes)
                                return i
                        }
                        return choices.length > 0 ? 0 : -1
                    }
                    enabled: model.length > 0
                    onActivated: {
                        const choices = root.regionChoices(root.selectedRegion)
                        if (currentIndex >= 0 && currentIndex < choices.length)
                            root.applyTimezoneChoice(choices[currentIndex], root.selectedRegion)
                    }
                }
            }

            Row {
                width: parent.width
                spacing: 8 * dpiScale

                Button {
                    width: (parent.width - parent.spacing) / 2
                    text: qsTranslate("Device", "Use system timezone")
                    onClicked: root.selectSystemTimezone()
                }

                Button {
                    width: (parent.width - parent.spacing) / 2
                    accent: true
                    text: qsTranslate("Device", "Close")
                    onClicked: timezonePopup.close()
                }
            }
        }
    }
}
