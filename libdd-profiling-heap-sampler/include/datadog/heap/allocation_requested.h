/**
 * @file allocation_requested.h
 *
 * Pre-allocation hook for the ddheap sampler.
 *
 * Call dd_allocation_requested() immediately before invoking the underlying
 * allocator. It runs the Poisson sampling decision for this allocation:
 * most calls return quickly on the fast path (a counter decrement and a
 * branch-not-taken); only allocations that cross the sampling boundary pay
 * the cost of drawing a fresh inter-sample interval and setting the reentry
 * guard.
 *
 * The returned dd_alloc_req_t carries the values the caller must forward:
 *   - size:      the number of bytes to request from the real allocator
 *                (may be larger than the original on architectures that
 *                store the sample flag in a 16-byte header before the user
 *                pointer).
 *   - user_size: the original application-requested size, reported to
 *                the profiler via the ddheap:alloc USDT.
 *   - alignment: passed through so dd_allocation_created can place the
 *                user pointer correctly relative to the raw pointer.
 *   - weight:    0 if not sampled; otherwise the unbiased size
 *                estimator (nsamples * interval) to attribute to this
 *                allocation.
 *
 * Always pair with dd_allocation_created(), even if the allocator fails.
 */
#ifndef DD_SAMPLERS_ALLOCATION_REQUESTED_H
#define DD_SAMPLERS_ALLOCATION_REQUESTED_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#include <datadog/heap/tl_state.h>

/*
 * Return type for dd_allocation_requested, paired with the `req`
 * argument of dd_allocation_created.
 *
 *   size      - size the wrapper MUST pass to its underlying allocator.
 *               Usually the caller's requested size; on sampled
 *               allocations on architectures that use in-band flagging
 *               (header magic), this is bumped for the header and any
 *               alignment slack.
 *   user_size - the size the application originally asked for. This is
 *               the value that gets reported to the profiler via the
 *               ddheap:alloc USDT, so that heap-size distributions are
 *               not skewed by the sampler's per-allocation overhead.
 *   alignment - alignment the wrapper MUST pass to its underlying
 *               allocator. Equals the alignment the caller passed to
 *               dd_allocation_requested; carried through so
 *               dd_allocation_created can place the user pointer
 *               correctly relative to the raw pointer.
 *   weight    - 0 if this allocation was not sampled; otherwise the
 *               unbiased size estimator (nsamples * interval) for
 *               aggregated reporting.
 *
 * 32 bytes on 64-bit targets, cache-line-friendly.
 */
typedef struct {
    size_t   size;
    size_t   user_size;
    size_t   alignment;
    uint64_t weight;
} dd_alloc_req_t;

/*
 * True if this request was sampled (weight > 0). Both the C fast path
 * (allocation_created.h) and callers that branch on sampled-ness (e.g.
 * gotter's calloc hook) should use this instead of comparing `weight == 0`
 * directly, so there is one named predicate rather than the same
 * comparison repeated in C and Rust.
 */
static inline __attribute__((always_inline))
bool dd_alloc_req_is_sampled(dd_alloc_req_t req) {
    return req.weight != 0;
}

/* Slow path for an allocation request. This is only taken when we think we
 * need to sample, and is declared as a separate function to avoid bloating
 * the instruction cache of the fast path
 */
dd_alloc_req_t dd_allocation_requested_slow(dd_tl_state_t *tl, size_t size,
                                             size_t alignment)
    __attribute__((warn_unused_result));

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
static inline __attribute__((always_inline, warn_unused_result))
dd_alloc_req_t dd_allocation_requested(size_t size, size_t alignment) {
    dd_alloc_req_t out = { size, size, alignment, 0 };

    // If we don't have TLS yet (this is defensive and should never happen), or
    // the reentry guard is set (meaning a sampled allocation is already in
    // flight on this thread and something in its slow path triggered another
    // allocation), pass through without sampling.
    // Either condition is rare on a hot path, so mark the branch unlikely.
    dd_tl_state_t *tl = dd_tl_state_get_or_init();
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
