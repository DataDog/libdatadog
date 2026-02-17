// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "reqwest")]
mod tests {
    use libdd_common::test_utils::{create_temp_file_path, parse_http_request, EnvGuard};
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

    /// Client resolves host via hickory when DD_USE_HICKORY_DNS is set.
    /// Uses http://example.com/ so DNS is actually exercised; example.com is reserved by
    /// RFC 2606 for documentation and testing. These tests require network access.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_hickory_dns_env_enabled() {
        let _guard = EnvGuard::set("DD_USE_HICKORY_DNS", "1");
        let endpoint = Endpoint::from_slice("http://example.com/");

        let (builder, url) = endpoint
            .to_reqwest_client_builder()
            .expect("should build client");
        let client = builder.build().expect("should create client");

        let response = client.get(&url).send().await.expect("request should succeed");
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "status: {}",
            response.status()
        );
    }

    /// Client resolves host via system resolver when DD_USE_HICKORY_DNS is unset.
    /// Uses http://example.com/ so DNS is actually exercised; example.com is reserved by
    /// RFC 2606 for documentation and testing. These tests require network access.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn test_hickory_dns_env_disabled() {
        let _guard = EnvGuard::remove("DD_USE_HICKORY_DNS");
        let endpoint = Endpoint::from_slice("http://example.com/");

        let (builder, url) = endpoint
            .to_reqwest_client_builder()
            .expect("should build client");
        let client = builder.build().expect("should create client");

        let response = client.get(&url).send().await.expect("request should succeed");
        assert!(
            response.status().is_success() || response.status().is_redirection(),
            "status: {}",
            response.status()
        );
    }
}
