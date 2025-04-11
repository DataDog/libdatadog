// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "mini_agent")]
pub use mini_agent::*;

#[cfg(feature = "mini_agent")]
mod mini_agent {
    use datadog_trace_protobuf::pb;
    use ddcommon::hyper_migration;
    use ddcommon::Endpoint;
    use http_body_util::BodyExt;
    use hyper::{body::Buf, Method, Request, StatusCode};
    use std::io::Write;
    use tracing::debug;

    pub async fn get_stats_from_request_body(
        body: hyper_migration::Body,
    ) -> anyhow::Result<pb::ClientStatsPayload> {
        let buffer = body.collect().await?.aggregate();

        let client_stats_payload: pb::ClientStatsPayload =
            match rmp_serde::from_read(buffer.reader()) {
                Ok(res) => res,
                Err(err) => {
                    anyhow::bail!("Error deserializing stats from request body: {err}")
                }
            };

        if client_stats_payload.stats.is_empty() {
            debug!("Empty trace stats payload received, but this is okay");
        }
        Ok(client_stats_payload)
    }

    pub fn construct_stats_payload(stats: Vec<pb::ClientStatsPayload>) -> pb::StatsPayload {
        pb::StatsPayload {
            agent_hostname: "".to_string(),
            agent_env: "".to_string(),
            stats,
            agent_version: "".to_string(),
            client_computed: true,
            split_payload: false,
        }
    }

    pub fn serialize_stats_payload(payload: pb::StatsPayload) -> anyhow::Result<Vec<u8>> {
        let msgpack = rmp_serde::to_vec_named(&payload)?;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&msgpack)?;
        match encoder.finish() {
            Ok(res) => Ok(res),
            Err(e) => anyhow::bail!("Error serializing stats payload: {e}"),
        }
    }

    pub async fn send_stats_payload(
        data: Vec<u8>,
        target: &Endpoint,
        api_key: &str,
    ) -> anyhow::Result<()> {
        let req = Request::builder()
            .method(Method::POST)
            .uri(target.url.clone())
            .header("Content-Type", "application/msgpack")
            .header("Content-Encoding", "gzip")
            .header("DD-API-KEY", api_key)
            .body(hyper_migration::Body::from(data.clone()))?;

        let client = hyper_migration::new_default_client();
        match client.request(req).await {
            Ok(response) => {
                if response.status() != StatusCode::ACCEPTED {
                    let body_bytes = response.into_body().collect().await?.to_bytes();
                    let response_body = String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                    anyhow::bail!("Server did not accept trace stats: {response_body}");
                }
                Ok(())
            }
            Err(e) => anyhow::bail!("Failed to send trace stats: {e}"),
        }
    }
}

#[cfg(test)]
#[cfg(feature = "mini_agent")]
mod mini_agent_tests {
    use crate::stats_utils;
    use datadog_trace_protobuf::pb::{
        ClientGroupedStats, ClientStatsBucket, ClientStatsPayload, Trilean::NotSet,
    };
    use ddcommon::hyper_migration;
    use hyper::Request;
    use serde_json::Value;

    #[tokio::test]
    #[cfg_attr(all(miri, target_os = "macos"), ignore)]
    async fn test_get_stats_from_request_body() {
        let stats_json = r#"{
            "Hostname": "TestHost",
            "Env": "test",
            "Version": "1.0.0",
            "Stats": [
                {
                    "Start": 0,
                    "Duration": 10000000000,
                    "Stats": [
                        {
                            "Name": "test-span",
                            "Service": "test-service",
                            "Resource": "test-span",
                            "Type": "",
                            "HTTPStatusCode": 0,
                            "Synthetics": false,
                            "Hits": 1,
                            "TopLevelHits": 1,
                            "Errors": 0,
                            "Duration": 10000000,
                            "OkSummary": [
                                0,
                                0,
                                0
                            ],
                            "ErrorSummary": [
                                0,
                                0,
                                0
                            ]
                        }
                    ]
                }
            ],
            "Lang": "javascript",
            "TracerVersion": "1.0.0",
            "RuntimeID": "00000000-0000-0000-0000-000000000000",
            "Sequence": 1
        }"#;

        let v: Value = match serde_json::from_str(stats_json) {
            Ok(value) => value,
            Err(err) => {
                panic!("Failed to parse stats JSON: {}", err);
            }
        };

        let bytes = rmp_serde::to_vec(&v).unwrap();
        let request = Request::builder()
            .body(hyper_migration::Body::from(bytes))
            .unwrap();

        let res = stats_utils::get_stats_from_request_body(request.into_body()).await;

        let client_stats_payload = ClientStatsPayload {
            hostname: "TestHost".to_string(),
            env: "test".to_string(),
            version: "1.0.0".to_string(),
            stats: vec![ClientStatsBucket {
                start: 0,
                duration: 10000000000,
                stats: vec![ClientGroupedStats {
                    service: "test-service".to_string(),
                    name: "test-span".to_string(),
                    resource: "test-span".to_string(),
                    http_status_code: 0,
                    r#type: "".to_string(),
                    db_type: "".to_string(),
                    hits: 1,
                    errors: 0,
                    duration: 10000000,
                    ok_summary: vec![0, 0, 0],
                    error_summary: vec![0, 0, 0],
                    synthetics: false,
                    top_level_hits: 1,
                    span_kind: "".to_string(),
                    peer_tags: vec![],
                    is_trace_root: NotSet.into(),
                }],
                agent_time_shift: 0,
            }],
            lang: "javascript".to_string(),
            tracer_version: "1.0.0".to_string(),
            runtime_id: "00000000-0000-0000-0000-000000000000".to_string(),
            sequence: 1,
            agent_aggregation: "".to_string(),
            service: "".to_string(),
            container_id: "".to_string(),
            tags: vec![],
            git_commit_sha: "".to_string(),
            image_tag: "".to_string(),
        };

        assert!(
            res.is_ok(),
            "Expected Ok result, but got Err: {}",
            res.unwrap_err()
        );
        assert_eq!(res.unwrap(), client_stats_payload)
    }

    #[tokio::test]
    #[cfg_attr(all(miri, target_os = "macos"), ignore)]
    async fn test_get_stats_from_request_body_without_stats() {
        let stats_json = r#"{
            "Hostname": "TestHost",
            "Env": "test",
            "Version": "1.0.0",
            "Lang": "javascript",
            "TracerVersion": "1.0.0",
            "RuntimeID": "00000000-0000-0000-0000-000000000000",
            "Sequence": 1
        }"#;

        let v: Value = match serde_json::from_str(stats_json) {
            Ok(value) => value,
            Err(err) => {
                panic!("Failed to parse stats JSON: {}", err);
            }
        };

        let bytes = rmp_serde::to_vec(&v).unwrap();
        let request = Request::builder()
            .body(hyper_migration::Body::from(bytes))
            .unwrap();

        let res = stats_utils::get_stats_from_request_body(request.into_body()).await;

        let client_stats_payload = ClientStatsPayload {
            hostname: "TestHost".to_string(),
            env: "test".to_string(),
            version: "1.0.0".to_string(),
            stats: vec![],
            lang: "javascript".to_string(),
            tracer_version: "1.0.0".to_string(),
            runtime_id: "00000000-0000-0000-0000-000000000000".to_string(),
            sequence: 1,
            agent_aggregation: "".to_string(),
            service: "".to_string(),
            container_id: "".to_string(),
            tags: vec![],
            git_commit_sha: "".to_string(),
            image_tag: "".to_string(),
        };

        assert!(
            res.is_ok(),
            "Expected Ok result, but got Err: {}",
            res.unwrap_err()
        );
        assert_eq!(res.unwrap(), client_stats_payload)
    }

    #[tokio::test]
    #[cfg_attr(all(miri, target_os = "macos"), ignore)]
    async fn test_serialize_client_stats_payload_without_stats() {
        let client_stats_payload_without_stats = ClientStatsPayload {
            hostname: "TestHost".to_string(),
            env: "test".to_string(),
            version: "1.0.0".to_string(),
            stats: vec![],
            lang: "javascript".to_string(),
            tracer_version: "1.0.0".to_string(),
            runtime_id: "00000000-0000-0000-0000-000000000000".to_string(),
            sequence: 1,
            agent_aggregation: "".to_string(),
            service: "".to_string(),
            container_id: "".to_string(),
            tags: vec![],
            git_commit_sha: "".to_string(),
            image_tag: "".to_string(),
        };

        let client_stats_payload_without_inner_stats = ClientStatsPayload {
            hostname: "TestHost".to_string(),
            env: "test".to_string(),
            version: "1.0.0".to_string(),
            stats: vec![ClientStatsBucket {
                start: 0,
                duration: 10000000000,
                stats: vec![],
                agent_time_shift: 0,
            }],
            lang: "javascript".to_string(),
            tracer_version: "1.0.0".to_string(),
            runtime_id: "00000000-0000-0000-0000-000000000000".to_string(),
            sequence: 1,
            agent_aggregation: "".to_string(),
            service: "".to_string(),
            container_id: "".to_string(),
            tags: vec![],
            git_commit_sha: "".to_string(),
            image_tag: "".to_string(),
        };

        let res = stats_utils::serialize_stats_payload(stats_utils::construct_stats_payload(vec![
            client_stats_payload_without_stats,
            client_stats_payload_without_inner_stats,
        ]));

        assert!(
            res.is_ok(),
            "Expected Ok result, but got Err: {}",
            res.unwrap_err()
        );
    }
}
