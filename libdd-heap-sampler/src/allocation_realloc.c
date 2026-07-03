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
    dd_realloc_prep_t out = {
        .raw_ptr     = old_user,
        .raw_size    = new_size,
        .old_offset  = 0,
        .alloc_req   = { 0, 0, 0, 0 },
        .kind        = DD_REALLOC_KIND_PASSTHROUGH,
    };

    if (old_user == NULL) {
        dd_alloc_req_t req = dd_allocation_requested(
            new_size, DD_REALLOC_DEFAULT_ALIGNMENT);
        out.raw_ptr = NULL;
        out.raw_size = req.size;
        out.alloc_req = req;
        out.kind = DD_REALLOC_KIND_ALLOC;
        return out;
    }

    if (new_size == 0) {
        dd_alloc_freed_t freed = dd_allocation_freed(old_user, 0, 0);
        out.raw_ptr = freed.ptr;
        out.raw_size = 0;
        out.kind = DD_REALLOC_KIND_FREE;
        return out;
    }

    void  *old_raw    = NULL;
    size_t old_offset = 0;
    if (!dd_sample_flag_peek(old_user, &old_raw, &old_offset)) {
        return out;  /* passthrough: not sampled */
    }

    /* Clear the magic header NOW, while we still own the block. Once the
     * real realloc runs it may free the old block internally; if we left
     * the magic intact, a future allocation that reuses that memory could
     * be falsely detected as sampled by dd_sample_flag_check, leading to
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
    {
        void *old_header = (char *)old_user - DD_HEADER_BYTES;
        const uint64_t zero = 0;
        memcpy(old_header, &zero, sizeof(zero));
    }
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
        void *hdr = (char *)old_user - DD_HEADER_BYTES;
        uint64_t m = DD_MAGIC;
        uint64_t o = (uint64_t)old_offset;
        memcpy(hdr, &m, sizeof(m));
        memcpy((char *)hdr + sizeof(m), &o, sizeof(o));
#endif
        return out;
    }

    out.raw_ptr    = old_raw;
    out.raw_size   = new_size + old_offset;
    out.old_offset = old_offset;
    out.kind       = DD_REALLOC_KIND_SAMPLED;
    return out;
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

    /* Underlying realloc failed: C says old_user is still live.
     * prepare() cleared the header optimistically, so re-stamp it
     * now so that a later free(old_user) correctly resolves the raw
     * pointer via dd_sample_flag_check. */
    if (new_raw == NULL) {
#if defined(__x86_64__)
        void *old_header = (char *)old_user - DD_HEADER_BYTES;
        uint64_t magic  = DD_MAGIC;
        uint64_t offset = (uint64_t)prep.old_offset;
        memcpy(old_header, &magic, sizeof(magic));
        memcpy((char *)old_header + sizeof(magic), &offset, sizeof(offset));
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
}
