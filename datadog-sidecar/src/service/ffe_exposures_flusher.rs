// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Serializes and forwards Feature Flags exposure events through the session's
//! shared EVP transport.
//!
//! The writer supplies the logical `/api/v2/exposures` intake path. `AgentOnly`
//! uses the historical local EVP v2 route. `PreferLocalThenDirect` discovers a
//! compatible local v4-before-v2 route, requiring both EVP identity headers,
//! and otherwise uses authenticated direct intake.

#[cfg(test)]
use crate::service::evp_proxy::{
    EVENT_PLATFORM_INTAKE_SUBDOMAIN as EVP_SUBDOMAIN_VALUE,
    SUBDOMAIN_HEADER as EVP_SUBDOMAIN_HEADER,
};
use crate::service::evp_transport::EvpTransport;
use crate::service::{ExposureVerdict, FfeExposure, FfeExposureBatch, FfeTelemetryContext};
use libdd_capabilities::{HttpClientCapability, SleepCapability};
use libdd_capabilities_impl::NativeCapabilities;
pub(crate) use libdd_ffe::telemetry::exposures::ExposureDeduplicator;
#[cfg(test)]
use libdd_ffe::telemetry::exposures::encode_exposure_batch_in_scope;
use libdd_ffe::telemetry::exposures::{encode_admitted_exposure_batch, is_complete};
mod dispatch;

use dispatch::{Capacity, Credit};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;

/// Historical Agent-only EVP v2 path for Feature Flags exposure intake.
#[cfg(test)]
pub(crate) const EVP_EXPOSURES_PATH: &str = "/evp_proxy/v2/api/v2/exposures";
const EXPOSURES_INTAKE_PATH: &str = "/api/v2/exposures";

const LOG_PREFIX: &str = "ffe_exposures_flusher";

/// POST a structured Feature Flags exposure batch through the selected EVP route.
/// Fire-and-forget: non-2xx responses are logged at `warn`, network errors at
/// `debug`, and dropped (matches dd-trace-go behaviour).
#[cfg(test)]
pub(crate) async fn send_batch<C: HttpClientCapability + SleepCapability>(
    client: &C,
    transport: &EvpTransport,
    deduplicator: &ExposureDeduplicator,
    batch: FfeExposureBatch,
) {
    let deduplication_scope = transport.deduplication_scope();
    send_encoded(
        client,
        transport,
        encode_exposure_batch_in_scope(deduplicator, deduplication_scope, batch),
    )
    .await;
}

async fn send_encoded<C: HttpClientCapability + SleepCapability>(
    client: &C,
    transport: &EvpTransport,
    encoded: Result<Option<String>, serde_json::Error>,
) {
    let payload = match encoded {
        Ok(Some(payload)) => payload,
        Ok(None) => return,
        Err(e) => {
            debug!("ffe_exposures_flusher: failed to encode exposure payload: {e:?}");
            return;
        }
    };
    transport
        .send_payload(
            client,
            EXPOSURES_INTAKE_PATH,
            "application/json",
            payload.into(),
            LOG_PREFIX,
            "exposure batch",
        )
        .await;
}

pub(super) type Job = (EvpTransport, FfeExposureBatch, Option<Credit>);

const DEFERRED_SLOTS: usize = 32;
const DEFERRED_BYTES: usize = 16 * 1024 * 1024;

const ADMISSION_QUEUE_CAPACITY: usize = 1024;
const MAX_BATCH_ROWS: usize = 64;
const COALESCE_WINDOW: Duration = Duration::from_millis(1);

async fn drain_admissions<C: HttpClientCapability + SleepCapability>(
    client: &C,
    rx: mpsc::Receiver<Job>,
) {
    dispatch::dispatch(rx, |route, batch| async move {
        send_encoded(client, &route, encode_admitted_exposure_batch(batch)).await;
    })
    .await;
}

#[derive(Default)]
struct Counters {
    full: AtomicU64,
    closed: AtomicU64,
    invalid: AtomicU64,
    missing_transport: AtomicU64,
}

fn count(counter: &AtomicU64, reason: &'static str) {
    let total = counter.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    if total.is_power_of_two() {
        debug!(reason, total, "FFE exposure submission refused");
    }
}

#[derive(Default, Debug, serde::Serialize, serde::Deserialize)]
pub(super) struct AdmissionStats {
    pub(super) full: u64,
    pub(super) closed: u64,
    pub(super) invalid: u64,
    pub(super) missing_transport: u64,
}

#[derive(Clone, Default)]
pub(crate) struct AdmissionQueue {
    writer: Arc<OnceLock<mpsc::Sender<Job>>>,
    capacity: Capacity,
    counters: Arc<Counters>,
}

impl AdmissionQueue {
    pub(super) fn stats(&self) -> AdmissionStats {
        AdmissionStats {
            full: self.counters.full.load(Ordering::Relaxed),
            closed: self.counters.closed.load(Ordering::Relaxed),
            invalid: self.counters.invalid.load(Ordering::Relaxed),
            missing_transport: self.counters.missing_transport.load(Ordering::Relaxed),
        }
    }

    pub(super) fn missing_transport(&self) {
        count(&self.counters.missing_transport, "missing_transport");
    }

    pub(super) fn invalid(&self) {
        count(&self.counters.invalid, "invalid");
    }

    fn refusal(&self, reason: mpsc::error::TrySendError<()>) -> ExposureVerdict {
        match reason {
            mpsc::error::TrySendError::Full(()) => {
                count(&self.counters.full, "full");
                ExposureVerdict::QueueFull
            }
            mpsc::error::TrySendError::Closed(()) => {
                count(&self.counters.closed, "closed");
                ExposureVerdict::Unavailable
            }
        }
    }

    #[cfg(test)]
    pub(super) fn capture(capacity: usize) -> (Self, mpsc::Receiver<Job>) {
        let (sender, receiver) = mpsc::channel(2 * (capacity + DEFERRED_SLOTS));
        (
            Self {
                writer: Arc::new(OnceLock::from(sender)),
                capacity: Capacity::new(capacity),
                ..Self::default()
            },
            receiver,
        )
    }

    fn writer(&self, client: &NativeCapabilities) -> &mpsc::Sender<Job> {
        self.writer.get_or_init(|| {
            let (tx, rx) = mpsc::channel(2 * (ADMISSION_QUEUE_CAPACITY + DEFERRED_SLOTS));
            let client = client.clone();
            tokio::spawn(async move { drain_admissions(&client, rx).await });
            tx
        })
    }

    pub(super) fn submit_deferred(
        &self,
        client: &NativeCapabilities,
        transport: &EvpTransport,
        cache: &ExposureDeduplicator,
        batch: FfeExposureBatch,
    ) -> bool {
        let Some(bytes) = libdd_ipc::codec::encoded_size(&batch)
            .ok()
            .and_then(|n| usize::try_from(n).ok())
        else {
            return false;
        };
        let Some(budget) = self.capacity.reserve(transport.scope_handle(), Some(bytes)) else {
            self.refusal(mpsc::error::TrySendError::Full(()));
            return false;
        };
        let slot = match self.writer(client).try_reserve() {
            Ok(slot) => slot,
            Err(reason) => {
                self.refusal(reason);
                return false;
            }
        };
        cache.publish_reserved_batch(transport.deduplication_scope(), batch, |changed| {
            slot.send((transport.clone(), changed, Some(budget)));
        });
        true
    }

    pub(crate) fn admit(
        &self,
        client: &NativeCapabilities,
        transport: &EvpTransport,
        cache: &ExposureDeduplicator,
        context: FfeTelemetryContext,
        event: FfeExposure,
        deliver: bool,
    ) -> ExposureVerdict {
        if !is_complete(&event)
            || [
                &context.service,
                &context.env,
                &context.version,
                &event.flag_key,
                &event.subject_id,
                &event.allocation_key,
                &event.variant,
                &event.features_json,
                &event.subject_attributes_json,
            ]
            .iter()
            .map(|s| s.len())
            .sum::<usize>()
                > 65536
        {
            self.invalid();
            return ExposureVerdict::Unavailable;
        }
        match cache.try_record_in_scope(transport.deduplication_scope(), &context, &event, || {
            if deliver {
                let queue = self.writer(client);
                let credit = self
                    .capacity
                    .reserve(transport.scope_handle(), None)
                    .ok_or(mpsc::error::TrySendError::Full(()))?;
                let permit = queue.try_reserve()?;
                permit.send((
                    transport.clone(),
                    FfeExposureBatch {
                        context: context.clone(),
                        exposures: vec![event.clone()],
                    },
                    Some(credit),
                ));
            }
            Ok::<_, mpsc::error::TrySendError<()>>(())
        }) {
            Ok(true) => ExposureVerdict::Admitted,
            Ok(false) => ExposureVerdict::Repeat,
            Err(reason) => self.refusal(reason),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::{FfeExposure, FfeTelemetryContext};
    use httpmock::MockServer;
    use libdd_capabilities::{Bytes, HttpError, MaybeSend};
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_common::Endpoint;
    use std::future;
    use std::time::Duration;

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
            serial_id: None,
            features_json: String::new(),
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
        let transport = EvpTransport::agent_only(base, EVP_SUBDOMAIN_VALUE).unwrap();
        let client = NativeCapabilities::new_client();

        send_batch(
            &client,
            &transport,
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
    async fn batched_admissions_preserve_routes_contexts_and_order() {
        let a = MockServer::start_async().await;
        let b = MockServer::start_async().await;
        let route = |s: &MockServer| {
            EvpTransport::agent_only(endpoint_for(s), EVP_SUBDOMAIN_VALUE).unwrap()
        };
        let job = |s: &MockServer, service: &str, v: &str| {
            (
                route(s),
                FfeExposureBatch {
                    context: FfeTelemetryContext {
                        service: service.into(),
                        ..context()
                    },
                    exposures: vec![exposure("user", "allocation", v)],
                },
                None,
            )
        };
        let expected = |service: &str, variants: &[&str]| {
            let batch = FfeExposureBatch {
                context: FfeTelemetryContext {
                    service: service.into(),
                    ..context()
                },
                exposures: variants
                    .iter()
                    .map(|v| exposure("user", "allocation", v))
                    .collect(),
            };
            encode_admitted_exposure_batch(batch).unwrap().unwrap()
        };
        let abc = a
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .body(expected("svc", &["A", "B", "A", "C"]));
                then.status(202);
            })
            .await;
        let other = a
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .body(expected("other", &["A"]));
                then.status(202);
            })
            .await;
        let second = b
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .body(expected("svc", &["X"]));
                then.status(202);
            })
            .await;
        let (tx, rx) = mpsc::channel(6);
        for item in [
            job(&a, "svc", "A"),
            job(&b, "svc", "X"),
            job(&a, "other", "A"),
            job(&a, "svc", "B"),
            job(&a, "svc", "A"),
            job(&a, "svc", "C"),
        ] {
            tx.try_send(item).unwrap();
        }
        drop(tx);
        drain_admissions(&NativeCapabilities::new_client(), rx).await;
        abc.assert_calls_async(1).await;
        other.assert_calls_async(1).await;
        second.assert_calls_async(1).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn full_queue_drains_in_capped_batches_and_accepts_next_assignment() {
        let server = MockServer::start_async().await;
        let posts = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path(EVP_EXPOSURES_PATH);
                then.status(202);
            })
            .await;
        let client = NativeCapabilities::new_client();
        let route = EvpTransport::agent_only(endpoint_for(&server), EVP_SUBDOMAIN_VALUE).unwrap();
        let cache = ExposureDeduplicator::new(256);
        let (queue, rx) = AdmissionQueue::capture(MAX_BATCH_ROWS + 1);
        let observe = |variant: usize| {
            queue.admit(
                &client,
                &route,
                &cache,
                context(),
                exposure("u", "alloc", &variant.to_string()),
                true,
            )
        };
        for i in 0..=MAX_BATCH_ROWS {
            assert_eq!(observe(i), ExposureVerdict::Admitted);
        }
        assert_eq!(observe(MAX_BATCH_ROWS + 1), ExposureVerdict::QueueFull);
        assert_eq!(observe(MAX_BATCH_ROWS), ExposureVerdict::Repeat);
        let consumer = tokio::spawn(async move {
            drain_admissions(&NativeCapabilities::new_client(), rx).await;
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while posts.calls_async().await < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(observe(MAX_BATCH_ROWS + 1), ExposureVerdict::Admitted);
        drop(queue);
        consumer.await.unwrap();
        posts.assert_calls_async(3).await;
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn later_route_progresses_while_first_writer_is_held() {
        let first = MockServer::start_async().await;
        let second = MockServer::start_async().await;
        let a = EvpTransport::agent_only(endpoint_for(&first), EVP_SUBDOMAIN_VALUE).unwrap();
        let b = EvpTransport::agent_only(endpoint_for(&second), EVP_SUBDOMAIN_VALUE).unwrap();
        let (tx, rx) = mpsc::channel(4);
        let (observed, mut started) = mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let gate = release.clone();
        let task = tokio::spawn(async move {
            dispatch::dispatch(rx, move |_, batch| {
                let gate = gate.clone();
                let observed = observed.clone();
                async move {
                    let id = &batch.exposures[0].variant;
                    observed.send(id.clone()).unwrap();
                    if id == "A1" {
                        gate.notified().await;
                    }
                }
            })
            .await;
        });
        let job = |route, v| {
            (
                route,
                FfeExposureBatch {
                    context: context(),
                    exposures: vec![exposure("user", "alloc", v)],
                },
                None,
            )
        };
        tx.send(job(a.clone(), "A1")).await.unwrap();
        async fn next(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
            tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap()
        }
        assert_eq!(next(&mut started).await, "A1");
        tx.send(job(a, "A2")).await.unwrap();
        tx.send(job(b, "B")).await.unwrap();
        assert_eq!(next(&mut started).await, "B");
        release.notify_one();
        assert_eq!(next(&mut started).await, "A2");
        drop(tx);
        task.await.unwrap();
    }

    #[tokio::test]
    #[cfg_attr(miri, ignore)]
    async fn deduplicates_exposures_across_connections() {
        use crate::service::{ConnectionSession, InstanceId};

        let server = MockServer::start_async().await;
        let mock = server
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST).path(EVP_EXPOSURES_PATH);
                then.status(202);
            })
            .await;
        let client = NativeCapabilities::new_client();
        let deduplicator = ExposureDeduplicator::new(4);

        // PHP workers share a sidecar cache but each connection creates its own transport.
        for runtime in ["worker-1", "worker-2", "worker-3"] {
            let mut session = ConnectionSession::for_test(InstanceId::new("session", runtime));
            session.set_default_evp_transport(endpoint_for(&server), EVP_SUBDOMAIN_VALUE);
            let transport = session.get_evp_transport(EVP_SUBDOMAIN_VALUE).unwrap();
            send_batch(
                &client,
                &transport,
                &deduplicator,
                FfeExposureBatch {
                    context: context(),
                    exposures: vec![exposure("user", "alloc", "variant")],
                },
            )
            .await;
        }

        mock.assert_calls_async(1).await;
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
        let transport = EvpTransport::agent_only(base, EVP_SUBDOMAIN_VALUE).unwrap();
        let client = NativeCapabilities::new_client();
        send_batch(
            &client,
            &transport,
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
        let endpoint = Endpoint {
            url: "http://localhost:8126".parse().unwrap(),
            timeout_ms: 1,
            ..Endpoint::default()
        };

        send_batch(
            &HangingCapabilities,
            &EvpTransport::agent_only(endpoint, EVP_SUBDOMAIN_VALUE).unwrap(),
            &ExposureDeduplicator::new(4),
            FfeExposureBatch {
                context: context(),
                exposures: vec![exposure("user", "alloc", "variant")],
            },
        )
        .await;
    }

    #[derive(Clone, Debug)]
    struct HangingCapabilities;

    impl HttpClientCapability for HangingCapabilities {
        fn new_client() -> Self {
            Self
        }

        fn new_periodic() -> Self {
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
