// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Frame pointer-based stack walking for musl Linux.
//!
//! This module provides an alternative to DWARF-based stack unwinding by walking
//! the stack using frame pointers (base pointers). This is useful on musl Linux
//! where the standard backtrace mechanisms fail due to missing unwind tables in libc.
//!
//! # Requirements
//!
//! For frame pointer walking to work correctly, code must be compiled with frame
//! pointers enabled. In Rust, this is done with:
//! - `RUSTFLAGS="-C force-frame-pointers=yes"`
//!
//! # Safety
//!
//! This module contains unsafe code that directly reads memory addresses. All
//! functions that dereference pointers validate addresses before reading to
//! prevent crashes from corrupted stacks.
//!
//! # Signal Safety
//!
//! The functions in this module are designed to be async-signal-safe. They:
//! - Do not allocate memory (caller provides output buffer)
//! - Do not use locks or mutexes
//! - Only perform direct memory reads

// This module is primarily used on musl targets. Allow dead code on other platforms.
#![cfg_attr(not(target_env = "musl"), allow(dead_code))]

use super::platform::MAX_BACKTRACE_FRAMES;
use libc::ucontext_t;

/// A raw stack frame containing register values.
///
/// This represents a single frame in the call stack, containing the
/// instruction pointer and frame pointer at that point.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawFrame {
    /// Instruction pointer (return address for this frame)
    pub ip: usize,
    /// Stack pointer at this frame
    pub sp: usize,
    /// Base/frame pointer at this frame
    pub bp: usize,
}

/// Context extracted from ucontext_t for starting frame pointer walking.
///
/// Contains the register values at the point of the crash/signal.
#[derive(Debug, Clone, Copy)]
pub struct FrameContext {
    /// Instruction pointer (RIP on x86_64, PC on aarch64)
    pub ip: usize,
    /// Stack pointer (RSP on x86_64, SP on aarch64)
    pub sp: usize,
    /// Base/frame pointer (RBP on x86_64, FP/X29 on aarch64)
    pub bp: usize,
}

impl FrameContext {
    /// Extract frame context from a ucontext_t pointer.
    ///
    /// # Safety
    ///
    /// The ucontext pointer must be valid and point to a properly initialized
    /// ucontext_t structure (typically provided by a signal handler).
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    pub unsafe fn from_ucontext(ucontext: *const ucontext_t) -> Option<Self> {
        if ucontext.is_null() {
            return None;
        }

        let mctx = &(*ucontext).uc_mcontext;
        Some(FrameContext {
            ip: mctx.gregs[libc::REG_RIP as usize] as usize,
            sp: mctx.gregs[libc::REG_RSP as usize] as usize,
            bp: mctx.gregs[libc::REG_RBP as usize] as usize,
        })
    }

    /// Extract frame context from a ucontext_t pointer.
    ///
    /// # Safety
    ///
    /// The ucontext pointer must be valid and point to a properly initialized
    /// ucontext_t structure (typically provided by a signal handler).
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    pub unsafe fn from_ucontext(ucontext: *const ucontext_t) -> Option<Self> {
        if ucontext.is_null() {
            return None;
        }

        let mctx = &(*ucontext).uc_mcontext;
        Some(FrameContext {
            ip: mctx.pc as usize,
            sp: mctx.sp as usize,
            // On aarch64, x29 is the frame pointer
            bp: mctx.regs[29] as usize,
        })
    }

    /// Extract frame context from a ucontext_t pointer.
    ///
    /// # Safety
    ///
    /// The ucontext pointer must be valid and point to a properly initialized
    /// ucontext_t structure (typically provided by a signal handler).
    #[cfg(target_os = "macos")]
    pub unsafe fn from_ucontext(ucontext: *const ucontext_t) -> Option<Self> {
        if ucontext.is_null() {
            return None;
        }

        let mcontext = (*ucontext).uc_mcontext;
        if mcontext.is_null() {
            return None;
        }

        #[cfg(target_arch = "x86_64")]
        {
            let ss = &(*mcontext).__ss;
            Some(FrameContext {
                ip: ss.__rip as usize,
                sp: ss.__rsp as usize,
                bp: ss.__rbp as usize,
            })
        }

        #[cfg(target_arch = "aarch64")]
        {
            let ss = &(*mcontext).__ss;
            Some(FrameContext {
                ip: ss.__pc as usize,
                sp: ss.__sp as usize,
                bp: ss.__fp as usize,
            })
        }
    }
}

/// Check if a memory address is likely readable.
///
/// This performs a basic validation that the address:
/// 1. Is not null/zero
/// 2. Is properly aligned for pointer access
/// 3. Is in a reasonable range (not too low, which would be unmapped)
///
/// Note: This does NOT guarantee the address is actually readable. A more
/// robust check would use mincore() or parse /proc/self/maps, but those
/// are not async-signal-safe.
#[inline]
fn is_likely_valid_address(addr: usize) -> bool {
    // Null or very low addresses are invalid (first page is typically unmapped)
    if addr < 4096 {
        return false;
    }

    // Check alignment for pointer-sized reads
    if addr % core::mem::size_of::<usize>() != 0 {
        return false;
    }

    // On 64-bit systems, user space addresses are typically in the lower half
    // of the address space. Kernel addresses start with 0xFFFF on x86_64.
    #[cfg(target_pointer_width = "64")]
    {
        // User space addresses should not have the high bit set
        // (kernel space starts at 0xFFFF800000000000 on x86_64 Linux)
        if addr > 0x0000_7FFF_FFFF_FFFF {
            return false;
        }
    }

    true
}

/// Validate that a frame pointer looks reasonable.
///
/// A valid frame pointer should:
/// 1. Be a valid address
/// 2. Be greater than the stack pointer (stack grows downward)
/// 3. Not be too far from the stack pointer (reasonable stack frame size)
#[inline]
fn is_valid_frame_pointer(bp: usize, sp: usize) -> bool {
    if !is_likely_valid_address(bp) {
        return false;
    }

    // Frame pointer should be >= stack pointer (stack grows down)
    if bp < sp {
        return false;
    }

    // Frame pointer shouldn't be too far from stack pointer
    // (assuming max stack frame size of 1MB)
    const MAX_FRAME_SIZE: usize = 1024 * 1024;
    if bp.saturating_sub(sp) > MAX_FRAME_SIZE {
        return false;
    }

    true
}

/// Walk the stack using frame pointers.
///
/// This function walks the call stack by following the chain of frame pointers.
/// On x86_64, each stack frame is laid out as:
///
/// ```text
/// High addresses
/// +------------------+
/// | return address   |  <- bp + 8 (sizeof pointer)
/// +------------------+
/// | saved bp         |  <- bp points here
/// +------------------+
/// | local variables  |
/// +------------------+
/// Low addresses       <- sp points somewhere here
/// ```
///
/// On aarch64, the frame record is similar but at the frame pointer:
/// ```text
/// +------------------+
/// | return address   |  <- fp + 8
/// +------------------+
/// | saved fp         |  <- fp points here
/// +------------------+
/// ```
///
/// # Arguments
///
/// * `context` - The initial frame context extracted from ucontext
/// * `frames` - Output buffer to store collected frames
///
/// # Returns
///
/// The number of frames collected.
///
/// # Safety
///
/// This function reads memory at addresses derived from the frame context.
/// The caller must ensure the context contains valid register values
/// (typically from a signal handler's ucontext).
pub unsafe fn walk_frame_pointers(context: &FrameContext, frames: &mut [RawFrame]) -> usize {
    let max_frames = frames.len().min(MAX_BACKTRACE_FRAMES);
    if max_frames == 0 {
        return 0;
    }

    let mut count = 0;

    // First frame is from the context itself
    frames[count] = RawFrame {
        ip: context.ip,
        sp: context.sp,
        bp: context.bp,
    };
    count += 1;

    let mut current_bp = context.bp;
    let mut current_sp = context.sp;

    while count < max_frames {
        // Validate the frame pointer before dereferencing
        if !is_valid_frame_pointer(current_bp, current_sp) {
            break;
        }

        // Read the return address (at bp + sizeof(pointer))
        let return_addr_ptr = current_bp + core::mem::size_of::<usize>();
        if !is_likely_valid_address(return_addr_ptr) {
            break;
        }

        let return_addr = *(return_addr_ptr as *const usize);

        // A zero return address indicates end of stack
        if return_addr == 0 {
            break;
        }

        // Validate the return address looks like code
        if !is_likely_valid_address(return_addr) {
            break;
        }

        // Read the saved frame pointer (at *bp)
        let saved_bp = *(current_bp as *const usize);

        // The saved BP should be higher than current BP (stack grows down)
        // or zero (end of chain)
        if saved_bp != 0 && saved_bp <= current_bp {
            break;
        }

        frames[count] = RawFrame {
            ip: return_addr,
            sp: current_bp + 2 * core::mem::size_of::<usize>(), // Approximate SP
            bp: saved_bp,
        };
        count += 1;

        // Move to the next frame
        current_sp = current_bp;
        current_bp = saved_bp;

        // Zero BP means we've reached the end of the chain
        if current_bp == 0 {
            break;
        }
    }

    count
}

/// Walk the stack using frame pointers and return frames in a Vec.
///
/// This is a convenience wrapper around `walk_frame_pointers` that allocates
/// storage for the frames.
///
/// # Safety
///
/// Same requirements as `walk_frame_pointers`.
///
/// # Note
///
/// This function allocates memory and should NOT be used in signal handlers.
/// Use `walk_frame_pointers` with a pre-allocated buffer instead.
#[cfg(test)]
pub unsafe fn walk_frame_pointers_alloc(context: &FrameContext) -> Vec<RawFrame> {
    let mut frames = vec![RawFrame::default(); MAX_BACKTRACE_FRAMES];
    let count = walk_frame_pointers(context, &mut frames);
    frames.truncate(count);
    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_likely_valid_address() {
        // Null is invalid
        assert!(!is_likely_valid_address(0));

        // Low addresses are invalid
        assert!(!is_likely_valid_address(100));
        assert!(!is_likely_valid_address(4095));

        // Unaligned addresses are invalid
        assert!(!is_likely_valid_address(4097)); // 4096 + 1

        // Reasonable addresses should be valid
        assert!(is_likely_valid_address(0x7FFF_0000_0000));

        // Kernel addresses should be invalid
        #[cfg(target_pointer_width = "64")]
        assert!(!is_likely_valid_address(0xFFFF_8000_0000_0000));
    }

    #[test]
    fn test_is_valid_frame_pointer() {
        // BP below SP is invalid (stack grows down)
        assert!(!is_valid_frame_pointer(0x1000, 0x2000));

        // BP equal to SP is valid
        assert!(is_valid_frame_pointer(0x7FFF_0000_1000, 0x7FFF_0000_1000));

        // BP slightly above SP is valid
        assert!(is_valid_frame_pointer(0x7FFF_0000_1100, 0x7FFF_0000_1000));

        // BP way above SP (> 1MB) is invalid
        assert!(!is_valid_frame_pointer(0x7FFF_0010_0000, 0x7FFF_0000_0000));
    }

    #[test]
    fn test_raw_frame_default() {
        let frame = RawFrame::default();
        assert_eq!(frame.ip, 0);
        assert_eq!(frame.sp, 0);
        assert_eq!(frame.bp, 0);
    }

    // Note: Testing actual frame walking requires either:
    // 1. Compiling with frame pointers enabled
    // 2. Using inline assembly to set up a known stack layout
    // These tests are better done as integration tests
}
