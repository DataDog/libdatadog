// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::agent_info::AgentInfoFetcher;
use crate::otlp::config::{OtlpProtocol, DEFAULT_OTLP_TIMEOUT};
use crate::otlp::OtlpTraceConfig;
#[cfg(feature = "telemetry")]
use crate::telemetry::TelemetryClientBuilder;
use crate::trace_exporter::agent_response::AgentResponsePayloadVersion;
use crate::trace_exporter::error::BuilderErrorKind;
#[cfg(feature = "telemetry")]
use crate::trace_exporter::TelemetryConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::trace_exporter::TraceExporterWorkers;
use crate::trace_exporter::{
    add_path, StatsComputationStatus, TraceExporter, TraceExporterError, TraceExporterInputFormat,
    TraceExporterOutputFormat, TracerMetadata, INFO_ENDPOINT,
};
use arc_swap::ArcSwap;
use libdd_capabilities::{HttpClientTrait, MaybeSend};
use libdd_common::{parse_uri, tag, Endpoint};
use libdd_dogstatsd_client::new;
use libdd_shared_runtime::SharedRuntime;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8126";

#[allow(missing_docs)]
#[derive(Debug, Default)]
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
    process_tags: String,
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
    #[cfg(feature = "telemetry")]
    telemetry: Option<TelemetryConfig>,
    shared_runtime: Option<Arc<SharedRuntime>>,
    health_metrics_enabled: bool,
    test_session_token: Option<String>,
    agent_rates_payload_version_enabled: bool,
    connection_timeout: Option<u64>,
    otlp_endpoint: Option<String>,
    otlp_headers: Vec<(String, String)>,
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

    pub fn set_process_tags(&mut self, process_tags: &str) -> &mut Self {
        process_tags.clone_into(&mut self.process_tags);
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

    #[cfg(feature = "telemetry")]
    /// Enables sending telemetry metrics.
    pub fn enable_telemetry(&mut self, cfg: TelemetryConfig) -> &mut Self {
        self.telemetry = Some(cfg);
        self
    }

    /// Set a shared runtime used by the exporter for background workers.
    pub fn set_shared_runtime(&mut self, shared_runtime: Arc<SharedRuntime>) -> &mut Self {
        self.shared_runtime = Some(shared_runtime);
        self
    }

    /// Enables health metrics emission.
    pub fn enable_health_metrics(&mut self) -> &mut Self {
        self.health_metrics_enabled = true;
        self
    }

    /// Enables storing and checking the agent payload
    pub fn enable_agent_rates_payload_version(&mut self) -> &mut Self {
        self.agent_rates_payload_version_enabled = true;
        self
    }

    /// Sets the agent's connection timeout.
    pub fn set_connection_timeout(&mut self, timeout_ms: Option<u64>) -> &mut Self {
        self.connection_timeout = timeout_ms;
        self
    }

    /// Enables OTLP HTTP/JSON export and sets the endpoint URL.
    ///
    /// When set, traces are sent to this endpoint in OTLP HTTP/JSON format instead of the
    /// Datadog agent. The host language is responsible for resolving the endpoint from its
    /// configuration (e.g. `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`) before calling this method.
    ///
    /// Example: `set_otlp_endpoint("http://localhost:4318/v1/traces")`
    pub fn set_otlp_endpoint(&mut self, url: &str) -> &mut Self {
        self.otlp_endpoint = Some(url.to_owned());
        self
    }

    /// Sets additional HTTP headers to include in OTLP trace export requests.
    ///
    /// Headers should be provided as key-value pairs. The host language is responsible for
    /// resolving headers from its configuration (e.g. `OTEL_EXPORTER_OTLP_TRACES_HEADERS`)
    /// before calling this method.
    pub fn set_otlp_headers(&mut self, headers: Vec<(String, String)>) -> &mut Self {
        self.otlp_headers = headers;
        self
    }

    #[allow(missing_docs)]
    pub fn build<H: HttpClientTrait + MaybeSend + Sync + 'static>(
        self,
    ) -> Result<TraceExporter<H>, TraceExporterError> {
        if !Self::is_inputs_outputs_formats_compatible(self.input_format, self.output_format) {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "Combination of input and output formats not allowed".to_string(),
                ),
            ));
        }

        let shared_runtime =
            self.shared_runtime
                .unwrap_or(Arc::new(SharedRuntime::new().map_err(|e| {
                    TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
                        e.to_string(),
                    ))
                })?));

        let dogstatsd = self.dogstatsd_url.and_then(|u| {
            new(Endpoint::from_slice(&u)).ok() // If we couldn't set the endpoint return
                                               // None
        });

        let base_url = self.url.as_deref().unwrap_or(DEFAULT_AGENT_URL);

        let agent_url: http::Uri = parse_uri(base_url).map_err(|e: anyhow::Error| {
            TraceExporterError::Builder(BuilderErrorKind::InvalidUri(e.to_string()))
        })?;

        let libdatadog_version = tag!("libdatadog_version", env!("CARGO_PKG_VERSION"));
        #[allow(unused_mut)]
        let mut stats = StatsComputationStatus::Disabled;

        #[cfg(not(target_arch = "wasm32"))]
        {
            let info_endpoint = Endpoint::from_url(add_path(&agent_url, INFO_ENDPOINT));
            let (info_fetcher, info_response_observer) =
                AgentInfoFetcher::<H>::new(info_endpoint.clone(), Duration::from_secs(5 * 60));
            let info_fetcher_handle =
                shared_runtime
                    .spawn_worker(info_fetcher, false)
                    .map_err(|e| {
                        TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
                            e.to_string(),
                        ))
                    })?;

            if let Some(bucket_size) = self.stats_bucket_size {
                stats = StatsComputationStatus::DisabledByAgent { bucket_size };
            }

            #[cfg(feature = "telemetry")]
            let (telemetry_client, telemetry_handle) = {
                let telemetry = self.telemetry.map(|telemetry_config| {
                    let mut builder = TelemetryClientBuilder::default()
                        .set_language(&self.language)
                        .set_language_version(&self.language_version)
                        .set_service_name(&self.service)
                        .set_service_version(&self.app_version)
                        .set_env(&self.env)
                        .set_tracer_version(&self.tracer_version)
                        .set_heartbeat(telemetry_config.heartbeat)
                        .set_url(base_url)
                        .set_debug_enabled(telemetry_config.debug_enabled);
                    if let Some(id) = telemetry_config.runtime_id {
                        builder = builder.set_runtime_id(&id);
                    }
                    if let Some(id) = telemetry_config.session_id {
                        builder = builder.set_session_id(&id);
                    }
                    if let Some(id) = telemetry_config.root_session_id {
                        builder = builder.set_root_session_id(&id);
                    }
                    if let Some(id) = telemetry_config.parent_session_id {
                        builder = builder.set_parent_session_id(&id);
                    }
                    Ok(builder.build())
                });
                match telemetry {
                    Some(Ok((client, worker))) => {
                        let handle = shared_runtime.spawn_worker(worker, false).map_err(|e| {
                            TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
                                e.to_string(),
                            ))
                        })?;
                        shared_runtime.block_on(client.start()).map_err(|e| {
                            TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
                                e.to_string(),
                            ))
                        })?;
                        (Some(client), Some(handle))
                    }
                    Some(Err(e)) => return Err(e),
                    None => (None, None),
                }
            };

            Ok(TraceExporter {
                endpoint: Endpoint {
                    url: agent_url,
                    test_token: self.test_session_token.map(|token| token.into()),
                    timeout_ms: self
                        .connection_timeout
                        .unwrap_or(Endpoint::default().timeout_ms),
                    ..Default::default()
                },
                metadata: TracerMetadata {
                    tracer_version: self.tracer_version,
                    language_version: self.language_version,
                    language_interpreter: self.language_interpreter,
                    language_interpreter_vendor: self.language_interpreter_vendor,
                    language: self.language,
                    git_commit_sha: self.git_commit_sha,
                    process_tags: self.process_tags,
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
                shared_runtime,
                dogstatsd,
                common_stats_tags: vec![libdatadog_version],
                client_side_stats: ArcSwap::new(stats.into()),
                previous_info_state: arc_swap::ArcSwapOption::new(None),
                info_response_observer,
                #[cfg(feature = "telemetry")]
                telemetry: telemetry_client,
                health_metrics_enabled: self.health_metrics_enabled,
                client: H::new_client(),
                workers: TraceExporterWorkers {
                    info_fetcher: info_fetcher_handle,
                    #[cfg(feature = "telemetry")]
                    telemetry: telemetry_handle,
                },
                agent_payload_response_version: self
                    .agent_rates_payload_version_enabled
                    .then(AgentResponsePayloadVersion::new),
                otlp_config: self.otlp_endpoint.map(|url| {
                    let mut headers = http::HeaderMap::new();
                    for (key, value) in self.otlp_headers {
                        match (
                            http::HeaderName::from_bytes(key.as_bytes()),
                            http::HeaderValue::from_str(&value),
                        ) {
                            (Ok(name), Ok(val)) => {
                                headers.insert(name, val);
                            }
                            _ => {
                                tracing::warn!(
                                    "Skipping invalid OTLP header: {:?}={:?}",
                                    key,
                                    value
                                );
                            }
                        }
                    }
                    OtlpTraceConfig {
                        endpoint_url: url,
                        headers,
                        timeout: self
                            .connection_timeout
                            .map(Duration::from_millis)
                            .unwrap_or(DEFAULT_OTLP_TIMEOUT),
                        protocol: OtlpProtocol::HttpJson,
                    }
                }),
            })
        }

        #[cfg(target_arch = "wasm32")]
        {
            let info_endpoint = Endpoint::from_url(add_path(&agent_url, INFO_ENDPOINT));
            let (_info_fetcher, info_response_observer) =
                AgentInfoFetcher::<H>::new(info_endpoint, Duration::from_secs(5 * 60));

            Ok(TraceExporter {
                endpoint: Endpoint {
                    url: agent_url,
                    test_token: self.test_session_token.map(|token| token.into()),
                    timeout_ms: self
                        .connection_timeout
                        .unwrap_or(Endpoint::default().timeout_ms),
                    ..Default::default()
                },
                metadata: TracerMetadata {
                    tracer_version: self.tracer_version,
                    language_version: self.language_version,
                    language_interpreter: self.language_interpreter,
                    language_interpreter_vendor: self.language_interpreter_vendor,
                    language: self.language,
                    git_commit_sha: self.git_commit_sha,
                    process_tags: self.process_tags,
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
                shared_runtime,
                dogstatsd,
                common_stats_tags: vec![libdatadog_version],
                client_side_stats: ArcSwap::new(stats.into()),
                previous_info_state: arc_swap::ArcSwapOption::new(None),
                info_response_observer,
                health_metrics_enabled: self.health_metrics_enabled,
                client: H::new_client(),
                agent_payload_response_version: self
                    .agent_rates_payload_version_enabled
                    .then(AgentResponsePayloadVersion::new),
                otlp_config: self.otlp_endpoint.map(|url| {
                    let mut headers = http::HeaderMap::new();
                    for (key, value) in self.otlp_headers {
                        match (
                            http::HeaderName::from_bytes(key.as_bytes()),
                            http::HeaderValue::from_str(&value),
                        ) {
                            (Ok(name), Ok(val)) => {
                                headers.insert(name, val);
                            }
                            _ => {
                                tracing::warn!(
                                    "Skipping invalid OTLP header: {:?}={:?}",
                                    key,
                                    value
                                );
                            }
                        }
                    }
                    OtlpTraceConfig {
                        endpoint_url: url,
                        headers,
                        timeout: self
                            .connection_timeout
                            .map(Duration::from_millis)
                            .unwrap_or(DEFAULT_OTLP_TIMEOUT),
                        protocol: OtlpProtocol::HttpJson,
                    }
                }),
            })
        }
    }

    fn is_inputs_outputs_formats_compatible(
        input: TraceExporterInputFormat,
        output: TraceExporterOutputFormat,
    ) -> bool {
        match input {
            TraceExporterInputFormat::V04 => matches!(
                output,
                TraceExporterOutputFormat::V04 | TraceExporterOutputFormat::V05
            ),
            TraceExporterInputFormat::V05 => matches!(output, TraceExporterOutputFormat::V05),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trace_exporter::error::BuilderErrorKind;
    use libdd_capabilities_impl::NativeCapabilities;

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
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_format(TraceExporterOutputFormat::V04)
            .set_client_computed_stats()
            .enable_telemetry(TelemetryConfig {
                heartbeat: 1000,
                runtime_id: None,
                debug_enabled: false,
                ..Default::default()
            });
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        assert_eq!(
            exporter
                .output_format
                .add_path(&exporter.endpoint.url)
                .to_string(),
            "http://192.168.1.1:8127/v0.4/traces"
        );
        assert_eq!(exporter.input_format, TraceExporterInputFormat::V04);
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
        let exporter = builder.build::<NativeCapabilities>().unwrap();

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

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_set_shared_runtime() {
        let mut builder = TraceExporterBuilder::default();
        let shared_runtime = Arc::new(SharedRuntime::new().unwrap());
        builder.set_shared_runtime(shared_runtime.clone());

        let exporter = builder.build::<NativeCapabilities>().unwrap();

        assert!(Arc::ptr_eq(&exporter.shared_runtime, &shared_runtime));
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

        let exporter = builder.build::<NativeCapabilities>();

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
}
