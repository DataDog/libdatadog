// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/**
 * @file allocation_realloc.h
 *
 * Sampler-side helpers to wrap an underlying `realloc` call.
 *
 * Frontend usage:
 *
 *   dd_realloc_prep_t prep = dd_allocation_realloc_prepare(old_user, new_size);
 *   void *new_raw = real_realloc(prep.raw_ptr, prep.raw_size);
 *   return dd_allocation_realloc_commit(old_user, new_raw, prep);
 *
 * The prep/commit split mirrors dd_allocation_requested/created and
 * dd_allocation_freed: the sampler owns the pointer tagging policy and
 * the frontend owns the call to the real allocator.
 *
 * For a sampled old allocation, a successful realloc is reported as:
 *
 *   ddheap:free(old sampled) + new unsampled allocation
 *
 * New blocks from realloc(NULL, size) use the normal allocation sampling
 * path. Existing unsampled blocks pass through unchanged. Existing
 * sampled blocks are torn down as above.
 */
#ifndef DD_SAMPLERS_ALLOCATION_REALLOC_H
#define DD_SAMPLERS_ALLOCATION_REALLOC_H

#include <datadog/heap/allocation_requested.h>

#include <stddef.h>
#include <stdint.h>

/* Which realloc case prepare() classified. */
typedef enum {
    DD_REALLOC_KIND_PASSTHROUGH = 0,
    DD_REALLOC_KIND_ALLOC       = 1,
    DD_REALLOC_KIND_FREE        = 2,
    DD_REALLOC_KIND_SAMPLED     = 3,
} dd_realloc_kind_t;

/*
 * Snapshot of the sampler state around realloc.
 *
 *   raw_ptr    - pointer the frontend MUST pass to the underlying
 *                realloc. NULL for realloc(NULL, size). Equal to
 *                old_user on the passthrough path.
 *   raw_size   - size the frontend MUST pass to the underlying realloc.
 *   old_offset - byte offset from raw to user in the OLD sampled block.
 *                Used by commit() to shift user data down after realloc
 *                succeeds. 0 except on the sampled-old path.
 *   alloc_req  - allocation request state for realloc(NULL, size), so
 *                commit() can pair the real realloc result with
 *                dd_allocation_created and close the sampler guard.
 *   kind       - which realloc case prepare() selected.
 */
typedef struct {
    void              *raw_ptr;
    size_t             raw_size;
    size_t             old_offset;
    dd_alloc_req_t     alloc_req;
    dd_realloc_kind_t  kind;
} dd_realloc_prep_t;

/*
 * Inspect old_user and compute the request to hand to the underlying
 * realloc. For sampled old allocations this is non-destructive: it does
 * not clear the sampler flag on old_user, so if realloc later returns
 * NULL the old allocation stays usable and its flag stays intact for the
 * eventual free.
 *
 * realloc(old_user, 0) is destructive by definition for the allocators
 * we hook. prepare() consumes the sampler flag in that case before the
 * frontend forwards the raw pointer to realloc(raw, 0).
 */
dd_realloc_prep_t dd_allocation_realloc_prepare(void *old_user, size_t new_size)
    __attribute__((warn_unused_result));

/*
 * Finalize the realloc. Given the return value of the underlying
 * realloc (`new_raw`, may be NULL on failure) and the prep struct from
 * prepare(), returns the user-visible pointer to hand to the
 * application.
 *
 * On realloc(NULL, size): pairs new_raw with dd_allocation_created and
 * returns the possibly tagged user pointer.
 *
 * On the sampled-old path: shifts old user contents from [old_offset, ...)
 * down to [0, ...), fires ddheap:free(old_user), and returns new_raw as
 * an unsampled pointer.
 *
 * On the unsampled/passthrough and realloc(ptr, 0) paths: returns new_raw
 * unchanged.
 * On sampled realloc failure (new_raw == NULL): returns NULL and leaves
 * old_user live with its sampler flag intact.
 */
void *dd_allocation_realloc_commit(void *old_user, void *new_raw, dd_realloc_prep_t prep)
    __attribute__((warn_unused_result));

#endif
