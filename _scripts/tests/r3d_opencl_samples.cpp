// SPDX-License-Identifier: GPL-3.0-or-later
// Exercise the production adapter against real SDK decoding and a strict OpenCL dispatch.
#include "../../vendor/mdk-r3d/OpenCLDebayer.cpp"
#include "../../vendor/mdk-r3d/base/ByteArray.h"
#include <chrono>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <string>

static decltype(&clCreateSubBuffer) realSubBuffer = nullptr;
static cl_uint alignmentBytes = 0;
static size_t subBuffers = 0, rejectedSubBuffers = 0;
static cl_mem CL_API_CALL strictSubBuffer(cl_mem buffer, cl_mem_flags flags, cl_buffer_create_type type, const void* info, cl_int* error) {
    ++subBuffers;
    const auto& region = *static_cast<const cl_buffer_region*>(info);
    if (region.origin % alignmentBytes) {
        ++rejectedSubBuffers;
        if (error) *error = CL_MISALIGNED_SUB_BUFFER_OFFSET;
        return nullptr;
    }
    return realSubBuffer(buffer, flags, type, info, error);
}
static std::string utf8(const wchar_t* value) {
    const int bytes = WideCharToMultiByte(CP_UTF8, 0, value, -1, nullptr, 0, nullptr, nullptr);
    std::string result(bytes, '\0');
    WideCharToMultiByte(CP_UTF8, 0, value, -1, result.data(), bytes, nullptr, nullptr);
    result.pop_back(); return result;
}
struct Completion { std::mutex mutex; std::condition_variable cv; bool done = false; int status = 0; };
static bool wait(Completion& completion) {
    std::unique_lock lock(completion.mutex);
    if (!completion.cv.wait_for(lock, std::chrono::seconds(30), [&] { return completion.done; })) { std::puts("CALLBACK_TIMEOUT"); ExitProcess(20); }
    completion.done = false;
    return completion.status == 0;
}
int wmain(int argc, wchar_t** argv) {
    setvbuf(stdout, nullptr, _IONBF, 0);
    if (argc != 5) { std::puts("usage: test <runtime> <video> <divisor> <frames>"); return 2; }
    const auto runtime = utf8(argv[1]), path = utf8(argv[2]);
    const int divisor = std::stoi(utf8(argv[3])), count = std::stoi(utf8(argv[4]));
    if ((divisor != 1 && divisor != 2 && divisor != 4 && divisor != 8 && divisor != 16) || count <= 0) return 2;
    for (auto alignment : {uint16_t(64), uint16_t(512), uint16_t(4096)}) {
        for (int size = 257; size < 1200; size += 37) {
            mdk::ByteArray buffer(size, alignment);
            if (!buffer || reinterpret_cast<uintptr_t>(buffer.data()) % alignment) { std::printf("ALIGNMENT_FAIL requested=%u\n", alignment); return 3; }
            for (int i = 0; i < size; ++i) buffer[i] = uint8_t(i * 13);
            const auto copy = buffer;
            if (std::memcmp(copy.constData(), buffer.constData(), size)) return 3;
        }
    }
    std::puts("ALIGNMENT_PASS");
    using namespace R3DSDK;
    if (InitializeSdk(runtime.c_str(), OPTION_RED_DECODER | OPTION_RED_OPENCL | OPTION_DELAY_GPU_COMPILE) != ISInitializeOK) return 4;
    std::printf("SDK %s\n", GetSdkVersion());
    Clip clip(path.c_str());
    if (clip.Status() != LSClipLoaded) return 5;
    auto mode = divisor == 1 ? DECODE_FULL_RES_PREMIUM : divisor == 2 ? DECODE_HALF_RES_PREMIUM : divisor == 4 ? DECODE_QUARTER_RES_GOOD : divisor == 8 ? DECODE_EIGHT_RES_GOOD : DECODE_SIXTEENTH_RES_GOOD;
    std::vector<OpenCLDeviceInfo> devices;
    if (R3DDecoderOptions::GetOpenCLDeviceList(devices) != R3DStatus_Ok || devices.empty()) return 6;
    mdk::OpenCLApi api;
    if (!api.load()) return 6;
    cl_uint alignmentBits = 0;
    if (api.sdk.clGetDeviceInfo(static_cast<cl_device_id>(devices[0].device_id), CL_DEVICE_MEM_BASE_ADDR_ALIGN, sizeof(alignmentBits), &alignmentBits, nullptr) != CL_SUCCESS) return 6;
    alignmentBytes = alignmentBits / 8;
    realSubBuffer = api.sdk.clCreateSubBuffer;
    api.sdk.clCreateSubBuffer = strictSubBuffer;
    mdk::OpenCLDebayer adapter(api);
    if (!adapter.init(devices[0])) return 6;
    ImageProcessingSettings settings;
    clip.GetDefaultImageProcessingSettings(settings);
    settings.ImagePipelineMode = Full_Graded; settings.HdrPeakNits = 1000; settings.CdlEnabled = true; settings.OutputToneMap = ToneMap_None;
    const int width = int(clip.Width() / divisor), height = int(clip.Height() / divisor);
    R3DDecoderOptions* options = nullptr;
    if (R3DDecoderOptions::CreateOptions(&options) != R3DStatus_Ok) return 7;
    options->setMemoryPoolSize(4096); options->setGPUMemoryPoolSize(4096); options->setGPUConcurrentFrameCount(1);
    options->setDecompressionThreadCount(0); options->setConcurrentImageCount(0);
    if (options->useDevice(devices[0]) != R3DStatus_Ok) return 7;
    R3DDecoder* reference = nullptr;
    const auto created = R3DDecoder::CreateDecoder(options, &reference);
    R3DDecoderOptions::ReleaseOptions(options);
    if (created != R3DStatus_Ok) return 7;
    Completion nativeCompletion, rawCompletion;
    mdk::VideoFrame expected(width, height, mdk::PixelFormat::BGRX);
    expected.setBuffers(nullptr);
    R3DDecodeJob* native = nullptr;
    if (R3DDecoder::CreateDecodeJob(&native) != R3DStatus_Ok) return 7;
    native->clip = &clip; native->videoTrackNo = 0; native->mode = mode;
    native->pixelType = PixelType_8Bit_BGRA_Interleaved; native->imageProcessingSettings = &settings;
    native->outputBuffer = expected.buffer()->data(); native->bytesPerRow = expected.buffer()->stride();
    native->outputBufferSize = expected.format().bytesPerFrame(width, height); native->privateData = &nativeCompletion;
    native->callback = [](R3DDecodeJob* job, R3DStatus status) {
        auto& state = *static_cast<Completion*>(job->privateData);
        std::lock_guard lock(state.mutex); state.status = int(status); state.done = true; state.cv.notify_one();
    };
    AsyncDecompressJob raw;
    raw.Clip = &clip; raw.Mode = mode; raw.VideoTrackNo = 0; raw.VideoFrameNo = 0; raw.AbortDecode = false; raw.OutputFrameMetadata = nullptr;
    raw.OutputBufferSize = AsyncDecoder::GetSizeBufferNeeded(raw);
    mdk::ByteArray input(int(raw.OutputBufferSize), uint16_t(4096));
    if (!input || reinterpret_cast<uintptr_t>(input.data()) % 4096) return 8;
    raw.OutputBuffer = input.data(); raw.PrivateData = &rawCompletion;
    raw.Callback = [](AsyncDecompressJob* job, DecodeStatus status) {
        auto& state = *static_cast<Completion*>(job->PrivateData);
        std::lock_guard lock(state.mutex); state.status = int(status); state.done = true; state.cv.notify_one();
    };
    AsyncDecoder decompressor; decompressor.Open();
    const size_t frames = std::min<size_t>(count, clip.VideoFrameCount());
    bool passed = true;
    for (size_t index = 0; index < frames; ++index) {
        native->videoFrameNo = index;
        if (reference->decode(native) != R3DStatus_Ok || !wait(nativeCompletion)) return 9;
        raw.VideoFrameNo = index;
        if (decompressor.DecodeForGpuSdk(raw) != DSDecodeOK || !wait(rawCompletion)) return 9;
        auto job = adapter.createJob(input.data(), raw.OutputBufferSize, width, height, mode, PixelType_8Bit_BGRA_Interleaved, &settings);
        const auto submitted = adapter.submit(job);
        const auto actual = adapter.wait(job, true);
        adapter.releaseJob(job);
        if (submitted != mdk::GpuDebayer::Status_Ok || !actual) { std::printf("PROCESS_FAIL frame=%zu status=%d\n", index, int(submitted)); return 10; }
        size_t changed = 0; int maximum = 0;
        for (int y = 0; y < height; ++y) for (int x = 0; x < width; ++x) for (int channel = 0; channel < 3; ++channel) {
            const auto a = actual.buffer()->constData()[size_t(y) * actual.buffer()->stride() + x * 4 + channel];
            const auto b = expected.buffer()->constData()[size_t(y) * expected.buffer()->stride() + x * 4 + channel];
            if (a != b) { ++changed; maximum = std::max(maximum, std::abs(int(a) - int(b))); }
        }
        std::printf("FRAME index=%zu width=%d height=%d changed_rgb_bytes=%zu maximum_difference=%d\n", index, width, height, changed, maximum);
        passed &= changed == 0;
    }
    decompressor.Close(); R3DDecoder::ReleaseDecoder(reference); R3DDecoder::ReleaseDecodeJob(native);
    std::printf("RESULT passed=%d frames=%zu subbuffers=%zu rejected_subbuffers=%zu\n", passed && !rejectedSubBuffers, frames, subBuffers, rejectedSubBuffers);
    return passed && !rejectedSubBuffers ? 0 : 11;
}
