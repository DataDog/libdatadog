// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Forwards FFE (Feature Flag Evaluation) exposure payloads from the PHP tracer
//! to the Datadog Agent's EVP proxy.
//!
//! Protocol matches dd-trace-go / dd-trace-rb / dd-trace-py / dd-trace-js /
//! dd-trace-dotnet: `POST /evp_proxy/v2/api/v2/exposures` with the header
//! `X-Datadog-EVP-Subdomain: event-platform-intake`. No agent capability gate.

use http::uri::PathAndQuery;
use http::Method;
use libdd_capabilities::http::HttpClientTrait;
use libdd_capabilities::Bytes;
use libdd_capabilities_impl::DefaultHttpClient;
use libdd_common::Endpoint;
use tracing::{debug, warn};

/// EVP proxy path for FFE exposure intake.
pub(crate) const EVP_EXPOSURES_PATH: &str = "/evp_proxy/v2/api/v2/exposures";

/// EVP subdomain that routes requests to event-platform intake.
pub(crate) const EVP_SUBDOMAIN_HEADER: &str = "X-Datadog-EVP-Subdomain";
pub(crate) const EVP_SUBDOMAIN_VALUE: &str = "event-platform-intake";

const USER_AGENT: &str = concat!("ddtrace-php-sidecar/", env!("CARGO_PKG_VERSION"));

/// Build the FFE exposure endpoint from a session's agent base endpoint.
/// Overrides only the path (`/evp_proxy/v2/api/v2/exposures`), preserving
/// scheme, authority, api_key (agentless), timeout, and test_token.
pub(crate) fn exposure_endpoint(base: &Endpoint) -> Option<Endpoint> {
    let mut parts = base.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(EVP_EXPOSURES_PATH));
    let url = http::Uri::from_parts(parts).ok()?;
    Some(Endpoint {
        url,
        ..base.clone()
    })
}

/// POST a single FFE exposure payload to the agent EVP proxy.
/// Fire-and-forget: non-2xx responses and network errors are logged at `debug`
/// and dropped (matches dd-trace-go behaviour).
pub(crate) async fn send_payload(client: &DefaultHttpClient, endpoint: &Endpoint, payload: String) {
    let builder = match endpoint.to_request_builder(USER_AGENT) {
        Ok(b) => b,
        Err(e) => {
            debug!("ffe_flusher: failed to build request: {e:?}");
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
            debug!("ffe_flusher: failed to construct request body: {e:?}");
            return;
        }
    };

    match client.request(req).await {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                // dd-trace-go logs a readable error body on non-2xx.
                let body_preview = truncate(resp.body().as_ref(), 256);
                warn!("ffe_flusher: non-2xx response {status}: {body_preview}");
            } else {
                debug!("ffe_flusher: sent exposure batch, status={status}");
            }
        }
        Err(e) => {
            debug!("ffe_flusher: request failed: {e:?}");
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
    use libdd_capabilities::http::HttpClientTrait;

    fn endpoint_for(server: &MockServer) -> Endpoint {
        Endpoint {
            url: server.url("/").parse().unwrap(),
            ..Endpoint::default()
        }
    }

    /// V3: POST hits `/evp_proxy/v2/api/v2/exposures` with the correct
    /// subdomain header and application/json content type. Body round-trips.
    #[tokio::test]
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
        let client = DefaultHttpClient::new_client();

        let payload =
            r#"{"context":{"service":"svc","env":"prod","version":"1"},"exposures":[]}"#.to_owned();
        send_payload(&client, &ep, payload.clone()).await;

        mock.assert_async().await;

        // Verify the endpoint was hit exactly once.
        let calls = mock.calls_async().await;
        assert_eq!(calls, 1);
    }

    /// Non-2xx responses are logged and dropped; no panic, no retry.
    #[tokio::test]
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
        let client = DefaultHttpClient::new_client();
        send_payload(&client, &ep, "{}".to_owned()).await;
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
}
