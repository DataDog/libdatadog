// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Platform detection utilities for crash tracking.
//!
//! This module provides utilities to detect the target platform and libc implementation
//! at compile time, enabling platform-specific behavior for backtrace collection.

// This module is primarily used on musl targets. Allow dead code on other platforms.
#![cfg_attr(not(target_env = "musl"), allow(dead_code))]

/// Returns true if compiled against musl libc.
///
/// On musl targets, DWARF-based stack unwinding may fail because musl is typically
/// built without unwind tables (`-fno-unwind-tables`). This function allows the
/// crash tracker to detect this at compile time and use alternative strategies.
#[cfg(target_env = "musl")]
pub const fn is_musl() -> bool {
    true
}

#[cfg(not(target_env = "musl"))]
pub const fn is_musl() -> bool {
    false
}

/// Minimum number of frames expected in a valid backtrace.
///
/// If a backtrace contains fewer frames than this, it's likely that unwinding
/// failed (common on musl) and we should attempt fallback strategies.
///
/// A typical crash backtrace should have at least:
/// 1. The crashing function
/// 2. The caller
/// 3. Some runtime/main frame
pub const MIN_EXPECTED_FRAMES: usize = 3;

/// Maximum number of frames to collect in a backtrace.
///
/// This limit prevents runaway frame walking in case of stack corruption.
pub const MAX_BACKTRACE_FRAMES: usize = 128;

/// Returns true if the current target architecture uses frame pointers by convention.
///
/// Note: Even on architectures that support frame pointers, they may be omitted
/// unless compiled with `-C force-frame-pointers=yes` or equivalent.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub const fn supports_frame_pointer_walking() -> bool {
    true
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub const fn supports_frame_pointer_walking() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_musl_detection_compiles() {
        // This test just verifies the function compiles and returns a bool
        let _ = is_musl();
    }

    #[test]
    fn test_frame_pointer_support() {
        #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
        assert!(supports_frame_pointer_walking());
    }

    #[test]
    fn test_constants_are_reasonable() {
        assert!(MIN_EXPECTED_FRAMES >= 1);
        assert!(MAX_BACKTRACE_FRAMES >= MIN_EXPECTED_FRAMES);
        assert!(MAX_BACKTRACE_FRAMES <= 1024); // Sanity check
    }
}
