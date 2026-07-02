// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, SpanEvent, SpanLink};
use crate::span::TraceData;
use serde_json::{Map, Number, Value};
use std::borrow::Borrow;

pub const SPAN_LINKS_KEY: &str = "_dd.span_links";
pub const SPAN_EVENTS_KEY: &str = "events";

fn attribute_array_value_to_json<T: TraceData>(v: &AttributeArrayValue<T>) -> Value {
    match v {
        AttributeArrayValue::String(s) => Value::String(s.borrow().to_owned()),
        AttributeArrayValue::Boolean(b) => Value::Bool(*b),
        AttributeArrayValue::Integer(i) => Value::Number(Number::from(*i)),
        AttributeArrayValue::Double(d) => Number::from_f64(*d)
            .map(Value::Number)
            .unwrap_or(Value::Null),
    }
}

fn attribute_any_value_to_json<T: TraceData>(v: &AttributeAnyValue<T>) -> Value {
    match v {
        AttributeAnyValue::SingleValue(sv) => attribute_array_value_to_json(sv),
        AttributeAnyValue::Array(arr) => {
            Value::Array(arr.iter().map(attribute_array_value_to_json).collect())
        }
    }
}

/// JSON-encodes span links into the legacy `meta["_dd.span_links"]` format.
///
/// Format: `[{"trace_id": "<32-hex>", "span_id": "<16-hex>", "attributes": {...}, ...}]`
pub fn span_links_to_json<T: TraceData>(links: &[SpanLink<T>]) -> anyhow::Result<String> {
    let arr: Vec<Value> = links
        .iter()
        .map(|link| {
            let mut obj = Map::new();
            obj.insert(
                "trace_id".to_owned(),
                Value::String(format!("{:016x}{:016x}", link.trace_id_high, link.trace_id)),
            );
            obj.insert(
                "span_id".to_owned(),
                Value::String(format!("{:016x}", link.span_id)),
            );
            if !link.attributes.is_empty() {
                let attrs: Map<String, Value> = link
                    .attributes
                    .iter()
                    .map(|(k, v)| (k.borrow().to_owned(), Value::String(v.borrow().to_owned())))
                    .collect();
                obj.insert("attributes".to_owned(), Value::Object(attrs));
            }
            if !link.tracestate.borrow().is_empty() {
                obj.insert(
                    "tracestate".to_owned(),
                    Value::String(link.tracestate.borrow().to_owned()),
                );
            }
            if link.flags != 0 {
                obj.insert("flags".to_owned(), Value::Number(Number::from(link.flags)));
            }
            Value::Object(obj)
        })
        .collect();
    Ok(serde_json::to_string(&arr)?)
}

/// JSON-encodes span events into the legacy `meta["events"]` format.
///
/// Format: `[{"name": "<str>", "time_unix_nano": <u64>, "attributes": {...}}]`
pub fn span_events_to_json<T: TraceData>(events: &[SpanEvent<T>]) -> anyhow::Result<String> {
    let arr: Vec<Value> = events
        .iter()
        .map(|event| {
            let mut obj = Map::new();
            obj.insert(
                "name".to_owned(),
                Value::String(event.name.borrow().to_owned()),
            );
            obj.insert(
                "time_unix_nano".to_owned(),
                Value::Number(Number::from(event.time_unix_nano)),
            );
            if !event.attributes.is_empty() {
                let attrs: Map<String, Value> = event
                    .attributes
                    .iter()
                    .map(|(k, v)| (k.borrow().to_owned(), attribute_any_value_to_json(v)))
                    .collect();
                obj.insert("attributes".to_owned(), Value::Object(attrs));
            }
            Value::Object(obj)
        })
        .collect();
    Ok(serde_json::to_string(&arr)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, SpanEvent, SpanLink};
    use crate::span::SliceData;
    use std::collections::HashMap;

    #[test]
    fn test_span_links_to_json_basic() {
        let links: Vec<SpanLink<SliceData<'_>>> = vec![SpanLink {
            trace_id: 0x1234567890abcdef,
            trace_id_high: 0xfedcba0987654321,
            span_id: 0xabcdef1234567890,
            attributes: HashMap::new(),
            tracestate: "",
            flags: 0,
        }];
        let json = span_links_to_json(&links).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["trace_id"], "fedcba09876543211234567890abcdef");
        assert_eq!(parsed[0]["span_id"], "abcdef1234567890");
        assert!(!parsed[0].as_object().unwrap().contains_key("attributes"));
        assert!(!parsed[0].as_object().unwrap().contains_key("tracestate"));
        assert!(!parsed[0].as_object().unwrap().contains_key("flags"));
    }

    #[test]
    fn test_span_links_to_json_with_all_fields() {
        let mut attrs = HashMap::new();
        attrs.insert("key", "value");
        let links: Vec<SpanLink<SliceData<'_>>> = vec![SpanLink {
            trace_id: 1,
            trace_id_high: 0,
            span_id: 2,
            attributes: attrs,
            tracestate: "dd=s:1",
            flags: 1,
        }];
        let json = span_links_to_json(&links).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["attributes"]["key"], "value");
        assert_eq!(parsed[0]["tracestate"], "dd=s:1");
        assert_eq!(parsed[0]["flags"], 1);
    }

    #[test]
    fn test_span_links_to_json_empty() {
        let links: Vec<SpanLink<SliceData<'_>>> = vec![];
        let json = span_links_to_json(&links).unwrap();
        assert_eq!(json, "[]");
    }

    #[test]
    fn test_span_events_to_json_basic() {
        let events: Vec<SpanEvent<SliceData<'_>>> = vec![SpanEvent {
            time_unix_nano: 1727211691770716000,
            name: "exception",
            attributes: HashMap::new(),
        }];
        let json = span_events_to_json(&events).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0]["name"], "exception");
        assert_eq!(parsed[0]["time_unix_nano"], 1727211691770716000u64);
        assert!(!parsed[0].as_object().unwrap().contains_key("attributes"));
    }

    #[test]
    fn test_span_events_to_json_with_attributes() {
        let mut attrs = HashMap::new();
        attrs.insert(
            "exception.type",
            AttributeAnyValue::SingleValue(AttributeArrayValue::String("ValueError")),
        );
        attrs.insert(
            "exception.escaped",
            AttributeAnyValue::SingleValue(AttributeArrayValue::Boolean(false)),
        );
        attrs.insert(
            "exception.count",
            AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(3)),
        );
        attrs.insert(
            "exception.rate",
            AttributeAnyValue::SingleValue(AttributeArrayValue::Double(0.5)),
        );
        attrs.insert(
            "stack.frames",
            AttributeAnyValue::Array(vec![
                AttributeArrayValue::String("frame1"),
                AttributeArrayValue::String("frame2"),
            ]),
        );
        let events: Vec<SpanEvent<SliceData<'_>>> = vec![SpanEvent {
            time_unix_nano: 100,
            name: "test",
            attributes: attrs,
        }];
        let json = span_events_to_json(&events).unwrap();
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["attributes"]["exception.type"], "ValueError");
        assert_eq!(parsed[0]["attributes"]["exception.escaped"], false);
        assert_eq!(parsed[0]["attributes"]["exception.count"], 3);
        assert_eq!(parsed[0]["attributes"]["exception.rate"], 0.5);
        let frames = parsed[0]["attributes"]["stack.frames"].as_array().unwrap();
        assert_eq!(frames.len(), 2);
    }

    #[test]
    fn test_span_events_to_json_empty() {
        let events: Vec<SpanEvent<SliceData<'_>>> = vec![];
        let json = span_events_to_json(&events).unwrap();
        assert_eq!(json, "[]");
    }
}
