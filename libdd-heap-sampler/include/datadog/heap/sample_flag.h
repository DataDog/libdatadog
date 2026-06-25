/**
 * @file sample_flag.h
 *
 * Architecture-specific "this allocation is sampled" flagging.

 * Two implementations live side-by-side in sample_flag.c under #ifdef:
 *   amd64 : header-magic (bump size, write magic before user pointer)
 *   arm64 : TBI pointer tag (flag ignored top byte)
 *
 * Memory tagging _should_ be the sweet spot as it adds no overhead to read,
 * but we will see how it works out in practice.
 */
#ifndef DD_SAMPLERS_SAMPLE_FLAG_H
#define DD_SAMPLERS_SAMPLE_FLAG_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

/*
 * Per-thread initialisation required by the flagging scheme.
 * Must be called once per thread before dd_sample_flag_apply/check are used.
 * On arm64 this issues prctl(PR_SET_TAGGED_ADDR_CTRL) so tagged pointers
 * survive syscalls; on other architectures this is a no-op.
 */
void dd_sample_flag_thread_init(void);

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

#if defined(__x86_64__)

#define DD_HEADER_BYTES 16
#define DD_PAGE_SIZE    4096

/*
 * x86-64 marks sampled allocations with a magic word written 16 bytes
 * before the user pointer. The allocator hands us a 16-byte aligned raw
 * pointer of size user_size + 32; we pick the user pointer inside that
 * surplus so reading 16 bytes behind it always stays within a mapped
 * page.
 *
 * Two layouts are used, distinguished by which magic is present:
 *   DD_MAGIC_A: user = raw + 16   (common case)
 *   DD_MAGIC_B: user = raw + 32   (only when raw + 16 would land in
 *                                  the first 16 bytes of a page; about
 *                                  1/256 of sampled allocations)
 *
 * With +32 surplus exactly one of {raw+16, raw+32} is always safe:
 * both are 16-byte aligned and their page offsets differ by 16, so at
 * most one can fall in the unsafe [0, 16) band. The two-magic encoding
 * lets us recover raw at free time without any extra metadata.
 * This costs us on average 32 bytes on every 512 KiB allocated, or about
 * 62 KiB on 1 GiB.
 *
 * Alternatives considered:
 *   - +16 surplus with a free+malloc retry when raw+16 is unsafe.
 *     Saves 16 bytes per sampled allocation in the common case (~99.6%)
 *     but plumbs malloc/free callbacks into the sampler and risks an
 *     OOM on the retry path. Not worth it: sampled allocations are
 *     rare so the absolute byte cost of always +32 is negligible.
 *   - Drop samples whose user pointer comes back in the unsafe band.
 *     Simple, but introduces a  bias in the sample set
 *     (allocations whose raw lands at offset 4080 of a page are never
 *     reported).
 */

#define DD_MAGIC_A 0xfab1eddec0dedca7ULL  /* user = raw + 16 */
#define DD_MAGIC_B 0xfab1eddec0dedca8ULL  /* user = raw + 32 */

/*
 * Layout helpers. x86_apply and x86_raw_from_user MUST be each other's
 * inverse: the magic stamped at apply time is what lets x86_raw_from_user
 * recover the same raw at check time.
 */

/*
 * Pick the user pointer inside the [raw, raw + 32) surplus and stamp
 * the matching magic into the 16-byte header immediately preceding it.
 * The chosen layout is encoded in the magic value itself so
 * x86_raw_from_user can recover `raw` at free time without any extra
 * metadata.
 *
 *   raw - 16-byte aligned pointer from the underlying allocator,
 *         backing a region of size user_size + 32.
 *
 * Returns the user-visible pointer (raw+16 or raw+32), chosen so the
 * 16-byte header slot never straddles the start of a page.
 */
static inline __attribute__((always_inline))
void *x86_apply(void *raw) {
    uintptr_t r = (uintptr_t)raw;
    uintptr_t u = r + DD_HEADER_BYTES;
    uint64_t magic = DD_MAGIC_A;
    if ((u & (DD_PAGE_SIZE - 1)) < DD_HEADER_BYTES) {
        u = r + 2 * DD_HEADER_BYTES;
        magic = DD_MAGIC_B;
    }
    memcpy((void *)(u - DD_HEADER_BYTES), &magic, sizeof(magic));
    return (void *)u;
}

/*
 * Inverse of x86_apply: given a user pointer previously produced by
 * it and the magic read out of the 16-byte header preceding `user`
 * (DD_MAGIC_A => user = raw+16, DD_MAGIC_B => raw+32), return the
 * original raw pointer to hand back to the underlying free.
 */
static inline __attribute__((always_inline))
void *x86_raw_from_user(void *user, uint64_t magic) {
    uintptr_t u = (uintptr_t)user;
    return (void *)(u - (magic == DD_MAGIC_A ? DD_HEADER_BYTES
                                              : 2 * DD_HEADER_BYTES));
}

static inline __attribute__((always_inline))
bool dd_sample_flag_check_fast(void *user, void **raw_out) {
    if (((uintptr_t)user & (DD_PAGE_SIZE - 1)) < DD_HEADER_BYTES) {
        return false;
    }

    void *header = (char *)user - DD_HEADER_BYTES;
    uint64_t magic;
    memcpy(&magic, header, sizeof(magic));
    if (magic != DD_MAGIC_A && magic != DD_MAGIC_B) {
        return false;
    }

    const uint64_t zero = 0;
    memcpy(header, &zero, sizeof(zero));
    *raw_out = x86_raw_from_user(user, magic);
    return true;
}

#elif defined(__aarch64__)

#define DD_TBI_TAG      0xDDu
#define DD_TBI_TAG_MASK ((uintptr_t)0xFFu << 56)
#define DD_TBI_TAGGED   ((uintptr_t)DD_TBI_TAG << 56)

static inline __attribute__((always_inline))
bool dd_sample_flag_check_fast(void *user, void **raw_out) {
    uintptr_t addr = (uintptr_t)user;
    if ((addr & DD_TBI_TAG_MASK) != DD_TBI_TAGGED) {
        return false;
    }
    *raw_out = (void *)(addr & ~DD_TBI_TAG_MASK);
    return true;
}

#else
#  error "dd_sample_flag: unsupported architecture (expected __x86_64__ or __aarch64__)"
#endif

#endif
