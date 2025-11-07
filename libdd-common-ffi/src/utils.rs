// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::panic::{catch_unwind, AssertUnwindSafe};

/// Wraps a C-FFI function in standard form
/// Expects the function to return a result type that implements into and to be decorated with
/// #[named].
#[macro_export]
macro_rules! wrap_with_ffi_result {
    ($body:block) => {{
        use std::panic::{catch_unwind, AssertUnwindSafe};

        catch_unwind(AssertUnwindSafe(|| {
            $crate::wrap_with_ffi_result_no_catch!({ $body })
        }))
        .map_or_else(
            |e| $crate::utils::handle_panic_error(e, function_name!()).into(),
            |result| result,
        )
    }};
}

/// Wraps a C-FFI function in standard form (no catch variant).
/// Same as `wrap_with_ffi_result` but does not try to catch panics.
#[macro_export]
macro_rules! wrap_with_ffi_result_no_catch {
    ($body:block) => {{
        use anyhow::Context;
        (|| $body)()
            .context(concat!(function_name!(), " failed"))
            .into()
    }};
}

/// Wraps a C-FFI function in standard form.
/// Expects the function to return a VoidResult and to be decorated with #[named].
#[macro_export]
macro_rules! wrap_with_void_ffi_result {
    ($body:block) => {{
        use std::panic::{catch_unwind, AssertUnwindSafe};

        catch_unwind(AssertUnwindSafe(|| {
            $crate::wrap_with_void_ffi_result_no_catch!({ $body })
        }))
        .map_or_else(
            |e| $crate::utils::handle_panic_error(e, function_name!()).into(),
            |result| result,
        )
    }};
}

/// Wraps a C-FFI function in standard form (no catch variant).
/// Same as `wrap_with_void_ffi_result` but does not try to catch panics.
#[macro_export]
macro_rules! wrap_with_void_ffi_result_no_catch {
    ($body:block) => {{
        use anyhow::Context;
        (|| {
            $body;
            anyhow::Ok(())
        })()
        .context(concat!(function_name!(), " failed"))
        .into()
    }};
}

pub trait ToHexStr {
    fn to_hex_str(&self) -> String;
}

impl ToHexStr for usize {
    fn to_hex_str(&self) -> String {
        format!("0x{self:X}")
    }
}

/// You probably don't want to use this directly. This is used by `wrap_with_*_ffi_result` macros to
/// turn a panic error into an actual nice error. Because the original panic may have been caused by
/// being unable to allocate, this helper handles failures to allocate as well, turning them into a
/// fallback error.
pub fn handle_panic_error(
    error: Box<dyn std::any::Any + Send + 'static>,
    function_name: &str,
) -> crate::Error {
    catch_unwind(AssertUnwindSafe(|| {
        // This pattern of String vs &str comes from
        // https://doc.rust-lang.org/std/panic/struct.PanicHookInfo.html#method.payload
        if let Some(s) = error.downcast_ref::<String>() {
            anyhow::anyhow!("{} failed: (panic) {}", function_name, s)
        } else if let Some(s) = error.downcast_ref::<&str>() {
            // panic!("double panic");
            anyhow::anyhow!("{} failed: (panic) {}", function_name, s)
        } else {
            anyhow::anyhow!(
                "{} failed: (panic) Unable to retrieve panic context",
                function_name
            )
        }
        .into()
    }))
    .unwrap_or(crate::error::CANNOT_ALLOCATE_ERROR)
}

#[cfg(test)]
mod tests {
    use assert_no_alloc::{assert_no_alloc, AllocDisabler};
    use function_name::named;

    #[cfg(debug_assertions)] // required when disable_release is set (default)
    #[global_allocator]
    static ALLOCATOR: AllocDisabler = AllocDisabler;

    #[test]
    fn test_handle_panic_error_fallback_does_not_allocate() {
        let mut error_result_buffer: [std::os::raw::c_char; 100] = [0; 100];

        assert_no_alloc(|| {
            // Simulate fallback code path of handle_panic_error + ddog_Error_message
            let fallback_error = crate::error::CANNOT_ALLOCATE_ERROR;
            let error_message = unsafe { crate::ddog_Error_message(Some(&fallback_error)) };

            // Stash error message so we can assert on it
            let n = error_message.len().min(error_result_buffer.len());
            error_result_buffer[..n].copy_from_slice(&error_message[..n]);
        });

        unsafe {
            let c_str = std::ffi::CStr::from_ptr(error_result_buffer.as_ptr());
            assert_eq!(
                c_str.to_str().unwrap(),
                "libdatadog failed: (panic) Cannot allocate error message"
            );
        };
    }

    #[test]
    #[named]
    #[allow(clippy::redundant_closure_call)]
    fn test_wrap_with_ffi_result_turns_panic_into_error() {
        // Save the current panic handler and replace it with a no-op so that Rust doesn't print
        // anything inside `wrap_with_ffi_result`...
        let original_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let result: crate::Result<()> = wrap_with_ffi_result!({
            panic!("this is a test panic message");
            #[allow(unreachable_code)]
            anyhow::Ok(())
        });

        // ...restore original behavior
        std::panic::set_hook(original_hook);

        assert_eq!(result.unwrap_err().to_string(), "test_wrap_with_ffi_result_turns_panic_into_error failed: (panic) this is a test panic message");
    }

    #[test]
    #[named]
    fn test_wrap_with_ffi_result_does_not_modify_other_kinds_of_errors() {
        let result: crate::result::VoidResult = wrap_with_void_ffi_result!({
            Err(anyhow::anyhow!("this is a test error message"))?;
        });

        assert_eq!(result.unwrap_err().to_string(), "test_wrap_with_ffi_result_does_not_modify_other_kinds_of_errors failed: this is a test error message");
    }
}
