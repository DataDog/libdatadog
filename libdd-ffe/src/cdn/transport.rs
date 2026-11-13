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
use libdd_regex::Regex;
use percent_encoding::percent_decode_str;
use std::future::Future;
use std::io::Read;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_RESPONSE_BODY_BYTES: usize = 10 << 20;
const INTERNAL_UNTRACED_HEADER: &str = "DD-Internal-Untraced-Request";
const API_KEY_HEADER: &str = "DD-API-KEY";
const AUTHORIZATION_HEADER: &str = "Authorization";
const SERVICE_HEADER: &str = "DD-Service";

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
    service: Option<String>,
}

impl HyperTransport {
    pub(super) fn new(language: String, version: String, service: Option<String>) -> Self {
        Self {
            // Leave redirects visible: credentials must never be forwarded
            // to a location supplied by the server.
            client: http_common::new_client_periodic(),
            language,
            version,
            service: usage_service(service),
        }
    }

    async fn get_inner(
        &self,
        endpoint: &CdnEndpoint,
        timeout: Duration,
        etag: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<PollResponse, TransportFailure> {
        let request = build_request(
            endpoint,
            etag,
            &self.language,
            &self.version,
            self.service.as_deref(),
        )?;
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
    service: Option<&str>,
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

    // The CDN uses this header to attribute usage to the service.
    if let Some(service) = service {
        builder = builder.header(SERVICE_HEADER, service);
    }
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

const MAX_SERVICE_LEN: usize = 100;

/// Returns the `DD-Service` value: the name APM shows for the service, or
/// `None`.
///
/// APM shows the normalized name ([`normalize_service`]), so `My Service`
/// becomes `my_service`. If that name is empty or is not printable ASCII
/// (`café-api`, `支付`), the request has no header. A different name would
/// attribute usage to a service that APM does not show.
///
/// The CDN alone decides which values it accepts. It drops the others. The
/// `http` crate accepts UTF-8 header values, but some tracer HTTP clients reject
/// them, so all SDKs send only printable ASCII. A service then gets the same
/// header in each language. The returned value is always a valid header
/// value, so a service cannot make requests fail.
fn usage_service(service: Option<String>) -> Option<String> {
    let service = service.filter(|service| !service.is_empty())?;
    let normalized = normalize_service(&service);
    let accepted = !normalized.is_empty() && normalized.bytes().all(|byte| byte.is_ascii_graphic());
    if !accepted {
        log::debug!(
            "Feature Flags: service {service:?} has no ASCII APM name; DD-Service header not sent"
        );
    }
    accepted.then_some(normalized)
}

/// Normalizes a service name with the Datadog Agent rules for spans, so the
/// result is the name that APM shows. The rules come from `NormalizeService`
/// in pkg/trace/traceutil/normalize, v0.82.3. Where the Agent uses its
/// `unnamed-service` fallback, this function returns an empty string.
///
/// - Cut the name to 100 bytes or less. Do not split a character.
/// - Lowercase ASCII letters.
/// - Keep ASCII letters and digits, `:`, `.`, `/`, `-`, and non-ASCII letters and decimal digits
///   ([`is_letter_or_decimal_digit`]).
/// - Replace each run of other characters, `_` included, with one `_`.
/// - Remove a run at the end. Keep a run at the start: ` svc` becomes `_svc`.
///
/// This function does not lowercase non-ASCII letters. The header is the same:
/// the Agent lowercases a character only if its UTF-8 length does not change,
/// so a non-ASCII letter stays non-ASCII and the request has no header.
fn normalize_service(service: &str) -> String {
    let mut end = service.len().min(MAX_SERVICE_LEN);
    while !service.is_char_boundary(end) {
        end -= 1;
    }
    let mut normalized = String::with_capacity(end);
    let mut pending_underscore = false;
    for c in service[..end].chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase()
            || c.is_ascii_digit()
            || ":./-".contains(c)
            || (!c.is_ascii() && is_letter_or_decimal_digit(c))
        {
            if std::mem::take(&mut pending_underscore) {
                normalized.push('_');
            }
            normalized.push(c);
        } else {
            pending_underscore = true;
        }
    }
    normalized
}

/// Like `unicode.IsLetter(c) || unicode.IsDigit(c)` in Go, which the Agent uses:
/// general category L or Nd. std `is_alphanumeric` also accepts No (`²`, `½`),
/// Nl (`Ⅻ`) and other alphabetic marks (U+0345). The Agent replaces those, so
/// `a²b` becomes `a_b`, a plain ASCII name that the header can carry.
///
/// A character that the regex Unicode tables do not know (`\p{Cn}`) is kept too.
/// A newer Unicode version can make it a letter, which the Agent then keeps.
/// Keeping it sends no header, never a different name.
fn is_letter_or_decimal_digit(c: char) -> bool {
    static LETTER_OR_DECIMAL_DIGIT: OnceLock<Option<Regex>> = OnceLock::new();
    let mut buf = [0; 4];
    // The pattern is constant, so it compiles. If it did not, keeping the
    // character sends no header, never a different name.
    LETTER_OR_DECIMAL_DIGIT
        .get_or_init(|| Regex::new(r"^[\p{L}\p{Nd}\p{Cn}]$").ok())
        .as_ref()
        .is_none_or(|regex| regex.is_match(c.encode_utf8(&mut buf)))
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
            build_request(&managed, Some("etag"), "python", "1.0", Some("checkout"))
                .expect("managed request");
        assert_eq!(managed_request.headers()[API_KEY_HEADER], "secret");
        assert_eq!(managed_request.headers()[SERVICE_HEADER], "checkout");
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

        let custom_request =
            build_request(&custom, None, "php", "1.27", Some("checkout")).expect("custom request");
        assert!(!custom_request.headers().contains_key(API_KEY_HEADER));
        // A service name is not a secret, so custom endpoints also get it.
        assert_eq!(custom_request.headers()[SERVICE_HEADER], "checkout");
        assert_eq!(
            custom_request.headers()[AUTHORIZATION_HEADER],
            "Basic dXNlcjpwYXNzd29yZA=="
        );
        assert_eq!(custom_request.uri(), "https://localhost/custom");
    }

    #[test]
    fn service_header_is_omitted_without_a_service() {
        let endpoint =
            CdnEndpoint::build("", "datadoghq.com", "", "secret").expect("managed endpoint");
        crate::test_logging::start_log_capture();
        for service in [None, Some("")] {
            let service = usage_service(service.map(Into::into));
            let request =
                build_request(&endpoint, Some("etag"), "php", "1.27", service.as_deref()).unwrap();
            assert!(
                !request.headers().contains_key(SERVICE_HEADER),
                "{service:?}"
            );
        }
        // No service is not a rejected service, so there is no log.
        assert!(crate::test_logging::captured_logs().is_empty());
    }

    #[test]
    fn the_service_is_sent_as_the_name_apm_shows_or_not_at_all() {
        // The expected values come from the Datadog Agent: `NormalizeService`
        // in pkg/trace/traceutil/normalize v0.82.3. The header is sent only if
        // that name is not empty and is printable ASCII. `None` means no header.
        let a99 = "a".repeat(99);
        let a100 = "a".repeat(100);
        let ab99 = format!("a{}", "b".repeat(99));
        let ab100 = format!("a{}", "b".repeat(100));
        let a100_b = format!("{a100} b");
        let a99_e = format!("{a99}é");
        let e50 = "é".repeat(50);
        let e60 = "é".repeat(60);
        // (configured service, name APM shows, DD-Service header)
        let rows: [(&str, &str, Option<&str>); 41] = [
            ("checkout", "checkout", Some("checkout")),
            ("My Service", "my_service", Some("my_service")),
            ("My-API", "my-api", Some("my-api")),
            ("UPPER", "upper", Some("upper")),
            ("bad\nvalue", "bad_value", Some("bad_value")),
            ("a\tb", "a_b", Some("a_b")),
            ("a  b!", "a_b", Some("a_b")),
            ("svc ", "svc", Some("svc")),
            (" svc", "_svc", Some("_svc")),
            ("a_", "a", Some("a")),
            ("svc-", "svc-", Some("svc-")),
            ("a..b", "a..b", Some("a..b")),
            ("2fa-service", "2fa-service", Some("2fa-service")),
            ("9lives", "9lives", Some("9lives")),
            (
                "unnamed-service",
                "unnamed-service",
                Some("unnamed-service"),
            ),
            (
                "svc_name.v2:api/edge-1",
                "svc_name.v2:api/edge-1",
                Some("svc_name.v2:api/edge-1"),
            ),
            ("my__service", "my_service", Some("my_service")),
            (
                "Default Web Site/Shop",
                "default_web_site/shop",
                Some("default_web_site/shop"),
            ),
            (&ab99, &ab99, Some(&ab99)),
            (&ab100, &ab99, Some(&ab99)),
            (&a100_b, &a100, Some(&a100)),
            (&a99_e, &a99, Some(&a99)),
            ("_svc", "_svc", Some("_svc")),
            ("-svc", "-svc", Some("-svc")),
            (".svc", ".svc", Some(".svc")),
            (":svc", ":svc", Some(":svc")),
            ("café-api", "café-api", None),
            ("Café", "café", None),
            ("支付", "支付", None),
            ("a🐶b", "a_b", Some("a_b")),
            // Not general category L or Nd: the Agent replaces these.
            ("a²b", "a_b", Some("a_b")),
            ("zz½", "zz", Some("zz")),
            ("a①aZ0", "a_az0", Some("a_az0")),
            ("900\u{345}Z", "900_z", Some("900_z")),
            ("aⅫb", "a_b", Some("a_b")),
            ("İstanbul", "İstanbul", None),
            (&e60, &e50, None),
            ("___", "", None),
            ("-", "-", Some("-")),
            ("", "", None),
            ("   ", "", None),
        ];
        let endpoint = CdnEndpoint::custom("http://localhost/config").unwrap();
        for (service, apm, header) in rows {
            assert_eq!(normalize_service(service), apm, "{service:?}");
            crate::test_logging::start_log_capture();
            let sent = usage_service(Some(service.to_owned()));
            assert_eq!(sent.as_deref(), header, "{service:?}");
            let request = build_request(&endpoint, None, "php", "1.27", sent.as_deref())
                .expect("the header is a valid header value");
            assert_eq!(
                request
                    .headers()
                    .get(SERVICE_HEADER)
                    .map(|value| value.to_str().unwrap()),
                header,
                "{service:?}"
            );
            // A service that is set but not sent has one debug log.
            let logs = crate::test_logging::captured_logs();
            if header.is_none() && !service.is_empty() {
                assert_eq!(logs.len(), 1, "{service:?}");
                assert_eq!(logs[0].level, log::Level::Debug);
                assert_eq!(
                    logs[0].message,
                    format!(
                        "Feature Flags: service {service:?} has no ASCII APM name; DD-Service header not sent"
                    )
                );
            } else {
                assert!(logs.is_empty(), "{service:?}");
            }
        }

        // The raw value is not a valid header value. If it went out as is,
        // every request would fail.
        assert!(matches!(
            build_request(&endpoint, None, "php", "1.27", Some("bad\nvalue")),
            Err(TransportFailure::BuildRequest)
        ));
    }

    #[test]
    fn a_character_the_unicode_tables_do_not_know_sends_no_header() {
        // U+0378 is unassigned. A newer Unicode version can make it a letter,
        // which the Agent keeps, so `a_b` could be a name that APM does not show.
        assert_eq!(normalize_service("a\u{378}B"), "a\u{378}b");
        assert_eq!(usage_service(Some("a\u{378}B".to_owned())), None);
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
        let request = build_request(&endpoint, None, "php", "1.27", Some("checkout")).unwrap();
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
                | "dd-service"
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
            let request = build_request(&endpoint, None, "php", "1.27", None).unwrap();
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
            let request = build_request(&endpoint, None, "php", "1.27", None).unwrap();
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
        let request = build_request(&endpoint, None, "php", "1.27", None).unwrap();
        assert_eq!(request.uri(), "https://localhost/caf%C3%A9?query=ol%C3%A9");
        assert_eq!(
            request.headers()[AUTHORIZATION_HEADER],
            "Basic dXNlcjpww6Rzcw=="
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore = "tests HTTP cancellation, not unsafe memory access")]
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
        let transport = HyperTransport::new("php".into(), "1.27".into(), None);
        let request = tokio::spawn(async move {
            transport
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
        let response = HyperTransport::new("php".into(), "1.27".into(), None)
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
        let transport = HyperTransport::new("python".into(), "1.0".into(), None);
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
