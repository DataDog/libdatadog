// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Client construction and request/response helpers for the shared HTTP
//! foundation. Native (non-wasm) only — wasm builds get only the portable
//! [`super::error`] types.

#![cfg(not(target_arch = "wasm32"))]

use http_body_util::BodyExt;
use hyper::body::Incoming;

use crate::connector::Connector;

use super::body::Body;
use super::error::Error;

/// HTTP response carrying our shared [`Body`].
pub type HttpResponse = http::Response<Body>;
/// HTTP request carrying our shared [`Body`].
pub type HttpRequest = http::Request<Body>;
/// Error type returned by the underlying hyper-util client.
pub type HttpRequestError = hyper_util::client::legacy::Error;

/// Future returned by `GenericHttpClient::request`.
pub type ResponseFuture = hyper_util::client::legacy::ResponseFuture;

/// Generic hyper-util client over an arbitrary connector and our shared
/// [`Body`].
pub type GenericHttpClient<C> = hyper_util::client::legacy::Client<C, Body>;

/// Returns a hyper-util client builder pre-wired with the Tokio executor.
///
/// Backends that need to tweak builder options (e.g. `pool_max_idle_per_host`)
/// start from this builder.
pub fn client_builder() -> hyper_util::client::legacy::Builder {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::default())
}

/// Create a new default-configuration hyper client for fixed-interval sending.
///
/// This client does not keep connections idle — otherwise we would get a pipe
/// closed every second connection because of the agent's low keep-alive.
///
/// This is generally not a problem if the client is used once every ten or
/// more seconds.
pub fn new_client_periodic() -> GenericHttpClient<Connector> {
    client_builder()
        .pool_max_idle_per_host(0)
        .build(Connector::default())
}

/// Create a new default-configuration hyper client.
///
/// Connections are kept open and reused.
pub fn new_default_client() -> GenericHttpClient<Connector> {
    client_builder().build(Connector::default())
}

/// Convert an inbound hyper response into an [`HttpResponse`] using our
/// shared [`Body`] type.
pub fn into_response(response: hyper::Response<Incoming>) -> HttpResponse {
    response.map(Body::Incoming)
}

/// Collect all bytes from a response body into a single buffer.
pub async fn collect_response_bytes(response: HttpResponse) -> Result<bytes::Bytes, Error> {
    Ok(response.into_body().collect().await?.to_bytes())
}

/// Build a mock response with an in-memory body, useful in tests.
pub fn mock_response(
    builder: http::response::Builder,
    body: hyper::body::Bytes,
) -> anyhow::Result<HttpResponse> {
    Ok(builder.body(Body::from_bytes(body))?)
}

/// Build an empty-bodied response from a [`http::response::Builder`].
pub fn empty_response(builder: http::response::Builder) -> Result<HttpResponse, Error> {
    Ok(builder.body(Body::empty())?)
}
