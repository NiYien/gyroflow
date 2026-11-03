// SPDX-License-Identifier: GPL-3.0-or-later
#include "xocn_codec.h"
#include "../../vendor/xocn/libraw_llvc3.h"
#include <array>
#include <stdexcept>

namespace xocn {
namespace {
using namespace detail;
#include "xocn_linear_curve.h"

int ceil_div(int value, int divisor) { return (value + divisor - 1) / divisor; }
struct Span { int first, end, chunk; };
int band_top(int rows) { return 4 + (8 - (rows / 4) % 8) % 8; }
struct BandLayout {
    int step, orientations;
    std::array<Span, 3> spans{};
    std::array<int, 3> cols{};
    struct Rows { int first, count; };
    Rows rows(int orientation, size_t record) const {
        const auto span = spans[orientation];
        const int begin = span.chunk + int(record) * (8 / step);
        const int first = std::clamp(begin, span.first, span.end);
        const int end = std::clamp(begin + 8 / step, span.first, span.end);
        return {first - span.first, end - first};
    }
};
BandLayout band_layout(int cols, int rows, int group) {
    const int step = group == 0 ? 8 : group == 4 ? 1 : 1 << (4 - group);
    const int top = band_top(rows), bottom = top + rows / 2;
    const int orientations = group == 0 || group == 4 ? 1 : 3;
    const int previous_cols = ceil_div(cols / 2, group == 0 ? 8 : group == 4 ? 1 : step / 2);
    BandLayout result{step, orientations};
    for (int i = 0; i < orientations; ++i) {
        const int phase = i == 0 ? 0 : step / 2;
        result.spans[i] = {ceil_div(top - phase, step), ceil_div(bottom - phase, step), ceil_div(4 - phase, step)};
        result.cols[i] = orientations == 1 ? previous_cols : i == 1 ? ceil_div(previous_cols, 2) : previous_cols / 2;
    }
    return result;
}
struct Picture {
    int cols, rows, top;
    std::vector<SonyArw6DirectoryEntry> directory;
};

Picture read_picture(const uint8_t* bytes, size_t length) {
    if (length < 128 || length > UINT32_MAX || std::memcmp(bytes, "SBRh", 4) || bytes[4] != 0x10
        || (sony_arw6_be16(bytes + 12) >> 4 & 0x3f) != 16 || sony_arw6_be16(bytes + 14) != 0x6e00)
        throw std::runtime_error("Unsupported X-OCN H picture header");
    const int cols = sony_arw6_be16(bytes + 8), rows = sony_arw6_be16(bytes + 10) * 2;
    if (cols < 32 || rows < 32 || (cols & 1) || (rows & 3) || uint64_t(cols) * rows > 32 * 1024 * 1024)
        throw std::runtime_error("Unsupported X-OCN raster");
    auto directory = sony_arw6_parse_directory(bytes, uint32_t(length));
    if (directory.size() != 13) throw std::runtime_error("Unsupported X-OCN band directory");
    return {cols, rows, band_top(rows), std::move(directory)};
}

std::vector<SonyArw6Plane> decode_bands(const uint8_t* stream, uint32_t size,
    const std::vector<SonyArw6DirectoryEntry>& directory, int cols, int rows, int group, int component) {
    const auto entry = sony_arw6_find_entry(directory, group, component);
    const auto packet = sony_arw6_parse_packet(stream + entry.start, entry.length);
    const auto layout = band_layout(cols, rows, group);
    sony_arw6_require(packet.type == layout.orientations);
    std::vector<SonyArw6Plane> output;
    for (int i = 0; i < layout.orientations; ++i)
        output.emplace_back(layout.spans[i].end - layout.spans[i].first, layout.cols[i]);
    for (size_t i = 0; i < packet.records.size(); ++i) {
        const auto& record = packet.records[i];
        SonyArw6NativeBits bits(packet.data, packet.length, record.payload_offset, record.byte_length);
        for (int orientation = 0; orientation < layout.orientations; ++orientation) {
            const auto rows = layout.rows(orientation, i);
            // Border chunks contain only the lattice rows inside the image.
            // Keep their orientation so each row uses its own quantizer.
            for (int row = 0; row < rows.count; ++row) {
                if (record.byte_length)
                    sony_arw6_decode_component(bits, ceil_div(output[orientation].cols, 4), record.selectors[orientation],
                        output[orientation].row(rows.first + row), output[orientation].cols);
            }
        }
        sony_arw6_require(bits.status == 0);
    }
    return output;
}

std::array<SonyArw6Plane, 10> decode_component_bands(const uint8_t* bytes, uint32_t length,
    const Picture& picture, int component) {
    std::array<SonyArw6Plane, 10> output;
    auto bands = decode_bands(bytes, length, picture.directory, picture.cols, picture.rows, 0, component);
    output[0] = sony_arw6_integrate_type1(bands[0], bands[0].rows, 0);
    for (int group = 1; group <= 3; ++group) {
        bands = decode_bands(bytes, length, picture.directory, picture.cols, picture.rows, group, component);
        for (int orientation = 0; orientation < 3; ++orientation)
            output[1 + (group - 1) * 3 + orientation] = std::move(bands[orientation]);
    }
    return output;
}

SonyArw6Plane inverse53(const SonyArw6Plane& low, const SonyArw6Plane& high, bool vertical, bool high_first = false) {
    const int low_count = vertical ? low.rows : low.cols;
    const int high_count = vertical ? high.rows : high.cols;
    const int lines = vertical ? low.cols : low.rows;
    sony_arw6_require(low_count > 0 && high_count > 0 && std::abs(low_count - high_count) <= 1);
    SonyArw6Plane output(vertical ? low_count + high_count : lines, vertical ? lines : low_count + high_count);
    if (vertical) {
        // Traverse contiguous rows, retaining the updated low samples in their output rows.
        for (int i = 0; i < low_count; ++i) {
            const int a = std::clamp(i + (high_first ? 0 : -1), 0, high_count - 1);
            const int b = std::clamp(i + (high_first ? 1 : 0), 0, high_count - 1);
            const auto* low_row = low.row(i);
            const auto* high_a = high.row(a);
            const auto* high_b = high.row(b);
            auto* target = output.row(i * 2 + int(high_first));
            for (int x = 0; x < lines; ++x)
                target[x] = low_row[x] - sony_arw6_floor_shift(int64_t(high_a[x]) + high_b[x] + 2, 2);
        }
        for (int i = 0; i < high_count; ++i) {
            const int a = std::clamp(i + (high_first ? -1 : 0), 0, low_count - 1);
            const int b = std::clamp(i + (high_first ? 0 : 1), 0, low_count - 1);
            const auto* high_row = high.row(i);
            const auto* low_a = output.row(a * 2 + int(high_first));
            const auto* low_b = output.row(b * 2 + int(high_first));
            auto* target = output.row(i * 2 + int(!high_first));
            for (int x = 0; x < lines; ++x)
                target[x] = high_row[x] + sony_arw6_floor_shift(int64_t(low_a[x]) + low_b[x], 1);
        }
        return output;
    }
    std::vector<int32_t> updated(low_count);
    for (int line = 0; line < lines; ++line) {
        for (int i = 0; i < low_count; ++i) {
            const int a = std::clamp(i + (high_first ? 0 : -1), 0, high_count - 1);
            const int b = std::clamp(i + (high_first ? 1 : 0), 0, high_count - 1);
            updated[i] = low.at(line, i) - sony_arw6_floor_shift(int64_t(high.at(line, a)) + high.at(line, b) + 2, 2);
            output.at(line, i * 2) = updated[i];
        }
        for (int i = 0; i < high_count; ++i) {
            const int a = std::clamp(i + (high_first ? -1 : 0), 0, low_count - 1);
            const int b = std::clamp(i + (high_first ? 0 : 1), 0, low_count - 1);
            const int value = high.at(line, i) + sony_arw6_floor_shift(int64_t(updated[a]) + updated[b], 1);
            output.at(line, i * 2 + 1) = value;
        }
    }
    return output;
}

SonyArw6Plane reconstruct_component(std::array<SonyArw6Plane, 10> bands, int top) {
    auto plane = std::move(bands[0]);
    for (int group = 1; group <= 3; ++group) {
        const int first = 1 + (group - 1) * 3;
        const int step = 1 << (4 - group);
        const bool high_first = ceil_div(top, step / 2) & 1;
        auto low_horizontal = inverse53(plane, bands[first + 1], true, high_first);
        auto high_horizontal = inverse53(bands[first], bands[first + 2], true, high_first);
        plane = inverse53(low_horizontal, high_horizontal, false);
    }
    return plane;
}

RawFrame merge_components(int cols, int rows, const std::vector<SonyArw6Plane>& components, const SonyArw6Plane& detail) {
    const auto green = sony_arw6_final_green(components[0], detail, 8);
    sony_arw6_require(green.rows == rows / 2 && green.cols == cols);
    RawFrame result{cols, rows, std::vector<uint16_t>(size_t(rows) * cols)};
    auto linear = [](int value) { return xocn_linear_curve[std::clamp(value + 2048, 0, 4095)]; };
    for (int y = 0; y < rows / 2; ++y) for (int x = 0; x < cols / 2; ++x) {
        const int a = sony_arw6_clamp_signed_code(green.at(y, 2 * x));
        const int b = sony_arw6_clamp_signed_code(green.at(y, 2 * x + 1));
        const int mean = sony_arw6_floor_shift(int64_t(a) + b, 1);
        result.pixels[size_t(2 * y) * cols + 2 * x] = linear(mean + 2 * components[1].at(y, x));
        result.pixels[size_t(2 * y) * cols + 2 * x + 1] = linear(green.at(y, 2 * x + 1));
        result.pixels[size_t(2 * y + 1) * cols + 2 * x] = linear(green.at(y, 2 * x));
        result.pixels[size_t(2 * y + 1) * cols + 2 * x + 1] = linear(mean + 2 * components[2].at(y, x));
    }
    return result;
}
}

RawFrame decode(const uint8_t* bytes, size_t length) {
    try {
        const auto picture = read_picture(bytes, length);
        const auto cols = picture.cols, rows = picture.rows;
        std::vector<SonyArw6Plane> components;
        for (int component = 0; component < 3; ++component) {
            auto bands = decode_component_bands(bytes, uint32_t(length), picture, component);
            components.push_back(reconstruct_component(std::move(bands), picture.top));
        }
        const auto detail_band = decode_bands(bytes, uint32_t(length), picture.directory, cols, rows, 4, 0);
        return merge_components(cols, rows, components, detail_band[0]);
    } catch (int) {
        throw std::runtime_error("Invalid X-OCN coefficient stream");
    }
}

bool decode_gpu(const uint8_t* bytes, size_t length, void* context, GpuReconstruct reconstruct,
    const GpuImage& image, const uint8_t* tone, uint8_t* output) {
    try {
        const auto picture = read_picture(bytes, length);
        if (picture.cols != int(image.raw_width) || picture.rows != int(image.raw_height))
            throw std::runtime_error("X-OCN raster changed");
        std::array<SonyArw6Plane, 31> planes;
        for (int component = 0; component < 3; ++component) {
            auto bands = decode_component_bands(bytes, uint32_t(length), picture, component);
            for (int i = 0; i < 10; ++i) planes[component * 10 + i] = std::move(bands[i]);
        }
        auto detail = decode_bands(bytes, uint32_t(length), picture.directory, picture.cols, picture.rows, 4, 0);
        planes.back() = std::move(detail[0]);
        std::array<GpuPlane, 31> views;
        for (size_t i = 0; i < planes.size(); ++i)
            views[i] = {planes[i].data.data(), uint32_t(planes[i].rows), uint32_t(planes[i].cols)};
        return reconstruct(context, views.data(), &image, xocn_linear_curve, tone, output);
    } catch (int) {
        throw std::runtime_error("Invalid X-OCN coefficient stream");
    }
}

bool decode_gpu_compressed(const uint8_t* bytes, size_t length, void* context, GpuDecode decode,
    const GpuImage& image, const uint8_t* tone, uint8_t* output, void (*before_wait)(void*), void* before_wait_user) {
    try {
        const auto picture = read_picture(bytes, length);
        if (picture.cols != int(image.raw_width) || picture.rows != int(image.raw_height))
            throw std::runtime_error("X-OCN raster changed");
        std::array<BandLayout, 5> bands;
        for (int group = 0; group <= 4; ++group) bands[group] = band_layout(picture.cols, picture.rows, group);
        std::array<GpuPlaneLayout, 31> planes{};
        for (int group = 0; group <= 4; ++group) for (int component = 0; component < (group == 4 ? 1 : 3); ++component) {
            const int base = group == 4 ? 30 : component * 10 + (group == 0 ? 0 : 1 + (group - 1) * 3);
            for (int i = 0; i < bands[group].orientations; ++i) {
                const auto span = bands[group].spans[i];
                planes[base + i] = {0, uint32_t(span.end - span.first), uint32_t(bands[group].cols[i]), 0};
            }
        }
        uint32_t offset = 0;
        for (auto& plane : planes) {
            plane.offset = offset;
            offset += plane.rows * plane.cols;
        }
        std::vector<GpuRecord> records;
        records.reserve(13 * (ceil_div(picture.rows, 16) + 1));
        for (int group = 0; group <= 4; ++group) for (int component = 0; component < (group == 4 ? 1 : 3); ++component) {
            const auto entry = sony_arw6_find_entry(picture.directory, group, component);
            const auto packet = sony_arw6_parse_packet(bytes + entry.start, entry.length);
            const auto& layout = bands[group];
            sony_arw6_require(packet.type == layout.orientations);
            const int base = group == 4 ? 30 : component * 10 + (group == 0 ? 0 : 1 + (group - 1) * 3);
            for (size_t i = 0; i < packet.records.size(); ++i) {
                const auto& source = packet.records[i];
                GpuRecord record{entry.start, packet.length, source.payload_offset,
                    ((((source.byte_length + 7) / 8) + 1) & ~1u), uint32_t(layout.orientations), uint32_t(group == 0)};
                for (int orientation = 0; orientation < layout.orientations; ++orientation) {
                    const auto rows = layout.rows(orientation, i);
                    const auto& plane = planes[base + orientation];
                    record.output[orientation] = {plane.offset + uint32_t(rows.first) * plane.cols,
                        plane.cols, uint32_t(rows.count), source.selectors[orientation]};
                }
                records.push_back(record);
            }
        }
        const GpuCompressed compressed{bytes, length, records.data(), uint32_t(records.size()), planes.data(), before_wait, before_wait_user};
        return decode(context, &compressed, &image, xocn_linear_curve, tone, output);
    } catch (int) {
        throw std::runtime_error("Invalid X-OCN coefficient stream");
    }
}
}
