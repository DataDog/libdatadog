// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    clippy::unreachable,
    clippy::too_many_lines,
    clippy::format_push_string
)]

extern crate alloc;

use alloc::{collections::BTreeSet, fmt};
use core::fmt::Display;
use std::collections::HashSet;

use libdd_tinybytes::{Bytes, BytesString};
use libdd_trace_obfuscation::{
    obfuscate::{obfuscate_pb_span, obfuscate_v04_span},
    obfuscation_config::ObfuscationConfig,
};
use libdd_trace_protobuf::pb::{
    self, attribute_any_value::AttributeAnyValueType,
    attribute_array_value::AttributeArrayValueType, AttributeAnyValue, AttributeArray,
    AttributeArrayValue, Span, SpanEvent,
};
use libdd_trace_utils::span::{
    v04::{
        AttributeAnyValue as V04AttributeAnyValue, AttributeArrayValue as V04AttributeArrayValue,
        SpanBytes, SpanEventBytes, SpanLinkBytes,
    },
    BytesData,
};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Testcase {
    name: String,
    config: ObfuscationConfig,
    input: libdd_trace_protobuf::pb::Span,
    expected: libdd_trace_protobuf::pb::Span,
}

#[cfg_attr(miri, ignore)] // large fixture suite, prohibitively slow under Miri
#[test]
fn test_obfuscate_span() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/obfuscation_test_spans.jsonl");
    let testcases_contents =
        std::fs::read_to_string(&path).expect("Testsuite jsonl file should still be here");

    let testcases = serde_json::Deserializer::from_str(&testcases_contents)
        .into_iter()
        .map(Result::unwrap);

    let mut failures = vec![];

    for Testcase {
        name,
        config,
        input,
        expected,
    } in testcases
    {
        // --- pb path (reference implementation) ---
        let mut pb_span = input.clone();
        obfuscate_pb_span(&mut pb_span, &config);
        if !span_equal(&pb_span, &expected) {
            failures.push(format!(
                "[{name} / pb]: \n{}",
                SpanComparison::new(&pb_span, &expected)
            ));
        }

        // --- v04 path (must produce feature parity with the pb path) ---
        // The *same* fixture input is converted to a v04 span, obfuscated, then
        // converted back to a pb span. Both paths are therefore checked against
        // the *same* `expected` value with the *same* comparison logic, so any
        // divergence between `obfuscate_pb_span` and `obfuscate_v04_span` shows
        // up as a feature-parity failure tagged with `/ v04`.
        let mut v04_span = pb_span_to_v04(&input);
        obfuscate_v04_span(&mut v04_span, &config);
        let pb_result = v04_span_to_pb(&v04_span);
        if !span_equal(&pb_result, &expected) {
            failures.push(format!(
                "[{name} / v04]: \n{}",
                SpanComparison::new(&pb_result, &expected)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} failed cases:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// ---------------------------------------------------------------------------
// pb::Span <-> v04::Span<BytesData> conversions
//
// These exist only so the fixture suite (deserialized into `pb::Span`) can drive
// `obfuscate_v04_span` and be compared back against the same `pb::Span` expected
// value. They are straightforward field-by-field mappings with no obfuscation
// logic of their own.
// ---------------------------------------------------------------------------

fn bs(s: &str) -> BytesString {
    BytesString::from_string(s.to_owned())
}

fn pb_span_to_v04(span: &pb::Span) -> SpanBytes {
    SpanBytes {
        service: bs(&span.service),
        name: bs(&span.name),
        resource: bs(&span.resource),
        r#type: bs(&span.r#type),
        trace_id: u128::from(span.trace_id),
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta: span.meta.iter().map(|(k, v)| (bs(k), bs(v))).collect(),
        metrics: span.metrics.iter().map(|(k, v)| (bs(k), *v)).collect(),
        meta_struct: span
            .meta_struct
            .iter()
            .map(|(k, v)| (bs(k), Bytes::copy_from_slice(v)))
            .collect(),
        span_links: span.span_links.iter().map(pb_span_link_to_v04).collect(),
        span_events: span.span_events.iter().map(pb_span_event_to_v04).collect(),
    }
}

fn v04_span_to_pb(span: &SpanBytes) -> pb::Span {
    pb::Span {
        service: span.service.as_str().to_owned(),
        name: span.name.as_str().to_owned(),
        resource: span.resource.as_str().to_owned(),
        r#type: span.r#type.as_str().to_owned(),
        // `trace_id` was widened from a pb `u64` in `pb_span_to_v04`, so this
        // round-trip never truncates.
        trace_id: u64::try_from(span.trace_id).unwrap(),
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta: span
            .meta
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.as_str().to_owned()))
            .collect(),
        metrics: span
            .metrics
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), *v))
            .collect(),
        meta_struct: span
            .meta_struct
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_vec()))
            .collect(),
        span_links: span.span_links.iter().map(v04_span_link_to_pb).collect(),
        span_events: span.span_events.iter().map(v04_span_event_to_pb).collect(),
    }
}

fn pb_span_link_to_v04(link: &pb::SpanLink) -> SpanLinkBytes {
    SpanLinkBytes {
        trace_id: link.trace_id,
        trace_id_high: link.trace_id_high,
        span_id: link.span_id,
        attributes: link
            .attributes
            .iter()
            .map(|(k, v)| (bs(k), bs(v)))
            .collect(),
        tracestate: bs(&link.tracestate),
        flags: link.flags,
    }
}

fn v04_span_link_to_pb(link: &SpanLinkBytes) -> pb::SpanLink {
    pb::SpanLink {
        trace_id: link.trace_id,
        trace_id_high: link.trace_id_high,
        span_id: link.span_id,
        attributes: link
            .attributes
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.as_str().to_owned()))
            .collect(),
        tracestate: link.tracestate.as_str().to_owned(),
        flags: link.flags,
    }
}

fn pb_span_event_to_v04(event: &pb::SpanEvent) -> SpanEventBytes {
    SpanEventBytes {
        time_unix_nano: event.time_unix_nano,
        name: bs(&event.name),
        attributes: event
            .attributes
            .iter()
            .map(|(k, v)| (bs(k), pb_attr_any_value_to_v04(v)))
            .collect(),
    }
}

fn v04_span_event_to_pb(event: &SpanEventBytes) -> pb::SpanEvent {
    pb::SpanEvent {
        time_unix_nano: event.time_unix_nano,
        name: event.name.as_str().to_owned(),
        attributes: event
            .attributes
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v04_attr_any_value_to_pb(v)))
            .collect(),
    }
}

fn pb_attr_any_value_to_v04(v: &pb::AttributeAnyValue) -> V04AttributeAnyValue<BytesData> {
    match AttributeAnyValueType::try_from(v.r#type).unwrap() {
        AttributeAnyValueType::StringValue => {
            V04AttributeAnyValue::SingleValue(V04AttributeArrayValue::String(bs(&v.string_value)))
        }
        AttributeAnyValueType::BoolValue => {
            V04AttributeAnyValue::SingleValue(V04AttributeArrayValue::Boolean(v.bool_value))
        }
        AttributeAnyValueType::IntValue => {
            V04AttributeAnyValue::SingleValue(V04AttributeArrayValue::Integer(v.int_value))
        }
        AttributeAnyValueType::DoubleValue => {
            V04AttributeAnyValue::SingleValue(V04AttributeArrayValue::Double(v.double_value))
        }
        AttributeAnyValueType::ArrayValue => V04AttributeAnyValue::Array(
            v.array_value
                .as_ref()
                .unwrap()
                .values
                .iter()
                .map(pb_attr_array_value_to_v04)
                .collect(),
        ),
    }
}

fn v04_attr_any_value_to_pb(v: &V04AttributeAnyValue<BytesData>) -> pb::AttributeAnyValue {
    match v {
        V04AttributeAnyValue::SingleValue(av) => match av {
            V04AttributeArrayValue::String(s) => pb::AttributeAnyValue {
                r#type: AttributeAnyValueType::StringValue.into(),
                string_value: s.as_str().to_owned(),
                ..Default::default()
            },
            V04AttributeArrayValue::Boolean(b) => pb::AttributeAnyValue {
                r#type: AttributeAnyValueType::BoolValue.into(),
                bool_value: *b,
                ..Default::default()
            },
            V04AttributeArrayValue::Integer(i) => pb::AttributeAnyValue {
                r#type: AttributeAnyValueType::IntValue.into(),
                int_value: *i,
                ..Default::default()
            },
            V04AttributeArrayValue::Double(d) => pb::AttributeAnyValue {
                r#type: AttributeAnyValueType::DoubleValue.into(),
                double_value: *d,
                ..Default::default()
            },
        },
        V04AttributeAnyValue::Array(values) => pb::AttributeAnyValue {
            r#type: AttributeAnyValueType::ArrayValue.into(),
            array_value: Some(pb::AttributeArray {
                values: values.iter().map(v04_attr_array_value_to_pb).collect(),
            }),
            ..Default::default()
        },
    }
}

fn pb_attr_array_value_to_v04(e: &pb::AttributeArrayValue) -> V04AttributeArrayValue<BytesData> {
    match AttributeArrayValueType::try_from(e.r#type).unwrap() {
        AttributeArrayValueType::StringValue => V04AttributeArrayValue::String(bs(&e.string_value)),
        AttributeArrayValueType::BoolValue => V04AttributeArrayValue::Boolean(e.bool_value),
        AttributeArrayValueType::IntValue => V04AttributeArrayValue::Integer(e.int_value),
        AttributeArrayValueType::DoubleValue => V04AttributeArrayValue::Double(e.double_value),
    }
}

fn v04_attr_array_value_to_pb(e: &V04AttributeArrayValue<BytesData>) -> pb::AttributeArrayValue {
    match e {
        V04AttributeArrayValue::String(s) => pb::AttributeArrayValue {
            r#type: AttributeArrayValueType::StringValue.into(),
            string_value: s.as_str().to_owned(),
            ..Default::default()
        },
        V04AttributeArrayValue::Boolean(b) => pb::AttributeArrayValue {
            r#type: AttributeArrayValueType::BoolValue.into(),
            bool_value: *b,
            ..Default::default()
        },
        V04AttributeArrayValue::Integer(i) => pb::AttributeArrayValue {
            r#type: AttributeArrayValueType::IntValue.into(),
            int_value: *i,
            ..Default::default()
        },
        V04AttributeArrayValue::Double(d) => pb::AttributeArrayValue {
            r#type: AttributeArrayValueType::DoubleValue.into(),
            double_value: *d,
            ..Default::default()
        },
    }
}

fn span_equal(
    span: &libdd_trace_protobuf::pb::Span,
    expected: &libdd_trace_protobuf::pb::Span,
) -> bool {
    span.service == expected.service
        && span.name == expected.name
        && span.resource == expected.resource
        && span.trace_id == expected.trace_id
        && span.span_id == expected.span_id
        && span.parent_id == expected.parent_id
        && span.start == expected.start
        && span.duration == expected.duration
        && span.error == expected.error
        && span.meta == expected.meta
        && span.metrics == expected.metrics
        && span.r#type == expected.r#type
        && span.meta_struct == expected.meta_struct
        && span.span_links == expected.span_links
        && span_events_equal(span, expected)
}

fn span_events_equal(
    span: &libdd_trace_protobuf::pb::Span,
    expected: &libdd_trace_protobuf::pb::Span,
) -> bool {
    span.span_events
        .iter()
        .zip(expected.span_events.iter())
        .all(|(span, expected)| span_event_equal(span, expected))
}

fn span_event_equal(
    span: &libdd_trace_protobuf::pb::SpanEvent,
    expected: &libdd_trace_protobuf::pb::SpanEvent,
) -> bool {
    span.attributes.keys().collect::<HashSet<_>>() == expected.attributes.keys().collect()
        && span
            .attributes
            .iter()
            .map(|(k1, v1)| (v1.clone(), expected.attributes[k1].clone()))
            .all(|(v1, v2)| attribute_any_value_equal(&v1, &v2))
}

fn attribute_any_value_equal(v1: &AttributeAnyValue, v2: &AttributeAnyValue) -> bool {
    v1.r#type == v2.r#type
        && match AttributeAnyValueType::try_from(v1.r#type).unwrap() {
            AttributeAnyValueType::StringValue => v1.string_value == v2.string_value,
            AttributeAnyValueType::BoolValue => v1.bool_value == v2.bool_value,
            AttributeAnyValueType::IntValue => v1.int_value == v2.int_value,
            AttributeAnyValueType::DoubleValue => v1.double_value == v2.double_value,
            // this is a bit too strict but is not causing problems for now
            AttributeAnyValueType::ArrayValue => attribute_array_eq(
                v1.array_value.as_ref().unwrap(),
                v2.array_value.as_ref().unwrap(),
            ),
        }
}

fn attribute_array_eq(v1: &AttributeArray, v2: &AttributeArray) -> bool {
    v1.values
        .iter()
        .zip(v2.values.iter())
        .all(|(e1, e2)| attribute_array_value_eq(e1, e2))
}

fn attribute_array_value_eq(e1: &AttributeArrayValue, e2: &AttributeArrayValue) -> bool {
    e1.r#type == e2.r#type
        && match AttributeArrayValueType::try_from(e1.r#type).unwrap() {
            AttributeArrayValueType::StringValue => e1.string_value == e2.string_value,
            AttributeArrayValueType::BoolValue => e1.bool_value == e2.bool_value,
            AttributeArrayValueType::IntValue => e1.int_value == e2.int_value,
            AttributeArrayValueType::DoubleValue => e1.double_value == e2.double_value,
        }
}

struct SpanComparison<'a> {
    left: &'a Span,
    right: &'a Span,
}

impl<'a> SpanComparison<'a> {
    const fn new(left: &'a Span, right: &'a Span) -> Self {
        Self { left, right }
    }
}
impl Display for SpanComparison<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        fn cmp_field<T: PartialEq + fmt::Debug>(left: &T, right: &T) -> String {
            if left == right {
                format!("{left:?}")
            } else {
                format!("{left:?} != {right:?}")
            }
        }

        fn fmt_attribute_array_value(v: &AttributeArrayValue) -> String {
            match AttributeArrayValueType::try_from(v.r#type).unwrap() {
                AttributeArrayValueType::StringValue => format!("String({:?})", v.string_value),
                AttributeArrayValueType::BoolValue => format!("Bool({})", v.bool_value),
                AttributeArrayValueType::IntValue => format!("Int({})", v.int_value),
                AttributeArrayValueType::DoubleValue => format!("Double({})", v.double_value),
            }
        }

        fn fmt_attribute_value(v: &AttributeAnyValue) -> String {
            match AttributeAnyValueType::try_from(v.r#type).unwrap() {
                AttributeAnyValueType::StringValue => format!("String({:?})", v.string_value),
                AttributeAnyValueType::BoolValue => format!("Bool({})", v.bool_value),
                AttributeAnyValueType::IntValue => format!("Int({})", v.int_value),
                AttributeAnyValueType::DoubleValue => format!("Double({})", v.double_value),
                AttributeAnyValueType::ArrayValue => {
                    let values = v
                        .array_value
                        .as_ref()
                        .map(|arr| {
                            arr.values
                                .iter()
                                .map(fmt_attribute_array_value)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    format!("Array([{}])", values.join(", "))
                }
            }
        }

        fn cmp_attribute_value(left: &AttributeAnyValue, right: &AttributeAnyValue) -> String {
            if left.r#type != right.r#type {
                return format!(
                    "{} != {}",
                    fmt_attribute_value(left),
                    fmt_attribute_value(right)
                );
            }
            if AttributeAnyValueType::try_from(left.r#type).unwrap()
                == AttributeAnyValueType::ArrayValue
            {
                let lv = left
                    .array_value
                    .as_ref()
                    .map(|a| a.values.as_slice())
                    .unwrap_or_default();
                let rv = right
                    .array_value
                    .as_ref()
                    .map(|a| a.values.as_slice())
                    .unwrap_or_default();
                if lv == rv {
                    return fmt_attribute_value(left);
                }
                let elems: Vec<String> = lv
                    .iter()
                    .zip(rv.iter())
                    .enumerate()
                    .map(|(i, (l, r))| {
                        let lf = fmt_attribute_array_value(l);
                        let rf = fmt_attribute_array_value(r);
                        if lf == rf {
                            format!("        {lf}")
                        } else {
                            format!("        [{i}]: {lf} != {rf}")
                        }
                    })
                    .collect();
                return format!("Array([\n{}\n                    ])", elems.join(",\n"));
            }
            let l = fmt_attribute_value(left);
            let r = fmt_attribute_value(right);
            if l == r {
                l
            } else {
                format!("{l} != {r}")
            }
        }

        fn cmp_span_events(left: &[SpanEvent], right: &[SpanEvent]) -> String {
            if left == right {
                return format!("{left:?}");
            }
            let mut out = String::from("[\n");
            for (i, (l, r)) in left.iter().zip(right.iter()).enumerate() {
                if l == r {
                    out.push_str(&format!("        [{i}] = {l:?},\n"));
                    continue;
                }
                out.push_str(&format!("        [{i}] = SpanEvent {{\n"));
                out.push_str(&format!(
                    "            time_unix_nano: {},\n",
                    cmp_field(&l.time_unix_nano, &r.time_unix_nano)
                ));
                out.push_str(&format!(
                    "            name: {},\n",
                    cmp_field(&l.name, &r.name)
                ));
                out.push_str("            attributes: {\n");
                let all_keys: BTreeSet<_> =
                    l.attributes.keys().chain(r.attributes.keys()).collect();
                for key in all_keys {
                    let diff = match (l.attributes.get(key), r.attributes.get(key)) {
                        (Some(lv), Some(rv)) => cmp_attribute_value(lv, rv),
                        (Some(lv), None) => format!("{} != <missing>", fmt_attribute_value(lv)),
                        (None, Some(rv)) => format!("<missing> != {}", fmt_attribute_value(rv)),
                        (None, None) => unreachable!(),
                    };
                    out.push_str(&format!("                {key:?}: {diff},\n"));
                }
                out.push_str("            },\n");
                out.push_str("        },\n");
            }
            out.push_str("    ]");
            out
        }

        macro_rules! field {
            ($name:literal, $field:ident) => {
                writeln!(
                    f,
                    "    {}: {},",
                    $name,
                    cmp_field(&self.left.$field, &self.right.$field)
                )?;
            };
        }

        writeln!(f, "Span {{")?;
        field!("service", service);
        field!("name", name);
        field!("resource", resource);
        field!("trace_id", trace_id);
        field!("span_id", span_id);
        field!("parent_id", parent_id);
        field!("start", start);
        field!("duration", duration);
        field!("error", error);
        field!("meta", meta);
        field!("metrics", metrics);
        field!("type", r#type);
        field!("meta_struct", meta_struct);
        field!("span_links", span_links);
        writeln!(
            f,
            "    span_events: {},",
            cmp_span_events(&self.left.span_events, &self.right.span_events)
        )?;
        writeln!(f, "}}")
    }
}
