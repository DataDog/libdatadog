// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod otlp_export_tests {
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_data_pipeline::trace_exporter::{
        TraceExporterBuilder, TraceExporterInputFormat, TraceExporterOutputFormat,
    };
    use libdd_trace_utils::span::v05::dict::SharedDict;
    use libdd_trace_utils::test_utils::{create_test_json_span, create_test_v05_span};
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::task;

    fn get_v04_trace_snapshot_test_payload(name_prefix: &str) -> Vec<u8> {
        let mut span_1 = create_test_json_span(1234, 12342, 0, 1, false);
        span_1["name"] = json!(format!("{name_prefix}_01"));
        span_1["metrics"] = json!({
            "_dd_metric1": 1.0,
            "_dd_metric2": 2.0,
            "_sampling_priority_v1": 1.0
        });
        let mut span_2 = create_test_json_span(1234, 12343, 12342, 1, false);
        span_2["name"] = json!(format!("{name_prefix}_02"));
        rmp_serde::to_vec_named(&vec![vec![span_1, span_2]]).unwrap()
    }

    fn get_v05_sampled_trace_payload() -> Vec<u8> {
        let mut dict = SharedDict::default();
        let span = create_test_v05_span(
            1234,
            12342,
            0,
            1,
            true,
            &mut dict,
            Some(vec![("_sampling_priority_v1".to_string(), 1.0)]),
        );
        rmp_serde::to_vec(&(dict, vec![vec![span]])).unwrap()
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn otlp_export_sends_correct_payload() {
        use httpmock::MockServer;

        let server = MockServer::start_async().await;
        let body_valid = Arc::new(AtomicBool::new(false));
        let matcher_flag = body_valid.clone();

        // Assert the request structure with json_body_includes, then inspect the complete span
        // array so a child without its own sampling priority must still carry the chunk-level
        // sampled decision.
        let mock = server
            .mock_async(move |when, then| {
                let flag = matcher_flag.clone();
                when.method("POST")
                    .path("/v1/traces")
                    .header("content-type", "application/json")
                    .header("datadog-client-computed-stats", "yes")
                    .json_body_includes(
                        serde_json::json!({
                            "resourceSpans": [{
                                "resource": {
                                    "attributes": [
                                        {"key": "service.name", "value": {"stringValue": "test"}},
                                        {"key": "deployment.environment.name", "value": {"stringValue": "test_env"}},
                                        {"key": "telemetry.sdk.name", "value": {"stringValue": "datadog"}},
                                        {"key": "telemetry.sdk.language", "value": {"stringValue": "test-lang"}},
                                        {"key": "telemetry.sdk.version", "value": {"stringValue": "1.0"}},
                                        {"key": "runtime-id", "value": {"stringValue": "test-runtime-id"}},
                                        {"key": "_dd.stats_computed", "value": {"stringValue": "true"}},
                                    ]
                                }
                            }]
                        })
                        .to_string(),
                    )
                    .is_true(move |req: &httpmock::prelude::HttpMockRequest| {
                        let Ok(body) = serde_json::from_slice::<serde_json::Value>(req.body_ref())
                        else {
                            return false;
                        };
                        let has_attribute =
                            |span: &serde_json::Value, key: &str, string_value: Option<&str>| {
                                span["attributes"].as_array().is_some_and(|attributes| {
                                    attributes.iter().any(|attribute| {
                                        attribute["key"] == key
                                            && string_value.is_none_or(|value| {
                                                attribute["value"]["stringValue"] == value
                                            })
                                    })
                                })
                            };
                        let valid = body["resourceSpans"][0]["scopeSpans"][0]["spans"]
                            .as_array()
                            .is_some_and(|spans| {
                                spans.len() == 2
                                    && spans.iter().all(|span| span["flags"] == 1)
                                    && spans.iter().any(|span| {
                                        has_attribute(
                                            span,
                                            "operation.name",
                                            Some("test_otlp_export_01"),
                                        ) && has_attribute(span, "_sampling_priority_v1", None)
                                    })
                                    && spans.iter().any(|span| {
                                        has_attribute(
                                            span,
                                            "operation.name",
                                            Some("test_otlp_export_02"),
                                        ) && !has_attribute(span, "_sampling_priority_v1", None)
                                    })
                            });
                        if valid {
                            flag.store(true, Ordering::SeqCst);
                        }
                        valid
                    });
                then.status(200).body("{}");
            })
            .await;

        let otlp_endpoint = format!("http://localhost:{}/v1/traces", server.port());

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporterBuilder::default();
            builder
                .set_otlp_endpoint(&otlp_endpoint)
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_runtime_id("test-runtime-id")
                .set_client_computed_stats();

            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");
            let data = get_v04_trace_snapshot_test_payload("test_otlp_export");
            let response = trace_exporter.send(data.as_ref());
            assert!(response.is_ok(), "OTLP send failed: {:?}", response.err());
        })
        .await;

        assert!(task_result.is_ok());
        assert_eq!(mock.calls_async().await, 1);
        assert!(
            body_valid.load(Ordering::SeqCst),
            "OTLP payload did not set sampled flags on both spans"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn otlp_v05_export_sets_sampled_flag() {
        use httpmock::MockServer;

        let server = MockServer::start_async().await;
        let mut mock = server
            .mock_async(|when, then| {
                when.method("POST").path("/v1/traces").json_body_includes(
                    serde_json::json!({
                        "resourceSpans": [{
                            "scopeSpans": [{
                                "spans": [{"flags": 1}]
                            }]
                        }]
                    })
                    .to_string(),
                );
                then.status(200).body("{}");
            })
            .await;

        let otlp_endpoint = format!("http://localhost:{}/v1/traces", server.port());
        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporterBuilder::default();
            builder
                .set_otlp_endpoint(&otlp_endpoint)
                .set_input_format(TraceExporterInputFormat::V05)
                .set_output_format(TraceExporterOutputFormat::V05);
            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");
            let response = trace_exporter.send(&get_v05_sampled_trace_payload());
            assert!(response.is_ok(), "OTLP send failed: {:?}", response.err());
        })
        .await;

        assert!(task_result.is_ok());
        assert_eq!(mock.calls_async().await, 1);
        mock.delete();
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn otlp_export_unsampled_traces_not_exported() {
        use httpmock::MockServer;

        let server = MockServer::start_async().await;
        let mut mock = server
            .mock_async(|when, then| {
                when.method("POST").path("/v1/traces");
                then.status(200).body("{}");
            })
            .await;

        let otlp_endpoint = format!("http://localhost:{}/v1/traces", server.port());

        // Build a v04 payload where all spans have sampling priority -1 (drop).
        let data = {
            let mut span = create_test_json_span(1234, 12341, 0, 1, true);
            span["metrics"]["_sampling_priority_v1"] = serde_json::json!(-1.0);
            rmp_serde::to_vec_named(&vec![vec![span]]).unwrap()
        };

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporterBuilder::default();
            builder
                .set_otlp_endpoint(&otlp_endpoint)
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test");

            let trace_exporter = builder
                .build::<NativeCapabilities>()
                .expect("Unable to build TraceExporter");
            let response = trace_exporter.send(data.as_ref());
            assert!(response.is_ok(), "send failed: {:?}", response.err());
        })
        .await;

        assert!(task_result.is_ok());
        // The mock must not have been called: unsampled traces should be dropped before export.
        assert_eq!(
            mock.calls_async().await,
            0,
            "Unsampled trace was exported — sampling is not being respected"
        );
        mock.delete();
    }
}
