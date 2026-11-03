// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>
#include <vector>

namespace xocn {
struct RawFrame {
    int width;
    int height;
    std::vector<uint16_t> pixels;
};
struct GpuPlane {
    const int32_t* data;
    uint32_t rows, cols;
};
struct GpuImage {
    uint32_t raw_width, raw_height, crop_x, crop_y, width, height;
    uint32_t output_width, output_height, stride, output_mode;
};
// Destination offsets count i32 samples; packet offsets count bytes.
struct GpuPlaneLayout {
    uint32_t offset, rows, cols, reserved;
};
struct GpuRecordPlane {
    uint32_t offset, cols, rows, selector;
};
struct GpuRecord {
    uint32_t packet_offset, packet_length, payload_offset, words;
    uint32_t orientations, integrate_ll, reserved[2];
    GpuRecordPlane output[3];
    uint32_t padding[4];
};
struct GpuCompressed {
    const uint8_t* bytes;
    size_t length;
    const GpuRecord* records;
    uint32_t record_count;
    const GpuPlaneLayout* planes;
    void (*before_wait)(void*) = nullptr;
    void* before_wait_user = nullptr;
};
static_assert(sizeof(GpuRecord) == 96);
static_assert(sizeof(GpuPlaneLayout) == 16);
using GpuCreate = void* (*)();
using GpuDestroy = void (*)(void*);
using GpuReconstruct = bool (*)(void*, const GpuPlane*, const GpuImage*, const uint16_t*, const uint8_t*, uint8_t*);
using GpuDecode = bool (*)(void*, const GpuCompressed*, const GpuImage*, const uint16_t*, const uint8_t*, uint8_t*);
RawFrame decode(const uint8_t* payload, size_t length);
bool decode_gpu(const uint8_t* payload, size_t length, void* context, GpuReconstruct reconstruct,
    const GpuImage& image, const uint8_t* tone, uint8_t* output);
bool decode_gpu_compressed(const uint8_t* payload, size_t length, void* context, GpuDecode decode,
    const GpuImage& image, const uint8_t* tone, uint8_t* output,
    void (*before_wait)(void*) = nullptr, void* before_wait_user = nullptr);
}
extern "C" void niyien_xocn_register_gpu(xocn::GpuCreate create, xocn::GpuDestroy destroy, xocn::GpuReconstruct reconstruct,
    xocn::GpuDecode decode = nullptr);
