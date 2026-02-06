// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub type UnwContext = libc::ucontext_t;

pub type UnwWord = u64;

// Opaque cursor structure
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UnwCursor {
    pub opaque: [UnwWord; 4096],
}

extern "C" {
    #[link_name = "_Uaarch64_getcontext"]
    pub fn unw_getcontext(context: *mut UnwContext) -> i32;
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

pub const UNW_REG_IP: i32 = 32; // Instruction Pointer
pub const UNW_REG_SP: i32 = 31; // Stack Pointer
pub const UNW_INIT_LOCAL_ONLY_IP: i32 = 1;
