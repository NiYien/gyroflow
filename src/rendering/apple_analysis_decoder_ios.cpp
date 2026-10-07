// SPDX-License-Identifier: GPL-3.0-or-later
// iOS uses MDK's stable public API: its bundled SDK has a different private
// C++ ABI from desktop. Frame ownership and Rust callbacks remain bounded.
#include <mdk/Player.h>
#include <mdk/VideoFrame.h>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <limits>
#include <cmath>
#include <memory>

struct AppleAnalysisRange { double from, to; };
using SelectFrame = int (*)(void *, int64_t); // -1: stop, 0: skip, 1: 8-bit, 2: 10-bit
using ReceiveFrame = bool (*)(void *, int64_t, uint32_t, uint32_t, const char *, const uint8_t *const *, const int *);
using Cancelled = bool (*)(void *);
namespace {
struct Queue {
    std::mutex mutex;
    std::condition_variable changed;
    std::deque<mdk::VideoFrame> frames;
    bool stopping = false, done = false, error = false;
};
struct PlayerApi {
    const mdkPlayerAPI *value = mdkPlayerAPI_new();
    ~PlayerApi() { if (value) mdkPlayerAPI_reset(&value, true); }
};
struct Session {
    std::shared_ptr<Queue> queue = std::make_shared<Queue>();
    PlayerApi api;
    mdk::Player player{api.value};
    ~Session() {
        { std::lock_guard lock(queue->mutex); queue->stopping = true; }
        queue->changed.notify_all();
        player.set(mdk::State::Stopped);
        player.waitFor(mdk::State::Stopped);
    }
};
}
extern "C" int niyien_apple_analysis_read(const char *path, const AppleAnalysisRange *ranges,
    size_t count, SelectFrame select, ReceiveFrame receive, Cancelled cancelled, void *user) {
    try {
        const AppleAnalysisRange whole{0, std::numeric_limits<double>::max()};
        if (!count) { ranges = &whole; count = 1; }
        for (size_t r = 0; r < count; ++r) {
            if (cancelled(user)) return 0;
            Session session;
            if (!session.api.value) return 1;
            auto queue = session.queue;
            auto &player = session.player;
            player.setDecoders(mdk::MediaType::Video, {"VT"});
            player.setDecoders(mdk::MediaType::Audio, {});
            player.setMute(true);
            player.onSync([] { return DBL_MAX; });
            // Register through the public C API, avoiding the C++ wrapper's
            // process-wide non-atomic callback token during parallel jobs.
            mdkMediaEventCallback event_callback{};
            event_callback.opaque = queue.get();
            event_callback.cb = [](const mdkMediaEvent *event, void *opaque) {
                auto *q = static_cast<Queue *>(opaque);
                if (event->error < 0) {
                    std::lock_guard lock(q->mutex);
                    q->error = true;
                    q->changed.notify_all();
                }
                return false;
            };
            session.api.value->onEvent(session.api.value->object, event_callback, nullptr);
            player.onFrame<mdk::VideoFrame>([queue](mdk::VideoFrame &frame, int) {
                std::unique_lock lock(queue->mutex);
                if (frame.timestamp() == mdk::TimestampEOS) {
                    queue->done = true;
                    queue->changed.notify_all();
                    return 0;
                }
                if (!frame) return 0;
                queue->changed.wait(lock, [&] { return queue->stopping || queue->frames.size() < 3; });
                if (!queue->stopping) queue->frames.push_back(frame);
                queue->changed.notify_all();
                return 0;
            });
            player.setMedia(path);
            player.setActiveTracks(mdk::MediaType::Video, {0});
            player.setActiveTracks(mdk::MediaType::Audio, {});
            player.setTimeout(15000);
            player.prepare(int64_t(ranges[r].from), [queue](int64_t position, bool *) {
                if (position < 0) {
                    std::lock_guard lock(queue->mutex);
                    queue->error = true;
                    queue->changed.notify_all();
                    return false;
                }
                return true;
            });
            player.set(mdk::State::Running);
            auto last_frame = std::chrono::steady_clock::now();
            size_t decoded = 0;
            while (!cancelled(user)) {
                mdk::VideoFrame frame;
                {
                    std::unique_lock lock(queue->mutex);
                    queue->changed.wait_for(lock, std::chrono::milliseconds(20), [&] {
                        return queue->error || queue->done || !queue->frames.empty();
                    });
                    if (queue->error) return 3;
                    if (queue->frames.empty()) {
                        if (queue->done) break;
                        if (std::chrono::steady_clock::now() - last_frame > std::chrono::seconds(15)) return 4;
                        continue;
                    }
                    frame = std::move(queue->frames.front());
                    queue->frames.pop_front();
                }
                queue->changed.notify_all();
                last_frame = std::chrono::steady_clock::now();
                if (!std::isfinite(frame.timestamp())) return 3;
                const auto us = int64_t(std::llround(frame.timestamp() * 1e6));
                if (us / 1000.0 - 0.001 > ranges[r].to) break;
                if (us / 1000.0 + 0.001 < ranges[r].from) continue;
                ++decoded;
                const int selected = select(user, us);
                if (selected < 0) return 0;
                if (!selected) continue;
                // Use public, explicitly named pixel formats. 4:2:2 chroma may
                // be subsampled, but the luma consumed by GRAY8 retains its depth.
                const bool ten_bit = selected == 2;
                auto host = frame.to(ten_bit ? mdk::PixelFormat::P010LE : mdk::PixelFormat::NV12);
                if (!host || host.planeCount() != 2) return 3;
                const uint8_t *planes[4] = {};
                int strides[4] = {};
                for (int plane = 0; plane < 2; ++plane) {
                    planes[plane] = host.bufferData(plane);
                    strides[plane] = host.bytesPerLine(plane);
                    if (!planes[plane]) return 3;
                }
                if (!receive(user, us, host.width(), host.height(), ten_bit ? "p010le" : "nv12", planes, strides)) return 0;
                last_frame = std::chrono::steady_clock::now();
            }
            if (!decoded && !cancelled(user)) return 3;
        }
        return 0;
    } catch (...) { return 3; }
}
