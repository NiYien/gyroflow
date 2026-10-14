// SPDX-License-Identifier: GPL-3.0-or-later
#import <Metal/Metal.h>
#include <mdk/Player.h>
#include <mdk/RenderAPI.h>
#include <mdk/VideoFrame.h>
#include <mdk/MediaInfo.h>
#include <atomic>
#include <cfloat>
#include <chrono>
#include <cstdio>
#include <thread>
#include <mutex>
using Clock = std::chrono::steady_clock;
static auto origin = Clock::now();
static double ms() { return std::chrono::duration<double, std::milli>(Clock::now()-origin).count(); }
int main(int argc, char** argv) {
    if (argc < 4) return 2;
    @autoreleasepool {
        mdk::setLogHandler([](mdk::LogLevel, const char* text){ std::fputs(text, stderr); });
        mdk::SetGlobalOption("log", "All");
        mdk::SetGlobalOption("plugins", "mdk-r3d");
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) return 3;
        id<MTLCommandQueue> queue = [device newCommandQueue];
        auto desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatRGBA8Unorm width:1920 height:1080 mipmapped:NO];
        desc.usage = MTLTextureUsageRenderTarget | MTLTextureUsageShaderRead;
        id<MTLTexture> texture = [device newTextureWithDescriptor:desc];
        mdk::Player p;
        mdk::MetalRenderAPI api;
        api.device = (__bridge const void*)device;
        api.cmdQueue = (__bridge const void*)queue;
        api.texture = (__bridge const void*)texture;
        p.setRenderAPI(&api);
        p.setVideoSurfaceSize(1920,1080);
        p.setDecoders(mdk::MediaType::Video, {argv[2]});
        p.setMute(true);
        p.setAudioBackends({"AudioQueue"});
        if (std::getenv("R3D_BENCH_UNPACED")) p.onSync([]{ return DBL_MAX; });
        p.setBufferRange(argc>4 ? std::atoi(argv[4]) : 0);
        p.setProperty("continue_at_end","1");
        std::atomic<int> phase{0};
        p.onFrame<mdk::VideoFrame>([&](mdk::VideoFrame& f,int) {
            if (f && f.timestamp() != mdk::TimestampEOS)
                printf("FRAME %d %.3f %.6f %d %d\n",phase.load(),ms(),f.timestamp(),f.width(),f.height());
            return 0;
        });
        p.onEvent([&](const mdk::MediaEvent& e) {
            printf("EVENT %d %.3f %s %s %lld\n",phase.load(),ms(),e.category.c_str(),e.detail.c_str(),(long long)e.error);
            return true;
        });
        p.setMedia(argv[1]);
        if (argc > 5) p.setActiveTracks(mdk::MediaType::Audio, {});
        p.prepare(0);
        auto renderFor = [&](double duration) {
            auto start = Clock::now();
            double previous = -1;
            while(std::chrono::duration<double, std::milli>(Clock::now()-start).count()<duration) {
                double t=ms(), pts=p.renderVideo(), cost=ms()-t;
                if(pts>=0 && pts!=previous) {
                    printf("RENDER %d %.3f %.6f %.3f\n",phase.load(),ms(),pts,cost);
                    previous=pts;
                }
                std::this_thread::sleep_for(std::chrono::milliseconds(4));
            }
        };
        renderFor(800);
        if (p.mediaInfo().duration < 3000) {
            p.set(mdk::State::Stopped);p.waitFor(mdk::State::Stopped);
            p.setVideoSurfaceSize(-1, -1);return 77;
        }
        double current = 0;
        auto play = [&](int part) {
            phase.store(part);
            printf("PLAY %d %.3f\n", part, ms());
            p.seek((int64_t)(current * 1000), mdk::SeekFlag::FromStart);
            p.set(mdk::State::Running);
        };
        auto pause = [&] {
            p.set(mdk::State::Paused);
            p.waitFor(mdk::State::Paused);
            renderFor(200);
            current = p.renderVideo();
        };
        auto seek = [&](int64_t pos) {
            printf("SEEK %d %.3f %lld\n",phase.load(),ms(),(long long)pos);
            p.seek(pos, mdk::SeekFlag::FromStart | mdk::SeekFlag::InCache);
        };
        play(1); renderFor(500); pause();
        play(2); renderFor(500); pause();
        phase.store(0); seek(1200); renderFor(250); current=p.renderVideo();
        play(3); renderFor(500);
        phase.store(4); seek(2100); renderFor(450);
        phase.store(5); seek(400); renderFor(600); pause();
        phase.store(0);
        for (int pos : {1800, 800, 2400, 500}) {seek(pos);renderFor(30);}
        renderFor(250);current=p.renderVideo();
        play(6); renderFor(700);pause();
        phase.store(0);seek(999999);renderFor(250);seek(0);renderFor(250);current=p.renderVideo();
        play(7);renderFor(700);
        phase.store(8);
        p.setLoop(1);
        seek(p.mediaInfo().duration - 300);
        renderFor(850);
        p.set(mdk::State::Stopped);
        p.waitFor(mdk::State::Stopped);
        p.setVideoSurfaceSize(-1, -1);
        return 0;
    }
}
