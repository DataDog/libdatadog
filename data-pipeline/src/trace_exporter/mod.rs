// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
pub mod agent_response;
pub mod builder;
pub mod error;
pub mod metrics;
pub mod stats;
mod trace_serializer;
mod transport;

// Re-export the builder
pub use builder::TraceExporterBuilder;

use self::agent_response::AgentResponse;
use self::metrics::MetricsEmitter;
use self::stats::StatsComputationStatus;
use self::trace_serializer::TraceSerializer;
use self::transport::TransportClient;
use crate::agent_info::{AgentInfoFetcher, ResponseObserver};
use crate::pausable_worker::PausableWorker;
use crate::stats_exporter::StatsExporter;
use crate::telemetry::{SendPayloadTelemetry, TelemetryClient};
use crate::trace_exporter::agent_response::{
    AgentResponsePayloadVersion, DATADOG_RATES_PAYLOAD_VERSION_HEADER,
};
use crate::trace_exporter::error::{InternalErrorKind, RequestError, TraceExporterError};
use crate::{
    agent_info::{self, schema::AgentInfo},
    health_metrics,
    health_metrics::HealthMetric,
};
use arc_swap::{ArcSwap, ArcSwapOption};
use datadog_trace_utils::msgpack_decoder;
use datadog_trace_utils::send_with_retry::{
    send_with_retry, RetryStrategy, SendWithRetryError, SendWithRetryResult,
};
use datadog_trace_utils::span::{Span, SpanText};
use datadog_trace_utils::trace_utils::TracerHeaderTags;
use ddcommon::MutexExt;
use ddcommon::{hyper_migration, Endpoint};
use ddcommon::{tag, tag::Tag};
use ddtelemetry::worker::TelemetryWorker;
use dogstatsd_client::Client;
use http_body_util::BodyExt;
use hyper::http::uri::PathAndQuery;
use hyper::Uri;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{borrow::Borrow, collections::HashMap, str::FromStr};
use tokio::runtime::Runtime;
use tracing::{error, info, warn};

const INFO_ENDPOINT: &str = "/info";

/// TraceExporterInputFormat represents the format of the input traces.
/// The input format can be either Proxy or V0.4, where V0.4 is the default.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[repr(C)]
pub enum TraceExporterInputFormat {
    /// Proxy format is used when the traces are to be sent to the agent without processing them.
    /// The whole payload is sent as is to the agent.
    Proxy,
    #[allow(missing_docs)]
    #[default]
    V04,
    V05,
}

/// TraceExporterOutputFormat represents the format of the output traces.
/// The output format can be either V0.4 or v0.5, where V0.4 is the default.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[repr(C)]
pub enum TraceExporterOutputFormat {
    #[allow(missing_docs)]
    #[default]
    V04,
    V05,
}

impl TraceExporterOutputFormat {
    /// Add the agent trace endpoint path to the URL.
    fn add_path(&self, url: &Uri) -> Uri {
        add_path(
            url,
            match self {
                TraceExporterOutputFormat::V04 => "/v0.4/traces",
                TraceExporterOutputFormat::V05 => "/v0.5/traces",
            },
        )
    }
}

/// Add a path to the URL.
///
/// # Arguments
///
/// * `url` - The URL to which the path is to be added.
/// * `path` - The path to be added to the URL.
fn add_path(url: &Uri, path: &str) -> Uri {
    let p_and_q = url.path_and_query();

    #[allow(clippy::unwrap_used)]
    let new_p_and_q = match p_and_q {
        Some(pq) => {
            let p = pq.path();
            let mut p = p.strip_suffix('/').unwrap_or(p).to_owned();
            p.push_str(path);

            PathAndQuery::from_str(p.as_str())
        }
        None => PathAndQuery::from_str(path),
    }
    // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
    .unwrap();
    let mut parts = url.clone().into_parts();
    parts.path_and_query = Some(new_p_and_q);
    // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
    #[allow(clippy::unwrap_used)]
    Uri::from_parts(parts).unwrap()
}

#[derive(Clone, Default, Debug)]
pub struct TracerMetadata {
    pub hostname: String,
    pub env: String,
    pub app_version: String,
    pub runtime_id: String,
    pub service: String,
    pub tracer_version: String,
    pub language: String,
    pub language_version: String,
    pub language_interpreter: String,
    pub language_interpreter_vendor: String,
    pub git_commit_sha: String,
    pub client_computed_stats: bool,
    pub client_computed_top_level: bool,
}

impl<'a> From<&'a TracerMetadata> for TracerHeaderTags<'a> {
    fn from(tags: &'a TracerMetadata) -> TracerHeaderTags<'a> {
        TracerHeaderTags::<'_> {
            lang: &tags.language,
            lang_version: &tags.language_version,
            tracer_version: &tags.tracer_version,
            lang_interpreter: &tags.language_interpreter,
            lang_vendor: &tags.language_interpreter_vendor,
            client_computed_stats: tags.client_computed_stats,
            client_computed_top_level: tags.client_computed_top_level,
            ..Default::default()
        }
    }
}

impl<'a> From<&'a TracerMetadata> for HashMap<&'static str, String> {
    fn from(tags: &'a TracerMetadata) -> HashMap<&'static str, String> {
        TracerHeaderTags::from(tags).into()
    }
}

#[derive(Debug)]
pub(crate) struct TraceExporterWorkers {
    pub info: PausableWorker<AgentInfoFetcher>,
    pub stats: Option<PausableWorker<StatsExporter>>,
    pub telemetry: Option<PausableWorker<TelemetryWorker>>,
}

/// The TraceExporter ingest traces from the tracers serialized as messagepack and forward them to
/// the agent while applying some transformation.
///
/// # Proxy
/// If the input format is set as `Proxy`, the exporter will forward traces to the agent without
/// deserializing them.
///
/// # Features
/// When the input format is set to `V04` the TraceExporter will deserialize the traces and perform
/// some operation before sending them to the agent. The available operations are described below.
///
/// ## V07 Serialization
/// The Trace exporter can serialize the traces to V07 before sending them to the agent.
///
/// ## Stats computation
/// The Trace Exporter can compute stats on traces. In this case the trace exporter will start
/// another task to send stats when a time bucket expire. When this feature is enabled the
/// TraceExporter drops all spans that may not be sampled by the agent.
#[allow(missing_docs)]
enum DeserInputFormat {
    V04,
    V05,
}

#[derive(Debug)]
pub struct TraceExporter {
    endpoint: Endpoint,
    metadata: TracerMetadata,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    // TODO - do something with the response callback - https://datadoghq.atlassian.net/browse/APMSP-1019
    runtime: Arc<Mutex<Option<Arc<Runtime>>>>,
    /// None if dogstatsd is disabled
    dogstatsd: Option<Client>,
    common_stats_tags: Vec<Tag>,
    client_computed_top_level: bool,
    client_side_stats: ArcSwap<StatsComputationStatus>,
    previous_info_state: ArcSwapOption<String>,
    info_response_observer: ResponseObserver,
    telemetry: Option<TelemetryClient>,
    health_metrics_enabled: bool,
    workers: Arc<Mutex<TraceExporterWorkers>>,
    agent_payload_response_version: Option<AgentResponsePayloadVersion>,
    http_client: Arc<Mutex<Option<hyper_migration::HttpClient>>>,
}

impl TraceExporter {
    #[allow(missing_docs)]
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    /// Get or create the HTTP client for reuse
    fn get_http_client(&self) -> hyper_migration::HttpClient {
        let mut client_guard = self.http_client.lock_or_panic();
        match client_guard.as_ref() {
            Some(client) => client.clone(),
            None => {
                let client = hyper_migration::new_default_client();
                *client_guard = Some(client.clone());
                client
            }
        }
    }

    /// Return the existing runtime or create a new one and start all workers
    fn runtime(&self) -> Result<Arc<Runtime>, TraceExporterError> {
        let mut runtime_guard = self.runtime.lock_or_panic();
        match runtime_guard.as_ref() {
            Some(runtime) => {
                // Runtime already running
                Ok(runtime.clone())
            }
            None => {
                // Create a new current thread runtime with all features enabled
                let runtime = Arc::new(
                    tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(1)
                        .enable_all()
                        .build()?,
                );
                *runtime_guard = Some(runtime.clone());
                self.start_all_workers(&runtime)?;
                Ok(runtime)
            }
        }
    }

    /// Manually start all workers
    pub fn run_worker(&self) -> Result<(), TraceExporterError> {
        self.runtime()?;
        Ok(())
    }

    /// Start all workers with the given runtime
    fn start_all_workers(&self, runtime: &Arc<Runtime>) -> Result<(), TraceExporterError> {
        let mut workers = self.workers.lock_or_panic();

        self.start_info_worker(&mut workers, runtime)?;
        self.start_stats_worker(&mut workers, runtime)?;
        self.start_telemetry_worker(&mut workers, runtime)?;

        Ok(())
    }

    /// Start the info worker
    fn start_info_worker(
        &self,
        workers: &mut TraceExporterWorkers,
        runtime: &Arc<Runtime>,
    ) -> Result<(), TraceExporterError> {
        workers.info.start(runtime).map_err(|e| {
            TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(e.to_string()))
        })
    }

    /// Start the stats worker if present
    fn start_stats_worker(
        &self,
        workers: &mut TraceExporterWorkers,
        runtime: &Arc<Runtime>,
    ) -> Result<(), TraceExporterError> {
        if let Some(stats_worker) = &mut workers.stats {
            stats_worker.start(runtime).map_err(|e| {
                TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(e.to_string()))
            })?;
        }
        Ok(())
    }

    /// Start the telemetry worker if present
    fn start_telemetry_worker(
        &self,
        workers: &mut TraceExporterWorkers,
        runtime: &Arc<Runtime>,
    ) -> Result<(), TraceExporterError> {
        if let Some(telemetry_worker) = &mut workers.telemetry {
            telemetry_worker.start(runtime).map_err(|e| {
                TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(e.to_string()))
            })?;
            if let Some(client) = &self.telemetry {
                runtime.block_on(client.start());
            }
        }
        Ok(())
    }

    pub fn stop_worker(&self) {
        let runtime = self.runtime.lock_or_panic().take();
        if let Some(ref rt) = runtime {
            // Stop workers to save their state
            let mut workers = self.workers.lock_or_panic();
            rt.block_on(async {
                let _ = workers.info.pause().await;
                if let Some(stats_worker) = &mut workers.stats {
                    let _ = stats_worker.pause().await;
                };
                if let Some(telemetry_worker) = &mut workers.telemetry {
                    let _ = telemetry_worker.pause().await;
                };
            });
        }
        // Drop runtime to shutdown all threads
        drop(runtime);
    }

    /// Send msgpack serialized traces to the agent
    ///
    /// # Arguments
    ///
    /// * data: A slice containing the serialized traces. This slice should be encoded following the
    ///   input_format passed to the TraceExporter on creating.
    /// * trace_count: The number of traces in the data
    ///
    /// # Returns
    /// * Ok(AgentResponse): The response from the agent
    /// * Err(TraceExporterError): An error detailing what went wrong in the process
    pub fn send(
        &self,
        data: &[u8],
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        self.check_agent_info();

        let res = match self.input_format {
            TraceExporterInputFormat::Proxy => self.send_proxy(data.as_ref(), trace_count),
            TraceExporterInputFormat::V04 => self.send_deser(data, DeserInputFormat::V04),
            TraceExporterInputFormat::V05 => self.send_deser(data, DeserInputFormat::V05),
        }?;
        if matches!(&res, AgentResponse::Changed { body } if body.is_empty()) {
            return Err(TraceExporterError::Agent(
                error::AgentErrorKind::EmptyResponse,
            ));
        }

        Ok(res)
    }

    /// Safely shutdown the TraceExporter and all related tasks
    pub fn shutdown(mut self, timeout: Option<Duration>) -> Result<(), TraceExporterError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        if let Some(timeout) = timeout {
            match runtime
                .block_on(async { tokio::time::timeout(timeout, self.shutdown_async()).await })
            {
                Ok(()) => Ok(()),
                Err(_e) => Err(TraceExporterError::Shutdown(
                    error::ShutdownError::TimedOut(timeout),
                )),
            }
        } else {
            runtime.block_on(self.shutdown_async());
            Ok(())
        }
    }

    /// Future used inside `Self::shutdown`.
    ///
    /// This function should not take ownership of the trace exporter as it will cause the runtime
    /// stored in the trace exporter to be dropped in a non-blocking context causing a panic.
    async fn shutdown_async(&mut self) {
        let stats_status = self.client_side_stats.load();
        if let StatsComputationStatus::Enabled {
            cancellation_token, ..
        } = stats_status.as_ref()
        {
            cancellation_token.cancel();

            let stats_worker = self.workers.lock_or_panic().stats.take();

            if let Some(stats_worker) = stats_worker {
                let _ = stats_worker.join().await;
            }
        }
        if let Some(telemetry) = self.telemetry.take() {
            telemetry.shutdown().await;
            let telemetry_worker = self.workers.lock_or_panic().telemetry.take();

            if let Some(telemetry_worker) = telemetry_worker {
                let _ = telemetry_worker.join().await;
            }
        }
    }

    /// Check if agent info state has changed
    fn has_agent_info_state_changed(&self, agent_info: &Arc<AgentInfo>) -> bool {
        Some(agent_info.state_hash.as_str())
            != self
                .previous_info_state
                .load()
                .as_deref()
                .map(|s| s.as_str())
    }

    fn check_agent_info(&self) {
        if let Some(agent_info) = agent_info::get_agent_info() {
            if self.has_agent_info_state_changed(&agent_info) {
                match &**self.client_side_stats.load() {
                    StatsComputationStatus::Disabled => {}
                    StatsComputationStatus::DisabledByAgent { .. } => {
                        let ctx = stats::StatsContext {
                            metadata: &self.metadata,
                            endpoint_url: &self.endpoint.url,
                            runtime: &self.runtime,
                        };
                        stats::handle_stats_disabled_by_agent(
                            &ctx,
                            &agent_info,
                            &self.client_side_stats,
                            &self.workers,
                        );
                    }
                    StatsComputationStatus::Enabled {
                        stats_concentrator, ..
                    } => {
                        let ctx = stats::StatsContext {
                            metadata: &self.metadata,
                            endpoint_url: &self.endpoint.url,
                            runtime: &self.runtime,
                        };
                        stats::handle_stats_enabled(
                            &ctx,
                            &agent_info,
                            stats_concentrator,
                            &self.client_side_stats,
                            &self.workers,
                        );
                    }
                }
                self.previous_info_state
                    .store(Some(agent_info.state_hash.clone().into()))
            }
        }
    }

    /// !!! This function is only for testing purposes !!!
    ///
    /// Waits the agent info to be ready by checking the agent_info state.
    /// It will only return Ok after the agent info has been fetched at least once or Err if timeout
    /// has been reached
    ///
    /// In production:
    /// 1) We should not synchronously wait for this to be ready before sending traces
    /// 2) It's not guaranteed to not block forever, since the /info endpoint might not be
    ///    available.
    ///
    /// The `send`` function will check agent_info when running, which will only be available if the
    /// fetcher had time to reach to the agent.
    /// Since agent_info can enable CSS computation, waiting for this during testing can make
    /// snapshots non-deterministic.
    #[cfg(feature = "test-utils")]
    pub fn wait_agent_info_ready(&self, timeout: Duration) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        loop {
            if std::time::Instant::now().duration_since(start) > timeout {
                anyhow::bail!("Timeout waiting for agent info to be ready",);
            }
            if agent_info::get_agent_info().is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send_proxy(
        &self,
        data: &[u8],
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        self.send_data_to_url(
            data,
            trace_count,
            self.output_format.add_path(&self.endpoint.url),
        )
    }

    fn send_data_to_url(
        &self,
        data: &[u8],
        trace_count: usize,
        uri: Uri,
    ) -> Result<AgentResponse, TraceExporterError> {
        self.runtime()?.block_on(async {
            self.send_request_and_handle_response(data, trace_count, uri)
                .await
        })
    }

    /// Send HTTP request and handle the response
    async fn send_request_and_handle_response(
        &self,
        data: &[u8],
        trace_count: usize,
        uri: Uri,
    ) -> Result<AgentResponse, TraceExporterError> {
        let transport_client = TransportClient::new(
            &self.metadata,
            self.health_metrics_enabled,
            self.dogstatsd.as_ref(),
            &self.common_stats_tags,
        );
        let req = transport_client.build_trace_request(data, trace_count, uri);
        let client = self.get_http_client();
        match client.request(req).await {
            Ok(response) => {
                let response = hyper_migration::into_response(response);
                transport_client
                    .process_http_response(response, trace_count, data.len())
                    .await
            }
            Err(err) => self.handle_request_error(err, data.len(), trace_count),
        }
    }

    /// Handle HTTP request errors
    fn handle_request_error(
        &self,
        err: hyper_util::client::legacy::Error,
        payload_size: usize,
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        error!(
            error = %err,
            "Request to agent failed"
        );
        let type_tag = tag!("type", "network");
        self.emit_metric(
            HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
            Some(vec![&type_tag]),
        );
        // Emit dropped bytes metric for network/connection errors
        self.emit_metric(
            HealthMetric::Distribution(
                health_metrics::TRANSPORT_DROPPED_BYTES,
                payload_size as i64,
            ),
            None,
        );
        self.emit_metric(
            HealthMetric::Distribution(
                health_metrics::TRANSPORT_TRACES_DROPPED,
                trace_count as i64,
            ),
            None,
        );
        Err(TraceExporterError::from(err))
    }

    /// Emit a health metric to dogstatsd
    fn emit_metric(&self, metric: HealthMetric, custom_tags: Option<Vec<&Tag>>) {
        if self.health_metrics_enabled {
            let emitter = MetricsEmitter::new(self.dogstatsd.as_ref(), &self.common_stats_tags);
            emitter.emit(metric, custom_tags);
        }
    }

    /// Send a list of trace chunks to the agent
    ///
    /// # Arguments
    /// * trace_chunks: A list of trace chunks. Each trace chunk is a list of spans.
    ///
    /// # Returns
    /// * Ok(String): The response from the agent
    /// * Err(TraceExporterError): An error detailing what went wrong in the process
    pub fn send_trace_chunks<T: SpanText>(
        &self,
        trace_chunks: Vec<Vec<Span<T>>>,
    ) -> Result<AgentResponse, TraceExporterError> {
        self.check_agent_info();
        self.send_trace_chunks_inner(trace_chunks)
    }

    /// Deserializes, processes and sends trace chunks to the agent
    fn send_deser(
        &self,
        data: &[u8],
        format: DeserInputFormat,
    ) -> Result<AgentResponse, TraceExporterError> {
        let (traces, _) = match format {
            DeserInputFormat::V04 => msgpack_decoder::v04::from_slice(data),
            DeserInputFormat::V05 => msgpack_decoder::v05::from_slice(data),
        }
        .map_err(|e| {
            error!("Error deserializing trace from request body: {e}");
            self.emit_metric(
                HealthMetric::Count(health_metrics::DESERIALIZE_TRACES_ERRORS, 1),
                None,
            );
            TraceExporterError::Deserialization(e)
        })?;
        info!(
            trace_count = traces.len(),
            "Trace deserialization completed successfully"
        );
        self.emit_metric(
            HealthMetric::Count(health_metrics::DESERIALIZE_TRACES, traces.len() as i64),
            None,
        );

        self.send_trace_chunks_inner(traces)
    }

    /// Send traces payload to agent with retry and telemetry reporting
    async fn send_traces_with_telemetry(
        &self,
        endpoint: &Endpoint,
        mp_payload: Vec<u8>,
        headers: HashMap<&'static str, String>,
        chunks: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        let strategy = RetryStrategy::default();
        let payload_len = mp_payload.len();

        // Send traces to the agent
        let result = send_with_retry(endpoint, mp_payload, &headers, &strategy, None).await;

        // Emit http.requests health metric based on number of attempts
        let requests_count = match &result {
            Ok((_, attempts)) => *attempts as i64,
            Err(err) => match err {
                SendWithRetryError::Http(_, attempts) => *attempts as i64,
                SendWithRetryError::Timeout(attempts) => *attempts as i64,
                SendWithRetryError::Network(_, attempts) => *attempts as i64,
                SendWithRetryError::Build(attempts) => *attempts as i64,
            },
        };
        self.emit_metric(
            HealthMetric::Distribution(health_metrics::TRANSPORT_REQUESTS, requests_count),
            None,
        );

        // Send telemetry for the payload sending
        if let Some(telemetry) = &self.telemetry {
            if let Err(e) = telemetry.send(&SendPayloadTelemetry::from_retry_result(
                &result,
                payload_len as u64,
                chunks as u64,
            )) {
                error!(?e, "Error sending telemetry");
            }
        }

        self.handle_send_result(result, chunks, payload_len).await
    }

    fn send_trace_chunks_inner<T: SpanText>(
        &self,
        mut traces: Vec<Vec<Span<T>>>,
    ) -> Result<AgentResponse, TraceExporterError> {
        let mut header_tags: TracerHeaderTags = self.metadata.borrow().into();

        // Process stats computation
        stats::process_traces_for_stats(
            &mut traces,
            &mut header_tags,
            &self.client_side_stats,
            self.client_computed_top_level,
        );

        let serializer = TraceSerializer::new(
            self.output_format,
            self.agent_payload_response_version.as_ref(),
        );
        let prepared = serializer.prepare_traces_payload(traces, header_tags)?;

        let endpoint = Endpoint {
            url: self.get_agent_url(),
            ..self.endpoint.clone()
        };

        self.runtime()?.block_on(async {
            self.send_traces_with_telemetry(
                &endpoint,
                prepared.data,
                prepared.headers,
                prepared.chunk_count,
            )
            .await
        })
    }

    /// Handle the result of sending traces to the agent
    async fn handle_send_result(
        &self,
        result: SendWithRetryResult,
        chunks: usize,
        payload_len: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        // Always emit http.sent.* metrics regardless of success/failure
        self.emit_metric(
            HealthMetric::Distribution(health_metrics::TRANSPORT_SENT_BYTES, payload_len as i64),
            None,
        );
        self.emit_metric(
            HealthMetric::Distribution(health_metrics::TRANSPORT_TRACES_SENT, chunks as i64),
            None,
        );

        match result {
            Ok((response, _)) => {
                self.handle_agent_response(chunks, response, payload_len)
                    .await
            }
            Err(err) => self.handle_send_error(err, payload_len, chunks).await,
        }
    }

    /// Handle errors from send with retry operation
    async fn handle_send_error(
        &self,
        err: SendWithRetryError,
        payload_len: usize,
        chunks: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        error!(?err, "Error sending traces");

        // Only emit the error metric for non-HTTP errors here
        // HTTP errors will be handled by handle_http_send_error with specific status codes
        match &err {
            SendWithRetryError::Http(_, _) => {
                // Will be handled by handle_http_send_error
            }
            SendWithRetryError::Timeout(_) => {
                let type_tag = tag!("type", "timeout");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
                    Some(vec![&type_tag]),
                );
            }
            SendWithRetryError::Network(_, _) => {
                let type_tag = tag!("type", "network");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
                    Some(vec![&type_tag]),
                );
            }
            SendWithRetryError::Build(_) => {
                let type_tag = tag!("type", "build");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
                    Some(vec![&type_tag]),
                );
            }
        };

        match err {
            SendWithRetryError::Http(response, _) => {
                self.handle_http_send_error(response, payload_len, chunks)
                    .await
            }
            SendWithRetryError::Timeout(_) => Err(TraceExporterError::from(io::Error::from(
                io::ErrorKind::TimedOut,
            ))),
            SendWithRetryError::Network(err, _) => Err(TraceExporterError::from(err)),
            SendWithRetryError::Build(_) => Err(TraceExporterError::from(io::Error::from(
                io::ErrorKind::Other,
            ))),
        }
    }

    /// Handle HTTP error responses from send with retry
    async fn handle_http_send_error(
        &self,
        response: hyper::Response<hyper_migration::Body>,
        payload_len: usize,
        chunks: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        let status = response.status();

        // Check if the agent state has changed for error responses
        self.info_response_observer.check_response(&response);

        // Emit send traces errors metric with status code type
        let type_tag =
            Tag::new("type", status.as_str()).unwrap_or_else(|_| tag!("type", "unknown"));
        self.emit_metric(
            HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
            Some(vec![&type_tag]),
        );

        // Emit dropped bytes metric for HTTP error responses, excluding 404 and 415
        if status.as_u16() != 404 && status.as_u16() != 415 {
            self.emit_metric(
                HealthMetric::Distribution(
                    health_metrics::TRANSPORT_DROPPED_BYTES,
                    payload_len as i64,
                ),
                None,
            );
            self.emit_metric(
                HealthMetric::Distribution(health_metrics::TRANSPORT_TRACES_DROPPED, chunks as i64),
                None,
            );
        }

        let body = self.read_error_response_body(response).await?;
        Err(TraceExporterError::Request(RequestError::new(
            status,
            &String::from_utf8_lossy(&body),
        )))
    }

    /// Read response body from error response
    async fn read_error_response_body(
        &self,
        response: hyper::Response<hyper_migration::Body>,
    ) -> Result<bytes::Bytes, TraceExporterError> {
        match response.into_body().collect().await {
            Ok(body) => Ok(body.to_bytes()),
            Err(err) => {
                error!(?err, "Error reading agent response body");
                Err(TraceExporterError::from(err))
            }
        }
    }

    /// Check if the agent's payload version has changed based on response headers
    fn check_payload_version_changed(
        &self,
        response: &hyper::Response<hyper_migration::Body>,
    ) -> bool {
        let status = response.status();
        match (
            status.is_success(),
            self.agent_payload_response_version.as_ref(),
            response.headers().get(DATADOG_RATES_PAYLOAD_VERSION_HEADER),
        ) {
            (false, _, _) => {
                // If the status is not success, the rates are considered unchanged
                false
            }
            (true, None, _) => {
                // if the agent_payload_response_version fingerprint is not enabled we always
                // return the new rates
                true
            }
            (true, Some(agent_payload_response_version), Some(new_payload_version)) => {
                if let Ok(new_payload_version_str) = new_payload_version.to_str() {
                    agent_payload_response_version.check_and_update(new_payload_version_str)
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Read response body and handle potential errors
    async fn read_response_body(
        &self,
        response: hyper::Response<hyper_migration::Body>,
    ) -> Result<String, TraceExporterError> {
        match response.into_body().collect().await {
            Ok(body) => Ok(String::from_utf8_lossy(&body.to_bytes()).to_string()),
            Err(err) => {
                error!(?err, "Error reading agent response body");
                let type_tag = tag!("type", "response_body");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
                    Some(vec![&type_tag]),
                );
                Err(TraceExporterError::from(err))
            }
        }
    }

    /// Handle successful trace sending response
    fn handle_successful_trace_response(
        &self,
        chunks: usize,
        status: hyper::StatusCode,
        body: String,
        payload_version_changed: bool,
    ) -> Result<AgentResponse, TraceExporterError> {
        info!(
            chunks = chunks,
            status = %status,
            "Trace chunks sent successfully to agent"
        );
        self.emit_metric(
            HealthMetric::Count(health_metrics::TRANSPORT_TRACES_SUCCESSFUL, chunks as i64),
            None,
        );

        Ok(if payload_version_changed {
            AgentResponse::Changed { body }
        } else {
            AgentResponse::Unchanged
        })
    }

    async fn handle_agent_response(
        &self,
        chunks: usize,
        response: hyper::Response<hyper_migration::Body>,
        payload_len: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        // Check if the agent state has changed
        self.info_response_observer.check_response(&response);

        let status = response.status();
        let payload_version_changed = self.check_payload_version_changed(&response);
        let body = self.read_response_body(response).await?;

        if !status.is_success() {
            warn!(
                status = %status,
                "Agent returned non-success status for trace send"
            );
            let type_tag =
                Tag::new("type", status.as_str()).unwrap_or_else(|_| tag!("type", "unknown"));
            self.emit_metric(
                HealthMetric::Count(health_metrics::TRANSPORT_TRACES_FAILED, 1),
                Some(vec![&type_tag]),
            );
            // Emit dropped bytes metric for non-success status codes, excluding 404 and 415
            if status.as_u16() != 404 && status.as_u16() != 415 {
                self.emit_metric(
                    HealthMetric::Distribution(
                        health_metrics::TRANSPORT_DROPPED_BYTES,
                        payload_len as i64,
                    ),
                    None,
                );
                self.emit_metric(
                    HealthMetric::Distribution(
                        health_metrics::TRANSPORT_TRACES_DROPPED,
                        chunks as i64,
                    ),
                    None,
                );
            }
            return Err(TraceExporterError::Request(RequestError::new(
                status, &body,
            )));
        }

        self.handle_successful_trace_response(chunks, status, body, payload_version_changed)
    }

    fn get_agent_url(&self) -> Uri {
        self.output_format.add_path(&self.endpoint.url)
    }

    #[cfg(test)]
    /// Test only function to check if the stats computation is active and the worker is running
    pub fn is_stats_worker_active(&self) -> bool {
        stats::is_stats_worker_active(&self.client_side_stats, &self.workers)
    }
}

#[derive(Debug, Default, Clone)]
pub struct TelemetryConfig {
    pub heartbeat: u64,
    pub runtime_id: Option<String>,
    pub debug_enabled: bool,
}

#[allow(missing_docs)]
pub trait ResponseCallback {
    #[allow(missing_docs)]
    fn call(&self, response: &str);
}

#[cfg(test)]
mod tests {
    use self::error::AgentErrorKind;
    use super::*;
    use datadog_trace_utils::msgpack_encoder;
    use datadog_trace_utils::span::v05;
    use datadog_trace_utils::span::SpanBytes;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use std::collections::HashMap;
    use std::net;
    use std::time::Duration;
    use tinybytes::BytesString;
    use tokio::time::sleep;

    // v05 messagepack empty payload -> [[""], []]
    const V5_EMPTY: [u8; 4] = [0x92, 0x91, 0xA0, 0x90];

    #[test]
    fn test_from_tracer_tags_to_tracer_header_tags() {
        let tracer_tags = TracerMetadata {
            tracer_version: "v0.1".to_string(),
            language: "rust".to_string(),
            language_version: "1.52.1".to_string(),
            language_interpreter: "rustc".to_string(),
            language_interpreter_vendor: "rust-lang".to_string(),
            client_computed_stats: true,
            client_computed_top_level: true,
            ..Default::default()
        };

        let tracer_header_tags: TracerHeaderTags = (&tracer_tags).into();

        assert_eq!(tracer_header_tags.tracer_version, "v0.1");
        assert_eq!(tracer_header_tags.lang, "rust");
        assert_eq!(tracer_header_tags.lang_version, "1.52.1");
        assert_eq!(tracer_header_tags.lang_interpreter, "rustc");
        assert_eq!(tracer_header_tags.lang_vendor, "rust-lang");
        assert!(tracer_header_tags.client_computed_stats);
        assert!(tracer_header_tags.client_computed_top_level);
    }

    #[test]
    fn test_from_tracer_tags_to_hashmap() {
        let tracer_tags = TracerMetadata {
            tracer_version: "v0.1".to_string(),
            language: "rust".to_string(),
            language_version: "1.52.1".to_string(),
            language_interpreter: "rustc".to_string(),
            client_computed_stats: true,
            client_computed_top_level: true,
            ..Default::default()
        };

        let hashmap: HashMap<&'static str, String> = (&tracer_tags).into();

        assert_eq!(hashmap.get("datadog-meta-tracer-version").unwrap(), "v0.1");
        assert_eq!(hashmap.get("datadog-meta-lang").unwrap(), "rust");
        assert_eq!(hashmap.get("datadog-meta-lang-version").unwrap(), "1.52.1");
        assert_eq!(
            hashmap.get("datadog-meta-lang-interpreter").unwrap(),
            "rustc"
        );
        assert!(hashmap.contains_key("datadog-client-computed-stats"));
        assert!(hashmap.contains_key("datadog-client-computed-top-level"));
    }

    fn read(socket: &net::UdpSocket) -> String {
        let mut buf = [0; 1_000];
        socket.recv(&mut buf).expect("No data");
        let datagram = String::from_utf8_lossy(buf.as_ref());
        datagram.trim_matches(char::from(0)).to_string()
    }

    fn build_test_exporter(
        url: String,
        dogstatsd_url: Option<String>,
        input: TraceExporterInputFormat,
        output: TraceExporterOutputFormat,
        enable_telemetry: bool,
        enable_health_metrics: bool,
    ) -> TraceExporter {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&url)
            .set_service("test")
            .set_env("staging")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(input)
            .set_output_format(output);

        if enable_health_metrics {
            builder.enable_health_metrics();
        }

        if let Some(url) = dogstatsd_url {
            builder.set_dogstatsd_url(&url);
        };

        if enable_telemetry {
            builder.enable_telemetry(Some(TelemetryConfig {
                heartbeat: 100,
                ..Default::default()
            }));
        }

        builder.build().unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_health_metrics() {
        let stats_socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = stats_socket.set_read_timeout(Some(Duration::from_millis(500)));

        let fake_agent = MockServer::start();
        let _mock_traces = fake_agent.mock(|_, then| {
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{ "rate_by_service": { "service:test,env:staging": 1.0, "service:test,env:prod": 0.3 } }"#);
        });

        let exporter = build_test_exporter(
            fake_agent.url("/v0.4/traces"),
            Some(stats_socket.local_addr().unwrap().to_string()),
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V04,
            false,
            true,
        );

        let traces: Vec<Vec<SpanBytes>> = vec![
            vec![SpanBytes {
                name: BytesString::from_slice(b"test").unwrap(),
                ..Default::default()
            }],
            vec![SpanBytes {
                name: BytesString::from_slice(b"test2").unwrap(),
                ..Default::default()
            }],
        ];
        let data = msgpack_encoder::v04::to_vec(&traces);

        let _result = exporter
            .send(data.as_ref(), 2)
            .expect("failed to send trace");

        // Collect all metrics
        let mut received_metrics = Vec::new();
        for _ in 0..5 {
            received_metrics.push(read(&stats_socket));
        }

        // Check that all expected metrics are present
        let expected_metrics = vec![
            format!(
                "datadog.tracer.exporter.deserialize.traces:2|c|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            format!(
                "datadog.tracer.exporter.transport.traces.successful:2|c|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            format!(
                "datadog.tracer.exporter.transport.sent.bytes:{}|d|#libdatadog_version:{}",
                data.len(),
                env!("CARGO_PKG_VERSION")
            ),
            format!(
                "datadog.tracer.exporter.transport.traces.sent:2|d|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            format!(
                "datadog.tracer.exporter.transport.requests:1|d|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
        ];

        for expected in expected_metrics {
            assert!(
                received_metrics.contains(&expected),
                "Expected metric '{expected}' not found in received metrics: {received_metrics:?}"
            );
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_invalid_traces() {
        let stats_socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = stats_socket.set_read_timeout(Some(Duration::from_millis(500)));

        let fake_agent = MockServer::start();

        let exporter = build_test_exporter(
            fake_agent.url("/v0.4/traces"),
            Some(stats_socket.local_addr().unwrap().to_string()),
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V04,
            false,
            true,
        );

        let bad_payload = b"some_bad_payload".as_ref();
        let result = exporter.send(bad_payload, 1);

        assert!(result.is_err());

        assert_eq!(
            &format!(
                "datadog.tracer.exporter.deserialize.errors:1|c|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            &read(&stats_socket)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_health_metrics_error() {
        let stats_socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = stats_socket.set_read_timeout(Some(Duration::from_millis(500)));

        let fake_agent = MockServer::start();
        let _mock_traces = fake_agent.mock(|_, then| {
            then.status(400)
                .header("content-type", "application/json")
                .body("{}");
        });

        let exporter = build_test_exporter(
            fake_agent.url("/v0.4/traces"),
            Some(stats_socket.local_addr().unwrap().to_string()),
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V04,
            false,
            true,
        );

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec(&traces);
        let result = exporter.send(data.as_ref(), 1);

        assert!(result.is_err());

        // Collect all metrics
        let mut metrics = Vec::new();
        loop {
            let mut buf = [0; 1_000];
            match stats_socket.recv(&mut buf) {
                Ok(size) => {
                    let datagram = String::from_utf8_lossy(&buf[..size]);
                    metrics.push(datagram.to_string());
                }
                Err(_) => break, // Timeout, no more metrics
            }
        }

        // Expected metrics
        let expected_deser = format!(
            "datadog.tracer.exporter.deserialize.traces:1|c|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );
        let expected_error = format!(
            "datadog.tracer.exporter.transport.traces.failed:1|c|#libdatadog_version:{},type:400",
            env!("CARGO_PKG_VERSION")
        );
        let expected_dropped = format!(
            "datadog.tracer.exporter.transport.dropped.bytes:{}|d|#libdatadog_version:{}",
            data.len(),
            env!("CARGO_PKG_VERSION")
        );
        let expected_sent_bytes = format!(
            "datadog.tracer.exporter.transport.sent.bytes:{}|d|#libdatadog_version:{}",
            data.len(),
            env!("CARGO_PKG_VERSION")
        );
        let expected_sent_traces = format!(
            "datadog.tracer.exporter.transport.traces.sent:1|d|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );
        let expected_dropped_traces = format!(
            "datadog.tracer.exporter.transport.traces.dropped:1|d|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );
        let expected_requests = format!(
            "datadog.tracer.exporter.transport.requests:5|d|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );

        // Verify all expected metrics are present
        assert!(
            metrics.contains(&expected_deser),
            "Missing deser_traces metric. Got: {metrics:?}"
        );
        assert!(
            metrics.contains(&expected_error),
            "Missing send.traces.errors metric. Got: {metrics:?}"
        );
        assert!(
            metrics.contains(&expected_dropped),
            "Missing http.dropped.bytes metric. Got: {metrics:?}"
        );
        assert!(
            metrics.contains(&expected_dropped_traces),
            "Missing http.dropped.traces metric. Got: {metrics:?}"
        );
        assert!(
            metrics.contains(&expected_sent_bytes),
            "Missing http.sent.bytes metric. Got: {metrics:?}"
        );
        assert!(
            metrics.contains(&expected_sent_traces),
            "Missing http.sent.traces metric. Got: {metrics:?}"
        );
        assert!(
            metrics.contains(&expected_requests),
            "Missing http.requests metric. Got: {metrics:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_health_metrics_dropped_bytes_exclusions() {
        let stats_socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = stats_socket.set_read_timeout(Some(Duration::from_millis(500)));

        // Test 404 - should NOT emit http.dropped.bytes
        let fake_agent = MockServer::start();
        let _mock_traces = fake_agent.mock(|_, then| {
            then.status(404)
                .header("content-type", "application/json")
                .body("{}");
        });

        let exporter = build_test_exporter(
            fake_agent.url("/v0.4/traces"),
            Some(stats_socket.local_addr().unwrap().to_string()),
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V04,
            false,
            true,
        );

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec(&traces);
        let result = exporter.send(data.as_ref(), 1);

        assert!(result.is_err());

        // Collect all metrics
        let mut received_metrics = Vec::new();
        for _ in 0..5 {
            received_metrics.push(read(&stats_socket));
        }

        // Expected metrics for 404 error
        let expected_deser = format!(
            "datadog.tracer.exporter.deserialize.traces:1|c|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );
        let expected_error = format!(
            "datadog.tracer.exporter.transport.traces.failed:1|c|#libdatadog_version:{},type:404",
            env!("CARGO_PKG_VERSION")
        );
        let expected_sent_bytes = format!(
            "datadog.tracer.exporter.transport.sent.bytes:{}|d|#libdatadog_version:{}",
            data.len(),
            env!("CARGO_PKG_VERSION")
        );
        let expected_sent_traces = format!(
            "datadog.tracer.exporter.transport.traces.sent:1|d|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );
        let expected_requests = format!(
            "datadog.tracer.exporter.transport.requests:5|d|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );

        // Should emit these metrics
        assert!(
            received_metrics.contains(&expected_deser),
            "Missing deser_traces metric. Got: {received_metrics:?}"
        );
        assert!(
            received_metrics.contains(&expected_error),
            "Missing send.traces.errors metric. Got: {received_metrics:?}"
        );
        assert!(
            received_metrics.contains(&expected_sent_bytes),
            "Missing http.sent.bytes metric. Got: {received_metrics:?}"
        );
        assert!(
            received_metrics.contains(&expected_sent_traces),
            "Missing http.sent.traces metric. Got: {received_metrics:?}"
        );
        assert!(
            received_metrics.contains(&expected_requests),
            "Missing http.requests metric. Got: {received_metrics:?}"
        );

        // Should NOT emit http.dropped.bytes for 404
        let dropped_bytes_metric = format!(
            "datadog.tracer.exporter.transport.dropped.bytes:{}|d|#libdatadog_version:{}",
            data.len(),
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            !received_metrics.contains(&dropped_bytes_metric),
            "Should not emit http.dropped.bytes for 404. Got: {received_metrics:?}"
        );

        // Should NOT emit http.dropped.traces for 404
        let dropped_traces_metric = format!(
            "datadog.tracer.exporter.transport.traces.dropped:1|d|#libdatadog_version:{}",
            env!("CARGO_PKG_VERSION")
        );
        assert!(
            !received_metrics.contains(&dropped_traces_metric),
            "Should not emit http.dropped.traces for 404. Got: {received_metrics:?}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_health_metrics_disabled() {
        let stats_socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = stats_socket.set_read_timeout(Some(Duration::from_millis(500)));

        let fake_agent = MockServer::start();
        let _mock_traces = fake_agent.mock(|_, then| {
            then.status(200)
                .header("content-type", "application/json")
                .body(r#"{ "rate_by_service": { "service:test,env:staging": 1.0 } }"#);
        });

        let exporter = build_test_exporter(
            fake_agent.url("/v0.4/traces"),
            Some(stats_socket.local_addr().unwrap().to_string()),
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V04,
            false,
            false, // Health metrics disabled
        );

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec(&traces);

        let _result = exporter
            .send(data.as_ref(), 1)
            .expect("failed to send trace");

        // Try to read metrics - should timeout since none are sent
        let mut buf = [0; 1_000];
        match stats_socket.recv(&mut buf) {
            Ok(_) => {
                let datagram = String::from_utf8_lossy(buf.as_ref());
                let received = datagram.trim_matches(char::from(0)).to_string();
                panic!(
                    "Expected no metrics when health metrics disabled, but received: {received}"
                );
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // This is expected - no metrics should be sent when disabled
                // WouldBlock on Unix, TimedOut on Windows
            }
            Err(e) => panic!("Unexpected error reading from socket: {e}"),
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_response_parse_default() {
        let server = MockServer::start();
        let _agent = server.mock(|_, then| {
            then.status(200)
                .header("content-type", "application/json")
                .body(
                    r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#,
                );
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8");
        let exporter = builder.build().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec(&traces);
        let result = exporter.send(data.as_ref(), 1).unwrap();

        assert_eq!(
            result,
            AgentResponse::Changed {
                body: r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#
                .to_string()
            }
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_response_error() {
        let server = MockServer::start();
        let _agent = server.mock(|_, then| {
            then.status(500)
                .header("content-type", "application/json")
                .body(r#"{ "error": "Unavailable" }"#);
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8");
        let exporter = builder.build().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec(&traces);
        let code = match exporter.send(data.as_ref(), 1).unwrap_err() {
            TraceExporterError::Request(e) => Some(e.status()),
            _ => None,
        }
        .unwrap();

        assert_eq!(code, 500);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_empty_response_error() {
        let server = MockServer::start();
        let _agent = server.mock(|_, then| {
            then.status(200)
                .header("content-type", "application/json")
                .body("");
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8");
        let exporter = builder.build().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec(&traces);
        let err = exporter.send(data.as_ref(), 1);

        assert!(err.is_err());
        assert_eq!(
            match err.unwrap_err() {
                TraceExporterError::Agent(e) => Some(e),
                _ => None,
            },
            Some(AgentErrorKind::EmptyResponse)
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_exporter_metrics_v4() {
        let server = MockServer::start();
        let response_body = r#"{
                        "rate_by_service": {
                            "service:foo,env:staging": 1.0,
                            "service:,env:": 0.8
                        }
                    }"#;
        let traces_endpoint = server.mock(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let metrics_endpoint = server.mock(|when, then| {
            when.method(POST)
                .body_includes("\"metric\":\"trace_api.bytes\"")
                .path("/telemetry/proxy/api/v2/apmtelemetry");
            then.status(200)
                .header("content-type", "application/json")
                .body("");
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .enable_telemetry(Some(TelemetryConfig {
                heartbeat: 100,
                ..Default::default()
            }));
        let exporter = builder.build().unwrap();

        let traces = vec![0x90];
        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        traces_endpoint.assert_calls(1);
        while metrics_endpoint.calls() == 0 {
            exporter
                .runtime
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .block_on(async {
                    sleep(Duration::from_millis(100)).await;
                })
        }
        metrics_endpoint.assert_calls(1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_exporter_metrics_v5() {
        let server = MockServer::start();
        let response_body = r#"{
                        "rate_by_service": {
                            "service:foo,env:staging": 1.0,
                            "service:,env:": 0.8
                        }
                    }"#;
        let traces_endpoint = server.mock(|when, then| {
            when.method(POST).path("/v0.5/traces");
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let metrics_endpoint = server.mock(|when, then| {
            when.method(POST)
                .body_includes("\"metric\":\"trace_api.bytes\"")
                .path("/telemetry/proxy/api/v2/apmtelemetry");
            then.status(200)
                .header("content-type", "application/json")
                .body("");
        });

        let exporter = build_test_exporter(
            server.url("/"),
            None,
            TraceExporterInputFormat::V05,
            TraceExporterOutputFormat::V05,
            true,
            true,
        );

        let v5: (Vec<BytesString>, Vec<Vec<v05::Span>>) = (vec![], vec![]);
        let traces = rmp_serde::to_vec(&v5).unwrap();
        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        traces_endpoint.assert_calls(1);
        while metrics_endpoint.calls() == 0 {
            exporter
                .runtime
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .block_on(async {
                    sleep(Duration::from_millis(100)).await;
                })
        }
        metrics_endpoint.assert_calls(1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_exporter_metrics_v4_to_v5() {
        let server = MockServer::start();
        let response_body = r#"{
                        "rate_by_service": {
                            "service:foo,env:staging": 1.0,
                            "service:,env:": 0.8
                        }
                    }"#;
        let traces_endpoint = server.mock(|when, then| {
            when.method(POST).path("/v0.5/traces").is_true(|req| {
                let bytes = tinybytes::Bytes::copy_from_slice(req.body_ref());
                bytes.to_vec() == V5_EMPTY
            });
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let metrics_endpoint = server.mock(|when, then| {
            when.method(POST)
                .body_includes("\"metric\":\"trace_api.bytes\"")
                .path("/telemetry/proxy/api/v2/apmtelemetry");
            then.status(200)
                .header("content-type", "application/json")
                .body("");
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .enable_telemetry(Some(TelemetryConfig {
                heartbeat: 100,
                ..Default::default()
            }))
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V05);

        let exporter = builder.build().unwrap();

        let traces = vec![0x90];
        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        traces_endpoint.assert_calls(1);
        while metrics_endpoint.calls() == 0 {
            exporter
                .runtime
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .block_on(async {
                    sleep(Duration::from_millis(100)).await;
                })
        }
        metrics_endpoint.assert_calls(1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    /// Tests that if agent_response_payload_version is not enabled
    /// the exporter always returns the response body
    fn test_agent_response_payload_version_disabled() {
        let server = MockServer::start();
        let response_body = r#"{
                        "rate_by_service": {
                            "service:foo,env:staging": 1.0,
                            "service:,env:": 0.8
                        }
                    }"#;
        let traces_endpoint = server.mock(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-rates-payload-version", "abc")
                .body(response_body);
        });

        let mut builder = TraceExporterBuilder::default();
        builder.set_url(&server.url("/"));
        let exporter = builder.build().unwrap();
        let traces = vec![0x90];
        for _ in 0..2 {
            let result = exporter.send(traces.as_ref(), 1).unwrap();
            let AgentResponse::Changed { body } = result else {
                panic!("Expected Changed response");
            };
            assert_eq!(body, response_body);
        }
        traces_endpoint.assert_calls(2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    /// Tests that if agent_response_payload_version is enabled
    /// the exporter returns the response body only once
    /// and then returns Unchanged response until the payload version header changes
    fn test_agent_response_payload_version() {
        let server = MockServer::start();
        let response_body = r#"{
                        "rate_by_service": {
                            "service:foo,env:staging": 1.0,
                            "service:,env:": 0.8
                        }
                    }"#;
        let mut traces_endpoint = server.mock(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-rates-payload-version", "abc")
                .body(response_body);
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .enable_agent_rates_payload_version();
        let exporter = builder.build().unwrap();
        let traces = vec![0x90];
        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Unchanged = result else {
            panic!("Expected Unchanged response");
        };
        traces_endpoint.assert_calls(2);
        traces_endpoint.delete();

        let traces_endpoint = server.mock(|when, then| {
            when.method(POST).path("/v0.4/traces");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-rates-payload-version", "def")
                .body(response_body);
        });
        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Unchanged = result else {
            panic!("Expected Unchanged response");
        };
        traces_endpoint.assert_calls(2);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_malfunction_info_4xx() {
        test_agent_malfunction_info(404, r#"{"error":"Not Found"}"#, Duration::from_secs(0));
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_malfunction_info_5xx() {
        test_agent_malfunction_info(
            500,
            r#"{"error":"Internal Server Error"}"#,
            Duration::from_secs(0),
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_malfunction_info_timeout() {
        test_agent_malfunction_info(
            408,
            r#"{"error":"Internal Server Error"}"#,
            Duration::from_secs(600),
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agent_malfunction_info_wrong_answer() {
        test_agent_malfunction_info(200, "WRONG_ANSWER", Duration::from_secs(0));
    }

    fn test_agent_malfunction_info(status: u16, response: &str, delay: Duration) {
        let server = MockServer::start();

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.4/traces");
            then.status(200).body(
                r#"{
                    "rate_by_service": {
                        "service:test,env:staging": 1.0,
                    }
                }"#,
            );
        });

        let mock_info = server.mock(|when, then| {
            when.method(GET).path("/info");
            then.delay(delay).status(status).body(response);
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("test")
            .set_env("staging")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .enable_stats(Duration::from_secs(10));
        let exporter = builder.build().unwrap();

        let trace_chunk = vec![SpanBytes {
            duration: 10,
            ..Default::default()
        }];

        let data = msgpack_encoder::v04::to_vec(&[trace_chunk]);

        // Wait for the info fetcher to get the config
        while mock_info.calls() == 0 {
            exporter
                .runtime
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .block_on(async {
                    sleep(Duration::from_millis(100)).await;
                })
        }

        let _ = exporter.send(data.as_ref(), 1).unwrap();

        exporter.shutdown(None).unwrap();

        mock_traces.assert();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_connection_timeout() {
        let exporter = TraceExporterBuilder::default().build().unwrap();

        assert_eq!(exporter.endpoint.timeout_ms, Endpoint::default().timeout_ms);

        let timeout = Some(42);
        let mut builder = TraceExporterBuilder::default();
        builder.set_connection_timeout(timeout);

        let exporter = builder.build().unwrap();

        assert_eq!(exporter.endpoint.timeout_ms, 42);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn stop_and_start_runtime() {
        let builder = TraceExporterBuilder::default();
        let exporter = builder.build().unwrap();
        exporter.stop_worker();
        exporter.run_worker().unwrap();
    }
}

#[cfg(test)]
mod single_threaded_tests {
    use super::*;
    use crate::agent_info;
    use datadog_trace_utils::msgpack_encoder;
    use datadog_trace_utils::span::SpanBytes;
    use httpmock::prelude::*;
    use std::time::Duration;
    use tokio::time::sleep;

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_shutdown() {
        // Clear the agent info cache to ensure test isolation
        agent_info::clear_cache_for_test();

        let server = MockServer::start();

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.4/traces");
            then.status(200).body("");
        });

        let mock_stats = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.6/stats");
            then.status(200).body("");
        });

        let _mock_info = server.mock(|when, then| {
            when.method(GET).path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(r#"{"version":"1","client_drop_p0s":true,"endpoints":["/v0.4/traces","/v0.6/stats"]}"#);
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("test")
            .set_env("staging")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .enable_stats(Duration::from_secs(10));
        let exporter = builder.build().unwrap();

        let trace_chunk = vec![SpanBytes {
            duration: 10,
            ..Default::default()
        }];

        let data = msgpack_encoder::v04::to_vec(&[trace_chunk]);

        // Wait for the info fetcher to get the config
        while agent_info::get_agent_info().is_none() {
            exporter
                .runtime
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .block_on(async {
                    sleep(Duration::from_millis(100)).await;
                })
        }

        let result = exporter.send(data.as_ref(), 1);
        // Error received because server is returning an empty body.
        assert!(result.is_err());

        // Wait for the stats worker to be active before shutting down to avoid potential flaky
        // tests on CI where we shutdown before the stats worker had time to start
        let start_time = std::time::Instant::now();
        while !exporter.is_stats_worker_active() {
            if start_time.elapsed() > Duration::from_secs(10) {
                panic!("Timeout waiting for stats worker to become active");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        exporter.shutdown(None).unwrap();

        // Wait for the mock server to process the stats
        for _ in 0..1000 {
            if mock_traces.calls() > 0 && mock_stats.calls() > 0 {
                break;
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        mock_traces.assert();
        mock_stats.assert();
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_shutdown_with_timeout() {
        // Clear the agent info cache to ensure test isolation
        agent_info::clear_cache_for_test();

        let server = MockServer::start();

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.4/traces");
            then.status(200).body(
                r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#,
            );
        });

        let _mock_stats = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.6/stats");
            then.delay(Duration::from_secs(10)).status(200).body("");
        });

        let _mock_info = server.mock(|when, then| {
            when.method(GET).path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(r#"{"version":"1","client_drop_p0s":true,"endpoints":["/v0.4/traces","/v0.6/stats"]}"#);
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&server.url("/"))
            .set_service("test")
            .set_env("staging")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .enable_stats(Duration::from_secs(10));
        let exporter = builder.build().unwrap();

        let trace_chunk = vec![SpanBytes {
            service: "test".into(),
            name: "test".into(),
            resource: "test".into(),
            r#type: "test".into(),
            duration: 10,
            ..Default::default()
        }];

        let data = msgpack_encoder::v04::to_vec(&[trace_chunk]);

        // Wait for agent_info to be present so that sending a trace will trigger the stats worker
        // to start
        while agent_info::get_agent_info().is_none() {
            exporter
                .runtime
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .block_on(async {
                    sleep(Duration::from_millis(100)).await;
                })
        }

        exporter.send(data.as_ref(), 1).unwrap();

        // Wait for the stats worker to be active before shutting down to avoid potential flaky
        // tests on CI where we shutdown before the stats worker had time to start
        let start_time = std::time::Instant::now();
        while !exporter.is_stats_worker_active() {
            if start_time.elapsed() > Duration::from_secs(10) {
                panic!("Timeout waiting for stats worker to become active");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        exporter
            .shutdown(Some(Duration::from_millis(5)))
            .unwrap_err(); // The shutdown should timeout

        mock_traces.assert();
    }
}
