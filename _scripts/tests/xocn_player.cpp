// SPDX-License-Identifier: GPL-3.0-or-later
#include <mdk/Player.h>
#include <atomic>
#include <chrono>
#include <codecvt>
#include <cstdio>
#include <locale>
#include <thread>
extern "C" void niyien_register_xocn_reader();

int wmain(int argc, wchar_t** argv) {
    if (argc != 2 && argc != 3) return 2;
    std::wstring_convert<std::codecvt_utf8_utf16<wchar_t>> utf8;
    const auto source = utf8.to_bytes(argv[1]) + ".xocn";
    mdk::SetGlobalOption("log", "Debug");
    mdk::setLogHandler([](mdk::LogLevel, const char* message) { std::fprintf(stderr, "%s", message); });
    niyien_register_xocn_reader();
    std::atomic<int> frames{0};
    std::atomic<int64_t> last_time{-1};
    std::atomic<int> invalid_before_stop{0};
    std::atomic<bool> stop_requested{false};
    std::atomic<bool> saw_tail{false}, looped{false};
    mdk::Player player;
    player.setMute(true);
    if (argc == 3) player.setActiveTracks(mdk::MediaType::Audio, {});
    player.onMediaStatusChanged([&](mdk::MediaStatus status) {
        if (int(status) & int(mdk::MediaStatus::Invalid)) {
            std::fprintf(stderr, "XOCN_PLAYER_INVALID stop=%d\n", int(stop_requested.load()));
            if (!stop_requested) ++invalid_before_stop;
        }
        return true;
    });
    player.onFrame<mdk::VideoFrame>([&](mdk::VideoFrame& frame, int) {
        if (frame && frame.timestamp() != mdk::TimestampEOS) {
            ++frames; last_time = int64_t(frame.timestamp() * 1000 + 0.5);
            if (last_time >= 6800) saw_tail = true;
            if (saw_tail && last_time < 1000) looped = true;
        }
        return 0;
    });
    player.setVideoSurfaceSize(64, 64);
    player.setMedia(source.c_str());
    player.prepare(0, [&](int64_t position, bool*) {
        const auto& info = player.mediaInfo();
        std::fprintf(stderr, "XOCN_PLAYER_PREPARE position=%lld streams=%zu width=%d\n", (long long)position, info.video.size(), info.video.empty()?0:info.video[0].codec.width);
        return true;
    });
    const auto start = std::chrono::steady_clock::now();
    while (!frames && std::chrono::steady_clock::now() - start < std::chrono::seconds(8)) std::this_thread::sleep_for(std::chrono::milliseconds(10));
    bool seek_ok = frames > 0;
    std::fprintf(stderr, "XOCN_PLAYER_STATUS before_seek=%x\n", unsigned(player.mediaStatus()));
    seek_ok = seek_ok && bool(int(player.mediaStatus()) & int(mdk::MediaStatus::Prepared));
    if (seek_ok) {
        player.set(mdk::State::Running);
        const auto play_started = std::chrono::steady_clock::now();
        while (last_time < 100 && std::chrono::steady_clock::now() - play_started < std::chrono::seconds(3)) std::this_thread::sleep_for(std::chrono::milliseconds(10));
        player.set(mdk::State::Paused);
        seek_ok = last_time >= 100;
        std::fprintf(stderr, "XOCN_PLAYER_INITIAL_PLAY last_time=%lld\n", (long long)last_time.load());
    }
    if (seek_ok) {
        player.setLoop(0);
        player.seek(int64_t(3000), mdk::SeekFlag::FromStart | mdk::SeekFlag::InCache);
        const auto seek_started = std::chrono::steady_clock::now();
        while (last_time != 3000 && std::chrono::steady_clock::now() - seek_started < std::chrono::seconds(6)) std::this_thread::sleep_for(std::chrono::milliseconds(10));
        seek_ok = last_time == 3000;
    }
    if (seek_ok) {
        player.seek(int64_t(6800), mdk::SeekFlag::FromStart);
        const auto tail_started = std::chrono::steady_clock::now();
        while (last_time != 6800 && std::chrono::steady_clock::now() - tail_started < std::chrono::seconds(6)) std::this_thread::sleep_for(std::chrono::milliseconds(10));
        player.setLoop(2);
        player.set(mdk::State::Running);
        const auto loop_started = std::chrono::steady_clock::now();
        while (!looped && std::chrono::steady_clock::now() - loop_started < std::chrono::seconds(12)) std::this_thread::sleep_for(std::chrono::milliseconds(10));
        seek_ok = looped;
    }
    stop_requested = true;
    player.set(mdk::State::Stopped);
    player.waitFor(mdk::State::Stopped, 3000);
    const bool passed = seek_ok && invalid_before_stop == 0;
    std::fprintf(stderr, "XOCN_PLAYER_%s frames=%d last_time=%lld invalid=%d looped=%d\n", passed ? "PASS" : "FAIL", frames.load(), (long long)last_time.load(), invalid_before_stop.load(), int(looped.load()));
    return passed ? 0 : 1;
}
