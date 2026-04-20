#ifndef DD_SAMPLERS_SAMPLE_FLAG_H
#define DD_SAMPLERS_SAMPLE_FLAG_H

#include <stdbool.h>
#include <stddef.h>

/*
 * Architecture-specific "this allocation is sampled" flagging.
 * Two implementations live side-by-side in sample_flag.c under #ifdef:
 *   amd64 : header-magic (bump size, write magic before user pointer)
 *   arm64 : TBI pointer tag (stash magic in ignored top byte)
 */

/*
 * Extra bytes the flag reserves on top of the user's requested size.
 * dd_allocation_requested adds this to the size it hands back to the
 * wrapper so the underlying allocator reserves room for the flag. 0 on
 * architectures that flag via pointer bits instead of memory.
 */
size_t dd_sample_flag_overhead(void);

/*
 * Apply the sampled flag. Takes the raw pointer from the underlying
 * allocator (of size `user-requested + dd_sample_flag_overhead()`) and
 * returns the user-visible pointer to hand to the application.
 */
void *dd_sample_flag_apply(void *raw);

/*
 * If `user` was previously returned by dd_sample_flag_apply, write the
 * raw pointer (to pass to the underlying free) into *raw_out and return
 * true. Otherwise leave *raw_out untouched and return false.
 */
bool dd_sample_flag_check(void *user, void **raw_out);

#endif
