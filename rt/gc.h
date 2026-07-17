// nova_gc.h — Simple mark-and-sweep garbage collector for Nova
// All @T references are managed by this GC.

#pragma once

#include <cstdint>
#include <cstddef>
#include <vector>
#include <unordered_set>
#include <functional>
#include <memory>
#include <stdexcept>
#include <iostream>

namespace nova {

// Forward declarations
class GC;
class GCObject;

// ─── GC Object base ──────────────────────────────────────────────────────────

class GCObject {
public:
    virtual ~GCObject() = default;
    
    // Mark this object and all objects it references
    virtual void gc_mark(GC& gc) = 0;
    
    // Size of this object in bytes (for collection threshold)
    virtual size_t gc_size() const { return sizeof(*this); }

    bool is_marked() const { return marked_; }
    void set_marked(bool m) { marked_ = m; }

private:
    bool marked_ = false;
};

// ─── GC pointer (smart pointer for @T) ──────────────────────────────────────

template<typename T>
class gc_ptr {
    static_assert(std::is_base_of_v<GCObject, T>, "gc_ptr requires GCObject base");

public:
    gc_ptr() : ptr_(nullptr) {}
    gc_ptr(std::nullptr_t) : ptr_(nullptr) {}
    
    explicit gc_ptr(T* ptr) : ptr_(ptr) {
        if (ptr_) GC::instance().register_ptr(this);
    }

    ~gc_ptr() {
        if (ptr_) GC::instance().unregister_ptr(this);
    }

    gc_ptr(const gc_ptr& other) : ptr_(other.ptr_) {
        if (ptr_) GC::instance().register_ptr(this);
    }

    gc_ptr(gc_ptr&& other) noexcept : ptr_(other.ptr_) {
        other.ptr_ = nullptr;
        if (ptr_) {
            GC::instance().unregister_ptr(&other);
            GC::instance().register_ptr(this);
        }
    }

    gc_ptr& operator=(const gc_ptr& other) {
        if (this != &other) {
            if (ptr_) GC::instance().unregister_ptr(this);
            ptr_ = other.ptr_;
            if (ptr_) GC::instance().register_ptr(this);
        }
        return *this;
    }

    gc_ptr& operator=(gc_ptr&& other) noexcept {
        if (this != &other) {
            if (ptr_) GC::instance().unregister_ptr(this);
            ptr_ = other.ptr_;
            other.ptr_ = nullptr;
            if (ptr_) GC::instance().register_ptr(this);
            GC::instance().unregister_ptr(&other);
        }
        return *this;
    }

    T* operator->() const { return ptr_; }
    T& operator*() const { return *ptr_; }
    T* get() const { return ptr_; }
    
    explicit operator bool() const { return ptr_ != nullptr; }
    bool operator==(std::nullptr_t) const { return ptr_ == nullptr; }
    bool operator!=(std::nullptr_t) const { return ptr_ != nullptr; }

private:
    template<typename U> friend class gc_ptr;
    T* ptr_;
};

// ─── GC class ────────────────────────────────────────────────────────────────

class GC {
public:
    static GC& instance() {
        static GC gc;
        return gc;
    }

    // Allocate a GC-managed object
    template<typename T, typename... Args>
    friend gc_ptr<T> gc_alloc(Args&&... args) {
        T* ptr = new T(std::forward<Args>(args)...);
        instance().allocated_.push_back(ptr);
        instance().bytes_allocated_ += ptr->gc_size();
        instance().maybe_collect();
        return gc_ptr<T>(ptr);
    }

    // Register a gc_ptr as a root
    void register_ptr(void* ptr_addr) {
        roots_.insert(ptr_addr);
    }

    void unregister_ptr(void* ptr_addr) {
        roots_.erase(ptr_addr);
    }

    // Force a collection
    void collect() {
        // Mark phase
        mark_roots();
        
        // Sweep phase
        sweep();
        
        bytes_allocated_ = 0;
        // Recalculate
        for (auto* obj : allocated_) {
            if (obj) bytes_allocated_ += 1; // approximate
        }
    }

    // Add a custom root (for global variables, etc.)
    // The function should mark all reachable objects via gc_mark
    void add_root(std::function<void(GC&)> root_fn) {
        custom_roots_.push_back(std::move(root_fn));
    }

    void mark_object(GCObject* obj) {
        if (!obj || obj->is_marked()) return;
        obj->set_marked(true);
        obj->gc_mark(*this);
    }

private:
    GC() = default;

    void maybe_collect() {
        if (bytes_allocated_ > 1024 * 1024) { // 1MB threshold
            collect();
        }
    }

    void mark_roots() {
        // Mark from registered gc_ptr roots
        // We scan the stack by looking at all registered pointer addresses
        // In a production system, this would scan the actual C++ stack
        for (auto* ptr_addr : roots_) {
            // ptr_addr points to a gc_ptr<T>, which contains a T*
            // We need to read the T* from it
            void** storage = static_cast<void**>(ptr_addr);
            GCObject* obj = static_cast<GCObject*>(*storage);
            if (obj) {
                mark_object(obj);
            }
        }

        // Mark from custom roots
        for (auto& root_fn : custom_roots_) {
            root_fn(*this);
        }
    }

    void sweep() {
        auto it = allocated_.begin();
        while (it != allocated_.end()) {
            if (!(*it)->is_marked()) {
                delete *it;
                it = allocated_.erase(it);
            } else {
                (*it)->set_marked(false); // Reset for next collection
                ++it;
            }
        }
    }

    std::vector<GCObject*> allocated_;
    std::unordered_set<void*> roots_;
    std::vector<std::function<void(GC&)>> custom_roots_;
    size_t bytes_allocated_ = 0;
};

// ─── Helper: alloc with constructor args ────────────────────────────────────

template<typename T, typename... Args>
gc_ptr<T> gc_alloc(Args&&... args) {
    return GC::gc_alloc<T>(std::forward<Args>(args)...);
}

// ─── Built-in GC-managed string ─────────────────────────────────────────────

class GCString : public GCObject {
public:
    GCString() = default;
    GCString(const std::string& s) : data_(s) {}
    GCString(std::string&& s) : data_(std::move(s)) {}

    void gc_mark(GC&) override { /* no references */ }
    size_t gc_size() const override { return data_.capacity(); }

    const std::string& str() const { return data_; }
    std::string& str() { return data_; }

private:
    std::string data_;
};

// ─── Built-in GC-managed array ──────────────────────────────────────────────

template<typename T>
class GCArray : public GCObject {
public:
    GCArray() = default;
    
    void push_back(const T& val) { data_.push_back(val); }
    T& operator[](size_t i) { return data_.at(i); }
    const T& operator[](size_t i) const { return data_.at(i); }
    size_t size() const { return data_.size(); }

    void gc_mark(GC& gc) override {
        // If T is a gc_ptr, mark each element
        if constexpr (std::is_base_of_v<GCObject, typename T::element_type>) {
            for (auto& elem : data_) {
                if (elem) gc.mark_object(elem.get());
            }
        }
    }
    size_t gc_size() const override { return data_.capacity() * sizeof(T); }

private:
    std::vector<T> data_;
};

} // namespace nova
