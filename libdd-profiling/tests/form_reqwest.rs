// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

mod common;

#[cfg(test)]
mod tests {
    use crate::common;
    use libdd_profiling::exporter::config;
    use libdd_profiling::exporter::reqwest_exporter::*;
    use libdd_profiling::exporter::Tag;
    use libdd_profiling::internal::EncodedProfile;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Helper to create an exporter from a mock server
    fn create_exporter(
        mock_server: &MockServer,
        family: &str,
        tags: Vec<Tag>,
    ) -> ProfileExporter {
        let base_url = mock_server.uri().parse().unwrap();
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        ProfileExporter::new("dd-trace-test", "1.0.0", family, tags, endpoint)
            .expect("exporter to construct")
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_new_exporter() {
        let base_url = "http://localhost:8126".parse().expect("url to parse");
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let exporter =
            ProfileExporter::new("dd-trace-foo", "1.2.3", "php", common::default_tags(), endpoint);
        assert!(exporter.is_ok());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_send_with_all_features() {
        let (mock_server, received_body) = common::setup_body_capture_mock().await;
        let exporter = create_exporter(&mock_server, "ruby", common::default_tags());

        let profile = EncodedProfile::test_instance().expect("To get a profile");
        let (internal_metadata, info) = common::test_metadata();
        
        let test_file_data = b"additional file content";
        let files = &[File {
            name: "test.txt",
            bytes: test_file_data,
        }];
        
        let additional_tags = vec![
            libdd_common::tag!("version", "1.0.0"),
            libdd_common::tag!("region", "us-east-1"),
        ];

        let status = exporter
            .send(
                profile,
                files,
                &additional_tags,
                Some(internal_metadata),
                Some(info),
                None,
            )
            .await
            .expect("send to succeed");
        assert_eq!(status, 200);

        let body = received_body.lock().unwrap();
        let event_json = common::extract_event_json_from_multipart(&body);
        common::verify_event_json(&event_json, "ruby");
        
        // Verify the file content matches what we sent (files are zstd-compressed)
        let extracted_file = common::extract_file_from_multipart(&body, "test.txt")
            .expect("test.txt should be in multipart body");
        let decompressed = common::decompress_zstd(&extracted_file)
            .expect("should decompress file");
        assert_eq!(decompressed, test_file_data);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_agentless_with_api_key() {
        let (mock_server, received_headers) = common::setup_header_capture_mock().await;

        let api_key = "test_api_key_12345678901234";
        let mock_url = format!("{}/api/v2/profile", mock_server.uri())
            .parse()
            .unwrap();
        let endpoint = libdd_common::Endpoint {
            url: mock_url,
            api_key: Some(api_key.into()),
            timeout_ms: 10_000,
            test_token: None,
        };

        let exporter = ProfileExporter::new("dd-trace-test", "2.0.0", "python", vec![], endpoint)
            .expect("exporter to construct");

        let profile = EncodedProfile::test_instance().expect("To get a profile");
        let status = exporter
            .send(profile, &[], &[], None, None, None)
            .await
            .expect("send to succeed");
        assert_eq!(status, 200);

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
    async fn test_custom_timeout() {
        let mock_server = common::setup_basic_mock().await;

        let base_url = mock_server.uri().parse().unwrap();
        let mut endpoint = config::agent(base_url).expect("endpoint to construct");
        endpoint.timeout_ms = 5000;

        let exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "go", vec![], endpoint)
            .expect("exporter to construct");

        let profile = EncodedProfile::test_instance().expect("To get a profile");
        let status = exporter
            .send(profile, &[], &[], None, None, None)
            .await
            .expect("send to succeed");
        assert_eq!(status, 200);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_timeout_actually_fires() {
        use std::time::Duration;

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/profiling/v1/input"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(10)))
            .expect(1)
            .mount(&mock_server)
            .await;

        let base_url = mock_server.uri().parse().unwrap();
        let mut endpoint = config::agent(base_url).expect("endpoint to construct");
        endpoint.timeout_ms = 100; // Set very short timeout on endpoint

        let exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "go", vec![], endpoint)
            .expect("exporter to construct");

        let profile = EncodedProfile::test_instance().expect("To get a profile");
        let result = exporter.send(profile, &[], &[], None, None, None).await;

        assert!(result.is_err(), "Expected request to timeout and fail");

        match result {
            Err(e) => {
                let error_msg = e.to_string();
                assert!(
                    error_msg.contains("error sending request")
                        || error_msg.to_lowercase().contains("timeout")
                        || error_msg.to_lowercase().contains("timed out"),
                    "Error should be a request/timeout error, got: {}",
                    error_msg
                );
            }
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }


    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_cancellation() {
        use std::time::Duration;
        use tokio_util::sync::CancellationToken;

        let mock_server = MockServer::start().await;

        // Set up a mock that responds slowly
        Mock::given(method("POST"))
            .and(path("/profiling/v1/input"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
            .expect(1)
            .mount(&mock_server)
            .await;

        let base_url = mock_server.uri().parse().unwrap();
        let endpoint = config::agent(base_url).expect("endpoint to construct");
        let exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "go", vec![], endpoint)
            .expect("exporter to construct");

        let cancel_token = CancellationToken::new();
        let cancel_token_clone = cancel_token.clone();

        // Cancel after 100ms
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel_token_clone.cancel();
        });

        let profile = EncodedProfile::test_instance().expect("To get a profile");
        let result = exporter
            .send(profile, &[], &[], None, None, Some(&cancel_token))
            .await;

        assert!(result.is_err(), "Expected request to be cancelled");

        match result {
            Err(e) => {
                let error_msg = e.to_string();
                assert!(
                    error_msg.to_lowercase().contains("cancel"),
                    "Error should mention cancellation, got: {}",
                    error_msg
                );
            }
            Ok(_) => panic!("Expected error but got Ok"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_unix_domain_socket() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixListener;

        // Create a temporary socket path
        let socket_path = std::env::temp_dir().join(format!("test-{}.sock", std::process::id()));

        // Clean up any existing socket
        let _ = std::fs::remove_file(&socket_path);

        // Create a Unix socket server
        let listener = UnixListener::bind(&socket_path).expect("Failed to bind Unix socket");

        // Spawn a simple HTTP server on the Unix socket
        let server_handle = tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                // Read the HTTP request (just consume it)
                let mut buffer = vec![0u8; 4096];
                let _ = stream.read(&mut buffer).await;

                // Send a minimal HTTP response
                let response = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });

        // Give the server time to start
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Create an endpoint using Unix socket URI
        let unix_uri = libdd_common::connector::uds::socket_path_to_uri(&socket_path)
            .expect("Failed to create Unix URI");

        let endpoint = libdd_common::Endpoint {
            url: unix_uri,
            api_key: None,
            timeout_ms: 5_000,
            test_token: None,
        };

        let exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "rust", vec![], endpoint)
            .expect("exporter to construct");

        let profile = EncodedProfile::test_instance().expect("To get a profile");
        let result = exporter.send(profile, &[], &[], None, None, None).await;

        // Wait for server to finish
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), server_handle).await;

        // Clean up socket
        let _ = std::fs::remove_file(&socket_path);

        assert!(
            result.is_ok(),
            "Unix socket request should succeed: {:?}",
            result
        );
        assert_eq!(result.unwrap(), 200);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_profile_convenience_method() {
        use libdd_profiling::internal::Profile;

        let mock_server = common::setup_basic_mock().await;
        let exporter = create_exporter(&mock_server, "rust", vec![]);

        // Create a simple profile
        let profile = Profile::try_new(
            &[libdd_profiling::api::ValueType {
                r#type: "samples",
                unit: "count",
            }],
            Some(libdd_profiling::api::Period {
                r#type: libdd_profiling::api::ValueType {
                    r#type: "cpu",
                    unit: "nanoseconds",
                },
                value: 1000000,
            }),
        )
        .expect("Failed to create profile");

        // Use the convenience method to export
        let status = profile
            .export_to_endpoint(&exporter, &[], &[], None, None, None, None, None)
            .await
            .expect("export should succeed");

        assert_eq!(status, 200);
    }

    #[cfg(unix)]
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_file_dump_endpoint() {
        // Create a temporary directory for the dump
        let temp_dir = std::env::temp_dir();
        let dump_file = temp_dir.join(format!("profile_dump_{}.http", std::process::id()));

        // Create a file:// endpoint using the config helper
        let endpoint = config::file(dump_file.to_string_lossy().as_ref())
            .expect("Failed to create file endpoint");

        let exporter = ProfileExporter::new("dd-trace-test", "1.0.0", "rust", vec![], endpoint)
            .expect("exporter to construct");

        let profile = EncodedProfile::test_instance().expect("To get a profile");
        let result = exporter.send(profile, &[], &[], None, None, None).await;

        assert!(
            result.is_ok(),
            "File dump request should succeed: {:?}",
            result
        );
        assert_eq!(result.unwrap(), 200);

        // Give the server task time to write the file
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        // Check that a file was created (with timestamp suffix)
        let parent_dir = dump_file.parent().unwrap();
        let file_stem = dump_file.file_stem().unwrap().to_string_lossy();

        // Find files matching the pattern
        let mut found_dump = false;
        if let Ok(entries) = std::fs::read_dir(parent_dir) {
            for entry in entries.flatten() {
                let filename = entry.file_name();
                let filename_str = filename.to_string_lossy();
                if filename_str.starts_with(&*file_stem) && filename_str.ends_with(".http") {
                    // Verify the file has content (binary data is OK)
                    let content = std::fs::read(entry.path()).expect("Failed to read dump file");

                    // Verify it looks like an HTTP request (check the beginning as text)
                    // The content may contain binary data, so only check the start
                    if content.len() > 100 {
                        let header_part = String::from_utf8_lossy(&content[..100]);
                        assert!(header_part.starts_with("POST "), "Should be a POST request");

                        // Check if multipart/form-data appears somewhere in headers
                        let searchable =
                            String::from_utf8_lossy(&content[..content.len().min(2000)]);
                        assert!(
                            searchable.contains("multipart/form-data"),
                            "Should contain multipart form data"
                        );
                    }

                    found_dump = true;

                    // Clean up
                    let _ = std::fs::remove_file(entry.path());
                    break;
                }
            }
        }

        assert!(found_dump, "Should have found a dump file matching pattern");
    }
}
