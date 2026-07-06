// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracing_integration_tests {
    use http_body_util::BodyExt;
    #[cfg(target_os = "linux")]
    use hyper::Uri;
    use libdd_capabilities_impl::{HttpClientCapability, NativeCapabilities};
    #[cfg(target_os = "linux")]
    use libdd_common::connector::uds::socket_path_to_uri;
    use libdd_common::{http_common, Endpoint};
    use libdd_tinybytes::{Bytes, BytesString};
    use libdd_trace_utils::send_data::SendData;
    use libdd_trace_utils::span::vec_map::VecMap;
    use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use libdd_trace_utils::test_utils::{create_test_json_span, create_test_no_alloc_span};
    use libdd_trace_utils::trace_utils::TracerHeaderTags;
    use libdd_trace_utils::tracer_payload::{decode_to_trace_chunks, TraceEncoding};
    use serde_json::json;
    #[cfg(target_os = "linux")]
    use std::fs::Permissions;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;

    fn get_v04_trace_snapshot_test_payload(name_prefix: &str) -> Bytes {
        let mut span_1 = create_test_json_span(1234, 12342, 12341, 1, false);
        span_1["name"] = json!(format!("{}_01", name_prefix));

        span_1["metrics"] = json!({
            "_dd_metric1": 1.0,
            "_dd_metric2": 2.0
        });
        span_1["span_events"] = json!([
            {
                "name": "test_span",
                "time_unix_nano": 1727211691770715042_u64
            },
            {
                "name": "exception",
                "time_unix_nano": 1727211691770716000_u64,
                "attributes": {
                    "exception.message": {"type": 0, "string_value": "Cannot divide by zero"},
                    "exception.version": {"type": 3, "double_value": 4.2},
                    "exception.escaped": {"type": 1, "bool_value": true},
                    "exception.count": {"type": 2, "int_value": 1},
                    "exception.lines": {"type": 4, "array_value": {
                        "values": [
                            {"type": 0, "string_value": "  File \"<string>\", line 1, in <module>"},
                            {"type": 0, "string_value": "  File \"<string>\", line 1, in divide"},
                        ]
                    }}
                }
            }
        ]);

        let mut span_2 = create_test_json_span(1234, 12343, 12341, 1, false);
        span_2["name"] = json!(format!("{}_02", name_prefix));
        span_2["span_links"] = json!([
            {
                "trace_id": 0xc151df7d6ee5e2d6_u64,
                "span_id": 0xa3978fb9b92502a8_u64,
                "attributes": {
                    "link.name":"Job #123"
                }
            },
            {
                "trace_id": 0xa918bf567eec151d_u64,
                "trace_id_high": 0x527ccbd68a74d57e_u64,
                "span_id": 0xc08c967f0e5e7b0a_u64
            }
        ]);

        let mut root_span = create_test_json_span(1234, 12341, 0, 0, true);
        root_span["name"] = json!(format!("{}_03", name_prefix));
        root_span["type"] = json!("web".to_owned());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span_1, span_2, root_span]]).unwrap();

        libdd_tinybytes::Bytes::from(encoded_data)
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_snapshot_test() {
        let relative_snapshot_path = "libdd-trace-utils/tests/snapshots/";
        let snapshot_name = "compare_send_data_v04_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;

        let header_tags = TracerHeaderTags {
            lang: "test-lang",
            lang_version: "2.0",
            lang_interpreter: "interpreter",
            lang_vendor: "vendor",
            tracer_version: "1.0",
            container_id: "id",
            ..Default::default()
        };

        let endpoint = Endpoint::from_url(
            test_agent
                .get_uri_for_endpoint("v0.4/traces", Some(snapshot_name))
                .await,
        );

        let data = get_v04_trace_snapshot_test_payload("test_send_data_v04_snapshot");

        let (payload_collection, _size) = decode_to_trace_chunks(data, TraceEncoding::V04)
            .expect("unable to convert TracerPayloadParams to TracerPayloadCollection");

        test_agent.start_session(snapshot_name, None).await;

        let data = SendData::new(
            300,
            payload_collection.into_tracer_payload_collection(),
            header_tags,
            &endpoint,
        );

        let _result = data.send(&NativeCapabilities::new_client()).await;

        test_agent.assert_snapshot(snapshot_name).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_meta_struct_snapshot_test() {
        let relative_snapshot_path = "libdd-trace-utils/tests/snapshots/";
        let snapshot_name = "compare_send_data_v04_trace_meta_struct_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;

        let header_tags = TracerHeaderTags {
            lang: "test-lang",
            lang_version: "2.0",
            lang_interpreter: "interpreter",
            lang_vendor: "vendor",
            tracer_version: "1.0",
            container_id: "id",
            ..Default::default()
        };

        let endpoint = Endpoint::from_url(
            test_agent
                .get_uri_for_endpoint("v0.4/traces", Some(snapshot_name))
                .await,
        );

        let meta_struct_data = rmp_serde::to_vec_named(&json!({
                "exploit": [
                {
                    "type": "test",
                    "language": "nodejs",
                    "id": "someuuid",
                    "message": "Threat detected",
                    "frames": [
                    {
                        "id": 0,
                        "file": "test.js",
                        "line": 1,
                        "column": 31,
                        "function": "test"
                    },
                    {
                        "id": 1,
                        "file": "test2.js",
                        "line": 54,
                        "column": 77,
                        "function": "test"
                    },
                    {
                        "id": 2,
                        "file": "test.js",
                        "line": 1245,
                        "column": 41,
                        "function": "test"
                    },
                    {
                        "id": 3,
                        "file": "test3.js",
                        "line": 2024,
                        "column": 32,
                        "function": "test"
                    }
                    ]
                }
                ]
        }))
        .unwrap();

        let mut root_span = create_test_no_alloc_span(1234, 12341, 0, 0, true);
        root_span.name = BytesString::from("test_send_data_v04_trace_meta_struct_snapshot_01");
        root_span.r#type = BytesString::from("web");
        root_span.meta_struct =
            vec![(BytesString::from("appsec"), Bytes::from(meta_struct_data))].into();

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![root_span]]).unwrap();

        let data = libdd_tinybytes::Bytes::from(encoded_data);

        let (payload_collection, _) = decode_to_trace_chunks(data, TraceEncoding::V04)
            .expect("unable to convert TracerPayloadParams to TracerPayloadCollection");

        test_agent.start_session(snapshot_name, None).await;

        let data = SendData::new(
            300,
            payload_collection.into_tracer_payload_collection(),
            header_tags,
            &endpoint,
        );

        let _result = data.send(&NativeCapabilities::new_client()).await;

        test_agent.assert_snapshot(snapshot_name).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    // It is valid for some tracers to send an empty array of spans to the agent
    async fn send_empty_v04_trace_test() {
        let test_agent = DatadogTestAgent::new(None, None, &[]).await;

        let header_tags = TracerHeaderTags {
            lang: "test-lang",
            lang_version: "2.0",
            lang_interpreter: "interpreter",
            lang_vendor: "vendor",
            tracer_version: "1.0",
            container_id: "id",
            ..Default::default()
        };

        let endpoint =
            Endpoint::from_url(test_agent.get_uri_for_endpoint("v0.4/traces", None).await);

        let empty_data = vec![0x90];
        let data = libdd_tinybytes::Bytes::from(empty_data);

        let (payload_collection, _) = decode_to_trace_chunks(data, TraceEncoding::V04)
            .expect("unable to convert TracerPayloadParams to TracerPayloadCollection");

        let data = SendData::new(
            0,
            payload_collection.into_tracer_payload_collection(),
            header_tags,
            &endpoint,
        );

        let result = data.send(&NativeCapabilities::new_client()).await;

        assert!(result.last_result.is_ok());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    #[cfg(target_os = "linux")]
    // Validate that we can correctly send traces to the agent via UDS
    async fn uds_snapshot_test() {
        let relative_snapshot_path = "libdd-trace-utils/tests/snapshots/";
        let snapshot_name = "compare_send_data_v04_trace_snapshot_uds_test";
        // Create a temporary directory for the socket to be mounted in the test agent container
        let socket_dir = tempfile::Builder::new()
            .prefix("dd-trace-test-")
            .tempdir()
            .expect("Failed to create temporary directory");

        std::fs::set_permissions(socket_dir.path(), Permissions::from_mode(0o755))
            .expect("Failed to set directory permissions");

        let absolute_socket_dir_path = socket_dir
            .path()
            .to_str()
            .expect("Failed to convert path to string")
            .to_owned();

        let absolute_socket_path = socket_dir.path().join("apm.socket");
        let socket_path = socket_path_to_uri(absolute_socket_path.as_path());
        let socket_uri = socket_path.unwrap();

        let mut parts = socket_uri.into_parts();
        let p_q = match parts.path_and_query {
            None => None,
            Some(pq) => {
                let path = pq.path();
                let path = path.strip_suffix('/').unwrap_or(path);
                Some(
                    format!("{path}/v0.4/traces?test_session_token={snapshot_name}")
                        .parse()
                        .unwrap(),
                )
            }
        };
        parts.path_and_query = p_q;

        let url = Uri::from_parts(parts).unwrap();

        let test_agent = DatadogTestAgent::new(
            Some(relative_snapshot_path),
            Some(&absolute_socket_dir_path),
            &[],
        )
        .await;

        let endpoint = Endpoint::from_url(url);

        let header_tags = TracerHeaderTags {
            lang: "test-lang",
            lang_version: "2.0",
            lang_interpreter: "interpreter",
            lang_vendor: "vendor",
            tracer_version: "1.0",
            container_id: "id",
            ..Default::default()
        };

        let data = get_v04_trace_snapshot_test_payload("test_send_data_v04_snapshot_uds");

        let (payload_collection, size) = decode_to_trace_chunks(data, TraceEncoding::V04)
            .expect("unable to convert TracerPayloadParams to TracerPayloadCollection");

        test_agent.start_session(snapshot_name, None).await;

        let data = SendData::new(
            size,
            payload_collection.into_tracer_payload_collection(),
            header_tags,
            &endpoint,
        );

        let _result = data.send(&NativeCapabilities::new_client()).await;

        test_agent.assert_snapshot(snapshot_name).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_remote_set_remote_config_data() {
        let snapshot_name = "test_remote_set_remote_config_data";
        let test_agent = DatadogTestAgent::new(None, None, &[]).await;

        test_agent
            .set_remote_config_response(
                r##"{
            "path": "2/APM_TRACING/1234/config",
            "msg": {
                "tracing_sampling_rules": [
                    {
                        "service": "test-service",
                        "name": "test-name",
                        "sample_rate": 0.5
                    }
                ]
            }
        }"##,
                Some(snapshot_name),
            )
            .await;

        let uri = test_agent
            .get_uri_for_endpoint("v0.7/config", Some(snapshot_name))
            .await;

        let res = http_common::new_default_client()
            .get(uri)
            .await
            .expect("Failed to get remote config data from test agent");
        assert_eq!(
            res.status(),
            200,
            "Expected status 200 for remote config data, but got {}",
            res.status()
        );
        let body = res
            .into_body()
            .collect()
            .await
            .expect("Failed to read body data")
            .to_bytes();
        let s = std::str::from_utf8(&body).expect("Failed to convert body to string");
        let response = serde_json::de::from_str::<serde_json::Value>(s)
            .expect("Failed to parse response as json");
        assert_eq!(
            response["client_configs"][0].as_str().unwrap(),
            "2/APM_TRACING/1234/config"
        );
        assert_eq!(
            response["target_files"][0]["path"].as_str().unwrap(),
            "2/APM_TRACING/1234/config"
        );
        assert_eq!(
            response["target_files"][0]["raw"].as_str().unwrap(),
            "eyJ0cmFjaW5nX3NhbXBsaW5nX3J1bGVzIjogW3sic2VydmljZSI6ICJ0ZXN0LXNlcnZpY2UiLCAibmFtZSI6ICJ0ZXN0LW5hbWUiLCAic2FtcGxlX3JhdGUiOiAwLjV9XX0="
        );
    }

    // ───────────────────────── V1 integration tests ──────────────────────────
    //
    // These tests cover the v1::Span encoder end-to-end: the payload is built directly from the
    // `TracerPayload` data model in Rust, encoded with `to_vec_from_payload_v1`, POSTed to the
    // `dd-apm-test-agent`'s `/v1.0/traces`, and validated via snapshot. The test-agent is the V1
    // decoder, so this exercises the full round-trip without us having to maintain one in this
    // crate.

    fn bs_v1(s: &str) -> BytesString {
        BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
    }

    /// 128-bit big-endian trace_id from `(high, low)` 64-bit halves.
    fn tid_bytes(high: u64, low: u64) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[..8].copy_from_slice(&high.to_be_bytes());
        out[8..].copy_from_slice(&low.to_be_bytes());
        out
    }

    /// POSTs a raw V1 msgpack payload to the test-agent's `/v1.0/traces` and asserts the agent
    /// returns 2xx. Headers are the minimum the agent needs to attach the payload to a snapshot
    /// session (`X-Datadog-Test-Session-Token` query param + `Datadog-Meta-Lang*` for routing).
    async fn post_v1_payload(uri: hyper::Uri, body: Vec<u8>) {
        use libdd_capabilities_impl::HttpClientCapability;
        let client = NativeCapabilities::new_client();
        let req = http::Request::builder()
            .method(http::Method::POST)
            .uri(uri)
            .header("Content-type", "application/msgpack")
            .header("Datadog-Meta-Lang", "test-lang")
            .header("Datadog-Meta-Lang-Version", "2.0")
            .header("Datadog-Meta-Lang-Interpreter", "interpreter")
            .header("Datadog-Meta-Tracer-Version", "1.0")
            .body(bytes::Bytes::from(body))
            .expect("failed to build request");
        let response = client.request(req).await.expect("request failed");
        assert!(
            response.status().is_success(),
            "test-agent rejected V1 payload: status={} body={:?}",
            response.status(),
            String::from_utf8_lossy(response.body())
        );
    }

    /// Builds a TracerPayload that exercises the multi-key attribute paths the v0.4 to V1 encoder
    /// can't cover on its own (HashMap iteration order makes byte-by-byte cross-validation flaky
    /// for n > 1), plus the primitive `AttributeValue` variants the test-agent currently
    /// supports.
    ///
    /// APMSP-3479 - TODO: `AttributeValue::List` and `AttributeValue::KeyValue` are deliberately
    /// omitted because not yet supported by `ddapm-test-agent` v1.56.0. Once test-agent V1
    /// support catches up, add them here too.
    fn make_v1_payload(name_prefix: &str) -> libdd_trace_utils::span::v1::TracerPayloadBytes {
        use libdd_trace_utils::span::v1::{
            AttributeValue, AttributeValueBytes, SpanBytes as V1SpanBytes, SpanEventBytes,
            SpanKind, SpanLinkBytes, TraceChunkBytes, TracerPayloadBytes,
        };

        // Multi-key attribute map on the root span — primitive variants only.
        let mut root_attrs: VecMap<BytesString, AttributeValueBytes> = VecMap::new();
        root_attrs.insert(bs_v1("http.method"), AttributeValue::String(bs_v1("GET")));
        root_attrs.insert(bs_v1("http.status_code"), AttributeValue::Int(200));
        root_attrs.insert(bs_v1("http.success"), AttributeValue::Bool(true));
        root_attrs.insert(bs_v1("http.duration_ms"), AttributeValue::Float(12.5));

        let span_link = SpanLinkBytes {
            trace_id: tid_bytes(0x0123_4567_89ab_cdef, 0xfedc_ba98_7654_3210),
            span_id: 0xa0a0_a0a0_a0a0_a0a0,
            tracestate: bs_v1("dd=t.tid:abc"),
            flags: 1,
            attributes: VecMap::new(),
        };

        let span_event = SpanEventBytes {
            time_unix_nano: 1_727_211_691_770_715_042,
            name: bs_v1("exception"),
            attributes: VecMap::new(),
        };

        let root_span = V1SpanBytes {
            service: bs_v1("test-service"),
            name: bs_v1(&format!("{name_prefix}_root")),
            resource: bs_v1("/api/users"),
            r#type: bs_v1("web"),
            span_id: 1,
            parent_id: 0,
            start: 1_000_000,
            duration: 5_000,
            span_kind: SpanKind::Server,
            env: bs_v1("test-env"),
            version: bs_v1("1.2.3"),
            component: bs_v1("http"),
            attributes: root_attrs,
            span_links: thin_vec::thin_vec![span_link],
            span_events: thin_vec::thin_vec![span_event],
            ..Default::default()
        };

        // Multi-key chunk-level attributes.
        let mut chunk_attrs = VecMap::new();
        chunk_attrs.insert(bs_v1("_dd.p.dm"), AttributeValue::String(bs_v1("-4")));
        chunk_attrs.insert(
            bs_v1("_dd.p.tid"),
            AttributeValue::String(bs_v1("0123456789abcdef")),
        );

        let chunk = TraceChunkBytes {
            trace_id: tid_bytes(0, 0xdeadbeef),
            priority: Some(1),
            origin: bs_v1("synthetics"),
            sampling_mechanism: Some(4),
            attributes: chunk_attrs,
            dropped_trace: false,
            spans: vec![root_span],
        };

        TracerPayloadBytes {
            language_name: bs_v1("test-lang"),
            language_version: bs_v1("2.0"),
            tracer_version: bs_v1("1.0"),
            runtime_id: bs_v1("test-runtime-id"),
            env: bs_v1("test-env"),
            hostname: bs_v1("test-host"),
            app_version: bs_v1("1.2.3"),
            attributes: VecMap::new(),
            chunks: vec![chunk],
        }
    }

    /// End-to-end round-trip: builds a V1 payload directly from `TracerPayload`, encodes it
    /// with `to_vec_from_payload_v1`, POSTs to the test-agent, and asserts the snapshot.
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v1_native_trace_snapshot_test() {
        use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_payload_v1;

        let relative_snapshot_path = "libdd-trace-utils/tests/snapshots/";
        let snapshot_name = "compare_send_data_v1_native_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;

        let uri = test_agent
            .get_uri_for_endpoint("v1.0/traces", Some(snapshot_name))
            .await;

        test_agent.start_session(snapshot_name, None).await;

        let payload = make_v1_payload("test_send_data_v1_native_snapshot");
        let encoded = to_vec_from_payload_v1(&payload);

        post_v1_payload(uri, encoded).await;

        test_agent.assert_snapshot(snapshot_name).await;
    }

    /// Cross-validates the v0.4→V1 and v1::Span encoders against a single canonical snapshot.
    /// Both encodings are POSTed into the same session with distinct `trace_id`s, so the
    /// snapshot records two traces — one per encoder — that must each decode to the same
    /// shape. Any drift in either encoder makes its trace diverge from the checked-in form.
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_and_v1_encoders_snapshot_test() {
        use libdd_trace_utils::msgpack_encoder::v1::{to_vec, to_vec_from_payload_v1};
        use libdd_trace_utils::span::v04::SpanBytes as V04SpanBytes;
        use libdd_trace_utils::span::v1::{
            AttributeValue, SpanBytes as V1SpanBytes, SpanKind, TraceChunkBytes, TracerPayloadBytes,
        };
        use libdd_trace_utils::tracer_metadata::TracerMetadata;

        let relative_snapshot_path = "libdd-trace-utils/tests/snapshots/";
        let snapshot_name = "compare_v04_and_v1_encoders_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;
        let uri = test_agent
            .get_uri_for_endpoint("v1.0/traces", Some(snapshot_name))
            .await;

        test_agent.start_session(snapshot_name, None).await;

        // ── v0.4 input — trace_id = 1 ──────────────────────────────────────────────
        let mut meta_v04 = VecMap::new();
        meta_v04.insert(bs_v1("env"), bs_v1("test-env"));
        meta_v04.insert(bs_v1("http.method"), bs_v1("GET"));
        let mut metrics_v04 = VecMap::new();
        metrics_v04.insert(bs_v1("http.duration_ms"), 12.5_f64);
        let v04_traces: Vec<Vec<V04SpanBytes>> = vec![vec![V04SpanBytes {
            service: bs_v1("svc"),
            name: bs_v1("op"),
            resource: bs_v1("res"),
            trace_id: 1,
            span_id: 1,
            start: 1_000_000,
            duration: 5_000,
            meta: meta_v04,
            metrics: metrics_v04,
            ..Default::default()
        }]];
        let metadata = TracerMetadata::default();

        // ── v1::Span input — trace_id = 2, semantically equivalent otherwise ───────
        // Distinct trace_id so the test-agent records both as separate traces in the snapshot
        // (rather than merging/deduping when (trace_id, span_id) collide).
        let mut attrs_v1 = VecMap::new();
        attrs_v1.insert(bs_v1("http.method"), AttributeValue::String(bs_v1("GET")));
        attrs_v1.insert(bs_v1("http.duration_ms"), AttributeValue::Float(12.5));
        let v1_payload = TracerPayloadBytes {
            chunks: vec![TraceChunkBytes {
                trace_id: tid_bytes(0, 2),
                spans: vec![V1SpanBytes {
                    service: bs_v1("svc"),
                    name: bs_v1("op"),
                    resource: bs_v1("res"),
                    span_id: 1,
                    start: 1_000_000,
                    duration: 5_000,
                    span_kind: SpanKind::Internal,
                    env: bs_v1("test-env"),
                    attributes: attrs_v1,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Default::default()
        };

        // ── POST both into the same session ────────────────────────────────────────
        let bytes_v04 = to_vec(&v04_traces, &metadata);
        let bytes_v1 = to_vec_from_payload_v1(&v1_payload);
        post_v1_payload(uri.clone(), bytes_v04).await;
        post_v1_payload(uri, bytes_v1).await;

        // Both POSTs share trace_id=1, so the test-agent merges them into a single trace of
        // 2 decoded spans. The checked-in snapshot is the canonical equivalent decoded form.
        test_agent.assert_snapshot(snapshot_name).await;
    }
}
