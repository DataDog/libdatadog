// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::invariants;
use crate::test_model::{TestLink, TestSpan, Value};
use arbitrary::{Arbitrary, Unstructured};
use libdd_trace_protobuf::pb;
use std::collections::HashMap;

// Limit size to avoid OOM and similar issues with large payloads.
const MAX_METRICS_SIZE: u8 = 10;
const MAX_META_SIZE: u8 = 10;
const MAX_ATTRS_SIZE: u8 = 10;
const MAX_META_STRUCT_SIZE: u8 = 100;
const MAX_LINKS_SIZE: u8 = 10;
const MAX_EVENTS_SIZE: u8 = 10;
/// The maximum nesting of V1 arrays and maps.
const MAX_VALUE_DEPTH: u8 = 3;
/// Languages for the fallback service name, `unnamed-<lang>-service`.
const LANGS: [&str; 4] = ["", "php", "python", "dotnet"];

/// Helper function to generate an arbitrary AttributeAnyValue
fn arbitrary_attribute_any_value(u: &mut Unstructured) -> arbitrary::Result<pb::AttributeAnyValue> {
    let value_type: u8 = u.arbitrary()?;

    match value_type % 4 {
        0 => {
            // String value
            Ok(pb::AttributeAnyValue {
                r#type: pb::attribute_any_value::AttributeAnyValueType::StringValue as i32,
                string_value: u.arbitrary()?,
                bool_value: false,
                int_value: 0,
                double_value: 0.0,
                array_value: None,
            })
        }
        1 => {
            // Bool value
            Ok(pb::AttributeAnyValue {
                r#type: pb::attribute_any_value::AttributeAnyValueType::BoolValue as i32,
                string_value: String::new(),
                bool_value: u.arbitrary()?,
                int_value: 0,
                double_value: 0.0,
                array_value: None,
            })
        }
        2 => {
            // Int value
            Ok(pb::AttributeAnyValue {
                r#type: pb::attribute_any_value::AttributeAnyValueType::IntValue as i32,
                string_value: String::new(),
                bool_value: false,
                int_value: u.arbitrary()?,
                double_value: 0.0,
                array_value: None,
            })
        }
        _ => {
            // Double value
            Ok(pb::AttributeAnyValue {
                r#type: pb::attribute_any_value::AttributeAnyValueType::DoubleValue as i32,
                string_value: String::new(),
                bool_value: false,
                int_value: 0,
                double_value: u.arbitrary()?,
                array_value: None,
            })
        }
    }
}

/// Custom wrapper to generate arbitrary Span data
#[derive(Debug)]
pub struct FuzzSpan {
    pub span: pb::Span,
}

impl<'a> Arbitrary<'a> for FuzzSpan {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        // Generate all basic fields
        let service: String = u.arbitrary()?;
        let name: String = u.arbitrary()?;
        let resource: String = u.arbitrary()?;
        let trace_id: u64 = u.arbitrary()?;
        let span_id: u64 = u.arbitrary()?;
        let parent_id: u64 = u.arbitrary()?;
        let start: i64 = u.arbitrary()?;
        let duration: i64 = u.arbitrary()?;
        let error: i32 = u.arbitrary()?;
        let r#type: String = u.arbitrary()?;

        // Generate meta HashMap (string -> string)
        let meta_size: u8 = u.arbitrary()?;
        let meta_size = (meta_size % MAX_META_SIZE) as usize;
        let mut meta = HashMap::new();
        for _ in 0..meta_size {
            let key: String = u.arbitrary()?;
            let value: String = u.arbitrary()?;
            meta.insert(key, value);
        }

        // Add special keys that normalize_span checks
        if u.arbitrary()? {
            let env_value: String = u.arbitrary()?;
            meta.insert("env".to_string(), env_value);
        }
        if u.arbitrary()? {
            let status_code: String = u.arbitrary()?;
            meta.insert("http.status_code".to_string(), status_code);
        }

        // Generate metrics HashMap (string -> f64)
        let metrics_size: u8 = u.arbitrary()?;
        let metrics_size = (metrics_size % MAX_METRICS_SIZE) as usize;
        let mut metrics = HashMap::new();
        for _ in 0..metrics_size {
            let key: String = u.arbitrary()?;
            let value: f64 = u.arbitrary()?;
            metrics.insert(key, value);
        }

        // Add special metrics that might be checked
        if u.arbitrary()? {
            let sampling_priority: f64 = u.arbitrary()?;
            metrics.insert("_sampling_priority_v1".to_string(), sampling_priority);
        }

        // Generate meta_struct HashMap (string -> Vec<u8>)
        let meta_struct_size: u8 = u.arbitrary()?;
        let meta_struct_size = (meta_struct_size % MAX_META_SIZE) as usize; // Limit size
        let mut meta_struct = HashMap::new();
        for _ in 0..meta_struct_size {
            let key: String = u.arbitrary()?;
            let value_len: u8 = u.arbitrary()?;
            let value_len = (value_len % MAX_META_STRUCT_SIZE) as usize; // Limit byte vec size
            let mut value = Vec::with_capacity(value_len);
            for _ in 0..value_len {
                value.push(u.arbitrary()?);
            }
            meta_struct.insert(key, value);
        }

        // Generate span_links
        let links_size: u8 = u.arbitrary()?;
        let links_size = (links_size % MAX_LINKS_SIZE) as usize;
        let mut span_links = Vec::new();
        for _ in 0..links_size {
            let link = pb::SpanLink {
                trace_id: u.arbitrary()?,
                trace_id_high: u.arbitrary()?,
                span_id: u.arbitrary()?,
                attributes: {
                    let attrs_size: u8 = u.arbitrary()?;
                    let attrs_size = (attrs_size % MAX_ATTRS_SIZE) as usize;
                    let mut attrs = HashMap::new();
                    for _ in 0..attrs_size {
                        let key: String = u.arbitrary()?;
                        let value: String = u.arbitrary()?;
                        attrs.insert(key, value);
                    }
                    attrs
                },
                tracestate: u.arbitrary()?,
                flags: u.arbitrary()?,
            };
            span_links.push(link);
        }

        // Generate span_events
        let events_size: u8 = u.arbitrary()?;
        let events_size = (events_size % MAX_EVENTS_SIZE) as usize;
        let mut span_events = Vec::new();
        for _ in 0..events_size {
            let event = pb::SpanEvent {
                name: u.arbitrary()?,
                time_unix_nano: u.arbitrary()?,
                attributes: {
                    let attrs_size: u8 = u.arbitrary()?;
                    let attrs_size = (attrs_size % MAX_ATTRS_SIZE) as usize;
                    let mut attrs = HashMap::new();
                    for _ in 0..attrs_size {
                        let key: String = u.arbitrary()?;
                        let value = arbitrary_attribute_any_value(u)?;
                        attrs.insert(key, value);
                    }
                    attrs
                },
            };
            span_events.push(event);
        }

        Ok(FuzzSpan {
            span: pb::Span {
                service,
                name,
                resource,
                trace_id,
                span_id,
                parent_id,
                start,
                duration,
                error,
                meta,
                metrics,
                r#type,
                meta_struct,
                span_links,
                span_events,
            },
        })
    }
}

/// Normalizes `fuzz_span` with the v0.4 normalizer and checks the result: the fuzzer catches the
/// panic of a failed check, as well as crashes and infinite loops.
pub fn fuzz_normalize_span(fuzz_span: FuzzSpan) {
    invariants::check_normalize_span(&fuzz_span.span);
}

/// An arbitrary V1 attribute value, nesting arrays and maps up to `depth` levels.
fn arbitrary_value(u: &mut Unstructured, depth: u8) -> arbitrary::Result<Value> {
    let max_kind = if depth == 0 { 4 } else { 6 };
    Ok(match u.int_in_range(0..=max_kind)? {
        0 => Value::String(u.arbitrary()?),
        1 => Value::Bool(u.arbitrary()?),
        2 => Value::Int(u.arbitrary()?),
        3 => Value::Float(u.arbitrary()?),
        4 => Value::Bytes(u.arbitrary()?),
        5 => {
            let len = u.int_in_range(0..=MAX_ATTRS_SIZE)?;
            Value::Array(
                (0..len)
                    .map(|_| arbitrary_value(u, depth - 1))
                    .collect::<arbitrary::Result<_>>()?,
            )
        }
        _ => Value::KeyValueList(arbitrary_attrs(u, depth - 1)?),
    })
}

/// Arbitrary V1 attributes, with values nesting up to `depth` levels.
fn arbitrary_attrs(u: &mut Unstructured, depth: u8) -> arbitrary::Result<HashMap<String, Value>> {
    let len = u.int_in_range(0..=MAX_ATTRS_SIZE)?;
    (0..len)
        .map(|_| Ok((u.arbitrary()?, arbitrary_value(u, depth)?)))
        .collect()
}

/// An arbitrary value for an attribute the normalizer reads as a string, more likely than
/// [`arbitrary_value`] to be a status code or a name.
fn arbitrary_string_like(u: &mut Unstructured) -> arbitrary::Result<Value> {
    Ok(match u.int_in_range(0..=2)? {
        0 => Value::Int(u.int_in_range(0..=700)?),
        1 => Value::String(u.int_in_range(0..=700u16)?.to_string()),
        _ => arbitrary_value(u, MAX_VALUE_DEPTH)?,
    })
}

/// Inserts an arbitrary value at `key` in `attrs`, half of the time.
fn maybe_insert(
    u: &mut Unstructured,
    attrs: &mut HashMap<String, Value>,
    key: &str,
    value: impl FnOnce(&mut Unstructured) -> arbitrary::Result<Value>,
) -> arbitrary::Result<()> {
    if u.arbitrary()? {
        attrs.insert(key.to_owned(), value(u)?);
    }
    Ok(())
}

/// An arbitrary V1 span, and the language of the tracer that sent it.
#[derive(Debug)]
pub struct FuzzV1Span {
    span: TestSpan,
    lang: &'static str,
}

impl<'a> Arbitrary<'a> for FuzzV1Span {
    fn arbitrary(u: &mut Unstructured<'a>) -> arbitrary::Result<Self> {
        let mut attrs = arbitrary_attrs(u, MAX_VALUE_DEPTH)?;
        // The attributes the normalizer reads.
        for key in ["peer.service", "_dd.base_service"] {
            maybe_insert(u, &mut attrs, key, |u| Ok(Value::String(u.arbitrary()?)))?;
        }
        maybe_insert(u, &mut attrs, "http.status_code", arbitrary_string_like)?;

        let links_size = u.int_in_range(0..=MAX_LINKS_SIZE)?;
        let mut links = Vec::new();
        for _ in 0..links_size {
            let mut attrs = arbitrary_attrs(u, MAX_VALUE_DEPTH)?;
            maybe_insert(u, &mut attrs, "link.name", arbitrary_string_like)?;
            links.push(TestLink { attrs });
        }

        Ok(FuzzV1Span {
            span: TestSpan {
                service: u.arbitrary()?,
                name: u.arbitrary()?,
                resource: u.arbitrary()?,
                typ: u.arbitrary()?,
                span_id: u.arbitrary()?,
                parent_id: u.arbitrary()?,
                start: u.arbitrary()?,
                duration: u.arbitrary()?,
                env: u.arbitrary()?,
                attrs,
                links,
            },
            lang: u.choose(&LANGS)?,
        })
    }
}

/// Normalizes `fuzz_span` with the V1 normalizer and checks the result.
pub fn fuzz_normalize_span_v1(fuzz_span: FuzzV1Span) {
    invariants::check_normalize_span_v1(&fuzz_span.span, fuzz_span.lang);
}

/// Runs each string normalizer on the valid UTF-8 prefix of `data` and checks the result.
pub fn fuzz_normalize_strings(data: &[u8]) {
    let s = match std::str::from_utf8(data) {
        Ok(s) => s,
        Err(e) => std::str::from_utf8(&data[..e.valid_up_to()]).unwrap_or_default(),
    };
    invariants::check_normalize_tag(s);
    invariants::check_normalize_tag_value(s);
    invariants::check_normalize_name(s);
    invariants::check_normalize_peer_service(s);
    for lang in LANGS {
        invariants::check_normalize_service(s, lang);
    }
    invariants::check_normalize_span_type(s);
    for limit in [0, 1, 2, 3, s.len() / 2, s.len()] {
        invariants::check_truncate_utf8(s, limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::{any, prop, proptest};

    /// Runs `target` on the value that `bytes` gives, as the fuzzer does.
    fn run<'a, T: Arbitrary<'a>>(bytes: &'a [u8], target: fn(T)) {
        if let Ok(value) = T::arbitrary_take_rest(Unstructured::new(bytes)) {
            target(value);
        }
    }

    proptest! {
        #[test]
        fn normalize_span(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            run(&bytes, fuzz_normalize_span);
        }

        #[test]
        fn normalize_span_v1(bytes in prop::collection::vec(any::<u8>(), 0..4096)) {
            run(&bytes, fuzz_normalize_span_v1);
        }

        #[test]
        fn normalize_strings(bytes in prop::collection::vec(any::<u8>(), 0..1024)) {
            fuzz_normalize_strings(&bytes);
        }
    }
}
