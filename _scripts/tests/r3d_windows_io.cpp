// SPDX-License-Identifier: GPL-3.0-or-later
// Run with an existing scratch directory; no user media is modified.
#include "R3DSDK.h"
#include "WindowsReadOnlyIO.h"
#include <cstdio>
#include <stdexcept>
#include <atomic>
#include <thread>
#include <vector>

static void require(bool condition, const char* message) {
    if (!condition) throw std::runtime_error(message);
}

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) return 2;
    const auto path = std::wstring(argv[1]) + L"/\u89c6\u9891-io-" + std::to_wstring(GetCurrentProcessId()) + L".R3D";
    const auto count = WideCharToMultiByte(CP_UTF8, 0, path.c_str(), -1, nullptr, 0, nullptr, nullptr);
    std::string utf8(count, '\0');
    WideCharToMultiByte(CP_UTF8, 0, path.c_str(), -1, utf8.data(), count, nullptr, nullptr);
    HANDLE writer = INVALID_HANDLE_VALUE;
    R3DSDK::IOInterface::Handle source = R3DSDK::IOInterface::HANDLE_FALLBACK;
    if (R3DSDK::InitializeSdk(".", OPTION_RED_NONE) != R3DSDK::ISInitializeOK) return 3;
    WindowsReadOnlyIO io;
    int result = 1;
    bool created = false;
    try {
        // Span more views than the cache retains, including boundary reads.
        constexpr size_t window = 64 * 1024 * 1024;
        std::vector<unsigned char> expected(4 * window + 1024 * 1024);
        for (size_t i = 0; i < expected.size(); ++i) expected[i] = static_cast<unsigned char>(i * 73 + i / 251);
        writer = CreateFileW(path.c_str(), GENERIC_READ | GENERIC_WRITE,
                             FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                             nullptr, CREATE_NEW, FILE_ATTRIBUTE_NORMAL, nullptr);
        require(writer != INVALID_HANDLE_VALUE, "Cannot create scratch source");
        created = true;
        DWORD written = 0;
        require(WriteFile(writer, expected.data(), DWORD(expected.size()), &written, nullptr)
                && written == expected.size(), "Cannot write scratch source");
        require(io.Open(utf8.c_str(), io.IO_READ) == io.HANDLE_FALLBACK,
                "An active writer must keep the SDK's normal I/O");
        CloseHandle(writer);
        writer = INVALID_HANDLE_VALUE;
        source = io.Open(utf8.c_str(), io.IO_READ);
        require(source != io.HANDLE_ERROR && source != io.HANDLE_FALLBACK, "Cannot map Unicode source");
        require(io.Filesize(source) == expected.size(), "Incorrect source size");
        std::vector<unsigned char> actual(98307);
        require(io.Read(actual.data(), actual.size(), 17, source), "Unaligned read failed");
        require(std::memcmp(actual.data(), expected.data() + 17, actual.size()) == 0, "Source bytes changed");
        std::atomic<bool> concurrentValid{true};
        auto readWindows = [&](size_t first) {
            const size_t offsets[] = {17, window - 20, window + 13, 2 * window + 19, 3 * window + 23, 4 * window + 11};
            std::vector<unsigned char> bytes(actual.size());
            for (size_t i = 0; i < 48; ++i) {
                const auto offset = offsets[(i + first) % 6];
                if (!io.Read(bytes.data(), bytes.size(), offset, source)
                    || std::memcmp(bytes.data(), expected.data() + offset, bytes.size()))
                    concurrentValid = false;
            }
        };
        std::thread first(readWindows, 0), second(readWindows, 3);
        first.join();
        second.join();
        require(concurrentValid, "Concurrent reads changed during view eviction");
        require(io.Read(nullptr, 0, expected.size(), source), "Empty EOF read failed");
        require(!io.Read(actual.data(), 1, expected.size(), source), "Read beyond EOF was accepted");
        require(!io.Read(actual.data(), 16, UINT64_MAX, source), "Overflowing read was accepted");
        require(io.Open(utf8.c_str(), io.IO_WRITE) == io.HANDLE_FALLBACK, "Writes must stay with the SDK");
        require(io.Open("sidecar.RMD", io.IO_READ) == io.HANDLE_FALLBACK, "Sidecars must stay with the SDK");
        io.Close(source);
        source = io.HANDLE_FALLBACK;
        std::puts("WINDOWS_R3D_IO_PASS: Unicode, exact bytes, concurrent view eviction, bounds, active writers and SDK fallback");
        result = 0;
    } catch (const std::exception& error) {
        std::fprintf(stderr, "%s\n", error.what());
    }
    if (source != io.HANDLE_ERROR && source != io.HANDLE_FALLBACK) io.Close(source);
    if (writer != INVALID_HANDLE_VALUE) CloseHandle(writer);
    if (created) DeleteFileW(path.c_str());
    return result;
}
