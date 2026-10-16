// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

//! Direct bindings to the trace exporter builder.

use super::{TraceExporter, sanitize_string};
use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::{catch_panic, gen_error};
use libdd_common::mutable_metadata::MutableMetadataHandle;
use libdd_common_ffi::{CharSlice, slice::AsBytes};
use libdd_data_pipeline::OtlpProtocol;
use libdd_data_pipeline::trace_exporter::stats::CardinalityLimitConfig;
use libdd_data_pipeline::trace_exporter::{
    TelemetryConfig, TelemetryInstrumentationSessions, TraceExporterBuilder,
    TraceExporterInputFormat, TraceExporterOutputFormat,
};
use libdd_shared_runtime::ForkSafeRuntime;
use std::{ptr::NonNull, sync::Arc, time::Duration};
use tracing::debug;

/// FFI-compatible mirror of the Rust `TelemetryConfig`: the two fields consumed by
/// [`ddog_trace_exporter_builder_enable_telemetry`]. Session identifiers are configured
/// separately via `ddog_trace_exporter_builder_set_telemetry_instrumentation_sessions`.
#[derive(Debug)]
#[repr(C)]
pub struct ExporterTelemetryConfig {
    /// How often telemetry should be sent, in milliseconds.
    pub heartbeat: u64,

    /// Whether to enable debug mode for telemetry.
    /// When enabled, sets the DD-Telemetry-Debug-Enabled header to true.
    /// Defaults to false.
    pub debug_enabled: bool,
}

/// A borrowed list of tag strings for builder setters that take a `Vec<String>`
/// (tracer tags, peer tags, metric tag keys).
///
/// `ptr` may be null only when `len` is 0; a non-zero `len` with a null `ptr` is rejected.
/// An empty list clears the corresponding builder field.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ExporterTagList<'a> {
    /// Pointer to the first `ddog_CharSlice`.
    pub ptr: *const CharSlice<'a>,
    /// Number of elements.
    pub len: usize,
}

/// A borrowed HTTP header name/value pair.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ExporterHeader<'a> {
    /// Header name.
    pub name: CharSlice<'a>,
    /// Header value.
    pub value: CharSlice<'a>,
}

/// A borrowed list of [`ExporterHeader`] pairs for builder setters that take a
/// `Vec<(String, String)>` (OTLP headers).
///
/// `ptr` may be null only when `len` is 0; a non-zero `len` with a null `ptr` is rejected.
/// An empty list clears the corresponding builder field.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct ExporterHeaderList<'a> {
    /// Pointer to the first `ddog_ExporterHeader`.
    pub ptr: *const ExporterHeader<'a>,
    /// Number of elements.
    pub len: usize,
}

/// Sanitizes a tag list into owned strings. Rejects a null `ptr` with a non-zero `len` and
/// invalid UTF-8 in any element.
fn sanitize_tag_list(list: ExporterTagList<'_>) -> Result<Vec<String>, Box<ExporterError>> {
    if list.ptr.is_null() {
        if list.len != 0 {
            return Err(Box::new(ExporterError::new(
                ErrorCode::InvalidArgument,
                &ErrorCode::InvalidArgument.to_string(),
            )));
        }
        return Ok(Vec::new());
    }
    // SAFETY: the caller guarantees `ptr` points to `len` readable elements for the duration
    // of the call.
    let slices = unsafe { core::slice::from_raw_parts(list.ptr, list.len) };
    let mut strings = Vec::with_capacity(slices.len());
    for slice in slices {
        strings.push(sanitize_string(*slice)?);
    }
    Ok(strings)
}

/// Sanitizes a header list into owned name/value pairs. Rejects a null `ptr` with a non-zero
/// `len` and invalid UTF-8 in any name or value.
fn sanitize_header_list(
    list: ExporterHeaderList<'_>,
) -> Result<Vec<(String, String)>, Box<ExporterError>> {
    if list.ptr.is_null() {
        if list.len != 0 {
            return Err(Box::new(ExporterError::new(
                ErrorCode::InvalidArgument,
                &ErrorCode::InvalidArgument.to_string(),
            )));
        }
        return Ok(Vec::new());
    }
    // SAFETY: the caller guarantees `ptr` points to `len` readable elements for the duration
    // of the call.
    let headers = unsafe { core::slice::from_raw_parts(list.ptr, list.len) };
    let mut pairs = Vec::with_capacity(headers.len());
    for header in headers {
        pairs.push((
            sanitize_string(header.name)?,
            sanitize_string(header.value)?,
        ));
    }
    Ok(pairs)
}

/// Creates a trace exporter builder. Free it with `ddog_trace_exporter_builder_free`, or
/// transfer ownership to `ddog_trace_exporter_builder_build` (including on build failure).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_new(
    out_handle: NonNull<Box<TraceExporterBuilder<ForkSafeRuntime>>>,
) {
    unsafe {
        catch_panic!(
            out_handle
                .as_ptr()
                .write(Box::<TraceExporterBuilder<ForkSafeRuntime>>::default()),
            ()
        )
    }
}

/// Frees a builder handle that has not been passed to `ddog_trace_exporter_builder_build`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_free(
    handle: Box<TraceExporterBuilder<ForkSafeRuntime>>,
) {
    drop(handle);
}

/// Sets traces destination.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_url(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    url: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            match sanitize_string(url) {
                Ok(s) => handle.set_url(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_tracer_version(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(version) {
                Ok(s) => handle.set_tracer_version(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_language(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    lang: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(lang) {
                Ok(s) => handle.set_language(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_language_version(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(version) {
                Ok(s) => handle.set_language_version(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_language_interpreter(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    interpreter: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(interpreter) {
                Ok(s) => handle.set_language_interpreter(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_hostname(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    hostname: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(hostname) {
                Ok(s) => handle.set_hostname(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_env(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    env: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(env) {
                Ok(s) => handle.set_env(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_app_version(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(version) {
                Ok(s) => handle.set_app_version(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_service(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    service: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(service) {
                Ok(s) => handle.set_service(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_enable_health_metrics(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            builder.enable_health_metrics();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enables telemetry metrics.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_enable_telemetry(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    telemetry_cfg: Option<&ExporterTelemetryConfig>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            if let Option::Some(telemetry_cfg) = telemetry_cfg {
                let cfg = TelemetryConfig {
                    heartbeat: telemetry_cfg.heartbeat,
                    debug_enabled: telemetry_cfg.debug_enabled,
                };
                debug!(telemetry_cfg = ?cfg, "Configuring telemetry");
                builder.enable_telemetry(cfg);
            }
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets optional instrumentation session headers on telemetry requests (`dd-session-id`,
/// `dd-root-session-id`, `dd-parent-session-id`). Each slice is stored as given; an empty
/// slice stores an empty session id.
///
/// All three slices are validated before any value is applied: invalid UTF-8 in any of them
/// returns an error and leaves the builder unchanged.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_telemetry_instrumentation_sessions(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    session_id: CharSlice,
    root_session_id: CharSlice,
    parent_session_id: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            let sessions = TelemetryInstrumentationSessions {
                session_id: match sanitize_string(session_id) {
                    Ok(s) => Some(s),
                    Err(e) => return Some(e),
                },
                root_session_id: match sanitize_string(root_session_id) {
                    Ok(s) => Some(s),
                    Err(e) => return Some(e),
                },
                parent_session_id: match sanitize_string(parent_session_id) {
                    Ok(s) => Some(s),
                    Err(e) => return Some(e),
                },
            };
            debug!(telemetry_sessions = ?sessions, "Configuring telemetry sessions");
            builder.set_telemetry_instrumentation_sessions(sessions);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enables client-side stats computation with the given bucket interval in seconds.
/// Pass `10` to use the default 10-second bucket interval.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_enable_stats(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    bucket_interval_secs: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            builder.enable_stats(Duration::from_secs(bucket_interval_secs));
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
/// This method must not be used with `ddog_trace_exporter_builder_enable_stats`, as it could
/// result in duplicate stats computation.
/// </div>
///
/// A common use case is in Application Security Monitoring (ASM) scenarios:
/// when APM is disabled but ASM is enabled, setting this header to `true`
/// ensures that no stats are computed at any level (exporter or agent).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_client_computed_stats(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            builder.set_client_computed_stats();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Shares a mutable metadata handle with the exporter builder.
///
/// Replaces metadata previously set by `ddog_trace_exporter_builder_set_process_tags` and
/// `ddog_trace_exporter_builder_set_runtime_id`. Calling those setters afterwards updates the
/// shared handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_mutable_metadata(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    metadata: Option<&MutableMetadataHandle>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let (Option::Some(builder), Option::Some(metadata)) = (builder, metadata) {
            builder.set_mutable_metadata(metadata.clone());
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the process tags to be included in the stats payload.
///
/// This should not be used when `ddog_trace_exporter_builder_set_mutable_metadata` is used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_process_tags(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    process_tags: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(process_tags) {
                Ok(s) => handle.set_process_tags(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the runtime id used in payloads.
///
/// This should not be used when `ddog_trace_exporter_builder_set_mutable_metadata` is used.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_runtime_id(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    runtime_id: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(runtime_id) {
                Ok(s) => handle.set_runtime_id(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_test_session_token(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    token: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(token) {
                Ok(s) => handle.set_test_session_token(&s),
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_connection_timeout(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    timeout_ms: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.set_connection_timeout(Some(timeout_ms));
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
/// The builder holds a clone of the `Arc` (increments the strong count), so the
/// original handle remains valid and must still be freed with
/// [`ddog_shared_runtime_free`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_shared_runtime(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    handle: Option<NonNull<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    unsafe {
        catch_panic!(
            match (builder, handle) {
                (Some(builder), Some(handle)) => {
                    // SAFETY: handle was produced by Arc::into_raw and the Arc is still alive.
                    // Increment the strong count before reconstructing so the builder's Arc
                    // is independent from the caller's handle.
                    Arc::increment_strong_count(handle.as_ptr());
                    builder.set_shared_runtime(Arc::from_raw(handle.as_ptr()));
                    None
                }
                _ => gen_error!(ErrorCode::InvalidArgument),
            },
            gen_error!(ErrorCode::Panic)
        )
    }
}

/// Enables OTLP trace export and sets the endpoint URL.
///
/// When set, traces are sent to this URL using the protocol selected by
/// `ddog_trace_exporter_builder_set_otlp_protocol` instead of the Datadog agent. The host language
/// is responsible for resolving the endpoint from its configuration (e.g.
/// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`) before calling this function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_otlp_endpoint(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    url: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            match sanitize_string(url) {
                Ok(s) => handle.set_otlp_endpoint(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the OTLP export protocol. Accepts the OTel-standard values `http/json` (default),
/// `http/protobuf`, or `grpc`; unknown values are rejected. The host language resolves the value
/// (e.g. from `OTEL_EXPORTER_OTLP_TRACES_PROTOCOL`).
/// The `grpc` protocol currently supports plaintext `http://` endpoints only.
///
/// Has no effect unless an OTLP endpoint is also configured via
/// `ddog_trace_exporter_builder_set_otlp_endpoint`; without one, traces are sent to the
/// Datadog agent and this protocol selection is ignored.
///
/// Returns `None` on success, `ErrorCode::InvalidArgument` for a null builder or an unaccepted
/// value, and `ErrorCode::InvalidInput` for a non-UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_otlp_protocol(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    protocol: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            let value = match sanitize_string(protocol) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            match value.parse::<OtlpProtocol>() {
                Ok(p) => {
                    handle.set_otlp_protocol(p);
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
/// `ddog_trace_exporter_builder_set_otlp_endpoint`; without one, traces are sent to the
/// Datadog agent and this scope metadata is ignored.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_otlp_instrumentation_scope(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    name: CharSlice,
    version: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            let name = match sanitize_string(name) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            let version = match sanitize_string(version) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            handle.set_otlp_instrumentation_scope(&name, &version);
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
/// `ddog_trace_exporter_builder_enable_stats`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_stats_cardinality_limit(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    limits: CardinalityLimitConfig,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            handle.set_stats_cardinality_limit(limits);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Returns a `CardinalityLimitConfig` with default values
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_default_stats_cardinality_limit()
-> CardinalityLimitConfig {
    CardinalityLimitConfig::default()
}

/// Enables agentless APM trace export and sets the intake URL and API key.
///
/// When set, APM trace spans are sent directly to the Datadog HTTP intake in JSON format
/// (`POST /v1/input`) instead of through the Datadog Agent. Agentless trace export is mutually
/// exclusive with both an agent URL ([`ddog_trace_exporter_builder_set_url`]) and an OTLP endpoint
/// ([`ddog_trace_exporter_builder_set_otlp_endpoint`]); combining either with this setter causes
/// [`ddog_trace_exporter_builder_build`] to return an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_agentless_endpoint(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    url: CharSlice,
    api_key: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            let endpoint = match sanitize_string(url) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            let api_key = match sanitize_string(api_key) {
                Ok(s) => s,
                Err(e) => return Some(e),
            };
            handle.set_agentless_endpoint(&endpoint, &api_key);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the request timeout (in milliseconds) used by the agentless intake transport.
///
/// Defaults to 15 seconds when unset. Calling this without also setting an agentless endpoint
/// causes [`ddog_trace_exporter_builder_build`] to return an error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_agentless_timeout(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    timeout_ms: u64,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            handle.set_agentless_timeout(Duration::from_millis(timeout_ms));
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the span obfuscation configuration from a JSON string.
///
/// `json` is deserialized into `ObfuscationConfig` (the same struct the Datadog Agent uses),
/// which controls obfuscation of HTTP URLs, SQL resources, Redis/Valkey commands, memcached
/// commands, credit-card numbers, elasticsearch/opensearch/mongodb bodies, and tag-replace
/// rules. Obfuscation is applied to every span on the agentless export path (see
/// [`ddog_trace_exporter_builder_set_agentless_endpoint`]); it has no effect on the agent path.
///
/// Returns an error if `json` is not valid UTF-8 or fails to deserialize. Example:
/// ```c
/// ddog_trace_exporter_builder_set_span_obfuscation_config(
///   cfg, DDOG_CHARSLICE_C(
///     "{\"http\":{\"remove_query_string\":true},"
///     "\"credit_cards\":{\"enabled\":true}}"));
/// ```
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_span_obfuscation_config(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    json: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            let json = match json.try_to_utf8() {
                Ok(s) => s,
                Err(_) => {
                    return Some(Box::new(ExporterError::new(
                        ErrorCode::InvalidInput,
                        &ErrorCode::InvalidInput.to_string(),
                    )));
                }
            };
            match serde_json::from_str::<
                libdd_trace_obfuscation::obfuscation_config::ObfuscationConfig,
            >(json)
            {
                Ok(cfg) => {
                    handle.set_span_obfuscation_config(cfg);
                    None
                }
                Err(e) => Some(Box::new(ExporterError::new(
                    ErrorCode::InvalidInput,
                    &format!("invalid obfuscation config JSON: {e}"),
                ))),
            }
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the URL to communicate with a dogstatsd server.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_dogstatsd_url(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    url: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(url) {
                Ok(s) => handle.set_dogstatsd_url(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the `Datadog-Meta-Git-Commit-Sha` header.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_git_commit_sha(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    git_commit_sha: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(git_commit_sha) {
                Ok(s) => handle.set_git_commit_sha(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the `Datadog-Container-Id` header.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_container_id(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    container_id: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(container_id) {
                Ok(s) => handle.set_container_id(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the `Datadog-Meta-Lang-Interpreter-Vendor` header.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_language_interpreter_vendor(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    lang_interpreter_vendor: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(lang_interpreter_vendor) {
                Ok(s) => handle.set_language_interpreter_vendor(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the input format of the trace payloads passed to `ddog_trace_exporter_send`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_input_format(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    input_format: TraceExporterInputFormat,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.set_input_format(input_format);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the output format used to serialize traces.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_output_format(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    output_format: TraceExporterOutputFormat,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.set_output_format(output_format);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Opt in to the V1 trace protocol.
///
/// V1 is only used after runtime negotiation with the agent via `/info`. When the agent does
/// not advertise the `/v1.0/traces` endpoint, the exporter falls back to V0.4 transparently.
/// V1 is only compatible with V0.4 input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_enable_v1_protocol(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.enable_v1_protocol();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Set the header indicating the tracer has computed the top-level tag.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_client_computed_top_level(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.set_client_computed_top_level();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets whether background workers spawned by this exporter are restarted in the child after
/// the process `fork()`s. Defaults to `true`.
///
/// Set to `false` if the trace exporter is recreated after the fork to avoid restarting workers
/// which are going to be discarded anyway.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_restart_after_fork(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    restart_after_fork: bool,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.set_restart_after_fork(restart_after_fork);
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enables storing and checking the agent payload version used to detect agent rate updates.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_enable_agent_rates_payload_version(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.enable_agent_rates_payload_version();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enables OpenTelemetry trace semantics when converting spans for OTLP export.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_enable_otel_trace_semantics(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            handle.enable_otel_trace_semantics();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the OTLP metrics intake endpoint used to export OTLP metrics. No effect unless an
/// OTLP endpoint is also configured via `ddog_trace_exporter_builder_set_otlp_endpoint`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_otlp_metrics_endpoint(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    url: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(handle) = builder {
            match sanitize_string(url) {
                Ok(s) => handle.set_otlp_metrics_endpoint(&s),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the tracer tags to include in requests.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_tracer_tags(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    tracer_tags: ExporterTagList,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            match sanitize_tag_list(tracer_tags) {
                Ok(tags) => builder.set_tracer_tags(tags),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Seeds the peer tags used for peer tags aggregation in stats computation (requires stats
/// computation to be enabled).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_peer_tags(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    peer_tags: ExporterTagList,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            match sanitize_tag_list(peer_tags) {
                Ok(tags) => builder.set_peer_tags(tags),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets additional metric tag keys.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_additional_metric_tag_keys(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    tag_keys: ExporterTagList,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            match sanitize_tag_list(tag_keys) {
                Ok(keys) => builder.set_additional_metric_tag_keys(keys),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets additional headers or gRPC metadata to include in OTLP trace export requests.
///
/// Headers should be provided as name/value pairs. The host language is responsible for
/// resolving headers from its configuration (e.g. `OTEL_EXPORTER_OTLP_TRACES_HEADERS`)
/// before calling this function.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_otlp_headers(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    headers: ExporterHeaderList,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            match sanitize_header_list(headers) {
                Ok(headers) => builder.set_otlp_headers(headers),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Additional HTTP headers for OTLP trace-metrics requests.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_otlp_metrics_headers(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    headers: ExporterHeaderList,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            match sanitize_header_list(headers) {
                Ok(headers) => builder.set_otlp_metrics_headers(headers),
                Err(e) => return Some(e),
            };
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Enable client-side stats obfuscation. Disabled by default.
///
/// Final activation also requires the agent to advertise a supported `obfuscation_version`
/// via the `/info` endpoint. When disabled, no `datadog-obfuscation-version` header is sent
/// on stats payloads.
#[cfg(feature = "stats-obfuscation")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_enable_client_side_stats_obfuscation(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            builder.enable_client_side_stats_obfuscation();
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the intake endpoint used to export client-side stats when agentless export is
/// configured. No effect unless an agentless endpoint is also configured via
/// `ddog_trace_exporter_builder_set_agentless_endpoint`; authentication reuses that
/// endpoint's API key.
///
/// `url` must be a fully-qualified `http`/`https` URL with a host/authority (e.g.
/// `https://trace.agent.{DD_SITE}/api/v0.2/stats`); a relative path is rejected at build
/// time because the background worker cannot route a request to it.
#[cfg(feature = "stats-obfuscation")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_agentless_stats_endpoint(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    url: CharSlice,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(builder) = builder {
            match sanitize_string(url) {
                Ok(s) => builder.set_agentless_stats_endpoint(&s),
                Err(e) => return Some(e),
            };
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
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_set_output_to_log(
    builder: Option<&mut TraceExporterBuilder<ForkSafeRuntime>>,
    max_line_size: usize,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Some(handle) = builder {
            handle.set_output_to_log((max_line_size != 0).then_some(max_line_size));
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Create a new TraceExporter instance.
///
/// When an OTLP endpoint is configured via `TraceExporterBuilder<ForkSafeRuntime>`, the exporter
/// sends traces using the configured `http/json`, `http/protobuf`, or `grpc` protocol instead of
/// the Datadog agent. The same payload (e.g. MessagePack) is passed to
/// `ddog_trace_exporter_send`; the library decodes and converts it to OTLP when OTLP is enabled.
///
/// # Arguments
///
/// * `out_handle` - The handle to write the TraceExporter instance in.
/// * `builder` - The builder used to set up the TraceExporter handle. Ownership is transferred to
///   this function even on failure; do not reuse or free it after this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ddog_trace_exporter_builder_build(
    builder: Option<Box<TraceExporterBuilder<ForkSafeRuntime>>>,
    out_handle: NonNull<Box<TraceExporter>>,
) -> Option<Box<ExporterError>> {
    unsafe {
        catch_panic!(
            if let Some(builder) = builder {
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
}

#[cfg(test)]
mod tests;
