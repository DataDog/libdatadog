// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
pub mod agent_response;
pub mod builder;
pub mod error;

// Re-export the builder
pub use builder::TraceExporterBuilder;

use self::agent_response::AgentResponse;
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
    span_concentrator::SpanConcentrator,
    stats_exporter,
};
use arc_swap::{ArcSwap, ArcSwapOption};
use bytes::Bytes;
use datadog_trace_utils::msgpack_decoder::{self, decode::error::DecodeError};
use datadog_trace_utils::send_with_retry::{
    send_with_retry, RetryStrategy, SendWithRetryError, SendWithRetryResult,
};
use datadog_trace_utils::span::{Span, SpanText};
use datadog_trace_utils::trace_utils::{self, TracerHeaderTags};
use datadog_trace_utils::tracer_payload;
use ddcommon::header::{
    APPLICATION_MSGPACK_STR, DATADOG_SEND_REAL_HTTP_STATUS_STR, DATADOG_TRACE_COUNT_STR,
};
use ddcommon::tag::Tag;
use ddcommon::MutexExt;
use ddcommon::{hyper_migration, Endpoint};
use ddtelemetry::worker::TelemetryWorker;
use dogstatsd_client::{Client, DogStatsDAction};
use either::Either;
use http_body_util::BodyExt;
use hyper::http::uri::PathAndQuery;
use hyper::{header::CONTENT_TYPE, Method, Uri};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{borrow::Borrow, collections::HashMap, str::FromStr, time};
use tokio::runtime::Runtime;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

const DEFAULT_STATS_ELIGIBLE_SPAN_KINDS: [&str; 4] = ["client", "server", "producer", "consumer"];
const STATS_ENDPOINT: &str = "/v0.6/stats";
const INFO_ENDPOINT: &str = "/info";

/// Prepared traces payload ready for sending to the agent
struct PreparedTracesPayload {
    /// Serialized msgpack payload
    data: Vec<u8>,
    /// HTTP headers for the request
    headers: HashMap<&'static str, String>,
    /// Number of trace chunks
    chunk_count: usize,
}

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
enum StatsComputationStatus {
    /// Client-side stats has been disabled by the tracer
    Disabled,
    /// Client-side stats has been disabled by the agent or is not supported. It can be enabled
    /// later if the agent configuration changes. This is also the state used when waiting for the
    /// /info response.
    DisabledByAgent { bucket_size: Duration },
    /// Client-side stats is enabled
    Enabled {
        stats_concentrator: Arc<Mutex<SpanConcentrator>>,
        cancellation_token: CancellationToken,
    },
}

#[derive(Debug)]
struct TraceExporterWorkers {
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
    workers: Arc<Mutex<TraceExporterWorkers>>,
    agent_payload_response_version: Option<AgentResponsePayloadVersion>,
}

enum DeserInputFormat {
    V04,
    V05,
}

impl TraceExporter {
    #[allow(missing_docs)]
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    fn runtime(&self) -> Result<Arc<Runtime>, TraceExporterError> {
        match self.runtime.lock_or_panic().as_ref() {
            Some(runtime) => Ok(runtime.clone()),
            None => self.run_worker(),
        }
    }

    pub fn run_worker(&self) -> Result<Arc<Runtime>, TraceExporterError> {
        let runtime = self.get_or_create_runtime()?;
        self.start_all_workers(&runtime)?;
        Ok(runtime)
    }

    /// Get existing runtime or create a new one
    fn get_or_create_runtime(&self) -> Result<Arc<Runtime>, TraceExporterError> {
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
                Ok(runtime)
            }
        }
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
                Err(e) => Err(TraceExporterError::Io(e.into())),
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

    /// Start the stats exporter and enable stats computation
    ///
    /// Should only be used if the agent enabled stats computation
    fn start_stats_computation(
        &self,
        span_kinds: Vec<String>,
        peer_tags: Vec<String>,
    ) -> anyhow::Result<()> {
        if let StatsComputationStatus::DisabledByAgent { bucket_size } =
            **self.client_side_stats.load()
        {
            let stats_concentrator =
                self.create_stats_concentrator(bucket_size, span_kinds, peer_tags);
            let cancellation_token = CancellationToken::new();
            let stats_worker = self.create_and_start_stats_worker(
                bucket_size,
                &stats_concentrator,
                &cancellation_token,
            )?;
            self.update_stats_state(stats_worker, stats_concentrator, cancellation_token);
        }
        Ok(())
    }

    /// Create a new stats concentrator with the given configuration
    fn create_stats_concentrator(
        &self,
        bucket_size: Duration,
        span_kinds: Vec<String>,
        peer_tags: Vec<String>,
    ) -> Arc<Mutex<SpanConcentrator>> {
        Arc::new(Mutex::new(SpanConcentrator::new(
            bucket_size,
            time::SystemTime::now(),
            span_kinds,
            peer_tags,
        )))
    }

    /// Create stats exporter and worker, then start the worker
    fn create_and_start_stats_worker(
        &self,
        bucket_size: Duration,
        stats_concentrator: &Arc<Mutex<SpanConcentrator>>,
        cancellation_token: &CancellationToken,
    ) -> anyhow::Result<PausableWorker<StatsExporter>> {
        let stats_exporter = stats_exporter::StatsExporter::new(
            bucket_size,
            stats_concentrator.clone(),
            self.metadata.clone(),
            Endpoint::from_url(add_path(&self.endpoint.url, STATS_ENDPOINT)),
            cancellation_token.clone(),
        );
        let mut stats_worker = PausableWorker::new(stats_exporter);
        let runtime = self.runtime()?;
        stats_worker.start(&runtime).map_err(|e| {
            TraceExporterError::Internal(InternalErrorKind::InvalidWorkerState(e.to_string()))
        })?;
        Ok(stats_worker)
    }

    /// Update the stats computation state with the new worker and components
    fn update_stats_state(
        &self,
        stats_worker: PausableWorker<StatsExporter>,
        stats_concentrator: Arc<Mutex<SpanConcentrator>>,
        cancellation_token: CancellationToken,
    ) {
        self.workers.lock_or_panic().stats = Some(stats_worker);
        self.client_side_stats
            .store(Arc::new(StatsComputationStatus::Enabled {
                stats_concentrator,
                cancellation_token,
            }));
    }

    /// Stops the stats exporter and disable stats computation
    ///
    /// Used when client-side stats is disabled by the agent
    fn stop_stats_computation(&self) {
        if let StatsComputationStatus::Enabled {
            stats_concentrator,
            cancellation_token,
        } = &**self.client_side_stats.load()
        {
            // If there's no runtime there's no exporter to stop
            if let Ok(runtime) = self.runtime() {
                runtime.block_on(async {
                    cancellation_token.cancel();
                });
                self.workers.lock_or_panic().stats = None;
                let bucket_size = stats_concentrator.lock_or_panic().get_bucket_size();

                self.client_side_stats
                    .store(Arc::new(StatsComputationStatus::DisabledByAgent {
                        bucket_size,
                    }));
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

    /// Get span kinds for stats computation with default fallback
    fn get_span_kinds_for_stats(agent_info: &Arc<AgentInfo>) -> Vec<String> {
        agent_info
            .info
            .span_kinds_stats_computed
            .clone()
            .unwrap_or_else(|| DEFAULT_STATS_ELIGIBLE_SPAN_KINDS.map(String::from).to_vec())
    }

    /// Handle stats computation when agent changes from disabled to enabled
    fn handle_stats_disabled_by_agent(&self, agent_info: &Arc<AgentInfo>) {
        if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
            // Client-side stats is supported by the agent
            let status = self.start_stats_computation(
                Self::get_span_kinds_for_stats(agent_info),
                agent_info.info.peer_tags.clone().unwrap_or_default(),
            );
            match status {
                Ok(()) => info!("Client-side stats enabled"),
                Err(_) => error!("Failed to start stats computation"),
            }
        } else {
            info!("Client-side stats computation has been disabled by the agent")
        }
    }

    /// Handle stats computation when it's already enabled
    fn handle_stats_enabled(
        &self,
        agent_info: &Arc<AgentInfo>,
        stats_concentrator: &Mutex<SpanConcentrator>,
    ) {
        if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
            let mut concentrator = stats_concentrator.lock_or_panic();
            concentrator.set_span_kinds(Self::get_span_kinds_for_stats(agent_info));
            concentrator.set_peer_tags(agent_info.info.peer_tags.clone().unwrap_or_default());
        } else {
            self.stop_stats_computation();
            info!("Client-side stats computation has been disabled by the agent")
        }
    }

    fn check_agent_info(&self) {
        if let Some(agent_info) = agent_info::get_agent_info() {
            if self.has_agent_info_state_changed(&agent_info) {
                match &**self.client_side_stats.load() {
                    StatsComputationStatus::Disabled => {}
                    StatsComputationStatus::DisabledByAgent { .. } => {
                        self.handle_stats_disabled_by_agent(&agent_info);
                    }
                    StatsComputationStatus::Enabled {
                        stats_concentrator, ..
                    } => {
                        self.handle_stats_enabled(&agent_info, stats_concentrator);
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

    /// Build HTTP request for sending trace data to agent
    fn build_trace_request(
        &self,
        data: &[u8],
        trace_count: usize,
        uri: Uri,
    ) -> hyper::Request<hyper_migration::Body> {
        let mut req_builder = self.create_base_request_builder(uri);
        req_builder = self.add_metadata_headers(req_builder);
        req_builder = self.add_trace_headers(req_builder, trace_count);
        self.build_request_with_body(req_builder, data)
    }

    /// Create base HTTP request builder with URI, user agent, and method
    fn create_base_request_builder(&self, uri: Uri) -> hyper::http::request::Builder {
        hyper::Request::builder()
            .uri(uri)
            .header(
                hyper::header::USER_AGENT,
                concat!("Tracer/", env!("CARGO_PKG_VERSION")),
            )
            .method(Method::POST)
    }

    /// Add metadata headers to the request builder
    fn add_metadata_headers(
        &self,
        mut req_builder: hyper::http::request::Builder,
    ) -> hyper::http::request::Builder {
        let headers: HashMap<&'static str, String> = self.metadata.borrow().into();
        for (key, value) in &headers {
            req_builder = req_builder.header(*key, value);
        }
        req_builder
    }

    /// Add trace-specific headers to the request builder
    fn add_trace_headers(
        &self,
        req_builder: hyper::http::request::Builder,
        trace_count: usize,
    ) -> hyper::http::request::Builder {
        req_builder
            .header("Content-type", "application/msgpack")
            .header("X-Datadog-Trace-Count", trace_count.to_string().as_str())
    }

    /// Build the final request with body
    fn build_request_with_body(
        &self,
        req_builder: hyper::http::request::Builder,
        data: &[u8],
    ) -> hyper::Request<hyper_migration::Body> {
        #[allow(clippy::unwrap_used)]
        req_builder
            .body(hyper_migration::Body::from_bytes(Bytes::copy_from_slice(
                data,
            )))
            // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
            .unwrap()
    }

    /// Handle HTTP error response and emit appropriate metrics
    async fn handle_http_error_response(
        &self,
        response: hyper::Response<hyper_migration::Body>,
    ) -> Result<AgentResponse, TraceExporterError> {
        let response_status = response.status();
        let response_body = self.extract_response_body(response).await;
        self.log_and_emit_error_metrics(response_status);
        Err(TraceExporterError::Request(RequestError::new(
            response_status,
            &response_body,
        )))
    }

    /// Extract response body from HTTP response
    async fn extract_response_body(
        &self,
        response: hyper::Response<hyper_migration::Body>,
    ) -> String {
        // TODO: Properly handle non-OK states to prevent possible panics
        // (APMSP-18190).
        #[allow(clippy::unwrap_used)]
        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(body_bytes.to_vec()).unwrap_or_default()
    }

    /// Log error and emit metrics for HTTP error response
    fn log_and_emit_error_metrics(&self, response_status: hyper::StatusCode) {
        let resp_tag_res = &Tag::new("response_code", response_status.as_str());
        match resp_tag_res {
            Ok(resp_tag) => {
                warn!(
                    response_code = response_status.as_u16(),
                    "HTTP error response received from agent"
                );
                self.emit_metric(
                    HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                    Some(vec![&resp_tag]),
                );
            }
            Err(tag_err) => {
                // This should really never happen as response_status is a
                // `NonZeroU16`, but if the response status or tag requirements
                // ever change in the future we still don't want to panic.
                error!(?tag_err, "Failed to serialize response_code to tag")
            }
        }
    }

    /// Handle successful HTTP response
    async fn handle_http_success_response(
        &self,
        response: hyper::Response<hyper_migration::Body>,
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        match response.into_body().collect().await {
            Ok(body) => {
                info!(trace_count, "Traces sent successfully to agent");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::STAT_SEND_TRACES, trace_count as i64),
                    None,
                );
                Ok(AgentResponse::Changed {
                    body: String::from_utf8_lossy(&body.to_bytes()).to_string(),
                })
            }
            Err(err) => {
                error!(
                    error = %err,
                    "Failed to read agent response body"
                );
                self.emit_metric(
                    HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                    None,
                );
                Err(TraceExporterError::from(err))
            }
        }
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
        let req = self.build_trace_request(data, trace_count, uri);
        match hyper_migration::new_default_client().request(req).await {
            Ok(response) => {
                let response = hyper_migration::into_response(response);
                self.process_http_response(response, trace_count).await
            }
            Err(err) => self.handle_request_error(err),
        }
    }

    /// Process HTTP response based on status code
    async fn process_http_response(
        &self,
        response: hyper::Response<hyper_migration::Body>,
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        if !response.status().is_success() {
            self.handle_http_error_response(response).await
        } else {
            self.handle_http_success_response(response, trace_count)
                .await
        }
    }

    /// Handle HTTP request errors
    fn handle_request_error(
        &self,
        err: hyper_util::client::legacy::Error,
    ) -> Result<AgentResponse, TraceExporterError> {
        error!(
            error = %err,
            "Request to agent failed"
        );
        self.emit_metric(
            HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
            None,
        );
        Err(TraceExporterError::from(err))
    }

    /// Emit a health metric to dogstatsd
    fn emit_metric(&self, metric: HealthMetric, custom_tags: Option<Vec<&Tag>>) {
        let has_custom_tags = custom_tags.is_some();
        if let Some(flusher) = &self.dogstatsd {
            let tags = match custom_tags {
                None => Either::Left(&self.common_stats_tags),
                Some(custom) => Either::Right(self.common_stats_tags.iter().chain(custom)),
            };
            match metric {
                HealthMetric::Count(name, c) => {
                    debug!(
                        metric_name = name,
                        count = c,
                        has_custom_tags = has_custom_tags,
                        "Emitting health metric to dogstatsd"
                    );
                    flusher.send(vec![DogStatsDAction::Count(name, c, tags.into_iter())])
                }
            }
        } else {
            debug!(
                metric = ?metric,
                "Skipping metric emission - dogstatsd client not configured"
            );
        }
    }

    /// Add all spans from the given iterator into the stats concentrator
    /// # Panic
    /// Will panic if another thread panicked will holding the lock on `stats_concentrator`
    fn add_spans_to_stats<T: SpanText>(
        &self,
        stats_concentrator: &Mutex<SpanConcentrator>,
        traces: &[Vec<Span<T>>],
    ) {
        #[allow(clippy::unwrap_used)]
        let mut stats_concentrator = stats_concentrator.lock().unwrap();

        let spans = traces.iter().flat_map(|trace| trace.iter());
        for span in spans {
            stats_concentrator.add_span(span);
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
                HealthMetric::Count(health_metrics::STAT_DESER_TRACES_ERRORS, 1),
                None,
            );
            TraceExporterError::Deserialization(e)
        })?;
        info!(
            trace_count = traces.len(),
            "Trace deserialization completed successfully"
        );
        self.emit_metric(
            HealthMetric::Count(health_metrics::STAT_DESER_TRACES, traces.len() as i64),
            None,
        );

        self.send_trace_chunks_inner(traces)
    }

    /// Process traces for stats computation and update header tags accordingly
    fn process_traces_for_stats<T: SpanText>(
        &self,
        traces: &mut Vec<Vec<Span<T>>>,
        header_tags: &mut TracerHeaderTags,
    ) {
        if let StatsComputationStatus::Enabled {
            stats_concentrator, ..
        } = &**self.client_side_stats.load()
        {
            if !self.client_computed_top_level {
                for chunk in traces.iter_mut() {
                    datadog_trace_utils::span::trace_utils::compute_top_level_span(chunk);
                }
            }
            self.add_spans_to_stats(stats_concentrator, traces);
            // Once stats have been computed we can drop all chunks that are not going to be
            // sampled by the agent
            let datadog_trace_utils::span::trace_utils::DroppedP0Stats {
                dropped_p0_traces,
                dropped_p0_spans,
            } = datadog_trace_utils::span::trace_utils::drop_chunks(traces);

            // Update the headers to indicate that stats have been computed and forward dropped
            // traces counts
            header_tags.client_computed_top_level = true;
            header_tags.client_computed_stats = true;
            header_tags.dropped_p0_traces = dropped_p0_traces;
            header_tags.dropped_p0_spans = dropped_p0_spans;
        }
    }

    /// Prepare traces payload and HTTP headers for sending to agent
    fn prepare_traces_payload<T: SpanText>(
        &self,
        traces: Vec<Vec<Span<T>>>,
        header_tags: TracerHeaderTags,
    ) -> Result<PreparedTracesPayload, TraceExporterError> {
        let payload = self.collect_and_process_traces(traces)?;
        let chunks = payload.size();
        let headers = self.build_traces_headers(header_tags, chunks);
        let mp_payload = self.serialize_payload(&payload)?;

        Ok(PreparedTracesPayload {
            data: mp_payload,
            headers,
            chunk_count: chunks,
        })
    }

    /// Collect trace chunks based on output format
    fn collect_and_process_traces<T: SpanText>(
        &self,
        traces: Vec<Vec<Span<T>>>,
    ) -> Result<tracer_payload::TraceChunks<T>, TraceExporterError> {
        let use_v05_format = match self.output_format {
            TraceExporterOutputFormat::V05 => true,
            TraceExporterOutputFormat::V04 => false,
        };
        trace_utils::collect_trace_chunks(traces, use_v05_format).map_err(|e| {
            TraceExporterError::Deserialization(DecodeError::InvalidFormat(e.to_string()))
        })
    }

    /// Build HTTP headers for traces request
    fn build_traces_headers(
        &self,
        header_tags: TracerHeaderTags,
        chunk_count: usize,
    ) -> HashMap<&'static str, String> {
        let mut headers: HashMap<&'static str, String> = header_tags.into();
        headers.insert(DATADOG_SEND_REAL_HTTP_STATUS_STR, "1".to_string());
        headers.insert(DATADOG_TRACE_COUNT_STR, chunk_count.to_string());
        headers.insert(CONTENT_TYPE.as_str(), APPLICATION_MSGPACK_STR.to_string());
        if let Some(agent_payload_response_version) = &self.agent_payload_response_version {
            headers.insert(
                DATADOG_RATES_PAYLOAD_VERSION_HEADER,
                agent_payload_response_version.header_value(),
            );
        }
        headers
    }

    /// Serialize payload to msgpack format
    fn serialize_payload<T: SpanText>(
        &self,
        payload: &tracer_payload::TraceChunks<T>,
    ) -> Result<Vec<u8>, TraceExporterError> {
        match payload {
            tracer_payload::TraceChunks::V04(p) => {
                rmp_serde::to_vec_named(p).map_err(TraceExporterError::Serialization)
            }
            tracer_payload::TraceChunks::V05(p) => {
                rmp_serde::to_vec(p).map_err(TraceExporterError::Serialization)
            }
        }
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

        self.handle_send_result(result, chunks).await
    }

    fn send_trace_chunks_inner<T: SpanText>(
        &self,
        mut traces: Vec<Vec<Span<T>>>,
    ) -> Result<AgentResponse, TraceExporterError> {
        let mut header_tags: TracerHeaderTags = self.metadata.borrow().into();

        // Process stats computation
        self.process_traces_for_stats(&mut traces, &mut header_tags);

        // Prepare payload and headers
        let prepared = self.prepare_traces_payload(traces, header_tags)?;

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
    ) -> Result<AgentResponse, TraceExporterError> {
        match result {
            Ok((response, _)) => self.handle_agent_response(chunks, response).await,
            Err(err) => self.handle_send_error(err).await,
        }
    }

    /// Handle errors from send with retry operation
    async fn handle_send_error(
        &self,
        err: SendWithRetryError,
    ) -> Result<AgentResponse, TraceExporterError> {
        error!(?err, "Error sending traces");
        self.emit_metric(
            HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
            None,
        );

        match err {
            SendWithRetryError::Http(response, _) => self.handle_http_send_error(response).await,
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
    ) -> Result<AgentResponse, TraceExporterError> {
        let status = response.status();

        // Check if the agent state has changed for error responses
        self.info_response_observer.check_response(&response);

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
                self.emit_metric(
                    HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                    None,
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
            HealthMetric::Count(health_metrics::STAT_SEND_TRACES, chunks as i64),
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
            self.emit_metric(
                HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                None,
            );
            return Err(TraceExporterError::Request(RequestError::new(
                status, &body,
            )));
        }

        self.handle_successful_trace_response(chunks, status, body, payload_version_changed)
    }

    fn get_agent_url(&self) -> Uri {
        self.output_format.add_path(&self.endpoint.url)
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
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_shutdown() {
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

        let mock_info = server.mock(|when, then| {
            when.method(GET).path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(r#"{"version":"1","client_drop_p0s":true}"#);
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

        let data = rmp_serde::to_vec_named(&vec![trace_chunk]).unwrap();

        // Wait for the info fetcher to get the config
        while mock_info.hits() == 0 {
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

        exporter.shutdown(None).unwrap();

        // Wait for the mock server to process the stats
        for _ in 0..500 {
            if mock_traces.hits() > 0 && mock_stats.hits() > 0 {
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

        let mock_info = server.mock(|when, then| {
            when.method(GET).path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(r#"{"version":"1","client_drop_p0s":true}"#);
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

        let data = rmp_serde::to_vec_named(&vec![trace_chunk]).unwrap();

        // Wait for the info fetcher to get the config
        while mock_info.hits() == 0 {
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

        exporter
            .shutdown(Some(Duration::from_millis(5)))
            .unwrap_err(); // The shutdown should timeout

        mock_traces.assert();
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
        let data = rmp_serde::to_vec_named(&traces).expect("failed to serialize static trace");

        let _result = exporter
            .send(data.as_ref(), 1)
            .expect("failed to send trace");

        assert_eq!(
            &format!(
                "datadog.libdatadog.deser_traces:2|c|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            &read(&stats_socket)
        );
        assert_eq!(
            &format!(
                "datadog.libdatadog.send.traces:2|c|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            &read(&stats_socket)
        );
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
        );

        let bad_payload = b"some_bad_payload".as_ref();
        let result = exporter.send(bad_payload, 1);

        assert!(result.is_err());

        assert_eq!(
            &format!(
                "datadog.libdatadog.deser_traces.errors:1|c|#libdatadog_version:{}",
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
        );

        let traces: Vec<Vec<SpanBytes>> = vec![vec![SpanBytes {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let data = rmp_serde::to_vec_named(&traces).expect("failed to serialize static trace");
        let result = exporter.send(data.as_ref(), 1);

        assert!(result.is_err());

        assert_eq!(
            &format!(
                "datadog.libdatadog.deser_traces:1|c|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            &read(&stats_socket)
        );
        // todo: support health metrics from within send data?
        //assert_eq!(&format!("datadog.libdatadog.send.traces.errors:1|c|#libdatadog_version:{},
        // response_code:400", env!("CARGO_PKG_VERSION")), &read(&stats_socket));
        assert_eq!(
            &format!(
                "datadog.libdatadog.send.traces.errors:1|c|#libdatadog_version:{}",
                env!("CARGO_PKG_VERSION")
            ),
            &read(&stats_socket)
        );
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
        let data = rmp_serde::to_vec_named(&traces).expect("failed to serialize static trace");
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
        let data = rmp_serde::to_vec_named(&traces).expect("failed to serialize static trace");
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
        let data = rmp_serde::to_vec_named(&traces).expect("failed to serialize static trace");
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
                .body_contains("\"metric\":\"trace_api.bytes\"")
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

        traces_endpoint.assert_hits(1);
        while metrics_endpoint.hits() == 0 {
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
        metrics_endpoint.assert_hits(1);
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
                .body_contains("\"metric\":\"trace_api.bytes\"")
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
        );

        let v5: (Vec<BytesString>, Vec<Vec<v05::Span>>) = (vec![], vec![]);
        let traces = rmp_serde::to_vec(&v5).unwrap();
        let result = exporter.send(traces.as_ref(), 1).unwrap();
        let AgentResponse::Changed { body } = result else {
            panic!("Expected Changed response");
        };
        assert_eq!(body, response_body);

        traces_endpoint.assert_hits(1);
        while metrics_endpoint.hits() == 0 {
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
        metrics_endpoint.assert_hits(1);
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
            when.method(POST).path("/v0.5/traces").matches(|req| {
                let bytes = tinybytes::Bytes::copy_from_slice(req.body.as_ref().unwrap());
                bytes.to_vec() == V5_EMPTY
            });
            then.status(200)
                .header("content-type", "application/json")
                .body(response_body);
        });

        let metrics_endpoint = server.mock(|when, then| {
            when.method(POST)
                .body_contains("\"metric\":\"trace_api.bytes\"")
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

        traces_endpoint.assert_hits(1);
        while metrics_endpoint.hits() == 0 {
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
        metrics_endpoint.assert_hits(1);
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
        traces_endpoint.assert_hits(2);
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
        traces_endpoint.assert_hits(2);
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
        traces_endpoint.assert_hits(2);
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

        let data = rmp_serde::to_vec_named(&vec![trace_chunk]).unwrap();

        // Wait for the info fetcher to get the config
        while mock_info.hits() == 0 {
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
}
