// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    collections::{BTreeSet, HashSet},
    fmt::{self, Display},
};

use libdd_trace_obfuscation::{obfuscate::obfuscate_span, obfuscation_config::ObfuscationConfig};
use libdd_trace_protobuf::pb::{
    attribute_any_value::AttributeAnyValueType, attribute_array_value::AttributeArrayValueType,
    AttributeAnyValue, AttributeArray, AttributeArrayValue, Span, SpanEvent,
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
        input: mut span,
        expected,
    } in testcases
    {
        obfuscate_span(&mut span, &config);
        if !span_equal(&span, &expected) {
            failures.push(format!(
                "[{name}]: \n{}",
                SpanComparison::new(&span, &expected)
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
    fn new(left: &'a Span, right: &'a Span) -> Self {
        Self { left, right }
    }
}
impl Display for SpanComparison<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
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
