// Copyright 2026-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use std::time::Duration;

use libdd_capabilities::{HttpClientCapability, SleepCapability};
#[cfg(feature = "stats-obfuscation")]
use libdd_common::Endpoint;
use libdd_trace_stats::stats_exporter::AgentlessStatsExporter;
#[cfg(feature = "stats-obfuscation")]
use libdd_trace_stats::stats_exporter::{AgentlessStatsTarget, StatsMetadata};
use libdd_trace_utils::span::span_pool::PooledChunks;
use libdd_trace_utils::tracer_metadata::TracerMetadata;
use thiserror::Error;

use super::{send_agentless_traces, AgentlessError, AgentlessTraceConfig};

/// Configuration for agentless trace stats.
#[derive(Debug)]
pub struct AgentlessStatsConfig {
    /// Full URL for the agentless stats intake.
    pub endpoint_url: String,
    /// Stats aggregation and flush interval.
    pub bucket_size: Duration,
    /// Peer tags included in aggregation keys.
    pub peer_tags: Vec<String>,
    /// Additional metric tags included in aggregation keys.
    pub additional_metric_tag_keys: Vec<String>,
}

/// Errors returned by the agentless v0.4 exporter.
#[derive(Debug, Error)]
pub enum AgentlessV04Error {
    /// The v0.4 payload could not be decoded.
    #[error("failed to decode v0.4 traces: {0}")]
    Deserialization(libdd_trace_utils::msgpack_decoder::decode::error::DecodeError),
    /// The stats endpoint URL is invalid.
    #[error("invalid agentless stats endpoint URL: {0}")]
    InvalidStatsEndpoint(String),
    /// The stats interval must be greater than zero.
    #[error("agentless stats interval must be greater than zero")]
    InvalidStatsInterval,
    /// Agentless stats require resource obfuscation before direct intake export.
    #[error("agentless stats require the `stats-obfuscation` feature")]
    StatsObfuscationDisabled,
    /// Trace export failed.
    #[error(transparent)]
    Trace(#[from] AgentlessError),
}

/// Agentless v0.4 trace exporter with optional client-side stats.
pub struct AgentlessV04Exporter<C>
where
    C: HttpClientCapability + SleepCapability,
{
    capabilities: C,
    metadata: TracerMetadata,
    trace_config: AgentlessTraceConfig,
    stats: Option<AgentlessStatsExporter<C>>,
    traces_have_top_level: bool,
}

impl<C> AgentlessV04Exporter<C>
where
    C: Clone + HttpClientCapability + SleepCapability,
{
    /// Create an agentless v0.4 exporter.
    pub fn new(
        capabilities: C,
        mut metadata: TracerMetadata,
        trace_config: AgentlessTraceConfig,
        stats_config: Option<AgentlessStatsConfig>,
    ) -> Result<Self, AgentlessV04Error> {
        let traces_have_top_level = metadata.client_computed_top_level;
        let stats =
            create_stats_exporter(capabilities.clone(), &metadata, &trace_config, stats_config)?;
        if stats.is_some() {
            metadata.client_computed_stats = true;
            metadata.client_computed_top_level = true;
        }
        Ok(Self {
            capabilities,
            metadata,
            trace_config,
            stats,
            traces_have_top_level,
        })
    }

    /// Decode and send one v0.4 payload.
    pub async fn send_v04(&self, payload: &[u8]) -> Result<(), AgentlessV04Error> {
        let (traces, _) = libdd_trace_utils::msgpack_decoder::v04::from_slice(payload)
            .map_err(AgentlessV04Error::Deserialization)?;
        let mut traces = PooledChunks::unpooled(traces);
        if let Some(stats) = &self.stats {
            stats.add_traces(&mut traces, self.traces_have_top_level);
            libdd_trace_utils::span::trace_utils::drop_chunks(&mut traces);
            if traces.is_empty() {
                return Ok(());
            }
        }
        send_agentless_traces(
            &self.capabilities,
            traces,
            &self.metadata,
            &self.trace_config,
            self.stats.is_some(),
        )
        .await?;
        Ok(())
    }

    /// Flush and send stats. Returns `false` when stats are disabled or no buckets are due.
    pub async fn flush_stats(&self, force: bool) -> anyhow::Result<bool> {
        match &self.stats {
            Some(stats) => stats.send(force).await,
            None => Ok(false),
        }
    }
}

#[cfg(not(feature = "stats-obfuscation"))]
fn create_stats_exporter<C>(
    capabilities: C,
    metadata: &TracerMetadata,
    trace_config: &AgentlessTraceConfig,
    stats_config: Option<AgentlessStatsConfig>,
) -> Result<Option<AgentlessStatsExporter<C>>, AgentlessV04Error>
where
    C: HttpClientCapability + SleepCapability,
{
    let _ = (capabilities, metadata, trace_config);
    match stats_config {
        Some(_) => Err(AgentlessV04Error::StatsObfuscationDisabled),
        None => Ok(None),
    }
}

#[cfg(feature = "stats-obfuscation")]
fn create_stats_exporter<C>(
    capabilities: C,
    metadata: &TracerMetadata,
    trace_config: &AgentlessTraceConfig,
    stats_config: Option<AgentlessStatsConfig>,
) -> Result<Option<AgentlessStatsExporter<C>>, AgentlessV04Error>
where
    C: HttpClientCapability + SleepCapability,
{
    let Some(config) = stats_config else {
        return Ok(None);
    };
    if config.bucket_size.is_zero() {
        return Err(AgentlessV04Error::InvalidStatsInterval);
    }
    let url = libdd_common::parse_uri(&config.endpoint_url)
        .map_err(|error| AgentlessV04Error::InvalidStatsEndpoint(error.to_string()))?;
    if !matches!(url.scheme_str(), Some("http" | "https")) || url.host().is_none() {
        return Err(AgentlessV04Error::InvalidStatsEndpoint(config.endpoint_url));
    }
    let version = agentless_stats_version(&metadata.tracer_version, &metadata.language);
    let target = AgentlessStatsTarget {
        endpoint: Endpoint {
            url,
            api_key: Some(trace_config.api_key.clone().into()),
            timeout_ms: u64::try_from(trace_config.timeout.as_millis()).unwrap_or(u64::MAX),
            ..Endpoint::default()
        },
        version,
    };
    let mut stats_metadata = StatsMetadata::from(metadata.clone());
    stats_metadata
        .container_id
        .clone_from(&metadata.container_id);
    Ok(Some(AgentlessStatsExporter::new(
        config.bucket_size,
        stats_metadata,
        target,
        capabilities,
        config.peer_tags,
        config.additional_metric_tag_keys,
    )))
}

/// Build the `StatsPayload.agent_version` value for an agentless tracer.
pub fn agentless_stats_version(tracer_version: &str, language: &str) -> String {
    format!("{tracer_version}-{language}")
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use bytes::Bytes;
    use libdd_capabilities::HttpError;
    #[cfg(feature = "stats-obfuscation")]
    use libdd_tinybytes::BytesString;
    #[cfg(feature = "stats-obfuscation")]
    use libdd_trace_utils::span::v04::SpanBytes;

    use super::*;

    #[derive(Clone, Debug, Default)]
    struct TestCapabilities {
        requests: Arc<Mutex<Vec<http::Request<Bytes>>>>,
    }

    impl HttpClientCapability for TestCapabilities {
        fn new_client() -> Self {
            Self::default()
        }

        fn new_without_connection_pooling() -> Self {
            Self::default()
        }

        async fn request(
            &self,
            request: http::Request<Bytes>,
        ) -> Result<http::Response<Bytes>, HttpError> {
            self.requests.lock().unwrap().push(request);
            Ok(http::Response::builder()
                .status(http::StatusCode::ACCEPTED)
                .body(Bytes::new())
                .unwrap())
        }
    }

    impl SleepCapability for TestCapabilities {
        fn new() -> Self {
            Self::default()
        }

        async fn sleep(&self, _duration: Duration) {}
    }

    fn metadata() -> TracerMetadata {
        TracerMetadata {
            hostname: "host-1".to_string(),
            env: "prod".to_string(),
            app_version: "2.0.0".to_string(),
            runtime_id: "runtime-1".to_string(),
            service: "service-1".to_string(),
            tracer_version: "1.2.3".to_string(),
            language: "nodejs".to_string(),
            language_version: "v24".to_string(),
            language_interpreter: "v8".to_string(),
            container_id: "container-1".to_string(),
            ..Default::default()
        }
    }

    fn trace_config() -> AgentlessTraceConfig {
        AgentlessTraceConfig {
            endpoint_url: "https://traces.example.test/api/v0.4/traces".to_string(),
            api_key: "test-api-key".to_string(),
            timeout: Duration::from_secs(1),
            obfuscation_config: Default::default(),
        }
    }

    fn stats_config() -> AgentlessStatsConfig {
        AgentlessStatsConfig {
            endpoint_url: "https://stats.example.test/api/v0.2/stats".to_string(),
            bucket_size: Duration::from_secs(10),
            peer_tags: Vec::new(),
            additional_metric_tag_keys: Vec::new(),
        }
    }

    #[cfg(feature = "stats-obfuscation")]
    fn payload_with_sampling_priority(sampling_priority: Option<f64>) -> Vec<u8> {
        let span = SpanBytes {
            name: BytesString::from_static("operation"),
            service: BytesString::from_static("service-1"),
            resource: BytesString::from_static("resource-1"),
            trace_id: 1,
            span_id: 2,
            start: 1,
            duration: 2,
            metrics: sampling_priority
                .map(|priority| vec![("_sampling_priority_v1".into(), priority)].into())
                .unwrap_or_default(),
            ..Default::default()
        };
        libdd_trace_utils::msgpack_encoder::v04::to_vec_from_v04(&[vec![span]])
    }

    #[cfg(feature = "stats-obfuscation")]
    fn payload() -> Vec<u8> {
        payload_with_sampling_priority(None)
    }

    #[cfg(feature = "stats-obfuscation")]
    #[test]
    fn sends_v04_traces_and_flushes_stats() {
        let capabilities = TestCapabilities::default();
        let exporter = AgentlessV04Exporter::new(
            capabilities.clone(),
            metadata(),
            trace_config(),
            Some(stats_config()),
        )
        .unwrap();

        futures::executor::block_on(async {
            exporter.send_v04(&payload()).await.unwrap();
            assert!(exporter.flush_stats(true).await.unwrap());
        });

        let requests = capabilities.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].uri().path(), "/api/v0.4/traces");
        assert_eq!(requests[1].uri().path(), "/api/v0.2/stats");
        assert_eq!(
            requests[0].headers()["datadog-client-computed-stats"],
            "true"
        );
        assert_eq!(
            requests[0].headers()["datadog-client-computed-top-level"],
            "true"
        );
        assert_eq!(
            requests[1].headers()["datadog-client-computed-stats"],
            "true"
        );
        assert_eq!(
            requests[1].headers()["datadog-client-computed-top-level"],
            "true"
        );
    }

    #[cfg(feature = "stats-obfuscation")]
    #[test]
    fn aggregates_and_drops_priority_zero_traces() {
        let capabilities = TestCapabilities::default();
        let exporter = AgentlessV04Exporter::new(
            capabilities.clone(),
            metadata(),
            trace_config(),
            Some(stats_config()),
        )
        .unwrap();

        futures::executor::block_on(async {
            exporter
                .send_v04(&payload_with_sampling_priority(Some(0.0)))
                .await
                .unwrap();
            assert!(exporter.flush_stats(true).await.unwrap());
        });

        let requests = capabilities.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].uri().path(), "/api/v0.2/stats");
    }

    #[cfg(feature = "stats-obfuscation")]
    #[test]
    fn sends_v04_traces_without_stats() {
        let capabilities = TestCapabilities::default();
        let exporter =
            AgentlessV04Exporter::new(capabilities.clone(), metadata(), trace_config(), None)
                .unwrap();

        futures::executor::block_on(async {
            exporter.send_v04(&payload()).await.unwrap();
            assert!(!exporter.flush_stats(true).await.unwrap());
        });

        let requests = capabilities.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].uri().path(), "/api/v0.4/traces");
        assert!(!requests[0]
            .headers()
            .contains_key("datadog-client-computed-stats"));
    }

    #[cfg(not(feature = "stats-obfuscation"))]
    #[test]
    fn rejects_stats_without_obfuscation_support() {
        let result = AgentlessV04Exporter::new(
            TestCapabilities::default(),
            metadata(),
            trace_config(),
            Some(stats_config()),
        );

        assert!(matches!(
            result,
            Err(AgentlessV04Error::StatsObfuscationDisabled)
        ));
    }

    #[cfg(not(feature = "stats-obfuscation"))]
    #[test]
    fn exports_traces_without_stats_or_obfuscation_support() {
        let result = AgentlessV04Exporter::new(
            TestCapabilities::default(),
            metadata(),
            trace_config(),
            None,
        );

        assert!(result.is_ok());
    }

    #[cfg(feature = "stats-obfuscation")]
    #[test]
    fn rejects_an_invalid_stats_endpoint_before_sending() {
        for endpoint in ["/api/v0.2/stats", "ftp://stats.example.test/api/v0.2/stats"] {
            let mut stats = stats_config();
            stats.endpoint_url = endpoint.to_string();
            let result = AgentlessV04Exporter::new(
                TestCapabilities::default(),
                metadata(),
                trace_config(),
                Some(stats),
            );

            assert!(matches!(
                result,
                Err(AgentlessV04Error::InvalidStatsEndpoint(_))
            ));
        }
    }

    #[cfg(feature = "stats-obfuscation")]
    #[test]
    fn rejects_a_zero_stats_interval() {
        let mut stats = stats_config();
        stats.bucket_size = Duration::ZERO;
        let result = AgentlessV04Exporter::new(
            TestCapabilities::default(),
            metadata(),
            trace_config(),
            Some(stats),
        );

        assert!(matches!(
            result,
            Err(AgentlessV04Error::InvalidStatsInterval)
        ));
    }

    #[cfg(feature = "stats-obfuscation")]
    #[test]
    fn rejects_an_invalid_v04_payload() {
        let exporter = AgentlessV04Exporter::new(
            TestCapabilities::default(),
            metadata(),
            trace_config(),
            Some(stats_config()),
        )
        .unwrap();

        let result = futures::executor::block_on(exporter.send_v04(b"invalid"));
        assert!(matches!(result, Err(AgentlessV04Error::Deserialization(_))));
    }
}
