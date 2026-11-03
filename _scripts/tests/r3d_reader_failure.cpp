// SPDX-License-Identifier: GPL-3.0-or-later
// Compile the real reader with failing OpenCL uploads, without production test switches.
#define CreateOpenCLDebayer UnusedOpenCLFactory
#include "../../vendor/mdk-r3d/OpenCLDebayer.cpp"
#undef CreateOpenCLDebayer
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <cstdio>

static std::atomic_bool failUploads{false};
static bool failNextSession = true;
static bool failPacking = false;
static decltype(&clEnqueueWriteBuffer) realWrite = nullptr;
static decltype(&clCreateKernel) realKernel = nullptr;
static cl_int CL_API_CALL upload(cl_command_queue queue, cl_mem memory, cl_bool block, size_t offset, size_t size, const void* source, cl_uint count, const cl_event* waits, cl_event* event) {
    if (failUploads.load() && !failPacking) return CL_OUT_OF_RESOURCES;
    return realWrite(queue, memory, block, offset, size, source, count, waits, event);
}
static cl_kernel CL_API_CALL createKernel(cl_program program, const char* name, cl_int* error) {
    if (failUploads.load() && failPacking && std::strcmp(name, "pack_rgb16") == 0) {
        if (error) *error = CL_INVALID_KERNEL_NAME;
        return nullptr;
    }
    return realKernel(program, name, error);
}
MDK_NS_BEGIN
GpuDebayer::Ptr CreateOpenCLDebayer() {
    failUploads.store(false);
    OpenCLApi api;
    if (!api.load()) return nullptr;
    realWrite = api.sdk.clEnqueueWriteBuffer;
    api.sdk.clEnqueueWriteBuffer = upload;
    realKernel = api.sdk.clCreateKernel;
    api.sdk.clCreateKernel = createKernel;
    std::vector<R3DSDK::OpenCLDeviceInfo> devices;
    if (R3DSDK::R3DDecoderOptions::GetOpenCLDeviceList(devices) != R3DSDK::R3DStatus_Ok || devices.empty()) return nullptr;
    auto adapter = std::make_unique<OpenCLDebayer>(api);
    if (!adapter->init(devices[0])) return nullptr;
    failUploads.store(failNextSession);
    return adapter;
}
MDK_NS_END
#include "../../vendor/mdk-r3d/R3DReader.cpp"
#include "../../vendor/mdk-r3d/R3DCxxAbi.cpp"
#include "../../vendor/mdk-r3d/Debayer.cpp"

static std::string utf8(const wchar_t* value) {
    const int bytes = WideCharToMultiByte(CP_UTF8, 0, value, -1, nullptr, 0, nullptr, nullptr);
    std::string result(bytes, '\0');
    WideCharToMultiByte(CP_UTF8, 0, value, -1, result.data(), bytes, nullptr, nullptr);
    result.pop_back(); return result;
}
#ifndef R3D_FAILURE_PLUGIN
int wmain(int argc, wchar_t** argv) {
    setvbuf(stdout, nullptr, _IONBF, 0);
    if (argc < 3 || argc > 5) return 2;
    const auto runtime = utf8(argv[1]), path = utf8(argv[2]);
    const int ahead = argc >= 4 ? std::stoi(utf8(argv[3])) : 2;
    if (argc == 5) {
        const auto failure = utf8(argv[4]);
        if (failure != "pack" && failure != "upload") return 2;
        failPacking = failure == "pack";
    }
    if (ahead != 1 && ahead != 2) return 2;
    mdk::SetGlobalOption("R3DSDK_DIR", runtime.c_str());
    mdk::FrameReader::registerOnce("R3DFailureTest", [] { return new mdk::R3DReader; });
    auto reader = mdk::FrameReader::create("R3DFailureTest");
    if (!reader) return 3;
    reader->setMedia(path);
    reader->setActiveTracks(mdk::MediaType::Video, {0});
    reader->setActiveTracks(mdk::MediaType::Audio, {});
    reader->setDecoders(mdk::MediaType::Video, {"R3D:gpu=opencl:scale=1920x1080:ahead=" + std::to_string(ahead)});
    std::mutex mutex;
    std::condition_variable cv;
    bool invalid = false;
    int frames = 0;
    reader->onMediaStatusChanged([&](mdk::MediaStatus, mdk::MediaStatus value) {
        if (value & mdk::MediaStatus::Invalid) { std::lock_guard lock(mutex); invalid = true; cv.notify_one(); }
        return true;
    });
    reader->onRead<mdk::VideoFrame>([&](const mdk::VideoFrame& frame, int) {
        if (frame && frame.format() && frame.timestamp() != mdk::TimestampEOS) { std::lock_guard lock(mutex); ++frames; cv.notify_one(); }
        return true;
    });
    reader->start(int64_t(0));
    {
        std::unique_lock lock(mutex);
        if (!cv.wait_for(lock, std::chrono::seconds(10), [&] { return invalid; })) { std::puts("FAILURE_NOT_REPORTED"); ExitProcess(4); }
    }
    reader->stop();
    if (!reader->waitFor(mdk::State::Stopped, 10000)) { std::puts("FAILURE_STOP_TIMEOUT"); ExitProcess(5); }
    if (frames != 0) { std::printf("FAILED_FRAME_DELIVERED count=%d\n", frames); return 6; }
    std::puts("DECODE_FAILURE_REPORTED_AND_STOPPED");
    failNextSession = false;
    { std::lock_guard lock(mutex); invalid = false; frames = 0; }
    reader->start(int64_t(0));
    {
        std::unique_lock lock(mutex);
        if (!cv.wait_for(lock, std::chrono::seconds(10), [&] { return frames >= 3 || invalid; }) || invalid) { std::puts("RECOVERY_FAILED"); ExitProcess(7); }
    }
    reader->stop();
    if (!reader->waitFor(mdk::State::Stopped, 10000)) { std::puts("RECOVERY_STOP_TIMEOUT"); ExitProcess(8); }
    std::printf("READER_FAILURE_RECOVERY_PASS ahead=%d frames=%d\n", ahead, frames);
    return 0;
}
#endif
