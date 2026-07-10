// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time,
};

use crate::span_concentrator::{FlushableConcentrator, SpanConcentrator};
use async_trait::async_trait;
use futures::stream::FuturesUnordered;
use futures::StreamExt as _;
use libdd_capabilities::{HttpClientCapability, MaybeSend, SleepCapability};
use libdd_common::Endpoint;
use libdd_shared_runtime::Worker;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::send_with_retry::{send_with_retry, RetryBackoffType, RetryStrategy};
use libdd_trace_utils::trace_utils::TracerHeaderTags;
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use std::fmt::Debug;
use tracing::error;

pub const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";

/// Health metric name for the number of spans collapsed.
pub const COLLAPSED_SPANS_HEALTH_METRIC: &str = "datadog.tracer.stats.collapsed_spans";

/// Telemetry metric name for the number of spans collapsed.
pub const COLLAPSED_SPANS_TELEMETRY_METRIC: &str = "tracers.stats_collapsed_spans";

/// Metadata needed by the stats exporter to annotate payloads and HTTP requests.
#[derive(Clone, Default, Debug)]
pub struct StatsMetadata {
    pub hostname: String,
    pub env: String,
    pub app_version: String,
    pub runtime_id: String,
    pub language: String,
    pub lang_version: String,
    pub lang_interpreter: String,
    pub lang_vendor: String,
    pub tracer_version: String,
    pub git_commit_sha: String,
    pub process_tags: String,
    pub service: String,
}

impl<'a> From<&'a StatsMetadata> for TracerHeaderTags<'a> {
    fn from(m: &'a StatsMetadata) -> TracerHeaderTags<'a> {
        TracerHeaderTags {
            lang: &m.language,
            lang_version: &m.lang_version,
            lang_interpreter: &m.lang_interpreter,
            lang_vendor: &m.lang_vendor,
            tracer_version: &m.tracer_version,
            ..Default::default()
        }
    }
}

impl From<TracerMetadata> for StatsMetadata {
    fn from(m: TracerMetadata) -> StatsMetadata {
        StatsMetadata {
            hostname: m.hostname,
            env: m.env,
            app_version: m.app_version,
            runtime_id: m.runtime_id,
            language: m.language,
            lang_version: m.language_version,
            lang_interpreter: m.language_interpreter,
            lang_vendor: m.language_interpreter_vendor,
            tracer_version: m.tracer_version,
            git_commit_sha: m.git_commit_sha,
            process_tags: m.process_tags,
            service: m.service,
        }
    }
}

/// An exporter that concentrates and sends stats to the agent.
///
/// `Cap` is the capabilities bundle (HTTP + sleep). Leaf crates pin it to a
/// concrete type (`NativeCapabilities` or `WasmCapabilities`).
#[derive(Debug)]
pub struct StatsExporter<
    Cap: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
    Con: FlushableConcentrator = SpanConcentrator,
> {
    flush_interval: time::Duration,
    concentrator: Arc<Mutex<Con>>,
    endpoint: Endpoint,
    meta: StatsMetadata,
    sequence_id: AtomicU64,
    capabilities: Cap,
    #[cfg(feature = "stats-obfuscation")]
    supported_obfuscation_version: &'static str,
    /// Optional telemetry handle and context key.
    #[cfg(feature = "telemetry")]
    telemetry: Option<(
        libdd_telemetry::worker::TelemetryWorkerHandle<Cap>,
        libdd_telemetry::metrics::ContextKey,
    )>,
    /// Optional DogStatsD client.
    #[cfg(feature = "dogstatsd")]
    dogstatsd: Option<libdd_dogstatsd_client::DogStatsDClient>,
}

impl<
        Cap: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
        Con: FlushableConcentrator,
    > StatsExporter<Cap, Con>
{
    /// Return a new StatsExporter
    ///
    /// - `flush_interval` the interval on which the concentrator is flushed
    /// - `concentrator` an impl of `FlushableConcentrator` storing the stats to be sent to the
    ///   agent
    /// - `meta` metadata used in ClientStatsPayload and as headers to send stats to the agent
    /// - `endpoint` the Endpoint used to send stats to the agent
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        flush_interval: time::Duration,
        concentrator: Arc<Mutex<Con>>,
        meta: StatsMetadata,
        endpoint: Endpoint,
        capabilities: Cap,
        #[cfg(feature = "stats-obfuscation")] supported_obfuscation_version: &'static str,
        #[cfg(feature = "telemetry")] telemetry: Option<
            libdd_telemetry::worker::TelemetryWorkerHandle<Cap>,
        >,
        #[cfg(feature = "dogstatsd")] dogstatsd: Option<libdd_dogstatsd_client::DogStatsDClient>,
    ) -> Self {
        #[cfg(feature = "telemetry")]
        let telemetry = telemetry.map(|handle| {
            let key = handle.register_metric_context(
                COLLAPSED_SPANS_TELEMETRY_METRIC.to_string(),
                vec![libdd_common::tag!("collapsed_spans", "whole_key")],
                libdd_telemetry::data::metrics::MetricType::Count,
                true,
                libdd_telemetry::data::metrics::MetricNamespace::Tracers,
            );
            (handle, key)
        });
        Self {
            flush_interval,
            concentrator,
            endpoint,
            meta,
            sequence_id: AtomicU64::new(0),
            capabilities,
            #[cfg(feature = "stats-obfuscation")]
            supported_obfuscation_version,
            #[cfg(feature = "telemetry")]
            telemetry,
            #[cfg(feature = "dogstatsd")]
            dogstatsd,
        }
    }

    /// Flush the stats stored in the concentrator and send them
    ///
    /// If the stats flushed from the concentrator contain at least one time bucket the stats are
    /// sent to `self.endpoint`. The stats are serialized as msgpack.
    ///
    /// # Errors
    /// The function will return an error in the following case:
    /// - The endpoint failed to build
    /// - The stats payload cannot be serialized as a valid http body
    /// - The http client failed while sending the request
    /// - The http status of the response is not 2xx
    ///
    /// # Panic
    /// Will panic if another thread panicked while holding the concentrator lock in which
    /// case stats cannot be flushed since the concentrator might be corrupted.
    /// Returns `Ok(true)` if stats were sent, `Ok(false)` if the concentrator had nothing to send.
    pub async fn send(&self, force_flush: bool) -> anyhow::Result<bool> {
        let flush = {
            #[allow(clippy::unwrap_used)]
            let mut concentrator = self.concentrator.lock().unwrap();
            concentrator.flush_buckets(force_flush)
        };

        if flush.collapsed_spans > 0 {
            #[cfg(feature = "telemetry")]
            if let Some((handle, key)) = &self.telemetry {
                let _ = handle.add_point(flush.collapsed_spans as f64, key, vec![]);
            }
            #[cfg(feature = "dogstatsd")]
            if let Some(client) = &self.dogstatsd {
                client.send(vec![libdd_dogstatsd_client::DogStatsDAction::Count(
                    COLLAPSED_SPANS_HEALTH_METRIC,
                    flush.collapsed_spans as i64,
                    [libdd_common::tag!("collapsed_spans", "whole_key")].iter(),
                )]);
            }
        }

        #[cfg(feature = "dogstatsd")]
        if let Some(client) = &self.dogstatsd {
            flush.collapsed_fields_metrics.emit_dogstatsd(client);
        }

        let futures = FuturesUnordered::new();

        if !flush.obfuscated_buckets.is_empty() {
            futures.push(self.send_payload(flush.obfuscated_buckets, true));
        }

        if !flush.unobfuscated_buckets.is_empty() {
            futures.push(self.send_payload(flush.unobfuscated_buckets, false));
        }

        let sent_stats = !futures.is_empty();

        futures
            .collect::<Vec<anyhow::Result<()>>>()
            .await
            .into_iter()
            .collect::<anyhow::Result<()>>()?;

        Ok(sent_stats)
    }

    /// Encode the given buckets into a stats payload and send it to the agent.
    ///
    /// `obfuscated` indicates whether the buckets were obfuscated client-side, in which case the
    /// `datadog-obfuscation-version` header is added.
    async fn send_payload(
        &self,
        buckets: Vec<pb::ClientStatsBucket>,
        #[cfg_attr(not(feature = "stats-obfuscation"), allow(unused))] obfuscated: bool,
    ) -> anyhow::Result<()> {
        let sequence = self.sequence_id.fetch_add(1, Ordering::Relaxed);
        let payload = encode_stats_payload(&self.meta, sequence, buckets);
        let body = rmp_serde::encode::to_vec_named(&payload)?;

        let mut headers: http::HeaderMap = TracerHeaderTags::from(&self.meta).into();
        headers.insert(
            http::header::CONTENT_TYPE,
            libdd_common::header::APPLICATION_MSGPACK,
        );
        #[cfg(feature = "stats-obfuscation")]
        if obfuscated {
            headers.insert(
                http::HeaderName::from_static("datadog-obfuscation-version"),
                http::HeaderValue::from_static(self.supported_obfuscation_version),
            );
        }

        let result = send_with_retry(
            &self.capabilities,
            &self.endpoint,
            body,
            &headers,
            &RetryStrategy::new(0, 0, RetryBackoffType::Constant, None),
        )
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => {
                error!(?err, "Error with the StatsExporter when sending stats");
                anyhow::bail!("Failed to send stats: {err}");
            }
        }
    }
}

#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
impl<
        Cap: HttpClientCapability + SleepCapability + MaybeSend + Sync + 'static,
        Con: FlushableConcentrator + Send + Debug,
    > Worker for StatsExporter<Cap, Con>
{
    async fn trigger(&mut self) {
        self.capabilities.sleep(self.flush_interval).await;
    }

    /// Flush and send stats on every trigger.
    async fn run(&mut self) {
        let _ = self.send(false).await; // bool return ignored by Worker
    }

    async fn shutdown(&mut self) {
        let _ = self.send(true).await;
    }
}

fn encode_stats_payload(
    meta: &StatsMetadata,
    sequence: u64,
    buckets: Vec<pb::ClientStatsBucket>,
) -> pb::ClientStatsPayload {
    pb::ClientStatsPayload {
        hostname: meta.hostname.clone(),
        env: if meta.env.is_empty() {
            "unknown-env".to_string()
        } else {
            meta.env.clone()
        },
        version: meta.app_version.clone(),
        runtime_id: meta.runtime_id.clone(),
        sequence,
        service: meta.service.clone(),
        stats: buckets,
        git_commit_sha: meta.git_commit_sha.clone(),
        process_tags: meta.process_tags.clone(),
        // These fields will be set by the Agent
        container_id: String::new(),
        tags: Vec::new(),
        agent_aggregation: String::new(),
        image_tag: String::new(),
        process_tags_hash: 0,
        lang: String::new(),
        tracer_version: String::new(),
    }
}

/// Return the stats endpoint url to send stats to the agent at `agent_url`
pub fn stats_url_from_agent_url(agent_url: &str) -> anyhow::Result<http::Uri> {
    let mut parts = agent_url.parse::<http::Uri>()?.into_parts();
    parts.path_and_query = Some(http::uri::PathAndQuery::from_static(STATS_ENDPOINT_PATH));
    Ok(http::Uri::from_parts(parts)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::span_concentrator::CardinalityLimitConfig;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime, SharedRuntime};
    use libdd_trace_utils::span::{trace_utils, v04::SpanSlice};
    use libdd_trace_utils::test_utils::poll_for_mock_hit;
    use time::Duration;
    use time::SystemTime;

    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}

    const BUCKETS_DURATION: Duration = Duration::from_secs(10);

    /// Fails to compile if stats exporter is not Send and Sync
    #[test]
    fn test_stats_exporter_sync_send() {
        let _ = is_send::<StatsExporter<NativeCapabilities>>;
        let _ = is_sync::<StatsExporter<NativeCapabilities>>;
    }

    fn get_test_metadata() -> StatsMetadata {
        StatsMetadata {
            hostname: "libdatadog-test".into(),
            env: "test".into(),
            app_version: "0.0.0".into(),
            language: "rust".into(),
            tracer_version: "0.0.0".into(),
            runtime_id: "e39d6d12-0752-489f-b488-cf80006c0378".into(),
            process_tags: "key1:value1,key2:value2".into(),
            ..Default::default()
        }
    }

    fn get_test_concentrator() -> SpanConcentrator {
        get_test_concentrator_with_obfuscation_config(
            #[cfg(feature = "stats-obfuscation")]
            None,
        )
    }

    fn get_test_concentrator_with_obfuscation_config(
        #[cfg(feature = "stats-obfuscation")] obfuscation_config: Option<
            crate::span_concentrator::SharedStatsComputationObfuscationConfig,
        >,
    ) -> SpanConcentrator {
        let mut concentrator = SpanConcentrator::new(
            BUCKETS_DURATION,
            // Make sure the oldest bucket will be flushed on next send
            SystemTime::now() - BUCKETS_DURATION * 3,
            vec![],
            vec![],
            None,
            vec![],
            #[cfg(feature = "stats-obfuscation")]
            obfuscation_config,
        );
        let mut trace = vec![];

        for i in 1..100 {
            trace.push(SpanSlice {
                service: "libdatadog-test",
                duration: i,
                ..Default::default()
            })
        }

        trace_utils::compute_top_level_span(trace.as_mut_slice());

        for span in trace.iter() {
            concentrator.add_span(span);
        }
        concentrator
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_send_stats() {
        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/v0.6/stats")
                    .body_includes("libdatadog-test")
                    .body_includes("key1:value1,key2:value2");
                then.status(200).body("");
            })
            .await;

        let stats_exporter = StatsExporter::<NativeCapabilities>::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            NativeCapabilities::new_client(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            None,
            #[cfg(feature = "dogstatsd")]
            None,
        );

        let send_status = stats_exporter.send(true).await;
        send_status.unwrap();

        mock.assert_async().await;
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_send_stats_fail() {
        let server = MockServer::start_async().await;

        let mut mock = server
            .mock_async(|_when, then| {
                then.status(503)
                    .header("content-type", "application/json")
                    .body(r#"{"status":"error"}"#);
            })
            .await;

        let stats_exporter = StatsExporter::<NativeCapabilities>::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            NativeCapabilities::new_client(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            None,
            #[cfg(feature = "dogstatsd")]
            None,
        );

        let send_status = stats_exporter.send(true).await;
        send_status.unwrap_err();

        assert!(
            poll_for_mock_hit(&mut mock, 10, 100, 1, true).await,
            "Expected a single attempt with no retries"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_run() {
        let shared_runtime = ForkSafeRuntime::new().expect("Failed to create runtime");

        let server = MockServer::start();

        let mut mock = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.6/stats")
                .body_includes("libdatadog-test")
                .body_includes("key1:value1,key2:value2");
            then.status(200).body("");
        });

        let caps = NativeCapabilities::new();
        let stats_exporter = StatsExporter::<NativeCapabilities>::new(
            // Use smaller buckets duration to speed up test
            Duration::from_secs(1),
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            caps.clone(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            None,
            #[cfg(feature = "dogstatsd")]
            None,
        );
        let _handle = shared_runtime
            .spawn_worker(stats_exporter, true)
            .expect("Failed to spawn worker");

        // Wait for stats to be flushed
        std::thread::sleep(Duration::from_secs(1));

        assert!(
            shared_runtime
                .block_on(poll_for_mock_hit(&mut mock, 10, 100, 1, false))
                .expect("Failed to use runtime"),
            "Expected max retry attempts"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_worker_shutdown() {
        let shared_runtime = ForkSafeRuntime::new().expect("Failed to create runtime");

        let server = MockServer::start();

        let mut mock = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.6/stats")
                .body_includes("libdatadog-test")
                .body_includes("key1:value1,key2:value2");
            then.status(200).body("");
        });

        let buckets_duration = Duration::from_secs(10);

        let caps = NativeCapabilities::new();
        let stats_exporter = StatsExporter::<NativeCapabilities>::new(
            buckets_duration,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            caps.clone(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            None,
            #[cfg(feature = "dogstatsd")]
            None,
        );

        let _handle = shared_runtime
            .spawn_worker(stats_exporter, true)
            .expect("Failed to spawn worker");

        shared_runtime.shutdown(None).unwrap();

        assert!(
            shared_runtime
                .block_on(poll_for_mock_hit(&mut mock, 10, 100, 1, false))
                .expect("Failed to get runtime"),
            "Expected max retry attempts"
        );
    }

    #[test]
    fn test_encode_stats_payload_defaults_empty_env() {
        // Test that empty env defaults to "unknown-env"
        let mut meta_with_empty_env = get_test_metadata();
        meta_with_empty_env.env = "".to_string();

        let buckets = vec![];
        let payload = encode_stats_payload(&meta_with_empty_env, 1, buckets.clone());

        assert_eq!(
            payload.env, "unknown-env",
            "Empty env should default to 'unknown-env'"
        );

        // Test that non-empty env is preserved
        let meta_with_env = get_test_metadata();
        let payload_with_env = encode_stats_payload(&meta_with_env, 2, buckets);

        assert_eq!(
            payload_with_env.env, "test",
            "Non-empty env should be preserved"
        );
    }
    #[cfg(feature = "stats-obfuscation")]
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_send_stats_with_obfuscation_header() {
        use crate::span_concentrator::StatsComputationObfuscationConfig;
        use arc_swap::ArcSwap;

        let server = MockServer::start_async().await;

        let mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .header("datadog-obfuscation-version", "1")
                    .path("/v0.6/stats")
                    .body_includes("libdatadog-test");
                then.status(200).body("");
            })
            .await;

        let concentrator = get_test_concentrator_with_obfuscation_config(Some(Arc::new(
            ArcSwap::from_pointee(StatsComputationObfuscationConfig {
                enabled: true,
                ..Default::default()
            }),
        )));

        let stats_exporter = StatsExporter::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(concentrator)),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            NativeCapabilities::new_client(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            None,
            #[cfg(feature = "dogstatsd")]
            None,
        );

        let send_status = stats_exporter.send(true).await;
        send_status.unwrap();

        mock.assert_async().await;
    }

    /// Build a concentrator with `max_entries_per_bucket = 1` pre-seeded with four distinct spans
    /// so that three spans are collapsed into the overflow bucket.
    fn get_collapsed_concentrator() -> SpanConcentrator {
        use libdd_trace_utils::span::{trace_utils, v04::SpanSlice};

        let mut concentrator = SpanConcentrator::new(
            BUCKETS_DURATION,
            SystemTime::now(),
            vec![],
            vec![],
            Some(CardinalityLimitConfig {
                whole_key_limit: 1, // max 1 distinct key → second span collapses
                ..Default::default()
            }),
            vec![],
            #[cfg(feature = "stats-obfuscation")]
            None,
        );

        let mut trace = vec![
            SpanSlice {
                service: "svc",
                resource: "resource-a",
                duration: 10,
                ..Default::default()
            },
            SpanSlice {
                service: "svc",
                resource: "resource-b",
                duration: 20,
                ..Default::default()
            },
            SpanSlice {
                service: "svc",
                resource: "resource-c",
                duration: 20,
                ..Default::default()
            },
            SpanSlice {
                service: "svc",
                resource: "resource-d",
                duration: 20,
                ..Default::default()
            },
        ];
        trace_utils::compute_top_level_span(trace.as_mut_slice());
        for span in &trace {
            concentrator.add_span(span);
        }
        concentrator
    }

    /// Verify that when `collapsed_spans == 0` the DogStatsD socket receives nothing.
    #[cfg(feature = "dogstatsd")]
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_no_emission_when_zero() {
        use std::net;

        let server = MockServer::start_async().await;
        server
            .mock_async(|_when, then| {
                then.status(200).body("");
            })
            .await;

        // Bind a UDP socket so we can detect whether anything arrives.
        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind UDP socket");
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(200)))
            .unwrap();
        let addr = socket.local_addr().unwrap().to_string();

        let dogstatsd_client =
            libdd_dogstatsd_client::DogStatsDClient::new(libdd_common::Endpoint::from_slice(&addr))
                .expect("failed to create dogstatsd client");

        // get_test_concentrator() has no cardinality collapse: collapsed_spans will be 0.
        let stats_exporter = StatsExporter::<NativeCapabilities>::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            NativeCapabilities::new_client(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            None,
            Some(dogstatsd_client),
        );

        stats_exporter.send(true).await.unwrap();

        // The socket must not have received any datagram.
        let mut buf = [0u8; 256];
        let result = socket.recv(&mut buf);
        assert!(
            result.is_err(),
            "No DogStatsD datagram expected when collapsed_spans == 0"
        );
    }

    /// Verify that `COLLAPSED_SPANS_METRIC` is emitted to DogStatsD when spans are collapsed.
    #[cfg(feature = "dogstatsd")]
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_collapsed_spans_dogstatsd() {
        use std::net;

        let server = MockServer::start_async().await;
        server
            .mock_async(|_when, then| {
                then.status(200).body("");
            })
            .await;

        let socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind UDP socket");
        socket
            .set_read_timeout(Some(std::time::Duration::from_millis(500)))
            .unwrap();
        let addr = socket.local_addr().unwrap().to_string();

        let dogstatsd_client =
            libdd_dogstatsd_client::DogStatsDClient::new(libdd_common::Endpoint::from_slice(&addr))
                .expect("failed to create dogstatsd client");

        let stats_exporter = StatsExporter::<NativeCapabilities>::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_collapsed_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            NativeCapabilities::new_client(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            None,
            Some(dogstatsd_client),
        );

        stats_exporter.send(true).await.unwrap();

        let mut buf = [0u8; 256];
        let n = socket
            .recv(&mut buf)
            .expect("expected a DogStatsD datagram");
        let datagram = std::str::from_utf8(&buf[..n]).expect("valid utf-8");
        assert_eq!(
            datagram, "datadog.tracer.stats.collapsed_spans:3|c|#collapsed_spans:whole_key",
            "DogStatsD datagram must match the expected format"
        );
    }

    /// Verify that `COLLAPSED_SPANS_METRIC` is enqueued to the telemetry worker when spans
    /// are collapsed. This does not verify the actual value of the metric.
    #[cfg(feature = "telemetry")]
    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_collapsed_spans_telemetry() {
        use libdd_telemetry::worker::TelemetryWorkerBuilder;

        let server = MockServer::start_async().await;
        server
            .mock_async(|_when, then| {
                then.status(200).body("");
            })
            .await;

        let (handle, _join_handle) = TelemetryWorkerBuilder::new(
            "test-host".to_string(),
            "test-service".to_string(),
            "rust".to_string(),
            "1.0".to_string(),
            "0.0.0".to_string(),
        )
        .spawn();

        let stats_exporter = StatsExporter::<NativeCapabilities>::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_collapsed_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            NativeCapabilities::new_client(),
            #[cfg(feature = "stats-obfuscation")]
            "1",
            #[cfg(feature = "telemetry")]
            Some(handle),
            #[cfg(feature = "dogstatsd")]
            None,
        );

        stats_exporter.send(true).await.unwrap();

        let stats_exporter_ref = &stats_exporter;
        let (handle_ref, _key) = stats_exporter_ref
            .telemetry
            .as_ref()
            .expect("telemetry must be set");
        let receiver = handle_ref.stats().expect("failed to request stats");
        let stats = receiver.await.expect("failed to receive stats");
        // metric_contexts == 1 verifies that exactly one metric name was registered
        // (i.e. COLLAPSED_SPANS_METRIC and nothing else).
        // metric_buckets.buckets == 1 verifies that a data point was recorded for it.
        // However it does not check the value of the data point.
        assert_eq!(
            stats.metric_contexts, 1,
            "exactly one metric context (COLLAPSED_SPANS_METRIC) should be registered"
        );
        assert_eq!(
            stats.metric_buckets.buckets, 1,
            "exactly one metric bucket expected after one collapsed-spans emission"
        );
    }
}
