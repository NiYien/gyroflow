/*
 * Copyright (c) 2023 WangBin <wbsecg1 at gmail.com>
 * r3d plugin for libmdk
 */
#pragma once

#include "R3DSDK.h"
#include "R3DSDKDecoder.h"

// MUST free() the returned string
char* MetadataItemKey(const R3DSDK::Clip* clip, size_t index);
char* MetadataItemAsString(const R3DSDK::Clip* clip, size_t index);

R3DSDK::R3DStatus SetupCudaCLDevices(R3DSDK::R3DDecoderOptions* opts, int type/*1: cuda, 2: opencl*/);