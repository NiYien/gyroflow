// SPDX-License-Identifier: GPL-3.0-or-later
// Windows opt-in test: compare every frame and retain more frames than the SDK job pool.
#define NOMINMAX
#include <windows.h>
#include <bcrypt.h>
#include <mdk/FrameReader.h>
#include <mdk/MediaInfo.h>
#include <mdk/VideoFrame.h>
#include <array>
#include <atomic>
#include <chrono>
#include <codecvt>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <deque>
#include <locale>
#include <mutex>
#include <stdexcept>
#include <vector>

struct Sample {
    double timestamp;
    int width, height;
    std::array<unsigned char, 32> rgb;
    bool operator==(const Sample&) const = default;
};

static std::vector<Sample> readFrames(const char* path, int ahead, size_t retain, BCRYPT_ALG_HANDLE algorithm) {
    auto reader = mdk::FrameReader::create("R3D");
    if (!reader) throw std::runtime_error("R3D reader unavailable");
    reader->setMedia(path);
    reader->setActiveTracks(mdk::MediaType::Audio, {});
    reader->setActiveTracks(mdk::MediaType::Video, {0});
    reader->setDecoders(mdk::MediaType::Video, {"R3D:gpu=auto:scale=1920x1080:ahead=" + std::to_string(ahead)});
    std::mutex mutex;
    std::condition_variable ready;
    bool done = false, valid = true;
    std::atomic<int64_t> expectedFrames{0};
    std::deque<std::pair<mdk::VideoFrame, double>> held;
    std::vector<Sample> samples;
    auto consume = [&] {
        auto frame = std::move(held.front().first);
        const auto timestamp = held.front().second;
        held.pop_front();
        valid &= frame.timestamp() == timestamp;
        auto rgba = frame.to(mdk::PixelFormat::RGBA);
        if (!rgba) { valid = false; return; }
        std::vector<unsigned char> rgb(size_t(rgba.width()) * rgba.height() * 3);
        const auto source = rgba.buffer()->constData();
        for (int y = 0; y < rgba.height(); ++y)
            for (int x = 0; x < rgba.width(); ++x)
                std::memcpy(rgb.data() + (size_t(y) * rgba.width() + x) * 3,
                            source + size_t(y) * rgba.buffer()->stride() + x * 4, 3);
        Sample sample{timestamp, frame.width(), frame.height(), {}};
        if (BCryptHash(algorithm, nullptr, 0, rgb.data(), ULONG(rgb.size()), sample.rgb.data(), ULONG(sample.rgb.size())) < 0)
            valid = false;
        samples.push_back(sample);
    };
    reader->onRead<mdk::VideoFrame>([&](const mdk::VideoFrame& frame, int) {
        if (!frame) {
            if (frame.timestamp() == mdk::TimestampEOS) {
                std::lock_guard lock(mutex);
                done = true;
                ready.notify_one();
            }
            return true;
        }
        held.emplace_back(frame, frame.timestamp());
        if (held.size() > retain) consume();
        return true;
    });
    reader->start(int64_t(0), [&](const mdk::MediaInfo* info, int64_t, bool*) {
        if (info && !info->video.empty()) expectedFrames = info->video[0].frames;
        return true;
    });
    {
        std::unique_lock lock(mutex);
        ready.wait_for(lock, std::chrono::seconds(120), [&] { return done; });
    }
    reader->stop();
    reader->waitFor(mdk::State::Stopped);
    while (!held.empty()) consume();
    if (!done || !valid || samples.empty() || int64_t(samples.size()) != expectedFrames)
        throw std::runtime_error("Missing frames, altered retained frames or incomplete decode");
    for (size_t i = 1; i < samples.size(); ++i)
        if (samples[i].timestamp <= samples[i - 1].timestamp)
            throw std::runtime_error("Duplicate or out-of-order frame");
    return samples;
}

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) return 2;
    std::wstring_convert<std::codecvt_utf8_utf16<wchar_t>> utf8;
    const auto path = utf8.to_bytes(argv[1]);
    mdk::SetGlobalOption("plugins", "mdk-r3d");
    BCRYPT_ALG_HANDLE algorithm = nullptr;
    if (BCryptOpenAlgorithmProvider(&algorithm, BCRYPT_SHA256_ALGORITHM, nullptr, 0) < 0) return 3;
    int result = 1;
    try {
        const auto serial = readFrames(path.c_str(), 1, 0, algorithm);
        const auto parallel = readFrames(path.c_str(), 2, 12, algorithm);
        if (serial != parallel) throw std::runtime_error("Parallel decoding changed RGB pixels, timestamps or dimensions");
        std::printf("R3D_DECODE_IDENTICAL %zu frames; retained 12 frames without overwrite\n", serial.size());
        result = 0;
    } catch (const std::exception& error) {
        std::fprintf(stderr, "%s\n", error.what());
    }
    BCryptCloseAlgorithmProvider(algorithm, 0);
    return result;
}
