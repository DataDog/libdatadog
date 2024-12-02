// Copyright 2023-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(test)]
mod tests {
    use crate::pb::{is_default, Span};

    #[test]
    fn test_is_default() {
        assert!(is_default(&false));
        assert!(!is_default(&true));

        assert!(is_default(&0));
        assert!(!is_default(&1));

        assert!(is_default(&""));
        assert!(!is_default(&"foo"));
    }

    #[test]
    fn test_serialize_span() {
        let mut span = Span {
            name: "test".to_string(),
            ..Default::default()
        };

        let json = serde_json::to_string(&span).unwrap();
        let expected = "{\"service\":\"\",\"name\":\"test\",\"resource\":\"\",\"trace_id\":0,\"span_id\":0,\"parent_id\":0,\"start\":0,\"duration\":0,\"meta\":{},\"metrics\":{},\"type\":\"\"}";
        assert_eq!(expected, json);

        span.error = 42;
        let json = serde_json::to_string(&span).unwrap();
        let expected = "{\"service\":\"\",\"name\":\"test\",\"resource\":\"\",\"trace_id\":0,\"span_id\":0,\"parent_id\":0,\"start\":0,\"duration\":0,\"error\":42,\"meta\":{},\"metrics\":{},\"type\":\"\"}";
        assert_eq!(expected, json);
    }

    use crate::pb::{ClientGroupedStats, ClientStatsBucket, ClientStatsPayload, Trilean::NotSet};

    #[cfg_attr(all(miri, target_os = "macos"), ignore)]
    #[tokio::test]
    async fn test_deserialize_client_stats_payload() {
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

        let deserialized_stats_json: ClientStatsPayload = serde_json::from_str(stats_json).unwrap();

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

        assert_eq!(deserialized_stats_json, client_stats_payload)
    }
}
