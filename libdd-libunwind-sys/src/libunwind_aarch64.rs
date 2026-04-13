// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub type UnwContext = libc::ucontext_t;

pub type UnwWord = u64;

// Opaque cursor structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UnwCursor {
    pub opaque: [UnwWord; 250],
}

extern "C" {
    #[link_name = "_ULaarch64_init_local2"]
    pub fn unw_init_local2(cursor: *mut UnwCursor, context: *mut UnwContext, flag: i32) -> i32;
    #[link_name = "_ULaarch64_step"]
    pub fn unw_step(cursor: *mut UnwCursor) -> i32;
    #[link_name = "_ULaarch64_get_reg"]
    pub fn unw_get_reg(cursor: *mut UnwCursor, reg: i32, valp: *mut UnwWord) -> i32;
    #[link_name = "_ULaarch64_get_proc_name"]
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

pub const UNW_REG_IP: i32 = 30; // Instruction Pointer
pub const UNW_REG_SP: i32 = 31; // Stack Pointer
pub const UNW_REG_FP: i32 = 29; // Frame Pointer
pub const UNW_INIT_LOCAL_ONLY_IP: i32 = 1;

/// Saves the current CPU context into `uc_mcontext.regs`.
/// On aarch64 libunwind does not emit a callable symbol for getcontext —
/// it uses a C preprocessor macro with inline assembly. This is the Rust
/// equivalent: save all GPRs, SP, LR, and PC into `uc_mcontext`.
///
/// # Safety
/// `context` must be a valid, non-null pointer to a zeroed or initialized `UnwContext`.
// This is only for testing purposes and allow the tests to work with libc and musl-libc
#[cfg(test)]
#[inline(always)]
pub unsafe fn getcontext(context: *mut UnwContext) -> i32 {
    let base = core::ptr::addr_of_mut!((*context).uc_mcontext.regs) as u64;
    let ret: u64;
    core::arch::asm!(
        "stp x0, x1, [x0, #0]",
        "stp x2, x3, [x0, #16]",
        "stp x4, x5, [x0, #32]",
        "stp x6, x7, [x0, #48]",
        "stp x8, x9, [x0, #64]",
        "stp x10, x11, [x0, #80]",
        "stp x12, x13, [x0, #96]",
        "stp x14, x15, [x0, #112]",
        "stp x16, x17, [x0, #128]",
        "stp x18, x19, [x0, #144]",
        "stp x20, x21, [x0, #160]",
        "stp x22, x23, [x0, #176]",
        "stp x24, x25, [x0, #192]",
        "stp x26, x27, [x0, #208]",
        "stp x28, x29, [x0, #224]",
        "mov x1, sp",
        "stp x30, x1, [x0, #240]",
        "adr x1, 2f",
        "str x1, [x0, #256]",
        "mov x0, #0",
        "2:",
        inout("x0") base => ret,
        out("x1") _,
        options(nostack, preserves_flags),
    );
    ret as i32
}
