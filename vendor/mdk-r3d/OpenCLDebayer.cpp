// SPDX-License-Identifier: GPL-3.0-or-later
#define NOMINMAX
#define CL_TARGET_OPENCL_VERSION 120
#include <windows.h>
#include "Debayer.h"
#include "R3DSDK.h"
#include "R3DSDKDecoder.h"
#include "R3DSDKOpenCL.h"
#include "mdk/VideoBuffer.h"
#include <algorithm>
#include <iostream>
#include <mutex>
#include <vector>

MDK_NS_BEGIN
namespace {
struct OpenCLApi {
    R3DSDK::EXT_OCLAPI_1_1 sdk;

    bool load() {
        // Keep the dispatch module alive for SDK callbacks and cached kernels.
        static auto library = LoadLibraryExW(L"OpenCL.dll", nullptr, LOAD_LIBRARY_SEARCH_SYSTEM32);
        if (!library) return false;
#define BIND(name) sdk.name = reinterpret_cast<decltype(sdk.name)>(GetProcAddress(library, #name)); if (!sdk.name) return false
        BIND(clSetKernelArg); BIND(clFlush); BIND(clFinish); BIND(clEnqueueCopyImage);
        BIND(clCreateContext); BIND(clCreateCommandQueue); BIND(clCreateSampler); BIND(clCreateKernel); BIND(clCreateBuffer);
        BIND(clCreateProgramWithSource); BIND(clCreateProgramWithBinary); BIND(clReleaseEvent); BIND(clReleaseSampler);
        BIND(clReleaseKernel); BIND(clReleaseMemObject); BIND(clReleaseProgram); BIND(clReleaseContext); BIND(clReleaseCommandQueue);
        BIND(clGetPlatformInfo); BIND(clGetDeviceIDs); BIND(clGetPlatformIDs); BIND(clGetDeviceInfo); BIND(clGetContextInfo);
        BIND(clGetImageInfo); BIND(clGetProgramBuildInfo); BIND(clGetProgramInfo); BIND(clGetKernelWorkGroupInfo); BIND(clBuildProgram);
        BIND(clEnqueueWriteBuffer); BIND(clEnqueueReadBuffer); BIND(clEnqueueCopyBuffer); BIND(clEnqueueCopyBufferToImage);
        BIND(clEnqueueWriteImage); BIND(clEnqueueNDRangeKernel); BIND(clEnqueueMapBuffer); BIND(clEnqueueUnmapMemObject);
        BIND(clWaitForEvents); BIND(clEnqueueBarrier); BIND(clEnqueueMarker); BIND(clCreateImage2D); BIND(clSetMemObjectDestructorCallback);
        BIND(clCreateSubBuffer); BIND(clGetMemObjectInfo); BIND(clCreateImage3D);
#undef BIND
        return true;
    }
};

class OpenCLDebayer final : public GpuDebayer {
    struct Job {
        const void* input;
        size_t inputSize;
        int width, height;
        R3DSDK::VideoDecodeMode mode;
        R3DSDK::VideoPixelType pixelType;
        R3DSDK::ImageProcessingSettings* settings;
        VideoFrame frame;
    };
    OpenCLApi api_;
    cl_context context_ = nullptr;
    cl_command_queue queue_ = nullptr;
    cl_device_id device_ = nullptr;
    std::unique_ptr<R3DSDK::REDCL> red_;
    R3DSDK::DebayerOpenCLJob* sdkJob_ = nullptr;
    cl_mem input_ = nullptr, output_ = nullptr;
    cl_mem packed_ = nullptr;
    cl_program packProgram_ = nullptr;
    cl_kernel packKernel_ = nullptr;
    size_t packedSize_ = 0;
    size_t inputSize_ = 0, outputSize_ = 0;
    int outputWidth_ = 0, outputHeight_ = 0;
    R3DSDK::VideoPixelType outputType_{};
    std::mutex mutex_;

    Status error(const char* stage, int sdkStatus, cl_int clStatus) {
        std::cerr << "R3D OpenCL failed: stage=" << stage << " sdk=" << sdkStatus << " cl=" << clStatus << std::endl;
        return Status_ErrorProcessing;
    }

public:
    explicit OpenCLDebayer(OpenCLApi api) : api_(api) {}

    ~OpenCLDebayer() override {
        if (sdkJob_) red_->releaseDebayerJob(sdkJob_);
        if (packed_) api_.sdk.clReleaseMemObject(packed_);
        if (packKernel_) api_.sdk.clReleaseKernel(packKernel_);
        if (packProgram_) api_.sdk.clReleaseProgram(packProgram_);
        if (output_) api_.sdk.clReleaseMemObject(output_);
        if (input_) api_.sdk.clReleaseMemObject(input_);
        red_.reset();
        if (queue_) api_.sdk.clReleaseCommandQueue(queue_);
        if (context_) api_.sdk.clReleaseContext(context_);
    }

    bool init(const R3DSDK::OpenCLDeviceInfo& info) {
        const auto device = static_cast<cl_device_id>(info.device_id);
        device_ = device;
        cl_uint alignmentBits = 0;
        const auto queried = api_.sdk.clGetDeviceInfo(device, CL_DEVICE_MEM_BASE_ADDR_ALIGN, sizeof(alignmentBits), &alignmentBits, nullptr);
        // RED places the payload at a 4 KiB host boundary inside its raw block.
        if (queried != CL_SUCCESS || !alignmentBits || alignmentBits % 8 || 4096 % (alignmentBits / 8)) {
            error("input_alignment", 0, queried != CL_SUCCESS ? queried : CL_INVALID_VALUE);
            return false;
        }
        const cl_context_properties properties[] = {CL_CONTEXT_PLATFORM, reinterpret_cast<cl_context_properties>(info.platform_id), 0};
        cl_int status = CL_SUCCESS;
        context_ = api_.sdk.clCreateContext(properties, 1, &device, nullptr, nullptr, &status);
        if (!context_ || status != CL_SUCCESS) { error("context", 0, status); return false; }
        queue_ = api_.sdk.clCreateCommandQueue(context_, device, 0, &status);
        if (!queue_ || status != CL_SUCCESS) { error("queue", 0, status); return false; }
        red_ = std::make_unique<R3DSDK::REDCL>(api_.sdk, "");
        const auto compatible = red_->checkCompatibility(context_, queue_, status);
        if (compatible != R3DSDK::REDCL::Status_Ok) { error("compatibility", compatible, status); return false; }
        sdkJob_ = red_->createDebayerJob();
        if (!sdkJob_) { error("create_job", 0, 0); return false; }
        std::clog << "R3D OpenCL aligned input: device=" << info.name << " device_alignment=" << alignmentBits / 8 << " raw_alignment=4096" << std::endl;
        return true;
    }

    void* createJob(const void* input, size_t size, int width, int height, R3DSDK::VideoDecodeMode mode, R3DSDK::VideoPixelType type, R3DSDK::ImageProcessingSettings* settings) override {
        return new Job{input, size, width, height, mode, type, settings, {}};
    }

    void releaseJob(void* value) override { delete static_cast<Job*>(value); }

    Status submit(void* value) override {
        const std::lock_guard lock(mutex_);
        auto& job = *static_cast<Job*>(value);
        cl_int status = CL_SUCCESS;
        const bool byteOutput = job.pixelType == R3DSDK::PixelType_8Bit_BGRA_Interleaved || job.pixelType == R3DSDK::PixelType_8Bit_BGR_Interleaved;
        const bool planar = job.pixelType == R3DSDK::PixelType_16Bit_RGB_Planar;
        const auto sdkPixel = byteOutput || planar ? R3DSDK::PixelType_16Bit_RGB_Interleaved : job.pixelType;
        const auto frameFormat = sdkPixel == R3DSDK::PixelType_16Bit_RGB_Interleaved ? PixelFormat::RGB48 : PixelFormat::RGBF16LE;
        sdkJob_->mode = job.mode;
        sdkJob_->pixelType = sdkPixel;
        sdkJob_->imageProcessingSettings = job.settings;
        sdkJob_->raw_host_mem = const_cast<void*>(job.input);
        if (!input_ || inputSize_ != job.inputSize) {
            if (input_) api_.sdk.clReleaseMemObject(input_);
            input_ = api_.sdk.clCreateBuffer(context_, CL_MEM_READ_ONLY, job.inputSize, nullptr, &status);
            if (!input_ || status != CL_SUCCESS) return error("input_buffer", 0, status);
            inputSize_ = job.inputSize;
        }
        sdkJob_->raw_device_mem = input_;
        cl_event uploaded = nullptr;
        status = api_.sdk.clEnqueueWriteBuffer(queue_, input_, CL_FALSE, 0, job.inputSize, job.input, 0, nullptr, &uploaded);
        if (status != CL_SUCCESS) return error("upload", 0, status);
        // The caller may reuse the raw block when submit returns, including on errors.
        struct UploadComplete {
            R3DSDK::EXT_OCLAPI_1_1& api;
            cl_event event;
            ~UploadComplete() { api.clWaitForEvents(1, &event); api.clReleaseEvent(event); }
        } uploadComplete{api_.sdk, uploaded};
        const size_t bytes = R3DSDK::DebayerOpenCLJob::ResultFrameSize(*sdkJob_);
        if (!output_ || outputWidth_ != job.width || outputHeight_ != job.height || outputType_ != job.pixelType) {
            if (output_) api_.sdk.clReleaseMemObject(output_);
            output_ = api_.sdk.clCreateBuffer(context_, CL_MEM_READ_WRITE, bytes, nullptr, &status);
            if (!output_ || status != CL_SUCCESS) return error("output_buffer", 0, status);
            outputWidth_ = job.width; outputHeight_ = job.height; outputType_ = job.pixelType;
            outputSize_ = bytes;
        }
        sdkJob_->output_device_mem = output_;
        sdkJob_->output_device_mem_size = outputSize_;
        const auto processed = red_->processAsync(context_, queue_, sdkJob_, status);
        struct Complete {
            R3DSDK::DebayerOpenCLJob* job;
            ~Complete() { job->completeAsync(); }
        } complete{sdkJob_};
        if (processed != R3DSDK::REDCL::Status_Ok) return error("process", processed, status);
        const int channels = job.pixelType == R3DSDK::PixelType_8Bit_BGR_Interleaved ? 3 : 4;
        VideoFrame frame(job.width, job.height, byteOutput ? (channels == 4 ? PixelFormat::BGRA : PixelFormat::BGR24) : frameFormat);
        frame.setBuffers(nullptr);
        if (byteOutput) {
            // RED truncates normalized 16-bit values; generic converters round instead.
            if (!packKernel_) {
                const char* source = R"(
                    __kernel void pack_rgb16(__global const ushort* source, __global uchar* target,
                                             uint width, uint stride, uint channels) {
                        uint i = get_global_id(0);
                        ushort3 rgb = vload3(i, source);
                        uint row = i / width;
                        uint offset = row * stride + (i - row * width) * channels;
                        target[offset] = (uchar)(rgb.z / 257u);
                        target[offset + 1] = (uchar)(rgb.y / 257u);
                        target[offset + 2] = (uchar)(rgb.x / 257u);
                        if (channels == 4) target[offset + 3] = 255;
                    }
                )";
                auto program = api_.sdk.clCreateProgramWithSource(context_, 1, &source, nullptr, &status);
                if (!program || status != CL_SUCCESS) return error("pack_program", 0, status);
                status = api_.sdk.clBuildProgram(program, 1, &device_, nullptr, nullptr, nullptr);
                if (status != CL_SUCCESS) {
                    char buildLog[4096]{};
                    api_.sdk.clGetProgramBuildInfo(program, device_, CL_PROGRAM_BUILD_LOG, sizeof(buildLog), buildLog, nullptr);
                    api_.sdk.clReleaseProgram(program);
                    std::cerr << "R3D OpenCL pack build: " << buildLog << std::endl;
                    return error("pack_build", 0, status);
                }
                auto kernel = api_.sdk.clCreateKernel(program, "pack_rgb16", &status);
                if (!kernel || status != CL_SUCCESS) { api_.sdk.clReleaseProgram(program); return error("pack_kernel", 0, status); }
                packProgram_ = program;
                packKernel_ = kernel;
            }
            const size_t packedBytes = size_t(frame.buffer()->stride()) * job.height;
            if (!packed_ || packedSize_ != packedBytes) {
                if (packed_) api_.sdk.clReleaseMemObject(packed_);
                packed_ = api_.sdk.clCreateBuffer(context_, CL_MEM_WRITE_ONLY, packedBytes, nullptr, &status);
                if (!packed_ || status != CL_SUCCESS) return error("pack_buffer", 0, status);
                packedSize_ = packedBytes;
            }
            const cl_uint width = job.width, stride = frame.buffer()->stride(), channelCount = channels;
            const void* values[] = {&output_, &packed_, &width, &stride, &channelCount};
            const size_t sizes[] = {sizeof(output_), sizeof(packed_), sizeof(width), sizeof(stride), sizeof(channelCount)};
            for (cl_uint i = 0; i < 5; ++i) {
                status = api_.sdk.clSetKernelArg(packKernel_, i, sizes[i], values[i]);
                if (status != CL_SUCCESS) return error("pack_argument", 0, status);
            }
            const size_t pixels = size_t(job.width) * job.height;
            status = api_.sdk.clEnqueueNDRangeKernel(queue_, packKernel_, 1, nullptr, &pixels, nullptr, 0, nullptr, nullptr);
            if (status != CL_SUCCESS) return error("pack_enqueue", 0, status);
            status = api_.sdk.clEnqueueReadBuffer(queue_, packed_, CL_TRUE, 0, packedSize_, frame.buffer()->data(), 0, nullptr, nullptr);
        } else {
            status = api_.sdk.clEnqueueReadBuffer(queue_, output_, CL_TRUE, 0, outputSize_, frame.buffer()->data(), 0, nullptr, nullptr);
        }
        if (status != CL_SUCCESS) return error("readback", 0, status);
        if (planar) frame = frame.to(PixelFormat::RGBP16);
        if (!frame) return error("convert", 0, 0);
        job.frame = std::move(frame);
        return Status_Ok;
    }

    VideoFrame wait(void* value, bool) override { return static_cast<Job*>(value)->frame; }
};
}

GpuDebayer::Ptr CreateOpenCLDebayer() {
    OpenCLApi api;
    if (!api.load()) return nullptr;
    std::vector<R3DSDK::OpenCLDeviceInfo> devices;
    if (R3DSDK::R3DDecoderOptions::GetOpenCLDeviceList(devices) != R3DSDK::R3DStatus_Ok) return nullptr;
    for (const auto& device : devices) {
        auto debayer = std::make_unique<OpenCLDebayer>(api);
        if (debayer->init(device)) return debayer;
    }
    return nullptr;
}
MDK_NS_END
