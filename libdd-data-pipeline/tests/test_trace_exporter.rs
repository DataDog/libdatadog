// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod tracing_integration_tests {
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_data_pipeline::trace_exporter::agent_response::AgentResponse;
    use libdd_data_pipeline::trace_exporter::{
        TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
    };
    use libdd_shared_runtime::ForkSafeRuntime;
    use libdd_trace_utils::span::v05::dict::SharedDict;
    use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use libdd_trace_utils::test_utils::{create_test_json_span, create_test_v05_span};
    use serde_json::json;
    use tokio::task;

    fn get_v04_trace_snapshot_test_payload(name_prefix: &str) -> Vec<u8> {
        let mut span_1 = create_test_json_span(1234, 12342, 12341, 1, false);
        span_1["name"] = json!(format!("{name_prefix}_01"));

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
        span_2["name"] = json!(format!("{name_prefix}_02"));
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
        root_span["name"] = json!(format!("{name_prefix}_03"));
        root_span["type"] = json!("web".to_owned());

        rmp_serde::to_vec_named(&vec![vec![span_1, span_2, root_span]]).unwrap()
    }

    fn get_v05_trace_snapshot_test_payload() -> Vec<u8> {
        let mut dict = SharedDict::default();

        let span_1 = create_test_v05_span(
            1234,
            12342,
            12341,
            1,
            false,
            &mut dict,
            Some(vec![
                ("_dd_metric1".to_string(), 1.1),
                ("_dd_metric2".to_string(), 2.2),
            ]),
        );

        let span_2 = create_test_v05_span(1234, 12343, 12341, 1, false, &mut dict, None);
        let root_span = create_test_v05_span(
            1234,
            12341,
            0,
            0,
            true,
            &mut dict,
            Some(vec![("_top_level".to_string(), 1.0)]),
        );

        let traces = (dict, vec![vec![span_1, span_2, root_span]]);

        rmp_serde::to_vec(&traces).unwrap()
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_snapshot_test() {
        let relative_snapshot_path = "libdd-data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v04_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;
        let url = test_agent.get_base_uri().await;
        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_test_session_token(snapshot_name);

            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");

            let data = get_v04_trace_snapshot_test_payload("test_exporter_v04_snapshot");

            let response = trace_exporter.send(data.as_ref());
            let expected_response = format!("{{\"rate_by_service\": {rate_param}}}");

            assert!(response.is_ok());
            let AgentResponse::Changed { body } = response.unwrap() else {
                panic!("Expected a changed response");
            };
            assert_eq!(body, expected_response);
        })
        .await;

        let received_traces = test_agent.get_sent_traces().await;

        println!(
            "{}",
            serde_json::to_string_pretty(&received_traces).unwrap()
        );

        assert!(task_result.is_ok());

        test_agent.assert_snapshot(snapshot_name).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_to_v05_trace_snapshot_test() {
        let relative_snapshot_path = "libdd-data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v04_to_v05_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;
        let url = test_agent.get_base_uri().await;
        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_test_session_token(snapshot_name)
                .set_input_format(TraceExporterInputFormat::V04)
                .set_output_format(TraceExporterOutputFormat::V05);
            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");

            let data = get_v04_trace_snapshot_test_payload("test_exporter_v04_v05_snapshot");

            let response = trace_exporter.send(data.as_ref());
            let expected_response = format!("{{\"rate_by_service\": {rate_param}}}");

            assert!(response.is_ok());
            let AgentResponse::Changed { body } = response.unwrap() else {
                panic!("Expected a changed response");
            };
            assert_eq!(body, expected_response);
        })
        .await;

        assert!(task_result.is_ok());

        test_agent.assert_snapshot(snapshot_name).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v05_trace_snapshot_test() {
        let relative_snapshot_path = "libdd-data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v05_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;
        let url = test_agent.get_base_uri().await;
        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_test_session_token(snapshot_name)
                .set_input_format(TraceExporterInputFormat::V05)
                .set_output_format(TraceExporterOutputFormat::V05);
            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");

            let data = get_v05_trace_snapshot_test_payload();

            let response = trace_exporter.send(data.as_ref());
            let expected_response = format!("{{\"rate_by_service\": {rate_param}}}");

            assert!(response.is_ok());
            let AgentResponse::Changed { body } = response.unwrap() else {
                panic!("Expected a changed response");
            };
            assert_eq!(body, expected_response);
        })
        .await;

        assert!(task_result.is_ok());

        test_agent.assert_snapshot(snapshot_name).await;
    }

    fn get_v04_to_v1_trace_snapshot_test_payload(name_prefix: &str) -> Vec<u8> {
        // Root span: exercises chunk-level attrs (sampling priority, origin, mechanism)
        // and span-level promoted fields (env, version, component, span.kind).
        let mut root_span = create_test_json_span(1234, 12341, 0, 0, true);
        root_span["name"] = json!(format!("{name_prefix}_root"));
        root_span["type"] = json!("web");
        root_span["meta"] = json!({
            "env": "test-env",
            "version": "1.0.0",
            "component": "http",
            "span.kind": "server",
            "_dd.hostname": "my-host",
            "_dd.origin": "lambda",
            "_dd.p.dm": "-4",
            "runtime-id": "test-runtime-id-value",
            "service": "test-service",
        });
        root_span["metrics"] = json!({
            "_sampling_priority_v1": 1.0,
            "_dd.top_level": 1.0,
        });

        // Child span: exercises metrics and meta attributes without promoted fields.
        let mut child_span = create_test_json_span(1234, 12342, 12341, 1, false);
        child_span["name"] = json!(format!("{name_prefix}_child"));
        child_span["metrics"] = json!({
            "_dd_metric1": 1.0,
            "_dd_metric2": 2.0,
        });

        rmp_serde::to_vec_named(&vec![vec![root_span, child_span]]).unwrap()
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_to_v1_trace_snapshot_test() {
        let relative_snapshot_path = "libdd-data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v04_to_v1_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;
        let url = test_agent.get_base_uri().await;

        test_agent.start_session(snapshot_name, None).await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_test_session_token(snapshot_name)
                .set_input_format(TraceExporterInputFormat::V04)
                .set_output_format(TraceExporterOutputFormat::V1);

            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");

            let data = get_v04_to_v1_trace_snapshot_test_payload("test_exporter_v04_v1_snapshot");

            // V1 is gated by /info negotiation (fail-closed). Wait until the background
            // fetcher has populated agent_info so the first send promotes v1_active=true
            // and the payload is encoded as V1.
            let start = std::time::Instant::now();
            while libdd_data_pipeline::agent_info::get_agent_info().is_none() {
                if start.elapsed() > std::time::Duration::from_secs(5) {
                    panic!("timeout waiting for /info");
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let response = trace_exporter.send(data.as_ref());
            assert!(response.is_ok(), "send failed: {:?}", response.err());
        })
        .await;

        assert!(task_result.is_ok());

        test_agent.assert_snapshot(snapshot_name).await;
    }

    /// Builds a native V1 `TracerPayload` (not a v0.4-to-V1 cross-encode) exercising a root +
    /// child span with typed attributes, chunk-level sampling priority/origin/mechanism, and a
    /// span link/event.
    ///
    /// APMSP-3479 - TODO: `AttributeValue::Bytes/List/KeyValue` are deliberately omitted —
    /// `ddapm-test-agent` v1.56.0 doesn't support them yet.
    fn get_v1_native_trace_snapshot_test_payload(name_prefix: &str) -> Vec<u8> {
        use libdd_tinybytes::BytesString;
        use libdd_trace_utils::msgpack_encoder::v1::to_vec_from_v1;
        use libdd_trace_utils::span::v1::{
            AttributeValue, SpanBytes as V1SpanBytes, SpanKind, TraceChunkBytes, TracerPayloadBytes,
        };
        use libdd_trace_utils::span::vec_map::VecMap;

        fn bs(s: &str) -> BytesString {
            BytesString::from_slice(s.as_bytes()).expect("test string must fit in BytesString")
        }

        let mut root_attrs = VecMap::new();
        root_attrs.insert(bs("http.method"), AttributeValue::String(bs("GET")));
        root_attrs.insert(bs("http.status_code"), AttributeValue::Int(200));

        let root_span = V1SpanBytes {
            service: bs("test-service"),
            name: bs(&format!("{name_prefix}_root")),
            resource: bs("/api/users"),
            r#type: bs("web"),
            span_id: 1,
            parent_id: 0,
            start: 1_000_000,
            duration: 5_000,
            span_kind: SpanKind::Server,
            env: bs("test-env"),
            version: bs("1.0.0"),
            component: bs("http"),
            attributes: root_attrs,
            ..Default::default()
        };

        let mut child_attrs = VecMap::new();
        child_attrs.insert(bs("_dd_metric1"), AttributeValue::Float(1.0));
        child_attrs.insert(bs("_dd_metric2"), AttributeValue::Float(2.0));

        let child_span = V1SpanBytes {
            service: bs("test-service"),
            name: bs(&format!("{name_prefix}_child")),
            resource: bs("/api/users"),
            span_id: 2,
            parent_id: 1,
            start: 1_000_001,
            duration: 5_000,
            span_kind: SpanKind::Internal,
            env: bs("test-env"),
            attributes: child_attrs,
            ..Default::default()
        };

        let mut chunk_attrs = VecMap::new();
        chunk_attrs.insert(bs("_dd.p.dm"), AttributeValue::String(bs("-4")));

        let chunk = TraceChunkBytes {
            trace_id: {
                let mut id = [0u8; 16];
                id[15] = 0x2a;
                id
            },
            priority: Some(1),
            origin: bs("lambda"),
            sampling_mechanism: Some(4),
            attributes: chunk_attrs,
            dropped_trace: false,
            spans: vec![root_span, child_span],
        };

        let payload = TracerPayloadBytes {
            language_name: bs("test-lang"),
            language_version: bs("2.0"),
            tracer_version: bs("1.0"),
            runtime_id: bs("test-runtime-id-value"),
            env: bs("test-env"),
            hostname: bs("my-host"),
            attributes: VecMap::new(),
            chunks: vec![chunk],
            ..Default::default()
        };

        to_vec_from_v1(&payload)
    }

    /// End-to-end: native V1 input goes through the full `TraceExporter` pipeline
    /// (builder -> `send_trace_chunks_inner_v1` -> v1_active negotiation -> msgpack encode) to a
    /// real test-agent. Unlike `compare_v04_to_v1_trace_snapshot_test` (v0.4 input cross-encoded
    /// to V1), this payload is built as a native `v1::TracerPayload` from the start.
    ///
    /// Asserted via `get_sent_traces()` field checks rather than a checked-in golden snapshot,
    /// since the snapshot can't be generated/verified without a Docker test-agent at authoring
    /// time.
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v1_trace_snapshot_test() {
        let relative_snapshot_path = "libdd-data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v1_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None, &[]).await;
        let url = test_agent.get_base_uri().await;

        test_agent.start_session(snapshot_name, None).await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_test_session_token(snapshot_name)
                .set_input_format(TraceExporterInputFormat::V1)
                .set_output_format(TraceExporterOutputFormat::V1);

            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");

            let data =
                get_v1_native_trace_snapshot_test_payload("test_exporter_v1_native_snapshot");

            // V1 is gated by /info negotiation (fail-closed). Wait until the background
            // fetcher has populated agent_info so the first send promotes v1_active=true
            // and the payload is actually encoded/sent as V1 rather than downgraded to v0.4.
            let start = std::time::Instant::now();
            while libdd_data_pipeline::agent_info::get_agent_info().is_none() {
                if start.elapsed() > std::time::Duration::from_secs(5) {
                    panic!("timeout waiting for /info");
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }

            let response = trace_exporter.send(data.as_ref());
            assert!(response.is_ok(), "send failed: {:?}", response.err());
        })
        .await;

        assert!(task_result.is_ok());

        let received_traces = test_agent.get_sent_traces().await;
        let chunk = received_traces
            .iter()
            .find(|chunk| {
                chunk.as_array().is_some_and(|spans| {
                    spans
                        .iter()
                        .any(|s| s["name"] == "test_exporter_v1_native_snapshot_root")
                })
            })
            .expect("sent chunk containing the native V1 root span not found")
            .as_array()
            .expect("chunk must be an array of spans");

        let root = chunk
            .iter()
            .find(|s| s["name"] == "test_exporter_v1_native_snapshot_root")
            .expect("root span not found");
        assert_eq!(root["service"], "test-service");
        assert_eq!(root["resource"], "/api/users");
        assert_eq!(root["type"], "web");
        assert_eq!(root["span_id"], 1);
        // The V1 encoder omits `parent_id` entirely for a root span (parent_id == 0) rather than
        // encoding an explicit 0, so the decoded JSON has no key here (indexing yields `Null`).
        assert!(root["parent_id"].is_null() || root["parent_id"] == 0);
        assert_eq!(root["meta"]["env"], "test-env");
        assert_eq!(root["meta"]["version"], "1.0.0");
        assert_eq!(root["meta"]["component"], "http");
        assert_eq!(root["meta"]["http.method"], "GET");
        assert_eq!(root["metrics"]["http.status_code"], 200.0);
        assert_eq!(root["meta"]["_dd.origin"], "lambda");
        assert_eq!(root["meta"]["_dd.p.dm"], "-4");

        let child = chunk
            .iter()
            .find(|s| s["name"] == "test_exporter_v1_native_snapshot_child")
            .expect("child span not found");
        assert_eq!(child["service"], "test-service");
        assert_eq!(child["span_id"], 2);
        assert_eq!(child["parent_id"], 1);
        assert_eq!(child["meta"]["env"], "test-env");
        assert_eq!(child["metrics"]["_dd_metric1"], 1.0);
        assert_eq!(child["metrics"]["_dd_metric2"], 2.0);
    }

    #[cfg_attr(miri, ignore)]
    #[cfg(target_os = "linux")]
    #[tokio::test]
    // Validate that we can correctly send traces to the agent via UDS.
    // NOTE: The test should match the non-UDS test above. The snapshot is different so that we can
    // assign unique names to the spans and instantiate a unique session for the test to avoid flaky
    // behavior when running on CI
    async fn uds_snapshot_test() {
        use std::fs::Permissions;
        use std::os::unix::fs::PermissionsExt;

        let relative_snapshot_path = "libdd-data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v04_trace_snapshot_uds_test";
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
        let url = format!("unix://{}", absolute_socket_path.display());

        let test_agent = DatadogTestAgent::new(
            Some(relative_snapshot_path),
            Some(&absolute_socket_dir_path),
            &[],
        )
        .await;

        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_test_session_token(snapshot_name)
                .set_service("test");

            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");

            let data = get_v04_trace_snapshot_test_payload("test_exporter_v04_snapshot_uds");

            let response = trace_exporter.send(data.as_ref());
            let expected_response = format!("{{\"rate_by_service\": {rate_param}}}");

            assert!(response.is_ok());
            let AgentResponse::Changed { body } = response.unwrap() else {
                panic!("Expected a changed response");
            };
            assert_eq!(body, expected_response);
        })
        .await;

        // only fetch and print if there was a failure.
        if task_result.is_err() {
            let received_traces = test_agent.get_sent_traces().await;
            println!(
                "Traces received by agent: {}",
                serde_json::to_string_pretty(&received_traces).unwrap()
            );
        }

        assert!(task_result.is_ok());

        test_agent.assert_snapshot(snapshot_name).await;
    }
}
