// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Controls as QQC

import "../components/"
import "../Util.js" as Util;

MenuItem {
    id: root;
    text: qsTr("Export settings");
    iconName: "save";
    innerItem.enabled: window.videoArea.vid.loaded;
    objectName: "export";

    property alias codec: codec;
    property alias codecOptions: codecOptions;
    property alias sizeMenu: sizeMenu;

    function updateCodecParams(): void {
        codec.currentIndexChanged();
    }

    property bool isOsx: Qt.platform.os == "osx";

    property var exportFormats: {
        let list = [
            // If changing, make sure it's in sync with render_queue.rs:get_output_url
            { "name": "H.264/AVC",     "max_size": [4096, 4096],   "extension": ".mp4",      "gpu": true,  "audio": true,  "variants": [ ] },
            { "name": "H.265/HEVC",    "max_size": [8192, 8192],   "extension": ".mp4",      "gpu": true,  "audio": true,  "variants": [ ] },
            { "name": "ProRes",        "max_size": [16384, 16384], "extension": ".mov",      "gpu": isOsx, "audio": true,  "variants": ["Proxy", "LT", "Standard", "HQ", "4444", "4444XQ"] },
            { "name": "DNxHD",         "max_size": [8192, 8192],   "extension": ".mov",      "gpu": false, "audio": true,  "variants": [/*"DNxHD", */"DNxHR LB", "DNxHR SQ", "DNxHR HQ", "DNxHR HQX", "DNxHR 444"] },
            { "name": "CineForm",      "max_size": [16384, 16384], "extension": ".mov",      "gpu": false, "audio": true,  "variants": [] },
            { "name": "EXR Sequence",  "max_size": false,          "extension": "_%05d.exr", "gpu": false, "audio": false, "variants": [] },
            { "name": "PNG Sequence",  "max_size": false,          "extension": "_%05d.png", "gpu": false, "audio": false, "variants": ["8-bit", "16-bit"] },
        ];
        if (Qt.platform.os == "android") { // We can't render sequences on Android because of file system restrictions
            list = list.filter(x => !x.name.includes("Sequence"));
        }
        // if (Qt.platform.os == "windows" || Qt.platform.os == "linux" || Qt.platform.os == "android") {
        //     list.push({ "name": "AV1", "max_size": [8192, 8192], "extension": ".mp4", "gpu": true, "audio": true, "variants": [ ] });
        // }
        return list;
    };

    property var outputSizePresets: {
        "16:9": [
            ["8k", 7680, 4320],
            ["6k", 6016, 3384],
            ["4k", 3840, 2160],
            ["1080p", 1920, 1080],
            ["720p",  1280, 720],
        ],
        "17:9": [
            ["4k", 4096, 2160],
            ["2k", 2048, 1080],
        ],
        "9:16": [
            ["8k", 4320, 7680],
            ["6k", 3384, 6016],
            ["4k", 2160, 3840],
            ["1080p", 1080, 1920],
            ["720p",  720, 1280],
        ],
        "4:3": [
            ["480p", 640, 480],
        ],
        "1:1": [
            ["4k", 2160, 2160],
            ["1080p", 1080, 1080],
        ],
    }

    Component.onCompleted: {
        let parsed = null;
        try { parsed = JSON.parse(settings.value("outputSizePresets", "")); } catch(e) { }
        if (parsed) {
            outputSizePresets = parsed;
        }
        // sett's Component.onCompleted (settings.init) runs first (child before
        // parent), so the persisted queue output-path setting is already restored
        // here — push it into render_queue for the initial state.
        root.syncQueueOutputPath();
    }

    Item {
        id: sett;
        property alias defaultCodec: codec.currentIndex;
        property alias exportAudio: audio.checked;
        property alias keyframeDistance: keyframeDistance.value;
        property alias preserveOtherTracks: preserveOtherTracks.checked;
        property alias padWithBlack: padWithBlack.checked;
        property alias exportTrimsSeparately: exportTrimsSeparately.checked;
        property alias useVulkanEncoder: useVulkanEncoder.checked;
        property alias useD3D12Encoder: useD3D12Encoder.checked;
        property alias metadataComment: metadataComment.text;
        property alias audioCodec: audioCodec.currentIndex;
        property alias interpolationMethod: interpolationMethod.currentIndex;
        property alias preserveOutputSettings: preserveOutputSettings.checked;
        property alias preserveOutputPath: preserveOutputPath.checked;
        property alias queueOutputMode: queueOutputModeBox.currentIndex;
        property alias queueFixedOutputPath: queueFixedOutputPathField.folderUrl;

        Component.onCompleted: settings.init(sett);
        function propChanged() { settings.propChanged(sett); }
    }

    property real aspectRatio: 1.0;
    property alias outWidth: outputWidth.value;
    property alias outHeight: outputHeight.value;
    property alias defaultWidth: outputWidth.defaultValue;
    property alias defaultHeight: outputHeight.defaultValue;
    property alias lockAspectRatioChecked: lockAspectRatio.checked;

    property alias outCodec: codec.currentText;
    property alias outBitrate: bitrate.value;
    property alias defaultBitrate: bitrate.defaultValue;
    property alias outGpu: gpu.checked;
    // False when the current codec / source combination cannot be GPU encoded.
    // Exposed so mirrored GPU-encoding toggles elsewhere can grey themselves out
    // instead of offering a switch that getExportOptions will override anyway.
    property alias outGpuEnabled: gpu.enabled2;
    property alias outAudio: audio.checked;
    property alias preserveOutputSettings: preserveOutputSettings;
    property alias preserveOutputPath: preserveOutputPath;
    // Only the explicit persisted preserve checkbox carries settings across
    // videos, and only in Full mode where the checkbox is actually reachable.
    // Simple mode treats preserve as inert regardless of the stored value:
    // settings migrated from other installs can carry a ghost
    // preserveOutputSettings=true that the user can neither see nor untick
    // (the checkbox is hidden), which used to force stale portrait
    // preservedWidth/Height onto every loaded video. All preserve consumers
    // (panel reads, preserved* writes, queue-clear reset exemption, batch
    // add scrub exemption) must go through this single gate.
    function isPreserveActive(): bool { return preserveOutputSettings.checked && !window.isSimpleMode; }
    // Same gate for the "Preserve export path" checkbox: a ghost checked
    // value must neither redirect the single-video output folder in Simple
    // mode nor let Simple-mode folder changes overwrite the preserved path.
    function isPreservePathActive(): bool { return preserveOutputPath.checked && !window.isSimpleMode; }
    property alias exportTrimsSeparately: exportTrimsSeparately;
    // [queue-batch-streamline T3] 队列输出路径设置
    property alias queueOutputMode: queueOutputModeBox.currentIndex
    property alias queueFixedOutputPath: queueFixedOutputPathField.folderUrl
    property string outCodecOptions: "";
    property real originalWidth: outWidth;
    property real originalHeight: outHeight;
    // Source bitrate of the currently loaded video, recorded in
    // videoInfoLoaded before any preserved* override is applied. Used by
    // resetToSourceDefaults() to restore pristine per-video defaults.
    property real sourceBitrate: 0;
    property bool lensProfileOutputSizeActive: false;
    property real lensProfileOutputWidth: 0;
    property real lensProfileOutputHeight: 0;

    property bool canExport: !resolutionWarning.visible && !resolutionWarning2.visible;

    function browseQueueOutputFolder(): void {
        const dialog = Qt.createQmlObject("import QtQuick.Dialogs; FolderDialog {}", root, "selectQueueOutputFolder");
        if (queueFixedOutputPathField.folderUrl) dialog.currentFolder = queueFixedOutputPathField.folderUrl;
        dialog.accepted.connect(function() {
            queueFixedOutputPathField.folderUrl = dialog.selectedFolder.toString();
            filesystem.folder_access_granted(dialog.selectedFolder);
        });
        dialog.open();
    }
    // Mirror the current render-queue output-path setting (mode + fixed path)
    // into render_queue so start() can re-derive queued jobs' output_folder from
    // it. Both simple and full mode changes funnel through queueOutputModeBox /
    // browseQueueOutputFolder, so this is the single sync choke point.
    function syncQueueOutputPath(): void {
        if (typeof render_queue !== "undefined")
            render_queue.set_queue_output_path(queueOutputModeBox.currentIndex, queueFixedOutputPathField.folderUrl);
    }

    function getExportOptions(): var {
        let encoderOpts = encoderOptions.text.replace("-qscale:v", "-qscale")
                                             .replace("-q:v", "-qscale");
        return {
            codec:          root.outCodec,
            codec_options:  root.outCodecOptions,
            output_folder:    window.outputFile.folderUrl.toString(),
            output_filename:  window.outputFile.filename,
            output_width:   root.outWidth,
            output_height:  root.outHeight,
            bitrate:        root.outBitrate,
            // Final choke point for the GPU capability constraint. updateGpuStatus
            // already forces the toggle off when `enabled2` is false, but paths that
            // write outGpu without going through it — setExportOptions restoring a
            // .gyroflow project that stored use_gpu:true — would otherwise resurrect
            // an unrenderable combination.
            use_gpu:        root.outGpu && gpu.enabled2,
            audio:          root.outAudio,
            pixel_format:   "",

            // Advanced
            encoder_options:       encoderOpts,
            metadata:              { comment: metadataComment.text },
            keyframe_distance:     keyframeDistance.value,
            preserve_other_tracks: preserveOtherTracks.checked,
            pad_with_black:        padWithBlack.checked,
            export_trims_separately: exportTrimsSeparately.checked,
            audio_codec:           audioCodec.currentText,
            interpolation:         interpolationMethod.currentText
        };
    }

    property bool disableUpdate: false;
    function notifySizeChanged(): void {
        controller.set_output_size(outWidth, outHeight);
        // Only the persistent preserve toggle writes preserved* settings, and
        // never from Simple mode so a Simple session cannot overwrite values
        // explicitly preserved in Full mode.
        if (isPreserveActive() && outWidth > 0 && outHeight > 0) {
            settings.setValue("preservedWidth", outWidth);
            settings.setValue("preservedHeight", outHeight);
        }
    }
    function ensureAspectRatio(byWidth: bool): void {
        if (lockAspectRatio.checked && aspectRatio > 0) {
            if (byWidth) {
                outHeight = Math.round(outWidth / aspectRatio);
            } else {
                outWidth = Math.round(outHeight * aspectRatio);
            }
        }
    }
    function setDefaultSize(w: real, h: real): void {
        aspectRatio   = w / h;
        defaultWidth  = w;
        defaultHeight = h;

        disableUpdate = true;
        if (isPreserveActive()) {
            const pw = +settings.value("preservedWidth",  w); if (pw > 0) w = pw;
            const ph = +settings.value("preservedHeight", h); if (ph > 0) h = ph;
            aspectRatio = w / h;
        }
        outWidth      = w;
        outHeight     = h;
        disableUpdate = false;
    }
    // Whether the loaded source stores more than 8 bits per component.
    // `vidInfo.pixelFormat` is assembled by VideoInformation::getPixelFormat and
    // ends in one of "8 bit" / "10 bit" / "12 bit" / "14 bit" / "16 bit" /
    // "16 bit float" / "32 bit float". Testing for "not 8 bit" covers every high
    // bit depth at once, where the previous "contains 10 bit" test silently let
    // 12/14/16-bit and floating point sources through. A source whose format
    // could not be read also reports "8 bit", so unknown input keeps the
    // conservative "do not trigger" behavior.
    function isHighBitDepthSource(): bool {
        const pf = (window.vidInfo && window.vidInfo.pixelFormat) ? ("" + window.vidInfo.pixelFormat) : "";
        if (!pf || pf === "---") return false;
        return !pf.includes("8 bit");
    }
    function videoInfoLoaded(w: real, h: real, br: real): void {
        lensProfileOutputSizeActive = false;
        lensProfileOutputWidth = 0;
        lensProfileOutputHeight = 0;
        setDefaultSize(w, h);
        root.originalWidth = w;
        root.originalHeight = h;
        root.sourceBitrate = br;
        Qt.callLater(notifySizeChanged);
        if (isPreserveActive()) {
            const pbr = +settings.value("preservedBitrate", br);
            if (pbr > 0) br = pbr;
        }

        outBitrate     = br;
        defaultBitrate = br;

        // Change to HEVC if the source is high bit depth (no GPU family encodes
        // H.264 above 8-bit in hardware).
        if (codec.currentIndex == 0 && isHighBitDepthSource()) {
            codec.currentIndex = 1;
        }

        codec.updateGpuStatus();
    }
    function lensProfileLoaded(w: real, h: real): void {
        setDefaultSize(w, h);
        Qt.callLater(notifySizeChanged);
    }
    // clear-queue-resets-export-settings: restore the panel to the current
    // main-preview video's own defaults (or pristine startup state when no
    // video is loaded). Invoked on render_queue.queue_cleared unless the
    // explicit preserve checkbox is on, so leftovers from a previous batch
    // cannot outlive an explicit "clear queue" fresh start.
    function resetToSourceDefaults(): void {
        if (originalWidth > 0 && originalHeight > 0) {
            setDefaultSize(originalWidth, originalHeight);
            Qt.callLater(notifySizeChanged);
            outBitrate     = sourceBitrate;
            defaultBitrate = sourceBitrate;
        } else {
            disableUpdate = true;
            outWidth  = 0;
            outHeight = 0;
            disableUpdate = false;
            outBitrate     = 0;
            defaultBitrate = 0;
            sourceBitrate  = 0;
        }
    }
    function lensProfileOutputDimensionLoaded(w: real, h: real): void {
        lensProfileOutputSizeActive = true;
        lensProfileOutputWidth = w;
        lensProfileOutputHeight = h;
        lensProfileLoaded(w, h);
    }
    function lensProfileOutputDimensionCleared(): void {
        if (!lensProfileOutputSizeActive) return;
        const shouldRestore = originalWidth > 0 && originalHeight > 0 &&
                              outWidth == lensProfileOutputWidth && outHeight == lensProfileOutputHeight &&
                              (outWidth != originalWidth || outHeight != originalHeight);
        lensProfileOutputSizeActive = false;
        lensProfileOutputWidth = 0;
        lensProfileOutputHeight = 0;
        if (shouldRestore) {
            lensProfileLoaded(originalWidth, originalHeight);
        } else {
            Qt.callLater(notifySizeChanged);
        }
    }
    function loadGyroflow(obj: var): void {
        const output = obj.output || { };
        if (output && Object.keys(output).length > 0) {
            if (output.output_path) {
                // Backwards compatibility
                if (window.outputFile.filename && output.output_path.endsWith("/") || output.output_path.endsWith("\\")) {
                    // It's a folder, so adjust current file
                    window.outputFile.setFolder(filesystem.path_to_url(output.output_path));
                } else {
                    window.outputFile.setUrl(filesystem.path_to_url(output.output_path));
                }
            }
            if (output.output_folder) {
                if (!output.output_folder.startsWith("/") && !output.output_folder.includes(":/") && !output.output_folder.includes(":\\")) { // It's likely a relative url
                    console.log("Resolving relative url: " + output.output_folder);
                    const current_folder = filesystem.get_folder(window.videoArea.loadedFileUrl);
                    const current_path = filesystem.url_to_path(current_folder);
                    const new_folder = current_path + "/" + output.output_folder + "/";
                    output.output_folder = filesystem.path_to_url(new_folder);
                    console.log("= " + output.output_folder);
                }
                window.outputFile.setFolder(output.output_folder);
            }
            if (output.output_filename) {
                window.outputFile.setFilename(output.output_filename);
            }

            if (output.codec)         Util.setComboValue(codec,        output.codec);
            if (output.codec_options) Util.setComboValue(codecOptions, output.codec_options);

            if (output.output_width && output.output_height) {
                setDefaultSize(output.output_width, output.output_height);
                Qt.callLater(notifySizeChanged);
            }
            if (output.bitrate) root.outBitrate = output.bitrate;
            if (output.hasOwnProperty("use_gpu")) root.outGpu   = output.use_gpu;
            if (output.hasOwnProperty("audio"))   root.outAudio = output.audio;

            // Advanced
            if (output.hasOwnProperty("encoder_options"))       encoderOptions.text         = output.encoder_options;
            if (output.hasOwnProperty("keyframe_distance"))     keyframeDistance.value      = +output.keyframe_distance;
            if (output.hasOwnProperty("preserve_other_tracks")) preserveOtherTracks.checked = output.preserve_other_tracks;
            if (output.hasOwnProperty("pad_with_black"))        padWithBlack.checked        = output.pad_with_black;
            if (output.hasOwnProperty("export_trims_separately")) exportTrimsSeparately.checked = output.export_trims_separately;
            if (output.hasOwnProperty("audio_codec"))           Util.setComboValue(audioCodec, output.audio_codec);
            if (output.hasOwnProperty("interpolation"))         Util.setComboValue(interpolationMethod, output.interpolation);
            if (output.hasOwnProperty("metadata")) {
                metadataComment.text = output.metadata.comment || "";
            }
        }
    }

    ComboBox {
        id: codec;
        model: exportFormats.map(x => x.name);
        width: parent.width;
        currentIndex: 1;
        function updateExtension(ext: string): void {
            window.outputFile.setFilename(window.outputFile.filename.replace(/(_%[0-9d]+)?\.[a-z0-9]+$/i, ext));
        }
        function updateGpuStatus(): void {
            const format = exportFormats[currentIndex];
            gpu.enabled2 = format.gpu;
            if (format.name == "H.264/AVC" && root.isHighBitDepthSource()) {
                gpu.enabled2 = false;
            }
            const gpuChecked = +settings.value("exportGpu-" + currentIndex, -1);
            if (!gpu.enabled2) {
                // `enabled2 == false` is a capability constraint, not a preference:
                // this combination cannot be encoded on the GPU at all. It must win
                // over the persisted per-codec preference, which previously kept
                // `checked` (and therefore the exported `use_gpu`) true behind a
                // greyed-out control. preventSave keeps the user's stored preference
                // intact, so it comes back when they switch to a codec that can use
                // the GPU.
                gpu.preventSave = true;
                gpu.checked = false;
                gpu.preventSave = false;
            } else if (gpuChecked == -1) {
                gpu.preventSave = true;
                gpu.checked = gpu.enabled2;
                gpu.preventSave = false;
            } else {
                gpu.checked = gpuChecked == 1;
            }

            encoderOptions.preventSave = true;
            encoderOptions.text = settings.value("encoderOptions-" + currentIndex, "");
            encoderOptions.preventSave = false;
        }
        onCurrentIndexChanged: {
            const format = exportFormats[currentIndex];
            audio.enabled2 = format.audio;
            if (!audio.enabled2) audio.checked = false;

            updateGpuStatus();
            updateExtension(format.extension);
        }
    }
    ComboBox {
        id: codecOptions;
        model: exportFormats[codec.currentIndex].variants;
        width: parent.width;
        visible: model.length > 0;
        onVisibleChanged: if (!visible) { root.outCodecOptions = ""; } else { root.outCodecOptions = currentText; }
        onCurrentTextChanged: root.outCodecOptions = currentText;
        onModelChanged: {
            const format = exportFormats[codec.currentIndex];
            if (format.name == "ProRes") currentIndex = 3; // ProRes HQ by default
            if (format.name == "DNxHD") currentIndex = 2; // DNxHR HQ by default
        }
    }
    Label {
        position: Label.LeftPosition;
        text: qsTr("Output size");
        Item {
            width: parent.width;
            height: outputWidth.height;
            NumberField {
                id: outputWidth;
                tooltip: qsTr("Width");
                anchors.verticalCenter: parent.verticalCenter;
                anchors.left: parent.left;
                width: (sizeMenuBtn.x - outputHeight.anchors.rightMargin - x - lockAspectRatio.width) / 2 - lockAspectRatio.anchors.leftMargin;
                onValueChanged: {
                    if (!disableUpdate) {
                        disableUpdate = true;
                        ensureAspectRatio(true);
                        Qt.callLater(notifySizeChanged);
                        disableUpdate = false;
                    }
                }
                live: false;
                onActiveFocusChanged: if (activeFocus) selectAll();
                reset: () => { aspectRatio = defaultValue / Math.max(1,outHeight); value = defaultValue; };
            }
            NumberField {
                id: outputHeight;
                tooltip: qsTr("Height");
                anchors.verticalCenter: parent.verticalCenter;
                anchors.right: sizeMenuBtn.left;
                anchors.rightMargin: 5 * dpiScale;
                width: outputWidth.width;
                onValueChanged: {
                    if (!disableUpdate) {
                        disableUpdate = true;
                        ensureAspectRatio(false);
                        Qt.callLater(notifySizeChanged);
                        disableUpdate = false;
                    }
                }
                live: false;
                onActiveFocusChanged: if (activeFocus) selectAll();
                reset: () => { aspectRatio = outWidth / Math.max(1,defaultValue); value = defaultValue; };
            }
            LinkButton {
                id: lockAspectRatio;
                checked: true;
                height: parent.height * 0.75;
                iconName: checked? "lock" : "unlocked";
                topPadding: 4 * dpiScale;
                bottomPadding: 4 * dpiScale;
                leftPadding: 3 * dpiScale;
                rightPadding: -3 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
                anchors.left: outputWidth.right;
                anchors.leftMargin: 5 * dpiScale;
                onClicked: checked = !checked;
                textColor: checked? styleAccentColor : styleTextColor;
                display: QQC.Button.IconOnly;
                tooltip: qsTr("Lock aspect ratio");
                onCheckedChanged: if (checked) { aspectRatio = outWidth / Math.max(1,outHeight); }
            }
            LinkButton {
                id: sizeMenuBtn;
                height: parent.height;
                iconName: "settings";
                leftPadding: 3 * dpiScale;
                rightPadding: 3 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
                anchors.right: parent.right;
                display: QQC.Button.IconOnly;
                tooltip: qsTr("Output size preset");
                onClicked: sizeMenu.openFrom(sizeMenuBtn);
            }
            TabbedPopup {
                id: sizeMenu;
                parent: window;
                width: sizeMenuBtn.parent.width;
                font.pixelSize: 11.5 * dpiScale;
                itemHeight: 27 * dpiScale;
                editable: true;
                editTooltip: qsTr("Edit sizes");
                formatItem: x => `${x[0]} (${x[1]} x ${x[2]})`;
                property var items: outputSizePresets;
                model: Object.assign({}, ...Object.entries(items).map(([k, v]) => {
                    let arr = [[qsTr("Original"), root.originalWidth, root.originalHeight]];
                    const parts = k.split(":");
                    if (parts.length == 2 && +parts[0].trim() > 0 && +parts[1].trim() > 0) {
                        const r = Math.abs(window.vidInfo.videoRotation);
                        const ow = r == 90 || r == 270? root.originalHeight : root.originalWidth;
                        const oh = r == 90 || r == 270? root.originalWidth  : root.originalHeight;
                        const wp = +parts[0].trim();
                        const hp = +parts[1].trim();
                        const sw = ow / wp;
                        const sh = oh / hp;
                        const scale = Math.min(sw, sh);
                        let nw = Math.round(wp * scale);
                        let nh = Math.round(hp * scale);
                        if (nw % 2 != 0) nw -= 1;
                        if (nh % 2 != 0) nh -= 1;
                        arr.push([qsTr("Proportional"), nw, nh]);

                        let nwz = Math.round(nw * (100.0 / window.stab.maxValues.maxZoom));
                        let nhz = Math.round(nh * (100.0 / window.stab.maxValues.maxZoom));
                        if (nwz % 2 != 0) nwz -= 1;
                        if (nhz % 2 != 0) nhz -= 1;
                        arr.push([qsTr("Based on \"Max zoom\""), nwz, nhz]);
                    }
                    return {[k]: arr.concat(v)};
                }));
                onClicked: function(index) {
                    const item = model[tabs[currentTab]][index];
                    sizeMenu.setSize(item[1], item[2]);
                }
                function openFrom(trigger: Item): void {
                    if (!trigger) {
                        open();
                        return;
                    }

                    const margin = 8 * dpiScale;
                    const popupWidth = Math.max(width, implicitWidth);
                    const popupHeight = Math.max(height, implicitHeight);
                    const maxX = Math.max(margin, window.width - popupWidth - margin);
                    const maxY = Math.max(margin, window.height - popupHeight - margin);
                    const pt = window.mapFromItem(trigger, 0, 0);
                    const preferredX = pt.x + trigger.width - popupWidth;
                    const belowY = pt.y + trigger.height;
                    const aboveY = pt.y - popupHeight;
                    const preferredY = (belowY + popupHeight <= window.height - margin || aboveY < margin)? belowY : aboveY;

                    x = Math.max(margin, Math.min(maxX, preferredX));
                    y = Math.max(margin, Math.min(maxY, preferredY));
                    open();
                }
                onEdit: function() {
                    sizeMenu.close();
                    sizeMenu.resetTab();
                    const dlg = messageBox(Modal.NoIcon, qsTr("You can edit the output size presets here:"), [
                        { text: qsTr("Save"), accent: true, clicked: function() {
                            let parsed = null;
                            try { parsed = JSON.parse(dlg.mainColumn.children[1].text); } catch(e) { }
                            if (parsed) {
                                settings.setValue("outputSizePresets", JSON.stringify(parsed));
                                sizeMenu.items = parsed;
                            } else {
                                messageBox(Modal.Error, qsTr("Invalid JSON format!"), [ { "text": qsTr("Ok") } ]);
                            }
                        } },
                        { text: qsTr("Cancel") },
                    ]);
                    const json = JSON.stringify(items)
                        .replace(/\],\[/g, '],\n        [')
                        .replace(/,"/g, ",\n    \"")
                        .replace(/:\[\[/g, ":[\n        [")
                        .replace(/\]\],/g, "]\n    ],")
                        .replace(/\]\]\}/g, "]\n    ]\n}")
                        .replace(/{"/g, "{\n    \"");
                    const tf = Qt.createComponent("../components/TextArea.qml").createObject(dlg.mainColumn, { text: json });
                    tf.anchors.horizontalCenter = dlg.mainColumn.horizontalCenter;
                }
                function setSize(w: real, h: real): void {
                    disableUpdate = true;
                    aspectRatio = w / h;
                    outWidth = w;
                    outHeight = h;
                    Qt.callLater(notifySizeChanged);
                    disableUpdate = false;
                }
            }
        }
    }

    InfoMessageSmall {
        id: resolutionWarning;
        type: InfoMessage.Error;
        property var maxSize: exportFormats[codec.currentIndex].max_size;
        show: maxSize && (outWidth > maxSize[0] || outHeight > maxSize[1]);
        text: qsTr("This resolution is not supported by the selected codec.") + "\n" +
              qsTr("Maximum supported resolution is %1.").arg(maxSize? maxSize.join("x") : "");
    }
    InfoMessageSmall {
        id: resolutionWarning2;
        type: InfoMessage.Error;
        show: (outWidth % 2) != 0 || (outHeight % 2) != 0;
        text: qsTr("Resolution must be divisible by 2.");
    }

    Label {
        position: Label.LeftPosition;
        text: qsTr("Bitrate");
        visible: outCodec === "H.264/AVC" || outCodec === "H.265/HEVC" || outCodec === "AV1";

        NumberField {
            id: bitrate;
            value: 0;
            defaultValue: 20;
            unit: qsTr("Mbps");
            width: parent.width;
            onValueChanged: {
                if (isPreserveActive() && value > 0) {
                    settings.setValue("preservedBitrate", value);
                }
            }
        }
    }

    CheckBox {
        id: gpu;
        text: qsTr("Use GPU encoding");
        checked: true;
        onCheckedChanged: {
            if (!preventSave)
                settings.setValue("exportGpu-" + codec.currentIndex, checked? 1 : 0);
        }
        property bool preventSave: false;
        property bool enabled2: true;
        enabled: enabled2;
        tooltip: enabled2? qsTr("GPU encoders typically generate output of lower quality than software encoders, but are significantly faster.") + "\n" +
                           qsTr("They require a higher bitrate to make output with the same perceptual quality, or they make output with a lower perceptual quality at the same bitrate.") + "\n" +
                           qsTr("Uncheck this option for maximum possible quality.")
                         :
                           qsTr("GPU acceleration is not available for the pixel format of this video.");
    }
    CheckBox {
        id: audio;
        text: qsTr("Export audio");
        checked: true;
        property bool enabled2: true;
        property bool enabled3: window.stab.videoSpeed.value == 1.0 && !window.stab.videoSpeed.isKeyframed;
        tooltip: !enabled3? qsTr("Audio export not available when changing video speed.") : "";
        enabled: enabled2 && enabled3;
    }

    // [queue-batch-streamline T3] 队列默认输出路径设置（顶层可见）
    Label {
        position: Label.TopPosition;
        text: qsTr("Render queue output path");
        ComboBox {
            id: queueOutputModeBox;
            model: [qsTr("Same as source file"), qsTr("Fixed path")];
            font.pixelSize: 12 * dpiScale;
            width: parent.width;
            currentIndex: 0;
            // Simple-mode dropdown writes through exportSettings.queueOutputMode
            // (this alias), so this handler covers both simple and full mode.
            onCurrentIndexChanged: root.syncQueueOutputPath();
        }
    }
    Row {
        visible: queueOutputModeBox.currentIndex === 1;
        width: parent.width;
        spacing: 5 * dpiScale;
        TextField {
            id: queueFixedOutputPathField;
            width: parent.width - browseQueueOutputBtn.width - parent.spacing;
            placeholderText: qsTr("Select output folder...");
            property string folderUrl: ""
            text: folderUrl ? filesystem.url_to_path(folderUrl) : ""
            readOnly: true;
            // Covers every fixed-path write point (browse dialog, Android SAF
            // add-time grants in RenderQueue.qml, settings restore).
            onFolderUrlChanged: root.syncQueueOutputPath();
        }
        LinkButton {
            id: browseQueueOutputBtn;
            height: queueFixedOutputPathField.height;
            text: qsTr("Browse");
            onClicked: root.browseQueueOutputFolder()
        }
    }

    AdvancedSection {
        Label {
            position: Label.TopPosition;
            text: qsTr("Custom encoder options");

            TextField {
                id: encoderOptions;
                width: parent.width;
                validator: RegularExpressionValidator {
                    regularExpression: /(-([^\s"]+)\s+("[^"]+"|[^\s"]+)\s*?)*/
                }
                onEditingFinished: {
                    if (!preventSave)
                        settings.setValue("encoderOptions-" + codec.currentIndex, text);
                }
                property bool preventSave: false;
            }
            LinkButton {
                id: encoderOptionsInfo;
                height: parent.height;
                iconName: "info";
                leftPadding: 3 * dpiScale;
                rightPadding: 3 * dpiScale;
                y: -encoderOptions.height;
                anchors.right: parent.right;
                display: QQC.Button.IconOnly;
                tooltip: qsTr("Show available options");
                onClicked: {
                    const text = render_queue.get_encoder_options(render_queue.get_default_encoder(root.outCodec, root.outGpu));
                    const el = window.messageBox(Modal.Info, text, [ { text: qsTr("Ok") } ], undefined, Text.RichText);
                    el.t.horizontalAlignment = Text.AlignLeft;
                }
            }
        }
        Label {
            position: Label.TopPosition;
            text: qsTr("Metadata comment");
            TextField {
                id: metadataComment;
                width: parent.width;
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Keyframe distance");

            NumberField {
                id: keyframeDistance;
                width: parent.width;
                height: 25 * dpiScale;
                value: 1;
                from: 0.01;
                precision: 2;
                unit: qsTr("s");
            }
        }
        CheckBox {
            id: preserveOtherTracks;
            text: qsTr("Preserve other tracks");
            checked: false;
            tooltip: qsTr("This disables trim range and you need to use the .mov output file extension");
            onCheckedChanged: if (checked) codec.updateExtension(".mov");
        }
        CheckBox {
            id: padWithBlack;
            text: qsTr("Use black frames outside trim range and keep original file duration");
            checked: false;
            width: parent.width;
            Component.onCompleted: contentItem.wrapMode = Text.WordWrap;
        }
        CheckBox {
            id: exportTrimsSeparately;
            text: qsTr("Export trim ranges as separate videos");
            checked: false;
            width: parent.width;
            Component.onCompleted: contentItem.wrapMode = Text.WordWrap;
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Audio codec");
            enabled: audio.checked;
            ComboBox {
                id: audioCodec;
                model: ["AAC", "PCM (s16le)", "PCM (s16be)", "PCM (s24le)", "PCM (s24be)"];
                font.pixelSize: 12 * dpiScale;
                width: parent.width;
                currentIndex: 0;
            }
        }
        Label {
            position: Label.LeftPosition;
            text: qsTr("Interpolation method");
            ComboBox {
                id: interpolationMethod;
                model: ["Bilinear", "Bicubic", "Lanczos4", "EWA: RobidouxSharp", "EWA: Robidoux", "EWA: Mitchell", "EWA: Catmull-Rom"];
                font.pixelSize: 12 * dpiScale;
                width: parent.width;
                currentIndex: 2;
            }
        }
        Label {
            position: Label.TopPosition;
            text: qsTr("Device for rendering");
            visible: root.outGpu && renderingDevice.model.length > 0;
            ComboBox {
                id: renderingDevice;
                model: [];
                font.pixelSize: 12 * dpiScale;
                width: parent.width;
                currentIndex: 0;
                property bool preventChange: true;
                property var orgList: [];
                Connections {
                    target: controller;
                    function onGpu_list_loaded(list: list<string>): void {
                        const saved = settings.value("renderingDevice", defaultInitializedDevice);
                        const toRemove = [ "[OpenCL]", "[wgpu]", "(Vulkan)", "(Metal)", "(Dx12)", "(Dx11)", "(Gl)" ];
                        list = list.map(x => {
                            for (const keyword of toRemove) {
                                x = x.replace(keyword, "").trim()
                            }
                            if (Qt.platform.os == "ios" && x.toLowerCase().includes("apple m")) {
                                root.exportFormats[2]['gpu'] = true; // ProRes is supported on apple silicon
                            }
                            return x;
                        });
                        list = [...new Set(list)];

                        renderingDevice.orgList = list;
                        renderingDevice.preventChange = true;
                        renderingDevice.model = list;
                        for (let i = 0; i < list.length; ++i) {
                            if (list[i] == saved) {
                                renderingDevice.currentIndex = i;
                                break;
                            }
                        }
                        if (saved != defaultInitializedDevice) {
                            Qt.callLater(renderingDevice.updateController);
                        }
                        renderingDevice.preventChange = false;
                    }
                }
                onCurrentTextChanged: {
                    if (preventChange) return;
                    Qt.callLater(renderingDevice.updateController);
                }
                function updateController(): void {
                    controller.set_rendering_gpu_type_from_name(renderingDevice.currentText);
                    settings.setValue("renderingDevice", renderingDevice.orgList[renderingDevice.currentIndex]);
                }
            }
        }
        CheckBox {
            id: preserveOutputSettings;
            text: qsTr("Preserve export settings");
            checked: false;
            tooltip: qsTr("Save output size and bitrate in settings and use it for all files.");
            onCheckedChanged: {
                if (checked) {
                    if (outputWidth.value  > 0) settings.setValue("preservedWidth",  outputWidth.value);
                    if (outputHeight.value > 0) settings.setValue("preservedHeight", outputHeight.value);
                    if (bitrate.value > 0) settings.setValue("preservedBitrate", bitrate.value);
                }
            }
        }
        CheckBox {
            id: preserveOutputPath;
            text: qsTr("Preserve export path");
            checked: false;
            tooltip: qsTr("Save output path in settings and use it for all files.");
            onCheckedChanged: {
                if (checked) {
                    const outputFolder = window.outputFile.folderUrl.toString();
                    if (outputFolder) settings.setValue("preservedOutputPath", outputFolder);
                }
            }
        }
        CheckBox {
            visible: Qt.platform.os == "linux" || Qt.platform.os == "windows";
            id: useVulkanEncoder;
            text: qsTr("Use experimental Vulkan encoder (HEVC only)");
            checked: false;
            width: parent.width;
            Component.onCompleted: contentItem.wrapMode = Text.WordWrap;
            onCheckedChanged: Qt.callLater(renderingDevice.updateController);
        }
        CheckBox {
            visible: Qt.platform.os == "windows";
            id: useD3D12Encoder;
            text: qsTr("Use experimental D3D12 encoder (HEVC and AVC)");
            checked: false;
            width: parent.width;
            Component.onCompleted: contentItem.wrapMode = Text.WordWrap;
            onCheckedChanged: Qt.callLater(renderingDevice.updateController);
        }

    }
}
