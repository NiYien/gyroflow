// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Dialogs

import "../components/"

MenuItem {
    id: root;
    text: qsTr("Lens profile");
    iconName: "lens";
    objectName: "lens";

    property int calibWidth: 0;
    property int calibHeight: 0;

    property int videoWidth: 0;
    property int videoHeight: 0;

    property real input_horizontal_stretch: 1;
    property real input_vertical_stretch: 1;

    property real cropFactor: 0;

    property bool lensProfilesListPrepared: false;
    property var distortionCoeffs: [];
    property string profileName;
    property string profileOriginalJson;
    property string profileChecksum;
    // Tracks the currently loaded lens profile's distortion_model. Used to
    // gate the distortion strength slider so it only appears for poly5 — the
    // single model whose 2-coefficient forward equation matches the tan(r)
    // Taylor mapping the slider uses.
    property string distortionModel: "";

    property bool fetched_from_github: false;
    property bool selected_manually: false;

    // Unified reentrancy guard for any code path that programmatically updates
    // k1..k4 fields, D_mid / D_corner sliders, or the anchor input — so the
    // bidirectional bindings between D <-> k don't ping-pong on every keystroke.
    property bool _internalUpdate: false;

    // Twin-bend distortion controls (poly5 only).
    //   anchorR  in [0.4, 0.9]: which on-screen ring D_mid refers to (r=1.0 is fixed for D_corner)
    //   dMid     in fraction (e.g. -0.056 = -5.6%): bend ratio at r=anchorR
    //   dCorner  in fraction: bend ratio at r=1.0
    // Source of truth: dMid, dCorner, anchorR. Derived: k1, k2 = solve linear system.
    // See docs/superpowers/specs/2026-05-15-poly5-anchored-twin-bend-design.md.
    property real anchorR: 0.5;
    property real dMid: 0.0;
    property real dCorner: 0.0;

    // Derive k1, k2 from current state and push to controller + k1/k2 fields.
    // Math: k1 = (dMid - r_a^4 * dCorner) / det,  k2 = (r_a^2 * dCorner - dMid) / det,
    //       det = r_a^2 * (1 - r_a^2).
    function deriveK(): void {
        const ra2 = root.anchorR * root.anchorR;
        const ra4 = ra2 * ra2;
        const det = ra2 * (1.0 - ra2);
        const k1Val = (root.dMid - ra4 * root.dCorner) / det;
        const k2Val = (ra2 * root.dCorner - root.dMid) / det;
        root._internalUpdate = true;
        k1.setInitialValue(k1Val);
        k2.setInitialValue(k2Val);
        k3.setInitialValue(0.0);
        k4.setInitialValue(0.0);
        controller.set_distortion_coeffs(k1Val, k2Val, 0.0, 0.0);
        root._internalUpdate = false;
    }

    // External k1, k2 provided (e.g. from lens preset load or manual k field edit).
    // Recompute dMid / dCorner under current anchorR and push to sliders without
    // re-triggering deriveK. Guarded against being called before Task 2 wires up
    // the slider ids (typeof check returns "undefined" pre-Task-2).
    function deriveDFromK(): void {
        if (typeof distortionMidSlider === "undefined" || typeof distortionCornerSlider === "undefined") return;
        const ra2 = root.anchorR * root.anchorR;
        const ra4 = ra2 * ra2;
        root.dMid    = k1.value * ra2 + k2.value * ra4;
        root.dCorner = k1.value + k2.value;
        root._internalUpdate = true;
        distortionMidSlider.value    = root.dMid * 100.0;
        distortionCornerSlider.value = root.dCorner * 100.0;
        root._internalUpdate = false;
    }

    // Switch anchor position. Semantics B: D values stay, k recomputed.
    function setAnchor(newR: real): void {
        root.anchorR = newR;
        if (typeof anchorInput !== "undefined") anchorInput.text = newR.toFixed(2);
        deriveK();
    }

    // One-button reset for the entire distortion block.
    function resetDistortion(): void {
        root._internalUpdate = true;
        root.dMid = 0.0;
        root.dCorner = 0.0;
        root.anchorR = 0.5;
        if (typeof anchorInput !== "undefined") anchorInput.text = "0.50";
        if (typeof distortionMidSlider !== "undefined") distortionMidSlider.value = 0;
        if (typeof distortionCornerSlider !== "undefined") distortionCornerSlider.value = 0;
        k1.setInitialValue(0.0);
        k2.setInitialValue(0.0);
        k3.setInitialValue(0.0);
        k4.setInitialValue(0.0);
        controller.set_distortion_coeffs(0.0, 0.0, 0.0, 0.0);
        root._internalUpdate = false;
    }

    FileDialog {
        id: fileDialog;
        property var extensions: ["json"];

        title: qsTr("Choose a lens profile")
        nameFilters: [qsTr("Lens profiles") + " (*.json" + (Qt.platform.os == "ios"? " *.txt" : "") + ")"];
        type: "lens";
        onAccepted: loadFile(fileDialog.selectedFile);
    }
    function loadFile(url: url): void {
        root.selected_manually = true;
        controller.load_lens_profile(url.toString());
    }

    function loadGyroflow(obj: var): void {
        if (typeof obj.light_refraction_coefficient !== "undefined") {
            isUnderwater.checked = Math.round(+obj.light_refraction_coefficient * 1000) == 1330;
        }
    }

    Component.onCompleted: {
        QT_TRANSLATE_NOOP("TableList", "Camera");
        QT_TRANSLATE_NOOP("TableList", "Lens");
        QT_TRANSLATE_NOOP("TableList", "Setting");
        QT_TRANSLATE_NOOP("TableList", "Additional info");
        QT_TRANSLATE_NOOP("TableList", "Dimensions");
        QT_TRANSLATE_NOOP("TableList", "Calibrated by");
        QT_TRANSLATE_NOOP("TableList", "Focal length");
        QT_TRANSLATE_NOOP("TableList", "Crop factor");
        QT_TRANSLATE_NOOP("TableList", "Asymmetrical");
        QT_TRANSLATE_NOOP("TableList", "Distortion model");
        QT_TRANSLATE_NOOP("TableList", "Digital lens");
    }
    Timer {
        id: profilesUpdateTimer;
        interval: 1000;
        property bool fromDisk: true;
        onTriggered: controller.load_profiles(fromDisk);
    }
    Connections {
        target: controller;
        function onAll_profiles_loaded(): void {
            if (!lensProfilesListPrepared) { // If it's the first load
                controller.request_profile_ratings();
            }

            lensProfilesListPrepared = true;

            root.loadFavorites();
            if (!root.fetched_from_github) {
                root.fetched_from_github = true;
                controller.fetch_profiles_from_github();
            }
        }
        function onLens_profiles_updated(fromDisk: bool): void {
            profilesUpdateTimer.fromDisk = fromDisk;
            profilesUpdateTimer.start();
        }
        function onLens_profile_loaded(json_str: string, filepath: string, checksum: string): void {
            if (json_str) {
                const obj = JSON.parse(json_str);
                if (obj) {
                    let lensInfo = {
                        "Camera":          obj.camera_brand + " " + obj.camera_model,
                        "Lens":            obj.lens_model,
                        "Setting":         obj.camera_setting,
                        "Additional info": obj.note,
                        "Dimensions":      obj.calib_dimension.w + "x" + obj.calib_dimension.h,
                        "Calibrated by":   obj.calibrated_by
                    };

                    if (+obj.focal_length > 0) lensInfo["Focal length"] = obj.focal_length.toFixed(2) + " mm";
                    if (+obj.crop_factor  > 0) lensInfo["Crop factor"]  = obj.crop_factor.toFixed(2) + "x";
                    if (obj.asymmetrical) lensInfo["Asymmetrical"] = qsTr("Yes");
                    root.distortionModel = obj.distortion_model || "opencv_fisheye";
                    if (obj.distortion_model && obj.distortion_model != "opencv_fisheye") lensInfo["Distortion model"] = obj.distortion_model;
                    if (obj.digital_lens) lensInfo["Digital lens"] = obj.digital_lens;

                    info.model = lensInfo;

                    root.cropFactor = +obj.crop_factor;

                    if (!root.selected_manually &&
                           (obj.calibrated_by == "Eddy" ||
                            obj.calibrated_by == "GoPro" ||
                            obj.calibrated_by == "DJI" ||
                            obj.calibrated_by == "Xtra" ||
                            obj.calibrated_by == "Insta360" ||
                            obj.calibrated_by == "Canon" ||
                            obj.calibrated_by == "Sony")) {
                        root.opened = false;
                        window.motionData.opened = false;
                    }

                    officialInfo.show = !obj.official && !settings.value("rated-profile-" + checksum, false);
                    officialInfo.canRate = true;
                    officialInfo.thankYou = false;
                    root.profileName = (filepath || obj.name || "").replace(/^.*?[\/\\]([^\/\\]+?)$/, "$1");
                    root.profileOriginalJson = json_str;
                    root.profileChecksum = checksum;

                    const hasOutputDimension = obj.output_dimension && obj.output_dimension.w > 0 && obj.output_dimension.h > 0;
                    if (window.exportSettings) {
                        if (hasOutputDimension && (window.exportSettings.lensProfileOutputSizeActive ||
                                window.exportSettings.outWidth != obj.output_dimension.w ||
                                window.exportSettings.outHeight != obj.output_dimension.h)) {
                            Qt.callLater(window.exportSettings.lensProfileOutputDimensionLoaded, obj.output_dimension.w, obj.output_dimension.h);
                        } else if (!hasOutputDimension) {
                            Qt.callLater(window.exportSettings.lensProfileOutputDimensionCleared);
                        }
                    }
                    if (+obj.frame_readout_time && Math.abs(+obj.frame_readout_time) > 0) {
                        window.stab.setFrameReadoutTime(obj.frame_readout_time, obj.frame_readout_direction);
                    }
                    if (+obj.gyro_lpf && Math.abs(+obj.gyro_lpf) > 0) {
                        window.motionData.setGyroLpf(obj.gyro_lpf);
                    }
                    if (obj.sync_settings && Object.keys(obj.sync_settings).length > 0) {
                        window.sync.loadGyroflow({
                            synchronization: obj.sync_settings
                        });
                    }

                    root.input_horizontal_stretch = obj.input_horizontal_stretch > 0.01? obj.input_horizontal_stretch : 1.0;
                    root.input_vertical_stretch   = obj.input_vertical_stretch   > 0.01? obj.input_vertical_stretch   : 1.0;

                    root.calibWidth  = obj.calib_dimension.w / root.input_horizontal_stretch;
                    root.calibHeight = obj.calib_dimension.h / root.input_vertical_stretch;
                    const coeffs = obj.fisheye_params.distortion_coeffs;
                    root.distortionCoeffs = coeffs;
                    const mtrx = obj.fisheye_params.camera_matrix;
                    // Populate k1..k4 from lens preset; D_mid / D_corner sliders
                    // will be synced after Task 2 wires up deriveDFromK().
                    root._internalUpdate = true;
                    distortionStrength.value = 0;
                    k1.setInitialValue(coeffs[0] || 0.0);
                    k2.setInitialValue(coeffs[1] || 0.0);
                    k3.setInitialValue(coeffs[2] || 0.0);
                    k4.setInitialValue(coeffs[3] || 0.0);
                    root._internalUpdate = false;
                    fx.setInitialValue(mtrx[0][0]);
                    fy.setInitialValue(mtrx[1][1]);
                    cx.setInitialValue(mtrx[0][2]);
                    cy.setInitialValue(mtrx[1][2]);

                    // Set asymmetrical lens center bias
                    /*if (obj.asymmetrical) {
                        console.log(-((mtrx[0][2] / (obj.calib_dimension.w / 2.0)) - 1.0));
                        console.log(-((mtrx[1][2] / (obj.calib_dimension.h / 2.0)) - 1.0));
                    }*/
                    // If focal length in pixels is large, it's more likely that Almeida pose estimator will yield better results
                    if (mtrx[0][0] > 10000) {
                        window.sync.poseMethod.currentIndex = 1; // Almeida
                    }
                }
                Qt.callLater(controller.recompute_threaded);
            }
        }
    }

    property int currentVideoAspectRatio: Math.round((root.videoWidth / Math.max(1, root.videoHeight)) * 1000);
    property int currentVideoAspectRatioSwapped: Math.round((root.videoHeight / Math.max(1, root.videoWidth)) * 1000);

    property var favorites: ({});
    function loadFavorites(): void {
        const list = settings.value("lensProfileFavorites", "");
        let fav = {};
        for (const x of list.split(",")) {
            if (x)
                fav[x] = 1;
        }
        favorites = fav;
    }
    function updateFavorites(): void {
        settings.setValue("lensProfileFavorites", Object.keys(favorites).filter(v => v).join(","));
    }

    Row {
        anchors.horizontalCenter: parent.horizontalCenter;
        spacing: 10 * dpiScale;
        Button {
            text: qsTr("Open file");
            iconName: "file-empty"
            onClicked: fileDialog.open2();
        }
        Button {
            text: qsTr("Create new");
            iconName: "plus";
            icon.width: 15 * dpiScale;
            icon.height: 15 * dpiScale;
            property var calibratorWnd: null;
            onClicked: {
                if (!calibratorWnd) {
                    ui_tools.init_calibrator();
                    calibratorWnd = Qt.createComponent("../Calibrator.qml").createObject(main_window)
                    calibratorWnd.show();
                    calibratorWnd.closing.connect(function(e) {
                        calibratorWnd.destroy();
                        calibratorWnd = null;
                    })
                }
            }
        }
    }

    InfoMessageSmall {
        id: officialInfo;
        type: InfoMessage.Warning;
        show: false;
        property bool canRate: true;
        property bool thankYou: false;
        text: qsTr("This lens profile is unofficial, we can't guarantee its correctness. Use at your own risk.") + (canRate? "<br>" +
              qsTr("Rate this profile: [Good] | [Bad]")
              .replace(/\[(.*?)\]/, "<a href=\"#good\">$1</a>")
              .replace(/\[(.*?)\]/, "<a href=\"#bad\">$1</a>") : (thankYou? "<br>" + qsTr("Thank you for rating this profile.") : ""));

        MouseArea {
            anchors.fill: parent;
            cursorShape: parent.t.hoveredLink? Qt.PointingHandCursor : Qt.ArrowCursor;
            acceptedButtons: Qt.NoButton;
        }
        Connections {
            target: officialInfo.t;
            function onLinkActivated(link: url): void {
                controller.rate_profile(root.profileName, root.profileOriginalJson, root.profileChecksum, link === "#good");
                if (link === "#good")
                    settings.setValue("rated-profile-" + root.profileChecksum, true);
                officialInfo.thankYou = true;
                officialInfo.canRate = false;
                tyTimer.start();
            }
        }
        Timer {
            id: tyTimer;
            interval: 5000;
            onTriggered: officialInfo.thankYou = false;
        }
    }

    InfoMessageSmall {
        type: lensRatio != videoRatio? InfoMessage.Error : InfoMessage.Warning;
        show: root.calibWidth > 0 && root.videoWidth > 0 && (root.calibWidth != root.videoWidth || root.calibHeight != root.videoHeight);
        property string lensRatio: (root.calibWidth / Math.max(1, root.calibHeight)).toFixed(3);
        property string videoRatio: (root.videoWidth / Math.max(1, root.videoHeight)).toFixed(3);
        text: lensRatio != videoRatio? qsTr("Lens profile aspect ratio doesn't match the file aspect ratio. The result will not look correct.") :
                                       qsTr("Lens profile dimensions don't match the file dimensions. The result may not look correct.");
    }

    TableList {
        id: info;
        copyable: true;
        model: ({ })
    }

    property bool hasLensParams: false;
    property bool hasFocalLength: false;
    property bool hasUnitPixelFocalLength: false;
    property bool hasBuiltinProfile: false;
    property bool hasAutoLens: hasLensParams || hasBuiltinProfile;

    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            if (is_main_video) {
                root.hasLensParams  = !!additional_data.has_lens_params;
                root.hasFocalLength = !!additional_data.has_focal_length;
                root.hasUnitPixelFocalLength = additional_data.hasOwnProperty("unit_pixel_focal_length");
                root.hasBuiltinProfile = !!additional_data.has_builtin_profile;
                userFocalLength.value = 0;
            }
        }
    }

    Label {
        id: focalLengthLabel;
        text: qsTr("Focal length (mm)");
        visible: root.hasUnitPixelFocalLength && !root.hasFocalLength && root.distortionCoeffs.length < 4;

        NumberField {
            id: userFocalLength;
            width: parent.width;
            precision: 2;
            value: 0;
            from: 0;
            unit: "mm";
            tooltip: qsTr("Enter focal length for manual lenses or lenses without electronic contacts");
            onValueChanged: {
                if (value > 0) {
                    controller.set_user_focal_length(value);
                }
            }
        }
    }

    AdvancedSection {
        btn.text: qsTr("Advanced");
        visible: Object.keys(info.model).length > 0

        CheckBox {
            id: isUnderwater;
            text: qsTr("Lens is under water");
            checked: false;
            tooltip: qsTr("Enable if you're filming under water. This will adjust the refraction coefficient.");
            property bool keyframesEnabled: false;

            onCheckedChanged: {
                controller.light_refraction_coefficient = checked? 1.33 : 1.0;
                if (keyframesEnabled) {
                    controller.set_keyframe("LightRefractionCoeff", window.videoArea.timeline.getTimestampUs(), checked? 1.33 : 1.0);
                }
            }
            ContextMenuMouseArea {
                cursorShape: Qt.ibeam;
                underlyingItem: isUnderwater;
                onContextMenu: (isHold, x, y) => menuLoader.popup(isUnderwater, x, y);
            }

            Component {
                id: isUnderwaterMenu;
                Menu {
                    font.pixelSize: 11.5 * dpiScale;
                    Action {
                        iconName: "keyframe";
                        text: qsTr("Enable keyframing");
                        checked: isUnderwater.keyframesEnabled;
                        onTriggered: {
                            checked = !checked;
                            isUnderwater.keyframesEnabled = checked;
                            if (!checked) {
                                controller.clear_keyframes_type("LightRefractionCoeff");
                            }
                        }
                    }
                    Action {
                        iconName: "plus";
                        enabled: isUnderwater.keyframesEnabled;
                        text: qsTr("Add keyframe");
                        onTriggered: controller.set_keyframe("LightRefractionCoeff", window.videoArea.timeline.getTimestampUs(), isUnderwater.checked? 1.33 : 1.0);
                    }
                }
            }
            ContextMenuLoader {
                id: menuLoader;
                sourceComponent: isUnderwaterMenu
            }
        }

        component SmallNumberField: NumberField {
            property bool preventChange2: true;
            // When true, a manual edit of this field will silently reset the
            // distortion strength slider to 0 (used for k1..k4 only).
            property bool isDistortionCoeff: false;
            width: parent.width / 2;
            precision: 12;
            property string param: "  ";
            tooltip: param[0] + "<font size=\"1\">" + param[1] + "</font>"
            font.pixelSize: 11 * dpiScale;
            onValueChanged: {
                if (!preventChange2) {
                    controller.set_lens_param(param, value);
                    if (isDistortionCoeff && !root._internalUpdate) {
                        // User typed a new k1 / k2 by hand: recompute D_mid /
                        // D_corner under current anchorR so the twin sliders
                        // stay in sync. deriveDFromK is wired up after Task 2.
                        if (root.distortionModel === "poly5" && typeof root.deriveDFromK === "function") {
                            root.deriveDFromK();
                        }
                    }
                }
            }
            function setInitialValue(v: real): void {
                preventChange2 = true;
                value = v;
                preventChange2 = false;
            }
        }

        Label {
            text: qsTr("Pixel focal length");

            Row {
                spacing: 4 * dpiScale;
                width: parent.width;
                SmallNumberField { id: fx; param: "fx"; }
                SmallNumberField { id: fy; param: "fy"; }
            }
        }
        Label {
            text: qsTr("Focal center");

            Row {
                spacing: 4 * dpiScale;
                width: parent.width;
                SmallNumberField { id: cx; param: "cx"; }
                SmallNumberField { id: cy; param: "cy"; }
            }
        }
        Label {
            text: qsTr("Distortion coefficients");

            Column {
                spacing: 4 * dpiScale;
                width: parent.width;

                // Single-parameter distortion strength control. Drags the four
                // k coefficients along the tan(theta)/theta Taylor curve so the
                // user can dial in barrel / pincushion correction with one slider
                // instead of typing four polynomial coefficients by hand.
                // Only shown when the loaded lens uses the poly5 distortion model;
                // other models map k1..k4 differently and the slider's Taylor
                // mapping would not produce the intended visual effect.
                Row {
                    spacing: 6 * dpiScale;
                    width: parent.width;
                    visible: root.distortionModel === "poly5";
                    height: visible ? implicitHeight : 0;

                    SliderWithField {
                        id: distortionStrength;
                        width: parent.width - resetDistortionBtn.width - 6 * dpiScale;
                        from: -100;
                        to: 100;
                        precision: 1;
                        defaultValue: 0;
                        value: 0;
                        unit: "";
                        // Subdivide each integer step into 10 sub-steps so the
                        // perceptually-sensitive [0, 1] region of the slider can
                        // be dialled in finely instead of jumping a full unit per
                        // mouse pixel.
                        Component.onCompleted: distortionStrength.slider.stepSize = 0.1;
                        onValueChanged: {
                            if (root._internalUpdate) return;
                            // Disabled in Task 1: function removed, slider replaced in Task 2.
                            // root.applyDistortionSliderValue(value);
                        }
                        HoverHandler { id: distortionHover; }
                        ToolTip {
                            visible: !isMobile && distortionHover.hovered && !distortionStrength.slider.pressed;
                            delay: 400;
                            text: qsTr("Drag to overwrite k1..k4 via tan(θ)/θ. Manually editing any k silently resets this slider to 0.");
                        }
                    }
                    LinkButton {
                        id: resetDistortionBtn;
                        textColor: styleTextColor;
                        iconName: "undo";
                        leftPadding: 6 * dpiScale;
                        rightPadding: 6 * dpiScale;
                        topPadding: 6 * dpiScale;
                        bottomPadding: 6 * dpiScale;
                        anchors.verticalCenter: parent.verticalCenter;
                        tooltip: qsTr("Drag the slider to overwrite k1..k4 via tan(θ)/θ. Manually editing a k silently resets the slider to 0. Click this button to clear slider and k1..k4 to 0.");
                        onClicked: {
                            root._internalUpdate = true;
                            distortionStrength.value = 0;
                            k1.setInitialValue(0.0);
                            k2.setInitialValue(0.0);
                            k3.setInitialValue(0.0);
                            k4.setInitialValue(0.0);
                            controller.set_distortion_coeffs(0.0, 0.0, 0.0, 0.0);
                            root._internalUpdate = false;
                        }
                    }
                }

                Row {
                    spacing: 4 * dpiScale;
                    width: parent.width;
                    SmallNumberField { id: k1; param: "k1"; precision: 16; isDistortionCoeff: true; }
                    SmallNumberField { id: k2; param: "k2"; precision: 16; isDistortionCoeff: true; }
                }
                Row {
                    spacing: 4 * dpiScale;
                    width: parent.width;
                    SmallNumberField { id: k3; param: "k3"; precision: 16; isDistortionCoeff: true; }
                    SmallNumberField { id: k4; param: "k4"; precision: 16; isDistortionCoeff: true; }
                }
            }
        }
        LinkButton {
            anchors.horizontalCenter: parent.horizontalCenter;
            text: qsTr("Export STMap");
            OutputPathField { id: opf; visible: false; }
            enabled: window.videoArea.vid.loaded;
            onClicked: {
                opf.selectFolder("", function(folder_url) {
                    if (controller.has_per_frame_lens_data()) {
                        messageBox(Modal.Question, qsTr("This file contains per-frame lens metadata. Do you want to export an STMap sequence or a single frame?"), [
                            { text: qsTr("Single frame"), accent: true, clicked: () => { controller.export_stmap(folder_url, false); } },
                            { text: qsTr("STMap sequence"), clicked: () => { controller.export_stmap(folder_url, true); } },
                        ]);
                    } else {
                        controller.export_stmap(folder_url, false);
                    }
                });
            }

            Connections {
                target: controller;
                function onStmap_progress(progress: real, ready: int, total: int): void {
                    window.videoArea.videoLoader.active = progress < 1;
                    window.videoArea.videoLoader.currentFrame = ready;
                    window.videoArea.videoLoader.totalFrames = total;
                    window.videoArea.videoLoader.text = progress < 1? qsTr("Exporting %1...") : "";
                    window.videoArea.videoLoader.progress = progress < 1? progress : -1;
                    window.videoArea.videoLoader.cancelable = true;
                }
            }
        }
    }

    DropTarget {
        parent: root.innerItem;
        color: styleBackground2;
        z: 999;
        anchors.rightMargin: -28 * dpiScale;
        anchors.topMargin: 35 * dpiScale;
        anchors.bottomMargin: -35 * dpiScale;
        extensions: fileDialog.extensions;
        onLoadFile: (url) => root.loadFile(url);
    }

    // -------------------------------------------------------------------
    // ---------------------- Maintenance functions ----------------------
    // -------------------------------------------------------------------
    /*
    property int fileno: 0;
    property var files: [
        ... // dir /b | clip
    ];
    Shortcut {
        sequences: ["F8"];
        onActivated: {
            root.fileno = Math.abs(++fileno % files.length);
            console.log(root.fileno);
            controller.load_lens_profile("file:///d:/lens_review/" + root.files[root.fileno]);
        }
    }
    Shortcut {
        sequences: ["F7"];
        onActivated: {
            root.fileno = Math.abs(--fileno % files.length);
            console.log(root.fileno);
            controller.load_lens_profile("file:///d:/lens_review/" + root.files[root.fileno]);
        }
    }
    Shortcut {
        sequences: ["Delete"];
        onActivated: {
            console.log("deleting " + root.files[root.fileno]);
            filesystem.move_to_trash("file:///d:/lens_review/" + root.files[root.fileno]);
        }
    }
    */
}
