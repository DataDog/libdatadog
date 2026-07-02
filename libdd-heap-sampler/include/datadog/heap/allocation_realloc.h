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
 * The MVP model for a sampled old allocation is:
 *
 *   successful sampled realloc = ddheap:free(old sampled)
 *                              + new unsampled allocation
 *
 * See gotter's `gotter_realloc` for the full four-case handling
 * (`ptr == NULL`, `size == 0`, unsampled-old, sampled-old); this pair
 * covers only the last two cases (regular passthrough vs sampled
 * teardown), since malloc/free/realloc(NULL,_) / realloc(_,0) map to
 * dedicated sampler primitives.
 *
 * Not paired with dd_allocation_requested / dd_allocation_created: the
 * new block is deliberately unsampled to avoid data-corruption hazards
 * around header stamping without knowing the original user-requested
 * size. Revisit once sampled headers carry the original user size.
 */
#ifndef DD_SAMPLERS_ALLOCATION_REALLOC_H
#define DD_SAMPLERS_ALLOCATION_REALLOC_H

#include <stdbool.h>
#include <stddef.h>

/*
 * Snapshot of the sampler state around a sampled realloc.
 *
 *   raw_ptr     - pointer the frontend MUST pass to the underlying
 *                 realloc. Equal to old_user on the passthrough path.
 *   raw_size    - size the frontend MUST pass to the underlying
 *                 realloc. Equal to new_size on the passthrough path.
 *   old_offset  - byte offset from raw to user in the OLD sampled
 *                 block. Used by commit() to shift user data down after
 *                 realloc succeeds. 0 on the passthrough path.
 *   was_sampled - true iff old_user was a sampled allocation. commit()
 *                 only runs the extra teardown work when this is true.
 */
typedef struct {
    void  *raw_ptr;
    size_t raw_size;
    size_t old_offset;
    bool   was_sampled;
} dd_realloc_prep_t;

/*
 * Inspect old_user and compute the request to hand to the underlying
 * realloc. Non-destructive: does not clear the sampler flag on
 * old_user, so if realloc later returns NULL the old allocation stays
 * usable and its flag stays intact for the eventual free.
 */
dd_realloc_prep_t dd_allocation_realloc_prepare(void *old_user, size_t new_size);

/*
 * Finalize the realloc. Given the return value of the underlying
 * realloc (`new_raw`, may be NULL on failure) and the prep struct from
 * prepare(), returns the user-visible pointer to hand to the
 * application.
 *
 * On the sampled path: shifts old user contents from [old_offset, ...)
 * down to [0, ...), fires ddheap:free(old_user), and returns new_raw
 * as an unsampled pointer.
 *
 * On the unsampled/passthrough path: returns new_raw unchanged.
 * On realloc failure (new_raw == NULL): returns NULL.
 */
void *dd_allocation_realloc_commit(void *old_user, void *new_raw, dd_realloc_prep_t prep);

#endif
