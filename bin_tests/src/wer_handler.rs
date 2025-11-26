// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! WER (Windows Error Reporting) handler for bin_tests.
//!
//! This module exports the WER callback function when built as a cdylib.
//! It's a thin wrapper that delegates to libdd-crashtracker's exception_event_callback.

use windows::Win32::Foundation::HANDLE;

/// WER out-of-process exception callback.
///
/// This function is called by Windows Error Reporting when a crash occurs.
/// It must be exported with C calling convention and this exact name for WER to find it.
///
/// # Parameters
/// * `context` - Address of WerContext in crashed process memory
/// * `process_handle` - Handle to the crashed process
/// * `thread_handle` - Handle to the crashed thread
/// * `_reserved` - Reserved for future use (unused)
///
/// # Returns
/// * `0` on success (WER continues processing)
/// * Non-zero on failure
///
/// # Safety
/// This function is called by Windows in a separate process context.
/// The handles are valid for the duration of the callback.
#[no_mangle]
pub extern "system" fn OutOfProcessExceptionEventCallback(
    context: usize,
    process_handle: HANDLE,
    thread_handle: HANDLE,
    _reserved: usize,
) -> u32 {
    // Call into libdd-crashtracker's exception handler
    match libdd_crashtracker::exception_event_callback(context, process_handle, thread_handle) {
        Ok(_) => {
            // Successfully processed crash
            0
        }
        Err(e) => {
            // Log error (WER may capture stderr)
            eprintln!("WER callback error: {:?}", e);
            1
        }
    }
}
