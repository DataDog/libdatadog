// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Runtime callback registration system for enhanced crash tracing
//!
//! This module provides APIs for runtime languages (Ruby, Python, PHP, etc.) to register
//! callbacks that can provide runtime-specific stack traces during crash handling.

/// Runtime-specific stack frame representation suitable for signal-safe context
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    ffi::c_char,
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use thiserror::Error;

static FRAME_CSTR: &std::ffi::CStr = c"frame";
static STACKTRACE_STRING_CSTR: &std::ffi::CStr = c"stacktrace_string";

/// Callback type identifier for different collection strategies
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CallbackType {
    Frame,
    StacktraceString,
}

impl CallbackType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CallbackType::Frame => "frame",
            CallbackType::StacktraceString => "stacktrace_string",
        }
    }

    pub fn as_cstr(&self) -> &'static std::ffi::CStr {
        match self {
            CallbackType::Frame => FRAME_CSTR,
            CallbackType::StacktraceString => STACKTRACE_STRING_CSTR,
        }
    }
}

impl std::str::FromStr for CallbackType {
    type Err = &'static str;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "frame" => Ok(CallbackType::Frame),
            "stacktrace_string" => Ok(CallbackType::StacktraceString),
            _ => Err("Invalid callback type"),
        }
    }
}

/// Global storage for the runtime callback
static RUNTIME_CALLBACK: AtomicPtr<(RuntimeStackCallback, CallbackType)> =
    AtomicPtr::new(ptr::null_mut());

#[repr(C)]
#[derive(Debug, Clone)]
pub struct RuntimeStackFrame {
    /// Fully qualified function/method name (null-terminated C string)
    /// Examples: "my_package.submodule.TestClass.method", "MyClass::method", "namespace::function"
    pub function_name: *const c_char,
    /// Source file name (null-terminated C string)
    pub file_name: *const c_char,
    /// Line number in source file (0 if unknown)
    pub line_number: u32,
    /// Column number in source file (0 if unknown)
    pub column_number: u32,
}

/// Function signature for runtime stack collection callbacks
///
/// This callback is invoked during crash handling in a signal context, so it must be best effort
/// signal-safe:
/// - No dynamic memory allocation
/// - No mutex operations
/// - No I/O operations
/// - Only async-signal-safe functions
///
/// # Parameters
/// - `emit_frame`: Function to call for each runtime frame (takes frame pointer)
/// - `emit_stacktrace_string`: Function to call for complete stacktrace string (takes C string)
///
/// # Safety
/// The callback function is marked unsafe because:
/// - It receives function pointers that take raw pointers as parameters
/// - The callback must ensure any pointers it passes to these functions are valid
/// - All C strings passed must be null-terminated and remain valid for the call duration
pub type RuntimeStackCallback = unsafe extern "C" fn(
    emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
    emit_stacktrace_string: unsafe extern "C" fn(*const c_char),
);

/// Runtime stack representation for JSON serialization
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeStack {
    /// Format identifier for this runtime stack
    pub format: String,
    /// Array of runtime-specific stack frames (optional, mutually exclusive with
    /// stacktrace_string)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub frames: Vec<RuntimeFrame>,
    /// Raw stacktrace string (optional, mutually exclusive with frames)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stacktrace_string: Option<String>,
}

/// JSON-serializable runtime stack frame
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RuntimeFrame {
    /// Fully qualified function/method name
    /// Examples: "my_package.submodule.TestClass.method", "MyClass::method", "namespace::function"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Source file name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    /// Line number in source file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number in source file
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

/// Errors that can occur during callback registration
#[derive(Debug, Error)]
pub enum CallbackError {
    #[error("Null callback function provided")]
    NullCallback,
}

/// Register a runtime stack collection callback
pub fn register_runtime_stack_callback(
    callback: RuntimeStackCallback,
    callback_type: CallbackType,
) -> Result<(), CallbackError> {
    if callback as usize == 0 {
        return Err(CallbackError::NullCallback);
    }

    let callback_data = Box::into_raw(Box::new((callback, callback_type)));
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
pub fn is_runtime_callback_registered() -> bool {
    !RUNTIME_CALLBACK.load(Ordering::SeqCst).is_null()
}

/// Get the callback type enum from the currently registered callback
///
/// Returns the callback type enum if a callback is registered, None otherwise
///
/// # Safety
/// This function loads from an atomic pointer and dereferences it.
/// The caller must ensure that no other thread is calling `clear_runtime_callback`
/// or `register_runtime_stack_callback` concurrently, as those could invalidate
/// the pointer between the null check and dereferencing.
pub unsafe fn get_registered_callback_type_enum() -> Option<CallbackType> {
    let callback_ptr = RUNTIME_CALLBACK.load(Ordering::SeqCst);
    if callback_ptr.is_null() {
        return None;
    }

    // Safety: callback_ptr was checked to be non-null above, and was created by
    // Box::into_raw() in register_runtime_stack_callback(), so it's a valid pointer
    // to a properly aligned, initialized tuple.
    let (_, callback_type) = &*callback_ptr;
    Some(*callback_type)
}

/// Get the callback type C string pointer from the currently registered callback
///
/// # Safety
/// This function loads from an atomic pointer and dereferences it.
/// The caller must ensure that no other thread is calling `clear_runtime_callback`
/// or `register_runtime_stack_callback` concurrently, as those could invalidate
/// the pointer between the null check and dereferencing.
pub unsafe fn get_registered_callback_type_ptr() -> *const std::ffi::c_char {
    let callback_ptr = RUNTIME_CALLBACK.load(Ordering::SeqCst);
    if callback_ptr.is_null() {
        return std::ptr::null();
    }

    // Safety: callback_ptr was checked to be non-null above, and was created by
    // Box::into_raw() in register_runtime_stack_callback(), so it's a valid pointer
    // to a properly aligned, initialized tuple. The returned C string pointer
    // points to static string literals, so it's always valid.
    let (_, callback_type) = &*callback_ptr;
    callback_type.as_cstr().as_ptr()
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
#[cfg(unix)]
pub(crate) unsafe fn invoke_runtime_callback_with_writer<W: std::io::Write>(
    writer: &mut W,
) -> Result<(), std::io::Error> {
    let callback_ptr = RUNTIME_CALLBACK.load(Ordering::SeqCst);
    if callback_ptr.is_null() {
        return Err(std::io::Error::other("No runtime callback registered"));
    }

    // Safety: callback_ptr was checked to be non-null above, and was created by
    // Box::into_raw() in register_runtime_stack_callback(), so it's a valid pointer
    // to a properly aligned, initialized tuple.
    let (callback_fn, _) = &*callback_ptr;

    use std::sync::atomic::{AtomicPtr, Ordering};

    // Thread-safe storage for the current callback context
    // Store as raw data and vtable pointers
    // We do this because trait objects are stored as raw pointers to its definition and also
    // the vtable for its impls, so we need to store both.
    static CURRENT_WRITER_DATA: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());
    static CURRENT_WRITER_VTABLE: AtomicPtr<()> = AtomicPtr::new(ptr::null_mut());

    let writer_trait_obj: *mut dyn std::io::Write = writer;
    let writer_parts: (*mut (), *mut ()) = unsafe { std::mem::transmute(writer_trait_obj) };

    // Store components atomically
    CURRENT_WRITER_DATA.store(writer_parts.0, Ordering::SeqCst);
    CURRENT_WRITER_VTABLE.store(writer_parts.1, Ordering::SeqCst);

    // Define the emit functions that read from the atomic storage
    unsafe extern "C" fn emit_frame_collector(frame: *const RuntimeStackFrame) {
        if frame.is_null() {
            return;
        }

        let writer_data = CURRENT_WRITER_DATA.load(Ordering::SeqCst);
        let writer_vtable = CURRENT_WRITER_VTABLE.load(Ordering::SeqCst);

        if writer_data.is_null() || writer_vtable.is_null() {
            return;
        }

        // Reconstruct fat pointer
        let writer_trait_obj: *mut dyn std::io::Write =
            std::mem::transmute((writer_data, writer_vtable));
        let writer = &mut *writer_trait_obj;

        // Write the frame as JSON
        let _ = emit_frame_as_json(writer, frame);
        let _ = writer.flush();
    }

    unsafe extern "C" fn emit_stacktrace_string_collector(stacktrace_string: *const c_char) {
        if stacktrace_string.is_null() {
            return;
        }

        let writer_data = CURRENT_WRITER_DATA.load(Ordering::SeqCst);
        let writer_vtable = CURRENT_WRITER_VTABLE.load(Ordering::SeqCst);

        if writer_data.is_null() || writer_vtable.is_null() {
            return;
        }

        // Reconstruct fat pointer
        let writer_trait_obj: *mut dyn std::io::Write =
            std::mem::transmute((writer_data, writer_vtable));
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
    // Safety: callback_fn was verified to be non-null during registration, and the
    // emit functions are valid for the duration of this call.
    callback_fn(emit_frame_collector, emit_stacktrace_string_collector);

    // Clear atomic storage
    CURRENT_WRITER_DATA.store(ptr::null_mut(), Ordering::SeqCst);
    CURRENT_WRITER_VTABLE.store(ptr::null_mut(), Ordering::SeqCst);

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
#[cfg(unix)]
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
    let mut first = true;

    // Convert C strings to Rust strings and write JSON fields
    if !frame_ref.function_name.is_null() {
        // Safety: frame_ref.function_name was checked to be non-null. The caller
        // guarantees it points to a valid, null-terminated C string.
        let c_str = std::ffi::CStr::from_ptr(frame_ref.function_name);
        if let Ok(s) = c_str.to_str() {
            if !s.is_empty() {
                write!(writer, "\"function\": \"{}\"", s)?;
                first = false;
            }
        }
    }

    if !frame_ref.file_name.is_null() {
        // Safety: frame_ref.file_name was checked to be non-null. The caller
        // guarantees it points to a valid, null-terminated C string.
        let c_str = std::ffi::CStr::from_ptr(frame_ref.file_name);
        if let Ok(s) = c_str.to_str() {
            if !s.is_empty() {
                if !first {
                    write!(writer, ", ")?;
                }
                write!(writer, "\"file\": \"{}\"", s)?;
                first = false;
            }
        }
    }

    if frame_ref.line_number != 0 {
        if !first {
            write!(writer, ", ")?;
        }
        write!(writer, "\"line\": {}", frame_ref.line_number)?;
        first = false;
    }

    if frame_ref.column_number != 0 {
        if !first {
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
        _emit_stacktrace_string: unsafe extern "C" fn(*const c_char),
    ) {
        let function_name = CString::new("TestModule.TestClass.test_function").unwrap();
        let file_name = CString::new("test.rb").unwrap();

        let frame = RuntimeStackFrame {
            function_name: function_name.as_ptr(),
            file_name: file_name.as_ptr(),
            line_number: 42,
            column_number: 10,
        };

        // Safety: frame is a valid RuntimeStackFrame with valid C string pointers
        emit_frame(&frame);
    }

    unsafe extern "C" fn test_emit_stacktrace_string_callback(
        _emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
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
        let result = register_runtime_stack_callback(test_emit_frame_callback, CallbackType::Frame);
        assert!(result.is_ok(), "Failed to register callback: {:?}", result);

        // Test duplicate registration succeeds (replaces previous)
        let result = register_runtime_stack_callback(test_emit_frame_callback, CallbackType::Frame);
        assert!(
            result.is_ok(),
            "Failed to re-register callback: {:?}",
            result
        );

        // Clean up
        ensure_callback_cleared();
    }

    #[test]
    fn test_frame_collection() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Register callback
        let result = register_runtime_stack_callback(test_emit_frame_callback, CallbackType::Frame);
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
        assert!(json_output.contains("\"line\": 42"), "Missing line number");
        assert!(
            json_output.contains("\"column\": 10"),
            "Missing column number"
        );

        // Clean up
        ensure_callback_cleared();
    }

    #[test]
    fn test_stacktrace_string_collection() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        // Register callback
        let result = register_runtime_stack_callback(
            test_emit_stacktrace_string_callback,
            CallbackType::StacktraceString,
        );
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
    fn test_direct_pipe_writing() {
        let _guard = TEST_MUTEX.lock().unwrap();
        ensure_callback_cleared();

        let result = register_runtime_stack_callback(test_emit_frame_callback, CallbackType::Frame);
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
        assert!(json_output.contains("\"line\": 42"), "Missing line number");
        assert!(
            json_output.contains("\"column\": 10"),
            "Missing column number"
        );

        ensure_callback_cleared();
    }
}
