// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once
#include <array>
#include <cstdint>
#include <cstring>
#include <initializer_list>
#include <istream>
#include <optional>

namespace crm {
struct Area {
  int left, top, width, height;
};

// Read only the selected CRAW track's short IAD1. LibRaw 0.22.2 handles the
// long IAD1 but skips this layout, including the C50's explicit video crop.
class MovieCropReader {
  struct Box { uint64_t body, end; std::array<char, 4> type; };
  std::istream &input_;
  unsigned remaining_ = 100000;

  bool read(uint64_t at, void *data, size_t size) {
    input_.clear();
    input_.seekg(static_cast<std::streamoff>(at));
    return bool(input_.read(static_cast<char *>(data), size));
  }
  static uint32_t u32(const uint8_t *p) {
    return uint32_t(p[0]) << 24 | uint32_t(p[1]) << 16 |
           uint32_t(p[2]) << 8 | p[3];
  }
  static int u16(const uint8_t *p) { return int(p[0]) << 8 | p[1]; }
  std::optional<Box> box(uint64_t at, uint64_t end) {
    uint8_t header[16];
    if (!remaining_ || at > end || end - at < 8 || !read(at, header, 8))
      return {};
    --remaining_;
    uint64_t size = u32(header), head = 8;
    if (size == 1) {
      if (end - at < 16 || !read(at + 8, header + 8, 8)) return {};
      size = uint64_t(u32(header + 8)) << 32 | u32(header + 12);
      head = 16;
    } else if (size == 0) {
      size = end - at;
    }
    if (size < head || size > end - at) return {};
    Box result{at + head, at + size, {}};
    std::memcpy(result.type.data(), header + 4, 4);
    return result;
  }
  std::optional<Box> child(uint64_t at, uint64_t end, const char *type,
                           unsigned index = 0) {
    while (at < end) {
      auto current = box(at, end);
      if (!current) return {};
      if (!std::memcmp(current->type.data(), type, 4) && index-- == 0)
        return current;
      at = current->end;
    }
    return {};
  }

public:
  explicit MovieCropReader(std::istream &input) : input_(input) {}
  std::optional<Area> read(int track, int raw_width, int raw_height,
                            int width, int height) {
    if (track < 0 || width < 4 || height < 4) return {};
    input_.clear();
    input_.seekg(0, std::ios::end);
    const auto size = input_.tellg();
    if (size < 0) return {};
    auto current = child(0, static_cast<uint64_t>(size), "moov");
    if (!current) return {};
    current = child(current->body, current->end, "trak", unsigned(track));
    for (const auto *name : {"mdia", "minf", "stbl", "stsd"}) {
      if (!current) return {};
      current = child(current->body, current->end, name);
    }
    uint8_t header[28];
    if (!current || current->end - current->body < 8 ||
        !read(current->body, header, 8) || u32(header) != 0 ||
        u32(header + 4) != 1) return {};
    auto entry = box(current->body + 8, current->end);
    if (!entry || entry->end != current->end ||
        std::memcmp(entry->type.data(), "CRAW", 4) ||
        entry->end - entry->body < 82 || !read(entry->body, header, 28) ||
        u16(header + 24) != width || u16(header + 26) != height) return {};
    current = child(entry->body + 82, entry->end, "CDI1");
    if (!current || current->end - current->body < 4 ||
        !read(current->body, header, 4) || u32(header) != 0) return {};
    current = child(current->body + 4, current->end, "IAD1");
    uint8_t iad[32];
    if (!current || current->end - current->body != sizeof(iad) ||
        !read(current->body, iad, sizeof(iad)) || u32(iad) != 0 ||
        u16(iad + 4) != raw_width || u16(iad + 6) != raw_height ||
        u16(iad + 8) != 1 || u16(iad + 10) != 0 ||
        u16(iad + 12) != 1 || u16(iad + 14) != 0) return {};
    const int left = u16(iad + 16), top = u16(iad + 18);
    const int right = u16(iad + 20), bottom = u16(iad + 22);
    if (right < left || bottom < top || right >= raw_width ||
        bottom >= raw_height || right - left + 1 != width ||
        bottom - top + 1 != height || (width & 1) || (height & 1)) return {};
    return Area{left, top, width, height};
  }
};
} // namespace crm
