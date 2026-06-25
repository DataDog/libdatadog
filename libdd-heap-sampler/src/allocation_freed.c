// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/allocation_freed.h>
#include <datadog/heap/probes.h>
#include <datadog/heap/sample_flag.h>

/*
 * Slow path for dd_allocation_freed. We only arrive here when
 * dd_sample_flag_check_fast confirmed that ptr carries the sample flag,
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
    (void)alignment;

    /* Fire with the user-visible pointer, matching what was reported at alloc
     * time, so the profiler can correlate the two events by address. */
    dd_probe_free(ptr);
    dd_alloc_freed_t out = {
        /* Return the raw pointer so the caller passes the real allocation base
         * to the deallocator, not the user pointer that may be offset or tagged. */
        .ptr  = raw,
        /* Recover the full allocation size including any header reserved at
         * alloc time. On arm64 dd_sample_flag_overhead() is 0, so this is
         * a no-op there. */
        .size = size + dd_sample_flag_overhead(),
    };
    return out;
}