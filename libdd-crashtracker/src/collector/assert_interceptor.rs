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
//! 1. Formats the assertion expression, file, line, and function into a human-readable message in a
//!    fixed-size static buffer.
//! 2. Publishes the buffer length via an atomic for the crash signal handler.
//! 3. Calls the original libc function so the process aborts normally (raising `SIGABRT`).
//!
//! The signal handler in `crash_handler.rs` checks this stored message
//! when handling `SIGABRT` and includes it in the crash report.
//!
//! This module is gated on 64-bit Linux (`mod.rs`).

use core::sync::atomic::{
    AtomicUsize,
    Ordering::{Acquire, Relaxed, Release},
};

const ASSERT_BUF_CAP: usize = 1024;

/// Fixed-size buffer for the formatted assert message.
///
/// Only written by [`hook_assert_fail`] (on the asserting thread, before
/// `abort()`), and read by the signal handler after `SIGABRT` delivery.
/// Because the asserting thread is the one that receives `SIGABRT`, there
/// is no concurrent access: the write completes before the signal fires.
///
/// SAFETY: access is guarded by `ASSERT_LEN`: a non-zero length (stored
/// with `Release`) means the buffer contains that many valid UTF-8 bytes.
/// The reader loads the length with `Acquire` before reading the buffer.
static mut ASSERT_BUF: [u8; ASSERT_BUF_CAP] = [0; ASSERT_BUF_CAP];

/// Number of valid bytes in `ASSERT_BUF`. Zero means no message stored.
/// Stored with `Release` after writing the buffer; loaded with `Acquire`
/// before reading it.
static ASSERT_LEN: AtomicUsize = AtomicUsize::new(0);

/// Copy `src` bytes into `ASSERT_BUF` starting at `offset`, returning
/// the new offset (clamped to `ASSERT_BUF_CAP`).
///
/// SAFETY: caller must ensure exclusive access to `ASSERT_BUF`.
unsafe fn buf_append(offset: usize, src: &[u8]) -> usize {
    let remaining = ASSERT_BUF_CAP - offset;
    let n = src.len().min(remaining);
    // SAFETY: `offset..offset+n` is clamped above, and
    // we have exclusive access to `ASSERT_BUF` per caller contract.
    // Use `addr_of_mut!` to obtain a raw pointer without creating a
    // mutable reference to the static.
    unsafe {
        let buf_ptr = core::ptr::addr_of_mut!(ASSERT_BUF);
        core::ptr::copy_nonoverlapping(src.as_ptr(), (*buf_ptr).as_mut_ptr().add(offset), n);
    }
    offset + n
}

/// Format the assert message directly into the static buffer without
/// heap allocation. Truncates if the message exceeds `ASSERT_BUF_CAP`.
///
/// SAFETY: caller must ensure exclusive access to `ASSERT_BUF`.
unsafe fn store_assert_message(assertion: &str, file: &str, line: u32, function: &str) {
    let mut off = 0;
    // SAFETY: all calls share the caller's exclusive-access guarantee.
    unsafe {
        off = buf_append(off, b"Assertion failed: (");
        off = buf_append(off, assertion.as_bytes());
        off = buf_append(off, b"), ");
        if !function.is_empty() {
            off = buf_append(off, b"function ");
            off = buf_append(off, function.as_bytes());
            off = buf_append(off, b", ");
        }
        off = buf_append(off, b"file ");
        off = buf_append(off, file.as_bytes());
        off = buf_append(off, b", line ");
    }

    // Format the line number without allocating. A u32 is at most 10 digits.
    let mut line_buf = [0u8; 10];
    let line_str = format_u32(line, &mut line_buf);
    // SAFETY: same exclusive-access guarantee.
    unsafe {
        off = buf_append(off, line_str.as_bytes());
        off = buf_append(off, b".");
    }

    ASSERT_LEN.store(off, Release);
}

/// Format a `u32` into a caller-provided byte buffer, returning the
/// decimal string slice. No heap allocation.
fn format_u32(mut n: u32, buf: &mut [u8; 10]) -> &str {
    if n == 0 {
        buf[0] = b'0';
        // SAFETY: b'0' is valid UTF-8.
        return unsafe { core::str::from_utf8_unchecked(&buf[..1]) };
    }
    let mut pos = buf.len();
    while n > 0 {
        pos -= 1;
        buf[pos] = b'0' + (n % 10) as u8;
        n /= 10;
    }
    // SAFETY: digits 0-9 are valid UTF-8.
    unsafe { core::str::from_utf8_unchecked(&buf[pos..]) }
}

/// Return the stored assert message, if any.
///
/// Async-signal-safe (only an atomic load + a slice construction from
/// a static buffer). Resets the length to zero so subsequent calls
/// return `None`.
pub(crate) fn take_assert_message() -> Option<&'static str> {
    let len = ASSERT_LEN.swap(0, Acquire);
    if len == 0 {
        return None;
    }
    // SAFETY: `store_assert_message` wrote `len` bytes of valid UTF-8
    // into `ASSERT_BUF` before the `Release` store of `ASSERT_LEN`.
    // Our `Acquire` load synchronizes with that store, so the buffer
    // contents are visible. No concurrent writer exists: the hook runs
    // once before abort, and the signal handler (us) runs after.
    // Use `addr_of!` to avoid creating a reference to the mutable static.
    let slice = unsafe {
        let buf_ptr = core::ptr::addr_of!(ASSERT_BUF);
        core::slice::from_raw_parts((*buf_ptr).as_ptr(), len)
    };
    Some(unsafe { core::str::from_utf8_unchecked(slice) })
}

/// # Safety
/// `ptr` must be null or point to a valid null terminated C string.
unsafe fn cstr_to_str(ptr: *const libc::c_char, fallback: &str) -> &str {
    if ptr.is_null() {
        return fallback;
    }
    // SAFETY: caller guarantees `ptr` is null or points to a valid
    // NUL-terminated C string. The null case is handled above.
    unsafe { core::ffi::CStr::from_ptr(ptr) }
        .to_str()
        .unwrap_or(fallback)
}

type AssertFailFn = unsafe extern "C" fn(
    *const libc::c_char,
    *const libc::c_char,
    libc::c_uint,
    *const libc::c_char,
) -> !;

/// Resolved address of the original `__assert_fail`, set once during
/// [`install_assert_hook`].
static ORIG_ASSERT_FN: AtomicUsize = AtomicUsize::new(0);

/// Our replacement for `__assert_fail`, installed using GOT patching.
unsafe extern "C" fn hook_assert_fail(
    assertion: *const libc::c_char,
    file: *const libc::c_char,
    line: libc::c_uint,
    function: *const libc::c_char,
) -> ! {
    // SAFETY: these pointers come from libc's `__assert_fail` contract:
    // `assertion`, `file`, and `function` are valid NUL-terminated C
    // strings (or null), and remain valid for the duration of this call.
    let assertion_str = unsafe { cstr_to_str(assertion, "<unknown>") };
    let file_str = unsafe { cstr_to_str(file, "<unknown>") };
    let function_str = unsafe { cstr_to_str(function, "") };

    // SAFETY: this is the only writer to `ASSERT_BUF`. The hook runs on
    // the asserting thread just before `abort()`, so no concurrent access
    // is possible.
    unsafe { store_assert_message(assertion_str, file_str, line, function_str) };

    let orig = ORIG_ASSERT_FN.load(Acquire);
    if orig != 0 {
        // SAFETY: `orig` was stored by `install_assert_hook` from a
        // successful `hook_symbol` call, which resolved the real
        // `__assert_fail` address by `dlsym`/GOT lookup. The address
        // points to libc's `__assert_fail` which has the `AssertFailFn`
        // signature.
        let func: AssertFailFn = unsafe { core::mem::transmute::<usize, AssertFailFn>(orig) };
        // SAFETY: `func` is the original `__assert_fail` with matching
        // signature, and the arguments are forwarded unchanged from our
        // caller
        unsafe { func(assertion, file, line, function) }
    } else {
        // SAFETY: `abort` is always safe to call; it raises SIGABRT.
        unsafe { libc::abort() }
    }
}

/// Install the `__assert_fail` GOT hook across all currently loaded
/// libraries.
///
/// Safe to call multiple times; only the first call patches.
pub(crate) fn install_assert_hook() {
    if ORIG_ASSERT_FN.load(Relaxed) != 0 {
        return;
    }

    // SAFETY: hook_assert_fail has the same signature as __assert_fail.
    let result = unsafe {
        libdd_gotter::hook_symbol(c"__assert_fail", hook_assert_fail as *const () as usize)
    };

    if let Ok(hook) = result {
        ORIG_ASSERT_FN.store(hook.orig_addr, Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_assert_message_with_function() {
        // Reset any leftover state from other tests.
        let _ = take_assert_message();

        // SAFETY: single-threaded test, exclusive access to `ASSERT_BUF`.
        unsafe { store_assert_message("x > 0", "foo.c", 42, "bar") };
        let msg = take_assert_message().unwrap();
        assert_eq!(
            msg,
            "Assertion failed: (x > 0), function bar, file foo.c, line 42."
        );
    }

    #[test]
    fn test_format_assert_message_without_function() {
        let _ = take_assert_message();

        // SAFETY: single-threaded test, exclusive access to `ASSERT_BUF`.
        unsafe { store_assert_message("ptr != NULL", "main.c", 100, "") };
        let msg = take_assert_message().unwrap();
        assert_eq!(
            msg,
            "Assertion failed: (ptr != NULL), file main.c, line 100."
        );
    }

    #[test]
    fn test_store_and_take_assert_message() {
        let _ = take_assert_message();

        unsafe { store_assert_message("test", "test.c", 1, "") };
        let msg = take_assert_message();
        assert!(msg.is_some());
        assert_eq!(
            msg.unwrap(),
            "Assertion failed: (test), file test.c, line 1."
        );

        // Second take should return None.
        assert!(take_assert_message().is_none());
    }

    #[test]
    fn test_take_assert_message_none_when_unset() {
        let _ = take_assert_message();
        assert!(take_assert_message().is_none());
    }

    #[test]
    fn test_truncation() {
        let _ = take_assert_message();

        let long_expr = "x".repeat(ASSERT_BUF_CAP);
        unsafe { store_assert_message(&long_expr, "f.c", 1, "") };
        let msg = take_assert_message().unwrap();
        assert_eq!(msg.len(), ASSERT_BUF_CAP);
        assert!(msg.starts_with("Assertion failed: (xxxx"));
    }

    #[test]
    fn test_format_u32() {
        let mut buf = [0u8; 10];
        assert_eq!(format_u32(0, &mut buf), "0");
        assert_eq!(format_u32(1, &mut buf), "1");
        assert_eq!(format_u32(42, &mut buf), "42");
        assert_eq!(format_u32(4294967295, &mut buf), "4294967295");
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_install_assert_hook() {
        install_assert_hook();
        // On statically linked binaries, __assert_fail won't appear in
        // the dynamic symbol table and dlsym returns null. Verify the
        // hook doesn't crash regardless.
        let orig = ORIG_ASSERT_FN.load(Acquire);
        if orig == 0 {
            eprintln!(
                "note: __assert_fail not found in dynamic symbol table \
                 (static libc?), GOT hook not installed"
            );
        }
    }
}
