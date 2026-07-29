// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Intercepts C `assert()` failures to capture the assertion expression
//! before the process aborts.
//!
//! During crashtracker initialization, [`install_assert_hook`] patches
//! the GOT entries for `__assert_fail` across all loaded libraries so
//! that calls are routed through our hook. This works regardless of
//! library load order (no `LD_PRELOAD` or link-order constraints).
//!
//! The hook:
//! 1. Formats the assertion expression, file, line, and function into a human-readable message.
//! 2. Stores the message in an `AtomicPtr` for the crash signal handler.
//! 3. Calls the original libc function so the process aborts normally (raising `SIGABRT`).
//!
//! The signal handler in `crash_handler.rs` checks this stored message
//! when handling `SIGABRT` and includes it in the crash report.
//!
//! Currently only supported on 64-bit Linux.

#![cfg(unix)]

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering::SeqCst};

static ASSERT_MESSAGE: AtomicPtr<String> = AtomicPtr::new(ptr::null_mut());

/// Store an assert-failure message for later retrieval by the signal handler.
///
/// Intentionally leaks the old message (if any): this runs on a path
/// that leads immediately to `abort()`, so freeing memory is unnecessary
/// and calling `free` in the signal handler is not async-signal-safe.
fn store_assert_message(message: String) {
    let new_ptr = Box::into_raw(Box::new(message));
    let _ = ASSERT_MESSAGE.swap(new_ptr, SeqCst);
}

/// Atomically take the stored assert message pointer, leaving null.
///
/// Async-signal-safe (only an atomic swap). The caller borrows the
/// returned pointer without reconstructing the `Box`, avoiding `free`
/// inside the signal handler.
pub(crate) fn take_assert_message_ptr() -> *mut String {
    ASSERT_MESSAGE.swap(ptr::null_mut(), SeqCst)
}

fn format_assert_message(assertion: &str, file: &str, line: u32, function: &str) -> String {
    if function.is_empty() {
        alloc::format!("Assertion failed: ({assertion}), file {file}, line {line}.")
    } else {
        alloc::format!(
            "Assertion failed: ({assertion}), function {function}, file {file}, line {line}."
        )
    }
}

/// # Safety
/// `ptr` must be null or point to a valid null terminated C string.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
unsafe fn cstr_to_str(ptr: *const libc::c_char, fallback: &str) -> &str {
    if ptr.is_null() {
        return fallback;
    }
    unsafe { core::ffi::CStr::from_ptr(ptr) }
        .to_str()
        .unwrap_or(fallback)
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
type AssertFailFn = unsafe extern "C" fn(
    *const libc::c_char,
    *const libc::c_char,
    libc::c_uint,
    *const libc::c_char,
) -> !;

/// Resolved address of the original `__assert_fail`, set once during
/// [`install_assert_hook`].
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
static ORIG_ASSERT_FN: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// Our replacement for `__assert_fail`, installed using GOT patching.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
unsafe extern "C" fn hook_assert_fail(
    assertion: *const libc::c_char,
    file: *const libc::c_char,
    line: libc::c_uint,
    function: *const libc::c_char,
) -> ! {
    let assertion_str = unsafe { cstr_to_str(assertion, "<unknown>") };
    let file_str = unsafe { cstr_to_str(file, "<unknown>") };
    let function_str = unsafe { cstr_to_str(function, "") };

    let message = format_assert_message(assertion_str, file_str, line, function_str);
    store_assert_message(message);

    let orig = ORIG_ASSERT_FN.load(core::sync::atomic::Ordering::Acquire);
    if orig != 0 {
        let func: AssertFailFn = unsafe { core::mem::transmute::<usize, AssertFailFn>(orig) };
        unsafe { func(assertion, file, line, function) }
    } else {
        unsafe { libc::abort() }
    }
}

/// Install the `__assert_fail` GOT hook across all currently loaded
/// libraries.
///
/// Safe to call multiple times; only the first call patches.
///
/// Only supported on 64-bit Linux. On other platforms this is a no-op.
#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
pub(crate) fn install_assert_hook() {
    if ORIG_ASSERT_FN.load(core::sync::atomic::Ordering::Acquire) != 0 {
        return;
    }

    let mut orig_addr: usize = 0;
    // SAFETY: hook_assert_fail has the same signature as __assert_fail.
    let patched = unsafe {
        libdd_gotter::hook_symbol(
            c"__assert_fail",
            hook_assert_fail as *const () as usize,
            &mut orig_addr,
        )
    };

    if patched && orig_addr != 0 {
        ORIG_ASSERT_FN.store(orig_addr, core::sync::atomic::Ordering::Release);
    }
}

/// No-op on unsupported platforms.
#[cfg(not(all(target_os = "linux", target_pointer_width = "64")))]
pub(crate) fn install_assert_hook() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_assert_message_with_function() {
        let msg = format_assert_message("x > 0", "foo.c", 42, "bar");
        assert_eq!(
            msg,
            "Assertion failed: (x > 0), function bar, file foo.c, line 42."
        );
    }

    #[test]
    fn test_format_assert_message_without_function() {
        let msg = format_assert_message("ptr != NULL", "main.c", 100, "");
        assert_eq!(
            msg,
            "Assertion failed: (ptr != NULL), file main.c, line 100."
        );
    }

    #[test]
    fn test_store_and_take_assert_message() {
        let _ = take_assert_message_ptr();

        store_assert_message("test assert".to_string());
        let ptr = take_assert_message_ptr();
        assert!(!ptr.is_null());

        let message = unsafe { &*ptr };
        assert_eq!(message, "test assert");

        assert!(take_assert_message_ptr().is_null());

        unsafe { drop(Box::from_raw(ptr)) };
    }

    #[test]
    fn test_take_assert_message_ptr_null_when_unset() {
        let old = take_assert_message_ptr();
        if !old.is_null() {
            unsafe { drop(Box::from_raw(old)) };
        }
        assert!(take_assert_message_ptr().is_null());
    }

    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_install_assert_hook() {
        install_assert_hook();
        // On statically linked binaries, __assert_fail won't appear in
        // the dynamic symbol table and dlsym returns null. Verify the
        // hook doesn't crash regardless.
        let orig = ORIG_ASSERT_FN.load(core::sync::atomic::Ordering::Acquire);
        if orig == 0 {
            eprintln!(
                "note: __assert_fail not found in dynamic symbol table \
                 (static libc?), GOT hook not installed"
            );
        }
    }
}
