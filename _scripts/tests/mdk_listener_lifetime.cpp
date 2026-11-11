// SPDX-License-Identifier: GPL-3.0-or-later

#define NOMINMAX
#include <QGuiApplication>
#include <QQuickItem>
#include <QJsonObject>
#include <QHash>
#include <QTimer>
#include <future>
#include <chrono>
#include <queue>
#include <atomic>
#include <array>
#include <functional>
#include <memory>
#include <mutex>
#include <vector>
#include <map>
#include <set>
#include <thread>
#include <cstdio>
#include "VideoTextureNode.h"
#include <mdk/global.h>
#include <mdk/MediaInfo.h>
#include <mdk/RenderAPI.h>
#include <mdk/c/Player.h>
#include <mdk/AudioFrame.h>
#include <mdk/VideoFrame.h>

// Expose only the wrapper and SDK player internals in this test translation unit.
#define private public
#include <mdk/Player.h>
using mdk::MediaInfo;
#include "MDKPlayer.h"
#undef private
#include "VideoTextureNode.cpp"
#include "MDKPlayer.cpp"

static void require(bool passed, const char *message) {
    if (!passed) { std::fprintf(stderr, "LISTENER_LIFETIME_FAIL %s\n", message); std::exit(2); }
}

int main(int argc, char **argv) {
    QGuiApplication app(argc, argv);
    mdk::SetGlobalOption("log", "Warning");
    auto wrapper = std::make_unique<MDKPlayer>();
    wrapper->initPlayer();
    wrapper->setupPlayer();
    auto *native = wrapper->m_player.get();
    auto *events = &native->event_cb_;
    auto *statuses = &native->status_cb_;
    require(events->size() == 1 && statuses->size() == 1, "unexpected listener count");
    auto event = events->begin()->second;
    auto status = statuses->begin()->second;
    auto guard = wrapper->m_listenerGuard;
    mdk::MediaEvent sample{};
    sample.category = "test";
    require(event(sample) && status(mdk::MediaStatus(0), mdk::MediaStatus(0)), "live listeners rejected dispatch");
    std::puts("LIVE_LISTENERS_PASS");

    std::promise<void> entered, release, finished;
    auto enteredFuture = entered.get_future();
    auto releaseFuture = release.get_future();
    auto finishedFuture = finished.get_future();
    std::thread active([&] {
        const std::lock_guard<std::recursive_mutex> lock(guard->mutex);
        // A recursive callback must remain valid on MDK's calling thread.
        require(status(mdk::MediaStatus(0), mdk::MediaStatus(0)), "recursive dispatch failed");
        entered.set_value();
        releaseFuture.wait();
    });
    enteredFuture.wait();
    std::thread unblock([&] {
        require(finishedFuture.wait_for(std::chrono::milliseconds(50)) == std::future_status::timeout,
                "destroy returned while the listener guard was held");
        release.set_value();
    });
    wrapper->destroyPlayer();
    finished.set_value();
    active.join();
    unblock.join();
    require(!guard->alive && !wrapper->m_listenerGuard, "destroy did not invalidate the guard");
    require(events->size() == 1 && statuses->size() == 1, "destroy removed or added listeners");
    require(!event(sample) && !status(mdk::MediaStatus(0), mdk::MediaStatus(0)), "retired listeners accepted dispatch");
    std::puts("DESTROY_WAITS_FOR_LISTENER_PASS");

    wrapper->initPlayer();
    wrapper->setupPlayer();
    auto nextGuard = wrapper->m_listenerGuard;
    require(nextGuard != guard && nextGuard->alive, "new player reused the retired guard");
    require(!event(sample) && !status(mdk::MediaStatus(0), mdk::MediaStatus(0)), "old listeners acted on the new player");
    native = wrapper->m_player.get();
    auto nextEvent = native->event_cb_.begin()->second;
    auto nextStatus = native->status_cb_.begin()->second;
    require(nextEvent(sample) && nextStatus(mdk::MediaStatus(0), mdk::MediaStatus(0)), "new listeners were disabled");
    std::puts("REPLACEMENT_LISTENERS_PASS");

    wrapper.reset();
    require(!nextGuard->alive, "wrapper destructor left listeners alive");
    require(!nextEvent(sample) && !nextStatus(mdk::MediaStatus(0), mdk::MediaStatus(0)), "listeners touched a destroyed wrapper");
    std::puts("LATE_DISPATCH_AFTER_DESTRUCTION_PASS");
    QTimer::singleShot(1100, &app, &QCoreApplication::quit);
    const auto result = app.exec();
    std::puts("LISTENER_LIFETIME_PASS");
    return result;
}
