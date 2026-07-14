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
 * Live-heap tracking master switch. build.rs defines this to 0 or 1 from
 * the `live-heap` cargo feature. Default to 1 when unset so raw-C
 * compilation and bindgen (which don't pass -D) see the full-featured
 * layout. When 0, sampled allocations are not flagged and frees are not
 * sampled: allocation profiling only. See allocation_*.{h,c}.
 */
#ifndef DD_HEAP_LIVE_TRACKING
#define DD_HEAP_LIVE_TRACKING 1
#endif

/*
 * Per-thread initialisation required by the flagging scheme.
 * Must be called once per thread before dd_sample_flag_apply/check are used.
 * On arm64 this issues prctl(PR_SET_TAGGED_ADDR_CTRL) so tagged pointers
 * survive syscalls; on other architectures this is a no-op.
 *
 * Returns true when the thread is safe to sample and false when the
 * flagging scheme is unavailable (e.g. arm64 kernel/seccomp rejected
 * PR_SET_TAGGED_ADDR_CTRL). Callers must treat a false return as
 * "disable sampling on this thread" and never call
 * dd_sample_flag_apply on it, otherwise tagged pointers will be
 * rejected by the kernel with EFAULT on the next syscall.
 */
bool dd_sample_flag_thread_init(void);

/*
 * Apply the sampled flag. Takes the raw pointer from the underlying
 * allocator and returns the user-visible pointer to hand to the
 * application. On architectures that offset the user pointer inside a
 * bumped allocation (x86-64), the offset is picked to satisfy the
 * caller-requested alignment.
 */
void *dd_sample_flag_apply(void *raw, size_t alignment);

/*
 * Non-destructive variant of dd_sample_flag_check_and_clear. Useful for realloc:
 * callers can resolve the raw pointer before calling the underlying
 * realloc, while leaving the old allocation's flag intact in case
 * realloc fails and the old allocation remains live.
 *
 * If `user` is sampled, returns true and fills raw_out / offset_out
 * without clearing the allocation's flag header.
 */
bool dd_sample_flag_peek(void *user, void **raw_out, size_t *offset_out);

/*
 * Largest alignment the sampler will honor. Above this we pass the
 * allocation through unsampled: the header + slack overhead grows with
 * alignment and stops being proportionate to any observability gain.
 * Sized in bytes; kept below one typical 4 KiB page so x86-64 never
 * returns a sampled pointer at page offset 0, which its fast-path
 * checker deliberately rejects.
 */
#define DD_SAMPLE_ALIGNMENT_CAP 1024

#if defined(__x86_64__)

#define DD_HEADER_BYTES 16
#define DD_PAGE_SIZE    4096

/*
 * x86-64 marks sampled allocations with a 16-byte header written
 * immediately before the user pointer. The header stores an 8-byte
 * magic word plus an 8-byte offset from `raw` to `user`, so recovery
 * at free time is direct: `raw = user - offset`.
 *
 * Layout:
 *   [raw ... slack ...] [magic(8) | offset(8)] [user_data ...]
 *                        ^ user - 16              ^ user
 *
 * The user pointer is placed at `raw + N` where
 *
 *   N = max(alignment, DD_HEADER_BYTES)
 *
 * plus one further `alignment`-sized bump when `raw + N` would land at
 * page offset < DD_HEADER_BYTES. That bump preserves the invariant
 * that the fast-path filter (`user & (PAGE-1) < 16 => unsampled`)
 * relies on to safely read the 16 header bytes without ever
 * dereferencing an unmapped previous page. Since `alignment` is a
 * power of two, adding another `alignment` bytes preserves the
 * requested user-pointer alignment.
 *
 * Compared with the previous two-magic (A/B) scheme the offset is now
 * stored explicitly, which drops one branch on the free path and
 * generalises cleanly to arbitrary caller alignments (Rust `Layout`,
 * `posix_memalign`, `aligned_alloc`). Overhead in the common
 * alignment <= 16 case is 16 bytes per sampled allocation, down from
 * the previous 32.
 */

#define DD_MAGIC 0xfab1eddec0dedca7ULL

/*
 * Bytes reserved before the user pointer for the header + alignment slack:
 * max(alignment, DD_HEADER_BYTES). x86_apply may add one more of these as a
 * page-boundary bump (see x86_apply).
 */
static inline __attribute__((always_inline))
size_t x86_base_offset(size_t alignment) {
    return alignment > DD_HEADER_BYTES ? alignment : DD_HEADER_BYTES;
}

/*
 * Single source of truth for the bumped allocation size of a sampled
 * `user_size`-byte request at `alignment`. Reserves 2 * base_offset (header
 * plus room for the page-boundary bump) and rounds up to a multiple of
 * alignment so aligned_alloc's size constraint holds. Returns false (leaving
 * *out_size untouched) if alignment exceeds the cap or the arithmetic would
 * overflow SIZE_MAX. bumped_alloc_size (allocation_requested.c) and
 * dd_allocation_freed_slow (allocation_freed.c) both call this so the formula
 * can never drift between the alloc and free sides.
 */
static inline __attribute__((always_inline))
bool x86_bumped_size(size_t user_size, size_t alignment, size_t *out_size) {
    if (alignment > DD_SAMPLE_ALIGNMENT_CAP) return false;
    size_t base = x86_base_offset(alignment);
    if (base > SIZE_MAX / 2) return false;
    size_t reserve = base * 2;
    if (reserve > SIZE_MAX - user_size) return false;
    size_t bumped = user_size + reserve;
    if (alignment > 1) {
        size_t mask = alignment - 1;
        if (bumped > SIZE_MAX - mask) return false;
        bumped = (bumped + mask) & ~mask;
    }
    *out_size = bumped;
    return true;
}

/*
 * The 16-byte header stamped immediately before a sampled user pointer:
 * an 8-byte magic followed by the 8-byte offset from `user` back to `raw`.
 * Read/written as one struct so the layout lives in a single place instead
 * of being open-coded as two memcpys at every site.
 */
typedef struct {
    uint64_t magic;
    uint64_t offset;
} x86_header_t;

_Static_assert(sizeof(x86_header_t) == DD_HEADER_BYTES,
               "x86_header_t must exactly fill the reserved header");

/* Stamp the header in front of `user` (offset = user - raw). */
static inline __attribute__((always_inline))
void x86_header_stamp(void *user, uint64_t offset) {
    x86_header_t hdr = { DD_MAGIC, offset };
    memcpy((char *)user - DD_HEADER_BYTES, &hdr, sizeof(hdr));
}

/* Read the header in front of `user`. */
static inline __attribute__((always_inline))
x86_header_t x86_header_read(const void *user) {
    x86_header_t hdr;
    memcpy(&hdr, (const char *)user - DD_HEADER_BYTES, sizeof(hdr));
    return hdr;
}

/* Zero the header so a later reuse of this block isn't misdetected as sampled. */
static inline __attribute__((always_inline))
void x86_header_clear(void *user) {
    memset((char *)user - DD_HEADER_BYTES, 0, sizeof(x86_header_t));
}

/*
 * Layout helpers. x86_apply and x86_raw_from_user MUST be each other's
 * inverse: the offset stamped at apply time is what lets
 * x86_raw_from_user recover the same raw at check time.
 */

/*
 * Pick the user pointer within the bumped allocation backing `raw`
 * such that:
 *   - `user - raw >= DD_HEADER_BYTES` (room for the header),
 *   - `user` is `alignment`-aligned,
 *   - `user & (DD_PAGE_SIZE - 1) >= DD_HEADER_BYTES` (fast-path filter
 *     never treats a sampled allocation as unsampled).
 *
 * Stamp (magic, offset) at `user - 16` so x86_raw_from_user can
 * recover `raw` at free time without any per-allocation metadata
 * beyond the header itself.
 *
 * The caller is responsible for ensuring `raw` is aligned to
 * `alignment` (via aligned_alloc/posix_memalign) or, when
 * `alignment <= DD_HEADER_BYTES`, that `raw` is at least
 * DD_HEADER_BYTES-aligned (malloc's default on x86-64 glibc/musl).
 */
static inline __attribute__((always_inline))
void *x86_apply(void *raw, size_t alignment) {
    uintptr_t r = (uintptr_t)raw;
    size_t n = x86_base_offset(alignment);
    uintptr_t u = r + n;
    if ((u & (DD_PAGE_SIZE - 1)) < DD_HEADER_BYTES) {
        n += x86_base_offset(alignment);
        u = r + n;
    }
    x86_header_stamp((void *)u, (uint64_t)n);
    return (void *)u;
}

/*
 * Inverse of x86_apply: given the offset recovered from the header,
 * return the original raw pointer to hand back to the underlying free.
 */
static inline __attribute__((always_inline))
void *x86_raw_from_user(void *user, uint64_t offset) {
    return (void *)((uintptr_t)user - (uintptr_t)offset);
}

/*
 * If `user` was previously returned by dd_sample_flag_apply, write the
 * raw pointer (to pass to the underlying free) into *raw_out and return
 * true. Otherwise leave *raw_out untouched and return false.
 */
static inline __attribute__((always_inline))
bool dd_sample_flag_check_and_clear(void *user, void **raw_out) {
    if (((uintptr_t)user & (DD_PAGE_SIZE - 1)) < DD_HEADER_BYTES) {
        return false;
    }

    x86_header_t hdr = x86_header_read(user);
    if (hdr.magic != DD_MAGIC) {
        return false;
    }
    if (hdr.offset < DD_HEADER_BYTES || hdr.offset > 2 * DD_SAMPLE_ALIGNMENT_CAP) {
        return false;
    }

    /* Clear the whole 16-byte header so a re-use of this address
     * (e.g. allocator returns the same block to a later, unsampled
     * allocation whose user data happens to encode the magic) doesn't
     * masquerade as a stale sampled allocation. */
    x86_header_clear(user);

    *raw_out = x86_raw_from_user(user, hdr.offset);
    return true;
}

#elif defined(__aarch64__)

#define DD_TBI_TAG      0xDDu
#define DD_TBI_TAG_MASK ((uintptr_t)0xFFu << 56)
#define DD_TBI_TAGGED   ((uintptr_t)DD_TBI_TAG << 56)

/*
 * If `user` was previously returned by dd_sample_flag_apply, write the
 * raw pointer (to pass to the underlying free) into *raw_out and return
 * true. Otherwise leave *raw_out untouched and return false.
 */
static inline __attribute__((always_inline))
bool dd_sample_flag_check_and_clear(void *user, void **raw_out) {
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
