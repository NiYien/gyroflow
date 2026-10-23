// SPDX-License-Identifier: GPL-3.0-or-later
// Link with the production CRM decoder, LibRaw and MDK. Pass Optyczne's
// complete C50 film 1 fixture (168 frames, 6960x4640 at 25 fps).
#include <cstdlib>
#include <cmath>
#include <cstdint>
#include <iostream>
#include <string>
#include <vector>

#define CHECK(condition) do { if (!(condition)) { \
  std::cerr << "CRM_NATIVE_GEOMETRY_FAIL at line " << __LINE__ \
            << ": " << #condition << '\n'; std::exit(1); } } while (false)

struct CrmInfo {
  double duration_ms, fps, bitrate;
  uint64_t frames;
  uint32_t width, height;
  int32_t rotation;
  char created[64];
};
struct CrmRange { double from, to; };
using FrameCallback = bool (*)(void *, double, uint32_t, uint32_t, uint32_t,
                              const uint8_t *);
extern "C" int niyien_crm_read(const char *, const char *, CrmInfo *,
    const CrmRange *, size_t, FrameCallback, bool (*)(void *), void *);

struct Frame {
  int64_t timestamp_us;
  uint64_t pixels;
  bool operator==(const Frame &other) const {
    return timestamp_us == other.timestamp_us && pixels == other.pixels;
  }
};
struct Received { std::vector<Frame> frames; size_t limit = 0; bool invalid = false; };
bool receive(void *user, double ms, uint32_t width, uint32_t height,
             uint32_t stride, const uint8_t *data) {
  auto &received = *static_cast<Received *>(user);
  if (!data || width != 1620 || height != 1080 || stride < width * 4) {
    received.invalid = true;
    return false;
  }
  uint64_t pixels = 14695981039346656037ull;
  for (uint32_t y = 0; y < height; ++y)
    for (uint32_t x = 0; x < width * 4; ++x) {
      pixels ^= data[size_t(y) * stride + x];
      pixels *= 1099511628211ull;
    }
  received.frames.push_back({std::llround(ms * 1000), pixels});
  return !received.limit || received.frames.size() < received.limit;
}
int main(int argc, char **argv) {
  if (argc != 2) return 2;
  const std::string path = argv[1];
  const char digits[] = "0123456789abcdef";
  std::string decoder = "CRM:source_hex=";
  for (unsigned char byte : path) {
    decoder.push_back(digits[byte >> 4]);
    decoder.push_back(digits[byte & 15]);
  }
  CrmInfo info{};
  CHECK(niyien_crm_read(path.c_str(), decoder.c_str(), &info,
      nullptr, 0, nullptr, nullptr, nullptr) == 0);
  CHECK(info.width == 6960 && info.height == 4640 && info.frames == 168);
  CHECK(std::abs(info.fps - 25) < 0.001);
  Received full;
  CHECK(niyien_crm_read(path.c_str(), decoder.c_str(), nullptr,
      nullptr, 0, receive, nullptr, &full) == 0);
  CHECK(!full.invalid && full.frames.size() == 168);
  for (size_t i = 0; i < full.frames.size(); ++i)
    CHECK(full.frames[i].timestamp_us == int64_t(i) * 40000);

  const CrmRange ranges[] = {{6000,6680}, {0,80}, {3200,3280}};
  Received selected;
  CHECK(niyien_crm_read(path.c_str(), decoder.c_str(), nullptr,
      ranges, 3, receive, nullptr, &selected) == 0);
  std::vector<Frame> expected(full.frames.begin()+150, full.frames.end());
  expected.insert(expected.end(), full.frames.begin(), full.frames.begin()+3);
  expected.insert(expected.end(), full.frames.begin()+80, full.frames.begin()+83);
  CHECK(!selected.invalid && selected.frames == expected);

  Received cancelled; cancelled.limit = 1;
  CHECK(niyien_crm_read(path.c_str(), decoder.c_str(), nullptr,
      nullptr, 0, receive, nullptr, &cancelled) == 0);
  CHECK(!cancelled.invalid && cancelled.frames.size() == 1);
  Received reopened;
  CHECK(niyien_crm_read(path.c_str(), decoder.c_str(), nullptr,
      nullptr, 0, receive, nullptr, &reopened) == 0);
  CHECK(!reopened.invalid && reopened.frames == full.frames);
  std::cout << "CRM_NATIVE_GEOMETRY_PASS frames=168 ranges=24 cancel_then_full=168\n";
}
