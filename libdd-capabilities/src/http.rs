// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! HTTP capability trait and error types.
//!
//! Request and response types are provided by the [`http`] crate, which is a
//! pure-types crate with no platform dependencies (compiles on wasm). The body
//! type is [`bytes::Bytes`].

use crate::maybe_send::MaybeSend;
use core::future::Future;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("Network error: {0}")]
    Network(anyhow::Error),
    #[error("Request timed out")]
    Timeout,
    #[error("Response body error: {0}")]
    ResponseBody(anyhow::Error),
    #[error("Invalid request: {0}")]
    InvalidRequest(anyhow::Error),
    #[error("HTTP error: {0}")]
    Other(anyhow::Error),
}

pub trait HttpClientTrait {
    fn new_client() -> Self;

    fn request(
        &self,
        req: http::Request<bytes::Bytes>,
    ) -> impl Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend;
}
