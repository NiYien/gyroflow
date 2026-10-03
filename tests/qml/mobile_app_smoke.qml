// SPDX-License-Identifier: GPL-3.0-or-later
// Full application harness. Copy beside the test executable as ui/main_window.qml.
import QtQuick

Item {
    id: smoke
    property var applicationWindow: null
    property var app: applicationWindow ? applicationWindow.getApp() : null
    property int stage: 0
    property int ticks: 0
    property var savedProjects: []
    property int firstId: 0
    property bool screenshotPending: false
    property int capturedStage: -1
    property bool completed: false
    property bool startupChecked: false
    property int settlingTicks: 0
    property real playbackTimestamp: -1
    property int playbackSamples: 0
    property bool folderListingRequested: false
    property var folderListing: null
    property bool fileListingRequested: false
    property var fileListing: null
    property var savedGyroRecords: []
    function findNamed(item, name) {
        if (item.objectName === name) return item;
        for (const child of item.children || []) {
            const found = findNamed(child, name);
            if (found) return found;
        }
        return null;
    }
    function differences(before, after, path) {
        if (JSON.stringify(before) === JSON.stringify(after)) return [];
        if (!before || !after || typeof before !== "object" || typeof after !== "object") return [path + ": " + JSON.stringify(before) + " -> " + JSON.stringify(after)];
        let result = [];
        for (const key of new Set(Object.keys(before).concat(Object.keys(after)))) result = result.concat(differences(before[key], after[key], path + "." + key));
        return result;
    }
    function check(condition, message) {
        if (!condition) { completed = true; console.error("MOBILE_SMOKE_FAIL", stage, message); if (applicationWindow) applicationWindow.closeConfirmed = true; Qt.exit(1); }
    }
    function capture(name) {
        if (capturedStage === stage) return true;
        capturedStage = stage;
        screenshotPending = true;
        app.grabToImage(function(result) {
            check(result.saveToFile(Qt.resolvedUrl("../../../target/mobile-smoke-" + name + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", "")), "screenshot");
            screenshotPending = false;
        });
        return false;
    }
    Component.onCompleted: {
        // Keep first-run onboarding out of interaction and layout screenshots.
        settings.setValue("lang", "zh_CN");
        settings.setValue("niyien_tutorial_seen_v1", "1");
        filesystem.mobile_folders_listed.connect(function(id, result) {
            if (id === 778) smoke.folderListing = JSON.parse(result);
            if (id === 779) smoke.fileListing = JSON.parse(result);
        });
        const component = Qt.createComponent(Qt.resolvedUrl("../../../src/ui/main_window.qml"));
        check(component.status === Component.Ready, component.errorString());
        applicationWindow = component.createObject(null);
    }
    Timer {
        interval: 16; repeat: true; running: !smoke.completed && (smoke.stage === 3 || smoke.stage === 5)
        onTriggered: {
            if (!smoke.app || !smoke.app.mobileUI.previewReady) return;
            const video = smoke.app.videoArea.vid;
            if (!video.playing) return;
            const wrapped = smoke.playbackTimestamp > video.duration - 150 && video.timestamp < 150;
            smoke.check(wrapped || video.timestamp + 2 >= smoke.playbackTimestamp, "opening playback must not seek backwards during warm-up");
            smoke.playbackTimestamp = video.timestamp;
            smoke.playbackSamples++;
        }
    }
    Timer {
        interval: 300; repeat: true; running: !smoke.completed
        onTriggered: {
            if (++smoke.ticks > 300) { smoke.check(false, "timeout"); return; }
            if (!smoke.app || smoke.screenshotPending) return;
            const app = smoke.app;
            if (mobileUiTest && !smoke.startupChecked) {
                const loader = smoke.findNamed(smoke.applicationWindow.contentItem, "AppLoader");
                if (loader && loader.status === Loader.Ready) {
                    const logo = smoke.findNamed(smoke.applicationWindow.contentItem, "startupLogo");
                    const background = smoke.findNamed(smoke.applicationWindow.contentItem, "startupLogoBackground");
                    smoke.check(!logo || !logo.visible, "mobile logo must disappear as soon as the app is ready");
                    smoke.check(!background || !background.visible, "mobile logo background must not linger");
                    smoke.check(loader.opacity === 1, "ready mobile interface must not wait for a fade-in");
                    if (+settings.value("theme", -1) === -1)
                        smoke.check(style === "light", "mobile first launch must default to light");
                    smoke.startupChecked = true;
                    console.log("MOBILE_STARTUP_PASS no decorative delay after readiness");
                }
            }
            app.onboardingActive = true;
            for (const child of app.children) {
                if (child.opened && child.text === qsTranslate("App", "Available updates") && typeof child.close === "function") {
                    child.close();
                    return;
                }
            }
            const ui = app.mobileUI;
            if (!mobileUiTest) {
                if (!app.advanced || smoke.ticks < 20) return;
                smoke.applicationWindow.width = 940;
                smoke.applicationWindow.height = 500;
                smoke.check(!app.useMobileWorkspace && !ui, "desktop isolation");
                const savedLanguage = settings.value("lang", "en");
                const savedStyle = style;
                ui_tools.set_language("en");
                ui_tools.set_language("en");
                smoke.check(qsTranslate("MobileWorkspace", "Videos") === "Videos", "English switch and repeated application");
                ui_tools.set_language("zh_CN");
                smoke.check(qsTranslate("MobileWorkspace", "Videos") === "视频", "language changes must still retranslate");
                ui_tools.set_language(savedLanguage);
                ui_tools.set_theme("dark"); ui_tools.set_theme("dark");
                smoke.check(style === "dark", "dark theme and repeated application");
                ui_tools.set_theme("light");
                smoke.check(style === "light", "theme changes must still apply");
                ui_tools.set_theme(savedStyle);
                console.log("MOBILE_SMOKE_PASS desktop isolation");
                smoke.completed = true; smoke.applicationWindow.closeConfirmed = true;
                Qt.quit(); return;
            }
            if (!ui || (smoke.stage < 8 && !app.mobileSettingsReady) || !app.advanced || !app.simpleMounting) return;
            if (smoke.stage === 0) {
                app.advanced.langList.currentIndex = 1;
                app.advanced.setThemeIndex(0, false);
                smoke.applicationWindow.width = 360;
                smoke.applicationWindow.height = 640;
                if (!smoke.folderListingRequested) {
                    smoke.folderListingRequested = true;
                    filesystem.list_mobile_folders(Qt.resolvedUrl("../../../target/mobile-folder-fixtures"), 778);
                    return;
                }
                if (!smoke.folderListing) return;
                smoke.check(!smoke.folderListing.error, "folder enumeration error");
                const urls = smoke.folderListing.folders.map(folder => folder.url);
                smoke.check(urls.length === 2, "two selectable folders");
                if (!smoke.fileListingRequested) {
                    smoke.fileListingRequested = true;
                    filesystem.folder_access_granted(Qt.resolvedUrl("../../../target/mobile-folder-fixtures"));
                    const locations = JSON.parse(filesystem.get_mobile_locations());
                    smoke.check(locations.some(url => url.includes("mobile-folder-fixtures")), "saved browser location");
                    filesystem.list_mobile_folders(urls[0], 779);
                    return;
                }
                if (!smoke.fileListing) return;
                smoke.check(!smoke.fileListing.error && smoke.fileListing.files.length === 3, "three selectable files in authorized folder");
                app.importMobileFiles(urls, false);
                smoke.stage++;
            } else if (smoke.stage === 1) {
                if (ui.rows.length !== 6 || ui.busy || ui.rows.some(r => !render_queue.get_gyroflow_data(r.id))) return;
                smoke.check(ui.columns === 1, "portrait columns");
                if (!smoke.capture("library")) return;
                ui.selectionMode = true;
                ui.toggleSelection(ui.rows[1].id);
                app.batchState.smoothness = 61;
                app.flushMobileSettings();
                smoke.stage++;
            } else if (smoke.stage === 2) {
                if (ui.busy) return;
                if (++smoke.settlingTicks < 5) return;
                for (const row of ui.rows) {
                    const params = JSON.parse(render_queue.get_job_display_params(row.id));
                    smoke.check(Math.abs(params.smoothness - 0.61) < 0.001, "global setting for " + row.id);
                }
                smoke.savedProjects = ui.rows.map(r => render_queue.get_gyroflow_data(r.id));
                smoke.firstId = ui.rows[0].id;
                ui.openInfo(smoke.firstId);
                smoke.stage = 20;
            } else if (smoke.stage === 20) {
                if (!ui.previewReady) return;
                smoke.check(ui.page === "library" && ui.panel === "info", "information opens directly from library");
                smoke.check(!app.videoArea.vid.playing, "information never autoplays");
                smoke.check(Object.keys(ui.videoInfo).length > 0, "information metadata loaded");
                if (!smoke.capture("direct-info")) return;
                ui.panel = "";
                ui.openPreview(smoke.firstId);
                smoke.stage = 3;
            } else if (smoke.stage === 3) {
                if (!ui.previewReady) return;
                if (smoke.playbackSamples < 30) return;
                smoke.check(app.videoArea.vid.playing, "tap opens and plays");
                smoke.check(ui.selectionCount === 1, "preview preserves selection");
                smoke.check(!ui.stablePreview, "unmatched video uses original");
                if (!smoke.capture("player")) return;
                app.videoArea.vid.pause();
                smoke.applicationWindow.width = 800;
                smoke.applicationWindow.height = 360;
                ui.controlsShown = true;
                smoke.stage = 31;
            } else if (smoke.stage === 31) {
                if (!smoke.capture("player-landscape")) return;
                smoke.applicationWindow.width = 360;
                smoke.applicationWindow.height = 640;
                ui.showPanel("info");
                smoke.stage = 4;
            } else if (smoke.stage === 4) {
                smoke.check(!app.videoArea.vid.playing, "info pauses playback");
                smoke.check(Object.keys(ui.videoInfo).length > 0, "video information");
                if (!smoke.capture("info")) return;
                ui.panel = "";
                smoke.playbackTimestamp = -1; smoke.playbackSamples = 0;
                ui.navigate(1);
                smoke.stage++;
            } else if (smoke.stage === 5) {
                if (!ui.previewReady) return;
                if (smoke.playbackSamples < 30) return;
                smoke.check(render_queue.editing_job_id === ui.rows[1].id, "next uses stable id");
                smoke.check(app.batchState.smoothness === 61, "preview does not overwrite globals");
                ui.returnToList();
                smoke.applicationWindow.width = 800;
                smoke.applicationWindow.height = 360;
                smoke.stage++;
            } else if (smoke.stage === 6) {
                smoke.check(ui.columns === 2 && ui.selectionCount === 1, "rotation preserves selection");
                smoke.check(!app.videoArea.vid.playing, "return pauses");
                if (!smoke.capture("landscape")) return;
                ui.showPanel("settings");
                smoke.stage++;
            } else if (smoke.stage === 7) {
                if (!smoke.capture("settings")) return;
                for (let i = 0; i < ui.rows.length; ++i) {
                    const diff = smoke.differences(JSON.parse(smoke.savedProjects[i]), JSON.parse(render_queue.get_gyroflow_data(ui.rows[i].id)), "job" + i);
                    smoke.check(!diff.length, "browse must not write " + diff.join("; "));
                }
                app.batchState.horizonLock = true;
                app.batchState.horizonLockAmount = 37;
                smoke.applicationWindow.width = 384;
                smoke.applicationWindow.height = 800;
                smoke.stage++;
            } else if (smoke.stage === 8) {
                smoke.check(smoke.findNamed(ui, "mobileHorizonLock").valueText === "37%", "horizon percentage follows global settings");
                if (!smoke.capture("settings-portrait")) return;
                ui.settingsTab = 1;
                ui.panelFlickable.contentY = 0;
                smoke.stage++;
            } else if (smoke.stage === 9) {
                if (!smoke.capture("settings-lens")) return;
                ui.settingsTab = 2;
                ui.panelFlickable.contentY = 0;
                smoke.stage++;
            } else if (smoke.stage === 10) {
                if (!smoke.capture("settings-preferences")) return;
                ui.dismissPanel();
                ui.finishSelection();
                smoke.stage = 12;
            } else if (smoke.stage === 11) {
                smoke.check(!app.useMobileWorkspace && app.videoArea.parent !== ui.previewHost, "diagnostic mode restores video parent");
                app.isSimpleMode = true;
                smoke.stage = 14;
            } else if (smoke.stage === 12) {
                if (!smoke.capture("library-actions")) return;
                const base = Qt.resolvedUrl("../../../target/mobile-folder-fixtures").toString();
                ui.pendingFolderImport = {url: base + "/A", kind: "video"};
                ui.showPanel("folderConfirm");
                smoke.stage = 16;
            } else if (smoke.stage === 16) {
                if (!smoke.capture("folder-confirm")) return;
                ui.back();
                smoke.check(ui.panel === "" && !ui.pendingFolderImport && ui.rows.length === 6, "cancel confirmation does not import");
                smoke.savedGyroRecords = ui.queueService.gyroFilesInfo;
                ui.queueService.gyroFilesInfo = [{filename: "2025-11-21_143000_mix.bin", parsed: true, duration_ms: 125000, created_at_ms: 1763735400000}];
                smoke.findNamed(ui, "mobileGyroData").expanded = true;
                smoke.stage = 15;
            } else if (smoke.stage === 15) {
                if (!smoke.capture("gyro-expanded")) return;
                smoke.findNamed(ui, "mobileGyroData").expanded = false;
                ui.queueService.gyroFilesInfo = smoke.savedGyroRecords;
                // Exercise the success UI without dispatching a matching worker.
                ui.deepStarted(smoke.firstId);
                smoke.stage = 17;
            } else if (smoke.stage === 17) {
                if (!smoke.capture("search-progress")) return;
                ui.deepFinished(smoke.firstId, true, "", 0);
                smoke.stage = 18;
            } else if (smoke.stage === 18) {
                if (!smoke.capture("search-result")) return;
                ui.back();
                smoke.stage = 13;
            } else if (smoke.stage === 13) {
                smoke.check(ui.continueAfterSummary && ui.selectionCount === 0, "search success exposes next actions");
                if (!smoke.capture("search-success")) return;
                ui.operation = null; ui.summary = "";
                app.isSimpleMode = false;
                smoke.stage = 11;
            } else if (smoke.stage === 14) {
                smoke.check(app.videoArea.parent === ui.previewHost && ui.rows.length === 6, "mobile workspace restores without rebuilding queue");
                ui.platformOs = "ios";
                ui.returnToList(); ui.showPanel("add");
                smoke.applicationWindow.width = 393; smoke.applicationWindow.height = 759;
                smoke.stage = 19;
            } else if (smoke.stage === 19) {
                smoke.check(smoke.findNamed(ui, "mobileChoosePhotos").visible, "iOS photos entry is direct");
                if (!smoke.capture("ios-add")) return;
                smoke.applicationWindow.width = 734; smoke.applicationWindow.height = 372;
                smoke.stage = 21;
            } else if (smoke.stage === 21) {
                if (!smoke.capture("ios-add-landscape")) return;
                ui.platformOs = Qt.platform.os;
                console.log("MOBILE_SMOKE_PASS two-folder import, global edit, preview, direct information without playback, navigation, rotation, browse isolation, mode switch, iOS import layout");
                ui.panel = ""; ui.selectionMode = false; ui.returnToList();
                smoke.completed = true; smoke.applicationWindow.closeConfirmed = true;
                Qt.quit();
            }
        }
    }
}
