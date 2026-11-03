// SPDX-License-Identifier: GPL-3.0-or-later
#define _DISABLE_CONSTEXPR_MUTEX_CONSTRUCTOR
#include "../../src/rendering/xocn_codec.cpp"
#include "../../src/rendering/xocn_reader.cpp"
#include <chrono>
#include <cstdio>
#include <fstream>
#include <stdexcept>
#include "xocn_gpu_bitstream.h"

namespace {
GpuCallbacks test_gpu_backend;
const xocn::RawFrame* test_raw_reference = nullptr;
const char* test_raw_output = nullptr;
uint64_t test_preview_calls = 0, test_raw_calls = 0, test_compressed_calls = 0;
uint64_t test_bitstream_success = 0;
double test_gpu_ms = 0;
bool test_synthetic_done = false;
bool synthetic_gpu(void* context, const uint16_t* linear, const uint8_t* tone) {
    struct Lattice {
        int cols, rows, top, ll_rows;
        std::array<int, 3> low_rows, high_rows;
    };
    const Lattice cases[] = {
        {32, 32, 4, 2, {2, 4, 8}, {2, 4, 8}},
        {34, 36, 11, 2, {2, 5, 9}, {3, 4, 9}},
        {34, 40, 10, 2, {2, 5, 10}, {3, 5, 10}},
        {34, 44, 9, 2, {2, 5, 11}, {3, 6, 11}},
        {34, 48, 8, 3, {3, 6, 12}, {3, 6, 12}},
        {34, 52, 7, 4, {4, 7, 13}, {3, 6, 13}},
        {34, 56, 6, 4, {4, 7, 14}, {3, 7, 14}},
        {34, 60, 5, 4, {4, 7, 15}, {3, 8, 15}},
    };
    uint32_t random = 0x53b19;
    for (const auto& lattice : cases) {
        std::array<xocn::detail::SonyArw6Plane, 31> planes;
        std::vector<xocn::detail::SonyArw6Plane> components;
        for (int component = 0; component < 3; ++component) {
            const int base = component * 10;
            planes[base] = {lattice.ll_rows, lattice.cols == 32 ? 2 : 3};
            for (int group = 0; group < 3; ++group) {
                const int first = base + 1 + group * 3;
                const int cols = 2 << group;
                planes[first] = {lattice.low_rows[group], cols};
                planes[first + 1] = {lattice.high_rows[group], cols + (lattice.cols == 32 ? 0 : 1)};
                planes[first + 2] = {lattice.high_rows[group], cols};
            }
        }
        planes.back() = {lattice.rows / 2, lattice.cols / 2};
        for (auto& plane : planes) for (auto& value : plane.data) {
            random = random * 1664525u + 1013904223u;
            value = int32_t(random % 4096u) - 2048;
        }
        for (int component = 0; component < 3; ++component) {
            std::array<xocn::detail::SonyArw6Plane, 10> bands;
            std::copy_n(planes.begin() + component * 10, 10, bands.begin());
            components.push_back(xocn::reconstruct_component(std::move(bands), lattice.top));
        }
        const auto expected = xocn::merge_components(lattice.cols, lattice.rows, components, planes.back());
        std::array<xocn::GpuPlane, 31> views;
        for (size_t i = 0; i < planes.size(); ++i)
            views[i] = {planes[i].data.data(), uint32_t(planes[i].rows), uint32_t(planes[i].cols)};
        const xocn::GpuImage image{uint32_t(lattice.cols), uint32_t(lattice.rows), 0, 0,
            uint32_t(lattice.cols), uint32_t(lattice.rows), uint32_t(lattice.cols), uint32_t(lattice.rows), uint32_t(lattice.cols * 2), 1};
        std::vector<uint16_t> actual(expected.pixels.size());
        if (!test_gpu_backend.reconstruct(context, views.data(), &image, linear, tone, reinterpret_cast<uint8_t*>(actual.data()))
            || actual != expected.pixels) {
            std::fprintf(stderr, "XOCN_GPU_SYNTHETIC_FAIL width=%d height=%d\n", lattice.cols, lattice.rows);
            const auto mismatch = std::mismatch(actual.begin(), actual.end(), expected.pixels.begin());
            if (mismatch.first != actual.end())
                std::fprintf(stderr, "XOCN_GPU_SYNTHETIC_MISMATCH pixel=%zu gpu=%u cpu=%u\n",
                    size_t(mismatch.first - actual.begin()), unsigned(*mismatch.first), unsigned(*mismatch.second));
            return false;
        }
    }
    std::puts("XOCN_GPU_SYNTHETIC_PASS cases=8 all_phase_combinations=1 odd_columns=1 negative_odd_coefficients=1");
    return verify_gpu_bitstreams(context, test_gpu_backend.decode, linear, tone, test_bitstream_success);
}
template<class Render> bool observe_output(void* context, const xocn::GpuImage* image,
    const uint16_t* linear, const uint8_t* tone, uint8_t* output, Render render) {
    const auto started = std::chrono::steady_clock::now();
    if (!render(image, output)) return false;
    test_gpu_ms += std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count();
    ++test_preview_calls;
    auto raw_image = *image;
    raw_image.output_mode = 1;
    raw_image.output_width = image->raw_width;
    raw_image.output_height = image->raw_height;
    raw_image.stride = image->raw_width * 2;
    std::vector<uint16_t> raw(size_t(image->raw_width) * image->raw_height);
    if (!render(&raw_image, reinterpret_cast<uint8_t*>(raw.data()))) return false;
    if (raw != test_raw_reference->pixels) {
        const auto mismatch = std::mismatch(raw.begin(), raw.end(), test_raw_reference->pixels.begin());
        std::fprintf(stderr, "XOCN_GPU_RAW_MISMATCH pixel=%zu gpu=%u cpu=%u\n",
            size_t(mismatch.first - raw.begin()), unsigned(*mismatch.first), unsigned(*mismatch.second));
        return false;
    }
    if (!test_raw_calls && test_raw_output) {
        std::ofstream file(test_raw_output, std::ios::binary);
        file.write(reinterpret_cast<const char*>(raw.data()), raw.size() * sizeof(uint16_t));
        if (!file) return false;
    }
    ++test_raw_calls;
    if (!test_synthetic_done) {
        if (!synthetic_gpu(context, linear, tone)) return false;
        test_synthetic_done = true;
    }
    return true;
}
bool observe_gpu(void* context, const xocn::GpuPlane* planes, const xocn::GpuImage* image,
    const uint16_t* linear, const uint8_t* tone, uint8_t* output) {
    return observe_output(context, image, linear, tone, output, [&](const auto* request, auto* target) {
        return test_gpu_backend.reconstruct(context, planes, request, linear, tone, target);
    });
}
bool observe_compressed(void* context, const xocn::GpuCompressed* compressed, const xocn::GpuImage* image,
    const uint16_t* linear, const uint8_t* tone, uint8_t* output) {
    const bool success = observe_output(context, image, linear, tone, output, [&](const auto* request, auto* target) {
        auto input = *compressed;
        if (request->output_mode != 0) { input.before_wait = nullptr; input.before_wait_user = nullptr; }
        return test_gpu_backend.decode(context, &input, request, linear, tone, target);
    });
    if (success) ++test_compressed_calls;
    return success;
}
void gpu_require(bool value, const char* message) { if (!value) throw std::runtime_error(message); }
void same_preview(const VideoFrame& expected, const VideoFrame& actual) {
    gpu_require(expected && actual && expected.width() == actual.width() && expected.height() == actual.height()
        && expected.timestamp() == actual.timestamp(), "GPU frame geometry or timestamp changed");
    for (int y = 0; y < expected.height(); ++y)
        gpu_require(std::memcmp(expected.buffer()->constData() + y * expected.buffer()->stride(),
            actual.buffer()->constData() + y * actual.buffer()->stride(), size_t(expected.width()) * 4) == 0,
            "RGBA pixels differ from the production CPU preview");
}
int failure_creates = 0, failure_reconstructs = 0, failure_destroys = 0, failure_compressed = 0;
void* unavailable_create() { ++failure_creates; return nullptr; }
void* failure_create() { ++failure_creates; return &failure_creates; }
void failure_destroy(void*) { ++failure_destroys; }
bool failure_reconstruct(void*, const xocn::GpuPlane*, const xocn::GpuImage*, const uint16_t*, const uint8_t*, uint8_t*) {
    ++failure_reconstructs;
    return false;
}
bool failure_decode(void*, const xocn::GpuCompressed*, const xocn::GpuImage*, const uint16_t*, const uint8_t*, uint8_t*) {
    ++failure_compressed;
    return false;
}
void fallback_samples(const char* source, Clip& cpu) {
    for (bool unavailable : {true, false}) {
        failure_creates = failure_reconstructs = failure_destroys = failure_compressed = 0;
        gpu_callbacks = {unavailable ? unavailable_create : failure_create, failure_destroy, failure_reconstruct, failure_decode};
        {
            Clip fallback(source, true);
            fallback.prepare_tone();
            for (size_t i = 0; i < 2; ++i) same_preview(cpu.video(i, 1080), fallback.video(i, 1080, true));
        }
        gpu_require(failure_creates == 1 && failure_reconstructs == 0 && failure_compressed == (unavailable ? 0 : 1)
            && failure_destroys == (unavailable ? 0 : 1), "GPU failure was retried or released incorrectly");
    }
    std::puts("XOCN_GPU_FALLBACK_PASS unavailable=2frames reconstruction_failure=2frames retries=0");
}
void prefetch_samples(const char* source, Clip& cpu) {
    gpu_callbacks = test_gpu_backend;
    Clip cached(source, true);
    cached.prepare_tone();
    const auto compare = [&](size_t index) { same_preview(cpu.video(index, 1080), cached.video(index, 1080, true)); };
    compare(0);
    const auto next = cached.take_cached_picture(1);
    gpu_require(!next.empty() && next == cpu.read(cpu.frames.at(1).picture), "Prefetched picture bytes changed");
    gpu_require(cached.take_cached_picture(1).empty(), "Prefetched picture was not consumed");
    compare(1);
    gpu_require(cached.take_cached_picture(175).empty() && cached.take_cached_picture(2).empty(), "A cache miss retained an unrelated picture");
    for (size_t index : {size_t(175), size_t(176), size_t(2), size_t(351), size_t(0)}) compare(index);
    compare(351);
    gpu_require(cached.take_cached_picture(352).empty(), "Prefetch crossed the final frame");
    std::puts("XOCN_PREFETCH_PASS cache_bytes=1 consumed=1 exact_index=1 forward_backward=1 tail_restart=1");
}
uint64_t benchmark_calls = 0;
bool benchmark_gpu(void* context, const xocn::GpuPlane* planes, const xocn::GpuImage* image,
    const uint16_t* linear, const uint8_t* tone, uint8_t* output) {
    const bool completed = test_gpu_backend.reconstruct(context, planes, image, linear, tone, output);
    return completed;
}
bool benchmark_compressed(void* context, const xocn::GpuCompressed* compressed, const xocn::GpuImage* image,
    const uint16_t* linear, const uint8_t* tone, uint8_t* output) {
    const bool completed = test_gpu_backend.decode(context, compressed, image, linear, tone, output);
    if (completed) ++benchmark_calls;
    return completed;
}
}

extern "C" int niyien_xocn_gpu_samples(const char* source, uint32_t frame_limit, const char* raw_output) {
    try {
        test_preview_calls = 0; test_raw_calls = 0; test_compressed_calls = 0; test_bitstream_success = 0; test_gpu_ms = 0; test_synthetic_done = false;
        test_raw_output = raw_output;
        test_gpu_backend = gpu_callbacks;
        gpu_require(test_gpu_backend.create && test_gpu_backend.destroy && test_gpu_backend.reconstruct && test_gpu_backend.decode, "Compressed GPU callbacks missing");
        gpu_callbacks = {};
        Clip cpu(source, true);
        cpu.prepare_tone();
        auto first_cpu = cpu.video(0, 1080);
        gpu_callbacks = {test_gpu_backend.create, test_gpu_backend.destroy, observe_gpu, observe_compressed};
        Clip gpu(source, true);
        gpu.prepare_tone();
        const auto frames = std::min<size_t>(frame_limit ? frame_limit : cpu.frames.size(), cpu.frames.size());
        uint64_t raw_pixels = 0, rgba_pixels = 0;
        double cpu_ms = 0, gpu_ms = 0;
        for (size_t i = 0; i < frames; ++i) {
            const auto compressed = cpu.read(cpu.frames[i].picture);
            const auto raw = xocn::decode(compressed.data(), compressed.size());
            test_raw_reference = &raw;
            auto started = std::chrono::steady_clock::now();
            const auto cpu_frame = i ? cpu.video(i, 1080) : first_cpu;
            cpu_ms += std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count();
            started = std::chrono::steady_clock::now();
            const auto gpu_frame = gpu.video(i, 1080, true);
            gpu_ms += std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count();
            gpu_require(test_preview_calls == i + 1 && test_raw_calls == i + 1 && test_compressed_calls == i + 1,
                "Compressed GPU did not complete both preview and RAW; coefficient GPU/fallback is not accepted");
            same_preview(cpu_frame, gpu_frame);
            raw_pixels += raw.pixels.size();
            rgba_pixels += uint64_t(cpu_frame.width()) * cpu_frame.height();
            if (i % 32 == 0) {
                std::printf("XOCN_GPU_PROGRESS frame=%zu\n", i);
                std::fflush(stdout);
            }
        }
        test_raw_reference = nullptr;
        fallback_samples(source, cpu);
        prefetch_samples(source, cpu);
        gpu_callbacks = test_gpu_backend;
        std::printf("XOCN_GPU_PASS frames=%zu raw_pixels=%llu rgba_pixels=%llu preview_calls=%llu raw_calls=%llu compressed_calls=%llu bitstream_success=%llu cpu_reference_ms_excluding_first=%.3f gpu_with_raw_validation_ms=%.3f gpu_preview_compute_ms=%.3f\n",
            frames, static_cast<unsigned long long>(raw_pixels), static_cast<unsigned long long>(rgba_pixels),
            static_cast<unsigned long long>(test_preview_calls), static_cast<unsigned long long>(test_raw_calls), static_cast<unsigned long long>(test_compressed_calls), static_cast<unsigned long long>(test_bitstream_success), cpu_ms, gpu_ms, test_gpu_ms);
        return 0;
    } catch (const std::exception& error) {
        gpu_callbacks = test_gpu_backend;
        std::fprintf(stderr, "XOCN_GPU_FAIL: %s\n", error.what());
        return 1;
    } catch (int error) {
        gpu_callbacks = test_gpu_backend;
        std::fprintf(stderr, "XOCN_GPU_FAIL: coefficient error %d\n", error);
        return 1;
    }
}

extern "C" int niyien_xocn_gpu_benchmark(const char* source, uint32_t frame_limit, uint32_t runs) {
    try {
        test_gpu_backend = gpu_callbacks;
        gpu_require(test_gpu_backend.decode != nullptr, "Compressed GPU callback missing");
        for (uint32_t run = 0; run < runs; ++run) for (bool hardware : {false, true}) {
            benchmark_calls = 0;
            gpu_callbacks = hardware ? GpuCallbacks{test_gpu_backend.create, test_gpu_backend.destroy, benchmark_gpu, benchmark_compressed} : GpuCallbacks{};
            const auto started = std::chrono::steady_clock::now();
            const auto elapsed = [&] { return std::chrono::duration<double, std::milli>(std::chrono::steady_clock::now() - started).count(); };
            Clip clip(source, true);
            clip.prepare_tone();
            const auto prepare_ms = elapsed();
            const auto frames = std::min<size_t>(frame_limit ? frame_limit : clip.frames.size(), clip.frames.size());
            double first_ms = 0;
            for (size_t i = 0; i < frames; ++i) {
                const auto frame = clip.video(i, 1080, hardware);
                gpu_require(bool(frame), "Benchmark preview missing");
                if (i == 0) first_ms = elapsed();
            }
            const auto total_ms = elapsed();
            gpu_require(!hardware || benchmark_calls == frames, "Benchmark used CPU fallback");
            std::printf("XOCN_GPU_BENCHMARK mode=%s run=%u frames=%zu prepare_ms=%.3f first_ms=%.3f total_ms=%.3f gpu_calls=%llu\n",
                hardware ? "GPU" : "CPU", run, frames, prepare_ms, first_ms, total_ms, static_cast<unsigned long long>(benchmark_calls));
            std::fflush(stdout);
        }
        gpu_callbacks = test_gpu_backend;
        std::puts("XOCN_GPU_BENCHMARK_PASS");
        return 0;
    } catch (const std::exception& error) {
        gpu_callbacks = test_gpu_backend;
        std::fprintf(stderr, "XOCN_GPU_BENCHMARK_FAIL: %s\n", error.what());
        return 1;
    }
}
