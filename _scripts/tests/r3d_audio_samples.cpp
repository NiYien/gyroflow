// SPDX-License-Identifier: GPL-3.0-or-later
#include <mdk/FrameReader.h>
#include <mdk/AudioFrame.h>
#include <chrono>
#include <cmath>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <limits>
#include <mutex>
#include <stdexcept>
#include <vector>

static std::vector<int32_t> readAudio(const char* path, bool preview) {
    auto reader = mdk::FrameReader::create("R3D");
    if (!reader) throw std::runtime_error("R3D reader unavailable");
    reader->setMedia(path);
    reader->setActiveTracks(mdk::MediaType::Video, {});
    reader->setActiveTracks(mdk::MediaType::Audio, {0});
    reader->setDecoders(mdk::MediaType::Video, {preview
        ? "R3D:gpu=metal:decompress=async:format=rgb48le:copy=1:scale=1920x1080"
        : "R3D:gpu=no:decompress=cpu"});
    std::mutex mutex;
    std::condition_variable ready;
    bool done = false, valid = true, announced = false;
    double end = 0;
    std::vector<int32_t> samples;
    reader->onRead<mdk::AudioFrame>([&](const mdk::AudioFrame& frame, int) {
        if (!frame) {
            if (frame.format() && frame.timestamp() == 0 && samples.empty()) announced = true;
            if (frame.timestamp() == mdk::TimestampEOS) {
                std::lock_guard lock(mutex);
                done = true;
                ready.notify_one();
            }
            return true;
        }
        const auto expected = preview ? mdk::AudioFormat::SampleFormat::F64 : mdk::AudioFormat::SampleFormat::S32;
        valid &= announced && frame.format().sampleFormat() == expected;
        valid &= frame.format().sampleRate() > 0 && frame.format().channels() > 0;
        if (!valid) return true;
        valid &= std::abs(frame.timestamp() - end) < 1.0 / frame.format().sampleRate();
        end = frame.timestamp() + frame.duration();
        const auto bytes = frame.buffer()->constData();
        for (int i = 0; i < frame.samples(); ++i) {
            int32_t pcm;
            if (preview) {
                double value;
                std::memcpy(&value, bytes + i * sizeof(value), sizeof(value));
                const auto scaled = value * 2147483648.0;
                if (!std::isfinite(scaled) || scaled != std::round(scaled)
                    || scaled < std::numeric_limits<int32_t>::min()
                    || scaled > std::numeric_limits<int32_t>::max()) {
                    valid = false;
                    return true;
                }
                pcm = int32_t(scaled);
            } else {
                std::memcpy(&pcm, bytes + i * sizeof(pcm), sizeof(pcm));
            }
            samples.push_back(pcm);
        }
        return true;
    });
    reader->start();
    {
        std::unique_lock lock(mutex);
        ready.wait_for(lock, std::chrono::seconds(15), [&] { return done; });
    }
    reader->stop();
    reader->waitFor(mdk::State::Stopped);
    if (!done || !valid || samples.empty()) throw std::runtime_error("Invalid PCM, format announcement or timestamps");
    return samples;
}

int main(int argc, char** argv) {
    if (argc != 2) return 2;
    mdk::SetGlobalOption("plugins", "mdk-r3d");
    try {
        const auto original = readAudio(argv[1], false);
        const auto preview = readAudio(argv[1], true);
        if (original != preview) throw std::runtime_error("Preview audio changed source PCM samples");
        std::printf("PCM_IDENTICAL %zu\n", original.size());
        return 0;
    } catch (const std::exception& e) {
        std::fprintf(stderr, "%s\n", e.what());
        return 1;
    }
}
