// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#include <datadog/heap/sample_flag.h>

#if defined(__x86_64__)

/*
 * x86-64 layout details live in sample_flag.h alongside DD_MAGIC and
 * the x86 helper pair. Summary: bump the size to reserve room for a
 * 16-byte (magic, offset) header plus alignment slack, let the helper
 * pick the user pointer inside the bumped region such that user is
 * alignment-aligned and user & (PAGE-1) >= 16, and stamp (magic,
 * offset) at user - 16. On free the offset stored in the header lets
 * us recover raw directly.
 */

bool dd_sample_flag_thread_init(void) {
    /* Nothing needed on x86-64; always safe. */
    return true;
}

void *dd_sample_flag_apply(void *raw, size_t alignment) {
    return x86_apply(raw, alignment);
}

bool dd_sample_flag_peek(void *user, void **raw_out, size_t *offset_out) {
    if (((uintptr_t)user & (DD_PAGE_SIZE - 1)) < DD_HEADER_BYTES) {
        return false;
    }

    void *header = (char *)user - DD_HEADER_BYTES;
    uint64_t magic;
    memcpy(&magic, header, sizeof(magic));
    if (magic != DD_MAGIC) {
        return false;
    }

    uint64_t offset;
    memcpy(&offset, (char *)header + sizeof(magic), sizeof(offset));
    if (offset < DD_HEADER_BYTES || offset > 2 * DD_SAMPLE_ALIGNMENT_CAP) {
        return false;
    }

    *raw_out = x86_raw_from_user(user, offset);
    *offset_out = (size_t)offset;
    return true;
}

#elif defined(__aarch64__)

/*
 * arm64: TBI (Top Byte Ignore) pointer tag.
 *
 * ARMv8 userspace ignores bits 56..63 of a virtual address, so we stash
 * DD_TBI_TAG in those bits to mark sampled allocations. The allocation
 * itself is untouched (no size bump, no header writes), so overhead is 0.
 *
 * On apply: OR DD_TBI_TAGGED into the pointer and return it.
 * On check: test bits 56..63; if they match DD_TBI_TAG, clear them to
 *   recover the raw pointer, write it to *raw_out, and return true.
 *
 * Tagged pointers must survive kernel boundaries (e.g. passing a tagged
 * pointer to read/write). dd_sample_flag_thread_init calls
 * prctl(PR_SET_TAGGED_ADDR_CTRL) on Linux to enable this. Without it the
 * kernel would reject tagged pointers with EFAULT.
 *
 * TODO: audit interaction with HWASan and MTE, which also use the top byte.
 */

void *dd_sample_flag_apply(void *raw, size_t alignment) {
    (void)alignment;
    return (void *)((uintptr_t)raw | DD_TBI_TAGGED);
}

bool dd_sample_flag_peek(void *user, void **raw_out, size_t *offset_out) {
    if (!dd_sample_flag_check(user, raw_out)) {
        return false;
    }
    *offset_out = 0;
    return true;
}

#if defined(__linux__)
#include <sys/prctl.h>

/* PR_SET_TAGGED_ADDR_CTRL / PR_TAGGED_ADDR_ENABLE only reached glibc in
 * 2.31 (Feb 2020); some of our internal CI images (notably the
 * libddprof-build centos image) pin an older glibc whose
 * <sys/prctl.h> does not declare them, breaking the build with an
 * undeclared-identifier error even though the kernel underneath (any
 * Linux ≥ 5.4) accepts the syscall fine. The prctl numbers are stable
 * kernel ABI — verified against `linux/prctl.h` — so define them
 * locally when the libc header does not. */
#ifndef PR_SET_TAGGED_ADDR_CTRL
#  define PR_SET_TAGGED_ADDR_CTRL 55
#endif
#ifndef PR_TAGGED_ADDR_ENABLE
#  define PR_TAGGED_ADDR_ENABLE (1UL << 0)
#endif

bool dd_sample_flag_thread_init(void) {
#if DD_HEAP_LIVE_TRACKING
    /* prctl returns 0 on success. On failure (older kernel without
     * PR_SET_TAGGED_ADDR_CTRL, seccomp filter blocking it, MTE
     * conflict, ...) tagged pointers would be rejected by the kernel
     * with EFAULT the next time one crosses a syscall. Report failure
     * so the caller disables sampling on this thread. */
    return prctl(PR_SET_TAGGED_ADDR_CTRL, PR_TAGGED_ADDR_ENABLE, 0, 0, 0) == 0;
#else
    /* No pointer tagging without live-heap tracking, so the tagged-address
     * prctl isn't needed; nothing can reject an untagged pointer. */
    return true;
#endif
}
#else
bool dd_sample_flag_thread_init(void) {
    /* No tagging on non-Linux arm64 targets; sampling is always safe. */
    return true;
}
#endif

#else
#  error "dd_sample_flag: unsupported architecture (expected __x86_64__ or __aarch64__)"
#endif