// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/allocation_created.h>
#include <datadog/heap/allocation_freed.h>
#include <datadog/heap/allocation_realloc.h>
#include <datadog/heap/probes.h>
#include <datadog/heap/sample_flag.h>

#include <stdint.h>
#include <string.h>

#define DD_REALLOC_DEFAULT_ALIGNMENT (sizeof(void *) * 2)

dd_realloc_prep_t dd_allocation_realloc_prepare(void *old_user, size_t new_size) {
    /* Default: hand the call to the real realloc unchanged. */
    const dd_realloc_prep_t passthrough = {
        .raw_ptr     = old_user,
        .raw_size    = new_size,
        .old_offset  = 0,
        .alloc_req   = { 0, 0, 0, 0 },
        .kind        = DD_REALLOC_KIND_PASSTHROUGH,
    };

    if (old_user == NULL) {
        /* realloc(NULL, n) == malloc(n): run the allocation sampling path. */
        dd_alloc_req_t req =
            dd_allocation_requested(new_size, DD_REALLOC_DEFAULT_ALIGNMENT);
        return (dd_realloc_prep_t){
            .raw_ptr    = NULL,
            .raw_size   = req.size,
            .old_offset = 0,
            .alloc_req  = req,
            .kind       = DD_REALLOC_KIND_ALLOC,
        };
    }

    if (new_size == 0) {
        /* realloc(ptr, 0) == free(ptr) for the allocators we hook. */
        dd_alloc_freed_t freed = dd_allocation_freed(old_user, 0, 0);
        return (dd_realloc_prep_t){
            .raw_ptr    = freed.ptr,
            .raw_size   = 0,
            .old_offset = 0,
            .alloc_req  = { 0, 0, 0, 0 },
            .kind       = DD_REALLOC_KIND_FREE,
        };
    }

#if DD_HEAP_LIVE_TRACKING
    void  *old_raw    = NULL;
    size_t old_offset = 0;
    if (!dd_sample_flag_peek(old_user, &old_raw, &old_offset)) {
        return passthrough;  /* not sampled */
    }

    /* Clear the magic header NOW, while we still own the block. Once the
     * real realloc runs it may free the old block internally; if we left
     * the magic intact, a future allocation that reuses that memory could
     * be falsely detected as sampled by dd_sample_flag_check_and_clear, leading to
     * a bogus raw pointer and a heap corruption on free.
     *
     * This is safe even if the real realloc subsequently fails (returns
     * NULL): in that case old_user is still live but now unsampled.
     * commit() returns NULL to the caller, so the application retains
     * old_user; a later free(old_user) will take the unsampled fast path
     * and pass old_user directly to the underlying free — which is
     * incorrect (it should pass old_raw). To handle this, commit()
     * re-stamps the header when realloc fails. */
#if defined(__x86_64__)
    x86_header_clear(old_user);
#endif

    /* Reserve room for the old header+slack ([0, old_offset)) plus
     * `new_size` bytes of user data at [old_offset, old_offset + new_size).
     * commit() shifts the user data down to [0, new_size). Overflow ->
     * fall back to passthrough with the caller-supplied size; the
     * underlying realloc will likely fail with a huge value, but
     * nothing gets silently truncated or misinterpreted.
     *
     * We already cleared the header above, so we must re-stamp it
     * before falling through to passthrough, otherwise a later free
     * would not recover old_raw. */
    if (new_size > SIZE_MAX - old_offset) {
#if defined(__x86_64__)
        x86_header_stamp(old_user, (uint64_t)old_offset);
#endif
        /* Hand the real realloc old_raw, not the offset user pointer, so
         * this doomed oversized request fails cleanly instead of corrupting
         * the heap. Otherwise a plain passthrough with the caller's size. */
        return (dd_realloc_prep_t){
            .raw_ptr    = old_raw,
            .raw_size   = new_size,
            .old_offset = 0,
            .alloc_req  = { 0, 0, 0, 0 },
            .kind       = DD_REALLOC_KIND_PASSTHROUGH,
        };
    }

    return (dd_realloc_prep_t){
        .raw_ptr    = old_raw,
        .raw_size   = new_size + old_offset,
        .old_offset = old_offset,
        .alloc_req  = { 0, 0, 0, 0 },
        .kind       = DD_REALLOC_KIND_SAMPLED,
    };
#else
    /* Live-heap tracking off: existing blocks are never flagged, so there
     * is no sampled realloc to recover. Pass straight through to the real
     * realloc. (realloc(NULL, n) already took the ALLOC path above.) */
    return passthrough;
#endif
}

void *dd_allocation_realloc_commit(void *old_user, void *new_raw, dd_realloc_prep_t prep) {
    if (prep.kind == DD_REALLOC_KIND_ALLOC) {
        return dd_allocation_created(new_raw, prep.alloc_req);
    }

    if (prep.kind == DD_REALLOC_KIND_FREE) {
        return new_raw;
    }

    if (prep.kind == DD_REALLOC_KIND_PASSTHROUGH) {
        return new_raw;
    }

#if DD_HEAP_LIVE_TRACKING
    /* Underlying realloc failed: C says old_user is still live.
     * prepare() cleared the header optimistically, so re-stamp it
     * now so that a later free(old_user) correctly resolves the raw
     * pointer via dd_sample_flag_check_and_clear. */
    if (new_raw == NULL) {
#if defined(__x86_64__)
        x86_header_stamp(old_user, (uint64_t)prep.old_offset);
#endif
        return NULL;
    }

    /* Sampled path. libc realloc copied the old block's bytes into
     * new_raw starting at index 0, so the old user data now sits at
     * new_raw + old_offset. Shift it down to new_raw = user offset 0
     * so we can hand new_raw back as an unsampled pointer.
     *
     * memmove (not memcpy) because when realloc extends in place,
     * new_raw == old_raw and source/destination overlap. */
    size_t user_size = prep.raw_size - prep.old_offset;
    char  *src       = (char *)new_raw + prep.old_offset;
    if ((void *)src != new_raw) {
        memmove(new_raw, src, user_size);
    }

    /* Report the free of the OLD sampled allocation (the address the
     * profiler last saw as live). No matching alloc is fired: the new
     * block is unsampled. dd_probe_free just emits the ddheap:free
     * USDT so the profiler can close the live-heap entry. */
    dd_probe_free(old_user);
    return new_raw;
#else
    /* Only ALLOC/FREE/PASSTHROUGH kinds occur without live-heap tracking;
     * prepare() never produces a sampled teardown. */
    (void)old_user;
    return new_raw;
#endif
}
