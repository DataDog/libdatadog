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

/// RAII guard that restores an environment variable to its previous value when dropped.
///
/// # Example
/// ```no_run
/// use libdd_common::test_utils::EnvGuard;
///
/// let _guard = EnvGuard::set("MY_VAR", "value");
/// // MY_VAR is set to "value"; when _guard drops, it is restored
/// ```
pub struct EnvGuard {
    key: &'static str,
    saved: Option<String>,
}

impl EnvGuard {
    /// Set the environment variable to the given value. The previous value (if any) is restored on drop.
    pub fn set(key: &'static str, value: &str) -> Self {
        let saved = std::env::var(key).ok();
        std::env::set_var(key, value);
        EnvGuard { key, saved }
    }

    /// Remove the environment variable. The previous value (if any) is restored on drop.
    pub fn remove(key: &'static str) -> Self {
        let saved = std::env::var(key).ok();
        std::env::remove_var(key);
        EnvGuard { key, saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.saved {
            Some(s) => std::env::set_var(self.key, s),
            None => std::env::remove_var(self.key),
        }
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

/// Parsed HTTP request components without multipart parsing.
/// This is the shared result from `parse_http_request_headers`.
struct ParsedRequestParts {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn parse_http_request_headers(data: &[u8]) -> anyhow::Result<ParsedRequestParts> {
    let mut header_buf = [httparse::EMPTY_HEADER; 64];
    let mut req = httparse::Request::new(&mut header_buf);

    let headers_len = match req.parse(data)? {
        httparse::Status::Complete(len) => len,
        httparse::Status::Partial => anyhow::bail!("Incomplete HTTP request"),
    };

    let method = req.method.context("No method found")?.to_string();
    let path = req.path.context("No path found")?.to_string();

    let mut headers = HashMap::new();
    for header in req.headers {
        let key = header.name.to_lowercase();
        let value = String::from_utf8_lossy(header.value).into_owned();
        headers.insert(key, value);
    }

    let body = data[headers_len..].to_vec();

    Ok(ParsedRequestParts {
        method,
        path,
        headers,
        body,
    })
}

fn extract_multipart_boundary(content_type: &str) -> anyhow::Result<String> {
    let mime: mime::Mime = content_type
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse Content-Type as MIME type: {}", e))?;

    let boundary = mime
        .get_param(mime::BOUNDARY)
        .context("No boundary parameter found in Content-Type")?
        .to_string();

    Ok(boundary)
}

/// Parse an HTTP request from raw bytes (async version).
///
/// If the Content-Type header indicates multipart/form-data, the multipart body will be
/// automatically parsed and available in the `multipart_parts` field.
///
/// Use this function in async contexts (e.g., `#[tokio::test]`). For synchronous contexts,
/// use [`parse_http_request_sync`] instead.
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
/// # async fn example() -> anyhow::Result<()> {
/// let request_bytes = b"POST /v1/input HTTP/1.1\r\nHost: example.com\r\n\r\nbody";
/// let request = parse_http_request(request_bytes).await?;
/// assert_eq!(request.method, "POST");
/// assert_eq!(request.path, "/v1/input");
/// # Ok(())
/// # }
/// ```
pub async fn parse_http_request(data: &[u8]) -> anyhow::Result<HttpRequest> {
    let parts = parse_http_request_headers(data)?;

    // Auto-parse multipart if Content-Type indicates multipart/form-data
    let multipart_parts = match parts.headers.get("content-type") {
        Some(ct) if ct.contains("multipart/form-data") => {
            let boundary = extract_multipart_boundary(ct)?;
            parse_multipart(boundary, parts.body.clone()).await?
        }
        _ => Vec::new(),
    };

    Ok(HttpRequest {
        method: parts.method,
        path: parts.path,
        headers: parts.headers,
        body: parts.body,
        multipart_parts,
    })
}

/// Parse an HTTP request from raw bytes (sync version).
///
/// If the Content-Type header indicates multipart/form-data, the multipart body will be
/// automatically parsed and available in the `multipart_parts` field.
///
/// **Note:** This function uses `futures::executor::block_on` internally for multipart parsing.
/// In async contexts (e.g., `#[tokio::test]`), prefer [`parse_http_request`] to avoid blocking
/// the async runtime.
///
/// # Arguments
/// * `data` - Raw HTTP request bytes including headers and body
///
/// # Returns
/// A parsed `HttpRequest` or an error if parsing fails
///
/// # Example
/// ```no_run
/// use libdd_common::test_utils::parse_http_request_sync;
///
/// let request_bytes = b"POST /v1/input HTTP/1.1\r\nHost: example.com\r\n\r\nbody";
/// let request = parse_http_request_sync(request_bytes).unwrap();
/// assert_eq!(request.method, "POST");
/// assert_eq!(request.path, "/v1/input");
/// ```
pub fn parse_http_request_sync(data: &[u8]) -> anyhow::Result<HttpRequest> {
    let parts = parse_http_request_headers(data)?;

    // Auto-parse multipart if Content-Type indicates multipart/form-data
    let multipart_parts = match parts.headers.get("content-type") {
        Some(ct) if ct.contains("multipart/form-data") => {
            let boundary = extract_multipart_boundary(ct)?;
            futures::executor::block_on(parse_multipart(boundary, parts.body.clone()))?
        }
        _ => Vec::new(),
    };

    Ok(HttpRequest {
        method: parts.method,
        path: parts.path,
        headers: parts.headers,
        body: parts.body,
        multipart_parts,
    })
}

async fn parse_multipart(boundary: String, body: Vec<u8>) -> anyhow::Result<Vec<MultipartPart>> {
    use futures_util::stream::once;

    let stream = once(async move { Ok::<_, std::io::Error>(bytes::Bytes::from(body)) });
    let mut multipart = multer::Multipart::new(stream, boundary);
    let mut parts = Vec::new();

    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or_default().to_string();
        let filename = field.file_name().map(|s| s.to_string());
        let content_type = field.content_type().map(|m| m.to_string());
        let content = field.bytes().await?.to_vec();

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
        let parsed = parse_http_request_sync(request).unwrap();

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
        let parsed = parse_http_request_sync(request).unwrap();

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

        let parsed = parse_http_request_sync(&request_bytes).unwrap();

        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.multipart_parts.len(), 1);
        assert_eq!(parsed.multipart_parts[0].name, "field");
        assert_eq!(parsed.multipart_parts[0].content, b"value");
    }

    #[tokio::test]
    async fn test_parse_http_request_async_with_multipart() {
        let content_type = "multipart/form-data; boundary=----WebKitFormBoundary";
        let body = b"------WebKitFormBoundary\r\nContent-Disposition: form-data; name=\"field\"\r\n\r\nvalue\r\n------WebKitFormBoundary--";
        let request = format!(
            "POST /v1/input HTTP/1.1\r\nHost: example.com\r\nContent-Type: {}\r\n\r\n",
            content_type
        );
        let mut request_bytes = request.into_bytes();
        request_bytes.extend_from_slice(body);

        let parsed = parse_http_request(&request_bytes).await.unwrap();

        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.multipart_parts.len(), 1);
        assert_eq!(parsed.multipart_parts[0].name, "field");
        assert_eq!(parsed.multipart_parts[0].content, b"value");
    }
}
