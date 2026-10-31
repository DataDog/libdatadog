// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use super::CdnEndpoint;
use base64::Engine;
use flate2::read::MultiGzDecoder;
use http::header::{
    ACCEPT_ENCODING, CONTENT_ENCODING, CONTENT_LENGTH, ETAG, IF_NONE_MATCH, RETRY_AFTER,
};
use http::{Method, Request};
use http_body_util::BodyExt;
use libdd_networking::http_common::{self, Body};
use percent_encoding::percent_decode_str;
use std::future::Future;
use std::io::Read;
use std::pin::Pin;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_RESPONSE_BODY_BYTES: usize = 10 << 20;
const INTERNAL_UNTRACED_HEADER: &str = "DD-Internal-Untraced-Request";
const API_KEY_HEADER: &str = "DD-API-KEY";
const AUTHORIZATION_HEADER: &str = "Authorization";

pub(super) type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PollResponse, TransportFailure>> + Send + 'a>>;

pub(super) trait Transport: Send + Sync {
    fn get<'a>(
        &'a self,
        endpoint: &'a CdnEndpoint,
        timeout: Duration,
        etag: Option<&'a str>,
        cancellation: &'a CancellationToken,
    ) -> FetchFuture<'a>;
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum TransportFailure {
    BuildRequest,
    Cancelled,
    TimedOut,
    Request,
    ResponseBody,
    ResponseTooLarge,
}

pub(super) struct PollResponse {
    pub(super) status: u16,
    pub(super) retry_after: Option<String>,
    pub(super) etag: Option<String>,
    pub(super) content_encoding: Option<String>,
    pub(super) body: Vec<u8>,
}

pub(super) struct HyperTransport {
    client: http_common::HttpClient,
    language: String,
    version: String,
}

impl HyperTransport {
    pub(super) fn new(language: String, version: String) -> Self {
        Self {
            // Leave redirects visible: credentials must never be forwarded
            // to a location supplied by the server.
            client: http_common::new_client_periodic(),
            language,
            version,
        }
    }

    async fn get_inner(
        &self,
        endpoint: &CdnEndpoint,
        timeout: Duration,
        etag: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<PollResponse, TransportFailure> {
        let request = build_request(endpoint, etag, &self.language, &self.version)?;
        let operation = async {
            let response = self
                .client
                .request(request)
                .await
                .map_err(|_| TransportFailure::Request)?;
            let status = response.status().as_u16();
            let etag = header_value(response.headers(), ETAG);
            let retry_after = header_value(response.headers(), RETRY_AFTER);
            let content_encoding = header_value(response.headers(), CONTENT_ENCODING);

            // A non-200 body can never contain configuration. Dropping it here
            // also prevents a malicious error response from consuming memory.
            if status != 200 {
                return Ok(PollResponse {
                    status,
                    retry_after,
                    etag,
                    content_encoding,
                    body: Vec::new(),
                });
            }

            if response
                .headers()
                .get(CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .is_some_and(|length| length > MAX_RESPONSE_BODY_BYTES as u64)
            {
                return Err(TransportFailure::ResponseTooLarge);
            }

            let body = collect_bounded(response.into_body(), MAX_RESPONSE_BODY_BYTES).await?;
            Ok(PollResponse {
                status,
                retry_after,
                etag,
                content_encoding,
                body,
            })
        };

        tokio::select! {
            _ = cancellation.cancelled() => Err(TransportFailure::Cancelled),
            result = tokio::time::timeout(timeout, operation) => {
                result.map_err(|_| TransportFailure::TimedOut)?
            }
        }
    }
}

impl Transport for HyperTransport {
    fn get<'a>(
        &'a self,
        endpoint: &'a CdnEndpoint,
        timeout: Duration,
        etag: Option<&'a str>,
        cancellation: &'a CancellationToken,
    ) -> FetchFuture<'a> {
        Box::pin(self.get_inner(endpoint, timeout, etag, cancellation))
    }
}

fn build_request(
    endpoint: &CdnEndpoint,
    etag: Option<&str>,
    language: &str,
    version: &str,
) -> Result<Request<Body>, TransportFailure> {
    let (request_url, authorization) = request_url_and_authorization(endpoint.uri())?;
    // Keep CDN headers explicit. Endpoint's generic request builder also adds
    // entity/container headers, which are not part of this configuration API.
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(request_url)
        .header(ACCEPT_ENCODING, "gzip")
        .header("DD-Client-Library-Language", language)
        .header("DD-Client-Library-Version", version)
        .header(INTERNAL_UNTRACED_HEADER, "1");

    if let Some(etag) = etag {
        builder = builder.header(IF_NONE_MATCH, etag);
    }
    if let Some(api_key) = endpoint.api_key() {
        builder = builder.header(API_KEY_HEADER, api_key);
    }
    if let Some(authorization) = authorization {
        builder = builder.header(AUTHORIZATION_HEADER, authorization);
    }

    builder
        .body(Body::empty())
        .map_err(|_| TransportFailure::BuildRequest)
}

fn request_url_and_authorization(
    endpoint: &http::Uri,
) -> Result<(http::Uri, Option<String>), TransportFailure> {
    let url = url::Url::parse(&endpoint.to_string()).map_err(|_| TransportFailure::BuildRequest)?;
    let Some((_, host)) = endpoint
        .authority()
        .and_then(|authority| authority.as_str().rsplit_once('@'))
    else {
        return Ok((endpoint.clone(), None));
    };

    let mut credentials = percent_decode_str(url.username()).collect::<Vec<_>>();
    credentials.push(b':');
    if let Some(password) = url.password() {
        credentials.extend(percent_decode_str(password));
    }
    let authorization = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(credentials)
    );

    // Remove URL credentials without normalizing the operator's path/query.
    let mut parts = endpoint.clone().into_parts();
    parts.authority = Some(host.parse().map_err(|_| TransportFailure::BuildRequest)?);
    let uri = http::Uri::from_parts(parts).map_err(|_| TransportFailure::BuildRequest)?;
    Ok((uri, Some(authorization)))
}

fn header_value(headers: &http::HeaderMap, name: http::header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

async fn collect_bounded(
    mut body: hyper::body::Incoming,
    limit: usize,
) -> Result<Vec<u8>, TransportFailure> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| TransportFailure::ResponseBody)?;
        if let Some(data) = frame.data_ref() {
            let remaining = limit.saturating_sub(bytes.len());
            if data.len() > remaining {
                return Err(TransportFailure::ResponseTooLarge);
            }
            bytes.extend_from_slice(data);
        }
    }
    Ok(bytes)
}

pub(super) fn decode_response_body(
    body: &[u8],
    content_encoding: Option<&str>,
    limit: usize,
) -> Result<Vec<u8>, TransportFailure> {
    if !content_encoding.is_some_and(|value| value.trim().eq_ignore_ascii_case("gzip")) {
        if body.len() > limit {
            return Err(TransportFailure::ResponseTooLarge);
        }
        return Ok(body.to_vec());
    }

    let decoder = MultiGzDecoder::new(body);
    let mut decoded = Vec::new();
    decoder
        .take(limit as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| TransportFailure::ResponseBody)?;
    if decoded.len() > limit {
        return Err(TransportFailure::ResponseTooLarge);
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn gzip(payload: &[u8]) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(payload).expect("compress test payload");
        encoder.finish().expect("finish test payload")
    }

    #[test]
    fn decoded_body_limit_applies_after_gzip() {
        let payload = vec![b'a'; 256];
        let compressed = gzip(&payload);
        assert!(compressed.len() < 64);
        assert_eq!(
            decode_response_body(&compressed, Some(" GZIP "), 64),
            Err(TransportFailure::ResponseTooLarge)
        );
        assert_eq!(
            decode_response_body(&gzip(b"configuration"), Some("gzip"), 64).expect("decode gzip"),
            b"configuration"
        );
    }

    #[test]
    fn gzip_member_limits_and_trailing_bytes_are_checked() {
        let mut members = gzip(&[b'a'; 32]);
        members.extend(gzip(&[b'b'; 33]));
        assert_eq!(
            decode_response_body(&members, Some("gzip"), 64),
            Err(TransportFailure::ResponseTooLarge)
        );
        assert_eq!(
            decode_response_body(&members, Some("gzip"), 65)
                .unwrap()
                .len(),
            65
        );

        for suffix in [b"garbage".to_vec(), gzip(b"truncated")[..5].to_vec()] {
            let mut body = gzip(b"configuration");
            body.extend(suffix);
            assert_eq!(
                decode_response_body(&body, Some("gzip"), 64),
                Err(TransportFailure::ResponseBody)
            );
        }
    }

    #[test]
    fn request_authentication_is_managed_only() {
        let managed =
            CdnEndpoint::build("", "datadoghq.com", "", "secret").expect("managed endpoint");
        let custom = CdnEndpoint::build("https://user:password@localhost/custom", "", "", "secret")
            .expect("custom endpoint");

        let managed_request =
            build_request(&managed, Some("etag"), "python", "1.0").expect("managed request");
        assert_eq!(managed_request.headers()[API_KEY_HEADER], "secret");
        assert_eq!(
            managed_request.headers()["DD-Client-Library-Language"],
            "python"
        );
        assert_eq!(
            managed_request.headers()["DD-Client-Library-Version"],
            "1.0"
        );
        assert_eq!(managed_request.headers()[IF_NONE_MATCH], "etag");
        assert_eq!(managed_request.headers()[INTERNAL_UNTRACED_HEADER], "1");

        let custom_request = build_request(&custom, None, "php", "1.27").expect("custom request");
        assert!(!custom_request.headers().contains_key(API_KEY_HEADER));
        assert_eq!(
            custom_request.headers()[AUTHORIZATION_HEADER],
            "Basic dXNlcjpwYXNzd29yZA=="
        );
        assert_eq!(custom_request.uri(), "https://localhost/custom");
    }

    #[test]
    fn custom_basic_auth_preserves_the_exact_path_and_query() {
        let endpoint = CdnEndpoint::build(
            "https://us%65r:p%40ss@[::1]:8443/a/../Config%2fraw?tenant=a%2Fb&tenant=x+y&empty=",
            "ignored",
            "must-not-be-added",
            "must-not-be-sent",
        )
        .unwrap();
        let request = build_request(&endpoint, None, "php", "1.27").unwrap();
        assert_eq!(
            request.uri(),
            "https://[::1]:8443/a/../Config%2fraw?tenant=a%2Fb&tenant=x+y&empty="
        );
        assert_eq!(
            request.headers()[AUTHORIZATION_HEADER],
            "Basic dXNlcjpwQHNz"
        );
        assert!(!request.headers().contains_key(API_KEY_HEADER));
        assert!(request.headers().keys().all(|name| matches!(
            name.as_str(),
            "accept-encoding"
                | "dd-client-library-language"
                | "dd-client-library-version"
                | "dd-internal-untraced-request"
                | "authorization"
        )));
    }

    #[test]
    fn custom_basic_auth_accepts_password_only_credentials() {
        for (userinfo, expected) in [
            (":token", "Basic OnRva2Vu"),
            (":p%40ss", "Basic OnBAc3M="),
            (":p[ss", "Basic OnBbc3M="),
            (":päss", "Basic OnDDpHNz"),
            (":", "Basic Og=="),
            ("", "Basic Og=="),
        ] {
            let endpoint = CdnEndpoint::custom(&format!(
                "https://{userinfo}@[::1]:8443/a/../Config%2fraw?tenant=a%2Fb"
            ))
            .unwrap();
            let request = build_request(&endpoint, None, "php", "1.27").unwrap();
            assert_eq!(request.headers()[AUTHORIZATION_HEADER], expected);
            assert_eq!(
                request.uri(),
                "https://[::1]:8443/a/../Config%2fraw?tenant=a%2Fb"
            );
            assert!(!request.headers().contains_key(API_KEY_HEADER));
        }
    }

    #[test]
    fn custom_basic_auth_accepts_unescaped_password_characters() {
        for (password, expected_authorization) in [
            ("p[ss", "Basic dXNlcjpwW3Nz"),
            ("päss", "Basic dXNlcjpww6Rzcw=="),
        ] {
            let endpoint = CdnEndpoint::custom(&format!(
                "https://user:{password}@[::1]:8443/a/../Config%2fraw?tenant=a%2Fb"
            ))
            .unwrap();
            let request = build_request(&endpoint, None, "php", "1.27").unwrap();
            assert_eq!(
                request.headers()[AUTHORIZATION_HEADER],
                expected_authorization
            );
            assert_eq!(
                request.uri(),
                "https://[::1]:8443/a/../Config%2fraw?tenant=a%2Fb"
            );
            assert!(!request.headers().contains_key(API_KEY_HEADER));
        }

        let endpoint = CdnEndpoint::custom("https://user:päss@localhost/café?query=olé").unwrap();
        let request = build_request(&endpoint, None, "php", "1.27").unwrap();
        assert_eq!(request.uri(), "https://localhost/caf%C3%A9?query=ol%C3%A9");
        assert_eq!(
            request.headers()[AUTHORIZATION_HEADER],
            "Basic dXNlcjpww6Rzcw=="
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_inflight_http_request() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint =
            CdnEndpoint::custom(&format!("http://{}/config", listener.local_addr().unwrap()))
                .unwrap();
        let (received, request_received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            received.send(()).unwrap();
            // Keep the response pending beyond the client's HTTP deadline.
            std::future::pending::<()>().await;
        });
        let cancellation = CancellationToken::new();
        let stop = cancellation.clone();
        let request = tokio::spawn(async move {
            HyperTransport::new("php".into(), "1.27".into())
                .get(&endpoint, Duration::from_secs(30), None, &stop)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), request_received)
            .await
            .unwrap()
            .unwrap();
        cancellation.cancel();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), request)
                .await
                .unwrap()
                .unwrap(),
            Err(TransportFailure::Cancelled)
        ));
        server.abort();
    }

    #[tokio::test]
    async fn retry_after_survives_discarding_an_error_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint =
            CdnEndpoint::custom(&format!("http://{}/config", listener.local_addr().unwrap()))
                .unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 10\r\nContent-Length: 100000000\r\n\r\n").await.unwrap();
        });
        let response = HyperTransport::new("php".into(), "1.27".into())
            .get(
                &endpoint,
                Duration::from_secs(5),
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 429);
        assert_eq!(response.retry_after.as_deref(), Some("10"));
        assert!(response.body.is_empty());
        server.await.unwrap();
    }

    #[tokio::test]
    async fn native_http_transport_does_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = CdnEndpoint::custom(&format!(
            "http://user:password@{}/config",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let location = format!("http://{}/redirected", destination.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert!(socket.read(&mut request).await.unwrap() > 0);
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let transport = HyperTransport::new("python".into(), "1.0".into());
        let response = transport
            .get(
                &endpoint,
                Duration::from_secs(5),
                None,
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(response.status, 302);
        assert!(
            tokio::time::timeout(Duration::from_millis(100), destination.accept())
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}
