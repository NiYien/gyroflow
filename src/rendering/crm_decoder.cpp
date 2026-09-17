// SPDX-License-Identifier: GPL-3.0-or-later
// LibRaw is used only for preview frames; MDK retains demux, timestamps and
// audio.
#define NOMINMAX
#define WIN32_LEAN_AND_MEAN
// Keep std::async compatible with older MSVC runtimes still used by the host
// app.
#define _DISABLE_CONSTEXPR_MUTEX_CONSTRUCTOR
#include "mdk/FrameReader.h"
#include "mdk/MediaInfo.h"
#include "mdk/Packet.h"
#include "mdk/VideoDecoder.h"
#include "mdk/VideoFrame.h"
#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <condition_variable>
#include <deque>
#include <filesystem>
#include <future>
#include <iostream>
#include <libraw/libraw.h>
#include <limits>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <thread>
#include <vector>
#ifdef _OPENMP
#include <omp.h>
#endif

namespace {
using namespace mdk;
using Raw = std::unique_ptr<LibRaw>;
using LogCallback = void (*)(const char *);
std::atomic<LogCallback> logger{nullptr};
struct DecodeConcurrency {
  unsigned frames, threads;
};
const DecodeConcurrency &decode_concurrency() {
  static const auto value = [] {
    const unsigned cores = std::max(1u, std::thread::hardware_concurrency());
#ifdef _OPENMP
    const unsigned frames = std::clamp(cores / 2, 1u, 4u);
    // CRX has four Bayer planes. Three threads leave one thread decoding twice.
    const unsigned threads = cores >= frames * 4 ? 4 : std::min(cores, 2u);
    return DecodeConcurrency{frames, threads};
#else
    return DecodeConcurrency{std::min(cores, 4u), 1};
#endif
  }();
  return value;
}
void report(const std::string &message) {
  if (auto callback = logger.load())
    callback(message.c_str());
  else
    std::fprintf(stderr, "%s\n", message.c_str());
}
struct Clip {
  std::string path;
  int width = 0, height = 0;
  std::vector<std::pair<int64_t, unsigned>> offsets;
  std::array<std::array<uint8_t, 65536>, 3> lut{};
};

Raw open_raw(const std::string &path, unsigned index, bool unpack) {
#ifdef _OPENMP
  omp_set_num_threads(int(decode_concurrency().threads));
#endif
  auto raw = std::make_unique<LibRaw>();
  raw->imgdata.rawparams.shot_select = index;
  raw->imgdata.rawparams.max_raw_memory_mb = 512;
#ifdef _WIN32
  const int result = raw->open_file(std::filesystem::u8path(path).c_str());
#else
  const int result = raw->open_file(path.c_str());
#endif
  if (result)
    throw std::runtime_error(libraw_strerror(result));
  if (unpack) {
    const int result = raw->unpack();
    if (result)
      throw std::runtime_error(libraw_strerror(result));
  }
  raw->adjust_to_raw_inset_crop(1);
  return raw;
}

std::array<int, 4> bayer_offsets(LibRaw &raw) {
  const auto &s = raw.imgdata.sizes;
  if (!raw.imgdata.rawdata.raw_image || !raw.imgdata.idata.filters ||
      s.width < 4 || s.height < 4 || s.left_margin + s.width > s.raw_width ||
      s.top_margin + s.height > s.raw_height || s.raw_pitch < s.raw_width * 2)
    throw std::runtime_error("Unsupported CRM Bayer layout");
  std::array<int, 4> offsets{-1, -1, -1, -1};
  for (int y = 0; y < 2; ++y)
    for (int x = 0; x < 2; ++x) {
      int color = raw.COLOR(s.top_margin + y, s.left_margin + x);
      if (color == 1 && offsets[1] >= 0)
        color = 3;
      if (color < 0 || color > 3)
        throw std::runtime_error("Unsupported CRM color pattern");
      offsets[color] = y * (s.raw_pitch / 2) + x;
    }
  if (std::find(offsets.begin(), offsets.end(), -1) != offsets.end())
    throw std::runtime_error("CRM requires a 2x2 RGB Bayer pattern");
  return offsets;
}

std::shared_ptr<Clip> open_clip(const std::string &path, int width,
                                int height) {
  auto raw = open_raw(path, 0, true);
  auto clip = std::make_shared<Clip>();
  clip->path = path;
  const auto &s = raw->imgdata.sizes;
  clip->width = s.width;
  clip->height = s.height;
  if ((width > 0 && width != s.width) || (height > 0 && height != s.height))
    throw std::runtime_error(
        "CRM container and active sensor dimensions differ");

  // Pinned LibRaw 0.22.2 exposes these sample tables. Map packet byte positions
  // rather than rounding timestamps, so seeking and edit-list PTS stay exact.
  const auto &unpacker = raw->get_internal_data_pointer()->unpacker_data;
  if (unpacker.crx_track_selected < 0 ||
      unpacker.crx_track_selected >= LIBRAW_CRXTRACKS_MAXCOUNT)
    throw std::runtime_error("CRM RAW track missing");
  const auto &track = unpacker.crx_header[unpacker.crx_track_selected];
  if (!track.sample_count || track.sample_count > 1000000 || !track.stsc_data ||
      !track.stsc_count || !track.chunk_offsets ||
      (!track.sample_size && !track.sample_sizes))
    throw std::runtime_error("Invalid CRM sample index");
  unsigned sample = 0, run = 0;
  clip->offsets.reserve(track.sample_count);
  for (unsigned chunk = 0; chunk < track.chunk_count; ++chunk) {
    while (run + 1 < track.stsc_count &&
           track.stsc_data[run + 1].first <= chunk + 1)
      ++run;
    int64_t offset = track.chunk_offsets[chunk];
    for (unsigned j = 0; j < track.stsc_data[run].count; ++j) {
      if (sample >= track.sample_count || offset < 0)
        throw std::runtime_error("Invalid CRM chunk index");
      const int64_t size =
          track.sample_size ? track.sample_size : track.sample_sizes[sample];
      if (size <= 0 || offset > INT64_MAX - size)
        throw std::runtime_error("Invalid CRM sample size");
      clip->offsets.emplace_back(offset, sample++);
      offset += size;
    }
  }
  if (sample != track.sample_count)
    throw std::runtime_error("Incomplete CRM sample index");
  std::sort(clip->offsets.begin(), clip->offsets.end());

  const auto offsets = bayer_offsets(*raw);
  const int pitch = s.raw_pitch / 2;
  auto base =
      raw->imgdata.rawdata.raw_image + s.top_margin * pitch + s.left_margin;
  std::vector<float> greens;
  std::array<std::vector<float>, 3> channels;
  for (int y = 0; y + 1 < s.height; y += 16)
    for (int x = 0; x + 1 < s.width; x += 16) {
      const auto p = base + y * pitch + x;
      const float g = (p[offsets[1]] + p[offsets[3]]) * .5f;
      greens.push_back(g);
      channels[0].push_back(p[offsets[0]]);
      channels[1].push_back(g);
      channels[2].push_back(p[offsets[2]]);
    }
  std::sort(greens.begin(), greens.end());
  const double black = greens[size_t((greens.size() - 1) * .005)];
  const double white = std::max(
      black + 64.0, double(greens[size_t((greens.size() - 1) * .995)]));
  double means[3]{};
  for (int c = 0; c < 3; ++c) {
    for (float value : channels[c])
      means[c] += std::max(0.0, double(value) - black);
    means[c] = std::max(1.0, means[c] / channels[c].size());
  }
  for (int c = 0; c < 3; ++c) {
    const double gain = std::clamp(means[1] / means[c], .5, 2.0);
    for (int value = 0; value < 65536; ++value)
      clip->lut[c][value] = uint8_t(std::lround(
          255 * std::pow(std::clamp((value - black) * gain / (white - black),
                                    0.0, 1.0),
                         .45)));
  }
  report("CRM: LibRaw " + std::string(LibRaw::version()) + ", " +
         std::to_string(width) + "x" + std::to_string(height) + ", " +
         std::to_string(sample) + " frames, fixed preview tone " +
         std::to_string(black) + ".." + std::to_string(white));
  return clip;
}

VideoFrame decode_frame(const std::shared_ptr<const Clip> &clip, unsigned index,
                        double pts, double duration) {
  auto raw = open_raw(clip->path, index, true);
  const auto &s = raw->imgdata.sizes;
  if (s.width != clip->width || s.height != clip->height)
    throw std::runtime_error("CRM frame size changed");
  const auto offsets = bayer_offsets(*raw);
  const double scale = std::min({1.0, 1920.0 / s.width, 1080.0 / s.height});
  const int ow = std::max(2, int(s.width * scale) / 2 * 2),
            oh = std::max(2, int(s.height * scale) / 2 * 2);
  int strides[1] = {ow * 4};
  VideoFrame frame(ow, oh, PixelFormat::RGBA, strides);
  auto dst = frame.buffer(0)->data();
  const int pitch = s.raw_pitch / 2, sw = s.width / 2, sh = s.height / 2;
  auto base =
      raw->imgdata.rawdata.raw_image + s.top_margin * pitch + s.left_margin;
  std::vector<int> x0(ow), x1(ow);
  std::vector<float> wx(ow);
  for (int x = 0; x < ow; ++x) {
    double fx = std::clamp((x + .5) * sw / ow - .5, 0.0, double(sw - 1));
    x0[x] = int(fx);
    x1[x] = std::min(x0[x] + 1, sw - 1);
    wx[x] = float(fx - x0[x]);
  }
#ifdef _OPENMP
#pragma omp parallel for schedule(static)
#endif
  for (int y = 0; y < oh; ++y) {
    const double fy = std::clamp((y + .5) * sh / oh - .5, 0.0, double(sh - 1));
    const int y0 = int(fy), y1 = std::min(y0 + 1, sh - 1);
    const float wy = float(fy - y0);
    auto out = dst + y * strides[0];
    for (int x = 0; x < ow; ++x) {
      const auto p00 = base + y0 * 2 * pitch + x0[x] * 2;
      const auto p01 = base + y0 * 2 * pitch + x1[x] * 2;
      const auto p10 = base + y1 * 2 * pitch + x0[x] * 2;
      const auto p11 = base + y1 * 2 * pitch + x1[x] * 2;
      const auto interpolate = [&](float v00, float v01, float v10, float v11) {
        const float a = v00 + wx[x] * (v01 - v00);
        const float b = v10 + wx[x] * (v11 - v10);
        return uint16_t(std::clamp(a + wy * (b - a), 0.0f, 65535.0f));
      };
      const auto red = interpolate(p00[offsets[0]], p01[offsets[0]],
                                   p10[offsets[0]], p11[offsets[0]]);
      const auto green = interpolate(
          (p00[offsets[1]] + p00[offsets[3]]) * .5f,
          (p01[offsets[1]] + p01[offsets[3]]) * .5f,
          (p10[offsets[1]] + p10[offsets[3]]) * .5f,
          (p11[offsets[1]] + p11[offsets[3]]) * .5f);
      const auto blue = interpolate(p00[offsets[2]], p01[offsets[2]],
                                    p10[offsets[2]], p11[offsets[2]]);
      out[x * 4] = clip->lut[0][red];
      out[x * 4 + 1] = clip->lut[1][green];
      out[x * 4 + 2] = clip->lut[2][blue];
      out[x * 4 + 3] = 255;
    }
  }
  frame.setTimestamp(pts).setDuration(duration);
  frame.pixelAspectRatio(float(double(s.width) * oh / (double(s.height) * ow)));
  return frame;
}

class CrmDecoder final : public VideoDecoder {
  std::string path_;
  std::shared_ptr<const Clip> clip_;
  std::deque<std::future<VideoFrame>> pending_;
  bool receive(bool wait) {
    if (pending_.empty())
      return true;
    if (!wait && pending_.front().wait_for(std::chrono::milliseconds(0)) !=
                     std::future_status::ready)
      return true;
    auto future = std::move(pending_.front());
    pending_.pop_front();
    auto frame = future.get();
    frameDecoded(std::move(frame));
    return true;
  }

public:
  const char *name() const override { return "CRM"; }
  ~CrmDecoder() override { close(); }
  bool open() override {
    try {
      report("CRM decoder open: codec=" + parameters().codec +
             ", tag=" + std::to_string(parameters().codec_tag) +
             ", source=" + std::to_string(!path_.empty()));
      if (path_.empty() || parameters().codec_tag != 0x57415243)
        return false;
      clip_ = open_clip(path_, parameters().width, parameters().height);
      report("CRM concurrency: " + std::to_string(decode_concurrency().frames) +
             " frames x " + std::to_string(decode_concurrency().threads) +
             " threads");
      onOpen();
      decoderReady(name());
      return true;
    } catch (const std::exception &e) {
      report(std::string("CRM open: ") + e.what());
      return false;
    }
  }
  bool close() override {
    for (auto &future : pending_)
      future.wait();
    pending_.clear();
    clip_.reset();
    if (isOpen())
      onClose();
    return true;
  }
  bool flush() override {
    for (auto &future : pending_)
      future.wait();
    pending_.clear();
    onFlush();
    return true;
  }
  int decode(const Packet &packet) override {
    try {
      if (!clip_)
        return -1;
      if (packet.isEnd() || packet.isDrain()) {
        while (!pending_.empty())
          receive(true);
        return packet.isEnd() ? INT_MAX : 0;
      }
      if (packet.isFlush()) {
        flush();
        return 0;
      }
      const auto it =
          std::lower_bound(clip_->offsets.begin(), clip_->offsets.end(),
                           std::pair<int64_t, unsigned>{packet.position, 0});
      if (it == clip_->offsets.end() || it->first != packet.position)
        throw std::runtime_error("CRM packet has no matching RAW frame");
      auto clip = clip_;
      const unsigned index = it->second;
      const double pts = packet.pts, duration = packet.duration;
      pending_.push_back(
          std::async(std::launch::async, [clip, index, pts, duration] {
            return decode_frame(clip, index, pts, duration);
          }));
      if (pending_.size() >= decode_concurrency().frames)
        receive(true);
      return 0;
    } catch (const std::exception &e) {
      report(std::string("CRM decode: ") + e.what());
      return -1;
    }
  }

protected:
  bool processOutput(int timeout = 0) override {
    try {
      return receive(timeout != 0);
    } catch (const std::exception &e) {
      report(std::string("CRM output: ") + e.what());
      return false;
    }
  }
  void onPropertyChanged(const std::string &key,
                         const std::string &value) override {
    if (key != "source_hex") {
      VideoDecoder::onPropertyChanged(key, value);
      return;
    }
    path_.clear();
    if (value.size() % 2 || value.size() > 131072)
      return;
    auto digit = [](char c) -> int {
      if (c >= '0' && c <= '9')
        return c - '0';
      if (c >= 'a' && c <= 'f')
        return c - 'a' + 10;
      return -1;
    };
    for (size_t i = 0; i < value.size(); i += 2) {
      int a = digit(value[i]), b = digit(value[i + 1]);
      if (a < 0 || b < 0 || (!a && !b)) {
        path_.clear();
        return;
      }
      path_.push_back(char((a << 4) | b));
    }
  }
};
} // namespace

extern "C" void niyien_register_crm_decoder() {
  if (mdk::abiVersion() != MDK_ABI_VERSION) {
    report("CRM decoder: incompatible MDK ABI");
    return;
  }
  mdk::VideoDecoder::registerOnce("CRM", [] { return new CrmDecoder(); });
}
extern "C" void niyien_crm_set_logger(LogCallback callback) {
  logger.store(callback);
}

struct CrmInfo {
  double duration_ms, fps, bitrate;
  uint64_t frames;
  uint32_t width, height;
  int32_t rotation;
  char created[64];
};
struct CrmRange {
  double from, to;
};
using CrmFrameCallback = bool (*)(void *, double, uint32_t, uint32_t, uint32_t,
                                  const uint8_t *);
using CrmCancelCallback = bool (*)(void *);

namespace {
struct ProcessingQueue {
  std::mutex mutex;
  std::condition_variable changed;
  std::deque<mdk::VideoFrame> frames;
  bool stopping = false, done = false, error = false, started = false;
};
struct ProcessingSession {
  mdk::FrameReader::Ptr reader = mdk::FrameReader::create();
  std::shared_ptr<ProcessingQueue> queue = std::make_shared<ProcessingQueue>();
  ~ProcessingSession() {
    // Wake the producer before stopping MDK. Its internal bounded take() queue
    // can otherwise keep the decoder waiting while cancellation waits for it.
    {
      std::lock_guard lock(queue->mutex);
      queue->stopping = true;
    }
    queue->changed.notify_all();
    if (reader) {
      reader->stop();
      reader->waitFor(mdk::State::Stopped);
    }
  }
};
} // namespace

// Processing owns a cancellable two-frame queue; Rust is called on its caller
// thread.
extern "C" int niyien_crm_read(const char *path, const char *decoder,
                               CrmInfo *output, const CrmRange *ranges,
                               size_t count, CrmFrameCallback callback,
                               CrmCancelCallback cancelled, void *user) {
  try {
    if (mdk::abiVersion() != MDK_ABI_VERSION)
      return 4;
    niyien_register_crm_decoder();
    const CrmRange whole{0, std::numeric_limits<double>::max()};
    if (!count) {
      ranges = &whole;
      count = 1;
    }
    for (size_t range = 0; range < count; ++range) {
      if (cancelled && cancelled(user))
        return 0;
      ProcessingSession session;
      auto &reader = session.reader;
      auto queue = session.queue;
      if (!reader)
        return 1;
      reader->setMedia(path);
      reader->setDecoders(mdk::MediaType::Video, {decoder});
      reader->setActiveTracks(mdk::MediaType::Video, {0});
      reader->setActiveTracks(mdk::MediaType::Audio, {});
      reader->setBufferRange(0, 100, false);
      reader->setTimeout(15000);
      reader->onEvent([queue](const mdk::MediaEvent &event) {
        // Nonnegative values also carry buffer progress and thread state.
        if (event.error < 0) {
          std::lock_guard lock(queue->mutex);
          queue->error = true;
          queue->changed.notify_all();
        }
        return false;
      });
      reader->onStateChanged([queue](mdk::State state) {
        std::lock_guard lock(queue->mutex);
        if (state == mdk::State::Running)
          queue->started = true;
        if (state == mdk::State::Stopped && queue->started)
          queue->done = true;
        queue->changed.notify_all();
      });
      reader->onRead<mdk::VideoFrame>(
          [queue](const mdk::VideoFrame &frame, int) {
            std::unique_lock lock(queue->mutex);
            if (frame.timestamp() == mdk::TimestampEOS) {
              queue->done = true;
              queue->changed.notify_all();
              return true;
            }
            if (!frame)
              return true;
            queue->changed.wait(lock, [&] {
              return queue->stopping || queue->frames.size() < 2;
            });
            if (!queue->stopping)
              queue->frames.push_back(frame);
            queue->changed.notify_all();
            return true;
          });
      auto opened = std::make_shared<std::promise<bool>>();
      auto notified = std::make_shared<std::atomic_bool>(false);
      auto ready = opened->get_future();
      const bool started = reader->start(
          int64_t(ranges[range].from),
          [=](const mdk::MediaInfo *info, int64_t, bool *) {
            if (notified->exchange(true))
              return bool(callback);
            const bool ok = info && !info->video.empty();
            if (ok && output) {
              const auto &video = info->video[0];
              output->duration_ms = video.duration;
              output->fps = video.codec.frame_rate;
              output->bitrate = video.codec.bit_rate / 1048576.0;
              output->width = video.codec.width;
              output->height = video.codec.height;
              output->frames = video.frames;
              output->rotation = video.rotation;
              auto found = info->metadata.find("creation_time");
              if (found != info->metadata.end())
                std::snprintf(output->created, sizeof(output->created), "%s",
                              found->second.c_str());
            }
            opened->set_value(ok);
            return ok && callback;
          });
      if (!started)
        return 2;
      while (ready.wait_for(std::chrono::milliseconds(20)) !=
             std::future_status::ready) {
        if (cancelled && cancelled(user)) {
          return 0;
        }
      }
      if (!ready.get())
        return 2;
      if (callback) {
        unsigned delivered = 0;
        while (!(cancelled && cancelled(user))) {
          mdk::VideoFrame frame;
          {
            std::unique_lock lock(queue->mutex);
            queue->changed.wait_for(lock, std::chrono::milliseconds(20), [&] {
              return !queue->frames.empty() || queue->done || queue->error;
            });
            if (queue->error)
              return 3;
            if (queue->frames.empty()) {
              if (queue->done)
                break;
              continue;
            }
            frame = std::move(queue->frames.front());
            queue->frames.pop_front();
          }
          queue->changed.notify_all();
          const double timestamp = frame.timestamp() * 1000.0;
          if (timestamp > ranges[range].to)
            break;
          if (timestamp + 0.001 < ranges[range].from)
            continue;
          ++delivered;
          if (!callback(user, timestamp, frame.width(), frame.height(),
                        frame.bytesPerLine(), frame.buffer(0)->constData())) {
            return 0;
          }
        }
        if (!delivered && !(cancelled && cancelled(user))) {
          report("CRM processing failed: no frames in the requested range");
          return 3;
        }
      }
    }
    return 0;
  } catch (const std::exception &e) {
    report(std::string("CRM processing: ") + e.what());
    return 3;
  }
}
