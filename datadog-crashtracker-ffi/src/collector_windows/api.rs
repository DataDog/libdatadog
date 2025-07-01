// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::Metadata;
use anyhow::Result;
use ddcommon::Endpoint;
use ddcommon_ffi::slice::AsBytes;
use ddcommon_ffi::CharSlice;
use std::ffi::c_void;
use windows::core::{HRESULT, HSTRING};
use windows::Win32::Foundation::{BOOL, E_FAIL, S_OK};
use windows::Win32::System::Diagnostics::Debug::OutputDebugStringW;
use windows::Win32::System::ErrorReporting::WER_RUNTIME_EXCEPTION_INFORMATION;

#[no_mangle]
#[must_use]
#[cfg(target_os = "windows")]
/// Initialize the crash-tracking infrastructure.
///
/// # Preconditions
///   None.
/// # Safety
///   Crash-tracking functions are not reentrant.
///   No other crash-handler functions should be called concurrently.
/// # Atomicity
///   This function is not atomic. A crash during its execution may lead to
///   unexpected crash-handling behaviour.
pub extern "C" fn ddog_crasht_init_windows(
    module: CharSlice,
    endpoint: Option<&Endpoint>,
    metadata: Metadata,
) -> bool {
    let result: Result<(), _> = (|| {
        datadog_crashtracker::init_crashtracking_windows(
            module.try_to_string()?,
            endpoint,
            metadata.try_into()?,
        )
    })();

    if let Err(e) = result {
        output_debug_string(format!("ddog_crasht_init_windows failed: {e}").as_str());
        return false;
    }

    true
}

fn output_debug_string(message: &str) {
    unsafe { OutputDebugStringW(&HSTRING::from(message)) };
}

#[no_mangle]
#[cfg(target_os = "windows")]
pub extern "C" fn OutOfProcessExceptionEventSignatureCallback(
    _context: *const c_void,
    _exception_information: *const WER_RUNTIME_EXCEPTION_INFORMATION,
    _index: i32,
    _name: *mut u16,
    _name_size: *mut u32,
    _value: *mut u16,
    _value_size: *mut u32,
) -> HRESULT {
    // This callback is not supposed to be called by WER because we don't claim the crash,
    // but we need to define it anyway because WER checks for its presence.
    output_debug_string("OutOfProcessExceptionEventSignatureCallback");
    S_OK
}

#[no_mangle]
#[cfg(target_os = "windows")]
pub extern "C" fn OutOfProcessExceptionEventDebuggerLaunchCallback(
    _context: *const c_void,
    _exception_information: *const WER_RUNTIME_EXCEPTION_INFORMATION,
    _is_custom_debugger: *mut BOOL,
    _debugger_launch: *mut u16,
    _debugger_launch_size: *mut u32,
    _is_debugger_auto_launch: *mut BOOL,
) -> HRESULT {
    // This callback is not supposed to be called by WER because we don't claim the crash,
    // but we need to define it anyway because WER checks for its presence.
    output_debug_string("OutOfProcessExceptionEventDebuggerLaunchCallback");
    S_OK
}

#[no_mangle]
#[cfg(target_os = "windows")]
pub extern "C" fn OutOfProcessExceptionEventCallback(
    context: *const c_void,
    exception_information: *const WER_RUNTIME_EXCEPTION_INFORMATION,
    _ownership_claimed: *mut BOOL,
    _event_name: *mut u16,
    _size: *mut u32,
    _signature_count: *mut u32,
) -> HRESULT {
    let result: Result<(), _> = (|| {
        anyhow::ensure!(
            !exception_information.is_null(),
            "exception_information is null"
        );

        let process_handle = unsafe { (*exception_information).hProcess };
        let thread_handle = unsafe { (*exception_information).hThread };

        datadog_crashtracker::exception_event_callback(
            context as usize,
            process_handle,
            thread_handle,
        )
    })();

    if let Err(e) = result {
        output_debug_string(format!("OutOfProcessExceptionEventCallback failed: {e}").as_str());
        return E_FAIL;
    }

    output_debug_string("OutOfProcessExceptionEventCallback succeeded");
    S_OK
}
