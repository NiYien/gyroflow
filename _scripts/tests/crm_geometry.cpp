// SPDX-License-Identifier: GPL-3.0-or-later
// Standalone C++17 regression for the selected CRAW track's short IAD1 crop.
#include "../../src/rendering/crm_geometry.h"
#include <cassert>
#include <fstream>
#include <iostream>
#include <sstream>
#include <string>

std::string number(uint32_t value) {
  std::string result(4, '\0');
  for (int i = 0; i < 4; ++i) result[i] = char(value >> ((3 - i) * 8));
  return result;
}
std::string box(const char *type, const std::string &body) {
  return number(uint32_t(body.size() + 8)) + std::string(type, 4) + body;
}
std::string track(std::string iad) {
  std::string visual(82, '\0');
  visual[24] = 0x1b; visual[25] = 0x30;
  visual[26] = 0x12; visual[27] = 0x20;
  auto craw = box("CRAW", visual + box("CDI1", number(0) + box("IAD1", iad)));
  return box("trak", box("mdia", box("minf", box("stbl",
      box("stsd", number(0) + number(1) + craw)))));
}
std::optional<crm::Area> read(const std::string &data, int index = 0,
                             int raw_width = 6984, int width = 6960) {
  std::istringstream input(data);
  return crm::MovieCropReader(input).read(index, raw_width, 4656, width, 4640);
}
int main(int argc, char **argv) {
  // Actual IAD1 payload from Optyczne's C50 Open Gate sample (film 1).
  const unsigned char bytes[] = {
      0,0,0,0, 0x1b,0x48,0x12,0x30, 0,1,0,0, 0,1,0,0,
      0,12,0,8, 0x1b,0x3b,0x12,0x27, 0,0,0,0, 0x1b,0x47,0x12,0x2f};
  const std::string iad(reinterpret_cast<const char *>(bytes), sizeof(bytes));
  const auto valid = box("moov", track(iad));
  const auto area = read(valid);
  assert(area && area->left == 12 && area->top == 8 &&
         area->width == 6960 && area->height == 4640);
  assert(!read(valid, 1));
  assert(!read(valid, -1));
  assert(!read(valid, 0, 6982));
  assert(!read(valid, 0, 6984, 6958));
  auto changed = iad;
  changed[17] = 13;
  assert(!read(box("moov", track(changed))));
  changed = iad; changed[20] = char(0xff); changed[21] = char(0xff);
  assert(!read(box("moov", track(changed))));
  changed = iad; changed[0] = 1;
  assert(!read(box("moov", track(changed))));
  for (int field : {9,11,13,15}) {
    changed = iad; changed[field] = 2;
    assert(!read(box("moov", track(changed))));
  }
  changed = iad + std::string(16, '\0');
  assert(!read(box("moov", track(changed))));
  assert(!read(box("moov", track(iad.substr(0, 24)))));
  const auto multiple = box("moov", box("trak", "") + track(iad));
  assert(!read(multiple));
  assert(read(multiple, 1));
  const auto extended = number(1) + std::string("moov") + number(0) +
                        number(uint32_t(valid.size() + 8)) + valid.substr(8);
  assert(read(extended));
  for (size_t cut = 0; cut < valid.size(); ++cut)
    assert(!read(valid.substr(0, cut)));
  assert(!read(number(0xffffffff) + "moov"));
  assert(!read(number(4) + "moov"));
  if (argc == 2) {
    std::ifstream input(argv[1], std::ios::binary);
    const auto actual = crm::MovieCropReader(input).read(0, 6984, 4656, 6960, 4640);
    assert(actual && actual->left == 12 && actual->top == 8);
  }
  std::cout << "CRM_GEOMETRY_PASS valid crop, selected track, encoded size, "
               "container size, malformed/truncated boxes, unsupported layouts\n";
}
