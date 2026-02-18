// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "reqwest")]
mod tests {
    use libdd_common::test_utils::{
        count_active_threads, create_temp_file_path, parse_http_request,
    };
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

    /// Client resolves host via hickory when use_system_resolver is false.
    /// Uses http://example.com/ so DNS is actually exercised; example.com is reserved by
    /// RFC 2606 for documentation and testing. These tests require network access.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_hickory_dns_when_system_resolver_disabled() {
        let endpoint = Endpoint::from_slice("http://example.com/").with_system_resolver(false);

        let (builder, url) = endpoint
            .to_reqwest_client_builder()
            .expect("should build client");
        let client = builder.build().expect("should create client");

        let response = client
            .get(&url)
            .send()
            .await
            .expect("request should succeed");
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "status: {}",
            response.status()
        );
    }

    /// Client resolves host via system resolver when with_system_resolver(true) is used.
    /// Uses http://example.com/ so DNS is actually exercised; example.com is reserved by
    /// RFC 2606 for documentation and testing. These tests require network access.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_system_resolver_when_requested() {
        let endpoint = Endpoint::from_slice("http://example.com/").with_system_resolver(true);

        let (builder, url) = endpoint
            .to_reqwest_client_builder()
            .expect("should build client");
        let client = builder.build().expect("should create client");

        let response = client
            .get(&url)
            .send()
            .await
            .expect("request should succeed");
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "status: {}",
            response.status()
        );
    }

    /// With hickory DNS, no extra thread is used for resolution; with the system resolver,
    /// reqwest uses a threadpool thread. We count before/after dropping each client to show
    /// hickory leaves no extra thread and the system resolver adds one while its client is alive.
    /// Uses http://example.com/ so DNS is exercised; example.com is reserved by RFC 2606.
    /// Requires network. Only runs on platforms where count_active_threads is implemented.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    async fn test_hickory_dns_saves_thread() {
        let endpoint = Endpoint::from_slice("http://example.com/");
        let count =
            || count_active_threads().expect("count_active_threads not supported on this platform");

        let initial = count();

        // Phase 1: hickory — count with client alive, then after drop.
        let (threads_hickory_alive, threads_after_hickory_drop) = {
            let (builder, url) = endpoint
                .clone()
                .with_system_resolver(false)
                .to_reqwest_client_builder()
                .expect("should build client");
            let client = builder.build().expect("should create client");
            let _ = client.get(&url).send().await;
            let alive = count();
            drop(client);
            let after_drop = count();
            (alive, after_drop)
        };

        // Expected relationship for hickory: no persistent resolver thread.
        // After drop we're back to initial; alive count is at least initial and at least
        // after_drop.
        assert_eq!(threads_after_hickory_drop, initial);
        assert!(threads_hickory_alive >= initial);
        assert!(threads_after_hickory_drop <= threads_hickory_alive);

        // Phase 2: system resolver — count with client alive, then after drop.
        let (threads_system_alive, threads_after_system_drop) = {
            let (builder, url) = endpoint
                .with_system_resolver(true)
                .to_reqwest_client_builder()
                .expect("should build client");
            let client = builder.build().expect("should create client");
            let _ = client.get(&url).send().await;
            let alive = count();
            drop(client);
            let after_drop = count();
            (alive, after_drop)
        };

        assert_eq!(threads_after_system_drop, initial + 1);
        assert_eq!(threads_system_alive, initial + 1);
        assert!(threads_after_system_drop <= threads_system_alive);
    }
}
