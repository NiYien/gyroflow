// SPDX-License-Identifier: GPL-3.0-or-later
// Run beside the application as ui/main_window.qml with target/mobile-folder-fixtures present.
import QtQuick

Item {
    id: smoke
    property var applicationWindow: null
    property var steps: []
    property int step: 0
    property int ticks: 0
    property int appliedAt: -1
    property int failures: 0
    property int checks: 0
    property int jobId: 0
    property string baselineSnapshot: ""
    property bool initialized: false
    property bool completed: false

    function check(value, message, detail) {
        checks++;
        if (value) return;
        failures++;
        console.warn("UI_LAYOUT_FAIL", steps[step] ? steps[step].label : "startup", message, JSON.stringify(detail || {}));
        if (failures <= 3 && applicationWindow) {
            const path = Qt.resolvedUrl("../../ui-layout-audit/failure-" + failures + ".png");
            applicationWindow.getApp().grabToImage(result => result.saveToFile(path));
        }
    }
    function find(item, predicate) {
        if (predicate(item)) return item;
        for (const child of item.children || []) {
            const found = find(child, predicate);
            if (found) return found;
        }
        return null;
    }
    function rect(app, item) {
        const point = item.mapToItem(app, 0, 0);
        return {x:point.x, y:point.y, width:item.width, height:item.height};
    }
    function inWindow(app, item, name) {
        check(!!item && item.visible, name + " visible");
        if (!item) return null;
        const r = rect(app, item);
        check(r.width > 0 && r.height > 0 && r.x >= -1 && r.y >= -1
            && r.x + r.width <= app.width + 1 && r.y + r.height <= app.height + 1,
            name + " inside window", r);
        return r;
    }
    function disjoint(a, b) {
        return a.x + a.width <= b.x + 1 || b.x + b.width <= a.x + 1
            || a.y + a.height <= b.y + 1 || b.y + b.height <= a.y + 1;
    }
    function snapshot(app) {
        const data = JSON.parse(app.controller.export_gyroflow_data("Simple", app.getAdditionalProjectData()));
        return JSON.stringify({stabilization:data.stabilization, gyro_source:data.gyro_source,
            offsets:data.offsets, lens:data.lens, output:data.output, synchronization:data.synchronization});
    }
    function checkLayout(app, loaded, queueShown, preserve) {
        const video = app.videoArea;
        check(video.vid.loaded === loaded, "video loaded state");
        check(video.queue.shown === queueShown, "queue state");
        if (app.isSimpleMode) {
            const actions = [];
            let actionBar = null;
            for (const name of ["simpleAutoSyncButton", "simpleExportStabilizedButton"]) {
                const button = find(app, item => item.objectName === name);
                const r = inWindow(app, button, name);
                if (button) actionBar = rect(app, button.parent.parent);
                if (r) actions.push(r);
                check(button && button.enabled === (loaded || queueShown), name + " enabled state");
            }
            if (actions.length === 2) {
                check(disjoint(actions[0], actions[1]), "primary actions do not overlap", actions);
                const sameColumn = Math.abs(actions[0].x - actions[1].x) < 1;
                const sameRow = Math.abs(actions[0].y - actions[1].y) < 1;
                check(app.isMobileLayout ? sameColumn : (app.width >= 1200 ? sameRow : sameRow || sameColumn), "primary actions align", actions);
            }
            const tools = [];
            for (const text of [qsTranslate("App", "Render queue"), queueShown ? qsTranslate("App", "Reset pairing") : qsTranslate("Popup", "Save project file"), queueShown ? qsTranslate("App", "Clear render queue") : ""]) {
                if (!text) continue;
                const tool = find(app, item => item.tooltip === text);
                const r = inWindow(app, tool, text);
                if (r) tools.push(r);
            }
            for (let i = 0; i < tools.length; ++i) {
                for (const action of actions) check(disjoint(tools[i], action), "tools do not overlap primary actions", {tool:tools[i], action:action});
                for (let j = i + 1; j < tools.length; ++j) check(disjoint(tools[i], tools[j]), "tools do not overlap each other", [tools[i], tools[j]]);
            }
            if (actionBar) {
                for (const r of actions.concat(tools)) {
                    check(r.x >= actionBar.x - 1 && r.y >= actionBar.y - 1
                        && r.x + r.width <= actionBar.x + actionBar.width + 1
                        && r.y + r.height <= actionBar.y + actionBar.height + 1,
                        "action bar reserves space for its controls", {control:r, bar:actionBar});
                }
            }
            const section = find(app, item => item.objectName === "simple-info");
            const table = section ? find(section, item => item.col1 !== undefined && item.col2 !== undefined) : null;
            check(!!table && table.visible === (loaded && section.opened), "simple metadata visibility");
        } else if (!app.isMobileLayout) {
            inWindow(app, app.renderBtn, "full-mode export");
        }
        check(video.previewArea.width > 0 && video.previewArea.height > 0, "preview has positive size");
        check(video.timeline.visible === loaded, "timeline follows loaded state");
        if (!loaded) check(Math.abs(video.previewArea.height - video.height) < 1, "empty preview reclaims timeline space");
        if (loaded && !app.isMobileLayout) check(video.previewArea.height < video.height, "desktop preview reserves timeline space");
        if (preserve) check(snapshot(app) === baselineSnapshot, "theme preserves processing parameters");
    }
    function add(label, apply, verify, ready) {
        steps.push({label:label, apply:apply, verify:verify, ready:ready});
    }
    function themeMatrix(context, loaded, queueShown) {
        for (let from = 0; from < 4; ++from) {
            add(context + " start " + from, app => app.advanced.setThemeIndex(from, false), app => checkLayout(app, loaded, queueShown, loaded));
            for (let to = 0; to < 4; ++to) {
                add(context + " " + from + "->" + to, app => app.advanced.setThemeIndex(to, false), app => checkLayout(app, loaded, queueShown, loaded));
                add(context + " return " + from, app => app.advanced.setThemeIndex(from, false), app => checkLayout(app, loaded, queueShown, loaded));
            }
        }
    }
    function prepare(app) {
        add("window", () => { applicationWindow.width = 1440; applicationWindow.height = 900; app.advanced.setThemeIndex(1, false); }, () => checkLayout(app, false, false, false));
        themeMatrix("empty", false, false);
        add("import", () => {
            app.advanced.setThemeIndex(1, false);
            app.videoArea.loadFile(Qt.resolvedUrl("../../../target/mobile-folder-fixtures/A/clip1.mp4"), true, 0, "", true);
        }, () => {
            baselineSnapshot = snapshot(app);
            checkLayout(app, true, false, true);
        }, () => !app.videoArea.previewLoading && !app.videoArea.videoLoader.active);
        themeMatrix("loaded", true, false);
        add("queue add", () => {
            jobId = render_queue.add_file(Qt.resolvedUrl("../../../target/mobile-folder-fixtures/A/clip2.mp4").toString(), "", app.getAdditionalProjectDataJson());
            app.videoArea.queue.shown = true;
        }, () => checkLayout(app, true, true, true), () => !!render_queue.get_gyroflow_data(jobId));
        themeMatrix("queue", true, true);
        add("queue preview", () => {
            app.advanced.setThemeIndex(1, false);
            app.videoArea.queue.shown = false;
            app.videoArea.loadGyroflowData(JSON.parse(render_queue.get_gyroflow_data(jobId)), jobId);
        }, () => {
            baselineSnapshot = snapshot(app);
            checkLayout(app, true, false, true);
        }, () => !app.videoArea.previewLoading && !app.videoArea.videoLoader.active);
        for (const size of [[900,600], [900,920], [1200,720], [1708,889]]) {
            for (let theme = 0; theme < 4; ++theme) {
                add("resize " + size + " theme " + theme, () => {
                    applicationWindow.width = size[0]; applicationWindow.height = size[1];
                    app.advanced.setThemeIndex(theme, false);
                    app.videoArea.queue.shown = false;
                }, () => checkLayout(app, true, false, true));
                add("resize queue " + size + " theme " + theme, () => { app.videoArea.queue.shown = true; }, () => checkLayout(app, true, true, true));
            }
        }
        add("full mode", () => {
            applicationWindow.width = 1440; applicationWindow.height = 900;
            app.advanced.setThemeIndex(1, false); app.videoArea.queue.shown = false; app.isSimpleMode = false;
        }, () => { baselineSnapshot = snapshot(app); checkLayout(app, true, false, true); });
        for (let theme = 0; theme < 4; ++theme) {
            add("full mode theme " + theme, () => app.advanced.setThemeIndex(theme, false), () => checkLayout(app, true, false, true));
        }
        add("return simple", () => {
            app.advanced.setThemeIndex(1, false); app.isSimpleMode = true;
        }, () => { baselineSnapshot = snapshot(app); checkLayout(app, true, false, true); });
        add("fullscreen", () => { app.videoArea.fullScreen = 2; }, () => {
            check(app.videoArea.width === app.width && app.videoArea.height === app.height, "fullscreen fills window");
        });
        add("exit fullscreen", () => { app.videoArea.fullScreen = 0; }, () => checkLayout(app, true, false, true));
        for (const language of ["en", "zh_TW", "zh_CN"]) {
            add("language " + language, () => {
                applicationWindow.width = 1440; applicationWindow.height = 900;
                app.videoArea.queue.shown = false; ui_tools.set_language(language);
            }, () => checkLayout(app, true, false, true));
            add("narrow language " + language, () => {
                applicationWindow.width = 900; applicationWindow.height = 600;
                app.videoArea.queue.shown = true;
            }, () => checkLayout(app, true, true, true));
        }
        add("scaling", () => {
            applicationWindow.width = 1440; applicationWindow.height = 900;
            app.videoArea.queue.shown = false; ui_tools.set_scaling(1.25);
        }, () => checkLayout(app, true, false, true));
        add("restore scaling", () => ui_tools.set_scaling(1.0), () => checkLayout(app, true, false, true));
        add("play", () => app.videoArea.vid.play(), () => check(app.videoArea.vid.playing, "play remains available"));
        add("pause", () => app.videoArea.vid.pause(), () => check(!app.videoArea.vid.playing, "pause remains available"));
    }
    Component.onCompleted: {
        console.warn("UI_LAYOUT_START");
        settings.setValue("lang", "zh_CN");
        settings.setValue("niyien_tutorial_seen_v1", "1");
        const component = Qt.createComponent(Qt.resolvedUrl("../../../src/ui/main_window.qml"));
        applicationWindow = component.createObject(null);
    }
    Timer {
        interval: 250; repeat: true; running: !smoke.completed
        onTriggered: {
            if (++ticks > 1600) {
                console.warn("UI_LAYOUT_TIMEOUT", step, steps[step] ? steps[step].label : "startup");
                completed = true; Qt.exit(1); return;
            }
            if (ticks < 20) return;
            const app = applicationWindow ? applicationWindow.getApp() : null;
            if (!app || !app.advanced || !app.stab || !app.sync || !app.motionData || !app.exportSettings || !app.videoArea.queue) return;
            app.onboardingActive = true;
            if (!initialized) { prepare(app); initialized = true; }
            if (step >= steps.length) {
                console.warn(failures ? "UI_LAYOUT_MATRIX_FAIL" : "UI_LAYOUT_MATRIX_PASS", steps.length, "cases", checks, "checks", failures, "failures");
                completed = true; applicationWindow.closeConfirmed = true; Qt.exit(failures ? 1 : 0); return;
            }
            const current = steps[step];
            if (appliedAt < 0) { current.apply(app); appliedAt = ticks; return; }
            if (ticks - appliedAt < 4 || (current.ready && !current.ready(app))) return;
            current.verify(app);
            if (["queue 0->2", "resize queue 900,600 theme 1", "narrow language en"].includes(current.label)) {
                const filename = current.label.replace(/[^a-z0-9]+/gi, "-");
                app.grabToImage(result => result.saveToFile(Qt.resolvedUrl("../../ui-layout-audit/" + filename + ".png")));
            }
            console.warn("UI_LAYOUT_CASE", step, current.label);
            step++; appliedAt = -1;
        }
    }
}
