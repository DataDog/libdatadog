// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Utility functions for parsing and inspecting HTTP requests and multipart form data.
//!
//! These utilities are primarily useful for testing and debugging profiling exports.

use std::collections::HashMap;

/// Represents a parsed HTTP request
#[derive(Debug)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
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
/// # Arguments
/// * `data` - Raw HTTP request bytes including headers and body
///
/// # Returns
/// A parsed `HttpRequest` or an error if parsing fails
///
/// # Example
/// ```no_run
/// use libdd_profiling::exporter::utils::parse_http_request;
///
/// let request_bytes = b"POST /v1/input HTTP/1.1\r\nHost: example.com\r\n\r\n";
/// let request = parse_http_request(request_bytes).unwrap();
/// assert_eq!(request.method, "POST");
/// ```
pub fn parse_http_request(data: &[u8]) -> anyhow::Result<HttpRequest> {
    // Split headers and body by double CRLF
    let separator = b"\r\n\r\n";
    let split_pos = data
        .windows(separator.len())
        .position(|window| window == separator)
        .ok_or_else(|| anyhow::anyhow!("No header/body separator found"))?;

    let header_section = &data[..split_pos];
    let body = &data[split_pos + separator.len()..];

    // Parse headers
    let header_str = std::str::from_utf8(header_section)?;
    let mut lines = header_str.lines();

    // Parse request line
    let request_line = lines
        .next()
        .ok_or_else(|| anyhow::anyhow!("No request line found"))?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        anyhow::bail!("Invalid request line");
    }
    let method = parts[0].to_string();
    let path = parts[1].to_string();

    // Parse headers
    let mut headers = HashMap::new();
    for line in lines {
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim().to_lowercase();
            let value = line[colon_pos + 1..].trim().to_string();
            headers.insert(key, value);
        }
    }

    Ok(HttpRequest {
        method,
        path,
        headers,
        body: body.to_vec(),
    })
}

/// Extract multipart boundary from Content-Type header
///
/// # Arguments
/// * `content_type` - The Content-Type header value (e.g., "multipart/form-data;
///   boundary=----abc123")
///
/// # Returns
/// The boundary string or an error if not found
///
/// # Example
/// ```
/// use libdd_profiling::exporter::utils::extract_boundary;
///
/// let content_type = "multipart/form-data; boundary=----WebKitFormBoundary123";
/// let boundary = extract_boundary(content_type).unwrap();
/// assert_eq!(boundary, "----WebKitFormBoundary123");
/// ```
pub fn extract_boundary(content_type: &str) -> anyhow::Result<String> {
    let boundary_prefix = "boundary=";
    let boundary = content_type
        .split(';')
        .find_map(|part| {
            let part = part.trim();
            part.strip_prefix(boundary_prefix)
                .map(|s| s.trim().to_string())
        })
        .ok_or_else(|| anyhow::anyhow!("No boundary found in Content-Type"))?;
    Ok(boundary)
}

/// Parse multipart form data
///
/// # Arguments
/// * `body` - The raw body bytes containing multipart data
/// * `boundary` - The multipart boundary string (without leading dashes)
///
/// # Returns
/// A vector of parsed `MultipartPart` instances
///
/// # Example
/// ```no_run
/// use libdd_profiling::exporter::utils::parse_multipart;
///
/// let body = b"--boundary\r\nContent-Disposition: form-data; name=\"field\"\r\n\r\nvalue\r\n--boundary--";
/// let parts = parse_multipart(body, "boundary").unwrap();
/// assert_eq!(parts.len(), 1);
/// ```
pub fn parse_multipart(body: &[u8], boundary: &str) -> anyhow::Result<Vec<MultipartPart>> {
    let delimiter = format!("--{}", boundary);
    let delimiter_bytes = delimiter.as_bytes();
    let end_delimiter = format!("--{}--", boundary);
    let end_delimiter_bytes = end_delimiter.as_bytes();

    let mut parts = Vec::new();
    let mut pos = 0;

    // Skip to first boundary
    if let Some(first_boundary) = find_subsequence(&body[pos..], delimiter_bytes) {
        pos += first_boundary + delimiter_bytes.len();
        // Skip CRLF after boundary
        if pos + 2 <= body.len() && &body[pos..pos + 2] == b"\r\n" {
            pos += 2;
        }
    } else {
        anyhow::bail!("No multipart boundary found");
    }

    loop {
        // Check if we've reached the end delimiter
        if body[pos..].starts_with(end_delimiter_bytes) {
            break;
        }

        // Find the next boundary or end
        let next_delimiter_pos = find_subsequence(&body[pos..], delimiter_bytes)
            .ok_or_else(|| anyhow::anyhow!("Expected delimiter not found"))?;

        // Extract this part's data (remove trailing CRLF before delimiter)
        let part_end = pos + next_delimiter_pos;
        let part_data = &body[pos..part_end];

        // Parse the part
        if let Ok(part) = parse_multipart_part(part_data) {
            parts.push(part);
        }

        // Move past the delimiter
        pos = part_end + delimiter_bytes.len();
        
        // Check if this is the end delimiter (delimiter followed by --)
        if pos + 2 <= body.len() && &body[pos..pos + 2] == b"--" {
            break;
        }
        
        // Skip CRLF after boundary
        if pos + 2 <= body.len() && &body[pos..pos + 2] == b"\r\n" {
            pos += 2;
        }
    }

    Ok(parts)
}

/// Parse a single multipart part
///
/// # Arguments
/// * `data` - Raw bytes for a single multipart part (including headers and content)
///
/// # Returns
/// A parsed `MultipartPart` or an error if parsing fails
pub fn parse_multipart_part(data: &[u8]) -> anyhow::Result<MultipartPart> {
    // Find header/body separator
    let separator = b"\r\n\r\n";
    let split_pos = data
        .windows(separator.len())
        .position(|window| window == separator)
        .ok_or_else(|| anyhow::anyhow!("No part header/body separator"))?;

    let header_section = &data[..split_pos];
    let mut content = data[split_pos + separator.len()..].to_vec();

    // Remove trailing CRLF from content if present
    if content.ends_with(b"\r\n") {
        content.truncate(content.len() - 2);
    }

    // Parse part headers
    let header_str = std::str::from_utf8(header_section)?;
    let mut name = String::new();
    let mut filename = None;
    let mut content_type = None;

    for line in header_str.lines() {
        let lower_line = line.to_lowercase();
        if lower_line.starts_with("content-disposition:") {
            // Extract name and filename
            for part in line.split(';') {
                let part = part.trim();
                if let Some(name_value) = part.strip_prefix("name=") {
                    name = name_value.trim_matches('"').to_string();
                } else if let Some(filename_value) = part.strip_prefix("filename=") {
                    filename = Some(filename_value.trim_matches('"').to_string());
                }
            }
        } else if lower_line.starts_with("content-type:") {
            if let Some(colon_pos) = line.find(':') {
                content_type = Some(line[colon_pos + 1..].trim().to_string());
            }
        }
    }

    Ok(MultipartPart {
        name,
        filename,
        content_type,
        content,
    })
}

/// Helper to find subsequence in bytes
fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_http_request() {
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
    }

    #[test]
    fn test_extract_boundary() {
        let content_type = "multipart/form-data; boundary=----WebKitFormBoundary123";
        let boundary = extract_boundary(content_type).unwrap();
        assert_eq!(boundary, "----WebKitFormBoundary123");
    }

    #[test]
    fn test_parse_multipart_simple() {
        let body = b"------WebKitFormBoundary\r\nContent-Disposition: form-data; name=\"field\"\r\n\r\nvalue\r\n------WebKitFormBoundary--";
        let parts = parse_multipart(body, "----WebKitFormBoundary").unwrap();

        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].name, "field");
        assert_eq!(parts[0].content, b"value");
    }
}
