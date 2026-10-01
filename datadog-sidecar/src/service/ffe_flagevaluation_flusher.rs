// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Coalesces Feature Flags evaluation batches and dispatches them through the
//! session's shared EVP transport.
//!
//! The writer supplies the logical `/api/v2/flagevaluation` intake path.
//! `AgentOnly` uses the historical local EVP v2 route.
//! `PreferLocalThenDirect` discovers a compatible local v4-before-v2 route,
//! requiring both EVP identity headers, and otherwise uses authenticated direct
//! intake. Delivery is fire-and-forget; failures follow the shared selector's
//! replay-safety rules.
//! Clients without explicit EVP configuration retain the legacy sender and its
//! optional originating-tracer identity headers.

use crate::service::evp_transport::EvpTransport;
use crate::service::{FfeFlagEvaluationBatch, FfeTelemetryContext};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common::Endpoint;
use libdd_ffe::telemetry::flagevaluation::{
    EVP_PAYLOAD_SIZE_LIMIT, FlagEvaluationEvpCoalescer as CommonFlagEvaluationEvpCoalescer,
    FlagEvaluationEvpSendConfig, FlagEvaluationEvpWriterStats, encode_flag_evaluation_payloads,
    flagevaluation_agent_proxy_endpoint, send_flag_evaluation_batch,
};
use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex as AsyncMutex;
use tracing::{debug, warn};

const COALESCE_DELAY: Duration = Duration::from_millis(250);
const USER_AGENT: &str = concat!("ddtrace-sidecar/", crate::sidecar_version!());
const FLAGEVALUATION_INTAKE_PATH: &str = "/api/v2/flagevaluation";
const LOG_PREFIX: &str = "ffe_flagevaluation_flusher";

pub(crate) const FLAG_EVALUATION_DROPPED_EVALUATIONS_METRIC: &str =
    "flagevaluation.evaluations.dropped";
pub(crate) const FLAG_EVALUATION_DEGRADED_EVALUATIONS_METRIC: &str =
    "flagevaluation.evaluations.degraded";
pub(crate) const FLAG_EVALUATION_PAYLOAD_SPLITS_METRIC: &str = "flagevaluation.payload.splits";

pub(crate) const FLAG_EVALUATION_REASON_DEGRADED_CAP: &str = "degraded_cap";
pub(crate) const FLAG_EVALUATION_REASON_CARDINALITY_CAP: &str = "cardinality_cap";
pub(crate) const FLAG_EVALUATION_REASON_PAYLOAD_LIMIT: &str = "payload_limit";

pub(crate) fn evp_origin_from_language(language: &str) -> Option<Cow<'_, str>> {
    if language.trim().is_empty() {
        return None;
    }

    Some(match language {
        "ruby" => Cow::Borrowed("dd-trace-rb"),
        "python" => Cow::Borrowed("dd-trace-py"),
        "javascript" | "nodejs" => Cow::Borrowed("dd-trace-js"),
        "rust" => Cow::Borrowed("dd-trace-rs"),
        language => Cow::Owned(format!("dd-trace-{language}")),
    })
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Delivery {
    Explicit(EvpTransport),
    Legacy {
        endpoint: Endpoint,
        send_config: FlagEvaluationEvpSendConfig,
    },
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct DestinationKey {
    delivery: Delivery,
    context: FfeTelemetryContext,
}

#[derive(Clone, Default)]
pub(crate) struct FlagEvaluationCoalescer {
    inner: CommonFlagEvaluationEvpCoalescer<DestinationKey>,
    flush_mutex: Arc<AsyncMutex<()>>,
}

impl FlagEvaluationCoalescer {
    pub(crate) fn enqueue(
        &self,
        client: NativeCapabilities,
        endpoint: Endpoint,
        batch: FfeFlagEvaluationBatch,
        origin: Option<impl AsRef<str>>,
        origin_version: impl AsRef<str>,
    ) {
        let mut send_config =
            FlagEvaluationEvpSendConfig::new(USER_AGENT).with_origin_version(origin_version);
        if let Some(origin) = origin {
            send_config = send_config.with_origin(origin);
        }
        self.enqueue_delivery(
            client,
            Delivery::Legacy {
                endpoint,
                send_config,
            },
            batch,
        );
    }

    pub(crate) fn enqueue_with_transport(
        &self,
        client: NativeCapabilities,
        transport: EvpTransport,
        batch: FfeFlagEvaluationBatch,
    ) {
        self.enqueue_delivery(client, Delivery::Explicit(transport), batch);
    }

    fn enqueue_delivery(
        &self,
        client: NativeCapabilities,
        delivery: Delivery,
        batch: FfeFlagEvaluationBatch,
    ) {
        let destination_key = DestinationKey {
            delivery,
            context: batch.context.clone(),
        };
        if self.inner.enqueue(destination_key, batch) {
            let coalescer = self.clone();
            tokio::spawn(async move {
                coalescer.flush_loop(client).await;
            });
        }
    }

    pub(crate) async fn flush_now(&self, client: NativeCapabilities) {
        let _guard = self.flush_mutex.lock().await;
        self.flush_available_batches(client).await;
    }

    async fn flush_available_batches(&self, client: NativeCapabilities) {
        let batches = self.inner.take_batches();
        futures::future::join_all(batches.into_iter().map(|(destination, batch)| {
            let client = client.clone();
            let coalescer = self.inner.clone();
            async move {
                match destination.delivery {
                    Delivery::Explicit(transport) => {
                        send_batch_with_writer_stats(&client, &transport, batch, &coalescer).await;
                    }
                    Delivery::Legacy {
                        endpoint,
                        send_config,
                    } => {
                        if let Some(result) =
                            send_flag_evaluation_batch(&client, &endpoint, batch, &send_config)
                                .await
                        {
                            coalescer.record_payload_build_result(&result);
                        }
                    }
                }
            }
        }))
        .await;
    }

    async fn flush_loop(self, client: NativeCapabilities) {
        loop {
            tokio::time::sleep(COALESCE_DELAY).await;
            {
                let _guard = self.flush_mutex.lock().await;
                self.flush_available_batches(client.clone()).await;
            }

            if self.inner.finish_flush_cycle() {
                break;
            }
        }
    }

    pub(crate) fn collect_writer_stats(&self) -> FlagEvaluationEvpWriterStats {
        self.inner.collect_writer_stats()
    }
}

/// Preserve the legacy fixed-v2 endpoint and direct-endpoint rejection for
/// clients which have not explicitly configured the shared EVP transport.
pub(crate) fn flagevaluation_endpoint(base: &Endpoint) -> Option<Endpoint> {
    flagevaluation_agent_proxy_endpoint(base)
}

async fn send_batch_with_writer_stats(
    client: &NativeCapabilities,
    transport: &EvpTransport,
    batch: FfeFlagEvaluationBatch,
    coalescer: &CommonFlagEvaluationEvpCoalescer<DestinationKey>,
) {
    let result = match encode_flag_evaluation_payloads(batch, EVP_PAYLOAD_SIZE_LIMIT) {
        Ok(result) => result,
        Err(error) => {
            debug!("{LOG_PREFIX}: failed to encode batch payload: {error:?}");
            return;
        }
    };

    if result.dropped_oversized_rows > 0 {
        warn!(
            "{LOG_PREFIX}: dropped {} flag evaluation row(s) because they exceeded the {} byte EVP payload limit after degradation",
            result.dropped_oversized_rows, EVP_PAYLOAD_SIZE_LIMIT
        );
    }

    for payload in &result.payloads {
        transport
            .send_payload(
                client,
                FLAGEVALUATION_INTAKE_PATH,
                "application/json",
                payload.clone().into(),
                LOG_PREFIX,
                "flag evaluation batch",
            )
            .await;
    }

    coalescer.record_payload_build_result(&result);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::evp_proxy::EVENT_PLATFORM_INTAKE_SUBDOMAIN;
    use crate::service::{FfeFlagEvaluationBatch, FfeTelemetryContext};
    use httpmock::MockServer;
    use libdd_capabilities::HttpClientCapability;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_common::Endpoint;
    use libdd_ffe::telemetry::flagevaluation::{
        EVP_FLAGEVALUATION_PATH, FfeFlagEvaluationEvent, FlagEvalEventContext, FlagKey,
    };
    use std::collections::BTreeMap;

    struct BorrowOnly(&'static str);

    impl AsRef<str> for BorrowOnly {
        fn as_ref(&self) -> &str {
            self.0
        }
    }

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

    #[test]
    fn enqueue_accepts_borrowed_producer_identity() {
        let coalescer = FlagEvaluationCoalescer::default();
        let client = NativeCapabilities::new_client();
        let batch = FfeFlagEvaluationBatch {
            context: context(),
            flag_evaluations: Vec::new(),
        };

        coalescer.enqueue(
            client,
            Endpoint::default(),
            batch,
            Some(BorrowOnly("dd-trace-php")),
            BorrowOnly("9.9.9"),
        );
    }

    #[test]
    fn self_telemetry_metric_names_describe_evaluation_count_units() {
        assert_eq!(
            FLAG_EVALUATION_DROPPED_EVALUATIONS_METRIC,
            "flagevaluation.evaluations.dropped"
        );
        assert_eq!(
            FLAG_EVALUATION_DEGRADED_EVALUATIONS_METRIC,
            "flagevaluation.evaluations.degraded"
        );
        assert_eq!(
            FLAG_EVALUATION_PAYLOAD_SPLITS_METRIC,
            "flagevaluation.payload.splits"
        );
    }

    #[test]
    fn derives_canonical_evp_origin_from_language() {
        for (language, expected) in [
            ("php", "dd-trace-php"),
            ("ruby", "dd-trace-rb"),
            ("python", "dd-trace-py"),
            ("javascript", "dd-trace-js"),
            ("nodejs", "dd-trace-js"),
            ("dotnet", "dd-trace-dotnet"),
            ("rust", "dd-trace-rs"),
            ("java", "dd-trace-java"),
        ] {
            assert_eq!(
                evp_origin_from_language(language).as_deref(),
                Some(expected)
            );
        }
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn coalesces_identical_batches_before_posting() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header("user-agent", USER_AGENT)
                    .header("DD-EVP-ORIGIN", "dd-trace-php")
                    .header("DD-EVP-ORIGIN-VERSION", "9.9.9")
                    .body_includes("\"evaluation_count\":10");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let coalescer = FlagEvaluationCoalescer::default();

        coalescer.enqueue(
            client.clone(),
            ep.clone(),
            batch(),
            Some("dd-trace-php"),
            "9.9.9",
        );
        coalescer.enqueue(client.clone(), ep, batch(), Some("dd-trace-php"), "9.9.9");

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
    async fn does_not_coalesce_different_producer_identities() {
        let server = MockServer::start_async().await;
        let baseline = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header("user-agent", USER_AGENT)
                    .header("DD-EVP-ORIGIN", "producer-a")
                    .header("DD-EVP-ORIGIN-VERSION", "1.0.0")
                    .body_includes("\"evaluation_count\":5");
                then.status(202);
            })
            .await;
        let different_origin = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header("user-agent", USER_AGENT)
                    .header("DD-EVP-ORIGIN", "producer-b")
                    .header("DD-EVP-ORIGIN-VERSION", "1.0.0")
                    .body_includes("\"evaluation_count\":5");
                then.status(202);
            })
            .await;
        let different_origin_version = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH)
                    .header("user-agent", USER_AGENT)
                    .header("DD-EVP-ORIGIN", "producer-a")
                    .header("DD-EVP-ORIGIN-VERSION", "2.0.0")
                    .body_includes("\"evaluation_count\":5");
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let ep = flagevaluation_endpoint(&base).unwrap();
        let client = NativeCapabilities::new_client();
        let coalescer = FlagEvaluationCoalescer::default();

        coalescer.enqueue(
            client.clone(),
            ep.clone(),
            batch(),
            Some("producer-a"),
            "1.0.0",
        );
        coalescer.enqueue(
            client.clone(),
            ep.clone(),
            batch(),
            Some("producer-b"),
            "1.0.0",
        );
        coalescer.enqueue(client.clone(), ep, batch(), Some("producer-a"), "2.0.0");
        coalescer.flush_now(client).await;

        baseline.assert_calls_async(1).await;
        different_origin.assert_calls_async(1).await;
        different_origin_version.assert_calls_async(1).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn flush_now_waits_for_in_flight_flush_section() {
        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .path(EVP_FLAGEVALUATION_PATH);
                then.status(202);
            })
            .await;

        let base = endpoint_for(&server);
        let transport = EvpTransport::agent_only(base, EVENT_PLATFORM_INTAKE_SUBDOMAIN).unwrap();
        let client = NativeCapabilities::new_client();
        let coalescer = FlagEvaluationCoalescer::default();
        let guard = coalescer.flush_mutex.lock().await;

        coalescer.enqueue_with_transport(client.clone(), transport, batch());

        let mut flush = tokio::spawn({
            let coalescer = coalescer.clone();
            async move {
                coalescer.flush_now(client).await;
            }
        });

        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut flush)
                .await
                .is_err(),
            "flush_now returned while another FFE flush section was in flight"
        );
        assert_eq!(mock.calls_async().await, 0);

        drop(guard);
        flush.await.unwrap();
        mock.assert_calls_async(1).await;
    }
}
