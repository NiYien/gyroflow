// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2024 NiYien

import QtQuick
import QtQuick.Controls as QQC

import "../components/"

Column {
    id: root;
    width: parent.width;
    spacing: 5 * dpiScale;

    property var exportSettings: window.exportSettings;

    // ── Output Path Mode (at top) ──
    Label {
        position: Label.LeftPosition;
        text: qsTranslate("Export", "Render queue output path");
        width: parent.width;
        ComboBox {
            id: simpleQueueOutputMode;
            model: [qsTranslate("Export", "Same as source file"), qsTranslate("Export", "Fixed path")];
            font.pixelSize: 12 * dpiScale;
            width: parent.width;
            currentIndex: exportSettings ? exportSettings.queueOutputMode : 0;
            onCurrentIndexChanged: {
                if (exportSettings) {
                    exportSettings.queueOutputMode.currentIndex = currentIndex;
                }
            }
        }
    }

    // ── Codec ──
    ComboBox {
        id: codec;
        model: exportSettings ? exportSettings.exportFormats.map(x => x.name) : [];
        width: parent.width;
        currentIndex: exportSettings ? exportSettings.outCodec === "H.264/AVC" ? 0 : exportSettings.outCodec === "H.265/HEVC" ? 1 : 0 : 1;
        onCurrentIndexChanged: {
            if (exportSettings) {
                exportSettings.codec.currentIndex = currentIndex;
            }
        }
        Component.onCompleted: {
            if (exportSettings) {
                currentIndex = exportSettings.codec.currentIndex;
            }
        }
    }
    // ── Codec Sub-options ──
    ComboBox {
        id: codecOptions;
        model: exportSettings ? exportSettings.exportFormats[codec.currentIndex].variants : [];
        width: parent.width;
        visible: model.length > 0;
        onCurrentIndexChanged: {
            if (exportSettings) {
                exportSettings.codecOptions.currentIndex = currentIndex;
            }
        }
        Component.onCompleted: {
            if (exportSettings && exportSettings.codecOptions) {
                currentIndex = exportSettings.codecOptions.currentIndex;
            }
        }
    }

    // ── Output Resolution with preset button ──
    Label {
        position: Label.LeftPosition;
        text: qsTranslate("Export", "Output size");
        Item {
            width: parent.width;
            height: simpleOutputWidth.height;
            NumberField {
                id: simpleOutputWidth;
                tooltip: qsTranslate("Export", "Width");
                anchors.verticalCenter: parent.verticalCenter;
                anchors.left: parent.left;
                width: (sizeMenuBtn.x - simpleOutputHeight.anchors.rightMargin - x - simpleLockAspect.width) / 2 - simpleLockAspect.anchors.leftMargin;
                value: exportSettings ? exportSettings.outWidth : 1920;
                onValueChanged: {
                    if (exportSettings && exportSettings.outWidth !== value) {
                        exportSettings.outWidth = value;
                        if (simpleLockAspect.checked) {
                            exportSettings.ensureAspectRatio(true);
                            simpleOutputHeight.value = exportSettings.outHeight;
                        }
                        exportSettings.notifySizeChanged();
                    }
                }
                live: false;
            }
            LinkButton {
                id: simpleLockAspect;
                checked: true;
                height: parent.height * 0.75;
                iconName: checked ? "lock" : "unlocked";
                topPadding: 4 * dpiScale;
                bottomPadding: 4 * dpiScale;
                leftPadding: 3 * dpiScale;
                rightPadding: -3 * dpiScale;
                anchors.verticalCenter: parent.verticalCenter;
                anchors.left: simpleOutputWidth.right;
                anchors.leftMargin: 5 * dpiScale;
                onClicked: checked = !checked;
                textColor: checked ? styleAccentColor : styleTextColor;
                display: QQC.Button.IconOnly;
                tooltip: qsTranslate("Export", "Lock aspect ratio");
                onCheckedChanged: if (checked && exportSettings) { exportSettings.aspectRatio = simpleOutputWidth.value / Math.max(1, simpleOutputHeight.value); }
            }
            NumberField {
                id: simpleOutputHeight;
                tooltip: qsTranslate("Export", "Height");
                anchors.verticalCenter: parent.verticalCenter;
                anchors.right: sizeMenuBtn.left;
                anchors.rightMargin: 5 * dpiScale;
                width: simpleOutputWidth.width;
                value: exportSettings ? exportSettings.outHeight : 1080;
                onValueChanged: {
                    if (exportSettings && exportSettings.outHeight !== value) {
                        exportSettings.outHeight = value;
                        if (simpleLockAspect.checked) {
                            exportSettings.ensureAspectRatio(false);
                            simpleOutputWidth.value = exportSettings.outWidth;
                        }
                        exportSettings.notifySizeChanged();
                    }
                }
                live: false;
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
                tooltip: qsTranslate("Export", "Output size preset");
                onClicked: {
                    // Reuse Full mode's size menu
                    if (exportSettings && exportSettings.sizeMenu) {
                        exportSettings.sizeMenu.y = exportSettings.sizeMenu.parent.y + exportSettings.sizeMenu.parent.height;
                        exportSettings.sizeMenu.open();
                    }
                }
            }
        }
    }

    // ── Bitrate ──
    Label {
        position: Label.LeftPosition;
        text: qsTranslate("Export", "Bitrate");
        visible: exportSettings && (exportSettings.outCodec === "H.264/AVC" || exportSettings.outCodec === "H.265/HEVC" || exportSettings.outCodec === "AV1");
        NumberField {
            id: simpleBitrate;
            value: exportSettings ? exportSettings.outBitrate : 20;
            defaultValue: 20;
            unit: qsTr("Mbps");
            width: parent.width;
            onValueChanged: {
                if (exportSettings && exportSettings.outBitrate !== value) {
                    exportSettings.outBitrate = value;
                }
            }
        }
    }

    // ── GPU Encoding ──
    CheckBox {
        id: simpleGpuEncoding;
        text: qsTranslate("Export", "Use GPU encoding");
        checked: exportSettings ? exportSettings.outGpu : true;
        onCheckedChanged: {
            if (exportSettings) {
                exportSettings.outGpu = checked;
            }
        }
    }

    // ── Sync from Full mode ──
    Connections {
        target: exportSettings;
        function onOutWidthChanged(): void { simpleOutputWidth.value = exportSettings.outWidth; }
        function onOutHeightChanged(): void { simpleOutputHeight.value = exportSettings.outHeight; }
        function onOutBitrateChanged(): void { simpleBitrate.value = exportSettings.outBitrate; }
    }
}
