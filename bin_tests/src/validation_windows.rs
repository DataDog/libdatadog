// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Windows-specific validation helpers for crash tracking tests.
//! Provides a fluent API for validating Windows crash payloads.

use anyhow::{Context, Result};
use serde_json::Value;

/// A fluent API for validating Windows crash payloads.
/// Allows chaining multiple validations together.
///
/// # Example
/// ```ignore
/// WindowsPayloadValidator::new(&crash_payload)
///     .validate_exception_code(0xC0000005)?
///     .validate_stack_exists()?
///     .validate_thread_info()?;
/// ```
pub struct WindowsPayloadValidator<'a> {
    payload: &'a Value,
}

impl<'a> WindowsPayloadValidator<'a> {
    /// Creates a new Windows payload validator.
    pub fn new(payload: &'a Value) -> Self {
        Self { payload }
    }

    /// Validates the Windows exception code matches the expected value.
    pub fn validate_exception_code(self, expected_code: u32) -> Result<Self> {
        let exception_code = self.payload["error"]["exception_code"]
            .as_u64()
            .or_else(|| {
                // Try as string (some formats may serialize as hex string)
                self.payload["error"]["exception_code"]
                    .as_str()
                    .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
            })
            .context("Missing or invalid exception_code in error section")?;

        anyhow::ensure!(
            exception_code == expected_code as u64,
            "Exception code mismatch: expected 0x{:X}, got 0x{:X}",
            expected_code,
            exception_code
        );

        Ok(self)
    }

    /// Validates that a stack trace exists and has at least one frame.
    pub fn validate_stack_exists(self) -> Result<Self> {
        let stack = self.payload["error"]["stack"]
            .as_array()
            .context("Missing or invalid stack in error section")?;

        anyhow::ensure!(!stack.is_empty(), "Stack trace is empty");

        Ok(self)
    }

    /// Validates that the stack has at least the specified number of frames.
    pub fn validate_min_stack_frames(self, min_frames: usize) -> Result<Self> {
        let stack = self.payload["error"]["stack"]
            .as_array()
            .context("Missing or invalid stack in error section")?;

        anyhow::ensure!(
            stack.len() >= min_frames,
            "Stack has {} frames, expected at least {}",
            stack.len(),
            min_frames
        );

        Ok(self)
    }

    /// Validates thread information in the crash report.
    pub fn validate_thread_info(self) -> Result<Self> {
        let threads = self.payload["threads"]
            .as_array()
            .context("Missing threads array in payload")?;

        anyhow::ensure!(
            !threads.is_empty(),
            "No thread information found in crash report"
        );

        // Find the crashed thread
        let crashed_thread = threads
            .iter()
            .find(|t| t["crashed"].as_bool() == Some(true))
            .context("No crashed thread found in threads array")?;

        // Validate crashed thread has a stack
        anyhow::ensure!(
            crashed_thread["stack"].is_array(),
            "Crashed thread has no stack information"
        );

        let crashed_stack = crashed_thread["stack"]
            .as_array()
            .context("Crashed thread stack is not an array")?;

        anyhow::ensure!(!crashed_stack.is_empty(), "Crashed thread stack is empty");

        Ok(self)
    }

    /// Validates that the OS information is Windows-specific.
    pub fn validate_os_info(self) -> Result<Self> {
        let os_info = &self.payload["os_info"];

        let os_type = os_info["type"]
            .as_str()
            .context("Missing or invalid OS type in os_info")?;

        anyhow::ensure!(
            os_type.to_lowercase().contains("windows"),
            "Expected Windows OS type, got: {}",
            os_type
        );

        // Optionally validate Windows version exists
        if let Some(version) = os_info["version"].as_str() {
            anyhow::ensure!(!version.is_empty(), "Windows version is empty");
        }

        Ok(self)
    }

    /// Validates metadata section exists and contains required fields.
    pub fn validate_metadata(self) -> Result<Self> {
        let metadata = &self.payload["metadata"];

        anyhow::ensure!(
            metadata.is_object(),
            "Metadata section missing or not an object"
        );

        // Validate required metadata fields
        let library_name = metadata["library_name"]
            .as_str()
            .context("Missing library_name in metadata")?;

        anyhow::ensure!(!library_name.is_empty(), "library_name is empty");

        let library_version = metadata["library_version"]
            .as_str()
            .context("Missing library_version in metadata")?;

        anyhow::ensure!(!library_version.is_empty(), "library_version is empty");

        Ok(self)
    }

    /// Validates the error kind.
    pub fn validate_error_kind(self, expected_kind: &str) -> Result<Self> {
        let kind = self.payload["error"]["kind"]
            .as_str()
            .context("Missing or invalid error kind")?;

        anyhow::ensure!(
            kind == expected_kind,
            "Error kind mismatch: expected '{}', got '{}'",
            expected_kind,
            kind
        );

        Ok(self)
    }

    /// Validates the error message contains specific text.
    pub fn validate_error_message_contains(self, expected_substring: &str) -> Result<Self> {
        let message = self.payload["error"]["message"]
            .as_str()
            .context("Missing or invalid error message")?;

        anyhow::ensure!(
            message.contains(expected_substring),
            "Error message does not contain '{}'. Got: '{}'",
            expected_substring,
            message
        );

        Ok(self)
    }

    /// Allows incomplete stacks (for scenarios like stack overflow).
    /// This is a marker method that doesn't validate anything but documents intent.
    pub fn allow_incomplete_stack(self) -> Result<Self> {
        // Check if incomplete flag exists and is true
        if let Some(incomplete) = self.payload.get("incomplete") {
            if let Some(is_incomplete) = incomplete.as_bool() {
                anyhow::ensure!(is_incomplete, "Expected incomplete flag to be true");
            }
        }
        // If no incomplete flag, that's okay too
        Ok(self)
    }

    /// Validates that module information exists (DLLs/EXEs).
    pub fn validate_modules_exist(self) -> Result<Self> {
        let modules = self
            .payload
            .get("modules")
            .or_else(|| self.payload.get("files"))
            .context("Missing modules/files section in payload")?;

        let modules_array = modules
            .as_array()
            .context("Modules/files is not an array")?;

        anyhow::ensure!(
            !modules_array.is_empty(),
            "No modules/files found in crash report"
        );

        Ok(self)
    }

    /// Validates UUID fields exist.
    pub fn validate_uuid_exists(self) -> Result<Self> {
        let uuid = self
            .payload
            .get("uuid")
            .or_else(|| self.payload.get("crash_uuid"))
            .context("Missing UUID in payload")?;

        let uuid_str = uuid.as_str().context("UUID is not a string")?;

        anyhow::ensure!(!uuid_str.is_empty(), "UUID is empty");

        // Basic UUID format validation (8-4-4-4-12 hex digits)
        let parts: Vec<&str> = uuid_str.split('-').collect();
        anyhow::ensure!(
            parts.len() == 5,
            "UUID does not have correct format (expected 5 parts separated by dashes)"
        );

        Ok(self)
    }

    /// Validates timestamp exists and is reasonable.
    pub fn validate_timestamp(self) -> Result<Self> {
        let timestamp = self
            .payload
            .get("timestamp")
            .context("Missing timestamp in payload")?;

        // Can be string or number
        if timestamp.is_string() {
            let ts_str = timestamp.as_str().unwrap();
            anyhow::ensure!(!ts_str.is_empty(), "Timestamp string is empty");
        } else if timestamp.is_number() {
            let ts_num = timestamp
                .as_u64()
                .context("Timestamp is not a valid number")?;
            anyhow::ensure!(ts_num > 0, "Timestamp is zero or negative");
        } else {
            anyhow::bail!("Timestamp is neither string nor number");
        }

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validate_exception_code() {
        let payload = json!({
            "error": {
                "exception_code": 0xC0000005u64
            }
        });

        let result = WindowsPayloadValidator::new(&payload).validate_exception_code(0xC0000005);

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_exception_code_mismatch() {
        let payload = json!({
            "error": {
                "exception_code": 0xC0000094u64
            }
        });

        let result = WindowsPayloadValidator::new(&payload).validate_exception_code(0xC0000005);

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_stack_exists() {
        let payload = json!({
            "error": {
                "stack": [
                    {"ip": "0x12345678"}
                ]
            }
        });

        let result = WindowsPayloadValidator::new(&payload).validate_stack_exists();

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_stack_empty() {
        let payload = json!({
            "error": {
                "stack": []
            }
        });

        let result = WindowsPayloadValidator::new(&payload).validate_stack_exists();

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_thread_info() {
        let payload = json!({
            "threads": [
                {
                    "crashed": true,
                    "stack": [{"ip": "0x12345678"}]
                }
            ]
        });

        let result = WindowsPayloadValidator::new(&payload).validate_thread_info();

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_os_info() {
        let payload = json!({
            "os_info": {
                "type": "Windows",
                "version": "10.0.19045"
            }
        });

        let result = WindowsPayloadValidator::new(&payload).validate_os_info();

        assert!(result.is_ok());
    }
}
