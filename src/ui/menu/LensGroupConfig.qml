// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Adrian <adrian.eddy at gmail>

pragma ComponentBehavior: Bound

import QtQuick

import "../components/"

MenuItem {
    id: root
    text: qsTr("Lens groups")
    iconName: "lens"
    objectName: "lens-group-config"
    opened: true

    property var statuses: []
    property var configs: []
    property var presets: []
    property int selectedLensIndex: 0
    property bool syncing: false
    property bool manualEditExpanded: false

    readonly property bool batchScope: !!(window.batchState
        && window.batchState.active
        && window.videoArea
        && window.videoArea.queue
        && window.videoArea.queue.selectedCount > 0)
    readonly property string scopeText: batchScope ? qsTr("Local") : qsTr("Global")
    readonly property int usedLensGroupCount: {
        let count = 0
        for (let i = 0; i < statuses.length; ++i) {
            if (statuses[i] && statuses[i].used)
                count++
        }
        return count
    }
    readonly property bool hasManualEntries: {
        for (let i = 0; i < configs.length; ++i) {
            const config = configs[i] || defaultConfig(i)
            if (hasManualFocusValue(config) || !!config.anamorphic_enabled || hasMixedState(config))
                return true
        }
        return false
    }
    readonly property bool compactMode: {
        if (usedLensGroupCount <= 0 || hasManualEntries)
            return false
        for (let i = 0; i < statuses.length; ++i) {
            const status = statuses[i]
            if (!status || !status.used)
                continue
            if (!status.has_auto_focus || status.has_missing_focus)
                return false
        }
        return true
    }
    readonly property bool editorVisible: !compactMode || manualEditExpanded || hasManualEntries

    readonly property bool lightTheme: style === "light"
    readonly property color cardColor: root.lightTheme ? "#ffffff" : styleButtonColor
    readonly property color sectionColor: root.lightTheme ? "#f7f9fc" : styleBackground2
    readonly property color borderColor: root.lightTheme ? "#d6dee8" : stylePopupBorder
    readonly property color mutedTextColor: root.lightTheme ? "#516171" : Qt.rgba(styleTextColor.r, styleTextColor.g, styleTextColor.b, 0.72)

    function selectedJobIds(): var {
        if (!batchScope || !window.videoArea || !window.videoArea.queue)
            return []
        return Object.keys(window.videoArea.queue.selectedJobs || {}).map(Number)
    }
    function selectedJobIdsJson(): string {
        return JSON.stringify(selectedJobIds())
    }
    function defaultStatus(index: int): var {
        return {
            lens_index: index,
            used: false,
            has_auto_focus: false,
            has_missing_focus: false,
            auto_focus_length_mm: null,
            video_count: 0
        }
    }
    function defaultConfig(index: int): var {
        return {
            lens_index: index,
            focal_length_mm: null,
            anamorphic_enabled: false,
            preset_id: null,
            squeeze_direction: "horizontal",
            squeeze_ratio: null,
            mixed_focal_length: false,
            mixed_anamorphic_enabled: false,
            mixed_preset_id: false,
            mixed_squeeze_direction: false,
            mixed_squeeze_ratio: false
        }
    }
    function normalizeStatuses(raw: var): var {
        let result = []
        for (let i = 0; i < 6; ++i) result.push(defaultStatus(i))
        if (!Array.isArray(raw)) return result
        for (let i = 0; i < raw.length; ++i) {
            const item = raw[i]
            if (!item) continue
            const index = item.lens_index !== undefined ? +item.lens_index : i
            if (index >= 0 && index < 6)
                result[index] = Object.assign(defaultStatus(index), item, { lens_index: index })
        }
        return result
    }
    function normalizeConfigs(raw: var): var {
        let result = []
        for (let i = 0; i < 6; ++i) result.push(defaultConfig(i))
        if (!Array.isArray(raw)) return result
        for (let i = 0; i < raw.length; ++i) {
            const item = raw[i]
            if (!item) continue
            const index = item.lens_index !== undefined ? +item.lens_index : i
            if (index >= 0 && index < 6)
                result[index] = Object.assign(defaultConfig(index), item, { lens_index: index })
        }
        return result
    }
    function parseJson(text: string, fallback: var): var {
        if (!text || text.length === 0) return fallback
        try {
            return JSON.parse(text)
        } catch (e) {
            console.warn("LensGroupConfig parse error:", e, text)
            return fallback
        }
    }
    function loadStatuses(): void {
        syncing = true
        if (batchScope) {
            statuses = normalizeStatuses(parseJson(render_queue.get_selected_lens_group_status_json(selectedJobIdsJson()), []))
        } else {
            statuses = normalizeStatuses(parseJson(controller.lens_group_status, []))
        }
        updateSelection()
        refreshUiFromSelection()
        syncing = false
    }
    function loadConfigs(): void {
        syncing = true
        if (batchScope) {
            configs = normalizeConfigs(parseJson(render_queue.get_selected_lens_group_config_json(selectedJobIdsJson()), []))
        } else {
            configs = normalizeConfigs(parseJson(controller.lens_group_config, []))
        }
        updateSelection()
        refreshUiFromSelection()
        syncing = false
    }
    function loadPresets(): void {
        presets = parseJson(controller.get_anamorphic_presets(), [])
        if (!Array.isArray(presets))
            presets = []
    }
    function updateSelection(): void {
        for (let i = 0; i < statuses.length; ++i) {
            const status = statuses[i]
            if (status.used && status.has_missing_focus) {
                selectedLensIndex = i
                return
            }
        }
        for (let i = 0; i < configs.length; ++i) {
            const config = configs[i] || defaultConfig(i)
            if (hasManualFocusValue(config) || !!config.anamorphic_enabled) {
                selectedLensIndex = i
                return
            }
        }
        for (let i = 0; i < statuses.length; ++i) {
            if (statuses[i].used) {
                selectedLensIndex = i
                return
            }
        }
        selectedLensIndex = 0
    }
    function currentStatus(): var {
        return statuses[selectedLensIndex] || defaultStatus(selectedLensIndex)
    }
    function currentConfig(): var {
        return configs[selectedLensIndex] || defaultConfig(selectedLensIndex)
    }
    function hasManualFocusValue(config: var): bool {
        return (config && config.focal_length_mm || 0) > 0
    }
    function hasMixedState(config: var): bool {
        return !!(config.mixed_focal_length
            || config.mixed_anamorphic_enabled
            || config.mixed_preset_id
            || config.mixed_squeeze_direction
            || config.mixed_squeeze_ratio)
    }
    function focusFieldValue(config: var): real {
        if (config.mixed_focal_length)
            return 0
        return config.focal_length_mm || 0
    }
    function lensGroupLabel(index: int): string {
        const status = statuses[index] || defaultStatus(index)
        const config = configs[index] || defaultConfig(index)
        if (!status.used)
            return "L" + (index + 1) + " - " + qsTr("Unused")
        if (hasMixedState(config))
            return "L" + (index + 1) + " - " + qsTr("Mixed")
        if (hasManualFocusValue(config))
            return "L" + (index + 1) + " " + config.focal_length_mm.toFixed(1) + "mm"
        if (status.has_auto_focus && status.auto_focus_length_mm > 0)
            return "L" + (index + 1) + " " + status.auto_focus_length_mm.toFixed(1) + "mm"
        if (status.has_missing_focus)
            return "L" + (index + 1) + " - " + qsTr("No focus")
        return "L" + (index + 1)
    }
    function lensGroupOptions(): var {
        let result = []
        for (let i = 0; i < 6; ++i) {
            const status = statuses[i] || defaultStatus(i)
            result.push({
                value: i,
                label: lensGroupLabel(i),
                enabled: !!status.used
            })
        }
        return result
    }
    function presetOptions(): var {
        let result = [
            {
                id: "__manual__",
                name: qsTr("Manual setup")
            }
        ]
        for (let i = 0; i < presets.length; ++i)
            result.push(presets[i])
        return result
    }
    function currentPresetIndex(): int {
        const config = currentConfig()
        if (config.mixed_preset_id || !config.preset_id)
            return 0
        const options = presetOptions()
        for (let i = 0; i < options.length; ++i) {
            if (options[i].id === config.preset_id)
                return i
        }
        return 0
    }
    function currentSqueezeRatio(): real {
        const config = currentConfig()
        if (config.mixed_squeeze_ratio)
            return 1.33
        if (config.preset_id) {
            const options = presetOptions()
            const index = currentPresetIndex()
            if (options[index] && options[index].squeeze_ratio > 0)
                return options[index].squeeze_ratio
        }
        return config.squeeze_ratio || 1.33
    }
    function refreshUiFromSelection(): void {
        if (!lensGroupCombo || !focalLengthField || !anamorphicBox || !presetCombo || !horizontalDirection || !verticalDirection || !squeezeRatioField)
            return

        const previousSyncing = syncing
        syncing = true
        const config = currentConfig()
        const direction = config.mixed_squeeze_direction ? "horizontal" : (config.squeeze_direction || "horizontal")

        if (lensGroupCombo.currentIndex !== selectedLensIndex)
            lensGroupCombo.currentIndex = selectedLensIndex
        if (focalLengthField.value !== focusFieldValue(config))
            focalLengthField.value = focusFieldValue(config)
        if (anamorphicBox.checked !== !!config.anamorphic_enabled)
            anamorphicBox.checked = !!config.anamorphic_enabled
        if (presetCombo.currentIndex !== currentPresetIndex())
            presetCombo.currentIndex = currentPresetIndex()
        if (horizontalDirection.checked !== (direction === "horizontal"))
            horizontalDirection.checked = direction === "horizontal"
        if (verticalDirection.checked !== (direction === "vertical"))
            verticalDirection.checked = direction === "vertical"
        if (squeezeRatioField.value !== currentSqueezeRatio())
            squeezeRatioField.value = currentSqueezeRatio()
        syncing = previousSyncing
    }
    function persistConfigs(next: var): void {
        if (batchScope) {
            render_queue.set_selected_lens_group_config(selectedJobIdsJson(), JSON.stringify(next))
            Qt.callLater(loadConfigs)
        } else {
            controller.set_lens_group_config(JSON.stringify(next))
            if (typeof render_queue !== "undefined" && render_queue.has_match_results())
                render_queue.reapply_lens_group_config()
        }
    }
    function updateCurrentConfig(mutator): void {
        if (syncing) return
        syncing = true
        let next = normalizeConfigs(configs)
        let config = Object.assign({}, next[selectedLensIndex] || defaultConfig(selectedLensIndex))
        mutator(config)
        config.mixed_focal_length = false
        config.mixed_anamorphic_enabled = false
        config.mixed_preset_id = false
        config.mixed_squeeze_direction = false
        config.mixed_squeeze_ratio = false
        next[selectedLensIndex] = config
        configs = next
        refreshUiFromSelection()
        syncing = false
        persistConfigs(next)
    }
    function clearCurrentFocalLength(): void {
        if (batchScope) {
            render_queue.clear_selected_lens_group_config(selectedJobIdsJson(), selectedLensIndex)
            Qt.callLater(loadConfigs)
        } else {
            updateCurrentConfig(config => {
                config.focal_length_mm = null
            })
        }
        manualEditExpanded = false
    }
    onSelectedLensIndexChanged: {
        if (!syncing)
            refreshUiFromSelection()
    }
    onBatchScopeChanged: {
        manualEditExpanded = false
        loadStatuses()
        loadConfigs()
    }

    Connections {
        target: controller
        function onLens_group_status_changed(): void {
            if (!root.batchScope) root.loadStatuses()
        }
        function onLens_group_config_changed(): void {
            if (!root.batchScope) root.loadConfigs()
        }
    }
    Connections {
        target: render_queue
        function onMatch_results_changed(): void {
            root.loadStatuses()
            root.loadConfigs()
        }
        function onMatch_apply_finished(): void {
            root.loadStatuses()
            root.loadConfigs()
        }
    }
    Connections {
        target: window.videoArea && window.videoArea.queue ? window.videoArea.queue : null
        function onSelectedJobsChanged(): void {
            if (root.batchScope) {
                root.loadStatuses()
                root.loadConfigs()
            }
        }
    }

    Component.onCompleted: {
        loadPresets()
        loadStatuses()
        loadConfigs()
    }

    Rectangle {
        width: parent.width
        height: contentColumn.implicitHeight + 20 * dpiScale
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
                height: headerColumn.implicitHeight + 16 * dpiScale
                radius: 10 * dpiScale
                color: root.sectionColor
                border.width: 1 * dpiScale
                border.color: root.borderColor

                Column {
                    id: headerColumn
                    anchors.fill: parent
                    anchors.margins: 10 * dpiScale
                    spacing: 6 * dpiScale

                    BasicText {
                        width: parent.width
                        leftPadding: 0
                        text: qsTr("Manually set lens focal length or anamorphic info.")
                        color: root.mutedTextColor
                        wrapMode: Text.WordWrap
                    }

                    BasicText {
                        width: parent.width
                        leftPadding: 0
                        text: root.scopeText
                        color: root.mutedTextColor
                        font.bold: true
                    }

                    Row {
                        visible: root.compactMode || root.manualEditExpanded
                        spacing: 8 * dpiScale

                        Button {
                            text: root.manualEditExpanded ? qsTr("Auto detect") : qsTr("Manual edit")
                            accent: true
                            height: 30 * dpiScale
                            leftPadding: 14 * dpiScale
                            rightPadding: 14 * dpiScale
                            onClicked: {
                                if (root.manualEditExpanded) {
                                    root.clearCurrentFocalLength()
                                } else {
                                    root.manualEditExpanded = true
                                }
                            }
                        }
                    }
                }
            }

            Column {
                width: parent.width
                spacing: 10 * dpiScale
                visible: root.editorVisible

                Label {
                    position: Label.LeftPosition
                    text: qsTr("Lens group")
                    width: parent.width

                    ComboBox {
                        id: lensGroupCombo
                        width: parent.width
                        textRole: "label"
                        model: root.lensGroupOptions()
                        currentIndex: Math.max(0, Math.min(root.selectedLensIndex, model.length - 1))
                        onActivated: {
                            if (!root.syncing && model[currentIndex] && model[currentIndex].enabled)
                                root.selectedLensIndex = currentIndex
                        }
                    }
                }

                Label {
                    position: Label.LeftPosition
                    text: qsTr("Focal length")
                    width: parent.width

                    NumberField {
                        id: focalLengthField
                        width: parent.width
                        value: root.focusFieldValue(root.currentConfig())
                        defaultValue: 0
                        from: 0
                        to: 2000
                        precision: 1
                        unit: qsTr("mm")
                        placeholderText: root.currentConfig().mixed_focal_length ? qsTr("Mixed") : ""
                        enabled: !!root.currentStatus().used
                        opacity: enabled ? 1.0 : 0.6
                        onValueChanged: {
                            if (root.syncing) return
                            root.updateCurrentConfig(config => {
                                config.focal_length_mm = value > 0 ? value : null
                            })
                        }
                    }
                }

                CheckBoxWithContent {
                    id: anamorphicBox
                    text: qsTr("Anamorphic lens")
                    cb.enabled: !!root.currentStatus().used
                    cb.opacity: cb.enabled ? 1.0 : 0.6
                    cb.onCheckedChanged: {
                        if (root.syncing) return
                        root.updateCurrentConfig(config => {
                            config.anamorphic_enabled = cb.checked
                            if (!cb.checked) {
                                config.preset_id = null
                                config.squeeze_direction = "horizontal"
                                config.squeeze_ratio = null
                            } else if (!config.squeeze_direction) {
                                config.squeeze_direction = "horizontal"
                            }
                        })
                    }

                    BasicText {
                        visible: root.currentConfig().mixed_anamorphic_enabled
                            || root.currentConfig().mixed_preset_id
                            || root.currentConfig().mixed_squeeze_direction
                            || root.currentConfig().mixed_squeeze_ratio
                        width: parent.width
                        leftPadding: 0
                        color: root.mutedTextColor
                        text: qsTr("Mixed")
                    }

                    Label {
                        position: Label.LeftPosition
                        text: qsTr("Preset")
                        width: parent.width

                        ComboBox {
                            id: presetCombo
                            width: parent.width
                            textRole: "name"
                            model: root.presetOptions()
                            onActivated: {
                                if (root.syncing) return
                                const option = model[currentIndex]
                                root.updateCurrentConfig(config => {
                                    if (option.id === "__manual__") {
                                        config.preset_id = null
                                        if (!config.squeeze_direction)
                                            config.squeeze_direction = "horizontal"
                                        if (!(config.squeeze_ratio > 0))
                                            config.squeeze_ratio = 1.33
                                    } else {
                                        config.preset_id = option.id
                                        if (!config.squeeze_direction)
                                            config.squeeze_direction = "horizontal"
                                        config.squeeze_ratio = option.squeeze_ratio
                                    }
                                })
                            }
                        }
                    }

                    Row {
                        width: parent.width
                        spacing: 12 * dpiScale

                        RadioButton {
                            id: horizontalDirection
                            width: (parent.width - parent.spacing) / 2
                            text: qsTr("Horizontal")
                            onCheckedChanged: {
                                if (root.syncing || !checked) return
                                root.updateCurrentConfig(config => config.squeeze_direction = "horizontal")
                            }
                        }

                        RadioButton {
                            id: verticalDirection
                            width: (parent.width - parent.spacing) / 2
                            text: qsTr("Vertical")
                            onCheckedChanged: {
                                if (root.syncing || !checked) return
                                root.updateCurrentConfig(config => config.squeeze_direction = "vertical")
                            }
                        }
                    }

                    Label {
                        position: Label.LeftPosition
                        text: qsTr("Squeeze ratio")
                        width: parent.width

                        NumberField {
                            id: squeezeRatioField
                            width: parent.width
                            value: 1.33
                            defaultValue: 1.33
                            from: 1.0
                            to: 3.0
                            precision: 2
                            readOnly: !!root.currentConfig().preset_id
                            placeholderText: root.currentConfig().mixed_squeeze_ratio ? qsTr("Mixed") : ""
                            opacity: readOnly ? 0.6 : 1.0
                            onValueChanged: {
                                if (root.syncing || readOnly) return
                                root.updateCurrentConfig(config => {
                                    config.squeeze_ratio = value > 1.0 ? value : null
                                })
                            }
                        }
                    }
                }
            }
        }
    }
}
