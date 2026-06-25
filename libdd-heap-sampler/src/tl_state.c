// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/tl_state.h>
#include <datadog/heap/sample_flag.h>

#include <stdint.h>
#include <time.h>

_Thread_local dd_tl_state_t dd_tl_state_storage;

/*
 * Fills a freshly zeroed dd_tl_state_t with its initial values.
 *
 * Seeds the LCG from a mix of the TLS storage address and a monotonic
 * clock read. Neither source is cryptographic, but the LCG only drives
 * allocation *sampling* decisions - all we need is that distinct threads
 * start with distinct non-zero seeds so their sample sequences
 * decorrelate. The TLS address gives per-thread and (thanks to ASLR)
 * per-process variation; the CLOCK_MONOTONIC nanoseconds decorrelate
 * threads created back-to-back whose TLS slots differ only in low bits.
 * clock_gettime is vDSO-served on Linux and available on every libc we
 * ship on, unlike <sys/random.h>'s getentropy (glibc >= 2.25, musl
 * >= 1.1.20). Copies in the default sampling interval so the eBPF
 * profiler can tune it per-thread at runtime by writing to the TLS slot
 * directly.
 */
static void tl_state_populate(dd_tl_state_t *st) {
    *st = (dd_tl_state_t){0};

    /* Set both flags before doing any work. The subsequent function calls
     * (dd_sample_flag_thread_init, clock_gettime) act as compiler barriers,
     * so the compiler cannot sink these writes below them. Any allocation
     * triggered inside those calls will therefore see both flags set and
     * bail out unsampled rather than recursing back into init.
     *
     */
    st->initialized    = true;
    st->reentry_guard  = true;

    dd_sample_flag_thread_init();

    struct timespec ts = {0};
    (void)clock_gettime(CLOCK_MONOTONIC, &ts);
    uint32_t seed = (uint32_t)((uintptr_t)st
                               ^ (uintptr_t)ts.tv_nsec
                               ^ ((uintptr_t)ts.tv_sec << 20));
    st->rng = seed ? seed : 1u;

    st->sampling_interval = DD_SAMPLING_INTERVAL_DEFAULT;
    st->reentry_guard = false;
}

dd_tl_state_t *dd_tl_state_get(void) {
    return dd_tl_state_get_fast();
}

/*
 * Initialises TLS for this thread on first call; returns NULL on subsequent
 * calls (state already exists). Callers that need the pointer regardless
 * should use dd_tl_state_get_or_init().
 */
dd_tl_state_t *dd_tl_state_init(void) {
    if (dd_tl_state_storage.initialized) return NULL;

    tl_state_populate(&dd_tl_state_storage);
    return &dd_tl_state_storage;
}

dd_tl_state_t *dd_tl_state_get_or_init(void) {
    return dd_tl_state_get_or_init_fast();
}
