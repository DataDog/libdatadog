// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::{
    AnyValue, ArrayValue, InstrumentationScope, KeyValue as ProtoKeyValue,
};
use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;

use crate::{AttributeArray, AttributeValue, KeyValue};

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ExportMetricsServiceRequest {
    #[prost(message, repeated, tag = "1")]
    pub(crate) resource_metrics: Vec<ResourceMetrics>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ResourceMetrics {
    #[prost(message, optional, tag = "1")]
    pub(crate) resource: Option<Resource>,
    #[prost(message, repeated, tag = "2")]
    pub(crate) scope_metrics: Vec<ScopeMetrics>,
    #[prost(string, tag = "3")]
    pub(crate) schema_url: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct ScopeMetrics {
    #[prost(message, optional, tag = "1")]
    pub(crate) scope: Option<InstrumentationScope>,
    #[prost(message, repeated, tag = "2")]
    pub(crate) metrics: Vec<Metric>,
    #[prost(string, tag = "3")]
    pub(crate) schema_url: String,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Metric {
    #[prost(string, tag = "1")]
    pub(crate) name: String,
    #[prost(string, tag = "2")]
    pub(crate) description: String,
    #[prost(string, tag = "3")]
    pub(crate) unit: String,
    #[prost(oneof = "metric::Data", tags = "5, 7, 9")]
    pub(crate) data: Option<metric::Data>,
}

pub(crate) mod metric {
    #[derive(Clone, PartialEq, prost::Oneof)]
    pub(crate) enum Data {
        #[prost(message, tag = "5")]
        Gauge(super::Gauge),
        #[prost(message, tag = "7")]
        Sum(super::Sum),
        #[prost(message, tag = "9")]
        Histogram(super::Histogram),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Gauge {
    #[prost(message, repeated, tag = "1")]
    pub(crate) data_points: Vec<NumberDataPoint>,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Sum {
    #[prost(message, repeated, tag = "1")]
    pub(crate) data_points: Vec<NumberDataPoint>,
    #[prost(enumeration = "AggregationTemporality", tag = "2")]
    pub(crate) aggregation_temporality: i32,
    #[prost(bool, tag = "3")]
    pub(crate) is_monotonic: bool,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct Histogram {
    #[prost(message, repeated, tag = "1")]
    pub(crate) data_points: Vec<HistogramDataPoint>,
    #[prost(enumeration = "AggregationTemporality", tag = "2")]
    pub(crate) aggregation_temporality: i32,
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct NumberDataPoint {
    #[prost(message, repeated, tag = "7")]
    pub(crate) attributes: Vec<ProtoKeyValue>,
    #[prost(fixed64, tag = "2")]
    pub(crate) start_time_unix_nano: u64,
    #[prost(fixed64, tag = "3")]
    pub(crate) time_unix_nano: u64,
    #[prost(uint32, tag = "8")]
    pub(crate) flags: u32,
    #[prost(oneof = "number_data_point::Value", tags = "4, 6")]
    pub(crate) value: Option<number_data_point::Value>,
}

pub(crate) mod number_data_point {
    #[derive(Clone, Copy, PartialEq, prost::Oneof)]
    pub(crate) enum Value {
        #[prost(double, tag = "4")]
        AsDouble(f64),
        #[prost(sfixed64, tag = "6")]
        AsInt(i64),
    }
}

#[derive(Clone, PartialEq, prost::Message)]
pub(crate) struct HistogramDataPoint {
    #[prost(message, repeated, tag = "9")]
    pub(crate) attributes: Vec<ProtoKeyValue>,
    #[prost(fixed64, tag = "2")]
    pub(crate) start_time_unix_nano: u64,
    #[prost(fixed64, tag = "3")]
    pub(crate) time_unix_nano: u64,
    #[prost(fixed64, tag = "4")]
    pub(crate) count: u64,
    #[prost(double, optional, tag = "5")]
    pub(crate) sum: Option<f64>,
    #[prost(fixed64, repeated, tag = "6")]
    pub(crate) bucket_counts: Vec<u64>,
    #[prost(double, repeated, tag = "7")]
    pub(crate) explicit_bounds: Vec<f64>,
    #[prost(uint32, tag = "10")]
    pub(crate) flags: u32,
    #[prost(double, optional, tag = "11")]
    pub(crate) min: Option<f64>,
    #[prost(double, optional, tag = "12")]
    pub(crate) max: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, prost::Enumeration)]
#[repr(i32)]
pub(crate) enum AggregationTemporality {
    Unspecified = 0,
    Delta = 1,
    Cumulative = 2,
}

pub(crate) fn resource(attributes: &[KeyValue]) -> Resource {
    Resource {
        attributes: key_values(attributes),
        dropped_attributes_count: 0,
        entity_refs: Vec::new(),
    }
}

pub(crate) fn instrumentation_scope(
    name: String,
    version: Option<String>,
    attributes: &[KeyValue],
) -> InstrumentationScope {
    InstrumentationScope {
        name,
        version: version.unwrap_or_default(),
        attributes: key_values(attributes),
        dropped_attributes_count: 0,
    }
}

pub(crate) fn key_values(attributes: &[KeyValue]) -> Vec<ProtoKeyValue> {
    attributes.iter().map(key_value).collect()
}

fn key_value(attribute: &KeyValue) -> ProtoKeyValue {
    ProtoKeyValue {
        key: attribute.key.clone(),
        value: Some(any_value(&attribute.value)),
        key_ref: 0,
    }
}

fn any_value(value: &AttributeValue) -> AnyValue {
    let value = match value {
        AttributeValue::Bool(value) => any_value::Value::BoolValue(*value),
        AttributeValue::I64(value) => any_value::Value::IntValue(*value),
        AttributeValue::F64(value) => any_value::Value::DoubleValue(*value),
        AttributeValue::String(value) => any_value::Value::StringValue(value.clone()),
        AttributeValue::Array(values) => any_value::Value::ArrayValue(ArrayValue {
            values: match values {
                AttributeArray::Bool(values) => values
                    .iter()
                    .map(|value| any_value::Value::BoolValue(*value))
                    .map(|value| AnyValue { value: Some(value) })
                    .collect(),
                AttributeArray::I64(values) => values
                    .iter()
                    .map(|value| any_value::Value::IntValue(*value))
                    .map(|value| AnyValue { value: Some(value) })
                    .collect(),
                AttributeArray::F64(values) => values
                    .iter()
                    .map(|value| any_value::Value::DoubleValue(*value))
                    .map(|value| AnyValue { value: Some(value) })
                    .collect(),
                AttributeArray::String(values) => values
                    .iter()
                    .cloned()
                    .map(any_value::Value::StringValue)
                    .map(|value| AnyValue { value: Some(value) })
                    .collect(),
            },
        }),
    };
    AnyValue { value: Some(value) }
}
