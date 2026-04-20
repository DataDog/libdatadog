#ifndef DD_SAMPLERS_ALLOCATION_FREED_H
#define DD_SAMPLERS_ALLOCATION_FREED_H

#include <stddef.h>

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
 *   ptr       - allocation being freed (user pointer returned by alloc)
 *   size      - size the caller knows about, or 0 if unknown (plain free)
 *   alignment - alignment used at allocation time, or 0
 */
dd_alloc_freed_t dd_allocation_freed(void *ptr, size_t size, size_t alignment)
    __attribute__((warn_unused_result));

#endif
