// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Forwards FFE (Feature Flag Evaluation) metric payloads from the PHP tracer
//! to a user-configured OTLP HTTP metrics intake.
//!
//! Unlike `ffe_exposures_flusher`, which targets the Datadog Agent's EVP proxy, this
//! flusher targets an OpenTelemetry-compatible OTLP metrics endpoint
//! (typically configured via `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`). The
//! endpoint is supplied per-call by the PHP tracer, not derived from the
//! sidecar session's agent base. PHP encodes the metric series as OTLP/protobuf
//! and the sidecar performs the HTTP POST.

use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use std::time::Duration;
use tracing::{debug, warn};

const USER_AGENT: &str = concat!("ddtrace-php-sidecar/", env!("CARGO_PKG_VERSION"));

/// Build an `Endpoint` for an OTLP metrics intake from a fully-qualified URL.
///
/// Production callers supply the URL via the FFI (typically the value of
/// `OTEL_EXPORTER_OTLP_METRICS_ENDPOINT`; the OpenTelemetry spec default is
/// `http://localhost:4318/v1/metrics`).
/// Returns `None` if the URL is unparseable. The OTLP endpoint is unrelated
/// to the Agent base, so we don't preserve any session fields here.
pub(crate) fn otlp_metrics_endpoint(url: &str) -> Option<Endpoint> {
    let url = url.parse().ok()?;
    Some(Endpoint {
        url,
        ..Endpoint::default()
    })
}

/// POST a single OTLP/protobuf metrics payload to the configured intake.
/// Fire-and-forget: non-2xx responses and network errors are logged and
/// dropped (matches dd-trace-go/py OTLP exporter behavior).
pub(crate) async fn send_payload<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    payload: Vec<u8>,
) {
    let builder = match endpoint.to_request_builder(USER_AGENT) {
        Ok(b) => b,
        Err(e) => {
            debug!("ffe_metrics_flusher: failed to build request: {e:?}");
            return;
        }
    };

    let req = match builder
        .method(Method::POST)
        .header("Content-Type", "application/x-protobuf")
        .body(Bytes::from(payload))
    {
        Ok(r) => r,
        Err(e) => {
            debug!("ffe_metrics_flusher: failed to construct request body: {e:?}");
            return;
        }
    };

    let timeout = Duration::from_millis(endpoint.timeout_ms);
    let response = tokio::select! {
        biased;
        result = client.request(req) => result,
        _ = client.sleep(timeout) => {
            debug!("ffe_metrics_flusher: request timed out after {timeout:?}");
            return;
        }
    };

    match response {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body_preview = truncate(resp.body().as_ref(), 256);
                warn!("ffe_metrics_flusher: non-2xx response {status}: {body_preview}");
            } else {
                debug!("ffe_metrics_flusher: sent metric batch, status={status}");
            }
        }
        Err(e) => {
            debug!("ffe_metrics_flusher: request failed: {e:?}");
        }
    }
}

fn truncate(bytes: &[u8], cap: usize) -> String {
    let take = bytes.len().min(cap);
    String::from_utf8_lossy(&bytes[..take]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::MockServer;
    use libdd_capabilities::{HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use std::future;

    /// POST hits the configured OTLP metrics path with application/x-protobuf.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn posts_protobuf_to_configured_endpoint() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path("/v1/metrics")
                    .header("content-type", "application/x-protobuf");
                then.status(202);
            })
            .await;

        let url = server.url("/v1/metrics");
        let ep = otlp_metrics_endpoint(&url).unwrap();
        let client = NativeCapabilities::new_client();

        // Tiny but valid protobuf-shaped bytes; the flusher does not inspect
        // payload content, it just relays.
        let payload = vec![0x0a, 0x00];
        send_payload(&client, &ep, payload).await;

        mock.assert_async().await;
        assert_eq!(mock.calls_async().await, 1);
    }

    /// Non-2xx responses are logged and dropped; no panic, no retry.
    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_2xx_does_not_panic() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path("/v1/metrics");
                then.status(500).body("intake overloaded");
            })
            .await;

        let url = server.url("/v1/metrics");
        let ep = otlp_metrics_endpoint(&url).unwrap();
        let client = NativeCapabilities::new_client();
        send_payload(&client, &ep, vec![0u8; 4]).await;
    }

    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let ep = Endpoint {
            url: "http://localhost:4318/v1/metrics".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_payload(&HangingCapabilities, &ep, vec![0u8; 4]).await;
    }

    #[test]
    fn default_endpoint_is_parseable() {
        let ep = otlp_metrics_endpoint("http://localhost:4318/v1/metrics").unwrap();
        assert_eq!(ep.url.scheme_str(), Some("http"));
        assert_eq!(ep.url.path(), "/v1/metrics");
    }

    #[test]
    fn invalid_url_returns_none() {
        assert!(otlp_metrics_endpoint("not a url").is_none());
    }

    #[derive(Clone, Debug)]
    struct HangingCapabilities;

    impl HttpClientCapability for HangingCapabilities {
        fn new_client() -> Self {
            Self
        }

        fn request(
            &self,
            _req: http::Request<Bytes>,
        ) -> impl future::Future<Output = Result<http::Response<Bytes>, HttpError>> + MaybeSend
        {
            future::pending()
        }
    }

    impl SleepCapability for HangingCapabilities {
        fn new() -> Self {
            Self
        }

        fn sleep(&self, _duration: Duration) -> impl future::Future<Output = ()> + MaybeSend {
            async {}
        }
    }
}
