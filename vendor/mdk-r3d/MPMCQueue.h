#pragma once

#include <list>
#include <mutex>
#include <condition_variable>

template <typename T>
class MPMCQueue
{
public:
    template<typename U>
    void push(U&& u) {
        std::unique_lock lock(mtx_);
        q_.push_back(std::forward<U>(u));
        cv_.notify_one();
    }

    bool tryPop(T& t) {
        std::unique_lock lock(mtx_);
        if (q_.empty())
            return false;
        t = q_.front();
        q_.pop_front();
        return true;
    }


    bool pop(T& t) {
        std::unique_lock lock(mtx_);
        if (q_.empty())
            cv_.wait(lock);
        if (q_.empty())
            return false;
        t = q_.front();
        q_.pop_front();
        return true;
    }

    void notifyAll() {
        cv_.notify_all();
    }

    auto clear() {
        std::lock_guard lock(mtx_);
        auto n = q_.size();
        q_.clear();
        return n;
    }

    auto size() const {
        std::lock_guard lock(mtx_);
        return q_.size();
    }
private:
    std::list<T> q_;
    mutable std::mutex mtx_;
    std::condition_variable cv_;
};