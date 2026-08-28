// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::encode_payload_from_v1;
use crate::span::v1::{
    AttributeValue, AttributeValueBytes, SpanBytes, SpanEventBytes, SpanKind, SpanLinkBytes,
    TraceChunkBytes,
};
use crate::span::vec_map::VecMap;
use crate::tracer_metadata::TracerMetadata;
use libdd_tinybytes::BytesString;
use serde_json::Value;

fn bs(s: &str) -> BytesString {
    BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
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

fn minimal_chunk(trace_id: [u8; 16], span: SpanBytes) -> TraceChunkBytes {
    TraceChunkBytes {
        trace_id,
        spans: vec![span],
        ..Default::default()
    }
}

fn minimal_span() -> SpanBytes {
    SpanBytes {
        service: bs("svc"),
        name: bs("op"),
        resource: bs("res"),
        span_id: 1,
        start: 1_000,
        duration: 500,
        ..Default::default()
    }
}

fn encode_first_span(chunks: &[TraceChunkBytes]) -> Value {
    let bytes = encode_payload_from_v1(chunks, &base_metadata()).expect("encode ok");
    let v = json_from_bytes(&bytes);
    v["traces"][0]["spans"][0].clone()
}

#[cfg_attr(miri, ignore)] // serde_json overhead is prohibitively slow under Miri
#[test]
fn top_level_payload_shape_and_metadata() {
    let chunk = minimal_chunk([0u8; 16], minimal_span());
    let bytes = encode_payload_from_v1(&[chunk], &base_metadata()).unwrap();
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
    assert_eq!(s["trace_id"], "0000000000000000");
    assert_eq!(s["span_id"], "0000000000000001");
    assert_eq!(s["parent_id"], "0000000000000000");
    assert_eq!(s["name"], "op");
    assert_eq!(s["resource"], "res");
    assert_eq!(s["service"], "svc");
    assert_eq!(s["error"], 0);
    assert_eq!(s["start"], 1_000);
    assert_eq!(s["duration"], 500);

    // Root span (no parent) gets `_trace_root`; first span of the chunk gets `_dd.compute_stats`.
    assert_eq!(s["metrics"]["_trace_root"], 1);
    assert_eq!(s["meta"]["_dd.compute_stats"], "1");
}

#[cfg_attr(miri, ignore)]
#[test]
fn resource_defaults_to_name_when_empty() {
    let span = SpanBytes {
        service: bs("svc"),
        name: bs("op"),
        // resource omitted (default empty)
        span_id: 1,
        start: 0,
        duration: 1,
        ..Default::default()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["resource"], "op");
}

#[cfg_attr(miri, ignore)]
#[test]
fn keeps_existing_dd_p_tid_in_meta() {
    // When the tracer already supplies `_dd.p.tid`, the encoder must pass it through unchanged
    // and must NOT auto-inject a second value.
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(
        bs("_dd.p.tid"),
        AttributeValue::String(bs("5b8efff798038103")),
    );
    attrs.insert(bs("some.tag"), AttributeValue::String(bs("kept")));
    let mut tid = [0u8; 16];
    tid[8..].copy_from_slice(&0x1234_5678_9abc_def0_u64.to_be_bytes());
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk(tid, span)]);
    assert_eq!(out["trace_id"], "123456789abcdef0");
    assert_eq!(out["meta"]["_dd.p.tid"], "5b8efff798038103");
    assert_eq!(out["meta"]["some.tag"], "kept");
}

#[cfg_attr(miri, ignore)]
#[test]
fn p_tid_is_auto_injected_from_trace_id_high_bits_when_absent() {
    let mut tid = [0u8; 16];
    tid[..8].copy_from_slice(&0xDEAD_BEEF_CAFE_BABE_u64.to_be_bytes());
    tid[8..].copy_from_slice(&0x0123_4567_89AB_CDEF_u64.to_be_bytes());
    let out = encode_first_span(&[minimal_chunk(tid, minimal_span())]);
    assert_eq!(out["trace_id"], "0123456789abcdef");
    assert_eq!(out["meta"]["_dd.p.tid"], "deadbeefcafebabe");
}

#[cfg_attr(miri, ignore)]
#[test]
fn promoted_fields_are_copied_into_meta() {
    let span = SpanBytes {
        env: bs("prod"),
        version: bs("1.2.3"),
        component: bs("http"),
        span_kind: SpanKind::Server,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["meta"]["env"], "prod");
    assert_eq!(out["meta"]["version"], "1.2.3");
    assert_eq!(out["meta"]["component"], "http");
    assert_eq!(out["meta"]["span.kind"], "server");
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_kind_internal_is_not_emitted() {
    let out = encode_first_span(&[minimal_chunk([0u8; 16], minimal_span())]);
    assert!(out["meta"].get("span.kind").is_none());
}

#[cfg_attr(miri, ignore)]
#[test]
fn attribute_sharing_a_promoted_key_name_is_dropped_in_favor_of_the_dedicated_field() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("env"), AttributeValue::String(bs("staging")));
    attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
    let span = SpanBytes {
        env: bs("prod"),
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["meta"]["env"], "prod");
    assert_eq!(out["meta"]["http.method"], "GET");
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_attributes_override_chunk_attributes_of_the_same_key() {
    let mut chunk_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    chunk_attrs.insert(bs("k"), AttributeValue::String(bs("from-chunk")));
    let mut span_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    span_attrs.insert(bs("k"), AttributeValue::String(bs("from-span")));

    let chunk = TraceChunkBytes {
        attributes: chunk_attrs,
        ..minimal_chunk(
            [0u8; 16],
            SpanBytes {
                attributes: span_attrs,
                ..minimal_span()
            },
        )
    };
    let out = encode_first_span(&[chunk]);
    assert_eq!(out["meta"]["k"], "from-span");
}

#[cfg_attr(miri, ignore)]
#[test]
fn chunk_attributes_propagate_to_every_span() {
    let mut chunk_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    chunk_attrs.insert(bs("shared"), AttributeValue::String(bs("chunk-value")));
    let chunk = TraceChunkBytes {
        attributes: chunk_attrs,
        spans: vec![
            SpanBytes {
                span_id: 1,
                ..minimal_span()
            },
            SpanBytes {
                span_id: 2,
                ..minimal_span()
            },
        ],
        ..Default::default()
    };
    let bytes = encode_payload_from_v1(&[chunk], &base_metadata()).unwrap();
    let v = json_from_bytes(&bytes);
    let spans = v["traces"][0]["spans"].as_array().unwrap();
    assert_eq!(spans.len(), 2);
    assert_eq!(spans[0]["meta"]["shared"], "chunk-value");
    assert_eq!(spans[1]["meta"]["shared"], "chunk-value");
    // Only the first span in the chunk gets _dd.compute_stats.
    assert_eq!(spans[0]["meta"]["_dd.compute_stats"], "1");
    assert!(spans[1]["meta"].get("_dd.compute_stats").is_none());
}

#[cfg_attr(miri, ignore)]
#[test]
fn trace_id_only_carries_low_64_bits() {
    let mut tid = [0u8; 16];
    tid[..8].copy_from_slice(&0xDEAD_BEEF_CAFE_BABE_u64.to_be_bytes());
    tid[8..].copy_from_slice(&0x0123_4567_89AB_CDEF_u64.to_be_bytes());
    let out = encode_first_span(&[minimal_chunk(tid, minimal_span())]);
    assert_eq!(out["trace_id"], "0123456789abcdef");
}

#[cfg_attr(miri, ignore)]
#[test]
fn error_true_emits_one_false_emits_zero() {
    let err_span = SpanBytes {
        error: true,
        ..minimal_span()
    };
    assert_eq!(
        encode_first_span(&[minimal_chunk([0u8; 16], err_span)])["error"],
        1
    );
    assert_eq!(
        encode_first_span(&[minimal_chunk([0u8; 16], minimal_span())])["error"],
        0
    );
}

#[cfg_attr(miri, ignore)]
#[test]
fn string_attribute_is_routed_to_meta() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["meta"]["http.method"], "GET");
}

#[cfg_attr(miri, ignore)]
#[test]
fn bool_attribute_is_stringified_in_meta() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("retry"), AttributeValue::Bool(true));
    attrs.insert(bs("cached"), AttributeValue::Bool(false));
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["meta"]["retry"], "true");
    assert_eq!(out["meta"]["cached"], "false");
}

#[cfg_attr(miri, ignore)]
#[test]
fn float_and_int_attributes_route_to_metrics_as_f64() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("duration_ms"), AttributeValue::Float(12.5));
    attrs.insert(bs("status"), AttributeValue::Int(200));
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["metrics"]["duration_ms"], 12.5);
    assert_eq!(out["metrics"]["status"], 200.0);
}

#[cfg_attr(miri, ignore)]
#[test]
fn top_level_metric_is_serialized_as_integer_not_float() {
    // `_top_level` must render as `1`, not `1.0`, on the wire.
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("_top_level"), AttributeValue::Int(1));
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let bytes = encode_payload_from_v1(&[minimal_chunk([0u8; 16], span)], &base_metadata())
        .expect("encode ok");
    let text = String::from_utf8(bytes).expect("utf8");
    assert!(
        text.contains("\"_top_level\":1") || text.contains("\"_top_level\": 1"),
        "expected integer rendering of _top_level, got: {text}"
    );
    assert!(!text.contains("\"_top_level\":1.0"));
}

#[cfg_attr(miri, ignore)]
#[test]
fn bytes_attribute_is_transcoded_into_meta_struct() {
    #[derive(serde::Serialize)]
    struct AppSec<'a> {
        rule_id: &'a str,
    }
    let payload = rmp_serde::to_vec_named(&AppSec {
        rule_id: "crs-913-110",
    })
    .unwrap();

    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(
        bs("_dd.appsec.json"),
        AttributeValue::Bytes(libdd_tinybytes::Bytes::from(payload)),
    );
    attrs.insert(bs("kept"), AttributeValue::String(bs("yes")));
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    // `Bytes` attributes are routed to `meta_struct`, not `meta`.
    assert!(out["meta"].get("_dd.appsec.json").is_none());
    assert_eq!(out["meta"]["kept"], "yes");
    let ms = out["meta_struct"]
        .as_object()
        .expect("meta_struct must be present and a JSON object");
    assert_eq!(ms["_dd.appsec.json"]["rule_id"], "crs-913-110");
}

#[cfg_attr(miri, ignore)]
#[test]
fn meta_struct_field_omitted_when_no_bytes_attributes() {
    let out = encode_first_span(&[minimal_chunk([0u8; 16], minimal_span())]);
    assert!(out.get("meta_struct").is_none());
}

#[cfg_attr(miri, ignore)]
#[test]
fn nested_bytes_attribute_in_list_is_routed_to_meta_struct() {
    let payload = rmp_serde::to_vec(&42u32).unwrap();

    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(
        bs("items"),
        AttributeValue::List(vec![
            AttributeValue::String(bs("first")),
            AttributeValue::Bytes(libdd_tinybytes::Bytes::from(payload)),
        ]),
    );
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);

    assert_eq!(out["meta"]["items.0"], "first");
    // Previously silently dropped: a `Bytes` value nested inside a `List` has no flattened
    // string/numeric form, so it must be routed to `meta_struct` like a top-level `Bytes` would.
    let ms = out["meta_struct"]
        .as_object()
        .expect("meta_struct must be present and a JSON object");
    assert_eq!(ms["items.1"], 42);
}

#[cfg_attr(miri, ignore)]
#[test]
fn non_finite_metric_attributes_are_dropped() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("nan_metric"), AttributeValue::Float(f64::NAN));
    attrs.insert(bs("inf_metric"), AttributeValue::Float(f64::INFINITY));
    attrs.insert(
        bs("neg_inf_metric"),
        AttributeValue::Float(f64::NEG_INFINITY),
    );
    attrs.insert(bs("finite_metric"), AttributeValue::Float(1.5));
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);

    // serde_json can't represent NaN/Inf; encoding must not error and these keys must be absent
    // rather than serialized as `null` or aborting the whole payload.
    let metrics = out["metrics"]
        .as_object()
        .expect("metrics must be present and a JSON object");
    assert!(!metrics.contains_key("nan_metric"));
    assert!(!metrics.contains_key("inf_metric"));
    assert!(!metrics.contains_key("neg_inf_metric"));
    assert_eq!(out["metrics"]["finite_metric"], 1.5);
}

#[cfg_attr(miri, ignore)]
#[test]
fn nested_key_value_attribute_is_flattened_with_dotted_keys() {
    let mut inner: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    inner.insert(bs("b"), AttributeValue::String(bs("v")));
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("a"), AttributeValue::KeyValue(inner));
    let span = SpanBytes {
        attributes: attrs,
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["meta"]["a.b"], "v");
}

#[cfg_attr(miri, ignore)]
#[test]
fn chunk_origin_priority_and_sampling_mechanism_are_mapped() {
    let chunk = TraceChunkBytes {
        origin: bs("rum"),
        priority: Some(1),
        sampling_mechanism: Some(3),
        ..minimal_chunk([0u8; 16], minimal_span())
    };
    let out = encode_first_span(&[chunk]);
    assert_eq!(out["meta"]["_dd.origin"], "rum");
    assert_eq!(out["meta"]["_dd.p.dm"], "-3");
    assert_eq!(out["metrics"]["_sampling_priority_v1"], 1.0);
}

#[cfg_attr(miri, ignore)]
#[test]
fn dropped_trace_forces_user_reject_priority() {
    let chunk = TraceChunkBytes {
        dropped_trace: true,
        priority: Some(1),
        ..minimal_chunk([0u8; 16], minimal_span())
    };
    let out = encode_first_span(&[chunk]);
    assert_eq!(out["metrics"]["_sampling_priority_v1"], -1.0);
}

#[cfg_attr(miri, ignore)]
#[test]
fn dropped_trace_keeps_existing_negative_priority() {
    let chunk = TraceChunkBytes {
        dropped_trace: true,
        priority: Some(-2),
        ..minimal_chunk([0u8; 16], minimal_span())
    };
    let out = encode_first_span(&[chunk]);
    assert_eq!(out["metrics"]["_sampling_priority_v1"], -2.0);
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_links_serialised_into_meta_as_json_string() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("link.name"), AttributeValue::String(bs("scheduled_by")));
    let mut tid = [0u8; 16];
    tid[..8].copy_from_slice(&0x0011_2233_4455_6677_u64.to_be_bytes());
    tid[8..].copy_from_slice(&0x9abc_def0_1234_5678_u64.to_be_bytes());
    let span = SpanBytes {
        span_links: thin_vec::thin_vec![SpanLinkBytes {
            trace_id: tid,
            span_id: 0xfeed_face_dead_beef,
            attributes: attrs,
            flags: 1,
            tracestate: bs("dd=s:1"),
        }],
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert!(out.get("span_links").is_none());
    let raw = out["meta"]["_dd.span_links"]
        .as_str()
        .expect("meta[_dd.span_links] must be a string");
    let links: Value = serde_json::from_str(raw).expect("must be valid JSON");
    let link_obj = &links[0];
    assert_eq!(link_obj["trace_id"], "00112233445566779abcdef012345678");
    assert_eq!(link_obj["span_id"], "feedfacedeadbeef");
    assert_eq!(link_obj["attributes"]["link.name"], "scheduled_by");
    assert_eq!(link_obj["flags"], 1);
    assert_eq!(link_obj["tracestate"], "dd=s:1");
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_link_attributes_are_filtered_to_string_and_bool() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(bs("kept"), AttributeValue::String(bs("v")));
    attrs.insert(bs("kept_bool"), AttributeValue::Bool(true));
    attrs.insert(bs("dropped"), AttributeValue::Int(1));
    let span = SpanBytes {
        span_links: thin_vec::thin_vec![SpanLinkBytes {
            trace_id: [0u8; 16],
            span_id: 7,
            attributes: attrs,
            ..Default::default()
        }],
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    let raw = out["meta"]["_dd.span_links"].as_str().unwrap();
    let links: Value = serde_json::from_str(raw).unwrap();
    let link = &links[0];
    assert_eq!(link["attributes"]["kept"], "v");
    assert_eq!(link["attributes"]["kept_bool"], "true");
    assert!(link["attributes"].get("dropped").is_none());
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_link_flags_sentinel_bit_masked() {
    let span = SpanBytes {
        span_links: thin_vec::thin_vec![SpanLinkBytes {
            trace_id: [0u8; 16],
            span_id: 7,
            flags: crate::span::SPAN_LINK_FLAGS_SET_SENTINEL | 0b1,
            ..Default::default()
        }],
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    let raw = out["meta"]["_dd.span_links"].as_str().unwrap();
    let links: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(links[0]["flags"], 1);
}

#[cfg_attr(miri, ignore)]
#[test]
fn existing_span_links_meta_is_kept_and_not_overwritten() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(
        bs("_dd.span_links"),
        AttributeValue::String(bs("[{\"already\":\"there\"}]")),
    );
    let span = SpanBytes {
        attributes: attrs,
        span_links: thin_vec::thin_vec![SpanLinkBytes {
            trace_id: [0u8; 16],
            span_id: 1,
            ..Default::default()
        }],
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert_eq!(out["meta"]["_dd.span_links"], "[{\"already\":\"there\"}]");
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_events_serialised_into_meta_as_json_string() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(
        bs("exception.message"),
        AttributeValue::String(bs("timeout")),
    );
    let span = SpanBytes {
        span_events: thin_vec::thin_vec![SpanEventBytes {
            time_unix_nano: 1_700_000_000_000_000_000,
            name: bs("exception"),
            attributes: attrs,
        }],
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    assert!(out.get("span_events").is_none());
    let raw = out["meta"]["events"]
        .as_str()
        .expect("meta[events] must be a string");
    let events: Value = serde_json::from_str(raw).expect("must be valid JSON");
    let evt = &events[0];
    assert_eq!(evt["name"], "exception");
    assert_eq!(evt["time_unix_nano"], 1_700_000_000_000_000_000_u64);
    assert_eq!(
        evt["attributes"]["exception.message"],
        serde_json::json!({"type": 0, "string_value": "timeout"})
    );
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_event_list_attribute_becomes_array_value_of_scalars() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(
        bs("list"),
        AttributeValue::List(vec![
            AttributeValue::String(bs("a")),
            AttributeValue::Int(2),
        ]),
    );
    let span = SpanBytes {
        span_events: thin_vec::thin_vec![SpanEventBytes {
            time_unix_nano: 1,
            name: bs("evt"),
            attributes: attrs,
        }],
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    let raw = out["meta"]["events"].as_str().unwrap();
    let events: Value = serde_json::from_str(raw).unwrap();
    let value = &events[0]["attributes"]["list"];
    assert_eq!(value["type"], 4);
    assert_eq!(
        value["array_value"]["values"],
        serde_json::json!([
            {"type": 0, "string_value": "a"},
            {"type": 2, "int_value": 2}
        ])
    );
}

#[cfg_attr(miri, ignore)]
#[test]
fn span_event_bytes_attribute_is_dropped() {
    let mut attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
    attrs.insert(
        bs("blob"),
        AttributeValue::Bytes(libdd_tinybytes::Bytes::copy_from_slice(b"\xde\xad")),
    );
    let span = SpanBytes {
        span_events: thin_vec::thin_vec![SpanEventBytes {
            time_unix_nano: 1,
            name: bs("evt"),
            attributes: attrs,
        }],
        ..minimal_span()
    };
    let out = encode_first_span(&[minimal_chunk([0u8; 16], span)]);
    let raw = out["meta"]["events"].as_str().unwrap();
    let events: Value = serde_json::from_str(raw).unwrap();
    assert!(events[0].get("attributes").is_none());
}
