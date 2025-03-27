// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::{
    borrow::Borrow,
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time,
};

use datadog_trace_protobuf::pb;
use datadog_trace_utils::send_with_retry::{send_with_retry, RetryStrategy};
use ddcommon::Endpoint;
use hyper;
use log::error;
use tokio::select;
use tokio_util::sync::CancellationToken;

use crate::{span_concentrator::SpanConcentrator, trace_exporter::TracerMetadata};

const STATS_ENDPOINT_PATH: &str = "/v0.6/stats";

/// An exporter that concentrates and sends stats to the agent
#[derive(Debug)]
pub struct StatsExporter {
    flush_interval: time::Duration,
    concentrator: Arc<Mutex<SpanConcentrator>>,
    endpoint: Endpoint,
    meta: TracerMetadata,
    sequence_id: AtomicU64,
    cancellation_token: CancellationToken,
}

impl StatsExporter {
    /// Return a new StatsExporter
    ///
    /// - `flush_interval` the interval on which the concentrator is flushed
    /// - `concentrator` SpanConcentrator storing the stats to be sent to the agent
    /// - `meta` metadata used in ClientStatsPayload and as headers to send stats to the agent
    /// - `endpoint` the Endpoint used to send stats to the agent
    /// - `cancellation_token` Token used to safely shutdown the exporter by force flushing the
    ///   concentrator
    pub fn new(
        flush_interval: time::Duration,
        concentrator: Arc<Mutex<SpanConcentrator>>,
        meta: TracerMetadata,
        endpoint: Endpoint,
        cancellation_token: CancellationToken,
    ) -> Self {
        Self {
            flush_interval,
            concentrator,
            endpoint,
            meta,
            sequence_id: AtomicU64::new(0),
            cancellation_token,
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
    pub async fn send(&self, force_flush: bool) -> anyhow::Result<()> {
        let payload = self.flush(force_flush);
        if payload.stats.is_empty() {
            return Ok(());
        }
        let body = rmp_serde::encode::to_vec_named(&payload)?;

        let mut headers: HashMap<&'static str, String> = self.meta.borrow().into();

        headers.insert(
            hyper::header::CONTENT_TYPE.as_str(),
            ddcommon::header::APPLICATION_MSGPACK_STR.to_string(),
        );

        let result = send_with_retry(
            &self.endpoint,
            body,
            &headers,
            &RetryStrategy::default(),
            None,
        )
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(err) => {
                error!("Error with the StateExporter when sending: {err}");
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
            self.meta.borrow(),
            sequence,
            #[allow(clippy::unwrap_used)]
            self.concentrator
                .lock()
                .unwrap()
                .flush(time::SystemTime::now(), force_flush),
        )
    }

    /// Run loop of the stats exporter
    ///
    /// Once started, the stats exporter will flush and send stats on every `self.flush_interval`.
    /// If the `self.cancellation_token` is cancelled, the exporter will force flush all stats and
    /// return.
    pub async fn run(&mut self) {
        loop {
            select! {
                _ = self.cancellation_token.cancelled() => {
                    let _ = self.send(true).await;
                    break;
                },
                _ = tokio::time::sleep(self.flush_interval) => {
                        let _ = self.send(false).await;
                },
            };
        }
    }
}

fn encode_stats_payload(
    meta: &TracerMetadata,
    sequence: u64,
    buckets: Vec<pb::ClientStatsBucket>,
) -> pb::ClientStatsPayload {
    pb::ClientStatsPayload {
        hostname: meta.hostname.clone(),
        env: meta.env.clone(),
        lang: meta.language.clone(),
        version: meta.app_version.clone(),
        runtime_id: meta.runtime_id.clone(),
        tracer_version: meta.tracer_version.clone(),
        sequence,
        stats: buckets,
        git_commit_sha: meta.git_commit_sha.clone(),
        // These fields are unused or will be set by the Agent
        service: String::new(),
        container_id: String::new(),
        tags: Vec::new(),
        agent_aggregation: String::new(),
        image_tag: String::new(),
    }
}

/// Return the stats endpoint url to send stats to the agent at `agent_url`
pub fn stats_url_from_agent_url(agent_url: &str) -> anyhow::Result<hyper::Uri> {
    let mut parts = agent_url.parse::<hyper::Uri>()?.into_parts();
    parts.path_and_query = Some(hyper::http::uri::PathAndQuery::from_static(
        STATS_ENDPOINT_PATH,
    ));
    Ok(hyper::Uri::from_parts(parts)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_trace_utils::span::{trace_utils, SpanBytes};
    use datadog_trace_utils::test_utils::poll_for_mock_hit;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use time::Duration;
    use time::SystemTime;

    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}

    const BUCKETS_DURATION: Duration = Duration::from_secs(10);

    /// Fails to compile if stats exporter is not Send and Sync
    #[test]
    fn test_stats_exporter_sync_send() {
        let _ = is_send::<StatsExporter>;
        let _ = is_sync::<StatsExporter>;
    }

    fn get_test_metadata() -> TracerMetadata {
        TracerMetadata {
            hostname: "libdatadog-test".into(),
            env: "test".into(),
            app_version: "0.0.0".into(),
            language: "rust".into(),
            tracer_version: "0.0.0".into(),
            runtime_id: "e39d6d12-0752-489f-b488-cf80006c0378".into(),
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
            trace.push(SpanBytes {
                service: "libdatadog-test".into(),
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
                    .body_contains("libdatadog-test");
                then.status(200).body("");
            })
            .await;

        let stats_exporter = StatsExporter::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            CancellationToken::new(),
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

        let stats_exporter = StatsExporter::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            CancellationToken::new(),
        );

        let send_status = stats_exporter.send(true).await;
        send_status.unwrap_err();

        assert!(
            poll_for_mock_hit(&mut mock, 10, 100, 5, true).await,
            "Expected max retry attempts"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_run() {
        let server = MockServer::start_async().await;

        let mut mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/v0.6/stats")
                    .body_contains("libdatadog-test");
                then.status(200).body("");
            })
            .await;

        let mut stats_exporter = StatsExporter::new(
            BUCKETS_DURATION,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            CancellationToken::new(),
        );

        tokio::time::pause();
        tokio::spawn(async move {
            stats_exporter.run().await;
        });
        // Wait for the stats to be flushed
        tokio::time::sleep(BUCKETS_DURATION + Duration::from_secs(1)).await;
        // Resume time to sleep while the stats are being sent
        tokio::time::resume();
        assert!(
            poll_for_mock_hit(&mut mock, 10, 100, 1, false).await,
            "Expected max retry attempts"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[tokio::test]
    async fn test_cancellation_token() {
        let server = MockServer::start_async().await;

        let mut mock = server
            .mock_async(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/v0.6/stats")
                    .body_contains("libdatadog-test");
                then.status(200).body("");
            })
            .await;

        let buckets_duration = Duration::from_secs(10);
        let cancellation_token = CancellationToken::new();

        let mut stats_exporter = StatsExporter::new(
            buckets_duration,
            Arc::new(Mutex::new(get_test_concentrator())),
            get_test_metadata(),
            Endpoint::from_url(stats_url_from_agent_url(&server.url("/")).unwrap()),
            cancellation_token.clone(),
        );

        tokio::spawn(async move {
            stats_exporter.run().await;
        });
        // Cancel token to trigger force flush
        cancellation_token.cancel();

        assert!(
            poll_for_mock_hit(&mut mock, 10, 100, 1, false).await,
            "Expected max retry attempts"
        );
    }
}
