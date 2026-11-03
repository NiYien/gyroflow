// SPDX-License-Identifier: GPL-3.0-or-later
#define NOMINMAX
#define _DISABLE_CONSTEXPR_MUTEX_CONSTRUCTOR
#include "mdk/FrameReader.h"
#include "mdk/MediaIO.h"
#include "mdk/MediaInfo.h"
#include "xocn_codec.h"
#include <array>
#include <atomic>
#include <cmath>
#include <condition_variable>
#include <cstring>
#include <iostream>
#include <mutex>
#include <optional>
#include <stdexcept>
#include <thread>

namespace {
using namespace mdk;
using Bytes = std::vector<uint8_t>;
struct GpuCallbacks {
    xocn::GpuCreate create = nullptr;
    xocn::GpuDestroy destroy = nullptr;
    xocn::GpuReconstruct reconstruct = nullptr;
    xocn::GpuDecode decode = nullptr;
};
GpuCallbacks gpu_callbacks;
uint32_t be32(const uint8_t* p) { return uint32_t(p[0]) << 24 | uint32_t(p[1]) << 16 | uint32_t(p[2]) << 8 | p[3]; }
uint16_t be16(const uint8_t* p) { return uint16_t(p[0]) << 8 | p[1]; }
struct Element { uint64_t offset = 0; uint32_t size = 0; };
struct Frame { Element picture; std::array<Element, 4> audio; };
struct SourceInfo {
    uint32_t width = 0, height = 0, rate_num = 0, rate_den = 0;
    uint64_t frames = 0;
    double duration_ms = 0, bitrate = 0, pixel_aspect = 1;
};
std::string reader_source(std::string value) {
    const std::string suffix = ".xocn";
    if (value.size() >= suffix.size() && value.compare(value.size() - suffix.size(), suffix.size(), suffix) == 0)
        value.resize(value.size() - suffix.size());
    return value;
}

class Clip {
public:
    ~Clip() { if (gpu_context) gpu_backend.destroy(gpu_context); }
    explicit Clip(const std::string& url, bool index) : io(MediaIO::createForUrl(url)) {
        if (!io || !io->isOpen() || !io->isSeekable() || io->size() <= 0)
            throw std::runtime_error("Cannot open seekable X-OCN source");
        const uint64_t length = uint64_t(io->size());
        for (uint64_t position = 0; position < length;) {
            if (length - position < 17) throw std::runtime_error("Truncated MXF key");
            const auto key = read({position, 17});
            if (std::memcmp(key.data(), "\x06\x0e\x2b\x34", 4)) throw std::runtime_error("Invalid MXF key");
            uint64_t size = key[16];
            uint64_t value = position + 17;
            if (size & 0x80) {
                const uint32_t count = uint32_t(size & 0x7f);
                if (!count || count > 8 || count > length - value) throw std::runtime_error("Invalid MXF length");
                const auto encoded = read({value, count});
                value += count;
                size = 0;
                for (auto byte : encoded) size = size << 8 | byte;
            }
            if (size > length - value) throw std::runtime_error("Truncated MXF element");
            position = value + size;
            if (!std::memcmp(key.data() + 4, "\x02\x53\x01\x01\x0d\x01\x01\x01\x01\x01", 10)
                && (key[14] == 0x27 || key[14] == 0x28 || key[14] == 0x29 || key[14] == 0x51 || key[14] == 0x5f)) {
                if (key[14] != 0x27) throw std::runtime_error("Not an X-OCN picture descriptor");
                if (size > 65536) throw std::runtime_error("Oversized MXF picture descriptor");
                descriptor(read({value, uint32_t(size)}));
            }
            if (!std::memcmp(key.data() + 4, "\x01\x02\x01\x06\x0e\x06", 6)) {
                if (size < 128 || size > 256 * 1024 * 1024) throw std::runtime_error("Invalid X-OCN picture size");
                const auto header = read({value, 16});
                if (std::memcmp(header.data(), "SBRh", 4)) throw std::runtime_error("Not an X-OCN H picture");
                const uint32_t stored_width = be16(header.data() + 8), stored_height = be16(header.data() + 10) * 2;
                if (stored_width != raw_width || stored_height != raw_height || !info.width || !info.height
                    || crop_x + info.width > raw_width || crop_y + info.height > raw_height || !info.rate_num || !info.rate_den)
                    throw std::runtime_error("Inconsistent X-OCN source geometry");
                if (!index) return;
                frames.push_back({{value, uint32_t(size)}, {}});
            }
            if (index && !frames.empty() && !std::memcmp(key.data() + 4, "\x01\x02\x01\x01\x0d\x01\x03\x01\x16\x04\x03", 11)) {
                const uint32_t track = key[15];
                if (track < 4 && size <= 1024 * 1024 && size % 3 == 0)
                    frames.back().audio[track] = {value, uint32_t(size)};
            }
        }
        if (frames.empty()) throw std::runtime_error("No X-OCN pictures");
        info.frames = frames.size();
        info.duration_ms = 1000.0 * info.frames * info.rate_den / info.rate_num;
        info.bitrate = double(length) * 8 / info.duration_ms / 1000;
    }
    Bytes read(Element element) {
        std::lock_guard lock(io_mutex);
        if (!io->seek(int64_t(element.offset))) throw std::runtime_error("X-OCN seek failed");
        Bytes result(element.size);
        size_t done = 0;
        while (done < result.size()) {
            const int64_t count = io->read(result.data() + done, int64_t(result.size() - done));
            if (count <= 0) throw std::runtime_error("X-OCN read failed");
            done += size_t(count);
        }
        return result;
    }
    Bytes take_cached_picture(size_t index) {
        Bytes bytes;
        if (cached_picture && cached_picture->first == index) bytes = std::move(cached_picture->second);
        cached_picture.reset();
        return bytes;
    }
    VideoFrame video(size_t index, int target_height, bool prefetch = false) {
        auto compressed = take_cached_picture(index);
        if (compressed.empty()) compressed = read(frames.at(index).picture);
        const double scale = std::min({1.0, 1920.0 / info.width, double(target_height) / info.height});
        const int output_width = std::max(2, int(info.width * scale) & ~1);
        const int output_height = std::max(2, int(info.height * scale) & ~1);
        VideoFrame output(output_width, output_height, PixelFormat::RGBA);
        output.setBuffers(nullptr);
        auto* pixels = output.buffer()->data();
        const auto stride = output.buffer()->stride();
        if (!gpu_attempted) {
            gpu_attempted = true;
            gpu_backend = gpu_callbacks;
            if (gpu_backend.create) {
                gpu_context = gpu_backend.create();
                if (!gpu_context) std::cerr << "XOCN GPU unavailable; using CPU reconstruction\n";
            }
        }
        const xocn::GpuImage image{raw_width, raw_height, crop_x, crop_y, info.width, info.height,
            uint32_t(output_width), uint32_t(output_height), uint32_t(stride), 0};
        Prefetch next{this, index + 1};
        const bool gpu_decoded = gpu_context && (gpu_backend.decode
            ? xocn::decode_gpu_compressed(compressed.data(), compressed.size(), gpu_context, gpu_backend.decode, image, tone.data(), pixels,
                prefetch ? prefetch_next : nullptr, prefetch ? &next : nullptr)
            : xocn::decode_gpu(compressed.data(), compressed.size(), gpu_context, gpu_backend.reconstruct, image, tone.data(), pixels));
        if (!gpu_decoded) {
            if (gpu_context) {
                gpu_backend.destroy(gpu_context);
                gpu_context = nullptr;
                std::cerr << "XOCN GPU reconstruction failed; using CPU reconstruction\n";
            }
            const auto raw = xocn::decode(compressed.data(), compressed.size());
            if (raw.width != int(raw_width) || raw.height != int(raw_height)) throw std::runtime_error("X-OCN raster changed");
            // Preview uses native sensor pixels, before display SAR or optical corrections.
            for (int y = 0; y < output_height; ++y) {
                auto* row = pixels + y * stride;
                const uint32_t source_y = (crop_y + uint32_t(y) * info.height / output_height) & ~1u;
                for (int x = 0; x < output_width; ++x) {
                    const uint32_t source_x = (crop_x + uint32_t(x) * info.width / output_width) & ~1u;
                    const size_t offset = size_t(source_y) * raw_width + source_x;
                    row[x * 4] = tone[raw.pixels[offset]];
                    row[x * 4 + 1] = tone[(uint32_t(raw.pixels[offset + 1]) + raw.pixels[offset + raw_width]) / 2];
                    row[x * 4 + 2] = tone[raw.pixels[offset + raw_width + 1]];
                    row[x * 4 + 3] = 255;
                }
            }
        }
        const double seconds = double(index) * info.rate_den / info.rate_num;
        output.setTimestamp(seconds).setDuration(double(info.rate_den) / info.rate_num);
        return output;
    }
    void prepare_tone() {
        for (size_t i = 0; i < tone.size(); ++i) {
            const double linear = std::clamp((double(i) - 512) * 0.18 / (1504 - 512), 0.0, 1.0);
            const double srgb = linear <= 0.0031308 ? linear * 12.92 : 1.055 * std::pow(linear, 1.0 / 2.4) - 0.055;
            tone[i] = uint8_t(std::lround(srgb * 255));
        }
    }
    SourceInfo info;
    std::vector<Frame> frames;
private:
    struct Prefetch { Clip* clip; size_t index; };
    static void prefetch_next(void* user) noexcept {
        const auto& next = *static_cast<Prefetch*>(user);
        auto& clip = *next.clip;
        clip.cached_picture.reset();
        if (next.index >= clip.frames.size()) return;
        try {
            clip.cached_picture.emplace(next.index, clip.read(clip.frames[next.index].picture));
        } catch (...) {
            // Retry this source read normally when its frame is requested.
            clip.cached_picture.reset();
        }
    }
    void descriptor(const Bytes& bytes) {
        double aspect = 0;
        bool sony_raw = false;
        for (size_t position = 0; position + 4 <= bytes.size();) {
            const auto tag = be16(bytes.data() + position), length = be16(bytes.data() + position + 2);
            position += 4;
            if (length > bytes.size() - position) throw std::runtime_error("Invalid MXF descriptor field");
            const auto* value = bytes.data() + position;
            if (length == 4) switch (tag) {
                case 0x3202: raw_height = be32(value); break;
                case 0x3203: raw_width = be32(value); break;
                case 0x3208: info.height = be32(value); break;
                case 0x3209: info.width = be32(value); break;
                case 0x320a: crop_x = be32(value); break;
                case 0x320b: crop_y = be32(value); break;
            }
            if (tag == 0x3001 && length == 8) { info.rate_num = be32(value); info.rate_den = be32(value + 4); }
            if (tag == 0x320e && length == 8 && be32(value + 4)) aspect = double(be32(value)) / be32(value + 4);
            if (tag == 0x3201 && length == 16)
                sony_raw = !std::memcmp(value, "\x06\x0e\x2b\x34", 4) && !std::memcmp(value + 8, "\x0e\x06\x04\x01\x02", 5);
            position += length;
        }
        if (!sony_raw) throw std::runtime_error("Not a Sony RAW picture descriptor");
        if (aspect > 0 && info.width && info.height) info.pixel_aspect = aspect * info.height / info.width;
    }
    std::unique_ptr<MediaIO> io;
    std::mutex io_mutex;
    uint32_t raw_width = 0, raw_height = 0, crop_x = 0, crop_y = 0;
    std::array<uint8_t, 65536> tone{};
    GpuCallbacks gpu_backend;
    void* gpu_context = nullptr;
    bool gpu_attempted = false;
    std::optional<std::pair<size_t, Bytes>> cached_picture;
};

class XocnReader final : public FrameReader {
public:
    const char* name() const override { return "XOCN"; }
    bool isSupported(const char* source, MediaType) const override {
        try { Clip clip(reader_source(source), false); return true; } catch (...) { return false; }
    }
    ~XocnReader() override {
        if (worker.joinable()) worker.join();
        if (audio_worker.joinable()) audio_worker.join();
    }
    void setTimeout(int64_t, TimeoutCallback) override {}
    int64_t buffered(int64_t*, float*) const override { return 0; }
    bool load() override {
        try {
            if (worker.joinable()) worker.join();
            if (audio_worker.joinable()) audio_worker.join();
            clip = std::make_unique<Clip>(reader_source(url()), true);
            clip->prepare_tone();
            const auto& source = clip->info;
            MediaInfo info{};
            info.format = "mxf";
            info.duration = int64_t(std::llround(source.duration_ms));
            info.bit_rate = int64_t(source.bitrate * 1000000);
            VideoStreamInfo video{};
            video.index = 0; video.frames = source.frames; video.duration = info.duration;
            video.codec.codec = "XOCN"; video.codec.width = source.width; video.codec.height = source.height;
            video.codec.frame_rate = float(double(source.rate_num) / source.rate_den);
            video.codec.format = PixelFormat::RGBA; video.codec.par = float(source.pixel_aspect);
            info.video.push_back(video);
            for (int track = 0; track < 4; ++track) if (clip->frames.front().audio[track].size) {
                AudioStreamInfo audio{};
                audio.index = track + 1; audio.duration = info.duration;
                audio.codec.codec = "pcm_s24le"; audio.codec.sample_rate = 48000; audio.codec.channels = 1;
                audio.codec.format = AudioFormat::SampleFormat::S32;
                audio.codec.channel_layout = AudioFormat(AudioFormat::SampleFormat::S32, 1, 48000).channelMap();
                audio.codec.bits_per_coded_sample = 24; audio.codec.bits_per_raw_sample = 24;
                audio.codec.block_align = 3; audio.codec.frame_size = int(clip->frames.front().audio[track].size / 3);
                audio.codec.bit_rate = 48000 * 24;
                for (const auto& frame : clip->frames) audio.frames += frame.audio[track].size / 3;
                info.audio.push_back(audio);
            }
            info.streams = int(info.video.size() + info.audio.size());
            stopping = false; next = 0; next_audio = 0; seek_id = 0; audio_seek_id = 0; video_ready_generation = 0; ++generation;
            changed(info);
            update(MediaStatus::Loaded);
            MediaEvent decoder{}; decoder.category = "decoder.video"; decoder.detail = "XOCN"; dispatchEvent(decoder);
            updateBufferingProgress(0);
            if (state() == State::Stopped) update(State::Running);
            worker = std::thread([this] { run(); });
            if (!activeTracks(MediaType::Audio).empty()) audio_worker = std::thread([this] { run_audio(); });
            return true;
        } catch (const std::exception& error) {
            std::cerr << "XOCN load: " << error.what() << '\n';
            return false;
        }
    }
    bool unload() override {
        stopping = true;
        ++generation;
        wake.notify_all();
        if (worker.joinable() && worker.get_id() != std::this_thread::get_id()) worker.join();
        if (audio_worker.joinable() && audio_worker.get_id() != std::this_thread::get_id()) audio_worker.join();
        clip.reset();
        update(MediaStatus::Unloaded);
        update(State::Stopped);
        return true;
    }
protected:
    bool seekTo(int64_t milliseconds, SeekFlag flags, int id) override {
        const bool wait_frame = !test_flag(flags, SeekFlag::IOCompleteCallback);
        int64_t position;
        {
            std::lock_guard lock(control);
            if (!clip || stopping) return false;
            if (test_flag(flags, SeekFlag::Frame | SeekFlag::FromNow))
                next = size_t(std::clamp<int64_t>(int64_t(current) + milliseconds, 0, int64_t(clip->frames.size() - 1)));
            else next = size_t(std::clamp<int64_t>(std::llround(std::max<int64_t>(0, milliseconds) * double(clip->info.rate_num) / clip->info.rate_den / 1000), 0, int64_t(clip->frames.size() - 1)));
            position = int64_t(std::llround(double(next) * clip->info.rate_den / clip->info.rate_num * 1000));
            seek_id = id;
            seek_wait_frame = wait_frame;
            next_audio = next;
            audio_seek_id = id;
            ++generation;
        }
        // A loop seeks from inside frameAvailable() and waits for this callback.
        // The indexed cursor is positioned now; waiting for this worker's next
        // decoded frame would deadlock the worker that issued the loop.
        if (!wait_frame) seekComplete(position, id);
        wake.notify_all();
        return true;
    }
private:
    void run() {
        try {
            while (!stopping) {
                size_t index;
                int id;
                bool wait_frame;
                uint64_t serial;
                { std::lock_guard lock(control); index = next; id = seek_id; wait_frame = seek_wait_frame; serial = generation; }
                if (index >= clip->frames.size()) {
                    VideoFrame eos; eos.setTimestamp(TimestampEOS); frameAvailable(eos);
                    if (serial != generation) continue;
                    update(mediaStatus() | MediaStatus::End);
                    // Keep the source alive for a later seek or a playback loop.
                    std::unique_lock lock(control);
                    wake.wait(lock, [&] { return stopping || serial != generation; });
                    continue;
                }
                auto video = clip->video(index, 1080, true);
                if (stopping || serial != generation) continue;
                if (index == 0 || id > 0) updateBufferingProgress(100);
                if (id > 0) {
                    if (wait_frame) seekComplete(int64_t(std::llround(video.timestamp() * 1000)), id);
                    VideoFrame flush(video.format()); flush.setTimestamp(video.timestamp()); frameAvailable(flush);
                }
                current = index;
                const bool accepted = frameAvailable(video);
                if (stopping) return;
                if (!accepted || serial != generation) continue;
                { std::lock_guard lock(control); if (serial == generation && accepted) { next = index + 1; seek_id = 0; video_ready_generation = serial; } }
                wake.notify_all();
            }
        } catch (const std::exception& error) {
            std::cerr << "XOCN decode: " << error.what() << '\n';
            MediaEvent event{}; event.category = "decoder.video"; event.detail = "XOCN"; event.error = -1; dispatchEvent(event);
            if (seek_id > 0) seekComplete(-1, seek_id);
            VideoFrame end; end.setTimestamp(TimestampEOS); frameAvailable(end);
        }
    }
    void run_audio() {
        try {
            const AudioFormat format(AudioFormat::SampleFormat::S32, 1, 48000);
            while (!stopping) {
                size_t index;
                int id;
                uint64_t serial;
                {
                    std::unique_lock lock(control);
                    // The audio renderer rejects a seek flush until its video target is ready.
                    wake.wait(lock, [&] { return stopping || video_ready_generation == generation; });
                    if (stopping) return;
                    index = next_audio; id = audio_seek_id; serial = generation;
                }
                if (index >= clip->frames.size()) {
                    for (int track : activeTracks(MediaType::Audio)) {
                        AudioFrame end; end.setTimestamp(TimestampEOS); frameAvailable(end, track);
                    }
                    std::unique_lock lock(control);
                    wake.wait(lock, [&] { return stopping || serial != generation; });
                    continue;
                }
                const double timestamp = double(index) * clip->info.rate_den / clip->info.rate_num;
                bool accepted = true;
                for (int track : activeTracks(MediaType::Audio)) {
                    if (stopping || serial != generation) break;
                    if (track < 0 || track >= 4) continue;
                    const auto element = clip->frames[index].audio[track];
                    if (!element.size) continue;
                    const auto pcm = clip->read(element);
                    if (stopping || serial != generation) break;
                    if (id > 0 || index == 0) {
                        updateBufferingProgress(100);
                        AudioFrame flush(format); flush.setTimestamp(timestamp); frameAvailable(flush, track);
                    }
                    AudioFrame audio(format, size_t(pcm.size() / 3) * 4);
                    auto* target = reinterpret_cast<uint32_t*>(audio.buffer()->data());
                    for (size_t i = 0; i < pcm.size() / 3; ++i)
                        target[i] = uint32_t(pcm[i * 3]) << 8 | uint32_t(pcm[i * 3 + 1]) << 16 | uint32_t(pcm[i * 3 + 2]) << 24;
                    audio.setTimestamp(timestamp);
                    accepted &= frameAvailable(audio, track);
                }
                { std::lock_guard lock(control); if (serial == generation && accepted) { next_audio = index + 1; audio_seek_id = 0; } }
            }
        } catch (const std::exception& error) {
            std::cerr << "XOCN audio: " << error.what() << '\n';
            for (int track : activeTracks(MediaType::Audio)) { AudioFrame end; end.setTimestamp(TimestampEOS); frameAvailable(end, track); }
        }
    }
    std::unique_ptr<Clip> clip;
    std::thread worker;
    std::thread audio_worker;
    std::mutex control;
    std::condition_variable wake;
    std::atomic<bool> stopping{false};
    std::atomic<uint64_t> generation{0};
    size_t next = 0, next_audio = 0;
    std::atomic<size_t> current{0};
    int seek_id = 0, audio_seek_id = 0;
    bool seek_wait_frame = true;
    uint64_t video_ready_generation = 0;
};
}

extern "C" void niyien_xocn_register_gpu(xocn::GpuCreate create, xocn::GpuDestroy destroy, xocn::GpuReconstruct reconstruct, xocn::GpuDecode decode) {
    gpu_callbacks = {create, destroy, reconstruct, decode};
}
extern "C" void niyien_register_xocn_reader() {
    if (mdk::abiVersion() != MDK_ABI_VERSION) return;
    mdk::FrameReader::registerOnce("XOCN", [] { return new XocnReader; });
}
extern "C" int niyien_xocn_info(const char* url, SourceInfo* output) {
    if (mdk::abiVersion() != MDK_ABI_VERSION) return -1;
    try { Clip clip(url, output != nullptr); if (output) *output = clip.info; return 0; }
    catch (...) { return -1; }
}

struct XocnRange { double from, to; };
extern "C" int niyien_xocn_read(const char* source, const XocnRange* ranges, size_t count,
    bool (*receive)(void*, double, uint32_t, uint32_t, uint32_t, const uint8_t*),
    bool (*cancelled)(void*), void* user) {
    try {
        if (mdk::abiVersion() != MDK_ABI_VERSION) return -1;
        if (cancelled && cancelled(user)) return 0;
        Clip clip(source, true);
        clip.prepare_tone();
        const double fps = double(clip.info.rate_num) / clip.info.rate_den;
        for (size_t range = 0; range < std::max<size_t>(1, count); ++range) {
            const auto from = count ? ranges[range].from : 0.0;
            const auto to = count ? ranges[range].to : clip.info.duration_ms;
            const size_t first = size_t(std::clamp<double>(std::ceil((from - 0.001) * fps / 1000), 0, clip.frames.size()));
            const size_t end = size_t(std::clamp<double>(std::floor((to + 0.001) * fps / 1000) + 1, 0, clip.frames.size()));
            for (size_t index = first; index < end; ++index) {
                if (cancelled && cancelled(user)) return 0;
                const auto frame = clip.video(index, 1080, index + 1 < end);
                if (!receive(user, frame.timestamp() * 1000, frame.width(), frame.height(), frame.buffer()->stride(), frame.buffer()->data())) return 0;
            }
        }
        return 0;
    } catch (const std::exception& error) {
        std::cerr << "XOCN analysis: " << error.what() << '\n';
        return -1;
    }
}
