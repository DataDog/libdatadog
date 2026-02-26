// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP response type for `libdd-http-client`.

/// An HTTP response received from the server.
#[derive(Debug)]
pub struct HttpResponse {
    /// HTTP status code (e.g. 200, 404, 503).
    pub status_code: u16,

    /// Response headers as a list of (name, value) pairs.
    pub headers: Vec<(String, String)>,

    /// Response body bytes.
    pub body: bytes::Bytes,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_accessible() {
        let resp = HttpResponse {
            status_code: 200,
            headers: vec![("content-type".to_owned(), "application/json".to_owned())],
            body: bytes::Bytes::from_static(b"{\"ok\":true}"),
        };
        assert_eq!(resp.status_code, 200);
        assert_eq!(resp.headers.len(), 1);
        assert_eq!(resp.headers[0].0, "content-type");
        assert_eq!(resp.headers[0].1, "application/json");
        assert_eq!(resp.body.as_ref(), b"{\"ok\":true}");
    }

    #[test]
    fn empty_response() {
        let resp = HttpResponse {
            status_code: 204,
            headers: vec![],
            body: bytes::Bytes::new(),
        };
        assert_eq!(resp.status_code, 204);
        assert!(resp.headers.is_empty());
        assert!(resp.body.is_empty());
    }

    #[test]
    fn debug_includes_status() {
        let resp = HttpResponse {
            status_code: 404,
            headers: vec![],
            body: bytes::Bytes::new(),
        };
        assert!(format!("{resp:?}").contains("404"));
    }
}
