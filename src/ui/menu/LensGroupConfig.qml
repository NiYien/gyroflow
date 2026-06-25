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
    opened: false
    btnHeight: 28 * dpiScale

    property var statuses: []
    property var configs: []
    property var presets: []
    property int selectedLensIndex: 0
    property bool syncing: false
    property bool manualEditExpanded: false
    // Lock auto-selection after the user has manually picked a lens group from
    // the dropdown — otherwise loadConfigs / loadStatuses would re-run
    // updateSelection on every persist and snap selection back to whichever
    // group hits hasManualFocusValue first (typically L1).
    property bool userPickedLens: false
    // Suppress persistence during component construction. NumberField defaults
    // (squeezeRatioField.value=1.33 etc.) trigger onValueChanged at init time,
    // which would otherwise cascade through updateCurrentConfig → persistConfigs
    // → controller.set_lens_group_config(default 6 configs, all has_values=false)
    // → settings::lens_group_configs_v1 = "[]" — wiping the user's persisted
    // L1-L6 right at startup. Set to true in Component.onCompleted after the
    // first loadConfigs() finishes.
    property bool _bootDone: false

    // batchScope is true whenever the render queue has at least one selected
    // job. Don't gate on batchState.active — that flag also requires the queue
    // panel to be visible, but the right-click "Edit" flow closes the queue
    // panel right after setting selection, which would suppress the per-job
    // hint + "Apply globally" button. Selection state alone is the right
    // signal for "are we editing per-job vs global".
    readonly property bool batchScope: !!(window.videoArea
        && window.videoArea.queue
        && window.videoArea.queue.selectedCount > 0)
    // Lens-group fields are editable at all times — the Manual edit flag only decides whether
    // the values are *applied* (manual focal auto-fills telemetry gaps regardless; anamorphic
    // needs the flag, which persistConfigs raises automatically once a group has a manual value).
    readonly property bool editorVisible: true

    readonly property bool lightTheme: style === "light"
    readonly property color cardColor: root.lightTheme ? "#ffffff" : styleButtonColor
    readonly property color sectionColor: root.lightTheme ? "#f7f9fc" : styleBackground2
    readonly property color borderColor: root.lightTheme ? "#d6dee8" : stylePopupBorder
    readonly property color mutedTextColor: root.lightTheme ? "#516171" : "#b8ffffff"

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
            pre_anamorphic_focal_length_mm: null,
            pre_anamorphic_focal_length_captured: false,
            anamorphic_enabled: false,
            preset_id: null,
            squeeze_direction: "horizontal",
            squeeze_ratio: null,
            lens_correction_amount: null,
            mixed_focal_length: false,
            mixed_anamorphic_enabled: false,
            mixed_preset_id: false,
            mixed_squeeze_direction: false,
            mixed_squeeze_ratio: false,
            mixed_lens_correction_amount: false
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
        presets = parseJson(controller.get_lens_presets(), [])
        if (!Array.isArray(presets))
            presets = []
    }
    function updateSelection(): void {
        // If the user already picked a lens group manually, do not let auto-select
        // override it on every persist (Part B fix A: editing focal in L3 was
        // snapping back to L1 because L1 had a persisted manual focal value).
        if (userPickedLens) return
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
    function anamorphicSuffix(config: var): string {
        if (!config || !config.anamorphic_enabled) return ""
        // Prefer preset name when configured. mixed_preset_id means multi-select
        // disagreement -> skip name (would be misleading).
        if (config.preset_id && !config.mixed_preset_id) {
            for (let i = 0; i < presets.length; ++i) {
                if (presets[i].id === config.preset_id)
                    return " · " + presets[i].name
            }
        }
        // Manual setup: show ratio + direction (H/V) when squeeze > 1.
        const ratio = config.squeeze_ratio || 0
        if (ratio > 1.0 && !config.mixed_squeeze_ratio) {
            const dir = (config.squeeze_direction === "vertical") ? "V" : "H"
            return " · " + ratio.toFixed(2).replace(/\.?0+$/, "") + "x-" + dir
        }
        return ""
    }
    function lensGroupLabel(index: int): string {
        const status = statuses[index] || defaultStatus(index)
        const config = configs[index] || defaultConfig(index)
        // Prefix a bullet for groups that were detected in the current telemetry —
        // a lightweight visual cue without disabling the row.
        const badge = status.used ? "● " : ""
        const anamorphic = anamorphicSuffix(config)
        // Part B fix D: per user request, don't tag the lens group label with
        // "- Mixed" in multi-select. Manual / auto focal labels still apply.
        if (hasManualFocusValue(config))
            return badge + "L" + (index + 1) + " " + config.focal_length_mm.toFixed(1) + "mm" + anamorphic
        if (status.has_auto_focus && status.auto_focus_length_mm > 0)
            return badge + "L" + (index + 1) + " " + status.auto_focus_length_mm.toFixed(1) + "mm" + anamorphic
        if (status.has_missing_focus)
            return badge + "L" + (index + 1) + " - " + qsTr("No focus")
        return "L" + (index + 1) + anamorphic
    }
    function lensGroupOptions(): var {
        let result = []
        for (let i = 0; i < 6; ++i) {
            result.push({
                value: i,
                label: lensGroupLabel(i),
                enabled: true
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
        if (!focalLengthField.activeFocus && focalLengthField.value !== focusFieldValue(config))
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
        if (lensCorrectionSlider) {
            // Fallback 0 covers the migration case where settings.json has
            // anamorphic_enabled=true but lens_correction_amount==null from
            // an older session that defaulted to 100%.
            const correctionVal = config.lens_correction_amount !== null && config.lens_correction_amount !== undefined
                ? +config.lens_correction_amount
                : 0
            if (lensCorrectionSlider.value !== correctionVal)
                lensCorrectionSlider.value = correctionVal
        }
        syncing = previousSyncing
    }
    function persistConfigs(next: var): void {
        // Skip persistence during boot — NumberField default-value initial
        // change events would otherwise wipe lens_group_configs_v1 to "[]".
        if (!_bootDone) return
        // "Manual" tracks whether any group actually carries a manual value: setting a focal
        // length / anamorphic enters manual mode (so anamorphic applies and the value reaches
        // queued jobs); clearing them all returns to telemetry auto. The fields stay editable
        // regardless of this flag (editorVisible is always true).
        const anyManual = next.some(c => root.hasManualFocusValue(c) || !!c.anamorphic_enabled)
        if (controller.lens_group_manual_edit !== anyManual)
            controller.lens_group_manual_edit = anyManual
        // simple-mode-ux-overhaul: write goes to global config unconditionally.
        // batchScope path removed — per-job overrides are now read-only data carried
        // from .gyroflow load, edited via the render-queue right-click menu.
        controller.set_lens_group_config(JSON.stringify(next))
        // Push focal length + anamorphic squeeze of the currently-edited group into the
        // main stabilizer so the live canvas preview actually reflects new fx/fy.
        // Returns JSON {"w":W,"h":H} when anamorphic pushes an output dimension so we
        // can propagate it to Export settings' output width/height NumberFields too.
        const outJson = controller.apply_lens_group_to_main(selectedLensIndex) + ""
        if (outJson.length > 0 && window.exportSettings) {
            try {
                const dim = JSON.parse(outJson)
                if (dim && dim.w > 0 && dim.h > 0) {
                    const isOriginalSize = dim.w == window.exportSettings.originalWidth && dim.h == window.exportSettings.originalHeight
                    if (isOriginalSize) {
                        Qt.callLater(window.exportSettings.lensProfileOutputDimensionCleared)
                    } else if (window.exportSettings.lensProfileOutputSizeActive ||
                               window.exportSettings.outWidth != dim.w ||
                               window.exportSettings.outHeight != dim.h) {
                        Qt.callLater(window.exportSettings.lensProfileOutputDimensionLoaded, dim.w, dim.h)
                    }
                }
            } catch (e) {
                console.warn("apply_lens_group_to_main parse error:", e, outJson)
            }
        }
        // simple-mode-ux-overhaul: enforce global-wins by clearing the per-job
        // override entry for every lens group on every job. Granularity is per-index
        // (clears L1..L6 wholesale) — acceptable per design.md Decision 2.
        if (typeof render_queue !== "undefined") {
            render_queue.clear_all_per_job_lens_group_for_indices(JSON.stringify([0, 1, 2, 3, 4, 5]))
            if (render_queue.has_match_results())
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
        config.mixed_lens_correction_amount = false
        next[selectedLensIndex] = config
        configs = next
        refreshUiFromSelection()
        syncing = false
        persistConfigs(next)
    }
    function clearCurrentFocalLength(): void {
        // simple-mode-ux-overhaul: clearing focal length is always a global edit.
        // persistConfigs takes care of clearing per-job overrides on all jobs.
        updateCurrentConfig(config => {
            config.focal_length_mm = null
        })
        manualEditExpanded = false
    }
    onSelectedLensIndexChanged: {
        if (!syncing)
            refreshUiFromSelection()
    }
    onBatchScopeChanged: {
        manualEditExpanded = false
        // Reset user lens pick when scope changes (entering / leaving batch view) —
        // each scope is allowed its own auto-selected lens group.
        userPickedLens = false
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
        // A NiYien lens hot-update package was downloaded+activated at runtime;
        // re-read the preset list (load_from_lens_package reads fresh from disk)
        // so newly published presets appear without an app restart.
        function onLens_presets_updated(): void {
            root.loadPresets()
            root.refreshUiFromSelection()
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
        // After the initial load + the cascade of NumberField initial-value
        // onValueChanged events has settled, allow persistence again.
        Qt.callLater(() => { root._bootDone = true })
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

                    // Global "Manual edit" toggle for all 6 lens groups. Persists to
                    // settings.json via controller.lens_group_manual_edit. When off, the
                    // fields below stay editable but calibration still follows telemetry
                    // (auto) for every group. When on, a group's focal length / anamorphic
                    // decision follows should_use_manual_config in Rust: missing focal can
                    // be filled manually, and anamorphic can override when enabled.
                    CheckBox {
                        id: manualEditSwitch
                        text: qsTr("Manual edit")
                        tooltip: qsTr("When on, each lens group falls back to its manually-entered focal length / anamorphic if the video has no telemetry focal length, or anamorphic is enabled. Focal length must be > 5mm to take effect.")
                        checked: controller.lens_group_manual_edit
                        onCheckedChanged: {
                            if (checked === controller.lens_group_manual_edit) return
                            controller.lens_group_manual_edit = checked
                            if (root.batchScope && root.selectedJobIds().length === 1)
                                controller.preview_lens_group_config(JSON.stringify(root.configs), root.selectedLensIndex)
                            // Toggling the global gate must re-decide auto/manual for every
                            // queued job too — the per-job render path reads the same
                            // settings flag, but only when reapply is invoked.
                            if (typeof render_queue !== "undefined" && render_queue.has_match_results())
                                render_queue.reapply_lens_group_config()
                        }
                    }
                }
            }

            Column {
                width: parent.width
                spacing: 10 * dpiScale
                visible: root.editorVisible

                // simple-mode-ux-overhaul: batchScope notice removed — edits are always
                // global now. Per-job overrides come from .gyroflow load only; users
                // adjust them via the render-queue right-click "Change lens group" menu.

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
                            if (!root.syncing) {
                                // Lock auto-selection — the user's pick is now sticky.
                                root.userPickedLens = true
                                root.selectedLensIndex = currentIndex
                            }
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
                        // Part B fix D: drop the "Mixed" placeholder in the focal field.
                        placeholderText: ""
                        // All 6 lens groups are editable at all times. The per-group Manual
                        // checkbox decides whether the value is actually applied.
                        enabled: true
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
                    // Always editable — applied only when the group's Manual toggle is on.
                    cb.enabled: true
                    cb.onCheckedChanged: {
                        if (root.syncing) return
                        root.updateCurrentConfig(config => {
                            config.anamorphic_enabled = cb.checked
                            if (!cb.checked) {
                                if (config.pre_anamorphic_focal_length_captured)
                                    config.focal_length_mm = config.pre_anamorphic_focal_length_mm
                                config.pre_anamorphic_focal_length_mm = null
                                config.pre_anamorphic_focal_length_captured = false
                                config.preset_id = null
                                config.squeeze_direction = "horizontal"
                                config.squeeze_ratio = null
                                // Clear the lens-correction override on disable so the next
                                // anamorphic enable starts fresh at the 0% default instead
                                // of inheriting the previous session's slider value.
                                config.lens_correction_amount = null
                            } else {
                                if (!config.pre_anamorphic_focal_length_captured) {
                                    config.pre_anamorphic_focal_length_mm = config.focal_length_mm || null
                                    config.pre_anamorphic_focal_length_captured = true
                                }
                                if (!config.squeeze_direction)
                                    config.squeeze_direction = "horizontal"
                                // First-time enable defaults to 0% — keeps the original
                                // anamorphic look untouched. User edits during the enabled
                                // session are preserved until the toggle is turned off.
                                if (config.lens_correction_amount === null
                                    || config.lens_correction_amount === undefined)
                                    config.lens_correction_amount = 0
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
                                        if ((option.focal_length_mm || 0) > 0)
                                            config.focal_length_mm = option.focal_length_mm
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

                    Label {
                        position: Label.LeftPosition
                        // Reuse the existing "Lens correction" translation from the Stabilization
                        // context (all 22 languages have it) instead of creating a new context.
                        text: qsTranslate("Stabilization", "Lens correction")
                        width: parent.width

                        SliderWithField {
                            id: lensCorrectionSlider
                            width: parent.width
                            from: 0
                            to: 100
                            value: 100
                            defaultValue: 100
                            unit: qsTr("%")
                            precision: 0
                            onValueChanged: {
                                if (root.syncing) return
                                root.updateCurrentConfig(config => {
                                    config.lens_correction_amount = value
                                })
                            }
                        }
                    }
                }

                // simple-mode-ux-overhaul: "Apply globally" button removed — every edit
                // is global by default now.
            }
        }
    }
}
