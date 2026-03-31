// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

// Context is platform ucontext_t (from libc)
pub type UnwContext = libc::ucontext_t;

pub type UnwWord = u64;

// Opaque cursor structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UnwCursor {
    pub opaque: [UnwWord; 127],
}

// This is a subset of the libunwind API.

extern "C" {
    #[link_name = "_ULx86_64_init_local2"]
    pub fn unw_init_local2(cursor: *mut UnwCursor, context: *mut UnwContext, flag: i32) -> i32;
    #[link_name = "_ULx86_64_step"]
    pub fn unw_step(cursor: *mut UnwCursor) -> i32;
    #[link_name = "_ULx86_64_get_reg"]
    pub fn unw_get_reg(cursor: *mut UnwCursor, reg: i32, valp: *mut UnwWord) -> i32;
    #[link_name = "_ULx86_64_get_proc_name"]
    pub fn unw_get_proc_name(
        cursor: *mut UnwCursor,
        name: *mut libc::c_char,
        len: usize,
        offset: *mut u64,
    ) -> i32;
    #[link_name = "unw_backtrace2"]
    pub fn unw_backtrace2(
        buffer: *mut *mut ::std::os::raw::c_void,
        size: i32,
        context: *mut UnwContext,
        flag: i32,
    ) -> i32;
}

// x86_64 register definitions for libunwind
pub const UNW_REG_IP: i32 = 16; // Instruction Pointer
pub const UNW_REG_SP: i32 = 17; // Stack Pointer
pub const UNW_REG_FP: i32 = 18; // Frame Pointer
pub const UNW_INIT_LOCAL_ONLY_IP: i32 = 1;

/// Saves the current CPU context into `uc_mcontext.gregs`.
/// gregs layout: [R8, R9, R10, R11, R12, R13, R14, R15,
///                RDI, RSI, RBP, RBX, RDX, RAX, RCX, RSP, RIP, ...]
///
/// # Safety
/// `context` must be a valid, non-null pointer to a zeroed or initialized `UnwContext`.
// This is only for testing purposes and allow the tests to work with libc and musl-libc
#[cfg(test)]
#[inline(always)]
pub unsafe fn getcontext(context: *mut UnwContext) -> i32 {
    let gregs = core::ptr::addr_of_mut!((*context).uc_mcontext.gregs) as u64;
    core::arch::asm!(
        "mov [rdi], r8",
        "mov [rdi + 8], r9",
        "mov [rdi + 16], r10",
        "mov [rdi + 24], r11",
        "mov [rdi + 32], r12",
        "mov [rdi + 40], r13",
        "mov [rdi + 48], r14",
        "mov [rdi + 56], r15",
        "mov [rdi + 64], rdi",
        "mov [rdi + 72], rsi",
        "mov [rdi + 80], rbp",
        "mov [rdi + 88], rbx",
        "mov [rdi + 96], rdx",
        "mov [rdi + 104], rax",
        "mov [rdi + 112], rcx",
        "mov [rdi + 120], rsp",
        "lea rax, [rip + 2f]",
        "mov [rdi + 128], rax",
        "2:",
        inout("rdi") gregs => _,
        out("rax") _,
        options(nostack, preserves_flags),
    );
    0
}
