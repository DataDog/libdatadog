// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::agent_info::{AgentInfoArc, AgentInfoFetcher};
use crate::{
    health_metrics, health_metrics::HealthMetric, span_concentrator::SpanConcentrator,
    stats_exporter,
};
use arc_swap::{ArcSwap, ArcSwapOption};
use bytes::Bytes;
use datadog_trace_utils::trace_utils::{self, SendData, TracerHeaderTags};
use datadog_trace_utils::tracer_payload::TraceCollection;
use datadog_trace_utils::{msgpack_decoder, tracer_payload};
use ddcommon::tag::Tag;
use ddcommon::{connector, tag, Endpoint};
use dogstatsd_client::{new_flusher, Client, DogStatsDAction};
use either::Either;
use hyper::body::HttpBody;
use hyper::http::uri::PathAndQuery;
use hyper::{Body, Method, Uri};
use log::{error, info};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{borrow::Borrow, collections::HashMap, str::FromStr, time};
use tokio::{runtime::Runtime, task::JoinHandle};
use tokio_util::sync::CancellationToken;

const DEFAULT_STATS_ELIGIBLE_SPAN_KINDS: [&str; 4] = ["client", "server", "producer", "consumer"];
const STATS_ENDPOINT: &str = "/v0.6/stats";
const INFO_ENDPOINT: &str = "/info";

// Keys used for sampling
#[allow(dead_code)] // TODO (APMSP-1583) these will be used with client side stats
const SAMPLING_PRIORITY_KEY: &str = "_sampling_priority_v1";
#[allow(dead_code)] // TODO (APMSP-1584) these will be used with client side stats
const SAMPLING_SINGLE_SPAN_MECHANISM: &str = "_dd.span_sampling.mechanism";
#[allow(dead_code)] // TODO (APMSP-1584) these will be used with client side stats
const SAMPLING_ANALYTICS_RATE_KEY: &str = "_dd1.sr.eausr";

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
}

/// TraceExporterOutputFormat represents the format of the output traces.
/// The output format can be either V0.4 or v0.7, where V0.4 is the default.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[repr(C)]
pub enum TraceExporterOutputFormat {
    #[allow(missing_docs)]
    #[default]
    V04,
    #[allow(missing_docs)]
    V07,
}

impl TraceExporterOutputFormat {
    /// Add the agent trace endpoint path to the URL.
    fn add_path(&self, url: &Uri) -> Uri {
        add_path(
            url,
            match self {
                TraceExporterOutputFormat::V04 => "/v0.4/traces",
                TraceExporterOutputFormat::V07 => "/v0.7/traces",
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
    let new_p_and_q = match p_and_q {
        Some(pq) => {
            let mut p = pq.path().to_string();
            if p.ends_with('/') {
                p.pop();
            }
            p.push_str(path);
            PathAndQuery::from_str(p.as_str())
        }
        None => PathAndQuery::from_str(path),
    }
    .unwrap();
    let mut parts = url.clone().into_parts();
    parts.path_and_query = Some(new_p_and_q);
    Uri::from_parts(parts).unwrap()
}

/* TODO (APMSP-1583) re-enable client side stats
struct DroppedP0Counts {
    pub dropped_p0_traces: usize,
    pub dropped_p0_spans: usize,
}

Remove spans and chunks only keeping the ones that may be sampled by the agent
fn drop_chunks(traces: &mut Vec<Vec<pb::Span>>) -> DroppedP0Counts {
    let mut dropped_p0_traces = 0;
    let mut dropped_p0_spans = 0;
    traces.retain_mut(|chunk| {
        // List of spans to keep even if the chunk is dropped
        let mut sampled_indexes = Vec::new();
        for (index, span) in chunk.iter().enumerate() {
            // ErrorSampler
            if span.error == 1 {
                // We send chunks containing an error
                return true;
            }
            // PrioritySampler and NoPrioritySampler
            let priority = span.metrics.get(SAMPLING_PRIORITY_KEY);
            if has_top_level(span) && (priority.is_none() || priority.is_some_and(|p| *p > 0.0))
{                 // We send chunks with positive priority or no priority
                return true;
            }
            // SingleSpanSampler and AnalyzedSpansSampler
            else if span
                .metrics
                .get(SAMPLING_SINGLE_SPAN_MECHANISM)
                .is_some_and(|m| *m == 8.0)
                || span.metrics.contains_key(SAMPLING_ANALYTICS_RATE_KEY)
            {
                // We send spans sampled by single-span sampling or analyzed spans
                sampled_indexes.push(index);
            }
        }
        dropped_p0_spans += chunk.len() - sampled_indexes.len();
        if sampled_indexes.is_empty() {
            // If no spans were sampled we can drop the whole chunk
            dropped_p0_traces += 1;
            return false;
        }
        let sampled_spans = sampled_indexes
            .iter()
            .map(|i| std::mem::take(&mut chunk[*i]))
            .collect();
        *chunk = sampled_spans;
        true
    });
    DroppedP0Counts {
        dropped_p0_traces,
        dropped_p0_spans,
    }
}
 */

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
pub struct TraceExporter {
    endpoint: Endpoint,
    metadata: TracerMetadata,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    // TODO - do something with the response callback - https://datadoghq.atlassian.net/browse/APMSP-1019
    _response_callback: Option<Box<dyn ResponseCallback>>,
    runtime: Runtime,
    /// None if dogstatsd is disabled
    dogstatsd: Option<Client>,
    common_stats_tags: Vec<Tag>,
    #[allow(dead_code)]
    client_computed_top_level: bool,
    client_side_stats: ArcSwap<StatsComputationStatus>,
    agent_info: AgentInfoArc,
    previous_info_state: ArcSwapOption<String>,
}

impl TraceExporter {
    #[allow(missing_docs)]
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    /// Send msgpack serialized traces to the agent
    #[allow(missing_docs)]
    pub fn send(&self, data: &[u8], trace_count: usize) -> Result<String, String> {
        self.check_agent_info();
        match self.input_format {
            TraceExporterInputFormat::Proxy => self.send_proxy(data, trace_count),
            TraceExporterInputFormat::V04 => {
                self.send_deser_ser(tinybytes::Bytes::copy_from_slice(data))
                // TODO: APMSP-1582 - Refactor data-pipeline-ffi so we can leverage a type that
                // implements tinybytes::UnderlyingBytes trait to avoid copying
            }
        }
    }

    /// Safely shutdown the TraceExporter and all related tasks
    pub fn shutdown(self, timeout: Option<Duration>) -> Result<(), String> {
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
                })
                .await
            }) {
                Ok(()) => Ok(()),
                Err(_) => Err("Shutdown timed out".to_string()),
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

    fn send_proxy(&self, data: &[u8], trace_count: usize) -> Result<String, String> {
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
    ) -> Result<String, String> {
        self.runtime
            .block_on(async {
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
                let req = req_builder
                    .body(Body::from(Bytes::copy_from_slice(data)))
                    .unwrap();

                match hyper::Client::builder()
                    .build(connector::Connector::default())
                    .request(req)
                    .await
                {
                    Ok(response) => {
                        let response_status = response.status();
                        if !response_status.is_success() {
                            let body_bytes = response.into_body().collect().await?.to_bytes();
                            let response_body =
                                String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                            let resp_tag_res = &Tag::new("response_code", response_status.as_str());
                            match resp_tag_res {
                                Ok(resp_tag) => {
                                    self.emit_metric(
                                        HealthMetric::Count(
                                            health_metrics::STAT_SEND_TRACES_ERRORS,
                                            1,
                                        ),
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
                            anyhow::bail!("Agent did not accept traces: {response_body}");
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
                                anyhow::bail!("Error reading agent response body: {err}");
                            }
                        }
                    }
                    Err(err) => {
                        self.emit_metric(
                            HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                            None,
                        );
                        anyhow::bail!("Failed to send traces: {err}")
                    }
                }
            })
            .or_else(|err| {
                error!("Error sending traces: {err}");
                Ok(String::from("{}"))
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

    // /// Add all spans from the given iterator into the stats concentrator
    // /// # Panic
    // /// Will panic if another thread panicked will holding the lock on `stats_concentrator`
    // fn add_spans_to_stats<'a>(&self, spans: impl Iterator<Item = &'a pb::Span>) {
    //     if let StatsComputationStatus::Enabled {
    //         stats_concentrator,
    //         cancellation_token: _,
    //         exporter_handle: _,
    //     } = &**self.client_side_stats.load()
    //     {
    //         let mut stats_concentrator = stats_concentrator.lock().unwrap();
    //         for span in spans {
    //             stats_concentrator.add_span(span);
    //         }
    //     }
    // }

    fn send_deser_ser(&self, data: tinybytes::Bytes) -> Result<String, String> {
        // let size = data.len();
        // TODO base on input format
        let (traces, size) = match msgpack_decoder::v04::decoder::from_slice(data) {
            Ok(res) => res,
            Err(err) => {
                error!("Error deserializing trace from request body: {err}");
                self.emit_metric(
                    HealthMetric::Count(health_metrics::STAT_DESER_TRACES_ERRORS, 1),
                    None,
                );
                return Ok(String::from("{}"));
            }
        };

        if traces.is_empty() {
            error!("No traces deserialized from the request body.");
            return Ok(String::from("{}"));
        }

        let num_traces = traces.len();

        self.emit_metric(
            HealthMetric::Count(health_metrics::STAT_DESER_TRACES, traces.len() as i64),
            None,
        );

        let header_tags: TracerHeaderTags = self.metadata.borrow().into();

        // Stats computation
        // if let StatsComputationStatus::Enabled { .. } = &**self.client_side_stats.load() {
        //     if !self.client_computed_top_level {
        //         for chunk in traces.iter_mut() {
        //             compute_top_level_span(chunk);
        //         }
        //     }
        //     self.add_spans_to_stats(traces.iter().flat_map(|trace| trace.iter()));
        //     // Once stats have been computed we can drop all chunks that are not going to be
        //     // sampled by the agent
        //     let dropped_counts = drop_chunks(&mut traces);
        //     header_tags.client_computed_top_level = true;
        //     header_tags.client_computed_stats = true;
        //     header_tags.dropped_p0_traces = dropped_counts.dropped_p0_traces;
        //     header_tags.dropped_p0_spans = dropped_counts.dropped_p0_spans;
        // }

        match self.output_format {
            TraceExporterOutputFormat::V04 => {
                let tracer_payload = trace_utils::collect_trace_chunks(
                    TraceCollection::V04(traces),
                    &header_tags,
                    &mut tracer_payload::DefaultTraceChunkProcessor,
                    self.endpoint.api_key.is_some(),
                );
                let endpoint = Endpoint {
                    url: self.output_format.add_path(&self.endpoint.url),
                    ..self.endpoint.clone()
                };
                let send_data = SendData::new(size, tracer_payload, header_tags, &endpoint);
                self.runtime.block_on(async {
                    let send_data_result = send_data.send().await;
                    match send_data_result.last_result {
                        Ok(response) => {
                            self.emit_metric(
                                HealthMetric::Count(
                                    health_metrics::STAT_SEND_TRACES,
                                    num_traces as i64,
                                ),
                                None,
                            );
                            match response.into_body().collect().await {
                                Ok(body) => {
                                    Ok(String::from_utf8_lossy(&body.to_bytes()).to_string())
                                }
                                Err(err) => {
                                    error!("Error reading agent response body: {err}");
                                    self.emit_metric(
                                        HealthMetric::Count(
                                            health_metrics::STAT_SEND_TRACES_ERRORS,
                                            1,
                                        ),
                                        None,
                                    );
                                    Ok(String::from("{}"))
                                }
                            }
                        }
                        Err(err) => {
                            error!("Error sending traces: {err}");
                            self.emit_metric(
                                HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                                None,
                            );
                            Ok(String::from("{}"))
                        }
                    }
                })
            }

            TraceExporterOutputFormat::V07 => todo!("We don't support translating to v07 yet"),
        }
    }
}

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8126";

#[allow(missing_docs)]
#[derive(Default)]
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
    response_callback: Option<Box<dyn ResponseCallback>>,
    dogstatsd_url: Option<String>,
    client_computed_stats: bool,
    client_computed_top_level: bool,

    // Stats specific fields
    /// A Some value enables stats-computation, None if it is disabled
    stats_bucket_size: Option<Duration>,
    peer_tags_aggregation: bool,
    compute_stats_by_span_kind: bool,
    peer_tags: Vec<String>,
}

impl TraceExporterBuilder {
    /// Set url of the agent
    pub fn set_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_owned());
        self
    }

    /// Set the URL to communicate with a dogstatsd server
    pub fn set_dogstatsd_url(mut self, url: &str) -> Self {
        self.dogstatsd_url = Some(url.to_owned());
        self
    }

    /// Set the hostname used for stats payload
    /// Only used when client-side stats is enabled
    pub fn set_hostname(mut self, hostname: &str) -> Self {
        hostname.clone_into(&mut self.hostname);
        self
    }

    /// Set the env used for stats payloads
    /// Only used when client-side stats is enabled
    pub fn set_env(mut self, env: &str) -> Self {
        env.clone_into(&mut self.env);
        self
    }

    /// Set the app version which corresponds to the `version` meta tag
    /// Only used when client-side stats is enabled
    pub fn set_app_version(mut self, app_version: &str) -> Self {
        app_version.clone_into(&mut self.app_version);
        self
    }

    /// Set the service name used for stats payloads.
    /// Only used when client-side stats is enabled
    pub fn set_service(mut self, service: &str) -> Self {
        service.clone_into(&mut self.service);
        self
    }

    /// Set the `git_commit_sha` corresponding to the `_dd.git.commit.sha` meta tag
    /// Only used when client-side stats is enabled
    pub fn set_git_commit_sha(mut self, git_commit_sha: &str) -> Self {
        git_commit_sha.clone_into(&mut self.git_commit_sha);
        self
    }

    /// Set the `Datadog-Meta-Tracer-Version` header
    pub fn set_tracer_version(mut self, tracer_version: &str) -> Self {
        tracer_version.clone_into(&mut self.tracer_version);
        self
    }

    /// Set the `Datadog-Meta-Lang` header
    pub fn set_language(mut self, lang: &str) -> Self {
        lang.clone_into(&mut self.language);
        self
    }

    /// Set the `Datadog-Meta-Lang-Version` header
    pub fn set_language_version(mut self, lang_version: &str) -> Self {
        lang_version.clone_into(&mut self.language_version);
        self
    }

    /// Set the `Datadog-Meta-Lang-Interpreter` header
    pub fn set_language_interpreter(mut self, lang_interpreter: &str) -> Self {
        lang_interpreter.clone_into(&mut self.language_interpreter);
        self
    }

    /// Set the `Datadog-Meta-Lang-Interpreter-Vendor` header
    pub fn set_language_interpreter_vendor(mut self, lang_interpreter_vendor: &str) -> Self {
        lang_interpreter_vendor.clone_into(&mut self.language_interpreter_vendor);
        self
    }

    #[allow(missing_docs)]
    pub fn set_input_format(mut self, input_format: TraceExporterInputFormat) -> Self {
        self.input_format = input_format;
        self
    }

    #[allow(missing_docs)]
    pub fn set_output_format(mut self, output_format: TraceExporterOutputFormat) -> Self {
        self.output_format = output_format;
        self
    }

    #[allow(missing_docs)]
    pub fn set_response_callback(mut self, response_callback: Box<dyn ResponseCallback>) -> Self {
        self.response_callback = Some(response_callback);
        self
    }

    /// Set the header indicating the tracer has computed the top-level tag
    pub fn set_client_computed_top_level(mut self) -> Self {
        self.client_computed_top_level = true;
        self
    }

    /// Set the header indicating the tracer has already computed stats.
    /// This should not be used when stats computation is enabled.
    pub fn set_client_computed_stats(mut self) -> Self {
        self.client_computed_stats = true;
        self
    }

    /// Enable stats computation on traces sent through this exporter
    pub fn enable_stats(mut self, bucket_size: Duration) -> Self {
        self.stats_bucket_size = Some(bucket_size);
        self
    }

    /// Enable peer tags aggregation for stats computation (requires stats computation to be
    /// enabled)
    pub fn enable_stats_peer_tags_aggregation(mut self, peer_tags: Vec<String>) -> Self {
        self.peer_tags_aggregation = true;
        self.peer_tags = peer_tags;
        self
    }

    /// Enable stats eligibility by span kind (requires stats computation to be
    /// enabled)
    pub fn enable_compute_stats_by_span_kind(mut self) -> Self {
        self.compute_stats_by_span_kind = true;
        self
    }

    #[allow(missing_docs)]
    pub fn build(self) -> anyhow::Result<TraceExporter> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let dogstatsd = self.dogstatsd_url.and_then(|u| {
            new_flusher(Endpoint::from_slice(&u)).ok() // If we couldn't set the endpoint return
                                                       // None
        });

        let agent_url: hyper::Uri = self.url.as_deref().unwrap_or(DEFAULT_AGENT_URL).parse()?;

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

        Ok(TraceExporter {
            endpoint: Endpoint::from_url(agent_url),
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
            _response_callback: self.response_callback,
            client_computed_top_level: self.client_computed_top_level,
            runtime,
            dogstatsd,
            common_stats_tags: vec![libdatadog_version],
            client_side_stats: ArcSwap::new(stats.into()),
            agent_info,
            previous_info_state: ArcSwapOption::new(None),
        })
    }
}

#[allow(missing_docs)]
pub trait ResponseCallback {
    #[allow(missing_docs)]
    fn call(&self, response: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use datadog_trace_utils::span_v04::Span;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    // use serde::Serialize;
    use std::collections::HashMap;
    use std::net;
    use std::time::Duration;
    use tinybytes::BytesString;
    use tokio::time::sleep;

    #[test]
    fn new() {
        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_url("http://192.168.1.1:8127/")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_language_interpreter_vendor("node")
            .set_git_commit_sha("797e9ea")
            .set_input_format(TraceExporterInputFormat::Proxy)
            .set_output_format(TraceExporterOutputFormat::V07)
            .build()
            .unwrap();

        assert_eq!(
            exporter
                .output_format
                .add_path(&exporter.endpoint.url)
                .to_string(),
            "http://192.168.1.1:8127/v0.7/traces"
        );
        assert_eq!(exporter.input_format, TraceExporterInputFormat::Proxy);
        assert_eq!(exporter.metadata.tracer_version, "v0.1");
        assert_eq!(exporter.metadata.language, "nodejs");
        assert_eq!(exporter.metadata.language_version, "1.0");
        assert_eq!(exporter.metadata.language_interpreter, "v8");
        assert_eq!(exporter.metadata.language_interpreter_vendor, "node");
        assert_eq!(exporter.metadata.git_commit_sha, "797e9ea");
        assert!(!exporter.metadata.client_computed_stats);
    }

    #[test]
    fn new_defaults() {
        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_client_computed_stats()
            .build()
            .unwrap();

        assert_eq!(
            exporter
                .output_format
                .add_path(&exporter.endpoint.url)
                .to_string(),
            "http://127.0.0.1:8126/v0.4/traces"
        );
        assert_eq!(exporter.input_format, TraceExporterInputFormat::V04);
        assert_eq!(exporter.metadata.tracer_version, "v0.1");
        assert_eq!(exporter.metadata.language, "nodejs");
        assert_eq!(exporter.metadata.language_version, "1.0");
        assert_eq!(exporter.metadata.language_interpreter, "v8");
        assert!(exporter.metadata.client_computed_stats);
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
    //
    // #[test]
    // fn test_drop_chunks() {
    //     let chunk_with_priority = vec![
    //         pb::Span {
    //             span_id: 1,
    //             metrics: HashMap::from([
    //                 (SAMPLING_PRIORITY_KEY.to_string(), 1.0),
    //                 ("_dd.top_level".to_string(), 1.0),
    //             ]),
    //             ..Default::default()
    //         },
    //         pb::Span {
    //             span_id: 2,
    //             parent_id: 1,
    //             ..Default::default()
    //         },
    //     ];
    //     let chunk_with_null_priority = vec![
    //         pb::Span {
    //             span_id: 1,
    //             metrics: HashMap::from([
    //                 (SAMPLING_PRIORITY_KEY.to_string(), 0.0),
    //                 ("_dd.top_level".to_string(), 1.0),
    //             ]),
    //             ..Default::default()
    //         },
    //         pb::Span {
    //             span_id: 2,
    //             parent_id: 1,
    //             ..Default::default()
    //         },
    //     ];
    //     let chunk_without_priority = vec![
    //         pb::Span {
    //             span_id: 1,
    //             metrics: HashMap::from([("_dd.top_level".to_string(), 1.0)]),
    //             ..Default::default()
    //         },
    //         pb::Span {
    //             span_id: 2,
    //             parent_id: 1,
    //             ..Default::default()
    //         },
    //     ];
    //     let chunk_with_error = vec![
    //         pb::Span {
    //             span_id: 1,
    //             error: 1,
    //             metrics: HashMap::from([
    //                 (SAMPLING_PRIORITY_KEY.to_string(), 0.0),
    //                 ("_dd.top_level".to_string(), 1.0),
    //             ]),
    //             ..Default::default()
    //         },
    //         pb::Span {
    //             span_id: 2,
    //             parent_id: 1,
    //             ..Default::default()
    //         },
    //     ];
    //     let chunk_with_a_single_span = vec![
    //         pb::Span {
    //             span_id: 1,
    //             metrics: HashMap::from([
    //                 (SAMPLING_PRIORITY_KEY.to_string(), 0.0),
    //                 ("_dd.top_level".to_string(), 1.0),
    //             ]),
    //             ..Default::default()
    //         },
    //         pb::Span {
    //             span_id: 2,
    //             parent_id: 1,
    //             metrics: HashMap::from([(SAMPLING_SINGLE_SPAN_MECHANISM.to_string(), 8.0)]),
    //             ..Default::default()
    //         },
    //     ];
    //     let chunk_with_analyzed_span = vec![
    //         pb::Span {
    //             span_id: 1,
    //             metrics: HashMap::from([
    //                 (SAMPLING_PRIORITY_KEY.to_string(), 0.0),
    //                 ("_dd.top_level".to_string(), 1.0),
    //             ]),
    //             ..Default::default()
    //         },
    //         pb::Span {
    //             span_id: 2,
    //             parent_id: 1,
    //             metrics: HashMap::from([(SAMPLING_ANALYTICS_RATE_KEY.to_string(), 1.0)]),
    //             ..Default::default()
    //         },
    //     ];
    //
    //     let chunks_and_expected_sampled_spans = vec![
    //         (chunk_with_priority, 2),
    //         (chunk_with_null_priority, 0),
    //         (chunk_without_priority, 2),
    //         (chunk_with_error, 2),
    //         (chunk_with_a_single_span, 1),
    //         (chunk_with_analyzed_span, 1),
    //     ];
    //
    //     for (chunk, expected_count) in chunks_and_expected_sampled_spans.into_iter() {
    //         let mut traces = vec![chunk];
    //         drop_chunks(&mut traces);
    //         if expected_count == 0 {
    //             assert!(traces.is_empty());
    //         } else {
    //             assert_eq!(traces[0].len(), expected_count);
    //         }
    //     }
    // }

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

        // let mock_stats = server.mock(|when, then| {
        //     when.method(POST)
        //         .header("Content-type", "application/msgpack")
        //         .path("/v0.6/stats");
        //     then.status(200).body("");
        // });

        let mock_info = server.mock(|when, then| {
            when.method(GET).path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(r#"{"version":"1","client_drop_p0s":true}"#);
        });

        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_url(&server.url("/"))
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .enable_stats(Duration::from_secs(10))
            .build()
            .unwrap();

        let trace_chunk = vec![Span {
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

        exporter.send(data.as_slice(), 1).unwrap();
        exporter.shutdown(None).unwrap();

        mock_traces.assert();
        //mock_stats.assert();
    }

    /* TODO (APMSP-1583) Re-enable with client stats
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_shutdown_with_timeout() {
        let server = MockServer::start();

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.4/traces");
            then.status(200).body("");
        });

        // let _mock_stats = server.mock(|when, then| {
        //     when.method(POST)
        //         .header("Content-type", "application/msgpack")
        //         .path("/v0.6/stats");
        //     then.delay(Duration::from_secs(10)).status(200).body("");
        // });

        let mock_info = server.mock(|when, then| {
            when.method(GET).path("/info");
            then.status(200)
                .header("content-type", "application/json")
                .header("datadog-agent-state", "1")
                .body(r#"{"version":"1","client_drop_p0s":true}"#);
        });

        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_url(&server.url("/"))
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .enable_stats(Duration::from_secs(10))
            .build()
            .unwrap();

        let trace_chunk = vec![Span {
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

        exporter.send(data.as_slice(), 1).unwrap();
        exporter
            .shutdown(Some(Duration::from_millis(500)))
            .unwrap_err(); // The shutdown should timeout

        mock_traces.assert();
    }
     */

    fn read(socket: &net::UdpSocket) -> String {
        let mut buf = [0; 1_000];
        socket.recv(&mut buf).expect("No data");
        let datagram = String::from_utf8_lossy(buf.as_ref());
        datagram.trim_matches(char::from(0)).to_string()
    }

    fn build_test_exporter(url: String, dogstatsd_url: String) -> TraceExporter {
        TraceExporterBuilder::default()
            .set_url(&url)
            .set_dogstatsd_url(&dogstatsd_url)
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .build()
            .unwrap()
    }

    #[test]
    #[cfg_attr(miri, ignore)]
    fn health_metrics() {
        let stats_socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = stats_socket.set_read_timeout(Some(Duration::from_millis(500)));

        let fake_agent = MockServer::start();
        let _mock_traces = fake_agent.mock(|_, then| {
            then.status(200)
                .header("content-type", "application/json")
                .body("{}");
        });

        let exporter = build_test_exporter(
            fake_agent.url("/v0.4/traces"),
            stats_socket.local_addr().unwrap().to_string(),
        );

        let traces: Vec<Vec<Span>> = vec![
            vec![Span {
                name: BytesString::from_slice(b"test").unwrap(),
                ..Default::default()
            }],
            vec![Span {
                name: BytesString::from_slice(b"test2").unwrap(),
                ..Default::default()
            }],
        ];
        let bytes = rmp_serde::to_vec_named(&traces).expect("failed to serialize static trace");
        let _result = exporter.send(&bytes, 1).expect("failed to send trace");

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
    fn invalid_traces() {
        let stats_socket = net::UdpSocket::bind("127.0.0.1:0").expect("failed to bind host socket");
        let _ = stats_socket.set_read_timeout(Some(Duration::from_millis(500)));

        let fake_agent = MockServer::start();

        let exporter = build_test_exporter(
            fake_agent.url("/v0.4/traces"),
            stats_socket.local_addr().unwrap().to_string(),
        );

        let _result = exporter
            .send(b"some_bad_payload", 1)
            .expect("failed to send trace");

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
    fn health_metrics_error() {
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
            stats_socket.local_addr().unwrap().to_string(),
        );

        let traces: Vec<Vec<Span>> = vec![vec![Span {
            name: BytesString::from_slice(b"test").unwrap(),
            ..Default::default()
        }]];
        let bytes = rmp_serde::to_vec_named(&traces).expect("failed to serialize static trace");
        let _result = exporter.send(&bytes, 1).expect("failed to send trace");

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
}
