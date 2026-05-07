// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Native HTTP client implementation backed by [`libdd_http_client`].

mod native {
    use std::sync::Arc;
    use std::time::Duration;

    use libdd_capabilities::http::{HttpClientTrait, HttpError};
    use libdd_capabilities::maybe_send::MaybeSend;

    use libdd_http_client::HttpClient;

    /// The native implementation of [`HttpClientTrait`].
    ///
    /// `DefaultHttpClient` wraps a [`libdd_http_client::HttpClient`] configured to use the
    /// `hyper-backend`. The hyper backend is pinned in `Cargo.toml` so that this crate (and
    /// all internal consumers of `HttpClientTrait`, including `libdd-data-pipeline`) routes
    /// through libdd-common's hyper stack — the same stack the previous `GenericHttpClient`
    /// implementation used. This avoids pulling in `reqwest` as a dependency of every
    /// internal HTTP consumer.
    ///
    /// # URL handling
    ///
    /// The wrapped `HttpClient` is built with a placeholder base URL; the actual URL is
    /// taken from each `http::Request`. Both `http(s)://`, `unix://<hex>`, and `windows:`
    /// schemes are supported — scheme dispatch happens inside libdd-common's `Connector`.
    ///
    /// # Status codes
    ///
    /// 4xx/5xx responses are returned as ordinary `http::Response<Bytes>` (status preserved),
    /// matching the previous `GenericHttpClient`-based behavior. Consumers branch on
    /// `status.is_client_error()` / `status.is_server_error()` themselves.
    ///
    /// # Construction failures
    ///
    /// `HttpClient::new` is fallible. To match the trait's infallible `new_client()`
    /// signature, any construction error is stashed in the struct and surfaced on the
    /// first `request()` call as `HttpError::Other`.
    #[derive(Clone)]
    pub struct DefaultHttpClient {
        // Arc'd because libdd_http_client::HttpClient is not Clone, and we need
        // DefaultHttpClient: Clone to satisfy HttpClientTrait.
        inner: Arc<Inner>,
    }

    /// Tokio's timer can panic on durations close to `Duration::MAX`. Use a year as
    /// "effectively no timeout" — callers wrap each `request()` with their own
    /// `tokio::time::timeout` anyway, matching the previous behavior where the hyper
    /// `GenericHttpClient` had no built-in per-request timeout.
    const EFFECTIVELY_NO_TIMEOUT: Duration = Duration::from_secs(60 * 60 * 24 * 365);

    /// Placeholder base URL passed to `HttpClient::new`. The hyper backend ignores the
    /// base URL and uses the per-request URL directly.
    const PLACEHOLDER_BASE_URL: &str = "http://placeholder.invalid";

    enum Inner {
        Ready(HttpClient),
        BuildFailed(String),
    }

    impl Inner {
        fn build() -> Self {
            // `treat_http_errors_as_errors(false)` preserves the previous behavior:
            // 4xx/5xx are passed through as `http::Response`, not converted to errors.
            let result = HttpClient::builder()
                .base_url(PLACEHOLDER_BASE_URL.to_owned())
                .timeout(EFFECTIVELY_NO_TIMEOUT)
                .treat_http_errors_as_errors(false)
                .build();
            match result {
                Ok(client) => Inner::Ready(client),
                Err(e) => Inner::BuildFailed(e.to_string()),
            }
        }
    }

    impl std::fmt::Debug for DefaultHttpClient {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let mut dbg = f.debug_struct("DefaultHttpClient");
            match &*self.inner {
                Inner::Ready(_) => {
                    dbg.field("state", &"ready");
                }
                Inner::BuildFailed(err) => {
                    dbg.field("state", &"build_failed").field("error", err);
                }
            }
            dbg.finish()
        }
    }

    impl HttpClientTrait for DefaultHttpClient {
        fn new_client() -> Self {
            Self {
                inner: Arc::new(Inner::build()),
            }
        }

        #[allow(clippy::manual_async_fn)]
        fn request(
            &self,
            req: http::Request<bytes::Bytes>,
        ) -> impl std::future::Future<Output = Result<http::Response<bytes::Bytes>, HttpError>> + MaybeSend
        {
            let inner = self.inner.clone();
            async move {
                let client = match &*inner {
                    Inner::Ready(c) => c,
                    Inner::BuildFailed(e) => {
                        return Err(HttpError::Other(anyhow::anyhow!(
                            "failed to build HttpClient: {e}"
                        )));
                    }
                };

                let http_request = boundary::http_to_libdd(req)?;
                let http_response = client
                    .send(http_request)
                    .await
                    .map_err(boundary::libdd_to_capability_err)?;
                boundary::libdd_to_http(http_response)
            }
        }
    }

    /// Boundary helpers translating between [`http`] crate types and
    /// [`libdd_http_client`] types.
    mod boundary {
        use bytes::Bytes;
        use libdd_capabilities::http::HttpError;
        use libdd_http_client::{HttpClientError, HttpMethod, HttpRequest, HttpResponse};

        pub(super) fn http_to_libdd(
            req: http::Request<Bytes>,
        ) -> Result<HttpRequest, HttpError> {
            let (parts, body) = req.into_parts();

            let method = method_to_libdd(&parts.method)?;
            let url = parts.uri.to_string();

            let mut request = HttpRequest::new(method, url).with_body(body);

            for (name, value) in parts.headers.iter() {
                let value_str = value.to_str().map_err(|_| {
                    HttpError::InvalidRequest(anyhow::anyhow!(
                        "request header '{}' contains non-UTF-8 value",
                        name
                    ))
                })?;
                request = request.with_header(name.as_str(), value_str);
            }

            Ok(request)
        }

        pub(super) fn libdd_to_http(
            response: HttpResponse,
        ) -> Result<http::Response<Bytes>, HttpError> {
            let mut builder = http::Response::builder().status(response.status_code());
            for (name, value) in response.headers() {
                builder = builder.header(name, value);
            }
            builder
                .body(response.body().clone())
                .map_err(|e| HttpError::Other(anyhow::anyhow!(e)))
        }

        pub(super) fn libdd_to_capability_err(err: HttpClientError) -> HttpError {
            match err {
                HttpClientError::TimedOut => HttpError::Timeout,
                HttpClientError::ConnectionFailed(msg) => {
                    HttpError::Network(anyhow::anyhow!(msg))
                }
                HttpClientError::IoError(msg) => HttpError::Network(anyhow::anyhow!(msg)),
                HttpClientError::InvalidConfig(msg) => {
                    HttpError::InvalidRequest(anyhow::anyhow!(msg))
                }
                // We construct the underlying client with treat_http_errors_as_errors(false),
                // so this variant should not occur in practice. Map it to `Other` defensively
                // rather than synthesizing a fake response.
                HttpClientError::RequestFailed { status, body } => HttpError::Other(
                    anyhow::anyhow!("unexpected RequestFailed: status {status}: {body}"),
                ),
            }
        }

        fn method_to_libdd(method: &http::Method) -> Result<HttpMethod, HttpError> {
            match *method {
                http::Method::GET => Ok(HttpMethod::Get),
                http::Method::POST => Ok(HttpMethod::Post),
                http::Method::PUT => Ok(HttpMethod::Put),
                http::Method::DELETE => Ok(HttpMethod::Delete),
                http::Method::HEAD => Ok(HttpMethod::Head),
                http::Method::PATCH => Ok(HttpMethod::Patch),
                http::Method::OPTIONS => Ok(HttpMethod::Options),
                ref other => Err(HttpError::InvalidRequest(anyhow::anyhow!(
                    "unsupported HTTP method: {other}"
                ))),
            }
        }
    }

}

pub use native::DefaultHttpClient;
