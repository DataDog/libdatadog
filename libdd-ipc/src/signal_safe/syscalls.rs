// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
use core::arch::asm;

/// A direct Linux syscall returning negative errno, without accessing libc or TLS.
///
/// # Safety
/// Arguments must satisfy the selected syscall's pointer and ownership requirements.
#[allow(clippy::too_many_arguments)]
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub unsafe fn raw_syscall6(
    number: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    let result: isize;
    // Linux x86_64 uses r10, not the C ABI's rcx, for argument four. `syscall` itself clobbers rcx
    // and r11; declaring both prevents the surrounding Rust from keeping live values there.
    asm!(
        "syscall",
        inlateout("rax") number as isize => result,
        in("rdi") arg1,
        in("rsi") arg2,
        in("rdx") arg3,
        in("r10") arg4,
        in("r8") arg5,
        in("r9") arg6,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack, preserves_flags),
    );
    result
}

/// A direct Linux syscall returning negative errno, without accessing libc or TLS.
///
/// # Safety
/// Arguments must satisfy the selected syscall's pointer and ownership requirements.
#[allow(clippy::too_many_arguments)]
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn raw_syscall6(
    number: usize,
    arg1: usize,
    arg2: usize,
    arg3: usize,
    arg4: usize,
    arg5: usize,
    arg6: usize,
) -> isize {
    let result: isize;
    // Linux AArch64 takes the syscall number in x8, arguments in x0..x5, and returns in x0.
    // Listing every register explicitly keeps the compiler from generating a helper call.
    asm!(
        "svc #0",
        in("x8") number,
        inlateout("x0") arg1 as isize => result,
        in("x1") arg2,
        in("x2") arg3,
        in("x3") arg4,
        in("x4") arg5,
        in("x5") arg6,
        options(nostack),
    );
    result
}

/// A direct Linux syscall returning negative errno, without accessing libc or TLS.
///
/// # Safety
/// Arguments must satisfy the selected syscall's pointer and ownership requirements.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[allow(clippy::too_many_arguments)]
pub unsafe fn raw_syscall6(
    _number: usize,
    _a: usize,
    _b: usize,
    _c: usize,
    _d: usize,
    _e: usize,
    _f: usize,
) -> isize {
    -(libc::ENOTSUP as isize)
}
