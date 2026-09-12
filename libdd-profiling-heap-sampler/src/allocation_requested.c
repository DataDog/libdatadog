// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/allocation_requested.h>
#include <datadog/heap/sample_flag.h>

#include <errno.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>

/*
 * Advances the Park-Miller LCG one step and returns the new 31-bit state.
 * Cheap, branch-free PRNG suitable for the sampling hot path.
 */
static uint32_t lcg_next(uint32_t *rng) {
    *rng = (uint32_t)(((uint64_t)(*rng) * 48271u) % 2147483647u);
    return *rng;
}

/*
 * Draws the next inter-sample gap in bytes from an exponential distribution
 * with the given mean. Clamped to [8, 20*mean] to avoid degenerate near-zero
 * gaps on one end and unbounded intervals on unlucky draws on the other.
 */
static uint64_t next_interval(uint32_t *rng, uint64_t mean) {
    double u = (double)lcg_next(rng) / 2147483647.0;
    if (u <= 0.0) u = 1e-10;  /* guard against log(0) = -inf */
    double v = -log(u) * (double)mean;
    double vmax = 20.0 * (double)mean;
    if (v > vmax) v = vmax;   /* cap runaway intervals on very lucky draws */
    if (v < 8.0)  v = 8.0;   /* floor keeps the counter moving forward */
    return (uint64_t)v;
}

/*
 * Called when remaining_bytes has crossed zero, meaning at least one sampling
 * point lies within this allocation. Draws fresh intervals until the counter
 * is negative again, advancing the point process through the entire allocation
 * so that remaining_bytes represents the distance to the next sample after
 * this allocation. Returns true if this allocation was sampled, false
 * otherwise.
 *
 * A sampling_interval of 0 is the documented "do not sample this thread"
 * value and returns false immediately.
 *
 * On the very first call for a thread, remaining_bytes_initialized is false
 * and we draw the initial interval from scratch. If that interval exceeds the
 * accumulated byte credit the counter goes back negative and we return false,
 * meaning no sample this time. This is normal and happens at most once per thread.
 *
 * Note: remaining_bytes has already been incremented by `size` in the inline
 * fast path; we arrive here because that increment pushed it to zero or above.
 */
static bool sample(dd_tl_state_t *tl) {
    uint64_t interval = tl->sampling_interval;
    if (interval == 0) return false;

    if (!tl->remaining_bytes_initialized) {
        /* First allocation on this thread: draw the initial interval and
         * subtract it from the credit accumulated so far. If we're already
         * back in the red, skip the sample; the counter just wasn't large
         * enough to cover the first interval. */
        tl->remaining_bytes -= (int64_t)next_interval(&tl->rng, interval);
        tl->remaining_bytes_initialized = true;
        if (tl->remaining_bytes < 0) return false;
    }

    /* remaining_bytes is >= 0, meaning we've crossed at least one full
     * interval boundary. Use integer division to skip over all the full
     * intervals that fit in the current credit (an optimization for very
     * large allocations), then keep drawing until we're back in the red.
     * This preserves the invariant that after processing an allocation,
     * remaining_bytes is exactly equivalent to advancing through every
     * sampling point individually. */
    tl->remaining_bytes %= (int64_t)interval;
    do {
        tl->remaining_bytes -= (int64_t)next_interval(&tl->rng, interval);
    } while (tl->remaining_bytes >= 0);

    return true;
}

/*
 * Computes the per-allocation unbiased byte weight for a sampled allocation.
 *
 * For Poisson/random-interval sampling with mean interval R, the probability
 * that an allocation of size S is sampled is:
 *
 *     p = 1 - exp(-S / R)
 *
 * The unbiased estimator for total allocated bytes represented by this sample
 * is:
 *
 *     weight = S / p
 *
 * Properties:
 *   - Small allocations (S << R): weight ~= R (one sampling interval)
 *   - Large allocations (S >> R): weight ~= S (allocation is almost certain
 *     to be sampled, so it represents only itself)
 *   - S == R: weight ~= 1.582 * R
 *
 * Uses expm1 for numerical stability when S is small relative to R.
 */
static uint64_t allocation_weight(uint64_t size, uint64_t interval) {
    if (size == 0 || interval == 0) {
        return 0;
    }

    double p = -expm1(-(double)size / (double)interval);
    double w = (double)size / p;

    /* Clamp to UINT64_MAX on overflow; round to nearest integer. */
    if (w >= (double)UINT64_MAX) {
        return UINT64_MAX;
    }

    return (uint64_t)(w + 0.5);
}

/*
 * Slow path for dd_allocation_requested. We only arrive here when the fast
 * path counter has crossed zero. Sets the reentry guard, runs the sampling
 * decision, and returns the allocation request the caller should forward to
 * the real allocator.
 *
 * If sample() returns false (first-interval miss on a fresh thread, or
 * interval == 0) the guard is closed here and a no-sample result is returned.
 * Otherwise the guard stays open until dd_allocation_created_slow closes it,
 * keeping any allocations triggered during the slow path from re-entering
 * the sampler.
 *
 * (ddprof: AllocationTracker::track_allocation / next_sample_interval)
 */
/*
 * Compute the bumped size to pass to the underlying allocator for a
 * sampled allocation. Returns true on success and writes the bumped
 * size to *out_size. Returns false when the arithmetic would overflow
 * or the alignment exceeds what the sampler is willing to honor, in
 * which case the caller must pass this allocation through unsampled.
 *
 * x86-64 places a 16-byte (magic, offset) header immediately before
 * the user pointer, and picks user = raw + max(alignment, 16) (plus
 * possibly another `alignment` bump to satisfy the page-boundary
 * invariant). The bumped size must reserve room for that offset plus
 * the user's requested bytes, and must satisfy aligned_alloc's
 * size %% alignment == 0 constraint (a superset of posix_memalign's
 * requirements).
 *
 * arm64 uses TBI tagging with no size bump.
 */
static bool bumped_alloc_size(size_t user_size, size_t alignment,
                              size_t *out_size) {
#if defined(__x86_64__) && DD_HEAP_LIVE_TRACKING
    /* Shared with dd_allocation_freed_slow via x86_bumped_size so the
     * alloc and free sides can never disagree on the formula. */
    return x86_bumped_size(user_size, alignment, out_size);
#else
    (void)alignment;
    *out_size = user_size;
    return true;
#endif
}

dd_alloc_req_t dd_allocation_requested_slow(dd_tl_state_t *tl, size_t size,
                                             size_t alignment) {
    /* Open the reentry guard before doing anything else. Any allocation that
     * happens between here and dd_allocation_created_slow (e.g. inside log()
     * or the USDT machinery) will see the guard set and pass through unsampled. */
    tl->reentry_guard = true;

    /* Save / restore errno: sample() reaches log(), which may set it. */
    int saved_errno = errno;
    bool sampled = sample(tl);
    errno = saved_errno;
    if (!sampled) {
        /* First-interval miss: no sample this time. Close the guard now since
         * dd_allocation_created_slow won't be called on the sampled path. */
        tl->reentry_guard = false;
        dd_alloc_req_t out = { size, size, alignment, 0 };
        return out;
    }

    /* Compute the per-allocation unbiased weighted bytes from the
     * application-requested size and the sampling interval.
     *
     * Zero-size allocations: allocation_weight(0, interval) returns 0,
     * and weighted_bytes==0 means "unsampled" to downstream code. Treat
     * them as unsampled to avoid leaving the reentry guard open (since
     * dd_allocation_created short-circuits when weighted_bytes==0 and
     * would never close it). */
    uint64_t weighted_bytes = allocation_weight((uint64_t)size, tl->sampling_interval);
    if (weighted_bytes == 0) {
        tl->reentry_guard = false;
        dd_alloc_req_t out = { size, size, alignment, 0 };
        return out;
    }

    size_t bumped;
    if (!bumped_alloc_size(size, alignment, &bumped)) {
        /* Alignment too large or arithmetic overflow: pass through as
         * an unsampled allocation rather than corrupt the request. The
         * guard must be closed here since dd_allocation_created_slow
         * won't be reached (weighted_bytes == 0 fast-path in the header). */
        tl->reentry_guard = false;
        dd_alloc_req_t out = { size, size, alignment, 0 };
        return out;
    }

    dd_alloc_req_t out = {
        .size      = bumped,
        .user_size = size,
        .alignment = alignment,
        .weighted_bytes = weighted_bytes,
    };
    return out;
}

/*
 * Test-only: expose the allocation_weight helper so unit tests can exercise
 * the weight calculation deterministically without depending on random
 * sampling. Not part of the public API.
 */
uint64_t dd_test_allocation_weight(uint64_t size, uint64_t interval) {
    return allocation_weight(size, interval);
}
