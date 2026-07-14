// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/allocation_created.h>
#include <datadog/heap/probes.h>
#include <datadog/heap/sample_flag.h>
#include <datadog/heap/tl_state.h>

#include <assert.h>

/*
 * Slow path for dd_allocation_created. We only arrive here when the paired
 * dd_allocation_requested_slow decided to sample (req.weight > 0).
 *
 * Applies the architecture-specific sample flag to raw (tagging the pointer
 * on arm64, writing a header magic word on x86-64) to produce the
 * user-visible pointer, then fires the ddheap:alloc USDT so an attached
 * profiler can record the sample. Finally closes the reentry guard that
 * dd_allocation_requested_slow opened.
 *
 * raw may be NULL if the underlying allocator failed; in that case we skip
 * the flag and USDT but still close the guard.
 *
 * TODO: on x86-64, consider abandoning the sample when raw falls within
 * DD_HEADER_BYTES of a page boundary. The free-side fast path already bails
 * in that case (to avoid reading before the page), so those allocations will
 * never emit a ddheap:free. Dropping them at alloc time would keep the
 * alloc/free pair balanced at the cost of occasionally missing a sample.
 *
 * (ddprof: AllocTrackerHelper::track / AllocationTracker push_alloc_sample)
 */
void *dd_allocation_created_slow(void *raw, dd_alloc_req_t req) {
    void *user = raw;
    if (raw != NULL) {
#if DD_HEAP_LIVE_TRACKING
        /* Apply the sample flag and fire the USDT. We use the user pointer
         * (post-flag) as the USDT argument so the profiler sees the same
         * address the application will. */
        user = dd_sample_flag_apply(raw, req.alignment);
#else
        /* Live-heap tracking off: no flagging, so the user pointer is the
         * raw pointer. The alloc USDT still fires for allocation profiling;
         * there just won't be a matching free. */
#endif
        /* Report the application-requested size, not the sampler-bumped
         * size (`req.size`), so heap-size distributions in the profiler
         * aren't skewed by per-sample overhead. */
        dd_probe_alloc(user, (uint64_t)req.user_size, req.weight);
    }

    /* Always close the reentry guard, even on allocation failure (raw == NULL),
     * so the thread isn't permanently locked out of sampling.
     *
     * In the real gotter/allocator flow, created is always paired with a
     * preceding requested that initialised this thread's TLS and opened the
     * guard, so if this thread has sampler state the guard must be open. A
     * NULL tl means the caller never went through requested (e.g. an isolated
     * unit-test call), which is allowed and simply has nothing to close. */
    dd_tl_state_t *tl = dd_tl_state_get();
    assert(!tl || tl->reentry_guard);
    if (tl) tl->reentry_guard = false;

    return user;
}
