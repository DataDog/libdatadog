// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Common test utilities for both hyper and reqwest exporters

use libdd_common::tag;
use libdd_profiling::exporter::Tag;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub fn default_tags() -> Vec<Tag> {
    vec![tag!("service", "php"), tag!("host", "bits")]
}

/// Helper function to extract event.json from multipart body
pub fn extract_event_json_from_multipart(body: &[u8]) -> serde_json::Value {
    let body_str = String::from_utf8_lossy(body);

    // Find the event.json section in the multipart body
    let lines: Vec<&str> = body_str.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        if line.contains(r#"filename="event.json""#) || line.contains("name=\"event\"") {
            // The JSON content is typically a few lines after the content-disposition header
            // Skip empty lines and content-type headers
            for potential_json_line in lines.iter().skip(i + 1) {
                let potential_json = potential_json_line.trim();
                if potential_json.starts_with('{') {
                    if let Ok(json) = serde_json::from_str(potential_json) {
                        return json;
                    }
                }
            }
        }
    }

    json!({})
}

/// Verify event JSON contains expected fields
pub fn verify_event_json(event_json: &serde_json::Value, expected_family: &str) {
    assert_eq!(event_json["family"], expected_family);
    assert_eq!(event_json["version"], "4");
    assert_eq!(event_json["internal"]["test_key"], "test_value");
    assert_eq!(
        event_json["internal"]["libdatadog_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(event_json["info"]["runtime"]["engine"], "ruby");
    assert_eq!(event_json["info"]["runtime"]["version"], "3.2.0");

    // Verify attachments
    assert!(event_json["attachments"].is_array());
    let attachments = event_json["attachments"].as_array().unwrap();
    assert!(attachments.contains(&json!("profile.pprof")));

    // Verify tags
    let tags_profiler = event_json["tags_profiler"].as_str().unwrap();
    assert!(tags_profiler.contains("service:php"));
    assert!(tags_profiler.contains("host:bits"));
    assert!(tags_profiler.contains("runtime_platform:"));
}

/// Create test metadata
pub fn test_metadata() -> (serde_json::Value, serde_json::Value) {
    let internal_metadata = json!({
        "test_key": "test_value",
        "libdatadog_version": env!("CARGO_PKG_VERSION"),
    });
    let info = json!({
        "runtime": {
            "engine": "ruby",
            "version": "3.2.0"
        }
    });
    (internal_metadata, info)
}

/// Setup a mock server for basic POST endpoint
pub async fn setup_basic_mock() -> MockServer {
    let mock_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/profiling/v1/input"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&mock_server)
        .await;
    mock_server
}

/// Setup a mock server with body capture
pub async fn setup_body_capture_mock() -> (MockServer, Arc<Mutex<Vec<u8>>>) {
    let mock_server = MockServer::start().await;
    let received_body = Arc::new(Mutex::new(Vec::new()));
    let received_body_clone = received_body.clone();

    Mock::given(method("POST"))
        .and(path("/profiling/v1/input"))
        .respond_with(move |req: &wiremock::Request| {
            *received_body_clone.lock().unwrap() = req.body.clone();
            ResponseTemplate::new(200)
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    (mock_server, received_body)
}

/// Setup a mock server with header capture for agentless testing
pub async fn setup_header_capture_mock(
) -> (MockServer, Arc<Mutex<Option<HashMap<String, Vec<String>>>>>) {
    let mock_server = MockServer::start().await;
    let received_headers = Arc::new(Mutex::new(None));
    let received_headers_clone = received_headers.clone();

    Mock::given(method("POST"))
        .and(path("/api/v2/profile"))
        .respond_with(move |req: &wiremock::Request| {
            // Convert wiremock headers to a simpler HashMap<String, Vec<String>>
            let headers: HashMap<String, Vec<String>> = req
                .headers
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        v.iter().map(|val| val.as_str().to_string()).collect(),
                    )
                })
                .collect();
            *received_headers_clone.lock().unwrap() = Some(headers);
            ResponseTemplate::new(200)
        })
        .expect(1)
        .mount(&mock_server)
        .await;

    (mock_server, received_headers)
}
