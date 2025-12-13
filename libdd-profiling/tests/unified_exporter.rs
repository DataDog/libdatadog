// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Tests for the unified exporter demonstrating both backends work with the same API

mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use libdd_profiling::exporter::{BackendType, ProfileExporter};
    use libdd_profiling::exporter::config;
    use libdd_profiling::internal::EncodedProfile;

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_both_backends_produce_same_results() {
        use serde_json::json;
        
        // Start separate mock servers for each backend
        let (hyper_mock_server, hyper_received_body) = common::setup_body_capture_mock().await;
        let (reqwest_mock_server, reqwest_received_body) = common::setup_body_capture_mock().await;

        // Prepare test data (shared across both backends)
        let additional_tags = vec![
            libdd_common::tag!("request_id", "12345"),
            libdd_common::tag!("env", "test"),
        ];
        
        let internal_metadata = json!({
            "profiler_version": "2.0.0",
            "feature_enabled": true,
        });
        
        let info = json!({
            "application": {
                "start_time": "2024-01-01T00:00:00Z",
                "env": "test"
            },
            "runtime": {
                "engine": "rust",
                "version": "1.75.0"
            }
        });

        // Test with hyper backend
        let hyper_uri = hyper_mock_server.uri();
        let additional_tags_clone = additional_tags.clone();
        let internal_metadata_clone = internal_metadata.clone();
        let info_clone = info.clone();
        let hyper_handle = tokio::task::spawn_blocking(move || {
            let base_url = hyper_uri.parse().unwrap();
            let endpoint = config::agent(base_url).expect("endpoint to construct");
            let exporter = ProfileExporter::new(
                "dd-trace-test",
                "1.0.0",
                "test-family",
                Some(vec![libdd_common::tag!("backend", "hyper")]),
                endpoint,
                BackendType::Hyper,
            )
            .expect("exporter to construct");

            assert_eq!(exporter.backend_type(), BackendType::Hyper);

            let profile = EncodedProfile::test_instance().expect("To get a profile");
            
            // Create file data for hyper backend
            let test_file_data = b"test file content for compression";
            let test_file = libdd_profiling::exporter::File {
                name: "test-file.txt",
                bytes: test_file_data,
            };
            
            let request = exporter
                .build(
                    profile,
                    &[test_file],
                    Some(&additional_tags_clone),
                    Some(internal_metadata_clone),
                    Some(info_clone),
                )
                .expect("request to be built");

            exporter.send(request, None).expect("send to succeed")
        });

        // Test with reqwest backend
        let reqwest_uri = reqwest_mock_server.uri();
        let reqwest_handle = tokio::task::spawn_blocking(move || {
            let base_url = reqwest_uri.parse().unwrap();
            let endpoint = config::agent(base_url).expect("endpoint to construct");
            let exporter = ProfileExporter::new(
                "dd-trace-test",
                "1.0.0",
                "test-family",
                Some(vec![libdd_common::tag!("backend", "reqwest")]),
                endpoint,
                BackendType::Reqwest,
            )
            .expect("exporter to construct");

            assert_eq!(exporter.backend_type(), BackendType::Reqwest);

            let profile = EncodedProfile::test_instance().expect("To get a profile");
            
            // Create file data for reqwest backend
            let test_file_data = b"test file content for compression";
            let test_file = libdd_profiling::exporter::File {
                name: "test-file.txt",
                bytes: test_file_data,
            };
            
            let request = exporter
                .build(
                    profile,
                    &[test_file],
                    Some(&additional_tags),
                    Some(internal_metadata),
                    Some(info),
                )
                .expect("request to be built");

            exporter.send(request, None).expect("send to succeed")
        });

        // Wait for both to complete
        let hyper_response = hyper_handle.await.unwrap();
        let reqwest_response = reqwest_handle.await.unwrap();

        // Both should return 200
        assert_eq!(hyper_response.status(), 200);
        assert_eq!(reqwest_response.status(), 200);

        // Verify both bodies were received
        let hyper_body = hyper_received_body.lock().unwrap().clone();
        let reqwest_body = reqwest_received_body.lock().unwrap().clone();
        assert!(!hyper_body.is_empty());
        assert!(!reqwest_body.is_empty());

        // Extract event JSON from both backends
        let hyper_event = common::extract_event_json_from_multipart(&hyper_body);
        let reqwest_event = common::extract_event_json_from_multipart(&reqwest_body);

        // Both should have the same basic structure and values
        assert_eq!(hyper_event["family"], "test-family");
        assert_eq!(reqwest_event["family"], "test-family");
        
        assert_eq!(hyper_event["version"], "4");
        assert_eq!(reqwest_event["version"], "4");

        // Both should have attachments (including the test file)
        assert_eq!(hyper_event["attachments"], reqwest_event["attachments"]);
        assert!(hyper_event["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("test-file.txt")));
        assert!(hyper_event["attachments"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("profile.pprof")));

        // Both should have the same profile start/end times (they use the same test instance)
        assert_eq!(hyper_event["start"], reqwest_event["start"]);
        assert_eq!(hyper_event["end"], reqwest_event["end"]);

        // Verify tags contain the backend-specific tag and additional tags
        let hyper_tags = hyper_event["tags_profiler"].as_str().unwrap();
        let reqwest_tags = reqwest_event["tags_profiler"].as_str().unwrap();
        assert!(hyper_tags.contains("backend:hyper"));
        assert!(reqwest_tags.contains("backend:reqwest"));
        assert!(hyper_tags.contains("request_id:12345"));
        assert!(reqwest_tags.contains("request_id:12345"));
        assert!(hyper_tags.contains("env:test"));
        assert!(reqwest_tags.contains("env:test"));

        // Both should have the custom internal metadata
        assert!(hyper_event["internal"]["libdatadog_version"].is_string());
        assert!(reqwest_event["internal"]["libdatadog_version"].is_string());
        assert_eq!(
            hyper_event["internal"]["libdatadog_version"],
            reqwest_event["internal"]["libdatadog_version"]
        );
        assert_eq!(hyper_event["internal"]["profiler_version"], "2.0.0");
        assert_eq!(reqwest_event["internal"]["profiler_version"], "2.0.0");
        assert_eq!(hyper_event["internal"]["feature_enabled"], true);
        assert_eq!(reqwest_event["internal"]["feature_enabled"], true);

        // Both should have the custom info metadata
        assert_eq!(
            hyper_event["info"]["application"]["start_time"],
            "2024-01-01T00:00:00Z"
        );
        assert_eq!(
            reqwest_event["info"]["application"]["start_time"],
            "2024-01-01T00:00:00Z"
        );
        assert_eq!(hyper_event["info"]["runtime"]["engine"], "rust");
        assert_eq!(reqwest_event["info"]["runtime"]["engine"], "rust");
        assert_eq!(hyper_event["info"]["runtime"]["version"], "1.75.0");
        assert_eq!(reqwest_event["info"]["runtime"]["version"], "1.75.0");
    }
}

