// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick

import "../"
import "../components/"

MenuItem {
    id: root;
    text: qsTr("Video information");
    iconName: "info";
    objectName: "info";

    property real videoRotation: 0;
    property real metadataRotation: 0;
    property real fps: 0;
    property real org_fps: 0;
    property string filename: "";
    property bool isCalibrator: false;
    property string pixelFormat: "";
    property bool hasAccessToInputDirectory: true;
    property alias infoList: list;
    property var orgModel: [];
    property bool hasTelemetryTime: false;

    Component.onCompleted: {
        QT_TRANSLATE_NOOP("TableList", "Created at");
        const fields = [
            QT_TRANSLATE_NOOP("TableList", "File name"),
            QT_TRANSLATE_NOOP("TableList", "Detected camera"),
            QT_TRANSLATE_NOOP("TableList", "Detected lens"),
            QT_TRANSLATE_NOOP("TableList", "Dimensions"),
            QT_TRANSLATE_NOOP("TableList", "Duration"),
            QT_TRANSLATE_NOOP("TableList", "Frame rate"),
            QT_TRANSLATE_NOOP("TableList", "Codec"),
            QT_TRANSLATE_NOOP("TableList", "Pixel format"),
            QT_TRANSLATE_NOOP("TableList", "Audio"),
            QT_TRANSLATE_NOOP("TableList", "Rotation"),
            QT_TRANSLATE_NOOP("TableList", "Contains gyro"),
        ];
        let model = {};
        for (const x of fields) model[x] = "---";
        list.model = model;

        orgModel = JSON.parse(JSON.stringify(model));
        orgModel["Created at"] = "---";

        QT_TRANSLATE_NOOP("TableList", "Shutter angle");
        QT_TRANSLATE_NOOP("TableList", "Shutter speed");
        QT_TRANSLATE_NOOP("TableList", "Exposure");
        QT_TRANSLATE_NOOP("TableList", "ISO");
        QT_TRANSLATE_NOOP("TableList", "Color primaries");
        QT_TRANSLATE_NOOP("TableList", "Gamma equation");
        QT_TRANSLATE_NOOP("TableList", "White balance mode");
        QT_TRANSLATE_NOOP("TableList", "White balance");
        QT_TRANSLATE_NOOP("TableList", "Iris");
        QT_TRANSLATE_NOOP("TableList", "Focal length");
        QT_TRANSLATE_NOOP("TableList", "Focus mode");
    }

    function cleanupModel() {
        let model = list.model;
        for (const x in model) {
            if (!orgModel[x])
                delete model[x];
        }
        list.model = model;
        list.modelChanged();
    }

    signal selectFileRequest();

    function loadFromVideoMetadata(md: var, org_w: int, org_h: int): void {
        const framerate = +md["stream.video[0].codec.frame_rate"] || 0;
        const w = org_w || md["stream.video[0].codec.width"] || 0;
        const h = org_h || md["stream.video[0].codec.height"] || 0;
        const bitrate = +md["stream.video[0].codec.bit_rate"]? ((+md["stream.video[0].codec.bit_rate"] / 1024 / 1024)) : 200;

        if (window) {
            window.lensProfile.videoWidth   = w;
            window.lensProfile.videoHeight  = h;
        }
        if (typeof calibrator_window !== "undefined") {
            calibrator_window.lensCalib.setVideoSize(w, h);
            calibrator_window.lensCalib.fps = framerate;
        }

        root.pixelFormat = getPixelFormat(md) || "---";

        root.metadataRotation = (360 - (md["stream.video[0].rotation"] || 0)) % 360; // Constrain to 0-360
        root.videoRotation = root.metadataRotation;

        list.model["Dimensions"]   = w && h? w + "x" + h : "---";
        list.model["Duration"]     = getDuration(md) || "---";
        list.model["Frame rate"]   = framerate? framerate.toFixed(3) + " fps" : "---";
        list.model["Codec"]        = getCodec(md) || "---";
        list.model["Pixel format"] = root.pixelFormat;
        list.model["Rotation"]     = (root.videoRotation) + " °";
        list.model["Audio"]        = getAudio(md) || "---";
        root.hasTelemetryTime = false;
        if (md["metadata.creation_time"]) {
            const created_at = (new Date(Date.parse(md["metadata.creation_time"])));
            list.model["Created at"] = created_at.toLocaleString();
            controller.set_video_created_at(created_at.getTime());
        } else {
            delete list.model["Created at"];
        }

        list.modelChanged();

        root.fps = framerate;
        root.org_fps = framerate;

        controller.set_video_rotation(root.videoRotation)

        // Swap output dimensions for 90/270 metadata rotation so the export
        // and preview start with the correct portrait/landscape orientation.
        // R3D files are excluded — rotation is not supported for this format.
        const isR3D = (root.filename || "").toLowerCase().endsWith(".r3d");
        let exportW = w, exportH = h;
        if (!isR3D && (root.metadataRotation == 90 || root.metadataRotation == 270)) {
            exportW = h;
            exportH = w;
        }
        Qt.callLater(window.exportSettings.videoInfoLoaded, exportW, exportH, bitrate);
    }
    function updateEntry(key: string, value: string): void {
        if (key == "File name") root.filename = value;
        list.updateEntry(key, value);
    }
    function updateEntryWithTrigger(key: string, value: string): void {
        list.updateEntryWithTrigger(key, value);
    }

    function getDuration(md): string {
        const s = +md["stream.video[0].duration"] / 1000;
        if (s > 60) {
            return Math.floor(s / 60) + " m " + Math.floor(s % 60) + " s";
        } else if (s > 0) {
            return s.toFixed(2) + " s";
        }
        return "";
    }
    function getCodec(md): string {
        const c = md["stream.video[0].codec.name"] || "";
        const bitrate = +md["stream.video[0].codec.bit_rate"]? ((+md["stream.video[0].codec.bit_rate"] / 1024 / 1024).toFixed(2) + " Mbps") : "";

        return c.toUpperCase() + (c? " " : "") + bitrate;
    }
    function getPixelFormat(md): string {
        let pt = md["stream.video[0].codec.format_name"] || "";
        let bits = "8 bit";
        if (pt.indexOf("10le") > -1) { bits = "10 bit"; pt = pt.replace("p10le", "").replace("10le", ""); }
        if (pt.indexOf("12le") > -1) { bits = "12 bit"; pt = pt.replace("p12le", "").replace("12le", ""); }
        if (pt.indexOf("14le") > -1) { bits = "14 bit"; pt = pt.replace("p14le", "").replace("14le", ""); }
        if (pt.indexOf("16le") > -1) { bits = "16 bit"; pt = pt.replace("p16le", "").replace("16le", ""); }
        if (pt.indexOf("f32le") > -1) { bits = "32 bit float"; pt = pt.replace("f32le", ""); }
        if (pt.indexOf("f16le") > -1) { bits = "16 bit float"; pt = pt.replace("f16le", ""); }

        return pt.toUpperCase() + (pt? " " : "") + bits;
    }
    function getAudio(md): string {
        const format = md["stream.audio[0].codec.name"]? (md["stream.audio[0].codec.name"].replace("_", " ").replace("pcm", "PCM").replace("aac", "AAC")) : "";
        const rate = md["stream.audio[0].codec.sample_rate"]? (md["stream.audio[0].codec.sample_rate"] + " Hz") : "";

        return format + (format? " " : "") + rate;
    }

    Connections {
        target: controller;
        function onTelemetry_loaded(is_main_video: bool, filename: string, camera: string, additional_data: var): void {
            if (is_main_video && additional_data.creation_date_utc) {
                root.hasTelemetryTime = true;
                // Display local time with timezone if available, otherwise UTC
                // Strip subsecond part (e.g. ".875") for display
                let displayStr = (additional_data.creation_date || additional_data.creation_date_utc).replace(/\.\d+$/, "");
                if (additional_data.timezone_offset) {
                    displayStr += " (" + additional_data.timezone_offset + ")";
                }
                list.model["Created at"] = displayStr;
                list.modelChanged();
            }
            if (is_main_video && additional_data.realtime_fps) {
                const realtimeFps = +additional_data.realtime_fps;
                list.model["Frame rate"] = realtimeFps.toFixed(3) + " fps";
                root.fps = realtimeFps;
            } else if (is_main_video && additional_data.telemetry_fps && root.fps == 0) {
                const telFps = +additional_data.telemetry_fps;
                list.model["Frame rate"] = telFps.toFixed(3) + " fps";
                root.fps = telFps;
                root.org_fps = telFps;
            }
            if (is_main_video && additional_data.image_stabilizer !== undefined) {
                list.model["Image stabilization"] = additional_data.image_stabilizer ? "On" : "Off";
            }
            list.modelChanged();
        }
    }

    Button {
        text: qsTr("Open file");
        iconName: "video"
        anchors.horizontalCenter: parent.horizontalCenter;
        onClicked: root.selectFileRequest();
    }

    InfoMessageSmall {
        show: !root.hasAccessToInputDirectory;
        type: InfoMessage.Info;
        text: qsTr("In order to detect project files, video sequences or image sequences, click here and select the directory with input files.");
        OutputPathField { id: opf; visible: false; }
        MouseArea {
            anchors.fill: parent;
            cursorShape: Qt.PointingHandCursor;
            onClicked: {
                opf.selectFolder("", function(_) {
                    window.videoArea.loadFile(window.videoArea.loadedFileUrl);
                });
            }
        }
    }

    TableList {
        id: list;
        columnSpacing: 6 * dpiScale;
        editableFields: isCalibrator? ({}) : ({
            "Rotation": {
                "unit": "°",
                "from": -360,
                "to": 360,
                "value": function() { return root.videoRotation; },
                "keyframe": "VideoRotation",
                "onChange": function(value) {
                    root.videoRotation = value;
                    root.updateEntry("Rotation", root.videoRotation + " °");
                    controller.set_video_rotation(root.videoRotation);
                }
            },
            "Frame rate": {
                "unit": "fps",
                "precision": 3,
                "width": 70,
                "value": function() { return root.fps; },
                "onChange": function(value) {
                    root.fps = +value;
                    root.updateEntry("Frame rate", (+value).toFixed(3) + " fps");
                    controller.override_video_fps(+value, true);

                    const scale = root.fps / root.org_fps;
                    window.sync.everyNthFrame.value = Math.max(1, Math.floor(scale));

                    window.videoArea.timeline.updateDurations();
                }
            }
        });
    }

    DropTarget {
        parent: root.innerItem;
        color: styleBackground2;
        z: 999;
        anchors.rightMargin: -28 * dpiScale;
        anchors.topMargin: 35 * dpiScale;
        anchors.bottomMargin: -35 * dpiScale;
        extensions: fileDialog.extensions;
        onLoadFile: (path) => window.videoArea.loadFile(path, false)
    }
}
