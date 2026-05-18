// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

/// A single part in a multipart form-data payload.
#[derive(Debug, Clone)]
pub struct MultipartPart {
    /// The field name for this part.
    pub name: String,
    /// The part's data.
    pub data: Vec<u8>,
    /// Optional filename for this part.
    pub filename: Option<String>,
    /// Optional MIME content type (e.g. `"application/json"`).
    pub content_type: Option<String>,
}

impl MultipartPart {
    /// Create a new multipart part with the given field name and data.
    pub fn new(name: impl Into<String>, data: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            data: data.into(),
            filename: None,
            content_type: None,
        }
    }

    /// Set the filename for this part.
    pub fn filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    /// Set the MIME content type for this part.
    pub fn content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }
}

const BOUNDARY: &str = "------------------------dd_multipart_boundary";

/// Encoded multipart form-data payload ready for use as an HTTP request body.
#[derive(Debug)]
pub struct MultipartFormData {
    body: Vec<u8>,
}

impl MultipartFormData {
    /// Encode the given parts into a multipart form-data payload.
    pub fn encode(parts: Vec<MultipartPart>) -> Self {
        let mut body = Vec::new();

        for part in parts {
            body.extend_from_slice(b"--");
            body.extend_from_slice(BOUNDARY.as_bytes());
            body.extend_from_slice(b"\r\n");

            // Content-Disposition header
            body.extend_from_slice(b"Content-Disposition: form-data; name=\"");
            body.extend_from_slice(part.name.as_bytes());
            body.extend_from_slice(b"\"");
            if let Some(filename) = &part.filename {
                body.extend_from_slice(b"; filename=\"");
                body.extend_from_slice(filename.as_bytes());
                body.extend_from_slice(b"\"");
            }
            body.extend_from_slice(b"\r\n");

            // Content-Type header (if specified)
            if let Some(ct) = &part.content_type {
                body.extend_from_slice(b"Content-Type: ");
                body.extend_from_slice(ct.as_bytes());
                body.extend_from_slice(b"\r\n");
            }

            // Blank line separating headers from body
            body.extend_from_slice(b"\r\n");

            // Part data
            body.extend_from_slice(&part.data);
            body.extend_from_slice(b"\r\n");
        }

        // Final boundary
        body.extend_from_slice(b"--");
        body.extend_from_slice(BOUNDARY.as_bytes());
        body.extend_from_slice(b"--\r\n");

        Self { body }
    }

    /// The Content-Type header value for this multipart payload.
    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={BOUNDARY}")
    }

    /// Consume this payload and return the encoded body bytes.
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_single_text_field() {
        let form = MultipartFormData::encode(vec![MultipartPart::new("field", b"value".to_vec())]);

        let body = String::from_utf8(form.into_body()).unwrap();
        assert!(body.contains("Content-Disposition: form-data; name=\"field\""));
        assert!(body.contains("value"));
        assert!(body.contains(&format!("--{BOUNDARY}--")));
    }

    #[test]
    fn encode_with_filename_and_content_type() {
        let form = MultipartFormData::encode(vec![MultipartPart::new("file", b"data".to_vec())
            .filename("test.bin")
            .content_type("application/octet-stream")]);

        let body = String::from_utf8(form.into_body()).unwrap();
        assert!(body.contains("filename=\"test.bin\""));
        assert!(body.contains("Content-Type: application/octet-stream"));
    }

    #[test]
    fn encode_multiple_parts() {
        let form = MultipartFormData::encode(vec![
            MultipartPart::new("metadata", br#"{"id":"123"}"#.to_vec())
                .content_type("application/json"),
            MultipartPart::new("file", vec![0xDE, 0xAD, 0xBE, 0xEF])
                .filename("data.bin")
                .content_type("application/octet-stream"),
        ]);

        let body = form.into_body();
        let body_str = String::from_utf8_lossy(&body);

        // Both parts present
        assert!(body_str.contains("name=\"metadata\""));
        assert!(body_str.contains("name=\"file\""));
        assert!(body_str.contains("filename=\"data.bin\""));
    }

    #[test]
    fn content_type_includes_boundary() {
        let form = MultipartFormData::encode(vec![]);
        assert_eq!(
            form.content_type(),
            format!("multipart/form-data; boundary={BOUNDARY}")
        );
    }
}
