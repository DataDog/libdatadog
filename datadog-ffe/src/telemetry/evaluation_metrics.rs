// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Reusable FFE evaluation metric aggregation and OTLP encoding primitives.

use super::FfeTelemetryContext;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    AnyValue, InstrumentationScope, KeyValue,
};
use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;
use prost::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

const METER_NAME: &str = "ddtrace.openfeature";
const METRIC_NAME: &str = "feature_flag.evaluations";
const METRIC_UNIT: &str = "{evaluation}";
const METRIC_DESCRIPTION: &str = "Number of feature flag evaluations";

const ATTR_SERVICE_NAME: &str = "service.name";
const ATTR_ENV: &str = "deployment.environment.name";
const ATTR_VERSION: &str = "service.version";
const ATTR_FLAG_KEY: &str = "feature_flag.key";
const ATTR_VARIANT: &str = "feature_flag.result.variant";
const ATTR_REASON: &str = "feature_flag.result.reason";
const ATTR_ERROR_TYPE: &str = "error.type";
const ATTR_ALLOCATION_KEY: &str = "feature_flag.result.allocation_key";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FfeEvaluationMetric {
    pub flag_key: String,
    pub variant: String,
    pub reason: String,
    pub error_type: Option<String>,
    pub allocation_key: Option<String>,
}

pub fn encode_metrics_payload(
    context: FfeTelemetryContext,
    metrics: Vec<FfeEvaluationMetric>,
) -> Option<Vec<u8>> {
    if metrics.is_empty() {
        return None;
    }

    let now = unix_nano_now();
    let data_points = aggregate(metrics)
        .into_iter()
        .map(|(attributes, count)| otlp::NumberDataPoint {
            attributes: attributes
                .into_iter()
                .map(|(key, value)| string_key_value(key, value))
                .collect(),
            start_time_unix_nano: now,
            time_unix_nano: now,
            value: Some(otlp::number_data_point::Value::AsInt(count)),
            flags: 0,
        })
        .collect::<Vec<_>>();

    if data_points.is_empty() {
        return None;
    }

    let request = otlp::ExportMetricsServiceRequest {
        resource_metrics: vec![otlp::ResourceMetrics {
            resource: Some(resource(context)),
            scope_metrics: vec![otlp::ScopeMetrics {
                scope: Some(InstrumentationScope {
                    name: METER_NAME.to_owned(),
                    version: String::new(),
                    attributes: vec![],
                    dropped_attributes_count: 0,
                }),
                metrics: vec![otlp::Metric {
                    name: METRIC_NAME.to_owned(),
                    description: METRIC_DESCRIPTION.to_owned(),
                    unit: METRIC_UNIT.to_owned(),
                    data: Some(otlp::metric::Data::Sum(otlp::Sum {
                        data_points,
                        aggregation_temporality: otlp::AggregationTemporality::Delta as i32,
                        is_monotonic: true,
                    })),
                }],
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }],
    };

    Some(request.encode_to_vec())
}

fn aggregate(metrics: Vec<FfeEvaluationMetric>) -> BTreeMap<BTreeMap<String, String>, i64> {
    let mut counts = BTreeMap::new();
    for metric in metrics {
        if metric.flag_key.is_empty() {
            continue;
        }
        let attrs = metric_attributes(metric);
        *counts.entry(attrs).or_insert(0) += 1;
    }
    counts
}

fn metric_attributes(metric: FfeEvaluationMetric) -> BTreeMap<String, String> {
    let reason = normalize(&metric.reason, "unknown");
    let mut attrs = BTreeMap::from([
        (ATTR_FLAG_KEY.to_owned(), metric.flag_key),
        (ATTR_VARIANT.to_owned(), metric.variant),
        (ATTR_REASON.to_owned(), reason.clone()),
    ]);

    if let Some(error_type) = metric.error_type {
        if !error_type.is_empty() {
            attrs.insert(
                ATTR_ERROR_TYPE.to_owned(),
                normalize(&error_type, "general"),
            );
        }
    }

    if let Some(allocation_key) = metric.allocation_key {
        if !allocation_key.is_empty()
            && !matches!(reason.as_str(), "error" | "default" | "disabled")
        {
            attrs.insert(ATTR_ALLOCATION_KEY.to_owned(), allocation_key);
        }
    }

    attrs
}

fn normalize(value: &str, default: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        default.to_owned()
    } else {
        value.to_ascii_lowercase()
    }
}

fn resource(context: FfeTelemetryContext) -> Resource {
    let mut attributes = vec![];
    if !context.service.is_empty() {
        attributes.push(string_key_value(
            ATTR_SERVICE_NAME.to_owned(),
            context.service,
        ));
    }
    if !context.env.is_empty() {
        attributes.push(string_key_value(ATTR_ENV.to_owned(), context.env));
    }
    if !context.version.is_empty() {
        attributes.push(string_key_value(ATTR_VERSION.to_owned(), context.version));
    }
    Resource {
        attributes,
        dropped_attributes_count: 0,
        entity_refs: vec![],
    }
}

fn string_key_value(key: String, value: String) -> KeyValue {
    KeyValue {
        key,
        value: Some(AnyValue {
            value: Some(any_value::Value::StringValue(value)),
        }),
        key_ref: 0,
    }
}

fn unix_nano_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(u128::from(u64::MAX)) as u64)
        .unwrap_or_default()
}

mod otlp {
    use libdd_trace_protobuf::opentelemetry::proto::common::v1::{InstrumentationScope, KeyValue};
    use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ExportMetricsServiceRequest {
        #[prost(message, repeated, tag = "1")]
        pub resource_metrics: ::prost::alloc::vec::Vec<ResourceMetrics>,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ResourceMetrics {
        #[prost(message, optional, tag = "1")]
        pub resource: ::core::option::Option<Resource>,
        #[prost(message, repeated, tag = "2")]
        pub scope_metrics: ::prost::alloc::vec::Vec<ScopeMetrics>,
        #[prost(string, tag = "3")]
        pub schema_url: ::prost::alloc::string::String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct ScopeMetrics {
        #[prost(message, optional, tag = "1")]
        pub scope: ::core::option::Option<InstrumentationScope>,
        #[prost(message, repeated, tag = "2")]
        pub metrics: ::prost::alloc::vec::Vec<Metric>,
        #[prost(string, tag = "3")]
        pub schema_url: ::prost::alloc::string::String,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Metric {
        #[prost(string, tag = "1")]
        pub name: ::prost::alloc::string::String,
        #[prost(string, tag = "2")]
        pub description: ::prost::alloc::string::String,
        #[prost(string, tag = "3")]
        pub unit: ::prost::alloc::string::String,
        #[prost(oneof = "metric::Data", tags = "7")]
        pub data: ::core::option::Option<metric::Data>,
    }

    pub mod metric {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub enum Data {
            #[prost(message, tag = "7")]
            Sum(super::Sum),
        }
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct Sum {
        #[prost(message, repeated, tag = "1")]
        pub data_points: ::prost::alloc::vec::Vec<NumberDataPoint>,
        #[prost(enumeration = "AggregationTemporality", tag = "2")]
        pub aggregation_temporality: i32,
        #[prost(bool, tag = "3")]
        pub is_monotonic: bool,
    }

    #[derive(Clone, PartialEq, ::prost::Message)]
    pub struct NumberDataPoint {
        #[prost(fixed64, tag = "2")]
        pub start_time_unix_nano: u64,
        #[prost(fixed64, tag = "3")]
        pub time_unix_nano: u64,
        #[prost(oneof = "number_data_point::Value", tags = "6")]
        pub value: ::core::option::Option<number_data_point::Value>,
        #[prost(message, repeated, tag = "7")]
        pub attributes: ::prost::alloc::vec::Vec<KeyValue>,
        #[prost(uint32, tag = "8")]
        pub flags: u32,
    }

    pub mod number_data_point {
        #[derive(Clone, PartialEq, ::prost::Oneof)]
        pub enum Value {
            #[prost(sfixed64, tag = "6")]
            AsInt(i64),
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
    use prost::Message;

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn metric(flag_key: &str, variant: &str, reason: &str) -> FfeEvaluationMetric {
        FfeEvaluationMetric {
            flag_key: flag_key.to_owned(),
            variant: variant.to_owned(),
            reason: reason.to_owned(),
            error_type: None,
            allocation_key: Some("alloc".to_owned()),
        }
    }

    #[test]
    fn encodes_otlp_counter_and_aggregates_matching_attributes() {
        let payload = encode_metrics_payload(
            context(),
            vec![
                metric("flag", "variant", "TARGETING_MATCH"),
                metric("flag", "variant", "targeting_match"),
            ],
        )
        .unwrap();

        let decoded = otlp::ExportMetricsServiceRequest::decode(payload.as_slice()).unwrap();
        let resource_metrics = &decoded.resource_metrics[0];
        let attrs = &resource_metrics.resource.as_ref().unwrap().attributes;
        assert!(attrs.iter().any(|kv| kv.key == ATTR_SERVICE_NAME));

        let data_points = match resource_metrics.scope_metrics[0].metrics[0]
            .data
            .as_ref()
            .unwrap()
        {
            otlp::metric::Data::Sum(sum) => &sum.data_points,
        };
        assert_eq!(data_points.len(), 1);
        assert_eq!(
            data_points[0].value,
            Some(otlp::number_data_point::Value::AsInt(2))
        );
    }

    #[test]
    fn excludes_allocation_key_for_error_default_and_disabled() {
        for reason in ["ERROR", "DEFAULT", "DISABLED"] {
            let attrs = metric_attributes(FfeEvaluationMetric {
                flag_key: "flag".to_owned(),
                variant: String::new(),
                reason: reason.to_owned(),
                error_type: Some("FLAG_NOT_FOUND".to_owned()),
                allocation_key: Some("alloc".to_owned()),
            });
            assert!(!attrs.contains_key(ATTR_ALLOCATION_KEY));
            assert_eq!(attrs[ATTR_ERROR_TYPE], "flag_not_found");
        }
    }
}
