// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2022 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Controls as QQC
import "../mobile" as Mobile

Text {
    readonly property bool mobileStyle: typeof window !== "undefined" && window.useMobileWorkspace === true
    leftPadding: 10 * dpiScale;
    onLinkActivated: (link) => Qt.openUrlExternally(link);
    color: styleTextColor;
    font.pixelSize: (mobileStyle ? Mobile.MobileStyle.body : 12) * dpiScale;
    font.family: mobileStyle ? Mobile.MobileStyle.fontFamily : styleFont;
    opacity: enabled? 1.0 : 0.6;
    linkColor: styleAccentColor;
}
