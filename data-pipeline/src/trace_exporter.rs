// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0
use crate::{span_concentrator::SpanConcentrator, stats_exporter};
use bytes::Bytes;
use datadog_trace_protobuf::pb;
use datadog_trace_utils::trace_utils::{self, SendData, TracerHeaderTags};
use datadog_trace_utils::tracer_payload;
use datadog_trace_utils::tracer_payload::TraceEncoding;
use ddcommon::{connector, Endpoint};
use hyper::http::uri::PathAndQuery;
use hyper::{Body, Client, Method, Uri};
use log::error;
use std::sync::{Arc, Mutex};
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
    #[default]
    V04,
}

/// TraceExporterOutputFormat represents the format of the output traces.
/// The output format can be either V0.4 or v0.7, where V0.4 is the default.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
#[repr(C)]
pub enum TraceExporterOutputFormat {
    #[default]
    V04,
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

struct TracerTags {
    tracer_version: String,
    language: String,
    language_version: String,
    language_interpreter: String,
}

impl<'a> From<&'a TracerTags> for TracerHeaderTags<'a> {
    fn from(tags: &'a TracerTags) -> TracerHeaderTags<'a> {
        TracerHeaderTags::<'_> {
            lang: &tags.language,
            lang_version: &tags.language_version,
            tracer_version: &tags.tracer_version,
            lang_interpreter: &tags.language_interpreter,
            ..Default::default()
        }
    }
}

impl<'a> From<&'a TracerTags> for HashMap<&'static str, String> {
    fn from(tags: &'a TracerTags) -> HashMap<&'static str, String> {
        TracerHeaderTags::<'_> {
            lang: &tags.language,
            lang_version: &tags.language_version,
            tracer_version: &tags.tracer_version,
            lang_interpreter: &tags.language_interpreter,
            ..Default::default()
        }
        .into()
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

pub struct TraceExporter {
    endpoint: Endpoint,
    tags: TracerTags,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    // TODO - do something with the response callback - https://datadoghq.atlassian.net/browse/APMSP-1019
    _response_callback: Option<Box<dyn ResponseCallback>>,
    runtime: Runtime,
    stats: StatsComputationStatus,
}

impl TraceExporter {
    pub fn builder() -> TraceExporterBuilder {
        TraceExporterBuilder::default()
    }

    pub fn send(&self, data: &[u8], trace_count: usize) -> Result<String, String> {
        match self.input_format {
            TraceExporterInputFormat::Proxy => self.send_proxy(data, trace_count),
            TraceExporterInputFormat::V04 => self.send_deser_ser(data),
        }
    }

    pub fn shutdown(self) -> Result<String, String> {
        match self.stats {
            StatsComputationStatus::StatsEnabled {
                stats_concentrator: _,
                cancellation_token: cancelation_token,
                exporter_handle,
            } => self.runtime.block_on(async {
                cancelation_token.cancel();
                let _ = exporter_handle.await;
            }),
            StatsComputationStatus::StatsDisabled => {}
        };
        Ok("Ok".to_string())
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

                match Client::builder()
                    .build(connector::Connector::default())
                    .request(req)
                    .await
                {
                    Ok(response) => {
                        if response.status() != 200 {
                            let body_bytes = hyper::body::to_bytes(response.into_body()).await?;
                            let response_body =
                                String::from_utf8(body_bytes.to_vec()).unwrap_or_default();
                            anyhow::bail!("Agent did not accept traces: {response_body}");
                        }
                        match hyper::body::to_bytes(response.into_body()).await {
                            Ok(body) => Ok(String::from_utf8_lossy(&body).to_string()),
                            Err(err) => {
                                anyhow::bail!("Error reading agent response body: {err}");
                            }
                        }
                    }
                    Err(err) => anyhow::bail!("Failed to send traces: {err}"),
                }
            })
            .or_else(|err| {
                error!("Error sending traces: {err}");
                Ok(String::from("{}"))
            })
    }

    /// Add all spans from the given iterator into the stats concentrator
    fn add_spans_to_stats<'a>(&self, spans: impl Iterator<Item = &'a pb::Span>) {
        // TODO: How do we want to react if we have an error i.e. another thread panicked with
        // the lock
        if let StatsComputationStatus::StatsEnabled {
            stats_concentrator,
            cancellation_token: _,
            exporter_handle: _,
        } = &self.stats
        {
            let mut stats_concentrator = stats_concentrator.lock().unwrap();
            for span in spans {
                let _ = stats_concentrator.add_span(span);
            }
        }
    }

    fn send_deser_ser(&self, data: &[u8]) -> Result<String, String> {
        let size = data.len();
        // TODO base on input format
        let traces: Vec<Vec<pb::Span>> = match rmp_serde::from_slice(data) {
            Ok(res) => res,
            Err(err) => {
                error!("Error deserializing trace from request body: {err}");
                return Ok(String::from("{}"));
            }
        };

        if traces.is_empty() {
            error!("No traces deserialized from the request body.");
            return Ok(String::from("{}"));
        }

        if let StatsComputationStatus::StatsEnabled { .. } = &self.stats {
            self.add_spans_to_stats(traces.iter().flat_map(|trace| trace.iter()));
        }

        let header_tags: TracerHeaderTags<'_> = (&self.tags).into();

        match self.output_format {
            TraceExporterOutputFormat::V04 => rmp_serde::to_vec_named(&traces).map_or_else(
                |err| {
                    error!("Error serializing traces: {err}");
                    Ok(String::from("{}"))
                },
                |res| {
                    self.send_data_to_url(
                        &res,
                        traces.len(),
                        self.output_format.add_path(&self.endpoint.url),
                    )
                },
            ),
            TraceExporterOutputFormat::V07 => {
                let tracer_payload = trace_utils::collect_trace_chunks(
                    traces,
                    &header_tags,
                    &mut tracer_payload::DefaultTraceChunkProcessor,
                    self.endpoint.api_key.is_some(),
                    TraceEncoding::V07,
                );

                let endpoint = Endpoint {
                    url: self.output_format.add_path(&self.endpoint.url),
                    ..self.endpoint.clone()
                };
                let send_data = SendData::new(size, tracer_payload, header_tags, &endpoint);
                self.runtime.block_on(async {
                    match send_data.send().await.last_result {
                        Ok(response) => match hyper::body::to_bytes(response.into_body()).await {
                            Ok(body) => Ok(String::from_utf8_lossy(&body).to_string()),
                            Err(err) => {
                                error!("Error reading agent response body: {err}");
                                Ok(String::from("{}"))
                            }
                        },
                        Err(err) => {
                            error!("Error sending traces: {err}");
                            Ok(String::from("{}"))
                        }
                    }
                })
            }
        }
    }
}

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

    // Stats specific fields
    stats_bucket_size: Option<time::Duration>,
    peer_tags_aggregation: bool,
    compute_stats_by_span_kind: bool,
    peer_tags: Vec<String>,
}

impl TraceExporterBuilder {
    pub fn set_url(mut self, url: &str) -> Self {
        self.url = Some(url.to_owned());
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

    pub fn set_tracer_version(mut self, tracer_version: &str) -> Self {
        tracer_version.clone_into(&mut self.tracer_version);
        self
    }

    pub fn set_language(mut self, lang: &str) -> Self {
        lang.clone_into(&mut self.language);
        self
    }

    pub fn set_language_version(mut self, lang_version: &str) -> Self {
        lang_version.clone_into(&mut self.language_version);
        self
    }

    pub fn set_language_interpreter(mut self, lang_interpreter: &str) -> Self {
        lang_interpreter.clone_into(&mut self.language_interpreter);
        self
    }

    pub fn set_input_format(mut self, input_format: TraceExporterInputFormat) -> Self {
        self.input_format = input_format;
        self
    }

    pub fn set_output_format(mut self, output_format: TraceExporterOutputFormat) -> Self {
        self.output_format = output_format;
        self
    }

    pub fn set_response_callback(mut self, response_callback: Box<dyn ResponseCallback>) -> Self {
        self.response_callback = Some(response_callback);
        self
    }

    pub fn enable_stats(mut self, bucket_size: time::Duration) -> Self {
        self.stats_bucket_size = Some(bucket_size);
        self
    }

    pub fn enable_stats_peer_tags_aggregation(mut self, peer_tags: Vec<String>) -> Self {
        self.peer_tags_aggregation = true;
        self.peer_tags = peer_tags;
        self
    }

    pub fn enable_compute_stats_by_span_kind(mut self) -> Self {
        self.compute_stats_by_span_kind = true;
        self
    }

    pub fn build(mut self) -> anyhow::Result<TraceExporter> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        let mut stats = StatsComputationStatus::StatsDisabled;

        if let Some(bucket_size) = self.stats_bucket_size {
            let stats_concentrator = Arc::new(Mutex::new(SpanConcentrator::new(
                bucket_size,
                time::SystemTime::now(),
                false,
                false,
                vec![],
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
                    self.url.as_deref().unwrap_or("http://127.0.0.1:8126"),
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

        Ok(TraceExporter {
            endpoint: Endpoint::from_slice(self.url.as_deref().unwrap_or("http://127.0.0.1:8126")),
            tags: TracerTags {
                tracer_version: std::mem::take(&mut self.tracer_version),
                language_version: std::mem::take(&mut self.language_version),
                language_interpreter: std::mem::take(&mut self.language_interpreter),
                language: std::mem::take(&mut self.language),
            },
            input_format: self.input_format,
            output_format: self.output_format,
            _response_callback: self.response_callback,
            runtime,
            stats,
        })
    }
}

pub trait ResponseCallback {
    fn call(&self, response: &str);
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use std::collections::HashMap;
    use time::Duration;

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
    }

    #[test]
    fn new_defaults() {
        let builder = TraceExporterBuilder::default();
        let exporter = builder
            .set_tracer_version("v0.1")
            .set_language("nodejs")
            .set_language_version("1.0")
            .set_language_interpreter("v8")
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
    }

    #[test]
    fn test_from_tracer_tags_to_tracer_header_tags() {
        let tracer_tags = TracerTags {
            tracer_version: "v0.1".to_string(),
            language: "rust".to_string(),
            language_version: "1.52.1".to_string(),
            language_interpreter: "rustc".to_string(),
        };

        let tracer_header_tags: TracerHeaderTags = (&tracer_tags).into();

        assert_eq!(tracer_header_tags.tracer_version, "v0.1");
        assert_eq!(tracer_header_tags.lang, "rust");
        assert_eq!(tracer_header_tags.lang_version, "1.52.1");
        assert_eq!(tracer_header_tags.lang_interpreter, "rustc");
    }

    #[test]
    fn test_from_tracer_tags_to_hashmap() {
        let tracer_tags = TracerTags {
            tracer_version: "v0.1".to_string(),
            language: "rust".to_string(),
            language_version: "1.52.1".to_string(),
            language_interpreter: "rustc".to_string(),
        };

        let hashmap: HashMap<&'static str, String> = (&tracer_tags).into();

        assert_eq!(hashmap.get("datadog-meta-tracer-version").unwrap(), "v0.1");
        assert_eq!(hashmap.get("datadog-meta-lang").unwrap(), "rust");
        assert_eq!(hashmap.get("datadog-meta-lang-version").unwrap(), "1.52.1");
        assert_eq!(
            hashmap.get("datadog-meta-lang-interpreter").unwrap(),
            "rustc"
        );
    }

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
        exporter.shutdown().unwrap();

        mock_traces.assert();
        mock_stats.assert();
    }
}
