// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/mobile" as Mobile
import "../../src/ui/mobile/MobileLogic.js" as Logic

TestCase {
    id: test
    name: "MobileWorkspace"
    when: windowShown
    width: 900; height: 900
    visible: true
    property var workspace
    property var samples: []
    QtObject {
        id: fakeBackend
        property string status: "stopped"
        property int editing_job_id: 0
        signal queue_changed()
        signal queue_finished()
        signal render_progress(int job_id, real progress, int current_frame, int total_frames, bool finished, real start_time, bool is_conversion)
        function get_mobile_queue_snapshot() { return JSON.stringify({ rows: test.samples, workersActive: false, deepActive: false }); }
    }
    Component { id: factory; Mobile.MobileWorkspace { backend: fakeBackend; unit: 1; dark: true } }
    Component { id: foldersFactory; Mobile.MobileFolderPicker { width: 360; height: 520; unit: 1; dark: false } }
    Component { id: inputFactory; TextInput { width: 120; height: 48; text: "35" } }
    Component { id: videoCardFactory; Mobile.MobileVideoCard { height: 72; unit: 1; dark: false } }
    Component { id: settingsFactory; Mobile.MobileSettings { unit: 1; dark: false; host: previewService } }
    QtObject {
        id: previewService
        property QtObject batchState: QtObject {
            property real smoothness: 50
            property bool horizonLock: false
            property real horizonLockAmount: 100
            property bool autoRotate: false
            property real zoomMode: 1
            property real lensCorrection: 1
        }
        property bool mobileAutoRotateAvailable: false
        property var exportSettings: null
        property var advanced: null
        property int comparisonCalls: 0
        function setMobileComparison(stable) { comparisonCalls++; }
        function openMobilePreview(id) { fakeBackend.editing_job_id = id; }
        property QtObject controller: QtObject {
            property bool gyro_loaded: true
            property bool video_loading_in_progress: false
            property bool loading_gyro_in_progress: false
            function stabilize_step_pending_for_preview() { return false; }
        }
        property QtObject videoArea: QtObject {
            property real mobilePreviewAspectRatio: 16 / 9
            property bool queueEditLoading: false
            property var queue: null
            property QtObject vid: QtObject {
                property bool playing: false
                property bool loaded: true
                property real duration: 1000
                property real timestamp: 0
                function play() { playing = true; }
                function pause() { playing = false; }
                function seekToTimestamp(value, exact) { timestamp = value; }
            }
        }
    }
    QtObject {
        id: folderFilesystem
        property var locations: ["file:///take"]
        signal mobile_folders_listed(int request_id, string result)
        function list_mobile_folders(url, id) {}
        function display_url(url) { return url; }
        function get_mobile_locations() { return JSON.stringify(locations); }
    }
    function init() {
        fakeBackend.editing_job_id = 0;
        previewService.comparisonCalls = 0;
        previewService.videoArea.queueEditLoading = false;
        previewService.videoArea.mobilePreviewAspectRatio = 16 / 9;
        previewService.videoArea.vid.loaded = true;
        previewService.videoArea.vid.playing = false;
        previewService.controller.video_loading_in_progress = false;
        previewService.controller.loading_gyro_in_progress = false;
        folderFilesystem.locations = ["file:///take"];
        samples = [];
        for (let i = 0; i < 32; ++i) samples.push({ id: i + 1, filename: "C" + (1000 + i) + ".mov", duration: 18500,
            thumbnail: "", status: "Queued", processing: 0, frame: 0, frames: 100, sync: "{}", error: "", previewable: true, focalLength: 50, manualLens: false, lensGroup: 3 });
        workspace = createTemporaryObject(factory, test, { width: 360, height: 640 });
        verify(workspace !== null);
        workspace.refresh();
        compare(workspace.rows.length, 32);
        waitForRendering(workspace);
    }
    function test_density_data() {
        return [{ tag: "portrait", w: 360, h: 640, columns: 1, count: 5 },
            { tag: "landscape", w: 800, h: 360, columns: 2, count: 6 },
            { tag: "compact", w: 640, h: 320, columns: 2, count: 4 }];
    }
    function test_density(data) {
        workspace.width = data.w; workspace.height = data.h;
        waitForRendering(workspace);
        compare(workspace.columns, data.columns);
        verify(Math.floor(workspace.libraryView.height / workspace.libraryView.cellHeight) * workspace.columns >= data.count);
        const button = findChild(workspace, "mobileSettingsButton");
        verify(button !== null);
        const point = button.mapToItem(workspace, button.width, button.height);
        verify(point.x <= workspace.width, "Settings must remain inside the viewport");
        let saved = false;
        workspace.grabToImage(function(result) {
            saved = result.saveToFile(Qt.resolvedUrl("../../target/mobile-ui-" + data.tag + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", ""));
        });
        tryVerify(() => saved, 5000);
    }
    function test_selection_and_back_never_dispatch() {
        const select = findChild(workspace.libraryView, "mobileCardSelect");
        mouseClick(select, select.width / 2, select.height / 2);
        verify(workspace.selectionMode);
        workspace.toggleSelection(7);
        compare(workspace.selectionCount, 2);
        workspace.showPanel("settings");
        verify(workspace.back());
        compare(workspace.selectionCount, 2);
        verify(workspace.back());
        compare(workspace.selectionCount, 0);
        compare(workspace.operation, null);
        compare(fakeBackend.status, "stopped");
    }
    function test_focused_input_remains_visible_when_keyboard_resizes_window() {
        workspace.showPanel("settings");
        const panel = workspace.panelFlickable;
        panel.contentHeight = 1200;
        const input = createTemporaryObject(inputFactory, panel.contentItem, { y: 1000 });
        input.forceActiveFocus();
        verify(input.activeFocus);
        workspace.height = 400;
        tryVerify(() => input.mapToItem(panel, 0, input.height).y <= panel.height - 15);
        verify(input.mapToItem(panel, 0, 0).y >= 0);
    }
    function test_selection_controls_stay_at_bottom() {
        const title = findChild(workspace, "mobilePageTitle");
        const originalTitle = title.text;
        const select = findChild(workspace.libraryView, "mobileCardSelect");
        mouseClick(select, select.width / 2, select.height / 2);
        compare(workspace.selectionCount, 1);
        compare(title.text, originalTitle);
        const cancel = findChild(workspace, "mobileCancelSelection");
        waitForRendering(workspace);
        verify(cancel.visible);
        verify(cancel.mapToItem(workspace, 0, 0).y > workspace.height / 2);
        const all = findChild(workspace, "mobileSelectAll");
        const allLabel = all.text;
        mouseClick(all, all.width / 2, all.height / 2);
        compare(workspace.selectionCount, workspace.rows.length);
        compare(all.text, allLabel);
        verify(!all.enabled);
        mouseClick(cancel, cancel.width / 2, cancel.height / 2);
        verify(!workspace.selectionMode);
        compare(workspace.selectionCount, 0);
        mouseClick(select, select.width / 2, select.height / 2);
        mouseClick(select, select.width / 2, select.height / 2);
        verify(!workspace.selectionMode);
        compare(workspace.operation, null);
    }
    function test_explicit_play_and_information_buttons() {
        const play = findChild(workspace.libraryView, "mobileCardPlay");
        verify(play !== null && play.visible);
        mouseClick(play, play.width / 2, play.height / 2);
        compare(workspace.page, "preview");
        compare(workspace.selectionCount, 0);
        workspace.returnToList();
        const info = findChild(workspace.libraryView, "mobileCardInfo");
        verify(info !== null && info.visible);
        mouseClick(info, info.width / 2, info.height / 2);
        compare(workspace.page, "library");
        compare(workspace.panel, "info");
        compare(workspace.selectionCount, 0);
        workspace.panel = "";
        workspace.selectionMode = true;
        workspace.toggleSelection(2);
        mouseClick(play, play.width / 2, play.height / 2);
        compare(workspace.page, "preview");
        compare(workspace.selectionCount, 1);
        workspace.returnToList();
        mouseClick(info, info.width / 2, info.height / 2);
        compare(workspace.panel, "info");
        compare(workspace.selectionCount, 1);
    }
    function test_skip_reasons_data() {
        return [
            {tag: "no_gyro", reason: "no_gyro", label: "No gyroscope data", chinese: "无陀螺仪数据"},
            {tag: "calibration", reason: "calibration", label: "Calibration pair", chinese: "校准配对视频"},
            {tag: "plugin", reason: "plugin_only", label: "Plugin stabilization only", chinese: "仅支持插件稳定"},
            {tag: "camera", reason: "image_stabilization", label: "In-camera stabilization on", chinese: "机内防抖已开启"},
            {tag: "stopped", reason: "user_stopped", label: "Stopped manually", chinese: "已手动停止"},
            {tag: "missing", reason: "", label: "Skip reason not recorded", chinese: "未记录跳过原因"},
            {tag: "unknown", reason: "future_reason", label: "Skip reason not recorded", chinese: "未记录跳过原因"}
        ];
    }
    function test_skip_reasons(data) {
        const record = {filename: "C0002.MP4", status: "Skipped", skipReason: data.reason};
        compare(workspace.rowStatus(record), data.label);
        const card = createTemporaryObject(videoCardFactory, test, {width: 288, record: record, statusText: data.chinese});
        verify(card !== null);
        waitForRendering(card);
        const status = findChild(card, "mobileVideoStatus");
        verify(status.visible);
        verify(!status.truncated, "Chinese skip reason must fit a 320-wide phone list");
        if (data.reason === "image_stabilization") verify(workspace.skipMessage(data.reason).includes("Turn off stabilization"));
        if (data.reason === "calibration") verify(workspace.skipMessage(data.reason).includes("calibration pair"));
    }
    function test_skip_error_fallback() {
        compare(workspace.rowStatus({status: "Skipped", skipReason: "future_reason", error: "The source file was removed"}), "The source file was removed");
    }
    function test_preview_loading_keeps_intermediate_sizes_covered() {
        workspace.host = previewService;
        workspace.openPreview(1);
        const cover = findChild(workspace, "mobilePreviewLoadingCover");
        const initialHeight = workspace.previewHost.height;
        verify(cover.visible);
        previewService.videoArea.queueEditLoading = true;
        previewService.videoArea.mobilePreviewAspectRatio = 1;
        workspace.previewLoaded();
        compare(workspace.previewHost.height, initialHeight);
        verify(cover.visible);
        previewService.controller.video_loading_in_progress = true;
        previewService.videoArea.queueEditLoading = false;
        workspace.previewLoaded();
        verify(cover.visible);
        previewService.controller.loading_gyro_in_progress = true;
        previewService.controller.video_loading_in_progress = false;
        workspace.previewLoaded();
        verify(cover.visible);
        previewService.videoArea.vid.loaded = false;
        previewService.controller.loading_gyro_in_progress = false;
        workspace.previewLoaded();
        verify(cover.visible);
        previewService.videoArea.mobilePreviewAspectRatio = 9 / 16;
        previewService.videoArea.vid.loaded = true;
        workspace.previewLoaded();
        verify(!cover.visible);
        compare(workspace.previewAspectRatio, 9 / 16);
        verify(previewService.videoArea.vid.playing);
        compare(previewService.comparisonCalls, 1);
        workspace.previewLoaded();
        compare(previewService.comparisonCalls, 1);
        previewService.videoArea.mobilePreviewAspectRatio = 16 / 9;
        compare(workspace.previewAspectRatio, 16 / 9);
        workspace.openPreview(2);
        verify(cover.visible);
        workspace.returnToList();
        verify(!cover.visible);
        workspace.previewLoaded();
        verify(!previewService.videoArea.vid.playing);
    }
    function test_preview_controls_layout_data() {
        return [{tag: "phone", w: 360, h: 760, ratio: 16 / 9},
            {tag: "small", w: 320, h: 480, ratio: 16 / 9},
            {tag: "vertical-video", w: 360, h: 760, ratio: 9 / 16},
            {tag: "landscape", w: 800, h: 360, ratio: 16 / 9},
            {tag: "short-landscape", w: 568, h: 320, ratio: 16 / 9},
            {tag: "tablet", w: 768, h: 1024, ratio: 16 / 9}];
    }
    function test_preview_controls_layout(data) {
        previewService.videoArea.mobilePreviewAspectRatio = data.ratio;
        previewService.videoArea.vid.playing = false;
        workspace.host = previewService;
        workspace.width = data.w; workspace.height = data.h;
        workspace.page = "preview"; workspace.previewJobId = 1; workspace.previewReady = true;
        waitForRendering(workspace);
        const preview = findChild(workspace, "mobilePreviewHost");
        const controls = findChild(workspace, "mobilePlaybackControls");
        const play = findChild(workspace, "mobilePlayPause");
        verify(preview.height > 0);
        verify(controls.y >= workspace.headerHeight);
        verify(controls.y + controls.height <= data.h - 12);
        if (!workspace.landscape) {
            compare(controls.y - preview.y - preview.height, 8);
            verify(preview.y >= workspace.headerHeight + 12);
            compare(play.width, 64); compare(play.height, 64);
        }
        for (const name of ["mobilePreviousVideo", "mobilePlayPause", "mobileNextVideo"]) {
            const button = findChild(workspace, name);
            verify(button.width >= 48 && button.height >= 48);
            const position = button.mapToItem(workspace, 0, 0);
            verify(position.y + button.height <= data.h - 12);
        }
        verify(play.emphasized);
        mouseClick(play, play.width / 2, play.height / 2);
        verify(previewService.videoArea.vid.playing);
        mouseClick(play, play.width / 2, play.height / 2);
        verify(!previewService.videoArea.vid.playing);
    }
    function test_horizon_percentage_inline_data() {
        return [{tag: "narrow", w: 280}, {tag: "phone", w: 328}, {tag: "wide", w: 640}];
    }
    function test_horizon_percentage_inline(data) {
        previewService.batchState.horizonLock = false;
        previewService.batchState.horizonLockAmount = 100;
        const settings = createTemporaryObject(settingsFactory, test, {width: data.w});
        verify(settings !== null);
        const toggle = findChild(settings, "mobileHorizonLock");
        const percent = findChild(toggle, "mobileToggleValue");
        const slider = findChild(settings, "mobileHorizonLockAmount");
        verify(!percent.visible);
        const rowHeight = toggle.height;
        mouseClick(toggle, toggle.width - 20, toggle.height / 2);
        compare(percent.text, "100%"); verify(percent.visible);
        compare(toggle.height, rowHeight);
        for (const value of [0, 37, 100]) {
            slider.value = value; slider.moved();
            compare(previewService.batchState.horizonLockAmount, value);
            compare(percent.text, value + "%");
        }
        const position = percent.mapToItem(toggle, 0, 0);
        verify(position.x >= 0);
        verify(position.x + percent.width <= toggle.indicator.x - 12);
        verify(Math.abs(position.y + percent.height / 2 - toggle.height / 2) <= 0.5,
            "Inline values stay centered within cross-platform font rounding");
        slider.value = 37; slider.moved();
        mouseClick(toggle, toggle.width - 20, toggle.height / 2);
        verify(!percent.visible); compare(previewService.batchState.horizonLockAmount, 37);
        mouseClick(toggle, toggle.width - 20, toggle.height / 2);
        compare(percent.text, "37%"); verify(percent.visible);
    }
    function test_ios_import_entries_data() {
        return [{tag: "iphone-se", w: 320, h: 548}, {tag: "iphone-safe-area", w: 393, h: 759},
            {tag: "iphone-landscape-safe-area", w: 734, h: 372}, {tag: "ipad", w: 1024, h: 1322},
            {tag: "ipad-split-view", w: 375, h: 980}];
    }
    function test_ios_import_entries(data) {
        workspace.platformOs = "ios";
        workspace.width = data.w; workspace.height = data.h;
        let photos = 0, files = 0, folders = 0;
        workspace.queueService = {importBusy: false, gyroFilesInfo: [],
            requestMobilePhotos: () => photos++, requestMobileFiles: () => files++,
            requestMobileFolderLocation: callback => folders++};
        workspace.showPanel("add");
        waitForRendering(workspace);
        const photo = findChild(workspace, "mobileChoosePhotos");
        const file = findChild(workspace, "mobileChooseVideos");
        const folder = findChild(workspace, "mobileChooseVideoFolders");
        verify(photo.visible && file.visible && folder.visible);
        verify(folder.mapToItem(workspace, 0, folder.height).y <= workspace.height);
        mouseClick(photo, photo.width / 2, photo.height / 2);
        compare(photos, 1); compare(files, 0); compare(workspace.panel, "");
        workspace.showPanel("add");
        mouseClick(file, file.width / 2, file.height / 2);
        compare(files, 1); compare(photos, 1); compare(workspace.panel, "");
        workspace.showPanel("add");
        mouseClick(folder, folder.width / 2, folder.height / 2);
        compare(folders, 1); compare(workspace.panel, "");
        workspace.showPanel("add"); workspace.importTab = 1;
        verify(!photo.visible);
        workspace.importTab = 0; workspace.platformOs = "android";
        verify(!photo.visible);
    }
    function swipeIosEdge(dx, dy) {
        const panelEdge = findChild(workspace, "mobileIosPanelBackEdge");
        const edge = panelEdge && panelEdge.visible ? panelEdge : findChild(workspace, "mobileIosBackEdge");
        verify(edge.visible);
        const start = edge.mapToItem(workspace, 8, Math.min(100, edge.height / 2));
        const touch = touchEvent(workspace);
        touch.press(0, workspace, start.x, start.y).commit();
        for (let step = 1; step <= 6; ++step) {
            touch.move(0, workspace, start.x + dx * step / 6, start.y + dy * step / 6).commit();
            wait(16);
        }
        touch.release(0, workspace, start.x + dx, start.y + dy).commit();
        wait(20);
    }
    function touchTapAt(x, y) {
        const touch = touchEvent(workspace);
        touch.press(0, workspace, x, y).commit();
        wait(16);
        touch.release(0, workspace, x, y).commit();
        wait(20);
    }
    function touchSwipeAt(x, y, dx, dy) {
        touchSwipeOn(workspace, x, y, dx, dy);
    }
    function touchSwipeOn(item, x, y, dx, dy) {
        const touch = touchEvent(item);
        touch.press(0, item, x, y).commit();
        for (let step = 1; step <= 8; ++step) {
            touch.move(0, item, x + dx * step / 8, y + dy * step / 8).commit();
            wait(16);
        }
        touch.release(0, item, x + dx, y + dy).commit();
        wait(20);
    }
    function test_settings_toggle_by_touch_data() {
        return [{tag: "portrait", w: 393, h: 759}, {tag: "landscape", w: 734, h: 372}];
    }
    function test_settings_toggle_by_touch(data) {
        workspace.platformOs = "ios"; workspace.width = data.w; workspace.height = data.h;
        const settings = findChild(workspace, "mobileSettingsButton");
        waitForRendering(workspace);
        const p = settings.mapToItem(workspace, settings.width / 2, settings.height / 2);
        touchTapAt(p.x, p.y);
        compare(workspace.panel, "settings");
        touchTapAt(p.x, p.y);
        compare(workspace.panel, "", "Tapping the settings position again must close it");
    }
    function test_settings_title_is_a_back_target() {
        workspace.platformOs = "ios";
        workspace.showPanel("settings");
        waitForRendering(workspace);
        const sheet = findChild(workspace, "mobileSheet");
        const title = sheet.children.find(child => child.visible && child.text === "Settings");
        verify(title !== undefined);
        const p = title.mapToItem(workspace, title.width / 2, title.height / 2);
        touchTapAt(p.x, p.y);
        compare(workspace.panel, "", "The settings label must share the back button's hit area");
    }
    function test_ios_back_from_screen_edge_data() {
        return [{tag: "portrait", w: 393, h: 759}, {tag: "landscape", w: 734, h: 372}];
    }
    function test_ios_back_from_screen_edge(data) {
        workspace.platformOs = "ios"; workspace.width = data.w; workspace.height = data.h;
        workspace.showPanel("settings");
        waitForRendering(workspace);
        touchSwipeAt(6, data.h / 2, 130, 0);
        compare(workspace.panel, "", "Back gestures originate at the screen edge in both orientations");
    }
    function test_ios_back_crosses_left_safe_area() {
        workspace.platformOs = "ios"; workspace.x = 44; workspace.width = 646; workspace.height = 372;
        workspace.screenLeftInset = 44;
        workspace.showPanel("settings");
        waitForRendering(workspace);
        touchSwipeOn(test, 6, 180, 130, 0);
        compare(workspace.panel, "", "The landscape safe-area margin must not swallow edge touches");
    }
    function test_ios_settings_header_swipes_data() {
        return [{tag: "right", dx: 110, dy: 0, closes: true},
            {tag: "left", dx: -110, dy: 0, closes: true},
            {tag: "short", dx: 30, dy: 0, closes: false},
            {tag: "diagonal", dx: 90, dy: 110, closes: false}];
    }
    function test_ios_settings_header_swipes(data) {
        workspace.platformOs = "ios"; workspace.width = 393; workspace.height = 759;
        workspace.showPanel("settings");
        waitForRendering(workspace);
        touchSwipeAt(200, 28, data.dx, data.dy);
        compare(workspace.panel, data.closes ? "" : "settings");
    }
    function test_ios_back_preserves_nested_navigation_and_controls() {
        workspace.platformOs = "ios"; workspace.host = previewService;
        workspace.width = 393; workspace.height = 759;
        workspace.showPanel("settings"); workspace.showPanel("privacy");
        waitForRendering(workspace);
        touchSwipeAt(200, 28, 110, 0);
        compare(workspace.panel, "settings");
        compare(workspace.panelTrail.length, 0);
        previewService.batchState.horizonLock = true;
        previewService.batchState.horizonLockAmount = 50;
        waitForRendering(workspace);
        const slider = findChild(workspace, "mobileHorizonLockAmount");
        verify(slider.visible);
        const p = slider.mapToItem(workspace, slider.width / 2, slider.height / 2);
        touchSwipeAt(p.x, p.y, 85, 0);
        compare(workspace.panel, "settings", "Changing a parameter must not navigate away");
        verify(previewService.batchState.horizonLockAmount > 50);
        workspace.forceActiveFocus();
        keyClick(Qt.Key_Escape);
        compare(workspace.panel, "");
        previewService.batchState.horizonLock = false;
    }
    function test_ios_edge_back_data() {
        return [{tag: "settings-phone", panel: "settings", w: 393, h: 759},
            {tag: "settings-landscape", panel: "settings", w: 734, h: 372},
            {tag: "result", panel: "deepResult", w: 393, h: 759},
            {tag: "preview", panel: "", w: 393, h: 759}];
    }
    function test_ios_edge_back(data) {
        workspace.platformOs = "ios"; workspace.width = data.w; workspace.height = data.h;
        if (data.panel) workspace.showPanel(data.panel); else workspace.page = "preview";
        waitForRendering(workspace);
        swipeIosEdge(12, 0);
        compare(workspace.panel, data.panel);
        if (!data.panel) compare(workspace.page, "preview");
        swipeIosEdge(120, 0);
        compare(workspace.panel, ""); compare(workspace.page, "library");
        compare(workspace.operation, null);
        if (data.panel) workspace.showPanel(data.panel); else workspace.page = "preview";
        swipeIosEdge(40, 0);
        compare(workspace.panel, data.panel);
        swipeIosEdge(3, 100);
        compare(workspace.panel, data.panel);
        if (!data.panel) compare(workspace.page, "preview");
    }
    function test_import_routes_data() {
        return [{ tag: "portrait", w: 360, h: 640 }, { tag: "landscape", w: 800, h: 360 }, { tag: "narrow", w: 280, h: 640 }];
    }
    function test_import_routes(data) {
        workspace.width = data.w; workspace.height = data.h;
        let picked = 0;
        let videosPicked = 0;
        workspace.queueService = { importBusy: false, gyroFilesInfo: [], requestMobileGyroFiles: function() { picked++; }, requestMobileFiles: function() { videosPicked++; } };
        compare(workspace.gyroBarHeight, 0);
        const add = findChild(workspace, "mobileAddButton");
        mouseClick(add, add.width / 2, add.height / 2);
        compare(workspace.panel, "add");
        waitForRendering(workspace);
        const videos = findChild(workspace, "mobileChooseVideos");
        mouseClick(videos, videos.width / 2, videos.height / 2);
        compare(videosPicked, 1);
        compare(workspace.panel, "");
        mouseClick(add, add.width / 2, add.height / 2);
        waitForRendering(workspace);
        const tabs = findChild(workspace, "mobilePanelTabs");
        const sheet = findChild(workspace, "mobileSheet");
        const sheetHeight = sheet.height;
        const videoFilesY = videos.mapToItem(workspace, 0, 0).y;
        const external = findChild(tabs, "mobileTab1");
        mouseClick(external, external.width / 2, external.height / 2);
        compare(workspace.panel, "add");
        compare(workspace.importTab, 1);
        compare(workspace.panelTrail.length, 0);
        waitForRendering(workspace);
        const gyro = findChild(workspace, "mobileAddGyroButton");
        compare(sheet.height, sheetHeight);
        compare(gyro.mapToItem(workspace, 0, 0).y, videoFilesY);
        compare(gyro.text, videos.text);
        compare(findChild(workspace, "mobileChooseVideoFolders").text, findChild(workspace, "mobileChooseGyroFolders").text);
        mouseClick(gyro, gyro.width / 2, gyro.height / 2);
        compare(picked, 1);
        workspace.panel = "";
        verify(findChild(workspace, "mobileMoreButton") === null);
        const settings = findChild(workspace, "mobileSettingsButton");
        mouseClick(settings, settings.width / 2, settings.height / 2);
        compare(workspace.panel, "settings");
        verify(!videos.visible && !gyro.visible);
        workspace.panel = "";
        samples = []; workspace.refresh();
        waitForRendering(workspace);
        verify(!add.visible);
        compare(workspace.footerHeight, 0);
        const empty = findChild(workspace, "mobileEmptyAddButton");
        mouseClick(empty, empty.width / 2, empty.height / 2);
        compare(workspace.panel, "add");
        verify(workspace.back());
        compare(workspace.panel, "");
    }
    function test_settings_tabs_and_panel_back() {
        workspace.showPanel("settings");
        const tabs = findChild(workspace, "mobilePanelTabs");
        compare(tabs.model.length, 3);
        for (let i = 0; i < 3; ++i) {
            waitForRendering(workspace);
            const tab = findChild(tabs, "mobileTab" + i);
            mouseClick(tab, tab.width / 2, tab.height / 2);
            compare(workspace.settingsTab, i);
            compare(workspace.panel, "settings");
            compare(workspace.panelTrail.length, 0);
            compare(workspace.settingsContent.visible, i === 1);
            compare(findChild(workspace, "mobileOutputSettings").visible, i === 2);
            compare(findChild(workspace, "mobileAppPreferences").visible, i === 2);
        }
        workspace.back();
        workspace.showPanel("settings");
        waitForRendering(workspace);
        const tasks = findChild(workspace, "mobileTaskButton");
        compare(tasks, null);
        workspace.back();
        compare(workspace.panel, "");
        workspace.showPanel("sources");
        workspace.showPanel("folders");
        workspace.back();
        compare(workspace.panel, "");
        compare(workspace.page, "library");
        compare(workspace.importTab, 1);
    }
    function test_folder_buttons_open_native_picker_directly_data() {
        return [{tag: "video", kind: "video", tab: 0, button: "mobileChooseVideoFolders"},
            {tag: "gyro", kind: "gyro", tab: 1, button: "mobileChooseGyroFolders"}];
    }
    function test_folder_buttons_open_native_picker_directly(data) {
        let requests = 0;
        let accept = null;
        let submitted = [];
        workspace.browseFilesInApp = true;
        workspace.filesystemService = folderFilesystem;
        workspace.queueService = {importBusy: false, gyroFilesInfo: [],
            requestMobileFolderLocation: (callback, direct) => { verify(direct, "Folder imports must request a directly cancellable browser"); requests++; accept = callback; },
            dt: {loadFiles: urls => submitted = urls}, addMobileGyroUrls: urls => submitted = urls};
        workspace.showPanel("add"); workspace.importTab = data.tab;
        waitForRendering(workspace);
        const button = findChild(workspace, data.button);
        mouseClick(button, button.width / 2, button.height / 2);
        compare(requests, 1);
        compare(workspace.panel, "");
        compare(workspace.panelTrail.length, 0);
        compare(submitted.length, 0);
        accept("file:///chosen");
        compare(submitted.length, 0);
        compare(workspace.panel, "folderConfirm");
        compare(findChild(workspace, "mobileFolderConfirmationPath").text, "file:///chosen");
        workspace.confirmFolderImport();
        compare(submitted.length, 1);
        compare(submitted[0], "file:///chosen");
        compare(workspace.panel, "");
        workspace.confirmFolderImport();
        compare(submitted.length, 1);
    }
    function test_gyro_list_expands_without_opening_import() {
        workspace.queueService = {importBusy: false, gyroFilesInfo: [
            {filename: "2025-11-21_13-17-34_recording_mix.bin", parsed: true, duration_ms: 125000, created_at_ms: 1763735400000},
            {filename: "second_mix.bin", parsed: false}]};
        const data = findChild(workspace, "mobileGyroData");
        const button = findChild(data, "mobileGyroSummary");
        const list = findChild(data, "mobileGyroRecordList");
        waitForRendering(workspace);
        const initialY = workspace.libraryView.y;
        mouseClick(button, button.width / 2, button.height / 2);
        waitForRendering(workspace);
        verify(data.expanded && list.visible);
        compare(list.count, 2);
        const filename = findChild(list, "mobileGyroFilename");
        const duration = findChild(list, "mobileGyroDuration");
        compare(duration.text, "02:05");
        compare(filename.y, duration.y);
        compare(filename.height, duration.height);
        verify(filename.x + filename.width + 8 <= duration.x, "Filename and duration must share one row without overlap");
        verify(duration.x + duration.width <= list.width - 12);
        compare(workspace.panel, "");
        verify(workspace.libraryView.y > initialY);
        workspace.showPanel("add"); workspace.importTab = 1;
        compare(findChild(findChild(workspace, "mobileSheet"), "mobileGyroRecordList"), null);
        workspace.dismissPanel();
        mouseClick(button, button.width / 2, button.height / 2);
        verify(!data.expanded);
        compare(workspace.libraryView.y, initialY);
    }
    function test_cancel_folder_import_exits_without_submitting() {
        workspace.showPanel("add");
        workspace.showPanel("folders");
        const picker = findChild(workspace, "mobileFolderPicker");
        picker.currentUrl = "file:///take/A/B";
        picker.ancestors = ["file:///take", "file:///take/A"];
        let submissions = 0;
        picker.accepted.connect(() => submissions++);
        verify(workspace.back());
        compare(workspace.panel, "");
        compare(workspace.panelTrail.length, 0);
        compare(workspace.page, "library");
        compare(submissions, 0);
    }
    function test_android_file_browser_reuses_location_data() {
        return [{tag: "video", kind: "video", filename: "clip.mp4"}, {tag: "gyro", kind: "gyro", filename: "take_mix.bin"}];
    }
    function test_android_file_browser_reuses_location(data) {
        let nativeCalls = 0;
        let submitted = [];
        workspace.browseFilesInApp = true;
        workspace.filesystemService = folderFilesystem;
        workspace.queueService = {importBusy: false, gyroFilesInfo: [], mobileVideoExtensions: ["mp4"],
            requestMobileFiles: () => nativeCalls++, requestMobileGyroFiles: () => nativeCalls++,
            requestMobileFolderLocation: () => nativeCalls++,
            dt: {loadFiles: urls => submitted = urls}, addMobileGyroUrls: urls => submitted = urls};
        workspace.showPanel("add");
        if (data.kind === "gyro") workspace.requestGyro(); else workspace.requestAdd(false);
        compare(workspace.panel, "files");
        const picker = findChild(workspace, "mobileFolderPicker");
        verify(picker.choosingLocation);
        const locations = findChild(picker, "mobileLocations");
        waitForRendering(workspace);
        mouseClick(locations, 100, 24);
        compare(picker.currentUrl, "file:///take");
        folderFilesystem.mobile_folders_listed(picker.requestId, JSON.stringify({url: picker.currentUrl,
            folders: [{name: "A", url: "file:///take/A"}], files: [
                {name: "clip.mp4", url: "file:///take/clip.mp4"},
                {name: "take_mix.bin", url: "file:///take/take_mix.bin"},
                {name: "notes.txt", url: "file:///take/notes.txt"}]}));
        compare(picker.entries.length, 2);
        compare(picker.entries[1].name, data.filename);
        const list = findChild(picker, "mobileFolderList");
        waitForRendering(workspace);
        let saved = false;
        workspace.grabToImage(result => { saved = result.saveToFile(Qt.resolvedUrl("../../target/mobile-browser-" + data.kind + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", "")); });
        tryVerify(() => saved);
        mouseClick(list, 100, 78);
        compare(picker.selectedCount, 1);
        compare(picker.currentUrl, "file:///take");
        const add = findChild(picker, "mobileConfirmFolders");
        mouseClick(add, add.width / 2, add.height / 2);
        compare(submitted.length, 1);
        compare(submitted[0], "file:///take/" + data.filename);
        compare(workspace.panel, "");
        compare(nativeCalls, 0);
        if (data.kind === "gyro") workspace.requestGyro(); else workspace.requestAdd(false);
        picker.ancestors = ["file:///take", "file:///take/A"];
        picker.currentUrl = "file:///take/A/B";
        workspace.back();
        compare(workspace.panel, "");
        compare(workspace.page, "library");
        compare(submitted.length, 1);
        compare(nativeCalls, 0);
    }
    function test_continue_processing_from_results_data() {
        return [{tag: "task-portrait", panel: "task", w: 360, h: 640},
            {tag: "task-landscape", panel: "task", w: 800, h: 360},
            {tag: "deep-result", panel: "deepResult", w: 360, h: 640}];
    }
    function test_continue_processing_from_results(data) {
        workspace.width = data.w; workspace.height = data.h;
        workspace.showPanel(data.panel);
        const actions = findChild(workspace, "mobileContinueProcessing");
        const exportButton = findChild(workspace, "mobileContinueExport");
        workspace.engineBusy = true;
        verify(!actions.visible);
        workspace.engineBusy = false;
        waitForRendering(workspace);
        verify(actions.visible);
        const actionTop = actions.mapToItem(workspace, 0, 0).y;
        workspace.panelFlickable.contentY = 400;
        compare(actions.mapToItem(workspace, 0, 0).y, actionTop);
        verify(workspace.panelFlickable.mapToItem(workspace, 0, workspace.panelFlickable.height).y <= actionTop);
        verify(actions.mapToItem(workspace, 0, actions.height).y <= workspace.height);
        let saved = false;
        workspace.grabToImage(result => { saved = result.saveToFile(Qt.resolvedUrl("../../target/mobile-continue-" + data.tag + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", "")); });
        tryVerify(() => saved);
        mouseClick(exportButton, exportButton.width / 2, exportButton.height / 2);
        compare(workspace.panel, "");
        compare(workspace.operation.kind, "export");
        compare(workspace.operation.targets.length, workspace.rows.length);
    }
    function test_focal_length_and_lens_group_are_exclusive() {
        compare(workspace.lensLabel({manualLens: false, focalLength: 50, lensGroup: 3}), "50 mm");
        compare(workspace.lensLabel({manualLens: true, focalLength: 50, lensGroup: 3}), "L3");
        verify(!workspace.lensLabel({manualLens: true, focalLength: 50}).includes("L1"));
        verify(!workspace.lensLabel({manualLens: false, focalLength: null, lensGroup: 3}).includes("L3"));
    }
    function test_folder_results_accept_encoded_names_and_ignore_stale_requests() {
        const picker = createTemporaryObject(foldersFactory, test, {filesystemService: folderFilesystem});
        picker.browse("file:///test/中文 space", []);
        folderFilesystem.mobile_folders_listed(picker.requestId, JSON.stringify({url: "file:///test/%E4%B8%AD%E6%96%87%20space", folders: [{name: "A", url: "file:///test/A"}]}));
        compare(picker.loading, false);
        compare(picker.folders.length, 1);
        picker.browse("file:///older", []);
        const olderId = picker.requestId;
        picker.browse("file:///newer", []);
        folderFilesystem.mobile_folders_listed(olderId, JSON.stringify({url: "file:///older", folders: [{name: "old", url: "file:///old"}]}));
        compare(picker.loading, true);
        compare(picker.folders.length, 0);
    }
    function test_folder_confirmation_cancel_data() {
        return [{tag: "video-back", kind: "video", depth: 3, close: false},
            {tag: "gyro-back", kind: "gyro", depth: 5, close: false},
            {tag: "video-close", kind: "video", depth: 2, close: true}];
    }
    function test_folder_resumes_with_trailing_slash_in_permission() {
        folderFilesystem.locations = ["file:///take/"];
        const picker = createTemporaryObject(foldersFactory, test, {filesystemService: folderFilesystem});
        picker.currentUrl = "file:///take/中文 space";
        picker.ancestors = ["file:///take"];
        verify(picker.start("gyro", false));
        compare(picker.currentUrl, "file:///take/中文 space");
        compare(picker.ancestors.length, 1);
    }
    function test_folder_confirmation_cancel(data) {
        let nativeCalls = 0, submitted = [];
        let accept = null;
        workspace.browseFilesInApp = true;
        workspace.filesystemService = folderFilesystem;
        workspace.queueService = {importBusy: false, gyroFilesInfo: [],
            requestMobileFolderLocation: callback => { nativeCalls++; accept = callback; },
            dt: {loadFiles: urls => submitted = urls}, addMobileGyroUrls: urls => submitted = urls};
        workspace.showPanel("add");
        workspace.requestFolder(data.kind);
        compare(workspace.panel, "");
        compare(nativeCalls, 1);
        accept("file:///test/" + "中文文件夹/".repeat(data.depth));
        compare(workspace.panel, "folderConfirm");
        waitForRendering(workspace);
        compare(submitted.length, 0);
        let saved = false;
        workspace.grabToImage(result => { saved = result.saveToFile(Qt.resolvedUrl("../../target/mobile-folder-confirm-" + data.tag + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", "")); });
        tryVerify(() => saved);
        if (data.close) {
            const cancel = findChild(workspace, "mobileCancelFolderImport");
            mouseClick(cancel, cancel.width / 2, cancel.height / 2);
        }
        else { workspace.forceActiveFocus(); keyClick(Qt.Key_Back); }
        compare(workspace.panel, "");
        compare(workspace.page, "library");
        compare(workspace.panelTrail.length, 0);
        compare(workspace.pendingFolderImport, null);
        workspace.confirmFolderImport();
        compare(submitted.length, 0);
    }
    function test_main_actions_and_success_next_step_data() {
        return [{tag: "portrait", w: 360, h: 640}, {tag: "landscape", w: 800, h: 360}, {tag: "narrow-landscape", w: 640, h: 320}];
    }
    function test_main_actions_and_success_next_step(data) {
        workspace.width = data.w; workspace.height = data.h;
        waitForRendering(workspace);
        const tools = findChild(workspace, "mobileLibraryActions");
        verify(tools.visible);
        verify(tools.mapToItem(workspace, 0, 0).y > workspace.height / 2);
        if (!workspace.wideFooter) compare(tools.width, findChild(workspace, "mobilePrimaryStabilize").parent.width);
        verify(findChild(tools, "mobileResetPairing").enabled);
        verify(findChild(tools, "mobileClearQueue").enabled);
        const reset = findChild(tools, "mobileResetPairing");
        const clear = findChild(tools, "mobileClearQueue");
        verify(!reset.quiet && !clear.quiet, "Management actions need visible button backgrounds");
        reset.text = "重置配对"; clear.text = "清空队列";
        verify(reset.width >= reset.implicitWidth, "Reset label must fit on one line");
        verify(clear.width >= clear.implicitWidth, "Clear label must fit on one line");
        workspace.toggleSelection(1);
        workspace.deepStarted(1);
        workspace.deepStage = "Scanning segment 1 of 4";
        verify(!findChild(workspace, "mobileTaskSecondaryStatus").visible);
        verify(!findChild(tools, "mobileClearQueue").enabled);
        workspace.showTaskDetails(0);
        verify(!findChild(workspace, "mobileTaskHeading").text.includes("Scanning"));
        workspace.deepFinished(1, true, "", 200);
        workspace.back();
        compare(workspace.panel, "");
        verify(workspace.continueAfterSummary);
        const stabilize = findChild(workspace, "mobilePrimaryStabilize");
        const exportButton = findChild(workspace, "mobilePrimaryExport");
        verify(stabilize.visible && exportButton.visible && exportButton.emphasized && !stabilize.emphasized);
        verify(stabilize.background.color !== exportButton.background.color);
        compare(stabilize.text, "Stabilize (for plugins)");
        verify(!findChild(workspace, "mobileTaskStatusAction").visible);
        compare(workspace.selectionCount, 0);
        let saved = false;
        workspace.grabToImage(result => { saved = result.saveToFile(Qt.resolvedUrl("../../target/mobile-success-next-" + data.tag + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", "")); });
        tryVerify(() => saved);
        mouseClick(stabilize, stabilize.width / 2, stabilize.height / 2);
        compare(workspace.operation.kind, "sync");
    }
    function test_drag_selection_survives_rotation() {
        workspace.beginDrag(2);
        workspace.selection = Logic.selectionRange(workspace.rows, workspace.dragBase, 2, 8, true);
        compare(workspace.selectionCount, 7);
        workspace.width = 800; workspace.height = 360;
        compare(workspace.dragSelecting, false);
        compare(workspace.selectionCount, 7);
        verify(workspace.libraryView.interactive);
    }
    function test_range_reversal_and_pruning() {
        const original = { 1: true, 12: true };
        const forward = Logic.selectionRange(workspace.rows, original, 2, 7, true);
        compare(Object.keys(forward).length, 8);
        const reverse = Logic.selectionRange(workspace.rows, original, 2, 3, true);
        compare(Object.keys(reverse).length, 4);
        compare(original[3], undefined);
        compare(Object.keys(Logic.pruneSelection(workspace.rows.slice(0, 5), reverse)).length, 3);
    }
    function test_history_and_duplicate_completion_not_double_counted() {
        const before = [{ id: 1, status: "Finished", lastExport: 4 }, { id: 2, status: "Queued" }];
        let op = Logic.beginOperation(1, "export", before);
        op = Logic.touchOperation(op, 2);
        const after = [{ id: 1, status: "Finished", lastExport: 4 }, { id: 2, status: "Finished", lastExport: 4 }];
        op = Logic.observeOperation(op, after, false);
        op = Logic.observeOperation(op, after, false);
        compare(Logic.counts(op).success, 1);
        compare(Logic.counts(op).settled, 1);
        op = Logic.observeOperation(op, after, true);
        compare(Logic.counts(op).success, 1);
        compare(Logic.counts(op).skipped, 1);
    }
    function test_cancel_preserves_completed_results() {
        let op = Logic.beginOperation(1, "sync", [{ id: 1, status: "Queued" }, { id: 2, status: "Queued" }]);
        op.stopping = true;
        op = Logic.observeOperation(op, [{ id: 1, status: "Finished", lastExport: 2 }, { id: 2, status: "Queued" }], true);
        compare(Logic.counts(op).success, 1);
        compare(Logic.counts(op).cancelled, 1);
        compare(op.active, false);
    }
    function test_close_task_panel_does_not_cancel() {
        workspace.operation = Logic.beginOperation(4, "deep", workspace.rows.slice(0, 1));
        workspace.panel = "task";
        workspace.back();
        compare(workspace.panel, "");
        compare(workspace.operation.id, 4);
        compare(workspace.operation.active, true);
    }
    function test_sync_completion_is_not_export_completion() {
        let op = Logic.beginOperation(2, "export", [{ id: 1, status: "Queued", epoch: 3 }]);
        op = Logic.observeOperation(op, [{ id: 1, status: "Finished", lastExport: 2, epoch: 4 }], false);
        compare(Logic.counts(op).success, 0);
        op = Logic.observeOperation(op, [{ id: 1, status: "Rendering", lastExport: null, epoch: 5 }], false);
        compare(Logic.counts(op).success, 0);
        op = Logic.observeOperation(op, [{ id: 1, status: "Finished", lastExport: 4, epoch: 5 }], true);
        compare(Logic.counts(op).success, 1);
    }
    function test_preview_preserves_reference_only_projects() {
        const reference = { project_file: "file:///saved.gyroflow" };
        compare(Logic.previewSnapshot(reference).project_file, reference.project_file);
        const committed = { project_file: "file:///older.gyroflow", videofile: "file:///clip.mp4", stabilization: { adaptive_zoom_window: 0 } };
        const preview = Logic.previewSnapshot(committed);
        compare(preview.project_file, undefined);
        compare(preview.stabilization.adaptive_zoom_window, 0);
        compare(committed.project_file, "file:///older.gyroflow");
    }
    function test_refused_dispatch_returns_to_buttons() {
        workspace.startAction("sync");
        tryCompare(workspace, "operation", null, 1500);
        verify(!workspace.busy);
    }
    function test_import_blocks_processing_until_files_have_loaded() {
        workspace.queueService = { importBusy: true };
        verify(workspace.busy);
        workspace.startAction("sync");
        compare(workspace.operation, null);
        workspace.queueService = { importBusy: false };
        verify(!workspace.busy);
    }
    function test_deep_layout_data() {
        return [{ tag: "portrait", w: 360, h: 800 }, { tag: "landscape", w: 800, h: 360 }, { tag: "narrow", w: 320, h: 568 }];
    }
    function test_deep_layout(data) {
        workspace.width = data.w; workspace.height = data.h;
        workspace.deepStarted(1);
        waitForRendering(workspace);
        const progress = findChild(workspace, "mobileTaskProgress");
        verify(progress.visible && progress.height >= 6);
        const point = progress.mapToItem(workspace, 0, progress.height);
        verify(workspace.height - point.y >= 16, "Progress must have bottom breathing room");
        const cancel = findChild(workspace, "mobileTaskStatusAction");
        verify(cancel.mapToItem(workspace, 0, cancel.height).y <= point.y - progress.height - 8);
        workspace.showTaskDetails(0);
        verify(findChild(workspace, "mobileTaskDetailProgress").visible);
        workspace.deepFinished(1, true, "", 200);
        waitForRendering(workspace);
        const sheet = findChild(workspace, "mobileSheet");
        const scroll = workspace.panelFlickable;
        compare(scroll.contentHeight, 0, "Success result only needs its title and processing buttons");
        verify(sheet.height <= workspace.height - 32);
        verify(scroll.height <= scroll.contentHeight + 1, "Result must not stretch empty space");
        verify(!findChild(workspace, "mobileTaskHeading").visible, "Do not repeat the success heading");
        const actions = findChild(workspace, "mobileContinueProcessing");
        verify(actions.visible);
        verify(actions.y >= scroll.y + scroll.height);
        let saved = false;
        workspace.grabToImage(function(result) {
            saved = result.saveToFile(Qt.resolvedUrl("../../target/mobile-deep-result-" + data.tag + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", ""));
        });
        tryVerify(() => saved, 5000);
    }
    function test_deep_success_does_not_stabilize_or_repeat() {
        workspace.deepStarted(1);
        workspace.deepFinished(1, true, "", 200);
        compare(workspace.panel, "deepResult");
        compare(workspace.operation.kind, "deep");
        compare(workspace.operation.active, false);
        workspace.panel = "";
        workspace.deepFinished(1, true, "", 200);
        compare(workspace.panel, "");
        verify(workspace.rowStatus({ status: "Finished", deepMatched: true }).includes("Deep search complete"));
    }
    function test_scroll_and_panel_preserve_anchor() {
        workspace.libraryView.contentY = 480;
        workspace.rememberList();
        const anchor = workspace.savedAnchorId;
        workspace.showPanel("settings");
        workspace.back();
        compare(workspace.libraryView.contentY, 480);
        workspace.width = 800; workspace.height = 360;
        waitForRendering(workspace);
        workspace.restoreList();
        compare(workspace.savedAnchorId, anchor);
        compare(workspace.libraryView.contentY, Math.floor((anchor - 1) / workspace.columns) * workspace.libraryView.cellHeight + workspace.savedAnchorOffset);
    }
    function test_touch_scroll_is_not_selection() {
        const grid = workspace.libraryView;
        mousePress(grid, 180, 390);
        mouseMove(grid, 180, 340, 30);
        mouseMove(grid, 180, 240, 30);
        mouseMove(grid, 180, 140, 30);
        mouseRelease(grid, 180, 140);
        tryVerify(() => !grid.moving, 4000);
        verify(grid.contentY > 0);
        compare(workspace.selectionCount, 0);
        verify(workspace.savedAnchorId > 1);
    }
    function test_long_press_then_tap_selects_without_preview() {
        const grid = workspace.libraryView;
        mousePress(grid, 160, 35);
        wait(650);
        mouseRelease(grid, 160, 35);
        compare(workspace.selectionCount, 1);
        mouseClick(grid, 160, 131);
        compare(workspace.selectionCount, 2);
        compare(workspace.page, "library");
        compare(workspace.dragSelecting, false);
    }
    function test_visual_variants_data() {
        return [{ tag: "light", w: 360, h: 640, dark: false, scale: 1, panel: "" },
            { tag: "large-type", w: 360, h: 640, dark: true, scale: 1.4, panel: "" },
            { tag: "tablet-portrait", w: 768, h: 1024, dark: false, scale: 1, panel: "" },
            { tag: "tablet-landscape", w: 1024, h: 768, dark: true, scale: 1, panel: "" },
            { tag: "settings", w: 800, h: 360, dark: true, scale: 1, panel: "settings" },
            { tag: "settings-portrait", w: 360, h: 640, dark: false, scale: 1, panel: "settings" },
            { tag: "settings-export", w: 360, h: 640, dark: false, scale: 1, panel: "settings", tab: 2 },
            { tag: "settings-app", w: 360, h: 640, dark: true, scale: 1, panel: "settings", tab: 2 },
            { tag: "folders", w: 800, h: 360, dark: true, scale: 1, panel: "folders" },
            { tag: "add", w: 360, h: 640, dark: false, scale: 1, panel: "add" },
            { tag: "selection", w: 360, h: 640, dark: false, scale: 1, panel: "", selection: true },
            { tag: "gyro", w: 360, h: 640, dark: false, scale: 1, panel: "sources" }];
    }
    function test_mobile_privacy_and_update_entries_data() {
        return [{tag: "ios", os: "ios", updates: false}, {tag: "android", os: "android", updates: true}];
    }
    function test_mobile_privacy_and_update_entries(data) {
        const settings = createTemporaryObject(settingsFactory, test, {width: 360, section: "app", platformOs: data.os});
        verify(settings !== null);
        compare(findChild(settings, "mobileAppUpdates").visible, data.updates);
        const privacy = findChild(settings, "mobilePrivacyPolicy");
        verify(privacy.visible);
        let document = "";
        settings.documentRequested.connect(kind => document = kind);
        privacy.clicked();
        compare(document, "privacy");
        workspace.showPanel("settings");
        workspace.showPanel(document);
        verify(workspace.documentPanel);
        workspace.back();
        compare(workspace.panel, "settings");
    }
    function test_large_type_metadata_remains_readable() {
        workspace.width = 360; workspace.height = 760; workspace.unit = 1.4;
        waitForRendering(workspace);
        const metadata = findChild(workspace, "mobileVideoMetadata");
        verify(!metadata.truncated);
        const project = findChild(workspace, "mobilePrimaryStabilize");
        const video = findChild(workspace, "mobilePrimaryExport");
        verify(project.width > workspace.width * 0.7);
        verify(video.y >= project.y + project.height);
        verify(video.mapToItem(workspace, 0, video.height).y <= workspace.height);
    }
    function test_visual_variants(data) {
        workspace.width = data.w; workspace.height = data.h; workspace.dark = data.dark; workspace.unit = data.scale;
        samples[0].filename = "超长文件名_Stabilisiertes_Video_mit_sehr_langem_Dateinamen_2026_09_08.mov";
        workspace.refresh();
        workspace.showPanel(data.panel);
        workspace.settingsTab = data.tab || 0;
        if (data.selection) { workspace.toggleSelection(1); workspace.toggleSelection(3); }
        waitForRendering(workspace);
        const button = findChild(workspace, "mobileSettingsButton");
        verify(button.mapToItem(workspace, button.width, 0).x <= workspace.width);
        let saved = false;
        workspace.grabToImage(function(result) { saved = result.saveToFile(Qt.resolvedUrl("../../target/mobile-ui-" + data.tag + ".png").toString().replace(Qt.platform.os === "windows" ? "file:///" : "file://", "")); });
        tryVerify(() => saved, 5000);
    }
}
