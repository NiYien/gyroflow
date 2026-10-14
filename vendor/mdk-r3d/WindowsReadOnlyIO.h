// SPDX-License-Identifier: MIT
#pragma once

#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include "R3DSDKCustomIO.h"
#include <cstdint>
#include <cstring>
#include <algorithm>
#include <deque>
#include <memory>
#include <mutex>
#include <string>

// Let Windows cache immutable source bytes across decoder reads. The SDK still
// handles sidecars, writes, active recordings and files that cannot be mapped.
class WindowsReadOnlyIO final : public R3DSDK::IOInterface {
    struct View {
        const unsigned char* bytes;
        uint64_t offset;
        size_t size;
        ~View() { UnmapViewOfFile(bytes); }
    };
    struct File {
        HANDLE handle;
        HANDLE mapping;
        uint64_t size;
        size_t window;
        std::mutex mutex;
        std::deque<std::shared_ptr<View>> views;

        File(HANDLE handle, HANDLE mapping, uint64_t size, size_t window)
            : handle(handle), mapping(mapping), size(size), window(window) {}

        std::shared_ptr<View> viewFor(uint64_t offset) {
            const std::lock_guard lock(mutex);
            for (auto it = views.begin(); it != views.end(); ++it) {
                const auto& view = *it;
                if (offset >= view->offset && offset - view->offset < view->size) {
                    auto found = view;
                    views.erase(it);
                    views.push_front(found);
                    return found;
                }
            }
            const auto start = offset / window * window;
            const auto length = size_t(std::min<uint64_t>(size - start, window));
            auto mapped = static_cast<const unsigned char*>(MapViewOfFile(mapping, FILE_MAP_READ,
                DWORD(start >> 32), DWORD(start), size_t(length)));
            if (!mapped) return {};
            auto view = std::shared_ptr<View>(new View{mapped, start, size_t(length)});
            // Keep only a small working set mapped. Unmapped source pages remain
            // available to the Windows cache and can be reclaimed under pressure.
            views.push_front(view);
            while (views.size() > 3) views.pop_back();
            return view;
        }
    };

    static bool copyBytes(void* output, const void* input, size_t bytes) {
        // A mapped read can fail if a drive disappears or an I/O error occurs.
        __try {
            std::memcpy(output, input, bytes);
            return true;
        } __except (EXCEPTION_EXECUTE_HANDLER) {
            return false;
        }
    }

public:
    Handle Open(const char* path, FileAccess access) override {
        const auto extension = std::strrchr(path, '.');
        if (access != IO_READ || !extension
            || (_stricmp(extension, ".R3D") && _stricmp(extension, ".NEV")))
            return HANDLE_FALLBACK;
        const auto count = MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, nullptr, 0);
        if (!count) return HANDLE_FALLBACK;
        std::wstring wide(count, L'\0');
        if (!MultiByteToWideChar(CP_UTF8, MB_ERR_INVALID_CHARS, path, -1, wide.data(), count))
            return HANDLE_FALLBACK;
        // Denying writes keeps the mapping's size and contents stable. A file
        // already open for recording falls back to the SDK's normal reader.
        auto handle = CreateFileW(wide.c_str(), GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_DELETE,
                                  nullptr, OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL, nullptr);
        if (handle == INVALID_HANDLE_VALUE) return HANDLE_FALLBACK;
        LARGE_INTEGER size;
        if (!GetFileSizeEx(handle, &size) || size.QuadPart <= 0) {
            CloseHandle(handle);
            return HANDLE_FALLBACK;
        }
        auto mapping = CreateFileMappingW(handle, nullptr, PAGE_READONLY, 0, 0, nullptr);
        if (!mapping) {
            CloseHandle(handle);
            return HANDLE_FALLBACK;
        }
        SYSTEM_INFO system;
        GetSystemInfo(&system);
        const size_t granularity = system.dwAllocationGranularity;
        const size_t window = (64 * 1024 * 1024 + granularity - 1) / granularity * granularity;
        auto file = new File(handle, mapping, uint64_t(size.QuadPart), window);
        if (!file->viewFor(0)) {
            Close(file);
            return HANDLE_FALLBACK;
        }
        return file;
    }

    unsigned long long Filesize(Handle handle) override {
        return static_cast<File*>(handle)->size;
    }

    void Close(Handle handle) override {
        auto file = static_cast<File*>(handle);
        file->views.clear();
        CloseHandle(file->mapping);
        CloseHandle(file->handle);
        delete file;
    }

    bool Read(void* output, size_t bytes, unsigned long long offset, Handle handle) override {
        const auto file = static_cast<File*>(handle);
        if (offset > file->size || bytes > file->size - offset) return false;
        if (!bytes) return true;
        // Hold a view until this read finishes, even if another decoder read
        // evicts it from the cache concurrently.
        auto destination = static_cast<unsigned char*>(output);
        while (bytes) {
            const auto view = file->viewFor(offset);
            if (!view) return false;
            const auto within = size_t(offset - view->offset);
            const auto length = std::min(bytes, view->size - within);
            if (!copyBytes(destination, view->bytes + within, length)) return false;
            destination += length;
            offset += length;
            bytes -= length;
        }
        return true;
    }

    bool Write(const void*, size_t, Handle) override { return false; }
    bool CreatePath(const char*) override { return false; }
};
