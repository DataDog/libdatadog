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
//! # Why libunwind?
//!
//! On musl Linux, the `backtrace` crate's integration with libunwind is unreliable.
//! By wrapping libunwind directly, we:
//! - Unwind from an arbitrary `ucontext_t` (not just current context)
//! - Handle ASLR automatically
//! - Unwind through dynamically loaded libraries (including musl libc)
//! - Use DWARF CFI / .eh_frame information (no frame pointers required)
//!
//! # Platform Support
//!
//! Supports x86_64 and aarch64 Linux (musl). Conditionally compiled only on
//! `target_env = "musl"`.

use super::frame_pointer_walker::RawFrame;
use super::platform::MAX_BACKTRACE_FRAMES;
use libc::ucontext_t;
use std::ffi::c_int;

// =============================================================================
// libunwind cursor and constants
// =============================================================================

/// Size of the unw_cursor_t structure (127 * sizeof(u64) = 1016 bytes).
/// From libunwind headers: #define UNW_TDEP_CURSOR_LEN 127
const UNW_CURSOR_SIZE: usize = 127;

/// Flag for unw_init_local2: indicates we're unwinding from a signal frame.
const UNW_INIT_SIGNAL_FRAME: c_int = 1;

/// Maximum valid user-space address on x86_64 Linux (48-bit virtual addressing).
const MAX_USER_SPACE_ADDR: usize = 0x800000000000;

/// Opaque cursor type matching libunwind's unw_cursor_t.
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

// =============================================================================
// x86_64 FFI bindings
// =============================================================================

#[cfg(target_arch = "x86_64")]
mod ffi {
    use super::*;

    /// libunwind register numbers for x86_64
    pub const REG_IP: c_int = 16; // RIP
    pub const REG_SP: c_int = 7; // RSP
    pub const REG_BP: c_int = 6; // RBP

    #[link(name = "unwind")]
    extern "C" {
        pub fn _ULx86_64_init_local2(
            cursor: *mut UnwCursor,
            context: *const ucontext_t,
            flags: c_int,
        ) -> c_int;
        pub fn _ULx86_64_step(cursor: *mut UnwCursor) -> c_int;
        pub fn _ULx86_64_get_reg(cursor: *const UnwCursor, reg: c_int, value: *mut u64) -> c_int;
    }

    #[inline]
    pub unsafe fn init_local(cursor: *mut UnwCursor, context: *const ucontext_t) -> c_int {
        _ULx86_64_init_local2(cursor, context, UNW_INIT_SIGNAL_FRAME)
    }

    #[inline]
    pub unsafe fn step(cursor: *mut UnwCursor) -> c_int {
        _ULx86_64_step(cursor)
    }

    #[inline]
    pub unsafe fn get_reg(cursor: *const UnwCursor, reg: c_int) -> u64 {
        let mut value: u64 = 0;
        if _ULx86_64_get_reg(cursor, reg, &mut value) >= 0 {
            value
        } else {
            0
        }
    }
}

// =============================================================================
// aarch64 FFI bindings
// =============================================================================

#[cfg(target_arch = "aarch64")]
mod ffi {
    use super::*;

    /// libunwind register numbers for aarch64
    pub const REG_IP: c_int = 30; // PC
    pub const REG_SP: c_int = 31; // SP
    pub const REG_BP: c_int = 29; // X29 (FP)

    #[link(name = "unwind")]
    extern "C" {
        pub fn _ULaarch64_init_local2(
            cursor: *mut UnwCursor,
            context: *const ucontext_t,
            flags: c_int,
        ) -> c_int;
        pub fn _ULaarch64_step(cursor: *mut UnwCursor) -> c_int;
        pub fn _ULaarch64_get_reg(cursor: *const UnwCursor, reg: c_int, value: *mut u64) -> c_int;
    }

    #[inline]
    pub unsafe fn init_local(cursor: *mut UnwCursor, context: *const ucontext_t) -> c_int {
        _ULaarch64_init_local2(cursor, context, UNW_INIT_SIGNAL_FRAME)
    }

    #[inline]
    pub unsafe fn step(cursor: *mut UnwCursor) -> c_int {
        _ULaarch64_step(cursor)
    }

    #[inline]
    pub unsafe fn get_reg(cursor: *const UnwCursor, reg: c_int) -> u64 {
        let mut value: u64 = 0;
        if _ULaarch64_get_reg(cursor, reg, &mut value) >= 0 {
            value
        } else {
            0
        }
    }
}

// =============================================================================
// Public API
// =============================================================================

/// Unwind the stack starting from the given ucontext.
///
/// Uses libunwind to walk the stack frames, starting from the register state
/// captured in `ucontext` (typically from a signal handler). This unwinds the
/// *crashed* thread's stack, not the signal handler's stack.
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
    if ffi::init_local(&mut cursor, ucontext) < 0 {
        return 0;
    }

    let mut count = 0;
    let mut prev_sp: usize = 0;

    loop {
        if count >= max_frames {
            break;
        }

        // Get current frame's registers
        let ip = ffi::get_reg(&cursor, ffi::REG_IP) as usize;
        let sp = ffi::get_reg(&cursor, ffi::REG_SP) as usize;
        let bp = ffi::get_reg(&cursor, ffi::REG_BP) as usize;

        // Validate frame to catch corruption
        if !is_valid_frame(ip, sp, prev_sp, count) {
            break;
        }

        frames[count] = RawFrame { ip, sp, bp };
        count += 1;
        prev_sp = sp;

        // Step to the previous frame (>0 = more frames, 0 = done, <0 = error)
        if ffi::step(&mut cursor) <= 0 {
            break;
        }
    }

    count
}

/// Validate that a frame looks reasonable.
#[inline]
fn is_valid_frame(ip: usize, sp: usize, prev_sp: usize, frame_idx: usize) -> bool {
    // IP must not be zero (end of stack)
    if ip == 0 {
        return false;
    }

    // IP must be in user space
    if ip > MAX_USER_SPACE_ADDR {
        return false;
    }

    // IP must not be in null page area
    if ip < 0x1000 {
        return false;
    }

    // SP should generally increase as we unwind (stack grows down)
    if frame_idx > 0 && sp != 0 && prev_sp != 0 {
        // If SP goes backwards by more than 1MB, something is wrong
        if sp < prev_sp && prev_sp - sp > 0x100000 {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_null_ucontext() {
        let mut frames = [RawFrame::default(); 32];
        let count = unsafe { unwind_from_ucontext(std::ptr::null(), &mut frames) };
        assert_eq!(count, 0);
    }

    #[test]
    fn test_is_valid_frame() {
        // Zero IP is invalid
        assert!(!is_valid_frame(0, 0x7fff0000, 0, 0));
        // IP in null page is invalid
        assert!(!is_valid_frame(0x100, 0x7fff0000, 0, 0));
        // IP above user space is invalid
        assert!(!is_valid_frame(0x900000000000, 0x7fff0000, 0, 0));
        // Valid IP
        assert!(is_valid_frame(0x555555554000, 0x7fff0000, 0, 0));
    }
}
