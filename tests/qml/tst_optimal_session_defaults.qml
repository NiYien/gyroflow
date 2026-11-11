// SPDX-License-Identifier: GPL-3.0-or-later
import QtQuick
import QtTest
import "../../src/ui/menu" as Menu

TestCase {
    id: testCase
    name: "OptimalSessionDefaults"
    when: windowShown
    visible: true
    width: 540; height: 900
    property real dpiScale: 1
    property string styleFont: "Arial"
    property string style: "dark"
    property string styleButtonColor: "#333333"
    property color styleTextColor: "#ffffff"
    property color styleTextColorOnAccent: "#ffffff"
    property color styleAccentColor: "#3a9cde"
    property color styleBackground: "#111111"
    property color styleBackground2: "#222222"
    property color styleHighlightColor: "#666666"
    property string stylePopupBorder: "#555555"
    property color styleSliderHandle: "#888888"
    property color styleSliderBackground: "#444444"
    property color styleVideoBorderColor: "#444444"
    property color styleHrColor: "#444444"
    property bool isMobile: false
    property bool isMobileLayout: false
    property bool animationsEnabled: false
    property bool isLandscape: true
    property var smoothingAlgorithms: ["No smoothing", "Default", "Plain 3D", "Fixed camera", "Optimal path"]

    QtObject {
        id: settingsStub
        property var values: ({"smoothing-4-smoothness": 0.15, "smoothing-1-smoothness": 0.7, "maxZoom": 180, "maxZoomIterations": 12})
        property var reads: []
        property var writes: []
        function value(key, fallback) { reads.push(key); return values[key] !== undefined ? values[key] : fallback; }
        function setValue(key, value) { writes = writes.concat([key]); values[key] = value; }
        function init(item) { }
        function propChanged(item) { }
    }
    property alias settings: settingsStub
    QtObject {
        id: controllerStub
        signal compute_progress(real id, real progress)
        signal telemetry_loaded(bool main, string filename, string camera, var data)
        signal rolling_shutter_estimated(real value)
        signal keyframes_changed()
        signal keyframe_value_updated(string key, real value)
        signal gyroflow_file_loaded(var data)
        signal smoothing_param_changed(string name, real value)
        property bool has_gravity_vectors: false
        property bool has_per_frame_focal_length: false
        property bool focal_length_smoothing_enabled: false
        property real focal_length_max_zoom_rate: 50
        property real frame_readout_time: 0
        property int frame_readout_direction: 0
        property real video_speed: 1
        property bool video_speed_affects_smoothing: false
        property bool video_speed_affects_zooming: false
        property bool video_speed_affects_zooming_limit: false
        property real adaptive_zoom_window: 4
        property real adaptive_zoom: 4
        property int zooming_method: 1
        property real zooming_center_x: 0
        property real zooming_center_y: 0
        property real additional_rotation_x: 0
        property real additional_rotation_y: 0
        property real additional_rotation_z: 0
        property real additional_translation_x: 0
        property real additional_translation_y: 0
        property real additional_translation_z: 0
        property real lens_correction_amount: 1
        property real fov: 1
        property real smoothness: 0.5
        property real zoomLimit: 120
        property int iterations: 5
        property var smoothnessCalls: []
        property int zoomCalls: 0
        property int iterationCalls: 0
        property var keyframeCalls: []
        property int methodId: 4
        property var methodCalls: []
        property var methodResetCalls: []
        property var methodBudgets: ({1:180,4:120})
        property var methodValues: ({1:{smoothness:0.15,alpha_0_1s:0.10},4:{smoothness:0.5}})
        function set_smoothing_method(index, resetZoom) {
            methodCalls = methodCalls.concat([index]);
            methodResetCalls = methodResetCalls.concat([resetZoom]);
            if (index !== methodId) {
                if (methodValues[methodId]) methodValues[methodId].smoothness = smoothness;
                methodId = index;
                smoothness = methodValues[index] ? methodValues[index].smoothness : 0;
            }
            if (resetZoom) zoomLimit = methodBudgets[index];
            const params = [{name:"smoothness", description:"Smoothness", type:"SliderWithField", from:0.001, to:1,
                value:smoothness, default:0.5, unit:"", precision:3, keyframe:"SmoothingParamSmoothness"}];
            if (index === 1) params.push({name:"alpha_0_1s", description:"Max smoothness at high velocity",type:"SliderWithField",
                from:0.01,to:1,value:methodValues[1].alpha_0_1s,default:0.10,unit:"s",precision:3});
            return params;
        }
        function set_smoothing_param(name, value) {
            smoothnessCalls = smoothnessCalls.concat([{name:name, value:value}]);
            if (name === "smoothness") smoothness = value;
            else if (methodValues[methodId]) methodValues[methodId][name] = value;
            smoothing_param_changed(name, value);
        }
        function get_max_zoom() { return zoomLimit; }
        function set_max_zoom(value, count) { zoomCalls++; zoomLimit = value; iterations = count; }
        function set_max_zoom_iterations(count) { iterationCalls++; iterations = count; }
        function set_fixed_zoom(value) { }
        function set_horizon_lock() { }
        function set_use_gravity_vectors(value) { }
        function set_horizon_lock_integration_method(value) { }
        function set_focal_length_smoothing_enabled(value) { }
        function set_focal_length_max_zoom_rate(value) { }
        function is_keyframed(key) { return false; }
        function get_keyframe_value(key, timestamp) { return 0; }
        function set_keyframe(key, timestamp, value) { keyframeCalls = keyframeCalls.concat([{key:key,value:value}]); }
        function clear_keyframes_type(key) { }
        function has_neuflow_support() { return false; }
        function has_optical_sync_support() { return true; }
        function set_of_method(value) { }
    }
    property alias controller: controllerStub
    QtObject {
        id: backendStub
        property bool has_video_jobs: false
        property bool batch_sync_ai_method: false
        function has_match_results() { return false; }
        function get_all_video_job_ids_json() { return "[]"; }
        function set_batch_auto_rotate(ids, value) { }
        function reapply_batch_auto_rotate(ids) { }
    }
    property alias render_queue: backendStub
    QtObject {
        id: windowStub
        property bool isSimpleMode: true
        property bool useMobileWorkspace: false
        property bool syncDirty: false
        property var stab: null
        property real width: 540
        property real height: 900
        property var batchState: QtObject {
            property bool active: false
            property real smoothness: 50
            property bool horizonLock: false
            property real horizonLockAmount: 100
            property real framerate: 0
            property int zoomMode: 1
            property real lensCorrection: 1
            property bool autoRotate: false
            property string detectedSource: ""
        }
        property var motionData: QtObject { property bool hasQuaternions: false; property bool hasRawGyro: true; property int integrationMethod: 1; property string detectedFormat: ""; }
        property var exportSettings: ({originalWidth:1920,originalHeight:1080})
        property var vidInfo: ({metadataRotation:0})
        property var videoArea: QtObject {
            property var vid: QtObject { property bool loaded: true; property real timestamp: 0; property real frameRate: 30; property int videoWidth:1920; property int videoHeight:1080; }
            property var timeline: QtObject { property real scaledFps: 30; function getTimestamp() { return 0; } function getTimestampUs() { return 0; } }
            property var queue: QtObject { property var gyroFilesInfo: []; property bool shown:false; property var selectedJobs:({}); property int selectedCount:0; property int matchVersion:0; }
        }
        function modal() { }
    }
    property alias window: windowStub
    Component { id: fullFactory; Menu.Stabilization { } }
    Component { id: simpleFactory; Menu.SimpleStabilization { } }
    Item { Item { Item { id: panelHost; width:500; height:900; } } }

    function find(node, predicate) {
        if (predicate(node)) return node;
        for (const child of node.children || []) { const found = find(child,predicate); if (found) return found; }
        return null;
    }
    function fullPanel() { const p = createTemporaryObject(fullFactory,panelHost,{width:500}); verify(p !== null); windowStub.stab=p; wait(30); return p; }
    function simplePanel() { const p = createTemporaryObject(simpleFactory,panelHost,{width:500}); verify(p !== null); wait(30); return p; }
    function zoomControl(panel) { return find(panel,x => x.keyframe === "MaxZoom" && x.slider !== undefined); }
    function methodControl(panel) { return find(panel,x => x.objectName === "smoothingMethod"); }
    function chooseMethod(panel,index) { methodControl(panel).activated(index); wait(30); }
    function clearCalls() { controllerStub.smoothnessCalls=[]; controllerStub.zoomCalls=0; controllerStub.iterationCalls=0; controllerStub.keyframeCalls=[]; controllerStub.methodCalls=[]; controllerStub.methodResetCalls=[]; }
    function init() {
        windowStub.useMobileWorkspace=false; windowStub.batchState.active=false; windowStub.batchState.smoothness=50;
        windowStub.stab=null; controllerStub.methodId=4;
        controllerStub.methodValues=({1:{smoothness:0.15,alpha_0_1s:0.10},4:{smoothness:0.5}});
        controllerStub.methodBudgets=({1:180,4:120});
        controllerStub.smoothness=.5; controllerStub.zoomLimit=120; controllerStub.iterations=5;
        settingsStub.values=({"smoothing-4-smoothness":0.15,"smoothing-1-smoothness":0.7,"maxZoom":180,"maxZoomIterations":12,"smoothingMethod":3});
        settingsStub.reads=[]; settingsStub.writes=[]; clearCalls();
    }
    function test_startup_ignores_saved_smoothing_and_zoom() {
        const full=fullPanel(), simple=simplePanel();
        fuzzyCompare(full.getParamElement("smoothness").value,.5,.000001);
        compare(full.getParamElement("smoothness").field.value,50);
        compare(simple.smoothnessSlider.field.value,50);
        compare(zoomControl(full).value,120);
        compare(controllerStub.zoomCalls,0);
        verify(!settingsStub.reads.some(k => k.indexOf("smoothing-")===0 || k==="maxZoom" || k==="maxZoomIterations"));
        verify(!settingsStub.writes.some(k => k.indexOf("smoothing-")===0 || k==="maxZoom" || k==="maxZoomIterations"));
    }
    function test_both_panels_expose_default_and_optimal_with_real_core_ids() {
        const full=fullPanel(), simple=simplePanel();
        for (const panel of [full,simple]) {
            const c=methodControl(panel); verify(c.visible); verify(c.enabled);
            compare(c.count,2); compare(c.textAt(0),"Default"); compare(c.textAt(1),"Optimal path"); compare(c.currentIndex,1);
        }
        clearCalls(); chooseMethod(simple,0);
        compare(controllerStub.methodCalls,[1]); compare(full.smoothingMethodId,1); compare(methodControl(full).currentIndex,0);
        compare(controllerStub.methodResetCalls,[true]); compare(zoomControl(full).value,180);
        compare(simple.smoothnessSlider.field.value,15); compare(controllerStub.smoothnessCalls.length,0);
        chooseMethod(full,1);
        compare(controllerStub.methodCalls,[1,4]); compare(methodControl(simple).currentIndex,1);
        compare(controllerStub.methodResetCalls,[true,true]); compare(zoomControl(full).value,120);
        compare(simple.smoothnessSlider.field.value,50); compare(controllerStub.smoothnessCalls.length,0);
    }
    function test_switching_preserves_parameters_but_resets_the_crop_budget() {
        const full=fullPanel(), simple=simplePanel();
        controllerStub.set_smoothing_param("smoothness",.723456);
        controllerStub.zoomLimit=143; controllerStub.iterations=12;
        controllerStub.methodBudgets[4]=135;
        clearCalls(); chooseMethod(full,0);
        controllerStub.set_smoothing_param("smoothness",.153456);
        full.getParamElement("alpha_0_1s").value=.15;
        clearCalls(); chooseMethod(simple,1);
        fuzzyCompare(full.getParamElement("smoothness").value,.723456,.0000001);
        fuzzyCompare(simple.smoothnessSlider.value,.723456,.0000001);
        compare(zoomControl(full).value,135);
        chooseMethod(simple,0);
        fuzzyCompare(full.getParamElement("smoothness").value,.153456,.0000001);
        fuzzyCompare(full.getParamElement("alpha_0_1s").value,.15,.0000001);
        compare(controllerStub.smoothnessCalls.length,0); compare(controllerStub.keyframeCalls.length,0);
        compare(controllerStub.zoomCalls,0); compare(controllerStub.iterationCalls,0);
        compare(zoomControl(full).value,180); compare(controllerStub.iterations,12);
        compare(controllerStub.methodResetCalls,[true,true]);
    }
    function test_imported_default_is_displayed_without_replacing_saved_parameters() {
        const full=fullPanel(), simple=simplePanel();
        controllerStub.methodId=1; controllerStub.smoothness=.153456; controllerStub.methodValues[1].alpha_0_1s=.15;
        controllerStub.zoomLimit=143; settingsStub.values.smoothingMethodChoice="Optimal path";
        settingsStub.writes=[];
        clearCalls();
        const project={stabilization:{method:"Default",smoothing_params:[{name:"smoothness",value:.153456},{name:"alpha_0_1s",value:.15}]}};
        full.loadGyroflow(project); simple.loadGyroflow(project); wait(40);
        compare(methodControl(full).currentIndex,0); compare(methodControl(simple).currentIndex,0);
        fuzzyCompare(full.getParamElement("smoothness").value,.153456,.0000001);
        fuzzyCompare(simple.smoothnessSlider.value,.153456,.0000001);
        fuzzyCompare(full.getParamElement("alpha_0_1s").value,.15,.0000001);
        compare(controllerStub.smoothnessCalls.length,0);
        compare(controllerStub.methodResetCalls,[false]); compare(zoomControl(full).value,143);
        compare(settingsStub.writes.length,0); compare(settingsStub.values.smoothingMethodChoice,"Optimal path");
    }
    function test_other_saved_method_is_not_replaced_by_two_choice_selector() {
        const full=fullPanel(), simple=simplePanel(); clearCalls();
        full.loadGyroflow({stabilization:{method:"Plain 3D"}}); wait(30);
        compare(controllerStub.methodCalls,[2]); compare(full.smoothingMethodId,2);
        for (const panel of [full,simple]) { compare(methodControl(panel).currentIndex,-1); compare(methodControl(panel).displayText,"Plain 3D"); }
    }
    function test_method_selection_does_not_act_as_a_batch_or_mobile_setting() {
        const full=fullPanel(), simple=simplePanel(); clearCalls();
        windowStub.batchState.active=true;
        for (const panel of [full,simple]) verify(!methodControl(panel).enabled);
        windowStub.batchState.active=false; windowStub.useMobileWorkspace=true;
        for (const panel of [full,simple]) verify(!methodControl(panel).visible);
        compare(controllerStub.methodCalls.length,0);
    }
    function test_method_preference_survives_restart_without_the_strength_data() {
        return [{tag:"Default",method:0,id:1,strength:.15,budget:180}, {tag:"Optimal",method:1,id:4,strength:.5,budget:120}];
    }
    function test_method_preference_survives_restart_without_the_strength(data) {
        const first=fullPanel(); chooseMethod(first,data.method);
        first.getParamElement("smoothness").value=.8;
        compare(settingsStub.values.smoothingMethodChoice,smoothingAlgorithms[data.id]);
        first.destroy(); windowStub.stab=null; wait(10);
        controllerStub.methodId=4; controllerStub.smoothness=.5;
        controllerStub.methodValues=({1:{smoothness:.15,alpha_0_1s:.10},4:{smoothness:.5}});
        clearCalls(); settingsStub.writes=[];
        const second=fullPanel(), simple=simplePanel();
        compare(second.smoothingMethodId,data.id); compare(methodControl(simple).currentIndex,data.method);
        fuzzyCompare(second.getParamElement("smoothness").value,data.strength,.0000001);
        fuzzyCompare(simple.smoothnessSlider.value,data.strength,.0000001);
        compare(zoomControl(second).value,data.budget); compare(settingsStub.writes.length,0);
    }
    function test_unknown_method_preference_does_not_restore_fixed_camera() {
        settingsStub.values.smoothingMethodChoice="Fixed camera";
        const full=fullPanel(); compare(full.smoothingMethodId,4);
        compare(methodControl(full).currentIndex,1); compare(zoomControl(full).value,120);
    }
    function test_backend_readback_does_not_echo_edits() {
        const full=fullPanel(), simple=simplePanel(); clearCalls();
        full.getParamElement("smoothness").keyframesEnabled=true;
        simple.smoothnessSlider.keyframesEnabled=true;
        controllerStub.zoomLimit=150; controllerStub.smoothness=.8;
        controllerStub.smoothing_param_changed("smoothness",.8);
        compare(full.getParamElement("smoothness").field.value,80);
        compare(simple.smoothnessSlider.field.value,80);
        compare(zoomControl(full).value,150);
        compare(controllerStub.smoothnessCalls.length,0); compare(controllerStub.zoomCalls,0);
        compare(controllerStub.keyframeCalls.length,0);
    }
    function test_project_readback_preserves_saved_value_and_iterations() {
        const full=fullPanel(); clearCalls();
        controllerStub.smoothness=.153456; controllerStub.zoomLimit=143; controllerStub.iterations=12;
        full.loadGyroflow({videofile:"saved.mp4",stabilization:{method:"Optimal path",smoothing_params:[{name:"smoothness",value:.153456}],max_zoom:143,max_zoom_iterations:12,adaptive_zoom_window:2.6}});
        wait(40);
        fuzzyCompare(controllerStub.smoothness,.153456,.0000001);
        compare(controllerStub.iterations,12); compare(zoomControl(full).value,143);
        compare(controllerStub.smoothnessCalls.length,0); compare(controllerStub.zoomCalls,0); compare(controllerStub.iterationCalls,0);
        compare(controllerStub.adaptive_zoom,4);
    }
    function test_manual_zoom_and_iteration_edits_stay_separate() {
        const full=fullPanel(); clearCalls();
        zoomControl(full).value=140;
        compare(controllerStub.zoomCalls,1); compare(controllerStub.zoomLimit,140);
        const label=find(full,x => x.text==="Zoom limit iterations");
        verify(label !== null);
        const iterations=find(label,x => x.slider !== undefined && x.value !== undefined);
        iterations.value=12;
        compare(controllerStub.iterationCalls,1); compare(controllerStub.zoomCalls,1);
    }
    function test_simple_percent_edits_and_keyframes_use_normalized_values() {
        const simple=simplePanel(); clearCalls();
        simple.smoothnessSlider.keyframesEnabled=true;
        simple.smoothnessSlider.field.value=75;
        fuzzyCompare(simple.smoothnessSlider.value,.75,.000001);
        fuzzyCompare(controllerStub.smoothness,.75,.000001);
        verify(controllerStub.keyframeCalls.length>0);
        fuzzyCompare(controllerStub.keyframeCalls[controllerStub.keyframeCalls.length-1].value,.75,.000001);
    }
    function test_batch_percent_edits_do_not_change_preview() {
        const simple=simplePanel(); clearCalls(); windowStub.batchState.active=true;
        simple.smoothnessSlider.value=.65;
        compare(windowStub.batchState.smoothness,65);
        compare(controllerStub.smoothnessCalls.length,0);
    }
    function test_new_panel_starts_at_fifty_again() {
        const first=fullPanel(); first.getParamElement("smoothness").value=.7;
        compare(controllerStub.smoothness,.7); first.destroy(); wait(10);
        controllerStub.smoothness=.5; controllerStub.zoomLimit=120; clearCalls();
        const second=fullPanel();
        compare(second.getParamElement("smoothness").field.value,50); compare(zoomControl(second).value,120);
        verify(!settingsStub.writes.some(k => k.indexOf("smoothing-")===0));
    }
}
