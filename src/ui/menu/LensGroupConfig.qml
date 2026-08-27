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
    // The lens the loaded .gyroflow carries, or null. Read-only: it never merges
    // into `configs` and is never written back — `configs` is the global L1-L6
    // table, and the panel reads and writes exactly that one source.
    property var projectLens: null
    property var manualCameraState: ({
        eligible: false,
        brand: "",
        model: "",
        selection_valid: false,
        brands: []
    })
    property int selectedLensIndex: 0
    property bool syncing: false
    // `selectedLensIndex` doubles as the real lens index (0..5), so `Now` needs a
    // value that can never collide with one.
    readonly property int nowSentinel: -1
    // batchScope hides `Now`: with a job selected the lens index is assigned, so it
    // is necessarily one of the six groups.
    readonly property bool hasNowEntry: !!projectLens && !batchScope
    // Tracks presence across reloads so a *refresh* of the same project (what a
    // save does) can be told apart from a project *arriving*.
    property bool _projectLensWasPresent: false
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
    function loadManualCameraState(): void {
        if (typeof render_queue === "undefined") return
        const parsed = parseJson(render_queue.get_manual_camera_state_json() + "", null)
        manualCameraState = parsed && Array.isArray(parsed.brands)
            ? parsed
            : { eligible: false, brand: "", model: "", selection_valid: false, brands: [] }
    }
    function manualCameraBrandOptions(): var {
        let result = []
        const brands = manualCameraState.brands || []
        for (let i = 0; i < brands.length; ++i) {
            result.push({
                value: brands[i].id,
                label: brands[i].label || brands[i].id,
                enabled: true
            })
        }
        const saved = manualCameraState.brand || ""
        if (saved.length > 0 && !result.some(item => item.value === saved))
            result.push({ value: saved, label: saved, enabled: false })
        return result
    }
    function manualCameraModelOptions(): var {
        let result = []
        const savedBrand = manualCameraState.brand || ""
        const brands = manualCameraState.brands || []
        for (let i = 0; i < brands.length; ++i) {
            if (brands[i].id !== savedBrand) continue
            const models = brands[i].models || []
            for (let j = 0; j < models.length; ++j) {
                result.push({
                    value: models[j].id,
                    label: models[j].label || models[j].id,
                    enabled: !!models[j].enabled
                })
            }
            break
        }
        const saved = manualCameraState.model || ""
        if (saved.length > 0 && !result.some(item => item.value === saved))
            result.push({ value: saved, label: saved, enabled: false })
        return result
    }
    function manualCameraOptionIndex(options: var, value: string): int {
        for (let i = 0; i < options.length; ++i) {
            if (options[i].value === value) return i
        }
        return -1
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
    function loadProjectLens(): void {
        const raw = controller.project_lens + ""
        const parsed = raw.length > 0 ? parseJson(raw, null) : null
        const arrived = !!parsed && !_projectLensWasPresent
        projectLens = parsed
        _projectLensWasPresent = !!parsed
        // A project just arrived — let auto-selection have a fresh say so the panel
        // shows what the project actually put on screen. A refresh of an already
        // loaded project (what saving does) deliberately does NOT steal the user's
        // current pick.
        if (arrived) userPickedLens = false
        syncing = true
        updateSelection()
        refreshUiFromSelection()
        syncing = false
    }
    function isNowSelected(): bool {
        return selectedLensIndex === nowSentinel
    }
    // Lens group the loaded project belongs to, or -1 when telemetry gave none.
    function projectLensGroupIndex(): int {
        const value = projectLens ? projectLens.lens_index : null
        if (value === null || value === undefined) return -1
        const index = +value
        return (index >= 0 && index < 6) ? index : -1
    }
    // The dropdown lists `Now` first when present, so its indices are offset from
    // the real lens indices by one — and the offset flips at runtime as the project
    // loads/clears and as batchScope toggles. Both directions go through these two
    // functions; nothing in this file may hand-roll the ±1.
    function comboIndexToLensIndex(comboIndex: int): int {
        if (!hasNowEntry) return comboIndex
        return comboIndex === 0 ? nowSentinel : comboIndex - 1
    }
    function lensIndexToComboIndex(lensIndex: int): int {
        if (!hasNowEntry) return Math.max(0, lensIndex)
        return lensIndex === nowSentinel ? 0 : lensIndex + 1
    }
    // The project lens rendered in the shape the editor controls consume, so the
    // read-only fields can be filled by the same refresh path as a real group.
    function projectLensAsConfig(): var {
        if (!projectLens) return defaultConfig(0)
        return Object.assign(defaultConfig(0), {
            focal_length_mm: projectLens.focal_length_mm,
            anamorphic_enabled: !!projectLens.anamorphic_enabled,
            preset_id: projectLens.preset_id,
            squeeze_direction: projectLens.squeeze_direction,
            squeeze_ratio: projectLens.squeeze_ratio,
            lens_correction_amount: projectLens.lens_correction_amount
        })
    }
    // "Now · ★45.0mm · Sirui Atar 50mm 1.33x" — every segment after the first is
    // dropped when the project does not carry it.
    //
    // Deliberately NO lens group number, even though the project lens carries one:
    // that index is the clip's identity (which group its gyro data says it belongs
    // to), while the focal length beside it is whatever lens is actually applied.
    // The two drift apart the moment the user switches groups and saves, and side
    // by side they read as "group L2's focal length is 50mm" — which would be
    // wrong. The index is still kept on `projectLens`; it is what the selection
    // lands on when `Now` disappears in batchScope.
    function nowLabel(): string {
        let parts = [qsTr("Now")]
        const focal = (projectLens ? projectLens.focal_length_mm : 0) || 0
        if (focal > 0) {
            // ★ marks a focal length that was user-owned when the project was
            // saved, matching the render queue's per-job override badge.
            const star = (projectLens && projectLens.focal_overridden) ? "★" : ""
            parts.push(star + focal.toFixed(1) + "mm")
        }
        return parts.join(" · ") + anamorphicSuffix(projectLensAsConfig())
    }
    function updateSelection(): void {
        // `Now` may have gone away since the last pass (a job got selected, or a new
        // video cleared the project), and a stale sentinel selection has to land on
        // something real. The project's own group beats the heuristic below: the
        // project knows which group it belongs to.
        if (isNowSelected() && !hasNowEntry) {
            const projectGroup = projectLensGroupIndex()
            if (projectGroup >= 0) {
                selectedLensIndex = projectGroup
                // Sticky, and the early return matters: loadStatuses() and
                // loadConfigs() each call this function, so without both the second
                // pass would drop straight through to the heuristics and overwrite
                // the landing we just made.
                userPickedLens = true
                return
            }
            // Nothing to land on (project carried no lens index, or the project is
            // gone entirely) — let the heuristics below choose.
            userPickedLens = false
        }
        // If the user already picked a lens group manually, do not let auto-select
        // override it on every persist (Part B fix A: editing focal in L3 was
        // snapping back to L1 because L1 had a persisted manual focal value).
        if (userPickedLens) return
        // A loaded project is the strongest available statement about what the main
        // preview is currently showing, so it wins over every heuristic below.
        if (hasNowEntry) {
            selectedLensIndex = nowSentinel
            return
        }
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
        if (isNowSelected()) return defaultStatus(0)
        return statuses[selectedLensIndex] || defaultStatus(selectedLensIndex)
    }
    function currentConfig(): var {
        if (isNowSelected()) return projectLensAsConfig()
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
        // `Now` first: it describes what is on screen right now, the six groups
        // below it are the user's own library.
        if (hasNowEntry) {
            result.push({
                value: nowSentinel,
                label: nowLabel(),
                enabled: true
            })
        }
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

        const comboIndex = lensIndexToComboIndex(selectedLensIndex)
        if (lensGroupCombo.currentIndex !== comboIndex)
            lensGroupCombo.currentIndex = comboIndex
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
    // Push focal length + anamorphic squeeze of the currently-selected group into the
    // main stabilizer so the live canvas preview actually reflects new fx/fy.
    // apply_lens_group_to_main returns JSON {"w":W,"h":H} when anamorphic pushes an
    // output dimension so we can propagate it to Export settings' output width/height
    // NumberFields too.
    //
    // Shared by two callers with deliberately different scopes:
    //   - persistConfigs()          value edit: also writes the global config and
    //                               clears per-job overrides across the queue
    //   - lensGroupCombo.onActivated  group switch: main preview only, the queue is
    //                               left completely untouched
    function applySelectedGroupToMain(): void {
        // `Now` is not a lens group — restoring the project lens is a different core
        // entry point. Guard here too so no caller can push the sentinel into
        // apply_lens_group_to_main(usize).
        if (isNowSelected()) {
            restoreProjectLensToMain()
            return
        }
        syncExportDimension(controller.apply_lens_group_to_main(selectedLensIndex) + "")
    }
    // Put the loaded project's lens back on the main preview. This is what makes the
    // "switch group, lose the project's lens" behaviour reversible — without it the
    // only way back is reloading the .gyroflow.
    function restoreProjectLensToMain(): void {
        syncExportDimension(controller.restore_project_lens() + "")
    }
    function syncExportDimension(outJson: string): void {
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
                console.warn("lens group output dimension parse error:", e, outJson)
            }
        }
    }
    function persistConfigs(next: var): void {
        // Skip persistence during boot — NumberField default-value initial
        // change events would otherwise wipe lens_group_configs_v1 to "[]".
        if (!_bootDone) return
        // `Now` is read-only project data with no slot in the six-group table.
        // Belt-and-braces alongside the disabled controls: anything that still
        // manages to fire a value change must not reach the global config.
        if (isNowSelected()) return
        // The manual-edit flag is user-controlled only (the lens-type switch above);
        // config values never flip it automatically.
        // simple-mode-ux-overhaul: write goes to global config unconditionally.
        // batchScope path removed — per-job overrides are now read-only data carried
        // from .gyroflow load, edited via the render-queue right-click menu.
        controller.set_lens_group_config(JSON.stringify(next))
        applySelectedGroupToMain()
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
        // See persistConfigs: `Now` has no slot to write into, and letting the
        // sentinel index through would append an out-of-range entry.
        if (isNowSelected()) return
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
    function showDeviceDisplayNotice(message: string): void {
        deviceDisplayNotice.text = message
        deviceDisplayNoticeTimer.restart()
    }
    function clearCurrentFocalLength(): void {
        // simple-mode-ux-overhaul: clearing focal length is always a global edit.
        // persistConfigs takes care of clearing per-job overrides on all jobs.
        updateCurrentConfig(config => {
            config.focal_length_mm = null
        })
    }
    onSelectedLensIndexChanged: {
        if (!syncing)
            refreshUiFromSelection()
    }
    onBatchScopeChanged: {
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
            if (typeof render_queue !== "undefined")
                render_queue.reload_manual_camera_catalog()
            root.loadManualCameraState()
            root.refreshUiFromSelection()
        }
        // Fires when a project is imported, when a new video clears it, and after a
        // successful save (which re-derives it from what was written to disk).
        function onProject_lens_changed(): void {
            root.loadProjectLens()
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
        function onManual_camera_changed(): void {
            root.loadManualCameraState()
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
        loadManualCameraState()
        // Before the two loads below: they call updateSelection(), which needs to
        // know whether a `Now` entry exists.
        loadProjectLens()
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

                    // Global lens-type switch for all 6 lens groups. Persists to
                    // settings.json via controller.lens_group_manual_edit. Both options
                    // are always visible (vertical switch), the knob points at the
                    // active one. When off, the editing fields below are hidden and
                    // calibration follows telemetry (auto) for every group; entered
                    // values are kept, not cleared. When on, a group's focal length /
                    // anamorphic decision follows should_use_manual_config in Rust:
                    // missing focal can be filled manually, and anamorphic can override
                    // when enabled. The flag is user-controlled only — editing config
                    // values never flips it.
                    Switch {
                        id: manualEditSwitch
                        width: parent.width
                        textOff: qsTr("Auto-focus lens")
                        textOn: qsTr("Manual-focus or anamorphic lens")
                        tooltip: qsTr("Turn on for manual-focus or anamorphic lenses: manually-entered focal length (> 5mm) applies when the video has no telemetry focal length, and anamorphic settings apply when enabled. Turning it off hides the fields but keeps the entered values.")
                        checked: controller.lens_group_manual_edit
                        onCheckedChanged: {
                            if (checked === controller.lens_group_manual_edit) return
                            controller.lens_group_manual_edit = checked
                            // isNowSelected guard: preview_lens_group_config takes a
                            // usize, so the sentinel must never reach it. (`Now` is
                            // hidden in batchScope, but the selection can lag a frame
                            // behind the scope change.)
                            if (root.batchScope && !root.isNowSelected() && root.selectedJobIds().length === 1)
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
                id: manualCameraColumn
                width: parent.width
                spacing: 10 * dpiScale
                visible: !!root.manualCameraState.eligible

                Label {
                    position: Label.LeftPosition
                    text: qsTr("Camera brand")
                    width: parent.width

                    ComboBox {
                        id: manualCameraBrandCombo
                        objectName: "manual-camera-brand"
                        width: parent.width
                        textRole: "label"
                        model: root.manualCameraBrandOptions()
                        currentIndex: root.manualCameraOptionIndex(model, root.manualCameraState.brand || "")
                        enabled: model.length > 0
                        opacity: enabled ? 1.0 : 0.5
                        onActivated: {
                            const option = model[currentIndex]
                            if (!option || !option.enabled || option.value === root.manualCameraState.brand)
                                return
                            render_queue.set_manual_camera_selection(option.value, "")
                        }
                    }
                }

                Label {
                    position: Label.LeftPosition
                    text: qsTr("Camera model")
                    width: parent.width

                    ComboBox {
                        id: manualCameraModelCombo
                        objectName: "manual-camera-model"
                        width: parent.width
                        textRole: "label"
                        model: root.manualCameraModelOptions()
                        currentIndex: root.manualCameraOptionIndex(model, root.manualCameraState.model || "")
                        enabled: (root.manualCameraState.brand || "").length > 0 && model.length > 0
                        opacity: enabled ? 1.0 : 0.5
                        onActivated: {
                            const option = model[currentIndex]
                            if (!option || !option.enabled || option.value === root.manualCameraState.model)
                                return
                            render_queue.set_manual_camera_selection(root.manualCameraState.brand, option.value)
                        }
                    }
                }
            }

            // Lens group selector follows
            // Deliberately OUTSIDE editorColumn: the dropdown is what tells the user
            // which group is in use and what lens the loaded project carries, so it
            // must not be folded away by the manual-edit switch. Only the editing
            // fields below collapse.
            Label {
                position: Label.LeftPosition
                text: qsTr("Lens group")
                width: parent.width

                ComboBox {
                    id: lensGroupCombo
                    width: parent.width
                    textRole: "label"
                    model: root.lensGroupOptions()
                    // Disabled rather than hidden in auto mode: switching groups has
                    // essentially no effect on the picture there (should_use_manual_config
                    // is false), so an enabled control would just look broken. The
                    // labels — including `Now` — stay readable.
                    enabled: controller.lens_group_manual_edit
                    opacity: enabled ? 1.0 : 0.5
                    currentIndex: Math.max(0, Math.min(root.lensIndexToComboIndex(root.selectedLensIndex), model.length - 1))
                    onActivated: {
                        if (!root.syncing) {
                            // Lock auto-selection — the user's pick is now sticky.
                            root.userPickedLens = true
                            root.selectedLensIndex = root.comboIndexToLensIndex(currentIndex)
                            // Switching must take effect on the main preview immediately —
                            // without this the canvas only updates when a value is edited
                            // (persistConfigs). Main preview only: this path deliberately
                            // does NOT write the global config nor clear/reapply per-job
                            // lens groups on queued jobs. `Now` routes to the project-lens
                            // restore inside applySelectedGroupToMain.
                            root.applySelectedGroupToMain()
                        }
                    }
                }
            }

            Column {
                id: editorColumn
                width: parent.width
                spacing: 10 * dpiScale
                // Expand/collapse follows the lens-type switch above. Same animation
                // pattern as AdvancedSection: the height Ease is only enabled around
                // the toggle so content-driven implicitHeight changes (e.g. the
                // anamorphic sub-section) don't animate.
                property bool opened: controller.lens_group_manual_edit
                visible: opacity > 0
                opacity: opened ? 1 : 0
                height: opened ? implicitHeight : -10 * dpiScale
                Ease on opacity { }
                Ease on height { id: editorHeightAnim; enabled: false; }
                onOpenedChanged: {
                    editorHeightAnim.enabled = true
                    editorAnimTimer.start()
                }
                Timer {
                    id: editorAnimTimer
                    interval: 700
                    onTriggered: editorHeightAnim.enabled = false
                }

                // simple-mode-ux-overhaul: batchScope notice removed — edits are always
                // global now. Per-job overrides come from .gyroflow load only; users
                // adjust them via the render-queue right-click "Change lens group" menu.

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
                        // `Now` is project data being reported back, not an input.
                        enabled: !root.isNowSelected()
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
                    // Except for `Now`, which is read-only project data.
                    cb.enabled: !root.isNowSelected()
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
                            enabled: !root.isNowSelected()
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
                            enabled: !root.isNowSelected()
                            onCheckedChanged: {
                                if (root.syncing || !checked) return
                                root.updateCurrentConfig(config => config.squeeze_direction = "horizontal")
                            }
                        }

                        RadioButton {
                            id: verticalDirection
                            width: (parent.width - parent.spacing) / 2
                            text: qsTr("Vertical")
                            enabled: !root.isNowSelected()
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
                            readOnly: root.isNowSelected() || !!root.currentConfig().preset_id
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
                            enabled: !root.isNowSelected()
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

                // device-language-and-lens-display: write/clear the focal length
                // display on the connected NiYien device (slots 0..5 = L1..L6).
                // Fire-and-forget — the notice only says "sent", never "confirmed".
                Column {
                    width: parent.width
                    spacing: 8 * dpiScale
                    visible: controller.device_connected

                    Row {
                        width: parent.width
                        spacing: 8 * dpiScale

                        Button {
                            width: (parent.width - parent.spacing) / 2
                            accent: true
                            text: qsTr("Display on Device")
                            enabled: controller.ota_state !== "updating"
                            onClicked: {
                                controller.send_device_lens_display()
                                root.showDeviceDisplayNotice(qsTr("Sent to the device."))
                            }
                        }

                        Button {
                            width: (parent.width - parent.spacing) / 2
                            text: qsTr("Clear Device Display")
                            enabled: controller.ota_state !== "updating"
                            onClicked: {
                                controller.clear_device_lens_display()
                                root.showDeviceDisplayNotice(qsTr("Sent to the device."))
                            }
                        }
                    }

                    BasicText {
                        id: deviceDisplayNotice
                        width: parent.width
                        leftPadding: 0
                        visible: text.length > 0
                        color: root.mutedTextColor
                        font.pixelSize: 11 * dpiScale
                        font.bold: true
                        wrapMode: Text.WordWrap
                    }

                    Timer {
                        id: deviceDisplayNoticeTimer
                        interval: 4000
                        repeat: false
                        onTriggered: deviceDisplayNotice.text = ""
                    }
                }
            }
        }
    }
}
