// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runtime callback registration system for enhanced crash tracing
//!
//! This module provides APIs for runtime languages (Ruby, Python, PHP, etc.) to register
//! callbacks that can provide runtime-specific stack traces during crash handling.

use crate::crash_info::StackFrame;
use ddcommon_ffi::{slice::AsBytes, CharSlice};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::ffi::c_char;

#[cfg(unix)]
use std::{
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use thiserror::Error;

#[cfg(unix)]
static FRAME_CSTR: &std::ffi::CStr = c"frame";
#[cfg(unix)]
static STACKTRACE_STRING_CSTR: &std::ffi::CStr = c"stacktrace_string";

/// Enum to store different types of callbacks
#[cfg(unix)]
#[derive(Debug)]
enum CallbackData {
    Frame(RuntimeFrameCallback),
    StacktraceString(RuntimeStacktraceStringCallback),
}

/// Global storage for the runtime callback
#[cfg(unix)]
static RUNTIME_CALLBACK: AtomicPtr<CallbackData> = AtomicPtr::new(ptr::null_mut());

#[repr(C)]
#[derive(Debug, Clone)]
pub struct RuntimeStackFrame {
    /// Fully qualified function/method name (length-prefixed string slice)
    /// Examples: "my_package.submodule.TestClass.method", "MyClass::method", "namespace::function"
    pub function_name: CharSlice<'static>,
    /// Source file name (length-prefixed string slice)
    pub file_name: CharSlice<'static>,
    /// Line number in source file (0 if unknown)
    pub line_number: u32,
    /// Column number in source file (0 if unknown)
    pub column_number: u32,
}

impl From<&RuntimeStackFrame> for StackFrame {
    /// Convert RuntimeStackFrame (C FFI) to StackFrame (internal representation)
    fn from(rsf: &RuntimeStackFrame) -> Self {
        let mut stack_frame = StackFrame::new();

        // Convert CharSlice to Rust strings and populate StackFrame
        if let Ok(Some(function_name)) = rsf.function_name.try_to_string_option() {
            stack_frame.function = Some(function_name);
        }

        if let Ok(Some(file_name)) = rsf.file_name.try_to_string_option() {
            stack_frame.file = Some(file_name);
        }

        if rsf.line_number != 0 {
            stack_frame.line = Some(rsf.line_number);
        }

        if rsf.column_number != 0 {
            stack_frame.column = Some(rsf.column_number);
        }

        stack_frame
    }
}

/// Function signature for runtime frame collection callbacks
///
/// This callback is invoked during crash handling in a signal context, so it must be signal-safe:
/// - No dynamic memory allocation
/// - No mutex operations
/// - No I/O operations
/// - Only async-signal-safe functions
///
/// # Parameters
/// - `emit_frame`: Function to call for each runtime frame (takes frame pointer)
///
/// # Safety
/// The callback function is marked unsafe because:
/// - It receives function pointers that take raw pointers as parameters
/// - The callback must ensure any pointers it passes to these functions are valid
pub type RuntimeFrameCallback =
    unsafe extern "C" fn(emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame));

/// Function signature for runtime stacktrace string collection callbacks
///
/// This callback is invoked during crash handling in a signal context, so it must be signal-safe:
/// - No dynamic memory allocation
/// - No mutex operations
/// - No I/O operations
/// - Only async-signal-safe functions
///
/// # Parameters
/// - `emit_stacktrace_string`: Function to call for complete stacktrace string (takes C string)
///
/// # Safety
/// The callback function is marked unsafe because:
/// - It receives function pointers that take raw pointers as parameters
/// - All C strings passed must be null-terminated and remain valid for the call duration
pub type RuntimeStacktraceStringCallback =
    unsafe extern "C" fn(emit_stacktrace_string: unsafe extern "C" fn(*const c_char));

/// Runtime stack representation for JSON serialization
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeStack {
    /// Format identifier for this runtime stack
    pub format: String,
    /// Array of runtime-specific stack frames (optional, mutually exclusive with
    /// stacktrace_string)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<StackFrame>,
    /// Raw stacktrace string (optional, mutually exclusive with frames)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stacktrace_string: Option<String>,
}

/// Errors that can occur during callback registration
#[derive(Debug, Error)]
pub enum CallbackError {
    #[error("Null callback function provided")]
    NullCallback,
}

/// Register a runtime frame collection callback
#[cfg(unix)]
pub fn register_runtime_frame_callback(
    callback: RuntimeFrameCallback,
) -> Result<(), CallbackError> {
    if callback as usize == 0 {
        return Err(CallbackError::NullCallback);
    }

    let callback_data = Box::into_raw(Box::new(CallbackData::Frame(callback)));
    let previous = RUNTIME_CALLBACK.swap(callback_data, Ordering::SeqCst);

    if !previous.is_null() {
        // Safety: previous was returned by Box::into_raw() above,
        // so it's guaranteed to be a valid Box pointer. We reconstruct the Box to drop it.
        let _ = unsafe { Box::from_raw(previous) };
    }

    Ok(())
}

/// Register a runtime stacktrace string collection callback
#[cfg(unix)]
pub fn register_runtime_stacktrace_string_callback(
    callback: RuntimeStacktraceStringCallback,
) -> Result<(), CallbackError> {
    if callback as usize == 0 {
        return Err(CallbackError::NullCallback);
    }

    let callback_data = Box::into_raw(Box::new(CallbackData::StacktraceString(callback)));
    let previous = RUNTIME_CALLBACK.swap(callback_data, Ordering::SeqCst);

    if !previous.is_null() {
        // Safety: previous was returned by Box::into_raw() above,
        // so it's guaranteed to be a valid Box pointer. We reconstruct the Box to drop it.
        let _ = unsafe { Box::from_raw(previous) };
    }

    Ok(())
}

/// Check if a runtime callback is currently registered
///
/// Returns true if a callback is registered, false otherwise
#[cfg(unix)]
pub fn is_runtime_callback_registered() -> bool {
    !RUNTIME_CALLBACK.load(Ordering::SeqCst).is_null()
}

/// Internal function to get the callback type for formatting purposes
///
/// # Safety
/// This function loads from an atomic pointer and dereferences it.
/// The caller must ensure that no other thread is calling `clear_runtime_callback`
/// or registration functions concurrently, as those could invalidate
/// the pointer between the null check and dereferencing.
#[cfg(all(unix, feature = "collector"))]
pub(crate) unsafe fn get_registered_callback_type_internal() -> Option<&'static str> {
    let callback_ptr = RUNTIME_CALLBACK.load(Ordering::SeqCst);
    if callback_ptr.is_null() {
        return None;
    }

    // Safety: callback_ptr was checked to be non-null above, and was created by
    // Box::into_raw() in registration functions, so it's a valid pointer
    // to a properly aligned, initialized CallbackData.
    let callback_data = &*callback_ptr;
    match callback_data {
        CallbackData::Frame(_) => Some("frame"),
        CallbackData::StacktraceString(_) => Some("stacktrace_string"),
    }
}

/// Get the callback type C string pointer from the currently registered callback
///
/// # Safety
/// This function loads from an atomic pointer and dereferences it.
/// The caller must ensure that no other thread is calling `clear_runtime_callback`
/// or registration functions concurrently, as those could invalidate
/// the pointer between the null check and dereferencing.
#[cfg(unix)]
pub unsafe fn get_registered_callback_type_ptr() -> *const std::ffi::c_char {
    let callback_ptr = RUNTIME_CALLBACK.load(Ordering::SeqCst);
    if callback_ptr.is_null() {
        return std::ptr::null();
    }

    // Safety: callback_ptr was checked to be non-null above, and was created by
    // Box::into_raw() in registration functions, so it's a valid pointer
    // to a properly aligned, initialized CallbackData. The returned C string pointer
    // points to static string literals, so it's always valid.
    let callback_data = &*callback_ptr;
    match callback_data {
        CallbackData::Frame(_) => FRAME_CSTR.as_ptr(),
        CallbackData::StacktraceString(_) => STACKTRACE_STRING_CSTR.as_ptr(),
    }
}

/// Clear the registered runtime callback
///
/// This function is primarily intended for testing purposes to clean up state
/// between tests. In production, callbacks typically remain registered for the
/// lifetime of the process.
///
/// # Safety
/// This function should only be called when it's safe to clear the callback,
/// such as during testing or application shutdown. The caller must ensure:
/// - No other thread is concurrently calling functions that dereference the callback pointer
/// - No signal handlers are currently executing that might invoke the callback
/// - The callback is not being used in any other way
#[cfg(unix)]
pub unsafe fn clear_runtime_callback() {
    let old_ptr = RUNTIME_CALLBACK.swap(std::ptr::null_mut(), Ordering::SeqCst);
    if !old_ptr.is_null() {
        // Safety: old_ptr was created by Box::into_raw() in register_runtime_stack_callback(),
        // so it's a valid Box pointer. We reconstruct the Box to properly drop the tuple.
        let _ = Box::from_raw(old_ptr);
    }
}

/// Internal function to invoke the registered runtime callback with direct pipe writing
///
/// This is called during crash handling to collect runtime-specific stack frames
/// and write them directly to the provided writer for efficiency.
///
/// # Safety
/// This function is intended to be called from signal handlers and must maintain
/// signal safety. It does not perform any dynamic allocation. The caller must ensure:
/// - No other thread is calling `clear_runtime_callback` concurrently
/// - The registered callback function is signal-safe
/// - The writer parameter remains valid for the duration of the call
#[cfg(all(unix, feature = "collector"))]
pub(crate) unsafe fn invoke_runtime_callback_with_writer<W: std::io::Write>(
    writer: &mut W,
) -> Result<(), std::io::Error> {
    let callback_ptr = RUNTIME_CALLBACK.load(Ordering::SeqCst);
    if callback_ptr.is_null() {
        return Err(std::io::Error::other("No runtime callback registered"));
    }

    // Safety: callback_ptr was checked to be non-null above, and was created by
    // Box::into_raw() in registration functions, so it's a valid pointer
    // to a properly aligned, initialized CallbackData.
    let callback_data = &*callback_ptr;

    use portable_atomic::{AtomicU128, Ordering};
    // TODO: Replace portable_atomic::AtomicU128 with std::sync::atomic::AtomicU128
    // when it stabilizes (see https://docs.rs/aarch64-std/latest/aarch64_std/sync/atomic/struct.AtomicU128.html)

    // Thread-safe storage for the current callback context
    // Store the fat pointer as an atomic u128 to ensure atomic access
    // Fat pointers on 64-bit platforms are exactly 128 bits (data ptr + vtable ptr)
    static CURRENT_WRITER_FAT_PTR: AtomicU128 = AtomicU128::new(0);

    let writer_trait_obj: *mut dyn std::io::Write = writer;
    let fat_ptr_bits: u128 = unsafe { std::mem::transmute(writer_trait_obj) };

    CURRENT_WRITER_FAT_PTR.store(fat_ptr_bits, Ordering::SeqCst);

    // Define the emit functions that read from the atomic storage
    unsafe extern "C" fn emit_frame_collector(frame: *const RuntimeStackFrame) {
        if frame.is_null() {
            return;
        }

        let fat_ptr_bits = CURRENT_WRITER_FAT_PTR.load(Ordering::SeqCst);
        if fat_ptr_bits == 0 {
            return;
        }

        // Reconstruct fat pointer from the atomic u128
        // Note: clippy suggests using `as` cast, but that doesn't work for fat pointers
        // which require both data and vtable pointers to be reconstructed from the u128
        #[allow(clippy::useless_transmute)]
        let writer_trait_obj: *mut dyn std::io::Write =
            unsafe { std::mem::transmute(fat_ptr_bits) };
        let writer = &mut *writer_trait_obj;

        let _ = emit_frame_as_json(writer, frame);
        let _ = writer.flush();
    }

    unsafe extern "C" fn emit_stacktrace_string_collector(stacktrace_string: *const c_char) {
        if stacktrace_string.is_null() {
            return;
        }

        let fat_ptr_bits = CURRENT_WRITER_FAT_PTR.load(Ordering::SeqCst);
        if fat_ptr_bits == 0 {
            return;
        }

        // Reconstruct fat pointer from the atomic u128
        // Note: clippy suggests using `as` cast, but that doesn't work for fat pointers
        // which require both data and vtable pointers to be reconstructed from the u128
        #[allow(clippy::useless_transmute)]
        let writer_trait_obj: *mut dyn std::io::Write =
            unsafe { std::mem::transmute(fat_ptr_bits) };
        let writer = &mut *writer_trait_obj;

        // Safety: stacktrace_string is guaranteed by the runtime callback contract to be
        // a valid, null-terminated C string that remains valid for the duration of this call.
        let cstr = std::ffi::CStr::from_ptr(stacktrace_string);
        let bytes = cstr.to_bytes();

        let _ = writer.write_all(bytes);
        let _ = writeln!(writer);
        let _ = writer.flush();
    }

    // Invoke the user callback with the simplified emit functions
    // Safety: callback functions were verified to be non-null during registration, and the
    // emit functions are valid for the duration of this call.
    match callback_data {
        CallbackData::Frame(callback_fn) => {
            callback_fn(emit_frame_collector);
        }
        CallbackData::StacktraceString(callback_fn) => {
            callback_fn(emit_stacktrace_string_collector);
        }
    }
    CURRENT_WRITER_FAT_PTR.store(0, Ordering::SeqCst);

    Ok(())
}

/// Emit a single runtime frame as JSON to the writer
///
/// This function writes a RuntimeStackFrame directly as JSON without intermediate allocation.
/// It must be signal-safe.
///
/// # Safety
/// The caller must ensure that `frame` is either null or points to a valid, properly
/// initialized RuntimeStackFrame. All C string pointers within the frame must be either
/// null or point to valid, null-terminated C strings.
#[cfg(all(unix, feature = "collector"))]
unsafe fn emit_frame_as_json(
    writer: &mut dyn std::io::Write,
    frame: *const RuntimeStackFrame,
) -> std::io::Result<()> {
    if frame.is_null() {
        return Ok(());
    }

    // Safety: frame was checked to be non-null above. The caller guarantees it points
    // to a valid RuntimeStackFrame.
    let frame_ref = &*frame;

    write!(writer, "{{")?;

    let mut first_field = true;

    if let Ok(Some(function_name)) = frame_ref.function_name.try_to_string_option() {
        write!(
            writer,
            "\"function\": \"{}\"",
            function_name.replace('"', "\\\"")
        )?;
        first_field = false;
    }

    if let Ok(Some(file_name)) = frame_ref.file_name.try_to_string_option() {
        if !first_field {
            write!(writer, ", ")?;
        }
        write!(writer, "\"file\": \"{}\"", file_name.replace('"', "\\\""))?;
        first_field = false;
    }

    if frame_ref.line_number != 0 {
        if !first_field {
            write!(writer, ", ")?;
        }
        write!(writer, "\"line\": {}", frame_ref.line_number)?;
        first_field = false;
    }

    if frame_ref.column_number != 0 {
        if !first_field {
            write!(writer, ", ")?;
        }
        write!(writer, "\"column\": {}", frame_ref.column_number)?;
    }

    writeln!(writer, "}}")?;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::ffi::CString;
    use std::sync::Mutex;

    // Use a mutex to ensure tests run sequentially to avoid race conditions
    // with the global static variable
    static TEST_MUTEX: Mutex<()> = Mutex::new(());

    unsafe extern "C" fn test_emit_frame_callback(
        emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
    ) {
        let function_name = "TestModule.TestClass.test_function";
        let file_name = "test.rb";

        let frame = RuntimeStackFrame {
            function_name: CharSlice::from(function_name),
            file_name: CharSlice::from(file_name),
            line_number: 42,
            column_number: 10,
        };

        // Safety: frame is a valid RuntimeStackFrame with valid CharSlice data
        emit_frame(&frame);
    }

    unsafe extern "C" fn test_emit_stacktrace_string_callback(
        emit_stacktrace_string: unsafe extern "C" fn(*const c_char),
    ) {
        let stacktrace_string = CString::new("test_stacktrace_string").unwrap();

        // Safety: stacktrace_string.as_ptr() returns a valid null-terminated C string
        emit_stacktrace_string(stacktrace_string.as_ptr());
    }

    fn ensure_callback_cleared() {
        // Ensure no callback is registered before starting
        let old_ptr = RUNTIME_CALLBACK.swap(ptr::null_mut(), Ordering::SeqCst);
        if !old_ptr.is_null() {
            let _ = unsafe { Box::from_raw(old_ptr) };
        }
    }

    #[test]
    fn test_callback_registration() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Test successful registration
        let result = register_runtime_frame_callback(test_emit_frame_callback);
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        // Test duplicate registration succeeds
        let result = register_runtime_frame_callback(test_emit_frame_callback);
        assert!(
            result.is_ok(),
            "Failed to re-register callback: {:?}",
            result
        );

        // Clean up
        ensure_callback_cleared();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_frame_collection() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Register callback
        let result = register_runtime_frame_callback(test_emit_frame_callback);
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        // Invoke callback and collect frames using writer
        let mut buffer = Vec::new();
        let invocation_result = unsafe { invoke_runtime_callback_with_writer(&mut buffer) };
        assert!(
            invocation_result.is_ok(),
            "Failed to invoke callback with writer"
        );

        let json_output = String::from_utf8(buffer).expect("Invalid UTF-8 in output");

        // Should contain the frame data as JSON
        assert!(
            json_output.contains("\"function\""),
            "Missing function field"
        );
        assert!(
            json_output.contains("TestModule.TestClass.test_function"),
            "Missing fully qualified function name"
        );
        assert!(json_output.contains("\"file\""), "Missing file field");
        assert!(json_output.contains("test.rb"), "Missing file name");
        assert!(
            json_output.contains("\"line\":42") || json_output.contains("\"line\": 42"),
            "Missing line number"
        );
        assert!(
            json_output.contains("\"column\":10") || json_output.contains("\"column\": 10"),
            "Missing column number"
        );

        // Clean up
        ensure_callback_cleared();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_stacktrace_string_collection() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Register callback
        let result =
            register_runtime_stacktrace_string_callback(test_emit_stacktrace_string_callback);
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        let mut buffer = Vec::new();
        let invocation_result = unsafe { invoke_runtime_callback_with_writer(&mut buffer) };
        assert!(
            invocation_result.is_ok(),
            "Failed to invoke callback with writer"
        );

        let json_output = String::from_utf8(buffer).expect("Invalid UTF-8 in output");

        // Should contain the stacktrace string
        assert!(
            json_output.contains("test_stacktrace_string"),
            "Missing stacktrace string"
        );

        ensure_callback_cleared();
    }

    #[test]
    fn test_no_callback_registered() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Test that invoking callback returns 0 frames
        let mut buffer = Vec::new();
        let invocation_result = unsafe { invoke_runtime_callback_with_writer(&mut buffer) };

        assert_eq!(
            invocation_result.unwrap_err().kind(),
            std::io::ErrorKind::Other,
            "Expected Other error when no callback registered"
        );

        assert!(
            buffer.is_empty(),
            "Expected empty buffer when no callback registered"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_direct_pipe_writing() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        let result = register_runtime_frame_callback(test_emit_frame_callback);
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        // Test writing directly to a buffer
        let mut buffer = Vec::new();
        let invocation_result = unsafe { invoke_runtime_callback_with_writer(&mut buffer) };
        assert!(
            invocation_result.is_ok(),
            "Failed to invoke callback with writer"
        );

        // Convert buffer to string and check JSON format
        let json_output = String::from_utf8(buffer).expect("Invalid UTF-8 in output");

        assert!(
            json_output.contains("\"function\""),
            "Missing function field"
        );
        assert!(
            json_output.contains("TestModule.TestClass.test_function"),
            "Missing fully qualified function name"
        );
        assert!(json_output.contains("\"file\""), "Missing file field");
        assert!(json_output.contains("test.rb"), "Missing file name");
        assert!(
            json_output.contains("\"line\":42") || json_output.contains("\"line\": 42"),
            "Missing line number"
        );
        assert!(
            json_output.contains("\"column\":10") || json_output.contains("\"column\": 10"),
            "Missing column number"
        );

        ensure_callback_cleared();
    }
}
