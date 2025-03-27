// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
#[cfg(test)]
mod tracing_integration_tests {
    use data_pipeline::trace_exporter::{
        TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
    };
    use datadog_trace_utils::span::v05::dict::SharedDict;
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use datadog_trace_utils::test_utils::{create_test_json_span, create_test_v05_span};
    use serde_json::json;
    #[cfg(target_os = "linux")]
    use std::fs::Permissions;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;
    use tinybytes::Bytes;
    use tokio::task;

    fn get_v04_trace_snapshot_test_payload(name_prefix: &str) -> Bytes {
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
                    "exception.lines": {"type": 4, "array_value": [
                        {"type": 0, "string_value": "  File \"<string>\", line 1, in <module>"},
                        {"type": 0, "string_value": "  File \"<string>\", line 1, in divide"},
                    ]}
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

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span_1, span_2, root_span]]).unwrap();

        tinybytes::Bytes::from(encoded_data)
    }

    fn get_v05_trace_snapshot_test_payload() -> Bytes {
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

        let traces = (dict.dict(), vec![vec![span_1, span_2, root_span]]);
        let encoded_data = rmp_serde::to_vec(&traces).unwrap();
        tinybytes::Bytes::from(encoded_data)
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_snapshot_test() {
        let relative_snapshot_path = "data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v04_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None).await;
        let url = test_agent.get_base_uri().await;
        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_query_params(format!("test_session_token={snapshot_name}").as_str());

            let trace_exporter = builder.build().expect("Unable to build TraceExporter");

            let data = get_v04_trace_snapshot_test_payload("test_exporter_v04_snapshot");
            let response = trace_exporter.send(data, 1);
            let expected_response = format!("{{\"rate_by_service\": {}}}", rate_param);

            assert!(response.is_ok());
            assert_eq!(response.unwrap().body, expected_response)
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
        let relative_snapshot_path = "data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v04_to_v05_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None).await;
        let url = test_agent.get_base_uri().await;
        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_query_params(format!("test_session_token={}", snapshot_name).as_str())
                .set_input_format(TraceExporterInputFormat::V04)
                .set_output_format(TraceExporterOutputFormat::V05);
            let trace_exporter = builder.build().expect("Unable to build TraceExporter");

            let data = get_v04_trace_snapshot_test_payload("test_exporter_v04_v05_snapshot");
            let response = trace_exporter.send(data, 1);
            let expected_response = format!("{{\"rate_by_service\": {}}}", rate_param);

            assert!(response.is_ok());
            assert_eq!(response.unwrap().body, expected_response)
        })
        .await;

        assert!(task_result.is_ok());

        test_agent.assert_snapshot(snapshot_name).await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v05_trace_snapshot_test() {
        let relative_snapshot_path = "data-pipeline/tests/snapshots/";
        let snapshot_name = "compare_exporter_v05_trace_snapshot_test";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None).await;
        let url = test_agent.get_base_uri().await;
        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_query_params(format!("test_session_token={}", snapshot_name).as_str())
                .set_input_format(TraceExporterInputFormat::V05)
                .set_output_format(TraceExporterOutputFormat::V05);
            let trace_exporter = builder.build().expect("Unable to build TraceExporter");

            let data = get_v05_trace_snapshot_test_payload();
            let response = trace_exporter.send(data, 1);
            let expected_response = format!("{{\"rate_by_service\": {}}}", rate_param);

            assert!(response.is_ok());
            assert_eq!(response.unwrap().body, expected_response)
        })
        .await;

        assert!(task_result.is_ok());

        test_agent.assert_snapshot(snapshot_name).await;
    }

    #[cfg_attr(miri, ignore)]
    #[cfg(target_os = "linux")]
    #[tokio::test]
    // Validate that we can correctly send traces to the agent via UDS.
    // NOTE: The test should match the non-UDS test above. The snapshot is different so that we can
    // assign unique names to the spans and instantiate a unique session for the test to avoid flaky
    // behavior when running on CI
    async fn uds_snapshot_test() {
        let relative_snapshot_path = "data-pipeline/tests/snapshots/";
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
        )
        .await;

        let rate_param = "{\"service:test,env:test_env\": 0.5, \"service:test2,env:prod\": 0.2}";
        test_agent
            .start_session(snapshot_name, Some(rate_param))
            .await;

        let task_result = task::spawn_blocking(move || {
            let mut builder = TraceExporter::builder();
            builder
                .set_url(url.to_string().as_ref())
                .set_language("test-lang")
                .set_language_version("2.0")
                .set_language_interpreter_vendor("vendor")
                .set_language_interpreter("interpreter")
                .set_tracer_version("1.0")
                .set_env("test_env")
                .set_service("test")
                .set_query_params(format!("test_session_token={snapshot_name}").as_str());

            let trace_exporter = builder.build().expect("Unable to build TraceExporter");

            let data = get_v04_trace_snapshot_test_payload("test_exporter_v04_snapshot_uds");
            let response = trace_exporter.send(data, 1);
            let expected_response = format!("{{\"rate_by_service\": {}}}", rate_param);

            assert!(response.is_ok());
            assert_eq!(response.unwrap().body, expected_response)
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
