// SPDX-License-Identifier: GPL-3.0-or-later
// Run beside an isolated app with source copies and a cases array supplied by the runner.
import QtQuick

Item {
    id: test
    property var cases: []
    property var applicationWindow: null
    property var ids: []
    property var loadedIds: ({})
    property int stage: 0
    property int previewIndex: 0
    property double started: Date.now()
    property double stageStarted: Date.now()
    property bool completed: false

    function check(value, message) {
        if (value) return true;
        console.warn("SONY_GEOMETRY_FAIL", stage, message);
        completed = true;
        if (applicationWindow) applicationWindow.closeConfirmed = true;
        Qt.exit(1);
        return false;
    }
    function advance(next) { console.warn("SONY_GEOMETRY_STAGE", next); stage = next; stageStarted = Date.now(); }
    function verifyProject(index) {
        const project = JSON.parse(render_queue.get_gyroflow_data(ids[index]));
        const info = project.video_info;
        const lens = project.calibration_data;
        const expected = cases[index];
        if (!check(info.width === 4992 && info.height === 3328 && info.num_frames === expected.frames && info.fps === 50,
                   "source geometry changed " + JSON.stringify(info))) return false;
        if (!check(lens && lens.fisheye_params.camera_matrix.length === 3, "no camera matrix")) return false;
        if (!check(Math.abs(lens.fisheye_params.camera_matrix[0][0] - expected.focal * expected.scale) < 0.001,
                   "wrong pixel focal " + JSON.stringify(lens))) return false;
        if (!check(lens.focal_length === expected.focal && lens.input_horizontal_stretch === 1.5,
                   "manual focal or squeeze lost")) return false;
        console.warn("SONY_GEOMETRY_PROJECT", index, JSON.stringify(lens));
        return true;
    }
    Connections {
        target: render_queue
        function onProcessing_done(id, byPreset) { if (!byPreset) test.loadedIds[id] = true; }
        function onError(id, text, arg, callback) { test.check(false, text + " " + arg); }
    }
    Component.onCompleted: {
        const component = Qt.createComponent("qrc:/src/ui/main_window.qml");
        if (!check(component.status === Component.Ready, component.errorString())) return;
        applicationWindow = component.createObject(null);
        applicationWindow.title = "【自动验收，请勿操作】Sony 镜头几何";
        applicationWindow.width = 1280;
        applicationWindow.height = 800;
        applicationWindow.show();
    }
    Timer {
        interval: 100; repeat: true; running: !test.completed
        onTriggered: {
            if (Date.now() - test.started > 300000) { test.check(false, "timeout"); return; }
            const app = test.applicationWindow ? test.applicationWindow.getApp() : null;
            if (!app || !app.advanced || !app.videoArea.queue || Date.now() - test.started < 6000) return;
            app.onboardingActive = true;
            if (test.stage === 0) {
                if (!test.check(test.cases.length === 2, "two fixtures required")) return;
                console.warn("SONY_GEOMETRY_CONFIG", "manual");
                app.controller.lens_group_manual_edit = true;
                console.warn("SONY_GEOMETRY_CONFIG", "groups");
                app.controller.set_lens_group_config(JSON.stringify(test.cases.map(function(c, i) {
                    return {lens_index: i, focal_length_mm: c.focal, anamorphic_enabled: true, squeeze_direction: "horizontal", squeeze_ratio: 1.5, lens_correction_amount: 0};
                })));
                console.warn("SONY_GEOMETRY_CONFIG", "queue");
                for (let i = 0; i < test.cases.length; ++i) {
                    const source = test.cases[i].url;
                    const id = render_queue.add_file(source, "", JSON.stringify({output: {
                        output_folder: source.substring(0, source.lastIndexOf("/") + 1),
                        output_filename: "geometry-" + i + ".mp4"
                    }}));
                    if (!test.check(id > 0, "queue rejected source")) return;
                    test.ids.push(id);
                }
                test.advance(1);
            } else if (test.stage === 1) {
                if (!test.ids.every(function(id) { return test.loadedIds[id]; })) return;
                for (let i = 0; i < test.ids.length; ++i)
                    render_queue.set_job_lens_index_override(JSON.stringify([test.ids[i]]), JSON.stringify(i));
                render_queue.reapply_lens_group_config();
                test.advance(2);
            } else if (test.stage === 2) {
                if (Date.now() - test.stageStarted < 3000) return;
                const gate = JSON.parse(render_queue.get_lens_groups_missing_data_json("sync"));
                if (!test.check(gate.jobs.length === 0 && gate.ready_count === 2, "batch blocked " + JSON.stringify(gate))) return;
                if (!test.verifyProject(0) || !test.verifyProject(1)) return;
                console.warn("SONY_GEOMETRY_GATE_PASS", JSON.stringify(gate));
                render_queue.set_jobs_optical_settings(JSON.stringify({translation: true, translation_auto: true}));
                render_queue.start_batch_autosync();
                test.advance(3);
            } else if (test.stage === 3) {
                const snapshot = JSON.parse(render_queue.get_mobile_queue_snapshot());
                if (!test.check(snapshot.rows.every(function(r) { return r.status !== "Error" && r.status !== "Skipped"; }), JSON.stringify(snapshot))) return;
                if (snapshot.workersActive || !snapshot.rows.every(function(r) { return r.status === "Finished" && r.lastExport === 2; })) return;
                if (!test.verifyProject(0) || !test.verifyProject(1)) return;
                console.warn("SONY_GEOMETRY_BATCH_PASS", JSON.stringify(snapshot));
                app.videoArea.loadGyroflowData(JSON.parse(render_queue.get_gyroflow_data(test.ids[0])), test.ids[0]);
                test.advance(4);
            } else if (test.stage === 4) {
                const video = app.videoArea.vid;
                if (app.videoArea.previewLoading || app.videoArea.videoLoader.active || !video.loaded) return;
                if (!test.check(video.videoWidth === 4992 && video.videoHeight === 3328, "preview source dimensions")) return;
                video.muted = true;
                video.seekToTimestamp(1000, true);
                test.advance(5);
            } else if (test.stage === 5) {
                const video = app.videoArea.vid;
                if (Math.abs(video.timestamp - 1000) > 40) return;
                if (++test.previewIndex < test.ids.length) {
                    app.videoArea.loadGyroflowData(JSON.parse(render_queue.get_gyroflow_data(test.ids[test.previewIndex])), test.ids[test.previewIndex]);
                    test.advance(4);
                } else {
                    console.warn("SONY_GEOMETRY_PASS native geometry, manual focal, anamorphic, batch gate, optical processing, project export and preview reload");
                    test.completed = true;
                    test.applicationWindow.closeConfirmed = true;
                    Qt.quit();
                }
            }
        }
    }
}
