// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
//
// This test validates the runtime stack collection callback mechanism using frame-by-frame mode
// with invalid UTF-8 characters in the frame data. It ensures the system properly handles
// non-UTF-8 sequences by converting them using lossy conversion without crashing.
//
// This test uses frame-by-frame callback to emit runtime stack data containing invalid UTF-8.

use crate::modes::behavior::Behavior;
use libdd_crashtracker::{
    clear_runtime_callback, register_runtime_frame_callback, CrashtrackerConfiguration,
    RuntimeStackFrame,
};
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
        register_runtime_frame_callback(test_runtime_callback_frame_with_invalid_utf8)
            .map_err(|e| anyhow::anyhow!("Failed to register runtime callback: {:?}", e))?;
        Ok(())
    }

    fn post(&self, _output_dir: &Path) -> anyhow::Result<()> {
        Ok(())
    }
}

// Signal-safe test callback that emits mock runtime stack frames with invalid UTF-8
unsafe extern "C" fn test_runtime_callback_frame_with_invalid_utf8(
    emit_frame: unsafe extern "C" fn(&RuntimeStackFrame),
) {
    // 0xFF is invalid utf8
    static INVALID_UTF8_FUNCTION_NAME: &[u8] = b"runtime_function_\xFF_invalid";
    static INVALID_UTF8_TYPE_NAME: &[u8] = b"TestModule.\xFF.TestClass";
    static INVALID_UTF8_FILE_NAME: &[u8] = b"script_\xFF.py";

    // Valid utf8
    static VALID_FUNCTION_NAME: &str = "valid_runtime_function";
    static VALID_FILE_NAME: &str = "valid_script.py";

    static MIXED_UTF8_FUNCTION_NAME: &[u8] = b"func_\xC0\x80_invalid_overlong"; // Invalid overlong encoding
    static MIXED_UTF8_TYPE_NAME: &[u8] = b"Class\xED\xA0\x80Name"; // Invalid surrogate sequence

    // Null bytes (valid in byte strings but problematic in C strings
    static NULL_BYTE_FUNCTION_NAME: &[u8] = b"func_with_\x00_null_byte";
    static NULL_BYTE_TYPE_NAME: &[u8] = b"Type\x00WithNull";
    static NULL_BYTE_FILE_NAME: &[u8] = b"file_\x00_null.py";

    // Invalid utf8 in function name
    let frame1 = RuntimeStackFrame {
        type_name: b"ValidType",
        function: INVALID_UTF8_FUNCTION_NAME,
        file: b"valid_file.py",
        line: 42,
        column: 15,
    };
    emit_frame(&frame1);

    // Invalid utf8 in type name
    let frame2 = RuntimeStackFrame {
        type_name: INVALID_UTF8_TYPE_NAME,
        function: b"valid_function",
        file: b"valid_file.py",
        line: 100,
        column: 8,
    };
    emit_frame(&frame2);

    // Invalid utf8 in file name
    let frame3 = RuntimeStackFrame {
        type_name: b"ValidType",
        function: b"valid_function",
        file: INVALID_UTF8_FILE_NAME,
        line: 200,
        column: 1,
    };
    emit_frame(&frame3);

    // Valid utf8
    let frame4 = RuntimeStackFrame {
        type_name: b"ValidType",
        function: VALID_FUNCTION_NAME.as_bytes(),
        file: VALID_FILE_NAME.as_bytes(),
        line: 300,
        column: 5,
    };
    emit_frame(&frame4);

    // Mixed invalid utf8 sequences
    let frame5 = RuntimeStackFrame {
        type_name: MIXED_UTF8_TYPE_NAME,
        function: MIXED_UTF8_FUNCTION_NAME,
        file: b"mixed_file.py",
        line: 400,
        column: 10,
    };
    emit_frame(&frame5);

    // Frame 6: Null bytes (edge case for C string handling)
    let frame6 = RuntimeStackFrame {
        type_name: NULL_BYTE_TYPE_NAME,
        function: NULL_BYTE_FUNCTION_NAME,
        file: NULL_BYTE_FILE_NAME,
        line: 500,
        column: 2,
    };
    emit_frame(&frame6);

    // Frame 7: Empty fields (edge case)
    let frame7 = RuntimeStackFrame {
        type_name: b"",
        function: b"",
        file: b"",
        line: 0,
        column: 0,
    };
    emit_frame(&frame7);
}

#[cfg(test)]
mod tests {
    use super::*;
    use libdd_crashtracker::{clear_runtime_callback, is_runtime_callback_registered};
    use serial_test::serial;

    #[test]
    #[serial(runtime_callback)]
    fn test_runtime_callback_frame_invalid_utf8_registration() {
        // Ensure clean state
        unsafe {
            clear_runtime_callback();
        }

        assert!(!is_runtime_callback_registered());

        let result = register_runtime_frame_callback(test_runtime_callback_frame_with_invalid_utf8);
        assert!(result.is_ok(), "Frame callback registration should succeed");
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
