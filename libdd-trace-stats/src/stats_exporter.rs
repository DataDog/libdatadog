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
use libdd_capabilities::{HttpClientCapability, MaybeSend, SleepCapability};
use libdd_common::Endpoint;
use libdd_shared_runtime::Worker;
use libdd_trace_protobuf::pb;
use libdd_trace_utils::send_with_retry::{send_with_retry, RetryStrategy};
use libdd_trace_utils::trace_utils::TracerHeaderTags;
use std::fmt::Debug;
use tracing::error;

pub const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";

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

/// An exporter that concentrates and sends stats to the agent.
///
/// `Cap` is the capabilities bundle (HTTP + sleep). Leaf crates pin it to a
/// concrete type (`NativeCapabilities` or `WasmCapabilities`).
#[derive(Debug)]
pub struct StatsExporter<
    Cap: HttpClientCapability + SleepCapability,
    Con: FlushableConcentrator = SpanConcentrator,
> {
    flush_interval: time::Duration,
    concentrator: Arc<Mutex<Con>>,
    endpoint: Endpoint,
    meta: StatsMetadata,
    sequence_id: AtomicU64,
    capabilities: Cap,
}

impl<Cap: HttpClientCapability + SleepCapability, Con: FlushableConcentrator>
    StatsExporter<Cap, Con>
{
    /// Return a new StatsExporter
    ///
    /// - `flush_interval` the interval on which the concentrator is flushed
    /// - `concentrator` an impl of `FlushableConcentrator` storing the stats to be sent to the
    ///   agent
    /// - `meta` metadata used in ClientStatsPayload and as headers to send stats to the agent
    /// - `endpoint` the Endpoint used to send stats to the agent
    /// - `cancellation_token` Token used to safely shutdown the exporter by force flushing the
    ///   concentrator
    pub fn new(
        flush_interval: time::Duration,
        concentrator: Arc<Mutex<Con>>,
        meta: StatsMetadata,
        endpoint: Endpoint,
        capabilities: Cap,
    ) -> Self {
        Self {
            flush_interval,
            concentrator,
            endpoint,
            meta,
            sequence_id: AtomicU64::new(0),
            capabilities,
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
        let payload = self.flush(force_flush);
        if payload.stats.is_empty() {
            return Ok(false);
        }
        let body = rmp_serde::encode::to_vec_named(&payload)?;

        let mut headers: http::HeaderMap = TracerHeaderTags::from(&self.meta).into();

        headers.insert(
            http::header::CONTENT_TYPE,
            libdd_common::header::APPLICATION_MSGPACK,
        );

        let result = send_with_retry(
            &self.capabilities,
            &self.endpoint,
            body,
            &headers,
            &RetryStrategy::default(),
        )
        .await;

        match result {
            Ok(_) => Ok(true),
            Err(err) => {
                error!(?err, "Error with the StateExporter when sending stats");
                anyhow::bail!("Failed to send stats: {err}");
            }
        }
    }

    /// Flush stats from the concentrator into a payload
    ///
    /// # Arguments
    /// - `force_flush` if true, triggers a force flush on the concentrator causing all buckets to
    ///   be flushed regardless of their age.
    ///
    /// # Panic
    /// Will panic if another thread panicked while holding the concentrator lock in which
    /// case stats cannot be flushed since the concentrator might be corrupted.
    fn flush(&self, force_flush: bool) -> pb::ClientStatsPayload {
        let sequence = self.sequence_id.fetch_add(1, Ordering::Relaxed);
        encode_stats_payload(
            &self.meta,
            sequence,
            #[allow(clippy::unwrap_used)]
            self.concentrator.lock().unwrap().flush_buckets(force_flush),
        )
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
        tokio::time::sleep(self.flush_interval).await;
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
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_shared_runtime::SharedRuntime;
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
        let mut concentrator = SpanConcentrator::new(
            BUCKETS_DURATION,
            // Make sure the oldest bucket will be flushed on next send
            SystemTime::now() - BUCKETS_DURATION * 3,
            vec![],
            vec![],
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
        );

        let send_status = stats_exporter.send(true).await;
        send_status.unwrap_err();

        assert!(
            poll_for_mock_hit(&mut mock, 10, 100, 5, true).await,
            "Expected max retry attempts"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_run() {
        let shared_runtime = SharedRuntime::new().expect("Failed to create runtime");

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
        );
        let _handle = shared_runtime
            .spawn_worker(stats_exporter, true, &caps)
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
        let shared_runtime = SharedRuntime::new().expect("Failed to create runtime");

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
        );

        let _handle = shared_runtime
            .spawn_worker(stats_exporter, true, &caps)
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
}
