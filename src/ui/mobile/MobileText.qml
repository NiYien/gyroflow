// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2026 NiYien
import QtQuick

Text {
    property real unit: 1
    property bool dark: true
    property bool secondary: false
    property bool heading: false
    font.family: MobileStyle.fontFamily
    font.pixelSize: (heading ? MobileStyle.title : secondary ? MobileStyle.caption : MobileStyle.body) * unit
    font.weight: heading ? Font.DemiBold : Font.Normal
    color: secondary ? MobileStyle.secondary(dark) : MobileStyle.text(dark)
    verticalAlignment: Text.AlignVCenter
}
