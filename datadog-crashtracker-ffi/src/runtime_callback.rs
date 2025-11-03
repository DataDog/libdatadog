// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! FFI bindings for runtime callback registration
//!
//! This module provides C-compatible FFI bindings for registering runtime-specific
//! crash callbacks that can provide stack traces for dynamic languages.
#[cfg(unix)]
use datadog_crashtracker::{
    get_registered_callback_type_ptr, is_runtime_callback_registered,
    register_runtime_frame_callback, register_runtime_stacktrace_string_callback, CallbackError,
    RuntimeStacktraceStringCallback,
};

pub use datadog_crashtracker::RuntimeStackFrame as ddog_RuntimeStackFrame;
use ddcommon_ffi::CharSlice;

/// Result type for runtime callback registration
#[cfg(unix)]
#[repr(C)]
#[derive(Debug, PartialEq, Eq)]
pub enum CallbackResult {
    Ok,
    Error,
}

#[cfg(unix)]
impl From<CallbackError> for CallbackResult {
    fn from(_error: CallbackError) -> Self {
        CallbackResult::Error
    }
}

#[repr(C)]
#[derive(Debug, Clone)]
pub struct RuntimeStackFrameFFI {
    /// Line number in source file (0 if unknown)
    pub line: u32,
    /// Column number in source file (0 if unknown)
    pub column: u32,
    /// Function name (fully qualified if possible)
    pub function: CharSlice<'static>,
    /// Source file name
    pub file: CharSlice<'static>,
    /// Type name (class/module/namespace/etc.)
    pub type_name: CharSlice<'static>,
}

pub type RuntimeStackFrameCallback =
    unsafe extern "C" fn(emit_frame: unsafe extern "C" fn(*const RuntimeStackFrameFFI));

/// Global storage for the FFI callback
#[cfg(unix)]
static mut STORED_FFI_CALLBACK: Option<RuntimeStackFrameCallback> = None;

/// Global storage for the core emit function during callback execution
#[cfg(unix)]
static mut STORED_CORE_EMIT: Option<
    unsafe extern "C" fn(&datadog_crashtracker::RuntimeStackFrame),
> = None;

#[cfg(unix)]
fn convert_ffi_to_core_frame(
    ffi_frame: &RuntimeStackFrameFFI,
) -> datadog_crashtracker::RuntimeStackFrame<'_> {
    use ddcommon_ffi::slice::AsBytes;

    datadog_crashtracker::RuntimeStackFrame {
        line: ffi_frame.line,
        column: ffi_frame.column,
        function: ffi_frame.function.as_bytes(),
        file: ffi_frame.file.as_bytes(),
        type_name: ffi_frame.type_name.as_bytes(),
    }
}

#[cfg(unix)]
unsafe extern "C" fn emit_ffi_frame(ffi_frame_ptr: *const RuntimeStackFrameFFI) {
    if ffi_frame_ptr.is_null() {
        return;
    }

    if let Some(core_emit) = STORED_CORE_EMIT {
        let ffi_frame = &*ffi_frame_ptr;
        let core_frame = convert_ffi_to_core_frame(ffi_frame);
        core_emit(&core_frame);
    }
}

/// Wrapper function that bridges FFI callback to core callback
#[cfg(unix)]
unsafe extern "C" fn ffi_callback_wrapper(
    emit_core_frame: unsafe extern "C" fn(&datadog_crashtracker::RuntimeStackFrame),
) {
    if let Some(ffi_callback) = STORED_FFI_CALLBACK {
        STORED_CORE_EMIT = Some(emit_core_frame);

        // Call the original FFI callback with our converting emit function
        ffi_callback(emit_ffi_frame);

        // Clear the stored function
        STORED_CORE_EMIT = None;
    }
}

/// Register a runtime stack collection callback
///
/// This function allows language runtimes to register a callback that will be invoked
/// during crash handling to collect runtime-specific stack traces.
///
/// # Arguments
/// - `callback`: The callback function to invoke during crashes
///
/// # Returns
/// - `CallbackResult::Ok` if registration succeeds
/// - `CallbackResult::Error` if registration fails
///
/// # Safety
/// - The callback must be signal-safe
/// - Only one callback can be registered at a time
/// - The callback must be registered once on CrashTracker initialization, before any crash occurs
///
/// # Example Usage from C
/// ```c
/// static void my_runtime_callback(
///     void (*emit_frame)(const ddog_RuntimeStackFrame*),
/// ) {
///     // Collect runtime frames and call emit_frame for each one
///     const char* function_name = "my_function";
///     const char* file_name = "script.rb";
///     ddog_CharSlice type_name = DDOG_CHARSLICE_FROM_CSTR("MyModule.MyClass");
///     ddog_crasht_RuntimeStackFrame frame = {
///         .type_name = &type_name,
///         .function_name = DDOG_CHARSLICE_FROM_CSTR(function_name),
///         .file_name = DDOG_CHARSLICE_FROM_CSTR(file_name),
///         .line_number = 42,
///         .column_number = 10
///     };
///     emit_frame(&frame);
/// }
///
///
/// ddog_CallbackResult result = ddog_crasht_register_runtime_frame_callback(
///     my_runtime_callback
/// );
/// ```
/// Register a runtime frame collection callback
///
/// This function allows language runtimes to register a callback that will be invoked
/// during crash handling to collect runtime-specific stack frames.
///
/// # Arguments
/// - `callback`: The callback function to invoke during crashes
///
/// # Returns
/// - `CallbackResult::Ok` if registration succeeds (replaces any existing callback)
/// - `CallbackResult::Error` if registration fails
///
/// # Safety
/// - The callback must be signal-safe
/// - Only one callback can be registered at a time (this replaces any existing one)
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_register_runtime_frame_callback(
    callback: RuntimeStackFrameCallback,
) -> CallbackResult {
    STORED_FFI_CALLBACK = Some(callback);

    // Register the wrapper with the core crate
    match register_runtime_frame_callback(ffi_callback_wrapper) {
        Ok(()) => CallbackResult::Ok,
        Err(e) => e.into(),
    }
}

/// Register a runtime stacktrace string collection callback
///
/// This function allows language runtimes to register a callback that will be invoked
/// during crash handling to collect runtime-specific stacktrace strings.
///
/// # Arguments
/// - `callback`: The callback function to invoke during crashes
///
/// # Returns
/// - `CallbackResult::Ok` if registration succeeds (replaces any existing callback)
/// - `CallbackResult::Error` if registration fails
///
/// # Safety
/// - The callback must be signal-safe
/// - Only one callback can be registered at a time (this replaces any existing one)
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_register_runtime_stacktrace_string_callback(
    callback: RuntimeStacktraceStringCallback,
) -> CallbackResult {
    match register_runtime_stacktrace_string_callback(callback) {
        Ok(()) => CallbackResult::Ok,
        Err(e) => e.into(),
    }
}

/// Check if a runtime callback is currently registered
///
/// Returns true if a callback is registered, false otherwise
///
/// # Safety
/// This function is safe to call at any time
#[cfg(unix)]
#[no_mangle]
pub extern "C" fn ddog_crasht_is_runtime_callback_registered() -> bool {
    is_runtime_callback_registered()
}

/// Get the callback type from the currently registered callback context
///
/// Returns the callback type C string pointer if a callback with valid context is registered,
/// null pointer otherwise
///
/// # Safety
/// - The returned pointer is valid only while the callback remains registered
/// - The caller should not free the returned pointer
/// - The returned string should be copied if it needs to persist beyond callback lifetime
#[cfg(unix)]
#[no_mangle]
pub unsafe extern "C" fn ddog_crasht_get_registered_callback_type() -> *const std::ffi::c_char {
    get_registered_callback_type_ptr()
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use datadog_crashtracker::clear_runtime_callback;
    use std::sync::Mutex;

    // Use a mutex to ensure tests run sequentially to avoid race conditions
    // with the global static variable
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn test_frame_callback(
        emit_frame: unsafe extern "C" fn(*const RuntimeStackFrameFFI),
    ) {
        let function_name = "TestModule.TestClass.test_function";
        let file_name = "test.rb";

        let frame = RuntimeStackFrameFFI {
            type_name: CharSlice::from("TestModule.TestClass"),
            function: CharSlice::from(function_name),
            file: CharSlice::from(file_name),
            line: 42,
            column: 10,
        };

        emit_frame(&frame);
    }

    unsafe extern "C" fn test_stacktrace_string_callback(
        emit_stacktrace_string: unsafe extern "C" fn(*const std::ffi::c_char),
    ) {
        let stacktrace_string = "test_stacktrace_string\0";
        emit_stacktrace_string(stacktrace_string.as_ptr() as *const std::ffi::c_char);
    }

    #[test]
    fn test_callback_invocation() {
        let _guard = TEST_MUTEX.lock().unwrap();
        unsafe {
            clear_runtime_callback();

            // Register our test callback
            let result = ddog_crasht_register_runtime_frame_callback(test_frame_callback);
            assert_eq!(result, CallbackResult::Ok);

            // Test that the wrapper can be invoked successfully
            unsafe extern "C" fn mock_emit_core_frame(
                _frame: &datadog_crashtracker::RuntimeStackFrame,
            ) {
                // Callback invoked successfully
            }

            ffi_callback_wrapper(mock_emit_core_frame);

            clear_runtime_callback();
        }
    }

    #[test]
    fn test_callback_registration() {
        let _guard = TEST_MUTEX.lock().unwrap();
        unsafe {
            clear_runtime_callback();

            assert!(!ddog_crasht_is_runtime_callback_registered());

            let result = ddog_crasht_register_runtime_stacktrace_string_callback(
                test_stacktrace_string_callback,
            );

            assert_eq!(result, CallbackResult::Ok);
            assert!(ddog_crasht_is_runtime_callback_registered());

            let callback_type_ptr = ddog_crasht_get_registered_callback_type();
            assert!(!callback_type_ptr.is_null());
            let callback_type_str = std::ffi::CStr::from_ptr(callback_type_ptr)
                .to_str()
                .unwrap();
            assert_eq!(callback_type_str, "stacktrace_string");

            let result = ddog_crasht_register_runtime_frame_callback(test_frame_callback);

            assert_eq!(result, CallbackResult::Ok);

            let callback_type_ptr = ddog_crasht_get_registered_callback_type();
            let callback_type_str = std::ffi::CStr::from_ptr(callback_type_ptr)
                .to_str()
                .unwrap();
            assert_eq!(callback_type_str, "frame");

            clear_runtime_callback();
        }
    }
}
