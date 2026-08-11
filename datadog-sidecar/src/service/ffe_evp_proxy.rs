// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Shared FFE EVP proxy transport helpers.
//!
//! FFE exposures and flagevaluation use different payload schemas, but both
//! are forwarded through the Agent EVP proxy with the same endpoint derivation,
//! subdomain header, timeout behavior, and fire-and-forget error handling.

use crate::service::evp_proxy;
use http::uri::PathAndQuery;
use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use std::time::Duration;
use tracing::{debug, warn};

pub(crate) use evp_proxy::EVENT_PLATFORM_INTAKE_SUBDOMAIN as EVP_SUBDOMAIN_VALUE;
pub(crate) use evp_proxy::SUBDOMAIN_HEADER as EVP_SUBDOMAIN_HEADER;

const USER_AGENT: &str = concat!("ddtrace-sidecar/", env!("CARGO_PKG_VERSION"));

/// Build an Agent EVP proxy endpoint from a session's agent base endpoint.
/// Overrides only the path, preserving scheme, authority, timeout, and test_token.
/// Returns `None` for agentless mode because EVP proxy routing is agent-only.
pub(crate) fn endpoint(base: &Endpoint, path: &'static str) -> Option<Endpoint> {
    if base.api_key.is_some() {
        return None;
    }

    let mut parts = base.url.clone().into_parts();
    parts.path_and_query = Some(PathAndQuery::from_static(path));
    let url = http::Uri::from_parts(parts).ok()?;
    Some(Endpoint {
        url,
        ..base.clone()
    })
}

/// POST a JSON payload to the Agent EVP proxy.
///
/// Fire-and-forget: non-2xx responses are logged at `warn`, network errors at
/// `debug`, and dropped.
pub(crate) async fn send_payload<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    payload: String,
    log_prefix: &'static str,
    success_name: &'static str,
) {
    let builder = match endpoint.to_request_builder(USER_AGENT) {
        Ok(b) => b,
        Err(e) => {
            debug!("{log_prefix}: failed to build request: {e:?}");
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
            debug!("{log_prefix}: failed to construct request body: {e:?}");
            return;
        }
    };

    let timeout = Duration::from_millis(endpoint.timeout_ms);
    let response = tokio::select! {
        biased;
        result = client.request(req) => result,
        _ = client.sleep(timeout) => {
            debug!("{log_prefix}: request timed out after {timeout:?}");
            return;
        }
    };

    match response {
        Ok(resp) => {
            let status = resp.status();
            if !status.is_success() {
                let body_preview = truncate(resp.body().as_ref(), 256);
                warn!("{log_prefix}: non-2xx response {status}: {body_preview}");
            } else {
                debug!("{log_prefix}: sent {success_name}, status={status}");
            }
        }
        Err(e) => {
            debug!("{log_prefix}: request failed: {e:?}");
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

    #[test]
    fn endpoint_preserves_authority_overrides_path() {
        let base = Endpoint {
            url: "http://agent.internal:8126/v0.4/traces".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = endpoint(&base, "/evp_proxy/v2/api/v2/exposures").unwrap();
        assert_eq!(ep.url.scheme_str(), Some("http"));
        assert_eq!(ep.url.authority().unwrap().as_str(), "agent.internal:8126");
        assert_eq!(ep.url.path(), "/evp_proxy/v2/api/v2/exposures");
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

        assert!(endpoint(&base, "/evp_proxy/v2/api/v2/exposures").is_none());
    }
}
