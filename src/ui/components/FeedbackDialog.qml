// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2026 Gyroflow contributors

import QtQuick
import QtQuick.Controls as QQC
import QtQuick.Layouts

// Sibling components (Button / BasicText / TextField / TextArea) live here.
import "."

Rectangle {
    id: root;
    color: "#aa000000";
    anchors.fill: parent;
    visible: false;
    z: 9999;
    focus: visible;

    // ---- public API ----
    property bool crashMode: false;
    property int  pendingCrashCount: 0;

    function open(): void {
        opacity = 0; visible = true; opAnim.start();
        statusLabel.text = "";
        progressBar.visible = false;
        submitBtn.enabled = true;
        if (root.crashMode) {
            descArea.text = "";
        } else {
            descArea.text = "";
        }
        emailField.text = "";
    }
    function close(): void {
        opacityAnim2.start();
    }
    NumberAnimation { id: opAnim;       target: root; property: "opacity"; from: 0; to: 1; duration: 150 }
    NumberAnimation { id: opacityAnim2; target: root; property: "opacity"; from: 1; to: 0; duration: 150;
                      onStopped: root.visible = false }

    // Block all clicks behind the modal
    MouseArea { anchors.fill: parent; hoverEnabled: true; onClicked: {} preventStealing: true; }

    function isValidEmail(s): bool {
        if (!s) return true; // empty = valid (optional)
        return /^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(s);
    }

    Connections {
        target: controller;
        function onFeedbackProgress(stage, pct) {
            progressBar.visible = true;
            progressBar.value = pct / 100.0;
            statusLabel.text = qsTr("Stage: %1 (%2%)").arg(stage).arg(pct);
        }
        function onFeedbackCompleted(success, id, error) {
            // Just close — App.qml's Connections handles the user-facing toast.
            progressBar.visible = false;
            submitBtn.enabled = true;
            root.close();
        }
    }

    Rectangle {
        id: card;
        anchors.centerIn: parent;
        width:  Math.min(parent.width  - 60 * dpiScale, 520 * dpiScale);
        height: Math.min(parent.height - 80 * dpiScale, contentCol.implicitHeight + 60 * dpiScale);
        color: styleBackground2;
        radius: 8 * dpiScale;
        border.color: stylePopupBorder;
        border.width: 1;

        ColumnLayout {
            id: contentCol;
            anchors.fill: parent;
            anchors.margins: 20 * dpiScale;
            spacing: 14 * dpiScale;

            BasicText {
                Layout.fillWidth: true;
                text: root.crashMode
                    ? qsTr("Report a problem (after crash)")
                    : qsTr("Report a problem");
                font.pixelSize: 18 * dpiScale;
                font.bold: true;
            }

            BasicText {
                Layout.fillWidth: true;
                text: root.crashMode
                    ? qsTr("Last session crashed; the crash log is attached automatically. Description and email are optional.")
                    : qsTr("Logs and project metadata will be uploaded to Niyien for analysis. Description and email are optional. No video files are uploaded.");
                font.pixelSize: 11 * dpiScale;
                wrapMode: Text.WordWrap;
                opacity: 0.75;
            }

            TextArea {
                id: descArea;
                Layout.fillWidth: true;
                Layout.preferredHeight: 100 * dpiScale;
                text: "";
                // Custom placeholder shown only when text is empty.
                BasicText {
                    visible: descArea.text.length === 0;
                    anchors.left: parent.left;
                    anchors.top: parent.top;
                    anchors.leftMargin: 10 * dpiScale;
                    anchors.topMargin: 10 * dpiScale;
                    text: qsTr("What happened? (optional)");
                    opacity: 0.5;
                    font.pixelSize: 14 * dpiScale;
                }
            }

            TextField {
                id: emailField;
                Layout.fillWidth: true;
                placeholderText: qsTr("Email (optional, for follow-up)");
            }

            QQC.ProgressBar {
                id: progressBar;
                Layout.fillWidth: true;
                visible: false;
                from: 0.0; to: 1.0; value: 0.0;
            }
            BasicText {
                id: statusLabel;
                Layout.fillWidth: true;
                text: "";
                font.pixelSize: 11 * dpiScale;
                wrapMode: Text.WordWrap;
                opacity: 0.85;
            }

            RowLayout {
                Layout.fillWidth: true;
                Layout.alignment: Qt.AlignRight;
                spacing: 10 * dpiScale;
                Item { Layout.fillWidth: true; }
                Button {
                    text: qsTr("Cancel");
                    onClicked: root.close();
                }
                Button {
                    id: submitBtn;
                    text: qsTr("Submit");
                    accent: true;
                    enabled: root.isValidEmail(emailField.text);
                    onClicked: {
                        submitBtn.enabled = false;
                        statusLabel.text = qsTr("Packaging…");
                        // Empty options JSON — Rust side defaults all toggles to true.
                        controller.submitFeedback(descArea.text, emailField.text, "{}");
                    }
                }
            }
        }
    }

    // Esc → cancel (only if not actively submitting)
    Keys.onEscapePressed: {
        if (submitBtn.enabled) root.close();
    }
    // Ctrl+Enter → submit shortcut
    Shortcut { sequence: "Ctrl+Return"; onActivated: if (root.visible && submitBtn.enabled) submitBtn.clicked(); }
}
