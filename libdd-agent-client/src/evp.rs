// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Types specific to [`crate::AgentClient::send_evp_event`].

/// A single EVP (Event Platform) event to send via [`crate::AgentClient::send_evp_event`].
///
/// The agent forwards the request to `<subdomain>.datadoghq.com<path>`.
#[derive(Debug, Clone)]
pub struct EvpEventRequest {
    /// Target intake subdomain, injected as `X-Datadog-EVP-Subdomain`
    /// (e.g. `"event-platform-intake"`).
    pub subdomain: String,
    /// Endpoint path on the intake (e.g. `"/api/v2/exposures"`). Must start with `'/'`.
    pub path: String,
    /// Pre-serialized payload body.
    pub body: bytes::Bytes,
    /// Value for the `Content-Type` header (e.g. `"application/json"`).
    pub content_type: String,
}
