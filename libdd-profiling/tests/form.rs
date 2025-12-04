// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

use libdd_profiling::exporter::{ProfileExporter, Request};
use libdd_profiling::internal::EncodedProfile;

fn multipart(
    exporter: &mut ProfileExporter,
    internal_metadata: Option<serde_json::Value>,
    info: Option<serde_json::Value>,
) -> Request {
    let profile = EncodedProfile::test_instance().expect("To get a profile");

    let additional_files = &[];

    let timeout: u64 = 10_000;
    exporter.set_timeout(timeout);

    let request = exporter
        .build(
            profile,
            additional_files,
            &[],
            None,
            internal_metadata,
            info,
        )
        .expect("request to be built");

    let actual_timeout = request.timeout().expect("timeout to exist");
    assert_eq!(actual_timeout, std::time::Duration::from_millis(timeout));
    request
}

#[cfg(test)]
mod tests {
    use crate::{common, multipart};
    use http_body_util::BodyExt;
    use libdd_profiling::exporter::*;
    use libdd_profiling::internal::EncodedProfile;
    use serde_json::json;

    fn default_tags() -> Vec<Tag> {
        common::default_tags()
    }

    fn parsed_event_json(request: Request) -> serde_json::Value {
        // Really hacky way of getting the event.json file contents, because I didn't want to
        // implement a full multipart parser and didn't find a particularly good
        // alternative. If you do figure out a better way, there's another copy of this code
        // in the profiling-ffi tests, please update there too :)
        let body = request.body();
        let body_bytes: String = String::from_utf8_lossy(
            &futures::executor::block_on(body.collect())
                .unwrap()
                .to_bytes(),
        )
        .to_string();
        let event_json = body_bytes
            .lines()
            .skip_while(|line| !line.contains(r#"filename="event.json""#))
            .nth(2)
            .unwrap();

        serde_json::from_str(event_json).unwrap()
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn multipart_agent() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let base_url = "http://localhost:8126".parse().expect("url to parse");
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let mut exporter = ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            "php",
            Some(default_tags()),
            endpoint,
        )
        .expect("exporter to construct");

        let request = multipart(&mut exporter, None, None);

        assert_eq!(
            request.uri().to_string(),
            "http://localhost:8126/profiling/v1/input"
        );

        let actual_headers = request.headers();
        assert!(!actual_headers.contains_key("DD-API-KEY"));
        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN").unwrap(),
            profiling_library_name
        );
        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN-VERSION").unwrap(),
            profiling_library_version
        );

        let parsed_event_json = parsed_event_json(request);
        assert_eq!(parsed_event_json["attachments"], json!(["profile.pprof"]));
        assert_eq!(parsed_event_json["endpoint_counts"], json!(null));
        assert_eq!(parsed_event_json["family"], json!("php"));
        assert_eq!(
            parsed_event_json["internal"],
            json!({"libdatadog_version": env!("CARGO_PKG_VERSION")})
        );
        let tags_profiler = parsed_event_json["tags_profiler"]
            .as_str()
            .unwrap()
            .split(',')
            .collect::<Vec<_>>();
        assert!(tags_profiler.contains(&"service:php"));
        assert!(tags_profiler.contains(&"host:bits"));
        let runtime_platform = tags_profiler
            .iter()
            .find(|tag| tag.starts_with("runtime_platform:"))
            .expect("runtime_platform tag should exist");
        assert!(
            runtime_platform.starts_with(&format!("runtime_platform:{}", std::env::consts::ARCH)),
            "expected platform tag to start with runtime_platform:{} but got '{}'",
            std::env::consts::ARCH,
            runtime_platform
        );
        assert_eq!(parsed_event_json["version"], json!("4"));
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn including_internal_metadata() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let base_url = "http://localhost:8126".parse().expect("url to parse");
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let mut exporter = ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            "php",
            Some(default_tags()),
            endpoint,
        )
        .expect("exporter to construct");

        let internal_metadata = json!({
            "no_signals_workaround_enabled": "true",
            "execution_trace_enabled": "false",
            "extra object": {"key": [1, 2, true]},
            "libdatadog_version": env!("CARGO_PKG_VERSION"),
        });
        let request = multipart(&mut exporter, Some(internal_metadata.clone()), None);
        let parsed_event_json = parsed_event_json(request);

        assert_eq!(parsed_event_json["internal"], internal_metadata);
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn including_info() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let base_url = "http://localhost:8126".parse().expect("url to parse");
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let mut exporter = ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            "php",
            Some(default_tags()),
            endpoint,
        )
        .expect("exporter to construct");

        let info = json!({
            "application": {
                "start_time": "2024-01-24T11:17:22+0000",
                "env": "test"
            },
            "runtime": {
                "engine": "ruby",
                "version": "3.2.0",
                "platform": "arm64-darwin22"
            },
            "profiler": {
                "version": "1.32.0",
                "libdatadog": "1.2.3-darwin",
                "settings": {}
            }
        });
        let request = multipart(&mut exporter, None, Some(info.clone()));
        let parsed_event_json = parsed_event_json(request);

        assert_eq!(parsed_event_json["info"], info);
    }

    #[test]
    // This test invokes an external function SecTrustSettingsCopyCertificates
    // which Miri cannot evaluate.
    #[cfg_attr(miri, ignore)]
    fn multipart_agentless() {
        let profiling_library_name = "dd-trace-foo";
        let profiling_library_version = "1.2.3";
        let api_key = "1234567890123456789012";
        let endpoint = config::agentless("datadoghq.com", api_key).expect("endpoint to construct");
        let mut exporter = ProfileExporter::new(
            profiling_library_name,
            profiling_library_version,
            "php",
            Some(default_tags()),
            endpoint,
        )
        .expect("exporter to construct");

        let request = multipart(&mut exporter, None, None);

        assert_eq!(
            request.uri().to_string(),
            "https://intake.profile.datadoghq.com/api/v2/profile"
        );

        let actual_headers = request.headers();

        assert_eq!(actual_headers.get("DD-API-KEY").unwrap(), api_key);

        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN").unwrap(),
            profiling_library_name
        );

        assert_eq!(
            actual_headers.get("DD-EVP-ORIGIN-VERSION").unwrap(),
            profiling_library_version
        );
    }

    // Integration tests with mock server

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_send_to_mock_server_hyper() {
        // Start a mock server
        let mock_server = common::setup_basic_mock().await;

        // Run in spawn_blocking to avoid nested runtime issue
        let mock_uri = mock_server.uri();
        let handle = tokio::task::spawn_blocking(move || {
            // Create exporter with mock server URL
            let base_url = mock_uri.parse().unwrap();
            let endpoint = config::agent(base_url).expect("endpoint to construct");
            let exporter = ProfileExporter::new(
                "dd-trace-foo",
                "1.2.3",
                "php",
                Some(default_tags()),
                endpoint,
            )
            .expect("exporter to construct");

            // Build request
            let profile = EncodedProfile::test_instance().expect("To get a profile");
            let request = exporter
                .build(profile, &[], &[], None, None, None)
                .expect("request to be built");

            // Send request
            exporter.send(request, None).expect("send to succeed")
        });

        let response = handle.await.unwrap();

        // Verify response
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_send_with_body_inspection_hyper() {
        // Start a mock server
        let (mock_server, received_body) = common::setup_body_capture_mock().await;

        // Run in spawn_blocking to avoid nested runtime issue
        let mock_uri = mock_server.uri();
        let handle = tokio::task::spawn_blocking(move || {
            // Create exporter with mock server URL
            let base_url = mock_uri.parse().unwrap();
            let endpoint = config::agent(base_url).expect("endpoint to construct");
            let exporter = ProfileExporter::new(
                "dd-trace-foo",
                "1.2.3",
                "ruby",
                Some(default_tags()),
                endpoint,
            )
            .expect("exporter to construct");

            // Build request with metadata
            let profile = EncodedProfile::test_instance().expect("To get a profile");
            let (internal_metadata, info) = common::test_metadata();

            let request = exporter
                .build(
                    profile,
                    &[],
                    &[],
                    None,
                    Some(internal_metadata.clone()),
                    Some(info.clone()),
                )
                .expect("request to be built");

            // Send request
            exporter.send(request, None).expect("send to succeed")
        });

        let response = handle.await.unwrap();
        assert_eq!(response.status(), 200);

        // Inspect the received body
        let body = received_body.lock().unwrap();
        let event_json = common::extract_event_json_from_multipart(&body);

        // Verify the event JSON contains expected fields
        common::verify_event_json(&event_json, "ruby");
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_agentless_with_api_key_hyper() {
        // Start a mock server
        let (mock_server, received_headers) = common::setup_header_capture_mock().await;

        // Run in spawn_blocking to avoid nested runtime issue
        let api_key = "test_api_key_12345678901234";
        let mock_url_str = format!("{}/api/v2/profile", mock_server.uri());
        let handle = tokio::task::spawn_blocking(move || {
            // Create an agentless-style endpoint but pointing to our mock server
            let mock_url = mock_url_str.parse().unwrap();
            let endpoint = libdd_common::Endpoint {
                url: mock_url,
                api_key: Some(api_key.into()),
                timeout_ms: 10_000,
                test_token: None,
            };

            let exporter = ProfileExporter::new("dd-trace-test", "2.0.0", "python", None, endpoint)
                .expect("exporter to construct");

            let profile = EncodedProfile::test_instance().expect("To get a profile");
            let request = exporter
                .build(profile, &[], &[], None, None, None)
                .expect("request to be built");

            exporter.send(request, None).expect("send to succeed")
        });

        let response = handle.await.unwrap();
        assert_eq!(response.status(), 200);

        // Verify API key was sent
        let headers = received_headers.lock().unwrap();
        let headers_map = headers.as_ref().unwrap();

        let api_key_header = headers_map
            .get("dd-api-key")
            .or_else(|| headers_map.get("DD-API-KEY"))
            .expect("API key header should be present");
        assert_eq!(api_key_header[0], api_key);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_timeout_configuration_hyper() {
        let mock_server = common::setup_basic_mock().await;

        // Run in spawn_blocking to avoid nested runtime issue
        let mock_uri = mock_server.uri();
        let handle = tokio::task::spawn_blocking(move || {
            let base_url = mock_uri.parse().unwrap();
            let endpoint = config::agent(base_url).expect("endpoint to construct");
            let mut exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "go", None, endpoint)
                .expect("exporter to construct");

            // Set custom timeout
            exporter.set_timeout(5000);

            let profile = EncodedProfile::test_instance().expect("To get a profile");
            let request = exporter
                .build(profile, &[], &[], None, None, None)
                .expect("request to be built");

            // Verify timeout is set correctly (state check)
            assert_eq!(
                request.timeout(),
                &Some(std::time::Duration::from_millis(5000))
            );

            exporter.send(request, None).expect("send to succeed")
        });

        let response = handle.await.unwrap();
        assert_eq!(response.status(), 200);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_timeout_actually_fires_hyper() {
        use std::time::Duration;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Mock that delays longer than the timeout
        Mock::given(method("POST"))
            .and(path("/profiling/v1/input"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .expect(1)
            .mount(&mock_server)
            .await;

        // Run in spawn_blocking to avoid nested runtime issue
        let mock_uri = mock_server.uri();
        let handle = tokio::task::spawn_blocking(move || {
            let base_url = mock_uri.parse().unwrap();
            let endpoint = config::agent(base_url).expect("endpoint to construct");
            let mut exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "go", None, endpoint)
                .expect("exporter to construct");

            // Set a very short timeout - 100ms
            exporter.set_timeout(100);

            let profile = EncodedProfile::test_instance().expect("To get a profile");
            let request = exporter
                .build(profile, &[], &[], None, None, None)
                .expect("request to be built");

            // This should timeout because the server delays for 10 seconds
            exporter.send(request, None)
        });

        let result = handle.await.unwrap();

        // Verify the request timed out
        assert!(result.is_err(), "Expected request to timeout");
        match result {
            Err(e) => {
                let error_msg = e.to_string();
                // Timeout errors should contain "timeout" or "timed out" in the message
                assert!(
                    error_msg.to_lowercase().contains("timeout")
                        || error_msg.to_lowercase().contains("timed out"),
                    "Error message should indicate timeout, got: {}",
                    error_msg
                );
            }
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }
}
