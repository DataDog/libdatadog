// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! libunwind-based stack unwinding for musl Linux.
//!
//! This module provides stack unwinding using libunwind, which can unwind
//! from an arbitrary `ucontext_t` (such as the one received in a signal handler).
//!
//! Unlike the `backtrace` crate which only unwinds from the current context,
//! this allows us to unwind the crashed thread's stack starting from the
//! exact point where the crash occurred.
//!
//! # Key Features
//!
//! - Unwinds from an arbitrary `ucontext_t` (not just current context)
//! - Handles ASLR automatically
//! - Unwinds through dynamically loaded libraries (including musl libc)
//! - Uses DWARF CFI / .eh_frame information
//!
//! # Platform Support
//!
//! Currently supports x86_64 Linux (musl). The module is conditionally compiled
//! only on supported platforms.

use super::frame_pointer_walker::RawFrame;
use super::platform::MAX_BACKTRACE_FRAMES;
use libc::ucontext_t;
use std::ffi::c_int;

/// libunwind register numbers for x86_64
#[cfg(target_arch = "x86_64")]
mod registers {
    pub const UNW_X86_64_RIP: i32 = 16;
    pub const UNW_X86_64_RSP: i32 = 7;
    pub const UNW_X86_64_RBP: i32 = 6;
}

/// libunwind register numbers for aarch64
#[cfg(target_arch = "aarch64")]
mod registers {
    // On aarch64, PC is register 30, SP is 31
    pub const UNW_AARCH64_PC: i32 = 30;
    pub const UNW_AARCH64_SP: i32 = 31;
    pub const UNW_AARCH64_FP: i32 = 29; // X29 = Frame Pointer
}

/// Size of the unw_cursor_t structure.
/// From libunwind-x86_64.h: #define UNW_TDEP_CURSOR_LEN 127
/// Each element is unw_word_t (u64), so total size is 127 * 8 = 1016 bytes
const UNW_CURSOR_SIZE: usize = 127;

/// Opaque cursor type matching libunwind's unw_cursor_t
#[repr(C)]
struct UnwCursor {
    opaque: [u64; UNW_CURSOR_SIZE],
}

impl Default for UnwCursor {
    fn default() -> Self {
        Self {
            opaque: [0u64; UNW_CURSOR_SIZE],
        }
    }
}

/// Flag indicating we're unwinding from a signal frame
const UNW_INIT_SIGNAL_FRAME: c_int = 1;

// FFI bindings to libunwind
// On x86_64, the "local" unwinding symbols are prefixed with _ULx86_64_
// (UL = Unwind Local, for unwinding within the same process)
#[cfg(target_arch = "x86_64")]
#[link(name = "unwind")]
extern "C" {
    // Initialize cursor from ucontext with flags
    // int unw_init_local2(unw_cursor_t *, unw_context_t *, int flags)
    // UNW_INIT_SIGNAL_FRAME = 1 tells libunwind this is a signal frame context
    fn _ULx86_64_init_local2(
        cursor: *mut UnwCursor,
        context: *const ucontext_t,
        flags: c_int,
    ) -> c_int;

    // Step to previous frame (returns >0 if more frames, 0 if done, <0 on error)
    fn _ULx86_64_step(cursor: *mut UnwCursor) -> c_int;

    // Get register value
    fn _ULx86_64_get_reg(cursor: *const UnwCursor, reg: c_int, value: *mut u64) -> c_int;
}

#[cfg(target_arch = "aarch64")]
#[link(name = "unwind")]
extern "C" {
    fn _ULaarch64_init_local2(
        cursor: *mut UnwCursor,
        context: *const ucontext_t,
        flags: c_int,
    ) -> c_int;
    fn _ULaarch64_step(cursor: *mut UnwCursor) -> c_int;
    fn _ULaarch64_get_reg(cursor: *const UnwCursor, reg: c_int, value: *mut u64) -> c_int;
}

/// Safe wrapper around libunwind's init_local2 with signal frame flag
#[cfg(target_arch = "x86_64")]
unsafe fn unw_init_local(cursor: *mut UnwCursor, context: *const ucontext_t) -> c_int {
    _ULx86_64_init_local2(cursor, context, UNW_INIT_SIGNAL_FRAME)
}

#[cfg(target_arch = "aarch64")]
unsafe fn unw_init_local(cursor: *mut UnwCursor, context: *const ucontext_t) -> c_int {
    _ULaarch64_init_local2(cursor, context, UNW_INIT_SIGNAL_FRAME)
}

/// Safe wrapper around libunwind's step
#[cfg(target_arch = "x86_64")]
unsafe fn unw_step(cursor: *mut UnwCursor) -> c_int {
    _ULx86_64_step(cursor)
}

#[cfg(target_arch = "aarch64")]
unsafe fn unw_step(cursor: *mut UnwCursor) -> c_int {
    _ULaarch64_step(cursor)
}

/// Safe wrapper around libunwind's get_reg
#[cfg(target_arch = "x86_64")]
unsafe fn unw_get_reg(cursor: *const UnwCursor, reg: c_int, value: *mut u64) -> c_int {
    _ULx86_64_get_reg(cursor, reg, value)
}

#[cfg(target_arch = "aarch64")]
unsafe fn unw_get_reg(cursor: *const UnwCursor, reg: c_int, value: *mut u64) -> c_int {
    _ULaarch64_get_reg(cursor, reg, value)
}

/// Get the instruction pointer from the cursor
#[cfg(target_arch = "x86_64")]
unsafe fn get_ip(cursor: &UnwCursor) -> usize {
    let mut ip: u64 = 0;
    if unw_get_reg(cursor, registers::UNW_X86_64_RIP, &mut ip) >= 0 {
        ip as usize
    } else {
        0
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn get_ip(cursor: &UnwCursor) -> usize {
    let mut ip: u64 = 0;
    if unw_get_reg(cursor, registers::UNW_AARCH64_PC, &mut ip) >= 0 {
        ip as usize
    } else {
        0
    }
}

/// Get the stack pointer from the cursor
#[cfg(target_arch = "x86_64")]
unsafe fn get_sp(cursor: &UnwCursor) -> usize {
    let mut sp: u64 = 0;
    if unw_get_reg(cursor, registers::UNW_X86_64_RSP, &mut sp) >= 0 {
        sp as usize
    } else {
        0
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn get_sp(cursor: &UnwCursor) -> usize {
    let mut sp: u64 = 0;
    if unw_get_reg(cursor, registers::UNW_AARCH64_SP, &mut sp) >= 0 {
        sp as usize
    } else {
        0
    }
}

/// Get the base/frame pointer from the cursor
#[cfg(target_arch = "x86_64")]
unsafe fn get_bp(cursor: &UnwCursor) -> usize {
    let mut bp: u64 = 0;
    if unw_get_reg(cursor, registers::UNW_X86_64_RBP, &mut bp) >= 0 {
        bp as usize
    } else {
        0
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn get_bp(cursor: &UnwCursor) -> usize {
    let mut bp: u64 = 0;
    if unw_get_reg(cursor, registers::UNW_AARCH64_FP, &mut bp) >= 0 {
        bp as usize
    } else {
        0
    }
}

/// Unwind the stack starting from the given ucontext.
///
/// This function uses libunwind to walk the stack frames, starting from
/// the register state captured in `ucontext` (typically from a signal handler).
///
/// # Arguments
///
/// * `ucontext` - Pointer to the ucontext_t from the signal handler
/// * `frames` - Output buffer to store collected frames
///
/// # Returns
///
/// The number of frames collected.
///
/// # Safety
///
/// The ucontext pointer must be valid and point to a properly initialized
/// ucontext_t structure (typically obtained from a signal handler).
pub unsafe fn unwind_from_ucontext(ucontext: *const ucontext_t, frames: &mut [RawFrame]) -> usize {
    if ucontext.is_null() {
        return 0;
    }

    let max_frames = frames.len().min(MAX_BACKTRACE_FRAMES);
    if max_frames == 0 {
        return 0;
    }

    let mut cursor = UnwCursor::default();

    // Initialize the cursor from the ucontext
    // On x86_64, unw_context_t is typedef'd to ucontext_t, so we can pass it directly
    if unw_init_local(&mut cursor, ucontext) < 0 {
        return 0;
    }

    let mut count = 0;
    let mut prev_sp: usize = 0;

    // Collect frames by stepping through the stack
    loop {
        if count >= max_frames {
            break;
        }

        // Get current frame's registers
        let ip = get_ip(&cursor);
        let sp = get_sp(&cursor);
        let bp = get_bp(&cursor);

        // Validate the frame to catch corruption
        // IP of 0 means we've reached the end
        if ip == 0 {
            break;
        }

        // IP should be in a reasonable address range (user space)
        // On x86_64 Linux, user space is typically below 0x800000000000
        // Addresses like 0x6462696c2f736563 are clearly garbage (ASCII strings)
        const MAX_USER_SPACE_ADDR: usize = 0x800000000000;
        if ip > MAX_USER_SPACE_ADDR {
            break;
        }

        // Very basic sanity check: IP should be page-aligned or at least
        // not be a very small value (which would be null page area)
        if ip < 0x1000 {
            break;
        }

        // SP should generally increase as we unwind (stack grows down)
        // Allow first frame to set the baseline
        if count > 0 && sp != 0 && prev_sp != 0 {
            // If SP goes backwards by more than a megabyte, something is wrong
            if sp < prev_sp && prev_sp - sp > 0x100000 {
                break;
            }
        }

        // Store the frame
        frames[count] = RawFrame { ip, sp, bp };
        count += 1;
        prev_sp = sp;

        // Step to the previous frame
        // Returns: >0 if there are more frames, 0 if we're at the end, <0 on error
        let step_result = unw_step(&mut cursor);
        if step_result <= 0 {
            break;
        }
    }

    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unwind_from_current_context() {
        // Create a ucontext for the current thread
        let mut context: ucontext_t = unsafe { std::mem::zeroed() };

        // Get the current context using getcontext()
        // This is different from a signal handler context, but tests the basic functionality
        unsafe {
            if libc::getcontext(&mut context) != 0 {
                panic!("getcontext failed");
            }
        }

        let mut frames = [RawFrame::default(); 32];
        let count = unsafe { unwind_from_ucontext(&context, &mut frames) };

        // We should get at least a few frames (test function, test harness, etc.)
        assert!(count >= 1, "Expected at least 1 frame, got {}", count);

        // First frame should have a valid IP
        assert!(frames[0].ip != 0, "First frame IP should not be 0");
    }
}
