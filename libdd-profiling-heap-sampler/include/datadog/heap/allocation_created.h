/**
 * @file allocation_created.h
 *
 * Post-allocation hook for the ddheap sampler.
 *
 * Call dd_allocation_created() immediately after the underlying allocator
 * returns. On the common (unsampled) fast path this is a single branch on
 * req.weight and an immediate return of the raw pointer. On the sampled slow
 * path it applies the architecture-specific sample flag to the raw pointer,
 * emits the ddheap:alloc USDT, and closes the reentry guard that was opened
 * by the paired dd_allocation_requested() call.
 *
 * Always call this even when the allocator returns NULL: the reentry guard
 * must be closed regardless of whether the allocation succeeded.
 */
#ifndef DD_SAMPLERS_ALLOCATION_CREATED_H
#define DD_SAMPLERS_ALLOCATION_CREATED_H

#include <datadog/heap/allocation_requested.h>   /* dd_alloc_req_t */

/*
 * Slow path. As with allocation_requested, this is fired only when
 * we hit a sampled allocation, and is intentionally placed separately
 * from dd_allocation_created so that we don't bloat the instruction
 * cache for the fast path.
 */
void *dd_allocation_created_slow(void *raw, dd_alloc_req_t req)
    __attribute__((warn_unused_result));

/*
 * Post-allocation hook. Pair with dd_allocation_requested.
 *
 * Wrapper usage:
 *   dd_alloc_req_t req = dd_allocation_requested(size, alignment);
 *   void *raw = real_alloc(..., req.size);
 *   return dd_allocation_created(raw, req);
 *
 * Fast path (not sampled): returns raw unchanged.
 * Slow path (sampled): applies the architecture-specific flag, emits
 * the ddheap:alloc USDT, closes the reentry guard opened by the paired
 * requested() call, returns the user-visible pointer.
 *
 * Safe when raw == NULL (allocator failed): no USDT, guard still closed.
 */
static inline __attribute__((always_inline, warn_unused_result))
void *dd_allocation_created(void *raw, dd_alloc_req_t req) {
    if (__builtin_expect(!dd_alloc_req_is_sampled(req), 1)) return raw;
    return dd_allocation_created_slow(raw, req);
}

#endif
