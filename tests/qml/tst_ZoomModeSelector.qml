// SPDX-License-Identifier: GPL-3.0-or-later

import QtQuick
import QtTest
import "../../src/ui/components"

TestCase {
    id: testCase;
    name: "ZoomModeSelector";
    when: windowShown;
    visible: true;
    width: 400;
    height: 300;

    property real dpiScale: 1;
    property string styleFont: "Arial";
    property string style: "dark";
    property color styleButtonColor: "#333333";
    property color styleTextColor: "#ffffff";
    property color styleHighlightColor: "#666666";
    property color stylePopupBorder: "#555555";
    property bool isMobile: false;
    property bool animationsEnabled: false;

    Component { id: factory; ZoomModeSelector { width: 220; } }
    property var selector;
    property var choices;

    function init() {
        selector = createTemporaryObject(factory, testCase);
        verify(selector !== null);
        choices = findChild(selector, "zoomChoices");
        verify(choices !== null);
    }

    function test_default_and_options() {
        compare(selector.currentIndex, 1);
        compare(choices.currentIndex, 0);
        compare(choices.count, 2);
        compare(choices.textAt(0), "Dynamic zooming");
        compare(choices.textAt(1), "Static zoom");
    }

    function test_legacy_readback_does_not_select_dynamic() {
        selector.currentIndex = 0;
        compare(choices.currentIndex, -1);
        compare(selector.currentIndex, 0);
        selector.enabled = false;
        selector.enabled = true;
        compare(selector.currentIndex, 0);
        selector.currentIndex = 2;
        compare(choices.currentIndex, 1);
        selector.currentIndex = 1;
        compare(choices.currentIndex, 0);
    }

    function test_keyboard_can_leave_legacy_and_switch_both_ways() {
        selector.currentIndex = 0;
        choices.forceActiveFocus();
        keyClick(Qt.Key_Down);
        compare(selector.currentIndex, 1);
        keyClick(Qt.Key_Down);
        compare(selector.currentIndex, 2);
        keyClick(Qt.Key_Up);
        compare(selector.currentIndex, 1);
        keyClick(Qt.Key_Up);
        compare(selector.currentIndex, 1);
    }

    function test_popup_can_select_static_from_legacy() {
        selector.currentIndex = 0;
        mouseClick(choices, choices.width / 2, choices.height / 2);
        tryCompare(choices.popup, "visible", true);
        compare(choices.popup.lv.count, 2);
        const staticItem = choices.popup.lv.itemAtIndex(1);
        verify(staticItem !== null);
        mouseClick(staticItem, staticItem.width / 2, staticItem.height / 2);
        compare(selector.currentIndex, 2);
    }
}
