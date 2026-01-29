// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Utility functions for parsing and inspecting HTTP requests and multipart form data.
//!
//! These utilities are primarily useful for testing and debugging profiling exports.

use anyhow::Context as _;
use std::collections::HashMap;

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
/// use libdd_profiling::exporter::utils::parse_http_request;
///
/// let request_bytes = b"POST /v1/input HTTP/1.1\r\nHost: example.com\r\n\r\n";
/// let request = parse_http_request(request_bytes).unwrap();
/// assert_eq!(request.method, "POST");
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
        let value = std::str::from_utf8(header.value)?.to_string();
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

/// Parse multipart form data from Content-Type header and body (internal helper)
///
/// Extracts the boundary from the Content-Type header and parses the multipart body.
/// This is called automatically by `parse_http_request` when appropriate.
fn parse_multipart(content_type: &str, body: &[u8]) -> anyhow::Result<Vec<MultipartPart>> {
    use multipart::server::Multipart;
    use std::io::Cursor;

    // Extract boundary from Content-Type header
    let mime: mime::Mime = content_type
        .parse()
        .context("Failed to parse Content-Type as MIME type")?;

    let boundary = mime
        .get_param(mime::BOUNDARY)
        .context("No boundary parameter found in Content-Type")?
        .as_str();

    // Parse multipart body using the library
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
