// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/allocation_requested.h>
#include <datadog/heap/sample_flag.h>

#include <errno.h>
#include <math.h>
#include <stdbool.h>
#include <stdint.h>
#include <time.h>

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
 * Called when remaining_bytes has crossed zero, meaning at least one sample
 * is owed. Draws fresh intervals until the counter is negative again, counting
 * how many samples fired. Returns nsamples * interval as the unbiased weight
 * estimator to attribute to this allocation. A sampling_interval of 0 is the
 * documented "do not sample this thread" value and returns 0 immediately.
 *
 * On the very first call for a thread, remaining_bytes_initialized is false
 * and we draw the initial interval from scratch. If that interval exceeds the
 * accumulated byte credit the counter goes back negative and we return 0,
 * meaning no sample this time. This is normal and happens at most once per thread.
 *
 * Note: remaining_bytes has already been incremented by `size` in the inline
 * fast path; we arrive here because that increment pushed it to zero or above.
 */
static uint64_t sample(dd_tl_state_t *tl) {
    uint64_t interval = tl->sampling_interval;
    if (interval == 0) return 0;

    if (!tl->remaining_bytes_initialized) {
        /* First allocation on this thread: draw the initial interval and
         * subtract it from the credit accumulated so far. If we're already
         * back in the red, skip the sample; the counter just wasn't large
         * enough to cover the first interval. */
        tl->remaining_bytes -= (int64_t)next_interval(&tl->rng, interval);
        tl->remaining_bytes_initialized = true;
        if (tl->remaining_bytes < 0) return 0;
    }

    /* remaining_bytes is >= 0, meaning we've crossed one full interval
     * boundary. Integer-divide to find how many full intervals fit in the
     * current credit (usually 1, but can be more for very large allocations),
     * then keep drawing until we're back in the red. Each iteration is one
     * additional sample. */
    size_t nsamples = (size_t)tl->remaining_bytes / interval;
    tl->remaining_bytes %= (int64_t)interval;
    do {
        tl->remaining_bytes -= (int64_t)next_interval(&tl->rng, interval);
        ++nsamples;
    } while (tl->remaining_bytes >= 0);

    /* Weight is the unbiased estimator: each sample represents `interval`
     * bytes on average, so nsamples * interval gives the expected total. */
    return (uint64_t)nsamples * interval;
}

/*
 * Slow path for dd_allocation_requested. We only arrive here when the fast
 * path counter has crossed zero. Sets the reentry guard, runs the sampling
 * decision, and returns the allocation request the caller should forward to
 * the real allocator.
 *
 * If sample() returns 0 (first-interval miss on a fresh thread) the guard is
 * closed here and a no-sample result is returned. Otherwise the guard stays
 * open until dd_allocation_created_slow closes it, keeping any allocations
 * triggered during the slow path from re-entering the sampler.
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

/*
 * Adaptive interval adjustment. Every DD_ADAPT_WINDOW samples, measure
 * elapsed time vs expected and nudge the interval proportionally.
 * P-controller with gain 0.5; converges without oscillation.
 */
#define DD_SAMPLING_INTERVAL_MAX (64u * 1024u * 1024u)

static void maybe_adapt(dd_tl_state_t *tl) {
    if (tl->ns_per_sample_target == 0) return;
    if (++tl->samples_since_adjust < DD_ADAPT_WINDOW) return;
    tl->samples_since_adjust = 0;

    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    uint64_t now = (uint64_t)ts.tv_sec * 1000000000ULL + (uint64_t)ts.tv_nsec;
    uint64_t elapsed = now - tl->last_ns;
    tl->last_ns = now;

    uint64_t expected = (uint64_t)DD_ADAPT_WINDOW * tl->ns_per_sample_target;
    if (expected == 0) return;

    double ratio = (double)elapsed / (double)expected;
    double adjusted = (double)tl->sampling_interval * (1.0 + 0.5 * (ratio - 1.0));

    if (adjusted < (double)DD_SAMPLING_INTERVAL_MIN)
        adjusted = (double)DD_SAMPLING_INTERVAL_MIN;
    if (adjusted > (double)DD_SAMPLING_INTERVAL_MAX)
        adjusted = (double)DD_SAMPLING_INTERVAL_MAX;

    tl->sampling_interval = (uint64_t)adjusted;
}

dd_alloc_req_t dd_allocation_requested_slow(dd_tl_state_t *tl, size_t size,
                                             size_t alignment) {
    /* Open the reentry guard before doing anything else. Any allocation that
     * happens between here and dd_allocation_created_slow (e.g. inside log()
     * or the USDT machinery) will see the guard set and pass through unsampled. */
    tl->reentry_guard = true;

    /* Save / restore errno: sample() reaches log(), which may set it. */
    int saved_errno = errno;
    uint64_t weight = sample(tl);
    errno = saved_errno;
    if (weight == 0) {
        /* First-interval miss: no sample this time. Close the guard now since
         * dd_allocation_created_slow won't be called on the sampled path. */
        tl->reentry_guard = false;
        dd_alloc_req_t out = { size, size, alignment, 0 };
        return out;
    }

    maybe_adapt(tl);

    size_t bumped;
    if (!bumped_alloc_size(size, alignment, &bumped)) {
        /* Alignment too large or arithmetic overflow: pass through as
         * an unsampled allocation rather than corrupt the request. The
         * guard must be closed here since dd_allocation_created_slow
         * won't be reached (weight == 0 fast-path in the header). */
        tl->reentry_guard = false;
        dd_alloc_req_t out = { size, size, alignment, 0 };
        return out;
    }

    dd_alloc_req_t out = {
        .size      = bumped,
        .user_size = size,
        .alignment = alignment,
        .weight    = weight,
    };
    return out;
}
