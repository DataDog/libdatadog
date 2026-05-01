// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! [`AgentClient`] and its send methods.

use std::collections::HashMap;

use bytes::Bytes;
use flate2::{write::GzEncoder, Compression};
use libdd_http_client::{HttpClient, HttpMethod, HttpRequest};
use serde_json::{from_slice, Value};
use std::io::Write as _;

use crate::{
    agent_info::AgentInfo,
    builder::AgentClientBuilder,
    error::SendError,
    telemetry::TelemetryRequest,
    traces::{AgentResponse, TraceFormat, TraceSendOptions},
};

/// A Datadog-agent-specialized HTTP client.
///
/// Wraps a configured [`libdd_http_client::HttpClient`] and injects Datadog-specific headers
/// automatically on every request:
///
/// - Language metadata headers (`Datadog-Meta-Lang`, `Datadog-Meta-Lang-Version`,
///   `Datadog-Meta-Lang-Interpreter`, `Datadog-Meta-Tracer-Version`) from the [`LanguageMetadata`]
///   supplied when creating the client.
/// - `User-Agent` derived from [`LanguageMetadata::user_agent`].
/// - Container/entity-ID headers (`Datadog-Container-Id`, `Datadog-Entity-ID`,
///   `Datadog-External-Env`) read from `/proc/self/cgroup` at startup.
/// - `x-datadog-test-session-token` when a test token was set.
/// - Any extra headers registered via [`AgentClientBuilder::extra_headers`].
///
/// Obtain via [`AgentClient::builder`].
///
/// [`LanguageMetadata`]: crate::LanguageMetadata
pub struct AgentClient {
    http: HttpClient,
    base_url: String,
    static_headers: Vec<(String, String)>,
}

impl AgentClient {
    pub(crate) fn new(http: HttpClient, static_headers: Vec<(String, String)>) -> Self {
        let base_url = http.config().base_url().to_string();
        Self {
            http,
            base_url,
            static_headers,
        }
    }

    /// Create a new [`AgentClientBuilder`].
    pub fn builder() -> AgentClientBuilder {
        AgentClientBuilder::new()
    }

    /// Send a serialised trace payload to the agent with automatically injected headers.
    ///
    /// # Returns
    ///
    /// An [`AgentResponse`] with the HTTP status and the parsed `rate_by_service` sampling
    /// rates from the agent response body.
    pub async fn send_traces(
        &self,
        payload: Bytes,
        trace_count: usize,
        format: TraceFormat,
        opts: TraceSendOptions,
    ) -> Result<AgentResponse, SendError> {
        let (path, content_type) = match format {
            TraceFormat::MsgpackV5 => ("/v0.5/traces", "application/msgpack"),
            TraceFormat::MsgpackV4 => ("/v0.4/traces", "application/msgpack"),
        };

        let mut request = HttpRequest::new(HttpMethod::Put, format!("{}{}", self.base_url, path))
            .with_body(payload)
            .with_headers(self.static_headers.iter().cloned())
            .with_header("Content-Type", content_type)
            .with_header("X-Datadog-Trace-Count", trace_count.to_string())
            .with_header("Datadog-Send-Real-Http-Status", "true");

        if opts.computed_top_level {
            request = request.with_header("Datadog-Client-Computed-Top-Level", "yes");
        }

        let response = self.http.send(request).await?;

        if response.status_code() >= 400 {
            return Err(SendError::HttpError {
                status: response.status_code(),
                body: response.body().clone(),
            });
        }

        let rate_by_service = parse_rate_by_service(response.body());
        Ok(AgentResponse {
            status: response.status_code(),
            rate_by_service,
        })
    }

    /// Send span stats (APM concentrator buckets) to `/v0.6/stats`.
    pub async fn send_stats(&self, payload: Bytes) -> Result<(), SendError> {
        let request = HttpRequest::new(HttpMethod::Put, format!("{}/v0.6/stats", self.base_url))
            .with_body(payload)
            .with_headers(self.static_headers.iter().cloned())
            .with_header("Content-Type", "application/msgpack");

        let response = self.http.send(request).await?;
        check_status(response)
    }

    /// Send data-streams pipeline stats to `/v0.1/pipeline_stats`.
    ///
    /// The payload is **always** gzip-compressed regardless of the client-level compression
    /// setting. This is a protocol requirement of the data-streams endpoint.
    pub async fn send_pipeline_stats(&self, payload: Bytes) -> Result<(), SendError> {
        let request = HttpRequest::new(
            HttpMethod::Put,
            format!("{}/v0.1/pipeline_stats", self.base_url),
        )
        .with_body(gzip_compress(payload)?)
        .with_headers(self.static_headers.iter().cloned())
        .with_header("Content-Type", "application/msgpack")
        .with_header("Content-Encoding", "gzip");

        let response = self.http.send(request).await?;
        check_status(response)
    }

    /// Send a telemetry event to the agent's telemetry proxy
    /// (`telemetry/proxy/api/v2/apmtelemetry`).
    pub async fn send_telemetry(&self, req: TelemetryRequest) -> Result<(), SendError> {
        let request = HttpRequest::new(
            HttpMethod::Post,
            format!("{}/telemetry/proxy/api/v2/apmtelemetry", self.base_url),
        )
        .with_body(req.body)
        .with_headers(self.static_headers.iter().cloned())
        .with_header("Content-Type", "application/json")
        .with_header("DD-Telemetry-Request-Type", &req.request_type)
        .with_header("DD-Telemetry-API-Version", &req.api_version)
        .with_header(
            "DD-Telemetry-Debug-Enabled",
            if req.debug { "true" } else { "false" },
        );

        let response = self.http.send(request).await?;
        check_status(response)
    }

    /// Send an event via the agent's EVP (Event Platform) proxy.
    ///
    /// The agent forwards the request to `<subdomain>.datadoghq.com<path>`. `subdomain`
    /// controls the target intake (injected as `X-Datadog-EVP-Subdomain`); `path` is the
    /// endpoint on that intake (e.g. `/api/v2/exposures`).
    pub async fn send_evp_event(
        &self,
        subdomain: &str,
        path: &str,
        payload: Bytes,
        content_type: &str,
    ) -> Result<(), SendError> {
        let request = HttpRequest::new(HttpMethod::Post, format!("{}{}", self.base_url, path))
            .with_body(payload)
            .with_headers(self.static_headers.iter().cloned())
            .with_header("Content-Type", content_type)
            .with_header("X-Datadog-EVP-Subdomain", subdomain);

        let response = self.http.send(request).await?;
        check_status(response)
    }

    /// Probe `GET /info` and return parsed agent capabilities.
    ///
    /// Returns `Ok(None)` when the agent returns 404 (remote-config / info not supported).
    pub async fn agent_info(&self) -> Result<Option<AgentInfo>, SendError> {
        #[derive(serde::Deserialize)]
        struct InfoResponse {
            version: Option<String>,
            endpoints: Option<Vec<String>>,
            client_drop_p0s: Option<bool>,
            config: Option<Value>,
        }

        let request = HttpRequest::new(HttpMethod::Get, format!("{}/info", self.base_url))
            .with_headers(self.static_headers.iter().cloned());

        let response = self.http.send(request).await?;

        if response.status_code() == 404 {
            return Ok(None);
        }

        if response.status_code() >= 400 {
            return Err(SendError::HttpError {
                status: response.status_code(),
                body: response.body().clone(),
            });
        }

        // Case-insensitive lookup of a response header value.
        let header = |name: &str| -> Option<String> {
            response
                .headers()
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
        };

        let container_tags_hash = header("datadog-container-tags-hash");
        let state_hash = header("datadog-agent-state");

        let info: InfoResponse =
            from_slice(response.body()).map_err(|e| SendError::Encoding(e.to_string()))?;

        Ok(Some(AgentInfo {
            endpoints: info.endpoints.unwrap_or_default(),
            client_drop_p0s: info.client_drop_p0s.unwrap_or(false),
            config: info.config.unwrap_or(Value::Null),
            version: info.version,
            container_tags_hash,
            state_hash,
        }))
    }
}

/// Parse `rate_by_service` from an agent trace response body.
fn parse_rate_by_service(body: &Bytes) -> Option<HashMap<String, f64>> {
    #[derive(serde::Deserialize)]
    struct TraceResponse {
        rate_by_service: Option<HashMap<String, f64>>,
    }

    from_slice::<TraceResponse>(body)
        .ok()
        .and_then(|r| r.rate_by_service)
}

/// Return `Ok(())` for 2xx, or `Err(SendError::HttpError)` for anything else.
fn check_status(response: libdd_http_client::HttpResponse) -> Result<(), SendError> {
    if response.status_code() >= 400 {
        Err(SendError::HttpError {
            status: response.status_code(),
            body: response.body().clone(),
        })
    } else {
        Ok(())
    }
}

/// Gzip-compress `payload` at level 6.
fn gzip_compress(payload: Bytes) -> Result<Bytes, SendError> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::new(6));
    encoder
        .write_all(&payload)
        .map_err(|e| SendError::Encoding(e.to_string()))?;
    let compressed = encoder
        .finish()
        .map_err(|e| SendError::Encoding(e.to_string()))?;
    Ok(Bytes::from(compressed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentClient, LanguageMetadata};

    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn test_client(port: u16) -> AgentClient {
        ensure_crypto_provider();
        AgentClient::builder()
            .http("localhost", port)
            .language_metadata(LanguageMetadata::new("python", "3.12", "CPython", "2.0"))
            .build()
            .unwrap()
    }

    #[test]
    fn builder_roundtrip() {
        let client = test_client(8126);
        assert!(client.base_url.contains("localhost"));
    }

    #[test]
    fn static_headers_contain_language_metadata() {
        let client = test_client(8126);
        let keys: Vec<&str> = client
            .static_headers
            .iter()
            .map(|(k, _)| k.as_str())
            .collect();
        assert!(keys.contains(&"Datadog-Meta-Lang"));
        assert!(keys.contains(&"Datadog-Meta-Lang-Version"));
        assert!(keys.contains(&"User-Agent"));
    }

    #[test]
    fn extra_headers_propagated() {
        ensure_crypto_provider();
        let client = AgentClient::builder()
            .http("localhost", 80)
            .language_metadata(LanguageMetadata::new("python", "3.12", "CPython", "2.0"))
            .extra_headers(vec![("X-Custom".to_owned(), "custom value".to_owned())])
            .build()
            .unwrap();

        assert_eq!(
            client
                .static_headers
                .iter()
                .find_map(|(key, value)| (key == "X-Custom").then_some(value.as_str())),
            Some("custom value")
        );
    }

    #[test]
    fn gzip_compress_produces_valid_gzip() {
        let input = Bytes::from_static(b"hello world");
        let compressed = gzip_compress(input).unwrap();
        // gzip magic bytes: 0x1f 0x8b
        assert_eq!(&compressed[..2], &[0x1f, 0x8b]);
    }

    #[test]
    fn parse_rate_by_service_valid_json() {
        let body = Bytes::from(r#"{"rate_by_service":{"service:env":0.5}}"#);
        let rates = parse_rate_by_service(&body).unwrap();
        assert_eq!(rates.get("service:env"), Some(&0.5));
    }

    #[test]
    fn parse_rate_by_service_absent_field() {
        let body = Bytes::from(r#"{"other":"value"}"#);
        assert!(parse_rate_by_service(&body).is_none());
    }
}
