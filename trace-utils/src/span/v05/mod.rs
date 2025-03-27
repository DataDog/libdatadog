// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use crate::span::v05::dict::SharedDict;
use crate::span::{AttributeArrayValue, SpanBytes, SpanEventBytes};
use anyhow::Result;
use serde::Serialize;
use std::collections::HashMap;
use tinybytes::BytesString;

use super::{AttributeAnyValueBytes, AttributeArrayValueBytes};

/// Structure that represent a TraceChunk Span which String fields are interned in a shared
/// dictionary. The number of elements is fixed by the spec and they all need to be serialized, in
/// case of adding more items the constant msgpack_decoder::v05::SPAN_ELEM_COUNT need to be
/// updated.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Span {
    pub service: u32,
    pub name: u32,
    pub resource: u32,
    pub trace_id: u64,
    pub span_id: u64,
    pub parent_id: u64,
    pub start: i64,
    pub duration: i64,
    pub error: i32,
    pub meta: HashMap<u32, u32>,
    pub metrics: HashMap<u32, f64>,
    pub r#type: u32,
}

///This structure is a wrapper around aa slice of span events
/// 
/// It is meant to overrdide the default serialization, so we can serialize attributes
/// differently from the original impl.
/// Span events are serialized to JSON and added to "meta" when serializing to v0.5 
/// 
/// The main difference with messagepacck serialization is that attributes with any types
/// are supposed to be mapped to their natural JSON representation.
/// 
/// Sadly, I haven't found a good way of overriding the default Serialize behavior, other
/// than just doing it for the whole data structures that embed it.
struct SpanEventsSerializerV05<'a>(&'a [SpanEventBytes]);
struct SpanEventSerializerV05<'a>(&'a SpanEventBytes);
struct SpanEventAttributesSerializerV05<'a>(&'a HashMap<BytesString, AttributeAnyValueBytes>);
struct AttributeAnyValueV05<'a>(&'a AttributeAnyValueBytes);

impl serde::Serialize for SpanEventsSerializerV05<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for span_event in self.0 {
            seq.serialize_element(&SpanEventSerializerV05(span_event))?;
        }
        seq.end()
    }
}

impl serde::Serialize for SpanEventSerializerV05<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        use serde::ser::SerializeStruct;
        let mut state = serializer.serialize_struct("SpanEventV05", 3)?;
        state.serialize_field("time_unix_nano", &self.0.time_unix_nano)?;
        state.serialize_field("name", &self.0.name)?;
        state.serialize_field(
            "attributes",
            &SpanEventAttributesSerializerV05(&self.0.attributes),
        )?;
        state.end()
    }
}

impl serde::Serialize for SpanEventAttributesSerializerV05<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in self.0 {
            map.serialize_entry(key, &AttributeAnyValueV05(value))?;
        }
        map.end()
    }
}

impl serde::Serialize for AttributeAnyValueV05<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        use serde::ser::SerializeSeq;
        match self.0 {
            AttributeAnyValueBytes::SingleValue(v) => {
                AttributeArrayValueV05::from_inner(v).serialize(serializer)
            }
            super::AttributeAnyValue::Array(attribute_array_values) => {
                let mut seq = serializer.serialize_seq(Some(attribute_array_values.len()))?;
                for value in attribute_array_values {
                    seq.serialize_element(&AttributeArrayValueV05::from_inner(value))?;
                }
                seq.end()
            }
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum AttributeArrayValueV05<'a> {
    String(&'a BytesString),
    Boolean(bool),
    Integer(i64),
    Double(f64),
}

impl<'a> AttributeArrayValueV05<'a> {
    fn from_inner(v: &'a AttributeArrayValueBytes) -> Self {
        use AttributeArrayValue::*;
        match v {
            String(string) => AttributeArrayValueV05::String(string),
            Boolean(boolean) => AttributeArrayValueV05::Boolean(*boolean),
            Integer(integer) => AttributeArrayValueV05::Integer(*integer),
            Double(double) => AttributeArrayValueV05::Double(*double),
        }
    }
}

pub fn from_span_bytes(span: &SpanBytes, dict: &mut SharedDict) -> Result<Span> {
    let mut meta = span.meta.iter().try_fold(
        HashMap::with_capacity(span.meta.len()),
        |mut meta, (k, v)| -> anyhow::Result<HashMap<u32, u32>> {
            meta.insert(dict.get_or_insert(k)?, dict.get_or_insert(v)?);
            Ok(meta)
        },
    )?;
    if !span.span_links.is_empty() {
        let serialized_span_links = serde_json::to_string(&span.span_links)?;
        meta.insert(
            dict.get_or_insert(&tinybytes::BytesString::from("span_links"))?,
            dict.get_or_insert(&tinybytes::BytesString::from(serialized_span_links))?,
        );
    }
    if !span.span_events.is_empty() {
        let serialized_span_events =
            serde_json::to_string(&SpanEventsSerializerV05(&span.span_events))?;
        meta.insert(
            dict.get_or_insert(&tinybytes::BytesString::from("events"))?,
            dict.get_or_insert(&tinybytes::BytesString::from(serialized_span_events))?,
        );
    }
    Ok(Span {
        service: dict.get_or_insert(&span.service)?,
        name: dict.get_or_insert(&span.name)?,
        resource: dict.get_or_insert(&span.resource)?,
        trace_id: span.trace_id,
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta,
        metrics: span.metrics.iter().try_fold(
            HashMap::with_capacity(span.metrics.len()),
            |mut metrics, (k, v)| -> anyhow::Result<HashMap<u32, f64>> {
                metrics.insert(dict.get_or_insert(k)?, *v);
                Ok(metrics)
            },
        )?,
        r#type: dict.get_or_insert(&span.r#type)?,
    })
}

#[cfg(test)]
mod tests {
    use crate::span::SpanLinkBytes;

    use super::*;
    use tinybytes::BytesString;

    #[test]
    fn from_span_bytes_test() {
        let span = SpanBytes {
            service: BytesString::from("service"),
            name: BytesString::from("name"),
            resource: BytesString::from("resource"),
            r#type: BytesString::from("type"),
            trace_id: 1,
            span_id: 1,
            parent_id: 0,
            start: 1,
            duration: 111,
            error: 0,
            meta: HashMap::from([(
                BytesString::from("meta_field"),
                BytesString::from("meta_value"),
            )]),
            metrics: HashMap::from([(BytesString::from("metrics_field"), 1.1)]),
            meta_struct: HashMap::new(),
            span_links: vec![SpanLinkBytes {
                trace_id: 12345,
                trace_id_high: 67890,
                span_id: 54321,
                attributes: HashMap::from([(BytesString::from("key"), BytesString::from("val"))]),
                tracestate: BytesString::from("tracestate_value"),
                flags: 1,
            }],
            span_events: vec![
                SpanEventBytes {
                    time_unix_nano: 123,
                    name: BytesString::from("ev1"),
                    attributes: HashMap::from([(
                        BytesString::from("str_attr"),
                        AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::String(
                            BytesString::from("val"),
                        )),
                    )]),
                },
                SpanEventBytes {
                    time_unix_nano: 456,
                    name: BytesString::from("ev2"),
                    attributes: HashMap::from([(
                        BytesString::from("bool_attr"),
                        AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Boolean(
                            true,
                        )),
                    )]),
                },
                SpanEventBytes {
                    time_unix_nano: 789,
                    name: BytesString::from("ev3"),
                    attributes: HashMap::from([(
                        BytesString::from("list_attr"),
                        AttributeAnyValueBytes::Array(vec![
                            AttributeArrayValueBytes::String(BytesString::from("val1")),
                            AttributeArrayValueBytes::String(BytesString::from("val2")),
                        ]),
                    )]),
                },
            ],
        };

        let mut dict = SharedDict::default();
        let v05_span = from_span_bytes(&span, &mut dict).unwrap();

        let dict = dict.dict();

        let get_index_from_str = |str: &str| -> u32 {
            dict.iter()
                .position(|s| s.as_str() == str)
                .unwrap()
                .try_into()
                .unwrap()
        };

        assert_eq!(v05_span.service, get_index_from_str("service"));
        assert_eq!(v05_span.name, get_index_from_str("name"));
        assert_eq!(v05_span.resource, get_index_from_str("resource"));
        assert_eq!(v05_span.r#type, get_index_from_str("type"));
        assert_eq!(v05_span.trace_id, 1);
        assert_eq!(v05_span.span_id, 1);
        assert_eq!(v05_span.parent_id, 0);
        assert_eq!(v05_span.start, 1);
        assert_eq!(v05_span.duration, 111);
        assert_eq!(v05_span.error, 0);
        assert_eq!(v05_span.meta.len(), 3);
        assert_eq!(v05_span.metrics.len(), 1);

        assert_eq!(
            *v05_span
                .meta
                .get(&get_index_from_str("meta_field"))
                .unwrap(),
            get_index_from_str("meta_value")
        );
        assert_eq!(
            *v05_span
                .metrics
                .get(&get_index_from_str("metrics_field"))
                .unwrap(),
            1.1
        );
        let mut meta = Vec::new();
        for (key, value) in &v05_span.meta {
            meta.push((dict[*key as usize].as_str(), dict[*value as usize].as_str()));
        }
        meta.sort();
        assert_eq!(meta, &[
            (
                "events",
                "[{\"time_unix_nano\":123,\"name\":\"ev1\",\"attributes\":{\"str_attr\":\"val\"}},{\"time_unix_nano\":456,\"name\":\"ev2\",\"attributes\":{\"bool_attr\":true}},{\"time_unix_nano\":789,\"name\":\"ev3\",\"attributes\":{\"list_attr\":[\"val1\",\"val2\"]}}]",
            ),
            (
                "meta_field",
                "meta_value",
            ),
            (
                "span_links",
                "[{\"trace_id\":12345,\"trace_id_high\":67890,\"span_id\":54321,\"attributes\":{\"key\":\"val\"},\"tracestate\":\"tracestate_value\",\"flags\":1}]",
            ),
        ]);
    }
}
