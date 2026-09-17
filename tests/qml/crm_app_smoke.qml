// SPDX-License-Identifier: GPL-3.0-or-later
// Place beside an isolated test executable as ui/main_window.qml.
import QtQuick
import QtQml.Models
Item {
    id: smoke
    property var applicationWindow: null
    property int stage: 0
    property int ticks: 0
    property int stageTicks: 0
    property int jobId: 0
    property bool completed: false
    property bool savedImage: false
    property bool thumbnailSeen: false
    property double stageStarted: Date.now()
    property url crm: "file:///C:/Users/Jhe/Downloads/062A7269.CRM"
    property url proxy: "file:///C:/Users/Jhe/Downloads/062A7269.mp4"
    Instantiator {
        model: render_queue.queue
        delegate: QtObject {
            property string thumbnail: thumbnail_url
            onThumbnailChanged: if (thumbnail.length>0) smoke.thumbnailSeen=true
        }
    }
    function check(value,message) {
        if (!value) {
            completed = true;
            console.warn("CRM_NATIVE_SMOKE_FAIL",stage,message);
            if (applicationWindow) applicationWindow.closeConfirmed = true;
            Qt.exit(1);
        }
    }
    function advance() {
        console.warn("CRM_NATIVE_STAGE",stage,"elapsed_ms",Date.now()-stageStarted);
        stage++; stageTicks=0; stageStarted=Date.now();
    }
    Component.onCompleted: {
        const component=Qt.createComponent("qrc:/src/ui/main_window.qml");
        check(component.status===Component.Ready,component.errorString());
        applicationWindow=component.createObject(null);
        applicationWindow.width=1280;
        applicationWindow.height=800;
        applicationWindow.hide();
        Qt.callLater(function() { smoke.applicationWindow.show(); });
    }
    Timer {
        interval: 200; repeat: true; running: !smoke.completed
        onTriggered: {
            if (++smoke.ticks>600) { smoke.check(false,"timeout");return; }
            const app=smoke.applicationWindow ? smoke.applicationWindow.getApp() : null;
            if (smoke.ticks<25 || !app || !app.advanced || !app.videoArea.queue || !app.motionData) return;
            app.onboardingActive=true;
            const video=app.videoArea;
            smoke.stageTicks++;
            if ((smoke.stage===2 || smoke.stage===4) && smoke.stageTicks>50) {smoke.check(false,"seek did not finish within ten seconds");return;}
            if (smoke.stageTicks % 25 === 0) console.warn("CRM_NATIVE_WAIT",smoke.stage,"timestamp",video.vid.timestamp,"playing",video.vid.playing,"loading",video.previewLoading,video.videoLoader.active);
            if (smoke.stage===0) {
                smoke.check(app.controller.supports_native_crm(),"native decoder available");
                video.loadMultipleFiles([smoke.crm],true);
                smoke.advance();
            } else if (smoke.stage===1) {
                if (video.previewLoading || video.videoLoader.active) return;
                smoke.check(video.loadedFileUrl.toString()===smoke.crm.toString(),"source remains CRM");
                smoke.check(video.vid.videoWidth===6000 && video.vid.videoHeight===3164,"original source dimensions");
                smoke.check(video.vid.frameCount===224,"original frame count");
                smoke.check(render_queue.is_plugin_only_video(smoke.crm.toString()),"video export blocked");
                video.vid.seekToTimestamp(1800,true);
                smoke.advance();
            } else if (smoke.stage===2) {
                if (Math.abs(video.vid.timestamp-1800)>40) return;
                video.vid.play();
                smoke.advance();
            } else if (smoke.stage===3) {
                if (video.vid.timestamp<2300) return;
                video.vid.pause();
                video.vid.seekToTimestamp(400,true);
                smoke.advance();
            } else if (smoke.stage===4) {
                if (Math.abs(video.vid.timestamp-400)>40) return;
                video.vid.grabToImage(function(result) {
                    smoke.savedImage=result.saveToFile("C:/Users/Jhe/Desktop/github/gyroflow/target/crm-integration/preview.png");
                });
                const filtered=JSON.parse(render_queue.filter_raw_proxy_siblings(JSON.stringify([smoke.crm.toString(),smoke.proxy.toString()]),JSON.stringify(["crm","mp4"])));
                smoke.check(filtered.length===1 && filtered[0]===smoke.crm.toString(),"prefer CRM when both selected");
                smoke.jobId=render_queue.add_file(smoke.crm.toString(),"",app.getAdditionalProjectDataJson());
                smoke.check(smoke.jobId>0,"queue accepts standalone CRM");
                smoke.advance();
            } else if (smoke.stage===5) {
                const text=render_queue.get_gyroflow_data(smoke.jobId);
                if (!text || !smoke.savedImage || !smoke.thumbnailSeen) return;
                const data=JSON.parse(text);
                const display=JSON.parse(render_queue.get_job_display_params(smoke.jobId));
                smoke.check(display.focal_length===24,"CRM focal length remains 24mm");
                smoke.check(data.videofile.toLowerCase().endsWith(".crm"),"project references original CRM");
                smoke.check(render_queue.is_job_plugin_only(smoke.jobId),"queue is project only");
                video.loadGyroflowData(data,smoke.jobId);
                smoke.advance();
            } else if (smoke.stage===6) {
                if (video.previewLoading || video.videoLoader.active) return;
                smoke.check(video.loadedFileUrl.toString()===smoke.crm.toString(),"queue reload keeps CRM");
                video.loadFile(smoke.proxy,true,0,"",true);
                smoke.advance();
            } else {
                if (video.previewLoading || video.videoLoader.active) return;
                smoke.check(video.loadedFileUrl.toString()===smoke.proxy.toString(),"switch back to ordinary video");
                console.warn("CRM_NATIVE_SMOKE_PASS standalone import, playback, seek, queue thumbnail, 24mm metadata, project reload, project-only gate, ordinary video");
                smoke.completed=true;
                smoke.applicationWindow.closeConfirmed=true;
                Qt.quit();
            }
        }
    }
}
