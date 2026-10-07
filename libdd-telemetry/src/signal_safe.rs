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
use embedded_io_async::{Read, Write};
#[cfg(feature = "signal-safe")]
use reqwless::{
    client::HttpResource,
    headers::ContentType,
    request::{Method, RequestBuilder},
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
#[derive(Debug)]
#[non_exhaustive] // variants depend on enabled features
pub enum Error {
    /// The caller-provided body buffer is too small.
    BufferTooSmall,
    /// A metric value is NaN or infinite and cannot be represented in telemetry.
    NonFiniteValue,
    /// The JSON serializer reported an error other than buffer exhaustion.
    Serialization,
    /// The HTTP transport failed.
    #[cfg(feature = "signal-safe")]
    Http(reqwless::Error),
    /// The telemetry endpoint returned a non-success status.
    #[cfg(feature = "signal-safe")]
    UnexpectedStatus(u16),
    /// More than [`MAX_EXTRA_HEADERS`] extra headers were supplied.
    #[cfg(feature = "signal-safe")]
    TooManyHeaders,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BufferTooSmall => f.write_str("telemetry body buffer is too small"),
            Self::NonFiniteValue => f.write_str("metric value is NaN or infinite"),
            Self::Serialization => f.write_str("failed to serialize telemetry request"),
            // `reqwless::Error` only implements `Debug`.
            #[cfg(feature = "signal-safe")]
            Self::Http(error) => write!(f, "HTTP transport failed: {error:?}"),
            #[cfg(feature = "signal-safe")]
            Self::UnexpectedStatus(status) => {
                write!(f, "telemetry endpoint returned status {status}")
            }
            #[cfg(feature = "signal-safe")]
            Self::TooManyHeaders => write!(f, "more than {MAX_EXTRA_HEADERS} extra headers"),
        }
    }
}

impl core::error::Error for Error {}

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
mod tests {
    #[cfg(feature = "signal-safe")]
    use core::convert::Infallible;

    #[cfg(feature = "signal-safe")]
    use alloc::{
        string::{String, ToString},
        vec::Vec,
    };

    use super::*;
    #[cfg(feature = "std")]
    use crate::data::{
        ApiVersion, GenerateMetrics, Payload, Telemetry,
        metrics::{Serie, Tag},
    };
    use crate::data::{Application, Host};

    #[cfg(feature = "std")]
    #[test]
    fn encoding_matches_the_owned_serde_model() {
        let tags = [
            TagRef::new("component", "signal-safe").unwrap(),
            TagRef::new("escaped", "\"yes\"\n").unwrap(),
        ];
        let points = [(1_234, 1.5), (1_235, 2.0)];
        let series = [MetricSeriesRef {
            namespace: MetricNamespace::Telemetry,
            metric: "submission.count",
            points: &points,
            tags: &tags,
            common: false,
            metric_type: MetricType::Count,
            interval: 10,
        }];
        let application = Application {
            service_name: "service\nname".to_string(),
            service_version: Some("1.2.3".to_string()),
            env: Some("staging".to_string()),
            language_name: "rust".to_string(),
            language_version: "1.87".to_string(),
            tracer_version: "35.0.0".to_string(),
            runtime_name: Some("rustc".to_string()),
            runtime_version: Some("1.87".to_string()),
            runtime_patches: Some("0".to_string()),
            process_tags: Some("region:us-east-1".to_string()),
        };
        let host = Host {
            hostname: "host".to_string(),
            container_id: Some("container".to_string()),
            os: Some("linux".to_string()),
            os_version: Some("1".to_string()),
            architecture: Some("x86_64".to_string()),
            kernel_name: Some("Linux".to_string()),
            kernel_release: Some("6".to_string()),
            kernel_version: Some("6.1".to_string()),
        };
        let request = MetricsRequest {
            tracer_time: 1_236,
            runtime_id: "runtime-id",
            seq_id: 7,
            application: &application,
            host: &host,
            origin: Some("library"),
            series: &series,
        };
        let payload = Payload::GenerateMetrics(GenerateMetrics {
            series: vec![Serie {
                namespace: MetricNamespace::Telemetry,
                metric: "submission.count".to_string(),
                points: points.to_vec(),
                tags: vec![
                    Tag::new("component", "signal-safe").unwrap(),
                    Tag::new("escaped", "\"yes\"\n").unwrap(),
                ],
                common: false,
                _type: MetricType::Count,
                interval: 10,
            }],
        });
        let expected = Telemetry {
            api_version: ApiVersion::V2,
            tracer_time: 1_236,
            runtime_id: "runtime-id",
            seq_id: 7,
            application: &application,
            host: &host,
            origin: Some("library"),
            payload: &payload,
        };

        let mut buffer = [0_u8; 2_048];
        let len = encode_metrics(&request, &mut buffer).unwrap();
        let actual: serde_json::Value = serde_json::from_slice(&buffer[..len]).unwrap();

        assert_eq!(actual, serde_json::to_value(expected).unwrap());
    }

    #[test]
    fn reports_a_small_buffer() {
        let application = Application::default();
        let host = Host::default();
        let request = MetricsRequest {
            tracer_time: 1,
            runtime_id: "runtime",
            seq_id: 1,
            application: &application,
            host: &host,
            origin: None,
            series: &[],
        };
        let mut buffer = [0_u8; 16];

        assert!(matches!(
            encode_metrics(&request, &mut buffer),
            Err(Error::BufferTooSmall)
        ));
    }

    #[test]
    fn rejects_non_finite_values() {
        let application = Application::default();
        let host = Host::default();
        let points = [(1, f64::NAN)];
        let series = [MetricSeriesRef {
            namespace: MetricNamespace::Telemetry,
            metric: "submission.count",
            points: &points,
            tags: &[],
            common: false,
            metric_type: MetricType::Count,
            interval: 10,
        }];
        let request = MetricsRequest {
            tracer_time: 1,
            runtime_id: "runtime",
            seq_id: 1,
            application: &application,
            host: &host,
            origin: None,
            series: &series,
        };
        let mut buffer = [0_u8; 1_024];

        assert!(matches!(
            encode_metrics(&request, &mut buffer),
            Err(Error::NonFiniteValue)
        ));
    }

    #[cfg(feature = "signal-safe")]
    struct FakeConnection {
        response: &'static [u8],
        response_offset: usize,
        request: Vec<u8>,
    }

    #[cfg(feature = "signal-safe")]
    impl embedded_io::ErrorType for FakeConnection {
        type Error = Infallible;
    }

    #[cfg(feature = "signal-safe")]
    impl embedded_io_async::Read for FakeConnection {
        async fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
            let remaining = &self.response[self.response_offset..];
            let length = remaining.len().min(buffer.len());
            buffer[..length].copy_from_slice(&remaining[..length]);
            self.response_offset += length;
            Ok(length)
        }
    }

    #[cfg(feature = "signal-safe")]
    impl embedded_io_async::Write for FakeConnection {
        async fn write(&mut self, buffer: &[u8]) -> Result<usize, Self::Error> {
            self.request.extend_from_slice(buffer);
            Ok(buffer.len())
        }
    }

    #[cfg(feature = "signal-safe")]
    fn empty_request<'a>(application: &'a Application, host: &'a Host) -> MetricsRequest<'a> {
        MetricsRequest {
            tracer_time: 1,
            runtime_id: "runtime",
            seq_id: 1,
            application,
            host,
            origin: None,
            series: &[],
        }
    }

    #[cfg(feature = "signal-safe")]
    #[tokio::test]
    async fn sends_encoded_metrics_and_accepts_success_statuses() {
        use reqwless::client::{HttpConnection, HttpResource};

        let application = Application {
            language_name: "rust".to_string(),
            tracer_version: "1.0.0".to_string(),
            ..Application::default()
        };
        let host = Host::default();
        let request = empty_request(&application, &host);
        let mut connection = FakeConnection {
            response: b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n",
            response_offset: 0,
            request: Vec::new(),
        };
        let mut body_buffer = [0_u8; 1_024];
        let mut response_buffer = [0_u8; 256];
        let status = {
            let mut resource = HttpResource {
                conn: HttpConnection::Plain(&mut connection),
                host: "localhost",
                base_path: "",
            };
            send_metrics(
                &mut resource,
                "/telemetry/proxy/api/v2/apmtelemetry",
                &request,
                &[("DD-API-KEY", "secret")],
                &mut body_buffer,
                &mut response_buffer,
            )
            .await
            .unwrap()
        };

        assert_eq!(status, 202);
        let written = String::from_utf8(connection.request).unwrap();
        assert!(written.starts_with("POST /telemetry/proxy/api/v2/apmtelemetry HTTP/1.1\r\n"));
        assert!(written.contains("dd-telemetry-request-type: generate-metrics\r\n"));
        assert!(written.contains("dd-client-library-language: rust\r\n"));
        assert!(written.contains("Connection: close\r\n"));
        assert!(written.contains("DD-API-KEY: secret\r\n"));
        assert!(written.ends_with("\"series\":[]}}"));
    }

    #[cfg(feature = "signal-safe")]
    #[tokio::test]
    async fn rejects_unsuccessful_statuses() {
        use reqwless::client::{HttpConnection, HttpResource};

        let application = Application {
            language_name: "rust".to_string(),
            tracer_version: "1.0.0".to_string(),
            ..Application::default()
        };
        let host = Host::default();
        let request = empty_request(&application, &host);
        let mut connection = FakeConnection {
            response: b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
            response_offset: 0,
            request: Vec::new(),
        };
        let mut resource = HttpResource {
            conn: HttpConnection::Plain(&mut connection),
            host: "localhost",
            base_path: "",
        };
        let mut body_buffer = [0_u8; 1_024];
        let mut response_buffer = [0_u8; 256];

        assert!(matches!(
            send_metrics(
                &mut resource,
                "/telemetry/proxy/api/v2/apmtelemetry",
                &request,
                &[],
                &mut body_buffer,
                &mut response_buffer,
            )
            .await,
            Err(Error::UnexpectedStatus(500))
        ));
    }

    #[cfg(feature = "signal-safe")]
    #[tokio::test]
    async fn rejects_too_many_extra_headers() {
        use reqwless::client::{HttpConnection, HttpResource};

        let application = Application::default();
        let host = Host::default();
        let request = empty_request(&application, &host);
        let mut connection = FakeConnection {
            response: b"",
            response_offset: 0,
            request: Vec::new(),
        };
        let mut resource = HttpResource {
            conn: HttpConnection::Plain(&mut connection),
            host: "localhost",
            base_path: "",
        };
        let extra_headers = [("x-header", "value"); MAX_EXTRA_HEADERS + 1];
        let mut body_buffer = [0_u8; 1_024];
        let mut response_buffer = [0_u8; 256];

        assert!(matches!(
            send_metrics(
                &mut resource,
                "/telemetry/proxy/api/v2/apmtelemetry",
                &request,
                &extra_headers,
                &mut body_buffer,
                &mut response_buffer,
            )
            .await,
            Err(Error::TooManyHeaders)
        ));
        assert!(connection.request.is_empty());
    }
}
