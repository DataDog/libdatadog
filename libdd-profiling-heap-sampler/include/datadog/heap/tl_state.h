/**
 * @file tl_state.h
 *
 * Per-thread sampler state. How to store this is _nuanced_. We can choose between
 * the older, thread-specific data (TSD) APIs  (pthread_getspecific, pthread_setspecific, ...),
 * and the newer, compiler-side one - __thread. In the latter case we must also
 * consider the impact of TLS model, and in all cases, we have to be careful
 * to not accidentally infinitely recurse when the access mechanism must allocate.
 *
 * --- Summary of options -----------------------------------------------------
 *
 *   A. initial-exec TLS, exclude musl late-dlopen
 *      Direct TP-relative load on every access. Fast. Works for static builds
 *      and glibc dynamic builds (our struct fits within glibc's static TLS
 *      surplus). Hard fails at dlopen time on musl: loader rejects before any
 *      of our code runs, so no runtime fallback is possible.
 *
 *   B. TLSDESC, single library <-- let's start here
 *      Works for static builds, glibc dynamic, and musl dynamic. For static
 *      builds the linker relaxes TLSDESC to local-exec automatically, so
 *      there is no per-access overhead vs A. For musl dynamic, __tls_get_addr
 *      is a pure DTV lookup (musl pre-populates at dlopen time): also no
 *      extra overhead vs what musl can offer. The reentrancy concern on glibc
 *      is eliminated because the gotter skips ld-linux in its GOT walk, so
 *      __tls_get_addr's internal malloc never goes through our hook. The only
 *      case where B costs more than necessary is glibc dynamic: we pay the
 *      TLSDESC indirect call where initial-exec would give us a direct
 *      TP-relative load. That is the sole remaining argument for option C.
 *      (Pre-warming via pthread_create hook was considered and rejected: it
 *      cannot cover threads that existed before the gotter installed.)
 *
 *   C. Two build variants, caller picks
 *      Build a -glibc variant (initial-exec) and a -musl variant (TLSDESC).
 *      Each gets optimal per-access performance for its runtime. The caller
 *      detects musl/glibc and loads the appropriate one. More complex build
 *      and deployment story.
 *
 * Constraints:
 *   - Must work for both static builds and dynamic builds, including late
 *     dlopen (we cannot assume we are loaded at startup).
 *   - Allocation reentrancy during TLS init is a real problem: any allocating
 *     path inside the sampler re-enters the hook before state exists.
 *   - The sampler must handle init generically, without relying on the caller
 *     (gotter, Rust allocator, etc.) doing any injection-specific setup first.
 *   - Ignoring musl + late dlopen as a supported target may be acceptable,
 *     and would simplify the dynamic build story considerably.
 *
 * --- Thread-local mechanism ------------------------------------------------
 *
 * Two broad approaches. "Thread-Specific Data" (TSD) is the POSIX runtime
 * model; "Thread-Local Storage" (TLS) is the compiler/linker model.
 *
 *   pthread_key_t     TSD. A runtime key-value store: pthread_key_create once,
 *                     pthread_getspecific / pthread_setspecific per thread.
 *                     The value is a void*, so you heap-allocate the struct
 *                     and store a pointer. pthread_getspecific is cheap on
 *                     glibc (array lookup in thread memory, no __tls_get_addr).
 *                     ddprof uses this (see allocation_tracker.cc) to avoid
 *                     __tls_get_addr allocating in the Global Dynamic model;
 *                     ddprof builds a universal musl/glibc binary so
 *                     initial-exec _Thread_local is not an option for them.
 *                     Downside: init must heap-allocate the struct, which
 *                     re-enters the allocator hook before state exists.
 *
 *   _Thread_local     TLS. C11 standard spelling of GCC's older __thread
 *   (__thread)        extension; both compile identically on GCC and Clang.
 *                     Storage lives in the thread's TLS segment, not the heap.
 *                     No allocation on access, but the TLS model matters (see
 *                     below) and __tls_get_addr can still allocate on first
 *                     access in the Global Dynamic model.
**/
#ifndef DD_SAMPLERS_TL_STATE_H
#define DD_SAMPLERS_TL_STATE_H

#include <stdatomic.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

/* 512 KiB mean between samples. */
#define DD_SAMPLING_INTERVAL_DEFAULT (512u * 1024u)

/* 64 KiB floor: intervals below this produce excessive overhead. */
#define DD_SAMPLING_INTERVAL_MIN (64u * 1024u)

/* Number of samples between adaptive interval adjustments. */
#define DD_ADAPT_WINDOW 10

/* Process-wide override for the target mean sampling interval. 0 means
 * "use DD_SAMPLING_INTERVAL_DEFAULT". Set via dd_set_default_sampling_interval(). */
extern _Atomic uint64_t dd_sampling_interval_override;

/* Process-wide target sample rate (samples/sec/thread). Default 10.
 * 0 = disabled (fixed interval). Set via dd_set_target_sample_rate(). */
extern _Atomic uint64_t dd_target_samples_per_sec;

/* Set the default mean sampling interval (bytes between samples).
 * Pass 0 to revert to DD_SAMPLING_INTERVAL_DEFAULT. Values below
 * 64 KiB are clamped to 64 KiB to avoid excessive overhead. */
void dd_set_default_sampling_interval(uint64_t interval_bytes);

/* Set the target sample rate for adaptive interval control.
 * Pass 0 to disable adaptation (uses fixed interval). */
void dd_set_target_sample_rate(uint64_t samples_per_sec);

/* Per-thread state for the Poisson sampler. See file header for the
 * rationale behind _Thread_local vs pthread TLS. */
typedef struct {
     uint64_t sampling_interval;             /* mean bytes between samples.
                                               This _will probably_ be constant, but if we
                                               drop it in the TLS we afford the eBPF profiler
                                               the opportunity to tune it to adjust overhead
                                               dynamically. A value of 0 explicitly disables
                                               sampling for this thread. */
    int64_t  remaining_bytes;               /* signed counter; sample when >= 0 */
    bool     remaining_bytes_initialized;   /* false until first interval drawn */
    bool     initialized;                   /* false until dd_tl_state_init() has run;
                                               field rather than a separate _Thread_local
                                               so only one TLS lookup is needed on the
                                               fast path (avoids a second TLSDESC call). */
    bool     reentry_guard;                 /* Set between dd_allocation_requested_slow and
                                               dd_allocation_created_slow while a sampled
                                               allocation is in flight.

                                               The slow path can itself trigger allocations:
                                               log() in next_interval() may touch lazy-init'd
                                               libc state on first call; USDT emission and any
                                               attached eBPF consumer can cause incidental
                                               userspace allocation; TLS materialisation on a
                                               fresh thread may call calloc.

                                               Without this guard, those inner allocations
                                               would re-enter dd_allocation_requested, corrupt
                                               the remaining_bytes counter, and in the worst
                                               case recurse infinitely. While the guard is set
                                               the fast path bails out immediately and the
                                               inner allocation is passed through unsampled. */
    uint32_t rng;                           /* LCG state */

    /* --- Adaptive rate control --- */
    uint64_t last_ns;                       /* CLOCK_MONOTONIC ns at last adjustment */
    uint64_t ns_per_sample_target;          /* 1e9 / target_rate; 0 = adapt off */
    uint8_t  samples_since_adjust;          /* counts up to DD_ADAPT_WINDOW */
} dd_tl_state_t;

extern _Thread_local dd_tl_state_t dd_tl_state_storage;

/* Returns the effective sampling interval: the process-wide override if
 * non-zero, otherwise DD_SAMPLING_INTERVAL_DEFAULT. */
static inline __attribute__((always_inline))
uint64_t dd_sampling_interval_effective(void) {
    uint64_t ov = atomic_load_explicit(&dd_sampling_interval_override,
                                       memory_order_relaxed);
    return (ov != 0) ? ov : DD_SAMPLING_INTERVAL_DEFAULT;
}

/*
 * Returns the current thread's state, or NULL if not yet initialised.
 * Never allocates. Callers must treat NULL as "don't sample".
 */
static inline __attribute__((always_inline))
dd_tl_state_t *dd_tl_state_get(void) {
    if (__builtin_expect(!dd_tl_state_storage.initialized, 0)) return NULL;
    return &dd_tl_state_storage;
}

/*
 * Ensures the current thread's tracking state exists, initializing it on the
 * first call and doing nothing on subsequent calls. This is a fire-and-forget
 * command: call it to eagerly warm TLS (e.g. from a thread-start hook) so the
 * first tracked allocation on the thread doesn't pay the init cost. Callers
 * that need the pointer should use dd_tl_state_get_or_init() instead.
 */
void dd_tl_state_init(void);

/*
 * Returns the current thread's tracking state, initializing it on first use.
 */
static inline __attribute__((always_inline))
dd_tl_state_t *dd_tl_state_get_or_init(void) {
    if (__builtin_expect(!dd_tl_state_storage.initialized, 0)) dd_tl_state_init();
    return &dd_tl_state_storage;
}

#endif
