// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tracing_integration_tests {
    use http_body_util::BodyExt;
    #[cfg(target_os = "linux")]
    use hyper::Uri;
    #[cfg(target_os = "linux")]
    use libdd_common::connector::uds::socket_path_to_uri;
    use libdd_common::hyper_migration::new_default_client;
    use libdd_common::{hyper_migration, Endpoint};
    use libdd_tinybytes::{Bytes, BytesString};
    use libdd_trace_utils::send_data::SendData;
    use libdd_trace_utils::test_utils::datadog_test_agent::DatadogTestAgent;
    use libdd_trace_utils::test_utils::{create_test_json_span, create_test_no_alloc_span};
    use libdd_trace_utils::trace_utils::TracerHeaderTags;
    use libdd_trace_utils::tracer_payload::{decode_to_trace_chunks, TraceEncoding};
    use serde_json::json;
    use std::collections::HashMap;
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

        let client = new_default_client();
        let _result = data.send(&client).await;

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
            HashMap::from([(BytesString::from("appsec"), Bytes::from(meta_struct_data))]);

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

        let client = new_default_client();
        let _result = data.send(&client).await;

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

        let client = new_default_client();
        let result = data.send(&client).await;

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

        let client = new_default_client();
        let _result = data.send(&client).await;

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

        let res = hyper_migration::new_default_client()
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
}
