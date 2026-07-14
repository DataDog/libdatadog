// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

pub mod dict;

use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, SpanEvent, SpanLink};
use crate::span::{SharedDictBytes, SpanText, TraceData};
use anyhow::Result;
use indexmap::map::RawEntryApiV1;
use libdd_tinybytes::BytesString;
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::HashMap;

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

/// Serializes a slice of [`SpanLink`]s into the JSON array the Datadog agent and backend
/// expect under the `_dd.span_links` meta key.
///
/// This matches the agent's `transform.MarshalLinks`
/// (datadog-agent `pkg/trace/transform/transform.go`), the canonical producer of
/// `_dd.span_links`:
/// - `trace_id` is the full 128-bit id hex-encoded as 32 lowercase chars (high 64 bits first, then
///   low 64 bits).
/// - `span_id` is the 64-bit id hex-encoded as 16 lowercase chars.
/// - `tracestate` and `attributes` are only emitted when non-empty.
/// - `flags` is only emitted when not zero.
struct SpanLinksSerializerV05<'a, T: TraceData>(&'a [SpanLink<T>]);
struct SpanLinkSerializerV05<'a, T: TraceData>(&'a SpanLink<T>);

impl<'a, T: TraceData> Serialize for SpanLinksSerializerV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for link in self.0 {
            seq.serialize_element(&SpanLinkSerializerV05::<T>(link))?;
        }
        seq.end()
    }
}

impl<'a, T: TraceData> Serialize for SpanLinkSerializerV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let link = self.0;
        let tracestate: &str = link.tracestate.borrow();
        let has_tracestate = !tracestate.is_empty();
        let has_attributes = !link.attributes.is_empty();
        let has_flags = link.flags != 0;
        let len = 2 + has_tracestate as usize + has_attributes as usize + has_flags as usize;
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry(
            "trace_id",
            &format!("{:016x}{:016x}", link.trace_id_high, link.trace_id),
        )?;
        map.serialize_entry("span_id", &format!("{:016x}", link.span_id))?;
        if has_tracestate {
            map.serialize_entry("tracestate", &link.tracestate)?;
        }
        if has_attributes {
            map.serialize_entry(
                "attributes",
                &SortedStrMapSerializerV05::<T>(&link.attributes),
            )?;
        }
        if has_flags {
            map.serialize_entry("flags", &link.flags)?;
        }
        map.end()
    }
}

/// Serializes a `HashMap<T::Text, T::Text>` as a JSON object with keys in sorted order,
/// keeping the output deterministic for snapshot testing
struct SortedStrMapSerializerV05<'a, T: TraceData>(&'a HashMap<T::Text, T::Text>);

impl<'a, T: TraceData> Serialize for SortedStrMapSerializerV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries: Vec<(&str, &T::Text)> =
            self.0.iter().map(|(k, v)| (k.borrow(), v)).collect();
        entries.sort_unstable_by_key(|(k, _)| *k);
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (key, value) in entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

/// Serializes a slice of [`SpanEvent`]s into the JSON array the Datadog agent and backend
/// expect under the `events` meta key, matching the agent's `MarshalEvents`
/// (`{time_unix_nano, name, attributes}` with attributes rendered as natural JSON rather than
/// the v0.4 msgpack tagged form).
struct SpanEventsSerializerV05<'a, T: TraceData>(&'a [SpanEvent<T>]);
struct SpanEventSerializerV05<'a, T: TraceData>(&'a SpanEvent<T>);
struct SpanEventAttributesSerializerV05<'a, T: TraceData>(
    &'a HashMap<T::Text, AttributeAnyValue<T>>,
);
struct AttributeAnyValueV05<'a, T: TraceData>(&'a AttributeAnyValue<T>);
struct AttributeArrayValueV05<'a, T: TraceData>(&'a AttributeArrayValue<T>);

impl<'a, T: TraceData> Serialize for SpanEventsSerializerV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for event in self.0 {
            seq.serialize_element(&SpanEventSerializerV05::<T>(event))?;
        }
        seq.end()
    }
}

impl<'a, T: TraceData> Serialize for SpanEventSerializerV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let event = self.0;
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("time_unix_nano", &event.time_unix_nano)?;
        map.serialize_entry("name", &event.name)?;
        map.serialize_entry(
            "attributes",
            &SpanEventAttributesSerializerV05::<T>(&event.attributes),
        )?;
        map.end()
    }
}

impl<'a, T: TraceData> Serialize for SpanEventAttributesSerializerV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // Sort keys to match Go's `encoding/json` (used by the agent) and keep output
        // deterministic, since the source is an unordered `HashMap`.
        let mut entries: Vec<(&str, &AttributeAnyValue<T>)> =
            self.0.iter().map(|(k, v)| (k.borrow(), v)).collect();
        entries.sort_unstable_by_key(|(k, _)| *k);
        let mut map = serializer.serialize_map(Some(entries.len()))?;
        for (key, value) in entries {
            map.serialize_entry(key, &AttributeAnyValueV05::<T>(value))?;
        }
        map.end()
    }
}

impl<'a, T: TraceData> Serialize for AttributeAnyValueV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AttributeAnyValue::SingleValue(value) => {
                AttributeArrayValueV05::<T>(value).serialize(serializer)
            }
            AttributeAnyValue::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(&AttributeArrayValueV05::<T>(value))?;
                }
                seq.end()
            }
        }
    }
}

impl<'a, T: TraceData> Serialize for AttributeArrayValueV05<'a, T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            AttributeArrayValue::String(value) => value.serialize(serializer),
            AttributeArrayValue::Boolean(value) => serializer.serialize_bool(*value),
            AttributeArrayValue::Integer(value) => serializer.serialize_i64(*value),
            AttributeArrayValue::Double(value) => serializer.serialize_f64(*value),
        }
    }
}

/// Gets the index of the interned string. If the string is not part of the dictionary it is
/// added and its corresponding index returned.
///
/// Checks if the span text is already interned before creating a
/// new ByteString instance from it.
fn get_or_insert(
    dict: &mut SharedDictBytes,
    str: &impl SpanText,
) -> Result<u32, std::num::TryFromIntError> {
    let entry = dict.map.raw_entry_mut_v1().from_key(str.borrow());
    let idx = entry.index();
    entry.or_insert_with(|| (str.to_bytes_string(), ()));
    idx.try_into()
}

/// Converts a v0.4 [`Span`](crate::span::v04::Span) into its v0.5 dictionary-encoded form.
///
/// The v0.5 format is a fixed 12-element positional array (service, name, resource, trace_id,
/// span_id, parent_id, start, duration, error, meta, metrics, type). It predates `span_links`,
/// `span_events`, and `meta_struct`, none of which have a dedicated slot.
///
/// `span_links` and `span_events` are carried in `meta` as JSON strings under the
/// `_dd.span_links` and `events` keys, matching the shapes the Datadog agent/backend understand
/// (the agent's `MarshalLinks` / `MarshalEvents`). Both are only emitted when non-empty.
///
/// `meta_struct` is intentionally dropped: it carries arbitrary binary (msgpack) blobs, the
/// v0.5 `meta` map is string->string only, and there is no agent-side meta-key convention for
/// reconstructing it from a v0.5 payload. Callers that must preserve `meta_struct` should use
/// the v0.4 output format.
///
/// Carrying links/events requires interning dynamically-built JSON strings, so the shared
/// dictionary always owns its strings ([`SharedDictBytes`]). Borrowed input text is copied into
/// the dictionary; owned text is reference-counted.
pub fn from_v04_span<T: TraceData>(
    span: crate::span::v04::Span<T>,
    dict: &mut SharedDictBytes,
) -> Result<Span> {
    let meta_len = span.meta.len();
    let metrics_len = span.metrics.len();

    // Serialize span links / span events before `span` is consumed below. v0.5 has no
    // dedicated slots for them, so they are flattened into `meta` as JSON strings.
    let serialized_span_links = if span.span_links.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&SpanLinksSerializerV05::<T>(
            &span.span_links,
        ))?)
    };
    let serialized_span_events = if span.span_events.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&SpanEventsSerializerV05::<T>(
            &span.span_events,
        ))?)
    };

    let extra_meta =
        serialized_span_links.is_some() as usize + serialized_span_events.is_some() as usize;

    // Intern fields in the same order as the base conversion to keep dictionary indices
    // stable; the span links / events keys are appended to `meta` afterwards.
    let service = get_or_insert(dict, &span.service)?;
    let name = get_or_insert(dict, &span.name)?;
    let resource = get_or_insert(dict, &span.resource)?;
    let mut meta = span.meta.into_iter().try_fold(
        HashMap::with_capacity(meta_len + extra_meta),
        |mut meta, (k, v)| -> anyhow::Result<HashMap<u32, u32>> {
            meta.insert(get_or_insert(dict, &k)?, get_or_insert(dict, &v)?);
            Ok(meta)
        },
    )?;

    if let Some(links_json) = serialized_span_links {
        let key = dict.get_or_insert(BytesString::from_static("_dd.span_links"))?;
        let value = dict.get_or_insert(BytesString::from(links_json))?;
        meta.insert(key, value);
    }
    if let Some(events_json) = serialized_span_events {
        let key = dict.get_or_insert(BytesString::from_static("events"))?;
        let value = dict.get_or_insert(BytesString::from(events_json))?;
        meta.insert(key, value);
    }

    let metrics = span.metrics.into_iter().try_fold(
        HashMap::with_capacity(metrics_len),
        |mut metrics, (k, v)| -> anyhow::Result<HashMap<u32, f64>> {
            metrics.insert(get_or_insert(dict, &k)?, v);
            Ok(metrics)
        },
    )?;
    let r#type = get_or_insert(dict, &span.r#type)?;

    Ok(Span {
        service,
        name,
        resource,
        trace_id: span.trace_id as u64,
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: span.start,
        duration: span.duration,
        error: span.error,
        meta,
        metrics,
        r#type,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::v04::{SpanBytes, VecMap};
    use crate::span::BytesData;
    use libdd_tinybytes::BytesString;

    /// Returns the JSON string interned in `meta` under `key`, if present.
    fn meta_json(dict: &SharedDictBytes, span: &Span, key: &str) -> Option<String> {
        let entries: Vec<&str> = dict.iter().map(|s| s.as_str()).collect();
        let key_idx = entries.iter().position(|s| *s == key)? as u32;
        let val_idx = *span.meta.get(&key_idx)?;
        Some(entries[val_idx as usize].to_string())
    }

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
            meta: vec![(
                BytesString::from("meta_field"),
                BytesString::from("meta_value"),
            )]
            .into(),
            metrics: vec![(BytesString::from("metrics_field"), 1.1)].into(),
            meta_struct: VecMap::new(),
            span_links: vec![],
            span_events: vec![],
        };

        let mut dict = SharedDictBytes::default();
        let v05_span = from_v04_span(span, &mut dict).unwrap();

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
        assert_eq!(v05_span.meta.len(), 1);
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
    }

    fn base_span() -> SpanBytes {
        SpanBytes {
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
            meta: vec![(
                BytesString::from("meta_field"),
                BytesString::from("meta_value"),
            )]
            .into(),
            metrics: VecMap::new(),
            meta_struct: VecMap::new(),
            span_links: vec![],
            span_events: vec![],
        }
    }

    /// Span links and span events are flattened into `meta` as agent-compatible JSON under
    /// the `_dd.span_links` and `events` keys.
    #[test]
    fn from_v04_span_serializes_links_and_events_test() {
        let mut span = base_span();
        span.span_links = vec![SpanLink::<BytesData> {
            trace_id: 12345,
            trace_id_high: 67890,
            span_id: 54321,
            attributes: HashMap::from([(BytesString::from("key"), BytesString::from("val"))]),
            tracestate: BytesString::from("tracestate_value"),
            flags: 1,
        }];
        span.span_events = vec![SpanEvent::<BytesData> {
            time_unix_nano: 123,
            name: BytesString::from("ev1"),
            attributes: HashMap::from([(
                BytesString::from("str_attr"),
                AttributeAnyValue::SingleValue(AttributeArrayValue::String(BytesString::from(
                    "val",
                ))),
            )]),
        }];

        let mut dict = SharedDictBytes::default();
        let v05_span = from_v04_span(span, &mut dict).unwrap();

        let links_json = meta_json(&dict, &v05_span, "_dd.span_links").unwrap();
        assert_eq!(
            links_json,
            "[{\"trace_id\":\"00000000000109320000000000003039\",\"span_id\":\"000000000000d431\",\"tracestate\":\"tracestate_value\",\"attributes\":{\"key\":\"val\"},\"flags\":1}]"
        );
        let events_json = meta_json(&dict, &v05_span, "events").unwrap();
        assert_eq!(
            events_json,
            "[{\"time_unix_nano\":123,\"name\":\"ev1\",\"attributes\":{\"str_attr\":\"val\"}}]"
        );
        // Original meta entry plus the two synthesized keys.
        assert_eq!(v05_span.meta.len(), 3);
    }

    /// Empty links/events add no meta keys (matches the agent, which only writes them when
    /// non-empty).
    #[test]
    fn from_v04_span_empty_links_events_no_meta_keys_test() {
        let mut dict = SharedDictBytes::default();
        let v05_span = from_v04_span(base_span(), &mut dict).unwrap();
        assert_eq!(v05_span.meta.len(), 1);
        assert!(meta_json(&dict, &v05_span, "_dd.span_links").is_none());
        assert!(meta_json(&dict, &v05_span, "events").is_none());
    }

    /// `meta_struct` has no v0.5 representation and must be dropped; conversion still succeeds
    /// and produces no extra meta keys.
    #[test]
    fn from_v04_span_drops_meta_struct_test() {
        let mut span = base_span();
        span.meta_struct = vec![(
            BytesString::from("appsec"),
            libdd_tinybytes::Bytes::from_static(&[0x01, 0x02, 0x03]),
        )]
        .into();

        let mut dict = SharedDictBytes::default();
        let v05_span = from_v04_span(span, &mut dict).unwrap();
        assert_eq!(v05_span.meta.len(), 1);
        assert!(meta_json(&dict, &v05_span, "appsec").is_none());
        assert!(meta_json(&dict, &v05_span, "meta_struct").is_none());
    }

    /// A link with no tracestate and no attributes serializes only hex `trace_id`/`span_id`;
    /// `flags` is dropped.
    #[test]
    fn span_link_minimal_serialization_test() {
        let links = vec![SpanLink::<BytesData> {
            trace_id: 0xdead_beef,
            trace_id_high: 0,
            span_id: 0xfeed,
            attributes: HashMap::new(),
            tracestate: BytesString::from(""),
            flags: 7,
        }];
        let json = serde_json::to_string(&SpanLinksSerializerV05::<BytesData>(&links)).unwrap();
        assert_eq!(
            json,
            "[{\"trace_id\":\"000000000000000000000000deadbeef\",\"span_id\":\"000000000000feed\",\"flags\":7}]"
        );
    }

    /// Multiple links serialize as an ordered JSON array preserving input order.
    #[test]
    fn span_links_multiple_serialization_test() {
        let links = vec![
            SpanLink::<BytesData> {
                span_id: 0x22,
                ..Default::default()
            },
            SpanLink::<BytesData> {
                span_id: 0x44,
                ..Default::default()
            },
        ];
        let json = serde_json::to_string(&SpanLinksSerializerV05::<BytesData>(&links)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 2);
        assert_eq!(parsed[0]["span_id"], serde_json::json!("0000000000000022"));
        assert_eq!(parsed[1]["span_id"], serde_json::json!("0000000000000044"));
    }

    /// A link with tracestate but no attributes emits `tracestate`, omits `attributes`.
    #[test]
    fn span_link_only_tracestate_serialization_test() {
        let links = vec![SpanLink::<BytesData> {
            span_id: 2,
            tracestate: BytesString::from("ts"),
            ..Default::default()
        }];
        let json = serde_json::to_string(&SpanLinksSerializerV05::<BytesData>(&links)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["tracestate"], serde_json::json!("ts"));
        assert!(parsed[0].get("attributes").is_none());
    }

    /// A link with attributes but no tracestate emits `attributes`, omits `tracestate`.
    #[test]
    fn span_link_only_attributes_serialization_test() {
        let links = vec![SpanLink::<BytesData> {
            span_id: 2,
            attributes: HashMap::from([(BytesString::from("k"), BytesString::from("v"))]),
            ..Default::default()
        }];
        let json = serde_json::to_string(&SpanLinksSerializerV05::<BytesData>(&links)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["attributes"]["k"], serde_json::json!("v"));
        assert!(parsed[0].get("tracestate").is_none());
    }

    /// Event attributes of every scalar type render as natural JSON.
    #[test]
    fn span_event_attribute_types_serialization_test() {
        let events = vec![SpanEvent::<BytesData> {
            time_unix_nano: 42,
            name: BytesString::from("ev"),
            attributes: HashMap::from([
                (
                    BytesString::from("int_attr"),
                    AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(-7)),
                ),
                (
                    BytesString::from("dbl_attr"),
                    AttributeAnyValue::SingleValue(AttributeArrayValue::Double(2.5)),
                ),
                (
                    BytesString::from("bool_attr"),
                    AttributeAnyValue::SingleValue(AttributeArrayValue::Boolean(true)),
                ),
            ]),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05::<BytesData>(&events)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let attrs = &parsed[0]["attributes"];
        assert_eq!(attrs["int_attr"], serde_json::json!(-7));
        assert_eq!(attrs["dbl_attr"], serde_json::json!(2.5));
        assert_eq!(attrs["bool_attr"], serde_json::json!(true));
        assert_eq!(parsed[0]["time_unix_nano"], serde_json::json!(42));
        assert_eq!(parsed[0]["name"], serde_json::json!("ev"));
    }

    /// Arrays of non-string scalars serialize as natural JSON arrays.
    #[test]
    fn span_event_non_string_array_serialization_test() {
        let events = vec![SpanEvent::<BytesData> {
            time_unix_nano: 1,
            name: BytesString::from("ev"),
            attributes: HashMap::from([(
                BytesString::from("arr"),
                AttributeAnyValue::Array(vec![
                    AttributeArrayValue::Integer(1),
                    AttributeArrayValue::Boolean(true),
                    AttributeArrayValue::Double(3.5),
                ]),
            )]),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05::<BytesData>(&events)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed[0]["attributes"]["arr"],
            serde_json::json!([1, true, 3.5])
        );
    }

    /// Event and link attributes are emitted with keys in sorted order (matching Go's
    /// `encoding/json`, used by the agent) so the output is deterministic despite the
    /// `HashMap` source.
    #[test]
    fn attributes_serialized_in_sorted_key_order_test() {
        let events = vec![SpanEvent::<BytesData> {
            time_unix_nano: 1,
            name: BytesString::from("ev"),
            attributes: HashMap::from([
                (
                    BytesString::from("zebra"),
                    AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(1)),
                ),
                (
                    BytesString::from("alpha"),
                    AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(2)),
                ),
                (
                    BytesString::from("mike"),
                    AttributeAnyValue::SingleValue(AttributeArrayValue::Integer(3)),
                ),
            ]),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05::<BytesData>(&events)).unwrap();
        assert_eq!(
            json,
            "[{\"time_unix_nano\":1,\"name\":\"ev\",\"attributes\":{\"alpha\":2,\"mike\":3,\"zebra\":1}}]"
        );

        let links = vec![SpanLink::<BytesData> {
            span_id: 1,
            attributes: HashMap::from([
                (BytesString::from("zzz"), BytesString::from("1")),
                (BytesString::from("aaa"), BytesString::from("2")),
            ]),
            ..Default::default()
        }];
        let json = serde_json::to_string(&SpanLinksSerializerV05::<BytesData>(&links)).unwrap();
        assert!(
            json.contains("\"attributes\":{\"aaa\":\"2\",\"zzz\":\"1\"}"),
            "link attributes not sorted: {json}"
        );
    }

    /// An event with an empty attributes map renders `"attributes":{}`.
    #[test]
    fn span_event_empty_attributes_serialization_test() {
        let events = vec![SpanEvent::<BytesData> {
            time_unix_nano: 1,
            name: BytesString::from("ev"),
            attributes: HashMap::new(),
        }];
        let json = serde_json::to_string(&SpanEventsSerializerV05::<BytesData>(&events)).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed[0]["attributes"], serde_json::json!({}));
    }
}
