// Copyright 2025-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::normalizer;
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

/// Main fuzzing function that tests normalize_span with arbitrary data
pub fn fuzz_normalize_span(fuzz_span: FuzzSpan) {
    let mut span = fuzz_span.span;

    // Call normalize_span - it may succeed or fail, both are valid
    // The fuzzer will catch panics, crashes, or infinite loops
    let _ = normalizer::normalize_span(&mut span);
}
