// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright © 2021-2023 Adrian <adrian.eddy at gmail>

import QtQuick
import QtQuick.Window
import QtQuick.Controls as QQC
import QtQuick.Controls.Material

import "components/"

Window {
    id: main_window;
    width:  isMobile? Screen.desktopAvailableWidth  : Math.min(Screen.width, 1650 * dpiScale);
    height: isMobile? Screen.desktopAvailableHeight : Math.min(Screen.height, 950 * dpiScale);
    minimumWidth: (isMobile || (typeof mobileUiTest !== "undefined" && mobileUiTest) ? 280 : 900) * dpiScale;
    minimumHeight: (isMobile || (typeof mobileUiTest !== "undefined" && mobileUiTest) ? 280 : 400) * dpiScale;
    visible: false;
    color: styleBackground;
    readonly property bool fastMobileStartup: isMobile || (typeof mobileUiTest !== "undefined" && mobileUiTest);
    readonly property double startupWindowCreatedAt: Date.now();
    readonly property bool startupLoading: !!appLoader && appLoader.status === Loader.Loading;
    property var safeAreaMargins: ({});
    readonly property bool applyMobileSafeArea: Qt.platform.os === "android" || (Qt.platform.os === "ios" && appLoader.item && appLoader.item.useMobileWorkspace);
    onApplyMobileSafeAreaChanged: updateMargins.restart();
    onActiveChanged: if (active) updateMargins.restart();
    onWidthChanged: updateMargins.start();
    onHeightChanged: updateMargins.start();
    Timer {
        id: updateMargins;
        interval: 100;
        onTriggered: main_window.safeAreaMargins = ui_tools.get_safe_area_margins(main_window);
    }

    title: brandDisplayName + " " + version;

    onVisibilityChanged: {
        if (visible) updateMargins.restart();
        Qt.callLater(() => {
            if (main_window.visibility != 0)
                sett.visibility = main_window.visibility;
        });
    }

    Item {
        id: sett;
        property alias windowX: main_window.x;
        property alias windowY: main_window.y;
        property alias windowWidth: main_window.width;
        property alias windowHeight: main_window.height;
        property int visibility: 0;
        Component.onCompleted: {
            settings.init(sett);
            // [first-run-language-init] Install the UI translator deterministically here,
            // before the window is shown, so the first paint is already in the resolved
            // language. Historically the translator was only installed as a side-effect of
            // the language ComboBox in the async-loaded Advanced panel, which left the first
            // frame (and the onboarding tutorial it captures) in English. Persisted "lang"
            // wins over the OS-locale default (get_default_language only reads the system
            // locale and would override a user's manual choice).
            ui_tools.set_language(settings.value("lang", ui_tools.get_default_language()));
            main_window.visible = true;
            if (!isMobile) Qt.callLater(() => ui_tools.ensure_window_visible(main_window));
        }
        function propChanged() { settings.propChanged(sett); }
    }

    Material.theme: Material.Dark;
    Material.accent: Material.Blue;

    function getApp(): App {
        for (let i = 0; i < contentItem.children.length; ++i) {
            let x = contentItem.children[i];
            if (x instanceof Loader) x = x.item;
            if (x.objectName == "App") return x;
        }
        return null;
    }

    Component.onCompleted: {
        ui_tools.set_icon(main_window);
        if (fastMobileStartup) ui_tools.accelerate_startup(main_window);
        // Android suspend/resume video recovery: track render-surface
        // teardown so VideoArea can decide on resume whether the MDK player
        // needs a media reload (see VideoArea's Qt.application Connections).
        if (Qt.platform.os === "android") ui_tools.watch_scene_graph_invalidation(main_window);
             if (!isMobile && sett.visibility == Window.FullScreen) main_window.showFullScreen();
        else if (!isMobile && sett.visibility == Window.Maximized)  main_window.showMaximized();
        else if (!isMobile) {
            Qt.callLater(() => {
                width = width + 1;
                height = height;
            });
        } else {
            // showMaximized (not showFullScreen / show) so the Android system
            // reserves the status / nav bar regions while the window still
            // takes the rest of the screen; pairs with the non-Fullscreen
            // theme in AndroidManifest.xml. Plain show() is unreliable on
            // Qt 6.7.3 Android (some MIUI builds leave the window invisible).
            Qt.callLater(() => { main_window.showMaximized(); });
        }
        updateMargins.start();
    }
    property bool isLandscape: width > height;

    property bool closeConfirmationModal: false;
    property bool closeConfirmed: false;
    onClosing: (close) => {
        let app = getApp();
        if (app) {
            if (app.useMobileWorkspace && app.mobileUI && app.mobileUI.back()) { close.accepted = false; return; }
            close.accepted = closeConfirmed || !app.wasModified;
            if (close.accepted) {
                settings.flush();
                ui_tools.closing();
                main_controller.cancel_current_operation();
                if (typeof calib_controller !== "undefined")
                    calib_controller.cancel_current_operation();
            }
            if (!close.accepted && !closeConfirmationModal) {
                closeConfirmationModal = true;
                app.messageBox(Modal.Question, qsTr("Are you sure you want to exit?"), [
                    { text: qsTr("Yes"), accent: true, clicked: () => { main_window.closeConfirmed = true; settings.flush(); main_window.close(); } },
                    { text: qsTr("No"), clicked: () => { main_window.closeConfirmationModal = false; } }
                ], null, undefined, "quit");
            }
        }
    }

    Rectangle {
        id: libg;
        objectName: "startupLogoBackground";
        anchors.fill: loadingImage;
        anchors.margins: -20 * dpiScale;
        radius: 10 * dpiScale;
        z: 9998;
        opacity: 0.5;
        Ease on opacity { duration: 1000; }
        visible: opacity > 0 && (!main_window.fastMobileStartup || !appLoader || appLoader.status !== Loader.Ready);
        color: styleBackground;
    }
    Image {
        id: loadingImage;
        objectName: "startupLogo";
        source: "qrc:/resources/logo" + (style === "dark"? "_white" : "_black") + ".svg";
        sourceSize.width: Math.min(400 * dpiScale, parent.width * 0.7);
        visible: !main_window.fastMobileStartup || !appLoader || appLoader.status !== Loader.Ready;
        opacity: main_window.fastMobileStartup ? 1 : 0;
        YAnimator       on y       { id: liy; running: !main_window.fastMobileStartup; from: -1000; to: -1000; duration: 1000; easing.type: Easing.OutExpo; }
        OpacityAnimator on opacity { id: lio; running: !main_window.fastMobileStartup; from: 0; to: 1; duration: 1000; easing.type: Easing.OutExpo; }
        anchors.horizontalCenter: parent.horizontalCenter;
        z: 9999;
        onHeightChanged: if (loadingIndicator) updateYAnim(loadingIndicator.y, height);
        function updateYAnim(indicatorY: real, imageHeight: real): void {
            liy.stop();
            if (main_window.fastMobileStartup) {
                y = indicatorY - imageHeight - 20 * dpiScale;
                return;
            }
            liy.from = indicatorY - imageHeight - 10 * dpiScale;
            liy.to = indicatorY - imageHeight - 30 * dpiScale;
            liy.restart();
        }
    }
    Loader {
        id: appLoader;
        objectName: "AppLoader";
        anchors.fill: parent;
        // Apply safe-area insets on Android so the UI never sits under the status
        // bar or gesture-nav strip. Desktop platforms get an empty margin map and
        // degrade to 0.
        anchors.topMargin:    main_window.applyMobileSafeArea ? (main_window.safeAreaMargins.top    || 0) : 0;
        anchors.bottomMargin: main_window.applyMobileSafeArea ? (main_window.safeAreaMargins.bottom || 0) : 0;
        anchors.leftMargin:   main_window.applyMobileSafeArea ? (main_window.safeAreaMargins.left   || 0) : 0;
        anchors.rightMargin:  main_window.applyMobileSafeArea ? (main_window.safeAreaMargins.right  || 0) : 0;
        asynchronous: true;
        opacity: appLoader.status == Loader.Ready? 1 : 0.5;
        onStatusChanged: {
            if (status == Loader.Ready) {
                console.debug("[startup] app_ready window_elapsed_ms=" + (Date.now() - main_window.startupWindowCreatedAt) + " splash_tail_ms=" + (main_window.fastMobileStartup ? 0 : 1000));
                Qt.callLater(item.isMobileLayoutChanged);
                Qt.callLater(item.isLandscapeChanged);
            }
        }
        Ease on opacity { enabled: !main_window.fastMobileStartup; }
        sourceComponent: Component {
            App { objectName: "App"; }
        }
    }
    QQC.BusyIndicator {
        id: loadingIndicator;
        anchors.centerIn: parent;
        visible: !main_window.fastMobileStartup || running;
        running: appLoader.status != Loader.Ready;
        onYChanged: if (loadingImage) loadingImage.updateYAnim(y, loadingImage.height);
        onRunningChanged: if (!running) {
            if (main_window.fastMobileStartup) {
                // The interface is ready; mobile startup has no decorative delay.
                loadingImage.destroy(); libg.destroy(); destroy();
                return;
            }
            destroy(700); lio.stop(); lio.from = 1; lio.to = 0; lio.restart(); libg.opacity = 0; libg.destroy(1000); loadingImage.destroy(1000);
        }
    }
}
