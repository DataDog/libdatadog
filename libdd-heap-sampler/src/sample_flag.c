#include <datadog/heap/sample_flag.h>

#if defined(__x86_64__)

/* ---- amd64 : header-magic -------------------------------------------------
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

size_t dd_sample_flag_overhead(void) {
    /* TODO: return 16 once the header scheme is implemented. */
    return 0;
}

void *dd_sample_flag_apply(void *raw) {
    /* TODO: write magic at raw, return raw + HEADER_BYTES. */
    return raw;
}

bool dd_sample_flag_check(void *user, void **raw_out) {
    (void)user; (void)raw_out;
    /* TODO: check magic header, recover raw pointer. */
    return false;
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
 * Caveats: pick DD_TAG to avoid collision with HWASan/MTE/libc use;
 * newer kernels accept tagged pointers for most syscalls via
 * prctl(PR_SET_TAGGED_ADDR_CTRL).
 * -------------------------------------------------------------------------- */

size_t dd_sample_flag_overhead(void) {
    return 0;
}

void *dd_sample_flag_apply(void *raw) {
    /* TODO: tag top byte of raw. */
    return raw;
}

bool dd_sample_flag_check(void *user, void **raw_out) {
    (void)user; (void)raw_out;
    /* TODO: check TBI tag, recover raw pointer. */
    return false;
}

#else
#  error "dd_sample_flag: unsupported architecture (expected __x86_64__ or __aarch64__)"
#endif
