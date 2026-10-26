// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use libdd_trace_protobuf::opentelemetry::proto::common::v1::InstrumentationScope;
use libdd_trace_protobuf::opentelemetry::proto::common::v1::any_value::Value;
use libdd_trace_protobuf::opentelemetry::proto::resource::v1::Resource;
use libdd_trace_protobuf::opentelemetry::proto::trace::v1::{ResourceSpans, ScopeSpans, Span};

use super::OtlpResourceInfo;
use super::mapper::{build_resource, proto_kv};

/// A snapshot of the caller's string resource attributes.
pub type OtlpResourceAttributes = BTreeMap<String, String>;

/// Resource snapshots for one export, keyed by (low 64 bits of trace ID, span ID).
///
/// The low trace ID matches both MessagePack input formats, even when sampling removes the
/// chunk root carrying the high bits. Share an `Arc` for spans with the same snapshot.
/// Missing spans use only the exporter identity; an empty snapshot has the same meaning.
pub type OtlpResourceAttributesBySpan = HashMap<(u64, u64), Arc<OtlpResourceAttributes>>;

pub(super) static EMPTY_RESOURCE_ATTRIBUTES: OtlpResourceAttributes = BTreeMap::new();

fn is_identity_attribute(key: &str) -> bool {
    matches!(
        key,
        "service"
            | "env"
            | "version"
            | "service.name"
            | "deployment.environment.name"
            | "service.version"
            | "telemetry.sdk.name"
            | "telemetry.sdk.language"
            | "telemetry.sdk.version"
            | "runtime-id"
            | "_dd.stats_computed"
            | "_dd.sdk.otlp_export"
            | "datadog.sdk.semantics"
    )
}

pub(super) fn duplicates_resource(
    resource: &OtlpResourceAttributes,
    key: &str,
    value: &str,
) -> bool {
    !resource.is_empty()
        && !is_identity_attribute(key)
        && resource.get(key).is_some_and(|v| v == value)
}

pub(super) fn attributes_for_span(
    resources: &OtlpResourceAttributesBySpan,
    trace_id: u128,
    span_id: u64,
) -> &OtlpResourceAttributes {
    // MessagePack deliberately carries only these lower 64 bits.
    resources
        .get(&(trace_id as u64, span_id))
        .map(Arc::as_ref)
        .unwrap_or(&EMPTY_RESOURCE_ATTRIBUTES)
}

pub(super) struct ResourceGroups<'a> {
    base: Resource,
    info: &'a OtlpResourceInfo,
    indices: HashMap<&'a OtlpResourceAttributes, usize>,
    groups: Vec<ResourceSpans>,
    capacity: usize,
    semantics: bool,
}

impl<'a> ResourceGroups<'a> {
    pub fn new(info: &'a OtlpResourceInfo, semantics: bool, capacity: usize) -> Self {
        Self {
            base: build_resource(info, semantics),
            info,
            indices: HashMap::new(),
            groups: Vec::new(),
            capacity,
            semantics,
        }
    }

    fn new_group(&self, attributes: &OtlpResourceAttributes, capacity: usize) -> ResourceSpans {
        let mut resource = self.base.clone();
        resource.attributes.extend(
            attributes
                .iter()
                .filter(|(key, _)| {
                    !is_identity_attribute(key)
                        && !(self.semantics
                            && matches!(key.as_str(), "error.msg" | "error.message" | "span.kind"))
                })
                .map(|(key, value)| proto_kv(key.clone(), Value::StringValue(value.clone()))),
        );
        ResourceSpans {
            resource: Some(resource),
            scope_spans: vec![ScopeSpans {
                scope: Some(InstrumentationScope {
                    name: self.info.instrumentation_scope_name.clone(),
                    version: self.info.instrumentation_scope_version.clone(),
                    attributes: Vec::new(),
                    dropped_attributes_count: 0,
                }),
                spans: Vec::with_capacity(capacity),
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        }
    }

    pub fn push(&mut self, attributes: &'a OtlpResourceAttributes, span: Span) {
        let index = if let Some(index) = self.indices.get(attributes) {
            *index
        } else {
            let index = self.groups.len();
            let capacity = if index == 0 { self.capacity } else { 0 };
            self.groups.push(self.new_group(attributes, capacity));
            self.indices.insert(attributes, index);
            index
        };
        self.groups[index].scope_spans[0].spans.push(span);
    }

    pub fn finish(mut self) -> Vec<ResourceSpans> {
        if self.groups.is_empty() {
            self.groups
                .push(self.new_group(&EMPTY_RESOURCE_ATTRIBUTES, 0));
        }
        self.groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::otlp_encoder::{
        ProtoExportTraceServiceRequest, map_traces_to_otlp_v1_with_resources,
        map_traces_to_otlp_with_resources,
    };
    use crate::span::{BytesData, v1, v04};
    use libdd_tinybytes::BytesString;

    fn snapshot(values: &[(&str, &str)]) -> Arc<OtlpResourceAttributes> {
        Arc::new(
            values
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        )
    }

    fn span(id: u64, team: &str) -> v04::Span<BytesData> {
        let mut span = v04::Span {
            trace_id: 1,
            span_id: id,
            ..Default::default()
        };
        span.meta
            .insert("team".into(), BytesString::from_string(team.into()));
        span
    }

    fn map(
        spans: Vec<v04::Span<BytesData>>,
        resources: &OtlpResourceAttributesBySpan,
        semantics: bool,
        v1: bool,
    ) -> ProtoExportTraceServiceRequest {
        let info = OtlpResourceInfo {
            service: "tracer-service".into(),
            ..Default::default()
        };
        if v1 {
            let chunk = v1::TraceChunk {
                trace_id: 1_u128.to_be_bytes(),
                spans: spans
                    .into_iter()
                    .map(|span| {
                        let mut output = v1::Span::<BytesData> {
                            span_id: span.span_id,
                            ..Default::default()
                        };
                        for (key, value) in span.meta {
                            output
                                .attributes
                                .insert(key, v1::AttributeValue::String(value));
                        }
                        for (key, value) in span.metrics {
                            output
                                .attributes
                                .insert(key, v1::AttributeValue::Float(value));
                        }
                        output
                    })
                    .collect(),
                ..Default::default()
            };
            map_traces_to_otlp_v1_with_resources(&[chunk], &info, semantics, resources)
        } else {
            map_traces_to_otlp_with_resources(&[spans], &info, semantics, resources)
        }
    }

    fn values(
        attributes: &[libdd_trace_protobuf::opentelemetry::proto::common::v1::KeyValue],
    ) -> BTreeMap<&str, &Value> {
        attributes
            .iter()
            .map(|kv| {
                (
                    kv.key.as_str(),
                    kv.value.as_ref().unwrap().value.as_ref().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn resource_snapshots_group_updates_and_removals() {
        for v1 in [false, true] {
            for semantics in [false, true] {
                let resources = OtlpResourceAttributesBySpan::from([
                    ((1, 1), snapshot(&[("team", "old")])),
                    ((1, 2), snapshot(&[("team", "new")])),
                    ((1, 3), snapshot(&[])),
                    ((1, 4), snapshot(&[("team", "old")])),
                ]);
                let request = map(
                    vec![
                        span(1, "old"),
                        span(2, "new"),
                        span(3, "span-only"),
                        span(4, "override"),
                    ],
                    &resources,
                    semantics,
                    v1,
                );
                assert_eq!(request.resource_spans.len(), 3);
                let groups = request.resource_spans;
                assert_eq!(
                    values(&groups[0].resource.as_ref().unwrap().attributes)["team"],
                    &Value::StringValue("old".into())
                );
                assert_eq!(groups[0].scope_spans[0].spans.len(), 2);
                assert!(
                    !values(&groups[0].scope_spans[0].spans[0].attributes).contains_key("team")
                );
                assert_eq!(
                    values(&groups[0].scope_spans[0].spans[1].attributes)["team"],
                    &Value::StringValue("override".into())
                );
                assert_eq!(
                    values(&groups[1].resource.as_ref().unwrap().attributes)["team"],
                    &Value::StringValue("new".into())
                );
                assert!(
                    !values(&groups[2].resource.as_ref().unwrap().attributes).contains_key("team")
                );
                assert_eq!(
                    values(&groups[2].scope_spans[0].spans[0].attributes)["team"],
                    &Value::StringValue("span-only".into())
                );
            }
        }
    }

    #[test]
    fn string_duplicates_do_not_remove_numeric_overrides_or_change_identity() {
        for v1 in [false, true] {
            for semantics in [false, true] {
                let resources = OtlpResourceAttributesBySpan::from([(
                    (1, 1),
                    snapshot(&[
                        ("workers", "7"),
                        ("team", ""),
                        ("service.name", "spoofed"),
                        ("_dd.sdk.otlp_export", "false"),
                    ]),
                )]);
                let mut input = span(1, "");
                input.metrics.insert("workers".into(), 7.0);
                input.meta.insert("service.name".into(), "spoofed".into());
                let request = map(vec![input], &resources, semantics, v1);
                let group = &request.resource_spans[0];
                let resource = values(&group.resource.as_ref().unwrap().attributes);
                assert_eq!(
                    resource["service.name"],
                    &Value::StringValue("tracer-service".into())
                );
                assert_eq!(
                    resource["_dd.sdk.otlp_export"],
                    &Value::StringValue("true".into())
                );
                assert_eq!(resource["team"], &Value::StringValue("".into()));
                assert_eq!(resource["workers"], &Value::StringValue("7".into()));
                let attributes = values(&group.scope_spans[0].spans[0].attributes);
                assert!(!attributes.contains_key("team"));
                assert_eq!(
                    attributes["service.name"],
                    &Value::StringValue("spoofed".into())
                );
                assert_eq!(
                    attributes["workers"],
                    &if v1 {
                        Value::DoubleValue(7.0)
                    } else {
                        Value::IntValue(7)
                    }
                );
            }
        }
    }

    #[test]
    fn deduplication_precedes_limits_and_dropped_counting() {
        for v1 in [false, true] {
            let attributes: OtlpResourceAttributes = (0..130)
                .map(|i| (format!("inherited{i}"), "same".into()))
                .collect();
            let resources =
                OtlpResourceAttributesBySpan::from([((1, 1), Arc::new(attributes.clone()))]);
            let mut input = v04::Span::<BytesData> {
                trace_id: 1,
                span_id: 1,
                ..Default::default()
            };
            for (key, value) in attributes {
                input.meta.insert(
                    BytesString::from_string(key),
                    BytesString::from_string(value),
                );
            }
            for i in 0..129 {
                input
                    .meta
                    .insert(BytesString::from_string(format!("local{i}")), "kept".into());
            }
            let request = map(vec![input], &resources, true, v1);
            let span = &request.resource_spans[0].scope_spans[0].spans[0];
            assert_eq!(span.attributes.len(), 128);
            assert_eq!(span.dropped_attributes_count, 1);
            assert!(span.attributes.iter().all(|kv| kv.key.starts_with("local")));
        }
    }

    #[test]
    fn sidecar_does_not_mutate_native_spans_or_messagepack() {
        let input = vec![vec![span(1, "core")]];
        let before = crate::msgpack_encoder::v04::to_vec_from_v04(&input);
        let resources =
            OtlpResourceAttributesBySpan::from([((1, 1), snapshot(&[("team", "core")]))]);
        map_traces_to_otlp_with_resources(&input, &OtlpResourceInfo::default(), true, &resources);
        assert_eq!(before, crate::msgpack_encoder::v04::to_vec_from_v04(&input));
    }
}
