// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test validates the runtime stack collection callback mechanism using string mode.
// It registers a test callback that provides mock runtime stacktrace string,
// then crashes and verifies that the runtime stacktrace string appear in the crash report.
//
// This test uses CallbackType::StacktraceString to emit structured runtime stack data.

use crate::modes::behavior::Behavior;
use datadog_crashtracker::{
    clear_runtime_callback, register_runtime_stack_callback, CallbackType,
    CrashtrackerConfiguration, RuntimeStackFrame,
};
use std::ffi::c_char;
use std::path::Path;

pub struct Test;

impl Behavior for Test {
    fn setup(
        &self,
        _output_dir: &Path,
        _config: &mut CrashtrackerConfiguration,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    fn pre(&self, _output_dir: &Path) -> anyhow::Result<()> {
        // Ensure clean state
        unsafe {
            clear_runtime_callback();
        }
        register_runtime_stack_callback(
            test_runtime_callback_string,
            CallbackType::StacktraceString,
        )
        .map_err(|e| anyhow::anyhow!("Failed to register runtime callback: {:?}", e))?;
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}

// Signal-safe test callback that emits mock runtime stacktrace string
unsafe extern "C" fn test_runtime_callback_string(
    _emit_frame: unsafe extern "C" fn(*const RuntimeStackFrame),
    emit_stacktrace_string: unsafe extern "C" fn(*const c_char),
) {
    static STACKTRACE_STRING: &[u8] = b"test_stacktrace_string\0";
    emit_stacktrace_string(STACKTRACE_STRING.as_ptr() as *const c_char);
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_crashtracker::{clear_runtime_callback, is_runtime_callback_registered};

    #[test]
    fn test_runtime_callback_string_registration() {
        unsafe {
            clear_runtime_callback();
        }

        assert!(!is_runtime_callback_registered());

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

        unsafe {
            clear_runtime_callback();
        }
        assert!(
            !is_runtime_callback_registered(),
            "Callback should be cleared"
        );
    }
}
