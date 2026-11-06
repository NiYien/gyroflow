// SPDX-License-Identifier: GPL-3.0-or-later
#include "../../vendor/qml-video-rs/src/cpp/HevcParameterSetWorkaround.h"
#include <cassert>
#include <cstdio>
#include <vector>

using Bytes = std::vector<uint8_t>;

static Bytes configuration(unsigned ppsCount = 2, bool multilayer = false, bool auxiliaryLayer = false) {
    Bytes data(23);
    data[0] = 1;
    data[1] = 2;
    data[22] = 3;
    for (unsigned type = 32; type <= 34; ++type) {
        const unsigned count = type == 34 ? ppsCount : 1;
        data.insert(data.end(), {uint8_t(128 | type), 0, uint8_t(count)});
        for (unsigned i = 0; i < count; ++i) {
            Bytes nal = type == 32 ? Bytes{64, 1, 12, uint8_t(multilayer ? 17 : 1), 255, 255}
                                  : Bytes{uint8_t(type << 1), uint8_t(auxiliaryLayer && type == 34 ? 9 : 1), 128};
            data.insert(data.end(), {0, uint8_t(nal.size())});
            data.insert(data.end(), nal.begin(), nal.end());
        }
    }
    return data;
}

static bool eligible(const Bytes &data, const char *codec = "hevc", bool enhanced = false, size_t tracks = 1) {
    return niyien::needsHevcParameterSetWorkaround(codec, data.data(), data.size(), enhanced, tracks);
}

int main() {
    const auto ordinary = configuration();
    assert(eligible(ordinary));
    assert(eligible(configuration(3)));
    auto main8 = ordinary;
    main8[1] = 1;
    assert(eligible(main8));
    assert(!eligible(configuration(1)));
    assert(!eligible(configuration(2, true)));
    assert(!eligible(configuration(2, false, true)));
    assert(!eligible(ordinary, "hevc", true));
    assert(!eligible(ordinary, "hevc", false, 2));
    for (const auto codec : {"h264", "prores", "mjpeg", "rawvideo", "r3d", "", static_cast<const char *>(nullptr)}) {
        assert(!eligible(ordinary, codec));
    }
    for (size_t length = 0; length < ordinary.size(); ++length) {
        assert(!niyien::needsHevcParameterSetWorkaround("hevc", ordinary.data(), length, false, 1));
    }
    assert(!niyien::needsHevcParameterSetWorkaround("hevc", nullptr, ordinary.size(), false, 1));
    auto invalid = ordinary;
    invalid.push_back(0);
    assert(!eligible(invalid));
    invalid = ordinary;
    invalid[26] = 255;
    invalid[27] = 255;
    assert(!eligible(invalid));
    invalid = ordinary;
    invalid[28] |= 128;
    assert(!eligible(invalid));
    invalid = ordinary;
    invalid[29] = 0;
    assert(!eligible(invalid));
    invalid = ordinary;
    invalid[23] = 128 | 39;
    assert(!eligible(invalid));
    invalid = ordinary;
    invalid[1] = 4;
    assert(!eligible(invalid));
    invalid = ordinary;
    invalid[25] = 0;
    assert(!eligible(invalid));
    std::puts("PASS: base-layer multi-PPS HEVC; enhancement, auxiliary, multiview, other codecs and malformed records excluded");
}
