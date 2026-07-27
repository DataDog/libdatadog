// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/tl_state.h>
#include <datadog/heap/sample_flag.h>

#include <errno.h>
#include <stdbool.h>
#include <stdatomic.h>
#include <stdint.h>
#include <time.h>

_Thread_local dd_tl_state_t dd_tl_state_storage;

/* Process-wide override; 0 = use compiled-in default. */
_Atomic uint64_t dd_sampling_interval_override = 0;

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

    bool flag_ok = dd_sample_flag_thread_init();

    struct timespec ts = {0};
    (void)clock_gettime(CLOCK_MONOTONIC, &ts);
    uint32_t seed = (uint32_t)((uintptr_t)st
                               ^ (uintptr_t)ts.tv_nsec
                               ^ ((uintptr_t)ts.tv_sec << 20));
    st->rng = seed ? seed : 1u;

    st->sampling_interval = dd_sampling_interval_effective();
    /* If the per-thread flagging scheme is unavailable (arm64 prctl
     * failure), leave reentry_guard set. The fast path in
     * dd_allocation_requested short-circuits on reentry_guard, so this
     * thread will pass every allocation through unsampled - no tagged
     * pointers get produced, no syscall EFAULTs. Cheaper than adding a
     * dedicated "sampling_disabled" field + branch on the hot path. */
    if (flag_ok) {
        st->reentry_guard = false;
    }
}

/*
 * Initialises TLS for this thread on the first call and is a no-op on
 * subsequent calls. Fire-and-forget: callers that need the pointer should
 * use dd_tl_state_get_or_init().
 */
void dd_tl_state_init(void) {
    if (dd_tl_state_storage.initialized) return;

    /* Save / restore errno: first-touch init calls prctl() / clock_gettime(). */
    int saved_errno = errno;
    tl_state_populate(&dd_tl_state_storage);
    errno = saved_errno;
}

/* 64 KiB floor: intervals below this produce excessive overhead for
 * negligible additional insight. The sampler's internal gap floor is 8
 * bytes, but we clamp much higher here to protect callers from
 * accidental misconfiguration. */
#define DD_SAMPLING_INTERVAL_MIN (64u * 1024u)

void dd_set_default_sampling_interval(uint64_t interval_bytes) {
    if (interval_bytes != 0 && interval_bytes < DD_SAMPLING_INTERVAL_MIN) {
        interval_bytes = DD_SAMPLING_INTERVAL_MIN;
    }
    atomic_store_explicit(&dd_sampling_interval_override, interval_bytes,
                          memory_order_relaxed);
}
