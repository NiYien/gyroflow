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
    readonly property bool mobileLayout: typeof isMobile !== "undefined" && isMobile;
    property string mobileDisclosure: "";
    property bool sending: false;
    property bool crashMode: false;
    property int  pendingCrashCount: 0;
    // Paths of the crash zips this dialog instance is "covering". The Cancel
    // / Esc / outside-click paths persist `.dismissed` sidecars for each so
    // the startup auto-prompt does not return on next launch. Manual menu
    // entry leaves this empty — no dismiss markers written.
    property var  pendingCrashPaths: [];
    // Latch so a successful Submit doesn't also write dismiss markers when
    // the dialog closes from the FeedbackCompleted handler.
    property bool _submitting: false;

    function open(): void {
        // Read the current language each time the persistent dialog is opened.
        root.mobileDisclosure = root.mobileLayout && typeof ui_tools !== "undefined"
            ? ui_tools.mobile_document(root.crashMode ? "feedback_crash_notice" : "feedback_notice") : "";
        opacity = 0; visible = true; opAnim.start();
        statusLabel.text = "";
        progressBar.visible = false;
        root.sending = false;
        _submitting = false;
        if (root.crashMode) {
            descArea.text = "";
        } else {
            descArea.text = "";
        }
        emailField.text = "";
    }
    function close(): void {
        // Dismiss = user did not submit. In crash mode, mark each covered
        // zip so the auto-prompt does not re-trigger on next launch. Manual
        // menu entry has pendingCrashPaths === [] → no-op.
        if (root.crashMode && !_submitting && root.pendingCrashPaths && root.pendingCrashPaths.length > 0) {
            controller.dismissCrashZips(root.pendingCrashPaths);
        }
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
            root.sending = false;
            root.close();
        }
    }

    Rectangle {
        id: card;
        objectName: "feedbackCard";
        readonly property real availableHeight: root.mobileLayout && Qt.inputMethod.visible
            ? Math.min(root.height, root.mapFromItem(null, 0, Qt.inputMethod.keyboardRectangle.y).y) : root.height;
        x: (parent.width - width) / 2;
        y: root.mobileLayout ? Math.max(8 * dpiScale, (availableHeight - height) / 2) : (parent.height - height) / 2;
        width: Math.min(parent.width - (root.mobileLayout ? 24 : 60) * dpiScale, 520 * dpiScale);
        height: root.mobileLayout
            ? Math.max(0, Math.min(availableHeight - 24 * dpiScale, contentCol.implicitHeight + 40 * dpiScale))
            : Math.min(parent.height - 80 * dpiScale, contentCol.implicitHeight + 60 * dpiScale);
        color: styleBackground2;
        radius: 8 * dpiScale;
        border.color: stylePopupBorder;
        border.width: 1;

        QQC.ScrollView {
            id: feedbackScroll;
            objectName: "feedbackMobileScroll";
            visible: root.mobileLayout;
            anchors.fill: parent;
            anchors.margins: 20 * dpiScale;
            clip: true;
            Flickable {
                id: feedbackFlickable;
                contentWidth: feedbackScroll.availableWidth;
                contentHeight: root.mobileLayout ? contentCol.implicitHeight : 0;
                clip: true;
            }
        }
        ColumnLayout {
            id: contentCol;
            objectName: "feedbackContent";
            // Keep the original desktop geometry outside the mobile scroll view.
            parent: root.mobileLayout ? feedbackFlickable.contentItem : card;
            x: root.mobileLayout ? 0 : 20 * dpiScale;
            y: root.mobileLayout ? 0 : 20 * dpiScale;
            width: root.mobileLayout ? feedbackScroll.availableWidth : card.width - 40 * dpiScale;
            height: root.mobileLayout ? implicitHeight : card.height - 40 * dpiScale;
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
                text: root.mobileDisclosure || (root.crashMode
                    ? qsTr("Last session crashed; the crash log is attached automatically. Description and email are optional.")
                    : qsTr("Logs and project metadata will be uploaded to Niyien for analysis. Description and email are optional. No video files are uploaded."));
                font.pixelSize: (root.mobileLayout ? 14 : 11) * dpiScale;
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
                    objectName: "feedbackSubmit";
                    text: qsTr("Submit");
                    accent: true;
                    enabled: !root.sending && root.isValidEmail(emailField.text);
                    onClicked: {
                        root.sending = true;
                        root._submitting = true;
                        statusLabel.text = qsTr("Packaging…");
                        // Restore the existing one-click diagnostic bundle defaults.
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
