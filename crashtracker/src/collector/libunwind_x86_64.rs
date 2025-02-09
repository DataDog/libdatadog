// libunwind_x86_64.rs

// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libc;

#[repr(C)]
pub struct UnwContext(pub [u8; 1024]); // Placeholder size for x86_64 unw_context_t

#[repr(C)]
pub struct UnwCursor(pub [u8; 1024]); // Placeholder size for x86_64 unw_cursor_t

// This is a subset of the libunwind API. A more complete binding can be generated using bindgen.

extern "C" {
    #[link_name = "_Ux86_64_init_local"]
    pub fn unw_init_local(cursor: *mut UnwCursor, context: *mut UnwContext) -> i32;
    #[link_name = "_Ux86_64_step"]
    pub fn unw_step(cursor: *mut UnwCursor) -> i32;
    #[link_name = "_Ux86_64_get_reg"]
    pub fn unw_get_reg(cursor: *mut UnwCursor, reg: i32, valp: *mut u64) -> i32;
    #[link_name = "_Ux86_64_get_proc_name"]
    pub fn unw_get_proc_name(
        cursor: *mut UnwCursor,
        name: *mut libc::c_char,
        len: usize,
        offset: *mut u64,
    ) -> i32;
}

// x86_64 register definitions for libunwind
pub const UNW_REG_IP: i32 = 16; // Instruction Pointer
pub const UNW_REG_SP: i32 = 17; // Stack Pointer
