// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::borrow::Borrow;
use std::collections::HashMap;

use anyhow::{ensure, Context};
use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_normalization::normalizer::normalize_trace;
use libdd_trace_obfuscation::obfuscate::obfuscate_span;
use libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::span::v04::{
    AttributeAnyValue, AttributeAnyValueBytes, AttributeArrayValue, AttributeArrayValueBytes, Span,
    SpanBytes, SpanEvent, SpanEventBytes, SpanLink, SpanLinkBytes, VecMap,
};
use libdd_trace_utils::span::TraceData;
use libdd_trace_utils::trace_utils::{compute_top_level_span, update_tracer_top_level};

pub(crate) fn normalize_and_obfuscate<T: TraceData>(
    traces: Vec<Vec<Span<T>>>,
    config: &ObfuscationConfig,
    client_computed_top_level: bool,
) -> anyhow::Result<Vec<Vec<SpanBytes>>> {
    let mut output = Vec::with_capacity(traces.len());

    for trace in traces {
        output.push(process_trace(trace, config, client_computed_top_level)?);
    }

    Ok(output)
}

fn process_trace<T: TraceData>(
    trace: Vec<Span<T>>,
    config: &ObfuscationConfig,
    client_computed_top_level: bool,
) -> anyhow::Result<Vec<SpanBytes>> {
    let trace_id = trace
        .first()
        .context("cannot normalize an empty agentless trace")?
        .trace_id;
    ensure!(
        trace.iter().all(|span| span.trace_id == trace_id),
        "agentless trace contains spans with different trace IDs"
    );

    let mut spans: Vec<_> = trace.into_iter().map(into_protobuf_span::<T>).collect();

    normalize_trace(&mut spans).context("failed to normalize agentless trace")?;
    if client_computed_top_level {
        for span in &mut spans {
            update_tracer_top_level(span);
        }
    } else {
        compute_top_level_span(&mut spans);
    }
    for span in &mut spans {
        obfuscate_span(span, config);
    }

    spans.into_iter().map(from_protobuf_span).collect()
}

fn into_protobuf_span<T: TraceData>(span: Span<T>) -> pb::Span {
    let Span {
        service,
        name,
        resource,
        r#type,
        trace_id,
        span_id,
        parent_id,
        start,
        duration,
        error,
        meta,
        metrics,
        meta_struct,
        span_links,
        span_events,
    } = span;

    let mut meta = own_text_map::<T>(meta);
    let trace_id_high = (trace_id >> 64) as u64;
    if trace_id_high != 0 && !meta.contains_key("_dd.p.tid") {
        meta.insert("_dd.p.tid".to_owned(), format!("{trace_id_high:016x}"));
    }

    pb::Span {
        service: own_string(service),
        name: own_string(name),
        resource: own_string(resource),
        trace_id: trace_id as u64,
        span_id,
        parent_id,
        start,
        duration,
        error,
        meta,
        metrics: own_map::<T>(metrics),
        r#type: own_string(r#type),
        meta_struct: own_byte_map::<T>(meta_struct),
        span_links: span_links
            .into_iter()
            .map(into_protobuf_link::<T>)
            .collect(),
        span_events: span_events
            .into_iter()
            .map(into_protobuf_event::<T>)
            .collect(),
    }
}

fn from_protobuf_span(span: pb::Span) -> anyhow::Result<SpanBytes> {
    Ok(SpanBytes {
        service: span.service.into(),
        name: span.name.into(),
        resource: span.resource.into(),
        r#type: span.r#type.into(),
        trace_id: u128::from(span.trace_id),
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta: span
            .meta
            .into_iter()
            .map(|(key, value)| (key.into(), value.into()))
            .collect(),
        metrics: span
            .metrics
            .into_iter()
            .map(|(key, value)| (key.into(), value))
            .collect(),
        meta_struct: span
            .meta_struct
            .into_iter()
            .map(|(key, value)| (key.into(), Bytes::from_underlying(value)))
            .collect(),
        span_links: span
            .span_links
            .into_iter()
            .map(from_protobuf_link)
            .collect(),
        span_events: span
            .span_events
            .into_iter()
            .map(from_protobuf_event)
            .collect::<anyhow::Result<_>>()?,
    })
}

fn own_string<T: Borrow<str>>(value: T) -> String {
    value.borrow().to_owned()
}

fn own_text_map<T: TraceData>(map: VecMap<T::Text, T::Text>) -> HashMap<String, String> {
    let mut output = HashMap::with_capacity(map.len());
    for (key, value) in map {
        output.insert(own_string(key), own_string(value));
    }
    output
}

fn own_map<T: TraceData>(map: VecMap<T::Text, f64>) -> HashMap<String, f64> {
    map.into_iter()
        .map(|(key, value)| (own_string(key), value))
        .collect()
}

fn own_byte_map<T: TraceData>(map: VecMap<T::Text, T::Bytes>) -> HashMap<String, Vec<u8>> {
    map.into_iter()
        .map(|(key, value)| (own_string(key), value.borrow().to_vec()))
        .collect()
}

fn into_protobuf_link<T: TraceData>(link: SpanLink<T>) -> pb::SpanLink {
    pb::SpanLink {
        trace_id: link.trace_id,
        trace_id_high: link.trace_id_high,
        span_id: link.span_id,
        attributes: link
            .attributes
            .into_iter()
            .map(|(key, value)| (own_string(key), own_string(value)))
            .collect(),
        tracestate: own_string(link.tracestate),
        flags: link.flags,
    }
}

fn from_protobuf_link(link: pb::SpanLink) -> SpanLinkBytes {
    SpanLink {
        trace_id: link.trace_id,
        trace_id_high: link.trace_id_high,
        span_id: link.span_id,
        attributes: link
            .attributes
            .into_iter()
            .map(|(key, value)| (BytesString::from(key), BytesString::from(value)))
            .collect(),
        tracestate: link.tracestate.into(),
        flags: link.flags,
    }
}

fn into_protobuf_event<T: TraceData>(event: SpanEvent<T>) -> pb::SpanEvent {
    pb::SpanEvent {
        time_unix_nano: event.time_unix_nano,
        name: own_string(event.name),
        attributes: event
            .attributes
            .into_iter()
            .map(|(key, value)| (own_string(key), into_protobuf_attribute(value)))
            .collect(),
    }
}

fn into_protobuf_attribute<T: TraceData>(value: AttributeAnyValue<T>) -> pb::AttributeAnyValue {
    match value {
        AttributeAnyValue::SingleValue(value) => into_protobuf_scalar::<T>(value),
        AttributeAnyValue::Array(values) => pb::AttributeAnyValue {
            r#type: pb::attribute_any_value::AttributeAnyValueType::ArrayValue as i32,
            array_value: Some(pb::AttributeArray {
                values: values
                    .into_iter()
                    .map(into_protobuf_array_scalar::<T>)
                    .collect(),
            }),
            ..Default::default()
        },
    }
}

fn into_protobuf_scalar<T: TraceData>(value: AttributeArrayValue<T>) -> pb::AttributeAnyValue {
    match value {
        AttributeArrayValue::String(value) => pb::AttributeAnyValue {
            r#type: pb::attribute_any_value::AttributeAnyValueType::StringValue as i32,
            string_value: own_string(value),
            ..Default::default()
        },
        AttributeArrayValue::Boolean(value) => pb::AttributeAnyValue {
            r#type: pb::attribute_any_value::AttributeAnyValueType::BoolValue as i32,
            bool_value: value,
            ..Default::default()
        },
        AttributeArrayValue::Integer(value) => pb::AttributeAnyValue {
            r#type: pb::attribute_any_value::AttributeAnyValueType::IntValue as i32,
            int_value: value,
            ..Default::default()
        },
        AttributeArrayValue::Double(value) => pb::AttributeAnyValue {
            r#type: pb::attribute_any_value::AttributeAnyValueType::DoubleValue as i32,
            double_value: value,
            ..Default::default()
        },
    }
}

fn into_protobuf_array_scalar<T: TraceData>(
    value: AttributeArrayValue<T>,
) -> pb::AttributeArrayValue {
    match value {
        AttributeArrayValue::String(value) => pb::AttributeArrayValue {
            r#type: pb::attribute_array_value::AttributeArrayValueType::StringValue as i32,
            string_value: own_string(value),
            ..Default::default()
        },
        AttributeArrayValue::Boolean(value) => pb::AttributeArrayValue {
            r#type: pb::attribute_array_value::AttributeArrayValueType::BoolValue as i32,
            bool_value: value,
            ..Default::default()
        },
        AttributeArrayValue::Integer(value) => pb::AttributeArrayValue {
            r#type: pb::attribute_array_value::AttributeArrayValueType::IntValue as i32,
            int_value: value,
            ..Default::default()
        },
        AttributeArrayValue::Double(value) => pb::AttributeArrayValue {
            r#type: pb::attribute_array_value::AttributeArrayValueType::DoubleValue as i32,
            double_value: value,
            ..Default::default()
        },
    }
}

fn from_protobuf_event(event: pb::SpanEvent) -> anyhow::Result<SpanEventBytes> {
    Ok(SpanEventBytes {
        time_unix_nano: event.time_unix_nano,
        name: event.name.into(),
        attributes: event
            .attributes
            .into_iter()
            .map(|(key, value)| Ok((key.into(), from_protobuf_attribute(value)?)))
            .collect::<anyhow::Result<_>>()?,
    })
}

fn from_protobuf_attribute(value: pb::AttributeAnyValue) -> anyhow::Result<AttributeAnyValueBytes> {
    use pb::attribute_any_value::AttributeAnyValueType;

    Ok(match value.r#type() {
        AttributeAnyValueType::StringValue => {
            AttributeAnyValue::SingleValue(AttributeArrayValue::String(value.string_value.into()))
        }
        AttributeAnyValueType::BoolValue => {
            AttributeAnyValue::SingleValue(AttributeArrayValue::Boolean(value.bool_value))
        }
        AttributeAnyValueType::IntValue => {
            AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(value.int_value))
        }
        AttributeAnyValueType::DoubleValue => {
            AttributeAnyValue::SingleValue(AttributeArrayValue::Double(value.double_value))
        }
        AttributeAnyValueType::ArrayValue => AttributeAnyValue::Array(
            value
                .array_value
                .context("agentless span event array is missing its values")?
                .values
                .into_iter()
                .map(from_protobuf_array_scalar)
                .collect::<anyhow::Result<_>>()?,
        ),
    })
}

fn from_protobuf_array_scalar(
    value: pb::AttributeArrayValue,
) -> anyhow::Result<AttributeArrayValueBytes> {
    use pb::attribute_array_value::AttributeArrayValueType;

    match value.r#type() {
        AttributeArrayValueType::StringValue => {
            Ok(AttributeArrayValue::String(value.string_value.into()))
        }
        AttributeArrayValueType::BoolValue => Ok(AttributeArrayValue::Boolean(value.bool_value)),
        AttributeArrayValueType::IntValue => Ok(AttributeArrayValue::Integer(value.int_value)),
        AttributeArrayValueType::DoubleValue => Ok(AttributeArrayValue::Double(value.double_value)),
    }
}
