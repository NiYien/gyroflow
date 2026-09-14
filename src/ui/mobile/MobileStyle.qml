// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
pragma Singleton
import QtQuick

QtObject {
    readonly property string fontFamily: Qt.platform.os === "windows" ? "Microsoft YaHei UI" : typeof mobileFont !== "undefined" ? mobileFont
        : Qt.platform.os === "android" ? "Roboto" : Qt.platform.os === "ios" ? ".AppleSystemUIFont" : "Segoe UI"
    readonly property int body: 16
    readonly property int caption: 14
    readonly property int title: 20
    function background(dark) { return dark ? "#13161b" : "#f4f5f7"; }
    function surface(dark) { return dark ? "#1c2027" : "#ffffff"; }
    function fill(dark) { return dark ? "#2b3039" : "#eceef1"; }
    function text(dark) { return dark ? "#edf0f4" : "#242a33"; }
    function secondary(dark) { return dark ? "#a0a9b6" : "#657080"; }
    function separator(dark) { return dark ? "#343b46" : "#dce0e6"; }
    function accent(dark) { return dark ? "#78a9ff" : "#245ac8"; }
}
