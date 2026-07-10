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

#include <stdint.h>

#ifdef __linux__
   /* libbpf/usdt vendored at libdd-profiling-heap-sampler/vendor/usdt.h. Provides
    * the variadic USDT() macro that emits the same v3 ELF-note format
    * that bpftrace, systemtap, and BPF tracers all consume. */
#  include <usdt.h>
#else
#  define USDT(provider, name, ...) ((void)0)
#endif

/*
 * Emits the `ddheap:alloc` USDT.
 *   user   - user-visible allocation pointer
 *   size   - allocation size in bytes
 *   weight - unbiased size estimator (nsamples * interval)
 */
void dd_probe_alloc(void *user, uint64_t size, uint64_t weight);

#if DD_HEAP_LIVE_TRACKING
/*
 * Emits the `ddheap:free` USDT.
 *   ptr - user-visible pointer being freed
 *
 * Only available when compiled with live-heap tracking. The absence of
 * the `ddheap:free` note in .note.stapsdt signals to external profilers
 * that this binary does not support live-heap correlation.
 */
void dd_probe_free(void *ptr);
#endif

#endif
