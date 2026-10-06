// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest

TestCase {
    id: testCase
    name: "GpuEncoderErrors"
    property var formatter

    function cpuAdvice() {
        return qsTranslate("App", "You can also turn off \"Use GPU encoding\" and try again. Exporting will be slower.");
    }

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

    function test_unsupported_codec_explains_how_to_export() {
        const text = formatter.getReadableError("gpu_encoder_failed:codec;H.265/HEVC;1920;1080");
        verify(text.includes("H.265/HEVC"));
        compare(text, qsTranslate("App", "This graphics card does not support exporting in %1. Choose another output format.").arg("H.265/HEVC") + "\n" + cpuAdvice());
        verify(!text.includes("Encoder not found"));
    }

    function test_resolution_error_names_the_failed_dimensions() {
        const text = formatter.getReadableError("gpu_encoder_failed:resolution;H.264/AVC;3840;2160");
        verify(text.includes("3840x2160"));
        verify(text.includes("1920x1080"));
        verify(text.endsWith(cpuAdvice()));
    }

    function test_pixel_format_error_does_not_claim_a_resolution_limit() {
        const text = formatter.getReadableError("gpu_encoder_failed:pixel_format;AV1;3840;2160");
        compare(text, qsTranslate("App", "This graphics card does not support the current color format. Try another output format.") + "\n" + cpuAdvice());
        verify(!text.includes("3840x2160"));
    }

    function test_other_render_errors_keep_the_original_cause() {
        compare(formatter.getReadableError("render_failed:Disk full"), qsTranslate("App", "Rendering failed: %1").arg("Disk full"));
        compare(formatter.getReadableError("An unrelated error"), "An unrelated error");
    }
}
