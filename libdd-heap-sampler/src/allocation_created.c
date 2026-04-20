#include "allocation_created.h"
#include "sample_flag.h"
#include "tl_state.h"

#ifdef __linux__
#  include <sys/sdt.h>
#else
#  define DTRACE_PROBE3(provider, name, a, b, c) ((void)0)
#endif

/*
 * Equivalent of the sampled branch of AllocTrackerHelper::track +
 * AllocationTracker::track_allocation's USDT/push_alloc_sample tail
 * (ddprof: src/lib/symbol_overrides.cc, src/lib/allocation_tracker.cc).
 */
void *dd_allocation_created_slow(void *raw, dd_alloc_req_t req) {
    void *user = raw;
    if (raw != NULL) {
        user = dd_sample_flag_apply(raw);
        DTRACE_PROBE3(ddheap, alloc, user, (uint64_t)req.size, req.weight);
    }

    /* Close the reentry guard opened by the paired requested() call. */
    dd_tl_state_t *tl = dd_tl_state_get();
    if (tl) tl->reentry_guard = false;

    return user;
}
