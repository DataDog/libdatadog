// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes and forwards FFE (Feature Flag Evaluation) exposure events to
//! the Datadog Agent's EVP proxy.
//!
//! Protocol matches dd-trace-go / dd-trace-rb / dd-trace-py / dd-trace-js /
//! dd-trace-dotnet: `POST /evp_proxy/v2/api/v2/exposures` with the header
//! `X-Datadog-EVP-Subdomain: event-platform-intake`. No agent capability gate.

use crate::service::FfeExposureBatch;
use datadog_ffe::telemetry::exposures::encode_exposure_batch;
pub(crate) use datadog_ffe::telemetry::exposures::ExposureDeduplicator;
use http::uri::PathAndQuery;
use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use std::time::Duration;
use tracing::{debug, warn};

/// EVP proxy path for FFE exposure intake.
pub(crate) const EVP_EXPOSURES_PATH: &str = "/evp_proxy/v2/api/v2/exposures";

/// EVP subdomain that routes requests to event-platform intake.
pub(crate) const EVP_SUBDOMAIN_HEADER: &str = "X-Datadog-EVP-Subdomain";
pub(crate) const EVP_SUBDOMAIN_VALUE: &str = "event-platform-intake";

const USER_AGENT: &str = concat!("ddtrace-sidecar/", env!("CARGO_PKG_VERSION"));

/// Build the FFE exposure endpoint from a session's agent base endpoint.
/// Overrides only the path (`/evp_proxy/v2/api/v2/exposures`), preserving
/// scheme, authority, timeout, and test_token.
/// Returns `None` for agentless mode because EVP proxy routing is agent-only.
pub(crate) fn exposure_endpoint(base: &Endpoint) -> Option<Endpoint> {
    if base.api_key.is_some() {
        return None;
    }

    let mut parts = base.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(EVP_EXPOSURES_PATH));
    let url = http::Uri::from_parts(parts).ok()?;
    Some(Endpoint {
        url,
        ..base.clone()
    })
}

/// POST a structured FFE exposure batch to the agent EVP proxy.
/// Fire-and-forget: non-2xx responses are logged at `warn`, network errors at
/// `debug`, and dropped (matches dd-trace-go behaviour).
pub(crate) async fn send_batch<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    deduplicator: &ExposureDeduplicator,
    batch: FfeExposureBatch,
) {
    let payload = match encode_exposure_batch(deduplicator, batch) {
        Ok(Some(payload)) => payload,
        Ok(None) => return,
        Err(e) => {
            debug!("ffe_exposures_flusher: failed to encode exposure payload: {e:?}");
            return;
        }
    };
    send_payload(client, endpoint, payload).await;
}

async fn send_payload<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    payload: String,
) {
    let builder = match endpoint.to_request_builder(USER_AGENT) {
        Ok(b) => b,
        Err(e) => {
            debug!("ffe_exposures_flusher: failed to build request: {e:?}");
            return;
        }
    };

    let req = match builder
        .method(Method::POST)
        .header("Content-Type", "application/json")
        .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
        .body(Bytes::from(payload))
    {
        Ok(r) => r,
        Err(e) => {
            debug!("ffe_exposures_flusher: failed to construct request body: {e:?}");
            return;
        }
    };

    let timeout = Duration::from_millis(endpoint.timeout_ms);
    let response = tokio::select! {
        biased;
        result = client.request(req) => result,
        _ = client.sleep(timeout) => {
            debug!("ffe_exposures_flusher: request timed out after {timeout:?}");
            return;
        }
    };

    match response {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body_preview = truncate(resp.body().as_ref(), 256);
                warn!("ffe_exposures_flusher: non-2xx response {status}: {body_preview}");
            } else {
                debug!("ffe_exposures_flusher: sent exposure batch, status={status}");
            }
        }
        Err(e) => {
            debug!("ffe_exposures_flusher: request failed: {e:?}");
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
    use crate::service::{FfeExposure, FfeTelemetryContext};
    use httpmock::MockServer;
    use libdd_capabilities::{HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use std::future;

    fn endpoint_for(server: &MockServer) -> Endpoint {
        Endpoint {
            url: server.url("/").parse().unwrap(),
            ..Endpoint::default()
        }
    }

    fn context() -> FfeTelemetryContext {
        FfeTelemetryContext {
            service: "svc".to_owned(),
            env: "prod".to_owned(),
            version: "1".to_owned(),
        }
    }

    fn exposure(subject_id: &str, allocation_key: &str, variant: &str) -> FfeExposure {
        FfeExposure {
            timestamp_ms: 123,
            flag_key: "flag".to_owned(),
            subject_id: subject_id.to_owned(),
            subject_attributes_json: r#"{"tier":"premium"}"#.to_owned(),
            allocation_key: allocation_key.to_owned(),
            variant: variant.to_owned(),
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn posts_to_evp_proxy() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_EXPOSURES_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = exposure_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();

        send_batch(
            &client,
            &ep,
            &ExposureDeduplicator::new(4),
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .await;

        mock.assert_async().await;
        assert_eq!(mock.calls_async().await, 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_2xx_does_not_panic() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path(EVP_EXPOSURES_PATH);
                then.status(500).body("intake overloaded");
            })
            .await;

        let base = endpoint_for(&server);
        let ep = exposure_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        send_batch(
            &client,
            &ep,
            &ExposureDeduplicator::new(4),
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .await;
    }

    #[cfg_attr(miri, ignore)] // tokio executor park/wake overhead is prohibitively slow under Miri
    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let ep = Endpoint {
            url: "http://localhost:8126".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_batch(
            &HangingCapabilities,
            &ep,
            &ExposureDeduplicator::new(4),
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .await;
    }

    #[test]
    fn endpoint_preserves_authority_overrides_path() {
        let base = Endpoint {
            url: "http://agent.internal:8126/v0.4/traces".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = exposure_endpoint(&base).unwrap();
        assert_eq!(ep.url.scheme_str(), Some("http"));
        assert_eq!(ep.url.authority().unwrap().as_str(), "agent.internal:8126");
        assert_eq!(ep.url.path(), EVP_EXPOSURES_PATH);
    }

    #[test]
    fn endpoint_rejects_agentless() {
        let base = Endpoint {
            url: "https://trace.agent.datadoghq.com/v0.4/traces"
                .parse()
                .unwrap(),
            api_key: Some("api-key".into()),
            ..Endpoint::default()
        };

        assert!(exposure_endpoint(&base).is_none());
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
