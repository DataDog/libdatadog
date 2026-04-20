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
#  include <sys/sdt.h>
#else
#  define DTRACE_PROBE1(provider, name, a) ((void)0)
#  define DTRACE_PROBE3(provider, name, a, b, c) ((void)0)
#endif

/*
 * Emits the `ddheap:alloc` USDT.
 *   user   - user-visible allocation pointer
 *   size   - allocation size in bytes
 *   weight - unbiased size estimator (nsamples * interval)
 */
void dd_probe_alloc(void *user, uint64_t size, uint64_t weight);

/*
 * Emits the `ddheap:free` USDT.
 *   ptr - user-visible pointer being freed
 */
void dd_probe_free(void *ptr);

#endif
