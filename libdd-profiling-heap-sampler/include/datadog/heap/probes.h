/*
 * USDT probe emission functions for the ddheap provider.
 *
 * Defined in probes.c as regular non-inline functions so that each probe
 * site has a single, stable address in the final binary. This matters
 * because bindgen's wrap_static_fns generates tiny wrapper stubs for any
 * static inline function it sees; if the DTRACE_PROBE macros expanded inside
 * those stubs the resulting .note.stapsdt entries would carry section-relative
 * offsets that bpftrace cannot resolve correctly.
 */

#ifndef DD_SAMPLERS_PROBES_H
#define DD_SAMPLERS_PROBES_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __linux__
   /* libbpf/usdt vendored at libdd-profiling-heap-sampler/vendor/usdt.h. Provides
    * the variadic USDT() macro that emits the same v3 ELF-note format
    * that bpftrace, systemtap, and BPF tracers all consume. */
#  include <usdt.h>
   /* Shared explicit semaphore for the ddheap provider. Defined in probes.c;
    * declared here so that other TUs (e.g. allocation_requested.h) can check
    * USDT_SEMA_IS_ACTIVE(ddheap_alloc) without emitting their own implicit
    * semaphore definitions. */
   USDT_DECLARE_SEMA(ddheap_alloc);
#else
#  define USDT(provider, name, ...) ((void)0)
#  define USDT_SEMA_IS_ACTIVE(sema) (1)
#endif

/*
 * Emits the `ddheap:alloc` USDT.
 *   user          - user-visible allocation pointer
 *   size          - application-requested allocation size in bytes
 *   weighted_bytes - estimated allocated bytes represented by this sampled
 *                   allocation, computed as size / (1 - exp(-size / interval))
 *                   where interval is the mean sampling distance.
 */
void dd_probe_alloc(void *user, uint64_t size, uint64_t weighted_bytes);

/*
 * Emits the `ddheap:free` USDT.
 *   ptr - user-visible pointer being freed
 *
 * The symbol always exists, but the USDT is only emitted when compiled
 * with live-heap tracking. The absence of the `ddheap:free` note in
 * .note.stapsdt signals to external profilers that this binary does not
 * support live-heap correlation.
 */
void dd_probe_free(void *ptr);

/*
 * Returns true when an external profiler is currently attached to the
 * ddheap:alloc USDT in this object file.
 *
 * This is a point-in-time read of the alloc probe's USDT semaphore. A profiler
 * can attach or detach immediately after this returns. Use it as a
 * diagnostic/readiness signal, not as a synchronization primitive.
 */
bool dd_heap_profiler_attached(void);

/*
 * Test-only: manually activate the ddheap:alloc USDT semaphore so that
 * dd_allocation_requested's fast-path guard allows sampling in a process
 * where no real profiler is attached. Call with `active=true` before
 * exercising the sampler and `active=false` to restore the default state.
 *
 * This writes the same 2-byte counter the kernel would increment when
 * bpftrace/eBPF attaches to the USDT.
 */
void dd_test_set_profiler_active(bool active);

#endif
