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
//! Only the messages and fields this encoder writes are declared here, with
//! their field numbers from `opentelemetry/proto/metrics/v1/metrics.proto`.

use prost::Message;

use crate::batch::MetricBatch;
use crate::point::{Attributes, Sum};
use crate::registry::PROFILE_ATTRIBUTE;

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

/// The time window of an export, in nanoseconds since the Unix epoch. Delta
/// points start at `start_time_unix_nano` and end at `time_unix_nano`.
#[derive(Debug, Clone, Copy)]
pub struct OtlpWindow {
    pub start_time_unix_nano: u64,
    pub time_unix_nano: u64,
}

impl MetricBatch {
    /// Encode the batch as an OTLP `ExportMetricsServiceRequest`.
    pub fn encode_otlp(&self, scope: OtlpScope, window: OtlpWindow) -> Vec<u8> {
        self.otlp_request(scope, window).encode_to_vec()
    }

    fn otlp_request(
        &self,
        scope: OtlpScope,
        window: OtlpWindow,
    ) -> pb::ExportMetricsServiceRequest {
        let mut resource_metrics: Vec<pb::ResourceMetrics> = Vec::new();
        for ((resource, profile), group) in &self.groups {
            // Groups are sorted by resource, so one resource's profiles are adjacent.
            let same_resource = resource_metrics.last().is_some_and(|last| {
                last.resource.as_ref().map(|r| &r.attributes) == Some(&key_values(resource))
            });
            if !same_resource {
                resource_metrics.push(pb::ResourceMetrics {
                    resource: Some(pb::Resource {
                        attributes: key_values(resource),
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
            for ((name, attributes), (unit, sum)) in &group.counters {
                let value = match sum {
                    Sum::Integer(total) => match i64::try_from(*total) {
                        Ok(total) => pb::number_data_point::Value::AsInt(total),
                        Err(_) => pb::number_data_point::Value::AsDouble(sum.value()),
                    },
                    Sum::Double(total) => pb::number_data_point::Value::AsDouble(*total),
                };
                sums.push((
                    metric(name, unit),
                    pb::NumberDataPoint {
                        attributes: key_values(attributes),
                        start_time_unix_nano: window.start_time_unix_nano,
                        time_unix_nano: window.time_unix_nano,
                        value: Some(value),
                        flags: 0,
                    },
                ));
            }
            let mut histograms: Vec<(usize, pb::HistogramDataPoint)> = Vec::new();
            for ((name, attributes), (unit, samples)) in &group.histograms {
                histograms.push((
                    metric(name, unit),
                    histogram_point(attributes, samples, bounds(name, unit), window),
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
                        attributes: vec![string_key_value(PROFILE_ATTRIBUTE, profile.id())],
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

fn histogram_point(
    attributes: &Attributes,
    samples: &[f64],
    bounds: &[f64],
    window: OtlpWindow,
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
        attributes: key_values(attributes),
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

fn key_values(attributes: &Attributes) -> Vec<pb::KeyValue> {
    attributes
        .iter()
        .map(|(key, value)| string_key_value(key, value))
        .collect()
}

/// The OTLP messages and fields this encoder writes.
#[allow(missing_docs)]
pub(crate) mod pb {
    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ExportMetricsServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_metrics: Vec<ResourceMetrics>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ResourceMetrics {
        #[prost(message, optional, tag = "1")]
        pub resource: Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_metrics: Vec<ScopeMetrics>,
        #[prost(string, tag = "3")]
        pub schema_url: String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Resource {
        #[prost(message, repeated, tag = "1")]
        pub attributes: Vec<KeyValue>,
        #[prost(uint32, tag = "2")]
        pub dropped_attributes_count: u32,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ScopeMetrics {
        #[prost(message, optional, tag = "1")]
        pub scope: Option<InstrumentationScope>,
        #[prost(message, repeated, tag = "2")]
        pub metrics: Vec<Metric>,
        #[prost(string, tag = "3")]
        pub schema_url: String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct InstrumentationScope {
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
    pub struct Metric {
        #[prost(string, tag = "1")]
        pub name: String,
        #[prost(string, tag = "2")]
        pub description: String,
        #[prost(string, tag = "3")]
        pub unit: String,
        #[prost(oneof = "metric::Data", tags = "7, 9")]
        pub data: Option<metric::Data>,
    }

    pub mod metric {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub enum Data {
            #[prost(message, tag = "7")]
            Sum(super::Sum),
            #[prost(message, tag = "9")]
            Histogram(super::Histogram),
        }
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Sum {
        #[prost(message, repeated, tag = "1")]
        pub data_points: Vec<NumberDataPoint>,
        #[prost(enumeration = "AggregationTemporality", tag = "2")]
        pub aggregation_temporality: i32,
        #[prost(bool, tag = "3")]
        pub is_monotonic: bool,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Histogram {
        #[prost(message, repeated, tag = "1")]
        pub data_points: Vec<HistogramDataPoint>,
        #[prost(enumeration = "AggregationTemporality", tag = "2")]
        pub aggregation_temporality: i32,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct NumberDataPoint {
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

    pub mod number_data_point {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub enum Value {
            #[prost(double, tag = "4")]
            AsDouble(f64),
            #[prost(sfixed64, tag = "6")]
            AsInt(i64),
        }
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct HistogramDataPoint {
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
    pub struct KeyValue {
        #[prost(string, tag = "1")]
        pub key: String,
        #[prost(message, optional, tag = "2")]
        pub value: Option<AnyValue>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct AnyValue {
        #[prost(oneof = "any_value::Value", tags = "1")]
        pub value: Option<any_value::Value>,
    }

    pub mod any_value {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub enum Value {
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
        let request = batch().otlp_request(SCOPE, WINDOW);
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
        );
        assert_eq!(point.bucket_counts[0], 1);
        assert_eq!(point.bucket_counts[1], 1);
        assert_eq!(point.bucket_counts[DURATION_BOUNDS.len()], 1);
    }

    #[test]
    fn an_empty_batch_encodes_no_resource() {
        let request = MetricBatch::new().otlp_request(SCOPE, WINDOW);
        assert!(request.resource_metrics.is_empty());
    }
}
