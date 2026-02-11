// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Capability trait implementations.

#[cfg(not(target_arch = "wasm32"))]
pub mod http;

#[cfg(not(target_arch = "wasm32"))]
pub use http::HyperHttpClient;

pub use libdd_capabilities::{
    HttpClientTrait, HttpError, HttpRequest, HttpResponse, RequestHead, RequestWithBody,
};
