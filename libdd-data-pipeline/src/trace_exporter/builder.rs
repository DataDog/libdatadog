// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::agent_info::AgentInfoFetcher;
use crate::agentless::config::{AgentlessTraceConfig, DEFAULT_AGENTLESS_TIMEOUT};
use crate::otlp::config::{OtlpProtocol, DEFAULT_OTLP_TIMEOUT};
use crate::otlp::{OtlpMetricsConfig, OtlpResourceInfo, OtlpTraceConfig};
#[cfg(all(not(target_arch = "wasm32"), feature = "telemetry"))]
use crate::telemetry::TelemetryClientBuilder;
use crate::trace_exporter::agent_response::AgentResponsePayloadVersion;
use crate::trace_exporter::error::BuilderErrorKind;
use crate::trace_exporter::log_writer::DEFAULT_LOG_MAX_LINE_SIZE;
#[cfg(all(not(target_arch = "wasm32"), feature = "telemetry"))]
use crate::trace_exporter::TelemetryConfig;
#[cfg(not(target_arch = "wasm32"))]
use crate::trace_exporter::TraceExporterWorkers;
use crate::trace_exporter::{
    add_path, StatsComputationStatus, TelemetryInstrumentationSessions, TraceExporter,
    TraceExporterError, TraceExporterInputFormat, TraceExporterOutputFormat, TraceSerializer,
    TracerMetadata, INFO_ENDPOINT,
};
use arc_swap::ArcSwap;
use libdd_capabilities::{HttpClientCapability, LogWriterCapability, MaybeSend, SleepCapability};
use libdd_common::{parse_uri, tag, Endpoint};
use libdd_dogstatsd_client::new;
use libdd_shared_runtime::SharedRuntime;
#[cfg(not(target_arch = "wasm32"))]
use libdd_shared_runtime::{BlockingRuntime, ForkSafeRuntime};
use libdd_trace_utils::trace_filter::TraceFilterer;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_AGENT_URL: &str = "http://127.0.0.1:8126";

/// Build an [`http::HeaderMap`] from key/value pairs, skipping malformed entries.
fn build_otlp_header_map(headers: Vec<(String, String)>) -> http::HeaderMap {
    let mut out = http::HeaderMap::new();
    for (k, v) in headers {
        match (
            http::HeaderName::from_bytes(k.as_bytes()),
            http::HeaderValue::from_str(&v),
        ) {
            (Ok(n), Ok(vv)) => {
                out.insert(n, vv);
            }
            _ => tracing::warn!("Skipping invalid OTLP header: {:?}={:?}", k, v),
        }
    }
    out
}

#[allow(missing_docs)]
#[derive(Debug)]
pub struct TraceExporterBuilder<R: SharedRuntime> {
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
    container_id: String,
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
    stats_cardinality_limit: Option<usize>,
    #[cfg(feature = "stats-obfuscation")]
    client_side_stats_obfuscation_enabled: bool,
    #[cfg(feature = "telemetry")]
    telemetry: Option<TelemetryConfig>,
    telemetry_instrumentation_sessions: TelemetryInstrumentationSessions,
    shared_runtime: Option<Arc<R>>,
    health_metrics_enabled: bool,
    test_session_token: Option<String>,
    agent_rates_payload_version_enabled: bool,
    connection_timeout: Option<u64>,
    otlp_endpoint: Option<String>,
    otlp_headers: Vec<(String, String)>,
    agentless_endpoint: Option<String>,
    agentless_api_key: Option<String>,
    agentless_timeout: Option<Duration>,
    otlp_protocol: OtlpProtocol,
    otlp_metrics_endpoint: Option<String>,
    otlp_metrics_headers: Vec<(String, String)>,
    otel_trace_semantics_enabled: bool,
    runtime_id: Option<String>,
    /// When true, traces are written as newline-delimited JSON to stdout (the
    /// Datadog Forwarder "log exporter" path) instead of being sent to an agent.
    output_to_log: bool,
    /// Optional override for the maximum size of a single emitted log line.
    log_max_line_size: Option<usize>,
}

/// Default is impl'd for `R = ForkSafeRuntime` only so that bare
/// `TraceExporterBuilder::default()` resolves unambiguously to the fork-safe variant on
/// native. Builders parameterized with another runtime construct via
/// [`TraceExporterBuilder::new`] explicitly.
#[cfg(not(target_arch = "wasm32"))]
impl Default for TraceExporterBuilder<ForkSafeRuntime> {
    fn default() -> Self {
        Self::new()
    }
}

impl<R: SharedRuntime> TraceExporterBuilder<R> {
    /// Construct a builder with all fields at their initial / `None` state.
    ///
    /// On native, `TraceExporterBuilder::<ForkSafeRuntime>::new()` and
    /// `TraceExporterBuilder::default()` are equivalent; for other runtimes (e.g.
    /// `BasicRuntime`, `LocalRuntime`) callers must use `new` directly.
    pub fn new() -> Self {
        Self {
            url: None,
            hostname: String::new(),
            env: String::new(),
            app_version: String::new(),
            service: String::new(),
            tracer_version: String::new(),
            language: String::new(),
            language_version: String::new(),
            language_interpreter: String::new(),
            language_interpreter_vendor: String::new(),
            git_commit_sha: String::new(),
            process_tags: String::new(),
            container_id: String::new(),
            input_format: TraceExporterInputFormat::default(),
            output_format: TraceExporterOutputFormat::default(),
            dogstatsd_url: None,
            client_computed_stats: false,
            client_computed_top_level: false,
            stats_bucket_size: None,
            peer_tags_aggregation: false,
            compute_stats_by_span_kind: false,
            peer_tags: Vec::new(),
            stats_cardinality_limit: None,
            #[cfg(feature = "stats-obfuscation")]
            client_side_stats_obfuscation_enabled: false,
            #[cfg(feature = "telemetry")]
            telemetry: None,
            telemetry_instrumentation_sessions: TelemetryInstrumentationSessions::default(),
            shared_runtime: None,
            health_metrics_enabled: false,
            test_session_token: None,
            agent_rates_payload_version_enabled: false,
            connection_timeout: None,
            otlp_endpoint: None,
            otlp_headers: Vec::new(),
            otlp_protocol: OtlpProtocol::default(),
            otlp_metrics_endpoint: None,
            otlp_metrics_headers: Vec::new(),
            otel_trace_semantics_enabled: false,
            runtime_id: None,
            agentless_endpoint: None,
            agentless_api_key: None,
            agentless_timeout: None,
            output_to_log: false,
            log_max_line_size: None,
        }
    }
}

impl<R: SharedRuntime> TraceExporterBuilder<R> {
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

    /// Set the `Datadog-Container-Id` header
    pub fn set_container_id(&mut self, container_id: &str) -> &mut Self {
        container_id.clone_into(&mut self.container_id);
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

    /// Opt in to the V1 trace protocol.
    ///
    /// V1 is only used after runtime negotiation with the agent via `/info`. When the agent does
    /// not advertise the `/v1.0/traces` endpoint, the exporter falls back to V0.4 transparently.
    /// V1 is only compatible with V0.4 input.
    pub fn enable_v1_protocol(&mut self) -> &mut Self {
        self.output_format = TraceExporterOutputFormat::V1;
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

    /// Sets the cardinality limit for client-side stats computation.
    ///
    /// When the number of distinct stats groups exceeds `limit`, additional groups are
    /// aggregated into a sentinel key instead of being tracked individually.
    /// This bounds memory usage when the trace population has very high cardinality.
    ///
    /// Has no effect unless stats computation is enabled.
    pub fn set_stats_cardinality_limit(&mut self, cardinality_limit: usize) -> &mut Self {
        self.stats_cardinality_limit = Some(cardinality_limit);
        self
    }

    /// Enable client-side stats obfuscation. Disabled by default.
    ///
    /// Final activation also requires the agent to advertise a supported
    /// `obfuscation_version` via the `/info` endpoint. When disabled, no
    /// `datadog-obfuscation-version` header is sent on stats payloads.
    #[cfg(feature = "stats-obfuscation")]
    pub fn enable_client_side_stats_obfuscation(&mut self) -> &mut Self {
        self.client_side_stats_obfuscation_enabled = true;
        self
    }

    #[cfg(feature = "telemetry")]
    /// Enables sending telemetry metrics.
    pub fn enable_telemetry(&mut self, cfg: TelemetryConfig) -> &mut Self {
        self.telemetry = Some(cfg);
        self
    }

    /// Sets optional instrumentation session headers on telemetry requests (`dd-session-id`, etc.).
    pub fn set_telemetry_instrumentation_sessions(
        &mut self,
        sessions: TelemetryInstrumentationSessions,
    ) -> &mut Self {
        self.telemetry_instrumentation_sessions = sessions;
        self
    }

    /// Set a shared runtime used by the exporter for background workers.
    ///
    /// See [`libdd_shared_runtime::SharedRuntime`] for guidance on choosing an implementation.
    pub fn set_shared_runtime(&mut self, shared_runtime: Arc<R>) -> &mut Self {
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
    /// OTLP trace export is mutually exclusive with agentless trace export
    /// ([`Self::set_agentless_endpoint`]); configuring both causes
    /// [`Self::build`]/[`Self::build_async`] to return
    /// [`BuilderErrorKind::InvalidConfiguration`]. Setting an agent URL via
    /// [`Self::set_url`] alongside OTLP is allowed; the agent URL is still used for
    /// auxiliary endpoints (e.g. agent info / stats).
    ///
    /// Example: `set_otlp_endpoint("http://localhost:4318/v1/traces")`
    pub fn set_otlp_endpoint(&mut self, url: &str) -> &mut Self {
        self.otlp_endpoint = Some(url.to_owned());
        self
    }

    /// Selects the OTLP export protocol: [`OtlpProtocol::HttpJson`] (default) or
    /// [`OtlpProtocol::HttpProtobuf`]. The host language resolves this from
    /// `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL` / `OTEL_EXPORTER_OTLP_PROTOCOL`; a `grpc` value is
    /// unsupported and is rejected when parsed into [`OtlpProtocol`], so it never reaches here.
    pub fn set_otlp_protocol(&mut self, protocol: OtlpProtocol) -> &mut Self {
        self.otlp_protocol = protocol;
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

    /// Enables agentless APM trace export and sets the intake URL and API key.
    ///
    /// When set, APM trace spans are sent directly to the Datadog HTTP intake in JSON format
    /// (`POST /v1/input`) instead of through the Datadog Agent. The host language is responsible
    /// for resolving the endpoint URL (default
    /// `https://public-trace-http-intake.logs.{DD_SITE}` or a custom override) and the API key
    /// from its configuration. This crate does not read environment variables.
    ///
    /// Agentless trace export is mutually exclusive with both OTLP trace export
    /// ([`Self::set_otlp_endpoint`]) and a configured agent URL ([`Self::set_url`]);
    /// combining either with this method causes [`Self::build`]/[`Self::build_async`]
    /// to return [`BuilderErrorKind::InvalidConfiguration`].
    /// the output format is ignored in agentless mode; payloads are always
    /// JSON
    ///
    /// Example: `set_agentless_endpoint("https://public-trace-http-intake.logs.datadoghq.com/v1/input", "<api-key>")`
    pub fn set_agentless_endpoint(&mut self, url: &str, api_key: &str) -> &mut Self {
        self.agentless_endpoint = Some(url.to_owned());
        self.agentless_api_key = Some(api_key.to_owned());
        self
    }

    /// Sets the request timeout used by the agentless intake transport.
    ///
    /// Defaults to 15 seconds when not set. Calling this method without also calling
    /// [`Self::set_agentless_endpoint`] causes [`Self::build`]/[`Self::build_async`] to
    /// return [`BuilderErrorKind::InvalidConfiguration`].
    pub fn set_agentless_timeout(&mut self, timeout: Duration) -> &mut Self {
        self.agentless_timeout = Some(timeout);
        self
    }

    /// Enable OTLP HTTP/JSON trace-metrics export to `url` (e.g. `.../v1/metrics`).
    pub fn set_otlp_metrics_endpoint(&mut self, url: &str) -> &mut Self {
        self.otlp_metrics_endpoint = Some(url.to_owned());
        self
    }

    /// Additional HTTP headers for OTLP trace-metrics requests.
    pub fn set_otlp_metrics_headers(&mut self, headers: Vec<(String, String)>) -> &mut Self {
        self.otlp_metrics_headers = headers;
        self
    }

    /// Enables OTel trace semantics, which does not add DD-specific per-span attributes
    /// (`service.name`, `operation.name`, `resource.name`, `span.type`, `error.msg`,
    ///  `error.message`, `span.kind`) to the OTLP payload.
    /// Also strips Datadog-specific `dd.*`/`_dd.*` data-point attributes from the exported
    /// histogram. This is useful when exporting to a native OTel backend that does not expect
    /// Datadog semantics. The host language tracer is expected to observe this behavior by
    /// setting the `DD_TRACE_OTEL_SEMANTICS_ENABLED` environment variable to `true`.
    pub fn enable_otel_trace_semantics(&mut self) -> &mut Self {
        self.otel_trace_semantics_enabled = true;
        self
    }

    /// Set the runtime identifier supplied by the language tracer.
    ///
    /// When set, this ID is reused for both OTLP trace exports and OTLP trace-metrics so that all
    /// signals can be correlated by the backend. If not set, a fresh UUID is generated.
    pub fn set_runtime_id(&mut self, id: &str) -> &mut Self {
        self.runtime_id = Some(id.to_owned());
        self
    }
    /// Configure the exporter to write traces as newline-delimited JSON to stdout
    /// (the Datadog Forwarder "log exporter" path) instead of sending them to a
    /// Datadog agent. This is the transport used in serverless environments (e.g.
    /// AWS Lambda) when no agent is reachable.
    ///
    /// `max_line_size` overrides the per-line byte cap; `None` (or `Some(0)`,
    /// which is coerced to the default) uses the default of 256 KiB, the AWS
    /// CloudWatch Logs per-event limit. When this is set, agent-specific behavior
    /// (agent-info polling, client-side stats, V1 negotiation) is bypassed.
    ///
    /// In this mode each span's `meta` is serialized to process stdout (and thus
    /// captured by CloudWatch Logs in Lambda); `meta_struct` is excluded because
    /// it holds raw msgpack the log intake cannot interpret. Writes are
    /// synchronous/blocking, so this mode targets single-threaded serverless
    /// runtimes where blocking stdout writes do not stall a shared async reactor.
    ///
    /// Takes precedence over an OTLP endpoint: if both this and `set_otlp_endpoint`
    /// are configured, traces are written to the log output and not sent via OTLP.
    pub fn set_output_to_log(&mut self, max_line_size: Option<usize>) -> &mut Self {
        self.output_to_log = true;
        // Treat `Some(0)` as "use the default" (a 0 cap would drop every span);
        // keeps parity with the FFI setter's 0-sentinel.
        self.log_max_line_size = max_line_size.filter(|&n| n != 0);
        self
    }

    /// Build the [`TraceExporter`] synchronously.
    ///
    /// Sync facade over [`Self::build_async`]; panics inside an existing tokio context.
    /// Requires `R: BlockingRuntime` so the builder can drive setup on the runtime. Not
    /// available on wasm — use [`Self::build_async`] there.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn build<
        C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    >(
        mut self,
    ) -> Result<TraceExporter<C, R>, TraceExporterError>
    where
        R: BlockingRuntime,
    {
        let shared_runtime = match self.shared_runtime.as_ref() {
            Some(rt) => rt.clone(),
            None => {
                let rt = Arc::new(R::new().map_err(|e| {
                    TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
                        e.to_string(),
                    ))
                })?);
                self.shared_runtime = Some(rt.clone());
                rt
            }
        };
        shared_runtime.block_on(self.build_async::<C>())?
    }

    /// Build the [`TraceExporter`] asynchronously.
    ///
    /// Awaits all async setup (e.g. telemetry start-up). Safe to drive from any async
    /// context. If [`set_shared_runtime`](Self::set_shared_runtime) was not called, a new
    /// runtime is constructed via [`SharedRuntime::new`].
    pub async fn build_async<
        C: HttpClientCapability + SleepCapability + LogWriterCapability + MaybeSend + Sync + 'static,
    >(
        self,
    ) -> Result<TraceExporter<C, R>, TraceExporterError> {
        if !Self::is_inputs_outputs_formats_compatible(self.input_format, self.output_format) {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "Combination of input and output formats not allowed".to_string(),
                ),
            ));
        }

        self.validate_export_targets()?;

        let shared_runtime = match self.shared_runtime {
            Some(rt) => rt,
            None => Arc::new(R::new().map_err(|e| {
                TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(e.to_string()))
            })?),
        };

        let dogstatsd = self
            .dogstatsd_url
            .and_then(|u| new(Endpoint::from_slice(&u)).ok().map(Arc::new));

        let base_url = self.url.as_deref().unwrap_or(DEFAULT_AGENT_URL);

        let agent_url: http::Uri = parse_uri(base_url).map_err(|e: anyhow::Error| {
            TraceExporterError::Builder(BuilderErrorKind::InvalidUri(e.to_string()))
        })?;

        let libdatadog_version = tag!("libdatadog_version", env!("CARGO_PKG_VERSION"));

        let capabilities = C::new_client();

        // --- Platform-specific worker setup ---
        // The blocks below spawn background workers via `SharedRuntime`. On
        // native, workers run on the tokio runtime; on wasm, they run on the JS
        // event loop via `spawn_local`. Telemetry remains native-only for now.

        #[cfg(feature = "stats-obfuscation")]
        use libdd_trace_stats::span_concentrator::StatsComputationObfuscationConfig;

        use crate::trace_exporter::stats::StatsComputationConfig;

        // Agentless mode has no Datadog Agent to poll, so we skip
        // starting the `/info` fetcher
        let agentless_enabled = self.agentless_endpoint.is_some();
        let info_endpoint = Endpoint::from_url(add_path(&agent_url, INFO_ENDPOINT));
        let (info_fetcher, info_response_observer) =
            AgentInfoFetcher::<C>::new(info_endpoint, Duration::from_secs(5 * 60));
        // TODO(APMSP-3609): consolidate per-mode worker gating (info-fetcher, telemetry,
        // stats concentrator) off the selected export destination in one place.
        // In log-export mode there is no agent to poll; skip spawning the worker
        // entirely so we don't make repeated failing `/info` calls (e.g. in Lambda).
        let info_fetcher_handle = if self.output_to_log || agentless_enabled {
            None
        } else {
            Some(
                shared_runtime
                    .spawn_worker(info_fetcher, false)
                    .map_err(|e| {
                        TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
                            e.to_string(),
                        ))
                    })?,
            )
        };
        // The handle is currently only tracked for shutdown on native; on wasm
        // it is dropped here (the worker keeps running on the JS event loop
        // until the page/module is torn down).
        #[cfg(target_arch = "wasm32")]
        drop(info_fetcher_handle);

        #[allow(unused_mut)]
        let mut stats = StatsComputationStatus::Disabled;
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(bucket_size) = self.stats_bucket_size {
            stats = StatsComputationStatus::DisabledByAgent { bucket_size };
        }

        #[cfg(all(not(target_arch = "wasm32"), feature = "telemetry"))]
        let (telemetry_client, telemetry_handle) = {
            let sessions = self.telemetry_instrumentation_sessions;
            let telemetry = self
                .telemetry
                .filter(|_| {
                    // no agent endpoint to talk to, so we skip the
                    // telemetry worker
                    !(agentless_enabled || self.output_to_log)
                })
                .map(|telemetry_config| {
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
                    if let Some(ref id) = sessions.session_id {
                        builder = builder.set_session_id(id);
                    }
                    if let Some(ref id) = sessions.root_session_id {
                        builder = builder.set_root_session_id(id);
                    }
                    if let Some(ref id) = sessions.parent_session_id {
                        builder = builder.set_parent_session_id(id);
                    }
                    Ok(builder.build())
                });
            match telemetry {
                Some(Ok((client_tel, worker))) => {
                    let handle = shared_runtime.spawn_worker(worker, false).map_err(|e| {
                        TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(
                            e.to_string(),
                        ))
                    })?;
                    client_tel.start().await;
                    (Some(client_tel), Some(handle))
                }
                Some(Err(e)) => return Err(e),
                None => (None, None),
            }
        };

        // Transport selection: agentless is mutually exclusive with both OTLP and a
        // user-supplied agent URL; OTLP and the agent URL may coexist. All exclusion
        // rules are enforced by `validate_export_targets` above, so we can just move the
        // fields out here.
        let otlp_endpoint = self.otlp_endpoint;
        let agentless_endpoint = self.agentless_endpoint;
        let agentless_api_key = self.agentless_api_key;

        let agentless_config = match (agentless_endpoint, agentless_api_key) {
            (Some(url), Some(api_key)) => Some(AgentlessTraceConfig {
                endpoint_url: url,
                api_key,
                timeout: self.agentless_timeout.unwrap_or(DEFAULT_AGENTLESS_TIMEOUT),
            }),
            _ => None,
        };

        let otlp_timeout = self
            .connection_timeout
            .map(Duration::from_millis)
            .unwrap_or(DEFAULT_OTLP_TIMEOUT);

        // `self.otlp_protocol` is always an HTTP encoding here: gRPC is rejected at the parse
        // boundary (`OtlpProtocol::from_str`) and so can never be constructed.
        let otlp_config = otlp_endpoint.map(|url| OtlpTraceConfig {
            endpoint_url: url,
            headers: build_otlp_header_map(self.otlp_headers),
            timeout: otlp_timeout,
            protocol: self.otlp_protocol,
            otel_trace_semantics_enabled: self.otel_trace_semantics_enabled,
        });

        let otlp_metrics_config = self.otlp_metrics_endpoint.map(|url| OtlpMetricsConfig {
            endpoint_url: url,
            headers: build_otlp_header_map(self.otlp_metrics_headers),
            timeout: otlp_timeout,
            protocol: OtlpProtocol::HttpJson,
            otel_trace_semantics_enabled: self.otel_trace_semantics_enabled,
        });

        let runtime_id = self
            .runtime_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // OTLP metrics + stats bucket size: start the concentrator unconditionally (bypass the
        // agent gate) so `check_agent_info` cannot later disable stats.
        #[allow(unused_mut)]
        let mut otlp_stats_enabled = false;
        #[cfg(not(target_arch = "wasm32"))]
        if let (Some(metrics_config), Some(bucket_size)) =
            (otlp_metrics_config.clone(), self.stats_bucket_size)
        {
            use crate::otlp::OtlpStatsExporter;
            use libdd_trace_stats::span_concentrator::SpanConcentrator;
            use std::sync::Mutex;
            let span_kinds = crate::trace_exporter::stats::DEFAULT_STATS_ELIGIBLE_SPAN_KINDS
                .iter()
                .map(|s| s.to_string())
                .collect();
            let concentrator = Arc::new(Mutex::new(SpanConcentrator::new(
                bucket_size,
                std::time::SystemTime::now(),
                span_kinds,
                self.peer_tags.clone(),
                None,
                #[cfg(feature = "stats-obfuscation")]
                None,
            )));
            let mut resource = OtlpResourceInfo::default();
            resource.service = self.service.clone();
            resource.env = self.env.clone();
            resource.app_version = self.app_version.clone();
            resource.language = self.language.clone();
            resource.tracer_version = self.tracer_version.clone();
            resource.runtime_id = runtime_id.clone();
            resource.hostname = self.hostname.clone();
            resource.process_tags = self.process_tags.clone();
            let worker = OtlpStatsExporter {
                flush_interval: bucket_size,
                concentrator: concentrator.clone(),
                config: metrics_config,
                resource,
                test_token: self.test_session_token.clone(),
                capabilities: capabilities.clone(),
            };
            let worker_handle = shared_runtime.spawn_worker(worker, false).map_err(|e| {
                TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(e.to_string()))
            })?;
            stats = StatsComputationStatus::Enabled {
                stats_concentrator: concentrator,
                worker_handle,
            };
            otlp_stats_enabled = true;
        }

        let log_output = self
            .output_to_log
            .then(|| self.log_max_line_size.unwrap_or(DEFAULT_LOG_MAX_LINE_SIZE));

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
                runtime_id,
                service: self.service,
                container_id: self.container_id,
            },
            input_format: self.input_format,
            output_format: self.output_format,
            v1_active: std::sync::atomic::AtomicBool::new(false),
            v1_unavailable_logged: std::sync::Once::new(),
            serializer: TraceSerializer::new(),
            client_computed_top_level: self.client_computed_top_level,
            shared_runtime,
            dogstatsd,
            common_stats_tags: vec![libdatadog_version],
            client_side_stats: StatsComputationConfig {
                status: ArcSwap::new(stats.into()),
                stats_cardinality_limit: self.stats_cardinality_limit,
                #[cfg(feature = "stats-obfuscation")]
                obfuscation_config: Arc::new(ArcSwap::from_pointee(
                    StatsComputationObfuscationConfig::default(),
                )),
                #[cfg(feature = "stats-obfuscation")]
                obfuscation_enabled: self.client_side_stats_obfuscation_enabled,
            },
            previous_info_state: arc_swap::ArcSwapOption::new(None),
            info_response_observer,
            #[cfg(all(not(target_arch = "wasm32"), feature = "telemetry"))]
            telemetry: telemetry_client,
            health_metrics_enabled: self.health_metrics_enabled,
            capabilities,
            #[cfg(not(target_arch = "wasm32"))]
            workers: TraceExporterWorkers {
                info_fetcher: info_fetcher_handle,
                #[cfg(feature = "telemetry")]
                telemetry: telemetry_handle,
            },
            agent_payload_response_version: self
                .agent_rates_payload_version_enabled
                .then(AgentResponsePayloadVersion::new),
            otlp_config,
            agentless_config,
            trace_filterer: ArcSwap::from_pointee(TraceFilterer::with_empty_conf()),
            otlp_stats_enabled,
            log_output,
        })
    }

    /// Reject configurations that combine mutually exclusive trace export targets.
    ///
    /// Trace export uses exactly one of three transports:
    /// - the Datadog Agent (via [`Self::set_url`], the default when no transport is set),
    /// - an OTLP HTTP/JSON endpoint (via [`Self::set_otlp_endpoint`]), or
    /// - the agentless intake (via [`Self::set_agentless_endpoint`]).
    ///
    /// Exclusion rules enforced here:
    /// - OTLP and agentless cannot both be configured.
    /// - Agentless cannot be combined with a caller-supplied agent URL.
    /// - Log output cannot be combined with OTLP or agentless trace export.
    /// - [`Self::set_agentless_timeout`] requires [`Self::set_agentless_endpoint`].
    ///
    /// OTLP and an agent URL may coexist: the agent URL is still useful for auxiliary
    /// agent endpoints (info, stats) even when trace payloads are routed to OTLP.
    fn validate_export_targets(&self) -> Result<(), TraceExporterError> {
        let otlp_set = self.otlp_endpoint.is_some();
        let agentless_set = self.agentless_endpoint.is_some();
        let agent_url_set = self.url.is_some();
        let log_output_set = self.output_to_log;

        if otlp_set && agentless_set {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "OTLP and agentless trace export cannot both be enabled".to_string(),
                ),
            ));
        }

        if agentless_set && agent_url_set {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "trace agent URL cannot be set when agentless trace export is enabled"
                        .to_string(),
                ),
            ));
        }

        if log_output_set && otlp_set {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "log trace export cannot be combined with OTLP trace export".to_string(),
                ),
            ));
        }

        if log_output_set && agentless_set {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "log trace export cannot be combined with agentless trace export".to_string(),
                ),
            ));
        }

        if !agentless_set && self.agentless_timeout.is_some() {
            return Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(
                    "agentless timeout was set but no agentless trace endpoint is configured"
                        .to_string(),
                ),
            ));
        }

        Ok(())
    }

    fn is_inputs_outputs_formats_compatible(
        input: TraceExporterInputFormat,
        output: TraceExporterOutputFormat,
    ) -> bool {
        match input {
            TraceExporterInputFormat::V04 => matches!(
                output,
                TraceExporterOutputFormat::V04
                    | TraceExporterOutputFormat::V05
                    | TraceExporterOutputFormat::V1
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
    use libdd_shared_runtime::ForkSafeRuntime;

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_log_output_mode() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_service("test")
            .set_input_format(TraceExporterInputFormat::V04)
            .set_output_to_log(None);
        let exporter = builder.build::<NativeCapabilities>().unwrap();
        // Log-output mode is enabled and the agent-info worker is not spawned.
        // (End-to-end send -> bytes is covered by the capability-injecting test in
        // `mod.rs` and the `log_writer` unit tests.)
        assert!(
            exporter.log_output.is_some(),
            "log_output should be set in log-output mode"
        );
        assert!(
            exporter.workers.info_fetcher.is_none(),
            "no agent-info worker should be spawned in log mode"
        );
    }

    #[test]
    fn set_output_to_log_some_zero_uses_default() {
        // `Some(0)` is coerced to "use the default cap" (a 0 cap would drop every
        // span), which is represented as `None` on the builder field.
        let mut builder = TraceExporterBuilder::default();
        builder.set_output_to_log(Some(0));
        assert!(builder.output_to_log);
        assert_eq!(builder.log_max_line_size, None);
    }

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
            .set_client_computed_stats();
        #[cfg(feature = "telemetry")]
        builder.enable_telemetry(TelemetryConfig {
            heartbeat: 1000,
            runtime_id: None,
            debug_enabled: false,
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
        #[cfg(feature = "telemetry")]
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
        #[cfg(feature = "telemetry")]
        assert!(exporter.telemetry.is_none());
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_set_shared_runtime() {
        let mut builder = TraceExporterBuilder::default();
        let shared_runtime = Arc::new(ForkSafeRuntime::new().unwrap());
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

    #[test]
    fn test_enable_v1_protocol_sets_output_format() {
        let mut builder = TraceExporterBuilder::default();
        builder.enable_v1_protocol();
        assert!(matches!(
            builder.output_format,
            TraceExporterOutputFormat::V1
        ));
    }

    #[test]
    fn test_v1_input_v05_incompatible() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_input_format(TraceExporterInputFormat::V05)
            .set_output_format(TraceExporterOutputFormat::V1);
        let result = builder.build::<NativeCapabilities>();
        assert!(matches!(
            result,
            Err(TraceExporterError::Builder(
                BuilderErrorKind::InvalidConfiguration(_)
            ))
        ));
    }

    fn assert_invalid_config(
        result: Result<TraceExporter<NativeCapabilities, ForkSafeRuntime>, TraceExporterError>,
    ) -> String {
        match result {
            Err(TraceExporterError::Builder(BuilderErrorKind::InvalidConfiguration(msg))) => msg,
            Err(other) => panic!("expected InvalidConfiguration, got {other:?}"),
            Ok(_) => panic!("expected InvalidConfiguration, got Ok"),
        }
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_otlp_and_agentless_rejected() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_otlp_endpoint("http://localhost:4318/v1/traces")
            .set_agentless_endpoint(
                "https://public-trace-http-intake.logs.datadoghq.com/v1/input",
                "api-key",
            );
        let msg = assert_invalid_config(builder.build::<NativeCapabilities>());
        assert!(
            msg.contains("OTLP") && msg.contains("agentless"),
            "unexpected error message: {msg}"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_agentless_and_agent_url_rejected() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url("http://localhost:8126")
            .set_agentless_endpoint(
                "https://public-trace-http-intake.logs.datadoghq.com/v1/input",
                "api-key",
            );
        let msg = assert_invalid_config(builder.build::<NativeCapabilities>());
        assert!(
            msg.contains("agent URL") && msg.contains("agentless"),
            "unexpected error message: {msg}"
        );
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_agentless_timeout_without_endpoint_rejected() {
        let mut builder = TraceExporterBuilder::default();
        builder.set_agentless_timeout(Duration::from_secs(5));
        let msg = assert_invalid_config(builder.build::<NativeCapabilities>());
        assert!(
            msg.contains("agentless timeout"),
            "unexpected error message: {msg}"
        );
    }

    #[cfg(feature = "telemetry")]
    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_agentless_skips_info_fetcher_and_telemetry() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_agentless_endpoint(
                "https://public-trace-http-intake.logs.datadoghq.com/v1/input",
                "api-key",
            )
            .enable_telemetry(TelemetryConfig {
                heartbeat: 1000,
                runtime_id: None,
                debug_enabled: false,
            });
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        // No `/info` poller is started when there is no agent to poll.
        assert!(exporter.workers.info_fetcher.is_none());
        // Telemetry talks to the agent base URL and is also skipped.
        assert!(exporter.workers.telemetry.is_none());
        assert!(exporter.telemetry.is_none());
        // Sanity: the agentless transport is actually configured.
        assert!(exporter.agentless_config.is_some());
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_otlp_with_agent_url_allowed() {
        // OTLP + agent URL must coexist (only agentless conflicts with the agent URL).
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_url("http://localhost:8126")
            .set_otlp_endpoint("http://localhost:4318/v1/traces");
        assert!(builder.build::<NativeCapabilities>().is_ok());
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn test_build_with_v1_starts_inactive() {
        let mut builder = TraceExporterBuilder::default();
        builder
            .set_input_format(TraceExporterInputFormat::V04)
            .enable_v1_protocol();
        let exporter = builder.build::<NativeCapabilities>().unwrap();

        assert!(matches!(
            exporter.output_format,
            TraceExporterOutputFormat::V1
        ));
        assert!(!exporter
            .v1_active
            .load(std::sync::atomic::Ordering::Relaxed));
        assert_eq!(
            exporter
                .effective_output_format()
                .add_path(&exporter.endpoint.url)
                .to_string(),
            "http://127.0.0.1:8126/v0.4/traces"
        );
    }
}
