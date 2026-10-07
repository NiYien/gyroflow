// SPDX-License-Identifier: GPL-3.0-or-later
// Async VideoToolbox decoding with bounded frame ownership. Rust callbacks run
// only on the caller thread; unsampled frames never cross into CPU memory.
#include <mdk/FrameReader.h>
#include <mdk/MediaInfo.h>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <deque>
#include <mutex>
#include <limits>
#include <cmath>

struct AppleAnalysisRange { double from, to; };
using SelectFrame = int (*)(void *, int64_t); // -1: stop, 0: skip, 1: retain
using ReceiveFrame = bool (*)(void *, int64_t, uint32_t, uint32_t, const char *, const uint8_t *const *, const int *);
using Cancelled = bool (*)(void *);

namespace {
struct Queue {
    std::mutex mutex;
    std::condition_variable changed;
    std::deque<mdk::VideoFrame> frames;
    bool stopping = false, done = false, error = false;
};
struct Session {
    std::shared_ptr<Queue> queue = std::make_shared<Queue>();
    mdk::FrameReader::Ptr reader = mdk::FrameReader::create();
    ~Session() {
        { std::lock_guard lock(queue->mutex); queue->stopping = true; }
        queue->changed.notify_all();
        if (reader) { reader->stop(); reader->waitFor(mdk::State::Stopped); }
    }
};
}

extern "C" int niyien_apple_analysis_read(const char *path, const AppleAnalysisRange *ranges,
    size_t count, SelectFrame select, ReceiveFrame receive, Cancelled cancelled, void *user) {
    try {
        if (mdk::abiVersion() != MDK_ABI_VERSION) return 1;
        const AppleAnalysisRange whole{0, std::numeric_limits<double>::max()};
        if (!count) { ranges = &whole; count = 1; }
        for (size_t r = 0; r < count; ++r) {
            if (cancelled(user)) return 0;
            Session session;
            auto queue = session.queue;
            auto &reader = session.reader;
            if (!reader) return 1;
            reader->setMedia(path);
            reader->setDecoders(mdk::MediaType::Video, {"VT"});
            reader->setActiveTracks(mdk::MediaType::Video, {0});
            reader->setActiveTracks(mdk::MediaType::Audio, {});
            reader->setBufferRange(0, 100, false);
            reader->setTimeout(15000);
            reader->onEvent([queue](const mdk::MediaEvent &event) {
                if (event.error < 0) {
                    std::lock_guard lock(queue->mutex);
                    queue->error = true;
                    queue->changed.notify_all();
                }
                return false;
            });
            reader->onRead<mdk::VideoFrame>([queue](const mdk::VideoFrame &frame, int) {
                std::unique_lock lock(queue->mutex);
                if (frame.timestamp() == mdk::TimestampEOS) {
                    queue->done = true;
                    queue->changed.notify_all();
                    return true;
                }
                if (!frame) return true;
                queue->changed.wait(lock, [&] { return queue->stopping || queue->frames.size() < 3; });
                if (!queue->stopping) queue->frames.push_back(frame);
                queue->changed.notify_all();
                return true;
            });
            if (!reader->start(int64_t(ranges[r].from), [queue](const mdk::MediaInfo *info, int64_t position, bool *) {
                if (!info || info->video.empty() || position < 0) {
                    std::lock_guard lock(queue->mutex);
                    queue->error = true;
                    queue->changed.notify_all();
                    return false;
                }
                return true;
            })) return 2;
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
                // Keep the decoded pixel format and the existing FFmpeg scaler.
                // Do not introduce RGB conversion, a new resize kernel or 8-bit truncation.
                auto host = frame.to(frame.format());
                if (!host || host.format().planeCount() > 4) return 3;
                const uint8_t *planes[4] = {};
                int strides[4] = {};
                for (int plane = 0; plane < host.format().planeCount(); ++plane) {
                    auto buffer = host.buffer(plane);
                    if (!buffer || !buffer->constData()) return 3;
                    planes[plane] = buffer->constData();
                    strides[plane] = host.bytesPerLine(plane);
                }
                if (!receive(user, us, host.width(), host.height(), host.format().name(), planes, strides)) return 0;
                last_frame = std::chrono::steady_clock::now();
            }
            if (!decoded && !cancelled(user)) return 3;
        }
        return 0;
    } catch (...) { return 3; }
}
