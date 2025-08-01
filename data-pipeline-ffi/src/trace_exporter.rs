// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::error::{ExporterError, ExporterErrorCode as ErrorCode};
use crate::response::ExporterResponse;
use data_pipeline::trace_exporter::{
    TelemetryConfig, TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use ddcommon_ffi::{
    CharSlice,
    {slice::AsBytes, slice::ByteSlice},
};
use std::{ptr::NonNull, time::Duration};
use tracing::error;

#[cfg(all(feature = "catch_panic", panic = "unwind"))]
use std::panic::{catch_unwind, AssertUnwindSafe};

macro_rules! gen_error {
    ($l:expr) => {
        Some(Box::new(ExporterError::new($l, &$l.to_string())))
    };
}

#[cfg(all(feature = "catch_panic", panic = "unwind"))]
macro_rules! catch_panic {
    ($f:expr, $err:expr) => {
        match catch_unwind(AssertUnwindSafe(|| $f)) {
            Ok(ret) => ret,
            Err(info) => {
                if let Some(s) = info.downcast_ref::<String>() {
                    error!(error = %ErrorCode::Panic, s);
                } else if let Some(s) = info.downcast_ref::<&str>() {
                    error!(error = %ErrorCode::Panic, s);
                } else {
                    error!(error = %ErrorCode::Panic, "Unable to retrieve panic context");
                }
                $err
            }
        }
    };
}

#[cfg(any(not(feature = "catch_panic"), panic = "abort"))]
macro_rules! catch_panic {
    ($f:expr, $err:expr) => {
        $f
    };
}

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
    health_metrics_enabled: bool,
    test_session_token: Option<String>,
    rates_payload_version: bool,
    connection_timeout: Option<u64>,
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
                config.telemetry_cfg = Some(TelemetryConfig {
                    heartbeat: telemetry_cfg.interval,
                    runtime_id: match sanitize_string(telemetry_cfg.runtime_id) {
                        Ok(s) => Some(s),
                        Err(e) => return Some(e),
                    },
                    debug_enabled: telemetry_cfg.debug_enabled,
                })
            } else {
                config.telemetry_cfg = Some(TelemetryConfig::default());
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

/// Enables or disables the rates payload version feature.
/// When enabled, the trace exporter checks the payload version in the agent's response.
/// If the version hasn't changed since the last payload, the exporter will return an empty
/// response.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_rates_payload_version(
    config: Option<&mut TraceExporterConfig>,
    rates_payload_version: bool,
) -> Option<Box<ExporterError>> {
    catch_panic!(
        if let Option::Some(config) = config {
            config.rates_payload_version = rates_payload_version;
            None
        } else {
            gen_error!(ErrorCode::InvalidArgument)
        },
        gen_error!(ErrorCode::Panic)
    )
}

/// Sets the timeout in ms for all agent's connections.
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

/// Create a new TraceExporter instance.
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
            // let config = &*ptr;
            let mut builder = TraceExporter::builder();
            builder
                .set_url(config.url.as_ref().unwrap_or(&"".to_string()))
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
                .set_input_format(config.input_format)
                .set_output_format(config.output_format)
                .set_connection_timeout(config.connection_timeout);

            if config.compute_stats {
                builder.enable_stats(Duration::from_secs(10));
            } else if config.client_computed_stats {
                builder.set_client_computed_stats();
            }

            if let Some(cfg) = &config.telemetry_cfg {
                builder.enable_telemetry(Some(cfg.clone()));
            }

            if let Some(token) = &config.test_session_token {
                builder.set_test_session_token(token);
            }

            if config.rates_payload_version {
                builder.enable_agent_rates_payload_version();
            }

            if config.health_metrics_enabled {
                builder.enable_health_metrics();
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
    trace_count: usize,
    response_out: Option<NonNull<Box<ExporterResponse>>>,
) -> Option<Box<ExporterError>> {
    let exporter = match handle {
        Some(exp) => exp,
        None => return gen_error!(ErrorCode::InvalidArgument),
    };

    catch_panic!(
        match exporter.send(&trace, trace_count) {
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
    use datadog_trace_utils::span::SpanSlice;
    use httpmock::prelude::*;
    use httpmock::MockServer;
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
            assert!(cfg.test_session_token.is_none());
            assert!(!cfg.rates_payload_version);
            assert!(cfg.connection_timeout.is_none());

            ddog_trace_exporter_config_free(cfg);
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
                }),
            );
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);
            ddog_trace_exporter_error_free(error);

            let mut cfg = TraceExporterConfig::default();
            let error = ddog_trace_exporter_config_enable_telemetry(Some(&mut cfg), None);
            assert!(error.is_none());
            assert_eq!(cfg.telemetry_cfg.as_ref().unwrap().heartbeat, 0);
            assert!(cfg.telemetry_cfg.as_ref().unwrap().runtime_id.is_none());

            let mut cfg = TraceExporterConfig::default();
            let error = ddog_trace_exporter_config_enable_telemetry(
                Some(&mut cfg),
                Some(&TelemetryClientConfig {
                    interval: 1000,
                    runtime_id: CharSlice::from("foo"),
                    debug_enabled: true,
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

    #[cfg_attr(miri, ignore)]
    #[test]
    fn exporter_constructor_error_test() {
        unsafe {
            let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();
            ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());

            let mut cfg = config.assume_init();
            let error = ddog_trace_exporter_config_set_service(
                Some(cfg.as_mut()),
                CharSlice::from("service"),
            );
            assert_eq!(error, None);

            ddog_trace_exporter_error_free(error);

            let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();

            let ret = ddog_trace_exporter_new(NonNull::new_unchecked(&mut ptr).cast(), Some(&cfg));

            let error = ret.as_ref().unwrap();
            assert_eq!(error.code, ErrorCode::InvalidUrl);

            ddog_trace_exporter_error_free(ret);

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
                0,
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
                0,
                Some(NonNull::new_unchecked(&mut response).cast()),
            );
            assert_eq!(ret, None);
            assert_eq!(
                response.assume_init().body.to_string_lossy(),
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
                0,
                Some(NonNull::new_unchecked(&mut response).cast()),
            );
            mock_traces.assert();
            assert_eq!(ret, None);
            assert_eq!(response.assume_init().body.to_string_lossy(), response_body);

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
                    .body_contains(r#""runtime_id":"foo""#);
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
                    heartbeat: 50,
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
                0,
                Some(NonNull::new_unchecked(&mut response).cast()),
            );
            mock_traces.assert();
            assert_eq!(ret, None);
            assert_eq!(response.assume_init().body.to_string_lossy(), response_body);

            ddog_trace_exporter_free(exporter);
            // It should receive 1 payloads: metrics
            mock_metrics.assert_hits(1);
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
    fn rates_payload_version_test() {
        unsafe {
            let error = ddog_trace_exporter_config_set_rates_payload_version(None, true);
            assert_eq!(error.as_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error);

            let mut config = Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_rates_payload_version(config.as_mut(), true);

            assert_eq!(error, None);

            let cfg = config.unwrap();
            assert!(cfg.rates_payload_version);
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
