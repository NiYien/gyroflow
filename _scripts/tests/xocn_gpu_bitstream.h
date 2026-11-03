// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <string>

namespace {
bool verify_gpu_bitstreams(void* context, xocn::GpuDecode decode, const uint16_t* linear,
    const uint8_t* tone, uint64_t& successful) {
    struct Case {
        const char* name;
        std::string bits;
        int width, plane, rows, orientations, shift;
        bool integrate, empty, valid;
        int limit_bytes = -1;
        int status = 0;
    };
    const std::string simple = "101101001";
    auto append = [](std::string& bits, uint32_t value, int width) {
        for (int i = width - 1; i >= 0; --i) bits += ((value >> i) & 1) ? '1' : '0';
    };
    std::string crossing = "10" + std::string(18, '0') + "1";
    for (uint32_t value : {1, 2, 3, 4}) append(crossing, value, 19);
    crossing += "01010";
    for (uint32_t value : {5, 6, 7, 8}) append(crossing, value, 19);
    crossing += "0101";
    const Case cases[] = {
        {"unaligned", simple, 64, 0, 1, 1, 0, false, false, true},
        {"zero_reuse", simple, 64, 0, 1, 1, 0, false, true, true},
        {"cross32_64", crossing, 64, 4, 1, 1, 0, false, false, true},
        {"row_reset", simple + simple, 64, 0, 2, 1, 0, false, false, true},
        {"orientations", simple + simple + simple, 64, 0, 1, 3, 0, false, false, true},
        {"quantized", simple, 64, 0, 1, 1, 3, false, false, true},
        {"ll_zero_prefix", "1011000110", 128, 0, 1, 1, 0, true, false, true},
        // A complete valid row remains in the packet; only the record word limit rejects it.
        {"overread", crossing, 64, 4, 1, 1, 0, false, false, false, 1, 2},
        {"width20", "10" + std::string(19, '0') + "1", 64, 0, 1, 1, 0, false, false, false, -1, 1},
    };
    successful = 0;
    for (const auto& sample : cases) {
        constexpr uint32_t height = 32, payload_offset = 3;
        std::vector<uint8_t> payload(64);
        payload[0] = 0x5a; payload[1] = 0xa5; payload[2] = 0xff;
        for (size_t bit = 0; bit < sample.bits.size(); ++bit)
            if (sample.bits[bit] == '1') payload[payload_offset + bit / 8] |= uint8_t(1u << (7 - bit % 8));
        const uint32_t byte_length = sample.empty ? 0 : sample.limit_bytes >= 0
            ? uint32_t(sample.limit_bytes) : uint32_t((sample.bits.size() + 7) / 8);
        std::array<xocn::detail::SonyArw6Plane, 31> planes;
        std::array<xocn::GpuPlaneLayout, 31> layout{};
        for (int group = 0; group <= 4; ++group) {
            const auto band = xocn::band_layout(sample.width, height, group);
            for (int component = 0; component < (group == 4 ? 1 : 3); ++component) {
                const int base = group == 4 ? 30 : component * 10 + (group == 0 ? 0 : 1 + (group - 1) * 3);
                for (int orientation = 0; orientation < band.orientations; ++orientation)
                    planes[base + orientation] = {band.spans[orientation].end - band.spans[orientation].first, band.cols[orientation]};
            }
        }
        uint32_t offset = 0;
        for (size_t i = 0; i < planes.size(); ++i) {
            layout[i] = {offset, uint32_t(planes[i].rows), uint32_t(planes[i].cols), 0};
            offset += uint32_t(planes[i].data.size());
        }
        xocn::GpuRecord active{0, uint32_t(payload.size()), payload_offset,
            ((((byte_length + 7) / 8) + 1) & ~1u), uint32_t(sample.orientations), uint32_t(sample.integrate)};
        xocn::detail::SonyArw6NativeBits bits(payload.data(), uint32_t(payload.size()), payload_offset, byte_length);
        std::array<int, 31> written{};
        for (int orientation = 0; orientation < sample.orientations; ++orientation) {
            const int plane = sample.plane + orientation * 10;
            written[plane] = sample.rows;
            active.output[orientation] = {layout[plane].offset, layout[plane].cols, uint32_t(sample.rows), uint32_t(sample.shift)};
            if (byte_length) for (int row = 0; row < sample.rows; ++row)
                xocn::detail::sony_arw6_decode_component(bits, xocn::ceil_div(planes[plane].cols, 4), sample.shift, planes[plane].row(row), planes[plane].cols);
            if (sample.integrate && bits.status == 0)
                planes[plane] = xocn::detail::sony_arw6_integrate_type1(planes[plane], planes[plane].rows, 0);
        }
        if (bits.status != sample.status) {
            std::fprintf(stderr, "XOCN_GPU_BITSTREAM_FAIL case=%s vendor_status=%d\n", sample.name, bits.status);
            return false;
        }
        std::vector<xocn::GpuRecord> records;
        for (size_t i = 0; i < planes.size(); ++i) if (written[i] < planes[i].rows) {
            xocn::GpuRecord zero{0, uint32_t(payload.size()), 0, 0, 1, 0};
            zero.output[0] = {layout[i].offset + uint32_t(written[i]) * layout[i].cols,
                layout[i].cols, layout[i].rows - uint32_t(written[i]), 0};
            records.push_back(zero);
        }
        records.push_back(active);
        const xocn::GpuCompressed compressed{payload.data(), payload.size(), records.data(), uint32_t(records.size()), layout.data()};
        const xocn::GpuImage image{uint32_t(sample.width), height, 0, 0, uint32_t(sample.width), height,
            uint32_t(sample.width), height, uint32_t(sample.width * 2), 1};
        std::vector<uint16_t> actual(size_t(sample.width) * height, 0xa5a5);
        const bool completed = decode(context, &compressed, &image, linear, tone, reinterpret_cast<uint8_t*>(actual.data()));
        if (completed != sample.valid) {
            std::fprintf(stderr, "XOCN_GPU_BITSTREAM_FAIL case=%s vendor_status=%d gpu_completed=%d\n", sample.name, bits.status, int(completed));
            return false;
        }
        if (!completed) {
            if (!std::all_of(actual.begin(), actual.end(), [](uint16_t value) { return value == 0xa5a5; })) {
                std::fprintf(stderr, "XOCN_GPU_BITSTREAM_FAIL case=%s invalid_output_was_copied\n", sample.name);
                return false;
            }
            continue;
        }
        std::vector<xocn::detail::SonyArw6Plane> components;
        for (int component = 0; component < 3; ++component) {
            std::array<xocn::detail::SonyArw6Plane, 10> bands;
            std::copy_n(planes.begin() + component * 10, 10, bands.begin());
            components.push_back(xocn::reconstruct_component(std::move(bands), xocn::band_top(height)));
        }
        const auto expected = xocn::merge_components(sample.width, height, components, planes.back());
        if (actual != expected.pixels) {
            const auto mismatch = std::mismatch(actual.begin(), actual.end(), expected.pixels.begin());
            std::fprintf(stderr, "XOCN_GPU_BITSTREAM_FAIL case=%s pixel=%zu gpu=%u cpu=%u\n", sample.name,
                size_t(mismatch.first - actual.begin()), unsigned(*mismatch.first), unsigned(*mismatch.second));
            return false;
        }
        ++successful;
    }
    std::printf("XOCN_GPU_BITSTREAM_PASS cases=9 successes=%llu rejected=2 unaligned=1 cross32_64=1 row_reset=1 orientations=1 zero_reuse=1 invalid_output_unchanged=1\n",
        static_cast<unsigned long long>(successful));
    return true;
}
}
