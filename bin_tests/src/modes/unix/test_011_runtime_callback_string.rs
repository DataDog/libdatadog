// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test validates the runtime stack collection callback mechanism using stacktrace string mode.
// It registers a test callback that provides a complete stacktrace string,
// then crashes and verifies that the runtime stacktrace appears in the crash report.
//
// This test uses CallbackType::StacktraceString to emit complete stacktrace text.
//
// NOTE: This test currently has issues with the receiver's line-by-line parsing
// of multiline stacktrace strings. It serves as a demonstration of the string mode
// but may timeout due to receiver implementation details.

use crate::modes::behavior::{file_write_msg, Behavior};
use datadog_crashtracker::{
    register_runtime_stack_callback, CallbackType, CrashtrackerConfiguration,
};
use std::ffi::{c_char, c_void};
use std::path::Path;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        // Write a marker file to indicate we're testing runtime callback string mode
        let marker_file = output_dir.join("runtime_callback_test");
        file_write_msg(&marker_file, "string_mode")?;
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // Register our test runtime callback before crashtracker initialization
        register_runtime_stack_callback(
            test_runtime_callback_string,
            CallbackType::StacktraceString,
        )
        .map_err(|e| anyhow::anyhow!("Failed to register runtime callback: {:?}", e))?;
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // Nothing to do post-initialization for this test
        Ok(())
    }
}

// Signal-safe test callback that emits a complete stacktrace string
unsafe extern "C" fn test_runtime_callback_string(
    _emit_frame: unsafe extern "C" fn(*const datadog_crashtracker::RuntimeStackFrame),
    emit_stacktrace_string: unsafe extern "C" fn(*const c_char),
) {
    // Use static null-terminated string to avoid allocation in signal context
    // IMPORTANT: No embedded newlines - the receiver processes this line by line
    static STACKTRACE: &[u8] = b"RuntimeError in script.py:42 runtime_function_1 -> module.py:100 runtime_function_2 -> main.py:10 runtime_main\0";

    // Emit the complete stacktrace string
    emit_stacktrace_string(STACKTRACE.as_ptr() as *const c_char);
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_crashtracker::{clear_runtime_callback, is_runtime_callback_registered};

    #[test]
    fn test_runtime_callback_string_registration() {
        // Ensure clean state
        unsafe {
            clear_runtime_callback();
        }

        // Test that no callback is initially registered
        assert!(!is_runtime_callback_registered());

        // Test string mode registration
        let result = register_runtime_stack_callback(
            test_runtime_callback_string,
            CallbackType::StacktraceString,
        );
        assert!(
            result.is_ok(),
            "String callback registration should succeed"
        );
        assert!(
            is_runtime_callback_registered(),
            "Callback should be registered"
        );

        // Clean up
        unsafe {
            clear_runtime_callback();
        }
        assert!(
            !is_runtime_callback_registered(),
            "Callback should be cleared"
        );
    }
}
