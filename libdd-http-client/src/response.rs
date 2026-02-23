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
