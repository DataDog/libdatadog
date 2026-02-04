// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Common test utilities for libdatadog crates.
//!
//! This module provides shared helper functions and types for testing,
//! including file cleanup, HTTP parsing, and temp file management.

use anyhow::Context;
use std::collections::HashMap;
use std::path::PathBuf;

/// RAII guard to ensure test files are cleaned up even if the test panics
///
/// # Example
/// ```no_run
/// use libdd_common::test_utils::TempFileGuard;
/// use std::path::PathBuf;
///
/// let file = TempFileGuard::new(PathBuf::from("/tmp/test.txt"));
/// // Use file...
/// // File is automatically deleted when guard goes out of scope
/// ```
pub struct TempFileGuard(PathBuf);

impl TempFileGuard {
    /// Create a new temp file guard for the given path
    pub fn new(path: PathBuf) -> Self {
        Self(path)
    }
}

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

/// Create a unique temporary file path with the given prefix
///
/// The path will be in the system temp directory with a unique name based on
/// process ID and a random number to avoid collisions.
///
/// # Arguments
/// * `prefix` - Prefix for the temp file name
/// * `extension` - File extension (without the dot)
///
/// # Returns
/// A `TempFileGuard` that will automatically clean up the file when dropped
///
/// # Example
/// ```no_run
/// use libdd_common::test_utils::create_temp_file_path;
///
/// let file = create_temp_file_path("test", "txt");
/// // file path is something like /tmp/test_12345_abc123.txt
/// ```
pub fn create_temp_file_path(prefix: &str, extension: &str) -> TempFileGuard {
    let file_path = std::env::temp_dir().join(format!(
        "{}_{}_{:x}.{}",
        prefix,
        std::process::id(),
        rand::random::<u64>(),
        extension
    ));
    TempFileGuard::new(file_path)
}

/// Represents a parsed HTTP request
#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
    pub multipart_parts: Vec<MultipartPart>,
}

/// Represents a parsed multipart form part
#[derive(Debug)]
pub struct MultipartPart {
    pub name: String,
    pub filename: Option<String>,
    pub content_type: Option<String>,
    pub content: Vec<u8>,
}

/// Parse an HTTP request from raw bytes
///
/// If the Content-Type header indicates multipart/form-data, the multipart body will be
/// automatically parsed and available in the `multipart_parts` field.
///
/// # Arguments
/// * `data` - Raw HTTP request bytes including headers and body
///
/// # Returns
/// A parsed `HttpRequest` or an error if parsing fails
///
/// # Example
/// ```no_run
/// use libdd_common::test_utils::parse_http_request;
///
/// let request_bytes = b"POST /v1/input HTTP/1.1\r\nHost: example.com\r\n\r\nbody";
/// let request = parse_http_request(request_bytes).unwrap();
/// assert_eq!(request.method, "POST");
/// assert_eq!(request.path, "/v1/input");
/// ```
pub fn parse_http_request(data: &[u8]) -> anyhow::Result<HttpRequest> {
    let mut header_buf = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut header_buf);

    let headers_len = match req.parse(data)? {
        httparse::Status::Complete(len) => len,
        httparse::Status::Partial => anyhow::bail!("Incomplete HTTP request"),
    };

    let method = req.method.context("No method found")?.to_string();
    let path = req.path.context("No path found")?.to_string();

    // Convert headers to HashMap with lowercase keys
    let mut headers = HashMap::new();
    for header in req.headers {
        let key = header.name.to_lowercase();
        let value = String::from_utf8_lossy(header.value).into_owned();
        headers.insert(key, value);
    }

    let body = data[headers_len..].to_vec();

    // Auto-parse multipart if Content-Type indicates multipart/form-data
    let multipart_parts = match headers.get("content-type") {
        Some(ct) if ct.contains("multipart/form-data") => parse_multipart(ct, &body)?,
        _ => Vec::new(),
    };

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
        multipart_parts,
    })
}

/// Parse multipart form data from Content-Type header and body
///
/// Extracts the boundary from the Content-Type header and parses the multipart body.
fn parse_multipart(content_type: &str, body: &[u8]) -> anyhow::Result<Vec<MultipartPart>> {
    use multipart::server::Multipart;
    use std::io::Cursor;

    // Extract boundary from Content-Type header
    let mime: mime::Mime = content_type
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse Content-Type as MIME type: {}", e))?;

    let boundary = mime
        .get_param(mime::BOUNDARY)
        .context("No boundary parameter found in Content-Type")?
        .as_str();

    // Parse multipart body
    let cursor = Cursor::new(body);
    let mut multipart = Multipart::with_body(cursor, boundary);
    let mut parts = Vec::new();

    while let Some(mut field) = multipart.read_entry()? {
        let headers = &field.headers;
        let name = headers.name.to_string();
        let filename = headers.filename.clone();
        let content_type = headers.content_type.as_ref().map(|ct| ct.to_string());

        let mut content = Vec::new();
        std::io::Read::read_to_end(&mut field.data, &mut content)?;

        parts.push(MultipartPart {
            name,
            filename,
            content_type,
            content,
        });
    }

    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temp_file_guard_and_path_generation() {
        // Test that create_temp_file_path generates correct path format
        let guard = create_temp_file_path("test_prefix", "dat");

        // Verify path format
        assert!(guard
            .as_ref()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("test_prefix_"));
        assert_eq!(guard.as_ref().extension().unwrap(), "dat");

        // Test RAII cleanup: create file and verify it's cleaned up when guard drops
        std::fs::write(&guard, b"test").expect("should write");
        assert!(guard.as_ref().exists());

        // Clone path for verification after guard is dropped
        let path = guard.as_ref().to_path_buf();
        drop(guard); // explicitly drop guard

        // File should be cleaned up
        assert!(!path.exists());
    }

    #[test]
    fn test_parse_http_request_basic() {
        let request = b"POST /v1/input HTTP/1.1\r\nHost: example.com\r\nContent-Type: application/json\r\n\r\n{\"test\":true}";
        let parsed = parse_http_request(request).unwrap();

        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/v1/input");
        assert_eq!(parsed.headers.get("host").unwrap(), "example.com");
        assert_eq!(
            parsed.headers.get("content-type").unwrap(),
            "application/json"
        );
        assert_eq!(parsed.body, b"{\"test\":true}");
        assert!(parsed.multipart_parts.is_empty());
    }

    #[test]
    fn test_parse_http_request_with_custom_headers() {
        let request =
            b"GET /test HTTP/1.1\r\nX-Custom-Header: value\r\nAnother-Header: 123\r\n\r\n";
        let parsed = parse_http_request(request).unwrap();

        assert_eq!(parsed.method, "GET");
        assert_eq!(parsed.path, "/test");
        assert_eq!(parsed.headers.get("x-custom-header").unwrap(), "value");
        assert_eq!(parsed.headers.get("another-header").unwrap(), "123");
        assert!(parsed.body.is_empty());
        assert!(parsed.multipart_parts.is_empty());
    }

    #[test]
    fn test_parse_http_request_with_multipart() {
        let content_type = "multipart/form-data; boundary=----WebKitFormBoundary";
        let body = b"------WebKitFormBoundary\r\nContent-Disposition: form-data; name=\"field\"\r\n\r\nvalue\r\n------WebKitFormBoundary--";
        let request = format!(
            "POST /v1/input HTTP/1.1\r\nHost: example.com\r\nContent-Type: {}\r\n\r\n",
            content_type
        );
        let mut request_bytes = request.into_bytes();
        request_bytes.extend_from_slice(body);

        let parsed = parse_http_request(&request_bytes).unwrap();

        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.multipart_parts.len(), 1);
        assert_eq!(parsed.multipart_parts[0].name, "field");
        assert_eq!(parsed.multipart_parts[0].content, b"value");
    }
}
