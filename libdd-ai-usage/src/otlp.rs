// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! OTLP encoding of a [`MetricBatch`]: an `ExportMetricsServiceRequest` in
//! protobuf, ready for an OTLP/HTTP `POST .../v1/metrics` with
//! `Content-Type: application/x-protobuf`.
//!
//! - One `ResourceMetrics` per resource, and one `ScopeMetrics` per profile. The scope carries the
//!   caller's name and version and the attribute `trajectory.profile` with the profile identifier.
//! - Counters are monotonic delta sums. A whole-number sum is an integer data point.
//! - Histograms are delta explicit-bucket histograms with count, sum, min and max. Bucket bounds
//!   are the OpenTelemetry GenAI advisory bounds where the conventions give them; other histograms
//!   have a single bucket.
//!
//! - With [`OtlpBinding::Datadog`], for an export that Datadog stores as it arrives (the Datadog
//!   Agent's OTLP receiver or Datadog's OTLP intake), the encoding follows the Datadog binding
//!   instead: a `{nanoUSD}` counter is summed exactly and sent in USD as a double, divided by 10^9
//!   once, and `{id}@{version}` attribute values use `/`. Metrics then store the same values
//!   whether they arrive over OTLP or DogStatsD.
//!
//! Only the messages and fields this encoder writes are declared here, with
//! their field numbers from `opentelemetry/proto/metrics/v1/metrics.proto`.

use std::borrow::Cow;

use prost::Message;

use crate::Profile;
use crate::batch::{MetricBatch, Samples, SeriesKey};
use crate::datadog::{NANO_USD, NANO_USD_PER_USD, is_versioned};
use crate::point::{Attributes, Sum};
use crate::registry::PROFILE_ATTRIBUTE;

/// The unit of a `{nanoUSD}` counter under [`OtlpBinding::Datadog`].
const USD: &str = "USD";

/// `gen_ai.client.operation.duration` and the other operation durations.
const DURATION_BOUNDS: &[f64] = &[
    0.01, 0.02, 0.04, 0.08, 0.16, 0.32, 0.64, 1.28, 2.56, 5.12, 10.24, 20.48, 40.96, 81.92,
];
/// `gen_ai.client.token.usage`.
const TOKEN_BOUNDS: &[f64] = &[
    1.0, 4.0, 16.0, 64.0, 256.0, 1024.0, 4096.0, 16384.0, 65536.0, 262144.0, 1048576.0, 4194304.0,
    16777216.0, 67108864.0,
];
/// `gen_ai.server.time_to_first_token`.
const FIRST_CHUNK_BOUNDS: &[f64] = &[
    0.001, 0.005, 0.01, 0.02, 0.04, 0.06, 0.08, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
];
/// `gen_ai.server.time_per_output_token`.
const CHUNK_INTERVAL_BOUNDS: &[f64] = &[
    0.01, 0.025, 0.05, 0.075, 0.1, 0.15, 0.2, 0.3, 0.4, 0.5, 0.75, 1.0, 2.5,
];

fn bounds(name: &str, unit: &str) -> &'static [f64] {
    match (name, unit) {
        ("gen_ai.client.inference.time_to_first_chunk", _) => FIRST_CHUNK_BOUNDS,
        ("gen_ai.client.inference.time_per_output_chunk", _) => CHUNK_INTERVAL_BOUNDS,
        (_, "s") => DURATION_BOUNDS,
        (_, "{token}") => TOKEN_BOUNDS,
        _ => &[],
    }
}

/// The instrumentation scope of the exported metrics: the exporter's own name
/// and version, for example the tracer integration that recorded them.
#[derive(Debug, Clone, Copy)]
pub struct OtlpScope<'a> {
    pub name: &'a str,
    pub version: &'a str,
}

/// Which binding an OTLP export follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OtlpBinding {
    /// The OpenTelemetry encoding: cost in integer `{nanoUSD}`, versioned
    /// values as `{id}@{version}`. For collectors and other backends.
    #[default]
    OpenTelemetry,
    /// The Datadog binding, for an export Datadog stores as it arrives: cost
    /// in USD, versioned values as `{id}/{version}`, as over DogStatsD.
    Datadog,
}

/// The time window of an export, in nanoseconds since the Unix epoch. Delta
/// points start at `start_time_unix_nano` and end at `time_unix_nano`.
#[derive(Debug, Clone, Copy)]
pub struct OtlpWindow {
    pub start_time_unix_nano: u64,
    pub time_unix_nano: u64,
}

impl MetricBatch {
    /// Encode the batch as an OTLP `ExportMetricsServiceRequest`, following
    /// the OpenTelemetry encoding.
    pub fn encode_otlp(&self, scope: OtlpScope, window: OtlpWindow) -> Vec<u8> {
        self.encode_otlp_with(scope, window, OtlpBinding::OpenTelemetry)
    }

    /// Encode the batch as an OTLP `ExportMetricsServiceRequest`, following
    /// `binding`.
    pub fn encode_otlp_with(
        &self,
        scope: OtlpScope,
        window: OtlpWindow,
        binding: OtlpBinding,
    ) -> Vec<u8> {
        self.otlp_request(scope, window, binding).encode_to_vec()
    }

    fn otlp_request(
        &self,
        scope: OtlpScope,
        window: OtlpWindow,
        binding: OtlpBinding,
    ) -> pb::ExportMetricsServiceRequest {
        request_of(
            self.groups
                .iter()
                .map(|(key, group)| (key, group.counters.iter(), group.histograms.iter())),
            scope,
            window,
            binding,
        )
    }
}

/// The request of `groups` (each with its Counter and Histogram series), in
/// order: a whole batch's, or the series of one part of it, borrowed.
fn request_of<'a, C, H>(
    groups: impl Iterator<Item = (&'a (Attributes, Profile), C, H)>,
    scope: OtlpScope,
    window: OtlpWindow,
    binding: OtlpBinding,
) -> pb::ExportMetricsServiceRequest
where
    C: Iterator<Item = (&'a SeriesKey, &'a (String, Sum))>,
    H: Iterator<Item = (&'a SeriesKey, &'a (String, Samples))>,
{
    {
        let datadog = binding == OtlpBinding::Datadog;
        let mut resource_metrics: Vec<pb::ResourceMetrics> = Vec::new();
        for ((resource, profile), counters, histograms_of) in groups {
            // Groups are sorted by resource, so one resource's profiles are adjacent.
            let same_resource = resource_metrics.last().is_some_and(|last| {
                last.resource.as_ref().map(|r| &r.attributes)
                    == Some(&key_values(resource, binding))
            });
            if !same_resource {
                resource_metrics.push(pb::ResourceMetrics {
                    resource: Some(pb::Resource {
                        attributes: key_values(resource, binding),
                        dropped_attributes_count: 0,
                    }),
                    scope_metrics: Vec::new(),
                    schema_url: String::new(),
                });
            }
            let mut metrics: Vec<pb::Metric> = Vec::new();
            let mut metric = |name: &str, unit: &str| -> usize {
                match metrics.iter().position(|m| m.name == name) {
                    Some(at) => at,
                    None => {
                        metrics.push(pb::Metric {
                            name: name.to_string(),
                            description: String::new(),
                            unit: unit.to_string(),
                            data: None,
                        });
                        metrics.len() - 1
                    }
                }
            };
            let mut sums: Vec<(usize, pb::NumberDataPoint)> = Vec::new();
            for ((name, attributes), (unit, sum)) in counters {
                let in_usd = datadog && unit == NANO_USD;
                let value = match sum {
                    // One division of the exact sum, converted to a double once.
                    _ if in_usd => {
                        pb::number_data_point::Value::AsDouble(sum.value() / NANO_USD_PER_USD)
                    }
                    Sum::Integer(total) => match i64::try_from(*total) {
                        Ok(total) => pb::number_data_point::Value::AsInt(total),
                        Err(_) => pb::number_data_point::Value::AsDouble(sum.value()),
                    },
                    Sum::Double(total) => pb::number_data_point::Value::AsDouble(*total),
                };
                sums.push((
                    metric(name, if in_usd { USD } else { unit }),
                    pb::NumberDataPoint {
                        attributes: key_values(attributes, binding),
                        start_time_unix_nano: window.start_time_unix_nano,
                        time_unix_nano: window.time_unix_nano,
                        value: Some(value),
                        flags: 0,
                    },
                ));
            }
            let mut histograms: Vec<(usize, pb::HistogramDataPoint)> = Vec::new();
            for ((name, attributes), (unit, samples)) in histograms_of {
                histograms.push((
                    metric(name, unit),
                    histogram_point(
                        attributes,
                        &samples.values,
                        bounds(name, unit),
                        window,
                        binding,
                    ),
                ));
            }
            for (at, point) in sums {
                if let Some(entry) = metrics.get_mut(at) {
                    match &mut entry.data {
                        Some(pb::metric::Data::Sum(sum)) => sum.data_points.push(point),
                        _ => {
                            entry.data = Some(pb::metric::Data::Sum(pb::Sum {
                                data_points: vec![point],
                                aggregation_temporality: pb::AggregationTemporality::Delta as i32,
                                is_monotonic: true,
                            }))
                        }
                    }
                }
            }
            for (at, point) in histograms {
                if let Some(entry) = metrics.get_mut(at) {
                    match &mut entry.data {
                        Some(pb::metric::Data::Histogram(histogram)) => {
                            histogram.data_points.push(point)
                        }
                        _ => {
                            entry.data = Some(pb::metric::Data::Histogram(pb::Histogram {
                                data_points: vec![point],
                                aggregation_temporality: pb::AggregationTemporality::Delta as i32,
                            }))
                        }
                    }
                }
            }
            if metrics.is_empty() {
                continue;
            }
            if let Some(last) = resource_metrics.last_mut() {
                last.scope_metrics.push(pb::ScopeMetrics {
                    scope: Some(pb::InstrumentationScope {
                        name: scope.name.to_string(),
                        version: scope.version.to_string(),
                        attributes: vec![string_key_value(
                            PROFILE_ATTRIBUTE,
                            &attribute_value(PROFILE_ATTRIBUTE, profile.id(), binding),
                        )],
                        dropped_attributes_count: 0,
                    }),
                    metrics,
                    schema_url: String::new(),
                });
            }
        }
        resource_metrics.retain(|r| !r.scope_metrics.is_empty());
        pb::ExportMetricsServiceRequest { resource_metrics }
    }
}

/// One series of a batch, as it is encoded: a Counter's sum, or every sample
/// of a Histogram.
#[derive(Clone, Copy)]
enum Series<'a> {
    Counter(&'a (Attributes, Profile), &'a SeriesKey, &'a (String, Sum)),
    Histogram(
        &'a (Attributes, Profile),
        &'a SeriesKey,
        &'a (String, Samples),
    ),
}

impl Series<'_> {
    /// A rough size of the series in a request: its attributes and one data
    /// point (a Histogram's buckets are encoded once per series).
    fn estimate(&self) -> usize {
        let (key, point) = match self {
            Series::Counter(_, key, _) => (key, 32),
            Series::Histogram(_, key, _) => (key, 160),
        };
        let attributes: usize = key.1.iter().map(|(k, v)| k.len() + v.len() + 8).sum();
        key.0.len() + attributes + point
    }
}

/// The size aimed at when packing a request, with room for the estimate's
/// error.
fn target(max_bytes: usize) -> usize {
    (max_bytes - max_bytes / 8).max(1)
}

impl MetricBatch {
    fn series(&self) -> Vec<Series<'_>> {
        let mut series = Vec::new();
        for (group_key, group) in &self.groups {
            series.extend(
                group
                    .counters
                    .iter()
                    .map(|(key, value)| Series::Counter(group_key, key, value)),
            );
            series.extend(
                group
                    .histograms
                    .iter()
                    .map(|(key, value)| Series::Histogram(group_key, key, value)),
            );
        }
        series
    }

    /// The batch as OTLP `ExportMetricsServiceRequest`s of at most
    /// `max_bytes` each (0 for no bound), following `binding`. A batch that
    /// fits is one request, the same bytes [`MetricBatch::encode_otlp_with`]
    /// writes; an empty batch is none. The points are split by series and
    /// the points of one series always go in the same request, which then
    /// exceeds `max_bytes` only when that series alone does.
    pub fn encode_otlp_parts(
        &self,
        scope: OtlpScope,
        window: OtlpWindow,
        binding: OtlpBinding,
        max_bytes: usize,
    ) -> Vec<Vec<u8>> {
        if self.is_empty() {
            return Vec::new();
        }
        // Sizes come from the request before it is written, so each part's
        // bytes are written once.
        let request = |series: &[Series<'_>]| request_of_series(series, scope, window, binding);
        let series = self.series();
        let estimates: Vec<usize> = series.iter().map(Series::estimate).collect();
        let estimated: usize = estimates.iter().sum();
        // A batch far over the bound is not built whole just to be measured:
        // the estimates are close to the encoded size, and a part they get
        // wrong is cut again below. Otherwise the whole request is built,
        // sent as it is when it fits, and its size calibrates the estimates.
        let scale = if max_bytes == 0 || estimated <= 2 * max_bytes {
            let whole = self.otlp_request(scope, window, binding);
            let whole_len = whole.encoded_len();
            if max_bytes == 0 || whole_len <= max_bytes {
                return vec![whole.encode_to_vec()];
            }
            whole_len as f64 / estimated.max(1) as f64
        } else {
            1.0
        };
        let aim = target(max_bytes) as f64;
        let mut parts = Vec::new();
        let mut start = 0;
        let mut size = 0.0;
        for (at, estimate) in estimates.iter().enumerate() {
            let estimate = *estimate as f64 * scale;
            if at > start && size + estimate > aim {
                encode_series(&series[start..at], &request, max_bytes, &mut parts);
                start = at;
                size = 0.0;
            }
            size += estimate;
        }
        encode_series(&series[start..], &request, max_bytes, &mut parts);
        parts
    }
}

/// The request of `series`, a run of a batch's series in its order.
fn request_of_series(
    series: &[Series<'_>],
    scope: OtlpScope,
    window: OtlpWindow,
    binding: OtlpBinding,
) -> pb::ExportMetricsServiceRequest {
    type Counters<'a> = Vec<(&'a SeriesKey, &'a (String, Sum))>;
    type Histograms<'a> = Vec<(&'a SeriesKey, &'a (String, Samples))>;
    let mut groups: Vec<(&(Attributes, Profile), Counters<'_>, Histograms<'_>)> = Vec::new();
    for one in series {
        let group = match one {
            Series::Counter(group, ..) | Series::Histogram(group, ..) => *group,
        };
        if !groups
            .last()
            .is_some_and(|last| std::ptr::eq(last.0, group))
        {
            groups.push((group, Vec::new(), Vec::new()));
        }
        if let Some(last) = groups.last_mut() {
            match *one {
                Series::Counter(_, key, value) => last.1.push((key, value)),
                Series::Histogram(_, key, value) => last.2.push((key, value)),
            }
        }
    }
    request_of(
        groups.into_iter().map(|(group, counters, histograms)| {
            (group, counters.into_iter(), histograms.into_iter())
        }),
        scope,
        window,
        binding,
    )
}

/// Encode `series` into `parts`, each at most `max_bytes` unless it holds a
/// single series: a request that does not fit is cut into as many even
/// pieces as its size needs.
fn encode_series(
    series: &[Series<'_>],
    request: &dyn Fn(&[Series<'_>]) -> pb::ExportMetricsServiceRequest,
    max_bytes: usize,
    parts: &mut Vec<Vec<u8>>,
) {
    if series.is_empty() {
        return;
    }
    let message = request(series);
    let len = message.encoded_len();
    if len <= max_bytes || series.len() == 1 {
        parts.push(message.encode_to_vec());
        return;
    }
    drop(message);
    let pieces = (len / target(max_bytes) + 1).clamp(2, series.len());
    for piece in series.chunks(series.len().div_ceil(pieces)) {
        encode_series(piece, request, max_bytes, parts);
    }
}

/// What an OTLP `ExportMetricsServiceResponse` says the endpoint rejected:
/// its `partial_success` rejected data points and error message (cut to
/// 256 bytes at a character boundary), when it states either. Read from the
/// protobuf body of an OTLP/HTTP or gRPC reply, or its JSON form
/// (`{"partialSuccess": {"rejectedDataPoints": "3", "errorMessage": ...}}`).
/// `None` for a full success or a body that is neither.
pub fn parse_export_partial_success(body: &[u8]) -> Option<(u64, String)> {
    let (rejected, message) = match body.iter().find(|b| !b.is_ascii_whitespace()) {
        Some(b'{') => partial_success_json(body)?,
        _ => {
            let response = pb::ExportMetricsServiceResponse::decode(body).ok()?;
            let partial = response.partial_success?;
            (
                u64::try_from(partial.rejected_data_points).unwrap_or(0),
                partial.error_message,
            )
        }
    };
    let mut cut = message.trim().to_string();
    if cut.len() > 256 {
        let mut end = 256;
        while !cut.is_char_boundary(end) {
            end -= 1;
        }
        cut.truncate(end);
    }
    (rejected > 0 || !cut.is_empty()).then_some((rejected, cut))
}

fn partial_success_json(body: &[u8]) -> Option<(u64, String)> {
    let text = std::str::from_utf8(body).ok()?;
    let document = crate::Json::parse(text).ok()?;
    let partial = document
        .get("partialSuccess")
        .or_else(|| document.get("partial_success"))?;
    let rejected = partial
        .get("rejectedDataPoints")
        .or_else(|| partial.get("rejected_data_points"))
        .and_then(|value| match value {
            // proto3 JSON writes int64 as a string.
            crate::Json::String(text) => text.parse::<u64>().ok(),
            crate::Json::Number(n) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
            _ => None,
        })
        .unwrap_or(0);
    let message = partial
        .get("errorMessage")
        .or_else(|| partial.get("error_message"))
        .and_then(crate::Json::as_str)
        .unwrap_or("")
        .to_string();
    Some((rejected, message))
}

fn histogram_point(
    attributes: &Attributes,
    samples: &[f64],
    bounds: &[f64],
    window: OtlpWindow,
    binding: OtlpBinding,
) -> pb::HistogramDataPoint {
    let mut bucket_counts = vec![0_u64; bounds.len() + 1];
    let mut sum = 0.0;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for sample in samples {
        // Bucket i holds (bounds[i-1], bounds[i]]: the upper bound is inclusive.
        let at = bounds.partition_point(|bound| bound < sample);
        if let Some(count) = bucket_counts.get_mut(at) {
            *count += 1;
        }
        sum += sample;
        min = min.min(*sample);
        max = max.max(*sample);
    }
    pb::HistogramDataPoint {
        attributes: key_values(attributes, binding),
        start_time_unix_nano: window.start_time_unix_nano,
        time_unix_nano: window.time_unix_nano,
        count: u64::try_from(samples.len()).unwrap_or(u64::MAX),
        sum: Some(sum),
        bucket_counts,
        explicit_bounds: bounds.to_vec(),
        flags: 0,
        min: (!samples.is_empty()).then_some(min),
        max: (!samples.is_empty()).then_some(max),
    }
}

fn string_key_value(key: &str, value: &str) -> pb::KeyValue {
    pb::KeyValue {
        key: key.to_string(),
        value: Some(pb::AnyValue {
            value: Some(pb::any_value::Value::StringValue(value.to_string())),
        }),
    }
}

/// An attribute's value under `binding`: the Datadog binding writes
/// `{id}@{version}` values with `/`, which Datadog keeps, instead of `@`.
fn attribute_value<'a>(attribute: &str, value: &'a str, binding: OtlpBinding) -> Cow<'a, str> {
    if binding == OtlpBinding::Datadog && is_versioned(attribute) {
        Cow::Owned(value.replace('@', "/"))
    } else {
        Cow::Borrowed(value)
    }
}

fn key_values(attributes: &Attributes, binding: OtlpBinding) -> Vec<pb::KeyValue> {
    attributes
        .iter()
        .map(|(key, value)| string_key_value(key, &attribute_value(key, value, binding)))
        .collect()
}

/// The OTLP messages and fields this encoder writes.
#[allow(missing_docs)]
pub(crate) mod pb {
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct ExportMetricsServiceResponse {
        #[prost(message, optional, tag = "1")]
        pub partial_success: Option<ExportMetricsPartialSuccess>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct ExportMetricsPartialSuccess {
        #[prost(int64, tag = "1")]
        pub rejected_data_points: i64,
        #[prost(string, tag = "2")]
        pub error_message: String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct ExportMetricsServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_metrics: Vec<ResourceMetrics>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct ResourceMetrics {
        #[prost(message, optional, tag = "1")]
        pub resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_metrics: Vec<ScopeMetrics>,
        #[prost(string, tag = "3")]
        pub schema_url: String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct Resource {
        #[prost(message, repeated, tag = "1")]
        pub attributes: Vec<KeyValue>,
        #[prost(uint32, tag = "2")]
        pub dropped_attributes_count: u32,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct ScopeMetrics {
        #[prost(message, optional, tag = "1")]
        pub scope: Option<InstrumentationScope>,
        #[prost(message, repeated, tag = "2")]
        pub metrics: Vec<Metric>,
        #[prost(string, tag = "3")]
        pub schema_url: String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct InstrumentationScope {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(string, tag = "2")]
        pub version: String,
        #[prost(message, repeated, tag = "3")]
        pub attributes: Vec<KeyValue>,
        #[prost(uint32, tag = "4")]
        pub dropped_attributes_count: u32,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct Metric {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(string, tag = "2")]
        pub description: String,
        #[prost(string, tag = "3")]
        pub unit: String,
        #[prost(oneof = "metric::Data", tags = "7, 9")]
        pub data: Option<metric::Data>,
    }

    pub(crate) mod metric {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub(crate) enum Data {
            #[prost(message, tag = "7")]
            Sum(super::Sum),
            #[prost(message, tag = "9")]
            Histogram(super::Histogram),
        }
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct Sum {
        #[prost(message, repeated, tag = "1")]
        pub data_points: Vec<NumberDataPoint>,
        #[prost(enumeration = "AggregationTemporality", tag = "2")]
        pub aggregation_temporality: i32,
        #[prost(bool, tag = "3")]
        pub is_monotonic: bool,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct Histogram {
        #[prost(message, repeated, tag = "1")]
        pub data_points: Vec<HistogramDataPoint>,
        #[prost(enumeration = "AggregationTemporality", tag = "2")]
        pub aggregation_temporality: i32,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct NumberDataPoint {
        #[prost(fixed64, tag = "2")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "3")]
        pub time_unix_nano: u64,
        #[prost(oneof = "number_data_point::Value", tags = "4, 6")]
        pub value: Option<number_data_point::Value>,
        #[prost(message, repeated, tag = "7")]
        pub attributes: Vec<KeyValue>,
        #[prost(uint32, tag = "8")]
        pub flags: u32,
    }

    pub(crate) mod number_data_point {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub(crate) enum Value {
            #[prost(double, tag = "4")]
            AsDouble(f64),
            #[prost(sfixed64, tag = "6")]
            AsInt(i64),
        }
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct HistogramDataPoint {
        #[prost(fixed64, tag = "2")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "3")]
        pub time_unix_nano: u64,
        #[prost(fixed64, tag = "4")]
        pub count: u64,
        #[prost(double, optional, tag = "5")]
        pub sum: Option<f64>,
        #[prost(fixed64, repeated, tag = "6")]
        pub bucket_counts: Vec<u64>,
        #[prost(double, repeated, tag = "7")]
        pub explicit_bounds: Vec<f64>,
        #[prost(message, repeated, tag = "9")]
        pub attributes: Vec<KeyValue>,
        #[prost(uint32, tag = "10")]
        pub flags: u32,
        #[prost(double, optional, tag = "11")]
        pub min: Option<f64>,
        #[prost(double, optional, tag = "12")]
        pub max: Option<f64>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct KeyValue {
        #[prost(string, tag = "1")]
        pub key: String,
        #[prost(message, optional, tag = "2")]
        pub value: Option<AnyValue>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub(crate) struct AnyValue {
        #[prost(oneof = "any_value::Value", tags = "1")]
        pub value: Option<any_value::Value>,
    }

    pub(crate) mod any_value {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub(crate) enum Value {
            #[prost(string, tag = "1")]
            StringValue(String),
        }
    }

    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::prost::Enumeration)]
    #[repr(i32)]
    pub enum AggregationTemporality {
        Unspecified = 0,
        Delta = 1,
        Cumulative = 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Json, Profile, project_result};

    const WINDOW: OtlpWindow = OtlpWindow {
        start_time_unix_nano: 1_000,
        time_unix_nano: 2_000,
    };
    const SCOPE: OtlpScope = OtlpScope {
        name: "test",
        version: "1.0",
    };

    fn batch() -> MetricBatch {
        let call = r#"{"operation_name": "chat", "provider_name": "anthropic",
            "duration_seconds": 1.5, "streaming": false, "input_tokens": 41200,
            "cache_read_input_tokens": 39000, "cache_write_input_tokens": 0,
            "input_basis": "includes_cache", "output_tokens": 512,
            "cost_usd": 0.02013, "cost_source": "estimated", "observation_point": "gateway"}"#;
        let observation = Json::parse(call).unwrap();
        let mut batch = MetricBatch::new();
        for profile in [Profile::ProviderAttempt, Profile::TokenBreakdown] {
            let projection = project_result(profile, &observation).unwrap();
            batch.add(profile, &projection).unwrap();
            batch.add(profile, &projection).unwrap();
        }
        batch
    }

    #[test]
    fn one_scope_per_profile_with_the_profile_attribute() {
        let request =
            pb::ExportMetricsServiceRequest::decode(batch().encode_otlp(SCOPE, WINDOW).as_slice())
                .unwrap();
        assert_eq!(request.resource_metrics.len(), 1);
        let scopes = &request.resource_metrics[0].scope_metrics;
        let profiles: Vec<String> = scopes
            .iter()
            .map(|s| {
                let scope = s.scope.as_ref().unwrap();
                assert_eq!(
                    (scope.name.as_str(), scope.version.as_str()),
                    ("test", "1.0")
                );
                assert_eq!(scope.attributes.len(), 1);
                assert_eq!(scope.attributes[0].key, PROFILE_ATTRIBUTE);
                match &scope.attributes[0].value.as_ref().unwrap().value {
                    Some(pb::any_value::Value::StringValue(v)) => v.clone(),
                    None => String::new(),
                }
            })
            .collect();
        assert_eq!(
            profiles,
            [Profile::ProviderAttempt.id(), Profile::TokenBreakdown.id()]
        );
    }

    #[test]
    fn counters_are_integer_delta_sums_and_histograms_have_buckets() {
        let request = batch().otlp_request(SCOPE, WINDOW, OtlpBinding::OpenTelemetry);
        let metrics = &request.resource_metrics[0].scope_metrics[0].metrics;
        let find = |name: &str| metrics.iter().find(|m| m.name == name).unwrap();

        let cost = find("trajectory.gen_ai.client.inference.usage.cost");
        assert_eq!(cost.unit, "{nanoUSD}");
        let Some(pb::metric::Data::Sum(sum)) = &cost.data else {
            panic!("cost is not a sum")
        };
        assert!(sum.is_monotonic);
        assert_eq!(
            sum.aggregation_temporality,
            pb::AggregationTemporality::Delta as i32
        );
        assert_eq!(sum.data_points.len(), 1);
        let point = &sum.data_points[0];
        assert_eq!(
            point.value,
            Some(pb::number_data_point::Value::AsInt(40_260_000))
        );
        assert_eq!(
            (point.start_time_unix_nano, point.time_unix_nano),
            (1_000, 2_000)
        );

        let duration = find("gen_ai.client.inference.duration");
        let Some(pb::metric::Data::Histogram(histogram)) = &duration.data else {
            panic!("duration is not a histogram")
        };
        let point = &histogram.data_points[0];
        assert_eq!(point.count, 2);
        assert_eq!(point.sum, Some(3.0));
        assert_eq!((point.min, point.max), (Some(1.5), Some(1.5)));
        assert_eq!(point.explicit_bounds, DURATION_BOUNDS);
        // 1.5 s falls in (1.28, 2.56].
        assert_eq!(point.bucket_counts[8], 2);
        assert_eq!(point.bucket_counts.iter().sum::<u64>(), 2);
    }

    #[test]
    fn bucket_upper_bounds_are_inclusive() {
        let point = histogram_point(
            &Attributes::new(),
            &[0.01, 0.0100001, 100.0],
            DURATION_BOUNDS,
            WINDOW,
            OtlpBinding::OpenTelemetry,
        );
        assert_eq!(point.bucket_counts[0], 1);
        assert_eq!(point.bucket_counts[1], 1);
        assert_eq!(point.bucket_counts[DURATION_BOUNDS.len()], 1);
    }

    #[test]
    fn an_empty_batch_encodes_no_resource() {
        let request = MetricBatch::new().otlp_request(SCOPE, WINDOW, OtlpBinding::Datadog);
        assert!(request.resource_metrics.is_empty());
    }

    #[test]
    fn the_datadog_binding_sends_cost_in_usd_and_versions_with_a_slash() {
        let batch = batch();
        let request = batch.otlp_request(SCOPE, WINDOW, OtlpBinding::Datadog);
        let scope_metrics = &request.resource_metrics[0].scope_metrics[0];
        let profile = &scope_metrics.scope.as_ref().unwrap().attributes[0];
        assert_eq!(
            profile.value.as_ref().unwrap().value,
            Some(pb::any_value::Value::StringValue(
                Profile::ProviderAttempt.id().replace('@', "/")
            ))
        );
        let metrics = &scope_metrics.metrics;
        let cost = metrics
            .iter()
            .find(|m| m.name == "trajectory.gen_ai.client.inference.usage.cost")
            .unwrap();
        assert_eq!(cost.unit, "USD");
        let Some(pb::metric::Data::Sum(sum)) = &cost.data else {
            panic!("cost is not a sum")
        };
        // Two calls of 0.02013 USD: the exact nano-USD sum, divided once.
        assert_eq!(
            sum.data_points[0].value,
            Some(pb::number_data_point::Value::AsDouble(0.04026))
        );
        // The same cost as the Datadog series DogStatsD sends.
        let series = batch.datadog_series(false).unwrap();
        let dogstatsd = series
            .iter()
            .find(|s| s.name == "trajectory.gen_ai.client.inference.usage.cost")
            .unwrap();
        assert_eq!(dogstatsd.value, 0.04026);
        // Token counts are unchanged.
        let tokens = metrics
            .iter()
            .find(|m| m.name == "gen_ai.client.inference.usage.input_tokens")
            .unwrap();
        assert_eq!(tokens.unit, "{token}");
        let Some(pb::metric::Data::Sum(sum)) = &tokens.data else {
            panic!("tokens are not a sum")
        };
        assert!(matches!(
            sum.data_points[0].value,
            Some(pb::number_data_point::Value::AsInt(_))
        ));
    }
}
