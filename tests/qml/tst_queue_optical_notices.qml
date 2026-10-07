// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

// queue-optical-analysis: what a queue row says when an experimental feature could not be applied
TestCase {
    id: testCase
    name: "QueueOpticalNotices"
    property var formatter

    function initTestCase() {
        // Exercise the production formatter without starting the full application.
        const request = new XMLHttpRequest();
        request.open("GET", Qt.resolvedUrl("../../src/ui/App.qml"), false);
        request.send();
        const source = request.responseText;
        const start = source.indexOf("    function getReadableError(");
        const end = source.indexOf("    function renameOutput(", start);
        verify(start >= 0 && end > start);
        formatter = Qt.createQmlObject("import QtQuick; QtObject {\n" + source.substring(start, end) + "\n}", testCase, "App.qml");
    }

    function fallback(names, reason) {
        return qsTranslate("App", "%1 could not be applied (%2). Processed without it.").arg(names).arg(reason);
    }

    function test_fallback_names_the_feature_and_the_reason() {
        compare(formatter.getReadableError("optical_fallback:translation:Not enough of the image could be tracked"),
                fallback(qsTranslate("MotionData", "Translation stabilization"), "Not enough of the image could be tracked"));
    }

    function test_several_features_and_a_reason_with_colons() {
        compare(formatter.getReadableError("optical_fallback:correction,translation:Decoder: error: 5"),
                fallback(qsTranslate("MotionData", "Optical correction") + qsTranslate("App", ", ") + qsTranslate("MotionData", "Translation stabilization"), "Decoder: error: 5"));
    }

    function test_a_reason_the_panel_shows_is_translated_like_the_panel() {
        compare(formatter.getReadableError("optical_fallback:translation:Needs motion data from the file"),
                fallback(qsTranslate("MotionData", "Translation stabilization"), qsTranslate("MotionData", "Needs motion data from the file")));
    }

    function test_reconstruction_fallback_for_a_clip_that_was_never_blocked() {
        compare(formatter.getReadableError("optical_fallback:reconstruction:NotConverged"),
                fallback(qsTranslate("MotionData", "Reconstruct in-camera stabilization"), "NotConverged"));
    }

    function test_skip_explains_the_failed_reconstruction() {
        compare(formatter.getReadableError("optical_skipped:reconstruction:NotConverged"),
                qsTranslate("App", "In-camera stabilization could not be reconstructed (%1).").arg("NotConverged"));
    }

    function test_other_texts_are_untouched() {
        compare(formatter.getReadableError("optical"), "optical");
        compare(formatter.getReadableError("render_failed:Disk full"), qsTranslate("App", "Rendering failed: %1").arg("Disk full"));
    }
}
