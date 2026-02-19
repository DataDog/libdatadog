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

    /// Both resolver configs produce a buildable reqwest client. Does not send a request (no
    /// network). Does not verify which resolver is actually used; that is done by
    /// test_system_resolver_uses_extra_thread.
    #[test]
    fn test_both_resolver_configs_build_client() {
        let url = "http://example.com/";
        for use_system_resolver in [false, true] {
            let endpoint = Endpoint::from_slice(url).with_system_resolver(use_system_resolver);
            let (builder, _) = endpoint
                .to_reqwest_client_builder()
                .expect("should build client");
            builder.build().expect("should create client");
        }
    }

    /// Verifies that the two resolver configs actually use different resolvers (default uses
    /// fewer threads than system). With the default (in-process) resolver, no extra thread is
    /// used for DNS; with the system resolver, reqwest uses a threadpool thread. Each phase
    /// runs in its own single-threaded tokio runtime (started then dropped). Requires network.
    /// Only runs on platforms where count_active_threads is implemented.
    ///
    /// TODO: Even the in-process resolver can lead to long-lived threads that outlast the
    /// runtime (e.g. on OSX the "Grand Central Dispatch" thread). This should be
    /// investigated so we can tighten or simplify the assertions.
    #[test]
    #[cfg_attr(miri, ignore)]
    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    fn test_system_resolver_uses_extra_thread() {
        let initial =
            count_active_threads().expect("count_active_threads not supported on this platform");

        let (threads_default_alive, threads_default_after_drop) =
            run_resolver_phase("http://example.com/", false);
        let (threads_system_alive, threads_system_after_drop) =
            run_resolver_phase("http://example.com/", true);

        let msg = format!(
            "initial={initial} default_alive={threads_default_alive} default_after_drop={threads_default_after_drop} system_alive={threads_system_alive} system_after_drop={threads_system_after_drop}",
        );

        assert!(
            threads_default_alive >= initial,
            "Sanity check: spawning the resolver should spawn threads. {}",
            msg
        );
        assert!(
            threads_default_after_drop <= initial + 2,
            "More threads survived than expected.  See TODO on this test. {}",
            msg
        );
        assert!(
            threads_system_alive > threads_default_alive,
            "We expect the system resolver to use at least one more thread than the in-process resolver while the client is alive. {}",
            msg
        );
        // After dropping the runtime, the system resolver's thread may or may not be reclaimed
        // depending on platform and timing; we only assert on the "alive" counts above.
    }

    /// Runs one resolver phase in a fresh single-threaded tokio runtime (started then dropped):
    /// build client with the given resolver setting, send one request, count threads with client
    /// alive, drop client, drop runtime, then count threads after drop. Returns (threads_alive,
    /// threads_after_drop).
    fn run_resolver_phase(url_slice: &str, use_system_resolver: bool) -> (usize, usize) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let alive = rt.block_on(async {
            let endpoint =
                Endpoint::from_slice(url_slice).with_system_resolver(use_system_resolver);
            let (builder, url) = endpoint
                .to_reqwest_client_builder()
                .expect("should build client");
            let client = builder.build().expect("should create client");
            let _ = client.get(&url).send().await;
            count_active_threads().expect("count_active_threads not supported on this platform")
        });
        drop(rt);
        let after_drop =
            count_active_threads().expect("count_active_threads not supported on this platform");
        (alive, after_drop)
    }
}
