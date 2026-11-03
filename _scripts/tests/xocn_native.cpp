// SPDX-License-Identifier: GPL-3.0-or-later
#include "mdk/FrameReader.h"
#include "mdk/MediaInfo.h"
#include "../../src/rendering/xocn_codec.h"
#include <codecvt>
#include <cstdio>
#include <fstream>
#include <iterator>
#include <locale>
#include <stdexcept>

extern "C" void niyien_register_xocn_reader();
static void require(bool value, const char* message) { if (!value) throw std::runtime_error(message); }

int wmain(int argc, wchar_t** argv) {
    if (argc < 2) return 2;
    std::wstring_convert<std::codecvt_utf8_utf16<wchar_t>> utf8;
    try {
        if (argc == 4) {
            std::ifstream file(utf8.to_bytes(argv[2]), std::ios::binary);
            const std::vector<uint8_t> data((std::istreambuf_iterator<char>(file)), {});
            const auto raw = xocn::decode(data.data(), data.size());
            require(raw.width == 5016 && raw.height == 3344, "raw geometry changed");
            std::ofstream out(utf8.to_bytes(argv[3]), std::ios::binary);
            out.write(reinterpret_cast<const char*>(raw.pixels.data()), raw.pixels.size() * 2);
            require(bool(out), "raw reference output failed");
        }
        niyien_register_xocn_reader();
        auto reader = mdk::FrameReader::create("XOCN");
        require(bool(reader), "reader registration failed");
        reader->setMedia(utf8.to_bytes(argv[1]));
        reader->setActiveTracks(mdk::MediaType::Video, {0});
        reader->setActiveTracks(mdk::MediaType::Audio, {});
        reader->setBufferFrames(mdk::MediaType::Video, 2);
        bool original_info = false;
        require(reader->start(int64_t(0), [&](const mdk::MediaInfo* info, int64_t, bool*) {
            original_info = info && info->video[0].codec.width == 4992 && info->video[0].codec.height == 3328 && info->video[0].frames == 352;
            return true;
        }), "reader start failed");
        mdk::VideoFrame frame;
        for (int i = 0; i < 8; ++i) {
            reader->take(&frame);
            require(bool(frame), "initial frames missing");
            require(std::abs(frame.timestamp() - i * 0.02) < 0.000001, "initial timestamp changed");
            require(frame.width() == 1620 && frame.height() == 1080, "preview dimensions changed");
        }
        mdk::MediaInfo info{};
        require(reader->get(&info), "source information missing");
        // MDK updates decoded dimensions after the first scaled frame; source
        // geometry is captured by the prepare callback and the application info API.
        require(original_info && info.video[0].frames == 352, "source geometry changed");
        require(std::abs(info.video[0].codec.par - 1.5) < 0.000001, "source pixel aspect lost");
        reader->stop();
        require(reader->waitFor(mdk::State::Stopped, 10000), "stop after initial frames failed");
        require(reader->start(int64_t(3000)), "range restart failed");
        do { reader->take(&frame); } while (!frame && frame.timestamp() != mdk::TimestampEOS);
        require(bool(frame) && std::abs(frame.timestamp() - 3.0) < 0.000001, "restart selected the wrong source frame");
        require(reader->seek(1000), "backward seek request failed");
        bool found = false;
        for (int i = 0; i < 12; ++i) {
            reader->take(&frame);
            if (frame && std::abs(frame.timestamp() - 1.0) < 0.000001) { found = true; break; }
        }
        require(found, "backward seek did not deliver its target");
        reader->stop();
        require(reader->waitFor(mdk::State::Stopped, 10000), "final stop failed");
        require(reader->start(int64_t(7000)), "tail restart failed");
        int tail_frames = 0;
        do { reader->take(&frame); if (frame) ++tail_frames; } while (frame || frame.timestamp() != mdk::TimestampEOS);
        require(tail_frames == 2, "tail frame count changed");
        require(reader->seek(0), "seek after EOS failed");
        do { reader->take(&frame); } while (!frame && frame.timestamp() != mdk::TimestampEOS);
        require(frame && frame.timestamp() == 0.0, "seek after EOS did not restart decoding");
        reader->stop();
        require(reader->waitFor(mdk::State::Stopped, 10000), "stop after EOS seek failed");
        std::puts("XOCN_NATIVE_PASS");
        return 0;
    } catch (const std::exception& error) {
        std::fprintf(stderr, "XOCN_NATIVE_FAIL: %s\n", error.what());
        return 1;
    }
}
