/*
 * Copyright (c) 2023 WangBin <wbsecg1 at gmail.com>
 * r3d plugin for libmdk
 */
#include "Debayer.h"
#include "R3DSDKMetal.h"
#include "mdk/VideoBuffer.h"
#include "mdk/VideoFormat.h"
#include "mdk/VideoFrame.h"
#include "mdk/global.h"
#include <memory>
#include <iostream>

using namespace std;
MDK_NS_BEGIN

static R3DSDK::EXT_METAL_API gApi{}; // TODO: pool

class MetalDebayer final : public GpuDebayer, protected R3DSDK::REDMetal
{
public:
    MetalDebayer() : R3DSDK::REDMetal(gApi) {
        dev_ = MTLCreateSystemDefaultDevice();
        cmdQ_ = [dev_ newCommandQueue];
        int err = 0;
        if (auto st = checkCompatibility(cmdQ_, err); st != R3DSDK::REDMetal::Status_Ok) {
            clog << "checkCompatibility failed: " << st << " error: " << err << endl;
        }
    }

    void* createJob(const void* hostMemInput, size_t hostMemSize, int width, int height, R3DSDK::VideoDecodeMode mode, R3DSDK::VideoPixelType pix, R3DSDK::ImageProcessingSettings* ips) override;
    void releaseJob(void* job) override {
        auto mtljob = (R3DSDK::DebayerMetalJob*)job;
        mtljob->raw_device_mem = nil;
        mtljob->output_device_image = nil;
        releaseDebayerJob(mtljob);
    }

    GpuDebayer::Status submit(void* job) override {
        int err;
        return (GpuDebayer::Status)processAsync(cmdQ_, (R3DSDK::DebayerMetalJob*)job, err);
    }

    VideoFrame wait(void* job, bool copy) override;

    GpuDebayer::Status flush() override { return GpuDebayer::Status_Ok; }

private:
    id<MTLDevice> dev_;
    id<MTLCommandQueue> cmdQ_;
};

void* MetalDebayer::createJob(const void *hostMemInput, size_t hostMemSize, int width, int height, R3DSDK::VideoDecodeMode mode, R3DSDK::VideoPixelType pix, R3DSDK::ImageProcessingSettings *ips)
{
    auto job = createDebayerJob();
    job->raw_host_mem = (void*)hostMemInput;
    job->mode = mode;
    job->pixelType = pix;
    job->imageProcessingSettings = ips;
    job->batchMode = false;
    // TODO: reuse mtl buffer, texture
    auto buf = [dev_ newBufferWithBytes:hostMemInput length:hostMemSize options:MTLResourceStorageModeManaged];
    //[buf didModifyRange:NSMakeRange(0, hostMemSize)]; // if managed storage
    job->raw_device_mem = buf;
    // seems BGRA does not work
    auto fmt = MTLPixelFormatBGRA8Unorm;
    switch (pix) {
    case R3DSDK::PixelType_16Bit_RGB_Interleaved:
        fmt = MTLPixelFormatRGBA16Uint;
        break;
    case R3DSDK::PixelType_HalfFloat_RGB_Interleaved:
    case R3DSDK::PixelType_HalfFloat_RGB_ACES_Int:
        fmt = MTLPixelFormatRGBA16Float;
        break;
    default:
        break;
    }
    auto desc = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:fmt width:width height:height mipmapped:false];
    desc.usage = MTLTextureUsageShaderWrite | MTLTextureUsageShaderRead;
    job->output_device_image = [dev_ newTextureWithDescriptor:desc];
    job->output_device_mem_size = R3DSDK::DebayerMetalJob::ResultFrameSize(*job);
    return job;
}

PixelFormat format(MTLPixelFormat mfmt)
{
    switch (mfmt) {
    case MTLPixelFormatBGRA8Unorm: return PixelFormat::BGRA;
    case MTLPixelFormatRGBA8Unorm: return PixelFormat::RGBA;
    case MTLPixelFormatRGBA16Uint: return PixelFormat::RGBA64;
    case MTLPixelFormatRGBA16Float: return PixelFormat::RGBAF16LE;
    default: return PixelFormat::Unknown;
    }
}

VideoFrame MetalDebayer::wait(void* job, bool copy)
{
    auto mtljob = (R3DSDK::DebayerMetalJob*)job;
    mtljob->completeAsync();
    class MetalVideoBuffer final : public NativeVideoBuffer {
        id<MTLTexture> tex_;
        MetalTextures mta_{};
        MemoryArray ma_{};
        VideoFrame frame_;
    public:
        MetalVideoBuffer(id<MTLTexture> tex) : tex_(tex) {}
        void* map(Type type, MapParameter* mp) override {
            mp->width[0] = (int)tex_.width;
            mp->height[0] = (int)tex_.height;
            mp->format = format(tex_.pixelFormat);
            if (type == HostMemory) {
                if (!ma_.data[0]) {
                    frame_ = VideoFrame(mp->width[0], mp->height[0], mp->format);
                    frame_.setBuffers(nullptr);
                    ma_.data[0] = frame_.buffer(0)->data();
                    mp->stride[0] = (int)frame_.buffer(0)->stride();
                    [tex_ getBytes:ma_.data[0] bytesPerRow:mp->stride[0] fromRegion:MTLRegionMake2D(0, 0, mp->width[0], mp->height[0]) mipmapLevel:0];
                }
                return &ma_;
            }
            if (type != MetalTexture)
                return nullptr;
            if (!mta_.tex[0]) {
                mta_.tex[0] = (__bridge void*)tex_;
            }
            return &mta_;
        }
    };

    auto tex = mtljob->output_device_image;
    VideoFrame frame((int)tex.width, (int)tex.height, format(tex.pixelFormat));
    if (copy) {
        frame.setBuffers(nullptr);
        [tex getBytes:frame.buffer(0)->data() bytesPerRow:frame.buffer()->stride() fromRegion:MTLRegionMake2D(0, 0, frame.width(), frame.height()) mipmapLevel:0];
    } else {
        frame.setNativeBuffer(make_shared<MetalVideoBuffer>(tex));
    }
    return frame;
}

GpuDebayer::Ptr CreateMTLDebayer() {
    return make_unique<MetalDebayer>();
}
MDK_NS_END
