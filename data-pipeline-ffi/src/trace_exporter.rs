// Copyright 2024-Present Datadog, Inc. https://www.datadoghq.com/
// SPDX-License-Identifier: Apache-2.0

use crate::error::{ExporterError as Error, ExporterErrorCode as ErrorCode};
use data_pipeline::trace_exporter::agent_response::AgentResponse;
use data_pipeline::trace_exporter::{
    TraceExporter, TraceExporterInputFormat, TraceExporterOutputFormat,
};
use ddcommon_ffi::{
    CharSlice,
    {slice::AsBytes, slice::ByteSlice},
};
use std::{ptr::NonNull, time::Duration};

/// The TraceExporterConfig object will hold the configuration properties for the TraceExporter.
/// Once the configuration is passed to the TraceExporter constructor the config is no longer
/// needed by the handle and it can be freed.
#[derive(Default, PartialEq)]
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
}

#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_new(
    out_handle: NonNull<Box<TraceExporterConfig>>,
) {
    out_handle
        .as_ptr()
        .write(Box::<TraceExporterConfig>::default());
}

/// Frees TraceExporterConfig handle internal resources.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_free(handle: Box<TraceExporterConfig>) {
    drop(handle);
}

/// Sets traces destination.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_url(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    url: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.url = Some(url.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Sets tracer's version to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_tracer_version(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    version: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.tracer_version = Some(version.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Sets tracer's language to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_language(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    lang: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.language = Some(lang.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Sets tracer's language version to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_lang_version(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    version: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.language_version = Some(version.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Sets tracer's language interpreter to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_lang_interpreter(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    interpreter: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.language_interpreter = Some(interpreter.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Sets hostname information to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_hostname(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    hostname: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.hostname = Some(hostname.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Sets environmet information to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_env(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    env: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.env = Some(env.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_version(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    version: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.version = Some(version.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Sets service name to be included in the headers request.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_config_set_service(
    config: ddcommon_ffi::Option<&mut TraceExporterConfig>,
    service: CharSlice,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(handle) = config {
        handle.service = Some(service.to_utf8_lossy().to_string());
        None.into()
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
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
    config: ddcommon_ffi::Option<&TraceExporterConfig>,
) -> ddcommon_ffi::Option<Error> {
    if let ddcommon_ffi::Option::Some(config) = config {
        let mut builder = TraceExporter::builder()
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
            .set_output_format(config.output_format);
        if config.compute_stats {
            builder = builder.enable_stats(Duration::from_secs(10))
            // TODO: APMSP-1317 Enable peer tags aggregation and stats by span_kind based on agent
            // configuration
        }

        match builder.build() {
            Ok(exporter) => {
                out_handle.as_ptr().write(Box::new(exporter));
                None.into()
            }
            Err(err) => Some(Error::from(err)).into(),
        }
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

/// Free the TraceExporter instance.
///
/// # Arguments
///
/// * handle - The handle to the TraceExporter instance.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_free(handle: Box<TraceExporter>) {
    drop(handle);
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
/// * `response` - Contains the agent response information.
#[no_mangle]
pub unsafe extern "C" fn ddog_trace_exporter_send(
    handle: &TraceExporter,
    trace: ByteSlice,
    trace_count: usize,
    response: ddcommon_ffi::Option<&mut AgentResponse>,
) -> ddcommon_ffi::Option<Error> {
    // necessary that the trace be static for the life of the FFI function call as the caller
    // currently owns the memory.
    //APMSP-1621 - Properly fix this sharp-edge by allocating memory on the Rust side
    let static_trace: ByteSlice<'static> = std::mem::transmute(trace);
    if let ddcommon_ffi::Option::Some(result) = response {
        match handle
            .send(
                tinybytes::Bytes::from_static(static_trace.as_slice()),
                trace_count,
            ) {
            Ok(resp) => {
                *result = resp;
                None.into()
            }
            Err(e) => Some(Error::from(e)).into(),
        }
    } else {
        let code = ErrorCode::InvalidArgument;
        ddcommon_ffi::Option::Some(Error::new(code, &code.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ddog_trace_exporter_error_free;
    use std::mem::MaybeUninit;

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

            ddog_trace_exporter_config_free(cfg);
        }
    }

    #[test]
    fn config_url_test() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_url(
                ddcommon_ffi::Option::None,
                CharSlice::from("http://localhost"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_url(
                config.as_mut(),
                CharSlice::from("http://localhost"),
            );

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.url.as_ref().unwrap(), "http://localhost");
        }
    }

    #[test]
    fn config_tracer_version() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_tracer_version(
                ddcommon_ffi::Option::None,
                CharSlice::from("1.0"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_tracer_version(
                config.as_mut(),
                CharSlice::from("1.0"),
            );
            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.tracer_version.as_ref().unwrap(), "1.0");
        }
    }

    #[test]
    fn config_language() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_language(
                ddcommon_ffi::Option::None,
                CharSlice::from("lang"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_language(config.as_mut(), CharSlice::from("lang"));

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.language.as_ref().unwrap(), "lang");
        }
    }

    #[test]
    fn config_lang_version() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_lang_version(
                ddcommon_ffi::Option::None,
                CharSlice::from("0.1"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_lang_version(
                config.as_mut(),
                CharSlice::from("0.1"),
            );

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.language_version.as_ref().unwrap(), "0.1");
        }
    }

    #[test]
    fn config_lang_interpreter_test() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_lang_interpreter(
                ddcommon_ffi::Option::None,
                CharSlice::from("foo"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_lang_interpreter(
                config.as_mut(),
                CharSlice::from("foo"),
            );

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.language_interpreter.as_ref().unwrap(), "foo");
        }
    }

    #[test]
    fn config_hostname_test() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_hostname(
                ddcommon_ffi::Option::None,
                CharSlice::from("hostname"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error = ddog_trace_exporter_config_set_hostname(
                config.as_mut(),
                CharSlice::from("hostname"),
            );

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.hostname.as_ref().unwrap(), "hostname");
        }
    }

    #[test]
    fn config_env_test() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_env(
                ddcommon_ffi::Option::None,
                CharSlice::from("env-test"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_env(config.as_mut(), CharSlice::from("env-test"));

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.env.as_ref().unwrap(), "env-test");
        }
    }

    #[test]
    fn config_version_test() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_version(
                ddcommon_ffi::Option::None,
                CharSlice::from("1.2"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_version(config.as_mut(), CharSlice::from("1.2"));

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.version.as_ref().unwrap(), "1.2");
        }
    }

    #[test]
    fn config_service_test() {
        unsafe {
            let mut error = ddog_trace_exporter_config_set_service(
                ddcommon_ffi::Option::None,
                CharSlice::from("service"),
            );
            assert_eq!(error.to_std_ref().unwrap().code, ErrorCode::InvalidArgument);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut config = ddcommon_ffi::Option::Some(TraceExporterConfig::default());
            let error =
                ddog_trace_exporter_config_set_service(config.as_mut(), CharSlice::from("service"));

            assert_eq!(error, ddcommon_ffi::Option::None);

            let cfg = config.to_std_ref().unwrap();
            assert_eq!(cfg.service.as_ref().unwrap(), "service");
        }
    }

    #[test]
    fn expoter_constructor_test() {
        unsafe {
            let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();
            ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());

            let mut cfg = config.assume_init();
            let error = ddog_trace_exporter_config_set_url(
                ddcommon_ffi::Option::Some(cfg.as_mut()),
                CharSlice::from("http://localhost"),
            );
            assert_eq!(error, ddcommon_ffi::Option::None);

            let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();

            let ret = ddog_trace_exporter_new(
                NonNull::new_unchecked(&mut ptr).cast(),
                ddcommon_ffi::Option::Some(&cfg),
            );
            let exporter = ptr.assume_init();

            assert_eq!(ret, ddcommon_ffi::Option::None);

            ddog_trace_exporter_free(exporter);
            ddog_trace_exporter_config_free(cfg);
        }
    }

    #[test]
    fn expoter_constructor_error_test() {
        unsafe {
            let mut config: MaybeUninit<Box<TraceExporterConfig>> = MaybeUninit::uninit();
            ddog_trace_exporter_config_new(NonNull::new_unchecked(&mut config).cast());

            let mut cfg = config.assume_init();
            let mut error = ddog_trace_exporter_config_set_service(
                ddcommon_ffi::Option::Some(cfg.as_mut()),
                CharSlice::from("service"),
            );
            assert_eq!(error, ddcommon_ffi::Option::None);

            ddog_trace_exporter_error_free(error.as_mut());

            let mut ptr: MaybeUninit<Box<TraceExporter>> = MaybeUninit::uninit();

            let mut ret = ddog_trace_exporter_new(
                NonNull::new_unchecked(&mut ptr).cast(),
                ddcommon_ffi::Option::Some(&cfg),
            );

            let error = ret.to_std_ref().unwrap();
            assert_eq!(error.code, ErrorCode::InvalidUrl);

            ddog_trace_exporter_error_free(ret.as_mut());

            ddog_trace_exporter_config_free(cfg);
        }
    }
}
