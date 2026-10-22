// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! reqwest-based HTTP backend (compiled when the `reqwest-backend` feature is active).

use crate::config::{HttpClientConfig, TransportConfig};
use crate::request::HttpMethod;
use crate::{HttpClientError, HttpRequest, HttpResponse};

/// A backend that sends HTTP requests via [`reqwest::Client`].
///
/// Holds a connection-pooling client that is reused across all requests.
#[derive(Debug)]
pub(crate) struct ReqwestBackend {
    client: reqwest::Client,
    /// Whether the client's base URL uses `https`, recorded at construction.
    base_url_is_https: bool,
}

impl super::Backend for ReqwestBackend {
    // Creates a `reqwest::Client` with connection pooling enabled.
    fn new(
        client_config: &HttpClientConfig,
        transport: TransportConfig,
    ) -> Result<Self, HttpClientError> {
        let mut builder = reqwest::Client::builder().timeout(client_config.timeout());
        let base_url_is_https = is_https_url(client_config.base_url());

        // Route reqwest HTTPS through the shared TLS configuration so the
        // compromised-key distrust check and the selected crypto provider apply
        // (see `crate::tls`). Without this, reqwest would build its own platform
        // verifier, bypassing the distrust wrapper.
        //
        // Only HTTPS base URLs get the preconfigured TLS backend: building the
        // configuration eagerly loads the system CA roots, which must not happen
        // for plain-HTTP clients in minimal environments that have no CA
        // certificates installed.
        #[cfg(any(feature = "https", feature = "fips"))]
        if base_url_is_https {
            builder = builder.tls_backend_preconfigured(crate::tls::build_tls_config()?);
        } else {
            // Plain-HTTP clients have no preconfigured TLS backend (see above),
            // so a redirect to an `https` URL would negotiate TLS on reqwest's
            // default verifier, bypassing the compromised-key distrust check.
            // Stop such redirects instead of following them: the redirect
            // response itself is returned to the caller and no TLS connection
            // to the redirected URL is ever made.
            builder = builder.redirect(reqwest::redirect::Policy::custom(|attempt| {
                // Custom policies do not inherit the default hop limit, so keep
                // parity with reqwest's default (`Policy::limited(10)`) to bound
                // loops: `previous()` includes the initial URL, so the default
                // allows 10 redirects and errors on the 11th.
                if attempt.previous().len() > 10 {
                    attempt.error("too many redirects")
                } else if attempt.url().scheme().eq_ignore_ascii_case("http") {
                    attempt.follow()
                } else {
                    attempt.stop()
                }
            }));
        }

        match transport {
            TransportConfig::Tcp => {}
            #[cfg(unix)]
            TransportConfig::UnixSocket(path) => {
                builder = builder.unix_socket(path);
            }
            #[cfg(windows)]
            TransportConfig::WindowsNamedPipe(pipe) => {
                builder = builder.windows_named_pipe(pipe);
            }
        }

        // Limit the number of idle connections kept in the pool, as a safety resource bound.
        builder = builder.pool_max_idle_per_host(crate::config::POOL_MAX_IDLE);

        if client_config.periodic() {
            // Pool connections with a small idle timeout instead of disabling pooling entirely,
            // so consecutive requests within a flush interval can still reuse a connection
            // while avoiding reusing connections the receiver may have closed.
            builder = builder.pool_idle_timeout(crate::config::PERIODIC_POOL_IDLE_TIMEOUT);
        }

        let client = builder
            .build()
            .map_err(|e| HttpClientError::InvalidConfig(error_chain(&e)))?;
        Ok(Self {
            client,
            base_url_is_https,
        })
    }

    async fn send(
        &self,
        request: HttpRequest,
        config: &HttpClientConfig,
    ) -> Result<HttpResponse, HttpClientError> {
        if is_https_url(&request.url) != self.base_url_is_https {
            return Err(HttpClientError::InvalidConfig(format!(
                "request URL '{}' does not use the same scheme as the client's base URL: \
                 requests are only accepted over the scheme the client was configured with",
                request.url
            )));
        }

        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Delete => reqwest::Method::DELETE,
            HttpMethod::Head => reqwest::Method::HEAD,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Options => reqwest::Method::OPTIONS,
        };

        let mut builder = request.headers.iter().fold(
            self.client.request(method, &request.url),
            |builder, (name, value)| builder.header(name, value),
        );

        if !request.multipart_parts.is_empty() && !request.body.is_empty() {
            return Err(HttpClientError::InvalidConfig(
                "request cannot have both multipart parts and a body".to_owned(),
            ));
        }

        if !request.multipart_parts.is_empty() {
            builder = builder.multipart(build_multipart_form(request.multipart_parts)?);
        } else if !request.body.is_empty() {
            builder = builder.body(request.body);
        }

        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }

        let response = builder.send().await.map_err(map_reqwest_error)?;

        let status = response.status().as_u16();

        // Collect headers before consuming the response body.
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| {
                let v = value.to_str().map_err(|_| {
                    HttpClientError::IoError(format!(
                        "response header '{}' contains non-UTF-8 value",
                        name
                    ))
                })?;
                Ok((name.as_str().to_string(), v.to_string()))
            })
            .collect::<Result<Vec<_>, HttpClientError>>()?;

        let body_bytes = response.bytes().await.map_err(map_reqwest_error)?;

        if config.treat_http_errors_as_errors() && status >= 400 {
            return Err(HttpClientError::RequestFailed {
                status,
                body: String::from_utf8_lossy(&body_bytes).into_owned(),
            });
        }

        Ok(HttpResponse {
            status_code: status,
            headers,
            body: body_bytes,
        })
    }
}

/// Convert our `MultipartPart` list into a `reqwest::multipart::Form`.
fn build_multipart_form(
    parts: Vec<crate::request::MultipartPart>,
) -> Result<reqwest::multipart::Form, HttpClientError> {
    parts
        .into_iter()
        .try_fold(reqwest::multipart::Form::new(), |form, part| {
            let mut reqwest_part = reqwest::multipart::Part::bytes(part.data.to_vec());

            if let Some(filename) = part.filename {
                reqwest_part = reqwest_part.file_name(filename);
            }

            if let Some(ct) = part.content_type {
                reqwest_part = reqwest_part
                    .mime_str(&ct)
                    .map_err(|e| HttpClientError::InvalidConfig(e.to_string()))?;
            }

            Ok(form.part(part.name, reqwest_part))
        })
}

/// Returns whether `url` is parsed with an `https` scheme.
fn is_https_url(url: &str) -> bool {
    reqwest::Url::parse(url)
        .map(|url| url.scheme().eq_ignore_ascii_case("https"))
        .unwrap_or(false)
}

/// Map a `reqwest::Error` to our `HttpClientError` variants.
///
/// The message includes the whole error source chain, since relevant details
/// (e.g. TLS rejection reasons) only appear in nested sources.
fn map_reqwest_error(e: reqwest::Error) -> HttpClientError {
    if e.is_timeout() {
        HttpClientError::TimedOut
    } else if e.is_connect() {
        HttpClientError::ConnectionFailed(error_chain(&e))
    } else {
        HttpClientError::IoError(error_chain(&e))
    }
}

/// Renders an error together with its whole source chain.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(error) = source {
        chain.push_str(": ");
        chain.push_str(&error.to_string());
        source = error.source();
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::HttpRequest;
    use crate::backend::Backend;
    use crate::config::{HttpClientConfig, TransportConfig};
    use crate::request::HttpMethod;
    use core::time::Duration;

    fn backend(base_url: &str) -> (ReqwestBackend, HttpClientConfig) {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let config = HttpClientConfig::new(base_url.to_owned(), Duration::from_secs(5));
        let backend = ReqwestBackend::new(&config, TransportConfig::Tcp).expect("valid config");
        (backend, config)
    }

    /// Asserts that `request_url` through a client built with `base_url` is
    /// rejected as a non-retryable scheme-mismatch configuration error.
    async fn assert_scheme_mismatch(base_url: &str, request_url: &str) {
        let (backend, config) = backend(base_url);
        let request = HttpRequest::new(HttpMethod::Get, request_url.to_owned());
        let error = backend.send(request, &config).await.expect_err(&format!(
            "request URL '{request_url}' must be rejected by a '{base_url}' client"
        ));
        assert!(
            matches!(error, HttpClientError::InvalidConfig(ref message) if message.contains("scheme")),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn rejects_request_urls_that_mismatch_the_base_scheme() {
        // HTTPS requests through a plain-HTTP client.
        assert_scheme_mismatch("http://localhost:8126", "https://example.com/").await;
        assert_scheme_mismatch("http://localhost:8126", " https://example.com/").await;
        assert_scheme_mismatch("http://localhost:8126", "ht	tps://example.com/").await;
        // Plain-HTTP requests through an HTTPS-based client.
        assert_scheme_mismatch("https://localhost:8126", "http://localhost:8126/ping").await;
        assert_scheme_mismatch("https://localhost:8126", " http://localhost:8126/ping").await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn matching_scheme_request_urls_pass_url_validation() {
        let (backend, config) = backend("http://localhost:8126");
        // Port 1 (discard protocol) is not served; the request must pass URL
        // validation and proceed to the refused connection.
        let request = HttpRequest::new(HttpMethod::Get, "http://127.0.0.1:1/ping".to_owned());
        if let Err(HttpClientError::InvalidConfig(message)) = backend.send(request, &config).await {
            panic!("request with a matching scheme must pass URL validation: {message}");
        }
    }
}
