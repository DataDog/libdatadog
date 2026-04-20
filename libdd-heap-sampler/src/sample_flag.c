#include <datadog/heap/sample_flag.h>

#include <stdint.h>
#include <string.h>

#if defined(__x86_64__)

void dd_sample_flag_thread_init(void) { /* nothing needed on x86-64 */ }

/* ---- amd64 : header magic -------------------------------------------------
 *
 * dd_sample_flag_overhead : return HEADER_BYTES (16 so user pointer stays
 *   16-byte aligned, matching malloc's guarantee on x86-64).
 *
 * dd_sample_flag_apply(raw):
 *   1. Write DD_MAGIC as a u64 at raw.
 *   2. Return raw + HEADER_BYTES.
 *
 * dd_sample_flag_check(user, &raw_out):
 *   1. Read the u64 at user - HEADER_BYTES.
 *   2. If it equals DD_MAGIC, clear the magic (so a reused region
 *      won't falsely report sampled), set *raw_out = user - HEADER_BYTES,
 *      return true.
 *   3. Else return false.
 * -------------------------------------------------------------------------- */

#define DD_HEADER_BYTES 16

/* "fabled decoded cat" — if you are going to have magic numbers, they better be good.
 * unique enough we shouldn't be accidentally recognising things as tracked allocations.
 */
#define DD_MAGIC        0xfab1eddec0dedca7ULL

/* Smallest mainstream page size; used to guard against reading across a
 * page boundary into potentially-unmapped memory when the user pointer
 * sits near the start of its page. Over-conservative (4 KiB) on systems
 * with larger pages (e.g. 16 KiB on newer aarch64), which just means a
 * slightly higher false-negative rate — never unsafe. */
#define DD_PAGE_SIZE    4096

size_t dd_sample_flag_overhead(void) {
    return DD_HEADER_BYTES;
}

void *dd_sample_flag_apply(void *raw) {
    const uint64_t magic = DD_MAGIC;
    memcpy(raw, &magic, sizeof(magic));
    return (char *)raw + DD_HEADER_BYTES;
}

bool dd_sample_flag_check(void *user, void **raw_out) {
    /* Skip the magic read if it would cross into the previous page.
     * Sampled allocations almost never place the user pointer within
     * DD_HEADER_BYTES of a page boundary (it'd require the raw pointer
     * to sit at offset PAGE_SIZE - 16 of its page, which is rare in
     * practice), so this costs us the occasional missed free event
     * rather than a meaningful sampling bias. */
    if (((uintptr_t)user & (DD_PAGE_SIZE - 1)) < DD_HEADER_BYTES) {
        return false;
    }

    void *header = (char *)user - DD_HEADER_BYTES;
    uint64_t magic;
    memcpy(&magic, header, sizeof(magic));
    if (magic != DD_MAGIC) {
        return false;
    }
    /* Clear so a region reused by a later unsampled allocation won't
     * falsely report sampled on its free. */
    const uint64_t zero = 0;
    memcpy(header, &zero, sizeof(zero));
    *raw_out = header;
    return true;
}

#elif defined(__aarch64__)

/* ---- arm64 : TBI pointer tagging ------------------------------------------
 *
 * ARMv8 userspace ignores the top byte of a virtual address. We stash a
 * magic byte in bits 56..63 of the returned pointer; the allocation
 * itself is untouched -- no size bump, no header writes.
 *
 * dd_sample_flag_overhead : return 0 (we tag pointer bits, not memory).
 *
 * dd_sample_flag_apply(raw):
 *   1. Return (raw | (DD_TAG << 56)).
 *
 * dd_sample_flag_check(user, &raw_out):
 *   1. Inspect bits 56..63 of user.
 *   2. If they match DD_TAG, set *raw_out = user with the top byte
 *      cleared, return true.
 *   3. Else return false.
 *
 *  We need to set a `prctl` so the pointers we return with the bits flipped work
 *  over syscalls: `prctl(PR_SET_TAGGED_ADDR_CTRL)`
 *
 * Caveats:
 *  * TODO We need to work out how this interacts with HWASan/MTE/libc in practice.
 * -------------------------------------------------------------------------- */

/* 0xDD — "Datadog". Sits in the high half of the tag byte range to avoid
 * the low tags used by HWASan (0x01–0x7F) and MTE (4-bit, 0x00–0x0F). */
#define DD_TBI_TAG      0xDDu
#define DD_TBI_TAG_MASK ((uintptr_t)0xFFu << 56)
#define DD_TBI_TAGGED   ((uintptr_t)DD_TBI_TAG << 56)

size_t dd_sample_flag_overhead(void) {
    return 0;
}

void *dd_sample_flag_apply(void *raw) {
    return (void *)((uintptr_t)raw | DD_TBI_TAGGED);
}

bool dd_sample_flag_check(void *user, void **raw_out) {
    uintptr_t addr = (uintptr_t)user;
    if ((addr & DD_TBI_TAG_MASK) != DD_TBI_TAGGED) {
        return false;
    }
    *raw_out = (void *)(addr & ~DD_TBI_TAG_MASK);
    return true;
}

#if defined(__linux__)
#include <sys/prctl.h>

void dd_sample_flag_thread_init(void) {
    prctl(PR_SET_TAGGED_ADDR_CTRL, PR_TAGGED_ADDR_ENABLE, 0, 0, 0);
}
#else
void dd_sample_flag_thread_init(void) { }
#endif

#else
#  error "dd_sample_flag: unsupported architecture (expected __x86_64__ or __aarch64__)"
#endif
