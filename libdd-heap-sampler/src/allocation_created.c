#include <datadog/heap/allocation_created.h>
#include <datadog/heap/probes.h>
#include <datadog/heap/sample_flag.h>
#include <datadog/heap/tl_state.h>

/*
 * Equivalent of the sampled branch of AllocTrackerHelper::track +
 * AllocationTracker::track_allocation's USDT/push_alloc_sample tail
 * (ddprof: src/lib/symbol_overrides.cc, src/lib/allocation_tracker.cc).
 */
void *dd_allocation_created_slow(void *raw, dd_alloc_req_t req) {
    void *user = raw;
    if (raw != NULL) {
        /* TODO: consider abandoning the sample here when
         * `raw + DD_HEADER_BYTES` would land in the first
         * DD_HEADER_BYTES bytes of a page. This would let us
         * use "isn't within DD_HEADER_BYTES of a page boundary" as a
         * precondition for checking the magic bytes on free ... at the expense
         * of occasionally dropping samples. Need to think this through!
         **/
        user = dd_sample_flag_apply(raw);
        dd_probe_alloc(user, (uint64_t)req.size, req.weight);
    }

    /* Close the reentry guard opened by the paired requested() call. */
    dd_tl_state_t *tl = dd_tl_state_get();
    if (tl) tl->reentry_guard = false;

    return user;
}
