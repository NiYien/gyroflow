// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

pragma ComponentBehavior: Bound

import QtQuick

import "../components/"

MenuItem {
    id: root;
    text: qsTr("Mounting position");
    iconName: "axes";
    objectName: "simple-mounting";
    opened: false;
    btnHeight: 28 * dpiScale;

    property string currentMode: ""
    property bool initialized: false

    // True while the effective angles were borrowed from a loaded .gyroflow
    // project instead of coming from the home value stored in settings. The
    // mounting angle is a device property: settings holds the one value the
    // user owns ("home"), and a project's rotation is only a temporary borrow
    // for the main preview — never written to disk, never pushed to the queue.
    property bool borrowedFromProject: false

    // Programmatic-write guard. Set while angles are pushed into the three
    // NumberFields (settings restore, project adoption, home restore) so their
    // onValueChanged handlers and modeCombo.onCurrentIndexChanged bail out
    // instead of running applyMode() and reintegrating the whole IMU timeline,
    // and so those writes are not mistaken for a user edit.
    // This is the "don't run at all" guard. It is deliberately NOT the same
    // flag as a "run, but don't persist" one: the two have different meanings
    // and merging them would either skip needed core updates or persist values
    // that must stay off disk.
    property bool _programmaticWrite: false

    // Provenance of the mounting change currently being propagated, read by the
    // handlers a programmatic write can trigger indirectly. adoptProjectRotation()
    // deliberately writes modeCombo.currentIndex *outside* _programmaticWrite
    // (suppressing that handler would cut both apply paths at once and leave the
    // core on the pre-adoption rotation), so onCurrentIndexChanged does fire
    // while a project is being adopted and must not treat it as a user edit.
    // Resting value is "manual": anything that fires while this is unchanged
    // really is the user editing the panel.
    property string _activeSource: "manual"

    // Must stay in sync with UI_RESET_BROADCAST_KEY in src/controller.rs.
    readonly property string uiResetKey: "__niyien_ui_reset"

    // Roll sign follows the legacy NiYien Tool convention (lens_set.cpp):
    // device mounted on the camera's left side = +90° about the optical axis.
    readonly property var presetAngles: ({
        "top":    [0, 0, 0],
        "bottom": [0, 180, 0],
        "left":   [0, 90, 0],
        "right":  [0, -90, 0]
    })
    readonly property var modeKeys:   ["top", "bottom", "left", "right", "custom"]
    readonly property var modeLabels: [qsTr("Top"), qsTr("Bottom"), qsTr("Left"), qsTr("Right"), qsTr("Custom")]

    // The three NumberFields are the single source of truth for the custom
    // angles — there is no mirror property. A mirrored copy is exactly what
    // used to desync from the screen: NumberField.updateValue() assigns to
    // `value` imperatively, which permanently drops any binding feeding it,
    // and children are constructed before the parent's Component.onCompleted,
    // so the binding was always dead before restoreSettings() ever ran.
    function setCustomAngles(pitch: real, roll: real, yaw: real): void {
        const prev = root._programmaticWrite;
        root._programmaticWrite = true;
        try {
            pitchField.value = pitch;
            rollField.value  = roll;
            yawField.value   = yaw;
        } finally {
            root._programmaticWrite = prev;
        }
    }

    // Apply the currently selected mounting angles. `source` decides which side
    // effects run:
    //   "manual"  the user edited the combo box or a custom angle field
    //             -> core + queue + disk; the new value becomes the home value
    //   "home"    startup restore or the UI reset broadcast -> core + queue
    //             (already on disk, so nothing is written back)
    //   "project" adopting a .gyroflow project's rotation -> core only; it is a
    //             temporary borrow that must not reach the queue or the disk
    //   "reapply" re-pushing the effective value after MotionData cleared the
    //             transforms -> core only; broadcasting here would leak a
    //             borrowed project value into the queue
    function applyMode(source: string): void {
        if (!root.initialized) return;
        const src = source || "manual";
        const angles = root.currentMode === "custom"
            ? [pitchField.value, rollField.value, yawField.value]
            : root.presetAngles[root.currentMode];
        if (angles) {
            controller.set_imu_rotation(angles[0], angles[1], angles[2]);
            // Mounting is a global device property: a value the user owns
            // propagates to every queued job too, so jobs enqueued before this
            // change follow it (the enqueue-time snapshot only covers later
            // ones). No-op per job when the value is unchanged. The queue only
            // ever follows the home value, so borrowed/re-applied values stop
            // at the main preview.
            if (src === "manual" || src === "home") {
                render_queue.apply_mounting_rotation_to_all(angles[0], angles[1], angles[2]);
            }
        }
        Qt.callLater(controller.recompute_gyro);
        // Only a user edit rewrites the home value on disk.
        if (src === "manual") {
            root.borrowedFromProject = false;
            root.saveSettings();
        }
    }

    function saveSettings(): void {
        settings.setValue("mountingMode", root.currentMode);
        settings.setValue("mountingCustomPitch", pitchField.value.toString());
        settings.setValue("mountingCustomRoll", rollField.value.toString());
        settings.setValue("mountingCustomYaw", yawField.value.toString());
    }

    // `gyroflow_file_loaded` carries three kinds of payload that need entirely
    // different handling:
    //   * the UI reset broadcast controller.rs emits on every load_video. Its
    //     JSON is identical to that of a top-mounted project (core stores
    //     all-zero angles as None, which exports as null), so it carries an
    //     explicit marker key. It means "go back to the home value".
    //   * a parameter preset — it has no `gyro_source` key at all and must not
    //     touch the mounting angle in any way.
    //   * a real .gyroflow project — its rotation is adopted as a temporary
    //     borrow for the main preview.
    function loadGyroflow(obj: var): void {
        if (!obj) return;
        if (obj[root.uiResetKey]) { root.restoreHome(); return; }
        if (!obj.hasOwnProperty("gyro_source")) return;
        root.adoptProjectRotation(obj.gyro_source);
    }

    // Adopt the mounting rotation stored in a loaded .gyroflow project: the
    // project reflects the device orientation of that shooting session, so the
    // main preview follows it. The project is authoritative for the preview —
    // a null/absent/malformed rotation means "no mounting" and normalizes to
    // top (0,0,0) instead of keeping the previous state. The adopted value is
    // only borrowed: it is neither persisted nor propagated to the queue.
    function adoptProjectRotation(gyro: var): void {
        const raw = (gyro || {}).rotation;
        const rot = (raw && raw.length === 3) ? raw : [0, 0, 0];
        let mode = "custom";
        for (const key in root.presetAngles) {
            const a = root.presetAngles[key];
            if (a[0] === rot[0] && a[1] === rot[1] && a[2] === rot[2]) { mode = key; break; }
        }
        // Compare against the currently effective angles BEFORE mutating any
        // state: applyMode() reintegrates the whole IMU timeline, so it must
        // only run when the adopted angles actually change something.
        const cur = root.currentMode === "custom"
            ? [pitchField.value, rollField.value, yawField.value]
            : (root.presetAngles[root.currentMode] || [0, 0, 0]);
        const changed = cur[0] !== rot[0] || cur[1] !== rot[1] || cur[2] !== rot[2];
        const prevIdx = modeCombo.currentIndex;
        const prevSource = root._activeSource;
        root._activeSource = "project";
        try {
            if (mode === "custom") {
                // Guarded write: the explicit applyMode() below (or the combo's
                // index change) is what pushes the adopted angles to the core.
                root.setCustomAngles(rot[0], rot[1], rot[2]);
            }
            root.currentMode = mode;
            const idx = root.modeKeys.indexOf(mode);
            // Deliberately NOT wrapped in _programmaticWrite: suppressing
            // onCurrentIndexChanged here would also cut the explicit call
            // below, so adoption would never reach the core. The handler reads
            // _activeSource instead and therefore applies without persisting
            // or broadcasting.
            modeCombo.currentIndex = idx >= 0 ? idx : 0;
            // A standalone project import fires no telemetry_loaded re-apply, and
            // an unchanged combo index fires no onCurrentIndexChanged — apply
            // explicitly then so the stab doesn't keep the pre-adoption rotation.
            if (changed && modeCombo.currentIndex === prevIdx) root.applyMode("project");
        } finally {
            root._activeSource = prevSource;
        }
        root.borrowedFromProject = true;
    }

    // UI reset broadcast: the effective value may be a rotation borrowed from
    // the previously loaded project, so actively re-read the home value and
    // apply it. Merely "not clearing it" would leave the old project's rotation
    // on screen. Re-applying an unchanged value is a per-job no-op in the queue
    // and a plain field write on the main stab.
    function restoreHome(): void {
        root.restoreSettings();
        root.borrowedFromProject = false;
        root.applyMode("home");
    }

    function restoreSettings(): void {
        let mode = settings.value("mountingMode", "");
        if (!mode) {
            // Migration from old settings
            const oldPos = settings.value("mountingPosition", "top");
            const oldRot = parseInt(settings.value("mountingRotation", "0")) || 0;
            mode = (oldRot === 0 && root.presetAngles.hasOwnProperty(oldPos)) ? oldPos : "custom";
            settings.setValue("mountingMode", mode);
        }
        // Programmatic write: pushing the stored angles into the fields must
        // not look like a user edit, and must not drag the whole IMU timeline
        // through a reintegration per field. The caller applies afterwards.
        const prev = root._programmaticWrite;
        root._programmaticWrite = true;
        try {
            root.currentMode = mode;
            root.setCustomAngles(
                parseFloat(settings.value("mountingCustomPitch", "0")) || 0,
                parseFloat(settings.value("mountingCustomRoll", "0")) || 0,
                parseFloat(settings.value("mountingCustomYaw", "0")) || 0
            );

            // Sync ComboBox index
            const idx = root.modeKeys.indexOf(root.currentMode);
            modeCombo.currentIndex = idx >= 0 ? idx : 0;
        } finally {
            root._programmaticWrite = prev;
        }
    }

    Component.onCompleted: {
        root.restoreSettings();
        root.initialized = true;
        root.applyMode("home");
        // A real project loaded before this selector finished its async load
        // left its rotation parked — adopt it now (borrowed, so it overrides
        // the restored home value on screen but never on disk).
        if (window.pendingMountingRotation) {
            root.adoptProjectRotation({ rotation: window.pendingMountingRotation });
            window.pendingMountingRotation = null;
        }
    }

    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            // Re-apply mounting rotation after MotionData clears it. This only
            // re-pushes the already effective value to the core: it must not
            // reach the queue (a borrowed project value would leak into it) and
            // must not touch the persisted home value.
            Qt.callLater(root.applyMode, "reapply");
        }
    }

    // ── Mode selector ──
    ComboBox {
        id: modeCombo;
        model: root.modeLabels;
        font.pixelSize: 12 * dpiScale;
        width: parent.width;
        currentIndex: 0;
        onCurrentIndexChanged: {
            if (root._programmaticWrite) return;
            if (!root.initialized) return;
            root.currentMode = root.modeKeys[currentIndex];
            // "manual" unless a project adoption is driving this index write.
            root.applyMode(root._activeSource);
        }
    }

    // ── Custom rotation angles (visible only in custom mode) ──
    Flow {
        width: parent.width;
        spacing: 5 * dpiScale;
        visible: root.currentMode === "custom";

        Label {
            position: Label.LeftPosition;
            text: qsTr("Pitch");
            width: undefined;
            inner.width: 50 * dpiScale;
            spacing: 5 * dpiScale;
            NumberField {
                id: pitchField;
                unit: "°";
                precision: 1;
                from: -360;
                to: 360;
                width: 50 * dpiScale;
                // No `value:` binding on purpose — NumberField.updateValue()
                // assigns to `value` during its own construction, which would
                // drop the binding before restoreSettings() could ever push
                // through it. The field owns the value; writes go the other way.
                onValueChanged: {
                    if (root._programmaticWrite) return;
                    if (!root.initialized) return;
                    root.applyMode(root._activeSource);
                }
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Roll");
            width: undefined;
            inner.width: 50 * dpiScale;
            spacing: 5 * dpiScale;
            NumberField {
                id: rollField;
                unit: "°";
                precision: 1;
                from: -360;
                to: 360;
                width: 50 * dpiScale;
                // See pitchField: the field itself is the source of truth.
                onValueChanged: {
                    if (root._programmaticWrite) return;
                    if (!root.initialized) return;
                    root.applyMode(root._activeSource);
                }
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Yaw");
            width: undefined;
            inner.width: 50 * dpiScale;
            spacing: 5 * dpiScale;
            NumberField {
                id: yawField;
                unit: "°";
                precision: 1;
                from: -360;
                to: 360;
                width: 50 * dpiScale;
                // See pitchField: the field itself is the source of truth.
                onValueChanged: {
                    if (root._programmaticWrite) return;
                    if (!root.initialized) return;
                    root.applyMode(root._activeSource);
                }
            }
        }
    }
}
