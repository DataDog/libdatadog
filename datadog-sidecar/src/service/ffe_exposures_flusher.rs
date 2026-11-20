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
use libdd_ffe::telemetry::exposures::encode_admitted_exposure_batch;
#[cfg(test)]
use libdd_ffe::telemetry::exposures::encode_exposure_batch_in_scope;
use libdd_ffe::telemetry::exposures::{admitted_exposure_payloads, is_complete};
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

const DEFERRED_ROWS: usize = 32 * 1024;
const HTTP_PAYLOAD_BYTES: usize = 4 * 1024 * 1024;
const DEFERRED_BYTES: usize = 16 * 1024 * 1024;

const ADMISSION_QUEUE_CAPACITY: usize = 1024;
const MAX_BATCH_ROWS: usize = 64;
const COALESCE_WINDOW: Duration = Duration::from_millis(1);
const FAST_ROW_BYTES: usize = 64 * 1024;

fn text_bytes(ctx: &FfeTelemetryContext, row: &FfeExposure) -> usize {
    ctx.service.len()
        + ctx.env.len()
        + ctx.version.len()
        + row.flag_key.len()
        + row.subject_id.len()
        + row.allocation_key.len()
        + row.variant.len()
        + row.features_json.len()
        + row.subject_attributes_json.len()
}

pub(super) fn fits_http(ctx: &FfeTelemetryContext, row: &FfeExposure) -> bool {
    // Even fully escaped fast-path text fits one 4 MiB body. Measure larger rows exactly.
    text_bytes(ctx, row) <= FAST_ROW_BYTES
        || admitted_exposure_payloads(
            FfeExposureBatch {
                context: ctx.clone(),
                exposures: vec![row.clone()],
            },
            HTTP_PAYLOAD_BYTES,
        )
        .is_ok_and(|mut bodies| matches!(bodies.next(), Some(Ok(_))))
}

async fn drain_admissions<C: HttpClientCapability + SleepCapability>(
    client: &C,
    rx: mpsc::Receiver<Job>,
) {
    dispatch::dispatch(rx, |route, batch| async move {
        match admitted_exposure_payloads(batch, HTTP_PAYLOAD_BYTES) {
            Ok(payloads) => {
                for body in payloads {
                    send_encoded(client, &route, body.map(Some)).await;
                }
            }
            Err(e) => debug!("ffe_exposures_flusher: failed to encode exposure context: {e:?}"),
        }
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
    #[serde(default)]
    pub(super) outstanding_rows: usize,
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
            outstanding_rows: self.capacity.outstanding_rows(),
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
        let (sender, receiver) = mpsc::channel(2 * (capacity + DEFERRED_ROWS));
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
            let (tx, rx) = mpsc::channel(2 * (ADMISSION_QUEUE_CAPACITY + DEFERRED_ROWS));
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
        let charge = dispatch::DeferredCharge::for_batch(&batch);
        if charge.bytes > FAST_ROW_BYTES
            && batch
                .exposures
                .iter()
                .any(|row| !fits_http(&batch.context, row))
        {
            self.invalid();
            return false;
        }
        let Some(budget) = self
            .capacity
            .reserve(transport.scope_handle(), Some(charge))
        else {
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
        if !is_complete(&event) || text_bytes(&context, &event) > FAST_ROW_BYTES {
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
        let first = a
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .body(expected("svc", &["A"]));
                then.status(202);
            })
            .await;
        let abc = a
            .mock_async(|when, then| {
                when.method(httpmock::Method::POST)
                    .body(expected("svc", &["B", "A", "C"]));
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
        first.assert_calls_async(1).await;
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

    #[test]
    fn writer_releases_capacity_during_the_next_coalescing_window() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(assert_writer_progress_during_coalescing());
    }

    async fn assert_writer_progress_during_coalescing() {
        let route = EvpTransport::agent_only(
            Endpoint {
                url: "http://localhost:8080/".parse().unwrap(),
                ..Endpoint::default()
            },
            EVP_SUBDOMAIN_VALUE,
        )
        .unwrap();
        let capacity = Capacity::new(1);
        let reserve = || capacity.reserve(route.scope_handle(), None);
        let (tx, rx) = mpsc::channel(3);
        let (writes, mut observed) = mpsc::unbounded_channel();
        let release = tokio::sync::Notify::new();
        let dispatcher = dispatch::dispatch(rx, |_, batch| {
            let writes = &writes;
            let release = &release;
            async move {
                let variants: Vec<_> = batch.exposures.into_iter().map(|row| row.variant).collect();
                let first = variants == ["A"];
                writes.send(variants).unwrap();
                if first {
                    release.notified().await;
                }
            }
        });
        tokio::pin!(dispatcher);
        let job = |variant, credit| {
            (
                route.clone(),
                FfeExposureBatch {
                    context: context(),
                    exposures: vec![exposure("user", "alloc", variant)],
                },
                credit,
            )
        };
        tx.try_send(job("A", Some(reserve().unwrap()))).unwrap();
        tokio::select! {
            _ = &mut dispatcher => panic!("writer exited with an open input"),
            first = observed.recv() => assert_eq!(first.unwrap(), ["A"]),
        }
        assert!(reserve().is_none());
        tx.try_send(job("B", None)).unwrap();
        assert!(futures::poll!(&mut dispatcher).is_pending());
        assert_eq!(tx.capacity(), tx.max_capacity());
        let start = tokio::time::Instant::now();
        release.notify_one();
        assert!(futures::poll!(&mut dispatcher).is_pending());
        assert!(
            reserve().is_some(),
            "A's credit stayed held during B's coalescing window"
        );
        assert_eq!(tokio::time::Instant::now(), start);
        assert!(observed.try_recv().is_err());
        tx.try_send(job("C", None)).unwrap();
        drop(tx);
        dispatcher.await;
        assert_eq!(observed.try_recv().unwrap(), ["B", "C"]);
        assert!(observed.try_recv().is_err());
    }

    #[test]
    fn small_checked_prefixes_pack_in_order_and_oversized_storage_cannot_commit() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let route = EvpTransport::agent_only(
                    Endpoint {
                        url: "http://localhost:8080/".parse().unwrap(),
                        ..Endpoint::default()
                    },
                    EVP_SUBDOMAIN_VALUE,
                )
                .unwrap();
                let cache = ExposureDeduplicator::new(1024);
                let (queue, rx) = AdmissionQueue::capture(1);
                let client = NativeCapabilities::new_client();
                let mut large = exposure("large", "alloc", "A");
                large.features_json = String::with_capacity(DEFERRED_BYTES / 2);
                let batch = |exposures| FfeExposureBatch {
                    context: context(),
                    exposures,
                };
                assert!(!queue.submit_deferred(&client, &route, &cache, batch(vec![large])));
                assert_eq!(queue.stats().outstanding_rows, 0);
                for i in 0..40 {
                    let rows = (i * 16..(i + 1) * 16)
                        .map(|n| exposure(&n.to_string(), "alloc", "A"))
                        .collect();
                    assert!(queue.submit_deferred(&client, &route, &cache, batch(rows)));
                }
                assert_eq!(
                    queue.admit(
                        &client,
                        &route,
                        &cache,
                        context(),
                        exposure("large", "alloc", "A"),
                        true
                    ),
                    ExposureVerdict::Admitted
                );
                assert_eq!(queue.stats().outstanding_rows, 641);
                let capacity = queue.capacity.clone();
                drop(queue);
                let (writes, mut received) = mpsc::unbounded_channel();
                dispatch::dispatch(rx, |_, batch| {
                    writes.send(batch).unwrap();
                    std::future::ready(())
                })
                .await;
                let mut sizes = vec![];
                let mut ids = vec![];
                while let Ok(batch) = received.try_recv() {
                    sizes.push(batch.exposures.len());
                    ids.extend(batch.exposures.into_iter().map(|e| e.subject_id));
                }
                assert_eq!(sizes, [512, 129]);
                assert_eq!(ids.pop().as_deref(), Some("large"));
                assert_eq!(ids, (0..640).map(|n| n.to_string()).collect::<Vec<_>>());
                assert_eq!(capacity.outstanding_rows(), 0);
            });
    }

    #[test]
    fn held_sink_delivers_disjoint_work_before_the_first_reply() {
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .start_paused(true)
            .build()
            .unwrap()
            .block_on(async {
                let route = EvpTransport::agent_only(
                    Endpoint {
                        url: "http://localhost:8080/".parse().unwrap(),
                        ..Endpoint::default()
                    },
                    EVP_SUBDOMAIN_VALUE,
                )
                .unwrap();
                let cache = ExposureDeduplicator::new(8);
                let (queue, rx) = AdmissionQueue::capture(8);
                let client = NativeCapabilities::new_client();
                let (writes, mut started) = mpsc::unbounded_channel();
                let gate = Arc::new(tokio::sync::Notify::new());
                let held = gate.clone();
                let writer = tokio::spawn(async move {
                    dispatch::dispatch(rx, |_, batch| {
                        let (writes, held) = (writes.clone(), held.clone());
                        async move {
                            let event = &batch.exposures[0];
                            writes.send(event.variant.clone()).unwrap();
                            if event.variant == "A" {
                                held.notified().await;
                            }
                        }
                    })
                    .await;
                });
                let record = |subject, variant| {
                    queue.admit(
                        &client,
                        &route,
                        &cache,
                        context(),
                        exposure(subject, "alloc", variant),
                        true,
                    )
                };
                assert_eq!(record("held", "A"), ExposureVerdict::Admitted);
                assert_eq!(started.recv().await.as_deref(), Some("A"));
                assert_eq!(record("disjoint", "X"), ExposureVerdict::Admitted);
                let later = tokio::time::timeout(Duration::from_secs(1), started.recv()).await;
                assert_eq!(
                    later.ok().flatten().as_deref(),
                    Some("X"),
                    "disjoint delivery waited for the first reply"
                );
                assert_eq!(queue.stats().outstanding_rows, 1);
                assert_eq!(record("held", "B"), ExposureVerdict::Admitted);
                assert_eq!(record("held", "B"), ExposureVerdict::Repeat);
                assert!(
                    tokio::time::timeout(Duration::from_secs(1), started.recv())
                        .await
                        .is_err(),
                    "a newer assignment overtook its earlier delivery"
                );
                assert_eq!(queue.stats().outstanding_rows, 2);
                gate.notify_one();
                assert_eq!(started.recv().await.as_deref(), Some("B"));
                assert_eq!(record("held", "B"), ExposureVerdict::Repeat);
                assert_eq!(queue.stats().outstanding_rows, 0);
                drop(queue);
                writer.await.unwrap();
            });
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
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let gate = release.clone();
        let task = tokio::spawn(async move {
            dispatch::dispatch(rx, move |_, batch| {
                let gate = gate.clone();
                let observed = observed.clone();
                async move {
                    let id = &batch.exposures[0].variant;
                    observed.send(id.clone()).unwrap();
                    if id.starts_with("slow-") {
                        gate.acquire().await.unwrap().forget();
                    }
                }
            })
            .await;
        });
        let job = |route, v: &str| {
            (
                route,
                FfeExposureBatch {
                    context: context(),
                    exposures: vec![exposure(v, "alloc", v)],
                },
                None,
            )
        };
        async fn next(rx: &mut mpsc::UnboundedReceiver<String>) -> String {
            tokio::time::timeout(Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap()
        }
        for i in 0..dispatch::ROUTE_WRITERS {
            let variant = format!("slow-{i}");
            tx.send(job(a.clone(), &variant)).await.unwrap();
            assert_eq!(next(&mut started).await, variant);
        }
        tx.send(job(a, "A2")).await.unwrap();
        tx.send(job(b, "B")).await.unwrap();
        assert_eq!(next(&mut started).await, "B");
        release.add_permits(dispatch::ROUTE_WRITERS);
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
