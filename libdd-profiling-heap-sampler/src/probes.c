// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/*
 * USDT emission for the ddheap provider.
 *
 * Kept as non-inline functions in a separate translation unit so that each
 * probe has one USDT() expansion and one .note.stapsdt entry. The intent
 * is that callers always reach the probe via a call instruction rather
 * than having it inlined at multiple sites.
 *
 * The immediate concern is bindgen's wrap_static_fns: if these were static
 * inline, it would generate a wrapper stub for each one containing its own
 * USDT() expansion, likely producing duplicate .note.stapsdt entries and
 * causing bpftrace to attach twice. LTO could in principle inline these
 * across TU boundaries and cause similar problems, though we haven't
 * tested that path.
 */

#include <datadog/heap/probes.h>

#include <errno.h>

/* Save / restore errno: an attached USDT consumer may perturb it. */
void dd_probe_alloc(void *user, uint64_t size, uint64_t weight) {
    int saved_errno = errno;
    USDT(ddheap, alloc, user, size, weight);
    errno = saved_errno;
}

void dd_probe_free(void *ptr) {
    int saved_errno = errno;
    USDT(ddheap, free, ptr);
    errno = saved_errno;
}
