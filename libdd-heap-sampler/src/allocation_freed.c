// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/allocation_freed.h>
#include <datadog/heap/probes.h>
#include <datadog/heap/sample_flag.h>

#include <stdint.h>

/*
 * Slow path for dd_allocation_freed. We only arrive here when
 * dd_sample_flag_check confirmed that ptr carries the sample flag,
 * meaning this allocation was previously sampled.
 *
 * Fires the ddheap:free USDT with the user-visible pointer, then returns
 * the raw pointer and adjusted size that the caller must forward to the
 * real deallocator. On x86-64 the size grows by DD_HEADER_BYTES to cover
 * the header that was reserved at allocation time; on arm64 the size is
 * unchanged since TBI tagging touches only pointer bits.
 */
dd_alloc_freed_t dd_allocation_freed_slow(void *ptr, void *raw, size_t size,
                                          size_t alignment) {
    /* Fire with the user-visible pointer, matching what was reported at alloc
     * time, so the profiler can correlate the two events by address. */
    dd_probe_free(ptr);

    dd_alloc_freed_t out = {
        /* Return the raw pointer so the caller passes the real allocation base
         * to the deallocator, not the user pointer that may be offset or tagged. */
        .ptr  = raw,
        .size = size,
    };

#if defined(__x86_64__)
    /* Recover the bumped size the allocator actually holds. This must
     * exactly mirror allocation_requested.c's bumped_alloc_size():
     *
     *   base    = max(alignment, DD_HEADER_BYTES)
     *   reserve = 2 * base
     *   bumped  = round_up(size + reserve, alignment)
     *
     * Do not use (user - raw) as the reserve. That offset is usually
     * only one `base`, while allocation reserved two so x86_apply() has
     * room for its optional page-boundary bump. Sized-free callers
     * (Rust GlobalAlloc::dealloc, sdallocx, operator delete(sz)) rely
     * on this being exact.
     *
     * When the caller doesn't know the alignment (alignment == 0),
     * fall back to size + offset. Plain free() ignores out.size so this
     * only matters for sized-free variants that must supply an alignment. */
    size_t offset = (size_t)((uintptr_t)ptr - (uintptr_t)raw);
    if (alignment == 0) {
        out.size = size + offset;
    } else {
        size_t base = alignment > DD_HEADER_BYTES ? alignment : DD_HEADER_BYTES;
        size_t reserve = base * 2;
        size_t bumped = size + reserve;
        if (alignment > 1) {
            size_t mask = alignment - 1;
            bumped = (bumped + mask) & ~mask;
        }
        out.size = bumped;
    }
#else
    (void)alignment;
#endif
    return out;
}