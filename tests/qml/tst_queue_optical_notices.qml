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
                fallback(qsTranslate("MotionData", "Translation stabilization"), qsTranslate("App", "Not enough of the image could be tracked")));
    }

    function test_several_features_and_a_reason_with_colons() {
        compare(formatter.getReadableError("optical_fallback:correction,translation:Decoder: error: 5"),
                fallback(qsTranslate("MotionData", "Optical correction") + qsTranslate("App", ", ") + qsTranslate("MotionData", "Translation stabilization"), qsTranslate("App", "Analysis failed. See the log for details.")));
    }

    function test_a_reason_the_panel_shows_is_translated_like_the_panel() {
        compare(formatter.getReadableError("optical_fallback:translation:Needs motion data from the file"),
                fallback(qsTranslate("MotionData", "Translation stabilization"), qsTranslate("MotionData", "Needs motion data from the file")));
    }

    function test_reconstruction_fallback_for_a_clip_that_was_never_blocked() {
        compare(formatter.getReadableError("optical_fallback:reconstruction:NotConverged"),
                fallback(qsTranslate("MotionData", "Reconstruct in-camera stabilization"), qsTranslate("App", "Analysis failed. See the log for details.")));
    }

    function test_skip_explains_the_failed_reconstruction() {
        compare(formatter.getReadableError("optical_skipped:reconstruction:NotConverged"),
                qsTranslate("App", "In-camera stabilization could not be reconstructed (%1).").arg(qsTranslate("App", "Analysis failed. See the log for details.")));
    }

    function test_known_internal_causes_use_the_existing_localized_message() {
        const feature = qsTranslate("MotionData", "Translation stabilization");
        compare(formatter.getReadableError("optical_fallback:translation:No motion data to correct"),
                fallback(feature, qsTranslate("MotionData", "Needs motion data from the file")));
        compare(formatter.getReadableError("optical_fallback:translation:NoMeasurements"),
                fallback(feature, qsTranslate("App", "The analysis gave no usable result")));
        compare(formatter.getReadableError("optical_fallback:translation:Motion data changed after analysis; analyze again"),
                fallback(feature, qsTranslate("MotionData", "Settings changed, analyze again")));
    }

    function test_unknown_or_missing_causes_never_expose_raw_diagnostics() {
        const expected = fallback(qsTranslate("MotionData", "Translation stabilization"), qsTranslate("App", "Analysis failed. See the log for details."));
        for (const cause of ["Decoder: error: 5", "SingularSystem", "GPU tracker: device lost", ""]) {
            compare(formatter.getReadableError("optical_fallback:translation:" + cause), expected);
        }
        compare(formatter.getReadableError("optical_fallback:translation"), expected);
    }

    function test_chinese_notice_contains_only_the_selected_language() {
        const feature = qsTranslate("MotionData", "Translation stabilization");
        if (feature !== "位移防抖") return;
        const tracked = formatter.getReadableError("optical_fallback:translation:Not enough of the image could be tracked");
        verify(!tracked.includes("Not enough") && !tracked.includes("could be tracked"));
        const unknown = formatter.getReadableError("optical_fallback:translation:Decoder: error: 5");
        verify(!unknown.includes("Decoder") && !unknown.includes("error"));
    }

    function test_other_texts_are_untouched() {
        compare(formatter.getReadableError("optical"), "optical");
        compare(formatter.getReadableError("render_failed:Disk full"), qsTranslate("App", "Rendering failed: %1").arg("Disk full"));
    }
}
