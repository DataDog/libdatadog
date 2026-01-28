// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Common test utilities shared across exporter tests

#![allow(dead_code)] // Test utilities are used across test modules

use libdd_profiling::exporter::utils::{parse_http_request, HttpRequest, MultipartPart};
use libdd_profiling::exporter::{File, MimeType, ProfileExporter};
use std::path::PathBuf;

/// Test constants
pub const TEST_LIB_NAME: &str = "dd-trace-foo";
pub const TEST_LIB_VERSION: &str = "1.2.3";
pub const FILE_WRITE_DELAY_MS: u64 = 200;

/// RAII guard to ensure test files are cleaned up even if the test panics
pub struct TempFileGuard(PathBuf);

impl Drop for TempFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl std::ops::Deref for TempFileGuard {
    type Target = PathBuf;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<std::path::Path> for TempFileGuard {
    fn as_ref(&self) -> &std::path::Path {
        self.0.as_ref()
    }
}

/// Create a file-based exporter and return the temp file path with auto-cleanup
pub fn create_file_exporter(
    profiling_library_name: &str,
    profiling_library_version: &str,
    family: &str,
    tags: Vec<libdd_common::tag::Tag>,
    api_key: Option<&str>,
) -> anyhow::Result<(ProfileExporter, TempFileGuard)> {
    use libdd_profiling::exporter::config;

    // Create a unique temp file path
    let file_path = std::env::temp_dir().join(format!(
        "libdd_test_{}_{}_{:x}.http",
        std::process::id(),
        chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0),
        rand::random::<u64>()
    ));

    let mut endpoint = config::file(file_path.to_string_lossy().as_ref())?;
    if let Some(key) = api_key {
        endpoint.api_key = Some(key.to_string().into());
    }

    let exporter = ProfileExporter::new(
        profiling_library_name,
        profiling_library_version,
        family,
        tags,
        endpoint,
    )?;

    Ok((exporter, TempFileGuard(file_path)))
}

/// Read and parse the dumped HTTP request file
pub fn read_and_parse_request(file_path: &std::path::Path) -> anyhow::Result<HttpRequest> {
    // Wait for file to be written
    std::thread::sleep(std::time::Duration::from_millis(FILE_WRITE_DELAY_MS));
    let request_bytes = std::fs::read(file_path)?;
    parse_http_request(&request_bytes)
}

/// Extract and parse the event.json part from multipart request
pub fn extract_event_json(request: &HttpRequest) -> anyhow::Result<serde_json::Value> {
    let event_part = request
        .multipart_parts
        .iter()
        .find(|p| p.filename.as_deref() == Some("event.json"))
        .ok_or_else(|| anyhow::anyhow!("event.json part not found"))?;

    Ok(serde_json::from_slice(&event_part.content)?)
}

/// Create standard test additional files with different MIME types
pub fn create_test_additional_files() -> Vec<File<'static>> {
    vec![
        File {
            name: "jit.pprof",
            bytes: b"fake-jit-data",
            mime: MimeType::ApplicationOctetStream,
        },
        File {
            name: "metadata.json",
            bytes: b"{\"test\": true}",
            mime: MimeType::ApplicationJson,
        },
    ]
}

/// Assert that a multipart part has the expected MIME type
pub fn assert_mime_type(parts: &[MultipartPart], part_name: &str, expected_mime: &str) {
    let part = parts
        .iter()
        .find(|p| p.name == part_name)
        .unwrap_or_else(|| panic!("{} part should exist", part_name));
    assert_eq!(
        part.content_type.as_deref(),
        Some(expected_mime),
        "{} should have {} content type",
        part_name,
        expected_mime
    );
}

/// Assert all standard MIME types for a complete export
/// (event, profile.pprof, jit.pprof, metadata.json)
pub fn assert_all_standard_mime_types(parts: &[MultipartPart]) {
    assert_mime_type(parts, "event", "application/json");
    assert_mime_type(parts, "profile.pprof", "application/octet-stream");
    assert_mime_type(parts, "jit.pprof", "application/octet-stream");
    assert_mime_type(parts, "metadata.json", "application/json");
}
