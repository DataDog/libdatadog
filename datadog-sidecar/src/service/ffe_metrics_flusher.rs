// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Aggregates, serializes, and forwards FFE (Feature Flag Evaluation) metric
//! events to a user-configured OTLP HTTP metrics intake.

use crate::service::{FfeEvaluationMetric, FfeTelemetryContext};
use datadog_ffe::telemetry::evaluation_metrics::encode_metrics_payload;
use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use std::time::Duration;
use tracing::{debug, warn};

const USER_AGENT: &str = concat!("ddtrace-sidecar/", env!("CARGO_PKG_VERSION"));

/// POST structured FFE metric events as OTLP/protobuf to the configured intake.
/// Fire-and-forget: non-2xx responses and network errors are logged and
/// dropped (matches dd-trace-go/py OTLP exporter behavior).
pub(crate) async fn send_metrics<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    context: FfeTelemetryContext,
    metrics: Vec<FfeEvaluationMetric>,
) {
    let Some(payload) = encode_metrics_payload(context, metrics) else {
        return;
    };
    send_payload(client, endpoint, payload).await;
}

async fn send_payload<C: HttpClientCapability + SleepCapability>(
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

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn metric(flag_key: &str, variant: &str, reason: &str) -> FfeEvaluationMetric {
        FfeEvaluationMetric {
            flag_key: flag_key.to_owned(),
            variant: variant.to_owned(),
            reason: reason.to_owned(),
            error_type: None,
            allocation_key: Some("alloc".to_owned()),
        }
    }

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

        let ep = Endpoint {
            url: server.url("/v1/metrics").parse().unwrap(),
            ..Endpoint::default()
        };
        let client = NativeCapabilities::new_client();

        send_metrics(
            &client,
            &ep,
            context(),
            vec![metric("flag", "variant", "TARGETING_MATCH")],
        )
        .await;

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

        let ep = Endpoint {
            url: server.url("/v1/metrics").parse().unwrap(),
            ..Endpoint::default()
        };
        let client = NativeCapabilities::new_client();
        send_metrics(
            &client,
            &ep,
            context(),
            vec![metric("flag", "variant", "TARGETING_MATCH")],
        )
        .await;
    }

    #[cfg_attr(miri, ignore)] // tokio executor park/wake overhead is prohibitively slow under Miri
    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let ep = Endpoint {
            url: "http://localhost:4318/v1/metrics".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_metrics(
            &HangingCapabilities,
            &ep,
            context(),
            vec![metric("flag", "variant", "TARGETING_MATCH")],
        )
        .await;
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

        async fn sleep(&self, _duration: Duration) {}
    }
}
