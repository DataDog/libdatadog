// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::encode_payload;
use crate::span::v04::{AttributeAnyValue, AttributeArrayValue, Span, SpanEvent, SpanLink, VecMap};
use crate::span::BytesData;
use crate::tracer_metadata::TracerMetadata;
use libdd_tinybytes::{Bytes, BytesString};
use serde_json::Value;
use std::collections::HashMap;

fn bs(s: &'static str) -> BytesString {
    BytesString::from_static(s)
}

fn base_metadata() -> TracerMetadata {
    TracerMetadata {
        hostname: "host-1".to_string(),
        env: "prod".to_string(),
        runtime_id: "rt-1".to_string(),
        service: "svc".to_string(),
        tracer_version: "1.2.3".to_string(),
        language: "nodejs".to_string(),
        language_version: "v20.11.0".to_string(),
        ..Default::default()
    }
}

fn json_from_bytes(b: &[u8]) -> Value {
    serde_json::from_slice(b).expect("payload must be valid JSON")
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn top_level_payload_shape_and_metadata() {
    let span: Span<BytesData> = Span {
        service: bs("svc"),
        name: bs("op"),
        resource: bs("res"),
        trace_id: 0xdeadbeef_u128,
        span_id: 1,
        parent_id: 0,
        start: 2_500_000_000,
        duration: 1_000_000,
        metrics: VecMap::from_iter([("_top_level".into(), 1.0)]),
        ..Default::default()
    };
    let bytes = encode_payload(&[vec![span]], &base_metadata()).unwrap();
    let v = json_from_bytes(&bytes);

    assert!(v.is_object());
    let traces = v.get("traces").unwrap().as_array().unwrap();
    assert_eq!(traces.len(), 1);

    let t = &traces[0];
    assert_eq!(t["hostname"], "host-1");
    assert_eq!(t["env"], "prod");
    assert_eq!(t["languageName"], "nodejs");
    assert_eq!(t["languageVersion"], "v20.11.0");
    assert_eq!(t["tracerVersion"], "1.2.3");
    assert_eq!(t["runtimeID"], "rt-1");

    let spans = t["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 1);
    let s = &spans[0];
    assert_eq!(s["trace_id"], "00000000deadbeef");
    assert_eq!(s["span_id"], "0000000000000001");
    assert_eq!(s["parent_id"], "0000000000000000");
    assert_eq!(s["name"], "op");
    assert_eq!(s["resource"], "res");
    assert_eq!(s["service"], "svc");
    assert_eq!(s["error"], 0);
    assert_eq!(s["start"], 2_500_000_000_i64);
    assert_eq!(s["duration"], 1_000_000);

    // Root span gets `_trace_root`, top-level (no parent), and first span gets
    // compute_stats.
    let metrics = s["metrics"].as_object().unwrap();
    assert_eq!(metrics["_trace_root"], 1);
    assert_eq!(metrics["_top_level"], 1);
    let meta = s["meta"].as_object().unwrap();
    assert_eq!(meta["_dd.compute_stats"], "1");
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn resource_defaults_to_name_when_empty() {
    let span: Span<BytesData> = Span {
        service: bs("svc"),
        name: bs("op"),
        // resource omitted (default empty)
        trace_id: 1,
        span_id: 1,
        parent_id: 0,
        start: 0,
        duration: 1,
        ..Default::default()
    };
    let bytes = encode_payload(&[vec![span]], &base_metadata()).unwrap();
    let v = json_from_bytes(&bytes);
    let s = &v["traces"][0]["spans"][0];
    assert_eq!(s["resource"], "op");
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn keeps_existing_dd_p_tid_in_meta() {
    // When the tracer already supplies `_dd.p.tid`, the encoder must pass it
    // through unchanged and must NOT auto-inject a second value.
    let mut span: Span<BytesData> = Span {
        service: bs("svc"),
        name: bs("op"),
        // 64-bit trace_id — upper 64 bits are zero, so no auto-inject would fire.
        trace_id: 0x1234_5678_9abc_def0_u128,
        span_id: 2,
        parent_id: 0,
        start: 0,
        duration: 1,
        ..Default::default()
    };
    span.meta.insert(bs("_dd.p.tid"), bs("5b8efff798038103"));
    span.meta.insert(bs("some.tag"), bs("kept"));

    let v = json_from_bytes(&encode_payload(&[vec![span]], &base_metadata()).unwrap());
    let s = &v["traces"][0]["spans"][0];
    // Only the low 64 bits appear in `trace_id`.
    assert_eq!(s["trace_id"], "123456789abcdef0");
    let meta = s["meta"].as_object().unwrap();
    // The tracer-supplied `_dd.p.tid` is preserved as-is.
    assert_eq!(meta["_dd.p.tid"], "5b8efff798038103");
    assert_eq!(meta["some.tag"], "kept");
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn span_links_serialised_into_meta_as_json_string() {
    // Span links are JSON-stringified and stored in meta["_dd.span_links"];
    // no top-level `span_links` field is emitted.
    let link = SpanLink::<BytesData> {
        trace_id: 0x9abc_def0_1234_5678,
        trace_id_high: 0x0011_2233_4455_6677,
        span_id: 0xfeed_face_dead_beef,
        attributes: HashMap::from([(bs("link.name"), bs("scheduled_by"))]),
        flags: 1,
        tracestate: bs("dd=s:1"),
    };
    let span: Span<BytesData> = Span {
        service: bs("svc"),
        name: bs("op"),
        trace_id: 1,
        span_id: 1,
        parent_id: 0,
        start: 0,
        duration: 1,
        span_links: vec![link],
        ..Default::default()
    };
    let v = json_from_bytes(&encode_payload(&[vec![span]], &base_metadata()).unwrap());
    let s = &v["traces"][0]["spans"][0];
    // No top-level `span_links` field.
    assert!(s.get("span_links").is_none_or(|v| v.is_null()));
    // Links are stored as a JSON string in meta["_dd.span_links"].
    let raw = s["meta"]["_dd.span_links"]
        .as_str()
        .expect("meta[_dd.span_links] must be a string");
    let links: serde_json::Value = serde_json::from_str(raw).expect("must be valid JSON");
    let link_obj = &links[0];
    // 32-char lowercase hex full 128-bit trace ID.
    let expected_trace_id = format!(
        "{:032x}",
        ((0x0011_2233_4455_6677u128) << 64) | 0x9abc_def0_1234_5678_u128
    );
    assert_eq!(link_obj["trace_id"], expected_trace_id);
    assert_eq!(expected_trace_id.len(), 32);
    assert_eq!(link_obj["span_id"], "feedfacedeadbeef");
    assert_eq!(link_obj["attributes"]["link.name"], "scheduled_by");
    assert_eq!(link_obj["flags"], 1);
    assert_eq!(link_obj["tracestate"], "dd=s:1");
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn span_events_serialised_into_meta_as_json_string() {
    // Span events are JSON-stringified and stored in meta["events"];
    // no top-level `span_events` field is emitted.
    let event = SpanEvent::<BytesData> {
        time_unix_nano: 1_700_000_000_000_000_000,
        name: bs("exception"),
        attributes: HashMap::from([(
            bs("exception.message"),
            AttributeAnyValue::SingleValue(AttributeArrayValue::String(bs("timeout"))),
        )]),
    };
    let span: Span<BytesData> = Span {
        service: bs("svc"),
        name: bs("op"),
        trace_id: 1,
        span_id: 1,
        parent_id: 0,
        start: 0,
        duration: 1,
        span_events: vec![event],
        ..Default::default()
    };
    let v = json_from_bytes(&encode_payload(&[vec![span]], &base_metadata()).unwrap());
    let s = &v["traces"][0]["spans"][0];
    // No top-level `span_events` field.
    assert!(s.get("span_events").is_none_or(|v| v.is_null()));
    // Events are stored as a JSON string in meta["events"].
    let raw = s["meta"]["events"]
        .as_str()
        .expect("meta[events] must be a string");
    let events: serde_json::Value = serde_json::from_str(raw).expect("must be valid JSON");
    let evt = &events[0];
    assert_eq!(evt["name"], "exception");
    assert_eq!(evt["time_unix_nano"], 1_700_000_000_000_000_000_u64);
    assert_eq!(evt["attributes"]["exception.message"], "timeout");
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn top_level_only_for_first_span_when_parent_in_other_service() {
    // Trace with two spans, parent in different service.
    let parent: Span<BytesData> = Span {
        service: bs("svc-a"),
        name: bs("op"),
        trace_id: 1,
        span_id: 10,
        parent_id: 0,
        start: 0,
        duration: 1,
        metrics: VecMap::from_iter([("_top_level".into(), 1.0)]),
        ..Default::default()
    };
    let child_same_service: Span<BytesData> = Span {
        service: bs("svc-a"),
        name: bs("op"),
        trace_id: 1,
        span_id: 11,
        parent_id: 10,
        start: 0,
        duration: 1,
        ..Default::default()
    };
    let child_other_service: Span<BytesData> = Span {
        service: bs("svc-b"),
        name: bs("op"),
        trace_id: 1,
        span_id: 12,
        parent_id: 10,
        start: 0,
        duration: 1,
        metrics: VecMap::from_iter([("_top_level".into(), 1.0)]),
        ..Default::default()
    };
    let v = json_from_bytes(
        &encode_payload(
            &[vec![parent, child_same_service, child_other_service]],
            &base_metadata(),
        )
        .unwrap(),
    );
    let spans = v["traces"][0]["spans"].as_array().unwrap();
    // Parent (root) is top-level + trace_root.
    assert_eq!(spans[0]["metrics"]["_top_level"], 1);
    assert_eq!(spans[0]["metrics"]["_trace_root"], 1);
    // Child in same service: NOT top-level.
    assert!(spans[1]["metrics"].get("_top_level").is_none());
    assert!(spans[1]["metrics"].get("_trace_root").is_none());
    // Child in other service: top-level (parent service differs).
    assert_eq!(spans[2]["metrics"]["_top_level"], 1);
    assert!(spans[2]["metrics"].get("_trace_root").is_none());
    // Only the first span in the chunk gets _dd.compute_stats.
    assert_eq!(spans[0]["meta"]["_dd.compute_stats"], "1");
    assert!(spans[1]["meta"].get("_dd.compute_stats").is_none());
    assert!(spans[2]["meta"].get("_dd.compute_stats").is_none());
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn meta_struct_msgpack_values_are_inlined_as_json_objects() {
    // `meta_struct` values are msgpack-encoded objects in memory. On the
    // agentless wire they must appear as real JSON objects (not byte arrays).
    #[derive(serde::Serialize)]
    struct AppSec<'a> {
        rule_id: &'a str,
        nested: Nested<'a>,
        list: Vec<i32>,
    }
    #[derive(serde::Serialize)]
    struct Nested<'a> {
        kind: &'a str,
        count: u32,
    }
    let payload = rmp_serde::to_vec_named(&AppSec {
        rule_id: "crs-913-110",
        nested: Nested {
            kind: "sqli",
            count: 3,
        },
        list: vec![1, 2, 3],
    })
    .unwrap();

    let mut span: Span<BytesData> = Span {
        service: bs("svc"),
        name: bs("op"),
        trace_id: 1,
        span_id: 1,
        parent_id: 0,
        start: 0,
        duration: 1,
        ..Default::default()
    };
    span.meta_struct
        .insert(bs("_dd.appsec.json"), Bytes::from(payload));

    let encoded = encode_payload(&[vec![span]], &base_metadata()).unwrap();
    let v = json_from_bytes(&encoded);
    let s = &v["traces"][0]["spans"][0];
    let ms = s["meta_struct"]
        .as_object()
        .expect("meta_struct must be a JSON object");

    // Well-formed entry is inlined as a JSON object.
    let inlined = ms
        .get("_dd.appsec.json")
        .expect("valid entry must be present")
        .as_object()
        .expect("valid entry must be a JSON object, not a byte array");
    assert_eq!(inlined["rule_id"], "crs-913-110");
    assert_eq!(inlined["nested"]["kind"], "sqli");
    assert_eq!(inlined["nested"]["count"], 3);
    assert_eq!(inlined["list"], serde_json::json!([1, 2, 3]));
}

#[cfg_attr(miri, ignore)] // serde_json/rmp_serde overhead is prohibitively slow under Miri
#[test]
fn meta_struct_field_omitted_when_empty() {
    // No meta_struct entries -> the field is not emitted at all.
    let span: Span<BytesData> = Span {
        service: bs("svc"),
        name: bs("op"),
        trace_id: 1,
        span_id: 1,
        parent_id: 0,
        start: 0,
        duration: 1,
        ..Default::default()
    };
    let v = json_from_bytes(&encode_payload(&[vec![span]], &base_metadata()).unwrap());
    let s = &v["traces"][0]["spans"][0];
    assert!(s.get("meta_struct").is_none());
}
