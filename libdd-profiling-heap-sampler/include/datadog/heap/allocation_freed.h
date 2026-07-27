/**
 * @file allocation_freed.h
 *
 * Pre-free hook for the ddheap sampler.
 *
 * Call dd_allocation_freed() before forwarding to the underlying deallocator.
 * On the fast path it checks whether the pointer carries the architecture-
 * specific sample flag (a top-byte tag on arm64, a magic header word on
 * x86-64); unsampled pointers return immediately with their inputs unchanged.
 * On the slow path it emits the ddheap:free USDT and returns the raw pointer
 * and adjusted size that the caller must pass to the real free.
 *
 * The caller must use the ptr and size from the returned dd_alloc_freed_t
 * rather than the originals when invoking the underlying deallocator, since
 * sampled allocations on x86-64 have a 16-byte header that must be included
 * in the free.
 */
#ifndef DD_SAMPLERS_ALLOCATION_FREED_H
#define DD_SAMPLERS_ALLOCATION_FREED_H

#include <stddef.h>

#include <datadog/heap/sample_flag.h>

/*
 * Return type for dd_allocation_freed. Mirrors dd_alloc_req_t on the
 * allocation side.
 *
 *   ptr  - pointer the caller MUST pass to the underlying deallocator.
 *          Equals the input for unsampled allocations; equals the raw
 *          pointer (user - sample_flag_overhead) for sampled ones on
 *          architectures that use inline flag headers.
 *   size - size the caller MUST pass to a sized-free variant; equals
 *          the input for unsampled allocations; may be larger for
 *          sampled ones that reserved header bytes at alloc time.
 */
typedef struct {
    void  *ptr;
    size_t size;
} dd_alloc_freed_t;

/* Slow path for sampled frees. */
dd_alloc_freed_t dd_allocation_freed_slow(void *ptr, void *raw, size_t size,
                                          size_t alignment)
    __attribute__((warn_unused_result));

/*
 * Hook invoked by an allocator wrapper BEFORE performing a free.
 * Wraps free, operator delete (sized and unsized), sdallocx, etc.
 *
 * Checks whether the allocation at `ptr` was previously sampled and
 * emits the matching `ddheap:free` USDT if so, then returns the
 * (ptr, size) the caller must forward to the underlying deallocator
 * verbatim.
 *
 * Args cover the superset of inputs any free-like call carries:
 *   ptr       - allocation being freed (user pointer returned by alloc),
 *               or NULL (free(NULL) is a no-op; returned unchanged)
 *   size      - size the caller knows about, or 0 if unknown (plain free)
 *   alignment - alignment used at allocation time. Pass 0 if unknown (e.g.
 *               plain free): the returned size then falls back to a best-
 *               effort value, which is fine because plain free ignores it.
 *               Sized-free callers (Rust GlobalAlloc::dealloc, sdallocx,
 *               sized operator delete) must pass the real alignment so the
 *               returned size exactly matches the bumped allocation.
 */
static inline __attribute__((always_inline))
dd_alloc_freed_t dd_allocation_freed(void *ptr, size_t size, size_t alignment) {
#if DD_HEAP_LIVE_TRACKING
    void *raw;
    if (__builtin_expect(dd_sample_flag_check_and_clear(ptr, &raw), 0)) {
        return dd_allocation_freed_slow(ptr, raw, size, alignment);
    }
#else
    /* Live-heap tracking off: nothing is flagged, so there is never a
     * sampled free to recover. Pass the free straight through. */
    (void)alignment;
#endif

    dd_alloc_freed_t out = { .ptr = ptr, .size = size };
    return out;
}

#endif
