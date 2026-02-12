// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "reqwest")]
mod tests {
    use libdd_common::test_utils::{create_temp_file_path, parse_http_request};
    use libdd_common::Endpoint;

    /// Helper to send a simple HTTP request and return the response
    async fn send_request(
        client: reqwest::Client,
        url: &str,
        body: &str,
    ) -> anyhow::Result<reqwest::Response> {
        Ok(client
            .post(url)
            .header("Content-Type", "text/plain")
            .header("X-Test-Header", "test-value")
            .body(body.to_string())
            .send()
            .await?)
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_file_dump_captures_http_request() {
        let file_path = create_temp_file_path("libdd_common_test", "http");

        // Create endpoint with file:// scheme
        let endpoint = Endpoint::from_slice(&format!("file://{}", file_path.display()));

        // Build reqwest client
        let (builder, url) = endpoint
            .to_reqwest_client_builder()
            .expect("should build client");
        let client = builder.build().expect("should create client");

        // Send a simple request
        let test_body = "Hello from test!";
        let response = send_request(client, &url, test_body)
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), 200);

        // Read the captured request
        // No sleep needed - the server only sends 200 after file.sync_all() completes
        let captured = std::fs::read(&*file_path).expect("should read dump file");

        // Parse and validate
        let request = parse_http_request(&captured)
            .await
            .expect("should parse captured request");

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/");

        // Find our custom headers
        assert_eq!(
            request.headers.get("content-type").map(|s| s.as_str()),
            Some("text/plain")
        );
        assert_eq!(
            request.headers.get("x-test-header").map(|s| s.as_str()),
            Some("test-value")
        );

        // Validate body
        assert_eq!(request.body, test_body.as_bytes());
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_unsupported_scheme_returns_error() {
        let endpoint = Endpoint::from_slice("ftp://example.com/file");

        let result = endpoint.to_reqwest_client_builder();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unsupported endpoint scheme"));
    }

    //#[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_dns_resolution_does_not_spawn_threads() {
        use libdd_common::test_utils::count_active_threads;

        // Count threads before DNS resolution
        let threads_before = count_active_threads().expect("Failed to count threads before");

        // Create an endpoint that will trigger DNS resolution
        // Using httpbin.org which is a reliable test service
        let endpoint = Endpoint::from_slice("https://httpbin.org").with_timeout(5000);

        // Build reqwest client from endpoint
        let (builder, url) = endpoint
            .to_reqwest_client_builder()
            .expect("Failed to create reqwest client builder");
        assert_eq!(threads_before, count_active_threads().unwrap());

        let client = builder.build().expect("Failed to build reqwest client");
        assert_eq!(threads_before, count_active_threads().unwrap());

        // Make a request that will trigger DNS resolution
        // The request may succeed or fail, but DNS resolution will definitely happen
        let _ = client.get(&format!("{}/get", url)).send().await;
        assert_eq!(threads_before, count_active_threads().unwrap());

        // Give any potential threads a moment to spawn if they were going to
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        assert_eq!(threads_before, count_active_threads().unwrap());

        // Count threads after DNS resolution
        let threads_after = count_active_threads().expect("Failed to count threads after");

        // With hickory-dns, DNS resolution should not spawn additional threads.
        // The count should remain the same. We allow a diff of 1 to account for potential
        // tokio runtime or reqwest client initialization threads that might be created
        // independently of DNS resolution.
        let diff = if threads_after > threads_before {
            threads_after - threads_before
        } else {
            threads_before - threads_after
        };

        assert!(
            diff <= 1,
            "DNS resolution should not spawn threads (before: {}, after: {}, diff: {})",
            threads_before,
            threads_after,
            diff
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_dns_resolution_does_not_spawn_threads_single_threaded() {
        use libdd_common::test_utils::count_active_threads;

        // Create a single-threaded tokio runtime to avoid multi-threaded runtime worker threads
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create single-threaded runtime");

        rt.block_on(async {
            // Count threads before DNS resolution
            let threads_before = count_active_threads().expect("Failed to count threads before");

            // Create an endpoint that will trigger DNS resolution
            // Using httpbin.org which is a reliable test service
            let endpoint = Endpoint::from_slice("https://httpbin.org").with_timeout(5000);

            // Build reqwest client from endpoint
            let (builder, url) = endpoint
                .to_reqwest_client_builder()
                .expect("Failed to create reqwest client builder");
            assert_eq!(threads_before, count_active_threads().unwrap());

            let client = builder.build().expect("Failed to build reqwest client");
            assert_eq!(threads_before, count_active_threads().unwrap());

            // Make a request that will trigger DNS resolution
            // The request may succeed or fail, but DNS resolution will definitely happen
            let _ = client.get(&format!("{}/get", url)).send().await;
            assert_eq!(threads_before, count_active_threads().unwrap());

            // Give any potential threads a moment to spawn if they were going to
            tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
            assert_eq!(threads_before, count_active_threads().unwrap());

            // Count threads after DNS resolution
            let threads_after = count_active_threads().expect("Failed to count threads after");

            // With hickory-dns and a single-threaded runtime, DNS resolution should not spawn
            // any additional threads. The count should remain exactly the same.
            assert_eq!(
                threads_before,
                threads_after,
                "DNS resolution should not spawn threads with single-threaded runtime (before: {}, after: {})",
                threads_before,
                threads_after
            );
        });
    }
}
