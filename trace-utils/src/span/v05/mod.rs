// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use crate::span::v05::dict::SharedDict;
use crate::span::{AttributeArrayValue, SpanBytes, SpanEventBytes, SpanLinkBytes};
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

///This structure is a wrapper around a slice of span events
///
/// It is meant to override the default serialization, so we can serialize attributes
/// differently from the original impl.
/// Span events are serialized to JSON and added to "meta" when serializing to v0.5
///
/// The main difference with messagepack serialization is that attributes with any types
/// are supposed to be mapped to their natural JSON representation.
///
/// There is no good way of overriding the default Serialize behavior other than doing it
/// for the whole data structures that embed it.
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
            AttributeAnyValueBytes::Array(attribute_array_values) => {
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
enum AttributeArrayValueV05<'a> {
    String(&'a BytesString),
    Boolean(bool),
    Integer(i64),
    Double(f64),
}

/// Wrappers that serialize span links into the JSON form the Datadog agent/backend
/// expects under the `_dd.span_links` meta key.
///
/// This must match the agent's `transform.MarshalLinks`
/// (datadog-agent `pkg/trace/transform/transform.go`), which is the canonical producer
/// of `_dd.span_links`. In particular:
/// - `trace_id` is the full 128-bit id, hex-encoded as 32 lowercase chars (high 64 bits first, then
///   low 64 bits).
/// - `span_id` is the 64-bit id, hex-encoded as 16 lowercase chars.
/// - `tracestate` and `attributes` are only emitted when non-empty.
/// - The v0.4 `flags` field is intentionally not emitted: it is not part of the `_dd.span_links`
///   contract (the agent's OTLP path drops it too).
struct SpanLinksSerializerV05<'a>(&'a [SpanLinkBytes]);
struct SpanLinkSerializerV05<'a>(&'a SpanLinkBytes);

impl serde::Serialize for SpanLinksSerializerV05<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for span_link in self.0 {
            seq.serialize_element(&SpanLinkSerializerV05(span_link))?;
        }
        seq.end()
    }
}

impl serde::Serialize for SpanLinkSerializerV05<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let link = self.0;
        // trace_id and span_id are always present; tracestate and attributes are
        // conditional, matching the agent's MarshalLinks output.
        let mut len = 2;
        if !link.tracestate.as_str().is_empty() {
            len += 1;
        }
        if !link.attributes.is_empty() {
            len += 1;
        }
        let mut map = serializer.serialize_map(Some(len))?;
        // 128-bit trace id: high 64 bits first, then low 64 bits, hex-encoded.
        map.serialize_entry(
            "trace_id",
            &format!("{:016x}{:016x}", link.trace_id_high, link.trace_id),
        )?;
        map.serialize_entry("span_id", &format!("{:016x}", link.span_id))?;
        if !link.tracestate.as_str().is_empty() {
            map.serialize_entry("tracestate", &link.tracestate)?;
        }
        if !link.attributes.is_empty() {
            map.serialize_entry("attributes", &link.attributes)?;
        }
        map.end()
    }
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

/// Converts a v0.4 [`SpanBytes`] into its v0.5 dictionary-encoded representation.
///
/// The v0.5 format is a fixed 12-element positional array (service, name, resource,
/// trace_id, span_id, parent_id, start, duration, error, meta, metrics, type — see the
/// agent's `UnmarshalMsgDictionary` decoder). It predates `span_links`, `span_events`,
/// and `meta_struct`, none of which have a dedicated slot.
///
/// `span_links` and `span_events` are carried in `meta` as JSON strings under the
/// `_dd.span_links` and `events` keys respectively, matching the keys/shapes the Datadog
/// agent and backend understand (the agent's OTLP `MarshalLinks` / `MarshalEvents`).
///
/// `meta_struct` is intentionally dropped: it carries arbitrary binary (msgpack) blobs,
/// the v0.5 `meta` map is string->string only, and there is no agent-side meta-key
/// convention for reconstructing `meta_struct` from a v0.5 payload. Callers that need to
/// preserve `meta_struct` must use the v0.4 output format.
pub fn from_span_bytes(span: &SpanBytes, dict: &mut SharedDict) -> Result<Span> {
    let service = dict.get_or_insert(&span.service)?;
    let name = dict.get_or_insert(&span.name)?;
    let resource = dict.get_or_insert(&span.resource)?;
    let mut meta = span.meta.iter().try_fold(
        HashMap::with_capacity(span.meta.len()),
        |mut meta, (k, v)| -> anyhow::Result<HashMap<u32, u32>> {
            meta.insert(dict.get_or_insert(k)?, dict.get_or_insert(v)?);
            Ok(meta)
        },
    )?;
    if !span.span_links.is_empty() {
        let serialized_span_links =
            serde_json::to_string(&SpanLinksSerializerV05(&span.span_links))?;
        meta.insert(
            dict.get_or_insert(&tinybytes::BytesString::from("_dd.span_links"))?,
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
        service,
        name,
        resource,
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
                "_dd.span_links",
                "[{\"trace_id\":\"00000000000109320000000000003039\",\"span_id\":\"000000000000d431\",\"tracestate\":\"tracestate_value\",\"attributes\":{\"key\":\"val\"}}]",
            ),
            (
                "events",
                "[{\"time_unix_nano\":123,\"name\":\"ev1\",\"attributes\":{\"str_attr\":\"val\"}},{\"time_unix_nano\":456,\"name\":\"ev2\",\"attributes\":{\"bool_attr\":true}},{\"time_unix_nano\":789,\"name\":\"ev3\",\"attributes\":{\"list_attr\":[\"val1\",\"val2\"]}}]",
            ),
            (
                "meta_field",
                "meta_value",
            ),
        ]);
    }

    /// Empty span_links / span_events must not add any `_dd.span_links` / `events`
    /// meta keys (matches the agent, which only writes them when non-empty).
    #[test]
    fn from_span_bytes_empty_links_and_events_test() {
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
            metrics: HashMap::new(),
            meta_struct: HashMap::new(),
            span_links: vec![],
            span_events: vec![],
        };

        let mut dict = SharedDict::default();
        let v05_span = from_span_bytes(&span, &mut dict).unwrap();
        let dict = dict.dict();

        let keys: Vec<&str> = v05_span
            .meta
            .keys()
            .map(|k| dict[*k as usize].as_str())
            .collect();
        assert_eq!(v05_span.meta.len(), 1);
        assert!(keys.contains(&"meta_field"));
        assert!(!keys.contains(&"_dd.span_links"));
        assert!(!keys.contains(&"events"));
    }

    /// A span link with no tracestate and no attributes serializes only `trace_id`
    /// and `span_id`, both hex-encoded.
    #[test]
    fn span_link_minimal_serialization_test() {
        let links = vec![SpanLinkBytes {
            trace_id: 0xdead_beef,
            trace_id_high: 0,
            span_id: 0xfeed,
            attributes: HashMap::new(),
            tracestate: BytesString::from(""),
            flags: 7,
        }];
        let json = serde_json::to_string(&SpanLinksSerializerV05(&links)).unwrap();
        // No tracestate, no attributes, and `flags` is intentionally dropped.
        assert_eq!(
            json,
            "[{\"trace_id\":\"000000000000000000000000deadbeef\",\"span_id\":\"000000000000feed\"}]"
        );
    }

    /// Span event attributes of every scalar type, plus arrays of non-string scalars,
    /// must render as their natural JSON representation.
    #[test]
    fn span_event_attribute_types_serialization_test() {
        let events = vec![SpanEventBytes {
            time_unix_nano: 42,
            name: BytesString::from("ev"),
            attributes: HashMap::from([
                (
                    BytesString::from("int_attr"),
                    AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Integer(-7)),
                ),
                (
                    BytesString::from("dbl_attr"),
                    AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Double(2.5)),
                ),
            ]),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05(&events)).unwrap();
        // HashMap ordering is non-deterministic, so parse and assert on the structure.
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let attrs = &parsed[0]["attributes"];
        assert_eq!(attrs["int_attr"], serde_json::json!(-7));
        assert_eq!(attrs["dbl_attr"], serde_json::json!(2.5));
        assert_eq!(parsed[0]["time_unix_nano"], serde_json::json!(42));
        assert_eq!(parsed[0]["name"], serde_json::json!("ev"));
    }

    /// Arrays of non-string scalars (bool/int/double) must serialize as natural JSON arrays.
    #[test]
    fn span_event_non_string_array_serialization_test() {
        let events = vec![SpanEventBytes {
            time_unix_nano: 1,
            name: BytesString::from("ev"),
            attributes: HashMap::from([(
                BytesString::from("arr"),
                AttributeAnyValueBytes::Array(vec![
                    AttributeArrayValueBytes::Integer(1),
                    AttributeArrayValueBytes::Boolean(true),
                    AttributeArrayValueBytes::Double(3.5),
                ]),
            )]),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05(&events)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed[0]["attributes"]["arr"],
            serde_json::json!([1, true, 3.5])
        );
    }

    /// Multi-attribute events serialize correctly; HashMap order is non-deterministic so
    /// the JSON is compared structurally rather than by exact string.
    #[test]
    fn span_event_multi_attribute_serialization_test() {
        let events = vec![SpanEventBytes {
            time_unix_nano: 9,
            name: BytesString::from("multi"),
            attributes: HashMap::from([
                (
                    BytesString::from("a"),
                    AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::String(
                        BytesString::from("x"),
                    )),
                ),
                (
                    BytesString::from("b"),
                    AttributeAnyValueBytes::SingleValue(AttributeArrayValueBytes::Boolean(false)),
                ),
            ]),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05(&events)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["attributes"]["a"], serde_json::json!("x"));
        assert_eq!(parsed[0]["attributes"]["b"], serde_json::json!(false));
    }

    /// `meta_struct` has no representation in the v0.5 format and must be dropped: the
    /// conversion succeeds and produces no extra meta keys (locks in the documented
    /// contract on `from_span_bytes`).
    #[test]
    fn from_span_bytes_drops_meta_struct_test() {
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
            metrics: HashMap::new(),
            meta_struct: HashMap::from([(
                BytesString::from("appsec"),
                tinybytes::Bytes::from(vec![0x01u8, 0x02, 0x03]),
            )]),
            span_links: vec![],
            span_events: vec![],
        };

        let mut dict = SharedDict::default();
        let v05_span = from_span_bytes(&span, &mut dict).unwrap();
        let dict = dict.dict();

        let keys: Vec<&str> = v05_span
            .meta
            .keys()
            .map(|k| dict[*k as usize].as_str())
            .collect();
        // Only the original meta entry survives; nothing derived from meta_struct.
        assert_eq!(v05_span.meta.len(), 1);
        assert!(keys.contains(&"meta_field"));
        assert!(!keys.contains(&"appsec"));
        assert!(!keys.contains(&"meta_struct"));
    }

    /// Multiple span links serialize as an ordered JSON array preserving input order.
    #[test]
    fn span_links_multiple_serialization_test() {
        let links = vec![
            SpanLinkBytes {
                trace_id: 0x11,
                trace_id_high: 0,
                span_id: 0x22,
                attributes: HashMap::new(),
                tracestate: BytesString::from(""),
                flags: 0,
            },
            SpanLinkBytes {
                trace_id: 0x33,
                trace_id_high: 0,
                span_id: 0x44,
                attributes: HashMap::new(),
                tracestate: BytesString::from(""),
                flags: 0,
            },
        ];
        let json = serde_json::to_string(&SpanLinksSerializerV05(&links)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[0]["span_id"], serde_json::json!("0000000000000022"));
        assert_eq!(parsed[1]["span_id"], serde_json::json!("0000000000000044"));
    }

    /// A link with tracestate but no attributes emits `tracestate` and omits `attributes`.
    #[test]
    fn span_link_only_tracestate_serialization_test() {
        let links = vec![SpanLinkBytes {
            trace_id: 1,
            trace_id_high: 0,
            span_id: 2,
            attributes: HashMap::new(),
            tracestate: BytesString::from("ts"),
            flags: 0,
        }];
        let json = serde_json::to_string(&SpanLinksSerializerV05(&links)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["tracestate"], serde_json::json!("ts"));
        assert!(parsed[0].get("attributes").is_none());
    }

    /// A link with attributes but no tracestate emits `attributes` and omits `tracestate`.
    #[test]
    fn span_link_only_attributes_serialization_test() {
        let links = vec![SpanLinkBytes {
            trace_id: 1,
            trace_id_high: 0,
            span_id: 2,
            attributes: HashMap::from([(BytesString::from("k"), BytesString::from("v"))]),
            tracestate: BytesString::from(""),
            flags: 0,
        }];
        let json = serde_json::to_string(&SpanLinksSerializerV05(&links)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["attributes"]["k"], serde_json::json!("v"));
        assert!(parsed[0].get("tracestate").is_none());
    }

    /// A span event with an empty attributes map renders `"attributes":{}`.
    #[test]
    fn span_event_empty_attributes_serialization_test() {
        let events = vec![SpanEventBytes {
            time_unix_nano: 1,
            name: BytesString::from("ev"),
            attributes: HashMap::new(),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05(&events)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["attributes"], serde_json::json!({}));
    }
}
