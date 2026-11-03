/* -*- C++ -*-
 * Copyright 2026 LibRaw LLC (info@libraw.org)
 * Copyright (C) 2026 Jiang Yaogeng (Sony ARW6 CRAW HQ decoder;
 *   code generated with assistance from GLM-5)
 *
 LibRaw is free software; you can redistribute it and/or modify
 it under the terms of the one of two licenses as you choose:

1. GNU LESSER GENERAL PUBLIC LICENSE version 2.1
   (See file LICENSE.LGPL provided in LibRaw distribution archive for details).

2. COMMON DEVELOPMENT AND DISTRIBUTION LICENSE (CDDL) Version 1.0
   (See file LICENSE.CDDL provided in LibRaw distribution archive for details).

 */


// Extracted from LibRaw 2caab57e269cc7fce17c3189e4d4d7e3e5861875.
// Only the portable LLVC3 primitives used by the X-OCN adapter are retained.
#pragma once
#include <algorithm>
#include <cstdint>
#include <cstring>
#include <vector>
namespace xocn { namespace detail {
using uchar = unsigned char;
using ushort = unsigned short;
using INT64 = int64_t;
constexpr int LIBRAW_EXCEPTION_IO_CORRUPT = -100008;
constexpr int SONY_ARW6_INTERNAL_BIAS = 2048;
constexpr size_t SONY_ARW6_MAX_PLANE_SAMPLES = size_t(128) * 1024 * 1024;
static inline uint16_t sony_arw6_be16(const uchar *p)
{
  return uint16_t((uint16_t(p[0]) << 8) | p[1]);
}

static inline uint32_t sony_arw6_be32(const uchar *p)
{
  return (uint32_t(p[0]) << 24) | (uint32_t(p[1]) << 16) |
         (uint32_t(p[2]) << 8) | uint32_t(p[3]);
}

static inline uint32_t sony_arw6_le32(const uchar *p)
{
  return (uint32_t(p[3]) << 24) | (uint32_t(p[2]) << 16) |
         (uint32_t(p[1]) << 8) | uint32_t(p[0]);
}

static inline void sony_arw6_require(bool cond)
{
  if (!cond)
    throw LIBRAW_EXCEPTION_IO_CORRUPT;
}

static inline int32_t sony_arw6_floor_shift(int64_t x, int bits)
{
  if (bits <= 0)
    return int32_t(x);
  const int64_t d = int64_t(1) << bits;
  return int32_t(x >= 0 ? x >> bits : -(((-x) + d - 1) >> bits));
}

static inline int32_t sony_arw6_sign16(int32_t x)
{
  x &= 0xffff;
  return x >= 0x8000 ? x - 0x10000 : x;
}

static inline int32_t sony_arw6_edge_detail(int32_t x, bool odd_mode)
{
  const int32_t signed_half_step = x > 0 ? 1 : (x < 0 ? -1 : 0);
  const bool is_odd = (x % 2) != 0;
  const bool use_half_step =
      odd_mode ? is_odd : (x != 0 && !is_odd);
  return 2 * x + (use_half_step ? signed_half_step : 0);
}


static size_t sony_arw6_checked_plane_samples(int rows, int cols)
{
  sony_arw6_require(rows >= 0 && cols >= 0);
  const uint64_t r = uint64_t(rows);
  const uint64_t c = uint64_t(cols);
  sony_arw6_require(c == 0 || r <= uint64_t(~size_t(0)) / c);
  const uint64_t samples = r * c;
  sony_arw6_require(samples <= uint64_t(SONY_ARW6_MAX_PLANE_SAMPLES));
  return size_t(samples);
}

struct SonyArw6Plane
{
  int rows;
  int cols;
  std::vector<int32_t> data;

  SonyArw6Plane() : rows(0), cols(0) {}
  SonyArw6Plane(int r, int c)
      : rows(r), cols(c), data(sony_arw6_checked_plane_samples(r, c))
  {
  }

  int32_t &at(int r, int c) { return data[size_t(r) * size_t(cols) + c]; }
  const int32_t &at(int r, int c) const
  {
    return data[size_t(r) * size_t(cols) + c];
  }
  int32_t *row(int r) { return &data[size_t(r) * size_t(cols)]; }
  const int32_t *row(int r) const { return &data[size_t(r) * size_t(cols)]; }
};

struct SonyArw6DirectoryEntry
{
  int group;
  int index;
  uint32_t start;
  uint32_t length;
};

struct SonyArw6StreamInfo
{
  int index;
  uint32_t offset;
  uint32_t length;
  int tile_x;
  int tile_y;
  int tile_w;
  int tile_h;
  int coded_width;
  int coded_half_height;
  int logical_height;
};

struct SonyArw6PacketRecord
{
  uint32_t byte_length;
  uchar selectors[3];
  uint32_t payload_offset;
};

struct SonyArw6Packet
{
  int type;
  int block_count;
  uint32_t control_bytes;
  uint32_t total_bytes;
  const uchar *data;
  uint32_t length;
  std::vector<SonyArw6PacketRecord> records;
};


class SonyArw6HeaderBits
{
  const uchar *data_;
  uint32_t size_;
  uint32_t bitpos_;

public:
  SonyArw6HeaderBits(const uchar *data, uint32_t size)
      : data_(data), size_(size), bitpos_(0)
  {
  }

  uint32_t read(int nbits)
  {
    sony_arw6_require(nbits >= 0 && nbits <= 32);
    sony_arw6_require(uint64_t(bitpos_) + uint32_t(nbits) <=
                      uint64_t(size_) * 8U);
    uint32_t out = 0;
    for (int i = 0; i < nbits; i++)
    {
      const uchar byte = data_[bitpos_ >> 3];
      const uint32_t bit = (byte >> (7 - (bitpos_ & 7))) & 1;
      out = (out << 1) | bit;
      bitpos_++;
    }
    return out;
  }
};

class SonyArw6NativeBits
{
  const uchar *data_;
  uint32_t size_;
  int64_t ptr_;
  uint64_t cur_;
  int bit_;
  int words_left_;

public:
  int status;

  SonyArw6NativeBits(const uchar *packet, uint32_t packet_len,
                     uint32_t payload_offset, uint32_t byte_length)
      : data_(packet), size_(packet_len), ptr_(int64_t(payload_offset) - 8),
        cur_(0), bit_(0),
        words_left_(((((int)byte_length + 7) / 8) + 1) & ~1), status(0)
  {
  }

  void load_next_word()
  {
    words_left_--;
    if (words_left_ < 0)
    {
      status = 2;
      return;
    }
    ptr_ += 8;
    uint64_t x = 0;
    for (int i = 0; i < 8; i++)
    {
      x <<= 8;
      const int64_t pos = ptr_ + i;
      if (pos >= 0 && pos < int64_t(size_))
        x |= data_[pos];
    }
    cur_ = x;
    bit_ = 64;
  }

  uint32_t read_bits(int nbits)
  {
    sony_arw6_require(nbits >= 0 && nbits <= 32);
    uint32_t out = 0;
    int remaining = nbits;
    while (remaining > 0)
    {
      if (bit_ <= 0)
      {
        load_next_word();
        if (status)
          return out << remaining;
      }
      const int take = std::min(remaining, bit_);
      bit_ -= take;
      out = (out << take) |
            uint32_t((cur_ >> bit_) & ((uint64_t(1) << take) - 1));
      remaining -= take;
    }
    return out;
  }

  int read_unary_zeros_plus_one()
  {
    int zeros = 0;
    while (!status)
    {
      const uint32_t bit = read_bits(1);
      if (status)
        break;
      if (bit)
        return zeros + 1;
      zeros++;
    }
    return zeros + 1;
  }
};

static int sony_arw6_update_width(SonyArw6NativeBits &br, int wstate)
{
  if (br.read_bits(1) == 0 || br.status)
    return wstate;
  if (br.read_bits(1) == 0 || br.status)
    return wstate + br.read_unary_zeros_plus_one();

  for (int zeros = 0; zeros < std::max(0, wstate - 1); zeros++)
  {
    if (br.read_bits(1))
      return wstate - (zeros + 1);
  }
  return 0;
}

static int sony_arw6_read_zero_run(SonyArw6NativeBits &br, int remaining)
{
  if (remaining <= 1)
    return remaining;
  int max_prefix = 0;
  for (int x = remaining - 1; x; x >>= 1)
    max_prefix++;
  int zeros = 0;
  while (zeros < max_prefix)
  {
    if (br.read_bits(1))
      break;
    if (br.status)
      return remaining;
    zeros++;
  }
  if (zeros >= max_prefix)
    return remaining;
  const int base = 1 << zeros;
  if (base >= remaining)
    return remaining;
  const int extra = zeros ? int(br.read_bits(zeros)) : 0;
  return std::min(base + extra, remaining);
}

static void sony_arw6_decode_coeff_group(SonyArw6NativeBits &br, int nbits,
                                         int shift, int32_t vals[4])
{
  for (int i = 0; i < 4; i++)
  {
    uint32_t x = nbits > 0 ? br.read_bits(nbits) : 0;
    int32_t v = int32_t(x);
    if (shift > 0 && x > 0)
      v = int32_t(((uint64_t(2) * x + 1) << (shift - 1)) - (x & 1));
    vals[i] = v;
  }
}

static void sony_arw6_apply_signs(SonyArw6NativeBits &br, int32_t vals[4])
{
  for (int i = 0; i < 4; i++)
    if (vals[i] > 0)
    {
      const uint32_t bit = br.read_bits(1);
      vals[i] = vals[i] - 2 * vals[i] * int32_t(bit);
    }
}

static void sony_arw6_decode_component(SonyArw6NativeBits &br, int groups,
                                       int shift, int32_t *dst, int row_width)
{
  int width_state = sony_arw6_update_width(br, 0);
  int gi = 0;
  int out = 0;
  while (gi < groups)
  {
    if (br.status || width_state > 0x13)
    {
      br.status = br.status ? br.status : 1;
      while (out < row_width)
        dst[out++] = 0;
      return;
    }
    if (width_state == 0)
    {
      const int run = sony_arw6_read_zero_run(br, groups - gi);
      for (int r = 0; r < run; r++)
        for (int k = 0; k < 4 && out < row_width; k++)
          dst[out++] = 0;
      gi += run;
      if (gi >= groups)
        break;
      width_state = br.read_unary_zeros_plus_one();
      continue;
    }

    int32_t vals[4];
    sony_arw6_decode_coeff_group(br, width_state, shift, vals);
    const int next_width =
        gi + 1 < groups ? sony_arw6_update_width(br, width_state) : width_state;
    sony_arw6_apply_signs(br, vals);
    for (int k = 0; k < 4 && out < row_width; k++)
      dst[out++] = vals[k];
    width_state = next_width;
    gi++;
  }
  while (out < row_width)
    dst[out++] = 0;
}


static std::vector<SonyArw6DirectoryEntry>
sony_arw6_parse_directory(const uchar *stream, uint32_t stream_size)
{
  sony_arw6_require(stream_size >= 0x80);
  std::vector<SonyArw6DirectoryEntry> entries;
  uint32_t group_lengths[5];
  const int offs[5] = {0, 3, 6, 9, 12};
  for (int g = 0; g < 5; g++)
    group_lengths[g] =
        ((sony_arw6_be32(stream + 0x10 + offs[g]) >> 4) & 0x0ffffff0);

  uint32_t pos = 0x30;
  uint32_t base = 0;
  for (int group = 0; group < 5; group++)
  {
    sony_arw6_require(pos + 0x10 <= stream_size);
    const int n_entries = stream[pos] & 0x0f;
    sony_arw6_require(n_entries >= 0 && n_entries <= 9);
    uint32_t values[9] = {0};
    for (int i = 0; i < 5; i++)
      values[i] = (sony_arw6_be32(stream + pos + offs[i]) & 0x00ffffff) << 4;
    uint32_t consumed = 0x10;
    if (n_entries >= 5)
    {
      sony_arw6_require(pos + 0x20 <= stream_size);
      for (int i = 0; i < 4; i++)
        values[5 + i] =
            (sony_arw6_be32(stream + pos + 0x10 + offs[i]) & 0x00ffffff)
            << 4;
      consumed = 0x20;
    }

    uint32_t local = 0;
    for (int index = 0; index < n_entries; index++)
    {
      SonyArw6DirectoryEntry e;
      e.group = group;
      e.index = index;
      sony_arw6_require(base <= UINT32_MAX - local &&
                        0x80 <= UINT32_MAX - base - local);
      e.start = 0x80 + base + local;
      e.length = values[index];
      sony_arw6_require(e.start <= stream_size &&
                        e.length <= stream_size - e.start);
      entries.push_back(e);
      sony_arw6_require(local <= UINT32_MAX - e.length);
      local += e.length;
    }
    sony_arw6_require(local <= group_lengths[group]);
    sony_arw6_require(base <= UINT32_MAX - group_lengths[group]);
    base += group_lengths[group];
    pos += consumed;
  }
  return entries;
}

static SonyArw6DirectoryEntry
sony_arw6_find_entry(const std::vector<SonyArw6DirectoryEntry> &entries,
                     int group, int index)
{
  for (size_t i = 0; i < entries.size(); i++)
    if (entries[i].group == group && entries[i].index == index)
      return entries[i];
  throw LIBRAW_EXCEPTION_IO_CORRUPT;
}

static SonyArw6Packet sony_arw6_parse_packet(const uchar *data, uint32_t length)
{
  sony_arw6_require(length >= 16);
  SonyArw6HeaderBits br(data, length);
  const uint32_t control_count = br.read(16);
  const uint32_t extra_count = br.read(24);
  const uint32_t tag4 = br.read(4);
  const uint32_t reserved4 = br.read(4);
  const uint32_t type2 = br.read(2);
  const uint32_t control_words = br.read(6);
  const uint32_t block_count = br.read(16);
  const uint32_t width_marker = br.read(8);
  const uint32_t reserved8 = br.read(8);
  for (int i = 0; i < 5; i++)
    sony_arw6_require(br.read(8) == 0);

  sony_arw6_require(tag4 == 4 && reserved4 == 0 && control_words == 0);
  sony_arw6_require(type2 == 1 || type2 == 3);
  sony_arw6_require(block_count > 0 && block_count <= 30000);
  sony_arw6_require(width_marker == 0x10 && reserved8 == 0);

  SonyArw6Packet packet;
  packet.type = int(type2);
  packet.block_count = int(block_count);
  const uint64_t control_bytes = (uint64_t(control_count) + 1U) << 4;
  const uint64_t total_bytes =
      (uint64_t(control_count) + 1U + extra_count) << 4;
  sony_arw6_require(control_bytes <= UINT32_MAX &&
                    total_bytes <= UINT32_MAX);
  packet.control_bytes = uint32_t(control_bytes);
  packet.total_bytes = uint32_t(total_bytes);
  packet.data = data;
  packet.length = length;
  sony_arw6_require(packet.total_bytes == length);
  sony_arw6_require(packet.control_bytes <= length);

  uint32_t cursor = packet.control_bytes;
  packet.records.reserve(block_count);
  for (uint32_t i = 0; i < block_count; i++)
  {
    SonyArw6PacketRecord rec;
    rec.byte_length = br.read(16);
    memset(rec.selectors, 0, sizeof(rec.selectors));
    for (uint32_t j = 0; j < type2; j++)
      rec.selectors[j] = uchar(br.read(4));
    rec.payload_offset = cursor;
    sony_arw6_require(rec.byte_length <= length &&
                      cursor <= length - rec.byte_length);
    cursor += rec.byte_length;
    packet.records.push_back(rec);
  }
  sony_arw6_require(cursor <= packet.total_bytes);
  return packet;
}


static SonyArw6Plane sony_arw6_integrate_type1(const SonyArw6Plane &coeffs,
                                               int rows, int dc_offset)
{
  sony_arw6_require(rows > 0 && rows <= coeffs.rows);
  SonyArw6Plane out(rows, coeffs.cols);
  for (int y = 0; y < rows; y++)
  {
    int32_t acc = sony_arw6_sign16(coeffs.at(y, 0)) * 2;
    out.at(y, 0) = acc / 2 + dc_offset;
    for (int x = 1; x < coeffs.cols; x++)
    {
      acc += sony_arw6_sign16(coeffs.at(y, x)) * 2;
      out.at(y, x) = acc / 2 + dc_offset;
    }
  }
  return out;
}


static SonyArw6Plane sony_arw6_final_green(const SonyArw6Plane &ll,
                                           const SonyArw6Plane &detail,
                                           int top_rows)
{
  const int h = ll.rows;
  const int w = ll.cols;
  sony_arw6_require(top_rows >= 0 && top_rows <= 8);
  sony_arw6_require(detail.cols == w &&
                    detail.rows >= 8 + std::max(0, h - top_rows));

  SonyArw6Plane selected(h, w);
  for (int y = 0; y < h; y++)
  {
    const int src_y = y < top_rows ? y : y + 8 - top_rows;
    for (int x = 0; x < w; x++)
      selected.at(y, x) = detail.at(src_y, x);
  }

  SonyArw6Plane odd_green(h, w);
  for (int y = 0; y < h; y++)
  {
    const int prev_y = y ? y - 1 : y;
    for (int x = 0; x < w; x++)
    {
      int32_t pred;
      if (x + 1 < w)
        pred = sony_arw6_floor_shift(
            selected.at(y, x + 1) + selected.at(prev_y, x) +
                selected.at(y, x) + selected.at(prev_y, x + 1),
            2);
      else
        pred = sony_arw6_floor_shift(
            (selected.at(prev_y, x) + selected.at(y, x)) * 2, 2);
      odd_green.at(y, x) =
          sony_arw6_floor_shift(int64_t(2) * ll.at(y, x) - pred, 1);
    }
  }

  SonyArw6Plane even_green(h, w);
  for (int y = 0; y < h; y++)
  {
    const int next_y = y + 1 < h ? y + 1 : y;
    even_green.at(y, 0) =
        selected.at(y, 0) +
        sony_arw6_floor_shift((odd_green.at(y, 0) + odd_green.at(next_y, 0)) *
                                  int64_t(2),
                              2);
    for (int x = 1; x < w; x++)
      even_green.at(y, x) =
          selected.at(y, x) +
          sony_arw6_floor_shift(odd_green.at(y, x - 1) +
                                    odd_green.at(next_y, x - 1) +
                                    odd_green.at(next_y, x) +
                                    odd_green.at(y, x),
                                2);
  }

  SonyArw6Plane out(h, w * 2);
  for (int y = 0; y < h; y++)
    for (int x = 0; x < w; x++)
    {
      out.at(y, x * 2) = even_green.at(y, x);
      out.at(y, x * 2 + 1) = odd_green.at(y, x);
    }
  return out;
}


static inline int32_t sony_arw6_clamp_signed_code(int32_t x)
{
  int code = x + SONY_ARW6_INTERNAL_BIAS;
  if (code < 0)
    code = 0;
  if (code > 4095)
    code = 4095;
  return code - SONY_ARW6_INTERNAL_BIAS;
}


} }

