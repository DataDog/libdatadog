#ifndef DD_SAMPLERS_TL_STATE_H
#define DD_SAMPLERS_TL_STATE_H

#include <stdbool.h>
#include <stdint.h>

/* 512 KiB mean between samples. */
#define DD_SAMPLING_INTERVAL_DEFAULT (512u * 1024u)

/*
 * Tracks thread-specific bits we need for our sampling.
 *
 * Accessed via pthread_key_create / pthread_getspecific rather than
 * __thread / _Thread_local to avoid the __tls_get_addr -> malloc
 * reentrancy trap in shared libraries.

 * Equivalent of TrackerThreadLocalState
 * (ddprof: include/lib/allocation_tracker_tls.hpp).
 */
typedef struct {
     uint64_t sampling_interval;             /* mean bytes between samples.
                                               This _will probably_ be constant, but if we
                                               drop it in the TLS we afford the eBPF profiler
                                               the opportunity to tune it to adjust overhead
                                               dynamically. Whether or not this turns out to be
                                               a clever idea remains to be seen. */
    int64_t  remaining_bytes;               /* signed counter; sample when >= 0 */
    bool     remaining_bytes_initialized;   /* false until first interval drawn */
    bool     reentry_guard;                 /* set while inside a sampler hook */
    uint32_t rng;                           /* LCG state */

} dd_tl_state_t;

/*
 * Gets the current tracking state associated with the running thread. Never allocates.
 * Callers must treat NULL as "don't track".
 * 
 * Equivalent of AllocationTracker::get_tl_state
 * (ddprof: src/lib/allocation_tracker.cc).
 */
dd_tl_state_t *dd_tl_state_get(void);

/*
 * Initializes the current thread's tracking state if not already present.
 * Returns the new state, or NULL if state already exists on this thread
 * or the underlying allocation failed.
 */
dd_tl_state_t *dd_tl_state_init(void);

#endif
