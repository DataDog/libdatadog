// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracing_integration_tests {
    use datadog_trace_utils::send_data::SendData;
    use datadog_trace_utils::test_utils::create_test_json_span;
    use datadog_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use datadog_trace_utils::trace_utils::TracerHeaderTags;
    use datadog_trace_utils::tracer_payload::{
        DefaultTraceChunkProcessor, TraceEncoding, TracerPayloadParams,
    };
    #[cfg(target_os = "linux")]
    use ddcommon::connector::uds::socket_path_to_uri;
    use ddcommon::Endpoint;
    #[cfg(target_os = "linux")]
    use hyper::Uri;
    use serde_json::json;
    #[cfg(target_os = "linux")]
    use std::fs::Permissions;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn compare_v04_trace_snapshot_test() {
        let relative_snapshot_path = "trace-utils/tests/snapshots/";
        let test_agent = DatadogTestAgent::new(Some(relative_snapshot_path), None).await;

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
                .get_uri_for_endpoint("v0.4/traces", Some("compare_v04_trace_snapshot_test"))
                .await,
        );

        let mut span_1 = create_test_json_span(1234, 12342, 12341, 1, false);
        span_1["metrics"] = json!({
            "_dd_metric1": 1.0,
            "_dd_metric2": 2.0
        });

        let span_2 = create_test_json_span(1234, 12343, 12341, 1, false);
        let mut root_span = create_test_json_span(1234, 12341, 0, 0, true);
        root_span["type"] = json!("web".to_owned());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span_1, span_2, root_span]]).unwrap();

        let data = tinybytes::Bytes::from(encoded_data);

        let payload_collection = TracerPayloadParams::new(
            data,
            &header_tags,
            &mut DefaultTraceChunkProcessor,
            false,
            TraceEncoding::V04,
        )
        .try_into()
        .expect("unable to convert TracerPayloadParams to TracerPayloadCollection");

        let data = SendData::new(300, payload_collection, header_tags, &endpoint);

        let _result = data.send().await;

        test_agent
            .assert_snapshot("compare_v04_trace_snapshot_test")
            .await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    // It is valid for some tracers to send an empty array of spans to the agent
    async fn send_empty_v04_trace_test() {
        let test_agent = DatadogTestAgent::new(None, None).await;

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
        let data = tinybytes::Bytes::from(empty_data);
        let tracer_header_tags = &TracerHeaderTags::default();

        let payload_collection = TracerPayloadParams::new(
            data,
            tracer_header_tags,
            &mut DefaultTraceChunkProcessor,
            false,
            TraceEncoding::V04,
        )
        .try_into()
        .expect("unable to convert TracerPayloadParams to TracerPayloadCollection");

        let data = SendData::new(0, payload_collection, header_tags, &endpoint);

        let result = data.send().await;

        assert!(result.last_result.is_ok());
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    #[cfg(target_os = "linux")]
    // Validate that we can correctly send traces to the agent via UDS
    async fn uds_snapshot_test() {
        let relative_snapshot_path = "trace-utils/tests/snapshots/";

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
                    format!(
                        "{path}/v0.4/traces?test_session_token=compare_v04_trace_snapshot_test"
                    )
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

        let mut span_1 = create_test_json_span(1234, 12342, 12341, 1, false);
        span_1["metrics"] = json!({
            "_dd_metric1": 1.0,
            "_dd_metric2": 2.0
        });

        let span_2 = create_test_json_span(1234, 12343, 12341, 1, false);
        let mut root_span = create_test_json_span(1234, 12341, 0, 0, true);
        root_span["type"] = json!("web".to_owned());

        let encoded_data = rmp_serde::to_vec_named(&vec![vec![span_1, span_2, root_span]]).unwrap();

        let data = tinybytes::Bytes::from(encoded_data);

        let payload_collection = TracerPayloadParams::new(
            data,
            &header_tags,
            &mut DefaultTraceChunkProcessor,
            false,
            TraceEncoding::V04,
        )
        .try_into()
        .expect("unable to convert TracerPayloadParams to TracerPayloadCollection");

        let data = SendData::new(300, payload_collection, header_tags, &endpoint);

        let _result = data.send().await;

        test_agent
            .assert_snapshot("compare_v04_trace_snapshot_test")
            .await;
    }
}
