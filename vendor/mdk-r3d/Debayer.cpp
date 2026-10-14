/*
 * Copyright (c) 2023 WangBin <wbsecg1 at gmail.com>
 * r3d plugin for libmdk
 */
#include "Debayer.h"
#include "R3DSDK.h"

MDK_NS_BEGIN

#if  (__APPLE__ + 0)
GpuDebayer::Ptr CreateMTLDebayer();
#endif
GpuDebayer::Ptr GpuDebayer::create(int type)
{
#if  (__APPLE__ + 0)
    if (type & OPTION_RED_METAL) {
        return CreateMTLDebayer();
    }
#endif // (__APPLE__ + 0)
    return nullptr;
}

MDK_NS_END