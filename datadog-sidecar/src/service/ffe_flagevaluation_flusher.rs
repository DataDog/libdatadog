// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes and forwards FFE (Feature Flag Evaluation) flag evaluation
//! batches to the Datadog Agent's EVP proxy.
//!
//! Protocol: `POST /evp_proxy/v2/api/v2/flagevaluation` with the header
//! `X-Datadog-EVP-Subdomain: event-platform-intake`. Fire-and-forget: non-2xx
//! responses are logged at `warn`, network errors at `debug`, and dropped
//! (matches dd-trace-go behaviour). No agent capability gate.

use crate::service::{evp_proxy, ffe_evp_proxy};
use crate::service::{FfeFlagEvaluationBatch, FfeTelemetryContext};
pub(crate) use datadog_ffe::telemetry::flagevaluation::FlagEvaluationEvpWriterStats;
use datadog_ffe::telemetry::flagevaluation::{
    encode_flag_evaluation_payloads, FlagEvaluationEvpCoalescer as CommonFlagEvaluationEvpCoalescer,
};
#[cfg(test)]
use ffe_evp_proxy::{EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::Endpoint;
use std::time::Duration;
use tracing::{debug, warn};

/// EVP proxy path for FFE flag evaluation intake.
pub(crate) const EVP_FLAGEVALUATION_PATH: &str = "/evp_proxy/v2/api/v2/flagevaluation";

const LOG_PREFIX: &str = "ffe_flagevaluation_flusher";
const COALESCE_DELAY: Duration = Duration::from_millis(250);

pub(crate) const FLAG_EVALUATION_ROWS_DROPPED_METRIC: &str = "flagevaluation.rows.dropped";
pub(crate) const FLAG_EVALUATION_ROWS_DEGRADED_METRIC: &str = "flagevaluation.rows.degraded";
pub(crate) const FLAG_EVALUATION_PAYLOAD_SPLITS_METRIC: &str = "flagevaluation.payload.splits";

pub(crate) const FLAG_EVALUATION_REASON_DEGRADED_CAP: &str = "degraded_cap";
pub(crate) const FLAG_EVALUATION_REASON_CARDINALITY_CAP: &str = "cardinality_cap";
pub(crate) const FLAG_EVALUATION_REASON_PAYLOAD_LIMIT: &str = "payload_limit";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DestinationKey {
    endpoint: Endpoint,
    context: FfeTelemetryContext,
}

impl DestinationKey {
    fn new(endpoint: Endpoint, context: &FfeTelemetryContext) -> Self {
        Self {
            endpoint,
            context: context.clone(),
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct FlagEvaluationCoalescer {
    inner: CommonFlagEvaluationEvpCoalescer<DestinationKey>,
}

impl FlagEvaluationCoalescer {
    pub(crate) fn enqueue(
        &self,
        client: NativeCapabilities,
        endpoint: Endpoint,
        batch: FfeFlagEvaluationBatch,
    ) {
        let destination_key = DestinationKey::new(endpoint, &batch.context);
        if self.inner.enqueue(destination_key, batch) {
            let coalescer = self.clone();
            tokio::spawn(async move {
                coalescer.flush_loop(client).await;
            });
        }
    }

    pub(crate) async fn flush_now(&self, client: NativeCapabilities) {
        let batches = self.inner.take_batches();
        futures::future::join_all(batches.into_iter().map(|(destination, batch)| {
            let client = client.clone();
            let coalescer = self.inner.clone();
            async move {
                send_batch_with_writer_stats(&client, &destination.endpoint, batch, &coalescer)
                    .await
            }
        }))
        .await;
    }

    async fn flush_loop(self, client: NativeCapabilities) {
        loop {
            tokio::time::sleep(COALESCE_DELAY).await;
            let batches = self.inner.take_batches();
            futures::future::join_all(batches.into_iter().map(|(destination, batch)| {
                let client = client.clone();
                let coalescer = self.inner.clone();
                async move {
                    send_batch_with_writer_stats(&client, &destination.endpoint, batch, &coalescer)
                        .await
                }
            }))
            .await;

            if self.inner.finish_flush_cycle() {
                break;
            }
        }
    }

    pub(crate) fn collect_writer_stats(&self) -> FlagEvaluationEvpWriterStats {
        self.inner.collect_writer_stats()
    }
}

/// Build the FFE flagevaluation endpoint from a session's agent base endpoint.
/// Overrides only the path (`/evp_proxy/v2/api/v2/flagevaluation`), preserving
/// scheme, authority, timeout, and test_token.
/// Returns `None` for agentless mode because EVP proxy routing is agent-only.
pub(crate) fn flagevaluation_endpoint(base: &Endpoint) -> Option<Endpoint> {
    ffe_evp_proxy::endpoint(base, EVP_FLAGEVALUATION_PATH)
}

/// POST a structured FFE flag evaluation batch to the agent EVP proxy.
/// Fire-and-forget: non-2xx responses are logged at `warn`, network errors at
/// `debug`, and dropped (matches dd-trace-go behaviour).
#[cfg(test)]
async fn send_batch<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
) {
    send_batch_with_limit(client, endpoint, batch, evp_proxy::PAYLOAD_SIZE_LIMIT, None).await;
}

async fn send_batch_with_writer_stats<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
    coalescer: &CommonFlagEvaluationEvpCoalescer<DestinationKey>,
) {
    send_batch_with_limit(
        client,
        endpoint,
        batch,
        evp_proxy::PAYLOAD_SIZE_LIMIT,
        Some(coalescer),
    )
    .await;
}

async fn send_batch_with_limit<C: HttpClientCapability + SleepCapability>(
    client: &C,
    endpoint: &Endpoint,
    batch: FfeFlagEvaluationBatch,
    payload_size_limit: usize,
    coalescer: Option<&CommonFlagEvaluationEvpCoalescer<DestinationKey>>,
) {
    let result = match encode_flag_evaluation_payloads(batch, payload_size_limit) {
        Ok(result) => result,
        Err(e) => {
            debug!("ffe_flagevaluation_flusher: failed to encode batch payload: {e:?}");
            return;
        }
    };

    if let Some(coalescer) = coalescer {
        coalescer.record_payload_build_result(&result);
    }

    if result.dropped_oversized_rows > 0 {
        warn!(
            "ffe_flagevaluation_flusher: dropped {} flag evaluation row(s) because they exceeded the {} byte EVP payload limit after degradation",
            result.dropped_oversized_rows,
            payload_size_limit
        );
    }

    for payload in result.payloads {
        ffe_evp_proxy::send_payload(
            client,
            endpoint,
            payload,
            LOG_PREFIX,
            "flag evaluation batch",
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{FfeFlagEvaluationBatch, FfeTelemetryContext};
    use datadog_ffe::telemetry::flagevaluation::{
        FfeFlagEvaluationEvent, FlagEvalEventContext, FlagKey, MAX_EVENTS_PER_POST,
    };
    use httpmock::MockServer;
    use libdd_capabilities::{Bytes, HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use std::collections::BTreeMap;
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

    fn eval_event() -> FfeFlagEvaluationEvent {
        FfeFlagEvaluationEvent {
            timestamp: 1_700_000_000_000,
            flag: FlagKey {
                key: "my-flag".to_owned(),
            },
            first_evaluation: 1_699_999_990_000,
            last_evaluation: 1_700_000_000_000,
            evaluation_count: 5,
            variant: None,
            allocation: None,
            targeting_rule: None,
            targeting_key: None,
            // `evaluation` is carried as a JSON-object STRING on the wire (bincode
            // can't carry serde_json::Value); the flusher re-expands it to an object.
            context: Some(FlagEvalEventContext {
                evaluation: Some(
                    serde_json::to_string(&{
                        let mut m = BTreeMap::new();
                        m.insert("country".to_owned(), serde_json::json!("US"));
                        m
                    })
                    .unwrap(),
                ),
                dd: None,
            }),
            error: None,
            runtime_default_used: false,
        }
    }

    fn batch() -> FfeFlagEvaluationBatch {
        FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: vec![eval_event()],
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn posts_to_evp_proxy() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();

        send_batch(&client, &ep, batch()).await;

        mock.assert_async().await;
        assert_eq!(mock.calls_async().await, 1);
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn splits_large_batches_before_posting() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let mut batch = batch();
        let event = batch.flag_evaluations[0].clone();
        batch.flag_evaluations = vec![event; MAX_EVENTS_PER_POST * 2 + 1];

        send_batch(&client, &ep, batch).await;

        mock.assert_calls_async(3).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn send_batch_splits_posts_by_encoded_byte_limit() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header(EVP_SUBDOMAIN_HEADER, EVP_SUBDOMAIN_VALUE)
                    .header("content-type", "application/json");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let mut batch = batch();
        let event = batch.flag_evaluations[0].clone();
        batch.flag_evaluations = vec![event; 3];
        let one_event_limit = encode_flag_evaluation_payloads(
            FfeFlagEvaluationBatch {
                context: context(),
                flag_evaluations: vec![batch.flag_evaluations[0].clone()],
            },
            usize::MAX,
        )
        .unwrap()
        .payloads
        .into_iter()
        .next()
        .unwrap()
        .len();

        send_batch_with_limit(&client, &ep, batch, one_event_limit, None).await;

        mock.assert_calls_async(3).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn coalesces_identical_batches_before_posting() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .body_includes("\"evaluation_count\":10");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let coalescer = FlagEvaluationCoalescer::default();

        coalescer.enqueue(client.clone(), ep.clone(), batch());
        coalescer.enqueue(client.clone(), ep, batch());

        for _ in 0..100 {
            if mock.calls_async().await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        mock.assert_calls_async(1).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn non_2xx_does_not_panic() {
        let server = MockServer::start_async().await;
        let _mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH);
                then.status(500).body("intake overloaded");
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        send_batch(&client, &ep, batch()).await;
        // Test passes if no panic occurs.
    }

    #[tokio::test]
    async fn timeout_returns_without_waiting_for_http_response() {
        let ep = Endpoint {
            url: "http://localhost:8126".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_batch(&HangingCapabilities, &ep, batch()).await;
        // Test passes if function returns before the pending HTTP future resolves.
    }

    #[test]
    fn endpoint_preserves_authority_overrides_path() {
        let base = Endpoint {
            url: "http://agent.internal:8126/v0.4/traces".parse().unwrap(),
            ..Endpoint::default()
        };
        let ep = flagevaluation_endpoint(&base).unwrap();
        assert_eq!(ep.url.scheme_str(), Some("http"));
        assert_eq!(ep.url.authority().unwrap().as_str(), "agent.internal:8126");
        assert_eq!(ep.url.path(), EVP_FLAGEVALUATION_PATH);
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
        assert!(flagevaluation_endpoint(&base).is_none());
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
