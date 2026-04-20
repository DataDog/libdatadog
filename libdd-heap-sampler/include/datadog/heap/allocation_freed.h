#ifndef DD_SAMPLERS_ALLOCATION_FREED_H
#define DD_SAMPLERS_ALLOCATION_FREED_H

#include <stddef.h>

/*
 * Hook invoked by an allocator wrapper BEFORE performing a free.
 * Wraps free, operator delete (sized and unsized), sdallocx, etc.
 *
 * Checks whether the allocation at `ptr` was sampled and emits the
 * matching free-side USDT if so, then returns the size the caller
 * should pass to the underlying deallocator. This is usually the same
 * as `size`, but may be larger when dd_allocation_created reserved
 * extra bytes for inline sampling metadata. Callers with a sized-free
 * variant MUST pass the returned value verbatim; callers wrapping
 * plain free can ignore it.
 *
 * Args cover the superset of inputs any free-like call carries:
 *   ptr       - allocation being freed (the pointer returned by the
 *               matching allocation call)
 *   size      - size the caller knows about, or 0 if unknown (plain free)
 *   alignment - alignment used at allocation time, or 0
 */
size_t dd_allocation_freed(void *ptr, size_t size, size_t alignment)
    __attribute__((warn_unused_result));

#endif
