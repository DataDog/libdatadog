// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::response::ExporterResponse;
use crate::{catch_panic, gen_error};
use libdd_capabilities_impl::NativeCapabilities;
use libdd_common_ffi::{
    CharSlice,
    {slice::AsBytes, slice::ByteSlice},
};
use libdd_data_pipeline::trace_exporter::{
    TelemetryConfig, TelemetryInstrumentationSessions, TraceExporter as GenericTraceExporter,
    TraceExporterInputFormat, TraceExporterOutputFormat,
};
use libdd_data_pipeline::OtlpProtocol;

// FFI pins the runtime parameter to `ForkSafeRuntime` for ABI stability. Rust callers that
// don't need the fork protocol can use `TraceExporter<NativeCapabilities, BasicRuntime>`
// directly.
pub(crate) type TraceExporter = GenericTraceExporter<NativeCapabilities, ForkSafeRuntime>;

use libdd_shared_runtime::ForkSafeRuntime;
use std::{ptr::NonNull, sync::Arc, time::Duration};
use tracing::debug;

#[inline]
fn sanitize_string(str: CharSlice) -> Result<String, Box<ExporterError>> {
    match str.try_to_utf8() {
        Ok(s) => Ok(s.to_string()),
        Err(_) => Err(Box::new(ExporterError::new(
            ErrorCode::InvalidInput,
            &ErrorCode::InvalidInput.to_string(),
        ))),
    }
}

/// FFI compatible configuration for the TelemetryClient.
#[derive(Debug)]
#[repr(C)]
pub struct TelemetryClientConfig<'a> {
    /// How often telemetry should be sent, in milliseconds.
    pub interval: u64,
    /// A V4 UUID that represents a tracer session. This ID should:
    /// - Be generated when the tracer starts
    /// - Be identical within the context of a host (i.e. multiple threads/processes that belong to
    ///   a single instrumented app should share the same runtime_id)
    /// - Be associated with traces to allow correlation between traces and telemetry data
    pub runtime_id: CharSlice<'a>,

    /// Whether to enable debug mode for telemetry.
    /// When enabled, sets the DD-Telemetry-Debug-Enabled header to true.
    /// Defaults to false.
    pub debug_enabled: bool,

    /// HTTP header `dd-session-id` (empty = omitted).
    pub session_id: CharSlice<'a>,
    /// HTTP header `dd-root-session-id` (empty = omitted).
    pub root_session_id: CharSlice<'a>,
    /// HTTP header `dd-parent-session-id` (empty = omitted).
    pub parent_session_id: CharSlice<'a>,
}

/// The TraceExporterConfig object will hold the configuration properties for the TraceExporter.
/// Once the configuration is passed to the TraceExporter constructor the config is no longer
/// needed by the handle and it can be freed.
#[derive(Debug, Default)]
pub struct TraceExporterConfig {
    url: Option<String>,
    tracer_version: Option<String>,
    language: Option<String>,
    language_version: Option<String>,
    language_interpreter: Option<String>,
    hostname: Option<String>,
    env: Option<String>,
    version: Option<String>,
    service: Option<String>,
    input_format: TraceExporterInputFormat,
    output_format: TraceExporterOutputFormat,
    compute_stats: bool,
    client_computed_stats: bool,
    telemetry_cfg: Option<TelemetryConfig>,
    telemetry_instrumentation_sessions: TelemetryInstrumentationSessions,
    health_metrics_enabled: bool,
    process_tags: Option<String>,
    test_session_token: Option<String>,
    connection_timeout: Option<u64>,
    otlp_timeout: Option<u64>,
    shared_runtime: Option<Arc<ForkSafeRuntime>>,
    otlp_endpoint: Option<String>,
    otlp_protocol: Option<OtlpProtocol>,
    otlp_instrumentation_scope_name: Option<String>,
    otlp_instrumentation_scope_version: Option<String>,
    output_to_log: bool,
    log_max_line_size: Option<usize>,
    stats_cardinality_limit: Option<usize>,
}

#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_new(
    out_handle: NonNull<Box<TraceExporterConfig>>,
) {
    catch_panic!(
        out_handle
            .as_ptr()
            .write(Box::<TraceExporterConfig>::default()),
        ()
    )
}

/// Frees TraceExporterConfig handle internal resources.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_free(handle: Box<TraceExporterConfig>) {
    drop(handle);
}

/// Sets traces destination.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_url(
    config: Option<&mut TraceExporterConfig>,
    url: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            handle.url = match sanitize_string(url) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets tracer's version to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_tracer_version(
    config: Option<&mut TraceExporterConfig>,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.tracer_version = match sanitize_string(version) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets tracer's language to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_language(
    config: Option<&mut TraceExporterConfig>,
    lang: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.language = match sanitize_string(lang) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets tracer's language version to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_lang_version(
    config: Option<&mut TraceExporterConfig>,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.language_version = match sanitize_string(version) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets tracer's language interpreter to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_lang_interpreter(
    config: Option<&mut TraceExporterConfig>,
    interpreter: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.language_interpreter = match sanitize_string(interpreter) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets hostname information to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_hostname(
    config: Option<&mut TraceExporterConfig>,
    hostname: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.hostname = match sanitize_string(hostname) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets environment information to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_env(
    config: Option<&mut TraceExporterConfig>,
    env: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.env = match sanitize_string(env) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_version(
    config: Option<&mut TraceExporterConfig>,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.version = match sanitize_string(version) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets service name to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_service(
    config: Option<&mut TraceExporterConfig>,
    service: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.service = match sanitize_string(service) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enables health metrics emission.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_enable_health_metrics(
    config: Option<&mut TraceExporterConfig>,
    is_enabled: bool,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(config) = config {
            config.health_metrics_enabled = is_enabled;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enables telemetry metrics.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_enable_telemetry(
    config: Option<&mut TraceExporterConfig>,
    telemetry_cfg: Option<&TelemetryClientConfig>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(config) = config {
            if let Option::Some(telemetry_cfg) = telemetry_cfg {
                let cfg = TelemetryConfig {
                    heartbeat: telemetry_cfg.interval,
                    runtime_id: match sanitize_string(telemetry_cfg.runtime_id) {
                        Ok(s) => Some(s),
                        Err(e) => return Some(e),
                    },
                    debug_enabled: telemetry_cfg.debug_enabled,
                };
                let sessions = TelemetryInstrumentationSessions {
                    session_id: match sanitize_string(telemetry_cfg.session_id) {
                        Ok(s) => Some(s),
                        Err(e) => return Some(e),
                    },
                    root_session_id: match sanitize_string(telemetry_cfg.root_session_id) {
                        Ok(s) => Some(s),
                        Err(e) => return Some(e),
                    },
                    parent_session_id: match sanitize_string(telemetry_cfg.parent_session_id) {
                        Ok(s) => Some(s),
                        Err(e) => return Some(e),
                    },
                };
                debug!(telemetry_cfg = ?cfg, telemetry_sessions = ?sessions, "Configuring telemetry");
                config.telemetry_cfg = Some(cfg);
                config.telemetry_instrumentation_sessions = sessions;
            }
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set client-side stats computation status.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_compute_stats(
    config: Option<&mut TraceExporterConfig>,
    is_enabled: bool,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(config) = config {
            config.compute_stats = is_enabled;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets `Datadog-Client-Computed-Stats` header to `true`.
/// This indicates that the upstream system has already computed the stats,
/// and no further stats computation should be performed.
///
/// <div class="warning">
/// This method must not be used when `compute_stats` is enabled, as it could
/// result in duplicate stats computation.
/// </div>
///
/// A common use case is in Application Security Monitoring (ASM) scenarios:
/// when APM is disabled but ASM is enabled, setting this header to `true`
/// ensures that no stats are computed at any level (exporter or agent).
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_client_computed_stats(
    config: Option<&mut TraceExporterConfig>,
    client_computed_stats: bool,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(config) = config {
            config.client_computed_stats = client_computed_stats;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the process tags to be included in the stats payload.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_process_tags(
    config: Option<&mut TraceExporterConfig>,
    process_tags: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.process_tags = match sanitize_string(process_tags) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the `X-Datadog-Test-Session-Token` header. Only used for testing with the test agent.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_test_session_token(
    config: Option<&mut TraceExporterConfig>,
    token: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.test_session_token = match sanitize_string(token) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the timeout in ms for all agent's connections.
///
/// When `ddog_trace_exporter_config_set_otlp_timeout` is unset, this value is also used as the
/// OTLP trace-export timeout.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_connection_timeout(
    config: Option<&mut TraceExporterConfig>,
    timeout_ms: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.connection_timeout = Some(timeout_ms);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the OTLP trace-export request timeout in ms, independent of the agent connection timeout.
///
/// Applies only to the OTLP export path (see
/// `ddog_trace_exporter_config_set_otlp_endpoint`). When left unset, the OTLP timeout falls back
/// to `ddog_trace_exporter_config_set_connection_timeout`; setting it here leaves the agent
/// connection timeout untouched.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_otlp_timeout(
    config: Option<&mut TraceExporterConfig>,
    timeout_ms: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = config {
            handle.otlp_timeout = Some(timeout_ms);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets a shared runtime for the TraceExporter to use for background workers.
///
/// `handle` must have been initialized with [`ddog_shared_runtime_new`].
///
/// When set, the exporter will use the provided runtime instead of creating its own.
/// This allows multiple exporters (or other components) to share a single runtime.
/// The config holds a clone of the `Arc` (increments the strong count), so the
/// original handle remains valid and must still be freed with
/// [`ddog_shared_runtime_free`].
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_shared_runtime(
    config: Option<&mut TraceExporterConfig>,
    handle: Option<NonNull<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        match (config, handle) {
            (Some(config), Some(handle)) => {
                // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
                // Increment the strong count before reconstructing so the config's Arc
                // is independent from the caller's handle.
                Arc::increment_strong_count(handle.as_ptr());
                config.shared_runtime = Some(Arc::from_raw(handle.as_ptr()));
                None
            }
            _ => gen_error!(ErrorCode::InvalidArgument),
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enables OTLP HTTP/JSON export and sets the endpoint URL.
///
/// When set, traces are sent to this URL in OTLP HTTP/JSON format instead of the Datadog
/// agent. The host language is responsible for resolving the endpoint from its configuration
/// (e.g. `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`) before calling this function.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_otlp_endpoint(
    config: Option<&mut TraceExporterConfig>,
    url: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            handle.otlp_endpoint = match sanitize_string(url) {
                Ok(s) => Some(s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the OTLP export protocol. Accepts the OTel-standard values `http/json` (default) or
/// `http/protobuf`; `grpc` is rejected as not yet supported. The host language resolves the value
/// (e.g. from `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL`).
///
/// Has no effect unless an OTLP endpoint is also configured via
/// `ddog_trace_exporter_config_set_otlp_endpoint`; without one, traces are sent to the
/// Datadog agent and this protocol selection is ignored.
///
/// Returns `None` on success, `ErrorCode::InvalidArgument` for a null config or an unaccepted
/// value, and `ErrorCode::InvalidInput` for a non-UTF-8 string.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_otlp_protocol(
    config: Option<&mut TraceExporterConfig>,
    protocol: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let value = match sanitize_string(protocol) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            // `FromStr` is the single source of truth for string -> OtlpProtocol. It accepts only
            // the supported HTTP encodings (`http/json`, `http/protobuf`); `grpc` and any unknown
            // value are rejected with an error, so an unsupported protocol can never be stored.
            match value.parse::<OtlpProtocol>() {
                Ok(p) => {
                    handle.otlp_protocol = Some(p);
                    None
                }
                Err(_) => gen_error!(ErrorCode::InvalidArgument),
            }
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets OTLP trace instrumentation scope metadata.
///
/// Has no effect unless an OTLP endpoint is also configured via
/// `ddog_trace_exporter_config_set_otlp_endpoint`; without one, traces are sent to the
/// Datadog agent and this scope metadata is ignored.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_otlp_instrumentation_scope(
    config: Option<&mut TraceExporterConfig>,
    name: CharSlice,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            let name = match sanitize_string(name) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            let version = match sanitize_string(version) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            handle.otlp_instrumentation_scope_name = Some(name);
            handle.otlp_instrumentation_scope_version = Some(version);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the cardinality limit for client-side stats computation.
///
/// When the number of distinct stats groups exceeds `limit`, additional groups are
/// aggregated into a sentinel key instead of being tracked individually.
/// This bounds memory usage when the trace population has very high cardinality.
///
/// Has no effect unless stats computation is enabled via
/// `ddog_trace_exporter_config_set_compute_stats`.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_stats_cardinality_limit(
    config: Option<&mut TraceExporterConfig>,
    limit: usize,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            handle.stats_cardinality_limit = Some(limit);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Configure the exporter to write traces as newline-delimited JSON to stdout (the Datadog
/// Forwarder "log exporter" path) instead of sending them to a Datadog agent. Used in serverless
/// environments (e.g. AWS Lambda) when no agent is reachable.
///
/// `max_line_size` overrides the per-line byte cap; pass `0` to use the default (256 KiB, the AWS
/// CloudWatch Logs limit). When enabled, agent-specific behavior (agent-info polling, client-side
/// stats, V1 negotiation) is bypassed.
///
/// In this mode each span's `meta` is serialized to process stdout (and thus captured by CloudWatch
/// Logs in Lambda); `meta_struct` is excluded because it holds raw msgpack the log intake cannot
/// interpret.
///
/// Writes are synchronous/blocking on stdout, so this mode targets single-threaded / current-thread
/// serverless runtimes (e.g. AWS Lambda) where a blocking write won't stall a shared async reactor.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_output_to_log(
    config: Option<&mut TraceExporterConfig>,
    max_line_size: usize,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = config {
            handle.output_to_log = true;
            handle.log_max_line_size = (max_line_size != 0).then_some(max_line_size);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Create a new TraceExporter instance.
///
/// When an OTLP endpoint is configured via `TraceExporterConfig`, the exporter sends traces to
/// that endpoint in OTLP over HTTP — JSON or protobuf per the configured protocol — instead of
/// to the Datadog agent. The same payload (e.g. MessagePack) is passed to
/// `ddog_trace_exporter_send`; the library decodes and converts it to OTLP when OTLP is enabled.
///
/// # Arguments
///
/// * `out_handle` - The handle to write the TraceExporter instance in.
/// * `config` - The configuration used to set up the TraceExporter handle.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_new(
    out_handle: NonNull<Box<TraceExporter>>,
    config: Option<&TraceExporterConfig>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(config) = config {
            let mut builder = TraceExporter::builder();
            // Only forward the agent URL when one was explicitly provided. Calling
            // `set_url("")` would mark the agent URL as configured and conflict with
            // agentless trace export, which rejects any caller-supplied agent URL at build
            // time. Leaving `url` unset lets the builder fall back to its default agent URL
            // when no transport override is configured.
            if let Some(url) = config.url.as_ref() {
                builder.set_url(url);
            }
            builder
                .set_tracer_version(config.tracer_version.as_ref().unwrap_or(&"".to_string()))
                .set_language(config.language.as_ref().unwrap_or(&"".to_string()))
                .set_language_version(config.language_version.as_ref().unwrap_or(&"".to_string()))
                .set_language_interpreter(
                    config
                        .language_interpreter
                        .as_ref()
                        .unwrap_or(&"".to_string()),
                )
                .set_hostname(config.hostname.as_ref().unwrap_or(&"".to_string()))
                .set_env(config.env.as_ref().unwrap_or(&"".to_string()))
                .set_app_version(config.version.as_ref().unwrap_or(&"".to_string()))
                .set_service(config.service.as_ref().unwrap_or(&"".to_string()))
                .set_process_tags(config.process_tags.as_deref().unwrap_or(""))
                .set_input_format(config.input_format)
                .set_output_format(config.output_format)
                .set_connection_timeout(config.connection_timeout);

            if let Some(limit) = config.stats_cardinality_limit {
                builder.set_stats_cardinality_limit(limit);
            }

            if config.compute_stats {
                builder.enable_stats(Duration::from_secs(10));
            } else if config.client_computed_stats {
                builder.set_client_computed_stats();
            }

            if let Some(cfg) = &config.telemetry_cfg {
                builder.enable_telemetry(cfg.clone());
            }
            builder.set_telemetry_instrumentation_sessions(
                config.telemetry_instrumentation_sessions.clone(),
            );

            if let Some(token) = &config.test_session_token {
                builder.set_test_session_token(token);
            }

            if config.health_metrics_enabled {
                builder.enable_health_metrics();
            }

            if let Some(runtime) = config.shared_runtime.clone() {
                builder.set_shared_runtime(runtime);
            }

            if let Some(ref url) = config.otlp_endpoint {
                builder.set_otlp_endpoint(url);
                if let Some(protocol) = config.otlp_protocol {
                    builder.set_otlp_protocol(protocol);
                }
                builder.set_otlp_timeout(config.otlp_timeout);
                builder.set_otlp_instrumentation_scope(
                    config
                        .otlp_instrumentation_scope_name
                        .as_deref()
                        .unwrap_or(""),
                    config
                        .otlp_instrumentation_scope_version
                        .as_deref()
                        .unwrap_or(""),
                );
            }

            if config.output_to_log {
                builder.set_output_to_log(config.log_max_line_size);
            }

            match builder.build() {
                Ok(exporter) => {
                    out_handle.as_ptr().write(Box::new(exporter));
                    None
                }
                Err(err) => Some(Box::new(ExporterError::from(err))),
            }
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Free the TraceExporter instance.
///
/// # Arguments
///
/// * handle - The handle to the TraceExporter instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_free(handle: Box<TraceExporter>) {
    let _ = catch_panic!(handle.shutdown(None), Ok(()));
}

/// Send traces to the Datadog Agent.
///
/// # Arguments
///
/// * `handle` - The handle to the TraceExporter instance.
/// * `trace` - The traces to send to the Datadog Agent in the input format used to create the
///   TraceExporter. The memory for the trace must be valid for the life of the call to this
///   function.
/// * `trace_count` - The number of traces to send to the Datadog Agent.
/// * `response_out` - Optional handle to store a pointer to the agent response information.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_send(
    handle: Option<&TraceExporter>,
    trace: ByteSlice,
    response_out: Option<NonNull<Box<ExporterResponse>>>,
) -> Option<Box<ExporterError>> {
    let exporter = match handle {
        Some(exp) => exp,
        None => return gen_error!(ErrorCode::InvalidArgument),
    };

    catch_panic!(
        match exporter.send(&trace) {
            Ok(resp) => {
                if let Some(result) = response_out {
                    result
                        .as_ptr()
                        .write(Box::new(ExporterResponse::from(resp)));
                }
                None
            }
            Err(e) => Some(Box::new(ExporterError::from(e))),
        },
        gen_error!(ErrorCode::Panic)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ddog_trace_exporter_error_free;
    use httpmock::prelude::*;
    use httpmock::MockServer;
    use libdd_trace_utils::span::v04::SpanSlice;
    use std::{borrow::Borrow, mem::MaybeUninit};

    #[test]
    fn config_constructor_test() {
        unsafe {
            let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();

            ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());

            let cfg = config.assume_init();
            assert_eq!(cfg.url, None);
            assert_eq!(cfg.tracer_version, None);
            assert_eq!(cfg.language, None);
            assert_eq!(cfg.language_version, None);
            assert_eq!(cfg.language_interpreter, None);
            assert_eq!(cfg.env, None);
            assert_eq!(cfg.hostname, None);
            assert_eq!(cfg.version, None);
            assert_eq!(cfg.service, None);
            assert_eq!(cfg.input_format, TraceExporterInputFormat::V04);
            assert_eq!(cfg.output_format, TraceExporterOutputFormat::V04);
            assert!(!cfg.compute_stats);
            assert!(cfg.telemetry_cfg.is_none());
            assert!(!cfg.health_metrics_enabled);
            assert!(cfg.process_tags.is_none());
            assert!(cfg.test_session_token.is_none());
            assert!(cfg.connection_timeout.is_none());
            assert!(!cfg.output_to_log);
            assert_eq!(cfg.log_max_line_size, None);
            assert_eq!(cfg.stats_cardinality_limit, None);
            assert!(cfg.otlp_instrumentation_scope_name.is_none());
            assert!(cfg.otlp_instrumentation_scope_version.is_none());

            ddog_trace_exporter_config_free(cfg);
        }
    }

    #[test]
    fn config_output_to_log_test() {
        unsafe {
            // Null config handle -> InvalidArgument.
            let error = ddog_trace_exporter_config_set_output_to_log(None, 0);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            // 0 is a sentinel for "use the default cap" -> None.
            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_output_to_log(config.as_mut(), 0);
            assert_eq!(error, None);
            let cfg = config.unwrap();
            assert!(cfg.output_to_log);
            assert_eq!(cfg.log_max_line_size, None);

            // Non-zero cap is stored as-is.
            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_output_to_log(config.as_mut(), 4096);
            assert_eq!(error, None);
            assert_eq!(config.unwrap().log_max_line_size, Some(4096));
        }
    }

    #[test]
    fn config_url_test() {
        unsafe {
            let error =
                ddog_trace_exporter_config_set_url(None, CharSlice::from("http://localhost"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_url(
                config.as_mut(),
                CharSlice::from("http://localhost"),
            );

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.url.as_ref().unwrap(), "http://localhost");
        }
    }

    #[test]
    fn config_tracer_version() {
        unsafe {
            let error = ddog_trace_exporter_config_set_tracer_version(None, CharSlice::from("1.0"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_tracer_version(
                config.as_mut(),
                CharSlice::from("1.0"),
            );
            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.tracer_version.as_ref().unwrap(), "1.0");
        }
    }

    #[test]
    fn config_language() {
        unsafe {
            let error = ddog_trace_exporter_config_set_language(None, CharSlice::from("lang"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_language(config.as_mut(), CharSlice::from("lang"));

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.language.as_ref().unwrap(), "lang");
        }
    }

    #[test]
    fn config_lang_version() {
        unsafe {
            let error = ddog_trace_exporter_config_set_lang_version(None, CharSlice::from("0.1"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_lang_version(
                config.as_mut(),
                CharSlice::from("0.1"),
            );

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.language_version.as_ref().unwrap(), "0.1");
        }
    }

    #[test]
    fn config_lang_interpreter_test() {
        unsafe {
            let error =
                ddog_trace_exporter_config_set_lang_interpreter(None, CharSlice::from("foo"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_lang_interpreter(
                config.as_mut(),
                CharSlice::from("foo"),
            );

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.language_interpreter.as_ref().unwrap(), "foo");
        }
    }

    #[test]
    fn config_hostname_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_hostname(None, CharSlice::from("hostname"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_hostname(
                config.as_mut(),
                CharSlice::from("hostname"),
            );

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.hostname.as_ref().unwrap(), "hostname");
        }
    }

    #[test]
    fn config_env_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_env(None, CharSlice::from("env-test"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_env(config.as_mut(), CharSlice::from("env-test"));

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.env.as_ref().unwrap(), "env-test");
        }
    }

    #[test]
    fn config_version_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_version(None, CharSlice::from("1.2"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_version(config.as_mut(), CharSlice::from("1.2"));

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.version.as_ref().unwrap(), "1.2");
        }
    }

    #[test]
    fn config_service_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_service(None, CharSlice::from("service"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_service(config.as_mut(), CharSlice::from("service"));

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.service.as_ref().unwrap(), "service");
        }
    }

    #[test]
    fn config_process_tags_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_process_tags(None, CharSlice::from("k:v"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_process_tags(
                config.as_mut(),
                CharSlice::from("key1:val1,key2:val2"),
            );

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert_eq!(cfg.process_tags.as_ref().unwrap(), "key1:val1,key2:val2");
        }
    }

    #[test]
    fn config_client_computed_stats_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_client_computed_stats(None, true);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_client_computed_stats(config.as_mut(), true);

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert!(cfg.client_computed_stats);
        }
    }

    #[test]
    fn config_telemetry_test() {
        unsafe {
            let error = ddog_trace_exporter_config_enable_telemetry(
                None,
                Some(&TelemetryClientConfig {
                    interval: 1000,
                    runtime_id: CharSlice::from("id"),
                    debug_enabled: false,
                    session_id: CharSlice::empty(),
                    root_session_id: CharSlice::empty(),
                    parent_session_id: CharSlice::empty(),
                }),
            );
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            let mut cfg = TraceExporterConfig::default();
            let error = ddog_trace_exporter_config_enable_telemetry(Some(&mut cfg), None);
            assert!(error.is_none());
            assert!(cfg.telemetry_cfg.is_none());

            let mut cfg = TraceExporterConfig::default();
            let error = ddog_trace_exporter_config_enable_telemetry(
                Some(&mut cfg),
                Some(&TelemetryClientConfig {
                    interval: 1000,
                    runtime_id: CharSlice::from("foo"),
                    debug_enabled: true,
                    session_id: CharSlice::empty(),
                    root_session_id: CharSlice::empty(),
                    parent_session_id: CharSlice::empty(),
                }),
            );
            assert!(error.is_none());
            assert_eq!(cfg.telemetry_cfg.as_ref().unwrap().heartbeat, 1000);
            assert!(cfg.telemetry_cfg.as_ref().unwrap().runtime_id.is_some());
            assert_eq!(
                cfg.telemetry_cfg
                    .as_ref()
                    .unwrap()
                    .runtime_id
                    .as_ref()
                    .unwrap(),
                "foo"
            );
            assert!(cfg.telemetry_cfg.as_ref().unwrap().debug_enabled);
            assert_eq!(
                cfg.telemetry_instrumentation_sessions.session_id.as_deref(),
                Some("")
            );
            assert_eq!(
                cfg.telemetry_instrumentation_sessions
                    .root_session_id
                    .as_deref(),
                Some("")
            );
            assert_eq!(
                cfg.telemetry_instrumentation_sessions
                    .parent_session_id
                    .as_deref(),
                Some("")
            );

            let mut cfg = TraceExporterConfig::default();
            let error = ddog_trace_exporter_config_enable_telemetry(
                Some(&mut cfg),
                Some(&TelemetryClientConfig {
                    interval: 500,
                    runtime_id: CharSlice::from("rid"),
                    debug_enabled: false,
                    session_id: CharSlice::from("sess-z"),
                    root_session_id: CharSlice::from("root-z"),
                    parent_session_id: CharSlice::from("par-z"),
                }),
            );
            assert!(error.is_none());
            let s = &cfg.telemetry_instrumentation_sessions;
            assert_eq!(s.session_id.as_deref(), Some("sess-z"));
            assert_eq!(s.root_session_id.as_deref(), Some("root-z"));
            assert_eq!(s.parent_session_id.as_deref(), Some("par-z"));
        }
    }

    #[test]
    fn config_timeout_test() {
        unsafe {
            let mut cfg = TraceExporterConfig::default();
            assert!(cfg.connection_timeout.is_none());

            ddog_trace_exporter_config_set_connection_timeout(Some(&mut cfg), 1000);
            assert_eq!(cfg.connection_timeout.unwrap(), 1000);
        }
    }

    #[test]
    fn config_otlp_timeout_test() {
        unsafe {
            let mut cfg = TraceExporterConfig::default();
            assert!(cfg.otlp_timeout.is_none());

            // Setting the OTLP timeout leaves the agent connection timeout untouched.
            ddog_trace_exporter_config_set_otlp_timeout(Some(&mut cfg), 250);
            assert_eq!(cfg.otlp_timeout.unwrap(), 250);
            assert!(cfg.connection_timeout.is_none());
        }
    }

    #[cfg_attr(miri, ignore)]
    #[test]
    fn exporter_constructor_test() {
        unsafe {
            let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();
            ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());

            let mut cfg = config.assume_init();
            let error = ddog_trace_exporter_config_set_url(
                Some(cfg.as_mut()),
                CharSlice::from("http://localhost"),
            );
            assert_eq!(error, None);

            let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();

            let ret = ddog_trace_exporter_new(
                NonNull::new_unchecked(&mut ptr).cast(),
                Some(cfg.borrow()),
            );
            let exporter = ptr.assume_init();

            assert_eq!(ret, None);

            ddog_trace_exporter_free(exporter);
            ddog_trace_exporter_config_free(cfg);
        }
    }

    #[test]
    fn exporter_send_test_arguments_test() {
        unsafe {
            let trace = ByteSlice::from(b"dummy contents" as &[u8]);
            let mut resp: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();
            let ret = ddog_trace_exporter_send(
                None,
                trace,
                Some(NonNull::new_unchecked(&mut resp).cast()),
            );

            assert!(ret.is_some());
            assert_eq!(ret.unwrap().code, ErrorCode::InvalidArgument);
        }
    }

    #[test]
    fn config_invalid_input_test() {
        unsafe {
            let mut config = Some(TraceExporterConfig::default());
            let invalid: [u8; 2] = [0x80u8, 0xFFu8];
            let error = ddog_trace_exporter_config_set_service(
                config.as_mut(),
                CharSlice::from_bytes(&invalid),
            );

            assert_eq!(error.unwrap().code, ErrorCode::InvalidInput);
        }
    }

    #[test]
    // Ignore because it seems, at least in the version we're currently using, miri can't emulate
    // libc::socket function.
    #[cfg_attr(miri, ignore)]
    fn exporter_send_check_rate_test() {
        unsafe {
            let server = MockServer::start();

            let _mock = server.mock(|when, then| {
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

            let cfg = TraceExporterConfig {
                url: Some(server.url("/")),
                tracer_version: Some("0.1".to_string()),
                language: Some("lang".to_string()),
                language_version: Some("0.1".to_string()),
                language_interpreter: Some("interpreter".to_string()),
                hostname: Some("hostname".to_string()),
                env: Some("env-test".to_string()),
                version: Some("1.0".to_string()),
                service: Some("test-service".to_string()),
                input_format: TraceExporterInputFormat::V04,
                output_format: TraceExporterOutputFormat::V04,
                ..Default::default()
            };

            let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
            let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();
            let mut ret =
                ddog_trace_exporter_new(NonNull::new_unchecked(&mut ptr).cast(), Some(&cfg));

            let exporter = ptr.assume_init();

            assert_eq!(ret, None);

            let data = rmp_serde::to_vec_named::<Vec<Vec<SpanSlice>>>(&vec![vec![]]).unwrap();
            let traces = ByteSlice::new(&data);
            ret = ddog_trace_exporter_send(
                Some(exporter.as_ref()),
                traces,
                Some(NonNull::new_unchecked(&mut response).cast()),
            );
            assert_eq!(ret, None);
            assert_eq!(
                String::from_utf8_lossy(&response.assume_init().body.unwrap()),
                r#"{
                    "rate_by_service": {
                        "service:foo,env:staging": 1.0,
                        "service:,env:": 0.8
                    }
                }"#,
            );
        }
    }

    #[test]
    // Ignore because it seems, at least in the version we're currently using, miri can't emulate
    // libc::socket function.
    #[cfg_attr(miri, ignore)]
    fn exporter_send_empty_array_test() {
        // Test added due to ensure the exporter is able to send empty arrays because some tracers
        // (.NET) ping the agent with the aforementioned data type.
        unsafe {
            let server = MockServer::start();
            let response_body = r#"{
                        "rate_by_service": {
                            "service:foo,env:staging": 1.0,
                            "service:,env:": 0.8
                        }
                    }"#;

            let mock_traces = server.mock(|when, then| {
                when.method(POST)
                    .header("Content-type", "application/msgpack")
                    .path("/v0.4/traces");
                then.status(200).body(response_body);
            });

            let cfg = TraceExporterConfig {
                url: Some(server.url("/")),
                tracer_version: Some("0.1".to_string()),
                language: Some("lang".to_string()),
                language_version: Some("0.1".to_string()),
                language_interpreter: Some("interpreter".to_string()),
                hostname: Some("hostname".to_string()),
                env: Some("env-test".to_string()),
                version: Some("1.0".to_string()),
                service: Some("test-service".to_string()),
                input_format: TraceExporterInputFormat::V04,
                output_format: TraceExporterOutputFormat::V04,
                ..Default::default()
            };

            let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
            let mut ret =
                ddog_trace_exporter_new(NonNull::new_unchecked(&mut ptr).cast(), Some(&cfg));

            let exporter = ptr.assume_init();

            assert_eq!(ret, None);

            let data = vec![0x90];
            let traces = ByteSlice::new(&data);
            let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();

            ret = ddog_trace_exporter_send(
                Some(exporter.as_ref()),
                traces,
                Some(NonNull::new_unchecked(&mut response).cast()),
            );
            mock_traces.assert();
            assert_eq!(ret, None);
            assert_eq!(
                String::from_utf8_lossy(&response.assume_init().body.unwrap()),
                response_body
            );

            ddog_trace_exporter_free(exporter);
        }
    }

    #[test]
    // Ignore because it seems, at least in the version we're currently using, miri can't emulate
    // libc::socket function.
    #[cfg_attr(miri, ignore)]
    fn exporter_send_telemetry_test() {
        unsafe {
            let server = MockServer::start();
            let response_body = r#"{
                        "rate_by_service": {
                            "service:foo,env:staging": 1.0,
                            "service:,env:": 0.8
                        }
                    }"#;
            let mock_traces = server.mock(|when, then| {
                when.method(POST).path("/v0.4/traces");
                then.status(200)
                    .header("content-type", "application/json")
                    .body(response_body);
            });

            let mock_metrics = server.mock(|when, then| {
                when.method(POST)
                    .path("/telemetry/proxy/api/v2/apmtelemetry")
                    .body_includes(r#""runtime_id":"foo""#)
                    .body_includes(r#""metric":"trace_api."#);
                then.status(200)
                    .header("content-type", "application/json")
                    .body("");
            });

            let cfg = TraceExporterConfig {
                url: Some(server.url("/")),
                tracer_version: Some("0.1".to_string()),
                language: Some("lang".to_string()),
                language_version: Some("0.1".to_string()),
                language_interpreter: Some("interpreter".to_string()),
                hostname: Some("hostname".to_string()),
                env: Some("env-test".to_string()),
                version: Some("1.0".to_string()),
                service: Some("test-service".to_string()),
                input_format: TraceExporterInputFormat::V04,
                output_format: TraceExporterOutputFormat::V04,
                telemetry_cfg: Some(TelemetryConfig {
                    heartbeat: 10000,
                    runtime_id: Some("foo".to_string()),
                    debug_enabled: true,
                }),
                ..Default::default()
            };

            let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();
            let mut ret =
                ddog_trace_exporter_new(NonNull::new_unchecked(&mut ptr).cast(), Some(&cfg));

            let exporter = ptr.assume_init();

            assert_eq!(ret, None);

            let data = vec![0x90];
            let traces = ByteSlice::new(&data);
            let mut response: MaybeUninit<Box<ExporterResponse>> = MaybeUninit::uninit();

            ret = ddog_trace_exporter_send(
                Some(exporter.as_ref()),
                traces,
                Some(NonNull::new_unchecked(&mut response).cast()),
            );
            mock_traces.assert();
            assert_eq!(ret, None);
            assert_eq!(
                String::from_utf8_lossy(&response.assume_init().body.unwrap()),
                response_body
            );

            ddog_trace_exporter_free(exporter);
            // It should receive 1 metrics payload (excluding heartbeats)
            mock_metrics.assert_calls(1);
        }
    }

    #[test]
    fn config_otlp_protocol_test() {
        unsafe {
            // Null config → InvalidArgument
            let error =
                ddog_trace_exporter_config_set_otlp_protocol(None, CharSlice::from("http/json"));
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            // "http/json" → success, stored
            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_otlp_protocol(
                config.as_mut(),
                CharSlice::from("http/json"),
            );
            assert_eq!(error, None);
            assert_eq!(
                config.as_ref().unwrap().otlp_protocol,
                Some(OtlpProtocol::HttpJson)
            );

            // "http/protobuf" → success, stored
            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_otlp_protocol(
                config.as_mut(),
                CharSlice::from("http/protobuf"),
            );
            assert_eq!(error, None);
            assert_eq!(
                config.as_ref().unwrap().otlp_protocol,
                Some(OtlpProtocol::HttpProtobuf)
            );

            // "grpc" → InvalidArgument
            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_otlp_protocol(
                config.as_mut(),
                CharSlice::from("grpc"),
            );
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            // Garbage value → InvalidArgument
            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_otlp_protocol(
                config.as_mut(),
                CharSlice::from("nonsense"),
            );
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            // Non-UTF-8 input → InvalidInput
            let mut config = Some(TraceExporterConfig::default());
            let invalid: [u8; 2] = [0x80u8, 0xFFu8];
            let error = ddog_trace_exporter_config_set_otlp_protocol(
                config.as_mut(),
                CharSlice::from_bytes(&invalid),
            );
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidInput);
            ddog_trace_exporter_error_free(error);
        }
    }

    #[test]
    fn config_otlp_instrumentation_scope_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_otlp_instrumentation_scope(
                None,
                CharSlice::from("dd-trace-js"),
                CharSlice::from("7.0.0-pre"),
            );
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_otlp_instrumentation_scope(
                config.as_mut(),
                CharSlice::from("dd-trace-js"),
                CharSlice::from("7.0.0-pre"),
            );
            assert_eq!(error, None);
            let cfg = config.as_ref().unwrap();
            assert_eq!(
                cfg.otlp_instrumentation_scope_name.as_deref(),
                Some("dd-trace-js")
            );
            assert_eq!(
                cfg.otlp_instrumentation_scope_version.as_deref(),
                Some("7.0.0-pre")
            );

            let mut config = Some(TraceExporterConfig::default());
            let invalid: [u8; 2] = [0x80u8, 0xFFu8];
            let error = ddog_trace_exporter_config_set_otlp_instrumentation_scope(
                config.as_mut(),
                CharSlice::from_bytes(&invalid),
                CharSlice::from("7.0.0-pre"),
            );
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidInput);
            ddog_trace_exporter_error_free(error);
            let cfg = config.as_ref().unwrap();
            assert!(cfg.otlp_instrumentation_scope_name.is_none());
            assert!(cfg.otlp_instrumentation_scope_version.is_none());
        }
    }

    #[test]
    fn set_otlp_protocol_stores_parsed_enum() {
        use libdd_data_pipeline::OtlpProtocol;
        let mut cfg = TraceExporterConfig::default();
        let err = unsafe {
            ddog_trace_exporter_config_set_otlp_protocol(
                Some(&mut cfg),
                CharSlice::from("http/protobuf"),
            )
        };
        assert!(err.is_none());
        assert_eq!(cfg.otlp_protocol, Some(OtlpProtocol::HttpProtobuf));
    }

    #[test]
    fn set_otlp_protocol_rejects_grpc_and_unknown() {
        let mut cfg = TraceExporterConfig::default();
        for bad in ["grpc", "nonsense"] {
            let err = unsafe {
                ddog_trace_exporter_config_set_otlp_protocol(Some(&mut cfg), CharSlice::from(bad))
            };
            assert!(err.is_some(), "expected error for {bad}");
            assert_eq!(cfg.otlp_protocol, None, "{bad} must not be stored");
        }
    }

    #[cfg(all(feature = "catch_panic", panic = "unwind"))]
    #[test]
    fn catch_panic_test() {
        let ret = catch_panic!(panic!("Panic!"), gen_error!(ErrorCode::Panic));

        assert!(ret.is_some());
        assert_eq!(ret.unwrap().code, ErrorCode::Panic);
    }

    #[test]
    fn config_stats_cardinality_limit_test() {
        unsafe {
            // Null config → InvalidArgument
            let error = ddog_trace_exporter_config_set_stats_cardinality_limit(None, 100);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            // Valid config → value stored
            let mut config = Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_stats_cardinality_limit(config.as_mut(), 500);
            assert_eq!(error, None);
            assert_eq!(config.unwrap().stats_cardinality_limit, Some(500));
        }
    }

    #[test]
    fn config_health_metrics_test() {
        unsafe {
            let error = ddog_trace_exporter_config_enable_health_metrics(None, true);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            assert!(!config.as_ref().unwrap().health_metrics_enabled);

            let error = ddog_trace_exporter_config_enable_health_metrics(config.as_mut(), true);
            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert!(cfg.health_metrics_enabled);
        }
    }
}
