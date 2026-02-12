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

/// Count the number of active threads in the current process.
///
/// This function works across different platforms:
/// - **Linux**: Reads from `/proc/self/status`
/// - **macOS**: Uses `ps -M` command
/// - **Windows**: Uses the Toolhelp32Snapshot API
///
/// # Returns
/// The number of active threads in the current process, or an error if the count cannot be determined.
///
/// # Example
/// ```no_run
/// use libdd_common::test_utils::count_active_threads;
///
/// let thread_count = count_active_threads().unwrap();
/// println!("Current process has {} threads", thread_count);
/// ```
pub fn count_active_threads() -> anyhow::Result<usize> {
    #[cfg(target_os = "linux")]
    {
        use std::fs;
        use std::io::BufRead;

        let status = fs::read_to_string("/proc/self/status")?;
        for line in status.lines() {
            if line.starts_with("Threads:") {
                let count = line
                    .split_whitespace()
                    .nth(1)
                    .context("Failed to parse thread count from /proc/self/status")?
                    .parse::<usize>()?;
                return Ok(count);
            }
        }
        anyhow::bail!("Threads: line not found in /proc/self/status");
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;

        let pid = std::process::id();
        let output = Command::new("ps")
            .args(&["-M", "-p", &pid.to_string()])
            .output()
            .context("Failed to execute ps command")?;

        if !output.status.success() {
            anyhow::bail!("ps command failed with status: {:?}", output.status);
        }

        let stdout = String::from_utf8(output.stdout)
            .context("Failed to parse ps output as UTF-8")?;

        // ps -M output format: header line + one line per thread
        // Count lines and subtract 1 for the header
        let lines: Vec<&str> = stdout.lines().collect();
        if lines.is_empty() {
            anyhow::bail!("ps output is empty");
        }

        // Subtract 1 for the header line
        let thread_count = lines.len().saturating_sub(1);
        Ok(thread_count)
    }

    #[cfg(windows)]
    {
        use std::mem::size_of;
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Diagnostics::ToolHelp::{
            CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD,
            THREADENTRY32,
        };

        let current_pid = std::process::id();
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE {
            anyhow::bail!("CreateToolhelp32Snapshot failed");
        }

        let mut thread_entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..Default::default()
        };

        let mut count = 0;
        if unsafe { Thread32First(snapshot, &mut thread_entry) } != 0 {
            loop {
                if thread_entry.th32OwnerProcessID == current_pid {
                    count += 1;
                }

                if unsafe { Thread32Next(snapshot, &mut thread_entry) } == 0 {
                    break;
                }
            }
        }

        unsafe {
            CloseHandle(snapshot);
        }

        Ok(count)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        anyhow::bail!("Thread counting is not implemented for this platform");
    }
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

    #[test]
    fn test_count_active_threads() {
        let initial_count = count_active_threads().expect("Failed to count threads");
        assert!(initial_count >= 1, "Expected at least 1 thread, got {}", initial_count);

        // Spawn some threads and verify the count increases
        use std::sync::{Arc, Barrier};
        let barrier = Arc::new(Barrier::new(6)); // 5 spawned threads + main thread

        let handles: Vec<_> = (0..5)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                })
            })
            .collect();

        barrier.wait();
        let count_with_threads = count_active_threads().expect("Failed to count threads");

        for handle in handles {
            handle.join().expect("Thread should join successfully");
        }

        assert_eq!(
            count_with_threads,
            initial_count + 5,
            "Expected exactly {} threads (initial: {}, with spawned: {})",
            initial_count + 5,
            initial_count,
            count_with_threads
        );
    }
}
