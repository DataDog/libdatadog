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
#[cfg(feature = "reqwest-backend")]
pub(crate) struct ReqwestBackend {
    client: reqwest::Client,
}

#[cfg(feature = "reqwest-backend")]
impl ReqwestBackend {
    /// Construct a new backend with the given timeout and transport.
    ///
    /// Creates a `reqwest::Client` with connection pooling enabled.
    pub(crate) fn new(
        timeout: std::time::Duration,
        transport: TransportConfig,
    ) -> Result<Self, HttpClientError> {
        let mut builder = reqwest::Client::builder().timeout(timeout);

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

        let client = builder
            .build()
            .map_err(|e| HttpClientError::InvalidConfig(e.to_string()))?;
        Ok(Self { client })
    }
}

#[cfg(feature = "reqwest-backend")]
impl super::Backend for ReqwestBackend {
    async fn send(
        &self,
        request: HttpRequest,
        config: &HttpClientConfig,
    ) -> Result<HttpResponse, HttpClientError> {
        let method = match request.method {
            HttpMethod::Get => reqwest::Method::GET,
            HttpMethod::Post => reqwest::Method::POST,
            HttpMethod::Put => reqwest::Method::PUT,
            HttpMethod::Delete => reqwest::Method::DELETE,
            HttpMethod::Head => reqwest::Method::HEAD,
            HttpMethod::Patch => reqwest::Method::PATCH,
            HttpMethod::Options => reqwest::Method::OPTIONS,
        };

        let mut builder = self.client.request(method, &request.url);

        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }

        if !request.body.is_empty() {
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

/// Map a `reqwest::Error` to our `HttpClientError` variants.
#[cfg(feature = "reqwest-backend")]
fn map_reqwest_error(e: reqwest::Error) -> HttpClientError {
    if e.is_timeout() {
        HttpClientError::TimedOut
    } else if e.is_connect() {
        HttpClientError::ConnectionFailed(e.to_string())
    } else {
        HttpClientError::IoError(e.to_string())
    }
}
