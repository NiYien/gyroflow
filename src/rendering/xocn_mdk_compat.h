// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <type_traits>

namespace xocn_mdk_compat {
#if defined(__BYTE_ORDER__) && __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
constexpr bool native_big_endian = true;
#else
constexpr bool native_big_endian = false;
#endif
template<class Enum> constexpr auto underlying(Enum value) noexcept {
    return static_cast<typename std::underlying_type<Enum>::type>(value);
}
constexpr unsigned bit_ceil(unsigned value) noexcept {
    unsigned result = 1;
    while (result < value) result <<= 1;
    return result;
}
}
