// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/components" as Components

TestCase {
    id: test
    name: "FeedbackOneClick"
    when: windowShown
    width: 390; height: 650
    visible: true
    property real dpiScale: 1
    property bool isMobile: true
    property string style: "light"
    property string styleFont: "Arial"
    property color styleBackground: "#ffffff"
    property color styleBackground2: "#f5f6f8"
    property color stylePopupBorder: "#cccccc"
    property color styleTextColor: "#222222"
    property color styleTextColorOnAccent: "#ffffff"
    property color styleAccentColor: "#006bd6"
    property color styleButtonColor: "#eeeeee"
    property color styleTextFieldColor: "#ffffff"
    property var window: QtObject { property bool useMobileWorkspace: test.isMobile; property bool isMobileLayout: test.isMobile }
    QtObject {
        id: ui_tools
        function mobile_document(kind) {
            return "Submitting sends your message, optional email and available logs, crash records, device information and settings to NiYien for troubleshooting. File names and paths may be included; video and audio files are not uploaded. You can instead email support@niyien.com without diagnostics.";
        }
    }
    QtObject {
        id: controller
        property var submitted: null
        property int submissionCount: 0
        signal feedbackProgress(string stage, real pct)
        signal feedbackCompleted(bool success, string id, string error)
        function dismissCrashZips(paths) {}
        function submitFeedback(message, email, options) { submitted = JSON.parse(options); submissionCount++; }
    }
    Components.FeedbackDialog { id: dialog }
    function init() {
        isMobile = true; width = 390; height = 650;
        controller.submitted = null; controller.submissionCount = 0;
        dialog.crashMode = false;
    }
    function cleanup() {
        controller.feedbackCompleted(true, "test", "");
        wait(180);
    }
    function test_one_click_upload_data() {
        return [
            { tag: "mobile", mobile: true, crash: false },
            { tag: "desktop", mobile: false, crash: false },
            { tag: "mobile_crash", mobile: true, crash: true }
        ];
    }
    function test_one_click_upload(data) {
        isMobile = data.mobile;
        if (!isMobile) { width = 1000; height = 760; }
        dialog.crashMode = data.crash;
        dialog.open();
        wait(180);
        compare(findChild(dialog, "feedbackDiagnosticConsent"), null);
        compare(findChild(dialog, "feedbackProjectConsent"), null);
        const submit = findChild(dialog, "feedbackSubmit");
        verify(submit && submit.enabled);
        mouseClick(submit);
        compare(controller.submissionCount, 1);
        compare(JSON.stringify(controller.submitted), "{}");
        verify(!submit.enabled);
        mouseClick(submit);
        compare(controller.submissionCount, 1);
        controller.feedbackCompleted(true, "test", "");
        wait(180);
        dialog.open();
        wait(180);
        verify(submit.enabled);
    }
    function test_desktop_original_geometry_data() {
        return [
            { tag: "normal", w: 1000, h: 760 },
            { tag: "short", w: 800, h: 360 }
        ];
    }
    function test_desktop_original_geometry(data) {
        isMobile = false; width = data.w; height = data.h;
        dialog.open();
        wait(180);
        const card = findChild(dialog, "feedbackCard");
        const content = findChild(dialog, "feedbackContent");
        const scroll = findChild(dialog, "feedbackMobileScroll");
        verify(card && content && scroll);
        verify(!scroll.visible);
        compare(content.parent, card);
        compare(card.width, Math.min(width - 60, 520));
        compare(card.height, Math.min(height - 80, content.implicitHeight + 60));
        compare(card.x, (width - card.width) / 2);
        compare(card.y, (height - card.height) / 2);
        compare(content.x, 20); compare(content.y, 20);
        compare(content.width, card.width - 40);
        compare(content.height, card.height - 40);
    }
    function test_mobile_short_window_can_reach_submit() {
        width = 650; height = 300;
        dialog.open();
        wait(180);
        const scroll = findChild(dialog, "feedbackMobileScroll");
        const content = findChild(dialog, "feedbackContent");
        const submit = findChild(dialog, "feedbackSubmit");
        verify(scroll.visible);
        compare(content.parent, scroll.contentItem.contentItem);
        verify(scroll.contentHeight > scroll.availableHeight);
        scroll.contentItem.contentY = scroll.contentHeight - scroll.availableHeight;
        wait(40);
        mouseClick(submit);
        compare(controller.submissionCount, 1);
    }
}
