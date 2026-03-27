// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod otlp_export_tests {
    use libdd_data_pipeline::trace_exporter::TraceExporter;
    use libdd_trace_utils::test_utils::create_test_json_span;
    use serde_json::json;
    use tokio::task;

    fn get_v04_trace_snapshot_test_payload(name_prefix: &str) -> Vec<u8> {
        let mut span_1 = create_test_json_span(1234, 12342, 12341, 1, false);
        span_1["name"] = json!(format!("{name_prefix}_01"));
        span_1["metrics"] = json!({
            "_dd_metric1": 1.0,
            "_dd_metric2": 2.0
        });
        let mut span_2 = create_test_json_span(1234, 12343, 12341, 1, false);
        span_2["name"] = json!(format!("{name_prefix}_02"));
        rmp_serde::to_vec_named(&vec![vec![span_1, span_2]]).unwrap()
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn otlp_export_sends_correct_payload() {
        use httpmock::MockServer;

        let server = MockServer::start_async().await;

        // Assert the OTLP request structure using json_body_includes matchers.
        // resourceSpans must be present with the correct service.name and environment resource
        // attributes, and spans must contain the expected name prefix.
        let mut mock = server
            .mock_async(|when, then| {
                when.method("POST")
                    .path("/v1/traces")
                    .header("content-type", "application/json")
                    .json_body_includes(
                        serde_json::json!({
                            "resourceSpans": [{
                                "resource": {
                                    "attributes": [
                                        {"key": "service.name", "value": {"stringValue": "test"}},
                                    ]
                                }
                            }]
                        })
                        .to_string(),
                    );
                then.status(200).body("{}");
            })
            .await;

        let otlp_endpoint = format!("http://localhost:{}/v1/traces", server.port());

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::builder();
            builder
                .set_otlp_endpoint(&otlp_endpoint)
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test");

            let trace_exporter = builder.build().expect("Unable to build TraceExporter");
            let data = get_v04_trace_snapshot_test_payload("test_otlp_export");
            let response = trace_exporter.send(data.as_ref());
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
            let mut builder = TraceExporter::builder();
            builder
                .set_otlp_endpoint(&otlp_endpoint)
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test");

            let trace_exporter = builder.build().expect("Unable to build TraceExporter");
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
