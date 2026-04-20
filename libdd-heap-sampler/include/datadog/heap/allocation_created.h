#ifndef DD_SAMPLERS_ALLOCATION_CREATED_H
#define DD_SAMPLERS_ALLOCATION_CREATED_H

#include <datadog/heap/allocation_requested.h>   /* dd_alloc_req_t */

/* Slow path, declared here so the inline fast path can dispatch. */
void *dd_allocation_created_slow(void *raw, dd_alloc_req_t req);

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
static inline __attribute__((always_inline))
void *dd_allocation_created(void *raw, dd_alloc_req_t req) {
    if (__builtin_expect(req.weight == 0, 1)) return raw;
    return dd_allocation_created_slow(raw, req);
}

#endif
