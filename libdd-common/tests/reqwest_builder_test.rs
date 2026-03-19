// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "reqwest")]
mod reqwest_tests {
    use libdd_common::test_utils::{
        count_active_threads, create_temp_file_path, parse_http_request,
    };
    use libdd_common::Endpoint;

    /// With rustls-no-provider, reqwest does not auto-install a crypto provider.
    /// Tests that build a reqwest client must ensure one is installed first.
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

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
        ensure_crypto_provider();
        let file_path = create_temp_file_path("libdd_common_test", "http");

        let endpoint = Endpoint::from_slice(&format!("file://{}", file_path.display()));
        let (builder, url) = endpoint
            .to_reqwest_client_builder()
            .expect("should build client");
        let client = builder.build().expect("should create client");

        let test_body = "Hello from test!";
        let response = send_request(client, &url, test_body)
            .await
            .expect("request should succeed");

        assert_eq!(response.status(), 200);

        let captured = std::fs::read(&*file_path).expect("should read dump file");
        let request = parse_http_request(&captured)
            .await
            .expect("should parse captured request");

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/");
        assert_eq!(
            request.headers.get("content-type").map(|s| s.as_str()),
            Some("text/plain")
        );
        assert_eq!(
            request.headers.get("x-test-header").map(|s| s.as_str()),
            Some("test-value")
        );
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

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_both_resolver_configs_build_client() {
        ensure_crypto_provider();
        let url = "http://example.com/";
        for use_system_resolver in [false, true] {
            let endpoint = Endpoint::from_slice(url).with_system_resolver(use_system_resolver);
            let (builder, _) = endpoint
                .to_reqwest_client_builder()
                .expect("should build client");
            builder.build().expect("should create client");
        }
    }

    #[test]
    #[allow(dead_code)]
    #[ignore]
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
            "More threads survived than expected. {}",
            msg
        );
        assert!(
            threads_system_alive > threads_default_alive,
            "We expect the system resolver to use at least one more thread than the in-process resolver while the client is alive. {}",
            msg
        );
    }

    fn run_resolver_phase(url_slice: &str, use_system_resolver: bool) -> (usize, usize) {
        ensure_crypto_provider();
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

#[cfg(feature = "test-utils")]
mod resolve_tests {
    use libdd_common::test_utils::{create_temp_file_path, parse_http_request_sync};
    use libdd_common::{Endpoint, ResolvedEndpointKind};

    #[test]
    fn test_http_endpoint_resolution_preserves_settings() {
        for use_system_resolver in [false, true] {
            let endpoint = Endpoint::from_slice("http://example.com/")
                .with_timeout(1234)
                .with_system_resolver(use_system_resolver);

            let resolved = endpoint
                .resolve_for_http()
                .expect("should resolve endpoint");

            assert!(matches!(resolved.kind, ResolvedEndpointKind::Tcp));
            assert_eq!(resolved.request_url, "http://example.com/");
            assert_eq!(resolved.timeout, std::time::Duration::from_millis(1234));
            assert_eq!(resolved.use_system_resolver, use_system_resolver);
        }
    }

    #[test]
    fn test_unsupported_scheme_returns_error() {
        let endpoint = Endpoint::from_slice("ftp://example.com/file");

        let result = endpoint.resolve_for_http();

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Unsupported endpoint scheme"));
    }

    #[cfg(unix)]
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_file_dump_captures_http_request() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixStream;

        let file_path = create_temp_file_path("libdd_common_test", "http");
        let endpoint = Endpoint::from_slice(&format!("file://{}", file_path.display()));

        let resolved = endpoint
            .resolve_for_http()
            .expect("should resolve endpoint");
        assert_eq!(resolved.request_url, "http://localhost/");

        let socket_path = match resolved.kind {
            ResolvedEndpointKind::UnixSocket { path } => path,
            other => panic!("unexpected resolved endpoint kind: {:?}", other),
        };

        let mut stream = UnixStream::connect(socket_path).expect("connect to dump server");
        let request = b"POST / HTTP/1.1\r\nHost: localhost\r\nContent-Type: text/plain\r\nX-Test-Header: test-value\r\nContent-Length: 16\r\n\r\nHello from test!";
        stream.write_all(request).expect("write request");
        stream.shutdown(std::net::Shutdown::Write).ok();

        let mut response = Vec::new();
        stream.read_to_end(&mut response).expect("read response");
        assert!(response.starts_with(b"HTTP/1.1 200 OK"));

        let captured = std::fs::read(&*file_path).expect("should read dump file");
        let parsed = parse_http_request_sync(&captured).expect("should parse captured request");

        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.path, "/");
        assert_eq!(
            parsed.headers.get("content-type").map(String::as_str),
            Some("text/plain")
        );
        assert_eq!(
            parsed.headers.get("x-test-header").map(String::as_str),
            Some("test-value")
        );
        assert_eq!(parsed.body, b"Hello from test!");
    }
}
