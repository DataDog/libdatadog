// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP capability trait and error types.
//!
//! Request and response types are provided by the [`http`] crate, which is a
//! pure-types crate with no platform dependencies (compiles on wasm). The body
//! type is [`bytes::Bytes`].

use crate::maybe_send::MaybeSend;
use core::fmt;
use core::future::Future;

#[derive(Debug, Clone)]
pub enum HttpError {
    Network(String),
    Timeout,
    ResponseBody(String),
    InvalidRequest(String),
    Other(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Network(msg) => write!(f, "Network error: {}", msg),
            HttpError::Timeout => write!(f, "Request timed out"),
            HttpError::ResponseBody(msg) => write!(f, "Response body error: {}", msg),
            HttpError::InvalidRequest(msg) => write!(f, "Invalid request: {}", msg),
            HttpError::Other(msg) => write!(f, "HTTP error: {}", msg),
        }
    }
}

impl std::error::Error for HttpError {}

pub trait HttpClientTrait {
    fn new_client() -> Self;

    fn request(
        &self,
        req: http::Request<bytes::Bytes>,
    ) -> impl Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend;
}
