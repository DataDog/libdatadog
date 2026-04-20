#include <datadog/heap/allocation_requested.h>
#include <datadog/heap/sample_flag.h>

#include <math.h>

/*
 * Advances the Park-Miller LCG one step and returns the new 31-bit state.
 * Cheap, branch-free PRNG for the sampling hot path. This is lifted from
 * ddprof's usage of std::minstd_rand
 */
static uint32_t lcg_next(uint32_t *rng) {
    *rng = (uint32_t)(((uint64_t)(*rng) * 48271u) % 2147483647u);
    return *rng;
}

/*
 * Draws the next inter-sample gap (bytes until we should sample again) from
 * an exponential distribution with the given mean, clamped to [8, 20*mean].
 *
 * Equivalent of AllocationTracker::next_sample_interval in ddprof
 */
static uint64_t next_interval(uint32_t *rng, uint64_t mean) {
    double u = (double)lcg_next(rng) / 2147483647.0;
    if (u <= 0.0) u = 1e-10;
    double v = -log(u) * (double)mean;
    double vmax = 20.0 * (double)mean;
    if (v > vmax) v = vmax;
    if (v < 8.0)  v = 8.0;
    return (uint64_t)v;
}

/*
 * Consumes the byte credit that just crossed zero, drawing fresh intervals
 * until the counter is negative again, and returns the unbiased weight
 * (nsamples * interval) to attribute to this allocation.
 *
 * Equivalent of AllocationTracker::track_allocation in ddprof

 * tl->remaining_bytes has already been incremented by `size` in the
 * inline fast path; we're here because it crossed zero.
 */
static uint64_t sample(dd_tl_state_t *tl) {
    uint64_t interval = tl->sampling_interval;

    if (!tl->remaining_bytes_initialized) {
        tl->remaining_bytes -= (int64_t)next_interval(&tl->rng, interval);
        tl->remaining_bytes_initialized = true;
        if (tl->remaining_bytes < 0) return 0;
    }

    size_t nsamples = (size_t)tl->remaining_bytes / interval;
    tl->remaining_bytes %= (int64_t)interval;
    do {
        tl->remaining_bytes -= (int64_t)next_interval(&tl->rng, interval);
        ++nsamples;
    } while (tl->remaining_bytes >= 0);

    return (uint64_t)nsamples * interval;
}

// Called from the fast path when we expect to sample.
dd_alloc_req_t dd_allocation_requested_slow(dd_tl_state_t *tl, size_t size,
                                             size_t alignment) {
    (void)alignment;

    tl->reentry_guard = true;

    uint64_t weight = sample(tl);
    if (weight == 0) {
        // Fast path crossed zero, but this was our first interval draw on the
        // thread and it exceeded our accumulated credit — no sample this time.
        // Close the guard and return a no-sample. This happens at most once per thread.
        tl->reentry_guard = false;
        dd_alloc_req_t out = { size, 0 };
        return out;
    }

    dd_alloc_req_t out = {
        .size   = size + dd_sample_flag_overhead(),
        .weight = weight,
    };
    return out;
}
