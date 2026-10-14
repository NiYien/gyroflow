/*
 * Copyright (c) 2023 WangBin <wbsecg1 at gmail.com>
 * r3d plugin for libmdk
 */
#pragma once
#include "mdk/VideoFrame.h"
#include "R3DSDKDefinitions.h"

MDK_NS_BEGIN

class GpuDebayer
{
public:
    enum Status {
        Status_Ok = 0,
        Status_ErrorProcessing = 1,
        Status_InvalidJobParameter = 2,

        //mode value passed in is not compatible with this SDK or the mode used with the DecodeForGpuSdk call is not compatible
        Status_InvalidJobParameter_mode = 3,//mode is not compatible with this SDK or the mode used with the DecodeForGpuSdk call

        //pointer is NULL, data is not from DecodeForGpuSdk, R3DSDK and GPUSDK versions are incompatible or buffer is not actually in host memory.
        Status_InvalidJobParameter_raw_host_mem = 4,
        Status_InvalidJobParameter_raw_device_mem = 5,

        //unsupported pixel type
        Status_InvalidJobParameter_pixelType = 6,

        //Output buffer Size must be non zero.
        //Output buffer must be allocated prior to passing it into the sdk
        Status_InvalidJobParameter_output_device_mem_size = 7,
        Status_InvalidJobParameter_output_device_mem = 8,

        //Image processing settings ColorVersion was set to ColorVersion1 which is not supported by this SDK
        Status_InvalidJobParameter_ColorVersion1 = 9,

        //GPU Device did not meet minimum requirements.
        Status_UnableToUseGPUDevice = 10,

        //Error loading R3DSDK dynamic library
        Status_UnableToLoadLibrary = 11,

        Status_ParameterUnsupported = 12,

		Status_InvalidAPIObject = 13
    };

    using Ptr = std::unique_ptr<GpuDebayer>;

    static Ptr create(int type /*OPTION_RED_xxx*/);

    virtual ~GpuDebayer() = default;

    virtual void* createJob(const void* hostMemInput, size_t hostMemSize, int width, int height, R3DSDK::VideoDecodeMode mode, R3DSDK::VideoPixelType pix, R3DSDK::ImageProcessingSettings* ips) = 0;
    virtual void releaseJob(void* job) = 0;

    virtual Status submit(void* job) = 0; // processAsync

    virtual mdk::VideoFrame wait(void* job, bool copy = false) = 0;

    virtual Status flush() { return Status_Ok; }
};

MDK_NS_END