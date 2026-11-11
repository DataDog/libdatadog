// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Conversion of a V1 [`TracerPayload`] into the intake's protobuf `idx` representation
//! (`AgentPayload.idxTracerPayloads`), used by the agentless send path.

use crate::msgpack_encoder::v1::normalize_span_start;
use crate::span::TraceData;
use crate::span::v1::{
    AttributeValue, Span, SpanEvent, SpanKind, SpanLink, TraceChunk, TracerPayload,
};
use crate::span::vec_map::VecMap;
use libdd_trace_model::TraceText;
use libdd_trace_normalization::normalizer::SamplerPriority;
use libdd_trace_protobuf::pb::idx;
use std::borrow::Borrow;
use std::collections::HashMap;
use std::collections::hash_map::Entry;

/// The payload has more distinct strings than a `u32` string reference can address.
#[derive(Debug)]
pub(crate) struct StringTableOverflow;

impl std::fmt::Display for StringTableOverflow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("V1 payload has more distinct strings than fit in a u32 string table")
    }
}

impl std::error::Error for StringTableOverflow {}

/// String table for one `idx::TracerPayload`. Index 0 is always the empty string, matching the
/// agent's `idx.StringTable`, so unset string fields can be left as reference 0.
struct StringTable<'a> {
    refs: HashMap<&'a str, u32>,
    strings: Vec<String>,
}

impl<'a> StringTable<'a> {
    fn new() -> Self {
        let mut refs = HashMap::new();
        refs.insert("", 0);
        Self {
            refs,
            strings: vec![String::new()],
        }
    }

    fn intern(&mut self, s: &'a str) -> Result<u32, StringTableOverflow> {
        if let Some(&r) = self.refs.get(s) {
            return Ok(r);
        }
        let r = u32::try_from(self.strings.len()).map_err(|_| StringTableOverflow)?;
        self.strings.push(s.to_owned());
        self.refs.insert(s, r);
        Ok(r)
    }

    fn text<T: TraceText>(&mut self, text: &'a T) -> Result<u32, StringTableOverflow> {
        self.intern(text.as_str())
    }
}

/// Converts a V1 tracer payload into its protobuf `idx` form.
///
/// Field semantics follow the V1 msgpack encoder: negative span starts are replaced by the
/// current time, negative durations become 0, and duplicate attribute keys keep the last value.
/// An unset chunk priority is encoded as `SamplerPriority::None` (-128), the "no priority"
/// value the agent's samplers recognize and the one the V07 agentless path sends.
///
/// # Errors
///
/// Returns [`StringTableOverflow`] if the payload has more than `u32::MAX` distinct strings.
pub(crate) fn to_idx_tracer_payload<T: TraceData>(
    payload: &TracerPayload<T>,
) -> Result<idx::TracerPayload, StringTableOverflow> {
    let mut table = StringTable::new();
    let container_id_ref = table.text(&payload.container_id)?;
    let language_name_ref = table.text(&payload.language_name)?;
    let language_version_ref = table.text(&payload.language_version)?;
    let tracer_version_ref = table.text(&payload.tracer_version)?;
    let runtime_id_ref = table.text(&payload.runtime_id)?;
    let env_ref = table.text(&payload.env)?;
    let hostname_ref = table.text(&payload.hostname)?;
    let app_version_ref = table.text(&payload.app_version)?;
    let attributes = convert_attributes(&payload.attributes, &mut table)?;
    let chunks = payload
        .chunks
        .iter()
        .map(|chunk| convert_chunk(chunk, &mut table))
        .collect::<Result<_, _>>()?;

    Ok(idx::TracerPayload {
        strings: table.strings,
        container_id_ref,
        language_name_ref,
        language_version_ref,
        tracer_version_ref,
        runtime_id_ref,
        env_ref,
        hostname_ref,
        app_version_ref,
        attributes,
        chunks,
        container_debug: None,
    })
}

fn convert_chunk<'a, T: TraceData>(
    chunk: &'a TraceChunk<T>,
    table: &mut StringTable<'a>,
) -> Result<idx::TraceChunk, StringTableOverflow> {
    Ok(idx::TraceChunk {
        // `SamplerPriority::None` is `i8::MIN`, so the discriminant cast to i32 is lossless.
        priority: chunk.priority.unwrap_or(SamplerPriority::None as i32),
        origin_ref: table.text(&chunk.origin)?,
        attributes: convert_attributes(&chunk.attributes, table)?,
        spans: chunk
            .spans
            .iter()
            .map(|span| convert_span(span, table))
            .collect::<Result<_, _>>()?,
        // Agent-owned, see `TraceChunk::dropped_trace`. Same as the v0.4 agentless path.
        dropped_trace: false,
        trace_id: chunk.trace_id.to_vec(),
        sampling_mechanism: chunk.sampling_mechanism.unwrap_or(0),
    })
}

fn convert_span<'a, T: TraceData>(
    span: &'a Span<T>,
    table: &mut StringTable<'a>,
) -> Result<idx::Span, StringTableOverflow> {
    Ok(idx::Span {
        service_ref: table.text(&span.service)?,
        name_ref: table.text(&span.name)?,
        resource_ref: table.text(&span.resource)?,
        span_id: span.span_id,
        parent_id: span.parent_id,
        start: normalize_span_start(span.start),
        duration: u64::try_from(span.duration).unwrap_or(0),
        error: span.error,
        attributes: convert_attributes(&span.attributes, table)?,
        type_ref: table.text(&span.r#type)?,
        links: span
            .span_links
            .iter()
            .map(|link| convert_link(link, table))
            .collect::<Result<_, _>>()?,
        events: span
            .span_events
            .iter()
            .map(|event| convert_event(event, table))
            .collect::<Result<_, _>>()?,
        env_ref: table.text(&span.env)?,
        version_ref: table.text(&span.version)?,
        component_ref: table.text(&span.component)?,
        kind: i32::from(convert_kind(span.span_kind)),
    })
}

fn convert_kind(kind: SpanKind) -> idx::SpanKind {
    match kind {
        SpanKind::Unspecified => idx::SpanKind::Unspecified,
        SpanKind::Internal => idx::SpanKind::Internal,
        SpanKind::Server => idx::SpanKind::Server,
        SpanKind::Client => idx::SpanKind::Client,
        SpanKind::Producer => idx::SpanKind::Producer,
        SpanKind::Consumer => idx::SpanKind::Consumer,
    }
}

fn convert_link<'a, T: TraceData>(
    link: &'a SpanLink<T>,
    table: &mut StringTable<'a>,
) -> Result<idx::SpanLink, StringTableOverflow> {
    Ok(idx::SpanLink {
        trace_id: link.trace_id.to_vec(),
        span_id: link.span_id,
        attributes: convert_attributes(&link.attributes, table)?,
        tracestate_ref: table.text(&link.tracestate)?,
        // Sent as-is, including the "flags set" sentinel bit, as the V1 msgpack encoder does.
        flags: link.flags,
    })
}

fn convert_event<'a, T: TraceData>(
    event: &'a SpanEvent<T>,
    table: &mut StringTable<'a>,
) -> Result<idx::SpanEvent, StringTableOverflow> {
    Ok(idx::SpanEvent {
        time: event.time_unix_nano,
        name_ref: table.text(&event.name)?,
        attributes: convert_attributes(&event.attributes, table)?,
    })
}

/// Converts an attribute map, keeping the last value of a duplicated key. Iterating in reverse
/// means shadowed values are skipped instead of converted and thrown away.
fn convert_attributes<'a, T: TraceData>(
    map: &'a VecMap<T::Text, AttributeValue<T>>,
    table: &mut StringTable<'a>,
) -> Result<HashMap<u32, idx::AnyValue>, StringTableOverflow> {
    let mut out = HashMap::with_capacity(map.len());
    for (k, v) in map.iter().rev() {
        if let Entry::Vacant(e) = out.entry(table.text(k)?) {
            e.insert(convert_value(v, table)?);
        }
    }
    Ok(out)
}

fn convert_value<'a, T: TraceData>(
    value: &'a AttributeValue<T>,
    table: &mut StringTable<'a>,
) -> Result<idx::AnyValue, StringTableOverflow> {
    use idx::any_value::Value;
    let value = match value {
        AttributeValue::String(s) => Value::StringValueRef(table.text(s)?),
        AttributeValue::Bool(b) => Value::BoolValue(*b),
        AttributeValue::Int(i) => Value::IntValue(*i),
        AttributeValue::Float(f) => Value::DoubleValue(*f),
        AttributeValue::Bytes(b) => Value::BytesValue(Borrow::<[u8]>::borrow(b).to_vec()),
        AttributeValue::Array(arr) => Value::ArrayValue(idx::ArrayValue {
            values: arr
                .iter()
                .map(|v| convert_value(v, table))
                .collect::<Result<_, _>>()?,
        }),
        AttributeValue::KeyValueList(map) => {
            // Last write wins, in insertion order. Doesn't allocate; quadratic only when the
            // map isn't marked deduped, but these maps are small.
            let mut key_values = Vec::with_capacity(map.len());
            for (k, v) in map.iter_unique() {
                key_values.push(idx::KeyValue {
                    key: table.text(k)?,
                    value: Some(convert_value(v, table)?),
                });
            }
            Value::KeyValueList(idx::KeyValueList { key_values })
        }
    };
    Ok(idx::AnyValue { value: Some(value) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span::BytesData;
    use idx::any_value::Value;
    use libdd_tinybytes::{Bytes, BytesString};
    use libdd_trace_model::SPAN_LINK_FLAGS_SET_SENTINEL;

    fn bs(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).unwrap()
    }

    fn resolve(p: &idx::TracerPayload, r: u32) -> &str {
        &p.strings[usize::try_from(r).unwrap()]
    }

    fn one_attr(key: &str, value: &str) -> VecMap<BytesString, AttributeValue<BytesData>> {
        let mut m = VecMap::new();
        m.insert(bs(key), AttributeValue::String(bs(value)));
        m
    }

    /// Looks up `key` in an idx attribute map and resolves a string value.
    fn str_attr<'p>(
        p: &'p idx::TracerPayload,
        attrs: &HashMap<u32, idx::AnyValue>,
        key: &str,
    ) -> &'p str {
        let (_, v) = attrs
            .iter()
            .find(|(k, _)| resolve(p, **k) == key)
            .unwrap_or_else(|| panic!("missing attribute {key}"));
        match v.value {
            Some(Value::StringValueRef(r)) => resolve(p, r),
            ref v => panic!("unexpected {v:?}"),
        }
    }

    /// Every string field has a distinct value, so a swapped ref is caught.
    fn sample_payload(with_duplicates: bool) -> TracerPayload<BytesData> {
        let mut span_attrs = VecMap::new();
        span_attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
        span_attrs.insert(bs("count"), AttributeValue::Int(1));
        if with_duplicates {
            span_attrs.insert(bs("count"), AttributeValue::Int(2));
        }
        span_attrs.insert(bs("ratio"), AttributeValue::Float(0.5));
        span_attrs.insert(bs("flag"), AttributeValue::Bool(true));
        span_attrs.insert(
            bs("raw"),
            AttributeValue::Bytes(Bytes::copy_from_slice(b"\xde\xad")),
        );
        span_attrs.insert(
            bs("list"),
            AttributeValue::Array(vec![
                AttributeValue::String(bs("item")),
                AttributeValue::Array(vec![AttributeValue::Int(3)]),
            ]),
        );
        let mut inner = VecMap::new();
        inner.insert(bs("deep"), AttributeValue::Int(9));
        let mut nested = VecMap::new();
        nested.insert(bs("nk"), AttributeValue::Bool(false));
        nested.insert(bs("inner"), AttributeValue::KeyValueList(inner));
        if with_duplicates {
            nested.insert(bs("nk"), AttributeValue::Bool(true));
        }
        span_attrs.insert(bs("kv"), AttributeValue::KeyValueList(nested));

        let span = Span {
            service: bs("svc"),
            name: bs("op"),
            resource: bs("res"),
            r#type: bs("web"),
            span_id: 2,
            parent_id: 1,
            start: 100,
            duration: 50,
            error: true,
            span_kind: SpanKind::Server,
            env: bs("span-env"),
            version: bs("span-version"),
            component: bs("span-component"),
            attributes: span_attrs,
            span_links: vec![SpanLink {
                trace_id: [7; 16],
                span_id: 9,
                tracestate: bs("dd=s:1"),
                flags: 1 | SPAN_LINK_FLAGS_SET_SENTINEL,
                attributes: one_attr("link.k", "link.v"),
            }]
            .into(),
            span_events: vec![SpanEvent {
                time_unix_nano: 42,
                name: bs("exception"),
                attributes: one_attr("event.k", "event.v"),
            }]
            .into(),
        };

        TracerPayload {
            container_id: bs("cid"),
            language_name: bs("php"),
            language_version: bs("8.3"),
            tracer_version: bs("1.0.0"),
            runtime_id: bs("rid"),
            env: bs("payload-env"),
            hostname: bs("host"),
            app_version: bs("app-1"),
            attributes: one_attr("payload.k", "payload.v"),
            chunks: vec![TraceChunk {
                trace_id: [1; 16],
                priority: Some(2),
                origin: bs("rum"),
                sampling_mechanism: Some(4),
                dropped_trace: true,
                attributes: one_attr("chunk.k", "chunk.v"),
                spans: vec![span],
            }],
        }
    }

    #[test]
    fn converts_every_field() {
        let p = to_idx_tracer_payload(&sample_payload(false)).unwrap();

        assert_eq!(p.strings[0], "");
        assert_eq!(resolve(&p, p.container_id_ref), "cid");
        assert_eq!(resolve(&p, p.language_name_ref), "php");
        assert_eq!(resolve(&p, p.language_version_ref), "8.3");
        assert_eq!(resolve(&p, p.tracer_version_ref), "1.0.0");
        assert_eq!(resolve(&p, p.runtime_id_ref), "rid");
        assert_eq!(resolve(&p, p.env_ref), "payload-env");
        assert_eq!(resolve(&p, p.hostname_ref), "host");
        assert_eq!(resolve(&p, p.app_version_ref), "app-1");
        assert_eq!(str_attr(&p, &p.attributes, "payload.k"), "payload.v");

        let chunk = &p.chunks[0];
        assert_eq!(chunk.trace_id, vec![1; 16]);
        assert_eq!(chunk.priority, 2);
        assert_eq!(chunk.sampling_mechanism, 4);
        // `dropped_trace` is agent-owned and never forwarded, even when set on the input.
        assert!(!chunk.dropped_trace);
        assert_eq!(resolve(&p, chunk.origin_ref), "rum");
        assert_eq!(str_attr(&p, &chunk.attributes, "chunk.k"), "chunk.v");

        let span = &chunk.spans[0];
        assert_eq!(resolve(&p, span.service_ref), "svc");
        assert_eq!(resolve(&p, span.name_ref), "op");
        assert_eq!(resolve(&p, span.resource_ref), "res");
        assert_eq!(resolve(&p, span.type_ref), "web");
        assert_eq!(resolve(&p, span.env_ref), "span-env");
        assert_eq!(resolve(&p, span.version_ref), "span-version");
        assert_eq!(resolve(&p, span.component_ref), "span-component");
        assert_eq!(
            (span.span_id, span.parent_id, span.start, span.duration),
            (2, 1, 100, 50)
        );
        assert!(span.error);
        assert_eq!(span.kind, i32::from(idx::SpanKind::Server));

        let link = &span.links[0];
        assert_eq!(link.trace_id, vec![7; 16]);
        assert_eq!(link.span_id, 9);
        assert_eq!(link.flags, 1 | SPAN_LINK_FLAGS_SET_SENTINEL);
        assert_eq!(resolve(&p, link.tracestate_ref), "dd=s:1");
        assert_eq!(str_attr(&p, &link.attributes, "link.k"), "link.v");

        let event = &span.events[0];
        assert_eq!(event.time, 42);
        assert_eq!(resolve(&p, event.name_ref), "exception");
        assert_eq!(str_attr(&p, &event.attributes, "event.k"), "event.v");
    }

    #[test]
    fn applies_defaults_for_unset_and_invalid_values() {
        let payload = TracerPayload::<BytesData> {
            chunks: vec![TraceChunk {
                spans: vec![Span {
                    start: -1,
                    duration: -5,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };
        let p = to_idx_tracer_payload(&payload).unwrap();

        assert_eq!(
            p.strings,
            vec![String::new()],
            "unset strings all use ref 0"
        );
        let chunk = &p.chunks[0];
        assert_eq!(chunk.priority, -128, "unset priority is PriorityNone");
        assert_eq!(chunk.sampling_mechanism, 0);
        let span = &chunk.spans[0];
        assert!(
            span.start > 0,
            "negative start is replaced by the current time"
        );
        assert_eq!(span.duration, 0, "negative duration is clamped to 0");
        assert_eq!(span.kind, i32::from(idx::SpanKind::Internal));
    }

    #[test]
    fn converts_attribute_values_last_write_wins() {
        let p = to_idx_tracer_payload(&sample_payload(true)).unwrap();
        let span = &p.chunks[0].spans[0];
        let attrs: HashMap<&str, &Value> = span
            .attributes
            .iter()
            .map(|(k, v)| (resolve(&p, *k), v.value.as_ref().unwrap()))
            .collect();

        assert_eq!(attrs.len(), 7, "duplicate key collapsed: {attrs:?}");
        assert_eq!(str_attr(&p, &span.attributes, "http.method"), "GET");
        assert_eq!(attrs["count"], &Value::IntValue(2));
        assert_eq!(attrs["ratio"], &Value::DoubleValue(0.5));
        assert_eq!(attrs["flag"], &Value::BoolValue(true));
        assert_eq!(attrs["raw"], &Value::BytesValue(vec![0xde, 0xad]));
        // The shadowed `count = 1` was never converted, so it added nothing to the table.
        assert!(!p.strings.iter().any(|s| s == "1"));

        let Value::ArrayValue(list) = attrs["list"] else {
            panic!("unexpected {:?}", attrs["list"]);
        };
        assert!(
            matches!(list.values[0].value, Some(Value::StringValueRef(r)) if resolve(&p, r) == "item")
        );
        assert_eq!(
            list.values[1].value,
            Some(Value::ArrayValue(idx::ArrayValue {
                values: vec![idx::AnyValue {
                    value: Some(Value::IntValue(3))
                }]
            }))
        );

        let Value::KeyValueList(kv) = attrs["kv"] else {
            panic!("unexpected {:?}", attrs["kv"]);
        };
        let keys: Vec<&str> = kv.key_values.iter().map(|e| resolve(&p, e.key)).collect();
        // Deduplicated, last write wins, insertion order of the surviving entries.
        assert_eq!(keys, ["inner", "nk"]);
        assert_eq!(
            kv.key_values[1].value.as_ref().unwrap().value,
            Some(Value::BoolValue(true))
        );
        let Some(Value::KeyValueList(inner)) = &kv.key_values[0].value.as_ref().unwrap().value
        else {
            panic!("expected nested key-value list");
        };
        assert_eq!(resolve(&p, inner.key_values[0].key), "deep");
        assert_eq!(
            inner.key_values[0].value.as_ref().unwrap().value,
            Some(Value::IntValue(9))
        );
    }

    /// The protobuf path must agree with what the msgpack path delivers: converting a payload
    /// directly gives the same result as converting it after a V1 msgpack round trip.
    #[test]
    fn matches_msgpack_round_trip() {
        let original = sample_payload(false);
        let encoded = crate::msgpack_encoder::v1::to_vec_from_v1(&original);
        let (decoded, _) = crate::msgpack_decoder::v1::from_bytes(Bytes::from(encoded)).unwrap();

        // Interning order depends on attribute iteration order, which the round trip may
        // change, so compare with both string tables sorted.
        assert_eq!(
            canonicalize(to_idx_tracer_payload(&original).unwrap()),
            canonicalize(to_idx_tracer_payload(&decoded).unwrap())
        );
    }

    /// Sorts the string table and rewrites every ref to match, so payloads that differ only in
    /// interning order compare equal.
    fn canonicalize(mut p: idx::TracerPayload) -> idx::TracerPayload {
        let mut sorted = p.strings.clone();
        sorted.sort();
        let new_ref: HashMap<u32, u32> = p
            .strings
            .iter()
            .enumerate()
            .map(|(old, s)| {
                let new = sorted.binary_search(s).unwrap();
                (u32::try_from(old).unwrap(), u32::try_from(new).unwrap())
            })
            .collect();
        let m = |r: &mut u32| *r = new_ref[r];
        fn attrs(a: &mut HashMap<u32, idx::AnyValue>, m: &impl Fn(&mut u32)) {
            *a = std::mem::take(a)
                .into_iter()
                .map(|(mut k, mut v)| {
                    m(&mut k);
                    value(&mut v, m);
                    (k, v)
                })
                .collect();
        }
        fn value(v: &mut idx::AnyValue, m: &impl Fn(&mut u32)) {
            match &mut v.value {
                Some(Value::StringValueRef(r)) => m(r),
                Some(Value::ArrayValue(a)) => a.values.iter_mut().for_each(|v| value(v, m)),
                Some(Value::KeyValueList(kv)) => {
                    for e in &mut kv.key_values {
                        m(&mut e.key);
                        if let Some(v) = &mut e.value {
                            value(v, m);
                        }
                    }
                    // A key-value list is a map: the encoder's on-the-fly dedup of a non-deduped
                    // `VecMap` emits it in hash order, so compare it order-insensitively.
                    kv.key_values.sort_by_key(|e| e.key);
                }
                _ => {}
            }
        }
        for r in [
            &mut p.container_id_ref,
            &mut p.language_name_ref,
            &mut p.language_version_ref,
            &mut p.tracer_version_ref,
            &mut p.runtime_id_ref,
            &mut p.env_ref,
            &mut p.hostname_ref,
            &mut p.app_version_ref,
        ] {
            m(r);
        }
        attrs(&mut p.attributes, &m);
        for c in &mut p.chunks {
            m(&mut c.origin_ref);
            attrs(&mut c.attributes, &m);
            for s in &mut c.spans {
                for r in [
                    &mut s.service_ref,
                    &mut s.name_ref,
                    &mut s.resource_ref,
                    &mut s.type_ref,
                    &mut s.env_ref,
                    &mut s.version_ref,
                    &mut s.component_ref,
                ] {
                    m(r);
                }
                attrs(&mut s.attributes, &m);
                for l in &mut s.links {
                    m(&mut l.tracestate_ref);
                    attrs(&mut l.attributes, &m);
                }
                for e in &mut s.events {
                    m(&mut e.name_ref);
                    attrs(&mut e.attributes, &m);
                }
            }
        }
        p.strings = sorted;
        p
    }
}
