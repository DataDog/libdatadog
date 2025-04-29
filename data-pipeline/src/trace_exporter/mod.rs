// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
pub mod agent_response;
pub mod error;
use crate::agent_info::{AgentInfoArc, AgentInfoFetcher};
use crate::telemetry::{SendPayloadTelemetry, TelemetryClient, TelemetryClientBuilder};
use crate::trace_exporter::error::{RequestError, TraceExporterError};
use crate::{
    health_metrics, health_metrics::HealthMetric, span_concentrator::SpanConcentrator,
    stats_exporter,
};
use arc_swap::{ArcSwap, ArcSwapOption};
use bytes::Bytes;
use datadog_trace_utils::msgpack_decoder::{self, decode::error::DecodeError};
use datadog_trace_utils::send_with_retry::{send_with_retry, RetryStrategy, SendWithRetryError};
use datadog_trace_utils::span::{Span, SpanText};
use datadog_trace_utils::trace_utils::{self, TracerHeaderTags};
use datadog_trace_utils::tracer_payload;
use ddcommon::header::{
    APPLICATION_MSGPACK_STR, DATADOG_SEND_REAL_HTTP_STATUS_STR, DATADOG_TRACE_COUNT_STR,
};
use ddcommon::tag::Tag;
use ddcommon::{hyper_migration, parse_uri, tag, Endpoint};
use dogstatsd_client::{new, Client, DogStatsDAction};
use either::Either;
use error::BuilderErrorKind;
use http_body_util::BodyExt;
use hyper::http::uri::PathAndQuery;
use hyper::{header::CONTENT_TYPE, Method, Uri};
use log::{error, info};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{borrow::Borrow, collections::HashMap, str::FromStr, time};
use tokio::{runtime::Runtime, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use self::agent_response::AgentResponse;

const DEFAULT_STATS_ELIGIBLE_SPAN_KINDS: [&str; 4] = ["client", "server", "producer", "consumer"];
const STATS_ENDPOINT: &str = "/v0.6/stats";
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
        exporter_handle: JoinHandle<()>,
    },
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
    runtime: Runtime,
    /// None if dogstatsd is disabled
    dogstatsd: Option<Client>,
    common_stats_tags: Vec<Tag>,
    client_computed_top_level: bool,
    client_side_stats: ArcSwap<StatsComputationStatus>,
    agent_info: AgentInfoArc,
    previous_info_state: ArcSwapOption<String>,
    telemetry: Option<TelemetryClient>,
}

impl TraceExporter {
    #[allow(missing_docs)]
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    /// Send msgpack serialized traces to the agent
    #[allow(missing_docs)]
    pub fn send(
        &self,
        data: &[u8],
        trace_count: usize,
    ) -> Result<AgentResponse, TraceExporterError> {
        self.check_agent_info();

        match self.input_format {
            TraceExporterInputFormat::Proxy => self.send_proxy(data.as_ref(), trace_count),
            TraceExporterInputFormat::V04 => match msgpack_decoder::v04::from_slice(data) {
                Ok((traces, _)) => self.send_trace_collection(traces),
                Err(e) => Err(TraceExporterError::Deserialization(e)),
            },
            TraceExporterInputFormat::V05 => match msgpack_decoder::v05::from_slice(data) {
                Ok((traces, _)) => self.send_trace_collection(traces),
                Err(e) => Err(TraceExporterError::Deserialization(e)),
            },
        }
        .and_then(|res| {
            if res.is_empty() {
                return Err(TraceExporterError::Agent(
                    error::AgentErrorKind::EmptyResponse,
                ));
            }

            Ok(AgentResponse::from(res))
        })
        .map_err(|err| {
            if let TraceExporterError::Deserialization(ref e) = err {
                error!("Error deserializing trace from request body: {e}");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::STAT_DESER_TRACES_ERRORS, 1),
                    None,
                );
            }
            err
        })
    }

    /// Safely shutdown the TraceExporter and all related tasks
    pub fn shutdown(self, timeout: Option<Duration>) -> Result<(), TraceExporterError> {
        if let Some(timeout) = timeout {
            match self.runtime.block_on(async {
                tokio::time::timeout(timeout, async {
                    let stats_status: Option<StatsComputationStatus> =
                        Arc::<StatsComputationStatus>::into_inner(
                            self.client_side_stats.into_inner(),
                        );
                    if let Some(StatsComputationStatus::Enabled {
                        stats_concentrator: _,
                        cancellation_token,
                        exporter_handle,
                    }) = stats_status
                    {
                        cancellation_token.cancel();
                        let _ = exporter_handle.await;
                    }
                    if let Some(telemetry) = self.telemetry {
                        telemetry.shutdown().await;
                    }
                })
                .await
            }) {
                Ok(()) => Ok(()),
                Err(e) => Err(TraceExporterError::Io(e.into())),
            }
        } else {
            self.runtime.block_on(async {
                let stats_status: Option<StatsComputationStatus> =
                    Arc::<StatsComputationStatus>::into_inner(self.client_side_stats.into_inner());
                if let Some(StatsComputationStatus::Enabled {
                    stats_concentrator: _,
                    cancellation_token,
                    exporter_handle,
                }) = stats_status
                {
                    cancellation_token.cancel();
                    let _ = exporter_handle.await;
                }
                if let Some(telemetry) = self.telemetry {
                    telemetry.shutdown().await;
                }
            });
            Ok(())
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
            let stats_concentrator = Arc::new(Mutex::new(SpanConcentrator::new(
                bucket_size,
                time::SystemTime::now(),
                span_kinds,
                peer_tags,
            )));

            let cancellation_token = CancellationToken::new();

            let mut stats_exporter = stats_exporter::StatsExporter::new(
                bucket_size,
                stats_concentrator.clone(),
                self.metadata.clone(),
                Endpoint::from_url(add_path(&self.endpoint.url, STATS_ENDPOINT)),
                cancellation_token.clone(),
            );

            let exporter_handle = self.runtime.spawn(async move {
                stats_exporter.run().await;
            });

            self.client_side_stats
                .store(Arc::new(StatsComputationStatus::Enabled {
                    stats_concentrator,
                    cancellation_token,
                    exporter_handle,
                }));
        };
        Ok(())
    }

    /// Stops the stats exporter and disable stats computation
    ///
    /// Used when client-side stats is disabled by the agent
    fn stop_stats_computation(&self) {
        if let StatsComputationStatus::Enabled {
            stats_concentrator,
            cancellation_token,
            exporter_handle: _,
        } = &**self.client_side_stats.load()
        {
            self.runtime.block_on(async {
                cancellation_token.cancel();
            });
            #[allow(clippy::unwrap_used)]
            let bucket_size = stats_concentrator.lock().unwrap().get_bucket_size();

            self.client_side_stats
                .store(Arc::new(StatsComputationStatus::DisabledByAgent {
                    bucket_size,
                }));
        }
    }

    /// Check for a new state of agent_info and update the trace exporter if needed
    fn check_agent_info(&self) {
        if let Some(agent_info) = self.agent_info.load().as_deref() {
            if Some(agent_info.state_hash.as_str())
                != self
                    .previous_info_state
                    .load()
                    .as_deref()
                    .map(|s| s.as_str())
            {
                match &**self.client_side_stats.load() {
                    StatsComputationStatus::Disabled => {}
                    StatsComputationStatus::DisabledByAgent { .. } => {
                        if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
                            // Client-side stats is supported by the agent
                            let status = self.start_stats_computation(
                                agent_info
                                    .info
                                    .span_kinds_stats_computed
                                    .clone()
                                    .unwrap_or_else(|| {
                                        DEFAULT_STATS_ELIGIBLE_SPAN_KINDS.map(String::from).to_vec()
                                    }),
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
                    StatsComputationStatus::Enabled {
                        stats_concentrator,
                        cancellation_token: _,
                        exporter_handle: _,
                    } => {
                        if agent_info.info.client_drop_p0s.is_some_and(|v| v) {
                            #[allow(clippy::unwrap_used)]
                            let mut concentrator = stats_concentrator.lock().unwrap();

                            concentrator.set_span_kinds(
                                agent_info
                                    .info
                                    .span_kinds_stats_computed
                                    .clone()
                                    .unwrap_or_else(|| {
                                        DEFAULT_STATS_ELIGIBLE_SPAN_KINDS.map(String::from).to_vec()
                                    }),
                            );
                            concentrator.set_peer_tags(
                                agent_info.info.peer_tags.clone().unwrap_or_default(),
                            );
                        } else {
                            self.stop_stats_computation();
                            info!("Client-side stats computation has been disabled by the agent")
                        }
                    }
                }
                self.previous_info_state
                    .store(Some(agent_info.state_hash.clone().into()))
            }
        }
    }

    /// !!! This function is only for testing purposes !!!
    /// This function waits the agent info to be ready by checking the agent_info state.
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
    /// snapshots non-determinitic.
    pub fn wait_agent_info_ready(&self, timeout: Duration) -> anyhow::Result<()> {
        let start = std::time::Instant::now();
        loop {
            if std::time::Instant::now().duration_since(start) > timeout {
                anyhow::bail!("Timeout waiting for agent info to be ready",);
            }
            if self.agent_info.load().is_some() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send_proxy(&self, data: &[u8], trace_count: usize) -> Result<String, TraceExporterError> {
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
    ) -> Result<String, TraceExporterError> {
        self.runtime.block_on(async {
            let mut req_builder = hyper::Request::builder()
                .uri(uri)
                .header(
                    hyper::header::USER_AGENT,
                    concat!("Tracer/", env!("CARGO_PKG_VERSION")),
                )
                .method(Method::POST);

            let headers: HashMap<&'static str, String> = self.metadata.borrow().into();

            for (key, value) in &headers {
                req_builder = req_builder.header(*key, value);
            }
            req_builder = req_builder
                .header("Content-type", "application/msgpack")
                .header("X-Datadog-Trace-Count", trace_count.to_string().as_str());

            #[allow(clippy::unwrap_used)]
            let req = req_builder
                .body(hyper_migration::Body::from_bytes(Bytes::copy_from_slice(
                    data,
                )))
                // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
                .unwrap();

            match hyper_migration::new_default_client().request(req).await {
                Ok(response) => {
                    let response_status = response.status();
                    if !response_status.is_success() {
                        // TODO: Properly handle non-OK states to prevent possible panics
                        // (APMSP-18190).
                        #[allow(clippy::unwrap_used)]
                        let body_bytes = response.into_body().collect().await.unwrap().to_bytes();
                        let response_body =
                            String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                        let resp_tag_res = &Tag::new("response_code", response_status.as_str());
                        match resp_tag_res {
                            Ok(resp_tag) => {
                                self.emit_metric(
                                    HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                                    Some(vec![&resp_tag]),
                                );
                            }
                            Err(tag_err) => {
                                // This should really never happen as response_status is a
                                // `NonZeroU16`, but if the response status or tag requirements
                                // ever change in the future we still don't want to panic.
                                error!("Failed to serialize response_code to tag {}", tag_err)
                            }
                        }
                        return Err(TraceExporterError::Request(RequestError::new(
                            response_status,
                            &response_body,
                        )));
                        //anyhow::bail!("Agent did not accept traces: {response_body}");
                    }
                    match response.into_body().collect().await {
                        Ok(body) => {
                            self.emit_metric(
                                HealthMetric::Count(
                                    health_metrics::STAT_SEND_TRACES,
                                    trace_count as i64,
                                ),
                                None,
                            );
                            Ok(String::from_utf8_lossy(&body.to_bytes()).to_string())
                        }
                        Err(err) => {
                            self.emit_metric(
                                HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                                None,
                            );
                            Err(TraceExporterError::from(err))
                            // anyhow::bail!("Error reading agent response body: {err}");
                        }
                    }
                }
                Err(err) => {
                    self.emit_metric(
                        HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                        None,
                    );
                    Err(TraceExporterError::from(err))
                }
            }
        })
    }

    /// Emit a health metric to dogstatsd
    fn emit_metric(&self, metric: HealthMetric, custom_tags: Option<Vec<&Tag>>) {
        if let Some(flusher) = &self.dogstatsd {
            let tags = match custom_tags {
                None => Either::Left(&self.common_stats_tags),
                Some(custom) => Either::Right(self.common_stats_tags.iter().chain(custom)),
            };
            match metric {
                HealthMetric::Count(name, c) => {
                    flusher.send(vec![DogStatsDAction::Count(name, c, tags.into_iter())])
                }
            }
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

    pub fn send_trace_chunks<T: SpanText>(
        &self,
        trace_chunks: Vec<Vec<Span<T>>>,
    ) -> Result<String, TraceExporterError> {
        self.check_agent_info();
        self.send_trace_collection(trace_chunks)
    }

    fn send_trace_collection<T: SpanText>(
        &self,
        mut traces: Vec<Vec<Span<T>>>,
    ) -> Result<String, TraceExporterError> {
        self.emit_metric(
            HealthMetric::Count(health_metrics::STAT_DESER_TRACES, traces.len() as i64),
            None,
        );

        let mut header_tags: TracerHeaderTags = self.metadata.borrow().into();

        // Stats computation
        if let StatsComputationStatus::Enabled {
            stats_concentrator, ..
        } = &**self.client_side_stats.load()
        {
            if !self.client_computed_top_level {
                for chunk in traces.iter_mut() {
                    datadog_trace_utils::span::trace_utils::compute_top_level_span(chunk);
                }
            }
            self.add_spans_to_stats(stats_concentrator, &traces);
            // Once stats have been computed we can drop all chunks that are not going to be
            // sampled by the agent
            let datadog_trace_utils::span::trace_utils::DroppedP0Stats {
                dropped_p0_traces,
                dropped_p0_spans,
            } = datadog_trace_utils::span::trace_utils::drop_chunks(&mut traces);

            // Update the headers to indicate that stats have been computed and forward dropped
            // traces counts
            header_tags.client_computed_top_level = true;
            header_tags.client_computed_stats = true;
            header_tags.dropped_p0_traces = dropped_p0_traces;
            header_tags.dropped_p0_spans = dropped_p0_spans;
        }

        let use_v05_format = match (self.input_format, self.output_format) {
            (TraceExporterInputFormat::V04, TraceExporterOutputFormat::V04) => false,
            (TraceExporterInputFormat::V04, TraceExporterOutputFormat::V05)
            | (TraceExporterInputFormat::V05, TraceExporterOutputFormat::V05) => true,
            (TraceExporterInputFormat::V05, TraceExporterOutputFormat::V04) => {
                // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
                unreachable!("Conversion from v05 to v04 not implemented")
            }
            (TraceExporterInputFormat::Proxy, _) => {
                // TODO: Properly handle non-OK states to prevent possible panics (APMSP-18190).
                unreachable!("Codepath invalid for proxy mode",)
            }
        };
        let payload = trace_utils::collect_trace_chunks(traces, use_v05_format).map_err(|e| {
            TraceExporterError::Deserialization(DecodeError::InvalidFormat(e.to_string()))
        })?;

        let chunks = payload.size();
        let endpoint = Endpoint {
            url: self.get_agent_url(),
            ..self.endpoint.clone()
        };
        let mut headers: HashMap<&str, String> = header_tags.into();
        headers.insert(DATADOG_SEND_REAL_HTTP_STATUS_STR, "1".to_string());
        headers.insert(DATADOG_TRACE_COUNT_STR, chunks.to_string());
        headers.insert(CONTENT_TYPE.as_str(), APPLICATION_MSGPACK_STR.to_string());

        let strategy = RetryStrategy::default();
        let mp_payload = match &payload {
            tracer_payload::TraceChunks::V04(p) => {
                rmp_serde::to_vec_named(p).map_err(TraceExporterError::Serialization)?
            }
            tracer_payload::TraceChunks::V05(p) => {
                rmp_serde::to_vec(p).map_err(TraceExporterError::Serialization)?
            }
        };

        let payload_len = mp_payload.len();

        self.runtime.block_on(async {
            // Send traces to the agent
            let result = send_with_retry(&endpoint, mp_payload, &headers, &strategy, None).await;

            // Send telemetry for the payload sending
            if let Some(telemetry) = &self.telemetry {
                if let Err(e) = telemetry.send(&SendPayloadTelemetry::from_retry_result(
                    &result,
                    payload_len as u64,
                    chunks as u64,
                )) {
                    error!("Error sending telemetry: {}", e.to_string());
                }
            }

            // Handle the result
            match result {
                Ok((response, _)) => {
                    let status = response.status();
                    let body = match response.into_body().collect().await {
                        Ok(body) => String::from_utf8_lossy(&body.to_bytes()).to_string(),
                        Err(err) => {
                            error!("Error reading agent response body: {err}");
                            self.emit_metric(
                                HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                                None,
                            );
                            return Err(TraceExporterError::from(err));
                        }
                    };

                    if status.is_success() {
                        self.emit_metric(
                            HealthMetric::Count(health_metrics::STAT_SEND_TRACES, chunks as i64),
                            None,
                        );
                        Ok(body)
                    } else {
                        self.emit_metric(
                            HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                            None,
                        );
                        Err(TraceExporterError::Request(RequestError::new(
                            status, &body,
                        )))
                    }
                }
                Err(err) => {
                    error!("Error sending traces: {err}");
                    self.emit_metric(
                        HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                        None,
                    );
                    match err {
                        SendWithRetryError::Http(response, _) => {
                            let status = response.status();
                            let body = match response.into_body().collect().await {
                                Ok(body) => body.to_bytes(),
                                Err(err) => {
                                    error!("Error reading agent response body: {err}");
                                    return Err(TraceExporterError::from(err));
                                }
                            };
                            Err(TraceExporterError::Request(RequestError::new(
                                status,
                                &String::from_utf8_lossy(&body),
                            )))
                        }
                        SendWithRetryError::Timeout(_) => Err(TraceExporterError::from(
                            io::Error::from(io::ErrorKind::TimedOut),
                        )),
                        SendWithRetryError::Network(err, _) => Err(TraceExporterError::from(err)),
                        SendWithRetryError::Build(_) => Err(TraceExporterError::from(
                            io::Error::from(io::ErrorKind::Other),
                        )),
                    }
                }
            }
        })
    }

    fn get_agent_url(&self) -> Uri {
        self.output_format.add_path(&self.endpoint.url)
    }
}

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8126";

#[derive(Debug, Default, Clone)]
pub struct TelemetryConfig {
    pub heartbeat: u64,
    pub runtime_id: Option<String>,
    pub debug_enabled: bool,
}

#[allow(missing_docs)]
#[derive(Default, Debug)]
pub struct TraceExporterBuilder {
    url: Option<String>,
    hostname: String,
    env: String,
    app_version: String,
    service: String,
    tracer_version: String,
    language: String,
    language_version: String,
    language_interpreter: String,
    language_interpreter_vendor: String,
    git_commit_sha: String,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    dogstatsd_url: Option<String>,
    client_computed_stats: bool,
    client_computed_top_level: bool,
    // Stats specific fields
    /// A Some value enables stats-computation, None if it is disabled
    stats_bucket_size: Option<Duration>,
    peer_tags_aggregation: bool,
    compute_stats_by_span_kind: bool,
    peer_tags: Vec<String>,
    telemetry: Option<TelemetryConfig>,
    test_session_token: Option<String>,
}

impl TraceExporterBuilder {
    /// Sets the URL of the agent.
    ///
    /// The agent supports the following URL schemes:
    ///
    /// - **TCP:** `http://<host>:<port>`
    ///   - Example: `set_url("http://localhost:8126")`
    ///
    /// - **UDS (Unix Domain Socket):** `unix://<path>`
    ///   - Example: `set_url("unix://var/run/datadog/apm.socket")`
    ///
    /// - **Windows Named Pipe:** `windows:\\.\pipe\<name>`
    ///   - Example: `set_url(r"windows:\\.\pipe\datadog-apm")`
    pub fn set_url(&mut self, url: &str) -> &mut Self {
        self.url = Some(url.to_owned());
        self
    }

    /// Set the URL to communicate with a dogstatsd server
    pub fn set_dogstatsd_url(&mut self, url: &str) -> &mut Self {
        self.dogstatsd_url = Some(url.to_owned());
        self
    }

    /// Set the hostname used for stats payload
    /// Only used when client-side stats is enabled
    pub fn set_hostname(&mut self, hostname: &str) -> &mut Self {
        hostname.clone_into(&mut self.hostname);
        self
    }

    /// Set the env used for stats payloads
    /// Only used when client-side stats is enabled
    pub fn set_env(&mut self, env: &str) -> &mut Self {
        env.clone_into(&mut self.env);
        self
    }

    /// Set the app version which corresponds to the `version` meta tag
    /// Only used when client-side stats is enabled
    pub fn set_app_version(&mut self, app_version: &str) -> &mut Self {
        app_version.clone_into(&mut self.app_version);
        self
    }

    /// Set the service name used for stats payloads.
    /// Only used when client-side stats is enabled
    pub fn set_service(&mut self, service: &str) -> &mut Self {
        service.clone_into(&mut self.service);
        self
    }

    /// Set the `git_commit_sha` corresponding to the `_dd.git.commit.sha` meta tag
    /// Only used when client-side stats is enabled
    pub fn set_git_commit_sha(&mut self, git_commit_sha: &str) -> &mut Self {
        git_commit_sha.clone_into(&mut self.git_commit_sha);
        self
    }

    /// Set the `Datadog-Meta-Tracer-Version` header
    pub fn set_tracer_version(&mut self, tracer_version: &str) -> &mut Self {
        tracer_version.clone_into(&mut self.tracer_version);
        self
    }

    /// Set the `Datadog-Meta-Lang` header
    pub fn set_language(&mut self, lang: &str) -> &mut Self {
        lang.clone_into(&mut self.language);
        self
    }

    /// Set the `Datadog-Meta-Lang-Version` header
    pub fn set_language_version(&mut self, lang_version: &str) -> &mut Self {
        lang_version.clone_into(&mut self.language_version);
        self
    }

    /// Set the `Datadog-Meta-Lang-Interpreter` header
    pub fn set_language_interpreter(&mut self, lang_interpreter: &str) -> &mut Self {
        lang_interpreter.clone_into(&mut self.language_interpreter);
        self
    }

    /// Set the `Datadog-Meta-Lang-Interpreter-Vendor` header
    pub fn set_language_interpreter_vendor(&mut self, lang_interpreter_vendor: &str) -> &mut Self {
        lang_interpreter_vendor.clone_into(&mut self.language_interpreter_vendor);
        self
    }

    #[allow(missing_docs)]
    pub fn set_input_format(&mut self, input_format: TraceExporterInputFormat) -> &mut Self {
        self.input_format = input_format;
        self
    }

    #[allow(missing_docs)]
    pub fn set_output_format(&mut self, output_format: TraceExporterOutputFormat) -> &mut Self {
        self.output_format = output_format;
        self
    }

    /// Set the header indicating the tracer has computed the top-level tag
    pub fn set_client_computed_top_level(&mut self) -> &mut Self {
        self.client_computed_top_level = true;
        self
    }

    /// Set the header indicating the tracer has already computed stats.
    /// This should not be used when stats computation is enabled.
    pub fn set_client_computed_stats(&mut self) -> &mut Self {
        self.client_computed_stats = true;
        self
    }

    /// Set the `X-Datadog-Test-Session-Token` header. Only used for testing with the test agent.
    pub fn set_test_session_token(&mut self, test_session_token: &str) -> &mut Self {
        self.test_session_token = Some(test_session_token.to_string());
        self
    }

    /// Enable stats computation on traces sent through this exporter
    pub fn enable_stats(&mut self, bucket_size: Duration) -> &mut Self {
        self.stats_bucket_size = Some(bucket_size);
        self
    }

    /// Enable peer tags aggregation for stats computation (requires stats computation to be
    /// enabled)
    pub fn enable_stats_peer_tags_aggregation(&mut self, peer_tags: Vec<String>) -> &mut Self {
        self.peer_tags_aggregation = true;
        self.peer_tags = peer_tags;
        self
    }

    /// Enable stats eligibility by span kind (requires stats computation to be
    /// enabled)
    pub fn enable_compute_stats_by_span_kind(&mut self) -> &mut Self {
        self.compute_stats_by_span_kind = true;
        self
    }

    /// Enables sending telemetry metrics.
    pub fn enable_telemetry(&mut self, cfg: Option<TelemetryConfig>) -> &mut Self {
        if let Some(cfg) = cfg {
            self.telemetry = Some(cfg);
        } else {
            self.telemetry = Some(TelemetryConfig::default());
        }
        self
    }

    #[allow(missing_docs)]
    pub fn build(self) -> Result<TraceExporter, TraceExporterError> {
        if !Self::is_inputs_outputs_formats_compatible(self.input_format, self.output_format) {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "Combination of input and output formats not allowed".to_string(),
                ),
            ));
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()?;

        let dogstatsd = self.dogstatsd_url.and_then(|u| {
            new(Endpoint::from_slice(&u)).ok() // If we couldn't set the endpoint return
                                               // None
        });

        let base_url = self.url.as_deref().unwrap_or(DEFAULT_AGENT_URL);

        let agent_url: hyper::Uri = parse_uri(base_url).map_err(|e: anyhow::Error| {
            TraceExporterError::Builder(BuilderErrorKind::InvalidUri(e.to_string()))
        })?;

        let libdatadog_version = tag!("libdatadog_version", env!("CARGO_PKG_VERSION"));
        let mut stats = StatsComputationStatus::Disabled;

        let info_fetcher = AgentInfoFetcher::new(
            Endpoint::from_url(add_path(&agent_url, INFO_ENDPOINT)),
            Duration::from_secs(5 * 60),
        );

        let agent_info = info_fetcher.get_info();
        runtime.spawn(async move {
            info_fetcher.run().await;
        });

        // Proxy mode does not support stats
        if self.input_format != TraceExporterInputFormat::Proxy {
            if let Some(bucket_size) = self.stats_bucket_size {
                // Client-side stats is considered not supported by the agent until we receive
                // the agent_info
                stats = StatsComputationStatus::DisabledByAgent { bucket_size };
            }
        }

        let telemetry = if let Some(telemetry_config) = self.telemetry {
            Some(runtime.block_on(async {
                let mut builder = TelemetryClientBuilder::default()
                    .set_language(&self.language)
                    .set_language_version(&self.language_version)
                    .set_service_name(&self.service)
                    .set_tracer_version(&self.tracer_version)
                    .set_heartbeat(telemetry_config.heartbeat)
                    .set_url(base_url)
                    .set_debug_enabled(telemetry_config.debug_enabled);
                if let Some(id) = telemetry_config.runtime_id {
                    builder = builder.set_runtime_id(&id);
                }
                builder.build().await
            })?)
        } else {
            None
        };

        if let Some(client) = &telemetry {
            runtime.block_on(client.start());
        }

        Ok(TraceExporter {
            endpoint: Endpoint {
                url: agent_url,
                test_token: self.test_session_token.map(|token| token.into()),
                ..Default::default()
            },
            metadata: TracerMetadata {
                tracer_version: self.tracer_version,
                language_version: self.language_version,
                language_interpreter: self.language_interpreter,
                language_interpreter_vendor: self.language_interpreter_vendor,
                language: self.language,
                git_commit_sha: self.git_commit_sha,
                client_computed_stats: self.client_computed_stats,
                client_computed_top_level: self.client_computed_top_level,
                hostname: self.hostname,
                env: self.env,
                app_version: self.app_version,
                runtime_id: uuid::Uuid::new_v4().to_string(),
                service: self.service,
            },
            input_format: self.input_format,
            output_format: self.output_format,
            client_computed_top_level: self.client_computed_top_level,
            runtime,
            dogstatsd,
            common_stats_tags: vec![libdatadog_version],
            client_side_stats: ArcSwap::new(stats.into()),
            agent_info,
            previous_info_state: ArcSwapOption::new(None),
            telemetry,
        })
    }

    fn is_inputs_outputs_formats_compatible(
        input: TraceExporterInputFormat,
        output: TraceExporterOutputFormat,
    ) -> bool {
        match input {
            TraceExporterInputFormat::Proxy => true,
            TraceExporterInputFormat::V04 => matches!(
                output,
                TraceExporterOutputFormat::V04 | TraceExporterOutputFormat::V05
            ),
            TraceExporterInputFormat::V05 => matches!(output, TraceExporterOutputFormat::V05),
        }
    }
}

#[allow(missing_docs)]
pub trait ResponseCallback {
    #[allow(missing_docs)]
    fn call(&self, response: &str);
}

#[cfg(test)]
mod tests {
    use self::error::AgentErrorKind;
    use self::error::BuilderErrorKind;
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

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_new() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url("http://192.168.1.1:8127/")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_language_interpreter_vendor("node")
            .set_git_commit_sha("797e9ea")
            .set_input_format(TraceExporterInputFormat::Proxy)
            .set_output_format(TraceExporterOutputFormat::V04)
            .set_client_computed_stats()
            .enable_telemetry(Some(TelemetryConfig {
                heartbeat: 1000,
                runtime_id: None,
                debug_enabled: false,
            }));
        let exporter = builder.build().unwrap();

        assert_eq!(
            exporter
                .output_format
                .add_path(&exporter.endpoint.url)
                .to_string(),
            "http://192.168.1.1:8127/v0.4/traces"
        );
        assert_eq!(exporter.input_format, TraceExporterInputFormat::Proxy);
        assert_eq!(exporter.metadata.tracer_version, "v0.1");
        assert_eq!(exporter.metadata.language, "nodejs");
        assert_eq!(exporter.metadata.language_version, "1.0");
        assert_eq!(exporter.metadata.language_interpreter, "v8");
        assert_eq!(exporter.metadata.language_interpreter_vendor, "node");
        assert_eq!(exporter.metadata.git_commit_sha, "797e9ea");
        assert!(exporter.metadata.client_computed_stats);
        assert!(exporter.telemetry.is_some());
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_new_defaults() {
        let builder = TraceExporterBuilder::default();
        let exporter = builder.build().unwrap();

        assert_eq!(
            exporter
                .output_format
                .add_path(&exporter.endpoint.url)
                .to_string(),
            "http://127.0.0.1:8126/v0.4/traces"
        );
        assert_eq!(exporter.input_format, TraceExporterInputFormat::V04);
        assert_eq!(exporter.metadata.tracer_version, "");
        assert_eq!(exporter.metadata.language, "");
        assert_eq!(exporter.metadata.language_version, "");
        assert_eq!(exporter.metadata.language_interpreter, "");
        assert!(!exporter.metadata.client_computed_stats);
        assert!(exporter.telemetry.is_none());
    }

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
            exporter.runtime.block_on(async {
                sleep(Duration::from_millis(100)).await;
            })
        }

        let result = exporter.send(data.as_ref(), 1);
        // Error received because server is returning an empty body.
        assert!(result.is_err());

        exporter.shutdown(None).unwrap();

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
            exporter.runtime.block_on(async {
                sleep(Duration::from_millis(100)).await;
            })
        }

        exporter.send(data.as_ref(), 1).unwrap();

        exporter
            .shutdown(Some(Duration::from_millis(500)))
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
        enable_telemrty: bool,
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

        if enable_telemrty {
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
            AgentResponse::from(
                r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#
                .to_string()
            )
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn test_builder_error() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url("")
            .set_service("foo")
            .set_env("foo-env")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8");

        let exporter = builder.build();

        assert!(exporter.is_err());

        let err = match exporter {
            Err(TraceExporterError::Builder(e)) => Some(e),
            _ => None,
        };

        assert_eq!(
            err.unwrap(),
            BuilderErrorKind::InvalidUri("empty string".to_string())
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
        assert_eq!(result.body, response_body);

        traces_endpoint.assert_hits(1);
        while metrics_endpoint.hits() == 0 {
            exporter.runtime.block_on(async {
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
        assert_eq!(result.body, response_body);

        traces_endpoint.assert_hits(1);
        while metrics_endpoint.hits() == 0 {
            exporter.runtime.block_on(async {
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
        assert_eq!(result.body, response_body);

        traces_endpoint.assert_hits(1);
        while metrics_endpoint.hits() == 0 {
            exporter.runtime.block_on(async {
                sleep(Duration::from_millis(100)).await;
            })
        }
        metrics_endpoint.assert_hits(1);
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
            exporter.runtime.block_on(async {
                sleep(Duration::from_millis(100)).await;
            })
        }

        let _ = exporter.send(data.as_ref(), 1).unwrap();

        exporter.shutdown(None).unwrap();

        mock_traces.assert();
    }
}
