// libunwind_arm.rs

// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libc;

#[repr(C)]
pub struct UnwContext(pub [u8; 512]); // Placeholder size for ARM unw_context_t

#[repr(C)]
pub struct UnwCursor(pub [u8; 512]); // Placeholder size for ARM unw_cursor_t

// This is a subset of the libunwind API. A more complete binding can be generated using bindgen.
extern "C" {
    #[link_name = "_Uarm_init_local"]
    pub fn unw_init_local(cursor: *mut UnwCursor, context: *mut UnwContext) -> i32;
    #[link_name = "_Uarm_step"]
    pub fn unw_step(cursor: *mut UnwCursor) -> i32;
    #[link_name = "_Uarm_get_reg"]
    pub fn unw_get_reg(cursor: *mut UnwCursor, reg: i32, valp: *mut u64) -> i32;
    #[link_name = "_Uarm_get_proc_name"]
    pub fn unw_get_proc_name(
        cursor: *mut UnwCursor,
        name: *mut libc::c_char,
        len: usize,
        offset: *mut u64,
    ) -> i32;
}

// ARM register definitions for libunwind
pub const UNW_REG_IP: i32 = 12; // ARM's Instruction Pointer (PC)
pub const UNW_REG_SP: i32 = 13; // ARM's Stack Pointer (SP)
