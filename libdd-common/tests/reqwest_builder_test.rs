// Copyright 2021-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

#[cfg(feature = "test-utils")]
mod tests {
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
