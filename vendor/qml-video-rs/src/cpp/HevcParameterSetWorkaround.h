// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <cstddef>
#include <cstdint>
#include <cstring>

namespace niyien {
// MDK's enhancement-layer parser can discard a second base-layer PPS. Restrict
// the workaround to complete hvcC records that explicitly declare one layer.
inline bool needsHevcParameterSetWorkaround(const char *codec, const uint8_t *data,
    size_t size, bool enhancementSignalled, size_t videoTracks) {
    if (!codec || std::strcmp(codec, "hevc") || enhancementSignalled || videoTracks != 1
        || !data || size < 23 || data[0] != 1) return false;
    const auto profile = data[1] & 31;
    if (profile != 1 && profile != 2) return false;

    size_t offset = 23;
    unsigned vps = 0, sps = 0, pps = 0;
    for (unsigned array = 0; array < data[22]; ++array) {
        if (size - offset < 3) return false;
        const auto type = data[offset++] & 63;
        if (type < 32 || type > 34) return false;
        const unsigned count = unsigned(data[offset]) * 256 + data[offset + 1];
        offset += 2;
        if (!count) return false;
        for (unsigned n = 0; n < count; ++n) {
            if (size - offset < 2) return false;
            const size_t length = size_t(data[offset]) * 256 + data[offset + 1];
            offset += 2;
            if (length < 3 || length > size - offset) return false;
            const auto nal = data + offset;
            if ((nal[0] & 128) || ((nal[0] >> 1) & 63) != type
                || (nal[0] & 1) || (nal[1] >> 3) || !(nal[1] & 7)) return false;
            if (type == 32) {
                // VPS starts with id:4, base-layer flags:2, max_layers_minus1:6.
                // These first two payload bytes precede any emulation prevention.
                if (length < 6 || (nal[2] & 15) != 12 || (nal[3] & 240)
                    || nal[4] != 255 || nal[5] != 255) return false;
                ++vps;
            } else if (type == 33) {
                ++sps;
            } else {
                ++pps;
            }
            offset += length;
        }
    }
    return offset == size && vps == 1 && sps == 1 && pps > 1;
}
} // namespace niyien
