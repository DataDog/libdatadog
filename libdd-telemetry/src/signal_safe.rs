// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Allocation-free submission of telemetry metric series.
//!
//! This module serializes a `generate-metrics` request into a caller-provided
//! buffer and sends it through the constrained transport in
//! [`libdd_http_client_lite`]. It performs no allocation, reads no
//! process-global configuration, and starts no runtime. Async-signal-safety
//! also depends on the transport, resolver, executor, and buffers supplied by
//! the caller.

use core::fmt::{self, Write as _};

use libdd_http_client_lite::{
    client::HttpClient,
    headers::ContentType,
    io::{embedded_nal_async::Dns, embedded_nal_async::TcpConnect},
    request::{Method, RequestBuilder},
};

use crate::{
    data::metrics::{MetricNamespace, MetricType},
    protocol,
};

/// Application identity included in a telemetry request.
#[derive(Debug, Clone, Copy)]
pub struct Application<'a> {
    /// Service that emitted the metric.
    pub service_name: &'a str,
    /// Implementation language, such as `rust`.
    pub language_name: &'a str,
    /// Version of the implementation language.
    pub language_version: &'a str,
    /// Version of the library emitting telemetry.
    pub library_version: &'a str,
}

/// A single telemetry metric point and its series metadata.
#[derive(Debug, Clone, Copy)]
pub struct Metric<'a> {
    /// Product namespace for the metric.
    pub namespace: MetricNamespace,
    /// Metric name without its namespace.
    pub name: &'a str,
    /// Unix timestamp in seconds.
    pub timestamp: u64,
    /// Metric value.
    pub value: f64,
    /// Preformatted `key:value` tags.
    pub tags: &'a [&'a str],
    /// Whether this is a common metric defined by the telemetry specification.
    pub common: bool,
    /// Metric aggregation type.
    pub kind: MetricType,
    /// Aggregation interval in seconds.
    pub interval: u64,
}

/// Borrowed data required to build a `generate-metrics` telemetry request.
#[derive(Debug, Clone, Copy)]
pub struct MetricsRequest<'a> {
    /// Request creation time as Unix seconds.
    pub tracer_time: u64,
    /// Stable identifier for this runtime.
    pub runtime_id: &'a str,
    /// Monotonically increasing request sequence number.
    pub seq_id: u64,
    /// Application identity.
    pub application: Application<'a>,
    /// Hostname reported to telemetry.
    pub hostname: &'a str,
    /// Metric series to submit.
    pub metrics: &'a [Metric<'a>],
}

/// Failure while encoding or sending a constrained telemetry request.
#[derive(Debug)]
pub enum Error {
    /// The caller-provided body buffer is too small.
    BufferTooSmall,
    /// A metric value is NaN or infinite and cannot be represented in JSON.
    NonFiniteValue,
    /// The HTTP transport failed.
    Http(libdd_http_client_lite::Error),
    /// The telemetry endpoint returned a non-success status.
    UnexpectedStatus(u16),
}

impl From<libdd_http_client_lite::Error> for Error {
    fn from(error: libdd_http_client_lite::Error) -> Self {
        Self::Http(error)
    }
}

/// Serializes a telemetry metric request into `buffer` without allocating.
///
/// Returns the number of initialized bytes in `buffer`.
pub fn encode_metrics(request: &MetricsRequest<'_>, buffer: &mut [u8]) -> Result<usize, Error> {
    let mut writer = BufferWriter::new(buffer);

    writer.write_str("{\"api_version\":\"")?;
    writer.write_str(protocol::API_VERSION)?;
    writer.write_str("\",\"tracer_time\":")?;
    write!(writer, "{}", request.tracer_time)?;
    writer.write_str(",\"runtime_id\":")?;
    write_json_string(&mut writer, request.runtime_id)?;
    writer.write_str(",\"seq_id\":")?;
    write!(writer, "{}", request.seq_id)?;

    writer.write_str(",\"application\":{\"service_name\":")?;
    write_json_string(&mut writer, request.application.service_name)?;
    writer.write_str(",\"language_name\":")?;
    write_json_string(&mut writer, request.application.language_name)?;
    writer.write_str(",\"language_version\":")?;
    write_json_string(&mut writer, request.application.language_version)?;
    writer.write_str(",\"tracer_version\":")?;
    write_json_string(&mut writer, request.application.library_version)?;
    writer.write_str("}")?;

    writer.write_str(",\"host\":{\"hostname\":")?;
    write_json_string(&mut writer, request.hostname)?;
    writer.write_str("},\"request_type\":\"")?;
    writer.write_str(protocol::GENERATE_METRICS_REQUEST_TYPE)?;
    writer.write_str("\",\"payload\":{\"series\":[")?;

    for (index, metric) in request.metrics.iter().enumerate() {
        if !metric.value.is_finite() {
            return Err(Error::NonFiniteValue);
        }
        if index != 0 {
            writer.write_char(',')?;
        }

        writer.write_str("{\"namespace\":\"")?;
        writer.write_str(metric.namespace.as_str())?;
        writer.write_str("\",\"metric\":")?;
        write_json_string(&mut writer, metric.name)?;
        writer.write_str(",\"points\":[[")?;
        write!(writer, "{},{}", metric.timestamp, metric.value)?;
        writer.write_str("]],\"tags\":[")?;

        for (tag_index, tag) in metric.tags.iter().enumerate() {
            if tag_index != 0 {
                writer.write_char(',')?;
            }
            write_json_string(&mut writer, tag)?;
        }

        writer.write_str("],\"common\":")?;
        writer.write_str(if metric.common { "true" } else { "false" })?;
        writer.write_str(",\"type\":\"")?;
        writer.write_str(metric.kind.as_str())?;
        writer.write_str("\",\"interval\":")?;
        write!(writer, "{}", metric.interval)?;
        writer.write_char('}')?;
    }

    writer.write_str("]}}")?;
    Ok(writer.len())
}

/// Encodes and submits telemetry metrics using caller-provided buffers.
///
/// The response buffer only needs to hold the HTTP response headers. A status
/// in the `200..=299` range is considered successful.
pub async fn send_metrics<T, D>(
    client: &mut HttpClient<'_, T, D>,
    url: &str,
    request: &MetricsRequest<'_>,
    body_buffer: &mut [u8],
    response_buffer: &mut [u8],
) -> Result<u16, Error>
where
    T: TcpConnect,
    D: Dns,
{
    let body_len = encode_metrics(request, body_buffer)?;
    let headers = [
        (
            protocol::REQUEST_TYPE_HEADER,
            protocol::GENERATE_METRICS_REQUEST_TYPE,
        ),
        (protocol::API_VERSION_HEADER, protocol::API_VERSION),
        (
            protocol::LIBRARY_LANGUAGE_HEADER,
            request.application.language_name,
        ),
        (
            protocol::LIBRARY_VERSION_HEADER,
            request.application.library_version,
        ),
    ];
    let mut http_request = client
        .request(Method::POST, url)
        .await?
        .headers(&headers)
        .content_type(ContentType::ApplicationJson)
        .body(&body_buffer[..body_len]);
    let status = http_request.send(response_buffer).await?.status.0;

    if (200..=299).contains(&status) {
        Ok(status)
    } else {
        Err(Error::UnexpectedStatus(status))
    }
}

struct BufferWriter<'a> {
    buffer: &'a mut [u8],
    len: usize,
}

impl<'a> BufferWriter<'a> {
    const fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, len: 0 }
    }

    const fn len(&self) -> usize {
        self.len
    }
}

impl fmt::Write for BufferWriter<'_> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        let destination = self.buffer.get_mut(self.len..end).ok_or(fmt::Error)?;
        destination.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

fn write_json_string(writer: &mut BufferWriter<'_>, value: &str) -> fmt::Result {
    writer.write_char('"')?;
    for character in value.chars() {
        match character {
            '"' => writer.write_str("\\\"")?,
            '\\' => writer.write_str("\\\\")?,
            '\u{08}' => writer.write_str("\\b")?,
            '\u{0c}' => writer.write_str("\\f")?,
            '\n' => writer.write_str("\\n")?,
            '\r' => writer.write_str("\\r")?,
            '\t' => writer.write_str("\\t")?,
            control if control <= '\u{1f}' => write!(writer, "\\u{:04x}", u32::from(control))?,
            other => writer.write_char(other)?,
        }
    }
    writer.write_char('"')
}

impl From<fmt::Error> for Error {
    fn from(_: fmt::Error) -> Self {
        Self::BufferTooSmall
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAGS: &[&str] = &["component:signal-safe", "escaped:\"yes\""];
    const METRICS: &[Metric<'_>] = &[Metric {
        namespace: MetricNamespace::Telemetry,
        name: "submission.count",
        timestamp: 1_234,
        value: 1.5,
        tags: TAGS,
        common: false,
        kind: MetricType::Count,
        interval: 10,
    }];

    fn request<'a>(metrics: &'a [Metric<'a>]) -> MetricsRequest<'a> {
        MetricsRequest {
            tracer_time: 1_235,
            runtime_id: "runtime-id",
            seq_id: 7,
            application: Application {
                service_name: "service\nname",
                language_name: "rust",
                language_version: "1.87",
                library_version: "35.0.0",
            },
            hostname: "host",
            metrics,
        }
    }

    #[test]
    fn encodes_generate_metrics_payload() {
        let mut buffer = [0_u8; 1_024];
        let len = encode_metrics(&request(METRICS), &mut buffer).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&buffer[..len]).unwrap();

        assert_eq!(value["api_version"], "v2");
        assert_eq!(value["request_type"], "generate-metrics");
        assert_eq!(value["application"]["service_name"], "service\nname");
        assert_eq!(value["payload"]["series"][0]["namespace"], "telemetry");
        assert_eq!(value["payload"]["series"][0]["points"][0][1], 1.5);
        assert_eq!(value["payload"]["series"][0]["tags"][1], "escaped:\"yes\"");
    }

    #[cfg(feature = "std")]
    #[test]
    fn matches_standard_telemetry_payload() {
        use crate::data::{
            metrics::Serie, ApiVersion, Application as StandardApplication, GenerateMetrics, Host,
            Payload, Telemetry,
        };
        use libdd_common::tag::Tag;

        let application = StandardApplication {
            service_name: "service\nname".to_string(),
            service_version: None,
            env: None,
            language_name: "rust".to_string(),
            language_version: "1.87".to_string(),
            tracer_version: "35.0.0".to_string(),
            runtime_name: None,
            runtime_version: None,
            runtime_patches: None,
            process_tags: None,
        };
        let host = Host {
            hostname: "host".to_string(),
            container_id: None,
            os: None,
            os_version: None,
            kernel_name: None,
            kernel_release: None,
            kernel_version: None,
        };
        let payload = Payload::GenerateMetrics(GenerateMetrics {
            series: vec![Serie {
                namespace: MetricNamespace::Telemetry,
                metric: "submission.count".to_string(),
                points: vec![(1_234, 1.5)],
                tags: vec![
                    Tag::new("component", "signal-safe").unwrap(),
                    Tag::new("escaped", "\"yes\"").unwrap(),
                ],
                common: false,
                _type: MetricType::Count,
                interval: 10,
            }],
        });
        let standard = Telemetry {
            api_version: ApiVersion::V2,
            tracer_time: 1_235,
            runtime_id: "runtime-id",
            seq_id: 7,
            application: &application,
            host: &host,
            origin: None,
            payload: &payload,
        };
        let mut buffer = [0_u8; 1_024];
        let len = encode_metrics(&request(METRICS), &mut buffer).unwrap();

        let constrained: serde_json::Value = serde_json::from_slice(&buffer[..len]).unwrap();
        let standard = serde_json::to_value(standard).unwrap();
        assert_eq!(constrained, standard);
    }

    #[test]
    fn reports_a_small_buffer() {
        let mut buffer = [0_u8; 16];
        assert!(matches!(
            encode_metrics(&request(METRICS), &mut buffer),
            Err(Error::BufferTooSmall)
        ));
    }

    #[test]
    fn rejects_non_finite_values() {
        let metrics = [Metric {
            value: f64::NAN,
            ..METRICS[0]
        }];
        let mut buffer = [0_u8; 1_024];
        assert!(matches!(
            encode_metrics(&request(&metrics), &mut buffer),
            Err(Error::NonFiniteValue)
        ));
    }
}
