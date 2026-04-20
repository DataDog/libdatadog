#ifndef DD_SAMPLERS_ALLOCATION_REQUESTED_H
#define DD_SAMPLERS_ALLOCATION_REQUESTED_H

#include <stddef.h>
#include <stdint.h>

#include <datadog/heap/tl_state.h>

/*
 * Return type for dd_allocation_requested, paired with the `req`
 * argument of dd_allocation_created.
 *
 *   size    - size the wrapper MUST pass to its underlying allocator.
 *             Usually the caller's requested size; on sampled
 *             allocations on architectures that use in-band flagging
 *             (header magic), this is bumped by the flag overhead.
 *   weight  - 0 if this allocation was not sampled; otherwise the
 *             unbiased size estimator (nsamples * interval) for
 *             aggregated reporting.
 *
 * Kept to 16 bytes so it's returned in registers.
 */
typedef struct {
    size_t   size;
    uint64_t weight;
} dd_alloc_req_t;

/* Slow path, declared here so the inline fast path can dispatch. */
dd_alloc_req_t dd_allocation_requested_slow(dd_tl_state_t *tl, size_t size,
                                             size_t alignment);

/*
 * Pre-allocation hook. Call BEFORE invoking the underlying allocator.
 * This lets us decide if we want to sample the allocation or not, in
 * which case in some situations (e.g., x86-64) we may increase the
 * allocation size to store sampling metadata.
 * 
 * Something like this - note that this function returns a new allocation size to use
 * for the actual alloc request :
 * 
 *   dd_alloc_req_t req = dd_allocation_requested(size, alignment);
 *   void *raw = real_alloc(..., req.size); 
 *   void *user = dd_allocation_created(raw, req);
 *   return user;
 *
 * The reentry guard is opened here (on sampled path only) and will be closed
 * by the paired dd_allocation_created call. ALWAYS pair them, even on
 * allocator failure (pass raw=NULL).
 */
static inline __attribute__((always_inline))
dd_alloc_req_t dd_allocation_requested(size_t size, size_t alignment) {
    dd_alloc_req_t out = { size, 0 };

    // If we don't have the TLS, or we are in a nested call, do nothing and bail.
    // tell the compiler this branch is _unlikely_.
    dd_tl_state_t *tl = dd_tl_state_get();
    if (__builtin_expect(!tl || tl->reentry_guard, 0)) return out;

    // If we haven't crossed the sampling boundary, do nothing and bail.
    // Tell the compiler this branch is _likely_ - i.e. we're mostly going to not sample
    tl->remaining_bytes += (int64_t)size;
    if (__builtin_expect(tl->remaining_bytes < 0, 1)) return out;

    // Sampling path! Jump out to dd_allocation_requested_slow to do the necessary work.
    // We're intentionally putting this in a separate function call to avoid bloating
    // the i-cache of the fast path. At the point we have decided to sample, we are less
    // concerned about the cost of the function call.
    return dd_allocation_requested_slow(tl, size, alignment);
}

#endif
