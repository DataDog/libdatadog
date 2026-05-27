// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes and forwards FFE (Feature Flag Evaluation) exposure events to
//! the Datadog Agent's EVP proxy.
//!
//! Protocol matches dd-trace-go / dd-trace-rb / dd-trace-py / dd-trace-js /
//! dd-trace-dotnet: `POST /evp_proxy/v2/api/v2/exposures` with the header
//! `X-Datadog-EVP-Subdomain: event-platform-intake`. No agent capability gate.

use crate::service::{FfeExposure, FfeExposureBatch, FfeTelemetryContext};
use http::uri::PathAndQuery;
use http::Method;
use libdd_capabilities::{Bytes, HttpClientCapability, SleepCapability};
use libdd_common::Endpoint;
use lru::LruCache;
use serde::Serialize;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{debug, warn};

/// EVP proxy path for FFE exposure intake.
pub(crate) const EVP_EXPOSURES_PATH: &str = "/evp_proxy/v2/api/v2/exposures";

/// EVP subdomain that routes requests to event-platform intake.
pub(crate) const EVP_SUBDOMAIN_HEADER: &str = "X-Datadog-EVP-Subdomain";
pub(crate) const EVP_SUBDOMAIN_VALUE: &str = "event-platform-intake";

const USER_AGENT: &str = concat!("ddtrace-sidecar/", env!("CARGO_PKG_VERSION"));
const DEFAULT_CACHE_LIMIT: usize = 65_536;

#[derive(Clone)]
pub(crate) struct ExposureDeduplicator {
    cache: Arc<Mutex<LruCache<ExposureCacheKey, ExposureCacheValue>>>,
}

impl Default for ExposureDeduplicator {
    fn default() -> Self {
        Self::new(DEFAULT_CACHE_LIMIT)
    }
}

impl ExposureDeduplicator {
    pub(crate) fn new(limit: usize) -> Self {
        let limit = NonZeroUsize::new(limit).unwrap_or(NonZeroUsize::MIN);
        Self {
            cache: Arc::new(Mutex::new(LruCache::new(limit))),
        }
    }

    fn should_send(&self, context: &FfeTelemetryContext, exposure: &FfeExposure) -> bool {
        let key = ExposureCacheKey {
            service: context.service.clone(),
            env: context.env.clone(),
            version: context.version.clone(),
            flag_key: exposure.flag_key.clone(),
            subject_id: exposure.subject_id.clone(),
        };
        let value = ExposureCacheValue {
            allocation_key: exposure.allocation_key.clone(),
            variant: exposure.variant.clone(),
        };

        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.get(&key).is_some_and(|cached| cached == &value) {
            return false;
        }

        cache.put(key, value);
        true
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct ExposureCacheKey {
    service: String,
    env: String,
    version: String,
    flag_key: String,
    subject_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ExposureCacheValue {
    allocation_key: String,
    variant: String,
}

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

pub(crate) fn encode_batch(
    deduplicator: &ExposureDeduplicator,
    batch: FfeExposureBatch,
) -> Option<String> {
    let exposures = batch
        .exposures
        .into_iter()
        .filter(is_complete)
        .filter(|exposure| deduplicator.should_send(&batch.context, exposure))
        .map(ExposureEvent::from)
        .collect::<Vec<_>>();

    if exposures.is_empty() {
        return None;
    }

    let payload = ExposurePayload {
        context: ExposurePayloadContext::from(batch.context),
        exposures,
    };
    match serde_json::to_string(&payload) {
        Ok(payload) => Some(payload),
        Err(e) => {
            debug!("ffe_exposures_flusher: failed to encode exposure payload: {e:?}");
            None
        }
    }
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
    let Some(payload) = encode_batch(deduplicator, batch) else {
        return;
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

fn is_complete(exposure: &FfeExposure) -> bool {
    !exposure.flag_key.is_empty()
        && !exposure.allocation_key.is_empty()
        && !exposure.variant.is_empty()
}

#[derive(Serialize)]
struct ExposurePayload {
    context: ExposurePayloadContext,
    exposures: Vec<ExposureEvent>,
}

#[derive(Serialize)]
struct ExposurePayloadContext {
    #[serde(skip_serializing_if = "String::is_empty")]
    service: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    env: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    version: String,
}

impl From<FfeTelemetryContext> for ExposurePayloadContext {
    fn from(value: FfeTelemetryContext) -> Self {
        Self {
            service: value.service,
            env: value.env,
            version: value.version,
        }
    }
}

#[derive(Serialize)]
struct ExposureEvent {
    timestamp: u64,
    allocation: Key,
    flag: Key,
    variant: Key,
    subject: Subject,
}

impl From<FfeExposure> for ExposureEvent {
    fn from(value: FfeExposure) -> Self {
        Self {
            timestamp: value.timestamp_ms,
            allocation: Key {
                key: value.allocation_key,
            },
            flag: Key {
                key: value.flag_key,
            },
            variant: Key { key: value.variant },
            subject: Subject {
                id: value.subject_id,
                attributes: subject_attributes(&value.subject_attributes_json),
            },
        }
    }
}

#[derive(Serialize)]
struct Key {
    key: String,
}

#[derive(Serialize)]
struct Subject {
    id: String,
    attributes: serde_json::Map<String, serde_json::Value>,
}

fn subject_attributes(json: &str) -> serde_json::Map<String, serde_json::Value> {
    if json.is_empty() {
        return serde_json::Map::new();
    }

    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(serde_json::Value::Object(attrs)) => attrs,
        Ok(_) | Err(_) => serde_json::Map::new(),
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
    use serde_json::Value;
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

    #[test]
    fn encodes_structured_batch_and_preserves_empty_subject() {
        let deduplicator = ExposureDeduplicator::new(4);
        let payload = encode_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("", "alloc", "variant")],
            },
        )
        .unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();

        assert_eq!(payload["context"]["service"], "svc");
        assert_eq!(payload["context"]["env"], "prod");
        assert_eq!(payload["context"]["version"], "1");
        assert_eq!(payload["exposures"][0]["subject"]["id"], "");
        assert_eq!(
            payload["exposures"][0]["subject"]["attributes"]["tier"],
            "premium"
        );
    }

    #[test]
    fn deduplicates_same_assignment_and_emits_changed_assignment() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc-a", "a")],
            },
        );
        let duplicate = encode_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc-a", "a")],
            },
        );
        let changed = encode_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc-b", "b")],
            },
        );

        assert!(first.is_some());
        assert!(duplicate.is_none());
        assert!(changed.is_some());
    }

    #[test]
    fn cache_key_includes_service_env_and_version() {
        let deduplicator = ExposureDeduplicator::new(4);
        let first = encode_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        );
        let other_service = encode_batch(
            &deduplicator,
            FfeExposureBatch {
                context: FfeTelemetryContext {
                    service: "other".to_owned(),
                    ..context()
                },
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        );

        assert!(first.is_some());
        assert!(other_service.is_some());
    }

    #[test]
    fn drops_incomplete_exposures() {
        let deduplicator = ExposureDeduplicator::new(4);
        let mut invalid = exposure("user", "alloc", "variant");
        invalid.allocation_key.clear();

        assert!(encode_batch(
            &deduplicator,
            FfeExposureBatch {
                context: context(),
                exposures: vec![invalid],
            },
        )
        .is_none());
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
