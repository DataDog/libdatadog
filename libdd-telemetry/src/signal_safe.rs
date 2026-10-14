// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Allocation-free submission of telemetry metric series.
//!
//! Requests borrow application and host identities allocated during process
//! initialization together with metric strings and slices. Metric collections
//! can use arrays, allocation-backed vectors, or fixed-capacity containers such
//! as `heapless::Vec`. JSON is serialized directly into a caller-provided
//! buffer. Async-signal-safety also depends on the transport, resolver,
//! executor, and buffers supplied by the caller.

use serde::{Serialize, Serializer};

pub use libdd_common::tag::{TagRef, TagValidationError};

use crate::{
    data::{
        Application, Host,
        metrics::{MetricNamespace, MetricType},
    },
    protocol,
};

#[cfg(feature = "signal-safe")]
use {
    embedded_io_async::{Read, Write},
    reqwless::{
        client::HttpResource,
        headers::ContentType,
        request::{Method, RequestBuilder},
    },
};

/// A borrowed telemetry metric series.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct MetricSeriesRef<'a> {
    /// Product namespace for the metric.
    pub namespace: MetricNamespace,
    /// Metric name without its namespace.
    pub metric: &'a str,
    /// Unix timestamp and metric value pairs.
    pub points: &'a [(u64, f64)],
    /// Validated metric tags.
    pub tags: &'a [TagRef<'a>],
    /// Whether this is a common metric defined by the telemetry specification.
    pub common: bool,
    /// Metric aggregation type.
    #[serde(rename = "type")]
    pub metric_type: MetricType,
    /// Aggregation interval in seconds.
    pub interval: u64,
}

/// Borrowed data required to build a `generate-metrics` telemetry request.
#[derive(Clone, Copy, Debug)]
pub struct MetricsRequest<'a> {
    /// Request creation time as Unix seconds.
    pub tracer_time: u64,
    /// Stable identifier for this runtime.
    pub runtime_id: &'a str,
    /// Monotonically increasing request sequence number.
    pub seq_id: u64,
    /// Application identity.
    pub application: &'a Application,
    /// Host identity.
    pub host: &'a Host,
    /// Optional telemetry origin.
    pub origin: Option<&'a str>,
    /// Metric series to submit.
    pub series: &'a [MetricSeriesRef<'a>],
}

#[derive(Serialize)]
struct GenerateMetricsRef<'a> {
    series: &'a [MetricSeriesRef<'a>],
}

#[derive(Serialize)]
struct WireRequest<'a> {
    api_version: &'static str,
    tracer_time: u64,
    runtime_id: &'a str,
    seq_id: u64,
    application: &'a Application,
    host: &'a Host,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin: Option<&'a str>,
    request_type: &'static str,
    payload: GenerateMetricsRef<'a>,
}

impl Serialize for MetricsRequest<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        WireRequest {
            api_version: protocol::API_VERSION,
            tracer_time: self.tracer_time,
            runtime_id: self.runtime_id,
            seq_id: self.seq_id,
            application: self.application,
            host: self.host,
            origin: self.origin,
            request_type: protocol::GENERATE_METRICS_REQUEST_TYPE,
            payload: GenerateMetricsRef {
                series: self.series,
            },
        }
        .serialize(serializer)
    }
}

/// Failure while encoding or sending a constrained telemetry request.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive] // variants depend on enabled features
pub enum Error {
    /// The caller-provided body buffer is too small.
    #[error("telemetry body buffer is too small")]
    BufferTooSmall,
    /// A metric value is NaN or infinite and cannot be represented in telemetry.
    #[error("metric value is NaN or infinite")]
    NonFiniteValue,
    /// The JSON serializer reported an error other than buffer exhaustion.
    #[error("failed to serialize telemetry request")]
    Serialization,
    /// The HTTP transport failed.
    #[cfg(feature = "signal-safe")]
    #[error("HTTP transport failed: {0:?}")]
    Http(reqwless::Error),
    /// The telemetry endpoint returned a non-success status.
    #[cfg(feature = "signal-safe")]
    #[error("telemetry endpoint returned status {0}")]
    UnexpectedStatus(u16),
    /// More than [`MAX_EXTRA_HEADERS`] extra headers were supplied.
    #[cfg(feature = "signal-safe")]
    #[error("more than {MAX_EXTRA_HEADERS} extra headers")]
    TooManyHeaders,
}

impl From<serde_json_core::ser::Error> for Error {
    fn from(error: serde_json_core::ser::Error) -> Self {
        match error {
            serde_json_core::ser::Error::BufferFull => Self::BufferTooSmall,
            _ => Self::Serialization,
        }
    }
}

#[cfg(feature = "signal-safe")]
impl From<reqwless::Error> for Error {
    fn from(error: reqwless::Error) -> Self {
        Self::Http(error)
    }
}

/// Serializes a telemetry metric request into `buffer` without allocating.
///
/// Returns the number of initialized bytes in `buffer`.
pub fn encode_metrics(request: &MetricsRequest<'_>, buffer: &mut [u8]) -> Result<usize, Error> {
    if request
        .series
        .iter()
        .flat_map(|series| series.points.iter())
        .any(|(_, value)| !value.is_finite())
    {
        return Err(Error::NonFiniteValue);
    }

    serde_json_core::to_slice(request, buffer).map_err(Into::into)
}

/// Maximum number of caller-provided headers accepted by [`send_metrics`].
#[cfg(feature = "signal-safe")]
pub const MAX_EXTRA_HEADERS: usize = 8;

#[cfg(feature = "signal-safe")]
const FIXED_HEADERS: usize = 5;

/// Encodes and submits telemetry metrics using caller-provided buffers.
///
/// `extra_headers` are sent after the telemetry protocol headers, for example
/// `DD-API-KEY` when submitting directly to the intake. At most
/// [`MAX_EXTRA_HEADERS`] are accepted.
///
/// The response buffer only needs to hold the HTTP response headers. A status
/// in the `200..=299` range is considered successful.
///
/// The request carries `Connection: close` and the response body is not read,
/// so `resource` must not be used for further requests.
#[cfg(feature = "signal-safe")]
pub async fn send_metrics<C>(
    resource: &mut HttpResource<'_, C>,
    path: &str,
    request: &MetricsRequest<'_>,
    extra_headers: &[(&str, &str)],
    body_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> Result<u16, Error>
where
    C: Read + Write,
{
    let body_len = encode_metrics(request, body_buffer)?;
    let application = request.application;
    // reqwless accepts a single header slice, so merge the protocol headers
    // and `extra_headers` on the stack.
    let mut headers = [("", ""); FIXED_HEADERS + MAX_EXTRA_HEADERS];
    let (fixed, extra) = headers.split_at_mut(FIXED_HEADERS);
    fixed.copy_from_slice(&[
        (
            protocol::REQUEST_TYPE_HEADER,
            protocol::GENERATE_METRICS_REQUEST_TYPE,
        ),
        (protocol::API_VERSION_HEADER, protocol::API_VERSION),
        (
            protocol::LIBRARY_LANGUAGE_HEADER,
            &application.language_name,
        ),
        (
            protocol::LIBRARY_VERSION_HEADER,
            &application.tracer_version,
        ),
        (protocol::CONNECTION_HEADER, protocol::CONNECTION_CLOSE),
    ]);
    extra
        .get_mut(..extra_headers.len())
        .ok_or(Error::TooManyHeaders)?
        .copy_from_slice(extra_headers);

    let response = resource
        .request(Method::POST, path)
        .headers(&headers[..FIXED_HEADERS + extra_headers.len()])
        .content_type(ContentType::ApplicationJson)
        .body(&body_buffer[..body_len])
        .send(response_buffer)
        .await?;
    match response.status.0 {
        status @ 200..=299 => Ok(status),
        status => Err(Error::UnexpectedStatus(status)),
    }
}

#[cfg(all(test, feature = "alloc"))]
mod tests;
