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
///     .and_then(|v| v.validate_uuid_present())
///     .and_then(|v| v.validate_error_stack_exists())
///     .and_then(|v| v.validate_threads())?;
/// ```
pub struct WindowsPayloadValidator<'a> {
    payload: &'a Value,
}

impl<'a> WindowsPayloadValidator<'a> {
    /// Creates a new Windows payload validator.
    pub fn new(payload: &'a Value) -> Result<Self> {
        Ok(Self { payload })
    }

    /// Validates that a stack trace exists and has at least one frame.
    pub fn validate_error_stack_exists(self) -> Result<Self> {
        let stack = self.payload["error"]["stack"]["frames"]
            .as_array()
            .context("Missing or invalid stack in error section")?;

        anyhow::ensure!(!stack.is_empty(), "Stack trace is empty");

        Ok(self)
    }

    /// Validates that the UUID field is present and not empty.
    pub fn validate_uuid_present(self) -> Result<Self> {
        let uuid = self.payload["uuid"]
            .as_str()
            .context("Missing or invalid uuid field")?;

        anyhow::ensure!(!uuid.is_empty(), "UUID is empty");

        // Basic UUID format validation (8-4-4-4-12 hex digits)
        let parts: Vec<&str> = uuid.split('-').collect();
        anyhow::ensure!(
            parts.len() == 5,
            "UUID does not have correct format (expected 5 parts separated by dashes)"
        );

        Ok(self)
    }

    /// Validates that error.is_crash is set to true.
    pub fn validate_is_crash_report(self) -> Result<Self> {
        let is_crash = self.payload["error"]["is_crash"]
            .as_bool()
            .context("Missing or invalid error.is_crash field")?;

        anyhow::ensure!(is_crash, "Expected is_crash to be true, got false");

        Ok(self)
    }

    /// Validates that data_schema_version field is set.
    pub fn validate_data_schema_version(self) -> Result<Self> {
        let version = self.payload["data_schema_version"]
            .as_str()
            .context("Missing or invalid data_schema_version field")?;

        anyhow::ensure!(!version.is_empty(), "data_schema_version is empty");

        Ok(self)
    }

    /// Validates that error.kind is set to "Panic".
    pub fn validate_error_kind_is_panic(self) -> Result<Self> {
        let kind = self.payload["error"]["kind"]
            .as_str()
            .context("Missing or invalid error.kind field")?;

        anyhow::ensure!(
            kind == "Panic",
            "Expected error.kind to be 'Panic', got '{}'",
            kind
        );

        Ok(self)
    }

    /// Validates that error.source_type is set to "Crashtracking" (case insensitive).
    pub fn validate_source_type(self) -> Result<Self> {
        let source_type = self.payload["error"]["source_type"]
            .as_str()
            .context("Missing or invalid error.source_type field")?;

        anyhow::ensure!(
            source_type.eq_ignore_ascii_case("Crashtracking"),
            "Expected source_type to be 'Crashtracking' (case insensitive), got '{}'",
            source_type
        );

        Ok(self)
    }

    /// Validates that the incomplete field is set to false.
    pub fn validate_report_is_complete(self) -> Result<Self> {
        let incomplete = self.payload["incomplete"]
            .as_bool()
            .context("Missing or invalid incomplete field")?;

        anyhow::ensure!(!incomplete, "Expected incomplete to be false, got true");

        Ok(self)
    }

    /// Validates thread information in the crash report.
    pub fn validate_threads(self) -> Result<Self> {
        let threads = self.payload["error"]["threads"]
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
            crashed_thread["stack"]["frames"].is_array(),
            "Crashed thread has no stack information"
        );

        let crashed_stack = crashed_thread["stack"]["frames"]
            .as_array()
            .context("Crashed thread stack is not an array")?;

        anyhow::ensure!(!crashed_stack.is_empty(), "Crashed thread stack is empty");

        Ok(self)
    }

    /// Validates that all required OS information fields are set.
    /// According to the schema, os_info must have: architecture, bitness, os_type, version.
    pub fn validate_os_info(self) -> Result<Self> {
        let os_info = &self.payload["os_info"];

        // Validate architecture field
        let architecture = os_info["architecture"]
            .as_str()
            .context("Missing or invalid os_info.architecture field")?;
        anyhow::ensure!(!architecture.is_empty(), "os_info.architecture is empty");

        // Validate bitness field
        let bitness = os_info["bitness"]
            .as_str()
            .context("Missing or invalid os_info.bitness field")?;
        anyhow::ensure!(!bitness.is_empty(), "os_info.bitness is empty");

        // Validate os_type field (and check it's Windows)
        let os_type = os_info["os_type"]
            .as_str()
            .context("Missing or invalid os_info.os_type field")?;
        anyhow::ensure!(
            os_type.to_lowercase().contains("windows"),
            "Expected Windows os_type, got: {}",
            os_type
        );

        // Validate version field
        let version = os_info["version"]
            .as_str()
            .context("Missing or invalid os_info.version field")?;
        anyhow::ensure!(!version.is_empty(), "os_info.version is empty");

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

    /// Validates the error message matches the expected Windows exception format.
    ///
    /// Checks that the message is in the format:
    /// "Process was terminated due to an unhandled exception '<name>' (0x<CODE>)"
    ///
    /// # Arguments
    /// * `exception` - The expected exception code (e.g., `ExceptionCode::AccessViolation`)
    ///
    /// # Example
    /// ```ignore
    /// use libdd_crashtracker::ExceptionCode;
    /// validator.validate_error_message(ExceptionCode::AccessViolation)?;
    /// ```
    pub fn validate_error_message(
        self,
        exception: libdd_crashtracker::ExceptionCode,
    ) -> Result<Self> {
        let message = self.payload["error"]["message"]
            .as_str()
            .context("Missing or invalid error message")?;

        let expected_message = format!(
            "Process was terminated due to an unhandled exception '{}' (0x{:X})",
            exception.name(),
            exception.code() as u32
        );

        anyhow::ensure!(
            message == expected_message,
            "Error message mismatch.\nExpected: '{}'\nGot:      '{}'",
            expected_message,
            message
        );

        Ok(self)
    }

    /// Validates that the incomplete field matches the expected value.
    ///
    /// # Arguments
    /// * `expected_incomplete` - The expected value for the incomplete field
    ///
    /// # Example
    /// ```ignore
    /// // For stack overflow crashes that may have incomplete stacks
    /// validator.validate_incomplete_stack(true)?;
    ///
    /// // For normal crashes with complete stacks
    /// validator.validate_incomplete_stack(false)?;
    /// ```
    pub fn validate_incomplete_stack(self, expected_incomplete: bool) -> Result<Self> {
        let incomplete = self.payload["incomplete"]
            .as_bool()
            .context("Missing or invalid incomplete field")?;

        anyhow::ensure!(
            incomplete == expected_incomplete,
            "Expected incomplete to be {}, got {}",
            expected_incomplete,
            incomplete
        );

        Ok(self)
    }

    /// Validates that the timestamp field is set and non-empty.
    pub fn validate_timestamp(self) -> Result<Self> {
        let timestamp = self.payload["timestamp"]
            .as_str()
            .context("Missing or invalid timestamp field")?;

        anyhow::ensure!(!timestamp.is_empty(), "timestamp is empty");

        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_validate_stack_exists() {
        let payload = json!({
            "error": {
                "stack": {
                    "frames": [
                        {"ip": "0x12345678"}
                    ]
                }
            }
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_error_stack_exists());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_stack_empty() {
        let payload = json!({
            "error": {
                "stack": {
                    "frames": []
                }
            }
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_error_stack_exists());

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_uuid_present() {
        let payload = json!({
            "uuid": "550e8400-e29b-41d4-a716-446655440000"
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_uuid_present());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_is_crash_report() {
        let payload = json!({
            "error": {
                "is_crash": true
            }
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_is_crash_report());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_os_info() {
        let payload = json!({
            "os_info": {
                "architecture": "x86_64",
                "bitness": "64",
                "os_type": "Windows",
                "version": "10.0.19045"
            }
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_os_info());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_data_schema_version() {
        let payload = json!({
            "data_schema_version": "1.4"
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_data_schema_version());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_data_schema_version_missing() {
        let payload = json!({});

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_data_schema_version());

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_error_kind_is_panic() {
        let payload = json!({
            "error": {
                "kind": "Panic"
            }
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_error_kind_is_panic());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_error_kind_wrong() {
        let payload = json!({
            "error": {
                "kind": "UnhandledException"
            }
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_error_kind_is_panic());

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_source_type() {
        let payload = json!({
            "error": {
                "source_type": "Crashtracking"
            }
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_source_type());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_source_type_case_insensitive() {
        let payload = json!({
            "error": {
                "source_type": "CRASHTRACKING"
            }
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_source_type());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_report_is_complete() {
        let payload = json!({
            "incomplete": false
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_report_is_complete());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_report_incomplete() {
        let payload = json!({
            "incomplete": true
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_report_is_complete());

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_incomplete_stack_true() {
        let payload = json!({
            "incomplete": true
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_incomplete_stack(true));

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_incomplete_stack_false() {
        let payload = json!({
            "incomplete": false
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_incomplete_stack(false));

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_incomplete_stack_mismatch() {
        let payload = json!({
            "incomplete": true
        });

        let result =
            WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_incomplete_stack(false));

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_threads() {
        let payload = json!({
            "error": {
                "threads": [
                    {
                        "crashed": true,
                        "stack": {
                            "frames": [{"ip": "0x12345678"}]
                        }
                    }
                ]
            }
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_threads());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_threads_no_crashed_thread() {
        let payload = json!({
            "error": {
                "threads": [
                    {
                        "crashed": false,
                        "stack": {
                            "frames": [{"ip": "0x12345678"}]
                        }
                    }
                ]
            }
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_threads());

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_metadata() {
        let payload = json!({
            "metadata": {
                "library_name": "libdatadog",
                "library_version": "1.0.0"
            }
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_metadata());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_metadata_missing_field() {
        let payload = json!({
            "metadata": {
                "library_name": "libdatadog"
            }
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_metadata());

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_error_message() {
        use libdd_crashtracker::ExceptionCode;

        let payload = json!({
            "error": {
                "message": "Process was terminated due to an unhandled exception 'EXCEPTION_ACCESS_VIOLATION' (0xC0000005)"
            }
        });

        let result = WindowsPayloadValidator::new(&payload)
            .and_then(|v| v.validate_error_message(ExceptionCode::AccessViolation));

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_error_message_wrong() {
        use libdd_crashtracker::ExceptionCode;

        let payload = json!({
            "error": {
                "message": "Wrong message"
            }
        });

        let result = WindowsPayloadValidator::new(&payload)
            .and_then(|v| v.validate_error_message(ExceptionCode::AccessViolation));

        assert!(result.is_err());
    }

    #[test]
    fn test_validate_timestamp() {
        let payload = json!({
            "timestamp": "2025-11-27T12:34:56Z"
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_timestamp());

        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_timestamp_empty() {
        let payload = json!({
            "timestamp": ""
        });

        let result = WindowsPayloadValidator::new(&payload).and_then(|v| v.validate_timestamp());

        assert!(result.is_err());
    }
}
