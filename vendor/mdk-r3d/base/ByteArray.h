/*
 * Copyright (c) 2016-2023 WangBin <wbsecg1 at gmail.com>
 */
#pragma once
#include <cassert>
#include <cstddef> //ptrdiff_t
#include <cstdlib>
#include <cstring>
#include <memory>
#include "mdk/global.h"

// ByteArrayLiteral/Static
MDK_NS_BEGIN
class ByteArray
{
public:
    using value_type = uint8_t;
    using size_type = int;
    using pointer = value_type*;
    using const_pointer = const value_type*;
    using reference = value_type&;
    using const_reference = const value_type&;
    using difference_type = ptrdiff_t;
    using iterator = pointer;
    using const_iterator = const_pointer;
    // begin()....
    //enum { kAlignment = 64 };// // std::align? TODO: as parameter of ctor, 64 for video data, 1 for other

    template<typename C, class = std::enable_if<sizeof(C) == sizeof(value_type)>>
    static ByteArray wrap(const C* b, int n = -1) {
        assert(b);
        if (n <= 0)
            n = (int)strlen((const char*)b)+1;
        auto d = std::make_shared<Data>();
        d->is_owner = false;
        d->data = (pointer)b;
        d->size = n;
        ByteArray a(std::move(d));
        return a;
    }

    ByteArray() = default;
    // resize/reserve n bytes, data is not initialized
    ByteArray(int n, uint16_t alignSize = 64) : kAlignment(alignSize) {resize(n);}
    // resize/reserve n bytes, every byte is initialized to v
    ByteArray(int n, value_type v) { fill(v, n);}
    template<typename C, class = std::enable_if<sizeof(C) == sizeof(char)>>
    ByteArray(const C* b, int n = -1) {
        if (n <= 0)
            n = (int)strlen((const char*)b)+1;
        resize(n);
        memcpy(data(), b, size());
    }

    template<typename C, class = std::enable_if<sizeof(C) == sizeof(char)>>
    ByteArray& operator=(const C* s) {
        ByteArray tmp(s);
        std::swap(d, tmp.d);
        return *this;
    }

    value_type* data() { return d->data + d->offset;}
    const value_type* data() const { return d->data + d->offset;}
    const value_type* constData() const { return d->data + d->offset;}
    value_type& operator[](int pos) {
        assert(pos < size());
        return d->data[pos + d->offset];
    }
    const value_type& operator[](int pos) const {
        assert(pos < size());
        return d->data[pos + d->offset];
    }

    bool isEmpty() const { return size() <= 0;}
    bool empty() const { return size() <= 0;}
    explicit operator bool() const { return !isEmpty(); }

    int size() const { return d->size; }
    int capacity() const { return d->cap;}
    bool resize(int n) {
        if (n <= capacity()) {
            d->size = n;
            return true;
        }
        if (!d->is_owner)
            return false;
        if (!reserve(n)) {
            if (d->is_owner)
                d->size = 0;
            return false;
        }
        d->size = n; // set size later, old size is used in reserve()
        return true;
    }
    bool reserve(int n) {
        if (n <= capacity()) {
            d->cap = n;
            return true;
        }
        if (!d->is_owner) // not the owner
            return false;
        pointer old = d->data;
        pointer p = reinterpret_cast<value_type*>(std::realloc(old, n + kAlignment)); // old memery is unchanged, or freed on success. the content does not change
        if (!p) {
            d->data = nullptr;
            d->cap = 0;
            if (old)
                std::free(old);
            p = reinterpret_cast<value_type*>(std::malloc(n + kAlignment)); // TODO: c++17 aligned_alloc
            if (!p)
                return false;
            const uint8_t offset = kAlignment - uint8_t(intptr_t(p) & (kAlignment-1));
            memcpy(p + offset, old + d->offset, size()); // copy old data of size() bytes
            d->offset = offset;
        } else {
            d->offset = kAlignment - uint8_t(intptr_t(p) & (kAlignment-1));
        }
        d->cap = n;
        d->data = p;
        return true;
    }
    // resize to n before setting every byte to v
    void fill(value_type v, int n = -1) {
        resize(n < 0 ? size() : n);
        if (size() > 0)
            memset(data(), v, size());
    }
    // clear: reset
    void clear() { d = std::make_shared<Data>(); }
    int use_count() const {return d.use_count();}

    // try to deep copy. does nothing if no other instance managing current object(approximate)
    // can not ensure no instance is accessing later when writing the detached(not really) object
    void tryDetach() {
        if (use_count() > 1)
            detach();
    }
    void detach() {
        auto old = d;
        d = std::make_shared<Data>(); // owner
        d->size = old->size;
        d->cap = old->cap;
        d->data = reinterpret_cast<value_type*>(std::malloc(d->cap + kAlignment));
        d->offset = kAlignment - uint8_t(intptr_t(d->data) & (kAlignment-1));
        memcpy(data(), old->data + old->offset, d->size);
    }
private:
    struct Data {
        ~Data() {
            if (is_owner && data)
                std::free(data);
        }
        bool is_owner = true;
        pointer data = nullptr;
        uint8_t offset = 0;
        int size = 0;
        int cap = 0;
    };
    ByteArray(std::shared_ptr<Data>&& dt) : d(std::move(dt)) {}
    // why SharedDataPtr crash?
    std::shared_ptr<Data> d = std::make_shared<Data>(); // can not use shared_ptr<char> because no way to release the ownership(required by realloc)
    // TODO: ByteArray is not shared, but use shared_ptr<ByteArray>. so we can reduce use of shared_ptr, e.g. in AudioFrame/VideoFrame, whose pimp is shared(TODO: also no share, must explicitly use shared_ptr<Frame>)
    uint16_t kAlignment = 64;
};

inline bool operator==(const ByteArray &a1, const char *a2)
{ return a2 ? strcmp((const char*)a1.constData(),a2) == 0 : a1.isEmpty(); }
inline bool operator==(const char *a1, const ByteArray &a2)
{ return a1 ? strcmp(a1,(const char*)a2.constData()) == 0 : a2.isEmpty(); }
inline bool operator!=(const ByteArray &a1, const char *a2)
{ return a2 ? strcmp((const char*)a1.constData(),a2) != 0 : !a1.isEmpty(); }
inline bool operator!=(const char *a1, const ByteArray &a2)
{ return a1 ? strcmp(a1,(const char*)a2.constData()) != 0 : !a2.isEmpty(); }
MDK_NS_END
