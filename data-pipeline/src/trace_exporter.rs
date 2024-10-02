// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::{
    health_metrics, health_metrics::HealthMetric, span_concentrator::SpanConcentrator,
    stats_exporter,
};
use bytes::Bytes;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils::{
    self, compute_top_level_span, has_top_level, SendData, TracerHeaderTags,
};
use datadog_trace_utils::tracer_payload;
use datadog_trace_utils::tracer_payload::TraceCollection;
use ddcommon::tag::Tag;
use ddcommon::{connector, tag, Endpoint};
use dogstatsd_client::{new_flusher, Client, DogStatsDAction};
use either::Either;
use hyper::http::uri::PathAndQuery;
use hyper::{Body, Method, Uri};
use log::error;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{borrow::Borrow, collections::HashMap, str::FromStr, time};
use tokio::{runtime::Runtime, task::JoinHandle};
use tokio_util::sync::CancellationToken;

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

/// Remove spans and chunks only keeping the ones that may be sampled by the agent
fn drop_chunks(traces: &mut Vec<Vec<pb::Span>>) {
    traces.retain_mut(|chunk| {
        let mut sampled_indexes = Vec::new();
        for (index, span) in chunk.iter().enumerate() {
            if span.error == 1 {
                // We send chunks containing an error
                return true;
            }
            let priority = span.metrics.get("_sampling_priority_v1");
            if priority.is_some_and(|p| *p > 0.0) {
                if has_top_level(span) {
                    // We send chunks with positive priority
                    return true;
                }
                // We send single spans with positive priority
                sampled_indexes.push(index);
            } else if priority.is_none() && has_top_level(span) {
                // We send chunks with no priority
                return true;
            } else if span.metrics.contains_key("_dd.sr.eausr") {
                // We send analyzed spans
                sampled_indexes.push(index);
            }
        }
        if sampled_indexes.is_empty() {
            // If no spans were sampled we can drop the whole chunk
            return false;
        }
        let sampled_spans = sampled_indexes
            .iter()
            .map(|i| std::mem::take(&mut chunk[*i]))
            .collect();
        *chunk = sampled_spans;
        true
    })
}

struct TracerTags {
    tracer_version: String,
    language: String,
    language_version: String,
    language_interpreter: String,
    client_computed_stats: bool,
    client_computed_top_level: bool,
}

impl<'a> From<&'a TracerTags> for TracerHeaderTags<'a> {
    fn from(tags: &'a TracerTags) -> TracerHeaderTags<'a> {
        TracerHeaderTags::<'_> {
            lang: &tags.language,
            lang_version: &tags.language_version,
            tracer_version: &tags.tracer_version,
            lang_interpreter: &tags.language_interpreter,
            client_computed_stats: tags.client_computed_stats,
            client_computed_top_level: tags.client_computed_top_level,
            ..Default::default()
        }
    }
}

impl<'a> From<&'a TracerTags> for HashMap<&'static str, String> {
    fn from(tags: &'a TracerTags) -> HashMap<&'static str, String> {
        TracerHeaderTags::from(tags).into()
    }
}

enum StatsComputationStatus {
    StatsDisabled,
    StatsEnabled {
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
    tags: TracerTags,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    // TODO - do something with the response callback - https://datadoghq.atlassian.net/browse/APMSP-1019
    _response_callback: Option<Box<dyn ResponseCallback>>,
    runtime: Runtime,
    /// None if dogstatsd is disabled
    dogstatsd: Option<Client>,
    common_stats_tags: Vec<Tag>,
    client_computed_top_level: bool,
    stats: StatsComputationStatus,
}

impl TraceExporter {
    #[allow(missing_docs)]
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    /// Send msgpack serialized traces to the agent
    #[allow(missing_docs)]
    pub fn send(&self, data: &[u8], trace_count: usize) -> Result<String, String> {
        match self.input_format {
            TraceExporterInputFormat::Proxy => self.send_proxy(data, trace_count),
            TraceExporterInputFormat::V04 => self.send_deser_ser(data),
        }
    }

    /// Safely shutdown the TraceExporter and all related tasks
    pub fn shutdown(self, timeout: Option<Duration>) -> Result<(), String> {
        match self.stats {
            StatsComputationStatus::StatsEnabled {
                stats_concentrator: _,
                cancellation_token: cancelation_token,
                exporter_handle,
            } => {
                if let Some(timeout) = timeout {
                    match self.runtime.block_on(async {
                        tokio::time::timeout(timeout, async {
                            cancelation_token.cancel();
                            let _ = exporter_handle.await;
                        })
                        .await
                    }) {
                        Ok(_) => Ok(()),
                        Err(_) => Err("Shutdown timed out".to_string()),
                    }
                } else {
                    self.runtime.block_on(async {
                        cancelation_token.cancel();
                        let _ = exporter_handle.await;
                    });
                    Ok(())
                }
            }
            StatsComputationStatus::StatsDisabled => Ok(()),
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

                let headers: HashMap<&'static str, String> = self.tags.borrow().into();

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
                            let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
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
                        match hyper::body::to_bytes(response.into_body()).await {
                            Ok(body) => {
                                self.emit_metric(
                                    HealthMetric::Count(
                                        health_metrics::STAT_SEND_TRACES,
                                        trace_count as i64,
                                    ),
                                    None,
                                );
                                Ok(String::from_utf8_lossy(&body).to_string())
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

    /// Add all spans from the given iterator into the stats concentrator
    /// # Panic
    /// Will panic if another thread panicked will holding the lock on `stats_concentrator`
    fn add_spans_to_stats<'a>(&self, spans: impl Iterator<Item = &'a pb::Span>) {
        if let StatsComputationStatus::StatsEnabled {
            stats_concentrator,
            cancellation_token: _,
            exporter_handle: _,
        } = &self.stats
        {
            let mut stats_concentrator = stats_concentrator.lock().unwrap();
            for span in spans {
                stats_concentrator.add_span(span);
            }
        }
    }

    fn send_deser_ser(&self, data: &[u8]) -> Result<String, String> {
        let size = data.len();
        // TODO base on input format
        let mut traces: Vec<Vec<pb::Span>> = match rmp_serde::from_slice(data) {
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

        self.emit_metric(
            HealthMetric::Count(health_metrics::STAT_DESER_TRACES, traces.len() as i64),
            None,
        );

        // Stats computation
        if let StatsComputationStatus::StatsEnabled { .. } = &self.stats {
            if !self.client_computed_top_level {
                for chunk in traces.iter_mut() {
                    compute_top_level_span(chunk);
                }
            }
            self.add_spans_to_stats(traces.iter().flat_map(|trace| trace.iter()));
            // Once stats have been computed we can drop all chunks that are not going to be
            // sampled by the agent
            drop_chunks(&mut traces);
        }

        let header_tags: TracerHeaderTags<'_> = (&self.tags).into();

        match self.output_format {
            TraceExporterOutputFormat::V04 => rmp_serde::to_vec_named(&traces)
                .map_err(|err| {
                    error!("Error serializing traces: {err}");
                    self.emit_metric(
                        HealthMetric::Count(health_metrics::STAT_SER_TRACES_ERRORS, 1),
                        None,
                    );
                    String::from("{}")
                })
                .and_then(|res| {
                    self.send_data_to_url(
                        &res,
                        traces.len(),
                        self.output_format.add_path(&self.endpoint.url),
                    )
                }),

            TraceExporterOutputFormat::V07 => {
                let tracer_payload = trace_utils::collect_trace_chunks(
                    TraceCollection::V07(traces),
                    &header_tags,
                    &mut tracer_payload::DefaultTraceChunkProcessor,
                    self.endpoint.api_key.is_some(),
                );

                let endpoint = Endpoint {
                    url: self.output_format.add_path(&self.endpoint.url),
                    ..self.endpoint.clone()
                };
                let send_data = SendData::new(size, tracer_payload, header_tags, &endpoint, None);
                self.runtime.block_on(async {
                    match send_data.send().await.last_result {
                        Ok(response) => match hyper::body::to_bytes(response.into_body()).await {
                            Ok(body) => Ok(String::from_utf8_lossy(&body).to_string()),
                            Err(err) => {
                                error!("Error reading agent response body: {err}");
                                self.emit_metric(
                                    HealthMetric::Count(health_metrics::STAT_SEND_TRACES_ERRORS, 1),
                                    None,
                                );
                                Ok(String::from("{}"))
                            }
                        },
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
        }
    }
}

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8126";

#[allow(missing_docs)]
#[derive(Default)]
pub struct TraceExporterBuilder {
    url: Option<String>,
    tracer_version: String,
    hostname: String,
    env: String,
    version: String,
    service: String,
    language: String,
    language_version: String,
    language_interpreter: String,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    response_callback: Option<Box<dyn ResponseCallback>>,
    dogstatsd_url: Option<String>,
    client_computed_stats: bool,
    client_computed_top_level: bool,

    // Stats specific fields
    /// A Some value enables stats-computation, None if it is disabled
    stats_bucket_size: Option<time::Duration>,
    peer_tags_aggregation: bool,
    compute_stats_by_span_kind: bool,
    peer_tags: Vec<String>,
}

impl TraceExporterBuilder {
    #[allow(missing_docs)]
    pub fn set_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_owned());
        self
    }

    /// Set the URL to communicate with a dogstatsd server
    pub fn set_dogstatsd_url(mut self, url: &str) -> Self {
        self.dogstatsd_url = Some(url.to_owned());
        self
    }

    pub fn set_hostname(mut self, hostname: &str) -> Self {
        hostname.clone_into(&mut self.hostname);
        self
    }

    pub fn set_env(mut self, env: &str) -> Self {
        env.clone_into(&mut self.env);
        self
    }

    pub fn set_version(mut self, version: &str) -> Self {
        version.clone_into(&mut self.version);
        self
    }

    pub fn set_service(mut self, service: &str) -> Self {
        service.clone_into(&mut self.service);
        self
    }

    #[allow(missing_docs)]
    pub fn set_tracer_version(mut self, tracer_version: &str) -> Self {
        tracer_version.clone_into(&mut self.tracer_version);
        self
    }

    #[allow(missing_docs)]
    pub fn set_language(mut self, lang: &str) -> Self {
        lang.clone_into(&mut self.language);
        self
    }

    #[allow(missing_docs)]
    pub fn set_language_version(mut self, lang_version: &str) -> Self {
        lang_version.clone_into(&mut self.language_version);
        self
    }

    #[allow(missing_docs)]
    pub fn set_language_interpreter(mut self, lang_interpreter: &str) -> Self {
        lang_interpreter.clone_into(&mut self.language_interpreter);
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
    pub fn enable_stats(mut self, bucket_size: time::Duration) -> Self {
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

        let libdatadog_version = tag!("libdatadog_version", env!("CARGO_PKG_VERSION"));
        let mut stats = StatsComputationStatus::StatsDisabled;

        // Proxy mode does not support stats
        if self.input_format != TraceExporterInputFormat::Proxy {
            if let Some(bucket_size) = self.stats_bucket_size {
                let stats_concentrator = Arc::new(Mutex::new(SpanConcentrator::new(
                    bucket_size,
                    time::SystemTime::now(),
                    self.peer_tags_aggregation,
                    self.compute_stats_by_span_kind,
                    self.peer_tags,
                )));

                let cancellation_token = CancellationToken::new();

                let mut stats_exporter = stats_exporter::StatsExporter::new(
                    self.stats_bucket_size.unwrap(),
                    stats_concentrator.clone(),
                    stats_exporter::LibraryMetadata {
                        hostname: self.hostname,
                        env: self.env,
                        version: self.version,
                        lang: self.language.clone(),
                        tracer_version: self.tracer_version.clone(),
                        runtime_id: uuid::Uuid::new_v4().to_string(),
                        service: self.service,
                        ..Default::default()
                    },
                    Endpoint::from_url(stats_exporter::stats_url_from_agent_url(
                        self.url.as_deref().unwrap_or(DEFAULT_AGENT_URL),
                    )?),
                    cancellation_token.clone(),
                );

                let exporter_handle = runtime.spawn(async move {
                    stats_exporter.run().await;
                });

                stats = StatsComputationStatus::StatsEnabled {
                    stats_concentrator,
                    cancellation_token,
                    exporter_handle,
                }
            }
        }

        Ok(TraceExporter {
            endpoint: Endpoint::from_slice(self.url.as_deref().unwrap_or(DEFAULT_AGENT_URL)),
            tags: TracerTags {
                tracer_version: self.tracer_version,
                language_version: self.language_version,
                language_interpreter: self.language_interpreter,
                language: self.language,
                client_computed_stats: self.client_computed_stats
                    || self.stats_bucket_size.is_some(),
                client_computed_top_level: self.client_computed_top_level
                    || self.stats_bucket_size.is_some(), /* Client side stats enforce client
                                                          * computed top level */
            },
            input_format: self.input_format,
            output_format: self.output_format,
            _response_callback: self.response_callback,
            client_computed_top_level: self.client_computed_top_level,
            runtime,
            dogstatsd,
            common_stats_tags: vec![libdatadog_version],
            stats,
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
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use std::collections::HashMap;
    use std::net;
    use std::time::Duration;

    #[test]
    fn new() {
        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_url("http://192.168.1.1:8127/")
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
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
        assert_eq!(exporter.tags.tracer_version, "v0.1");
        assert_eq!(exporter.tags.language, "nodejs");
        assert_eq!(exporter.tags.language_version, "1.0");
        assert_eq!(exporter.tags.language_interpreter, "v8");
        assert!(!exporter.tags.client_computed_stats);
    }

    #[test]
    fn new_defaults() {
        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .enable_stats(Duration::from_secs(10))
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
        assert_eq!(exporter.tags.tracer_version, "v0.1");
        assert_eq!(exporter.tags.language, "nodejs");
        assert_eq!(exporter.tags.language_version, "1.0");
        assert_eq!(exporter.tags.language_interpreter, "v8");
        assert!(exporter.tags.client_computed_stats);
    }

    #[test]
    fn test_from_tracer_tags_to_tracer_header_tags() {
        let tracer_tags = TracerTags {
            tracer_version: "v0.1".to_string(),
            language: "rust".to_string(),
            language_version: "1.52.1".to_string(),
            language_interpreter: "rustc".to_string(),
            client_computed_stats: true,
            client_computed_top_level: true,
        };

        let tracer_header_tags: TracerHeaderTags = (&tracer_tags).into();

        assert_eq!(tracer_header_tags.tracer_version, "v0.1");
        assert_eq!(tracer_header_tags.lang, "rust");
        assert_eq!(tracer_header_tags.lang_version, "1.52.1");
        assert_eq!(tracer_header_tags.lang_interpreter, "rustc");
        assert!(tracer_header_tags.client_computed_stats);
        assert!(tracer_header_tags.client_computed_top_level);
    }

    #[test]
    fn test_from_tracer_tags_to_hashmap() {
        let tracer_tags = TracerTags {
            tracer_version: "v0.1".to_string(),
            language: "rust".to_string(),
            language_version: "1.52.1".to_string(),
            language_interpreter: "rustc".to_string(),
            client_computed_stats: true,
            client_computed_top_level: true,
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

    #[test]
    fn test_drop_chunks() {
        let chunk_with_priority = vec![
            pb::Span {
                span_id: 1,
                metrics: HashMap::from([
                    ("_sampling_priority_v1".to_string(), 1.0),
                    ("_dd.top_level".to_string(), 1.0),
                ]),
                ..Default::default()
            },
            pb::Span {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_null_priority = vec![
            pb::Span {
                span_id: 1,
                metrics: HashMap::from([
                    ("_sampling_priority_v1".to_string(), 0.0),
                    ("_dd.top_level".to_string(), 1.0),
                ]),
                ..Default::default()
            },
            pb::Span {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_without_priority = vec![
            pb::Span {
                span_id: 1,
                metrics: HashMap::from([("_dd.top_level".to_string(), 1.0)]),
                ..Default::default()
            },
            pb::Span {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_error = vec![
            pb::Span {
                span_id: 1,
                error: 1,
                metrics: HashMap::from([
                    ("_sampling_priority_v1".to_string(), 0.0),
                    ("_dd.top_level".to_string(), 1.0),
                ]),
                ..Default::default()
            },
            pb::Span {
                span_id: 2,
                parent_id: 1,
                ..Default::default()
            },
        ];
        let chunk_with_a_single_span = vec![
            pb::Span {
                span_id: 1,
                metrics: HashMap::from([
                    ("_sampling_priority_v1".to_string(), 0.0),
                    ("_dd.top_level".to_string(), 1.0),
                ]),
                ..Default::default()
            },
            pb::Span {
                span_id: 2,
                parent_id: 1,
                metrics: HashMap::from([("_sampling_priority_v1".to_string(), 1.0)]),
                ..Default::default()
            },
        ];
        let chunk_with_analyzed_span = vec![
            pb::Span {
                span_id: 1,
                metrics: HashMap::from([
                    ("_sampling_priority_v1".to_string(), 0.0),
                    ("_dd.top_level".to_string(), 1.0),
                ]),
                ..Default::default()
            },
            pb::Span {
                span_id: 2,
                parent_id: 1,
                metrics: HashMap::from([("_dd.sr.eausr".to_string(), 1.0)]),
                ..Default::default()
            },
        ];

        let chunks_and_expected_sampled_spans = vec![
            (chunk_with_priority, 2),
            (chunk_with_null_priority, 0),
            (chunk_without_priority, 2),
            (chunk_with_error, 2),
            (chunk_with_a_single_span, 1),
            (chunk_with_analyzed_span, 1),
        ];

        for (chunk, expected_count) in chunks_and_expected_sampled_spans.into_iter() {
            let mut traces = vec![chunk];
            drop_chunks(&mut traces);
            if expected_count == 0 {
                assert!(traces.is_empty());
            } else {
                println!("{:?}", traces[0]);
                assert_eq!(traces[0].len(), expected_count);
                println!("----")
            }
        }
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_shutdown() {
        let server = MockServer::start();

        let mock_traces = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.7/traces");
            then.status(200).body("");
        });

        let mock_stats = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.6/stats");
            then.status(200).body("");
        });
        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_url(&server.url("/"))
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V07)
            .enable_stats(Duration::from_secs(10))
            .build()
            .unwrap();

        let mut trace_chunk = vec![pb::Span {
            duration: 10,
            ..Default::default()
        }];

        trace_utils::compute_top_level_span(&mut trace_chunk);

        let data = rmp_serde::to_vec_named(&vec![trace_chunk]).unwrap();

        exporter.send(data.as_slice(), 1).unwrap();
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
                .path("/v0.7/traces");
            then.status(200).body("");
        });

        let _mock_stats = server.mock(|when, then| {
            when.method(POST)
                .header("Content-type", "application/msgpack")
                .path("/v0.6/stats");
            then.delay(Duration::from_secs(10)).status(200).body("");
        });
        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_url(&server.url("/"))
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V07)
            .enable_stats(Duration::from_secs(10))
            .build()
            .unwrap();

        let mut trace_chunk = vec![pb::Span {
            duration: 10,
            ..Default::default()
        }];

        trace_utils::compute_top_level_span(&mut trace_chunk);

        let data = rmp_serde::to_vec_named(&vec![trace_chunk]).unwrap();

        exporter.send(data.as_slice(), 1).unwrap();
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

        let traces: Vec<Vec<pb::Span>> = vec![
            vec![pb::Span {
                name: "test".to_string(),
                ..Default::default()
            }],
            vec![pb::Span {
                name: "test2".to_string(),
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

        let traces: Vec<Vec<pb::Span>> = vec![vec![pb::Span {
            name: "test".to_string(),
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
        assert_eq!(&format!("datadog.libdatadog.send.traces.errors:1|c|#libdatadog_version:{},response_code:400", env!("CARGO_PKG_VERSION")), &read(&stats_socket));
    }
}
