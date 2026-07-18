// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
pub mod agent_response;
pub mod builder;
pub mod error;
mod log_writer;
pub mod metrics;
pub mod stats;
mod trace_serializer;

// Re-export the builder
pub use builder::TraceExporterBuilder;
use libdd_trace_utils::trace_filter::TraceFilterer;

use self::agent_response::AgentResponse;
use self::log_writer::write_log_traces;
use self::metrics::MetricsEmitter;
use self::stats::StatsComputationStatus;
use self::trace_serializer::TraceSerializer;
use crate::agent_info::ResponseObserver;
use crate::agentless::{send_agentless_traces_http, AgentlessTraceConfig};
use crate::otlp::{map_traces_to_otlp, send_otlp_traces_http, OtlpResourceInfo, OtlpTraceConfig};
#[cfg(feature = "telemetry")]
use crate::telemetry::{SendPayloadTelemetry, TelemetryClient};
use crate::trace_exporter::agent_response::{
    AgentResponsePayloadVersion, DATADOG_RATES_PAYLOAD_VERSION,
};
use crate::trace_exporter::error::{
    InternalErrorKind, RequestError, ShutdownError, TraceExporterError,
};
use crate::trace_exporter::stats::StatsComputationConfig;
use crate::{
    agent_info::{self, schema::AgentInfo},
    health_metrics,
    health_metrics::{HealthMetric, SendResult, TransportErrorType},
};
use arc_swap::{ArcSwap, ArcSwapOption};
use bytes::Bytes;
use futures::stream::{FuturesUnordered, StreamExt};
use http::header::HeaderMap;
use http::uri::PathAndQuery;
use http::Uri;
use libdd_capabilities::{HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability};
use libdd_common::tag::Tag;
use libdd_common::Endpoint;
use libdd_dogstatsd_client::Client;
#[cfg(not(target_arch = "wasm32"))]
use libdd_shared_runtime::BlockingRuntime;
use libdd_shared_runtime::{SharedRuntime, WorkerHandle};
use libdd_trace_utils::msgpack_decoder;
use libdd_trace_utils::send_with_retry::{
    send_with_retry, RetryStrategy, SendWithRetryError, SendWithRetryResult,
};
use libdd_trace_utils::span::{v04::Span, TraceData};
use libdd_trace_utils::trace_utils::TracerHeaderTags;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Once};
use std::time::Duration;
use std::{borrow::Borrow, str::FromStr};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

const INFO_ENDPOINT: &str = "/info";
const V04_TRACES_ENDPOINT: &str = "/v0.4/traces";
const V05_TRACES_ENDPOINT: &str = "/v0.5/traces";
const V1_TRACES_ENDPOINT: &str = "/v1.0/traces";

/// Build the HTTP headers required by the agentless intake.
///
/// Includes the API key, content-type, trace count, `Datadog-Meta-*` tracer headers,
/// and entity headers (container-id / entity-id / external-env) when available.
fn build_agentless_headers(
    metadata: &TracerMetadata,
    api_key: &str,
    trace_count: usize,
) -> Result<HeaderMap, TraceExporterError> {
    let mut headers: HeaderMap = {
        let tags: TracerHeaderTags = metadata.into();
        tags.into()
    };

    let api_key_val = http::HeaderValue::from_str(api_key).map_err(|_| {
        TraceExporterError::Internal(error::InternalErrorKind::InvalidWorkerState(
            "Invalid Datadog API key value for dd-api-key header".to_string(),
        ))
    })?;
    headers.insert(http::HeaderName::from_static("dd-api-key"), api_key_val);

    headers.insert(
        http::header::CONTENT_TYPE,
        libdd_common::header::APPLICATION_JSON,
    );

    headers.insert(
        http::HeaderName::from_static("x-datadog-trace-count"),
        http::HeaderValue::from(trace_count),
    );

    for (name, value) in libdd_common::entity_id::get_entity_headers() {
        if let (Ok(name), Ok(value)) = (
            http::HeaderName::from_bytes(name.as_bytes()),
            http::HeaderValue::from_str(value),
        ) {
            headers.insert(name, value);
        }
    }

    Ok(headers)
}

/// Values for optional telemetry HTTP session headers (`dd-session-id`, root/parent).
#[derive(Debug, Default, Clone)]
pub struct TelemetryInstrumentationSessions {
    pub session_id: Option<String>,
    pub root_session_id: Option<String>,
    pub parent_session_id: Option<String>,
}

/// TraceExporterInputFormat represents the format of the input traces.
/// The input format can be either Proxy or V0.4, where V0.4 is the default.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[repr(C)]
pub enum TraceExporterInputFormat {
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
    V1,
}

impl TraceExporterOutputFormat {
    /// Add the agent trace endpoint path to the URL.
    fn add_path(&self, url: &Uri) -> Uri {
        add_path(
            url,
            match self {
                TraceExporterOutputFormat::V04 => V04_TRACES_ENDPOINT,
                TraceExporterOutputFormat::V05 => V05_TRACES_ENDPOINT,
                TraceExporterOutputFormat::V1 => V1_TRACES_ENDPOINT,
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

pub use libdd_trace_utils::tracer_metadata::TracerMetadata;

/// Handles for the background workers owned by a [`TraceExporter`].
#[derive(Debug)]
pub(crate) struct TraceExporterWorkers {
    /// `None` when no background `/info` fetcher is started (agentless trace
    /// export mode, log-export mode).
    info_fetcher: Option<WorkerHandle>,
    #[cfg(feature = "telemetry")]
    telemetry: Option<WorkerHandle>,
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

impl From<TraceExporterInputFormat> for DeserInputFormat {
    fn from(f: TraceExporterInputFormat) -> Self {
        match f {
            TraceExporterInputFormat::V04 => DeserInputFormat::V04,
            TraceExporterInputFormat::V05 => DeserInputFormat::V05,
        }
    }
}

/// `C` is the capabilities bundle (HTTP, sleep). Leaf crates pin it to a concrete type
/// (`NativeCapabilities` or `WasmCapabilities`).
///
/// `R` is the [`SharedRuntime`] used to host background workers. See
/// [`libdd_shared_runtime::SharedRuntime`] for guidance on choosing an implementation.
#[derive(Debug)]
pub struct TraceExporter<
    C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    R: SharedRuntime,
> {
    endpoint: Endpoint,
    metadata: TracerMetadata,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    /// Set to true while the agent advertises `/v1.0/traces` in `/info`; false otherwise.
    /// Only consulted when `output_format` is V1.
    v1_active: AtomicBool,
    /// Used to emit a one-shot warning when V1 is requested by the SDK but the agent never
    /// advertises `/v1.0/traces`. Without it we'd either spam the warning on every `/info`
    /// poll or stay silent and leave SDK authors without a signal.
    v1_unavailable_logged: Once,
    serializer: TraceSerializer,
    shared_runtime: Arc<R>,
    /// None if dogstatsd is disabled
    dogstatsd: Option<Arc<Client>>,
    common_stats_tags: Vec<Tag>,
    client_computed_top_level: bool,
    client_side_stats: StatsComputationConfig,
    previous_info_state: ArcSwapOption<String>,
    info_response_observer: ResponseObserver,
    #[cfg(feature = "telemetry")]
    telemetry: Option<TelemetryClient<C>>,
    health_metrics_enabled: bool,
    capabilities: C,
    workers: TraceExporterWorkers,
    agent_payload_response_version: Option<AgentResponsePayloadVersion>,
    /// When set, traces are exported via OTLP HTTP/JSON instead of the Datadog agent.
    otlp_config: Option<OtlpTraceConfig>,
    /// When set, APM trace spans are exported directly to the Datadog HTTP intake (agentless)
    /// instead of via the Datadog Agent
    agentless_config: Option<AgentlessTraceConfig>,
    trace_filterer: ArcSwap<TraceFilterer>,
    /// When true, span stats are computed and exported as OTLP metrics. The concentrator is
    /// started at build time, so agent-driven stats (de)activation in `check_agent_info` is
    /// skipped.
    otlp_stats_enabled: bool,
    /// When `Some(max_line_size)`, traces are written as newline-delimited JSON
    /// through the [`LogWriterCapability`] (the Datadog Forwarder "log exporter"
    /// path) instead of being sent to an agent. Used in serverless environments
    /// where no agent is reachable.
    log_output: Option<usize>,
}

impl<
        C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
        R: SharedRuntime,
    > TraceExporter<C, R>
{
    #[allow(missing_docs)]
    pub fn builder() -> TraceExporterBuilder<R> {
        TraceExporterBuilder::new()
    }

    /// Stop the background workers owned by this exporter.
    ///
    /// Sync facade over [`Self::shutdown_async`]; panics inside an existing tokio context.
    /// Workers from other components sharing the same `R` runtime are unaffected.
    ///
    /// # Errors
    /// Returns [`TraceExporterError::Shutdown(ShutdownError::TimedOut)`] if a timeout was
    /// given and elapsed before all workers finished.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn shutdown(self, timeout: Option<Duration>) -> Result<(), TraceExporterError>
    where
        R: BlockingRuntime,
    {
        let runtime = self.shared_runtime.clone();
        runtime.block_on(self.shutdown_async(timeout))?
    }

    /// Async version of [`Self::shutdown`].
    ///
    /// # Errors
    /// Returns [`TraceExporterError::Shutdown(ShutdownError::TimedOut)`] if a timeout was
    /// given and elapsed before all workers finished.
    pub async fn shutdown_async(self, timeout: Option<Duration>) -> Result<(), TraceExporterError> {
        let Some(timeout) = timeout else {
            self.shutdown_workers().await;
            return Ok(());
        };
        // Runtime-agnostic timeout: race the shutdown work against a capability-driven
        // sleep, same pattern as `worker::send_request` / `agent_info::fetcher`.
        // `tokio::time::timeout` would tie us to a tokio reactor we don't have on wasm.
        let sleeper = <C as SleepCapability>::new();
        tokio::select! {
            biased;
            _ = self.shutdown_workers() => Ok(()),
            _ = sleeper.sleep(timeout) => Err(TraceExporterError::Shutdown(
                ShutdownError::TimedOut(timeout),
            )),
        }
    }

    async fn shutdown_workers(self) {
        let mut handles: Vec<WorkerHandle> = Vec::new();

        if let StatsComputationStatus::Enabled { worker_handle, .. } =
            &**self.client_side_stats.status.load()
        {
            handles.push(worker_handle.clone());
        }

        if let Some(info_fetcher) = self.workers.info_fetcher {
            handles.push(info_fetcher);
        }

        #[cfg(feature = "telemetry")]
        if let Some(telemetry) = self.workers.telemetry {
            handles.push(telemetry);
        }

        let mut futures: FuturesUnordered<_> = handles.into_iter().map(|h| h.stop()).collect();

        while let Some(result) = futures.next().await {
            if let Err(e) = result {
                error!("Worker failed to shutdown: {:?}", e);
            }
        }
    }

    /// Send msgpack serialized traces to the agent.
    ///
    /// Sync facade over [`Self::send_async`]; panics inside an existing tokio context.
    /// `data` must be encoded per the `input_format` given to the builder. Returns the
    /// agent response on success.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send(&self, data: &[u8]) -> Result<AgentResponse, TraceExporterError>
    where
        R: BlockingRuntime,
    {
        self.shared_runtime.block_on(self.send_async(data))?
    }

    /// Send msgpack serialized traces to the agent.
    ///
    /// `data` must be encoded per the `input_format` given to the builder.
    /// [`Self::send`] is the sync facade over this method.
    pub async fn send_async(&self, data: &[u8]) -> Result<AgentResponse, TraceExporterError> {
        // In log-export mode there is no agent to negotiate with; skip the poll.
        if self.log_output.is_none() {
            self.check_agent_info().await;
        }

        let format: DeserInputFormat = self.input_format.into();

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
        debug!(
            trace_count = traces.len(),
            "Trace deserialization completed successfully"
        );
        self.emit_metric(
            HealthMetric::Count(health_metrics::DESERIALIZE_TRACES, traces.len() as i64),
            None,
        );

        let res = self.send_trace_chunks_inner(traces).await?;
        if matches!(&res, AgentResponse::Changed { body } if body.is_empty()) {
            return Err(TraceExporterError::Agent(
                error::AgentErrorKind::EmptyResponse,
            ));
        }
        Ok(res)
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

    /// Reconcile in-process stats state with the latest agent info.
    /// Async so the `Enabled` arm can await a stats-worker shutdown without `block_on`.
    async fn check_agent_info(&self) {
        let Some(agent_info) = agent_info::get_agent_info() else {
            return;
        };
        if !self.has_agent_info_state_changed(&agent_info) {
            return;
        }

        if matches!(self.output_format, TraceExporterOutputFormat::V1) {
            self.refresh_v1_active(&agent_info);
        }

        // OTLP trace metrics run the concentrator independently; skip stats enable/disable.
        if self.otlp_stats_enabled {
            return;
        }

        self.trace_filterer.store(Arc::new(TraceFilterer::new(
            &agent_info.info.filter_tags.require,
            &agent_info.info.filter_tags.reject,
            &agent_info.info.filter_tags_regex.require,
            &agent_info.info.filter_tags_regex.reject,
            &agent_info.info.ignore_resources,
        )));

        // load_full() avoids holding an ArcSwap Guard (!Send) across .await.
        let status = self.client_side_stats.status.load_full();
        match &*status {
            StatsComputationStatus::Disabled => {}
            StatsComputationStatus::DisabledByAgent { .. } => {
                let ctx = stats::StatsContext {
                    metadata: &self.metadata,
                    endpoint_url: &self.endpoint.url,
                    shared_runtime: &*self.shared_runtime,
                    stats_cardinality_limit: self.client_side_stats.stats_cardinality_limit,
                    dogstatsd: if self.health_metrics_enabled {
                        self.dogstatsd.clone()
                    } else {
                        None
                    },
                    #[cfg(feature = "telemetry")]
                    telemetry: self.telemetry.as_ref().map(|t| t.clone_handle()),
                    #[cfg(not(feature = "telemetry"))]
                    _phantom: std::marker::PhantomData,
                };
                stats::handle_stats_disabled_by_agent(
                    &ctx,
                    &agent_info,
                    self.capabilities.clone(),
                    &self.client_side_stats,
                );
            }
            StatsComputationStatus::Enabled {
                stats_concentrator, ..
            } => {
                stats::handle_stats_enabled(
                    &agent_info,
                    stats_concentrator,
                    &self.client_side_stats,
                )
                .await;
            }
        }
        self.previous_info_state
            .store(Some(agent_info.state_hash.clone().into()))
    }

    /// Reconcile `v1_active` with the agent's currently-advertised endpoints. Called only when
    /// V1 is configured and the agent info state has changed, so transitions are logged at most
    /// once per change. Note: `v1_active` can also transition `true → false` outside this path,
    /// via the fail-closed hook in `send_trace_chunks_inner` when the agent returns 404 on
    /// `/v1.0/traces` (the agent does not bump its state hash on 404).
    fn refresh_v1_active(&self, agent_info: &Arc<AgentInfo>) {
        let supports_v1 = agent_info
            .info
            .endpoints
            .as_ref()
            .is_some_and(|e| e.iter().any(|p| p == V1_TRACES_ENDPOINT));
        let previous = self.v1_active.swap(supports_v1, Ordering::Relaxed);
        match (previous, supports_v1) {
            (false, true) => debug!("V1 trace protocol enabled (agent advertises /v1.0/traces)"),
            (true, false) => {
                warn!("V1 trace protocol no longer advertised by agent; falling back to v0.4")
            }
            (false, false) => {
                self.v1_unavailable_logged.call_once(|| {
                    warn!(
                        "V1 trace protocol requested by SDK but agent does not advertise {V1_TRACES_ENDPOINT}; continuing on v0.4"
                    );
                });
            }
            (true, true) => {}
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
    /// The `send` function will check agent_info when running, which will only be available if the
    /// fetcher had time to reach to the agent.
    /// Since agent_info can enable CSS computation, waiting for this during testing can make
    /// snapshots non-deterministic.
    #[cfg(feature = "test-utils")]
    pub async fn wait_agent_info_ready(&self, timeout: Duration) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        loop {
            if std::time::Instant::now().duration_since(start) > timeout {
                anyhow::bail!("Timeout waiting for agent info to be ready",);
            }
            if agent_info::get_agent_info().is_some() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Emit a health metric to dogstatsd
    fn emit_metric(&self, metric: HealthMetric, custom_tags: Option<Vec<&Tag>>) {
        if self.health_metrics_enabled {
            let emitter = MetricsEmitter::new(self.dogstatsd.as_deref(), &self.common_stats_tags);
            emitter.emit(metric, custom_tags);
        }
    }

    /// Emit all health metrics from a SendResult
    fn emit_send_result(&self, result: &SendResult) {
        if self.health_metrics_enabled {
            let emitter = MetricsEmitter::new(self.dogstatsd.as_deref(), &self.common_stats_tags);
            emitter.emit_from_send_result(result);
        }
    }

    /// Send a list of trace chunks to the agent (or OTLP endpoint when configured).
    ///
    /// Sync facade over [`Self::send_trace_chunks_async`]; panics inside an existing
    /// tokio context.
    ///
    /// # Arguments
    /// * trace_chunks: A list of trace chunks. Each trace chunk is a list of spans.
    /// * cancellation_token: When provided, cancelling the token aborts the send while it is in
    ///   progress. The send only observes a token that is cancelled while the request is in-flight;
    ///   a token cancelled before this call returns immediately, and a token cancelled after the
    ///   send has already finished has no effect. Cancelling an in-flight send may cause the trace
    ///   chunks being sent to be lost.
    ///
    /// # Returns
    /// * Ok(AgentResponse): The response from the agent (or Unchanged for OTLP)
    /// * Err(TraceExporterError): An error detailing what went wrong in the process
    #[cfg(not(target_arch = "wasm32"))]
    pub fn send_trace_chunks<T: TraceData>(
        &self,
        trace_chunks: Vec<Vec<Span<T>>>,
        cancellation_token: Option<&CancellationToken>,
    ) -> Result<AgentResponse, TraceExporterError>
    where
        R: BlockingRuntime,
    {
        self.shared_runtime.block_on(async {
            match cancellation_token {
                Some(token) => {
                    tokio::select! {
                        res = self.send_trace_chunks_async(trace_chunks) => res,
                        _ = token.cancelled() => Err(TraceExporterError::Io(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "send cancelled via cancellation token",
                        ))),
                    }
                }
                None => self.send_trace_chunks_async(trace_chunks).await,
            }
        })?
    }

    /// Send a list of trace chunks to the agent, asynchronously (or OTLP when configured).
    ///
    /// # Arguments
    /// * trace_chunks: A list of trace chunks. Each trace chunk is a list of spans.
    ///
    /// # Returns
    /// * Ok(AgentResponse): The response from the agent (or Unchanged for OTLP)
    /// * Err(TraceExporterError): An error detailing what went wrong in the process
    pub async fn send_trace_chunks_async<T: TraceData>(
        &self,
        trace_chunks: Vec<Vec<Span<T>>>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // In log-export mode there is no agent to negotiate with; skip the poll.
        if self.log_output.is_none() {
            self.check_agent_info().await;
        }
        self.send_trace_chunks_inner(trace_chunks).await
    }

    /// Sends trace chunks to the Datadog agentless intake (`/v1/input`) as JSON.
    async fn send_agentless_traces_inner<T: TraceData>(
        &self,
        traces: Vec<Vec<Span<T>>>,
        config: &AgentlessTraceConfig,
    ) -> Result<AgentResponse, TraceExporterError> {
        let trace_count = traces.len();
        let json_body = libdd_trace_utils::agentless_encoder::encode_payload(
            &traces,
            &self.metadata,
        )
        .map_err(|e| {
            error!("Agentless JSON serialization error: {e}");
            TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(e.to_string()))
        })?;

        let headers = build_agentless_headers(&self.metadata, &config.api_key, trace_count)?;

        send_agentless_traces_http(&self.capabilities, config, headers, json_body).await?;
        Ok(AgentResponse::Unchanged)
    }

    /// Sends trace chunks via OTLP HTTP (JSON or protobuf) when OTLP config is enabled.
    async fn send_otlp_traces_inner<T: TraceData>(
        &self,
        traces: Vec<Vec<Span<T>>>,
        config: &OtlpTraceConfig,
    ) -> Result<AgentResponse, TraceExporterError> {
        let resource_info = {
            let mut r = OtlpResourceInfo::default();
            r.service = self.metadata.service.clone();
            r.env = self.metadata.env.clone();
            r.app_version = self.metadata.app_version.clone();
            r.language = self.metadata.language.clone();
            r.tracer_version = self.metadata.tracer_version.clone();
            r.runtime_id = self.metadata.runtime_id.clone();
            r.client_computed_stats = self.otlp_stats_enabled;
            r.instrumentation_scope_name = config.instrumentation_scope_name.clone();
            r.instrumentation_scope_version = config.instrumentation_scope_version.clone();
            r
        };
        // Single prost OTLP IR; the configured protocol encodes the same request to its wire
        // format (JSON or protobuf). OTel-semantics gating (omit DD-specific attrs) happens in
        // the mapper.
        let request =
            map_traces_to_otlp(traces, &resource_info, config.otel_trace_semantics_enabled);
        let body = config.protocol.encode(&request).map_err(|e| {
            error!("OTLP serialization error: {e}");
            TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(format!(
                "failed to encode OTLP request: {e}"
            )))
        })?;
        // Also set the header: resource attributes survive Collector hops, headers don't.
        let effective_config;
        let config_to_use = if self.otlp_stats_enabled {
            effective_config = {
                let mut c = config.clone();
                c.headers.insert(
                    http::HeaderName::from_static("datadog-client-computed-stats"),
                    http::HeaderValue::from_static("yes"),
                );
                c
            };
            &effective_config
        } else {
            config
        };
        send_otlp_traces_http(
            &self.capabilities,
            config_to_use,
            self.endpoint.test_token.as_deref(),
            body,
        )
        .await?;
        Ok(AgentResponse::Unchanged)
    }

    /// Send traces payload to agent with retry and telemetry reporting
    async fn send_traces_with_telemetry(
        &self,
        endpoint: &Endpoint,
        mp_payload: Vec<u8>,
        headers: HeaderMap,
        chunks: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        let strategy = RetryStrategy::default();
        let payload_len = mp_payload.len();

        // Send traces to the agent
        let result = send_with_retry(
            &self.capabilities,
            endpoint,
            mp_payload,
            &headers,
            &strategy,
        )
        .await;

        #[cfg(feature = "telemetry")]
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

    /// Synchronous log-export path: encode every span to newline-delimited
    /// Forwarder JSON and write it through the log-output capability (stdout on
    /// native; host/JS on wasm). No agent, stats, OTLP, or telemetry is involved.
    ///
    /// Unlike the OTLP path, spans are emitted as-is and unsampled (p0) chunks are
    /// NOT dropped here: the reference log exporters (JS/Go/Py/Java) write every
    /// span they are handed and defer sampling to the trace intake behind the
    /// Datadog Forwarder.
    ///
    /// Returns [`AgentResponse::Unchanged`] as there is no agent response to relay.
    fn send_trace_chunks_to_log<T: TraceData>(
        &self,
        traces: &[Vec<Span<T>>],
        max_line_size: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        let stats = write_log_traces(&self.capabilities, traces, max_line_size)
            .map_err(TraceExporterError::Io)?;
        debug!(
            spans_written = stats.spans_written,
            spans_dropped = stats.spans_dropped,
            "Wrote traces to log exporter"
        );
        Ok(AgentResponse::Unchanged)
    }

    async fn send_trace_chunks_inner<T: TraceData>(
        &self,
        mut traces: Vec<Vec<Span<T>>>,
    ) -> Result<AgentResponse, TraceExporterError> {
        // TODO(APMSP-3608): log-output silently takes precedence over OTLP/agent here.
        // The builder should reject conflicting destinations at build time instead.
        if let Some(max_line_size) = self.log_output {
            return self.send_trace_chunks_to_log(&traces, max_line_size);
        }

        let mut header_tags: TracerHeaderTags = self.metadata.borrow().into();

        if let Some(ref config) = self.agentless_config {
            // For agentless we want to tag top level spans, but not perform
            // stats aggregation or span drops
            if !self.client_computed_top_level {
                for chunk in traces.iter_mut() {
                    libdd_trace_utils::span::trace_utils::compute_top_level_span(chunk);
                }
            }

            return self.send_agentless_traces_inner(traces, config).await;
        }

        // Process stats computation and drop non-sampled (p0) chunks.
        // This must run before the OTLP path so that unsampled spans are not exported.
        stats::process_traces_for_stats(
            &mut traces,
            &mut header_tags,
            &self.client_side_stats.status,
            self.client_computed_top_level,
            &self.trace_filterer.load(),
            #[cfg(feature = "telemetry")]
            self.telemetry.as_ref(),
        );

        for chunk in &mut traces {
            for span in chunk.iter_mut() {
                span.dedup();
            }
        }

        // OTLP path: send sampled traces via OTLP when an OTLP endpoint is configured.
        // Unlike the agent path, there is no downstream agent to drop unsampled traces,
        // so drop_chunks is always called here regardless of whether stats are enabled.
        if let Some(ref config) = self.otlp_config {
            libdd_trace_utils::span::trace_utils::drop_chunks(&mut traces);
            if traces.is_empty() {
                return Ok(AgentResponse::Unchanged);
            }
            return self.send_otlp_traces_inner(traces, config).await;
        }

        // Snapshot the effective format once so the serializer and the URL agree even if
        // `v1_active` flips mid-send (the background `/info` fetcher can race us otherwise).
        let effective_format = self.effective_output_format();

        let prepared = match self.serializer.prepare_traces_payload(
            traces,
            header_tags,
            &self.metadata,
            self.agent_payload_response_version.as_ref(),
            effective_format,
        ) {
            Ok(p) => p,
            Err(e) => {
                error!("Error serializing traces: {e}");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::SERIALIZE_TRACES_ERRORS, 1),
                    None,
                );
                return Err(e);
            }
        };

        let endpoint = Endpoint {
            url: effective_format.add_path(&self.endpoint.url),
            ..self.endpoint.clone()
        };

        let result = self
            .send_traces_with_telemetry(
                &endpoint,
                prepared.data,
                prepared.headers,
                prepared.chunk_count,
            )
            .await;

        // State-hash trap mitigation: the agent does not return a `Datadog-Agent-State`
        // header on 404, so without this hook we'd stay pinned to V1 until the next `/info`
        // poll (up to the fetcher's refresh interval). On a 404 to `/v1.0/traces`, fail
        // closed immediately and force an `/info` refresh so the next send uses V0.4 and
        // V1 support is re-detected as soon as the agent advertises it again.
        if effective_format == TraceExporterOutputFormat::V1 {
            if let Err(TraceExporterError::Request(ref e)) = result {
                if e.status() == http::StatusCode::NOT_FOUND
                    && self.v1_active.swap(false, Ordering::Relaxed)
                {
                    warn!(
                            "V1 trace send returned 404; agent no longer advertises {V1_TRACES_ENDPOINT} — falling back to V0.4"
                        );
                    self.info_response_observer.manual_trigger();
                }
            }
        }

        result
    }

    /// Handle the result of sending traces to the agent
    async fn handle_send_result(
        &self,
        result: SendWithRetryResult,
        chunks: usize,
        payload_len: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        match result {
            Ok((response, attempts)) => {
                self.handle_agent_response(chunks, response, payload_len, attempts)
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

        match err {
            SendWithRetryError::Http(response, attempts) => {
                self.handle_http_send_error(response, payload_len, chunks, attempts)
                    .await
            }
            SendWithRetryError::Timeout(attempts) => {
                let send_result =
                    SendResult::failure(TransportErrorType::Timeout, payload_len, chunks, attempts);
                self.emit_send_result(&send_result);
                Err(TraceExporterError::from(io::Error::from(
                    io::ErrorKind::TimedOut,
                )))
            }
            SendWithRetryError::Network(err, attempts) => {
                let send_result =
                    SendResult::failure(TransportErrorType::Network, payload_len, chunks, attempts);
                self.emit_send_result(&send_result);
                Err(TraceExporterError::from(err))
            }
            SendWithRetryError::ResponseBody(attempts) => {
                let send_result = SendResult::failure(
                    TransportErrorType::ResponseBody,
                    payload_len,
                    chunks,
                    attempts,
                );
                self.emit_send_result(&send_result);
                Err(TraceExporterError::from(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "failed to read response body",
                )))
            }
            SendWithRetryError::Build(attempts) => {
                let send_result =
                    SendResult::failure(TransportErrorType::Build, payload_len, chunks, attempts);
                self.emit_send_result(&send_result);
                Err(TraceExporterError::from(io::Error::from(
                    io::ErrorKind::Other,
                )))
            }
        }
    }

    /// Handle HTTP error responses from send with retry
    async fn handle_http_send_error(
        &self,
        response: http::Response<Bytes>,
        payload_len: usize,
        chunks: usize,
        attempts: u32,
    ) -> Result<AgentResponse, TraceExporterError> {
        let status = response.status();

        // Check if the agent state has changed for error responses
        self.info_response_observer.check_response(&response);

        let send_result = SendResult::failure(
            TransportErrorType::Http(status.as_u16()),
            payload_len,
            chunks,
            attempts,
        );
        self.emit_send_result(&send_result);

        let body = String::from_utf8_lossy(response.body());
        Err(TraceExporterError::Request(RequestError::new(
            status, &body,
        )))
    }

    /// Check if the agent's payload version has changed based on response headers
    fn check_payload_version_changed(&self, response: &http::Response<Bytes>) -> bool {
        let is_success = response.status().is_success();
        let version_header = response
            .headers()
            .get(DATADOG_RATES_PAYLOAD_VERSION)
            .and_then(|v| v.to_str().ok());
        match (
            is_success,
            self.agent_payload_response_version.as_ref(),
            version_header,
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
                agent_payload_response_version.check_and_update(new_payload_version)
            }
            _ => false,
        }
    }

    /// Handle successful trace sending response
    fn handle_successful_trace_response(
        &self,
        chunks: usize,
        payload_len: usize,
        attempts: u32,
        body: String,
        payload_version_changed: bool,
    ) -> Result<AgentResponse, TraceExporterError> {
        debug!(chunks = chunks, "Trace chunks sent successfully to agent");
        let send_result = SendResult::success(payload_len, chunks, attempts);
        self.emit_send_result(&send_result);

        Ok(if payload_version_changed {
            AgentResponse::Changed { body }
        } else {
            AgentResponse::Unchanged
        })
    }

    async fn handle_agent_response(
        &self,
        chunks: usize,
        response: http::Response<Bytes>,
        payload_len: usize,
        attempts: u32,
    ) -> Result<AgentResponse, TraceExporterError> {
        // Check if the agent state has changed
        self.info_response_observer.check_response(&response);

        let status = response.status();
        let payload_version_changed = self.check_payload_version_changed(&response);
        let body = String::from_utf8_lossy(response.body()).to_string();

        if !status.is_success() {
            warn!(
                status = status.as_u16(),
                "Agent returned non-success status for trace send"
            );
            let send_result = SendResult::failure(
                TransportErrorType::Http(status.as_u16()),
                payload_len,
                chunks,
                attempts,
            );
            self.emit_send_result(&send_result);
            return Err(TraceExporterError::Request(RequestError::new(
                status, &body,
            )));
        }

        self.handle_successful_trace_response(
            chunks,
            payload_len,
            attempts,
            body,
            payload_version_changed,
        )
    }

    /// Return the trace output format that will actually be used to encode and send the next
    /// payload.
    ///
    /// When V1 is configured, the effective format is V1 only after the agent has advertised
    /// `/v1.0/traces` via the `/info` endpoint (fail-closed). Until then — and any time the
    /// agent rolls back this capability — V1 transparently falls back to V0.4. V0.4 and V0.5
    /// pass through unchanged.
    fn effective_output_format(&self) -> TraceExporterOutputFormat {
        match self.output_format {
            TraceExporterOutputFormat::V1 if self.v1_active.load(Ordering::Relaxed) => {
                TraceExporterOutputFormat::V1
            }
            TraceExporterOutputFormat::V1 => TraceExporterOutputFormat::V04,
            other => other,
        }
    }

    #[cfg(test)]
    #[cfg(not(target_arch = "wasm32"))]
    /// Test only function to check if the stats computation is active and the worker is running
    pub fn is_stats_worker_active(&self) -> bool {
        stats::is_stats_worker_active(&self.client_side_stats.status)
    }
}

#[cfg(feature = "telemetry")]
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
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_shared_runtime::ForkSafeRuntime;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::msgpack_encoder;
    use libdd_trace_utils::span::v04::SpanBytes;
    use std::net;

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

        let headers: HeaderMap = (&tracer_tags).into();

        assert_eq!(headers.get("datadog-meta-tracer-version").unwrap(), "v0.1");
        assert_eq!(headers.get("datadog-meta-lang").unwrap(), "rust");
        assert_eq!(headers.get("datadog-meta-lang-version").unwrap(), "1.52.1");
        assert_eq!(
            headers.get("datadog-meta-lang-interpreter").unwrap(),
            "rustc"
        );
        assert!(headers.contains_key("datadog-client-computed-stats"));
        assert!(headers.contains_key("datadog-client-computed-top-level"));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_effective_output_format_v04_passthrough() {
        let exporter = build_test_exporter(
            "http://127.0.0.1:8126".to_string(),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V04,
            false,
            false,
        );
        assert!(matches!(
            exporter.effective_output_format(),
            TraceExporterOutputFormat::V04
        ));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_effective_output_format_v1_pre_negotiation_falls_back_to_v04() {
        let exporter = build_test_exporter(
            "http://127.0.0.1:8126".to_string(),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V1,
            false,
            false,
        );
        assert!(matches!(
            exporter.effective_output_format(),
            TraceExporterOutputFormat::V04
        ));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_effective_output_format_v1_post_negotiation_uses_v1() {
        let exporter = build_test_exporter(
            "http://127.0.0.1:8126".to_string(),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V1,
            false,
            false,
        );
        exporter
            .v1_active
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(matches!(
            exporter.effective_output_format(),
            TraceExporterOutputFormat::V1
        ));
        assert_eq!(
            exporter
                .effective_output_format()
                .add_path(&exporter.endpoint.url)
                .to_string(),
            "http://127.0.0.1:8126/v1.0/traces"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_refresh_v1_active_enables_when_endpoint_advertised() {
        use crate::agent_info::schema::{AgentInfo, AgentInfoStruct};
        let exporter = build_test_exporter(
            "http://127.0.0.1:8126".to_string(),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V1,
            false,
            false,
        );
        let agent_info = Arc::new(AgentInfo {
            state_hash: "hash-1".to_string(),
            info: AgentInfoStruct {
                endpoints: Some(vec![
                    V04_TRACES_ENDPOINT.to_string(),
                    V1_TRACES_ENDPOINT.to_string(),
                ]),
                ..Default::default()
            },
        });
        exporter.refresh_v1_active(&agent_info);
        assert!(exporter
            .v1_active
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_refresh_v1_active_disables_when_endpoint_disappears() {
        use crate::agent_info::schema::{AgentInfo, AgentInfoStruct};
        let exporter = build_test_exporter(
            "http://127.0.0.1:8126".to_string(),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V1,
            false,
            false,
        );
        exporter
            .v1_active
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let agent_info = Arc::new(AgentInfo {
            state_hash: "hash-2".to_string(),
            info: AgentInfoStruct {
                endpoints: Some(vec![V04_TRACES_ENDPOINT.to_string()]),
                ..Default::default()
            },
        });
        exporter.refresh_v1_active(&agent_info);
        assert!(!exporter
            .v1_active
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_refresh_v1_active_handles_missing_endpoints_field() {
        use crate::agent_info::schema::{AgentInfo, AgentInfoStruct};
        let exporter = build_test_exporter(
            "http://127.0.0.1:8126".to_string(),
            None,
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V1,
            false,
            false,
        );
        let agent_info = Arc::new(AgentInfo {
            state_hash: "hash-3".to_string(),
            info: AgentInfoStruct {
                endpoints: None,
                ..Default::default()
            },
        });
        exporter.refresh_v1_active(&agent_info);
        assert!(!exporter
            .v1_active
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    fn read(socket: &net::UdpSocket) -> String {
        let mut buf = [0; 1_000];
        socket.recv(&mut buf).expect("No data");
        let datagram = String::from_utf8_lossy(buf.as_ref());
        datagram.trim_matches(char::from(0)).to_string()
    }

    pub(crate) fn build_test_exporter(
        url: String,
        dogstatsd_url: Option<String>,
        input: TraceExporterInputFormat,
        output: TraceExporterOutputFormat,
        enable_telemetry: bool,
        enable_health_metrics: bool,
    ) -> TraceExporter<NativeCapabilities, ForkSafeRuntime> {
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
            #[cfg(feature = "telemetry")]
            builder.enable_telemetry(TelemetryConfig {
                heartbeat: 100,
                ..Default::default()
            });
        }

        builder.build::<NativeCapabilities>().unwrap()
    }

    // Capturing capabilities: delegate HTTP/sleep to the native impls, but capture
    // log output into a thread-local buffer so log-mode tests can assert the emitted
    // bytes without writing to real stdout. `build` constructs `C` via
    // `C::new_client`, so the buffer is shared through a thread-local rather than an
    // instance field; `new_client` clears it so each build starts fresh.
    thread_local! {
        static LOG_CAPTURE: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    #[derive(Clone, Debug)]
    struct CapturingCapabilities(NativeCapabilities);

    impl HttpClientCapability for CapturingCapabilities {
        fn new_client() -> Self {
            LOG_CAPTURE.with(|c| c.borrow_mut().clear());
            Self(NativeCapabilities::new_client())
        }
        fn request(
            &self,
            req: http::Request<bytes::Bytes>,
        ) -> impl std::future::Future<
            Output = Result<http::Response<bytes::Bytes>, libdd_capabilities::http::HttpError>,
        > + MaybeSend {
            self.0.request(req)
        }
    }

    impl SleepCapability for CapturingCapabilities {
        fn new() -> Self {
            Self(NativeCapabilities::new())
        }
        fn sleep(&self, duration: Duration) -> impl std::future::Future<Output = ()> + MaybeSend {
            self.0.sleep(duration)
        }
    }

    impl LogWriterCapability for CapturingCapabilities {
        fn write_log_output(&self, bytes: &[u8]) -> std::io::Result<()> {
            LOG_CAPTURE.with(|c| c.borrow_mut().extend_from_slice(bytes));
            Ok(())
        }
    }

    fn captured_log() -> Vec<u8> {
        LOG_CAPTURE.with(|c| c.borrow().clone())
    }

    // The real `send` entry point decodes msgpack, hits the log branch, and writes
    // Forwarder-format JSON bytes through the log-output capability.
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_log_mode_send_writes_forwarder_json() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_service("test")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_to_log(None);
        let exporter = builder.build::<CapturingCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"aws.lambda").unwrap(),
            trace_id: 1,
            span_id: 2,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);

        let resp = exporter.send(data.as_ref()).unwrap();
        assert!(matches!(resp, AgentResponse::Unchanged));

        let text = String::from_utf8(captured_log()).unwrap();
        assert!(text.ends_with('\n'), "line must be newline-terminated");
        let line = text.trim_end();
        let v: serde_json::Value = serde_json::from_str(line).expect("valid json line");
        // Forwarder is_trace contract + the span actually round-tripped through
        // msgpack decode -> log encode with hex ids.
        assert!(v["traces"][0][0]["trace_id"].is_string());
        assert_eq!(v["traces"][0][0]["span_id"], "0000000000000002");
        assert_eq!(v["traces"][0][0]["name"], "aws.lambda");
    }

    // Log mode must make zero agent HTTP calls — also guards the worker-gating fix
    // (info-fetcher is not spawned in log mode).
    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_log_mode_makes_no_agent_requests() {
        let fake_agent = MockServer::start();
        // No `when` constraints => matches any request to any path.
        let any = fake_agent.mock(|_when, then| {
            then.status(200).body("{}");
        });

        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url(&fake_agent.url("/"))
            .set_service("test")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_to_log(None);
        let exporter = builder.build::<CapturingCapabilities>().unwrap();
        // Structural guarantee: no agent-info worker is spawned in log mode.
        assert!(exporter.workers.info_fetcher.is_none());

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        // `send` is synchronous and, in log mode, returns after writing through the
        // capability without initiating any HTTP; combined with the structural assert
        // above this is deterministic (no background worker can race the mock).
        exporter.send(data.as_ref()).unwrap();

        assert_eq!(any.calls(), 0, "log mode must not contact the agent");
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
            fake_agent.url(V04_TRACES_ENDPOINT),
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
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);

        let _result = exporter.send(data.as_ref()).expect("failed to send trace");

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
            fake_agent.url(V04_TRACES_ENDPOINT),
            Some(stats_socket.local_addr().unwrap().to_string()),
            TraceExporterInputFormat::V04,
            TraceExporterOutputFormat::V04,
            false,
            true,
        );

        let bad_payload = b"some_bad_payload".as_ref();
        let result = exporter.send(bad_payload);

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
            fake_agent.url(V04_TRACES_ENDPOINT),
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
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

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
            "datadog.tracer.exporter.transport.requests:6|d|#libdatadog_version:{}",
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
            fake_agent.url(V04_TRACES_ENDPOINT),
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
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

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
            "datadog.tracer.exporter.transport.requests:6|d|#libdatadog_version:{}",
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
            fake_agent.url(V04_TRACES_ENDPOINT),
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
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);

        let _result = exporter.send(data.as_ref()).expect("failed to send trace");

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
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::Interrupted =>
            {
                // This is expected - no metrics should be sent when disabled.
                // WouldBlock on Unix, TimedOut on Windows.
                // Interrupted can occur when signals interrupt the blocking
                // recvfrom() syscall before the timeout expires.
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

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8");
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref()).unwrap();

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

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8");
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let code = match exporter.send(data.as_ref()).unwrap_err() {
            TraceExporterError::Request(e) => Some(e.status()),
            _ => None,
        }
        .unwrap();

        assert_eq!(code, http::StatusCode::INTERNAL_SERVER_ERROR);
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

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8");
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let err = exporter.send(data.as_ref());

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
            when.method(POST).path(V04_TRACES_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-rates-payload-version", "abc")
                .body(response_body);
        });

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder.set_url(&server.url("/"));
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let traces = vec![0x90];
        for _ in 0..2 {
            let result = exporter.send(traces.as_ref()).unwrap();
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
            when.method(POST).path(V04_TRACES_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-rates-payload-version", "abc")
                .body(response_body);
        });

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .enable_agent_rates_payload_version();
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        let traces = vec![0x90];
        let result = exporter.send(traces.as_ref()).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        let result = exporter.send(traces.as_ref()).unwrap();
        let AgentResponse::Unchanged = result else {
            panic!("Expected Unchanged response");
        };
        traces_endpoint.assert_calls(2);
        traces_endpoint.delete();

        let traces_endpoint = server.mock(|when, then| {
            when.method(POST).path(V04_TRACES_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-rates-payload-version", "def")
                .body(response_body);
        });
        let result = exporter.send(traces.as_ref()).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        let result = exporter.send(traces.as_ref()).unwrap();
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
                .path(V04_TRACES_ENDPOINT);
            then.status(200).body(
                r#"{
                    "rate_by_service": {
                        "service:test,env:staging": 1.0,
                    }
                }"#,
            );
        });

        let mock_info = server.mock(|when, then| {
            when.method(GET).path(INFO_ENDPOINT);
            then.delay(delay).status(status).body(response);
        });

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
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
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let trace_chunk = vec![SpanBytes {
            duration: 10,
            ..Default::default()
        }];

        let data = msgpack_encoder::v04::to_vec_from_v04(&[trace_chunk]);

        // Wait for the info fetcher to get the config
        while mock_info.calls() == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }

        let _ = exporter.send(data.as_ref()).unwrap();

        mock_traces.assert();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_connection_timeout() {
        let exporter = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder()
            .build::<NativeCapabilities>()
            .unwrap();

        assert_eq!(exporter.endpoint.timeout_ms, Endpoint::default().timeout_ms);

        let timeout = Some(42);
        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder.set_connection_timeout(timeout);

        let exporter = builder.build::<NativeCapabilities>().unwrap();

        assert_eq!(exporter.endpoint.timeout_ms, 42);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_otlp_export_via_builder() {
        let server = MockServer::start();
        let mock_otlp = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/traces")
                .header("Content-Type", "application/json");
            then.status(200).body("");
        });

        let otlp_endpoint = format!("{}/v1/traces", server.url("/").trim_end_matches('/'));
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url("http://127.0.0.1:8126")
            .set_service("svc")
            .set_env("env")
            .set_tracer_version("1.0")
            .set_language("rust")
            .set_language_version("1.0")
            .set_language_interpreter("rustc")
            .set_otlp_endpoint(&otlp_endpoint)
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            resource: BytesString::from_static("res"),
            trace_id: 1,
            span_id: 2,
            parent_id: 0,
            start: 1000,
            duration: 100,
            error: 0,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

        assert!(
            result.is_ok(),
            "OTLP send should succeed: {:?}",
            result.err()
        );
        mock_otlp.assert();
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agentless_export_via_builder() {
        let server = MockServer::start();
        let mock_intake = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/input")
                .header("Content-Type", "application/json")
                .header("dd-api-key", "test-api-key")
                .header("X-Datadog-Trace-Count", "1")
                .header("datadog-meta-lang", "nodejs")
                .header("datadog-meta-tracer-version", "1.0");
            then.status(200).body("");
        });

        let intake_url = format!("{}/v1/input", server.url("/").trim_end_matches('/'));
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_service("svc")
            .set_env("env")
            .set_tracer_version("1.0")
            .set_language("nodejs")
            .set_language_version("v20.11.0")
            .set_language_interpreter("v8")
            .set_agentless_endpoint(&intake_url, "test-api-key")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            resource: BytesString::from_static("res"),
            trace_id: 0xdeadbeef,
            span_id: 2,
            parent_id: 0,
            start: 2_500_000_000,
            duration: 1_000_000,
            error: 0,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        let result = exporter.send(data.as_ref());

        assert!(
            result.is_ok(),
            "Agentless send should succeed: {:?}",
            result.err()
        );
        mock_intake.assert();

        assert_eq!(mock_intake.calls(), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_agentless_export_body_shape() {
        let server = MockServer::start();
        let mock_intake = server.mock(|when, then| {
            when.method(POST)
                .path("/v1/input")
                .body_includes("\"traces\":")
                .body_includes("\"spans\":")
                .body_includes("\"hostname\":\"h-1\"")
                .body_includes("\"languageName\":\"nodejs\"")
                .body_includes("\"_dd.compute_stats\":\"1\"")
                .body_includes("\"_top_level\":1")
                .body_includes("\"_trace_root\":1")
                .body_includes("\"parent_id\":\"0000000000000000\"");
            then.status(200).body("");
        });

        let intake_url = format!("{}/v1/input", server.url("/").trim_end_matches('/'));
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_hostname("h-1")
            .set_service("svc")
            .set_env("env")
            .set_tracer_version("1.0")
            .set_language("nodejs")
            .set_language_version("v20.11.0")
            .set_language_interpreter("v8")
            .set_agentless_endpoint(&intake_url, "k")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04);
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"op").unwrap(),
            service: BytesString::from_static("svc"),
            resource: BytesString::from_static("res"),
            trace_id: 1,
            span_id: 2,
            parent_id: 0,
            start: 0,
            duration: 1,
            ..Default::default()
        }]];
        let data = msgpack_encoder::v04::to_vec_from_v04(&traces);
        exporter.send(data.as_ref()).unwrap();
        mock_intake.assert();
    }
}

#[cfg(test)]
#[cfg(feature = "telemetry")]
mod telemetry_metrics_tests {
    use super::*;
    use crate::trace_exporter::tests::build_test_exporter;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_shared_runtime::ForkSafeRuntime;
    use libdd_tinybytes::BytesString;
    use libdd_trace_utils::span::v05;

    // v05 messagepack empty payload -> [[""], []]
    const V5_EMPTY: [u8; 4] = [0x92, 0x91, 0xA0, 0x90];

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
            when.method(POST).path(V04_TRACES_ENDPOINT);
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

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .enable_telemetry(TelemetryConfig {
                heartbeat: 100,
                ..Default::default()
            });
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces = vec![0x90];
        let result = exporter.send(traces.as_ref()).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        traces_endpoint.assert_calls(1);
        while metrics_endpoint.calls() == 0 {
            std::thread::sleep(Duration::from_millis(100));
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
            when.method(POST).path(V05_TRACES_ENDPOINT);
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
        let result = exporter.send(traces.as_ref()).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        traces_endpoint.assert_calls(1);
        while metrics_endpoint.calls() == 0 {
            std::thread::sleep(Duration::from_millis(100));
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
            when.method(POST).path(V05_TRACES_ENDPOINT).is_true(|req| {
                let bytes = libdd_tinybytes::Bytes::copy_from_slice(req.body_ref());
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

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .enable_telemetry(TelemetryConfig {
                heartbeat: 100,
                ..Default::default()
            })
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V05);

        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let traces = vec![0x90];
        let result = exporter.send(traces.as_ref()).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        traces_endpoint.assert_calls(1);
        while metrics_endpoint.calls() == 0 {
            std::thread::sleep(Duration::from_millis(100));
        }
        metrics_endpoint.assert_calls(1);
    }
}

#[cfg(test)]
mod single_threaded_tests {
    use super::stats::STATS_ENDPOINT;
    use super::*;
    use crate::agent_info;
    use httpmock::prelude::*;
    use libdd_capabilities_impl::NativeCapabilities;
    use libdd_shared_runtime::ForkSafeRuntime;
    use libdd_trace_utils::msgpack_encoder;
    use libdd_trace_utils::span::v04::SpanBytes;

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_shutdown() {
        // Clear the agent info cache to ensure test isolation
        agent_info::clear_cache_for_test();

        let server = MockServer::start();

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path(V04_TRACES_ENDPOINT);
            then.status(200).body("");
        });

        let mock_stats = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path(STATS_ENDPOINT);
            then.status(200).body("");
        });

        let _mock_info = server.mock(|when, then| {
            when.method(GET).path(INFO_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(format!(
                    r#"{{"version":"1","client_drop_p0s":true,"endpoints":["{V04_TRACES_ENDPOINT}","{STATS_ENDPOINT}"]}}"#
                ));
        });

        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
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
            .set_shared_runtime(runtime.clone())
            .enable_stats(Duration::from_secs(10));
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let trace_chunk = vec![SpanBytes {
            duration: 10,
            ..Default::default()
        }];

        let data = msgpack_encoder::v04::to_vec_from_v04(&[trace_chunk]);

        // Wait for the info fetcher to get the config
        while agent_info::get_agent_info().is_none() {
            std::thread::sleep(Duration::from_millis(100));
        }

        let result = exporter.send(data.as_ref());
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

        runtime.shutdown(None).unwrap();

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
                .path(V04_TRACES_ENDPOINT);
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
                .path(STATS_ENDPOINT);
            then.delay(Duration::from_secs(10)).status(200).body("");
        });

        let _mock_info = server.mock(|when, then| {
            when.method(GET).path(INFO_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(format!(
                    r#"{{"version":"1","client_drop_p0s":true,"endpoints":["{V04_TRACES_ENDPOINT}","{STATS_ENDPOINT}"]}}"#
                ));
        });

        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
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
            .set_shared_runtime(runtime.clone())
            .enable_stats(Duration::from_secs(10));
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        let trace_chunk = vec![SpanBytes {
            service: "test".into(),
            name: "test".into(),
            resource: "test".into(),
            r#type: "test".into(),
            duration: 10,
            ..Default::default()
        }];

        let data = msgpack_encoder::v04::to_vec_from_v04(&[trace_chunk]);

        // Wait for agent_info to be present so that sending a trace will trigger the stats worker
        // to start
        while agent_info::get_agent_info().is_none() {
            std::thread::sleep(Duration::from_millis(100));
        }

        exporter.send(data.as_ref()).unwrap();

        // Wait for the stats worker to be active before shutting down to avoid potential flaky
        // tests on CI where we shutdown before the stats worker had time to start
        let start_time = std::time::Instant::now();
        while !exporter.is_stats_worker_active() {
            if start_time.elapsed() > Duration::from_secs(10) {
                panic!("Timeout waiting for stats worker to become active");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        runtime
            .shutdown(Some(Duration::from_millis(5)))
            .unwrap_err(); // The shutdown should timeout

        mock_traces.assert();
    }

    #[cfg(feature = "stats-obfuscation")]
    fn build_obfuscation_test_exporter(
        url: String,
        runtime: Arc<ForkSafeRuntime>,
        opt_in: bool,
    ) -> TraceExporter<NativeCapabilities, ForkSafeRuntime> {
        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&url)
            .set_service("test")
            .set_env("staging")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .set_shared_runtime(runtime)
            .enable_stats(Duration::from_secs(10));
        if opt_in {
            builder.enable_client_side_stats_obfuscation();
        }
        builder.build::<NativeCapabilities>().unwrap()
    }

    #[cfg(feature = "stats-obfuscation")]
    fn run_obfuscation_test(opt_in: bool, agent_obfuscation_version: Option<u32>) -> bool {
        agent_info::clear_cache_for_test();

        let server = MockServer::start();

        let _mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path(V04_TRACES_ENDPOINT);
            then.status(200).body("");
        });

        let _mock_stats = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path(STATS_ENDPOINT);
            then.status(200).body("");
        });

        let info_body = match agent_obfuscation_version {
            Some(v) => format!(
                r#"{{"version":"1","client_drop_p0s":true,"obfuscation_version":{v},"endpoints":["{V04_TRACES_ENDPOINT}","{STATS_ENDPOINT}"]}}"#
            ),
            None => format!(
                r#"{{"version":"1","client_drop_p0s":true,"endpoints":["{V04_TRACES_ENDPOINT}","{STATS_ENDPOINT}"]}}"#
            ),
        };
        let _mock_info = server.mock(|when, then| {
            when.method(GET).path(INFO_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(info_body);
        });

        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());
        let exporter = build_obfuscation_test_exporter(server.url("/"), runtime.clone(), opt_in);

        while agent_info::get_agent_info().is_none() {
            std::thread::sleep(Duration::from_millis(100));
        }

        let trace_chunk = vec![SpanBytes {
            duration: 10,
            ..Default::default()
        }];
        let data = msgpack_encoder::v04::to_vec_from_v04(&[trace_chunk]);
        let _ = exporter.send(data.as_ref());

        let start = std::time::Instant::now();
        while !exporter.is_stats_worker_active() {
            if start.elapsed() > Duration::from_secs(10) {
                panic!("Timeout waiting for stats worker to become active");
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let result = exporter.client_side_stats.obfuscation_config.load().enabled;
        let _ = runtime.shutdown(None);
        result
    }

    /// Runs the three opt-in × agent-support cases sequentially in a single test
    /// to avoid races on the process-global agent info cache.
    #[cfg(feature = "stats-obfuscation")]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_client_side_stats_obfuscation_opt_in() {
        let current_obf_version = crate::trace_exporter::stats::SUPPORTED_OBFUSCATION_VERSION;
        let prev_obf_version = crate::trace_exporter::stats::SUPPORTED_OBFUSCATION_VERSION - 1;
        // Opt-in OFF, agent supports → must stay disabled.
        assert!(
            !run_obfuscation_test(false, Some(current_obf_version)),
            "obfuscation must stay disabled when builder opt-in is absent"
        );
        // Opt-in ON, agent does not advertise support → disabled.
        assert!(
            !run_obfuscation_test(true, None),
            "obfuscation must stay disabled when agent does not advertise support"
        );

        // Opt-in ON, agent obfuscation_version < tracer obfuscation_version -> disabled;
        assert!(
            !run_obfuscation_test(true, Some(prev_obf_version)),
            "obfuscation must stay disabled when agent.obfuscation_version < tracer.obfuscation_version"
        );

        // Opt-in ON, agent supports → enabled.
        assert!(
            run_obfuscation_test(true, Some(current_obf_version)),
            "obfuscation must activate when opted in and agent supports"
        );
    }

    /// Agent rollback / partial-V1 scenario: `/info` advertises `/v1.0/traces` but the actual
    /// endpoint returns 404 (e.g. customer rolled back the agent without `/info` reflecting it).
    /// The fail-closed hook must flip `v1_active` to false on the first 404 so the next send
    /// uses V0.4.
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_v1_404_fails_closed_to_v04() {
        agent_info::clear_cache_for_test();

        let server = MockServer::start();

        let mock_v1 = server.mock(|when, then| {
            when.method(POST).path(V1_TRACES_ENDPOINT);
            then.status(404).body("");
        });

        let mock_v04 = server.mock(|when, then| {
            when.method(POST).path(V04_TRACES_ENDPOINT);
            then.status(200).body("{}");
        });

        let _mock_info = server.mock(|when, then| {
            when.method(GET).path(INFO_ENDPOINT);
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(format!(
                    r#"{{"version":"1","client_drop_p0s":true,"endpoints":["{V1_TRACES_ENDPOINT}","{V04_TRACES_ENDPOINT}"]}}"#
                ));
        });

        let runtime = Arc::new(ForkSafeRuntime::new().unwrap());

        let mut builder = TraceExporter::<NativeCapabilities, ForkSafeRuntime>::builder();
        builder
            .set_url(&server.url("/"))
            .set_service("test")
            .set_env("staging")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_shared_runtime(runtime.clone())
            .enable_v1_protocol();
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        // Wait until /info has been fetched so the next send promotes v1_active=true.
        let start = std::time::Instant::now();
        while agent_info::get_agent_info().is_none() {
            if start.elapsed() > Duration::from_secs(5) {
                panic!("timeout waiting for /info");
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        let trace_chunk = vec![SpanBytes {
            duration: 10,
            ..Default::default()
        }];
        let data = msgpack_encoder::v04::to_vec_from_v04(&[trace_chunk]);

        // 1st send: /info has promoted v1_active=true, so this hits /v1.0/traces and 404s.
        let result1 = exporter.send(&data);
        assert!(result1.is_err(), "first send should error on 404");
        assert!(
            !exporter.v1_active.load(Ordering::Relaxed),
            "v1_active must flip to false after a V1 404"
        );

        // 2nd send: effective format is now V0.4 → hits /v0.4/traces and succeeds.
        let result2 = exporter.send(&data);
        assert!(
            result2.is_ok(),
            "second send (V0.4 fallback) should succeed: {:?}",
            result2.err()
        );

        // The first send retries internally on 4xx (send_with_retry default), so V1 is hit
        // multiple times before the fail-closed flip; we only care that it was hit at all.
        assert!(
            mock_v1.calls() >= 1,
            "V1 endpoint must be tried at least once before the fail-closed flip"
        );
        mock_v04.assert();
    }
}
